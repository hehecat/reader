//! 书源调试（bookSourceDebugSSE）：逐规则执行测试引擎
//!
//! 复用现有规则引擎（search/book/explore），按步骤输出：
//! 规则解析 → URL 构造 → 请求 → 规则应用，每步含规则名/请求 URL/耗时/结果长度/错误/解析明细。

use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};

use crate::model::BookSource;
use crate::parser::rule::parse_rule;
use crate::service::book::{analyze_content_from, ContentRule, TocRule};
use crate::service::crawler;
use crate::service::search::{
    analyze_book_list_for_explore, field, split_url_suffix, to_absolute, SearchRule, UrlSuffix,
};

/// 调试步骤（SSE step 事件 message 载荷）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStep {
    /// 规则名（如 ruleSearch.bookList / 请求 URL / 抓取 / 规则应用）
    pub rule_name: String,
    /// 请求 URL（非请求步骤为空）
    pub url: String,
    /// 耗时（毫秒）
    pub elapsed_ms: i64,
    /// 结果长度（字符数）
    pub result_len: usize,
    /// 错误信息（无则空）
    pub error: Option<String>,
    /// 解析明细（规则类型/字段等）
    pub detail: Value,
}

impl DebugStep {
    fn new(rule_name: impl Into<String>) -> Self {
        Self {
            rule_name: rule_name.into(),
            url: String::new(),
            elapsed_ms: 0,
            result_len: 0,
            error: None,
            detail: Value::Null,
        }
    }
}

