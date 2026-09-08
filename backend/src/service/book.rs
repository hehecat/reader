//! 书籍链路：详情（ruleBookInfo）/ 目录（ruleToc）/ 正文（ruleContent）
//!
//! 对齐 legacy WebBook：getBookInfo / getChapterList / getBookContent
//! v1：CSS/JSONPath/JS（简单）规则；多页目录/正文（nextTocUrl/nextContentUrl）循环支持

use anyhow::Result;
use serde::Deserialize;

use crate::model::book_chapter::{BookChapter, BookInfo};
use crate::model::BookSource;
use crate::parser::css_chain::css_chain;
use crate::parser::rule::{apply, parse_rule, RuleKind};
use crate::service::crawler;
use crate::service::search::split_url_suffix;
use std::collections::HashMap;

/// E7（legacy BookChapterList.kt:61-67 / BookContent.kt:64-70——翻页 URL 重建
/// AnalyzeUrl 语义）：翻页地址同样支持 `{{js}}` 模板、`<js>`/`@js:` 整段与
/// `,{...}` 后缀；此处复用搜索管线展开（key 为空、page=1），失败回退原文。
/// 注：后缀中的 method/body 暂不透传到下一页请求（罕见形态，待办）。
fn expand_next_url(next: &str, base: &str, source: &BookSource, ns: &str) -> String {
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

/// ruleBookInfo 结构（legacy BookInfoRule）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BookInfoRule {
    pub name: Option<String>,
    pub author: Option<String>,
    pub kind: Option<String>,
    pub intro: Option<String>,
    pub cover_url: Option<String>,
    pub toc_url: Option<String>,
    /// 更新时间（legacy BookInfoRule.updateTime）
    pub update_time: Option<String>,
    /// 是否允许用规则结果覆盖非空原有书名/作者（legacy canReName）
    pub can_re_name: Option<String>,
    pub word_count: Option<String>,
    pub last_chapter: Option<String>,
    pub init: Option<String>,
}

/// ruleToc 结构（legacy TocRule）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TocRule {
    pub chapter_list: Option<String>,
    pub chapter_name: Option<String>,
    pub chapter_url: Option<String>,
    /// legacy isVolume 规则（命中 true → 卷标题）
    pub is_volume: Option<String>,
    pub chapter_vip: Option<String>,
    pub update_time: Option<String>,
    pub next_toc_url: Option<String>,
    pub chapter_type: Option<String>,
    pub init: Option<String>,
    pub pre_update_js: Option<String>,
}

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
        match crate::service::browser::solve_cf_challenge(ns, &final_url, &[], 15_000, proxy).await
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

/// ruleRelated 结构（GAP 17b：相关推荐——字段与 ruleExplore 一致：bookList + 字段规则）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RelatedRule {
    pub book_list: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub book_url: Option<String>,
    pub cover_url: Option<String>,
}

/// 相关推荐解析（GAP 17b）：ruleRelated 应用详情页 HTML，同 ruleExplore 风格
/// （bookList CSS 链式 + 字段规则）→ [{name, author, bookUrl, coverUrl}]
pub fn analyze_related_books(
    html: &str,
    base_url: &str,
    source: &BookSource,
) -> Vec<crate::model::book_chapter::RelatedBook> {
    let rule: RelatedRule = source
        .rule_related
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(book_list_rule) = rule.book_list.clone() else {
        return vec![];
    };
    // 复用 ruleExplore 书单解析（SearchRule 字段名与 RelatedRule 一致）
    let search_rule = crate::service::search::SearchRule {
        book_list: rule.book_list,
        name: rule.name,
        author: rule.author,
        book_url: rule.book_url,
        cover_url: rule.cover_url,
        ..Default::default()
    };
    crate::service::search::analyze_book_list_for_explore(
        "default",
        html,
        base_url,
        source,
        &search_rule,
        &book_list_rule,
    )
    .into_iter()
    .map(|b| crate::model::book_chapter::RelatedBook {
        name: b.name,
        author: b.author,
        book_url: b.book_url,
        cover_url: b.cover_url,
    })
    .collect()
}

/// 详情解析（ruleBookInfo 字段应用于详情页 HTML）
/// F12/AR4：`book_name` 为解析前已知的书名（搜索结果/书架）——@get:{bookName} 回退源
#[allow(clippy::too_many_arguments)]
pub fn analyze_book_info(
    ns: &str,
    html: &str,
    base_url: &str,
    source: &BookSource,
    book_url: &str,
    book_name: Option<&str>,
) -> BookInfo {
    let rule: BookInfoRule = source
        .rule_book_info
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // legado init：先提取详情上下文（如 $.data），字段规则相对应用
    // @put/@get 变量随本书流程贯通（legado Book.putVariable）——详情→目录共享
    let mut vars = crate::parser::rule::load_book_vars(ns, &source.book_source_url, book_url);
    vars.book_name = book_name.map(str::to_string);
    // E10/AR5：详情页真实 URL → JS 求值绑定 baseUrl（搜索场景无章节上下文）
    vars.insert("baseUrl".to_string(), base_url.to_string());
    let html = crate::parser::rule::apply_init_with_vars(html, rule.init.as_deref(), &mut vars);
    let html = html.as_str();
    // tocUrl 规则（legacy BookInfo.tocUrl 为完整字段规则）：
    // ① 选择器形态（CSS/XPath/JSONPath/@js 链）→ field 全量求值；
    // ② 求值为空时回退 v1 直接路径/URL 拼接 + {{}} 内嵌模板展开
    let toc_url = rule
        .toc_url
        .as_deref()
        .map(|r| {
            let evaluated = crate::service::search::field_with_vars(html, Some(r), "", &mut vars);
            if !evaluated.is_empty() {
                evaluated
            } else {
                crate::service::search::expand_embedded_with_vars(r, html, &vars)
            }
        })
        .filter(|r| !r.is_empty())
        .map(|r| to_abs(&r, base_url));

    let info = BookInfo {
        // legacy BookInfo.kt:61-90：name/author 经 formatBookName/Author 清洗，
        // kind 多值归一，wordCount 走 wordCountFormat
        name: crate::service::search::format_book_name(&crate::service::search::field_with_vars(
            html,
            rule.name.as_deref(),
            "",
            &mut vars,
        )),
        author: crate::service::search::format_book_author(
            &crate::service::search::field_with_vars(html, rule.author.as_deref(), "", &mut vars),
        ),
        kind: crate::service::search::opt_field_with_vars(html, rule.kind.as_deref(), &mut vars)
            .map(|k| crate::service::search::normalize_kind_list(&k)),
        intro: crate::service::search::opt_field_with_vars(html, rule.intro.as_deref(), &mut vars),
        update_time: crate::service::search::opt_field_with_vars(
            html,
            rule.update_time.as_deref(),
            &mut vars,
        ),
        cover_url: crate::service::search::opt_field_with_vars(
            html,
            rule.cover_url.as_deref(),
            &mut vars,
        )
        .map(|c| to_abs(&c, base_url)),
        // legacy BookInfo：tocUrl 为空时用 baseUrl（详情页即目录页）
        toc_url: toc_url.clone().or_else(|| Some(base_url.to_string())),
        word_count: crate::service::search::opt_field_with_vars(
            html,
            rule.word_count.as_deref(),
            &mut vars,
        )
        .map(|w| crate::service::search::word_count_format(&w)),
        latest_chapter_title: crate::service::search::opt_field_with_vars(
            html,
            rule.last_chapter.as_deref(),
            &mut vars,
        ),
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: source.book_source_name.clone(),
        language: None,
        publisher: None,
        published_at: None,
        related_books: analyze_related_books(html, base_url, source),
        book_type: source.book_source_type,
    };
    // 目录流程（getBookToc）只带 tocUrl，无 bookUrl——按两个键都存，保证命中
    crate::parser::rule::save_book_vars(ns, &source.book_source_url, book_url, &vars);
    if let Some(t) = &toc_url {
        crate::parser::rule::save_book_vars(ns, &source.book_source_url, t, &vars);
    }
    info
}

/// legacy BookInfo.canReName 语义：书源规则未声明 canReName 时，保留书架已有
/// 书名/作者（避免详情页刷新或换源把用户自定义的名称/作者覆盖为空）。
pub fn merge_existing_identity(
    info: &mut BookInfo,
    source: &BookSource,
    existing_name: &str,
    existing_author: &str,
) {
    if existing_name.is_empty() && existing_author.is_empty() {
        return;
    }
    let rule: BookInfoRule = source
        .rule_book_info
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let can_re_name = rule
        .can_re_name
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !can_re_name {
        if !existing_name.is_empty() {
            info.name = existing_name.to_string();
        }
        if !existing_author.is_empty() {
            info.author = existing_author.to_string();
        }
    }
}

