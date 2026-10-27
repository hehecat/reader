//! 本地书籍导入（EPUB / TXT / MOBI / AZW3 / PDF / FB2 / DOCX / CBZ / UMD）
//!
//! - EPUB：zip 解包 → container.xml → OPF 元数据 → spine 章节（XHTML → 纯文本）→ 封面
//! - TXT：编码检测（UTF-8/GBK）→ 分章（章节标题正则）
//! - MOBI/AZW3：mobi crate（PalmDB header + 记录表 + 解压）→ rawml HTML → 纯文本 → 分章；
//!   azw3（KF8）暂走 mobi 兼容层，结构差异/加密时返回友好错误
//! - PDF：lopdf 按页提取文本（每页解压上限防炸弹；大 PDF 限 300 页）→ 标题分章或页分章
//! - FB2：quick-xml 解析 body/section/title/p → 分章（每 section 一章）
//! - DOCX：zip + word/document.xml → 段落提取（标题样式分章或按规则/字数回退）
//! - CBZ：zip 内图片列表 → 章节 = 按文件名自然序的图片页（正文为 base64 data URI 图片标记）
//! - UMD：手写解析（对齐 me.ag2s.umdlib 语义）——魔数 + section/附加块状态机 →
//!   属性（标题/作者/年月日/题材/出版商）→ 0x83 章节偏移 + 0x84 标题 + zlib 正文块（UTF-16LE）

use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Read;

use crate::service::epub::{parse_opf, OpfMeta};

/// 导入的书籍
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedBook {
    pub meta: OpfMeta,
    /// 章节（标题 + 正文文本）
    pub chapters: Vec<Chapter>,
    /// 封面（原始字节）
    #[serde(skip)]
    pub cover: Option<Vec<u8>>,
    /// 格式（epub/txt/mobi/azw3/pdf/fb2/docx）
    pub format: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Chapter {
    pub title: String,
    pub content: String,
}

/// 本地书上传时的书籍类型（legacy BookType：0 文本/2 漫画）
pub fn local_book_type(ext: &str) -> i64 {
    if ext.eq_ignore_ascii_case("cbz") {
        2
    } else {
        0
    }
}

/// legacy LocalBook.analyzeNameAuthor：从文件名解析书名/作者。
/// 按序尝试 `《书名》作者：xx`、`《书名》`、`书名 作者：xx`、`书名 by xx`，
/// 未命中时用 legacy BookHelp.formatBookName/formatBookAuthor 清洗。
pub fn analyze_name_author(file_name: &str) -> (String, String) {
    let stem = match file_name.rfind('.') {
        Some(idx) if idx > 0 => &file_name[..idx],
        _ => file_name,
    };
    // legacy LocalBook.nameAuthorPatterns（首个命中即返回）
    let patterns: [&str; 4] = [
        r"(.*?)《([^《》]+)》.*?作者：(.*)",
        r"(.*?)《([^《》]+)》(.*)",
        r"(^)(.+) 作者：(.+)$",
        r"(^)(.+) by (.+)$",
    ];
    for p in patterns {
        if let Ok(re) = crate::util::regex::Regex::new(p) {
            if let Some(caps) = re.captures_iter(stem).next() {
                if let (Some(name), Some(g1), Some(g3)) = (caps.get(2), caps.get(1), caps.get(3)) {
                    let author = format_book_author(&format!("{}{}", g1.as_str(), g3.as_str()));
                    return (name.as_str().to_string(), author);
                }
            }
        }
    }
    let name = format_book_name(stem);
    let remainder = stem.replace(&name, "");
    let author = if remainder.len() != stem.len() {
        format_book_author(&remainder)
    } else {
        String::new()
    };
    (name, author)
}

/// legacy BookHelp.formatBookName：去掉「作者 xx」/「xx 著」后缀
fn format_book_name(name: &str) -> String {
    let cleaned = crate::util::regex::Regex::new(r"\s+作\s*者.*|\s+\S+\s+著")
        .ok()
        .map(|re| re.replace_all(name, "").into_owned())
        .unwrap_or_else(|| name.to_string());
    cleaned.trim().to_string()
}

/// legacy BookHelp.formatBookAuthor：去掉「作者：/作者 」前缀与「著」后缀
fn format_book_author(author: &str) -> String {
    let cleaned = crate::util::regex::Regex::new(r"^\s*作\s*者[:：\s]+|\s+著")
        .ok()
        .map(|re| re.replace_all(author, "").into_owned())
        .unwrap_or_else(|| author.to_string());
    cleaned.trim().to_string()
}

/// EPUB 目录模式（legacy Book.tocUrl 六模式：toc / spin<toc / spin+toc / toc+spin / toc<spin）
pub const EPUB_TOC_MODES: &[&str] = &["toc", "spin<toc", "spin+toc", "toc+spin", "toc<spin"];

/// EPUB 目录默认模式（legacy EpubFile：tocUrl 为空时按 spin+toc）
pub const DEFAULT_EPUB_TOC_MODE: &str = "spin+toc";

/// 字符串是否为合法 EPUB 目录模式
pub fn is_epub_toc_mode(s: &str) -> bool {
    EPUB_TOC_MODES.contains(&s)
}

/// EPUB 解析（支持 legacy tocUrl 六模式控制目录顺序/标题）
///
/// 六模式语义（对齐 legacy EpubFile.getChapterListBy*）：
/// - `toc`：仅用 toc.ncx / nav.xhtml 目录（内容按目录条目 href 读取）
/// - `spin+toc`（默认）：spine 顺序为骨架，toc 标题在 spine 标题为空时覆盖
/// - `spin<toc`：spine 顺序为骨架，toc 标题强制覆盖 spine 标题
/// - `toc+spin`：toc 顺序为骨架，spine 标题在 toc 标题为空时覆盖
/// - `toc<spin`：toc 顺序为骨架，spine 标题强制覆盖 toc 标题
pub fn parse_epub(bytes: &[u8], toc_mode: &str) -> Result<ImportedBook> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("EPUB 不是有效的 zip")?;

    // 1. container.xml → OPF 路径
    let container =
        read_zip(&mut zip, "META-INF/container.xml").context("缺少 META-INF/container.xml")?;
    let container_str = String::from_utf8_lossy(&container);
    let opf_path = extract_attr_simple(&container_str, "rootfile", "full-path")
        .context("container.xml 缺少 rootfile")?;

    // 2. OPF 元数据
    let opf = read_zip(&mut zip, &opf_path).context("读取 OPF 失败")?;
    let meta = parse_opf(&String::from_utf8_lossy(&opf));

    // 3-4. spine/manifest + 章节内容（公共提取）
    let opf_str = String::from_utf8_lossy(&opf);
    let spin_chapters = opf_chapters(&mut zip, &opf_path, &opf_str);

    // 5. 封面
    let cover = meta.cover_href.as_ref().and_then(|href| {
        let full = resolve_opf_path(&opf_path, href);
        read_zip(&mut zip, &full).ok()
    });

    // 6. toc 目录解析（toc.ncx EPUB2 / nav.xhtml EPUB3）并按模式合并章节顺序/标题
    let toc_entries = parse_epub_toc(&mut zip, &opf_path, &opf_str);
    let chapters = merge_epub_chapters(&mut zip, &toc_entries, spin_chapters, toc_mode);

    Ok(ImportedBook {
        meta,
        chapters,
        cover,
        format: "epub".into(),
    })
}

/// 从 OPF manifest 定位 toc 文件并解析目录条目（对齐 legado EpubFile.getChapterList）：
/// - EPUB2：`application/x-dtbncx+xml` 的 toc.ncx → navMap/navPoint 深度优先
/// - EPUB3：`properties="nav"` 的 nav.xhtml → 第一个 `nav[epub:type=toc]` 的链接
///
/// 返回 (href, title) 列表（href 为相对 OPF 的规范化路径，含片段前缀）。
fn parse_epub_toc<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    opf_path: &str,
    opf_str: &str,
) -> Vec<(String, String)> {
    let manifest: std::collections::HashMap<String, (String, String)> = extract_manifest(opf_str);
    // EPUB3 nav 优先；EPUB2 NCX 其次（manifest 顺序保持）
    let mut toc_href: Option<String> = None;
    let mut ncx_href: Option<String> = None;
    for (href, mediatype) in manifest.values() {
        if mediatype.contains("dtbncx") && ncx_href.is_none() {
            ncx_href = Some(resolve_opf_path(opf_path, href));
        }
        if mediatype.contains("nav") && toc_href.is_none() {
            toc_href = Some(resolve_opf_path(opf_path, href));
        }
    }
    if let Some(path) = &toc_href {
        if let Ok(bytes) = read_zip(zip, path) {
            let html = String::from_utf8_lossy(&bytes);
            let list = nav_links(&html, path);
            if !list.is_empty() {
                return list;
            }
        }
    }
    if let Some(path) = &ncx_href {
        if let Ok(bytes) = read_zip(zip, path) {
            let xml = String::from_utf8_lossy(&bytes);
            let list = ncx_nav_points(&xml, path);
            if !list.is_empty() {
                return list;
            }
        }
    }
    Vec::new()
}