/// 提取规则字符串中的 JS 代码（`<js>…</js>` / `@js:` 前缀 / `规则@js:` 后缀链）——
/// debug 步骤输出 JS 片段用（与生产解析的提取规则同源）
fn extract_js_code(rule: &str) -> Option<String> {
    let r = rule.trim();
    if let Some(rest) = r.strip_prefix("@js:") {
        return Some(rest.trim().to_string());
    }
    if let Some(start) = r.find("<js>") {
        let rest = &r[start + 4..];
        let end = rest.find("</js>").unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    if let Some((_, js)) = r.split_once("@js:") {
        return Some(js.trim().to_string());
    }
    None
}

/// JS 片段前 100 字符（Unicode 字符计数）
fn js_snippet(code: &str) -> String {
    code.chars().take(100).collect()
}

/// GAP 156：把本步骤内发生的 JS eval 错误附加到步骤输出——
/// `error` = 错误消息；`detail.jsError` = 原始错误；`detail.jsSnippet` = JS 片段前 100 字符；
/// `detail.jsRule` = 来源规则。取走线程局部记录（无则不改动步骤）。
fn attach_js_error(step: &mut DebugStep, rule: Option<&str>) {
    let Some(err) = crate::parser::js::take_last_js_error() else {
        return;
    };
    step.error = Some(format!("JS 执行失败: {err}"));
    let mut detail = step.detail.clone();
    if !detail.is_object() {
        detail = json!({});
    }
    detail["jsError"] = json!(err);
    if let Some(code) = rule.and_then(extract_js_code) {
        detail["jsSnippet"] = json!(js_snippet(&code));
        detail["jsRule"] = json!(rule.unwrap_or(""));
    }
    step.detail = detail;
}

/// 执行调试：逐步骤回调 on_step，返回最终结果 JSON
pub async fn run_debug(
    ns: &str,
    source: &BookSource,
    action: &str,
    key: &str,
    target_url: &str,
    mut on_step: impl FnMut(&DebugStep),
) -> Result<Value> {
    // P2：每轮开始清空线程局部 LAST_JS_ERROR——上一轮（同一 tokio 工作线程）遗留的
    // JS 错误记录不得错挂到本轮步骤（attach_js_error 只应取走本轮 eval 产生的错误）
    let _ = crate::parser::js::take_last_js_error();
    match action {
        "search" => debug_search(ns, source, key, &mut on_step).await,
        "explore" => debug_explore(ns, source, target_url, &mut on_step).await,
        "toc" => debug_toc(ns, source, target_url, &mut on_step).await,
        "content" => debug_content(ns, source, target_url, &mut on_step).await,
        _ => Err(anyhow::anyhow!(
            "不支持的调试动作（search|explore|toc|content）"
        )),
    }
}

/// 请求执行（带步骤输出）
async fn debug_fetch(
    ns: &str,
    url: &str,
    suffix: &UrlSuffix,
    source: &BookSource,
    key: &str,
    on_step: &mut impl FnMut(&DebugStep),
) -> Result<crawler::FetchResponse> {
    let mut headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();
    if let Some(extra) = &suffix.headers {
        for (k, v) in extra {
            headers.insert(k.clone(), v.clone());
        }
    }
    let post_body = suffix.body.as_ref().map(|b| {
        b.replace("{{key}}", key)
            .replace("{{page}}", "1")
            .replace("{key}", key)
            .replace("{page}", "1")
    });
    let method = suffix.method.as_deref().unwrap_or("GET").to_string();
    let started = Instant::now();
    let result = if method.eq_ignore_ascii_case("POST") {
        crawler::http_post_retry(
            ns,
            url,
            &headers,
            15,
            post_body.as_deref(),
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await
    } else {
        crawler::http_get_retry(
            ns,
            url,
            &headers,
            15,
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await
    };
    match result {
        Ok(resp) => {
            on_step(&DebugStep {
                rule_name: "请求 URL".into(),
                url: url.to_string(),
                elapsed_ms: started.elapsed().as_millis() as i64,
                result_len: resp.body.len(),
                error: None,
                detail: json!({ "method": method, "status": resp.status }),
            });
            Ok(resp)
        }
        Err(e) => {
            on_step(&DebugStep {
                rule_name: "请求 URL".into(),
                url: url.to_string(),
                elapsed_ms: started.elapsed().as_millis() as i64,
                result_len: 0,
                error: Some(e.to_string()),
                detail: json!({ "method": method }),
            });
            Err(e)
        }
    }
}

/// search：规则解析 → URL 构造 → 抓取 → 规则应用
async fn debug_search(
    ns: &str,
    source: &BookSource,
    key: &str,
    on_step: &mut impl FnMut(&DebugStep),
) -> Result<Value> {
    // ① 规则解析
    let mut step = DebugStep::new("规则解析（ruleSearch）");
    let rule: SearchRule = match &source.rule_search {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => SearchRule::default(),
    };
    let book_list_rule = rule.book_list.clone().unwrap_or_default();
    let parsed = parse_rule(&book_list_rule);
    step.detail = json!({
        "bookList": book_list_rule,
        "bookListKind": format!("{:?}", parsed.kind),
        "name": rule.name, "author": rule.author, "bookUrl": rule.book_url,
        "coverUrl": rule.cover_url, "wordCount": rule.word_count,
    });
    step.result_len = book_list_rule.len();
    on_step(&step);

    let Some(search_url) = source.search_url.clone() else {
        on_step(&DebugStep {
            rule_name: "URL 构造".into(),
            url: String::new(),
            elapsed_ms: 0,
            result_len: 0,
            error: Some("书源未配置 searchUrl".into()),
            detail: Value::Null,
        });
        return Err(anyhow::anyhow!("书源未配置 searchUrl"));
    };

    // ② URL 构造
    let headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();
    let started = Instant::now();
    // 书源桥接（带用户命名空间：URL 构造 JS 内 java.* 可用）
    let bridge = crate::parser::js::JsBridge::from_source(source, ns);
    let (url, suffix) = match crate::service::search::build_request_url(
        &search_url,
        key,
        1,
        &source.book_source_url,
        &headers,
        &bridge,
    ) {
        Ok(v) => v,
        Err(e) => {
            let mut step = DebugStep {
                rule_name: "URL 构造".into(),
                url: search_url.clone(),
                elapsed_ms: started.elapsed().as_millis() as i64,
                result_len: 0,
                error: Some(e.to_string()),
                detail: Value::Null,
            };
            // GAP 156：URL 构造 JS（@js: 前缀）失败时附加 JS 片段前 100 字符
            if let Some(code) = extract_js_code(&search_url) {
                step.detail = json!({"jsSnippet": js_snippet(&code), "jsRule": search_url});
            }
            on_step(&step);
            return Err(e);
        }
    };
    on_step(&DebugStep {
        rule_name: "URL 构造".into(),
        url: url.clone(),
        elapsed_ms: started.elapsed().as_millis() as i64,
        result_len: url.len(),
        error: None,
        detail: json!({
            "method": suffix.method, "js": suffix.js, "bodyJs": suffix.body_js,
            "charset": suffix.charset, "body": suffix.body,
        }),
    });

    // ③ 抓取
    let resp = debug_fetch(ns, &url, &suffix, source, key, on_step).await?;
    let base = resp.url.clone();

    // ④ 规则应用
    let mut step = DebugStep::new("规则应用（bookList 字段）");
    let started = Instant::now();
    let books =
        analyze_book_list_for_explore(ns, &resp.body, &base, source, &rule, &book_list_rule);
    step.elapsed_ms = started.elapsed().as_millis() as i64;
    step.result_len = books.len();
    step.detail = json!({
        "count": books.len(),
        "first": books.first().map(|b| json!({
            "name": b.name, "author": b.author, "bookUrl": b.book_url,
        })),
    });
    // GAP 156：bookList JS 规则 eval 失败 → 错误消息 + JS 片段前 100 字符
    attach_js_error(&mut step, Some(&book_list_rule));
    on_step(&step);
    Ok(json!(books))
}

/// explore：规则解析（exploreUrl 条目）→ URL 构造 → 抓取 → 规则应用
async fn debug_explore(
    ns: &str,
    source: &BookSource,
    target_url: &str,
    on_step: &mut impl FnMut(&DebugStep),
) -> Result<Value> {
    let mut step = DebugStep::new("规则解析（ruleExplore）");
    let rule: SearchRule = match &source.rule_explore {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => SearchRule::default(),
    };
    let book_list_rule = rule.book_list.clone().unwrap_or_default();
    let parsed = parse_rule(&book_list_rule);
    step.detail = json!({
        "bookList": book_list_rule,
        "bookListKind": format!("{:?}", parsed.kind),
        "exploreEntries": crate::service::explore::parse_explore_entries(source.explore_url.as_deref().unwrap_or("")).len(),
    });
    step.result_len = book_list_rule.len();
    on_step(&step);

    // 目标 URL：显式传入优先，否则取 exploreUrl 首个条目
    let raw = if !target_url.is_empty() {
        target_url.to_string()
    } else {
        crate::service::explore::parse_explore_entries(source.explore_url.as_deref().unwrap_or(""))
            .into_iter()
            .find(|e| e.r#type == "book")
            .map(|e| e.url)
            .unwrap_or_default()
    };
    if raw.is_empty() {
        return Err(anyhow::anyhow!("未配置 exploreUrl 且未传入 url"));
    }

    // URL 构造（{{page}} 占位 + 相对路径拼 base + ,{...} 后缀）
    let mut step = DebugStep::new("URL 构造");
    let started = Instant::now();
    let url = raw.replace("{{page}}", "1").replace("{page}", "1");
    let url = if url.starts_with('/') && !url.starts_with("//") {
        let base = source
            .book_source_url
            .split('#')
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        format!("{base}{url}")
    } else {
        url
    };
    let (final_url, suffix) = split_url_suffix(&url);
    step.url = final_url.clone();
    step.elapsed_ms = started.elapsed().as_millis() as i64;
    step.result_len = final_url.len();
    step.detail = json!({ "method": suffix.method, "charset": suffix.charset });
    on_step(&step);

    // 抓取
    let resp = debug_fetch(ns, &final_url, &suffix, source, "", on_step).await?;
    let base = resp.url.clone();

    // 规则应用
    let mut step = DebugStep::new("规则应用（bookList 字段）");
    let started = Instant::now();
    let books =
        analyze_book_list_for_explore(ns, &resp.body, &base, source, &rule, &book_list_rule);
    step.elapsed_ms = started.elapsed().as_millis() as i64;
    step.result_len = books.len();
    step.detail = json!({ "count": books.len() });
    on_step(&step);
    Ok(json!(books))
}

/// toc：规则解析 →（必要时经 ruleBookInfo.tocUrl 解析目录地址）→ 抓取目录页 → chapterList 提取 → 字段规则 → nextTocUrl 队列循环（≤5 页）
async fn debug_toc(
    ns: &str,
    source: &BookSource,
    toc_url: &str,
    on_step: &mut impl FnMut(&DebugStep),
) -> Result<Value> {
    if toc_url.is_empty() {
        return Err(anyhow::anyhow!("请输入目录链接（url 参数）"));
    }
    let mut step = DebugStep::new("规则解析（ruleToc）");
    let rule: TocRule = match &source.rule_toc {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => TocRule::default(),
    };
    let list_rule = rule.chapter_list.clone().unwrap_or_default();
    let parsed = parse_rule(&list_rule);
    step.detail = json!({
        "chapterList": list_rule,
        "chapterListKind": format!("{:?}", parsed.kind),
        "chapterName": rule.chapter_name, "chapterUrl": rule.chapter_url,
        "nextTocUrl": rule.next_toc_url,
    });
    step.result_len = list_rule.len();
    on_step(&step);

    // 若书源配置了 ruleBookInfo.tocUrl（例如详情页 `/book/123` → 目录接口 `/toc/123`），先解析真实目录 URL
    let info_started = Instant::now();
    let resolved_toc_url = crate::service::book::resolve_toc_url(ns, source, toc_url).await;
    if resolved_toc_url != toc_url {
        on_step(&DebugStep {
            rule_name: "详情目录解析（ruleBookInfo.tocUrl）".into(),
            url: resolved_toc_url.clone(),
            elapsed_ms: info_started.elapsed().as_millis() as i64,
            result_len: resolved_toc_url.len(),
            error: None,
            detail: json!({ "bookUrl": toc_url, "tocUrl": resolved_toc_url }),
        });
    }

    let mut all: Vec<Value> = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    let mut visited = std::collections::HashSet::new();
    queue.push_back(resolved_toc_url.clone());
    visited.insert(resolved_toc_url.clone());
    let mut vars = crate::parser::rule::load_book_vars_merged(
        ns,
        &source.book_source_url,
        toc_url,
        &resolved_toc_url,
    );
    let mut tried_book_fallback = resolved_toc_url == toc_url;

    let mut page = 0usize;
    while page < 5 {
        let Some(current_url) = queue.pop_front() else {
            break;
        };
        page += 1;
        // 清掉上一页 nextTocUrl 字段求值可能留下的 JS 错误记录（避免错挂到本页步骤）
        let _ = crate::parser::js::take_last_js_error();
        // 抓取目录页
        let resp =
            match debug_fetch(ns, &current_url, &UrlSuffix::default(), source, "", on_step).await {
                Ok(r) => r,
                Err(e) => return Err(e),
            };
        let base = resp.url.clone();
        vars.insert("baseUrl".to_string(), base.clone());

        // chapterList 提取（与生产 analyze_toc 同源 toc_items——含 <js>/@js: 兜底，
        // 保证 JS 规则确实执行、失败可被 attach_js_error 捕获）
        let mut step = DebugStep::new(format!("chapterList 提取（第 {page} 页）"));
        let started = Instant::now();
        let items: Vec<String> = crate::service::book::toc_items(&list_rule, &resp.body);
        step.elapsed_ms = started.elapsed().as_millis() as i64;
        step.result_len = items.len();
        step.detail = json!({ "count": items.len() });
        // GAP 156：chapterList JS 规则（<js>/@js:）eval 失败 → 错误消息 + JS 片段前 100 字符
        attach_js_error(&mut step, Some(&list_rule));
        on_step(&step);

        // 首页若因镜像 tocUrl 失效得到 0 章，回退原书页 toc_url 再试一次（对齐生产 analyze_toc）
        if page == 1 && items.is_empty() && !tried_book_fallback {
            tried_book_fallback = true;
            if visited.insert(toc_url.to_string()) {
                queue.push_front(toc_url.to_string());
                continue;
            }
        }

        // 字段规则（前 20 条示例）
        let mut step = DebugStep::new("字段规则（chapterName/chapterUrl）");
        let started = Instant::now();
        let start_index = all.len() as i64;
        let mut page_chapters = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let title = field(item, rule.chapter_name.as_deref(), "");
            let url = rule
                .chapter_url
                .as_deref()
                .map(|r| field(item, Some(r), ""))
                .unwrap_or_default();
            if title.is_empty() && url.is_empty() {
                continue;
            }
            page_chapters.push(json!({
                "title": title,
                "url": to_absolute(&url, &base),
                "index": start_index + i as i64,
            }));
            if page_chapters.len() >= 20 {
                break;
            }
        }
        step.elapsed_ms = started.elapsed().as_millis() as i64;
        step.result_len = page_chapters.len();
        step.detail = json!({ "sample": page_chapters.first() });
        // GAP 156：chapterName/chapterUrl 的 @js: 后缀链 eval 失败 → 错误消息 + JS 片段前 100 字符
        let js_field_rule = [rule.chapter_name.as_deref(), rule.chapter_url.as_deref()]
            .into_iter()
            .flatten()
            .find(|r| extract_js_code(r).is_some());
        attach_js_error(&mut step, js_field_rule);
        on_step(&step);
        all.extend(page_chapters);

        // nextTocUrl：与生产 analyze_toc 共用多候选拆分与已访问去重，支持 `option@value` 等多元素翻页
        if let Some(next_rule) = rule.next_toc_url.as_deref() {
            for cand in crate::service::book::next_toc_urls(
                &resp.body, next_rule, &base, source, ns, &mut vars,
            ) {
                if cand != base && visited.insert(cand.clone()) {
                    queue.push_back(cand);
                }
            }
        }
    }
    Ok(json!(all))
}

/// content：规则解析 → 抓取章节页 → content 规则应用 + sourceRegex/replaceRegex 清洗 → 多页循环
async fn debug_content(
    ns: &str,
    source: &BookSource,
    chapter_url: &str,
    on_step: &mut impl FnMut(&DebugStep),
) -> Result<Value> {
    if chapter_url.is_empty() {
        return Err(anyhow::anyhow!("请输入章节链接（chapterUrl 参数）"));
    }
    let mut step = DebugStep::new("规则解析（ruleContent）");
    let rule: ContentRule = match &source.rule_content {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => ContentRule::default(),
    };
    step.detail = json!({
        "content": rule.content,
        "sourceRegex": rule.source_regex,
        "replaceRegex": rule.replace_regex,
        "nextContentUrl": rule.next_content_url,
    });
    step.result_len = rule.content.as_deref().map(|s| s.len()).unwrap_or(0);
    on_step(&step);

    let mut parts: Vec<String> = Vec::new();
    let mut current_url = chapter_url.to_string();
    for page in 0..5usize {
        // 清掉上一页 nextContentUrl 字段求值可能留下的 JS 错误记录（避免错挂到本页步骤）
        let _ = crate::parser::js::take_last_js_error();
        let resp =
            match debug_fetch(ns, &current_url, &UrlSuffix::default(), source, "", on_step).await {
                Ok(r) => r,
                Err(e) => return Err(e),
            };
        let base = resp.url.clone();

        let mut step = DebugStep::new(format!("content 规则应用（第 {} 页）", page + 1));
        let started = Instant::now();
        let content = analyze_content_from(&resp.body, source);
        step.elapsed_ms = started.elapsed().as_millis() as i64;
        step.result_len = content.len();
        step.error = if content.is_empty() {
            Some("未提取到正文".into())
        } else {
            None
        };
        step.detail = json!({ "chars": content.chars().count() });
        // GAP 156：content 规则 JS（<js>/@js: 后缀链）eval 失败 → 错误消息 + JS 片段前 100 字符
        attach_js_error(&mut step, rule.content.as_deref());
        on_step(&step);
        if !content.is_empty() {
            parts.push(content);
        }

        let next = rule
            .next_content_url
            .as_deref()
            .map(|r| field(&resp.body, Some(r), ""))
            .unwrap_or_default();
        if next.is_empty() {
            break;
        }
        current_url = to_absolute(&next, &base);
    }
    Ok(json!({ "content": parts.join("\n") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_parse_kind_detection() {
        // 规则解析结果（CSS/JSONPath）——调试步骤依赖的底层能力
        let css = parse_rule("div.book");
        assert_eq!(format!("{:?}", css.kind), "Css");
        let jp = parse_rule("$.data[*]");
        assert_eq!(format!("{:?}", jp.kind), "JsonPath");
    }

    #[test]
    fn test_debug_step_serialize() {
        let step = DebugStep {
            rule_name: "规则解析（ruleSearch）".into(),
            url: "https://a.com/s".into(),
            elapsed_ms: 12,
            result_len: 3,
            error: None,
            detail: json!({ "kind": "Css" }),
        };
        let v = serde_json::to_value(&step).unwrap();
        assert_eq!(v["ruleName"], "规则解析（ruleSearch）");
        assert_eq!(v["elapsedMs"], 12);
        assert_eq!(v["detail"]["kind"], "Css");
    }

    // ==================== GAP 156：JS 规则执行步骤错误详情 ====================

    #[test]
    fn test_extract_js_code_variants() {
        // <js>…</js> 包裹
        assert_eq!(
            extract_js_code("<js>return 1</js>"),
            Some("return 1".to_string())
        );
        // @js: 前缀
        assert_eq!(
            extract_js_code("@js:JSON.parse(result)"),
            Some("JSON.parse(result)".to_string())
        );
        // 提取规则 @js: 后缀链（legado）
        assert_eq!(
            extract_js_code("$.path@js:java.aesBase64DecodeToString(v)"),
            Some("java.aesBase64DecodeToString(v)".to_string())
        );
        // 非 JS 规则 → None
        assert_eq!(extract_js_code("div.book@text"), None);
        assert_eq!(extract_js_code(""), None);
    }

    #[test]
    fn test_js_snippet_100_chars() {
        // ASCII
        let s = "a".repeat(250);
        assert_eq!(js_snippet(&s).len(), 100);
        // 中文（Unicode 字符计数——100 个汉字）
        let cn = "汉".repeat(120);
        assert_eq!(js_snippet(&cn).chars().count(), 100);
        // 短代码原样
        assert_eq!(js_snippet("var x = 1;"), "var x = 1;");
    }

    #[test]
    fn test_attach_js_error_records_error_and_snippet() {
        // eval 失败（运行期 throw）——map_js_error 记录线程局部
        let vars = std::collections::HashMap::new();
        let r = crate::parser::js::eval_js_json("throw new Error('书单解析爆炸')", &vars);
        assert!(r.is_err(), "throw 应失败");
        let raw = crate::parser::js::take_last_js_error().expect("eval 失败应留下错误记录");
        assert!(raw.contains("书单解析爆炸"), "原始错误消息: {raw}");

        // 再次失败，随后 attach 到步骤——错误消息 + JS 片段前 100 字符
        let r = crate::parser::js::eval_js_json("throw new Error('书单解析爆炸')", &vars);
        assert!(r.is_err());
        let mut step = DebugStep::new("规则应用（bookList 字段）");
        attach_js_error(&mut step, Some("<js>throw new Error('书单解析爆炸')</js>"));
        let err = step.error.expect("步骤应带错误");
        assert!(
            err.contains("JS 执行失败"),
            "错误消息含 JS 执行失败前缀: {err}"
        );
        assert!(err.contains("书单解析爆炸"), "错误消息含原始错误: {err}");
        assert!(
            step.detail["jsError"]
                .as_str()
                .unwrap_or("")
                .contains("书单解析爆炸"),
            "detail.jsError 含原始错误"
        );
        assert_eq!(
            step.detail["jsSnippet"], "throw new Error('书单解析爆炸')",
            "JS 片段为规则代码前 100 字符"
        );
        assert_eq!(
            step.detail["jsRule"],
            "<js>throw new Error('书单解析爆炸')</js>"
        );
        // 记录已被取走——再次 attach 不再重复
        let mut step2 = DebugStep::new("x");
        attach_js_error(&mut step2, None);
        assert!(step2.error.is_none());
    }

    #[test]
    fn test_attach_js_error_ignores_successful_eval() {
        // eval 成功 → 无错误记录 → attach 不改动步骤
        let vars = std::collections::HashMap::new();
        let r = crate::parser::js::eval_js_json("JSON.stringify([{a:1}])", &vars);
        assert!(r.is_ok());
        assert!(crate::parser::js::take_last_js_error().is_none());
        let mut step = DebugStep::new("规则应用");
        attach_js_error(&mut step, Some("@js:JSON.stringify([{a:1}])"));
        assert!(step.error.is_none(), "成功 eval 不应挂错误");
    }

    /// P2：每轮 run_debug 清空线程局部 LAST_JS_ERROR——上一轮（同一 tokio 工作线程）
    /// 遗留的 JS 错误不得错挂到本轮步骤（attach_js_error 只取本轮 eval 产生的错误）
    #[tokio::test]
    async fn test_run_debug_clears_stale_js_error_per_round() {
        use std::collections::HashMap;
        // 模拟上一轮遗留：eval 失败留下线程局部记录且未取走
        let vars = HashMap::new();
        let _ = crate::parser::js::eval_js_json("throw new Error('stale-round')", &vars);
        assert!(
            crate::parser::js::take_last_js_error().is_some(),
            "预置遗留错误记录"
        );
        let _ = crate::parser::js::eval_js_json("throw new Error('stale-round')", &vars);

        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let body = r#"<html><body><div class="content">正文内容。</div></body></html>"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let source = BookSource {
            book_source_url: format!("http://{addr}/sources.json"),
            rule_content: Some(serde_json::json!({ "content": "div.content@text" })),
            ..Default::default()
        };
        let mut steps: Vec<DebugStep> = Vec::new();
        let chapter_url = format!("http://{addr}/ch1.html");
        let result = run_debug("default", &source, "content", "", &chapter_url, |s| {
            steps.push(s.clone())
        })
        .await;
        assert!(result.is_ok(), "content 调试应成功: {:?}", result.err());
        // 本轮任何步骤不得携带上一轮遗留的 stale 错误
        assert!(!steps.is_empty(), "应产出步骤");
        for s in &steps {
            if let Some(e) = &s.error {
                assert!(
                    !e.contains("stale-round"),
                    "步骤 {} 不得携带遗留错误: {e}",
                    s.rule_name
                );
            }
            if let Some(d) = s.detail.as_object() {
                assert!(
                    !d.contains_key("jsError"),
                    "步骤 {} 不得携带遗留 jsError",
                    s.rule_name
                );
            }
        }
        // 本轮无 JS 失败 → 线程局部已清空
        assert!(
            crate::parser::js::take_last_js_error().is_none(),
            "运行后不应残留错误记录"
        );
        // 控制组：不带 run_debug 的 attach 仍会取到遗留错误（证明清空发生在 run_debug 内）
        let _ = crate::parser::js::eval_js_json("throw new Error('stale-round')", &vars);
        let mut step = DebugStep::new("x");
        attach_js_error(&mut step, None);
        assert!(
            step.error.is_some(),
            "控制组：遗留错误应被 attach 捕获（无清空时）"
        );
    }
}
