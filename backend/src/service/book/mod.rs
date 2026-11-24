//! 书籍链路：详情（ruleBookInfo）/ 目录（ruleToc）/ 正文（ruleContent）
//!
//! 对齐 legacy WebBook：getBookInfo / getChapterList / getBookContent
//! v1：CSS/JSONPath/JS（简单）规则；多页目录/正文（nextTocUrl/nextContentUrl）循环支持

mod content;
mod info;
mod media;
mod purify;
#[cfg(test)]
mod tests;
mod toc;

use std::collections::HashMap;

use anyhow::Result;

use crate::model::BookSource;
use crate::service::crawler;

pub(crate) use content::html_content_to_text;
pub use content::{
    analyze_content, analyze_content_from, analyze_content_from_with_vars, ContentRule,
};
pub use info::{
    analyze_book_info, analyze_book_info_with_existing, analyze_related_books, fetch_book_info,
    merge_existing_identity, resolve_toc_url, BookInfoRule, RelatedRule,
};
pub use media::{analyze_comic_images, analyze_media_url, audio_content_type};
pub use purify::{apply_replace_rules, PurifyTarget};
pub use toc::{analyze_toc, parse_toc_page, TocRule};
pub(crate) use toc::{next_toc_urls, toc_items};

/// E7（legacy BookChapterList.kt:61-67 / BookContent.kt:64-70——翻页 URL 重建
/// AnalyzeUrl 语义）：翻页地址同样支持 `{{js}}` 模板、`<js>`/`@js:` 整段与
/// `,{...}` 后缀；此处复用搜索管线展开（key 为空、page=1），失败回退原文。
/// 注：后缀中的 method/body 暂不透传到下一页请求（罕见形态，待办）。
pub(super) fn expand_next_url(next: &str, base: &str, source: &BookSource, ns: &str) -> String {
    if !next.contains("{{") && !next.contains("<js>") && !next.starts_with("@js:") {
        return next.to_string();
    }
    let bridge =
        crate::parser::js::JsBridge::new(&source.book_source_url, &source.book_source_name)
            .with_namespace(ns);
    match crate::service::search::build_request_url(next, "", 1, base, &HashMap::new(), &bridge) {
        Ok((u, _suffix)) => u,
        Err(e) => {
            tracing::warn!("翻页 URL 展开失败（保留原文）[{next}]: {e:#}");
            next.to_string()
        }
    }
}

/// 相对 URL → 绝对
pub(super) fn to_abs(url: &str, base: &str) -> String {
    crate::service::search::to_absolute(url, base)
}

