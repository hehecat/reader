//! 正文抓取、规则清洗与 HTML→纯文本转换（ruleContent）

use anyhow::Result;
use serde::Deserialize;

use crate::model::BookSource;

use super::{expand_next_url, fetch_url, to_abs};

/// ruleContent 结构（legacy ContentRule）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContentRule {
    pub content: Option<String>,
    pub next_content_url: Option<String>,
    pub source_regex: Option<String>,
    pub replace_regex: Option<String>,
    pub init: Option<String>,
    pub pre_update_js: Option<String>,
}

/// 正文解析（ruleContent：content 字段 + sourceRegex 清洗 + 多页）
/// F12/AR4：`chapter_title`/`book_name` 为 @get:{title}/@get:{bookName} 内建回退源
/// P1 跨阶段：`book_url` 非空时双键合并读取（详情 @put 直达正文），结束双键回写
///
/// legacy 对齐：抓取报错标记运行期失效快照，成功则清除该源标记。
#[allow(clippy::too_many_arguments)]
pub async fn analyze_content(
    ns: &str,
    chapter_url: &str,
    source: &BookSource,
    max_pages: usize,
    chapter_title: Option<&str>,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<String> {
    match analyze_content_impl(
        ns,
        chapter_url,
        source,
        max_pages,
        chapter_title,
        book_name,
        book_url,
    )
    .await
    {
        Ok(v) => {
            crate::service::health::clear_source_invalid(ns, &source.book_source_url);
            Ok(v)
        }
        Err(e) => {
            crate::service::health::mark_source_invalid(
                ns,
                &source.book_source_url,
                &e.to_string(),
            );
            Err(e)
        }
    }
}

async fn analyze_content_impl(
    ns: &str,
    chapter_url: &str,
    source: &BookSource,
    max_pages: usize,
    chapter_title: Option<&str>,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current_url = chapter_url.to_string();
    // 详情/目录流程的 @put 变量按章节 URL 共享（analyze_toc 已逐章落盘）
    // P1 双键合并：book_url 级作底、章节级覆盖——直接取正文时详情阶段变量仍可见
    let mut vars = crate::parser::rule::load_book_vars_merged(
        ns,
        &source.book_source_url,
        book_url,
        chapter_url,
    );
    vars.chapter_title = chapter_title.map(str::to_string);
    vars.book_name = book_name.map(str::to_string);
    // E10/AR5：章节上下文 → JS 绑定 chapter.url / title（legacy setChapter）
    vars.chapter_url = Some(chapter_url.to_string());

    for _page in 0..max_pages {
        let resp = fetch_url(ns, &current_url, source).await?;
        let base = resp.url.clone();
        // E10/AR5：真实页 URL → JS 求值绑定 baseUrl
        vars.insert("baseUrl".to_string(), base.clone());
        let content = analyze_content_from_with_vars(&resp.body, source, &mut vars);
        if !content.is_empty() {
            parts.push(content);
        }

        let rule: ContentRule = source
            .rule_content
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let next = rule
            .next_content_url
            .as_deref()
            .map(|r| {
                crate::service::search::field_url_with_vars(
                    &resp.body,
                    Some(r),
                    "",
                    &base,
                    &mut vars,
                )
            })
            .unwrap_or_default();
        if next.is_empty() {
            break;
        }
        // E7：翻页 URL 过模板/JS 管线后再绝对化
        let next = expand_next_url(&next, &base, source, ns);
        current_url = to_abs(&next, &base);
        crate::parser::rule::save_book_vars(ns, &source.book_source_url, &current_url, &vars);
    }

    // P1 双键回写：章节键 + book_url 键（后续其他章节直接取正文时 book 级仍可见——
    // legacy 单 varMap 书级语义）
    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        chapter_url,
        &vars,
    );
    Ok(parts.join("\n"))
}

/// 单页正文解析（纯函数，可测）
///
/// GAP 97：规则提取结果原样返回——书源正文含 HTML 标签（@html 提取或 JSON 正文源
/// 直接携带 <p>/<br> 等）时不做剥离/转义，前端已有纯文本渲染负责展示。
/// GAP 109：contentReplace（legacy 命名）即 ruleContent.replaceRegex（`模式##替换`），
/// 与 sourceRegex（删除型）均在解析期应用——正文净化在 getBookContent 返回前完成。
pub fn analyze_content_from(html: &str, source: &BookSource) -> String {
    analyze_content_from_with_vars(html, source, &mut crate::parser::rule::RuleVars::new())
}