/// 解析 nav.xhtml 的 toc 导航链接（nav[epub:type=toc] 优先；无则第一个 nav）。
/// href 相对 nav 文件路径解析；标题取 <a> 文本（去空白）。
fn nav_links(html: &str, nav_path: &str) -> Vec<(String, String)> {
    let doc = scraper::Html::parse_document(html);
    let sel_toc = scraper::Selector::parse(r#"nav[epub\:type="toc"]"#)
        .or_else(|_| scraper::Selector::parse("nav"))
        .ok();
    let sel_any = scraper::Selector::parse("nav").ok();
    let sel = sel_toc.or(sel_any);
    let Some(sel) = sel else { return vec![] };
    let mut out = Vec::new();
    for nav in doc.select(&sel) {
        let a_sel = scraper::Selector::parse("a[href]").ok();
        if let Some(a_sel) = a_sel {
            for a in nav.select(&a_sel) {
                let Some(href) = a.value().attr("href") else {
                    continue;
                };
                let title = a.text().collect::<String>().trim().to_string();
                if title.is_empty() {
                    continue;
                }
                let href = href.split('#').next().unwrap_or(href);
                let full = resolve_opf_path(nav_path, href);
                out.push((full, title));
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// 解析 toc.ncx 的 navMap（navPoint 深度优先，含嵌套子点；标题取 navLabel/text）。
fn ncx_nav_points(xml: &str, ncx_path: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let stack: Vec<(usize, String)> = Vec::new();
    // 简化解析：逐 navPoint 开闭标签处理（不引入 XML 依赖——EPUB2 NCX 结构固定）
    let mut pos = 0usize;
    while let Some(open) = xml[pos..].find("<navPoint") {
        let open_abs = pos + open;
        let tag_end = xml[open_abs..]
            .find('>')
            .map(|i| open_abs + i + 1)
            .unwrap_or(xml.len());
        // 自闭合 navPoint（罕见）跳过
        if xml[open_abs..tag_end].ends_with("/>") {
            pos = tag_end;
            continue;
        }
        // 找对应 </navPoint>（先于下一个 <navPoint 的闭合——简单处理：从 tag_end 找最近 </navPoint>）
        let content_start = tag_end;
        let close_rel = xml[content_start..].find("</navPoint>");
        let Some(close_rel) = close_rel else { break };
        let close_abs = content_start + close_rel;
        let inner = &xml[content_start..close_abs];
        // 文本标签（不嵌套 navPoint——直接截取）
        let text = if let Some(ti) = inner.find("<text>") {
            let after = &inner[ti + 6..];
            if let Some(te) = after.find("</text>") {
                after[..te].trim().to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let src = if let Some(si) = inner.find("content") {
            let after = &inner[si + 7..];
            if let Some(q0) = after.find('"') {
                let rest = &after[q0 + 1..];
                if let Some(q1) = rest.find('"') {
                    rest[..q1].to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        // 子 navPoint 的深度：inner 中若含 <navPoint 则为嵌套（父级在前），
        // 简化：全部 navPoint 顺序输出（legado 也按 navPoint 出现顺序深度优先）
        if !text.is_empty() && !src.is_empty() {
            let href = src.split('#').next().unwrap_or(&src);
            let full = resolve_opf_path(ncx_path, href);
            out.push((full, text));
        }
        let _ = &stack;
        pos = close_abs + "</navPoint>".len();
    }
    out
}

/// 按目录模式合并 spine 章节与 toc 目录（骨架 + 标题覆盖，对齐 legacy）
/// toc_entries 的 href 为已解析的 zip 内完整路径（相对 zip 根），直接用于读取。
fn merge_epub_chapters<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    toc_entries: &[(String, String)],
    spin_chapters: Vec<(String, Chapter)>,
    toc_mode: &str,
) -> Vec<Chapter> {
    let mode = if is_epub_toc_mode(toc_mode) {
        toc_mode
    } else {
        DEFAULT_EPUB_TOC_MODE
    };
    // 无 toc 目录 → 退化为 spine（任何模式）
    if toc_entries.is_empty() {
        return spin_chapters.into_iter().map(|(_, c)| c).collect();
    }
    match mode {
        // spine 骨架：toc 标题覆盖（spin<toc 强制；spin+toc 仅当 spine 标题为空）
        "spin<toc" | "spin+toc" => {
            let force = mode == "spin<toc";
            let title_map: std::collections::HashMap<&str, &str> = toc_entries
                .iter()
                .map(|(h, t)| (h.as_str(), t.as_str()))
                .collect();
            spin_chapters
                .into_iter()
                .map(|(href, c)| {
                    let mut c = c;
                    if let Some(t) = title_map.get(href.as_str()) {
                        if force || c.title.is_empty() {
                            c.title = (*t).to_string();
                        }
                    }
                    c
                })
                .collect()
        }
        // toc 骨架：内容按 toc 条目 href 读取，标题用 spine 覆盖
        //（toc<spin 强制；toc+spin 仅当 toc 标题为空）
        "toc+spin" | "toc<spin" => {
            let force = mode == "toc<spin";
            let spin_titles: std::collections::HashMap<String, String> = spin_chapters
                .into_iter()
                .map(|(h, c)| (h, c.title))
                .collect();
            let mut out = Vec::new();
            for (href, title) in toc_entries {
                let spin_title = spin_titles.get(href.as_str());
                let mut title = title.clone();
                if let Some(st) = spin_title {
                    if force || title.is_empty() {
                        title = st.clone();
                    }
                }
                let full = href;
                if let Ok(bytes) = read_zip(zip, full) {
                    let html = String::from_utf8_lossy(&bytes);
                    let text = html_to_text(&html);
                    if !text.trim().is_empty() {
                        out.push(Chapter {
                            title,
                            content: text,
                        });
                    }
                }
            }
            if !out.is_empty() {
                return out;
            }
            // toc 条目内容全部读取失败 → 回退 spine
            spin_titles
                .into_values()
                .map(|t| Chapter {
                    title: t,
                    content: String::new(),
                })
                .collect()
        }
        // `toc`：仅 toc 目录
        _ => {
            let mut out = Vec::new();
            for (href, title) in toc_entries {
                let full = href;
                if let Ok(bytes) = read_zip(zip, full) {
                    let html = String::from_utf8_lossy(&bytes);
                    let text = html_to_text(&html);
                    if !text.trim().is_empty() {
                        out.push(Chapter {
                            title: title.clone(),
                            content: text,
                        });
                    }
                }
            }
            out
        }
    }
}

/// 解析包含 OPF 的 zip（无 container.xml 的裸 OPF 结构——解包 EPUB 重新打包/纯 OPF 目录 zip）
/// 自动查找 zip 内 .opf（根目录优先）→ OPF 元数据 + spine 顺序章节
pub fn parse_opf_zip(bytes: &[u8]) -> Result<ImportedBook> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("不是有效的 zip")?;
    // 找 .opf（优先根目录——其次任意路径）
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();
    let opf_path = names
        .iter()
        .find(|n| n.ends_with(".opf") && !n.contains('/'))
        .or_else(|| names.iter().find(|n| n.ends_with(".opf")))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("zip 内未找到 .opf 文件"))?;
    let opf = read_zip(&mut zip, &opf_path).context("读取 OPF 失败")?;
    let meta = parse_opf(&String::from_utf8_lossy(&opf));
    let opf_str = String::from_utf8_lossy(&opf);
    let chapters: Vec<Chapter> = opf_chapters(&mut zip, &opf_path, &opf_str)
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    if chapters.is_empty() {
        return Err(anyhow::anyhow!("OPF 未解析到章节内容（spine 引用缺失）"));
    }
    let cover = meta.cover_href.as_ref().and_then(|href| {
        let full = resolve_opf_path(&opf_path, href);
        read_zip(&mut zip, &full).ok()
    });
    Ok(ImportedBook {
        meta,
        chapters,
        cover,
        format: "epub".into(),
    })
}

/// 从 OPF 提取章节（spine 顺序；空则 fallback manifest 全部 xhtml）。
/// 返回 (href 规范化路径, 章节)——href 供 toc 模式合并时做键匹配。
fn opf_chapters<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    opf_path: &str,
    opf_str: &str,
) -> Vec<(String, Chapter)> {
    let spine_refs: Vec<String> = extract_all_attr(opf_str, "itemref", "idref");
    let manifest: std::collections::HashMap<String, (String, String)> = extract_manifest(opf_str);
    let mut chapters = Vec::new();
    for idref in &spine_refs {
        let Some((href, mediatype)) = manifest.get(idref) else {
            continue;
        };
        if !mediatype.contains("xhtml") && !mediatype.contains("html") {
            continue;
        }
        let full_path = resolve_opf_path(opf_path, href);
        let Ok(content_bytes) = read_zip(zip, &full_path) else {
            continue;
        };
        let html = String::from_utf8_lossy(&content_bytes);
        let text = html_to_text(&html);
        if text.trim().is_empty() {
            continue;
        }
        let title = extract_title(&html).unwrap_or_else(|| format!("第 {} 节", chapters.len() + 1));
        chapters.push((
            full_path,
            Chapter {
                title,
                content: text,
            },
        ));
    }
    if chapters.is_empty() {
        for (href, mediatype) in manifest.values() {
            if !mediatype.contains("xhtml") && !mediatype.contains("html") {
                continue;
            }
            let full_path = resolve_opf_path(opf_path, href);
            if let Ok(content_bytes) = read_zip(zip, &full_path) {
                let html = String::from_utf8_lossy(&content_bytes);
                let text = html_to_text(&html);
                if !text.trim().is_empty() {
                    let title = extract_title(&html)
                        .unwrap_or_else(|| format!("第 {} 节", chapters.len() + 1));
                    chapters.push((
                        full_path,
                        Chapter {
                            title,
                            content: text,
                        },
                    ));
                }
            }
        }
    }
    chapters
}

/// TXT 解析（编码检测 + 分章；使用内置默认规则）
pub fn parse_txt(bytes: &[u8]) -> Result<ImportedBook> {
    parse_txt_with_rules(bytes, &[])
}

/// TXT 解析（编码检测 + 分章；rules 为空时用内置启用规则，否则用用户自定义规则）
pub fn parse_txt_with_rules(bytes: &[u8], user_rules: &[String]) -> Result<ImportedBook> {
    // 编码检测：UTF-8 优先 → UTF-16 LE/BE（BOM 识别——Windows 记事本另存 UTF-16 常见）→ GBK/GB18030
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            if bytes.starts_with(&[0xFF, 0xFE]) {
                encoding_rs::UTF_16LE.decode(bytes).0.into_owned()
            } else if bytes.starts_with(&[0xFE, 0xFF]) {
                encoding_rs::UTF_16BE.decode(bytes).0.into_owned()
            } else {
                encoding_rs::GBK.decode(bytes).0.into_owned()
            }
        }
    };
    // 去掉 BOM
    let text = text.trim_start_matches('\u{feff}').to_string();

    // 分章：优先用户自定义 TXT 目录规则（txt_toc_rules），无则用内置启用规则
    let rules: Vec<String> = if user_rules.is_empty() {
        default_toc_rule_regexes()
    } else {
        user_rules.to_vec()
    };
    let mut chapters = split_by_rules(&text, &rules);
    if chapters.is_empty() {
        // 无章节标记：长文本按 10000 字分块 / 短文本整本一章
        chapters = chunk_fallback(&text);
    }

    // 元数据（文件名信息由调用方补充——这里取首行做书名猜测）
    let title = text.lines().next().unwrap_or("本地书籍").trim().to_string();
    let meta = OpfMeta {
        title: title.clone(),
        author: String::new(),
        ..Default::default()
    };

    Ok(ImportedBook {
        meta,
        chapters,
        cover: None,
        format: "txt".into(),
    })
}

/// 内置默认 TXT 目录规则定义（对齐 legacy DefaultData.txtTocRule.json 全量）
#[derive(Debug, Clone, Copy)]
pub struct DefaultTocRuleDef {
    pub name: &'static str,
    pub rule: &'static str,
    pub enable: bool,
    pub serial_number: i64,
}

/// legacy 内置 18 条 TXT 目录规则（含禁用项；TXT 分章只取 enable=true，与
/// legacy TextFile.getTocRules 语义一致）。正则原文来自
/// `src/main/resources/defaultData/txtTocRule.json`，经正则兼容层编译
/// （lookbehind/lookahead 自动升级 fancy-regex）。
pub const DEFAULT_TOC_RULE_DEFS: &[DefaultTocRuleDef] = &[
    DefaultTocRuleDef {
        name: "目录(去空白)",
        rule: r"(?<=[　\s])(?:序章|序言|卷首语|扉页|楔子|正文(?!完|结)|终章|后记|尾声|番外|第?\s{0,4}[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?\s{0,4}(?:章|节(?!课)|卷|集(?![合和])|部(?![分赛游])|篇(?!张))).{0,30}$",
        enable: true,
        serial_number: 0,
    },
    DefaultTocRuleDef {
        name: "目录",
        rule: r"^[ 　\t]{0,4}(?:序章|序言|卷首语|扉页|楔子|正文(?!完|结)|终章|后记|尾声|番外|第?\s{0,4}[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?\s{0,4}(?:章|节(?!课)|卷|集(?![合和])|部(?![分赛游])|篇(?!张))).{0,30}$",
        enable: true,
        serial_number: 1,
    },
    DefaultTocRuleDef {
        name: "目录(匹配简介)",
        rule: r"(?<=[　\s])(?:(?:内容|文章)?简介|文案|前言|序章|序言|卷首语|扉页|楔子|正文(?!完|结)|终章|后记|尾声|番外|第?\s{0,4}[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?\s{0,4}(?:章|节(?!课)|卷|集(?![合和])|部(?![分赛游])|回(?![合来事去])|场(?![和合比电是])|篇(?!张))).{0,30}$",
        enable: false,
        serial_number: 2,
    },
    DefaultTocRuleDef {
        name: "目录(古典、轻小说备用)",
        rule: r"^[ 　\t]{0,4}(?:序章|楔子|正文(?!完|结)|终章|后记|尾声|番外|第?\s{0,4}[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?\s{0,4}(?:章|节(?!课)|卷|集(?![合和])|部(?![分赛游])|回(?![合来事去])|场(?![和合比电是])|话|篇(?!张))).{0,30}$",
        enable: false,
        serial_number: 3,
    },
    DefaultTocRuleDef {
        name: "数字(纯数字标题)",
        rule: r"(?<=[　\s])\d+\.?[ 　\t]{0,4}$",
        enable: false,
        serial_number: 4,
    },
    DefaultTocRuleDef {
        name: "数字 分隔符 标题名称",
        rule: r"^[ 　\t]{0,4}\d{1,5}[：:,.， 、_—\-].{1,30}$",
        enable: true,
        serial_number: 5,
    },
    DefaultTocRuleDef {
        name: "大写数字 分隔符 标题名称",
        rule: r"^[ 　\t]{0,4}(?:序章|序言|卷首语|扉页|楔子|正文(?!完|结)|终章|后记|尾声|番外|[〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8})[ 、_—\-].{1,30}$",
        enable: true,
        serial_number: 6,
    },
    DefaultTocRuleDef {
        name: "正文 标题/序号",
        rule: r"^[ 　\t]{0,4}正文[ 　]{1,4}.{0,20}$",
        enable: true,
        serial_number: 7,
    },
    DefaultTocRuleDef {
        name: "Chapter/Section/Part/Episode 序号 标题",
        rule: r"^[ 　\t]{0,4}(?:[Cc]hapter|[Ss]ection|[Pp]art|ＰＡＲＴ|[Nn][oO]\.|[Ee]pisode|(?:内容|文章)?简介|文案|前言|序章|楔子|正文(?!完|结)|终章|后记|尾声|番外)\s{0,4}\d{1,4}.{0,30}$",
        enable: true,
        serial_number: 8,
    },
    DefaultTocRuleDef {
        name: "Chapter(去简介)",
        rule: r"^[ 　\t]{0,4}(?:[Cc]hapter|[Ss]ection|[Pp]art|ＰＡＲＴ|[Nn][Oo]\.|[Ee]pisode)\s{0,4}\d{1,4}.{0,30}$",
        enable: false,
        serial_number: 9,
    },
    DefaultTocRuleDef {
        name: "特殊符号 序号 标题",
        rule: r"(?<=[\s　])[【〔〖「『〈［\[](?:第|[Cc]hapter)[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,10}[章节].{0,20}$",
        enable: true,
        serial_number: 10,
    },
    DefaultTocRuleDef {
        name: "特殊符号 标题(成对)",
        rule: r"(?<=[\s　]{0,4})(?:[\[〈「『〖〔《（【\(].{1,30}[\）】）》〕〗』」〉\]]?|(?:内容|文章)?简介|文案|前言|序章|楔子|正文(?!完|结)|终章|后记|尾声|番外)[ 　]{0,4}$",
        enable: false,
        serial_number: 11,
    },
    DefaultTocRuleDef {
        name: "特殊符号 标题(单个)",
        rule: r"(?<=[\s　]{0,4})(?:[☆★✦✧].{1,30}|(?:内容|文章)?简介|文案|前言|序章|楔子|正文(?!完|结)|终章|后记|尾声|番外)[ 　]{0,4}$",
        enable: true,
        serial_number: 12,
    },
    DefaultTocRuleDef {
        name: "章/卷 序号 标题",
        rule: r"^[ \t　]{0,4}(?:(?:内容|文章)?简介|文案|前言|序章|序言|卷首语|扉页|楔子|正文(?!完|结)|终章|后记|尾声|番外|[卷章][\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8})[ 　]{0,4}.{0,30}$",
        enable: true,
        serial_number: 13,
    },
    DefaultTocRuleDef {
        name: "顶格标题",
        rule: r"^\S.{1,20}$",
        enable: false,
        serial_number: 14,
    },
    DefaultTocRuleDef {
        name: "双标题(前向)",
        rule: r"(?m)(?<=[ \t　]{0,4})第[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章.{0,30}$(?=[\s　]{0,8}第[\d零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章)",
        enable: false,
        serial_number: 15,
    },
    DefaultTocRuleDef {
        name: "双标题(后向)",
        rule: r"(?m)(?<=[ \t　]{0,4}第[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章.{0,30}$[\s　]{0,8})第[\d零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章.{0,30}$",
        enable: false,
        serial_number: 16,
    },
    DefaultTocRuleDef {
        name: "标题 特殊符号 序号",
        rule: r"^.{1,20}[(（][\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}[)）][ 　\t]{0,4}$",
        enable: true,
        serial_number: 17,
    },
];

/// 参与 TXT 分章的默认规则正则（legacy TextFile.getTocRules 只取启用规则）
pub fn default_toc_rule_regexes() -> Vec<String> {
    DEFAULT_TOC_RULE_DEFS
        .iter()
        .filter(|d| d.enable)
        .map(|d| d.rule.to_string())
        .collect()
}

/// legacy TextFile 长章节拆分（book.getSplitLongChapter() 开启时）：
/// 正则分章后单章字节长 > maxLengthWithToc(100KB) → 按 ~maxLengthWithNoToc(10KB)
/// 在换行边界二次切分，子章标题 "{原标题}(n)"（legacy TextFile:160）。
fn split_long_chapters(chapters: Vec<Chapter>, enabled: bool) -> Vec<Chapter> {
    // legacy TextFile.kt 常量
    const MAX_WITH_TOC: usize = 100 * 1024;
    const CHUNK: usize = 10 * 1024;
    if !enabled {
        return chapters;
    }
    let mut out = Vec::with_capacity(chapters.len());
    for c in chapters {
        if c.content.len() <= MAX_WITH_TOC {
            out.push(c);
            continue;
        }
        let bytes = c.content.as_bytes();
        let mut start = 0usize;
        let mut idx = 0usize;
        while start < bytes.len() {
            idx += 1;
            let mut end = (start + CHUNK).min(bytes.len());
            if end < bytes.len() {
                // legacy：从 start+maxLen 起向后找第一个换行符作为终止（块 ≥ CHUNK；
                // 剩余文本无换行则硬切到末尾）
                match bytes[end..].iter().position(|&b| b == b'\n') {
                    Some(p) => end += p,
                    None => end = bytes.len(),
                }
            }
            // 防止切断 UTF-8 多字节字符（仅未到文本末尾时需要回退）
            while end < bytes.len() && end > start && (bytes[end] & 0xC0) == 0x80 {
                end -= 1;
            }
            if end <= start {
                end = bytes.len();
            }
            out.push(Chapter {
                title: format!("{}({})", c.title, idx),
                content: String::from_utf8_lossy(&bytes[start..end]).into_owned(),
            });
            start = end;
        }
    }
    out
}

/// 用规则列表分章（txtTocRule 语义——正则匹配行作为章节标题）
/// 规则按 legado TextFile 语义：先选「命中数最多」的单一规则（原序靠前优先），
/// 再以该规则分割（legacy getTocRule：倒序遍历 + cs >= maxCs → 即原序最前且命中最多）。
/// 规则以 MULTILINE 编译（`^`/`$` 按行锚定，规则匹配整行章节标题）
fn split_by_rules(text: &str, rules: &[String]) -> Vec<Chapter> {
    // 单规则选择：命中数最多者胜；相等时保留更早的规则（对齐 legacy）
    let mut best: Option<crate::util::regex::Regex> = None;
    let mut best_count = 0usize;
    for rule in rules {
        // GAP 153：TXT 目录规则经兼容层编译（lookbehind 自动升级 fancy-regex）
        match crate::util::regex::RegexBuilder::new(rule)
            .multi_line(true)
            .build()
        {
            Ok(re) => {
                let count = re.captures_iter(text).count();
                if count > 0 && count > best_count {
                    best_count = count;
                    best = Some(re);
                }
            }
            Err(e) => tracing::warn!("TXT 目录规则编译失败（忽略该规则）: {e}"),
        }
    }
    let Some(re) = best else {
        // 无任何规则命中 → 返回空（调用方回退：长文本按字数分块，短文本整本一章）
        return Vec::new();
    };
    let mut chapters = Vec::new();
    let mut last_pos = 0usize;
    let mut last_title = "正文".to_string();
    let mut kept: Vec<(usize, usize, String)> = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(0) {
            let title = m.as_str().trim().to_string();
            if !title.is_empty() && m.start() >= last_pos {
                kept.push((m.start(), m.end(), title));
                last_pos = m.end();
            }
        }
    }
    last_pos = 0;
    for (start, end, title) in kept {
        let content = text[last_pos..start].trim().to_string();
        if !content.is_empty() {
            chapters.push(Chapter {
                title: last_title.clone(),
                content,
            });
        }
        last_title = title;
        last_pos = end;
    }
    let tail = text[last_pos..].trim().to_string();
    if !tail.is_empty() {
        chapters.push(Chapter {
            title: last_title,
            content: tail,
        });
    }
    chapters
}

/// 读 TXT 文件并分章（legacy 本地书：bookUrl = storage/data/.../xx.txt）
pub fn parse_txt_file(path: &std::path::Path) -> Result<ImportedBook> {
    let bytes = std::fs::read(path)?;
    parse_txt(&bytes)
}

/// 读 TXT 文件并分章（用户自定义规则版本）
pub fn parse_txt_file_with_rules(
    path: &std::path::Path,
    user_rules: &[String],
) -> Result<ImportedBook> {
    let bytes = std::fs::read(path)?;
    parse_txt_with_rules(&bytes, user_rules)
}

// ---------- 通用分章 ----------

/// 无章节标记回退：长文本按 10000 字分块（避免单章过大渲染卡顿），短文本整本一章
fn chunk_fallback(text: &str) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    if text.trim().is_empty() {
        return chapters;
    }
    const CHUNK: usize = 10_000;
    let body = text.trim().to_string();
    if body.chars().count() > CHUNK * 2 {
        let mut start = 0usize;
        let chars: Vec<char> = body.chars().collect();
        let mut part = 1;
        while start < chars.len() {
            let end = (start + CHUNK).min(chars.len());
            let chunk: String = chars[start..end].iter().collect();
            chapters.push(Chapter {
                title: format!("第 {part} 部分"),
                content: chunk,
            });
            start = end;
            part += 1;
        }
    } else {
        chapters.push(Chapter {
            title: "正文".into(),
            content: body,
        });
    }
    chapters
}

/// 纯文本分章（内置默认规则；无匹配时回退 chunk_fallback）
fn chapters_from_plain_text(text: &str) -> Vec<Chapter> {
    let rules = default_toc_rule_regexes();
    let chapters = split_by_rules(text, &rules);
    if chapters.is_empty() {
        chunk_fallback(text)
    } else {
        chapters
    }
}

// ---------- MOBI / AZW3 ----------

/// P1-C3：MOBI/AZW3 解压前长度校验（Huffman 炸弹防护）——PalmDoc 头声称的未压缩正文长度
/// （text_length / record_count×record_size）与 CDIC 词典短语数超上限即拒绝，避免进入外部
/// mobi crate 的无限分配/解压路径。文件结构不完整/不可解析时静默放行（交由 mobi crate
/// 报其友好错误）。
fn validate_mobi_lengths(bytes: &[u8]) -> Result<()> {
    // PalmDB header：78B；记录数 u16 BE @76；记录表 8B/条 @78
    if bytes.len() < 78 {
        return Ok(());
    }
    let num_records = u16::from_be_bytes([bytes[76], bytes[77]]) as usize;
    if num_records == 0 {
        return Ok(());
    }
    let rec_list_end = 78usize.saturating_add(num_records.saturating_mul(8));
    if rec_list_end > bytes.len() {
        return Ok(()); // 记录表越界：交给 mobi crate 报错
    }
    let rec0_off = u32::from_be_bytes([bytes[78], bytes[79], bytes[80], bytes[81]]) as usize;
    // 记录 0 = PalmDocHeader（16B）：compression u16、unused u16、text_length u32 @+4、
    // record_count u16 @+8、record_size u16 @+10
    if rec0_off + 16 > bytes.len() {
        return Ok(());
    }
    let compression = u16::from_be_bytes([bytes[rec0_off], bytes[rec0_off + 1]]);
    let text_length = u32::from_be_bytes([
        bytes[rec0_off + 4],
        bytes[rec0_off + 5],
        bytes[rec0_off + 6],
        bytes[rec0_off + 7],
    ]) as u64;
    let record_count = u16::from_be_bytes([bytes[rec0_off + 8], bytes[rec0_off + 9]]) as u64;
    let record_size = u16::from_be_bytes([bytes[rec0_off + 10], bytes[rec0_off + 11]]) as u64;
    if text_length > MAX_MOBI_TEXT_BYTES {
        anyhow::bail!(
            "MOBI 声称正文 {text_length} 字节超出上限（{}MB），已拒绝",
            MAX_MOBI_TEXT_BYTES / 1024 / 1024
        );
    }
    if record_count.saturating_mul(record_size) > MAX_MOBI_TEXT_BYTES {
        anyhow::bail!(
            "MOBI 记录容量 {record_count}×{record_size} 超出上限（{}MB），已拒绝",
            MAX_MOBI_TEXT_BYTES / 1024 / 1024
        );
    }
    if compression != 2 {
        return Ok(()); // 非 Huffman：无需 CDIC 词典校验
    }
    // MOBI header @ rec0+16："MOBI" + header_length；first_huff_record @+96（0x60）、
    // huff_record_count @+100（0x64）
    if rec0_off + 16 + 116 > bytes.len() {
        return Ok(());
    }
    if &bytes[rec0_off + 16..rec0_off + 20] != b"MOBI" {
        return Ok(());
    }
    let first_huff = u32::from_be_bytes([
        bytes[rec0_off + 16 + 96],
        bytes[rec0_off + 16 + 97],
        bytes[rec0_off + 16 + 98],
        bytes[rec0_off + 16 + 99],
    ]) as usize;
    let huff_count = u32::from_be_bytes([
        bytes[rec0_off + 16 + 100],
        bytes[rec0_off + 16 + 101],
        bytes[rec0_off + 16 + 102],
        bytes[rec0_off + 16 + 103],
    ]) as usize;
    // CDIC 记录：first_huff 之后各条（第一条是 HUFF 字典，其后为 CDIC）；
    // 与 mobi crate 相同口径累计短语数（n = min(1<<bits, num_phrases - 已累计)）
    let mut dict_len: u64 = 0;
    for i in 1..huff_count {
        let rec_idx = first_huff + i;
        if rec_idx >= num_records {
            break;
        }
        let entry = 78 + rec_idx * 8;
        if entry + 4 > bytes.len() {
            break;
        }
        let off = u32::from_be_bytes([
            bytes[entry],
            bytes[entry + 1],
            bytes[entry + 2],
            bytes[entry + 3],
        ]) as usize;
        if off + 16 > bytes.len() {
            break;
        }
        if &bytes[off..off + 4] != b"CDIC" {
            continue;
        }
        let num_phrases = u32::from_be_bytes([
            bytes[off + 8],
            bytes[off + 9],
            bytes[off + 10],
            bytes[off + 11],
        ]) as u64;
        let bits = u32::from_be_bytes([
            bytes[off + 12],
            bytes[off + 13],
            bytes[off + 14],
            bytes[off + 15],
        ]) as u64;
        let n = if bits >= 64 {
            num_phrases
        } else {
            (1u64 << bits).min(num_phrases)
        };
        dict_len = dict_len.saturating_add(n);
        if dict_len > MAX_MOBI_CDIC_PHRASES {
            anyhow::bail!(
                "MOBI Huffman 词典短语数超上限（{} 条），已拒绝",
                MAX_MOBI_CDIC_PHRASES
            );
        }
    }
    Ok(())
}

/// PalmDoc LZ77 解压（MOBI 压缩方式 2）。
///
/// mobi crate 仅对外暴露已按 UTF-8/WIN1252 转码的字符串；中文 MOBI 常把
/// GBK/GB18030 内容标成未知编码，`content_as_string_lossy` 会按 UTF-8 宽松
/// 解码产生乱码。这里自行解压原始字节，再交给 `decode_bytes` 做统计式探测。
fn palmdoc_decompress(data: &[u8]) -> Vec<u8> {
    let mut pos = 0usize;
    let mut text: Vec<u8> = Vec::new();
    let mut prev: Option<u8> = None;
    while pos < data.len() {
        let byte = data[pos];
        pos += 1;
        if let Some(old) = prev.take() {
            // 高两位为 ID，低 14 位为 offset(11) + length(3)。
            let dist_len = u16::from_be_bytes([old, byte]) & 0x3fff;
            let offset = (dist_len >> 3) as usize;
            let len = ((dist_len & 0x0007) + 3) as usize;
            let start = if offset > text.len() {
                offset % text.len().max(1)
            } else {
                text.len() - offset
            };
            // offset==0 时 start==text.len()，无源可拷（与原实现的 break 一致）；
            // 其余情况按 LZ77 语义逐字节复制，允许 start+len 越过当前长度（边拷边扩）
            if start < text.len() {
                for i in start..start + len {
                    text.push(text[i]);
                }
            }
            continue;
        }
        match byte {
            0x0 | 0x09..=0x7f => text.push(byte),
            0x1..=0x8 => {
                let n = byte as usize;
                if pos + n <= data.len() {
                    text.extend_from_slice(&data[pos..pos + n]);
                    pos += n;
                }
            }
            0x80..=0xbf => prev = Some(byte),
            _ => {
                text.push(b' ');
                text.push(byte ^ 0x80);
            }
        }
    }
    text
}

/// 只读字节游标（Huffman 解压用，语义同 mobi crate 的 Reader：仅前向读取）。
struct ByteCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn read_u32(&mut self) -> Option<u32> {
        let s = self.data.get(self.pos..self.pos.checked_add(4)?)?;
        self.pos += 4;
        Some(u32::from_be_bytes(s.try_into().ok()?))
    }

    fn read_u64(&mut self) -> Option<u64> {
        let s = self.data.get(self.pos..self.pos.checked_add(8)?)?;
        self.pos += 8;
        Some(u64::from_be_bytes(s.try_into().ok()?))
    }
}

struct MobiHuffDecoder {
    dictionary: Vec<Option<(Vec<u8>, bool)>>,
    code_dict: [(u8, bool, u32); 256],
    min_codes: [u32; 33],
    max_codes: [u32; 33],
}

impl Default for MobiHuffDecoder {
    fn default() -> Self {
        Self {
            dictionary: Vec::new(),
            code_dict: [(0, false, 0); 256],
            min_codes: [0; 33],
            max_codes: [u32::MAX; 33],
        }
    }
}

impl MobiHuffDecoder {
    fn load_code_dictionary(&mut self, data: &[u8], offset: usize) -> Result<(), String> {
        let mut cur = ByteCursor::new(data);
        cur.pos = offset;
        for code in self.code_dict.iter_mut() {
            let v = cur
                .read_u32()
                .ok_or_else(|| "HUFF 码表读取失败".to_string())?;
            let (code_len, term, mut max_code) = ((v & 0x1F) as u8, (v & 0x80) == 0x80, v >> 8);
            if code_len == 0 {
                return Err("HUFF 码长越界".to_string());
            }
            if code_len <= 8 && !term {
                return Err("HUFF 终止码非法".to_string());
            }
            max_code =
                ((max_code + 1) << (32u32.saturating_sub(code_len as u32))).saturating_sub(1);
            *code = (code_len, term, max_code);
        }
        Ok(())
    }

    fn load_min_max_codes(&mut self, data: &[u8], offset: usize) -> Result<(), String> {
        let mut cur = ByteCursor::new(data);
        cur.pos = offset;
        for code_len in 1..=32usize {
            let v = cur
                .read_u32()
                .ok_or_else(|| "HUFF 最小码读取失败".to_string())?;
            self.min_codes[code_len] = v << (32 - code_len);
            let v = cur
                .read_u32()
                .ok_or_else(|| "HUFF 最大码读取失败".to_string())?;
            self.max_codes[code_len] = ((v + 1) << (32 - code_len)).saturating_sub(1);
        }
        Ok(())
    }

    fn load_huff(&mut self, huff: &[u8]) -> Result<(), String> {
        let mut cur = ByteCursor::new(huff);
        let magic = cur.read_u32().ok_or("HUFF 头读取失败")?;
        let header_len = cur.read_u32().ok_or("HUFF 头长度读取失败")?;
        if magic.to_be_bytes() != *b"HUFF" || header_len != 0x18 {
            return Err("HUFF 头非法".to_string());
        }
        let cache_offset = cur.read_u32().ok_or("HUFF cache 偏移读取失败")? as usize;
        let base_offset = cur.read_u32().ok_or("HUFF base 偏移读取失败")? as usize;
        self.load_code_dictionary(huff, cache_offset)?;
        self.load_min_max_codes(huff, base_offset)
    }

    fn load_cdic_record(&mut self, cdic: &[u8]) -> Result<(), String> {
        let mut cur = ByteCursor::new(cdic);
        let magic = cur.read_u32().ok_or("CDIC 头读取失败")?;
        let header_len = cur.read_u32().ok_or("CDIC 头长度读取失败")?;
        if magic.to_be_bytes() != *b"CDIC" || header_len != 0x10 {
            return Err("CDIC 头非法".to_string());
        }
        let num_phrases = cur.read_u32().ok_or("CDIC 短语数读取失败")?;
        let bits = cur.read_u32().ok_or("CDIC 位数读取失败")?;
        let n = (1u32 << bits).min(num_phrases - self.dictionary.len() as u32);
        let mut offsets = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let s = cdic.get(cur.pos..cur.pos + 2).ok_or("CDIC 偏移读取失败")?;
            cur.pos += 2;
            offsets.push(u16::from_be_bytes([s[0], s[1]]));
        }
        for offset in offsets {
            let phrase = cdic.get(16 + offset as usize..).ok_or("CDIC 短语越界")?;
            if phrase.len() < 2 {
                return Err("CDIC 短语头越界".to_string());
            }
            let num_bytes = u16::from_be_bytes([phrase[0], phrase[1]]);
            let len = (num_bytes & 0x7FFF) as usize;
            let bytes = phrase.get(2..2 + len).ok_or("CDIC 短语内容越界")?.to_vec();
            self.dictionary
                .push(Some((bytes, (num_bytes & 0x8000) == 0x8000)));
        }
        Ok(())
    }

    fn unpack(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut bits_left = data.len() * 8;
        let mut cur = ByteCursor::new(data);
        let mut x = cur.read_u64().ok_or("HUFF 位流不足")?;
        let mut n = 32i8;
        let mut unpacked: Vec<u8> = Vec::new();
        loop {
            if n <= 0 {
                if bits_left < 32 {
                    for _ in 0..bits_left / 8 {
                        x = (x << 8) | u64::from(cur.read_u8().ok_or("HUFF 位流不足")?);
                    }
                    x <<= 32 - bits_left;
                } else {
                    x = (x << 32) | u64::from(cur.read_u32().ok_or("HUFF 位流不足")?);
                }
                n += 32;
            }
            let code = (x >> n) as u32;
            let (mut code_len, term, mut max_code) = self.code_dict[(code >> 24) as usize];
            if !term {
                code_len += self.min_codes[code_len as usize..]
                    .iter()
                    .position(|&min_code| code >= min_code)
                    .ok_or("HUFF 最小码未命中")? as u8;
                max_code = self.max_codes[code_len as usize];
            }
            let index = ((max_code - code) >> (32 - code_len as usize)) as usize;
            let (mut slice, flag) = self
                .dictionary
                .get_mut(index)
                .ok_or("HUFF 词典索引越界")?
                .take()
                .ok_or("HUFF 词典项缺失")?;
            if !flag {
                slice = self.unpack(&slice)?;
            }
            unpacked.extend_from_slice(&slice);
            self.dictionary[index] = Some((slice, true));
            n -= code_len as i8;
            bits_left = match bits_left.checked_sub(code_len as usize) {
                None | Some(0) => break,
                Some(i) => i,
            };
        }
        Ok(unpacked)
    }
}

