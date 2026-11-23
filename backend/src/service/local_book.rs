//! 本地书籍导入（仅支持核心 EPUB / TXT）
//!
//! - EPUB：zip 解包 → `META-INF/container.xml` → OPF 元数据 → spine/toc 章节（XHTML → 纯文本）→ 封面
//! - TXT：编码检测（UTF-8 / UTF-16 LE/BE / GBK）→ 章节标题正则智能分章 / 长章节拆分

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
    /// 格式（epub / txt）
    pub format: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Chapter {
    pub title: String,
    pub content: String,
}

/// 本地书上传时的书籍类型（0 = 文本）
pub fn local_book_type(_ext: &str) -> i64 {
    0
}

/// 从文件名解析书名/作者。
/// 按序尝试 `《书名》作者：xx`、`《书名》`、`书名 作者：xx`、`书名 by xx`，
/// 未命中时清洗「作者 xx」「xx 著」后缀。
pub fn analyze_name_author(file_name: &str) -> (String, String) {
    let stem = match file_name.rfind('.') {
        Some(idx) if idx > 0 => &file_name[..idx],
        _ => file_name,
    };
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

fn format_book_name(name: &str) -> String {
    let cleaned = crate::util::regex::Regex::new(r"\s+作\s*者.*|\s+\S+\s+著")
        .ok()
        .map(|re| re.replace_all(name, "").into_owned())
        .unwrap_or_else(|| name.to_string());
    cleaned.trim().to_string()
}

fn format_book_author(author: &str) -> String {
    let cleaned = crate::util::regex::Regex::new(r"^\s*作\s*者[:：\s]+|\s+著")
        .ok()
        .map(|re| re.replace_all(author, "").into_owned())
        .unwrap_or_else(|| author.to_string());
    cleaned.trim().to_string()
}

// ===========================================================================
// EPUB 解析
// ===========================================================================

/// EPUB 目录模式（toc / spin<toc / spin+toc / toc+spin / toc<spin）
pub const EPUB_TOC_MODES: &[&str] = &["toc", "spin<toc", "spin+toc", "toc+spin", "toc<spin"];

/// EPUB 目录默认模式
pub const DEFAULT_EPUB_TOC_MODE: &str = "spin+toc";

/// 字符串是否为合法 EPUB 目录模式
pub fn is_epub_toc_mode(s: &str) -> bool {
    EPUB_TOC_MODES.contains(&s)
}

/// EPUB 解析（支持 tocUrl 目录模式控制顺序与标题）
///
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
    let opf_str = String::from_utf8_lossy(&opf);
    let meta = parse_opf(&opf_str);

    // 3. spine/manifest 章节内容
    let spin_chapters = opf_chapters(&mut zip, &opf_path, &opf_str);

    // 4. 封面
    let cover = meta.cover_href.as_ref().and_then(|href| {
        let full = resolve_opf_path(&opf_path, href);
        read_zip(&mut zip, &full).ok()
    });

    // 5. toc 目录解析（toc.ncx EPUB2 / nav.xhtml EPUB3）并按模式合并
    let toc_entries = parse_epub_toc(&mut zip, &opf_path, &opf_str);
    let chapters = merge_epub_chapters(&mut zip, &toc_entries, spin_chapters, toc_mode);

    Ok(ImportedBook {
        meta,
        chapters,
        cover,
        format: "epub".into(),
    })
}