/// [`analyze_content_from`] 带变量版本（@put/@get 贯通详情→目录→正文）
pub fn analyze_content_from_with_vars(
    html: &str,
    source: &BookSource,
    vars: &mut crate::parser::rule::RuleVars,
) -> String {
    let rule: ContentRule = source
        .rule_content
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(content_rule) = rule.content.clone() else {
        return String::new();
    };
    let html = crate::parser::rule::apply_init_with_vars(html, rule.init.as_deref(), vars);
    // legado preUpdateJs：解析前 JS 预处理（result=抓取内容 → 返回新内容）
    let html = if let Some(js) = &rule.pre_update_js {
        if !js.trim().is_empty() {
            vars.insert("result".to_string(), html.clone());
            crate::parser::js::eval_js(js.trim(), vars).unwrap_or(html.clone())
        } else {
            html
        }
    } else {
        html
    };
    // E10（legacy AnalyzeRule.kt:661 bindings["src"]=当前解析文档）：
    // 正文内嵌 <js>/{{js}} 与 java.setContent 的 src 应为预处理后的文档本身而非书源地址
    vars.insert("src".to_string(), html.clone());
    let mut content = crate::service::search::field_with_vars(&html, Some(&content_rule), "", vars);
    // sourceRegex 清洗（legacy：正则移除干扰内容；GAP 153：lookbehind 经 fancy-regex）
    if let Some(sr) = &rule.source_regex {
        if !sr.is_empty() {
            match crate::util::regex::Regex::new(sr) {
                Ok(re) => content = re.replace_all(&content, "").to_string(),
                Err(e) => tracing::warn!("sourceRegex 编译失败（跳过清洗）: {e}"),
            }
        }
    }
    // replaceRegex 替换（legacy BookContent.kt:109-113 getString 语义）：
    // 支持 @get / {{...}} 内嵌模板展开、&& 多段链、前导 ##（书源常直接写 `##广告正则` 或 `##pat##rep`）、
    // ### 尾标仅首次替换；无 ## 的纯段视为删除型整段正则。空模式严禁执行以防逐字插入。
    if let Some(rr) = &rule.replace_regex {
        let resolved = crate::parser::rule::resolve_get(rr, vars);
        let expanded = if resolved.contains("{{") {
            crate::service::search::expand_embedded_with_vars(&resolved, &html, vars)
        } else {
            resolved
        };
        for raw_seg in expanded.split("&&") {
            let seg = raw_seg.trim();
            if seg.is_empty() {
                continue;
            }
            let seg = seg.strip_prefix("##").unwrap_or(seg);
            let mut parts = seg.splitn(3, "##");
            let Some(pat_raw) = parts.next() else {
                continue;
            };
            let pat = pat_raw.trim();
            if pat.is_empty() {
                continue;
            }
            let Some(rep) = parts.next() else {
                // 无 ##（或原串为 `##pat`）：整段为匹配删除正则
                match crate::util::regex::Regex::new(pat) {
                    Ok(re) => content = re.replace_all(&content, "").to_string(),
                    Err(e) => tracing::warn!("replaceRegex 段编译失败（跳过）: {e}"),
                }
                continue;
            };
            let replace_first = parts.next().map(|f| f.contains('#')).unwrap_or(false);
            match crate::util::regex::Regex::new(pat) {
                Ok(re) => {
                    content = if replace_first {
                        re.replace_first(&content, rep).to_string()
                    } else {
                        re.replace_all(&content, rep).to_string()
                    };
                }
                Err(e) => tracing::warn!("replaceRegex 段编译失败（跳过替换）: {e}"),
            }
        }
    }
    html_content_to_text(&content)
}