/// MOBI Huffman 解压（压缩方式 17480）。
fn mobi_huff_decompress(huffs: &[&[u8]], sections: &[&[u8]]) -> Result<Vec<Vec<u8>>, String> {
    if huffs.is_empty() {
        return Err("HUFF 记录缺失".to_string());
    }
    let mut decoder = MobiHuffDecoder::default();
    decoder.load_huff(huffs[0])?;
    for cdic in &huffs[1..] {
        decoder.load_cdic_record(cdic)?;
    }
    sections.iter().map(|s| decoder.unpack(s)).collect()
}

/// MOBI 头 0xF2 处的 trailing/multibyte flags（仅 MOBI 头长度 >= 0xE4 且版本 >= 5 时有效）。
/// 与 KindleUnpack 的 `getRawML` 同口径：bit0 = multibyte overlap，bit1 及更高位每出现一次
/// 表示记录尾部附加了一段 4 字节的 trailing entry。
fn mobi_trailing_flags(rec0: &[u8]) -> u16 {
    const HEADER_LEN_OFF: usize = 0x14;
    const VERSION_OFF: usize = 0x68;
    const TRAILING_FLAGS_OFF: usize = 0xF2;
    if rec0.len() < TRAILING_FLAGS_OFF + 2 || rec0.len() < VERSION_OFF + 4 {
        return 0;
    }
    let header_len = u32::from_be_bytes([
        rec0[HEADER_LEN_OFF],
        rec0[HEADER_LEN_OFF + 1],
        rec0[HEADER_LEN_OFF + 2],
        rec0[HEADER_LEN_OFF + 3],
    ]);
    let version = u32::from_be_bytes([
        rec0[VERSION_OFF],
        rec0[VERSION_OFF + 1],
        rec0[VERSION_OFF + 2],
        rec0[VERSION_OFF + 3],
    ]);
    if header_len < 0xE4 || version < 5 {
        return 0;
    }
    u16::from_be_bytes([rec0[TRAILING_FLAGS_OFF], rec0[TRAILING_FLAGS_OFF + 1]])
}

/// 去掉 KindleMOBI 文本记录尾部的 trailing entries 与 multibyte overlap。
/// 不处理就解压时会把附加数据当成 PalmDoc 指令，导致 4KB 边界后乱码。
fn mobi_strip_trailing_data(data: &[u8], flags: u16) -> &[u8] {
    let multibyte = flags & 1 != 0;
    let mut trailing_entries = 0usize;
    let mut f = flags;
    while f > 1 {
        if f & 2 != 0 {
            trailing_entries += 1;
        }
        f >>= 1;
    }
    let mut end = data.len();
    for _ in 0..trailing_entries {
        if end < 4 {
            return &data[..end];
        }
        let tail = &data[end - 4..end];
        let mut n = 0usize;
        for &b in tail {
            if b & 0x80 != 0 {
                n = 0;
            }
            n = n.saturating_mul(128).saturating_add((b & 0x7F) as usize);
        }
        end = end.saturating_sub(n);
    }
    if multibyte && end > 0 {
        let n = (data[end - 1] & 3) as usize + 1;
        end = end.saturating_sub(n);
    }
    &data[..end]
}

/// 从原始 PDB 字节读取记录偏移表（mobi crate 的 RawRecords 不暴露原始记录长度，
/// 这里自行切片以支持 trailing data 清理）。
fn mobi_record_offsets(content: &[u8]) -> Result<Vec<usize>> {
    if content.len() < 78 {
        anyhow::bail!("MOBI PalmDB 头缺失");
    }
    let num_records = u16::from_be_bytes([content[76], content[77]]) as usize;
    if num_records == 0 {
        anyhow::bail!("MOBI 记录数为 0");
    }
    let table_end = 78usize
        .checked_add(num_records.saturating_mul(8))
        .ok_or_else(|| anyhow::anyhow!("MOBI 记录表长度溢出"))?;
    if table_end > content.len() {
        anyhow::bail!("MOBI 记录表越界");
    }
    let mut offsets = Vec::with_capacity(num_records);
    for i in 0..num_records {
        let p = 78 + i * 8;
        offsets.push(u32::from_be_bytes([
            content[p],
            content[p + 1],
            content[p + 2],
            content[p + 3],
        ]) as usize);
    }
    Ok(offsets)
}

/// 按 PDB 记录偏移切出某条记录的原始内容（含尾部附加数据，供调用方决定是否清理）。
fn mobi_raw_record<'a>(content: &'a [u8], offsets: &[usize], index: usize) -> Result<&'a [u8]> {
    let start = *offsets
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("MOBI 记录 {index} 不存在"))?;
    let end = offsets.get(index + 1).copied().unwrap_or(content.len());
    content
        .get(start..end.min(content.len()))
        .ok_or_else(|| anyhow::anyhow!("MOBI 记录 {index} 内容越界"))
}

/// 提取 MOBI 可读正文的原始字节（不转码），供编码探测使用。
/// `original` 必须是完整 PDB 文件字节；mobi crate 的 `content` 只保留零填充头 + 原始记录区。
fn mobi_raw_content(book: &mobi::Mobi, original: &[u8]) -> Result<Vec<u8>> {
    use mobi::headers::Compression;
    let content = original;
    let offsets = mobi_record_offsets(content)?;
    let rec0 = mobi_raw_record(content, &offsets, 0)?;
    let trailing_flags = mobi_trailing_flags(rec0);
    let range = book.readable_records_range();
    let range_start = range.start.min(offsets.len());
    let range_end = range.end.min(offsets.len().saturating_sub(1));
    if range_start >= range_end {
        anyhow::bail!("MOBI 可读文本记录范围非法: {range_start}..{range_end}");
    }
    let mut out = Vec::new();
    match book.compression() {
        Compression::PalmDoc => {
            for index in range_start..range_end {
                let record = mobi_raw_record(content, &offsets, index)?;
                let part = palmdoc_decompress(mobi_strip_trailing_data(record, trailing_flags));
                if out.len().saturating_add(part.len()) as u64 > MAX_MOBI_TEXT_BYTES {
                    anyhow::bail!(
                        "MOBI 解压正文超出上限（{}MB）",
                        MAX_MOBI_TEXT_BYTES / 1024 / 1024
                    );
                }
                out.extend_from_slice(&part);
            }
        }
        Compression::No => {
            for index in range_start..range_end {
                let record = mobi_raw_record(content, &offsets, index)?;
                let record = mobi_strip_trailing_data(record, trailing_flags);
                if out.len().saturating_add(record.len()) as u64 > MAX_MOBI_TEXT_BYTES {
                    anyhow::bail!(
                        "MOBI 正文超出上限（{}MB）",
                        MAX_MOBI_TEXT_BYTES / 1024 / 1024
                    );
                }
                out.extend_from_slice(record);
            }
        }
        Compression::Huff => {
            let huff_start = book.metadata.mobi.first_huff_record as usize;
            let huff_count = book.metadata.mobi.huff_record_count as usize;
            let end = huff_start.saturating_add(huff_count).min(offsets.len());
            if huff_start >= end {
                anyhow::bail!("MOBI Huffman 记录范围非法");
            }
            let mut huffs = Vec::with_capacity(end - huff_start);
            for index in huff_start..end {
                huffs.push(mobi_raw_record(content, &offsets, index)?);
            }
            let mut sections = Vec::with_capacity(range_end - range_start);
            for index in range_start..range_end {
                let record = mobi_raw_record(content, &offsets, index)?;
                sections.push(mobi_strip_trailing_data(record, trailing_flags));
            }
            let parts = mobi_huff_decompress(&huffs, &sections)
                .map_err(|e| anyhow::anyhow!("MOBI Huffman 解压失败: {e}"))?;
            for part in parts {
                if out.len().saturating_add(part.len()) as u64 > MAX_MOBI_TEXT_BYTES {
                    anyhow::bail!(
                        "MOBI 解压正文超出上限（{}MB）",
                        MAX_MOBI_TEXT_BYTES / 1024 / 1024
                    );
                }
                out.extend_from_slice(&part);
            }
        }
    }
    Ok(out)
}

/// MOBI（mobi7）解析：PalmDB header → 记录表 → 解压（Palmdoc/Huff/无压缩）→ rawml HTML → 纯文本分章
pub fn parse_mobi(bytes: &[u8]) -> Result<ImportedBook> {
    parse_mobi_impl(bytes, "mobi")
}

/// AZW3（KF8）解析：先走 mobi 兼容层（部分 azw3 携带 mobi7 回退正文；纯 KF8 结构返回友好错误）
pub fn parse_azw3(bytes: &[u8]) -> Result<ImportedBook> {
    parse_mobi_impl(bytes, "azw3")
}

fn parse_mobi_impl(bytes: &[u8], format: &str) -> Result<ImportedBook> {
    // P1-C3：解压前长度校验（Huffman 炸弹防护）
    validate_mobi_lengths(bytes)?;
    let book = mobi::Mobi::new(bytes.to_vec())
        .context("MOBI/AZW3 解析失败（不是有效的 PalmDB/MOBI 文件，或 KF8 加密暂不支持）")?;
    let raw_bytes = mobi_raw_content(&book, bytes)?;
    if raw_bytes.iter().all(|b| b.is_ascii_whitespace()) {
        anyhow::bail!("MOBI 未包含可读文本（可能已加密）");
    }
    // mobi7 正文是 rawml HTML（<mbp:pagebreak/> 分隔章节）——转纯文本再分章。
    // 编码探测：中文 MOBI 常把 GBK/GB18030 标成未知编码，mobi crate 会按
    // UTF-8 lossy 产生乱码；这里先经 chardetng 统计识别再转 HTML 纯文本。
    let raw = crate::service::crawler::decode_bytes(&raw_bytes, None);
    let text = html_to_text(&raw);
    let mut chapters = chapters_from_plain_text(&text);
    if chapters.is_empty() && !text.trim().is_empty() {
        chapters.push(Chapter {
            title: "正文".into(),
            content: text.trim().to_string(),
        });
    }
    let mut meta = OpfMeta {
        title: book.title(),
        author: book.author().unwrap_or_default(),
        ..Default::default()
    };
    if let Some(d) = book.description() {
        meta.description = Some(d);
    }
    if let Some(p) = book.publisher() {
        meta.publisher = Some(p);
    }
    meta.language = Some(format!("{:?}", book.language()));
    // 封面：首个图片记录（MOBI 约定 record 0 为封面）
    let cover = book
        .image_records()
        .into_iter()
        .next()
        .map(|r| r.content.to_vec());
    Ok(ImportedBook {
        meta,
        chapters,
        cover,
        format: format.into(),
    })
}

// ---------- PDF ----------

/// 大 PDF 防卡：最多提取前 300 页
pub const PDF_MAX_PAGES: usize = 300;

/// PDF 分章规则：默认规则去掉“数字+空格+标题”（PDF 页码行易误匹配）
pub const PDF_TOC_RULES: &[&str] = &[
    r"^\s*第\s*[0-9一二三四五六七八九十百千万零〇两]+\s*[章节卷回集部篇][^\n]{0,40}[ \t]*$",
    r"^\s*第\s*[0-9一二三四五六七八九十百千万零〇两]+\s*卷[^\n]{0,40}[ \t]*$",
    r"^\s*(序章|楔子|番外|后记|尾声|前言|引子|正文|终章)[^\n]{0,40}[ \t]*$",
    r"^\s*[Cc][Hh][Aa][Pp][Tt][Ee][Rr]\s+\d+[^\n]{0,40}[ \t]*$",
];

/// PDF 解析：lopdf 按页提取文本（每页解压上限 8MB 防炸弹）→ 标题分章或页分章
pub fn parse_pdf(bytes: &[u8]) -> Result<ImportedBook> {
    let doc = lopdf::Document::load_mem(bytes).context("PDF 解析失败（文件损坏、加密或非 PDF）")?;
    let total_pages = doc.get_pages().len();
    if total_pages == 0 {
        anyhow::bail!("PDF 没有页面");
    }
    let limit = total_pages.min(PDF_MAX_PAGES);
    let mut pages = Vec::with_capacity(limit);
    for num in 1..=limit {
        match doc.extract_text_with_limit(&[num as u32], 8 * 1024 * 1024) {
            Ok(t) => pages.push(t),
            Err(e) => {
                tracing::warn!("PDF 第 {num} 页文本提取失败：{e}");
                pages.push(String::new());
            }
        }
    }
    if pages.iter().all(|p| p.trim().is_empty()) {
        anyhow::bail!("PDF 未提取到文本（扫描版/图片型 PDF 暂不支持 OCR）");
    }
    // 元数据（Info 字典）
    let mut meta = OpfMeta::default();
    if let Ok(info_id) = doc
        .trailer
        .get(b"Info")
        .map(|o| o.as_reference())
        .unwrap_or(Err(lopdf::Error::ObjectType {
            expected: "reference",
            found: "none",
        }))
    {
        if let Ok(info) = doc.get_dictionary(info_id) {
            meta.title = pdf_meta_string(info.get(b"Title"));
            meta.author = pdf_meta_string(info.get(b"Author"));
            meta.description = Some(pdf_meta_string(
                info.get(b"Subject").or_else(|_| info.get(b"Keywords")),
            ));
        }
    }
    let chapters = chapters_from_pages(pages);
    Ok(ImportedBook {
        meta,
        chapters,
        cover: None,
        format: "pdf".into(),
    })
}

/// PDF 元数据字符串解码（PDFDocEncoding/UTF-16BE/UTF-8；lopdf 0.44 Dictionary::get 返回 Result）
fn pdf_meta_string(v: Result<&lopdf::Object, lopdf::Error>) -> String {
    v.ok()
        .and_then(|o| lopdf::decode_text_string(o).ok())
        .map(|s| s.trim().trim_start_matches('\u{feff}').trim().to_string())
        .unwrap_or_default()
}

/// PDF 分章：优先按章节标题规则（跨页全文匹配）；无标题 → 按页分章（每页一章）
fn chapters_from_pages(pages: Vec<String>) -> Vec<Chapter> {
    let rules: Vec<String> = PDF_TOC_RULES.iter().map(|s| s.to_string()).collect();
    let joined = pages.join("\n\n");
    let by_rules = split_by_rules(&joined, &rules);
    if !by_rules.is_empty() {
        return by_rules;
    }
    let mut chapters = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let t = page.trim();
        if !t.is_empty() {
            chapters.push(Chapter {
                title: format!("第 {} 页", i + 1),
                content: t.to_string(),
            });
        }
    }
    if chapters.is_empty() {
        return chunk_fallback(&joined);
    }
    chapters
}

// ---------- FB2 ----------