/// 带已有书架身份的详情解析（legacy WebBook 更新详情时 canReName=true——
/// 是否真正覆盖由书源 canReName 规则决定）
pub fn analyze_book_info_with_existing(
    html: &str,
    base_url: &str,
    source: &BookSource,
    book_url: &str,
    existing_name: &str,
    existing_author: &str,
) -> BookInfo {
    let mut info = analyze_book_info(
        "default",
        html,
        base_url,
        source,
        book_url,
        Some(existing_name).filter(|n| !n.is_empty()),
    );
    merge_existing_identity(&mut info, source, existing_name, existing_author);
    info
}

/// 详情抓取 + loginCheckJs + ruleBookInfo 解析（router 详情/换源取书名共用）
/// F12/AR4：`book_name` 为解析前已知的书名（搜索结果/书架）——@get:{bookName} 回退源
///
/// legacy 对齐：抓取报错标记运行期失效快照（getInvalidBookSources 600 秒内直接返回），
/// 成功则清除该源标记。
pub async fn fetch_book_info(
    ns: &str,
    url: &str,
    source: &BookSource,
    book_name: Option<&str>,
) -> Result<BookInfo> {
    match fetch_book_info_impl(ns, url, source, book_name).await {
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

async fn fetch_book_info_impl(
    ns: &str,
    url: &str,
    source: &BookSource,
    book_name: Option<&str>,
) -> Result<BookInfo> {
    let mut resp = fetch_url(ns, url, source).await?;
    resp.body = apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;
    Ok(analyze_book_info(
        ns, &resp.body, &resp.url, source, url, book_name,
    ))
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
    let mut vars = std::collections::HashMap::new();
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

/// 目录解析（ruleToc：chapterList 定位 + 字段规则；多页 nextTocUrl 循环）
/// F12/AR4：`book_name` 为当前书名——@get:{bookName} 内建回退源（legacy setBook）
/// P1 跨阶段：`book_url` 非空时按 book 级作底、tocUrl 级覆盖合并读取（详情 @put 直达目录）；
/// 结束后双键回写（目录阶段变量对正文可见）
///
/// legacy 对齐：抓取报错标记运行期失效快照，成功则清除该源标记。
#[allow(clippy::too_many_arguments)]
pub async fn analyze_toc(
    ns: &str,
    toc_url: &str,
    source: &BookSource,
    max_pages: usize,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<BookChapter>> {
    match analyze_toc_impl(ns, toc_url, source, max_pages, book_name, book_url).await {
        Ok(v) => {
            crate::service::health::clear_source_invalid(ns, &source.book_source_url);
            if let Some(st) = crate::parser::rule::registered_storage() {
                let _ = st
                    .record_path_outcome(ns, &source.book_source_url, "toc", !v.is_empty())
                    .await;
            }
            Ok(v)
        }
        Err(e) => {
            crate::service::health::mark_source_invalid(
                ns,
                &source.book_source_url,
                &e.to_string(),
            );
            if let Some(st) = crate::parser::rule::registered_storage() {
                let _ = st
                    .record_path_outcome(ns, &source.book_source_url, "toc", false)
                    .await;
            }
            Err(e)
        }
    }
}

/// 目录抓取总预算(秒, env READER_TOC_BUDGET_SECS 覆盖, 默认 30): 链表分页串行抓,
/// 卡源单页 CF 求解+超时可到 25-35s, 无总预算时实测 100s+ 无响应; 到点即报错可重试
fn toc_budget_secs() -> u64 {
    static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let base = *B.get_or_init(|| {
        std::env::var("READER_TOC_BUDGET_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v: &u64| *v > 0)
            .unwrap_or(30)
    });
    match TOC_BUDGET_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => base,
        v => v,
    }
}

/// 测试覆盖总预算(None = 用 env/默认)
static TOC_BUDGET_OVERRIDE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
fn set_toc_budget_secs(v: u64) {
    TOC_BUDGET_OVERRIDE.store(v, std::sync::atomic::Ordering::Relaxed);
}

async fn analyze_toc_impl(
    ns: &str,
    toc_url: &str,
    source: &BookSource,
    max_pages: usize,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<BookChapter>> {
    let mut all: Vec<BookChapter> = Vec::new();
    let toc_t0 = std::time::Instant::now();
    let toc_budget = std::time::Duration::from_secs(toc_budget_secs());
    let mut current_url = toc_url.to_string();
    let mut reverse = false;
    // legado Book.putVariable：详情（getBookInfo）写入的变量在目录/正文流程共享
    // P1 双键合并：book_url 级作底、toc_url 级覆盖
    let mut vars =
        crate::parser::rule::load_book_vars_merged(ns, &source.book_source_url, book_url, toc_url);
    vars.book_name = book_name.map(str::to_string);

    for _page in 0..max_pages {
        if toc_t0.elapsed() > toc_budget {
            return Err(anyhow::anyhow!(
                "目录抓取超时(预算 {}s, 已抓 {} 页): 源响应过慢, 可点刷新目录重试",
                toc_budget.as_secs(),
                _page
            ));
        }
        let resp = fetch_url(ns, &current_url, source).await?;
        // legado WebBook.getChapterList：目录页抓取后执行 loginCheckJs
        let page_body = apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;
        let base = resp.url.clone();
        // E10/AR5：真实页 URL → JS 求值绑定 baseUrl（push_js_context 透传）
        vars.insert("baseUrl".to_string(), base.clone());
        let rule: TocRule = source
            .rule_toc
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let Some(list_rule) = rule.chapter_list.clone() else {
            break;
        };
        // legado 目录列表前缀：`-` = 全部目录倒序；`+` = 仅去前缀
        let (list_rule, page_reverse) = crate::service::search::strip_list_rule_prefix(&list_rule);
        reverse |= page_reverse;

        // legado init：目录上下文提取（每页应用）
        let mut page_html =
            crate::parser::rule::apply_init_with_vars(&page_body, rule.init.as_deref(), &mut vars);
        // legado preUpdateJs：目录解析前 JS 预处理（result=抓取内容）
        if let Some(js) = &rule.pre_update_js {
            if !js.trim().is_empty() {
                vars.insert("result".to_string(), page_html.clone());
                page_html = crate::parser::js::eval_js(js.trim(), &vars).unwrap_or(page_html);
            }
        }
        // E10（目录路径补全）：src 绑定 = 当前页预处理后文档
        vars.insert("src".to_string(), page_html.clone());
        let items = toc_items(&list_rule, &page_html);
        let start_index = all.len() as i64;
        let chapters = chapters_from_items(&items, &rule, &base, start_index, &mut vars);
        // 正文流程只带章节 URL——按章节 URL 再存一份，保证 getBookContent 命中 (批量单事务)
        let urls: Vec<String> = chapters.iter().map(|ch| ch.url.clone()).collect();
        crate::parser::rule::save_book_vars_batch(ns, &source.book_source_url, &urls, &vars);
        all.extend(chapters);

        // 多页目录
        let next = rule
            .next_toc_url
            .as_deref()
            .map(|r| {
                crate::service::search::field_url_with_vars(
                    &page_body,
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

    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        toc_url,
        &vars,
    );
    // legado：多页目录汇总后去重（LinkedHashSet 保序）；`-` 前缀时最终列表倒序
    let mut all = dedupe_chapters(all);
    if reverse {
        all.reverse();
    }
    for (i, ch) in all.iter_mut().enumerate() {
        ch.index = i as i64;
    }
    Ok(all)
}

/// 单页目录解析（ruleToc 应用一次——getChapterListByRule 调试接口复用）
pub async fn parse_toc_page(
    ns: &str,
    url: &str,
    source: &BookSource,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<BookChapter>> {
    let resp = fetch_url(ns, url, source).await?;
    let page_body = apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;
    let base = resp.url.clone();
    // P1 双键合并：book_url 级作底、当前页级覆盖
    let mut vars =
        crate::parser::rule::load_book_vars_merged(ns, &source.book_source_url, book_url, url);
    vars.book_name = book_name.map(str::to_string);
    // E10/AR5：真实页 URL → JS 求值绑定 baseUrl
    vars.insert("baseUrl".to_string(), base.clone());
    let rule: TocRule = source
        .rule_toc
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(list_rule) = rule.chapter_list.clone() else {
        return Ok(vec![]);
    };
    let (list_rule, reverse) = crate::service::search::strip_list_rule_prefix(&list_rule);
    let mut page_html =
        crate::parser::rule::apply_init_with_vars(&page_body, rule.init.as_deref(), &mut vars);
    if let Some(js) = &rule.pre_update_js {
        if !js.trim().is_empty() {
            vars.insert("result".to_string(), page_html.clone());
            page_html = crate::parser::js::eval_js(js.trim(), &vars).unwrap_or(page_html);
        }
    }
    let items = toc_items(&list_rule, &page_html);
    let chapters = chapters_from_items(&items, &rule, &base, 0, &mut vars);
    let urls: Vec<String> = chapters.iter().map(|ch| ch.url.clone()).collect();
    crate::parser::rule::save_book_vars_batch(ns, &source.book_source_url, &urls, &vars);
    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        url,
        &vars,
    );
    let mut chapters = dedupe_chapters(chapters);
    if reverse {
        chapters.reverse();
    }
    for (i, ch) in chapters.iter_mut().enumerate() {
        ch.index = i as i64;
    }
    Ok(chapters)
}

/// chapterList 规则 → 章节上下文列表（CSS/JSONPath/Regex/JS 全类型）
pub(crate) fn toc_items(list_rule: &str, body: &str) -> Vec<String> {
    let parsed = parse_rule(list_rule);
    let mut items: Vec<String> = match parsed.kind {
        RuleKind::Css => css_chain(list_rule, body),
        RuleKind::JsonPath | RuleKind::Regex => apply(list_rule, body),
        RuleKind::Js => js_chapter_items(list_rule, body),
        _ => vec![],
    };
    // <js> 包裹形式（parse_rule 不识别为 Js）——兜底
    if items.is_empty()
        && (list_rule.contains("<js>") || list_rule.trim_start().starts_with("@js:"))
    {
        items = js_chapter_items(list_rule, body);
    }
    items
}

/// JS chapterList（<js> 或 @js:——eval 返回章节对象数组）→ 每项 JSON 文本
/// （数组经递归 JSON 转换——避免 ToString 的 "[object Object]" 使解析为空）
fn js_chapter_items(rule: &str, body: &str) -> Vec<String> {
    let code = if rule.trim_start().starts_with("@js:") {
        rule.trim_start()[4..].to_string()
    } else if let Some(start) = rule.find("<js>") {
        let rest = &rule[start + 4..];
        let end = rest.find("</js>").unwrap_or(rest.len());
        rest[..end].to_string()
    } else {
        return vec![];
    };
    let mut vars = std::collections::HashMap::new();
    vars.insert("result".to_string(), body.to_string());
    vars.insert("key".to_string(), String::new());
    vars.insert("page".to_string(), "1".to_string());
    let Ok(result) = crate::parser::js::eval_js_json(&code, &vars) else {
        return vec![];
    };
    match result {
        serde_json::Value::Array(list) => list
            .iter()
            .map(|item| match item {
                serde_json::Value::Object(_) => item.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect(),
        serde_json::Value::Object(_) => vec![result.to_string()],
        _ => vec![],
    }
}

/// 章节上下文列表 → 章节（字段规则应用 + 相对 URL 转绝对）
fn chapters_from_items(
    items: &[String],
    rule: &TocRule,
    base: &str,
    start_index: i64,
    vars: &mut crate::parser::rule::RuleVars,
) -> Vec<BookChapter> {
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let title = crate::service::search::field_with_vars(
                item,
                rule.chapter_name.as_deref(),
                "",
                vars,
            );
            let url = match &rule.chapter_url {
                Some(r) => {
                    crate::service::search::field_url_with_vars(item, Some(r), "", base, vars)
                }
                None => String::new(),
            };
            if title.is_empty() && url.is_empty() {
                return None;
            }
            // legacy isVolume 规则优先；无规则时按标题特征判断
            let is_volume = match &rule.is_volume {
                Some(r) => {
                    let v = crate::service::search::field_with_vars(item, Some(r), "", vars);
                    is_true(&v)
                }
                None => title.starts_with("卷") || title.contains("【卷"),
            };
            // legacy：卷章节无 URL 用 标题+序号 占位；普通章节无 URL 用当前页 URL
            let url = if url.is_empty() {
                if is_volume {
                    format!("{title}{i}")
                } else {
                    base.to_string()
                }
            } else {
                to_abs(&url, base)
            };
            let mut title = title;
            // legacy isVip 命中 → 标题前加锁图标
            if let Some(vip_rule) = &rule.chapter_vip {
                let v = crate::service::search::field_with_vars(item, Some(vip_rule), "", vars);
                if is_true(&v) {
                    title = format!("\u{1F512}{title}");
                }
            }
            // legacy BookChapter.tag：目录规则 updateTime 的解析结果
            let tag = crate::service::search::opt_field_with_vars(
                item,
                rule.update_time.as_deref(),
                vars,
            )
            .filter(|s| !s.trim().is_empty());
            Some(BookChapter {
                title,
                url,
                tag,
                is_volume,
                index: start_index + i as i64,
            })
        })
        .collect()
}

/// 章节去重（legacy LinkedHashSet 语义）：按 (title, url, is_volume) 保序去重
fn dedupe_chapters(chapters: Vec<BookChapter>) -> Vec<BookChapter> {
    let mut seen = std::collections::HashSet::new();
    chapters
        .into_iter()
        .filter(|c| seen.insert((c.title.clone(), c.url.clone(), c.is_volume)))
        .collect()
}

/// legacy `String?.isTrue()`：空白/`null`/`false`/`no`/`not`/`0` → false，其余 true
fn is_true(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("null") {
        return false;
    }
    !v.eq_ignore_ascii_case("false")
        && !v.eq_ignore_ascii_case("no")
        && !v.eq_ignore_ascii_case("not")
        && v != "0"
}

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
fn collect_urls(value: &str, out: &mut Vec<String>) {
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
    // 支持 && 多段链、### 尾标仅首次替换；无 ## 的纯段视为删除型整段正则
    if let Some(rr) = &rule.replace_regex {
        for seg in rr.split("&&") {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            let mut parts = seg.splitn(3, "##");
            let Some(pat) = parts.next() else { continue };
            let Some(rep) = parts.next() else {
                // 无 ##：整段为匹配删除正则
                match crate::util::regex::Regex::new(pat.trim()) {
                    Ok(re) => content = re.replace_all(&content, "").to_string(),
                    Err(e) => tracing::warn!("replaceRegex 段编译失败（跳过）: {e}"),
                }
                continue;
            };
            let replace_first = parts.next().map(|f| f.contains('#')).unwrap_or(false);
            match crate::util::regex::Regex::new(pat.trim()) {
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

/// 相对 URL → 绝对
fn to_abs(url: &str, base: &str) -> String {
    crate::service::search::to_absolute(url, base)
}

/// legado init 语义：先提取上下文（JSONPath/CSS/JS），字段规则相对应用
/// fetch_url 的 legado AnalyzeUrl 后缀支持：js 修改 URL / headers / bodyJs / POST
#[cfg(test)]
mod fetch_url_suffix_tests {
    use super::*;
    use crate::model::book_source::BookSource;
    use crate::service::crawler::ssrf_allow_private_guard;

    /// 简单 mock：记录请求头，返回固定 body（或按 path 路由）
    async fn mock() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let req = String::from_utf8_lossy(&buf);
                    let path = req
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split(' ')
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    let has_x = req.to_lowercase().contains("x-test");
                    let body = if has_x {
                        format!("BODY-FOR-{path}-WITH-HEADER")
                    } else {
                        format!("BODY-FOR-{path}")
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body,
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetch_url_js_and_headers_suffix() {
        let _ssrf = ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1
        let base = mock().await;
        let mut source = BookSource::default();
        source.book_source_url = format!("{base}/src");
        // js 键返回完整新 URL（eval 注入 baseUrl/key/page/headerMap/result——无 url 变量）
        let url =
            format!("{base}/orig,{{\"js\":\"'{base}/new'\",\"headers\":{{\"X-Test\":\"1\"}}}}");
        let resp = fetch_url("default", &url, &source).await.unwrap();
        assert!(
            resp.body.contains("/new") && resp.body.contains("WITH-HEADER"),
            "js 修改 URL + headers 应生效: {}",
            resp.body
        );
    }

    #[tokio::test]
    async fn fetch_url_body_js() {
        let _ssrf = ssrf_allow_private_guard(true);
        let base = mock().await;
        let source = BookSource::default();
        let url = format!("{base}/x,{{\"bodyJs\":\"result.replace('BODY','TEXT')\"}}");
        let resp = fetch_url("default", &url, &source).await.unwrap();
        assert!(
            resp.body.contains("TEXT-FOR"),
            "bodyJs 应改写响应体: {}",
            resp.body
        );
    }

    #[tokio::test]
    async fn fetch_url_plain_no_suffix() {
        let _ssrf = ssrf_allow_private_guard(true);
        let base = mock().await;
        let source = BookSource::default();
        let resp = fetch_url("default", &format!("{base}/plain"), &source)
            .await
            .unwrap();
        assert_eq!(resp.body, "BODY-FOR-/plain");
    }
}

#[cfg(test)]
mod init_rule_tests {
    use super::*;
    use crate::model::book_source::BookSource;

    fn src_with(book_info: serde_json::Value) -> BookSource {
        let mut s = BookSource::default();
        s.rule_book_info = Some(book_info);
        s
    }

    #[test]
    fn dbg_rule_deser() {
        let v = serde_json::json!({"init": "$.data", "name": "$.novelName", "author": "$.author", "tocUrl": "/novel/{{$.novelId}}/chapters"});
        let r: BookInfoRule = serde_json::from_value(v).unwrap();
        eprintln!("init={:?} name={:?} toc={:?}", r.init, r.name, r.toc_url);
    }

    #[test]
    fn dbg_apply_init() {
        let html = r#"{"code":0,"data":{"novelId":"bY7oM0","novelName":"诡秘之主","author":"爱潜水的乌贼"}}"#;
        let out = crate::parser::rule::apply_init(html, Some("$.data"));
        eprintln!("apply_init($.data) = {out}");
        let v = crate::parser::rule::apply("$.novelName", &out);
        eprintln!("apply($.novelName) = {v:?}");
    }

    #[test]
    fn book_info_init_jsonpath_context() {
        // 猫眼类 JSON API：init=$.data → name/author 在 data 子对象上
        let source = src_with(serde_json::json!({
            "init": "$.data",
            "name": "$.novelName",
            "author": "$.author",
            "tocUrl": "/novel/{{$.novelId}}/chapters"
        }));
        let html = r#"{"code":0,"data":{"novelId":"bY7oM0","novelName":"诡秘之主","author":"爱潜水的乌贼"}}"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://api.jmlldsc.com/novel/bY7oM0?isSearch=1",
            &source,
            "http://api.jmlldsc.com/novel/bY7oM0?isSearch=1",
            None,
        );
        assert_eq!(
            info.name, "诡秘之主",
            "init 后 name 应相对 data 提取: {:?}",
            info.name
        );
        assert_eq!(info.author, "爱潜水的乌贼");
        assert!(
            info.toc_url
                .as_deref()
                .unwrap_or("")
                .ends_with("/novel/bY7oM0/chapters"),
            "tocUrl {{}} 内嵌应展开: {:?}",
            info.toc_url
        );
    }

    #[test]
    fn book_info_init_absent_uses_raw() {
        let source = src_with(serde_json::json!({
            "name": "class.title@text",
            "author": "class.author@text"
        }));
        let html = r#"<html><body><div class="title">书名A</div><div class="author">作者A</div></body></html>"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://x.com",
            &source,
            "http://x.com/b",
            None,
        );
        assert_eq!(info.name, "书名A");
        assert_eq!(info.author, "作者A");
    }

    #[test]
    fn book_info_init_js() {
        let source = src_with(serde_json::json!({
            "init": "@js:JSON.parse(result).data",
            "name": "$.novelName"
        }));
        let html = r#"{"data":{"novelName":"JS书名"}}"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://x.com",
            &source,
            "http://x.com/b",
            None,
        );
        assert_eq!(
            info.name, "JS书名",
            "JS init 后 JSONPath 相对提取: {:?}",
            info.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_source() -> BookSource {
        BookSource {
            book_source_url: "http://127.0.0.1:9999".into(),
            book_source_name: "测试源".into(),
            rule_book_info: Some(serde_json::json!({
                "name": "h1.bookname@text", "author": "p.author@text",
                "intro": "div.intro@text", "coverUrl": "img.cover@src",
                "tocUrl": "/toc"
            })),
            rule_toc: Some(serde_json::json!({
                "chapterList": "ul.chapters@li",
                "chapterName": "a@text", "chapterUrl": "a@href"
            })),
            rule_content: Some(serde_json::json!({
                "content": "div.content@text"
            })),
            ..Default::default()
        }
    }

    #[test]
    fn test_analyze_info() {
        let html = r#"<h1 class="bookname">测试书</h1><p class="author">作者X</p>
            <div class="intro">简介内容</div><img class="cover" src="/cover.jpg">"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://127.0.0.1:9999/book/1",
            &test_source(),
            "http://127.0.0.1:9999/book/1",
            None,
        );
        assert_eq!(info.name, "测试书");
        assert_eq!(info.author, "作者X");
        assert_eq!(info.intro.as_deref(), Some("简介内容"));
        assert_eq!(
            info.cover_url.as_deref(),
            Some("http://127.0.0.1:9999/cover.jpg"),
            "详情封面应转绝对 URL（否则书架显示首字封面）"
        );
        assert_eq!(info.toc_url.as_deref(), Some("http://127.0.0.1:9999/toc"));
    }

    /// legacy BookInfo：tocUrl 规则缺失/为空时回退 baseUrl（详情页即目录页）
    #[test]
    fn test_analyze_info_toc_fallback_base() {
        let mut src = test_source();
        src.rule_book_info = Some(serde_json::json!({
            "name": "h1.bookname@text",
            "author": "p.author@text"
        }));
        let base = "http://127.0.0.1:9999/book/1";
        let html = r#"<h1 class="bookname">测试书</h1><p class="author">作者X</p>"#;
        let info = analyze_book_info("default", html, base, &src, base, None);
        assert_eq!(info.toc_url.as_deref(), Some(base), "tocUrl 应回退 baseUrl");
    }

    /// legacy canReName：书源规则未声明 canReName 时保留书架已有书名/作者
    #[test]
    fn test_can_rename_only_with_rule() {
        let mut src = test_source();
        src.rule_book_info = Some(serde_json::json!({
            "name": "h1.bookname@text",
            "author": "p.author@text"
        }));
        let base = "http://127.0.0.1:9999/book/1";
        let html = r#"<h1 class="bookname">规则新名</h1><p class="author">规则作者</p>"#;
        let info = analyze_book_info_with_existing(html, base, &src, base, "书架旧名", "书架作者");
        assert_eq!(info.name, "书架旧名", "无 canReName 不应覆盖书架书名");
        assert_eq!(info.author, "书架作者", "无 canReName 不应覆盖书架作者");

        // canReName 规则非空 → 允许覆盖
        src.rule_book_info = Some(serde_json::json!({
            "name": "h1.bookname@text",
            "author": "p.author@text",
            "canReName": "true"
        }));
        let info = analyze_book_info_with_existing(html, base, &src, base, "书架旧名", "书架作者");
        assert_eq!(info.name, "规则新名");
        assert_eq!(info.author, "规则作者");
    }

    /// legacy BookChapter.tag：目录规则 updateTime 解析结果写入章节附加信息
    #[test]
    fn test_chapter_tag_from_update_time() {
        let rule = TocRule {
            chapter_list: Some("@js:[{t:'章A',u:'/x/1',time:'2026-08-08'}]".into()),
            chapter_name: Some("$.t".into()),
            chapter_url: Some("$.u".into()),
            update_time: Some("$.time".into()),
            ..Default::default()
        };
        let items = toc_items(rule.chapter_list.as_deref().unwrap(), "{}");
        assert_eq!(items.len(), 1);
        let mut vars = crate::parser::rule::RuleVars::new();
        let chapters = chapters_from_items(&items, &rule, "https://src.test", 0, &mut vars);
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].tag.as_deref(), Some("2026-08-08"));
    }

    /// 链表分页串行抓的总预算: 每页 2s 的双页源 + 预算 1s → 第二页前报错, 不再百秒无响应
    #[tokio::test]
    async fn test_toc_budget_caps_serial_pagination() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let mut req = String::new();
                    loop {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        req.push_str(&String::from_utf8_lossy(&buf[..n]));
                        if req.contains("\r\n\r\n") {
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let path = req.split_whitespace().nth(1).unwrap_or("/");
                    let body = if path.starts_with("/toc2") {
                        r#"{"ch":[{"t":"第二章","u":"/c/2"}]}"#
                    } else {
                        r#"{"ch":[{"t":"第一章","u":"/c/1"}],"next":"/toc2"}"#
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        let mut src = test_source();
        src.book_source_url = format!("http://{addr}/src");
        src.rule_toc = Some(serde_json::json!({
            "chapterList": "@js:$.ch",
            "chapterName": "$.t",
            "chapterUrl": "$.u",
            "nextTocUrl": "$.next"
        }));
        set_toc_budget_secs(1);
        let t0 = std::time::Instant::now();
        let res = analyze_toc("default", &format!("http://{addr}/toc"), &src, 20, None, "").await;
        let elapsed = t0.elapsed();
        set_toc_budget_secs(0);
        let err = res.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            err.contains("预算") && elapsed < std::time::Duration::from_secs(10),
            "应报预算超时且快: err={err} elapsed={elapsed:?}"
        );
    }

    /// 详情 @put → 目录 @get：书级变量跨 getBookInfo/getChapterList 贯通
    #[tokio::test]
    async fn test_put_get_flows_into_toc() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let base = serve(r#"{"data":[{"t":"第一章","u":"/c/1"}]}"#).await;
        let mut src = test_source();
        src.book_source_url = format!("{base}/src");
        src.rule_book_info = Some(serde_json::json!({
            "init": "@put:{bid:$.book_id}",
            "name": "$.name",
            "coverUrl": "$.cover",
            "tocUrl": format!("{base}/toc")
        }));
        src.rule_toc = Some(serde_json::json!({
            "chapterList": "$.data",
            "chapterName": "$.t",
            "chapterUrl": "https://x.test/c/@get:{bid}{{$.u}}"
        }));
        let book_url = format!("{base}/book/1");
        let html = r#"{"book_id":"abc","name":"书","cover":"/c.jpg"}"#;
        let info = analyze_book_info("default", html, &book_url, &src, &book_url, None);
        let expected_cover = format!("{base}/c.jpg");
        assert_eq!(info.name, "书");
        assert_eq!(info.cover_url.as_deref(), Some(expected_cover.as_str()));
        let toc_url = info.toc_url.clone().unwrap();
        let chapters = analyze_toc("default", &toc_url, &src, 2, None, &book_url)
            .await
            .unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "第一章");
        assert_eq!(
            chapters[0].url, "https://x.test/c/abc/c/1",
            "目录规则中 @get 应取详情 @put 的值: {:?}",
            chapters[0].url
        );
    }

    // ---------------- P1：@put/@get 变量持久化 + 跨阶段贯通 ----------------

    /// 串行化 SQLite 注册类测试（BOOK_VARS_STORAGE 为进程级单例——并发注册会互相覆盖句柄）
    static BOOK_VARS_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 独立临时库 + 注册全局持久化句柄；收尾必须调 [`teardown_book_vars_db`]
    async fn setup_book_vars_db(tag: &str) -> (crate::storage::Storage, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("reader-p1-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = crate::storage::init(&config).await.unwrap();
        crate::parser::rule::register_book_vars_storage(storage.clone());
        (storage, dir)
    }

    fn teardown_book_vars_db(storage: crate::storage::Storage, dir: &std::path::Path) {
        crate::parser::rule::clear_book_vars_storage();
        drop(storage);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P1-1 详情 @put → 重启模拟（清内存缓存）→ 不跑目录直接取正文——
    /// book_url 键从 SQLite 读穿透回填，正文规则中的 @get:{bid} 仍可命中
    #[tokio::test]
    async fn test_detail_put_survives_restart_into_content() {
        let _guard = BOOK_VARS_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let detail_base = serve(r#"{"book_id":"abc","name":"书"}"#).await;
        let ch_base = serve("BID=abc&TAIL").await;

        let ns = format!("p1dc-{}", uuid::Uuid::new_v4());
        let (storage, dir) = setup_book_vars_db("dc").await;
        let mut src = test_source();
        src.book_source_url = format!("{detail_base}/src");
        src.rule_book_info = Some(serde_json::json!({
            "init": "@put:{bid:$.book_id}",
            "name": "$.name"
        }));
        src.rule_content = Some(serde_json::json!({ "content": "^BID=@get:{bid}&(.+)$" }));

        // ① 详情阶段：@put 写入（内存 + SQLite 同步双写）
        let book_url = format!("{detail_base}/book/1");
        analyze_book_info(
            &ns,
            r#"{"book_id":"abc","name":"书"}"#,
            &book_url,
            &src,
            &book_url,
            None,
        );
        let row = storage
            .get_book_vars_cache(&ns, &src.book_source_url, &book_url)
            .await
            .unwrap();
        assert!(
            row.as_deref().map(|j| j.contains("abc")).unwrap_or(false),
            "详情 @put 应已同步落库: {row:?}"
        );

        // ② 重启模拟：清内存缓存（SQLite 保留）
        crate::parser::rule::clear_book_vars_memory_cache_ns(&ns);

        // ③ 不跑目录直接取正文：book_url 级读穿透回填 → @get:{bid} 命中
        let chapter_url = format!("{ch_base}/c/1");
        let content = analyze_content(&ns, &chapter_url, &src, 1, None, None, &book_url)
            .await
            .unwrap();
        assert_eq!(
            content, "TAIL",
            "重启后直接取正文应能读到详情阶段 @put 的变量"
        );

        teardown_book_vars_db(storage, &dir);
    }

    /// P1-2 目录阶段变量对正文可见（跨阶段）：目录 init @put 的 tid 逐章落盘；
    /// 重启模拟后——① 已知章节按章节键读穿透命中；② 全新章节 URL 无章节键，
    /// 经 book 级双键合并回退仍命中
    #[tokio::test]
    async fn test_toc_vars_visible_to_content_after_restart() {
        let _guard = BOOK_VARS_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let toc_base = serve(
            r#"{"toc_id":"77","data":[{"t":"第一章","u":"/c/1"},{"t":"第二章","u":"/c/2"}]}"#,
        )
        .await;
        let ch_base = serve("TID=77&TAIL").await;

        let ns = format!("p1tv-{}", uuid::Uuid::new_v4());
        let (storage, dir) = setup_book_vars_db("tv").await;
        let mut src = test_source();
        src.book_source_url = format!("{toc_base}/src");
        src.rule_toc = Some(serde_json::json!({
            "init": "@put:{tid:$.toc_id}",
            "chapterList": "$.data",
            "chapterName": "$.t",
            "chapterUrl": format!("{ch_base}/c/@get:{{tid}}")
        }));
        src.rule_content = Some(serde_json::json!({ "content": "^TID=@get:{tid}&(.+)$" }));

        // ① 目录阶段：tid 写入并逐章 + 目录级落盘
        let toc_url = format!("{toc_base}/toc");
        let chapters = analyze_toc(&ns, &toc_url, &src, 2, None, "").await.unwrap();
        assert_eq!(chapters.len(), 2);
        assert!(
            chapters[0].url.ends_with("/c/77"),
            "章节 URL 应拼入目录阶段 @get 值: {}",
            chapters[0].url
        );
        let ch_row = storage
            .get_book_vars_cache(&ns, &src.book_source_url, &chapters[0].url)
            .await
            .unwrap();
        assert!(
            ch_row.as_deref().map(|j| j.contains("77")).unwrap_or(false),
            "目录阶段应逐章落库: {ch_row:?}"
        );

        // ② 重启模拟
        crate::parser::rule::clear_book_vars_memory_cache_ns(&ns);

        // ③a 已知章节直接取正文：章节键读穿透回填
        let content = analyze_content(&ns, &chapters[0].url, &src, 1, None, None, "")
            .await
            .unwrap();
        assert_eq!(content, "TAIL", "重启后章节键应从 SQLite 回填目录阶段变量");

        // ③b 全新章节 URL（无章节键）：book 级（tocUrl 键）合并回退命中
        let fresh = format!("{ch_base}/c/fresh");
        let content2 = analyze_content(&ns, &fresh, &src, 1, None, None, &toc_url)
            .await
            .unwrap();
        assert_eq!(
            content2, "TAIL",
            "无章节键时应经 book 级双键合并读到目录阶段变量"
        );

        teardown_book_vars_db(storage, &dir);
    }

    /// P1 rule-API 级回环：两级保存落库 → 清内存 → load_merged 读穿透重组
    #[tokio::test]
    async fn test_book_vars_sqlite_roundtrip_unit() {
        let _guard = BOOK_VARS_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let ns = format!("p1rt-{}", uuid::Uuid::new_v4());
        let (storage, dir) = setup_book_vars_db("rt").await;
        let src_key = "https://src.test/rt";
        let mut vars = crate::parser::rule::RuleVars::new();
        vars.insert("token".to_string(), "T0KEN-9".to_string());
        crate::parser::rule::save_book_vars_two_level(
            &ns,
            src_key,
            "https://b.test/book",
            "https://b.test/c/1",
            &vars,
        );

        let leaf = storage
            .get_book_vars_cache(&ns, src_key, "https://b.test/c/1")
            .await
            .unwrap();
        assert!(leaf.is_some(), "leaf 键应落库");
        let root = storage
            .get_book_vars_cache(&ns, src_key, "https://b.test/book")
            .await
            .unwrap();
        assert!(root.is_some(), "root 键应落库");

        crate::parser::rule::clear_book_vars_memory_cache_ns(&ns);
        let merged = crate::parser::rule::load_book_vars_merged(
            &ns,
            src_key,
            "https://b.test/book",
            "https://b.test/c/1",
        );
        assert_eq!(
            merged.get("token").map(String::as_str),
            Some("T0KEN-9"),
            "清内存后应读穿透 SQLite 回填"
        );
        // 上下文字段不随持久化复活
        assert!(merged.chapter_title.is_none() && merged.book_name.is_none());

        teardown_book_vars_db(storage, &dir);
    }

    #[test]
    fn test_analyze_content_from() {
        let html =
            r#"<html><div class="content">第一章正文内容测试。</div><script>干扰</script></html>"#;
        let content = analyze_content_from(html, &test_source());
        assert_eq!(content, "第一章正文内容测试。");
    }

    #[test]
    fn test_analyze_content_replace() {
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({
            "content": "div.content@text",
            "replaceRegex": "\\s+## "
        }));
        let html = r#"<div class="content">多   个  空格</div>"#;
        let content = analyze_content_from(html, &src);
        assert_eq!(content, "多个空格");
    }

    /// E9：replaceRegex 多段链（&&）+ ### 尾标仅首次替换 + 无 ## 段删除语义
    #[test]
    fn test_analyze_content_replace_multi_segment() {
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({
            "content": "div.content@text",
            // 段1：删除"广告"（无 ## → 删除型）；段2：尾部标记替换为【完】；
            // 段3：o→0 仅首次（### 尾标）
            "replaceRegex": "广告&&尾部##【完】&&o##0###"
        }));
        let html = r#"<div class="content">广告正文oogo尾部</div>"#;
        let content = analyze_content_from(html, &src);
        assert_eq!(
            content, "正文0ogo【完】",
            "段序：删广告→尾部换【完】→首个 o 变 0"
        );
    }

    /// chapterList JS 规则（JSON.parse(result).data 数组）→ 章节上下文列表
    #[test]
    fn test_toc_items_js_array() {
        let body =
            r#"{"data":[{"title":"第一章","href":"/c/1"},{"title":"第二章","href":"/c/2"}]}"#;
        let items = toc_items("@js:JSON.parse(result).data", body);
        assert_eq!(items.len(), 2, "JS chapterList 应解析出 2 项");
        assert!(items[0].contains("第一章"));
        assert!(items[0].contains("/c/1"));
    }

    /// chapterList JS 数组字面量 + 字段规则 → 章节（title/url 绝对化/index）
    #[test]
    fn test_toc_js_full_pipeline() {
        let rule = TocRule {
            chapter_list: Some("@js:[{t:'章A',u:'/x/1'},{t:'章B',u:'/x/2'}]".into()),
            chapter_name: Some("$.t".into()),
            chapter_url: Some("$.u".into()),
            ..Default::default()
        };
        let items = toc_items(rule.chapter_list.as_deref().unwrap(), "{}");
        assert_eq!(items.len(), 2);
        let mut vars = crate::parser::rule::RuleVars::new();
        let chapters = chapters_from_items(&items, &rule, "https://src.test", 5, &mut vars);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "章A");
        assert_eq!(chapters[0].url, "https://src.test/x/1");
        assert_eq!(chapters[0].index, 5);
        assert_eq!(chapters[1].index, 6);
    }

    /// <js> 包裹 chapterList 兜底
    #[test]
    fn test_toc_items_js_html_wrapped() {
        let body = r#"{"data":[{"title":"包章","url":"/b/1"}]}"#;
        let items = toc_items("<js>JSON.parse(result).data</js>", body);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("包章"));
    }

    /// GAP 17b：ruleRelated CSS 链式解析 → relatedBooks
    #[test]
    fn test_analyze_related_books() {
        let mut src = test_source();
        src.rule_related = Some(serde_json::json!({
            "bookList": "ul.related@li",
            "name": "a.bookname@text",
            "author": "span.author@text",
            "bookUrl": "a@href",
            "coverUrl": "img@src"
        }));
        let html = r#"<ul class="related">
            <li><a class="bookname" href="/r/1">推荐书1</a><span class="author">作者甲</span><img src="/c1.jpg"></li>
            <li><a class="bookname" href="/r/2">推荐书2</a><span class="author">作者乙</span><img src="/c2.jpg"></li>
        </ul>"#;
        let related = analyze_related_books(html, "http://127.0.0.1:9999/book/1", &src);
        assert_eq!(related.len(), 2);
        assert_eq!(related[0].name, "推荐书1");
        assert_eq!(related[0].author, "作者甲");
        assert_eq!(related[0].book_url, "http://127.0.0.1:9999/r/1");
        // coverUrl 经 field_url 绝对化（与 ruleExplore 书单一致）
        assert_eq!(
            related[0].cover_url.as_deref(),
            Some("http://127.0.0.1:9999/c1.jpg")
        );
        assert_eq!(related[1].name, "推荐书2");
        // 无 ruleRelated / 无 bookList → 空
        assert!(analyze_related_books(html, "http://x", &test_source()).is_empty());
        let mut src2 = test_source();
        src2.rule_related = Some(serde_json::json!({"name": "a@text"}));
        assert!(analyze_related_books(html, "http://x", &src2).is_empty());
    }

    /// GAP 17b：getBookInfo 完整链路——analyze_book_info 返回 relatedBooks
    #[test]
    fn test_analyze_info_includes_related_books() {
        let mut src = test_source();
        src.rule_related = Some(serde_json::json!({
            "bookList": "ul.related@li",
            "name": "a@text",
            "bookUrl": "a@href"
        }));
        let html = r#"<h1 class="bookname">测试书</h1><p class="author">作者X</p>
            <ul class="related"><li><a href="/r/9">推荐书9</a></li></ul>"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://127.0.0.1:9999/book/1",
            &src,
            "http://127.0.0.1:9999/book/1",
            None,
        );
        assert_eq!(info.related_books.len(), 1);
        assert_eq!(info.related_books[0].name, "推荐书9");
        assert_eq!(info.related_books[0].book_url, "http://127.0.0.1:9999/r/9");
    }

    /// GAP 153：sourceRegex/replaceRegex 支持 lookbehind（regex crate 不支持）
    #[test]
    fn test_analyze_content_lookbehind_regex() {
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({
            "content": "div.content@text",
            "sourceRegex": "(?<=广告：)\\S+",
            "replaceRegex": "(?<=第).+(?=章)##X"
        }));
        let html = r#"<div class="content">正文：第一章 测试内容 广告：烦人</div>"#;
        let content = analyze_content_from(html, &src);
        // sourceRegex（lookbehind）移除 "烦人"；replaceRegex（lookbehind+lookahead）把 "一章 " 替换为 X
        assert_eq!(content, "正文：第X章 测试内容 广告：");
    }

    /// 书源正文含 HTML 标签（<p>/<br>/&nbsp; 等）时转纯文本：
    /// 换行元素 → \n、实体解码、其余标签剥离（前端纯文本渲染，原样透传会显示标签字面量）。
    #[test]
    fn test_analyze_content_converts_html_to_text() {
        let mut src = test_source();
        // @html 提取：<p>/<br> → 换行
        src.rule_content = Some(serde_json::json!({ "content": "div.content@html" }));
        let html = r#"<div class="content"><p>第一段</p><br><p>第二段</p></div>"#;
        let content = analyze_content_from(html, &src);
        assert_eq!(content, "第一段\n第二段", "HTML 应转纯文本段落: {content}");

        // 扩充块级标签（header/main/pre 等）与 CR 换行归一
        src.rule_content = Some(serde_json::json!({ "content": "div.content@html" }));
        let html = "<div class=\"content\"><header>标题</header><main><pre>第一段\r\n第二段</pre></main></div>";
        let content = analyze_content_from(html, &src);
        assert_eq!(
            content, "标题\n第一段\n第二段",
            "扩充块级标签与 CR 归一: {content}"
        );

        // &nbsp;/&amp;/数字实体解码 + 非换行标签剥离
        src.rule_content = Some(serde_json::json!({ "content": "div.content@html" }));
        let html = r#"<div class="content">甲&nbsp;乙 &amp; 丙 <span>保留</span><script>去掉</script>&#65;</div>"#;
        let content = analyze_content_from(html, &src);
        assert_eq!(
            content, "甲 乙 & 丙 保留A",
            "实体应解码、非换行标签应剥离: {content}"
        );

        // 无 @ 的裸选择器（legacy 兼容）→ 取纯文本（仅此处剥离，规则显式 @html 时保留）
        src.rule_content = Some(serde_json::json!({ "content": "div.content" }));
        let content = analyze_content_from(html, &src);
        assert!(!content.contains("<"), "裸选择器取文本: {content}");
        assert!(content.contains("甲") && content.contains("乙"));

        // 清洗（sourceRegex/replaceRegex）在 HTML → 文本前作用于原样内容
        src.rule_content = Some(serde_json::json!({
            "content": "div.content@html",
            "replaceRegex": "第一段##甲段"
        }));
        let html = r#"<div class="content"><p>第一段</p><br><p>第二段</p></div>"#;
        let content = analyze_content_from(html, &src);
        assert_eq!(
            content, "甲段\n第二段",
            "replaceRegex 后再转纯文本: {content}"
        );

        // 纯文本正文原样返回（无 HTML/实体不引入差异）
        let content = html_content_to_text("纯文本 1 < 2 & 3");
        assert_eq!(content, "纯文本 1 < 2 & 3");
    }

    /// 用户报告：部分书源返回无任何换行的纯文本正文，整章连成一大段。
    /// 长纯文本按句末标点补换行（短文本/已有换行不处理）。
    #[test]
    fn test_html_content_to_text_smart_paragraph_breaks() {
        let long = format!(
            "{}。{}。{}。",
            "他抬头看了看窗外的夜色".repeat(10),
            "远处传来隐约的钟声".repeat(10),
            "故事从这里正式开始".repeat(10)
        );
        let content = html_content_to_text(&long);
        assert!(
            content.contains('\n'),
            "长纯文本应按句末标点补换行: {}",
            &content[..content.len().min(80)]
        );
        // 已有换行不二次处理
        let with_nl = "第一段。\n第二段。";
        assert_eq!(html_content_to_text(with_nl), with_nl);
        // 短文本不处理
        let short = "短正文没有换行。";
        assert_eq!(html_content_to_text(short), short);
    }

    /// 用户报告：正文混入站点内嵌的正则脚本（`(本章未完|记住网址|加入书签)`）。
    /// script/style/noscript/template/iframe/head 的内容对正文不可见，必须整段剔除。
    #[test]
    fn test_html_content_to_text_skips_invisible_containers() {
        let html = concat!(
            r#"<div class="content"><p>正文第一段。</p>"#,
            r#"<script>var x = "(本章未完.*继续阅读)|记住.*网址.*com|『加入书签，方便阅读』";</script>"#,
            r#"<style>.ad{display:none}</style>"#,
            r#"<noscript>请开启 JavaScript</noscript>"#,
            r#"<p>正文第二段。</p></div>"#
        );
        let out = html_content_to_text(html);
        assert!(
            !out.contains("本章未完") && !out.contains("记住") && !out.contains("加入书签"),
            "脚本/样式/无脚本内容不应泄漏到正文: {out}"
        );
        assert!(!out.contains("display:none"), "样式内容不应泄漏: {out}");
        assert!(!out.contains("JavaScript"), "noscript 内容不应泄漏: {out}");
        assert!(
            out.contains("正文第一段。") && out.contains("正文第二段。"),
            "正文应保留: {out}"
        );
    }

    /// GAP 109：contentReplace/replaceRegex 在 ruleContent 解析已应用——
    /// 构造含广告行的正文，断言净化（广告行移除、正文保留）。
    /// （legacy 的 contentReplace 对应 ruleContent.replaceRegex：`模式##替换`，
    /// 逐条 replace_all；sourceRegex 为删除型清洗，同样在解析期应用）
    #[test]
    fn test_analyze_content_replace_regex_cleans_ad_lines() {
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({
            "content": "div.content@html",
            // 广告行 + 推广行 → 替换为空（净化）
            "replaceRegex": "【广告】.*?。|（推广）.*?。##"
        }));
        let html = concat!(
            r#"<div class="content"><p>正文第一段。</p>"#,
            r#"<p>【广告】本书由某某网首发，请支持正版。</p>"#,
            r#"<p>（推广）加群领福利。</p>"#,
            r#"<p>正文第二段。</p></div>"#
        );
        let content = analyze_content_from(html, &src);
        assert!(
            !content.contains("【广告】"),
            "广告行应被 replaceRegex 清除: {content}"
        );
        assert!(!content.contains("（推广）"), "推广行应被清除: {content}");
        assert!(
            content.contains("正文第一段。") && content.contains("正文第二段。"),
            "正文应保留: {content}"
        );

        // sourceRegex（删除型）同样应用于正文
        src.rule_content = Some(serde_json::json!({
            "content": "div.content@text",
            "sourceRegex": "【广告】[^】]*"
        }));
        let html = r#"<div class="content">正文甲。【广告】这是一条广告】正文乙。</div>"#;
        let content = analyze_content_from(html, &src);
        assert!(
            !content.contains("广告"),
            "sourceRegex 应删除广告片段: {content}"
        );
        assert!(content.contains("正文甲。") && content.contains("正文乙。"));
    }

    // ---------------- 非文本内容分派（音频/漫画/视频/文件） ----------------

    /// 微型 HTTP 服务器：固定响应体（同 crawler 测试模式）
    async fn serve(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for _ in 0..10 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut resp = head.into_bytes();
                resp.extend_from_slice(body.as_bytes());
                let _ = sock.write_all(&resp).await;
            }
        });
        format!("http://{addr}")
    }

    /// 音频书：ruleContent.content 提取音频 URL → analyze_media_url 返回音频流直链
    #[tokio::test]
    async fn test_analyze_media_url_audio() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
        let base =
            serve(r#"<html><div class="player"><audio src="/stream/1.mp3"></audio></div></html>"#)
                .await;
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({
            "content": "div.player audio@src"
        }));
        let url = analyze_media_url(
            "default",
            &format!("{base}/chapter/1"),
            &src,
            None,
            None,
            "",
        )
        .await
        .unwrap();
        assert_eq!(
            url,
            format!("{base}/stream/1.mp3"),
            "音频 URL 应提取并绝对化"
        );
        // contentType 映射
        assert_eq!(audio_content_type(&url), "audio/mpeg");
        assert_eq!(
            audio_content_type("https://x/a.m3u8?t=1"),
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(audio_content_type("https://x/a.m4a"), "audio/mp4");
    }

    /// 音频书：无 ruleContent（章节 URL 即音频流直链）→ 原样返回章节 URL
    #[tokio::test]
    async fn test_analyze_media_url_direct_chapter() {
        let mut src = test_source();
        src.rule_content = None;
        let url = analyze_media_url(
            "default",
            "https://cdn.example.com/audio/42.m4a",
            &src,
            None,
            None,
            "",
        )
        .await
        .unwrap();
        assert_eq!(url, "https://cdn.example.com/audio/42.m4a");
    }

    /// 漫画书：CSS 规则命中多个图片节点 → images 列表（绝对化 + 去重）
    #[tokio::test]
    async fn test_analyze_comic_images_css() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
        let base = serve(
            r#"<html><div class="imgs"><img src="/p/1.jpg"><img src="/p/2.jpg"><img src="/p/1.jpg"></div></html>"#,
        )
        .await;
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({ "content": "div.imgs img@src" }));
        let images =
            analyze_comic_images("default", &format!("{base}/comic/1"), &src, None, None, "")
                .await
                .unwrap();
        assert_eq!(
            images,
            vec![format!("{base}/p/1.jpg"), format!("{base}/p/2.jpg")],
            "应提取全部图片且去重保序、相对转绝对"
        );
    }

    /// 漫画书：@js: 规则返回 JSON 字符串数组 → images 列表
    #[tokio::test]
    async fn test_analyze_comic_images_js_array() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
        let base = serve(r#"{"data":["/a/1.webp","/a/2.webp"]}"#).await;
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({
            "content": "@js:JSON.parse(result).data"
        }));
        let images =
            analyze_comic_images("default", &format!("{base}/comic/2"), &src, None, None, "")
                .await
                .unwrap();
        assert_eq!(
            images,
            vec![format!("{base}/a/1.webp"), format!("{base}/a/2.webp")],
            "JS 数组应逐个提取并绝对化"
        );
    }

    /// 漫画书：无规则且章节 URL 即图片直链 → 单图列表；有规则但提取不到 → 空列表
    #[tokio::test]
    async fn test_analyze_comic_images_direct() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
                                                                             // 有规则但页面无匹配 → 空列表
        let base = serve(r#"<html><div class="imgs"></div></html>"#).await;
        let mut src = test_source();
        src.rule_content = Some(serde_json::json!({ "content": "div.imgs img@src" }));
        let images =
            analyze_comic_images("default", &format!("{base}/comic/3"), &src, None, None, "")
                .await
                .unwrap();
        assert!(images.is_empty(), "有规则但提取不到 → 空列表");

        // 无规则且章节 URL 即图片直链 → 单图列表（不抓取）
        let mut src2 = test_source();
        src2.rule_content = None;
        let images2 = analyze_comic_images(
            "default",
            "https://img.example.com/comic/5.jpg",
            &src2,
            None,
            None,
            "",
        )
        .await
        .unwrap();
        assert_eq!(
            images2,
            vec!["https://img.example.com/comic/5.jpg".to_string()]
        );
    }

    /// collect_urls 纯函数：URL 文本 / JSON 字符串数组 / 对象数组 / 空
    #[test]
    fn test_collect_urls() {
        let mut out = Vec::new();
        collect_urls("https://a.mp3", &mut out);
        collect_urls(r#"["https://b.jpg","/c.png"]"#, &mut out);
        collect_urls(
            r#"[{"url":"https://d.webp"},{"src":"https://e.avif"}]"#,
            &mut out,
        );
        collect_urls("  ", &mut out);
        assert_eq!(
            out,
            vec![
                "https://a.mp3",
                "https://b.jpg",
                "/c.png",
                "https://d.webp",
                "https://e.avif"
            ]
        );
        // 对象数组取 url/src/href 首命中
        let mut out2 = Vec::new();
        collect_urls(r#"[{"name":"x","href":"/h/1"}]"#, &mut out2);
        assert_eq!(out2, vec!["/h/1"]);
    }

    /// 目录字段规则：isVolume/isVip 命中、卷章节空 URL 用标题+序号、普通章节空 URL 用 base
    #[test]
    fn test_chapters_from_items_volume_vip_fallback() {
        let rule = TocRule {
            chapter_list: Some("$.data".into()),
            chapter_name: Some("$.t".into()),
            chapter_url: Some("$.u".into()),
            is_volume: Some("$.vol".into()),
            chapter_vip: Some("$.vip".into()),
            ..Default::default()
        };
        let items = vec![
            r#"{"t":"第一卷 风云","u":"","vol":"1","vip":"0"}"#.to_string(),
            r#"{"t":"第一章","u":"/c/1","vol":"0","vip":"true"}"#.to_string(),
            r#"{"t":"第二章","u":"","vol":"0","vip":"false"}"#.to_string(),
        ];
        let mut vars = crate::parser::rule::RuleVars::new();
        let chapters = chapters_from_items(&items, &rule, "https://src.test/toc", 0, &mut vars);
        assert_eq!(chapters.len(), 3);
        assert!(chapters[0].is_volume, "isVolume 规则命中应为卷");
        assert_eq!(chapters[0].url, "第一卷 风云0", "卷章节空 URL 用标题+序号");
        assert_eq!(chapters[1].title, "\u{1F512}第一章", "isVip 命中加锁前缀");
        assert_eq!(chapters[1].url, "https://src.test/c/1");
        assert!(!chapters[2].is_volume);
        assert_eq!(
            chapters[2].url, "https://src.test/toc",
            "普通章节空 URL 用 base"
        );
    }

    /// 目录去重：重复 (title,url,is_volume) 保序去重
    #[test]
    fn test_dedupe_chapters() {
        let ch = |title: &str, url: &str, vol: bool| BookChapter {
            title: title.into(),
            url: url.into(),
            tag: None,
            is_volume: vol,
            index: 0,
        };
        let input = vec![
            ch("章A", "/a", false),
            ch("章B", "/b", false),
            ch("章A", "/a", false),
            ch("卷X", "/v", true),
        ];
        let out = dedupe_chapters(input);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].title, "章A");
        assert_eq!(out[1].title, "章B");
        assert_eq!(out[2].title, "卷X");
    }

    /// `-` 前缀目录规则：JSONPath 目录倒序
    #[tokio::test]
    async fn test_analyze_toc_reverse_prefix() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let base = serve(r#"{"data":[{"t":"第一章","u":"/c/1"},{"t":"第二章","u":"/c/2"}]}"#).await;
        let mut src = test_source();
        src.book_source_url = format!("{base}/src");
        src.rule_toc = Some(serde_json::json!({
            "chapterList": "-$.data",
            "chapterName": "$.t",
            "chapterUrl": "$.u"
        }));
        let chapters = analyze_toc("default", &format!("{base}/toc"), &src, 2, None, "")
            .await
            .unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第二章", "`-` 前缀目录应倒序");
        assert_eq!(chapters[1].title, "第一章");
        assert_eq!(chapters[0].index, 0);
        assert_eq!(chapters[1].index, 1);
    }

    /// loginCheckJs 自动执行：true/空保持响应体；false 保持并继续；其他返回值重写响应体
    #[tokio::test]
    async fn test_apply_login_check_js() {
        let src = test_source();
        // 无 loginCheckJs → 原样
        assert_eq!(
            apply_login_check_js("default", &src, "<html>正文</html>", "https://a.com", None).await,
            "<html>正文</html>"
        );

        // true → 原样
        let mut src = test_source();
        src.login_check_js = Some("true".into());
        assert_eq!(
            apply_login_check_js("default", &src, "BODY", "https://a.com", None).await,
            "BODY"
        );
        // false → 原样 + 不中断
        src.login_check_js = Some("false".into());
        assert_eq!(
            apply_login_check_js("default", &src, "BODY", "https://a.com", None).await,
            "BODY"
        );
        // JS 重写响应体
        src.login_check_js = Some("result.replace('A', 'B')".into());
        assert_eq!(
            apply_login_check_js("default", &src, "AAA", "https://a.com", None).await,
            "BAA"
        );
        // JS 失败 → 原样
        src.login_check_js = Some("throw new Error('x')".into());
        assert_eq!(
            apply_login_check_js("default", &src, "BODY", "https://a.com", None).await,
            "BODY"
        );
    }

    /// 详情链路：fetch_book_info 执行 loginCheckJs 后解析（JS 重写详情页）
    #[tokio::test]
    async fn test_fetch_book_info_applies_login_check_js() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let base = serve(r#"<html><h1 class="bookname">旧名</h1></html>"#).await;
        let mut src = test_source();
        src.book_source_url = format!("{base}/src");
        src.login_check_js = Some("result.replace('旧名', '新名')".into());
        let info = fetch_book_info("default", &format!("{base}/book/1"), &src, None)
            .await
            .unwrap();
        assert_eq!(info.name, "新名", "详情链路应执行 loginCheckJs 重写");
    }

    /// concurrentRate：详情/目录/正文共用 fetch_url 入口，请求前按毫秒限速
    #[tokio::test]
    async fn test_fetch_url_applies_concurrent_rate() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let times = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i64>::new()));
        let times2 = times.clone();
        tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let times2 = times2.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    times2.lock().unwrap().push(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as i64,
                    );
                    let body = "ok";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        let mut src = test_source();
        src.concurrent_rate = Some("80".into());
        let url = format!("http://{addr}/x");
        // A2 语义：concurrentRate 限速在 acquire 准入层生效（service::search
        // test_concurrent_rate_interval_shared / _window_shared 两测已验证时序）。
        // 此处仅冒烟验证带 rate 的抓取链路正常（注：不能在此断言端到端 gap——
        // reqwest 冷启动可能超过窗口期使第二次 acquire 免等，属正确放行）。
        let _ = fetch_url("default", &url, &src).await.unwrap();
        let _ = fetch_url("default", &url, &src).await.unwrap();
        let recorded = times.lock().unwrap();
        assert_eq!(recorded.len(), 2);
    }
}