fn parse_epub_toc<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    opf_path: &str,
    opf_str: &str,
) -> Vec<(String, String)> {
    let manifest: std::collections::HashMap<String, (String, String)> = extract_manifest(opf_str);
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

fn nav_links(html: &str, nav_path: &str) -> Vec<(String, String)> {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse(r#"nav[epub\:type="toc"]"#)
        .or_else(|_| scraper::Selector::parse("nav"))
        .ok();
    let Some(sel) = sel else { return vec![] };
    let mut out = Vec::new();
    for nav in doc.select(&sel) {
        if let Ok(a_sel) = scraper::Selector::parse("a[href]") {
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

fn ncx_nav_points(xml: &str, ncx_path: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(open) = xml[pos..].find("<navPoint") {
        let open_abs = pos + open;
        let tag_end = xml[open_abs..]
            .find('>')
            .map(|i| open_abs + i + 1)
            .unwrap_or(xml.len());
        if xml[open_abs..tag_end].ends_with("/>") {
            pos = tag_end;
            continue;
        }
        let content_start = tag_end;
        let Some(close_rel) = xml[content_start..].find("</navPoint>") else {
            break;
        };
        let close_abs = content_start + close_rel;
        let inner = &xml[content_start..close_abs];
        let text = if let Some(ti) = inner.find("<text>") {
            let after = &inner[ti + 6..];
            after
                .find("</text>")
                .map(|te| after[..te].trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let src = if let Some(si) = inner.find("content") {
            let after = &inner[si + 7..];
            if let Some(q0) = after.find('"') {
                let rest = &after[q0 + 1..];
                rest.find('"')
                    .map(|q1| rest[..q1].to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        if !text.is_empty() && !src.is_empty() {
            let href = src.split('#').next().unwrap_or(&src);
            let full = resolve_opf_path(ncx_path, href);
            out.push((full, text));
        }
        pos = close_abs + "</navPoint>".len();
    }
    out
}

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
    if toc_entries.is_empty() {
        return spin_chapters.into_iter().map(|(_, c)| c).collect();
    }
    match mode {
        "spin<toc" | "spin+toc" => {
            let force = mode == "spin<toc";
            let title_map: std::collections::HashMap<&str, &str> = toc_entries
                .iter()
                .map(|(h, t)| (h.as_str(), t.as_str()))
                .collect();
            spin_chapters
                .into_iter()
                .map(|(href, mut c)| {
                    if let Some(t) = title_map.get(href.as_str()) {
                        if force || c.title.is_empty() {
                            c.title = (*t).to_string();
                        }
                    }
                    c
                })
                .collect()
        }
        "toc+spin" | "toc<spin" => {
            let force = mode == "toc<spin";
            let spin_titles: std::collections::HashMap<String, String> = spin_chapters
                .into_iter()
                .map(|(h, c)| (h, c.title))
                .collect();
            let mut out = Vec::new();
            for (href, title) in toc_entries {
                let mut title = title.clone();
                if let Some(st) = spin_titles.get(href.as_str()) {
                    if force || title.is_empty() {
                        title = st.clone();
                    }
                }
                if let Ok(bytes) = read_zip(zip, href) {
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
            spin_titles
                .into_values()
                .map(|t| Chapter {
                    title: t,
                    content: String::new(),
                })
                .collect()
        }
        _ => {
            let mut out = Vec::new();
            for (href, title) in toc_entries {
                if let Ok(bytes) = read_zip(zip, href) {
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

// ===========================================================================
// TXT 解析与智能分章
// ===========================================================================

/// TXT 解析（编码检测 + 分章；使用内置默认规则）
pub fn parse_txt(bytes: &[u8]) -> Result<ImportedBook> {
    parse_txt_with_rules(bytes, &[])
}

/// TXT 解析（编码检测 + 分章；rules 为空时用内置启用规则，否则用用户自定义规则）
pub fn parse_txt_with_rules(bytes: &[u8], user_rules: &[String]) -> Result<ImportedBook> {
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
    let text = text.trim_start_matches('\u{feff}').to_string();

    let rules: Vec<String> = if user_rules.is_empty() {
        default_toc_rule_regexes()
    } else {
        user_rules.to_vec()
    };
    let mut chapters = split_by_rules(&text, &rules);
    if chapters.is_empty() {
        chapters = chunk_fallback(&text);
    }

    let title = text.lines().next().unwrap_or("本地书籍").trim().to_string();
    let meta = OpfMeta {
        title,
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

/// 内置默认 TXT 目录规则定义
#[derive(Debug, Clone, Copy)]
pub struct DefaultTocRuleDef {
    pub name: &'static str,
    pub rule: &'static str,
    pub enable: bool,
    pub serial_number: i64,
}

/// 内置 18 条 TXT 目录规则（TXT 分章默认取 enable=true）
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
        rule: r"(?m)(?<=[ \t　]{0,4})第[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章.{0,30}$(?=[\s　]{0,8}第[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章)",
        enable: false,
        serial_number: 15,
    },
    DefaultTocRuleDef {
        name: "双标题(后向)",
        rule: r"(?m)(?<=[ \t　]{0,4}第[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章.{0,30}$[\s　]{0,8})第[\d〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章.{0,30}$",
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

/// 参与 TXT 分章的默认规则正则（只取启用规则）
pub fn default_toc_rule_regexes() -> Vec<String> {
    DEFAULT_TOC_RULE_DEFS
        .iter()
        .filter(|d| d.enable)
        .map(|d| d.rule.to_string())
        .collect()
}

/// 长章节拆分（单章 > 100KB 时按 ~10KB 在换行边界二次切分，子章标题 `{原标题}(n)`）
fn split_long_chapters(chapters: Vec<Chapter>, enabled: bool) -> Vec<Chapter> {
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
                match bytes[end..].iter().position(|&b| b == b'\n') {
                    Some(p) => end += p,
                    None => end = bytes.len(),
                }
            }
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

fn split_by_rules(text: &str, rules: &[String]) -> Vec<Chapter> {
    let mut best: Option<crate::util::regex::Regex> = None;
    let mut best_count = 0usize;
    for rule in rules {
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

/// 读 TXT 文件并分章
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

// ===========================================================================
// 格式白名单与统一分派
// ===========================================================================

/// 判断是否本地书（local:// 或文件路径型本地书）
pub fn is_local_book(book_url: &str, origin: &str) -> bool {
    book_url.starts_with("local://")
        || origin == "loc_book"
        || book_url.starts_with("storage/")
        || has_supported_ext(book_url)
}

/// 支持的本地书扩展名白名单（仅核心 EPUB + TXT）
pub const SUPPORTED_EXTENSIONS: &[&str] = &["epub", "txt"];

/// 取文件名/路径的小写扩展名（不含点；无扩展名返回空串）
pub fn file_ext(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn has_supported_ext(name: &str) -> bool {
    let ext = file_ext(name);
    !ext.is_empty() && SUPPORTED_EXTENSIONS.contains(&ext.as_str())
}

/// 按扩展名分派解析（仅支持 `epub` 与 `txt`）。
/// `toc_mode` 对 epub 生效；`split_long` 对 txt 生效。
pub fn parse_file_bytes(
    bytes: &[u8],
    ext: &str,
    user_rules: &[String],
    toc_mode: &str,
    split_long: bool,
) -> Result<ImportedBook> {
    match ext {
        "epub" => parse_epub(bytes, toc_mode),
        "txt" => {
            let mut imported = parse_txt_with_rules(bytes, user_rules)?;
            imported.chapters = split_long_chapters(imported.chapters, split_long);
            Ok(imported)
        }
        other => anyhow::bail!("不支持的格式：{other}（仅支持 epub / txt）"),
    }
}

/// 按文件扩展名分派解析（路径版本）
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

/// 兼容默认参数签名：默认 EPUB 目录模式、不拆长章节
pub fn parse_loc_book_path_default(
    path: &std::path::Path,
    user_rules: &[String],
) -> Result<ImportedBook> {
    parse_loc_book_path(path, user_rules, DEFAULT_EPUB_TOC_MODE, false)
}

// ===========================================================================
// Zip / XML / HTML 工具
// ===========================================================================

/// zip 单条目解压输出上限（防解压炸弹）
const MAX_ZIP_ENTRY_BYTES: u64 = 500 * 1024 * 1024;

fn read_zip<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    path: &str,
) -> Result<Vec<u8>> {
    read_zip_limited(zip, path, MAX_ZIP_ENTRY_BYTES)
}

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

fn extract_attr_simple(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let idx = xml
        .find(&format!("<{tag} "))
        .or_else(|| xml.find(&format!("<{tag}>")))?;
    let rest = &xml[idx..];
    let end = rest.find('>')?;
    let block = &rest[..end];
    attr_value(block, attr)
}

fn extract_all_attr(xml: &str, tag: &str, attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(idx) = rest.find(&format!("<{tag}")) {
        let after = &rest[idx..];
        let Some(end) = after.find('>') else { break };
        let block = &after[..end];
        if let Some(v) = attr_value(block, attr) {
            out.push(v);
        }
        rest = &after[end + 1..];
    }
    out
}

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

fn resolve_opf_path(opf_path: &str, href: &str) -> String {
    let href_clean = href.split('#').next().unwrap_or(href);
    if let Some(idx) = opf_path.rfind('/') {
        format!("{}/{}", &opf_path[..idx], href_clean)
    } else {
        href_clean.to_string()
    }
}

fn html_to_text(html: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let mut parts = Vec::new();
    for el in doc.root_element().descendants() {
        if let scraper::node::Node::Element(e) = el.value() {
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
        return doc
            .root_element()
            .text()
            .collect::<String>()
            .trim()
            .to_string();
    }
    parts.join("\n\n")
}

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
                r#"<html><head><title>第一章</title></head><body><h1>第一章</h1><p>内容一。</p></body></html>"#
                    .as_bytes(),
            )
            .unwrap();
            zip.start_file("OEBPS/c2.xhtml", opts).unwrap();
            zip.write_all(
                r#"<html><head><title>第二章</title></head><body><h1>第二章</h1><p>内容二。</p></body></html>"#
                    .as_bytes(),
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

    #[test]
    fn parse_epub_toc_modes() {
        let bytes = build_test_epub();
        let b = parse_epub(&bytes, DEFAULT_EPUB_TOC_MODE).unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章", "第二章"]);
        assert!(b.chapters[0].content.contains("内容一。"));

        let b = parse_epub(&bytes, "spin<toc").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["TOC-1", "TOC-2"]);

        let b = parse_epub(&bytes, "toc").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["TOC-2", "TOC-1"]);

        let b = parse_epub(&bytes, "toc<spin").unwrap();
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第二章", "第一章"]);

        assert!(is_epub_toc_mode("toc"));
        assert!(is_epub_toc_mode("spin<toc"));
        assert!(!is_epub_toc_mode(""));
    }

    #[test]
    fn test_supported_extensions_and_dispatch() {
        assert_eq!(SUPPORTED_EXTENSIONS, &["epub", "txt"]);
        assert!(is_local_book("books/a.epub", ""));
        assert!(is_local_book("books/a.txt", ""));
        assert!(!is_local_book("https://example.com/book.pdf", ""));
        assert!(parse_file_bytes(b"x", "mobi", &[], DEFAULT_EPUB_TOC_MODE, false).is_err());
        assert!(parse_file_bytes(b"x", "pdf", &[], DEFAULT_EPUB_TOC_MODE, false).is_err());
        assert!(parse_file_bytes(b"x", "cbz", &[], DEFAULT_EPUB_TOC_MODE, false).is_err());
    }

    #[test]
    fn test_split_long_chapters() {
        let big = "x\n".repeat(61_440);
        let chapters_len = big.len();
        let chapters = vec![Chapter {
            title: "长章".into(),
            content: big,
        }];
        assert_eq!(split_long_chapters(chapters.clone(), false).len(), 1);
        let split = split_long_chapters(chapters, true);
        assert!(split.len() >= 11);
        assert_eq!(split[0].title, "长章(1)");
        let total: usize = split.iter().map(|c| c.content.len()).sum();
        assert_eq!(total, chapters_len);
    }

    const SAMPLE: &str = "第一章 起点\n内容一。\n第二章 成长\n内容二。\n尾声\n结局。";

    #[test]
    fn test_parse_txt_default_and_custom_rules() {
        let book = parse_txt(SAMPLE.as_bytes()).unwrap();
        let titles: Vec<&str> = book.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长", "尾声"]);
        assert_eq!(book.chapters[1].content, "内容二。");

        let rules = vec![
            r"^\s*第\s*[0-9一二三四五六七八九十百千万零〇两]+\s*[章节卷回集部篇].*".to_string(),
        ];
        let custom = parse_txt_with_rules(SAMPLE.as_bytes(), &rules).unwrap();
        let titles: Vec<&str> = custom.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章 起点", "第二章 成长"]);
    }

    #[test]
    fn test_parse_txt_gbk_and_chunk_fallback() {
        let text = "第一章 测试\n内容。";
        let (gbk_bytes, _, _) = encoding_rs::GBK.encode(text);
        let book = parse_txt(&gbk_bytes).unwrap();
        assert_eq!(book.chapters[0].title, "第一章 测试");

        let body = "字".repeat(25_000);
        let chunked = parse_txt(body.as_bytes()).unwrap();
        assert_eq!(chunked.chapters.len(), 3);
    }

    #[test]
    fn test_default_toc_rule_defs() {
        assert_eq!(DEFAULT_TOC_RULE_DEFS.len(), 18);
        assert_eq!(default_toc_rule_regexes().len(), 10);
    }

    #[test]
    fn analyze_name_author_patterns() {
        assert_eq!(
            analyze_name_author("前缀《凡人修仙传》作者：忘语.txt"),
            ("凡人修仙传".to_string(), "前缀忘语".to_string())
        );
        assert_eq!(
            analyze_name_author("凡人修仙传 作者：忘语.txt"),
            ("凡人修仙传".to_string(), "忘语".to_string())
        );
        assert_eq!(
            analyze_name_author("Dune by Frank Herbert.txt"),
            ("Dune".to_string(), "Frank Herbert".to_string())
        );
        assert_eq!(
            analyze_name_author("三体 作者 刘慈欣.txt"),
            ("三体".to_string(), "刘慈欣".to_string())
        );
        assert_eq!(
            analyze_name_author("活着 余华 著.txt"),
            ("活着".to_string(), "余华".to_string())
        );
    }

    #[test]
    fn zip_entry_oversize_rejected() {
        use std::io::Write;
        let big = vec![0u8; 300 * 1024];
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            zip.start_file("big.bin", zip::write::FileOptions::default())
                .unwrap();
            zip.write_all(&big).unwrap();
            zip.finish().unwrap();
        }
        let bytes = buf.into_inner();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).unwrap();
        let err = read_zip_limited(&mut zip, "big.bin", 100_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("超出大小上限"), "超限应拒绝: {err}");
        let ok = read_zip_limited(&mut zip, "big.bin", 400_000).unwrap();
        assert_eq!(ok.len(), 300 * 1024);
    }
}