/// FB2 解析：quick-xml 提取 description（书名/作者/简介）+ 第一个 body 的 section 分章
pub fn parse_fb2(bytes: &[u8]) -> Result<ImportedBook> {
    let xml = String::from_utf8_lossy(bytes);
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut buf = Vec::new();

    let mut meta = OpfMeta::default();
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut cur: Option<(String, String)> = None; // 当前 section 的 (title, content)
    let mut body_count = 0usize;
    let mut in_main_body = false;
    let mut section_depth = 0usize;
    let mut in_title = false;
    let mut in_para = false;
    let mut para_break = false; // 段落之间插换行
    let mut in_book_title = false;
    let mut in_annotation = false;
    let mut in_author_field = false;
    let mut author_parts: Vec<String> = Vec::new();

    // 段落文本入缓冲（段落之间插换行；同一段落内多个文本片断保留原始空白直接拼接）
    fn push_para(dst: &mut String, s: &str, break_before: &mut bool) {
        if s.trim().is_empty() {
            return;
        }
        if *break_before && !dst.is_empty() && !dst.ends_with('\n') {
            dst.push('\n');
        }
        dst.push_str(s);
        *break_before = false;
    }
    macro_rules! flush_section {
        () => {
            if let Some((title, content)) = cur.take() {
                let content = content.trim().to_string();
                if !content.is_empty() || !title.trim().is_empty() {
                    let title = if title.trim().is_empty() {
                        format!("第 {} 节", chapters.len() + 1)
                    } else {
                        title.split_whitespace().collect::<Vec<_>>().join(" ")
                    };
                    chapters.push(Chapter { title, content });
                }
            }
        };
    }

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                match std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") {
                    "body" => {
                        if body_count == 0 {
                            in_main_body = true;
                        }
                        body_count += 1;
                    }
                    "section" => {
                        if in_main_body {
                            if section_depth == 0 {
                                flush_section!();
                                cur = Some((String::new(), String::new()));
                            }
                            section_depth += 1;
                        }
                    }
                    "title" => {
                        if in_main_body && section_depth > 0 {
                            in_title = true;
                        }
                    }
                    "p" | "subtitle" | "cite" | "poem" | "stanza" | "epigraph" | "text-author" => {
                        if in_main_body && section_depth > 0 {
                            in_para = true;
                            para_break = true;
                        }
                    }
                    "book-title" => in_book_title = true,
                    "annotation" => in_annotation = true,
                    "first-name" | "last-name" | "middle-name" | "nickname" => {
                        in_author_field = true;
                    }
                    "FictionBook" => {
                        for attr in e.attributes().flatten() {
                            if std::str::from_utf8(attr.key.local_name().as_ref()).unwrap_or("")
                                == "lang"
                            {
                                if let Ok(v) =
                                    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                {
                                    meta.language = Some(v.into_owned());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                match std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") {
                    "body" => {
                        if in_main_body {
                            flush_section!();
                            in_main_body = false;
                        }
                    }
                    "section" => {
                        if in_main_body && section_depth > 0 {
                            section_depth -= 1;
                            if section_depth == 0 {
                                flush_section!();
                            }
                        }
                    }
                    "title" => in_title = false,
                    "p" | "subtitle" | "cite" | "poem" | "stanza" | "epigraph" | "text-author" => {
                        in_para = false;
                    }
                    "book-title" => in_book_title = false,
                    "annotation" => in_annotation = false,
                    "first-name" | "last-name" | "middle-name" | "nickname" => {
                        in_author_field = false;
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                let Ok(s) = t.xml10_content() else { continue };
                if s.trim().is_empty() {
                    continue;
                }
                if in_book_title {
                    meta.title.push_str(&s);
                } else if in_annotation {
                    meta.description
                        .get_or_insert_with(String::new)
                        .push_str(&s);
                } else if in_author_field {
                    author_parts.push(s.trim().to_string());
                } else if in_main_body && section_depth > 0 {
                    if let Some((title, content)) = cur.as_mut() {
                        if in_title {
                            push_para(title, &s, &mut para_break);
                        } else if in_para {
                            push_para(content, &s, &mut para_break);
                        }
                    }
                }
            }
            // CDATA 段与文本同处理（xml10_content 对两者均可用）
            Ok(quick_xml::events::Event::CData(t)) => {
                let Ok(s) = t.xml10_content() else { continue };
                if s.trim().is_empty() {
                    continue;
                }
                if in_book_title {
                    meta.title.push_str(&s);
                } else if in_annotation {
                    meta.description
                        .get_or_insert_with(String::new)
                        .push_str(&s);
                } else if in_author_field {
                    author_parts.push(s.trim().to_string());
                } else if in_main_body && section_depth > 0 {
                    if let Some((title, content)) = cur.as_mut() {
                        if in_title {
                            push_para(title, &s, &mut para_break);
                        } else if in_para {
                            push_para(content, &s, &mut para_break);
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::GeneralRef(r)) => {
                let s = if r.is_char_ref() {
                    r.resolve_char_ref().ok().flatten().map(|c| c.to_string())
                } else {
                    r.decode().ok().and_then(|name| {
                        quick_xml::escape::unescape(&format!("&{name};"))
                            .ok()
                            .map(|c| c.into_owned())
                    })
                };
                if let Some(s) = s {
                    if in_book_title {
                        meta.title.push_str(&s);
                    } else if in_annotation {
                        meta.description
                            .get_or_insert_with(String::new)
                            .push_str(&s);
                    } else if in_author_field {
                        author_parts.push(s);
                    } else if in_main_body && section_depth > 0 {
                        if let Some((title, content)) = cur.as_mut() {
                            if in_title {
                                title.push_str(&s);
                            } else if in_para {
                                content.push_str(&s);
                            }
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => anyhow::bail!("FB2 XML 解析失败：{e}"),
            _ => {}
        }
    }
    flush_section!();

    meta.title = meta.title.trim().to_string();
    if meta.author.is_empty() {
        meta.author = author_parts
            .iter()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
    }
    if let Some(d) = meta.description.as_mut() {
        *d = d.trim().to_string();
    }
    if chapters.is_empty() {
        anyhow::bail!("FB2 未解析到章节内容（缺少 body/section）");
    }
    Ok(ImportedBook {
        meta,
        chapters,
        cover: None,
        format: "fb2".into(),
    })
}

// ---------- DOCX ----------

/// DOCX 解析：zip + word/document.xml → 段落（含标题样式）→ 标题样式分章；无标题样式时回退纯文本规则分章
pub fn parse_docx(bytes: &[u8]) -> Result<ImportedBook> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("DOCX 不是有效的 zip")?;
    let document =
        read_zip(&mut zip, "word/document.xml").context("DOCX 缺少 word/document.xml")?;
    // 元数据（可选 docProps/core.xml）
    let mut meta = OpfMeta::default();
    if let Ok(core) = read_zip(&mut zip, "docProps/core.xml") {
        let (title, author) = docx_core_meta(&String::from_utf8_lossy(&core));
        meta.title = title;
        meta.author = author;
    }

    let xml = String::from_utf8_lossy(&document);
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut buf = Vec::new();
    let mut paras: Vec<(Option<String>, String)> = Vec::new(); // (样式, 文本)
    let mut in_p = false;
    let mut in_t = false;
    let mut p_style: Option<String> = None;
    let mut p_buf = String::new();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                match std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") {
                    "p" => {
                        in_p = true;
                        p_style = None;
                        p_buf.clear();
                    }
                    "pStyle" => {
                        if in_p {
                            for attr in e.attributes().flatten() {
                                if std::str::from_utf8(attr.key.local_name().as_ref()).unwrap_or("")
                                    == "val"
                                {
                                    if let Ok(v) =
                                        attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                    {
                                        p_style = Some(v.into_owned());
                                    }
                                }
                            }
                        }
                    }
                    "t" => in_t = true,
                    "tab" => {
                        if in_p {
                            p_buf.push('\t');
                        }
                    }
                    "br" | "cr" if in_p => {
                        p_buf.push('\n');
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                match std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") {
                    "p" => {
                        in_p = false;
                        let text = p_buf.trim().to_string();
                        if !text.is_empty() {
                            paras.push((p_style.take(), text));
                        }
                    }
                    "t" => in_t = false,
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if in_p && in_t {
                    if let Ok(s) = t.xml10_content() {
                        p_buf.push_str(&s);
                    }
                }
            }
            Ok(quick_xml::events::Event::GeneralRef(r)) => {
                if in_p && in_t {
                    let s = if r.is_char_ref() {
                        r.resolve_char_ref().ok().flatten().map(|c| c.to_string())
                    } else {
                        r.decode().ok().and_then(|name| {
                            quick_xml::escape::unescape(&format!("&{name};"))
                                .ok()
                                .map(|c| c.into_owned())
                        })
                    };
                    if let Some(s) = s {
                        p_buf.push_str(&s);
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => anyhow::bail!("DOCX 解析失败：{e}"),
            _ => {}
        }
    }

    let has_heading = paras
        .iter()
        .any(|(s, _)| s.as_deref().map(is_heading_style).unwrap_or(false));
    let chapters = if has_heading {
        docx_heading_chapters(&paras)
    } else {
        // 无标题样式：纯文本规则分章（或按字数分块）
        let joined = paras
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        chapters_from_plain_text(&joined)
    };
    if chapters.is_empty() {
        anyhow::bail!("DOCX 未解析到章节内容");
    }
    Ok(ImportedBook {
        meta,
        chapters,
        cover: None,
        format: "docx".into(),
    })
}

/// 标题样式判断（Word 内置 Heading1..9 / 中文“标题 1” / 旧版数字样式 1..9）
fn is_heading_style(style: &str) -> bool {
    let s = style.trim().to_lowercase();
    s.starts_with("heading")
        || s.starts_with("标题")
        || (s.len() == 1
            && s.chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false))
}

/// 按标题样式段落分章：标题段落开启新章节，其余段落并入当前章节
fn docx_heading_chapters(paras: &[(Option<String>, String)]) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut cur: Option<Chapter> = None;
    for (style, text) in paras {
        if style.as_deref().map(is_heading_style).unwrap_or(false) {
            if let Some(c) = cur.take() {
                if !c.content.trim().is_empty() || !c.title.trim().is_empty() {
                    chapters.push(c);
                }
            }
            cur = Some(Chapter {
                title: text.clone(),
                content: String::new(),
            });
        } else if let Some(c) = cur.as_mut() {
            if !c.content.is_empty() {
                c.content.push_str("\n\n");
            }
            c.content.push_str(text);
        } else {
            // 首个标题前的正文 → 归入“正文”章
            cur = Some(Chapter {
                title: "正文".into(),
                content: text.clone(),
            });
        }
    }
    if let Some(c) = cur.take() {
        if !c.content.trim().is_empty() || !c.title.trim().is_empty() {
            chapters.push(c);
        }
    }
    chapters
}

/// docProps/core.xml → (标题, 作者)
fn docx_core_meta(xml: &str) -> (String, String) {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut title = String::new();
    let mut author = String::new();
    let mut in_title = false;
    let mut in_creator = false;
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                match std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") {
                    "title" => in_title = true,
                    "creator" => in_creator = true,
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                match std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") {
                    "title" => in_title = false,
                    "creator" => in_creator = false,
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if let Ok(s) = t.xml10_content() {
                    if in_title {
                        title.push_str(s.trim());
                    } else if in_creator {
                        author.push_str(s.trim());
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            _ => {}
        }
    }
    (title.trim().to_string(), author.trim().to_string())
}

// ---------- CBZ（漫画压缩包） ----------

/// 图片扩展名 → MIME（无扩展名/非图片返回 None）
pub(crate) fn image_mime(name: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// 文件名自然排序（数字段按数值比较：page2 < page10；其余按不区分大小写字符序）。
/// 用于漫画页排序——纯字典序会把 10.jpg 排在 2.jpg 前面。
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let (da, db) = (a[i].is_ascii_digit(), b[j].is_ascii_digit());
        if da && db {
            // 跳过前导零后比较数值（先比有效位数，再逐位）
            let (mut x, mut y) = (i, j);
            while x < a.len() && a[x] == b'0' {
                x += 1;
            }
            while y < b.len() && b[y] == b'0' {
                y += 1;
            }
            let (mut xe, mut ye) = (x, y);
            while xe < a.len() && a[xe].is_ascii_digit() {
                xe += 1;
            }
            while ye < b.len() && b[ye].is_ascii_digit() {
                ye += 1;
            }
            let ord = (xe - x).cmp(&(ye - y));
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
            for k in 0..(xe - x) {
                let o = a[x + k].cmp(&b[y + k]);
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            i = xe;
            j = ye;
        } else {
            let o = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            if o != std::cmp::Ordering::Equal {
                return o;
            }
            i += 1;
            j += 1;
        }
    }
    a.len().cmp(&b.len())
}

/// CBZ 解析（漫画压缩包）：zip 内图片列表 → 章节 = 按文件名自然序的图片页。
///
/// 每页一章：title = 页文件名，content = markdown 图片语法 + base64 data URI
/// （`![页名](data:image/jpeg;base64,...)`）。前端 ReaderView 的 singleImageUrl
/// 识别该形式并直接渲染 <img>，data URI 无需额外图片服务路由，导入/导出/重扫自包含。
/// 对齐 legacy CbzFile：解析 ComicInfo.xml 的 Title/Writer 作为书名/作者，并取
/// zip 条目顺序的首张图片作封面（封面字节走 uploaded.cover 落盘 covers/）。
pub fn parse_cbz(bytes: &[u8]) -> Result<ImportedBook> {
    parse_cbz_impl(bytes, MAX_CBZ_TOTAL_BYTES)
}

/// 带累计输出上限的 CBZ 解析（P1-C3；测试用小上限验证超限路径）
fn parse_cbz_impl(bytes: &[u8], total_max: u64) -> Result<ImportedBook> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("CBZ 不是有效的 zip")?;
    let mut pages: Vec<(String, usize)> = Vec::new();
    let mut comic_info_name: Option<String> = None;
    let mut first_image: Option<String> = None;
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index(i) else { continue };
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_string();
        // ComicInfo.xml 可位于任意目录（legacy 仅根目录；放宽为不丢失元数据）
        if std::path::Path::new(&name)
            .file_name()
            .map(|s| s.eq_ignore_ascii_case("ComicInfo.xml"))
            .unwrap_or(false)
        {
            comic_info_name = Some(name.clone());
        }
        let base = std::path::Path::new(&name)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        // 跳过隐藏文件（.DS_Store 等）；非图片扩展名不参与分页
        if base.starts_with('.') {
            continue;
        }
        if image_mime(&name).is_some() {
            // legacy 取 zip 条目顺序的首张图片作封面（非自然序）
            if first_image.is_none() {
                first_image = Some(name.clone());
            }
            pages.push((name, i));
        }
    }
    if pages.is_empty() {
        anyhow::bail!("CBZ 内未找到图片（支持 jpg/jpeg/png/gif/webp/bmp/svg）");
    }
    // 按文件名自然序（数字感知）：page2.jpg < page10.jpg
    pages.sort_by(|x, y| natural_cmp(&x.0, &y.0));
    // ComicInfo.xml 元数据（Title/Writer）
    let mut meta = OpfMeta::default();
    if let Some(info_name) = comic_info_name {
        if let Ok(xml) = read_zip(&mut zip, &info_name) {
            let xml = String::from_utf8_lossy(&xml);
            meta.title = crate::service::epub::extract_tag(&xml, "Title")
                .map(|s| crate::service::epub::decode_entities(&s))
                .unwrap_or_default();
            meta.author = crate::service::epub::extract_tag(&xml, "Writer")
                .map(|s| crate::service::epub::decode_entities(&s))
                .unwrap_or_default();
        }
    }
    // 封面：zip 条目顺序首张图片（legacy updateCover 行为；读取失败忽略）
    let cover = first_image
        .as_deref()
        .and_then(|n| read_zip(&mut zip, n).ok())
        .filter(|b| !b.is_empty());
    use base64::Engine;
    let mut chapters = Vec::with_capacity(pages.len());
    // P1-C3：全部条目累计输出上限（解压炸弹防护——条目多/单条目大均受限）
    let mut total = 0u64;
    for (name, _idx) in pages {
        let bytes = read_zip(&mut zip, &name).context("读取 CBZ 图片失败")?;
        total = total.saturating_add(bytes.len() as u64);
        if total > total_max {
            anyhow::bail!(
                "CBZ 图片累计超出大小上限（{}MB），已拒绝",
                total_max / 1024 / 1024
            );
        }
        let file_name = std::path::Path::new(&name)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        let mime = image_mime(&name).unwrap_or("image/jpeg");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        // alt 文本避免前端 singleImageUrl 正则的 `]` 边界字符
        let alt = file_name.replace([']', ')'], "-");
        chapters.push(Chapter {
            title: file_name.clone(),
            content: format!("![{alt}](data:{mime};base64,{b64})"),
        });
    }
    Ok(ImportedBook {
        meta,
        chapters,
        cover,
        format: "cbz".into(),
    })
}

/// zip 条目相对路径安全性（zip-slip 防护）：拒绝绝对路径、盘符、含 `..` 分量的条目名
fn is_safe_zip_entry_path(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return false;
    }
    if name.contains(':') {
        return false;
    }
    name.split(['/', '\\']).all(|c| c != "..")
}

/// CBZ 章节页图解压（getBookContent 漫画页图模式，legacy type=2 img 标签列表对齐）：
/// 把 zip 内图片条目解压到 out_dir（保留子目录结构），返回自然序相对路径列表。
/// - zip-slip 防护：条目名经 is_safe_zip_entry_path 校验，非法/隐藏/非图片条目跳过
/// - P1-C3 解压炸弹防护：单条目与累计输出同 CBZ 上限
pub fn extract_cbz_chapter_images(bytes: &[u8], out_dir: &std::path::Path) -> Result<Vec<String>> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("CBZ 不是有效的 zip")?;
    let mut names: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index(i) else { continue };
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_string();
        if !is_safe_zip_entry_path(&name) {
            continue;
        }
        let base = std::path::Path::new(&name.replace('\\', "/"))
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // 隐藏文件（.DS_Store 等）；非图片扩展名不作为页图
        if base.starts_with('.') || image_mime(&name).is_none() {
            continue;
        }
        names.push(name);
    }
    anyhow::ensure!(!names.is_empty(), "CBZ 内未找到图片");
    names.sort_by(|a, b| natural_cmp(a, b));
    std::fs::create_dir_all(out_dir)?;
    let mut out = Vec::with_capacity(names.len());
    let mut total = 0u64;
    for name in &names {
        let data = read_zip_limited(&mut zip, name, MAX_ZIP_ENTRY_BYTES)?;
        total = total.saturating_add(data.len() as u64);
        anyhow::ensure!(
            total <= MAX_CBZ_TOTAL_BYTES,
            "CBZ 图片累计超出大小上限（{}MB），已拒绝",
            MAX_CBZ_TOTAL_BYTES / 1024 / 1024
        );
        let rel = name.replace('\\', "/");
        let dest = out_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &data)?;
        out.push(rel);
    }
    Ok(out)
}

/// 目录下图片文件清单（递归；隐藏文件跳过；自然序）
pub fn list_image_files(dir: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                walk(&p, &format!("{prefix}{name}/"), out);
            } else if p.is_file() && image_mime(&name).is_some() {
                out.push(format!("{prefix}{name}"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort_by(|a, b| natural_cmp(a, b));
    out
}

// ---------- UMD ----------

/// UMD（掌上书院）解析——语义对齐 me.ag2s.umdlib（legacy 阅读器 UmdFile 所用解析库）。
///
/// 文件结构（umdlib UmdReader 状态机）：
/// - 魔数：4 字节 LE int 0xDE9A9B89（文件字节 89 9B 9A DE）
/// - section：`#`(0x23) + 类型(2B LE) + 标志(1B) + 长度字节(1B，数据长 = 值 - 5) + 数据
/// - section 后可跟 0..n 个附加块：`$`(0x24) + 校验号(4B LE) + 长度(4B LE，数据长 = 值 - 9) + 数据
/// - 属性 section：0x01 文件类型（1=文本 0x02=漫画）、0x02 标题、0x03 作者、0x04-0x06 年月日、
///   0x07 题材、0x08 出版商、0x09 零售商、0x0B 未压缩正文总字节数、0x0C 结束（文件总长）、
///   0x0A/0xF1 内容 ID/许可证（跳过）
/// - 章节偏移 section 0x83：附加块 = n × 4B LE 章节偏移（字节偏移，指向拼接后的未压缩正文）
/// - 章节 section 0x84：附加块校验号 == section 头校验号 → n 个标题（1B 长度 + UTF-16LE 字节）；
///   校验号不同 → zlib 压缩正文块（按序拼接）
/// - 封面 section 0x82：附加块 = 封面图片字节
///
/// 正文为 UTF-16LE；按章节偏移切片后解码，U+2029 段落分隔符替换为 \n（umdlib getContentString）。
pub fn parse_umd(bytes: &[u8]) -> Result<ImportedBook> {
    parse_umd_impl(bytes, MAX_UMD_TEXT_BYTES)
}

/// 带正文输出上限的 UMD 解析（P1-C3；测试用小上限验证解压炸弹超限路径）
#[allow(clippy::while_let_loop)] // 显式 loop + let-else 便于多处 break，改成 while let 反而难读
fn parse_umd_impl(bytes: &[u8], max_text: u64) -> Result<ImportedBook> {
    let mut cur = UmdCursor::new(bytes);
    let magic = cur.read_u32le().context("UMD 文件过短")?;
    if magic != 0xDE9A9B89 {
        anyhow::bail!("不是有效的 UMD 文件（魔数不符）");
    }
    let mut meta = OpfMeta::default();
    let mut umd_year = String::new();
    let mut umd_month = String::new();
    let mut umd_day = String::new();
    let mut umd_type: u8 = 1;
    let mut content_lengths: Vec<usize> = Vec::new();
    let mut titles: Vec<String> = Vec::new();
    let mut contents: Vec<u8> = Vec::new();
    let mut total_content_len: Option<usize> = None;
    let mut num_chapters: usize = 0;
    let mut additional_check: u32 = 0;
    let mut cover: Option<Vec<u8>> = None;
    let mut prev_section: u16 = 0;
    let mut end = false;
    let mut next: Option<u8> = None;

    loop {
        if end {
            break;
        }
        let b = match next.take() {
            Some(b) => b,
            None => match cur.read_u8() {
                Ok(b) => b,
                Err(_) => break, // EOF：正常结束（文件可无 0x0C 结束标记）
            },
        };
        if b != 0x23 {
            break;
        }
        let seg_type = cur.read_u16le().context("UMD section 头损坏")?;
        let _seg_flag = cur.read_u8().context("UMD section 头损坏")?;
        let len_byte = cur.read_u8().context("UMD section 头损坏")?;
        let length = len_byte as i32 - 5;
        if length < 0 {
            anyhow::bail!("UMD section 长度非法（{len_byte}）");
        }
        match seg_type {
            1 => {
                umd_type = cur.read_u8().context("UMD 文件类型缺失")?;
                let _ = cur.bytes(2).context("UMD 文件类型段损坏")?;
            }
            2..=9 => {
                let raw = cur.bytes(length as usize).context("UMD 属性段损坏")?;
                let s = umd_utf16_string(&raw);
                match seg_type {
                    2 => meta.title = s,
                    3 => meta.author = s,
                    4 => umd_year = s,
                    5 => umd_month = s,
                    6 => umd_day = s,
                    7 => {
                        if !s.is_empty() {
                            meta.subjects.push(s);
                        }
                    }
                    8 => meta.publisher = Some(s),
                    _ => { /* 0x09 零售商：忽略 */ }
                }
            }
            10 => {
                let _ = cur.bytes(length as usize).context("UMD 内容 ID 段损坏")?;
            }
            11 => {
                total_content_len = Some(cur.read_u32le().context("UMD 正文长度段损坏")? as usize);
            }
            12 => {
                end = true;
                let _ = cur.read_u32le().context("UMD 结束段损坏")?;
            }
            13 => {}
            14 => {
                let _ = cur.read_u8().context("UMD 段损坏")?;
            }
            15 => {
                let _ = cur.bytes(length as usize).context("UMD 段损坏")?;
            }
            129 | 131 | 132 => {
                additional_check = cur.read_u32le().context("UMD 章节段损坏")?;
            }
            130 => {
                let _ = cur.read_u8().context("UMD 封面段损坏")?;
                additional_check = cur.read_u32le().context("UMD 封面段损坏")?;
            }
            135 => {
                let _ = cur.read_u8().context("UMD 段损坏")?;
                let _ = cur.read_u8().context("UMD 段损坏")?;
                let _ = cur.bytes(4).context("UMD 段损坏")?;
            }
            240 => {}
            241 => {
                let _ = cur.bytes(16).context("UMD 许可证段损坏")?;
            }
            _ => {
                if length > 0 {
                    let _ = cur.bytes(length as usize).context("UMD 未知段损坏")?;
                }
            }
        }
        // 0x0A/0xF1 的附加块归属前一 section（umdlib 语义）
        let effective = if seg_type == 241 || seg_type == 10 {
            prev_section
        } else {
            seg_type
        };
        // 附加块（`$` 开头）
        loop {
            let b2 = match cur.read_u8() {
                Ok(b2) => b2,
                Err(_) => break, // EOF
            };
            if b2 != 0x24 {
                next = Some(b2);
                break;
            }
            let check = cur.read_u32le().context("UMD 附加块损坏")?;
            let len32 = cur.read_u32le().context("UMD 附加块损坏")?;
            let block_len = len32 as i64 - 9;
            if block_len < 0 {
                anyhow::bail!("UMD 附加块长度非法（{len32}）");
            }
            match effective {
                129 => {
                    let _ = cur.bytes(block_len as usize).context("UMD 附加块损坏")?;
                }
                130 => {
                    cover = Some(cur.bytes(block_len as usize).context("UMD 封面损坏")?);
                }
                131 => {
                    num_chapters = (block_len / 4) as usize;
                    content_lengths.clear();
                    for _ in 0..num_chapters {
                        content_lengths
                            .push(cur.read_u32le().context("UMD 章节偏移损坏")? as usize);
                    }
                }
                132 => {
                    if additional_check != check {
                        // 正文块：zlib 解压后按序拼接（P1-C3：输出上限——解压炸弹防护）
                        let compressed = cur.bytes(block_len as usize).context("UMD 正文块损坏")?;
                        let mut out = Vec::new();
                        flate2::read::ZlibDecoder::new(&compressed[..])
                            .take(max_text + 1)
                            .read_to_end(&mut out)
                            .context("UMD 正文解压失败")?;
                        if out.len() as u64 > max_text {
                            anyhow::bail!(
                                "UMD 正文解压超出大小上限（{}MB），已拒绝",
                                max_text / 1024 / 1024
                            );
                        }
                        contents.extend_from_slice(&out);
                        if contents.len() as u64 > max_text {
                            anyhow::bail!(
                                "UMD 正文累计超出大小上限（{}MB），已拒绝",
                                max_text / 1024 / 1024
                            );
                        }
                    } else {
                        // 标题块
                        for _ in 0..num_chapters {
                            let tlen = cur.read_u8().context("UMD 标题损坏")? as usize;
                            let raw = cur.bytes(tlen).context("UMD 标题损坏")?;
                            titles.push(umd_utf16_string(&raw));
                        }
                    }
                }
                _ => {
                    let _ = cur.bytes(block_len as usize).context("UMD 附加块损坏")?;
                }
            }
        }
        prev_section = seg_type;
    }

    if umd_type == 2 {
        anyhow::bail!("暂不支持 UMD 漫画（图片型）文件");
    }
    if titles.is_empty() && contents.is_empty() {
        anyhow::bail!("UMD 未解析到章节内容");
    }
    // 出版时间：年[-月[-日]]（legacy UmdHeader year/month/day）
    if !umd_year.is_empty() {
        let mut published = umd_year;
        if !umd_month.is_empty() {
            published.push('-');
            published.push_str(&umd_month);
            if !umd_day.is_empty() {
                published.push('-');
                published.push_str(&umd_day);
            }
        }
        meta.published_at = Some(published);
    }

    // 章节切片：偏移指向拼接后未压缩正文（UTF-16LE 字节）；最后一章到 total_content_len
    //（缺失/越界时回退到实际拼接长度）
    let total = total_content_len.unwrap_or(contents.len());
    let mut chapters = Vec::with_capacity(titles.len());
    for (idx, title) in titles.iter().enumerate() {
        let start = content_lengths
            .get(idx)
            .copied()
            .unwrap_or(0)
            .min(contents.len());
        let end = if idx + 1 < content_lengths.len() {
            content_lengths[idx + 1]
        } else {
            total
        };
        let end = if end <= start || end > contents.len() {
            contents.len()
        } else {
            end
        };
        let text = umd_utf16_string(&contents[start..end]);
        if text.trim().is_empty() && title.trim().is_empty() {
            continue;
        }
        chapters.push(Chapter {
            title: title.clone(),
            content: text,
        });
    }
    // 无标题但有正文（缺 0x84 的变体文件）：整书单章兜底
    if chapters.is_empty() && !contents.is_empty() {
        let text = umd_utf16_string(&contents);
        if !text.trim().is_empty() {
            let title = if meta.title.is_empty() {
                "正文".to_string()
            } else {
                meta.title.clone()
            };
            chapters.push(Chapter {
                title,
                content: text,
            });
        }
    }
    if chapters.is_empty() {
        anyhow::bail!("UMD 未解析到章节内容");
    }
    Ok(ImportedBook {
        meta,
        chapters,
        cover,
        format: "umd".into(),
    })
}

/// UMD 顺序读取游标
struct UmdCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> UmdCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> std::io::Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        self.pos += 1;
        Ok(b)
    }

    fn read_u16le(&mut self) -> std::io::Result<u16> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32le(&mut self) -> std::io::Result<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn bytes(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        if self.pos + buf.len() > self.data.len() {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        buf.copy_from_slice(&self.data[self.pos..self.pos + buf.len()]);
        self.pos += buf.len();
        Ok(())
    }
}

/// UTF-16LE 解码（encoding_rs；容忍奇数尾字节；剥离 BOM）
fn umd_utf16_string(bytes: &[u8]) -> String {
    let (s, _) = encoding_rs::UTF_16LE.decode_without_bom_handling(bytes);
    let s = s.into_owned();
    // BOM（FF FE）被无 BOM 处理保留为 U+FEFF → 剥离
    let s = s.strip_prefix('\u{feff}').unwrap_or(&s).to_string();
    // U+2029 段落分隔符 → 换行（umdlib getContentString 语义）
    s.replace('\u{2029}', "\n")
}

/// 判断是否本地书（local:// 或文件路径型 legacy 本地书）
pub fn is_local_book(book_url: &str, origin: &str) -> bool {
    book_url.starts_with("local://")
        || origin == "loc_book"
        || book_url.starts_with("storage/")
        || has_supported_ext(book_url)
}

/// 支持的本地书扩展名白名单（上传 / getBookToc / getBookContent 分派共用）
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "epub", "txt", "mobi", "azw3", "pdf", "fb2", "docx", "zip", "cbz", "umd",
];

/// 取文件名/路径的小写扩展名（不含点；无扩展名返回空串）
pub fn file_ext(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

/// 路径是否带白名单扩展名（大小写不敏感）
fn has_supported_ext(name: &str) -> bool {
    let ext = file_ext(name);
    !ext.is_empty() && SUPPORTED_EXTENSIONS.contains(&ext.as_str())
}

/// 按扩展名分派解析（bytes 版本；扩展名小写、不含点）。
/// toc_mode 仅对 epub/zip 生效（legacy tocUrl 六模式；非法值按默认 spin+toc）；
/// split_long 对 txt 生效（legacy getSplitLongChapter——单章 >100KB 拆分）。
pub fn parse_file_bytes(
    bytes: &[u8],
    ext: &str,
    user_rules: &[String],
    toc_mode: &str,
    split_long: bool,
) -> Result<ImportedBook> {
    match ext {
        "epub" => parse_epub(bytes, toc_mode),
        // zip：优先标准 EPUB（container.xml）→ fallback 裸 OPF 结构
        "zip" => parse_epub(bytes, toc_mode).or_else(|_| parse_opf_zip(bytes)),
        "txt" => {
            let mut imported = parse_txt_with_rules(bytes, user_rules)?;
            imported.chapters = split_long_chapters(imported.chapters, split_long);
            Ok(imported)
        }
        "mobi" => parse_mobi(bytes),
        "azw3" => parse_azw3(bytes),
        "pdf" => parse_pdf(bytes),
        "fb2" => parse_fb2(bytes),
        "docx" => parse_docx(bytes),
        "cbz" => parse_cbz(bytes),
        "umd" => parse_umd(bytes),
        other => anyhow::bail!("不支持的格式：{other}"),
    }
}

/// 按文件扩展名分派解析（路径版本；getBookToc/getBookContent 的 loc_book 分支共用）。
/// toc_mode 仅对 epub/zip 生效；split_long 仅对 txt 生效（目录与正文需同参保证 #index 一致）。
pub fn parse_loc_book_path(
    path: &std::path::Path,
    user_rules: &[String],
    toc_mode: &str,
    split_long: bool,
) -> Result<ImportedBook> {
    let bytes = std::fs::read(path)?;
    let ext = file_ext(&path.to_string_lossy());
    parse_file_bytes(&bytes, &ext, user_rules, toc_mode, split_long)
}

/// 兼容旧签名：默认 EPUB 目录模式、不拆长章节
pub fn parse_loc_book_path_default(
    path: &std::path::Path,
    user_rules: &[String],
) -> Result<ImportedBook> {
    parse_loc_book_path(path, user_rules, DEFAULT_EPUB_TOC_MODE, false)
}

// ---------- 工具 ----------

/// P1-C3：zip 单条目解压输出上限（EPUB/DOCX/CBZ 解压炸弹防护——超限拒绝）
const MAX_ZIP_ENTRY_BYTES: u64 = 500 * 1024 * 1024;

/// P1-C3：CBZ 全部条目累计输出上限（与单条目同值）
const MAX_CBZ_TOTAL_BYTES: u64 = 500 * 1024 * 1024;

/// P1-C3：MOBI 声称未压缩正文长度上限（解压前校验——Huffman 炸弹防护）
const MAX_MOBI_TEXT_BYTES: u64 = 500 * 1024 * 1024;

/// P1-C3：MOBI Huffman 词典短语数上限（CDIC num_phrases 分配前校验；正常词典数百~数千条）
const MAX_MOBI_CDIC_PHRASES: u64 = 1 << 20;

/// P1-C3：UMD 正文 zlib 解压输出上限
const MAX_UMD_TEXT_BYTES: u64 = 500 * 1024 * 1024;

fn read_zip<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    path: &str,
) -> Result<Vec<u8>> {
    read_zip_limited(zip, path, MAX_ZIP_ENTRY_BYTES)
}

/// 带输出上限的 zip 条目读取（P1-C3；测试用小上限验证超限路径）
fn read_zip_limited<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    path: &str,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let mut f = zip.by_name(path)?;
    let mut buf = Vec::new();
    std::io::Read::take(&mut f, max_bytes + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        anyhow::bail!(
            "条目 [{path}] 解压后超出大小上限（{}MB），已拒绝",
            max_bytes / 1024 / 1024
        );
    }
    Ok(buf)
}

