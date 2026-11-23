//! 非文本书籍内容提取（音频 / 漫画 / 视频 / 文件）

use anyhow::Result;

use crate::model::BookSource;

use super::content::ContentRule;
use super::{fetch_url, to_abs};

/// 音频 URL → contentType（m3u8 走 HLS，其余按扩展名映射；未知默认 audio/mpeg）
///
/// 非文本正文返回契约（legacy getBookContent，由 router 直接以 JSON 构造，
/// P3-A：原 MediaContent 枚举从未被构造 → 已删除，此处保留 contentType 映射）：
/// - 音频书（book_type=1）：`{audioUrl, contentType}`
/// - 漫画书（book_type=2）：`{images: [url, ...]}`（章节 = 图片列表）
/// - 文件书（book_type=3）：`{downloadUrl}`
/// - 视频书（book_type=4）：`{videoUrl}`
pub fn audio_content_type(url: &str) -> &'static str {
    let path = url.split('?').next().unwrap_or(url).to_lowercase();
    if path.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else if path.ends_with(".mp3") {
        "audio/mpeg"
    } else if path.ends_with(".m4a") || path.ends_with(".aac") {
        "audio/mp4"
    } else if path.ends_with(".ogg") || path.ends_with(".oga") {
        "audio/ogg"
    } else if path.ends_with(".wav") {
        "audio/wav"
    } else if path.ends_with(".flac") {
        "audio/flac"
    } else if path.ends_with(".opus") {
        "audio/opus"
    } else if path.ends_with(".mp4") || path.ends_with(".m4v") {
        "audio/mp4"
    } else {
        "audio/mpeg"
    }
}

/// 规则结果 → URL 列表（兼容三种形态：普通 URL 文本 / JSON 字符串数组
/// （@js: 返回数组经 js_result_to_string 序列化）/ JSON 对象数组）
pub(super) fn collect_urls(value: &str, out: &mut Vec<String>) {
    let t = value.trim();
    if t.is_empty() {
        return;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        match v {
            serde_json::Value::String(s) => {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    out.push(s);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    match item {
                        serde_json::Value::String(s) => {
                            let s = s.trim().to_string();
                            if !s.is_empty() {
                                out.push(s);
                            }
                        }
                        // 对象数组（如 [{url: "..."}]）：取 url/src/href 字段
                        serde_json::Value::Object(map) => {
                            for k in ["url", "src", "href"] {
                                if let Some(serde_json::Value::String(s)) = map.get(k) {
                                    let s = s.trim().to_string();
                                    if !s.is_empty() {
                                        out.push(s);
                                        break;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => out.push(t.to_string()),
        }
    } else {
        out.push(t.to_string());
    }
}

/// 媒体 URL 提取（音频/视频/文件书共用）：ruleContent.content 规则应用到章节页 → URL；
/// 规则缺失或提取为空 → 章节 URL 本身（音频书章节 URL 常即音频流 URL 直链）。
/// P1 跨阶段：`book_url` 非空时双键合并读取（详情 @put 直达正文），结束双键回写
pub async fn analyze_media_url(
    ns: &str,
    chapter_url: &str,
    source: &BookSource,
    chapter_title: Option<&str>,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<String> {
    let rule: ContentRule = source
        .rule_content
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(content_rule) = rule.content.clone() else {
        return Ok(chapter_url.to_string());
    };
    if content_rule.trim().is_empty() {
        return Ok(chapter_url.to_string());
    }
    // P1 双键合并：book_url 级作底、章节级覆盖（legacy book→chapter 单 varMap 回退链）
    let mut vars = crate::parser::rule::load_book_vars_merged(
        ns,
        &source.book_source_url,
        book_url,
        chapter_url,
    );
    vars.chapter_title = chapter_title.map(str::to_string);
    vars.book_name = book_name.map(str::to_string);
    let resp = fetch_url(ns, chapter_url, source).await?;
    let base = resp.url.clone();
    // 规则结果可能含多值（CSS 命中多个/JSON 数组）——取首个 URL
    let mut urls: Vec<String> = Vec::new();
    let mut page_html =
        crate::parser::rule::apply_init_with_vars(&resp.body, rule.init.as_deref(), &mut vars);
    if let Some(js) = &rule.pre_update_js {
        if !js.trim().is_empty() {
            vars.insert("result".to_string(), page_html.clone());
            page_html = crate::parser::js::eval_js(js.trim(), &vars).unwrap_or(page_html);
        }
    }
    for v in crate::parser::rule::apply_with_vars(&content_rule, &page_html, &mut vars) {
        collect_urls(&v, &mut urls);
        if !urls.is_empty() {
            break;
        }
    }
    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        chapter_url,
        &vars,
    );
    let Some(mut url) = urls.into_iter().next() else {
        return Ok(chapter_url.to_string());
    };
    url = to_abs(&url, &base);
    Ok(url)
}

/// 漫画书图片列表提取（ruleContent.content 规则 → 图片 URL 列表）：
/// - CSS/JSONPath/Regex 规则：全部命中值均为图片 URL
/// - @js:/<js> 规则：结果可为 URL 字符串或字符串数组（JSON 序列化形态）
/// - 规则缺失：章节 URL 本身为图片直链时直接返回
///
/// P1 跨阶段：`book_url` 非空时双键合并读取（详情 @put 直达正文），结束双键回写
pub async fn analyze_comic_images(
    ns: &str,
    chapter_url: &str,
    source: &BookSource,
    chapter_title: Option<&str>,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<String>> {
    let rule: ContentRule = source
        .rule_content
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(content_rule) = rule.content.clone() else {
        // 无规则：章节 URL 即图片直链（部分图源章节 URL 直接指向图片）
        if looks_like_image_url(chapter_url) {
            return Ok(vec![chapter_url.to_string()]);
        }
        return Ok(vec![]);
    };
    if content_rule.trim().is_empty() {
        return Ok(vec![]);
    }
    // P1 双键合并：book_url 级作底、章节级覆盖
    let mut vars = crate::parser::rule::load_book_vars_merged(
        ns,
        &source.book_source_url,
        book_url,
        chapter_url,
    );
    vars.chapter_title = chapter_title.map(str::to_string);
    vars.book_name = book_name.map(str::to_string);
    let resp = fetch_url(ns, chapter_url, source).await?;
    let base = resp.url.clone();
    let mut urls: Vec<String> = Vec::new();
    let mut page_html =
        crate::parser::rule::apply_init_with_vars(&resp.body, rule.init.as_deref(), &mut vars);
    if let Some(js) = &rule.pre_update_js {
        if !js.trim().is_empty() {
            vars.insert("result".to_string(), page_html.clone());
            page_html = crate::parser::js::eval_js(js.trim(), &vars).unwrap_or(page_html);
        }
    }
    for v in crate::parser::rule::apply_with_vars(&content_rule, &page_html, &mut vars) {
        collect_urls(&v, &mut urls);
    }
    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        chapter_url,
        &vars,
    );
    // 绝对化 + 去重保序
    let mut seen = std::collections::HashSet::new();
    let mut images: Vec<String> = Vec::new();
    for u in urls {
        let abs = to_abs(&u, &base);
        if seen.insert(abs.clone()) {
            images.push(abs);
        }
    }
    Ok(images)
}

/// 是否为图片直链（按扩展名判断，忽略查询串）
fn looks_like_image_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    [
        ".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".avif", ".svg",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
}