/// 抓取（复用搜索的 URL 附加参数处理；自动带书源 cookie——按用户命名空间）
///
/// legado AnalyzeUrl 语义：URL 可带 `,{...}` 后缀（js 修改 URL / headers / method+body /
/// bodyJs 响应后处理 / charset）——目录/正文/详情/媒体/漫画抓取统一生效（搜索链路已支持）。
pub async fn fetch_url(ns: &str, url: &str, source: &BookSource) -> Result<crawler::FetchResponse> {
    // legado concurrentRate：详情/目录/正文/媒体抓取统一限速（A2 共享滑窗/间隔）
    crate::service::search::concurrent_rate_acquire(ns, source).await;
    let mut headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();
    let (url_part, suffix) = crate::service::search::split_url_suffix(url);
    let mut final_url = url_part;
    if let Some(js) = &suffix.js {
        let vars = crate::service::search::js_vars("", 0, &source.book_source_url, &headers, "");
        if let Ok(u) = crate::parser::js::eval_js(js, &vars) {
            if !u.is_empty() {
                final_url = u;
            }
        }
    }
    if let Some(extra) = &suffix.headers {
        for (k, v) in extra {
            // E15（legacy AnalyzeUrl.kt:78-81）：header 中的 `proxy` 键是代理指令
            // 而非请求头——提取后移除，映射到抓取代理参数
            if k.eq_ignore_ascii_case("proxy") {
                continue;
            }
            headers.insert(k.clone(), v.clone());
        }
    }
    // 代理优先级：URL option/headers 的 proxy 键 > 书源 proxyUrl
    let proxy = source
        .proxy_url
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .or_else(|| {
            suffix
                .headers
                .as_ref()
                .and_then(|h| {
                    h.iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("proxy"))
                        .map(|(_, v)| v.as_str())
                })
                .filter(|p| !p.trim().is_empty())
        });
    // A1 webView 抓取：URL option webView=true 时经浏览器渲染取最终 HTML（JS 动态页）。
    // 浏览器未启用/求解失败时回退普通 HTTP（legacy WebView 失败同样回落 onError→重试语义）
    let mut resp = if suffix.web_view == Some(true) {
        match crate::service::browser::solve_cf_challenge(ns, &final_url, &[], 25_000, proxy).await
        {
            Ok(sol) => crawler::FetchResponse {
                body: sol.html,
                url: final_url.clone(),
                headers: Vec::new(),
                status: 200,
            },
            Err(e) => {
                tracing::warn!("webView 渲染失败，回退 HTTP 抓取 [{final_url}]: {e}");
                match suffix
                    .method
                    .as_deref()
                    .unwrap_or("GET")
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "POST" => {
                        crawler::http_post_retry(
                            ns,
                            &final_url,
                            &headers,
                            25,
                            suffix.body.as_deref(),
                            suffix.charset.as_deref(),
                            proxy,
                            suffix.retry,
                        )
                        .await?
                    }
                    _ => {
                        crawler::http_get_retry(
                            ns,
                            &final_url,
                            &headers,
                            25,
                            suffix.charset.as_deref(),
                            proxy,
                            suffix.retry,
                        )
                        .await?
                    }
                }
            }
        }
    } else {
        match suffix
            .method
            .as_deref()
            .unwrap_or("GET")
            .to_ascii_uppercase()
            .as_str()
        {
            "POST" => {
                crawler::http_post_retry(
                    ns,
                    &final_url,
                    &headers,
                    15,
                    suffix.body.as_deref(),
                    suffix.charset.as_deref(),
                    proxy,
                    suffix.retry,
                )
                .await?
            }
            _ => {
                crawler::http_get_retry(
                    ns,
                    &final_url,
                    &headers,
                    15,
                    suffix.charset.as_deref(),
                    proxy,
                    suffix.retry,
                )
                .await?
            }
        }
    };
    // bodyJs：对响应体执行 JS 后作为新响应体（result=原响应体）
    if let Some(js) = &suffix.body_js {
        let vars =
            crate::service::search::js_vars("", 0, &source.book_source_url, &headers, &resp.body);
        if let Ok(b) = crate::parser::js::eval_js(js, &vars) {
            if !b.is_empty() {
                resp.body = b;
            }
        }
    }
    Ok(resp)
}

/// 自动执行书源 loginCheckJs（legacy WebBook：搜索/探索/详情/目录抓取后调用）。
///
/// 语义：注入 cookie（当前书源 cookie 串）/result（响应体）/url（最终 URL）；
/// 返回 `true`/`1`/空 → 登录态正常，响应体不变；`false`/`0` → 登录态异常，
/// 记日志但继续解析（legacy 同样不中断抓取）；其余非空返回值 → 作为新响应体
/// （兼容 JS 重写/提取响应内容的写法）。执行失败不中断抓取，返回原响应体。
pub async fn apply_login_check_js(
    ns: &str,
    source: &BookSource,
    body: &str,
    url: &str,
    bridge: Option<&crate::parser::js::JsBridge>,
) -> String {
    let Some(js) = source
        .login_check_js
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return body.to_string();
    };
    let cookie = crate::service::crawler::cookie_for(ns, url)
        .await
        .unwrap_or_default();
    let mut vars = HashMap::new();
    vars.insert("cookie".to_string(), cookie);
    vars.insert("result".to_string(), body.to_string());
    vars.insert("url".to_string(), url.to_string());
    let out = match bridge {
        Some(b) => crate::parser::js::eval_js_with_bridge(js, &vars, b).unwrap_or_default(),
        None => crate::parser::js::eval_js(js, &vars).unwrap_or_default(),
    };
    let out = out.trim();
    if out.is_empty() || out.eq_ignore_ascii_case("true") || out == "1" {
        body.to_string()
    } else if out.eq_ignore_ascii_case("false") || out == "0" {
        tracing::debug!(
            "书源 [{}] loginCheckJs 判定未登录，继续解析（{url}）",
            source.book_source_name
        );
        body.to_string()
    } else {
        out.to_string()
    }
}