/// 正文 HTML → 纯文本（legado WebView 显示语义）。
///
/// 书源正文规则常用 `@html`，返回 `<p>/<br>/&nbsp;` 等 HTML；Reader Dev 前端按纯文本
/// 渲染，原样返回会把这些标签/实体显示在正文里。此处把换行元素转 `\n`、常见实体解码、
/// 其余标签剥离。纯文本正文（无 `<`/`&`）原样返回，不引入额外差异。
pub(crate) fn html_content_to_text(content: &str) -> String {
    if !content.contains('<') && !content.contains('&') {
        return smart_paragraph_breaks(content);
    }
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    while i < content.len() {
        let ch = content[i..].chars().next().unwrap();
        if ch == '<' {
            let Some(gt) = content[i..].find('>') else {
                out.push('<');
                i += 1;
                continue;
            };
            let tag = &content[i + 1..i + gt];
            let tag_name = tag
                .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            // script/style/head 等容器内容对正文不可见：整段跳过，避免站点内嵌
            // 的反广告正则/统计脚本/样式文本漏进纯文本正文（用户报告正文出现
            // `(本章未完|记住网址|加入书签)` 替换正则串即源于此）
            if matches!(
                tag_name.as_str(),
                "script" | "style" | "noscript" | "template" | "iframe" | "head"
            ) {
                let rest = &content[i..];
                let close = format!("</{tag_name}");
                if let Some(pos) = rest.to_ascii_lowercase().find(&close) {
                    let tail = &rest[pos + close.len()..];
                    let end = tail
                        .find('>')
                        .map(|p| pos + close.len() + p + 1)
                        .unwrap_or(rest.len());
                    i += end;
                } else {
                    i = content.len();
                }
                continue;
            }
            if is_block_tag(&tag_name) && !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            i += gt + 1;
            continue;
        }
        if ch == '&' {
            if let Some(semi) = content[i..].find(';') {
                let entity = &content[i + 1..i + semi];
                if let Some(decoded) = decode_html_entity(entity) {
                    out.push_str(&decoded);
                    i += semi + 1;
                    continue;
                }
            }
            out.push('&');
            i += 1;
            continue;
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    // `\r\n`/`\r` 归一为 `\n`（部分站点仅用 CR 换行；前端按 \n 分段）
    out = out.replace("\r\n", "\n").replace('\r', "\n");
    // 连续换行压缩为单个（`<p></p>`/`<br><br>` 不产生空段落）
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_newline = false;
    for ch in out.chars() {
        if ch == '\n' {
            if !prev_newline {
                collapsed.push('\n');
                prev_newline = true;
            }
        } else {
            collapsed.push(ch);
            prev_newline = false;
        }
    }
    smart_paragraph_breaks(&collapsed)
}

/// 纯文本正文智能分句：部分书源只返回 `textContent`（无 `<p>/<br>`），整个章节
/// 连成一段。此时按中文/通用句末标点补换行，恢复自然段落；已有换行、短文本
/// 或英文句子密集时不处理（避免破坏原文格式）。
fn smart_paragraph_breaks(content: &str) -> String {
    if content.contains('\n') || content.chars().count() < 200 {
        return content.to_string();
    }
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len() + 64);
    let mut line_len = 0usize;
    let mut pending_break = false;
    for (i, &ch) in chars.iter().enumerate() {
        out.push(ch);
        line_len += 1;
        if pending_break {
            out.push('\n');
            line_len = 0;
            pending_break = false;
        } else if line_len >= 120 {
            let sentence_end =
                matches!(ch, '。' | '！' | '？' | '；' | '…' | '．' | '.' | '!' | '?');
            if sentence_end {
                // 句号后跟右引号/括号时等闭合后再换行
                let next = chars.get(i + 1).copied().unwrap_or(' ');
                let closing = matches!(next, '”' | '』' | '」' | '）' | '】' | '》' | '\'' | '"');
                if !closing {
                    out.push('\n');
                    line_len = 0;
                } else {
                    // 等右引号/括号 push 完再换行，避免下一句与闭合符粘连
                    pending_break = true;
                }
            } else if line_len >= 400 {
                out.push('\n');
                line_len = 0;
            }
        }
    }
    out
}

fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "br" | "p"
            | "div"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "tr"
            | "dd"
            | "dt"
            | "hr"
            | "section"
            | "article"
            | "blockquote"
            | "table"
            | "ul"
            | "ol"
            | "pre"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "summary"
            | "details"
            | "figure"
            | "figcaption"
            | "caption"
            | "thead"
            | "tbody"
            | "tfoot"
            | "center"
            | "address"
            | "fieldset"
            | "legend"
            | "menu"
            | "dir"
            | "colgroup"
    )
}

fn decode_html_entity(entity: &str) -> Option<String> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    if let Some(dec) = entity.strip_prefix('#') {
        return dec
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    match entity {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        // &nbsp; 按普通空格处理（中文正文无宽度差异；避免 U+00A0 影响分词/复制）
        "nbsp" => Some(" ".to_string()),
        _ => crate::parser::xpath::html_entity(entity).map(|c| c.to_string()),
    }
}