/// 提取第一个指定属性（如 <rootfile full-path="content.opf">）
fn extract_attr_simple(xml: &str, tag: &str, attr: &str) -> Option<String> {
    // 匹配 <tag 后跟 空格/>/换行（排除 <tags> 等更长标签名）
    let idx = xml
        .find(&format!("<{tag} "))
        .or_else(|| xml.find(&format!("<{tag}>")))?;
    let rest = &xml[idx..];
    let end = rest.find('>')?;
    let block = &rest[..end];
    let pat = format!("{attr}=\"");
    let pat2 = format!("{attr}='");
    if let Some(i) = block.find(&pat) {
        return block[i + pat.len()..].split('"').next().map(str::to_string);
    }
    if let Some(i) = block.find(&pat2) {
        return block[i + pat2.len()..]
            .split('\'')
            .next()
            .map(str::to_string);
    }
    None
}

/// 提取所有 itemref 的 idref
fn extract_all_attr(xml: &str, tag: &str, attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(idx) = rest.find(&format!("<{tag}")) {
        let after = &rest[idx..];
        let Some(end) = after.find('>') else { break };
        let block = &after[..end];
        let pat = format!("{attr}=\"");
        if let Some(i) = block.find(&pat) {
            if let Some(v) = block[i + pat.len()..].split('"').next() {
                out.push(v.to_string());
            }
        }
        rest = &after[end + 1..];
    }
    out
}

/// manifest：id → (href, mediatype)
fn extract_manifest(xml: &str) -> std::collections::HashMap<String, (String, String)> {
    let mut map = std::collections::HashMap::new();
    let mut rest = xml;
    while let Some(idx) = rest.find("<item") {
        let after = &rest[idx..];
        let Some(end) = after.find('>') else { break };
        let block = &after[..end];
        let id = attr_value(block, "id");
        let href = attr_value(block, "href");
        let mediatype = attr_value(block, "media-type");
        if let (Some(id), Some(href)) = (id, href) {
            map.insert(id, (href, mediatype.unwrap_or_default()));
        }
        rest = &after[end + 1..];
    }
    map
}

fn attr_value(block: &str, attr: &str) -> Option<String> {
    let pat = format!("{attr}=\"");
    let pat2 = format!("{attr}='");
    if let Some(i) = block.find(&pat) {
        return block[i + pat.len()..].split('"').next().map(str::to_string);
    }
    if let Some(i) = block.find(&pat2) {
        return block[i + pat2.len()..]
            .split('\'')
            .next()
            .map(str::to_string);
    }
    None
}

/// OPF 相对路径 → zip 全路径
fn resolve_opf_path(opf_path: &str, href: &str) -> String {
    let href_clean = href.split('#').next().unwrap_or(href);
    if let Some(idx) = opf_path.rfind('/') {
        format!("{}/{}", &opf_path[..idx], href_clean)
    } else {
        href_clean.to_string()
    }
}

/// XHTML → 纯文本（保留段落）
fn html_to_text(html: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let mut parts = Vec::new();
    for el in doc.root_element().descendants() {
        if let scraper::node::Node::Element(e) = el.value() {
            // 跳过样式/脚本（EPUB 封面/内嵌 CSS 噪音）
            if matches!(e.name(), "style" | "script") {
                continue;
            }
            if matches!(e.name(), "p" | "div" | "h1" | "h2" | "h3" | "br" | "li") {
                let text = el
                    .descendants()
                    .filter_map(|d| match d.value() {
                        scraper::node::Node::Text(t) => Some(t.text.trim().to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    if parts.is_empty() {
        // fallback：body 全部文本
        return doc
            .root_element()
            .text()
            .collect::<String>()
            .trim()
            .to_string();
    }
    parts.join("\n\n")
}

/// 提取 <title>（优先 h1/h2，其次 head title）
fn extract_title(html: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    for sel in ["h1", "h2", "h3", "title"] {
        if let Ok(selector) = scraper::Selector::parse(sel) {
            if let Some(el) = doc.select(&selector).next() {
                let t = el.text().collect::<String>().trim().to_string();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_book_type_maps_extension() {
        assert_eq!(local_book_type("epub"), 0, "EPUB 文本");
        assert_eq!(local_book_type("txt"), 0, "TXT 文本");
        assert_eq!(local_book_type("PDF"), 0, "PDF 文本");
        assert_eq!(local_book_type("cbz"), 2, "CBZ 漫画");
        assert_eq!(local_book_type("CBZ"), 2, "扩展名大小写不敏感");
    }

    /// 裸 OPF zip（无 container.xml）：解析成功 + spine 顺序章节
    #[test]
    fn parse_opf_zip_bare_structure() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::FileOptions::default();
            zip.start_file("book.opf", opts).unwrap();
            zip.write_all(
                r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">OPF测试书</dc:title></metadata>
  <manifest>
    <item id="c1" href="chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="chap2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="c1"/><itemref idref="c2"/>
  </spine>
</package>"#
                    .as_bytes(),
            )
            .unwrap();
            zip.start_file("chap1.xhtml", opts).unwrap();
            zip.write_all(
                "<html><head><title>第一章</title></head><body><p>第一段内容</p></body></html>"
                    .as_bytes(),
            )
            .unwrap();
            zip.start_file("chap2.xhtml", opts).unwrap();
            zip.write_all(
                "<html><head><title>第二章</title></head><body><p>第二段内容</p></body></html>"
                    .as_bytes(),
            )
            .unwrap();
            zip.finish().unwrap();
        }
        let bytes = buf.into_inner();
        let book = parse_opf_zip(&bytes).expect("解析成功");
        assert_eq!(book.meta.title, "OPF测试书");
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "第一章");
        assert!(book.chapters[1].content.contains("第二段内容"));
    }

    /// zip 内无 OPF → 明确报错
    #[test]
    fn parse_opf_zip_missing_opf() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            zip.start_file("readme.txt", zip::write::FileOptions::default())
                .unwrap();
            zip.write_all(b"no opf here").unwrap();
            zip.finish().unwrap();
        }
        let err = parse_opf_zip(&buf.into_inner()).unwrap_err().to_string();
        assert!(err.contains(".opf"), "错误应提示缺 OPF: {err}");
    }

    #[test]
    fn parse_legacy_epub_dir() {
        // 依赖审计实测环境遗留的 fixture（target/search-test），缺失时跳过（不视为失败）
        let p = "C:/Users/chong/pr-review/reader-dev/target/search-test/storage/data/transwarp/狼爱似火_迷羊/狼爱似火.epub/index.epub";
        let Ok(bytes) = std::fs::read(p) else {
            eprintln!("skip: fixture 不存在（{p}）");
            return;
        };
        match parse_epub(&bytes, DEFAULT_EPUB_TOC_MODE) {
            Ok(b) => println!("OK: {} 章, title={}", b.chapters.len(), b.meta.title),
            Err(e) => println!("ERR: {e}"),
        }
    }

    /// 构造最小 EPUB（EPUB2：toc.ncx；EPUB3：nav.xhtml）验证六模式：
    /// - spine 顺序 [c1, c2]（标题 第一章/第二章）
    /// - toc.ncx 顺序 [c2, c1]（标题 TOC-2/TOC-1）
    #[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
    fn build_test_epub() -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::FileOptions::default();
            zip.start_file("mimetype", opts).unwrap();
            zip.write_all(b"application/epub+zip").unwrap();
            zip.start_file("META-INF/container.xml", opts).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
            )
            .unwrap();
            zip.start_file("OEBPS/content.opf", opts).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">Test</dc:title></metadata>
  <manifest>
    <item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="c2.xhtml" media-type="application/xhtml+xml"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"#,
            )
            .unwrap();
            zip.start_file("OEBPS/c1.xhtml", opts).unwrap();
            zip.write_all(
                r#"<html><head><title>第一章</title></head><body><h1>第一章</h1><p>内容一。</p></body></html>"#.as_bytes(),
            )
            .unwrap();
            zip.start_file("OEBPS/c2.xhtml", opts).unwrap();
            zip.write_all(
                r#"<html><head><title>第二章</title></head><body><h1>第二章</h1><p>内容二。</p></body></html>"#.as_bytes(),
            )
            .unwrap();
            zip.start_file("OEBPS/toc.ncx", opts).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <navMap>
    <navPoint id="n2"><navLabel><text>TOC-2</text></navLabel><content src="c2.xhtml"/></navPoint>
    <navPoint id="n1"><navLabel><text>TOC-1</text></navLabel><content src="c1.xhtml"/></navPoint>
  </navMap>
</ncx>"#,
            )
            .unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// legacy TextFile 长章节拆分：>100KB 且开启 → ~10KB 换行边界切块，标题 {原标题}(n)
    #[test]
    fn test_split_long_chapters() {
        // 120KB 内容：每行 "x" + \n（2 字节）→ 61440 行
        let big = "x\n".repeat(61_440);
        assert!(big.len() > 100 * 1024);
        let chapters_len = big.len();
        let chapters = vec![Chapter {
            title: "长章".into(),
            content: big,
        }];
        // 未开启：原样保留
        let kept = split_long_chapters(chapters.clone(), false);
        assert_eq!(kept.len(), 1);
        // 开启：拆成多块，全部 ≥10KB（换行边界向后取），标题带 (n)
        let split = split_long_chapters(chapters, true);
        assert!(split.len() >= 11, "应拆 ≥11 块，实际 {}", split.len());
        assert_eq!(split[0].title, "长章(1)");
        assert_eq!(
            split[split.len() - 1].title,
            format!("长章({})", split.len())
        );
        for (i, c) in split.iter().enumerate() {
            assert!(!c.content.is_empty());
            if i < split.len() - 1 {
                assert!(
                    c.content.len() >= 10 * 1024,
                    "块{}应≥10KB，实际{}",
                    i,
                    c.content.len()
                );
                assert!(c.content.ends_with('x'), "块{}应以完整行结束", i);
            }
        }
        // 分块连续无间隙：总字节与原内容一致（边界换行归入后一块）
        let total: usize = split.iter().map(|c| c.content.len()).sum();
        assert_eq!(total, chapters_len, "内容字节守恒");
    }

    /// parse_file_bytes txt 分支接入 split_long
    #[test]
    fn test_parse_txt_split_long_via_dispatch() {
        // 第一章标记 + >100KB 正文
        let mut body = String::from("第一章 起点\n");
        body.push_str(&"y\n".repeat(70_000)); // 140KB
        let imported =
            parse_file_bytes(body.as_bytes(), "txt", &[], DEFAULT_EPUB_TOC_MODE, true).unwrap();
        assert!(imported.chapters.len() > 1, "开启时应拆分");
        assert!(imported.chapters[0].title.starts_with("第一章 起点("));
        let plain =
            parse_file_bytes(body.as_bytes(), "txt", &[], DEFAULT_EPUB_TOC_MODE, false).unwrap();
        assert_eq!(plain.chapters.len(), 1, "关闭时保持单章");
    }

    #[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
    fn parse_epub_toc_modes() {
        let bytes = build_test_epub();
        // 默认 spin+toc：spine 顺序，标题用 spine 的（spine 标题非空不覆盖）
        let b = parse_epub(&bytes, DEFAULT_EPUB_TOC_MODE).unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章", "第二章"]);
        assert!(b.chapters[0].content.contains("内容一。"));
        // spin<toc：spine 顺序，toc 标题强制覆盖
        let b = parse_epub(&bytes, "spin<toc").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["TOC-1", "TOC-2"]);
        // toc：仅 toc 目录顺序（c2 在前）
        let b = parse_epub(&bytes, "toc").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["TOC-2", "TOC-1"]);
        assert!(b.chapters[0].content.contains("内容二。"));
        // toc+spin：toc 顺序，标题用 toc 的（toc 标题非空不覆盖）
        let b = parse_epub(&bytes, "toc+spin").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["TOC-2", "TOC-1"]);
        // toc<spin：toc 顺序，spine 标题强制覆盖
        let b = parse_epub(&bytes, "toc<spin").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第二章", "第一章"]);
        // 非法模式 → 默认 spin+toc
        let b = parse_epub(&bytes, "bogus").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章", "第二章"]);
        // is_epub_toc_mode 判定
        assert!(is_epub_toc_mode("toc"));
        assert!(is_epub_toc_mode("spin<toc"));
        assert!(!is_epub_toc_mode("storage/books/a.epub"));
        assert!(!is_epub_toc_mode(""));
    }
    const SAMPLE: &str = "第一章 起点\n内容一。\n第二章 成长\n内容二。\n尾声\n结局。";

    /// 默认规则分章：第X章 + 尾声
    #[test]
    fn test_parse_txt_default_rules() {
        let book = parse_txt(SAMPLE.as_bytes()).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长", "尾声"]);
        assert_eq!(book.chapters[1].content, "内容二。");
        assert_eq!(book.chapters[2].content, "结局。");
    }

    /// 用户自定义规则分章（规则传入时替代默认规则）
    #[test]
    fn test_parse_txt_custom_rules() {
        // 用户规则只匹配「第X章」（不匹配尾声）→ 尾声并入上一章
        let rules = vec![
            r"^\s*第\s*[0-9一二三四五六七八九十百千万零〇两]+\s*[章节卷回集部篇].*".to_string(),
        ];
        let book = parse_txt_with_rules(SAMPLE.as_bytes(), &rules).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长"]);
        assert_eq!(book.chapters[1].content, "内容二。\n尾声\n结局。");
    }

    /// 空规则列表回退默认规则
    #[test]
    fn test_parse_txt_empty_rules_falls_back() {
        let book = parse_txt_with_rules(SAMPLE.as_bytes(), &[]).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长", "尾声"]);
    }

    /// 无章节标记长文本按 10000 字分块
    #[test]
    fn test_parse_txt_long_text_chunked() {
        let body = "字".repeat(25_000);
        let book = parse_txt(body.as_bytes()).unwrap();
        assert_eq!(book.chapters.len(), 3);
        assert!(book
            .chapters
            .iter()
            .all(|c| c.title.starts_with("第 ") && c.title.ends_with(" 部分")));
    }

    /// GBK 编码文本可解析
    #[test]
    fn test_parse_txt_gbk() {
        let text = "第一章 测试\n内容。";
        let (gbk_bytes, _, _) = encoding_rs::GBK.encode(text);
        let book = parse_txt(&gbk_bytes).unwrap();
        assert_eq!(book.chapters[0].title, "第一章 测试");
        assert_eq!(book.chapters[0].content, "内容。");
    }

    /// legacy 默认规则全量：18 条定义、10 条启用；单规则选择（命中数最多）——
    /// 「目录」规则命中最多（第一章 起点 + 尾声），英文/数字分隔符规则命中各 1 不参与
    #[test]
    fn test_default_toc_rule_defs_legacy_set() {
        assert_eq!(DEFAULT_TOC_RULE_DEFS.len(), 18, "legacy 内置 18 条规则");
        let enabled = DEFAULT_TOC_RULE_DEFS.iter().filter(|d| d.enable).count();
        assert_eq!(enabled, 10, "legacy 默认启用 10 条");
        assert_eq!(default_toc_rule_regexes().len(), 10);

        let sample =
            "第一章 起点\n内容一。\nChapter 2 The Road\n内容二。\n3. 独白\n内容三。\n尾声\n结局。";
        let book = parse_txt(sample.as_bytes()).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        // legacy TextFile.getTocRule：只选命中数最多的单一规则
        assert_eq!(titles, vec!["第一章 起点", "尾声"]);
    }

    /// 禁用规则不参与分章（顶格标题/纯数字标题为 legacy 禁用项）
    #[test]
    fn test_default_toc_rules_respects_enable() {
        let sample = "第一章 起点\n这是一段普通文字\n第二章 成长\n另一段普通内容";
        let book = parse_txt(sample.as_bytes()).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长"]);
        assert_eq!(book.chapters[0].content, "这是一段普通文字");
        assert_eq!(book.chapters[1].content, "另一段普通内容");
    }

    /// 不同规则重叠命中（行首数字标题 + 行内第X章）：单规则选择取命中数最多的
    /// 「目录(去空白)」规则（第二章也命中），行首数字成为正文残片——legacy 行为
    #[test]
    fn test_parse_txt_overlapping_rules_no_panic() {
        let sample = "1 第一章 起点\n内容。\n2 第二章 成长\n内容。";
        let book = parse_txt(sample.as_bytes()).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["正文", "第一章 起点", "第二章 成长"]);
        assert_eq!(book.chapters[0].content, "1");
        assert_eq!(book.chapters[1].content, "内容。\n2");
        assert_eq!(book.chapters[2].content, "内容。");
    }

    // ---------------- 新格式：MOBI/AZW3 ----------------

    /// 损坏数据：错误友好（提示 MOBI/AZW3 而非 panic）
    #[test]
    fn test_parse_mobi_garbage_friendly_error() {
        let err = parse_mobi(b"not a mobi file at all").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("MOBI") || msg.contains("mobi"),
            "错误应提及 MOBI：{msg}"
        );
        assert!(
            parse_azw3(b"garbage").is_err(),
            "azw3 兼容层对垃圾数据应报错"
        );
    }

    /// 分派：mobi/azw3 走兼容层，未知扩展名报“不支持的格式”
    #[test]
    fn test_parse_file_bytes_dispatch() {
        assert!(parse_file_bytes(b"x", "mobi", &[], DEFAULT_EPUB_TOC_MODE, false).is_err());
        assert!(parse_file_bytes(b"x", "azw3", &[], DEFAULT_EPUB_TOC_MODE, false).is_err());
        let err = parse_file_bytes(b"x", "epub", &[], DEFAULT_EPUB_TOC_MODE, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("EPUB") || msg.contains("zip"),
            "EPUB 错误应友好：{msg}"
        );
        let err = parse_file_bytes(b"x", "rar", &[], DEFAULT_EPUB_TOC_MODE, false).unwrap_err();
        assert!(format!("{err:#}").contains("不支持的格式"));
    }

    /// 小样本 MOBI：手工构造最小 PalmDB（mobi7 未压缩文本记录）
    /// 布局：PDB header(78B) + 记录表(3×8B + extra 2B) + 记录0（PalmDocHeader 16B +
    ///   MobiHeader 232B + 名字 8B） + 记录1（正文 HTML） + 记录2（尾部占位）
    /// 注：mobi crate 的 RawRecords::range 会排除末条记录（end = (b-1).min(len-1)），
    /// 故 first_non_book_index 需指向第 3 条（1..3 才包含记录 1）。
    #[test]
    fn test_parse_mobi_minimal_sample() {
        let html: &[u8] = "<html><body><p>第一章 起点</p><mbp:pagebreak/><p>内容一。</p><mbp:pagebreak/><p>第二章 成长</p><mbp:pagebreak/><p>内容二。</p></body></html>".as_bytes();
        let mut pdb = Vec::new();
        // PDB header（78B）：name(32) attributes(2) version(2) created(4) modified(4)
        //   backup(4) modnum(4) app_info(4) sort_info(4) type(4) creator(4) uid(4) next(4) num_records(2)
        pdb.extend_from_slice(b"TestBook\0");
        pdb.resize(32, 0);
        pdb.extend_from_slice(&[0, 0, 0, 0]); // attributes + version
        pdb.extend_from_slice(&[0u8; 12]); // created + modified + backup
        pdb.extend_from_slice(&[0u8; 8]); // modnum + app_info
        pdb.extend_from_slice(&[0u8; 4]); // sort_info
        pdb.extend_from_slice(b"BOOK"); // type
        pdb.extend_from_slice(b"READ"); // creator
        pdb.extend_from_slice(&[0u8; 8]); // uid + next
        pdb.extend_from_slice(&3u16.to_be_bytes()); // num_records
        assert_eq!(pdb.len(), 78);
        // 记录表：3 条（offset + id）+ extra_bytes(2)
        let rec0_off = 78 + 8 * 3 + 2; // 104
        let rec0_len = 16 + 232 + 8 + 8; // PalmDocHeader + MobiHeader + 填充 + 名字
        let rec1_off = rec0_off + rec0_len;
        let rec2_off = rec1_off + html.len();
        for off in [rec0_off, rec1_off, rec2_off] {
            pdb.extend_from_slice(&(off as u32).to_be_bytes());
            pdb.extend_from_slice(&[0u8; 4]);
        }
        pdb.extend_from_slice(&[0u8; 2]); // extra_bytes
        assert_eq!(pdb.len(), rec0_off);
        // 记录 0：PalmDocHeader（16B）——compression=1（No，未压缩）
        pdb.extend_from_slice(&1u16.to_be_bytes());
        pdb.extend_from_slice(&[0u8; 2]);
        pdb.extend_from_slice(&(html.len() as u32).to_be_bytes()); // text_length
        pdb.extend_from_slice(&3u16.to_be_bytes()); // record_count
        pdb.extend_from_slice(&4096u16.to_be_bytes()); // record_size
        pdb.extend_from_slice(&[0u8; 4]); // encryption(0) + unused
                                          // MobiHeader（232B）："MOBI" + header_length + 224B payload
        pdb.extend_from_slice(b"MOBI");
        pdb.extend_from_slice(&232u32.to_be_bytes());
        let mut mobi = vec![0u8; 224];
        let mut put = |off: usize, bytes: &[u8]| {
            mobi[off..off + bytes.len()].copy_from_slice(bytes);
        };
        put(0, &2u32.to_be_bytes()); // mobi_type = MobiPocketBook
        put(4, &65001u32.to_be_bytes()); // text_encoding = UTF-8
        put(56, &3u32.to_be_bytes()); // first_non_book_index（可读文本 = 1..3 → 记录 1）
        put(60, &256u32.to_be_bytes()); // name_offset（记录 0 内偏移）
        put(64, &8u32.to_be_bytes()); // name_length
        put(84, &3u32.to_be_bytes()); // first_image_index（无图片）
        put(168, &1u16.to_be_bytes()); // first_content_record
        pdb.extend_from_slice(&mobi);
        pdb.extend_from_slice(&[0u8; 8]); // 填充到 name_offset
        pdb.extend_from_slice(b"TestBook"); // 书名（8B）
        assert_eq!(pdb.len(), rec1_off);
        // 记录 1：正文 HTML
        pdb.extend_from_slice(html);
        // 记录 2：尾部占位（空）
        pdb.extend_from_slice(&[]);
        assert_eq!(pdb.len(), rec2_off);
        let book = parse_mobi(&pdb).unwrap();
        assert_eq!(book.meta.title, "TestBook");
        assert!(!book.chapters.is_empty(), "应解析出章节");
        let joined: String = book
            .chapters
            .iter()
            .map(|c| c.title.clone() + &c.content)
            .collect();
        assert!(
            joined.contains("内容一") && joined.contains("内容二"),
            "应提取到正文：{joined}"
        );
    }

    // ---------------- PDF ----------------

    /// 损坏数据：错误友好（提示 PDF）
    #[test]
    fn test_parse_pdf_garbage_friendly_error() {
        let err = parse_pdf(b"%PDF-1.4 this is not a real pdf").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("PDF"), "错误应提及 PDF：{msg}");
    }

    /// 页分章：无标题规则时按页分章（每页一章，空页跳过）
    #[test]
    fn test_chapters_from_pages_page_split() {
        let pages = vec!["第一页内容".into(), "".into(), "第二页内容".into()];
        let chapters = chapters_from_pages(pages);
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第 1 页", "第 3 页"]);
        assert_eq!(chapters[0].content, "第一页内容");
    }

    /// 标题分章：跨页出现“第一章/第二章”时按标题分章而非按页（标题前内容归入“正文”章）
    #[test]
    fn test_chapters_from_pages_title_split() {
        let pages = vec![
            "序言\n第一章 起点\n内容一。".into(),
            "第二章 成长\n内容二。".into(),
        ];
        let chapters = chapters_from_pages(pages);
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["正文", "第一章 起点", "第二章 成长"]);
    }

    /// PDF 元数据字符串解码（UTF-16BE / UTF-8 BOM / 纯 ASCII / 错误）
    #[test]
    fn test_pdf_meta_string_decode() {
        use lopdf::Object;
        let utf16 = Object::String(
            b"\xFE\xFF\x00T\x00e\x00s\x00t".to_vec(),
            lopdf::StringFormat::Literal,
        );
        assert_eq!(pdf_meta_string(Ok(&utf16)), "Test");
        let utf8 = Object::String(
            b"\xEF\xBB\xBF\xE4\xB9\xA6".to_vec(),
            lopdf::StringFormat::Literal,
        );
        assert_eq!(pdf_meta_string(Ok(&utf8)), "书");
        let plain = Object::String(b"plain".to_vec(), lopdf::StringFormat::Literal);
        assert_eq!(pdf_meta_string(Ok(&plain)), "plain");
        assert_eq!(pdf_meta_string(Err(lopdf::Error::DictKey("x".into()))), "");
    }

    // ---------------- FB2 ----------------

    /// 小样本 FB2：description（书名/作者/简介）+ body 两个 section → 分章
    #[test]
    fn test_parse_fb2_sample() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" lang="zh">
  <description>
    <title-info>
      <genre>fantasy</genre>
      <author><first-name>刘</first-name><last-name>慈欣</last-name></author>
      <book-title>三体</book-title>
      <annotation><p>黑暗森林法则。</p></annotation>
    </title-info>
  </description>
  <body>
    <section>
      <title><p>第一章 起点</p></title>
      <p>内容一。</p>
    </section>
    <section>
      <title><p>第二章 成长</p></title>
      <p>内容二。</p>
      <p>第二段。</p>
    </section>
  </body>
</FictionBook>"#;
        let book = parse_fb2(xml.as_bytes()).unwrap();
        assert_eq!(book.meta.title, "三体");
        assert_eq!(book.meta.author, "刘 慈欣");
        assert_eq!(book.meta.language.as_deref(), Some("zh"));
        assert!(book
            .meta
            .description
            .as_deref()
            .unwrap()
            .contains("黑暗森林"));
        assert_eq!(book.format, "fb2");
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长"]);
        assert!(
            book.chapters[1].content.contains("内容二")
                && book.chapters[1].content.contains("第二段")
        );
    }

    /// FB2 实体引用（&amp; 等）正确解码
    #[test]
    fn test_parse_fb2_entities() {
        let xml = r#"<FictionBook><description><title-info><book-title>A &amp; B</book-title></title-info></description><body><section><title><p>第 1 节</p></title><p>1 &lt; 2 &amp;&amp; 3</p></section></body></FictionBook>"#;
        let book = parse_fb2(xml.as_bytes()).unwrap();
        assert_eq!(book.meta.title, "A & B");
        assert!(book.chapters[0].content.contains("1 < 2 && 3"));
    }

    /// FB2 损坏/空数据：错误友好
    #[test]
    fn test_parse_fb2_garbage_friendly_error() {
        // 空 body（无 section）→ “未解析到章节内容”
        let err = parse_fb2(b"<FictionBook><description/><body/></FictionBook>").unwrap_err();
        assert!(format!("{err:#}").contains("FB2"), "应提示 FB2");
        // 标签不匹配 → XML 解析错误
        let err = parse_fb2(b"<FictionBook><body><section></FictionBook>").unwrap_err();
        assert!(format!("{err:#}").contains("FB2"), "应提示 FB2");
    }

    // ---------------- DOCX ----------------

    /// 小样本 DOCX：内存构造 zip（word/document.xml 含 Heading1 段落）→ 标题样式分章
    #[test]
    fn test_parse_docx_sample() {
        use std::io::Write;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第一章 起点</w:t></w:r></w:p>
    <w:p><w:r><w:t>内容一。</w:t></w:r></w:p>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第二章 成长</w:t></w:r></w:p>
    <w:p><w:r><w:t>内容二。</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let core_xml = r#"<?xml version="1.0"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>测试书</dc:title><dc:creator>作者甲</dc:creator></cp:coreProperties>"#;
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::default();
            zw.start_file("word/document.xml", opts).unwrap();
            zw.write_all(document_xml.as_bytes()).unwrap();
            zw.start_file("docProps/core.xml", opts).unwrap();
            zw.write_all(core_xml.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let book = parse_docx(&bytes).unwrap();
        assert_eq!(book.meta.title, "测试书");
        assert_eq!(book.meta.author, "作者甲");
        assert_eq!(book.format, "docx");
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长"]);
        assert_eq!(book.chapters[1].content, "内容二。");
    }

    /// DOCX 无标题样式：回退纯文本规则分章（第X章 段落）
    #[test]
    fn test_parse_docx_no_heading_falls_back_to_rules() {
        use std::io::Write;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>第一章 起点</w:t></w:r></w:p>
    <w:p><w:r><w:t>内容一。</w:t></w:r></w:p>
    <w:p><w:r><w:t>第二章 成长</w:t></w:r></w:p>
    <w:p><w:r><w:t>内容二。</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::default();
            zw.start_file("word/document.xml", opts).unwrap();
            zw.write_all(document_xml.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let book = parse_docx(&cursor.into_inner()).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长"]);
    }

    /// DOCX 损坏数据：错误友好
    #[test]
    fn test_parse_docx_garbage_friendly_error() {
        let err = parse_docx(b"not a zip").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("DOCX") || msg.contains("zip"),
            "错误应提及 DOCX/zip：{msg}"
        );
        // 合法 zip 但缺 document.xml
        use std::io::Write;
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            zw.start_file("other.txt", zip::write::FileOptions::default())
                .unwrap();
            zw.write_all(b"x").unwrap();
            zw.finish().unwrap();
        }
        let err = parse_docx(&cursor.into_inner()).unwrap_err();
        assert!(
            format!("{err:#}").contains("document.xml"),
            "应提示缺少 document.xml"
        );
    }

    /// 扩展名工具与白名单
    #[test]
    fn test_file_ext_and_whitelist() {
        assert_eq!(file_ext("book.PDF"), "pdf");
        assert_eq!(file_ext("book.azw3"), "azw3");
        assert_eq!(file_ext("book"), "");
        assert!(SUPPORTED_EXTENSIONS.contains(&"mobi"));
        assert!(SUPPORTED_EXTENSIONS.contains(&"fb2"));
        assert!(SUPPORTED_EXTENSIONS.contains(&"docx"));
        assert!(SUPPORTED_EXTENSIONS.contains(&"cbz"));
        assert!(SUPPORTED_EXTENSIONS.contains(&"umd"));
        assert!(is_local_book("storage/data/x/book.mobi", ""));
        assert!(is_local_book("C:/tmp/book.fb2", ""));
        assert!(is_local_book("C:/tmp/book.cbz", ""));
        assert!(is_local_book("C:/tmp/book.umd", ""));
        assert!(!is_local_book("https://a.com/book", ""));
    }

    // ---------- MOBI/AZW3 导入健壮性（构造最小合法 PalmDB+MOBI 文件往返） ----------

    /// 构造最小合法 KF7 MOBI：PalmDB 头 + 3 记录（记录 0 = PalmDoc 头 + MOBI 头 + EXTH + 书名；
    /// 记录 1 = 文本；记录 2 = 尾部占位（mobi crate 的 range 切片不含末记录——与真实文件
    /// 尾部图片/索引记录布局一致）），无压缩。EXTH 位于记录 0 内（MOBI 头之后、full name 之前）
    fn build_mini_mobi(title: &str, author: &str, text: &str) -> Vec<u8> {
        build_mini_mobi_raw(title, author, text.as_bytes(), 65001, 1)
    }

    /// 可指定正文原始字节 / 声明编码 / 压缩方式的最小 MOBI（回归测试编码探测用）
    fn build_mini_mobi_raw(
        title: &str,
        author: &str,
        text: &[u8],
        encoding: u32,
        compression: u16,
    ) -> Vec<u8> {
        // EXTH（记录 0 内，MOBI 头之后）
        let mut exth: Vec<u8> = Vec::new();
        exth.extend_from_slice(b"EXTH");
        exth.extend_from_slice(&12u32.to_be_bytes());
        exth.extend_from_slice(&2u32.to_be_bytes());
        exth.extend_from_slice(&100u32.to_be_bytes());
        exth.extend_from_slice(&((8 + author.len()) as u32).to_be_bytes());
        exth.extend_from_slice(author.as_bytes());
        exth.extend_from_slice(&503u32.to_be_bytes());
        exth.extend_from_slice(&((8 + title.len()) as u32).to_be_bytes());
        exth.extend_from_slice(title.as_bytes());
        if exth.len() % 2 == 1 {
            exth.push(0);
        }
        let name_offset = 16 + 232 + exth.len();

        // 记录 0 = PalmDoc 头（16B）+ MOBI 头（232B）+ EXTH + full name
        let mut rec0: Vec<u8> = Vec::new();
        // PalmDoc 头（mobi crate 在记录 0 起始处读取）
        rec0.extend_from_slice(&compression.to_be_bytes());
        rec0.extend_from_slice(&0u16.to_be_bytes()); // unused
        rec0.extend_from_slice(&(text.len() as u32).to_be_bytes()); // text_length
        rec0.extend_from_slice(&1u16.to_be_bytes()); // record_count（1 条文本记录）
        rec0.extend_from_slice(&4096u16.to_be_bytes()); // record_size
        rec0.extend_from_slice(&0u16.to_be_bytes()); // encryption = 无
        rec0.extend_from_slice(&0u16.to_be_bytes()); // unused
        rec0.extend_from_slice(b"MOBI");
        rec0.extend_from_slice(&232u32.to_be_bytes());
        let mut f: Vec<u8> = Vec::new();
        f.extend_from_slice(&2u32.to_be_bytes()); // mobi_type = book
        f.extend_from_slice(&encoding.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes()); // id
        f.extend_from_slice(&6u32.to_be_bytes()); // gen version
        f.extend_from_slice(&0xFFFFFFFFu32.to_be_bytes()); // ortho index
        f.extend_from_slice(&0u32.to_be_bytes()); // inflect index
        f.extend_from_slice(&0u32.to_be_bytes()); // index names
        f.extend_from_slice(&0u32.to_be_bytes()); // index keys
        for _ in 0..6 {
            f.extend_from_slice(&0xFFFFFFFFu32.to_be_bytes()); // extra indices
        }
        f.extend_from_slice(&3u32.to_be_bytes()); // first non-book index（记录 3）
        f.extend_from_slice(&(name_offset as u32).to_be_bytes()); // full name offset（记录 0 内）
        f.extend_from_slice(&(title.len() as u32).to_be_bytes()); // full name length
        f.extend_from_slice(&0u16.to_be_bytes()); // unused
        f.push(0); // locale
        f.push(9); // language code
        f.extend_from_slice(&0u32.to_be_bytes()); // input language
        f.extend_from_slice(&0u32.to_be_bytes()); // output language
        f.extend_from_slice(&6u32.to_be_bytes()); // format version
        f.extend_from_slice(&0xFFFFFFFFu32.to_be_bytes()); // first image index
        f.extend_from_slice(&0u32.to_be_bytes()); // huff record offset
        f.extend_from_slice(&0u32.to_be_bytes()); // huff record count
        f.extend_from_slice(&0u32.to_be_bytes()); // huff table offset
        f.extend_from_slice(&0u32.to_be_bytes()); // huff table length
        f.extend_from_slice(&0x40u32.to_be_bytes()); // EXTH flags on
        f.extend_from_slice(&[0u8; 32]); // unused_0
        f.extend_from_slice(&0u32.to_be_bytes()); // unused_1
        f.extend_from_slice(&0xFFFFFFFFu32.to_be_bytes()); // drm offset
        f.extend_from_slice(&0u32.to_be_bytes()); // drm count
        f.extend_from_slice(&0u32.to_be_bytes()); // drm size
        f.extend_from_slice(&0u32.to_be_bytes()); // drm flags
        f.extend_from_slice(&[0u8; 8]); // unused_2
        f.extend_from_slice(&1u16.to_be_bytes()); // first content record
        f.extend_from_slice(&1u16.to_be_bytes()); // last content record
        f.extend_from_slice(&0u32.to_be_bytes()); // unused_3
        f.extend_from_slice(&0xFFFFFFFFu32.to_be_bytes()); // fcis record
        f.extend_from_slice(&0u32.to_be_bytes()); // unused_4
        f.extend_from_slice(&0xFFFFFFFFu32.to_be_bytes()); // flis record
        f.extend_from_slice(&0u32.to_be_bytes()); // unused_5
        f.extend_from_slice(&0u64.to_be_bytes()); // unused_6
        f.extend_from_slice(&0u32.to_be_bytes()); // unused_7
        f.extend_from_slice(&0u32.to_be_bytes()); // compilation section count
        f.extend_from_slice(&0u32.to_be_bytes()); // data section count
        f.extend_from_slice(&0u32.to_be_bytes()); // unused_8
        f.extend_from_slice(&0u32.to_be_bytes()); // extra record data flags
        f.extend_from_slice(&0u32.to_be_bytes()); // first index record（0 = 无）
        assert_eq!(f.len(), 224, "MOBI 头字段应填满 232-8 字节");
        rec0.extend_from_slice(&f);
        rec0.extend_from_slice(&exth);
        rec0.extend_from_slice(title.as_bytes());
        if rec0.len() % 2 == 1 {
            rec0.push(0);
        }

        // 记录 1：文本（无压缩）；记录 2：尾部占位（空）
        let text_bytes = text;

        let rec0_off = 78 + 3 * 8 + 2; // PalmDB 头 + 记录表 + extra_bytes 字段
        let rec1_off = rec0_off + rec0.len();
        let rec2_off = rec1_off + text_bytes.len();

        let mut out: Vec<u8> = Vec::new();
        // PalmDB 头
        let mut name = [0u8; 32];
        let nb = title.as_bytes();
        name[..nb.len().min(32)].copy_from_slice(&nb[..nb.len().min(32)]);
        out.extend_from_slice(&name);
        out.extend_from_slice(&0u16.to_be_bytes()); // attributes
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        for _ in 0..6 {
            out.extend_from_slice(&0u32.to_be_bytes()); // 日期/序号/应用/排序
        }
        out.extend_from_slice(b"BOOK");
        out.extend_from_slice(b"MOBI");
        out.extend_from_slice(&0u32.to_be_bytes()); // uid seed
        out.extend_from_slice(&0u32.to_be_bytes()); // next record list
        out.extend_from_slice(&3u16.to_be_bytes()); // 记录数
                                                    // 记录表
        for off in [rec0_off, rec1_off, rec2_off] {
            out.extend_from_slice(&(off as u32).to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes()); // id
        }
        out.extend_from_slice(&0u16.to_be_bytes()); // extra bytes（mobi crate 读取）
                                                    // 记录体
        out.extend_from_slice(&rec0);
        out.extend_from_slice(text_bytes);
        out.push(0); // 占位记录（空）
        out
    }

    /// MOBI 导入：构造的最小 KF7 文件应完整读回（标题/作者/章节）
    #[test]
    fn parse_mobi_roundtrip() {
        let text = "简介内容\n第一章\n这里是正文一\n第二章\n这里是正文二";
        let bytes = build_mini_mobi("测试书", "作者甲", text);
        let book = parse_mobi(&bytes).expect("最小 MOBI 应可解析");
        assert_eq!(book.meta.title, "测试书");
        assert_eq!(book.meta.author, "作者甲");
        assert!(
            book.chapters.len() >= 2,
            "应分出章节: {:?}",
            book.chapters.len()
        );
        let ch1 = book
            .chapters
            .iter()
            .find(|c| c.title == "第一章")
            .expect("含第一章");
        assert!(ch1.content.contains("这里是正文一"));
        let ch2 = book
            .chapters
            .iter()
            .find(|c| c.title == "第二章")
            .expect("含第二章");
        assert!(ch2.content.contains("这里是正文二"));
        assert_eq!(book.format, "mobi");
    }

    /// AZW3 导入：同一容器经 parse_azw3 读回（KF8 兼容层）
    #[test]
    fn parse_azw3_roundtrip() {
        let bytes = build_mini_mobi("AZW3测试书", "作者乙", "内容简介\n第一章\n这里是正文A");
        let book = parse_azw3(&bytes).expect("最小 AZW3 应可解析");
        assert_eq!(book.meta.title, "AZW3测试书");
        assert_eq!(book.format, "azw3");
        assert!(!book.chapters.is_empty());
    }

    /// 非法输入：明确报错而非 panic
    #[test]
    fn parse_mobi_invalid_bytes_errors() {
        let err = parse_mobi(b"not a mobi file at all").unwrap_err();
        assert!(
            format!("{err:#}").contains("MOBI"),
            "应提示 MOBI 相关错误: {err:#}"
        );
        assert!(parse_azw3(b"").is_err());
    }

    /// PalmDoc LZ77：字面量 / 复制运行 / 空格异或三类指令
    #[test]
    fn palmdoc_decompress_instructions() {
        assert_eq!(palmdoc_decompress(b"AB\x02CD"), b"ABCD");
        // 两次 0xC1 展开后输出长于输入位置，距离对才有合法引用窗口
        assert_eq!(palmdoc_decompress(b"\xC1\xC1\x80\x08"), b" A AAAA");
        assert_eq!(palmdoc_decompress(b"\xC1"), b" A");
        assert_eq!(palmdoc_decompress(b"AB\x08CDEFGHIJ"), b"ABCDEFGHIJ");
    }

    /// 把任意字节编码为合法 PalmDoc 流：高位字节用 0x01 复制指令包裹
    fn palmdoc_compress(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match b {
                0x00 | 0x09..=0x7f => out.push(b),
                0x01..=0x08 | 0x80..=0xff => {
                    out.push(0x01);
                    out.push(b);
                }
            }
        }
        out
    }

    /// 声明编码为未知（2）的 GBK MOBI：不再按 UTF-8 lossy 产生乱码
    #[test]
    fn mobi_gbk_unknown_encoding_roundtrip() {
        let html = "<html><body><p>第一章 这里是正文一</p><p>第二章 这里是正文二</p></body></html>";
        let (gbk_bytes, _, _) = encoding_rs::GBK.encode(html);
        let bytes = build_mini_mobi_raw("测试书", "作者甲", &gbk_bytes, 2, 1);
        let book = parse_mobi(&bytes).expect("GBK MOBI 应可解析");
        assert_eq!(book.meta.title, "测试书");
        let joined: String = book
            .chapters
            .iter()
            .map(|c| format!("{}\n{}", c.title, c.content))
            .collect();
        assert!(joined.contains("这里是正文一"));
        assert!(joined.contains("这里是正文二"));
        assert!(!joined.contains('\u{fffd}'), "GBK 解码不应出现替换字符");
    }

    /// PalmDoc 压缩 + GBK 编码组合：先无损解压原始字节再探测编码
    #[test]
    fn mobi_gbk_palmdoc_compressed_roundtrip() {
        let html = "<html><body><p>第一章 测试正文</p></body></html>";
        let (gbk, _, _) = encoding_rs::GBK.encode(html);
        let compressed = palmdoc_compress(&gbk);
        let bytes = build_mini_mobi_raw("压缩书", "作者乙", &compressed, 2, 2);
        let book = parse_mobi(&bytes).expect("PalmDoc 压缩 GBK MOBI 应可解析");
        let joined: String = book.chapters.iter().map(|c| c.content.clone()).collect();
        assert!(joined.contains("测试正文"));
        assert!(
            !joined.contains('\u{fffd}'),
            "压缩 GBK 解码不应出现替换字符"
        );
    }

    /// KindleMOBI 在每条 4KB 文本记录尾部附加 trailing entry + multibyte overlap。
    /// 不清理时 PalmDoc 会把附加字节当指令解析，4KB 边界后出现乱码。
    #[test]
    fn mobi_palmdoc_trailing_data_flags_roundtrip() {
        let html = "<html><body><p>第一章 测试正文</p></body></html>";
        let (utf8, _, _) = encoding_rs::UTF_8.encode(html);
        let compressed = palmdoc_compress(&utf8);
        let mut bytes = build_mini_mobi_raw("尾部书", "作者丙", &compressed, 65001, 2);

        // extra_record_data_flags（u32）在记录 0 偏移 0xF0；置 3 = multibyte + 1 个 trailing entry
        let rec0_off = 78 + 3 * 8 + 2;
        let flags_off = rec0_off + 0xF0;
        bytes[flags_off..flags_off + 4].copy_from_slice(&3u32.to_be_bytes());

        // 布局：压缩正文 + multibyte overlap（2B）+ trailing entry（末 4B 编码长度 4）
        let rec1_off = u32::from_be_bytes(bytes[86..90].try_into().unwrap()) as usize;
        let trailer = [5u8, 1, 0, 0, 0, 4];
        let insert_at = rec1_off + compressed.len();
        bytes.splice(insert_at..insert_at, trailer.iter().copied());

        // 记录 2 偏移后移 6 字节
        let rec2_entry = 78 + 2 * 8;
        let rec2_off = u32::from_be_bytes(bytes[rec2_entry..rec2_entry + 4].try_into().unwrap())
            + trailer.len() as u32;
        bytes[rec2_entry..rec2_entry + 4].copy_from_slice(&rec2_off.to_be_bytes());

        let book = parse_mobi(&bytes).expect("带 trailing data 的 PalmDoc MOBI 应可解析");
        let joined: String = book.chapters.iter().map(|c| c.content.clone()).collect();
        assert!(joined.contains("测试正文"), "正文应完整：{joined}");
        assert!(
            !joined.contains('\u{fffd}'),
            "trailing data 不应产生替换字符"
        );
    }

    // ---------- CBZ（漫画压缩包） ----------

    /// 1x1 透明 PNG（最小合法 PNG 头）
    const PNG_1PX: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// 构造内存 CBZ：entries = (zip 内路径, 字节)
    fn build_cbz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::FileOptions::default();
            for (name, data) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// CBZ：图片页按文件名自然序成章；正文为 base64 data URI 图片标记；非图片/隐藏文件跳过
    #[test]
    fn parse_cbz_pages_and_natural_order() {
        let bytes = build_cbz(&[
            ("notes.txt", b"not an image"),
            (".DS_Store", b"hidden"),
            ("page/10.png", PNG_1PX),
            ("page/2.jpg", b"jpeg-bytes"),
            ("page/1.png", PNG_1PX),
            ("cover.jpg", b"cover-bytes"),
        ]);
        let book = parse_cbz(&bytes).expect("CBZ 解析成功");
        assert_eq!(book.format, "cbz");
        assert_eq!(book.meta.title, "", "CBZ 无内嵌元数据（书名由文件名兜底）");
        // 4 个图片页（notes/.DS_Store 跳过）；自然序：cover.jpg < 1.png < 2.jpg < 10.png
        // （'c' < 'p' 且数字段按数值：2 < 10）
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["cover.jpg", "1.png", "2.jpg", "10.png"]);
        // 正文 = markdown 图片 + base64 data URI，可还原原始字节
        let first = book
            .chapters
            .iter()
            .find(|c| c.title == "1.png")
            .expect("含 1.png 页");
        assert!(first.content.starts_with("![1.png](data:image/png;base64,"));
        use base64::Engine;
        let b64 = first
            .content
            .trim_start_matches("![1.png](data:image/png;base64,")
            .trim_end_matches(')');
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
            PNG_1PX,
            "base64 应还原原始图片字节"
        );
        let jpeg = book
            .chapters
            .iter()
            .find(|c| c.title == "2.jpg")
            .expect("含 2.jpg 页");
        assert!(jpeg.content.contains("data:image/jpeg;base64,"));
    }

    /// CBZ：ComicInfo.xml Title/Writer 作为书名/作者；zip 条目顺序首图作封面
    #[test]
    fn parse_cbz_comic_info_and_cover() {
        let bytes = build_cbz(&[
            (
                "ComicInfo.xml",
                "<ComicInfo><Title>海贼王</Title><Writer>尾田荣一郎</Writer></ComicInfo>"
                    .as_bytes(),
            ),
            ("0002.jpg", b"page-2"),
            ("0001.jpg", b"cover-bytes"),
        ]);
        let book = parse_cbz(&bytes).expect("CBZ 解析成功");
        assert_eq!(book.meta.title, "海贼王");
        assert_eq!(book.meta.author, "尾田荣一郎");
        // 封面 = zip 条目顺序首图（0002.jpg 先于 0001.jpg）
        assert_eq!(book.cover.as_deref(), Some(&b"page-2"[..]));
        // 章节仍按自然序：0001.jpg < 0002.jpg
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["0001.jpg", "0002.jpg"]);
    }

    /// legacy analyzeNameAuthor：书名/作者模式 + 回退清洗
    #[test]
    fn analyze_name_author_patterns() {
        // 《书名》+ 作者：xx
        assert_eq!(
            analyze_name_author("前缀《凡人修仙传》作者：忘语.txt"),
            ("凡人修仙传".to_string(), "前缀忘语".to_string())
        );
        // 书名 作者：xx
        assert_eq!(
            analyze_name_author("凡人修仙传 作者：忘语.txt"),
            ("凡人修仙传".to_string(), "忘语".to_string())
        );
        // 书名 by xx
        assert_eq!(
            analyze_name_author("Dune by Frank Herbert.txt"),
            ("Dune".to_string(), "Frank Herbert".to_string())
        );
        // 回退：去掉「作者 xx」「xx 著」后缀
        assert_eq!(
            analyze_name_author("三体 作者 刘慈欣.txt"),
            ("三体".to_string(), "刘慈欣".to_string())
        );
        assert_eq!(
            analyze_name_author("活着 余华 著.txt"),
            ("活着".to_string(), "余华".to_string())
        );
    }

    /// CBZ 错误路径：非 zip / zip 内无图片
    #[test]
    fn parse_cbz_errors() {
        let err = parse_cbz(b"not a zip").unwrap_err();
        assert!(
            format!("{err:#}").contains("zip"),
            "应提示 zip 错误: {err:#}"
        );
        let bytes = build_cbz(&[("readme.txt", b"hi")]);
        let err = parse_cbz(&bytes).unwrap_err();
        assert!(
            format!("{err:#}").contains("未找到图片"),
            "应提示无图片: {err:#}"
        );
    }

    /// 自然排序：数字段按数值；数字 < 字母（p2 < pa）；大小写不敏感
    #[test]
    fn natural_sort_order() {
        let mut v = vec![
            "page10.png",
            "page2.jpg",
            "page1.png",
            "Page2.png",
            "p2a.png",
            "p2b.png",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            vec![
                "p2a.png",
                "p2b.png",
                "page1.png",
                "page2.jpg",
                "Page2.png",
                "page10.png"
            ],
            "数字段按数值且数字先于字母"
        );
        let mut v2 = vec!["01.png", "1.png", "2.png"];
        v2.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v2,
            vec!["1.png", "01.png", "2.png"],
            "前导零等值时按完整串长稳定排序"
        );
    }

    // ---------- UMD ----------

    /// 构造 UMD（镜像 me.ag2s.umdlib UmdBook.buildUmd 布局）：魔数 + 头部 + 属性 +
    /// 0x0B 正文总长 + 0x83 偏移 + 0x84 标题 + zlib 正文块 + 0xF1 + 0x81 + 封面 + 0x0C
    fn build_umd(
        title: &str,
        author: &str,
        chapters: &[(&str, &str)],
        with_cover: bool,
    ) -> Vec<u8> {
        use std::io::Write;
        let utf16 =
            |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect() };
        let mut out = Vec::new();
        out.extend_from_slice(&[0x89, 0x9B, 0x9A, 0xDE]);
        // 头部 section：'#' + 0x01 00 00 08 + 类型(1=文本) + 2 随机
        out.extend_from_slice(&[0x23, 0x01, 0x00, 0x00, 0x08, 0x01, 0x12, 0x34]);
        // 属性：'#' + 类型 + 00 00 + (长度+5) + UTF-16LE
        let mut prop = |t: u8, s: &str| {
            let b = utf16(s);
            out.extend_from_slice(&[0x23, t, 0x00, 0x00, (b.len() + 5) as u8]);
            out.extend_from_slice(&b);
        };
        prop(0x02, title);
        prop(0x03, author);
        prop(0x04, "2013");
        prop(0x05, "8");
        prop(0x06, "8");
        prop(0x07, "都市");
        prop(0x08, "某出版社");
        // 正文（UTF-16LE 拼接）+ 章节偏移
        let mut contents = Vec::new();
        let mut offsets = Vec::new();
        for (_, c) in chapters {
            offsets.push(contents.len());
            let b = utf16(c);
            contents.extend_from_slice(&b);
        }
        out.extend_from_slice(&[0x23, 0x0B, 0x00, 0x00, 0x09]);
        out.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        // 0x83 章节偏移：'#' 83 00 00 09 + 随机 + '$' + 同随机 + (4n+9) + n×偏移
        out.extend_from_slice(&[0x23, 0x83, 0x00, 0x00, 0x09]);
        out.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        out.push(0x24);
        out.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        out.extend_from_slice(&((offsets.len() * 4 + 9) as u32).to_le_bytes());
        for o in &offsets {
            out.extend_from_slice(&(*o as u32).to_le_bytes());
        }
        // 0x84 标题：'#' 84 00 01 09 + 随机 + '$' + 同随机 + (总长+9) + 每章(1B 长 + UTF-16LE)
        out.extend_from_slice(&[0x23, 0x84, 0x00, 0x01, 0x09]);
        out.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        out.push(0x24);
        out.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let mut title_bytes = Vec::new();
        for (t, _) in chapters {
            let b = utf16(t);
            title_bytes.push(b.len() as u8);
            title_bytes.extend_from_slice(&b);
        }
        out.extend_from_slice(&((title_bytes.len() + 9) as u32).to_le_bytes());
        out.extend_from_slice(&title_bytes);
        // 正文块：'$' + 随机 + (压缩长+9) + zlib 压缩数据
        let chunk_rb = [0x55u8, 0x66, 0x77, 0x88];
        out.push(0x24);
        out.extend_from_slice(&chunk_rb);
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&contents).unwrap();
        let compressed = enc.finish().unwrap();
        out.extend_from_slice(&((compressed.len() + 9) as u32).to_le_bytes());
        out.extend_from_slice(&compressed);
        // '# F1 00 00 15' + 16 字节许可证
        out.extend_from_slice(&[0x23, 0xF1, 0x00, 0x00, 0x15]);
        out.extend_from_slice(&[0u8; 16]);
        // '# 81 00 01 09' + 0 + '$' + 0 + (4+9) + 块随机字节
        out.extend_from_slice(&[0x23, 0x81, 0x00, 0x01, 0x09]);
        out.extend_from_slice(&[0u8; 4]);
        out.push(0x24);
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&(4u32 + 9).to_le_bytes());
        out.extend_from_slice(&chunk_rb);
        // 封面：'#' 82 00 01 0A 01 + 随机 + '$' + 同随机 + (长+9) + 图片字节
        if with_cover {
            out.extend_from_slice(&[0x23, 0x82, 0x00, 0x01, 0x0A, 0x01]);
            out.extend_from_slice(&[0x21, 0x43, 0x65, 0x87]);
            out.push(0x24);
            out.extend_from_slice(&[0x21, 0x43, 0x65, 0x87]);
            out.extend_from_slice(&((PNG_1PX.len() + 9) as u32).to_le_bytes());
            out.extend_from_slice(PNG_1PX);
        }
        // 结束：'#' 0C 00 01 09 + 文件总长
        out.extend_from_slice(&[0x23, 0x0C, 0x00, 0x01, 0x09]);
        out.extend_from_slice(&((out.len() + 4) as u32).to_le_bytes());
        out
    }

    /// UMD：属性（标题/作者/题材/出版时间/出版商）+ 多章标题与正文（含 U+2029 换行）+ 封面
    #[test]
    fn parse_umd_roundtrip() {
        let chapters = [
            ("第一章", "第一段正文。\u{2029}第二段正文。"),
            ("第二章", "第二章正文内容。"),
        ];
        let bytes = build_umd("测试书", "测试作者", &chapters, true);
        let book = parse_umd(&bytes).expect("UMD 解析成功");
        assert_eq!(book.format, "umd");
        assert_eq!(book.meta.title, "测试书");
        assert_eq!(book.meta.author, "测试作者");
        assert_eq!(book.meta.published_at.as_deref(), Some("2013-8-8"));
        assert_eq!(book.meta.publisher.as_deref(), Some("某出版社"));
        assert!(book.meta.subjects.iter().any(|s| s == "都市"));
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "第一章");
        assert_eq!(
            book.chapters[0].content, "第一段正文。\n第二段正文。",
            "U+2029 应替换为换行"
        );
        assert_eq!(book.chapters[1].title, "第二章");
        assert_eq!(book.chapters[1].content, "第二章正文内容。");
        assert_eq!(book.cover.as_deref(), Some(PNG_1PX), "封面应提取");
    }

    /// 构造无 0x84 标题的 UMD 变体（有 0x83 偏移 + 正文块）：验证单章兜底
    fn build_umd_no_titles(title: &str, content: &str) -> Vec<u8> {
        use std::io::Write;
        let utf16 =
            |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect() };
        let mut out = Vec::new();
        out.extend_from_slice(&[0x89, 0x9B, 0x9A, 0xDE]);
        out.extend_from_slice(&[0x23, 0x01, 0x00, 0x00, 0x08, 0x01, 0x12, 0x34]);
        let tb = utf16(title);
        out.extend_from_slice(&[0x23, 0x02, 0x00, 0x00, (tb.len() + 5) as u8]);
        out.extend_from_slice(&tb);
        let cb = utf16(content);
        out.extend_from_slice(&[0x23, 0x0B, 0x00, 0x00, 0x09]);
        out.extend_from_slice(&(cb.len() as u32).to_le_bytes());
        // 0x83：1 个偏移（0）
        out.extend_from_slice(&[0x23, 0x83, 0x00, 0x00, 0x09]);
        out.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        out.push(0x24);
        out.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        out.extend_from_slice(&(4u32 + 9).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        // 0x84：仅段头（无标题附加块）——正文块直接挂其后
        out.extend_from_slice(&[0x23, 0x84, 0x00, 0x01, 0x09]);
        out.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        // 正文块
        let chunk_rb = [0x55u8, 0x66, 0x77, 0x88];
        out.push(0x24);
        out.extend_from_slice(&chunk_rb);
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&cb).unwrap();
        let compressed = enc.finish().unwrap();
        out.extend_from_slice(&((compressed.len() + 9) as u32).to_le_bytes());
        out.extend_from_slice(&compressed);
        out.extend_from_slice(&[0x23, 0xF1, 0x00, 0x00, 0x15]);
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&[0x23, 0x81, 0x00, 0x01, 0x09]);
        out.extend_from_slice(&[0u8; 4]);
        out.push(0x24);
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&(4u32 + 9).to_le_bytes());
        out.extend_from_slice(&chunk_rb);
        out.extend_from_slice(&[0x23, 0x0C, 0x00, 0x01, 0x09]);
        out.extend_from_slice(&((out.len() + 4) as u32).to_le_bytes());
        out
    }

    /// UMD：无 0x84 标题但有正文 → 单章兜底；坏魔数/截断/漫画型 → 明确报错
    #[test]
    fn parse_umd_variants_and_errors() {
        // 单章兜底：无 0x84 标题、有正文块
        let bytes = build_umd_no_titles("兜底书", "正文内容");
        let book = parse_umd(&bytes).expect("变体应可解析");
        assert_eq!(book.chapters.len(), 1);
        assert_eq!(book.chapters[0].title, "兜底书", "无标题时用书名兜底");
        assert!(book.chapters[0].content.contains("正文内容"));

        // 坏魔数
        let err = parse_umd(b"").unwrap_err();
        assert!(format!("{err:#}").contains("UMD") || format!("{err:#}").contains("过短"));
        let mut bad = build_umd("t", "a", &[], false);
        bad[0] = 0x00;
        let err = parse_umd(&bad).unwrap_err();
        assert!(
            format!("{err:#}").contains("魔数"),
            "应提示魔数错误: {err:#}"
        );

        // 截断（正文块中间切断）→ 报错
        let full = build_umd("t", "a", &[("c", "内容")], false);
        let cut = &full[..full.len() - 10];
        assert!(parse_umd(cut).is_err(), "截断文件应报错");

        // 漫画型（umdType=2）→ 明确拒绝
        let mut comic = build_umd("t", "a", &[], false);
        comic[9] = 0x02;
        let err = parse_umd(&comic).unwrap_err();
        assert!(
            format!("{err:#}").contains("漫画"),
            "应提示漫画不支持: {err:#}"
        );
    }

    /// 真实 UMD 样本回归（样本在 target/search-test/samples/——缺失时跳过）：
    /// 明朝那些事儿（7 章）/ 天涯-青春疼痛小说（单章 + 封面）
    #[test]
    fn parse_real_umd_samples() {
        let samples = [
            (
                "C:/Users/chong/pr-review/reader-dev/target/search-test/samples/明朝那些事儿.umd",
                "明朝那些事儿（1-7全套）终极版",
                7usize,
            ),
            (
                "C:/Users/chong/pr-review/reader-dev/target/search-test/samples/用左手爱你.umd",
                "青春疼痛小说：用左手爱你",
                1usize,
            ),
        ];
        for (path, title, n) in samples {
            let Ok(bytes) = std::fs::read(path) else {
                eprintln!("跳过（样本缺失）: {path}");
                continue;
            };
            let book = parse_umd(&bytes).expect("真实 UMD 样本解析成功");
            assert_eq!(book.meta.title, title);
            assert_eq!(book.chapters.len(), n, "{title} 章节数");
            assert!(
                book.chapters.iter().all(|c| !c.content.is_empty()),
                "章节正文不应为空"
            );
            assert!(
                book.cover.is_some() && !book.cover.as_ref().unwrap().is_empty(),
                "样本应带封面"
            );
        }
    }

    /// 分派：cbz/umd 走对应解析器；白名单生效
    #[test]
    fn test_parse_file_bytes_cbz_umd_dispatch() {
        // cbz：合法 zip 无图片 → CBZ 解析器错误（而非“不支持的格式”）
        let zip_bytes = build_cbz(&[("a.txt", b"x")]);
        let err =
            parse_file_bytes(&zip_bytes, "cbz", &[], DEFAULT_EPUB_TOC_MODE, false).unwrap_err();
        assert!(
            format!("{err:#}").contains("未找到图片"),
            "cbz 应走 parse_cbz: {err:#}"
        );
        // umd：坏文件 → UMD 解析器错误（魔数/长度）
        let err =
            parse_file_bytes(b"garbage", "umd", &[], DEFAULT_EPUB_TOC_MODE, false).unwrap_err();
        assert!(format!("{err:#}").contains("UMD") || format!("{err:#}").contains("过短"));
        // 未知扩展名仍拒绝
        let err = parse_file_bytes(b"x", "rar", &[], DEFAULT_EPUB_TOC_MODE, false).unwrap_err();
        assert!(format!("{err:#}").contains("不支持的格式"));
    }

    // ---------------- P1-C3：解压炸弹防护 ----------------

    /// zip 单条目输出上限：超限拒绝（测试用小上限验证超限路径）
    #[test]
    fn zip_entry_oversize_rejected() {
        let big = vec![0u8; 300 * 1024]; // 300KB 条目
        let bytes = build_cbz(&[("big.bin", &big)]);
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).unwrap();
        let err = read_zip_limited(&mut zip, "big.bin", 100_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("超出大小上限"), "超限应拒绝: {err}");
        // 未超限（同条目用更大上限）→ 正常读取
        let ok = read_zip_limited(&mut zip, "big.bin", 400_000).unwrap();
        assert_eq!(ok.len(), 300 * 1024);
    }

    /// CBZ 累计输出上限：多条目合计超限拒绝；未超限正常
    #[test]
    fn cbz_total_oversize_rejected() {
        let page = vec![0x89u8; 60 * 1024]; // 60KB/页
        let bytes = build_cbz(&[("p1.jpg", &page), ("p2.jpg", &page), ("p3.jpg", &page)]);
        let err = parse_cbz_impl(&bytes, 100_000).unwrap_err().to_string();
        assert!(err.contains("累计超出"), "CBZ 累计超限应拒绝: {err}");
        // 默认上限（500MB）下正常解析
        let book = parse_cbz(&bytes).unwrap();
        assert_eq!(book.chapters.len(), 3);
    }

    /// UMD 正文 zlib 解压炸弹：小压缩块解出超限正文 → 拒绝；未超限正常
    #[test]
    fn umd_zlib_bomb_rejected() {
        let big = "字".repeat(200_000); // UTF-16LE ≈ 400KB
        let bytes = build_umd("炸弹书", "作者", &[("第一章", &big)], false);
        let err = parse_umd_impl(&bytes, 50_000).unwrap_err().to_string();
        assert!(err.contains("超出大小上限"), "UMD 解压炸弹应拒绝: {err}");
        // 默认上限（500MB）下正常解析
        let book = parse_umd(&bytes).unwrap();
        assert_eq!(book.chapters.len(), 1);
        assert!(book.chapters[0].content.len() > 100_000);
    }

    /// 构造最小 PalmDB（记录 0 = PalmDocHeader + MOBI 头；可选 HUFF/CDIC 记录）
    /// 供 validate_mobi_lengths 测试——text_length/记录容量/CDIC 短语数炸弹各变体
    fn build_pdb_for_validation(
        compression: u16,
        text_length: u32,
        record_count: u16,
        record_size: u16,
        cdic: Option<(u32, u32)>, // (num_phrases, bits)
    ) -> Vec<u8> {
        let rec0_len = 16 + 232; // PalmDocHeader(16) + MOBI 头（"MOBI"+len+224 payload）
        let n_records = if cdic.is_some() { 3 } else { 1 };
        let rec_list = 78 + n_records * 8;
        let rec1_off = rec_list + rec0_len;
        let rec2_off = rec1_off + 16; // HUFF 记录 16B
        let mut pdb = Vec::new();
        pdb.extend_from_slice(b"TestBook\0");
        pdb.resize(32, 0);
        pdb.extend_from_slice(&[0u8; 44]);
        pdb.extend_from_slice(&(n_records as u16).to_be_bytes());
        assert_eq!(pdb.len(), 78, "PalmDB 头应 78B");
        let mut put_rec = |off: usize| {
            pdb.extend_from_slice(&(off as u32).to_be_bytes());
            pdb.extend_from_slice(&[0u8; 4]);
        };
        put_rec(rec_list);
        if cdic.is_some() {
            put_rec(rec1_off);
            put_rec(rec2_off);
        }
        // 记录 0：PalmDocHeader（16B）
        pdb.extend_from_slice(&compression.to_be_bytes());
        pdb.extend_from_slice(&[0u8; 2]);
        pdb.extend_from_slice(&text_length.to_be_bytes());
        pdb.extend_from_slice(&record_count.to_be_bytes());
        pdb.extend_from_slice(&record_size.to_be_bytes());
        pdb.extend_from_slice(&[0u8; 4]);
        // MOBI 头（16 + 224）
        pdb.extend_from_slice(b"MOBI");
        pdb.extend_from_slice(&232u32.to_be_bytes());
        let mut payload = vec![0u8; 224];
        if cdic.is_some() {
            // 与 mobi crate 一致：remaining-header[88..92] = first_huff_record（abs MOBI+96）
            payload[88..92].copy_from_slice(&1u32.to_be_bytes()); // first_huff_record = 1
            payload[92..96].copy_from_slice(&2u32.to_be_bytes()); // huff_record_count = 2
        }
        pdb.extend_from_slice(&payload);
        if let Some((num_phrases, bits)) = cdic {
            // 记录 1：HUFF（16B）
            pdb.extend_from_slice(b"HUFF");
            pdb.extend_from_slice(&0x18u32.to_be_bytes());
            pdb.extend_from_slice(&16u32.to_be_bytes());
            pdb.extend_from_slice(&16u32.to_be_bytes());
            // 记录 2：CDIC（16B 头）——num_phrases @+8、bits @+12
            pdb.extend_from_slice(b"CDIC");
            pdb.extend_from_slice(&0x10u32.to_be_bytes());
            pdb.extend_from_slice(&num_phrases.to_be_bytes());
            pdb.extend_from_slice(&bits.to_be_bytes());
        }
        pdb
    }

    /// MOBI 解压前长度校验：良性文件放行；text_length / 记录容量 / CDIC 短语数炸弹拒绝
    #[test]
    fn mobi_length_validation_rejects_bombs() {
        // 良性（PalmDoc 未压缩）→ 放行
        assert!(validate_mobi_lengths(&build_pdb_for_validation(1, 1_000, 10, 100, None)).is_ok());
        // text_length 炸弹（声称 600MB > 500MB 上限）→ 拒绝
        let err = validate_mobi_lengths(&build_pdb_for_validation(
            2,
            600 * 1024 * 1024,
            10,
            100,
            None,
        ))
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("声称正文") && err.contains("超出上限"),
            "{err}"
        );
        // 记录容量炸弹（65535×65535 ≈ 4.3GB）→ 拒绝
        let err = validate_mobi_lengths(&build_pdb_for_validation(2, 1_000, 65535, 65535, None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("记录容量"), "{err}");
        // CDIC 短语数炸弹（num_phrases=0xFFFFFFFF、bits=31 → 2^31 条）→ 拒绝
        let err = validate_mobi_lengths(&build_pdb_for_validation(
            2,
            1_000,
            10,
            100,
            Some((0xFFFF_FFFF, 31)),
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("词典短语数"), "{err}");
        // CDIC 合法量级（1 万条）→ 放行
        assert!(validate_mobi_lengths(&build_pdb_for_validation(
            2,
            1_000,
            10,
            100,
            Some((10_000, 14))
        ))
        .is_ok());
        // 截断/损坏文件：静默放行（交给 mobi crate 报友好错误，不 panic）
        assert!(validate_mobi_lengths(&[0u8; 10]).is_ok());
        assert!(
            validate_mobi_lengths(&build_pdb_for_validation(1, 1_000, 10, 100, None)[..100])
                .is_ok()
        );
    }
}
