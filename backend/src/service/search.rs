//! 搜索链路：searchUrl 构造 + 抓取 + ruleSearch 规则应用 → SearchBook
//!
//! 对齐 legacy WebBook.searchBook / BookList.analyzeBookList 语义（v1：无 JS）

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::model::BookSource;
use crate::parser::js::JsBridge;
use crate::parser::rule::{
    apply, apply_with_vars, parse_rule, push_js_context, resolve_get, RuleKind, RuleVars,
};
use crate::service::crawler;
use crate::storage::Storage;

/// 搜索结果（兼容 legacy SearchBook 字段）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBook {
    pub book_url: String,
    pub origin: String,
    pub origin_name: String,
    #[serde(rename = "type")]
    pub book_type: i64,
    pub name: String,
    pub author: String,
    pub kind: Option<String>,
    pub cover_url: Option<String>,
    pub intro: Option<String>,
    pub word_count: Option<String>,
    pub latest_chapter_title: Option<String>,
    /// 更新时间（legacy SearchBook/BookInfoRule.updateTime 契约；空规则时省略）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
    pub toc_url: String,
    pub time: i64,
    pub variable: Option<String>,
    pub origin_order: i64,
}

/// ruleSearch 结构（legacy BookListRule 字段）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRule {
    pub book_list: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub kind: Option<String>,
    pub intro: Option<String>,
    pub book_url: Option<String>,
    pub cover_url: Option<String>,
    pub word_count: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub score: Option<String>,
    pub comment: Option<String>,
    pub tags: Option<String>,
    pub serial_number: Option<String>,
    pub variable: Option<serde_json::Value>,
}

/// 构造搜索 URL（legado 语义：{{key}}/{{page}} 双花括号 + {key} 单花括号 + 相对路径拼 baseUrl）
pub fn build_search_url(search_url: &str, key: &str, page: i64, base_url: &str) -> String {
    let mut url = search_url.to_string();
    // 双花括号优先
    url = url
        .replace("{{key}}", key)
        .replace("{{page}}", &page.to_string());
    // 单花括号
    url = url
        .replace("{key}", key)
        .replace("{page}", &page.to_string());
    // <2,3,4> 页数规则：取第 page 个（超出取最后）
    if url.contains('<') && url.contains('>') {
        if let Some(start) = url.find('<') {
            if let Some(end) = url.find('>') {
                let inner = &url[start + 1..end];
                let pages: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();
                if !pages.is_empty() {
                    let idx = ((page as usize).saturating_sub(1)).min(pages.len() - 1);
                    let rep = format!("<{inner}>");
                    url = url.replace(&rep, pages[idx]);
                }
            }
        }
    }
    // 协议相对（//host/path）拼 base scheme
    if url.starts_with("//") {
        if let Ok(base) = Url::parse(base_url) {
            return format!("{}:{url}", base.scheme());
        }
    }
    // 相对路径拼 baseUrl（含无 scheme 的 `bookajax/search.do?q=1` 这类路径）
    if url.starts_with('/') && !url.starts_with("//") {
        if let Ok(base) = Url::parse(base_url) {
            if let Some(host) = base.host_str() {
                let scheme = base.scheme();
                let port = base.port().map(|p| format!(":{p}")).unwrap_or_default();
                return format!("{scheme}://{host}{port}{url}");
            }
        }
    }
    // 无 scheme 的相对 URL（相对 base 目录拼接）；data:/mailto: 等保留原样
    if !url.starts_with("http://")
        && !url.starts_with("https://")
        && !url.starts_with("data:")
        && !url.is_empty()
    {
        if let Ok(base) = Url::parse(base_url) {
            if let Ok(joined) = base.join(&url) {
                return joined.to_string();
            }
        }
    }
    url
}

/// 相对 URL → 绝对（基于 base）
pub fn to_absolute(url: &str, base: &str) -> String {
    if url.is_empty() {
        return url.to_string();
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    if url.starts_with("//") {
        if let Ok(b) = Url::parse(base) {
            return format!("{}:{url}", b.scheme());
        }
        return url.to_string();
    }
    if let Ok(joined) = Url::parse(base).and_then(|b| b.join(url)) {
        return joined.to_string();
    }
    url.to_string()
}

/// searchUrl 附加参数（`url,{...}` 后缀 JSON；v1 支持 js/bodyJs，其他键忽略）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UrlSuffix {
    /// js：执行 JS 修改 URL（注入 key/page/result（空字符串）/baseUrl/headerMap），返回值作为 URL
    pub js: Option<String>,
    /// bodyJs：对响应体执行 JS 后作为新响应体（注入 result=原响应体）
    pub body_js: Option<String>,
    /// 请求方法（POST/GET，默认 GET）
    pub method: Option<String>,
    /// POST body（支持 {{key}}/{{page}} 模板替换）
    pub body: Option<String>,
    /// 附加请求头（与书源 header 合并）
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// 响应字符集（GB2312/GBK/UTF-8 等）
    pub charset: Option<String>,
    /// A1 webView 抓取（legacy AnalyzeUrl UrlOption.webView）：true 时经浏览器渲染
    /// 取最终 HTML（JS 动态页面）；浏览器不可用/失败时回退普通 HTTP 并告警
    #[serde(rename = "webView")]
    pub web_view: Option<bool>,
    /// 请求重试次数（legacy AnalyzeUrl UrlOption.retry：URL option `{...}` 的 retry 键；
    /// None/0 → 单次请求；JSON 数字或字符串均可——legacy setRetry(String)）
    #[serde(deserialize_with = "deserialize_retry")]
    pub retry: Option<u32>,
}

/// retry 键容错解析：数字 / 字符串数字 → Some(n)；缺失/非法/0 语义交由使用方（单次请求）
fn deserialize_retry<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match v {
        Some(serde_json::Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Some(serde_json::Value::String(s)) => s.trim().parse::<u32>().ok(),
        _ => None,
    })
}

/// 切分 `url,{...}` 后缀：优先第一个 `,{` 位置（后缀 JSON 内部含逗号时——js/headers 等多键
/// 后缀——从最后逗号切会把前面的键残留进 URL 主体）；回退最后一个合法 JSON 逗号。
pub(crate) fn split_url_suffix(url: &str) -> (String, UrlSuffix) {
    // ① 第一个 `,{`：逗号后整段为合法 UrlSuffix JSON → 直接切
    if let Some(pos) = url.find(",{") {
        let rest = url[pos + 1..].trim_start();
        if let Ok(suffix) = serde_json::from_str::<UrlSuffix>(rest) {
            return (url[..pos].to_string(), suffix);
        }
    }
    // ② 回退：从最后一个「逗号后整段为合法 JSON」的位置切（URL 本身含 ,{ 且非后缀）
    let mut split: Option<(usize, UrlSuffix)> = None;
    for (i, ch) in url.char_indices() {
        if ch != ',' {
            continue;
        }
        let rest = url[i + 1..].trim_start();
        if !rest.starts_with('{') {
            continue;
        }
        if let Ok(suffix) = serde_json::from_str::<UrlSuffix>(rest) {
            split = Some((i, suffix));
        }
    }
    match split {
        Some((i, suffix)) => (url[..i].to_string(), suffix),
        None => (url.to_string(), UrlSuffix::default()),
    }
}

/// legacy AppPattern.nameRegex + BookHelp.formatBookName：
/// 剔除书名中的作者尾巴（"xxx 作者：yyy"/"xxx yyy 著"）
pub(crate) fn format_book_name(name: &str) -> String {
    static NAME_RE: std::sync::LazyLock<crate::util::regex::Regex> =
        std::sync::LazyLock::new(|| {
            crate::util::regex::RegexBuilder::new(r"\s+作\s*者.*|\s+\S+\s+著")
                .build()
                .expect("nameRegex 编译失败")
        });
    NAME_RE
        .replace_all(name, "")
        .trim_matches(|c: char| c <= ' ')
        .to_string()
}

/// legacy AppPattern.authorRegex + BookHelp.formatBookAuthor：
/// 剔除「作者：」前缀与「著」后缀
pub(crate) fn format_book_author(author: &str) -> String {
    static AUTHOR_RE: std::sync::LazyLock<crate::util::regex::Regex> =
        std::sync::LazyLock::new(|| {
            crate::util::regex::RegexBuilder::new(r"^\s*作\s*者[:：\s]+|\s+著")
                .build()
                .expect("authorRegex 编译失败")
        });
    AUTHOR_RE
        .replace_all(author, "")
        .trim_matches(|c: char| c <= ' ')
        .to_string()
}

/// legacy StringUtils.wordCountFormat：纯数字 → "N字"；>10000 → "#.#万字"
/// （一位小数、去尾零）；非数字原样；数字 ≤0 → 空
pub(crate) fn word_count_format(wc: &str) -> String {
    if !wc.is_empty() && wc.bytes().all(|b| b.is_ascii_digit()) {
        let Ok(n) = wc.parse::<i64>() else {
            return String::new();
        };
        if n > 10_000 {
            let w = n as f64 / 10_000.0;
            let s = format!("{w:.1}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
            return format!("{s}万字");
        }
        if n > 0 {
            return format!("{n}字");
        }
        return String::new();
    }
    wc.to_string()
}

/// legacy kind 多值归一：getStringList 按 [,;，；] 拆分 → joinToString(",")，
/// 空段丢弃、段内去空白
pub(crate) fn normalize_kind_list(kind: &str) -> String {
    kind.split([',', ';', '，', '；'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// JS 注入变量（key/page/baseUrl/headerMap(JSON 字符串)/result）
pub(crate) fn js_vars(
    key: &str,
    page: i64,
    base_url: &str,
    headers: &HashMap<String, String>,
    result: &str,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("key".to_string(), key.to_string());
    vars.insert("page".to_string(), page.to_string());
    vars.insert("baseUrl".to_string(), base_url.to_string());
    vars.insert(
        "headerMap".to_string(),
        serde_json::to_string(headers).unwrap_or_else(|_| "{}".to_string()),
    );
    vars.insert("result".to_string(), result.to_string());
    vars
}

/// legacy AnalyzeUrl.replaceKeyPageJs（AnalyzeUrl.kt:129-156）：
/// 先于字面替换执行 URL 中**全部** `{{js}}` 表达式（注入 key/page(数字)/baseUrl，
/// java.* 桥可用），结果回填；单个表达式求值失败保持原样（不中断 URL 构造）。
pub(crate) fn expand_url_js_templates(
    url: &str,
    key: &str,
    page: i64,
    base_url: &str,
    headers: &HashMap<String, String>,
    bridge: &JsBridge,
) -> String {
    if !url.contains("{{") {
        return url.to_string();
    }
    let vars = js_vars(key, page, base_url, headers, "");
    let numbers: Vec<(&str, i64)> = vec![("page", page)];
    let mut out = String::with_capacity(url.len());
    let mut i = 0usize;
    while let Some(rel) = url[i..].find("{{") {
        let s = i + rel;
        out.push_str(&url[i..s]);
        match url[s + 2..].find("}}") {
            Some(e_rel) => {
                let e = s + 2 + e_rel;
                let code = url[s + 2..e].trim();
                let replaced =
                    crate::parser::js::eval_js_with_bridge_num(code, &vars, bridge, &numbers)
                        .inspect_err(|_| {
                            let err = crate::parser::js::take_last_js_error()
                                .unwrap_or_else(|| "unknown".to_string());
                            eprintln!("URL {{{{js}}}} 求值失败 [{code}]: {err}");
                        })
                        .unwrap_or_else(|_| format!("{{{{{code}}}}}"));
                out.push_str(&replaced);
                i = e + 2;
            }
            None => {
                // 无闭合 → 剩余文本原样保留
                out.push_str(&url[s..]);
                return out;
            }
        }
    }
    out.push_str(&url[i.min(url.len())..]);
    out
}

/// 构造搜索请求 URL：
/// 1) `<js>…</js>` / `@js:`/`js:` → JS 返回值作为搜索 URL（注入 key/page/baseUrl/headerMap）；
/// 2) `,{...}` 后缀解析：js 键对 URL 执行 JS（注入 key/page/result 为空字符串/baseUrl/headerMap）；
/// 3) 模板替换：先执行全部 `{{js}}` 表达式（legacy replaceKeyPageJs），再 {{key}}/{key}/{{page}}/{page} 字面替换与相对路径拼接
pub(crate) fn build_request_url(
    search_url: &str,
    key: &str,
    page: i64,
    base_url: &str,
    headers: &HashMap<String, String>,
    bridge: &JsBridge,
) -> Result<(String, UrlSuffix)> {
    // 0) legacy replaceKeyPageJs：展开全部 {{js}} 表达式
    let expanded = expand_url_js_templates(search_url, key, page, base_url, headers, bridge);
    // 1) `<js>…</js>` 包裹（legado JS_PATTERN：URL 可整体为 JS 规则；`</js>` 后的
    //    `,{...}` 后缀保留待 2) 解析）
    let raw = expanded.trim_start();
    let url = if let Some((prefix, code, tail)) = wrapped_js_parts(raw) {
        let vars = js_vars(key, page, base_url, headers, "");
        let mut result = crate::parser::js::eval_js_with_bridge(&code, &vars, bridge)?;
        // 前缀/后缀通过 `@result` 拼接（legado analyzeJs 语义；无 @result 时前后文本
        // 直接拼回结果——书源通常整体为 JS，此分支兼容 `<js>…</js>,{...}` 等拼接形态）
        if !prefix.trim().is_empty() {
            result = if prefix.contains("@result") {
                prefix.replace("@result", &result)
            } else {
                format!("{prefix}{result}")
            };
        }
        if !tail.trim().is_empty() {
            result = if tail.contains("@result") {
                tail.replace("@result", &result)
            } else {
                format!("{result}{tail}")
            };
        }
        result
    } else {
        match raw.strip_prefix("@js:").or_else(|| raw.strip_prefix("js:")) {
            Some(code) => {
                let vars = js_vars(key, page, base_url, headers, "");
                crate::parser::js::eval_js_with_bridge(code.trim(), &vars, bridge)?
            }
            // 非 JS 规则 → 使用展开后的 URL（保留 0) 步的 {{js}} 展开成果）
            None => raw.to_string(),
        }
    };
    // 2) `,{...}` 后缀
    let (url_part, mut suffix) = split_url_suffix(&url);
    let url = match suffix.js.take() {
        Some(js) => {
            let vars = js_vars(key, page, base_url, headers, "");
            crate::parser::js::eval_js_with_bridge(&js, &vars, bridge)?
        }
        None => url_part,
    };
    // 3) 模板替换 + 相对路径拼接
    Ok((build_search_url(&url, key, page, base_url), suffix))
}

/// 切分 `<js>…</js>` 包裹规则：返回 (前缀, JS 代码, `</js>` 后剩余文本)
fn wrapped_js_parts(rule: &str) -> Option<(String, String, String)> {
    let r = rule.trim_start();
    let start = r.find("<js>")?;
    let code_start = start + 4;
    let rest = &r[code_start..];
    let code_end = rest.find("</js>")?;
    Some((
        r[..start].to_string(),
        rest[..code_end].to_string(),
        rest[code_end + "</js>".len()..].to_string(),
    ))
}

/// data URI 前缀检测（`data:;base64,` / `data:text/plain;base64,` 等）
pub(crate) fn is_data_uri(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("data:") && lower.contains(";base64,")
}

/// bodyJs：对响应体执行 JS 后作为新响应体（注入 result=原响应体）
fn apply_body_js(
    body: &str,
    suffix: &UrlSuffix,
    key: &str,
    page: i64,
    base_url: &str,
    headers: &HashMap<String, String>,
    bridge: &JsBridge,
) -> Result<String> {
    let Some(js) = &suffix.body_js else {
        return Ok(body.to_string());
    };
    let vars = js_vars(key, page, base_url, headers, body);
    crate::parser::js::eval_js_with_bridge(js, &vars, bridge)
}

/// 并发率（legado concurrentRate）：纯数字 = 每次请求前 sleep 该毫秒；
/// `n/window`（如 20/60000）→ 每次请求间隔 window/n 毫秒
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn concurrent_rate_sleep_ms(rate: Option<&str>) -> u64 {
    let Some(rate) = rate else { return 0 };
    let rate = rate.trim();
    if rate.is_empty() {
        return 0;
    }
    if let Ok(ms) = rate.parse::<u64>() {
        return ms;
    }
    if let Some((count, window)) = rate.split_once('/') {
        if let (Ok(c), Ok(w)) = (count.trim().parse::<u64>(), window.trim().parse::<u64>()) {
            if let Some(r) = w.checked_div(c) {
                return r;
            }
        }
    }
    0
}

/// A2 书源级共享限速（legado AnalyzeUrl concurrentRate 真实语义）：
///
/// - `n/window`（如 20/60000）= 时间窗内最多 n 次请求——**按 (ns, 源) 共享滑动窗口**，
///   并发协程排队 acquire，超窗等待最早记录过期（此前为无状态 per-call sleep，
///   N 协程同时睡完同时打请求，限速形同虚设）
/// - 纯数字（如 500）= 同源相邻两次请求最小间隔毫秒——共享「上次请求时刻」，
///   间隔不足时补足等待
///
/// 表项随进程存活（书源数有限，无需淘汰）；窗口队列仅存时间戳，内存开销可忽略。
static RATE_LIMITERS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<RateWindow>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

struct RateWindow {
    /// n/window → limit=n；纯数字间隔 → limit=1、window=interval
    limit: u64,
    window: std::time::Duration,
    hits: tokio::sync::Mutex<std::collections::VecDeque<std::time::Instant>>,
}

impl RateWindow {
    async fn acquire(&self) {
        loop {
            let wait = {
                let mut g = self.hits.lock().await;
                let now = std::time::Instant::now();
                while let Some(front) = g.front() {
                    if now.duration_since(*front) >= self.window {
                        g.pop_front();
                    } else {
                        break;
                    }
                }
                if (g.len() as u64) < self.limit {
                    g.push_back(now);
                    return; // 获得配额
                }
                // 等最早一条滑出窗口（+1ms 余量防整 spin）
                self.window - now.duration_since(*g.front().expect("非空已保证"))
                    + Duration::from_millis(1)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// 解析 concurrentRate 字符串 → 限速参数；无法解析返回 None（不限速）
fn parse_rate(rate: &str) -> Option<(u64, std::time::Duration)> {
    let rate = rate.trim();
    if rate.is_empty() {
        return None;
    }
    if let Some((c, w)) = rate.split_once('/') {
        let c: u64 = c.trim().parse().ok()?;
        let w_ms: u64 = w.trim().parse().ok()?;
        if c == 0 || w_ms == 0 {
            return None;
        }
        return Some((c, Duration::from_millis(w_ms)));
    }
    let ms: u64 = rate.parse().ok()?;
    if ms == 0 {
        return None;
    }
    Some((1, Duration::from_millis(ms)))
}

/// 请求前调用：按书源并发率配置阻塞至获得配额（无配置立即返回）
pub(crate) async fn concurrent_rate_acquire(ns: &str, source: &BookSource) {
    concurrent_rate_acquire_raw(
        ns,
        &source.book_source_url,
        source.concurrent_rate.as_deref(),
    )
    .await;
}

/// [`concurrent_rate_acquire`] 裸版（RssSource 等其他源类型复用）
pub(crate) async fn concurrent_rate_acquire_raw(ns: &str, source_url: &str, rate: Option<&str>) {
    let Some(rate) = rate else {
        return;
    };
    let Some((limit, window)) = parse_rate(rate) else {
        return;
    };
    let key = format!("{ns}\u{0}{source_url}");
    let win = {
        let mut table = RATE_LIMITERS.lock().expect("RATE_LIMITERS 中毒");
        table
            .entry(key)
            .or_insert_with(|| {
                std::sync::Arc::new(RateWindow {
                    limit,
                    window,
                    hits: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
                })
            })
            .clone()
    };
    win.acquire().await;
}

/// 执行单个书源搜索；搜索成功（命中 ≥1 条）时记录书源使用统计（use_count+1）
///
/// legacy 对齐：抓取报错时标记运行期失效快照（getInvalidBookSources 600 秒内直接返回），
/// 成功则清除该源标记。
pub async fn search_one_source(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    key: &str,
    page: i64,
    timeout_secs: u64,
) -> Result<Vec<SearchBook>> {
    // F2/EG5 配套：600s 内已标记失效的书源跳过请求
    if crate::service::health::is_source_invalid(ns, source.book_source_url.as_str()) {
        return Ok(vec![]);
    }
    // 10min 搜索结果缓存: 同关键词+页+源重搜免回源 (连载新书容忍 10min 延迟)
    let cache_key = format!("{key}\u{1}{page}\u{1}{}", source.book_source_url);
    if let Ok(Some(json)) = storage.get_search_cache(ns, &cache_key, 600_000).await {
        if let Ok(books) = serde_json::from_str::<Vec<SearchBook>>(&json) {
            return Ok(books);
        }
    }
    match search_one_source_impl(storage, ns, source, key, page, timeout_secs).await {
        Ok(v) => {
            crate::service::health::clear_source_invalid(ns, &source.book_source_url);
            if !v.is_empty() {
                if let Ok(json) = serde_json::to_string(&v) {
                    let _ = storage.put_search_cache(ns, &cache_key, &json).await;
                }
            }
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

async fn search_one_source_impl(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    key: &str,
    page: i64,
    timeout_secs: u64,
) -> Result<Vec<SearchBook>> {
    let Some(search_url) = source.search_url.clone() else {
        return Ok(vec![]);
    };
    let rule: SearchRule = match &source.rule_search {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => return Ok(vec![]),
    };
    let Some(book_list_rule) = rule.book_list.clone() else {
        return Ok(vec![]);
    };

    let headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();

    // 书源 JS 桥接（带用户命名空间：java.ajax / java.startBrowserAwait 自动携带
    // 书源 cookie；同流程内多次 eval 共享 java.put/get / setContent 文档）
    let bridge =
        JsBridge::new(&source.book_source_url, &source.book_source_name).with_namespace(ns);

    // 1) @js:/js: 前缀 + 2) `,{...}` 后缀（js 修改 URL）→ 最终请求 URL
    let (url, suffix) = build_request_url(
        &search_url,
        key,
        page,
        &source.book_source_url,
        &headers,
        &bridge,
    )?;

    // 3) 并发率：共享滑窗/最小间隔限速（A2——替代原 per-call sleep）

    // 搜索路径不限流: 跨关键词间隔等待是新词首结果 30-50s 的主因;
    // 同键重复已由 search_cache 回放覆盖, 书源防封由并发窗口(64)与超时兜底.
    // 目录/正文/详情仍走 fetch_url 内的限流.

    // 附加 headers（书源 header + 后缀 headers 合并）
    let mut req_headers = headers.clone();
    if let Some(extra) = &suffix.headers {
        for (k, v) in extra {
            req_headers.insert(k.clone(), v.clone());
        }
    }
    // 桥接同步请求头（JS 内 java.headerMap 可读/改写；eval 后 headers() 取回）
    bridge.set_headers(req_headers.clone());
    // POST body 模板替换（{{key}}/{{page}}）
    let post_body = suffix.body.as_ref().map(|b| {
        b.replace("{{key}}", key)
            .replace("{{page}}", &page.to_string())
            .replace("{key}", key)
            .replace("{page}", &page.to_string())
    });
    // 书源抓取（自动带书源 cookie——按用户命名空间）
    let method = suffix.method.as_deref().unwrap_or("GET");
    tracing::debug!(
        "搜索请求 [{}] {} {} body={}",
        source.book_source_name,
        method,
        url,
        post_body.as_deref().unwrap_or("")
    );
    // A1 webView：搜索 URL option webView=true 时经浏览器渲染（失败回退 HTTP）
    let resp = if suffix.web_view == Some(true) {
        match crate::service::browser::solve_cf_challenge(
            ns,
            &url,
            &[],
            15_000,
            source.proxy_url.as_deref(),
        )
        .await
        {
            Ok(sol) => crawler::FetchResponse {
                body: sol.html,
                url: url.clone(),
                headers: Vec::new(),
                status: 200,
            },
            Err(e) => {
                tracing::warn!("webView 搜索渲染失败，回退 HTTP [{url}]: {e}");
                if method.eq_ignore_ascii_case("POST") {
                    crawler::http_post_retry(
                        ns,
                        &url,
                        &req_headers,
                        15,
                        post_body.as_deref(),
                        suffix.charset.as_deref(),
                        source.proxy_url.as_deref(),
                        suffix.retry,
                    )
                    .await?
                } else {
                    crawler::http_get_retry(
                        ns,
                        &url,
                        &req_headers,
                        timeout_secs,
                        suffix.charset.as_deref(),
                        source.proxy_url.as_deref(),
                        suffix.retry,
                    )
                    .await?
                }
            }
        }
    } else if method.eq_ignore_ascii_case("POST") {
        crawler::http_post_retry(
            ns,
            &url,
            &req_headers,
            15,
            post_body.as_deref(),
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await?
    } else {
        crawler::http_get_retry(
            ns,
            &url,
            &req_headers,
            timeout_secs,
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await?
    };
    let base = resp.url.clone();
    // bodyJs：对响应体执行 JS 后作为新响应体
    let body = apply_body_js(
        &resp.body,
        &suffix,
        key,
        page,
        &source.book_source_url,
        &req_headers,
        &bridge,
    )?;
    // legado WebBook.searchBook：搜索响应后执行 loginCheckJs
    let body =
        crate::service::book::apply_login_check_js(ns, source, &body, &resp.url, Some(&bridge))
            .await;
    let books = analyze_book_list_impl(
        ns,
        &body,
        &base,
        source,
        &rule,
        &book_list_rule,
        key,
        &url,
        Some(&bridge),
    );

    // 书源使用统计：搜索命中（结果非空）→ use_count+1 / use_ts 刷新；
    // 计数失败仅记 debug 日志，不影响搜索流程（搜索/换源共用此入口）
    if !books.is_empty() {
        if let Err(e) = storage
            .bump_book_source_use(ns, &source.book_source_url)
            .await
        {
            tracing::debug!("书源使用计数失败 [{}]: {e}", source.book_source_name);
        }
    }

    tracing::info!(
        "搜索 [{}] key={} → {} 条",
        source.book_source_name,
        key,
        books.len()
    );
    Ok(books)
}

/// 发现页解析（无 key；无书源桥接——发现流程暂不注入 cookie 上下文）
pub(crate) fn analyze_book_list_for_explore(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    rule: &SearchRule,
    book_list_rule: &str,
) -> Vec<SearchBook> {
    analyze_book_list_impl(
        ns,
        body,
        base_url,
        source,
        rule,
        book_list_rule,
        "",
        base_url,
        None,
    )
}

/// 解析书单（对齐 legacy BookList.analyzeBookList v1：无 JS/无变量）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
fn analyze_book_list(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    rule: &SearchRule,
    book_list_rule: &str,
    _key: &str,
    bridge: &JsBridge,
) -> Vec<SearchBook> {
    analyze_book_list_impl(
        ns,
        body,
        base_url,
        source,
        rule,
        book_list_rule,
        _key,
        base_url,
        Some(bridge),
    )
}

#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
fn analyze_book_list_impl(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    rule: &SearchRule,
    book_list_rule: &str,
    _key: &str,
    request_url: &str,
    bridge: Option<&JsBridge>,
) -> Vec<SearchBook> {
    // legado BookList：响应 URL 匹配 bookUrlPattern → 按详情页规则解析为单本
    if let Some(pat) = source
        .book_url_pattern
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        let matched = crate::util::regex::Regex::new(pat)
            .map(|r| r.is_match(base_url))
            .unwrap_or(false);
        if matched {
            return single_detail_search_book(ns, body, base_url, source, request_url);
        }
    }
    // legado BookList：列表规则前缀 `-` = 结果倒序；`+` = 去掉前缀（兼容旧写法）
    let (list_rule, reverse) = strip_list_rule_prefix(book_list_rule);
    let list_rule = list_rule.as_str();
    // bookList 规则类型检测
    let parsed = parse_rule(list_rule);
    let mut items: Vec<String> = match parsed.kind {
        RuleKind::Css => css_items(list_rule, body),
        RuleKind::JsonPath => apply(list_rule, body),
        RuleKind::Regex => apply(list_rule, body),
        RuleKind::Js => js_book_list(list_rule, body, base_url, bridge),
        _ => vec![],
    };
    // JS 规则（<js> 或 @js: 开头——eval 返回 JSON 书单数组）
    if items.is_empty()
        && (list_rule.contains("<js>") || list_rule.trim_start().starts_with("@js:"))
    {
        items = js_book_list(list_rule, body, base_url, bridge);
    }
    if reverse {
        items.reverse();
    }
    // legado BookList：列表为空且书源未配置 bookUrlPattern → 按详情页规则解析单本
    if items.is_empty()
        && source
            .book_url_pattern
            .as_deref()
            .map(|p| p.trim().is_empty())
            .unwrap_or(true)
    {
        return single_detail_search_book(ns, body, base_url, source, request_url);
    }

    items
        .into_iter()
        .enumerate()
        .filter_map(|(idx, item_html)| {
            // legado：同一本书条目共用一个 AnalyzeRule——@put 跨字段存入、@get 后置字段读取
            let mut vars = RuleVars::new();
            // E10/AR5：搜索无章节上下文，仅绑定 baseUrl（JS 字段规则可用）
            vars.insert("baseUrl".to_string(), base_url.to_string());
            // E10：src = 当前条目 HTML（legacy AnalyzeRule.kt:661 bindings["src"]=content——
            // 逐条目解析时 item_html 即"当前文档"，条目内 <js>/{{js}} 的 src 可取当前元素 HTML）
            vars.insert("src".to_string(), item_html.clone());
            let mut book = SearchBook {
                origin: source.book_source_url.clone(),
                origin_name: source.book_source_name.clone(),
                origin_order: source.custom_order,
                // 非文本书源（音频/漫画等）：bookSourceType 透传到搜索结果的 type——
                // 入架后 books.book_type 即带类型，阅读器按此分派渲染
                book_type: source.book_source_type.clamp(0, 4),
                time: chrono::Utc::now().timestamp_millis(),
                ..Default::default()
            };
            // 字段规则（在每本书元素上下文中应用）
            book.name = field_with_bridge_vars(
                &item_html,
                rule.name.as_deref(),
                &book.name,
                bridge,
                &mut vars,
            );
            // legacy BookList.kt:168 formatBookName——剔除书名中的作者尾巴等脏数据
            book.name = format_book_name(&book.name);
            if book.name.is_empty() {
                return None;
            }
            // legacy BookList.kt:173 formatBookAuthor——剔除「作者：」前缀/「著」后缀
            book.author =
                field_with_bridge_vars(&item_html, rule.author.as_deref(), "", bridge, &mut vars);
            book.author = format_book_author(&book.author);
            // legacy BookList.kt:178 kind=getStringList(...).joinToString(",")——多值归一
            book.kind =
                opt_field_with_bridge_vars(&item_html, rule.kind.as_deref(), bridge, &mut vars)
                    .map(|k| normalize_kind_list(&k));
            book.intro =
                opt_field_with_bridge_vars(&item_html, rule.intro.as_deref(), bridge, &mut vars);
            book.cover_url = rule
                .cover_url
                .as_deref()
                .map(|r| field_url_with_vars(&item_html, Some(r), "", base_url, &mut vars))
                .filter(|v| !v.is_empty());
            book.word_count = opt_field_with_bridge_vars(
                &item_html,
                rule.word_count.as_deref(),
                bridge,
                &mut vars,
            )
            .map(|w| word_count_format(&w));
            book.latest_chapter_title = opt_field_with_bridge_vars(
                &item_html,
                rule.last_chapter.as_deref(),
                bridge,
                &mut vars,
            );
            book.update_time = opt_field_with_bridge_vars(
                &item_html,
                rule.update_time.as_deref(),
                bridge,
                &mut vars,
            )
            .filter(|s| !s.trim().is_empty());
            let book_url = field_url_with_vars(
                &item_html,
                rule.book_url.as_deref(),
                "",
                base_url,
                &mut vars,
            );
            if book_url.is_empty() {
                // legacy getSearchItem：bookUrl 为空时回退当前响应 URL
                book.book_url = base_url.to_string();
            } else {
                book.book_url = book_url;
            }
            // 详情页 URL 规则（bookUrlPattern 正则应匹配——v1 记录即可）
            if book.name.is_empty() {
                return None;
            }
            // 搜索阶段 tocUrl 留空（进入详情时获取）
            let _ = idx;
            book.toc_url = String::new();
            Some(book)
        })
        .collect()
}

/// 详情页单本解析结果 → SearchBook（legacy BookList.getInfoItem → Book.toSearchBook）
pub(crate) fn single_search_book(
    info: crate::model::book_chapter::BookInfo,
    source: &BookSource,
    book_url: &str,
) -> SearchBook {
    SearchBook {
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: source.book_source_name.clone(),
        origin_order: source.custom_order,
        book_type: source.book_source_type.clamp(0, 4),
        name: info.name,
        author: info.author,
        kind: info.kind,
        cover_url: info.cover_url,
        intro: info.intro,
        word_count: info.word_count,
        latest_chapter_title: info.latest_chapter_title,
        update_time: info.update_time,
        toc_url: info.toc_url.unwrap_or_default(),
        time: chrono::Utc::now().timestamp_millis(),
        variable: None,
    }
}

/// legado BookList.getInfoItem：按 ruleBookInfo 解析当前响应为单本（name 非空才返回）
fn single_detail_search_book(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    request_url: &str,
) -> Vec<SearchBook> {
    let info =
        crate::service::book::analyze_book_info(ns, body, base_url, source, request_url, None);
    if info.name.is_empty() {
        return vec![];
    }
    vec![single_search_book(info, source, request_url)]
}

/// legado 列表规则前缀（BookList/BookChapterList/RssParserByRule 共用语义）：
/// `-` → 结果倒序；`+` → 仅去前缀（旧版兼容写法）；其余原样。
pub(crate) fn strip_list_rule_prefix(rule: &str) -> (String, bool) {
    let trimmed = rule.trim_start();
    if let Some(rest) = trimmed.strip_prefix('-') {
        (rest.trim_start().to_string(), true)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (rest.trim_start().to_string(), false)
    } else {
        (rule.to_string(), false)
    }
}

/// 精确匹配文本归一化：去首尾空白 + 全角 ASCII → 半角（U+FF01..U+FF5E → U+21..U+7E，
/// 全角空格 U+3000 → 半角空格）+ 小写。用于「精确」搜索：大小写/全半角差异不敏感。
pub(crate) fn normalize_search_text(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect::<String>()
        .to_lowercase()
}

/// 单本是否精确命中：书名或作者与 key 严格等值（归一化后；作者为空不参与匹配）
pub(crate) fn exact_match(book: &SearchBook, key: &str) -> bool {
    let q = normalize_search_text(key);
    if q.is_empty() {
        return true;
    }
    let name = normalize_search_text(&book.name);
    let author = normalize_search_text(&book.author);
    name == q || (!author.is_empty() && author == q)
}

/// 精确搜索过滤：书源规则解析后，仅保留书名/作者等值命中的结果（模糊模式不做此过滤）
pub(crate) fn filter_exact(books: Vec<SearchBook>, key: &str) -> Vec<SearchBook> {
    books.into_iter().filter(|b| exact_match(b, key)).collect()
}

/// CSS 书单：链式 CSS（legado）→ 元素 html 列表
/// JS 书单规则（legado `<js>代码</js>` 或 `@js:代码`——eval 返回 JSON 数组，每项为书对象）
fn js_book_list(rule: &str, body: &str, base_url: &str, bridge: Option<&JsBridge>) -> Vec<String> {
    // 提取 JS 代码
    let code = if rule.trim_start().starts_with("@js:") {
        rule.trim_start()[4..].to_string()
    } else if let Some(start) = rule.find("<js>") {
        let rest = &rule[start + 4..];
        let end = rest.find("</js>").unwrap_or(rest.len());
        rest[..end].to_string()
    } else {
        return vec![];
    };
    // 执行（注入 result=响应体、key/page）并直接取结构化结果：
    // 数组/对象经递归 JSON 转换（js_value_to_json）——避免 boa ToString 对数组
    // 元素对象输出 "[object Object]" 导致 JSON.parse 解析为空（bookList 修复核心）
    let mut vars = std::collections::HashMap::new();
    vars.insert("result".to_string(), body.to_string());
    vars.insert("key".to_string(), String::new());
    vars.insert("page".to_string(), "1".to_string());
    vars.insert("baseUrl".to_string(), base_url.to_string());
    // 带书源桥接执行（搜索流程：java.ajax/startBrowserAwait/setContent 等可用）
    let result = match bridge {
        Some(b) => crate::parser::js::eval_js_json_with_bridge(&code, &vars, b),
        None => crate::parser::js::eval_js_json(&code, &vars),
    };
    let Ok(result) = result else {
        return vec![];
    };
    // 数组：每项书对象/字符串 → 上下文（JSON 文本）；单对象兼容
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

fn css_items(rule: &str, body: &str) -> Vec<String> {
    crate::parser::css_chain::css_chain(rule, body)
}

/// URL 型字段规则（legado isUrl 语义）：展开内嵌后若是路径/URL 直接拼接，否则走规则解析
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
fn field_url(context: &str, rule: Option<&str>, default: &str, base: &str) -> String {
    field_url_impl(context, rule, default, base, None)
}

/// [`field_url`] 带变量版本（@get 引用已存变量——搜索条目内跨字段贯通）
pub(crate) fn field_url_with_vars(
    context: &str,
    rule: Option<&str>,
    default: &str,
    base: &str,
    vars: &mut RuleVars,
) -> String {
    field_url_impl(context, rule, default, base, Some(vars))
}

fn field_url_impl(
    context: &str,
    rule: Option<&str>,
    default: &str,
    base: &str,
    mut vars: Option<&mut RuleVars>,
) -> String {
    let Some(rule) = rule else {
        return default.to_string();
    };
    let expanded = expand_embedded_impl(rule, context, vars.as_deref_mut());
    // legado makeUpRule：@get 在类型检测/URL 直判前替换
    // （URL 字段可拼 `https://x/...@get:{id}`——{{}} 内嵌已在上一步展开）
    let expanded = match vars.as_deref() {
        Some(v) => resolve_get(&expanded, v),
        None => expanded,
    };
    // URL 型：路径或完整 URL → 直接返回（相对转绝对）；// 开头是 XPath 不在此列
    if expanded.starts_with('/') && !expanded.starts_with("//") {
        return to_absolute(&expanded, base);
    }
    if expanded.starts_with("http://") || expanded.starts_with("https://") {
        return expanded;
    }
    // 规则解析（CSS/JSONPath/Regex 等）；结果为相对路径时转绝对
    // （legado isUrl：URL 字段走 getString0 仅取首个）
    let v = field_impl(context, Some(&expanded), default, None, vars, true);
    if v.starts_with('/') && !v.starts_with("//") {
        to_absolute(&v, base)
    } else {
        v
    }
}

/// 展开 {{$.xxx}} 内嵌规则（legado：{{}} 内为 JSONPath/JS，v1 支持 JSONPath）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn expand_embedded(rule: &str, context: &str) -> String {
    expand_embedded_impl(rule, context, None)
}

/// [`expand_embedded`] 带变量版本：先替换 `@get:{key}`（legado makeUpRule）
pub(crate) fn expand_embedded_with_vars(rule: &str, context: &str, vars: &RuleVars) -> String {
    expand_embedded_impl(&resolve_get(rule, vars), context, None)
}

#[allow(clippy::while_let_loop)] // 显式 loop + let-else 便于多处 break，改成 while let 反而难读
fn expand_embedded_impl(rule: &str, context: &str, mut vars: Option<&mut RuleVars>) -> String {
    if !rule.contains("{{") {
        return rule.to_string();
    }
    let mut result = rule.to_string();
    loop {
        let Some(start) = result.find("{{") else {
            break;
        };
        let Some(end_rel) = result[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + end_rel;
        let inner = &result[start + 2..end];
        let mut replacement = String::new();
        if inner.starts_with("$.") || inner.starts_with("$[") || inner.starts_with('{') {
            // JSONPath 内嵌：从上下文（可能是 JSON 对象文本）提取
            let values = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(inner, context, v),
                None => apply(inner, context),
            };
            if let Some(v) = values.first() {
                replacement = v.clone();
            }
        } else if inner.starts_with('@') || inner.starts_with("//") {
            // 规则引用（legado isRule：@ 开头 / // 开头 → 递归按规则求值）
            let values = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(inner, context, v),
                None => apply(inner, context),
            };
            replacement = values.join("\n");
        } else {
            // E10：JS 表达式（legado {{js}} 模板——注入 result=上下文 + 章节/书上下文，
            // 条目内 src=当前元素 HTML 可用；求值失败 → 空串）
            replacement = crate::parser::rule::inline_js(inner.trim(), context, vars.as_deref());
        }
        result.replace_range(start..=end + 1, &replacement);
    }
    result
}

/// 字段规则应用（上下文为单本书元素 html；无书源桥接——每次 eval 独立空 bridge）
pub(crate) fn field(context: &str, rule: Option<&str>, default: &str) -> String {
    field_impl(context, rule, default, None, None, false)
}

/// 字段规则应用（带书源桥接：搜索流程共享 ns bridge，java.* 可用）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn field_with_bridge(
    context: &str,
    rule: Option<&str>,
    default: &str,
    bridge: Option<&JsBridge>,
) -> String {
    field_impl(context, rule, default, bridge, None, false)
}

/// [`field`] 带变量版本（@put/@get 条目级贯通）
pub(crate) fn field_with_vars(
    context: &str,
    rule: Option<&str>,
    default: &str,
    vars: &mut RuleVars,
) -> String {
    field_impl(context, rule, default, None, Some(vars), false)
}

/// [`field_with_bridge`] 带变量版本
pub(crate) fn field_with_bridge_vars(
    context: &str,
    rule: Option<&str>,
    default: &str,
    bridge: Option<&JsBridge>,
    vars: &mut RuleVars,
) -> String {
    field_impl(context, rule, default, bridge, Some(vars), false)
}

fn field_impl(
    context: &str,
    rule: Option<&str>,
    default: &str,
    bridge: Option<&JsBridge>,
    mut vars: Option<&mut RuleVars>,
    is_url: bool,
) -> String {
    let Some(rule) = rule else {
        return default.to_string();
    };
    // legado 内嵌规则：{{$.xxx}} 从上下文提取替换（v1 支持 JSONPath 内嵌）
    let rule = expand_embedded_impl(rule, context, vars.as_deref_mut());
    // @js: 后缀链（legado）：`提取规则@js:code` → 先提取，结果注入 result 执行 JS
    // （如猫眼章节 URL：$.path@js:java.aesBase64DecodeToString(...)）
    if let Some((main_part, js_code)) = rule.split_once("@js:") {
        let main_part = main_part.trim();
        if !main_part.is_empty() {
            let extracted = if main_part.starts_with("$.") || main_part.starts_with('{') {
                match vars.as_deref_mut() {
                    Some(v) => apply_with_vars(main_part, context, v),
                    None => crate::parser::rule::apply(main_part, context),
                }
            } else if main_part.starts_with("//") {
                crate::parser::xpath::xpath_select(main_part, context)
            } else {
                crate::parser::css_chain::css_chain(main_part, context)
            };
            // AR3：提取段为空 → 整条后缀链终止（legado：段空结果为 null，
            // getString 循环 result?.let 跳过后续 JS，最终返回空串而非以空串续喂）
            let first = match extracted.into_iter().find(|s| !s.is_empty()) {
                Some(s) => s,
                None => return default.to_string(),
            };
            let mut js_env = std::collections::HashMap::new();
            js_env.insert("result".to_string(), first);
            // E10：@js: 后缀链同样绑定章节/书上下文（baseUrl/src——legacy evalJS 全量注入）
            push_js_context(&mut js_env, vars.as_deref());
            let s = match bridge {
                Some(b) => crate::parser::js::eval_js_with_bridge(js_code.trim(), &js_env, b),
                None => crate::parser::js::eval_js(js_code.trim(), &js_env),
            };
            if let Ok(s) = s {
                if !s.is_empty() {
                    return s;
                }
            }
            return default.to_string();
        }
    }
    let r = parse_rule(&rule);
    match r.kind {
        RuleKind::Css => {
            // 链式 CSS（legado：class./tag./@text/@href 等）；经 apply 执行以保留
            // ##替换链 / <js> 链（apply_post：替换正则/替换串/### 首个匹配）
            let v = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(&rule, context, v),
                None => crate::parser::rule::apply(&rule, context),
            };
            if is_url {
                // legado getString0（isUrl=true）：URL 字段仍仅取首个结果（AR2 多命中
                // 连接不适用于 URL 字段）
                if let Some(first) = v.first() {
                    // 无 @ 的单选择器规则：元素 HTML → 取文本（兼容旧书源写法）
                    if !r.body.contains('@') {
                        // jsoup text() 语义：跳过 script/style 子树
                        let txt = visible_text(first);
                        if !txt.is_empty() {
                            return txt;
                        }
                    }
                    return first.clone();
                }
                return default.to_string();
            }
            // AR2：legacy AnalyzeByJSoup.getString 为全量语义——所有非空结果以 "\n"
            // 连接（getStringList(...).joinToString("\n")），非仅首个命中
            let items: Vec<String> = v.into_iter().filter(|s| !s.is_empty()).collect();
            if items.is_empty() {
                return default.to_string();
            }
            // 无 @ 的单选择器规则：元素 HTML → 取文本（兼容旧书源写法）
            if !r.body.contains('@') {
                // jsoup text() 语义：跳过 script/style 子树
                let texts: Vec<String> = items
                    .iter()
                    .map(|s| visible_text(s))
                    .filter(|t| !t.is_empty())
                    .collect();
                if !texts.is_empty() {
                    return texts.join("\n");
                }
            }
            items.join("\n")
        }
        RuleKind::JsonPath => {
            let v = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(&rule, context, v),
                None => apply(&rule, context),
            };
            // AR2b：与 CSS 分支同为 legacy getString 全量语义——多值（JSON 数组
            // 展开的正文段落等）以 "\n" 连接，非仅首个命中
            let items: Vec<String> = v.into_iter().filter(|s| !s.is_empty()).collect();
            if items.is_empty() {
                return default.to_string();
            }
            items.join("\n")
        }
        RuleKind::Regex => {
            let v = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(&rule, context, v),
                None => apply(&rule, context),
            };
            v.into_iter().next().unwrap_or_else(|| default.to_string())
        }
        RuleKind::Js => {
            // 纯 JS 字段规则（@js:/js: 前缀——parse_rule 已剥前缀，body 即代码）：
            // 注入 result=上下文执行；数组/对象结果自动 JSON 化（js_result_to_string）
            let mut js_env = std::collections::HashMap::new();
            js_env.insert("result".to_string(), context.to_string());
            js_env.insert("key".to_string(), String::new());
            js_env.insert("page".to_string(), "1".to_string());
            // E10：字段级 JS 同样绑定章节/书上下文（baseUrl/src——legacy evalJS 全量注入）
            push_js_context(&mut js_env, vars.as_deref());
            let s = match bridge {
                Some(b) => crate::parser::js::eval_js_with_bridge(&r.body, &js_env, b),
                None => crate::parser::js::eval_js(&r.body, &js_env),
            }
            .unwrap_or_default();
            if s.is_empty() {
                default.to_string()
            } else {
                s
            }
        }
        _ => default.to_string(),
    }
}

/// 元素 HTML 的可见文本（jsoup text()：跳过 script/style 子树 + trim）
fn visible_text(html: &str) -> String {
    let doc = scraper::Html::parse_fragment(html);
    collect_visible_text(doc.root_element())
}

fn collect_visible_text(el: scraper::ElementRef<'_>) -> String {
    if el.value().name() == "script" || el.value().name() == "style" {
        return String::new();
    }
    let mut s = String::new();
    for child in el.children() {
        match child.value() {
            scraper::node::Node::Text(txt) => s.push_str(&txt.text),
            scraper::node::Node::Element(_) => {
                if let Some(e) = scraper::ElementRef::wrap(child) {
                    s.push_str(&collect_visible_text(e));
                }
            }
            _ => {}
        }
    }
    s.trim().to_string()
}

#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn opt_field(context: &str, rule: Option<&str>) -> Option<String> {
    opt_field_with_bridge(context, rule, None)
}

#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn opt_field_with_bridge(
    context: &str,
    rule: Option<&str>,
    bridge: Option<&JsBridge>,
) -> Option<String> {
    let v = field_with_bridge(context, rule, "", bridge);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// [`opt_field`] 带变量版本
pub(crate) fn opt_field_with_vars(
    context: &str,
    rule: Option<&str>,
    vars: &mut RuleVars,
) -> Option<String> {
    let v = field_with_vars(context, rule, "", vars);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// [`opt_field_with_bridge`] 带变量版本
pub(crate) fn opt_field_with_bridge_vars(
    context: &str,
    rule: Option<&str>,
    bridge: Option<&JsBridge>,
    vars: &mut RuleVars,
) -> Option<String> {
    let v = field_with_bridge_vars(context, rule, "", bridge, vars);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// E8：字段清洗四件套（legacy formatBookName/Author/wordCountFormat/kind 归一）
    #[test]
    fn test_field_cleaning_helpers() {
        // nameRegex：剔除作者尾巴
        assert_eq!(format_book_name("诡秘之主 作者：爱潜水的乌贼"), "诡秘之主");
        assert_eq!(format_book_name("某书 乌贼 著"), "某书");
        assert_eq!(format_book_name("  正常书名  "), "正常书名");
        assert_eq!(format_book_name(""), "");
        // authorRegex：「作者：」前缀 / 「著」后缀
        assert_eq!(format_book_author("作者：乌贼"), "乌贼");
        assert_eq!(format_book_author("作 者 : 乌贼"), "乌贼");
        assert_eq!(format_book_author("乌贼 著"), "乌贼");
        assert_eq!(format_book_author("普通作者"), "普通作者");
        // wordCountFormat：纯数字 → N字 / 万字；非数字原样；0 → 空
        assert_eq!(word_count_format("123"), "123字");
        assert_eq!(word_count_format("150000"), "15万字");
        assert_eq!(word_count_format("1234567"), "123.5万字");
        assert_eq!(word_count_format("20000"), "2万字", "尾零去除");
        assert_eq!(word_count_format("0"), "", "0 → 空");
        assert_eq!(word_count_format("约三万"), "约三万");
        assert_eq!(word_count_format(""), "");
        // kind 多值归一：分隔符统一为半角逗号、去空段
        assert_eq!(normalize_kind_list("玄幻, 冒险；科幻"), "玄幻,冒险,科幻");
        assert_eq!(normalize_kind_list("a;;b，，c"), "a,b,c");
        assert_eq!(normalize_kind_list("单值"), "单值");
        assert_eq!(normalize_kind_list(" , , "), "");
    }

    /// E1/E2：URL 模板 {{js}} 展开（legacy replaceKeyPageJs）+ page 数值语义
    #[test]
    fn test_expand_url_js_templates() {
        let bridge = crate::parser::js::JsBridge::default();
        let headers = HashMap::new();
        // 数值算术：page+1 = 2（非 "11"）
        assert_eq!(
            expand_url_js_templates(
                "https://a.com/x?p={{page+1}}",
                "k",
                1,
                "https://a.com",
                &headers,
                &bridge
            ),
            "https://a.com/x?p=2"
        );
        // 标准 JS 内建可用（encodeURI）
        assert_eq!(
            expand_url_js_templates(
                "https://a.com/s?q={{encodeURI(key)}}&p={{page}}",
                "abc def",
                3,
                "https://a.com",
                &headers,
                &bridge
            ),
            "https://a.com/s?q=abc%20def&p=3"
        );
        // java.* 桥在模板内可用
        assert_eq!(
            expand_url_js_templates(
                "https://a.com/t?m={{java.md5Encode16('abc')}}",
                "k",
                1,
                "https://a.com",
                &headers,
                &bridge
            ),
            "https://a.com/t?m=3cd24fb0d6963f7d"
        );
        // 求值失败 → 原样保留（安全回退）
        let bad = "https://a.com/x?z={{noSuchFnZZZ()}}";
        assert_eq!(
            expand_url_js_templates(bad, "k", 1, "https://a.com", &headers, &bridge),
            bad
        );
        // 无闭合 {{ → 剩余文本原样保留
        let open = "https://a.com/x?q={{abc";
        assert_eq!(
            expand_url_js_templates(open, "k", 1, "https://a.com", &headers, &bridge),
            open
        );
        // 多个表达式混合展开
        assert_eq!(
            expand_url_js_templates(
                "https://a.com/{{key}}/p{{page+2}}/s{{page*3}}",
                "书名",
                1,
                "https://a.com",
                &headers,
                &bridge
            ),
            "https://a.com/书名/p3/s3"
        );
    }

    /// build_request_url 集成：{{js}} 先于字面替换执行
    #[test]
    fn test_build_request_url_expands_js_template() {
        let bridge = crate::parser::js::JsBridge::default();
        let headers = HashMap::new();
        let (url, _suffix) = build_request_url(
            "https://a.com/search?q={{key}}&p={{page+1}}",
            "斗罗",
            4,
            "https://a.com",
            &headers,
            &bridge,
        )
        .unwrap();
        assert_eq!(url, "https://a.com/search?q=斗罗&p=5");
    }

    #[test]
    fn test_field_url_with_vars_resolves_get() {
        let mut vars = RuleVars::new();
        vars.insert("bid".to_string(), "abc".to_string());
        assert_eq!(
            resolve_get("https://x.test/c/@get:{bid}/x", &vars),
            "https://x.test/c/abc/x"
        );
        let out = field_url_with_vars(
            r#"{"u":"/c/1"}"#,
            Some("https://x.test/c/@get:{bid}{{$.u}}"),
            "",
            "http://base",
            &mut vars,
        );
        assert_eq!(
            out, "https://x.test/c/abc/c/1",
            "URL 字段应替换 @get 与 {{}}"
        );
    }

    #[test]
    fn test_build_url_double_brace() {
        let u = build_search_url(
            "/novel/search?q={{key}}&p={{page}}",
            "诡秘",
            2,
            "https://a.com",
        );
        assert_eq!(u, "https://a.com/novel/search?q=诡秘&p=2");
    }

    #[test]
    fn test_build_url_single_brace() {
        let u = build_search_url("https://a.com/s?k={key}", "测试", 1, "https://a.com");
        assert_eq!(u, "https://a.com/s?k=测试");
    }

    #[test]
    fn test_build_url_page_picker() {
        let u = build_search_url("https://a.com/<1,2,3>", "x", 2, "https://a.com");
        assert_eq!(u, "https://a.com/2");
    }

    #[test]
    fn test_absolute() {
        assert_eq!(to_absolute("/b/1", "https://a.com"), "https://a.com/b/1");
        assert_eq!(
            to_absolute("https://x.com/b", "https://a.com"),
            "https://x.com/b"
        );
    }

    #[test]
    fn test_analyze_real_json_list() {
        // 真实猫眼 JSON（15 条）+ 真实规则
        let body = match std::fs::read_to_string("target/cat-eye.json") {
            Ok(b) => b,
            Err(_) => return, // 无测试数据时跳过
        };
        let rule: SearchRule = serde_json::from_value(serde_json::json!({
            "bookList": "$.data[*]", "name": "$.novelName", "author": "$.authorName",
            "intro": "$.summary", "bookUrl": "/novel/{{$.novelId}}?isSearch=1",
            "coverUrl": "$.cover", "wordCount": "$.wordNum"
        }))
        .unwrap();
        // 中间环节：bookList 提取
        let items = crate::parser::rule::apply("$.data[*]", &body);
        println!("bookList items: {}", items.len());
        if let Some(first) = items.first() {
            println!(
                "首项前 100: {}",
                first.chars().take(100).collect::<String>()
            );
        }
        // 直接测字段规则
        let name = field(&items[0], Some("$.novelName"), "");
        println!("field('$.novelName') = {:?}", name);
        let book_url = field(&items[0], Some("/novel/{{$.novelId}}?isSearch=1"), "");
        println!("field(bookUrl 内嵌) = {:?}", book_url);
        let src = BookSource {
            book_source_url: "http://api.jmlldsc.com".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            &body,
            "http://api.jmlldsc.com",
            &src,
            &rule,
            "$.data[*]",
            "诡秘之主",
            &JsBridge::default(),
        );
        println!("真实 JSON 解析: {} 本", books.len());
        assert!(!books.is_empty(), "真实数据解析为空");
        assert_eq!(books[0].name, "诡秘之主");
        assert!(
            books[0].book_url.contains("bY7oM0"),
            "bookUrl 内嵌规则: {}",
            books[0].book_url
        );
    }

    #[test]
    fn test_analyze_empty_list_falls_back_to_detail() {
        let mut src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        src.rule_search = Some(serde_json::json!({
            "bookList": "div.none",
            "name": "h1@text"
        }));
        src.rule_book_info = Some(serde_json::json!({
            "name": "h1@text",
            "author": "p@text",
            "tocUrl": "/toc"
        }));
        let rule: SearchRule = serde_json::from_value(src.rule_search.clone().unwrap()).unwrap();
        let html = r#"<h1>书名</h1><p>作者</p>"#;
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com/book/1",
            &src,
            &rule,
            "div.none",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 1, "列表为空应按详情页解析单本");
        assert_eq!(books[0].name, "书名");
        assert_eq!(books[0].author, "作者");
        assert_eq!(books[0].toc_url, "https://a.com/toc");
        assert_eq!(books[0].book_url, "https://a.com/book/1");
    }

    /// legado BookList：响应 URL 匹配 bookUrlPattern → 直接按详情页解析单本
    #[test]
    fn test_analyze_single_detail_by_url_pattern() {
        let mut src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        src.book_url_pattern = Some(r"^https://a\.com/book/\d+$".into());
        src.rule_book_info = Some(serde_json::json!({
            "name": "h1@text",
            "author": "p@text",
            "coverUrl": "img@src",
            "tocUrl": "/toc"
        }));
        let rule: SearchRule = SearchRule::default();
        let html = r#"<h1>书名</h1><p>作者</p><img src="/cover.jpg">"#;
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com/book/42",
            &src,
            &rule,
            "div.none",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 1, "bookUrlPattern 命中应按详情页解析");
        assert_eq!(books[0].name, "书名");
        assert_eq!(books[0].author, "作者");
        assert_eq!(books[0].book_url, "https://a.com/book/42");
        assert_eq!(books[0].toc_url, "https://a.com/toc");
        assert_eq!(
            books[0].cover_url.as_deref(),
            Some("https://a.com/cover.jpg")
        );
    }

    /// legacy getSearchItem：bookUrl 规则结果为空时回退 baseUrl，而不是丢弃条目
    #[test]
    fn test_analyze_item_empty_book_url_falls_back_base() {
        let mut src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        src.rule_search = Some(serde_json::json!({
            "bookList": "div.book",
            "name": "h2@text",
            "author": "p@text"
        }));
        let rule: SearchRule = serde_json::from_value(src.rule_search.clone().unwrap()).unwrap();
        let html = r#"<div class="book"><h2>书名</h2><p>作者</p></div>"#;
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com/list",
            &src,
            &rule,
            "div.book",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].book_url, "https://a.com/list");
    }

    /// E10：搜索条目内 src = 当前条目 HTML（legacy AnalyzeRule.kt:661
    /// bindings["src"]=content——逐条目解析时 item_html 即"当前文档"）：
    /// 条目字段的 <js> 链与 {{js}} 内嵌均可取到当前书元素 HTML（逐条目不同）
    #[test]
    fn test_analyze_item_src_binding() {
        let html =
            r#"<div class="book"><h2>书名甲</h2></div><div class="book"><h2>书名乙</h2></div>"#;
        let rule = SearchRule {
            book_list: Some("div.book".into()),
            // <js> 链：src=当前条目 HTML → 长度后缀随条目变化
            name: Some("h2@text<js>result + '#' + src.length</js>".into()),
            // {{js}}：URL 模板内嵌——与 <js> 链同源（同一 vars 的 src）
            book_url: Some("/d{{src.length}}".into()),
            ..Default::default()
        };
        let src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com/list",
            &src,
            &rule,
            "div.book",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 2);
        let n0: usize = books[0].name.rsplit('#').next().unwrap().parse().unwrap();
        let n1: usize = books[1].name.rsplit('#').next().unwrap().parse().unwrap();
        assert!(
            books[0].name.starts_with("书名甲#") && n0 > 0,
            "<js> 应能取到条目 src: {}",
            books[0].name
        );
        assert_ne!(
            books[0].name, books[1].name,
            "不同条目的 src 应为各自元素 HTML"
        );
        // {{js}} 内嵌展开出 URL（/d{长度} → 转绝对），且与 <js> 链取到的同一 src 一致
        assert_eq!(books[0].book_url, format!("https://a.com/d{n0}"));
        assert_eq!(books[1].book_url, format!("https://a.com/d{n1}"));
    }

    /// SearchRule.updateTime → SearchBook.updateTime 透传（legacy SearchBook 契约）
    #[test]
    fn test_analyze_list_update_time() {
        let mut src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        src.rule_search = Some(serde_json::json!({
            "bookList": "div.book",
            "name": "h2@text",
            "bookUrl": "a@href",
            "updateTime": "span.time@text"
        }));
        let rule: SearchRule = serde_json::from_value(src.rule_search.clone().unwrap()).unwrap();
        let html = r#"<div class="book"><h2>书名</h2><a href="/b/1">详情</a><span class="time">2026-08-08</span></div>"#;
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com/list",
            &src,
            &rule,
            "div.book",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].update_time.as_deref(), Some("2026-08-08"));
    }

    #[test]
    fn test_normalize_search_text() {
        // 全角 ASCII → 半角 + 小写 + 去首尾空白
        assert_eq!(normalize_search_text(" ＡＢＣ１２３ "), "abc123");
        assert_eq!(normalize_search_text("ＦｕｌｌＷｉｄｔｈ"), "fullwidth");
        assert_eq!(normalize_search_text("AbC"), "abc");
        // 全角空格 U+3000 → 半角空格；首尾空白（含全角）被 trim 去除
        assert_eq!(normalize_search_text("　全角　空格　"), "全角 空格");
        // 中文与混合内容原样保留（小写化对中文无影响）
        assert_eq!(normalize_search_text("诡秘之主"), "诡秘之主");
    }

    #[test]
    fn test_filter_exact() {
        let mk = |name: &str, author: &str| SearchBook {
            name: name.into(),
            author: author.into(),
            ..Default::default()
        };
        let books = vec![
            mk("诡秘之主", "爱潜水的乌贼"),
            mk("诡秘之主2", "爱潜水的乌贼"),
            mk("ＧＭＴ", "测试作者"),
            mk("凡人修仙传", "忘语"),
            mk("Book ABC", ""),
        ];
        // 书名等值命中（大小写不敏感）
        let hit = filter_exact(books.clone(), "诡秘之主");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "诡秘之主");
        // 包含但不等值（模糊命中）→ 精确下不命中
        let hit = filter_exact(books.clone(), "诡秘");
        assert!(hit.is_empty());
        // 全角书名 + 半角 key：全半角忽略等值命中
        let hit = filter_exact(books.clone(), "gmt");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "ＧＭＴ");
        // 半角书名 + 全角 key
        let hit = filter_exact(books.clone(), "ＢＯＯＫ ａｂｃ");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "Book ABC");
        // 作者等值命中
        let hit = filter_exact(books.clone(), "忘语");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "凡人修仙传");
        // 作者为空：不因空作者误命中；空 key 不过滤
        let hit = filter_exact(books.clone(), "");
        assert_eq!(hit.len(), books.len());
        // 无命中
        let hit = filter_exact(books, "不存在的书");
        assert!(hit.is_empty());
    }

    #[test]
    fn test_analyze_html_list() {
        let html = r#"<div class="book"><h2>书名A</h2><p>作者甲</p><a href="/book/1">详情</a></div>
                       <div class="book"><h2>书名B</h2><p>作者乙</p><a href="/book/2">详情</a></div>"#;
        let rule = SearchRule {
            book_list: Some("div.book".into()),
            name: Some("h2".into()),
            author: Some("p".into()),
            book_url: Some("a@href".into()),
            ..Default::default()
        };
        let src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com",
            &src,
            &rule,
            "div.book",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].name, "书名A");
        assert_eq!(books[0].author, "作者甲");
        assert_eq!(books[0].book_url, "https://a.com/book/1");
    }

    /// 字段规则 ## 替换链（真实书源：class.title@tag.h2@text##《(.*)》##$1### 形态）：
    /// 替换须在 field 层生效（此前 Css 分支绕过 apply_post 丢失替换）
    #[test]
    fn test_field_css_replace_chain() {
        let html = r#"<div class="book"><h2 class="t">《测试书》</h2><p>作者甲</p></div>"#;
        // 三段替换（去书名号）
        let name = field(html, Some("class.t@text##《(.*)》##$1"), "");
        assert_eq!(name, "测试书");
        // 字段上下文为单本书元素：链式提取 + 替换
        let el = crate::parser::css_chain::css_chain("div.book", html);
        let name2 = field(&el[0], Some("class.t@text##《(.*)》##$1"), "");
        assert_eq!(name2, "测试书");
        // @js: 链字段（提取结果进 JS result 变量）
        let name3 = field(
            &el[0],
            Some("class.t@text@js:result.replace('《','【').replace('》','】')"),
            "",
        );
        assert_eq!(name3, "【测试书】");
        // 纯替换规则（### replaceFirst）——上下文仅含书名
        let h2 = crate::parser::css_chain::css_chain("class.t", html);
        let name4 = field(&h2[0], Some("##《(.*)》##[$1]###"), "");
        assert_eq!(name4, "[测试书]");
    }

    /// AR2：字段 Css 规则多命中 → 全量非空结果以 "\n" 连接（legacy
    /// AnalyzeByJSoup.getString 的 joinToString("\n") 语义），非仅首个；
    /// URL 字段不受影响——仍走 getString0 仅取首个
    #[test]
    fn test_field_css_multi_hit_joined_and_url_first() {
        let html = r#"<div><h2 class="t">书名甲</h2><h2 class="t">书名乙</h2></div>"#;
        // 文本字段：多命中全量连接
        assert_eq!(field(html, Some("class.t@text"), ""), "书名甲\n书名乙");
        // 空命中回退 default 不变
        assert_eq!(field(html, Some("class.missing@text"), "默认"), "默认");
        // URL 字段：多命中仍仅取首个（isUrl → getString0），并转绝对地址
        let urls = r#"<div><a class="l" href="/1">a</a><a class="l" href="/2">b</a></div>"#;
        let mut vars = RuleVars::new();
        assert_eq!(
            field_url_with_vars(urls, Some("class.l@href"), "", "http://base", &mut vars),
            "http://base/1"
        );
    }

    /// AR3：后缀链提取段为空 → 整链终止返回空（legacy 段空结果为 null，
    /// 后续 JS 跳过；此前以空串续喂 JS 得 "0"）
    #[test]
    fn test_field_empty_extract_terminates_js_suffix() {
        let html = r#"<div><p>正文</p></div>"#;
        // class.missing 无命中 → JS 不执行 → 空串（非 "0"）
        assert_eq!(
            field(html, Some("class.missing@text@js:result.length"), ""),
            ""
        );
        // 命中时链照常工作
        assert_eq!(field(html, Some("tag.p@text@js:result + '!'"), ""), "正文!");
    }

    /// bookList 修复：JS 返回 JSON.parse(result).data 数组 → 逐本书解析（此前 ToString
    /// 输出 "[object Object]" 导致解析为空）
    #[test]
    fn test_js_book_list_json_parse_array() {
        let body = r#"{"code":0,"data":[
            {"novelName":"书A","authorName":"作者甲","novelId":"id1"},
            {"novelName":"书B","authorName":"作者乙","novelId":"id2"}
        ]}"#;
        let rule = SearchRule {
            book_list: Some("@js:JSON.parse(result).data".into()),
            name: Some("$.novelName".into()),
            author: Some("$.authorName".into()),
            book_url: Some("/novel/{{$.novelId}}".into()),
            ..Default::default()
        };
        let src = BookSource {
            book_source_url: "https://api.test".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            body,
            "https://api.test",
            &src,
            &rule,
            "@js:JSON.parse(result).data",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 2, "JS bookList 应解析出 2 本书");
        assert_eq!(books[0].name, "书A");
        assert_eq!(books[0].author, "作者甲");
        assert_eq!(books[0].book_url, "https://api.test/novel/id1");
        assert_eq!(books[1].name, "书B");
    }

    /// bookList 修复：JS 直接返回数组字面量（非字符串出口）
    #[test]
    fn test_js_book_list_array_literal() {
        let rule = SearchRule {
            book_list: Some("@js:[{name:'直A',url:'/a'},{name:'直B',url:'/b'}]".into()),
            name: Some("$.name".into()),
            book_url: Some("$.url".into()),
            ..Default::default()
        };
        let src = BookSource {
            book_source_url: "https://api.test".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            "{}",
            "https://api.test",
            &src,
            &rule,
            "@js:[{name:'直A',url:'/a'},{name:'直B',url:'/b'}]",
            "",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].name, "直A");
        assert_eq!(books[0].book_url, "https://api.test/a");
    }

    /// bookList 兼容：JS 内 JSON.stringify 返回字符串数组——自动解析
    #[test]
    fn test_js_book_list_stringify_backward_compat() {
        let rule = SearchRule {
            book_list: Some("@js:JSON.stringify([{name:'串A',url:'/s/a'}])".into()),
            name: Some("$.name".into()),
            book_url: Some("$.url".into()),
            ..Default::default()
        };
        let src = BookSource {
            book_source_url: "https://api.test".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            "{}",
            "https://api.test",
            &src,
            &rule,
            "@js:JSON.stringify([{name:'串A',url:'/s/a'}])",
            "",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].name, "串A");
    }

    /// bookList：<js> 包裹形式
    #[test]
    fn test_js_book_list_html_wrapped() {
        let rule = SearchRule {
            book_list: Some("<js>JSON.parse(result).data</js>".into()),
            name: Some("$.name".into()),
            book_url: Some("$.url".into()),
            ..Default::default()
        };
        let body = r#"{"data":[{"name":"包A","url":"/w/1"}]}"#;
        let src = BookSource {
            book_source_url: "https://api.test".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            body,
            "https://api.test",
            &src,
            &rule,
            "<js>JSON.parse(result).data</js>",
            "",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].name, "包A");
    }

    /// 字段规则：@js: 前缀（analyze 系列字段出口——result 注入上下文执行）
    #[test]
    fn test_field_js_rule() {
        let ctx = r#"{"data":{"name":"字段书","author":"字段作者"}}"#;
        assert_eq!(
            field(ctx, Some("@js:JSON.parse(result).data.name"), ""),
            "字段书"
        );
        assert_eq!(
            field(ctx, Some("@js:JSON.parse(result).data.author"), ""),
            "字段作者"
        );
        // 失败回退默认值
        assert_eq!(
            field(ctx, Some("@js:JSON.parse(result).data.missing.x"), "默认"),
            "默认"
        );
    }

    #[test]
    fn test_js_search_url_prefix() {
        // @js: 前缀：JS 返回值作为搜索 URL（注入 key/page/baseUrl）
        let headers = HashMap::new();
        let (url, suffix) = build_request_url(
            r#"@js:baseUrl + "/s?q=" + key + "&p=" + page"#,
            "测试书",
            2,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(url, "https://a.com/s?q=测试书&p=2");
        assert!(suffix.js.is_none() && suffix.body_js.is_none());
    }

    #[test]
    fn test_js_search_url_wrapped_tag() {
        // `<js>…</js>` 整体包裹：JS 返回值作为搜索 URL（legado JS_PATTERN）
        let headers = HashMap::new();
        let (url, suffix) = build_request_url(
            r#"<js>baseUrl + "/s?q=" + key + "&p=" + page</js>"#,
            "测试书",
            2,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(url, "https://a.com/s?q=测试书&p=2");
        assert!(suffix.js.is_none() && suffix.body_js.is_none());
    }

    #[test]
    fn test_js_search_url_wrapped_tag_with_suffix() {
        // `<js>` 后的 `,{...}` 后缀保留并解析（charset/headers 等）
        let headers = HashMap::new();
        let (url, suffix) = build_request_url(
            r#"<js>baseUrl + "/search?key=" + key</js>,{"charset":"GBK","headers":{"X-A":"1"}}"#,
            "测试",
            1,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(url, "https://a.com/search?key=测试");
        assert_eq!(suffix.charset.as_deref(), Some("GBK"));
        assert_eq!(
            suffix
                .headers
                .as_ref()
                .and_then(|h| h.get("X-A"))
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn test_build_search_url_relative_path_without_slash() {
        // `bookajax/search.do?keyword=...` 这类无 scheme 相对 URL → base 目录拼接
        let u = build_search_url(
            "bookajax/search.do?keyword=文明乐园",
            "文明乐园",
            1,
            "https://a.com",
        );
        assert!(
            u.starts_with("https://a.com/bookajax/search.do?keyword="),
            "相对路径应拼到 base 目录: {u}"
        );
    }

    #[test]
    fn test_build_search_url_protocol_relative() {
        let u = build_search_url("//cdn.example.com/s?q={{key}}", "k", 1, "https://a.com");
        assert_eq!(u, "https://cdn.example.com/s?q=k");
    }

    #[test]
    fn test_data_uri_detection() {
        assert!(is_data_uri("data:;base64,eyJhIjoxfQ=="));
        assert!(is_data_uri("data:text/plain;base64,QQ=="));
        assert!(!is_data_uri("https://a.com/s"));
        assert!(!is_data_uri("data:text/plain,hello"));
    }

    #[test]
    fn test_js_search_url_header_map_json() {
        // headerMap 以 JSON 字符串注入，JS 可读取
        let headers = HashMap::from([("User-Agent".to_string(), "UA1".to_string())]);
        let (url, _) = build_request_url(
            r#"@js:baseUrl + "/s?h=" + (JSON.parse(headerMap)["User-Agent"] ? "yes" : "no")"#,
            "k",
            1,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(url, "https://a.com/s?h=yes");
    }

    #[test]
    fn test_url_suffix_js_modifies_url() {
        // `,{"js":...}` 后缀：JS 修改 URL（注入 key/page/result 为空字符串/baseUrl）
        let headers = HashMap::new();
        let (url, suffix) = build_request_url(
            r#"https://a.com/search?q={{key}},{"js":"baseUrl + '/mod?k=' + key + '&p=' + page"}"#,
            "测试",
            1,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(url, "https://a.com/mod?k=测试&p=1");
        // js 键已消费：不再出现在后缀中，bodyJs 保留为空
        assert!(suffix.js.is_none());
    }

    #[test]
    fn test_url_suffix_body_js_rewrites_body() {
        // bodyJs：对响应体执行 JS 后作为新响应体（注入 result=原响应体）
        let suffix: UrlSuffix =
            serde_json::from_str(r#"{"bodyJs":"result.replace('A','B')"}"#).unwrap();
        let headers = HashMap::new();
        let body = apply_body_js(
            "AAA",
            &suffix,
            "k",
            1,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(body, "BAA");
    }

    #[test]
    fn test_url_suffix_parse_ignores_unknown_keys() {
        // 其他键（method 等）忽略；js/bodyJs 同时解析
        let (url, suffix) = split_url_suffix(
            r#"https://a.com/s,{"js":"baseUrl","method":"POST","bodyJs":"result + '!'"}"#,
        );
        assert_eq!(url, "https://a.com/s");
        assert_eq!(suffix.js.as_deref(), Some("baseUrl"));
        assert_eq!(suffix.body_js.as_deref(), Some("result + '!'"));
    }

    #[test]
    fn test_url_without_suffix_unchanged() {
        let headers = HashMap::new();
        let (url, suffix) = build_request_url(
            "https://a.com/s?q={{key}}",
            "k",
            1,
            "https://a.com",
            &headers,
            &JsBridge::default(),
        )
        .unwrap();
        assert_eq!(url, "https://a.com/s?q=k");
        assert!(suffix.js.is_none() && suffix.body_js.is_none());
    }

    #[test]
    fn test_concurrent_rate_sleep_ms() {
        assert_eq!(concurrent_rate_sleep_ms(Some("1000")), 1000);
        assert_eq!(concurrent_rate_sleep_ms(Some("20/60000")), 3000);
        assert_eq!(concurrent_rate_sleep_ms(Some(" 500 ")), 500);
        assert_eq!(concurrent_rate_sleep_ms(Some("abc")), 0);
        assert_eq!(concurrent_rate_sleep_ms(None), 0);
    }

    /// A2：共享滑窗——10 协程抢 3/300ms 窗口，总耗时 ≥ (10/3-1)*300ms 且窗口内不超发
    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_rate_window_shared() {
        let src = BookSource {
            book_source_url: format!("test://rate-window-{}", std::process::id()),
            concurrent_rate: Some("3/300".into()),
            ..Default::default()
        };
        // 唯一 ns 隔离其他测试
        let ns = format!("ns-{}", std::process::id());
        let start = std::time::Instant::now();
        let mut futs = Vec::new();
        for _ in 0..10 {
            futs.push(concurrent_rate_acquire(&ns, &src));
        }
        for f in futs {
            f.await;
        }
        let elapsed = start.elapsed();
        // 10 次请求 / 3 每窗 → 至少等 2 个完整窗口 ≈ 600ms（余量放宽到 550）
        assert!(
            elapsed >= std::time::Duration::from_millis(550),
            "共享滑窗应串行化超窗请求，实际 {elapsed:?}"
        );
    }

    /// A2：纯数字间隔——同源两次 acquire 至少间隔该毫秒
    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_rate_interval_shared() {
        let src = BookSource {
            book_source_url: format!("test://rate-interval-{}", std::process::id()),
            concurrent_rate: Some("150".into()),
            ..Default::default()
        };
        let ns = format!("ns-i-{}", std::process::id());
        let start = std::time::Instant::now();
        concurrent_rate_acquire(&ns, &src).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        concurrent_rate_acquire(&ns, &src).await;
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(140),
            "第二次 acquire 应补足最小间隔"
        );
    }

    /// legado 列表规则前缀：`-` 倒序、`+` 去前缀、其余原样
    #[test]
    fn test_strip_list_rule_prefix() {
        assert_eq!(
            strip_list_rule_prefix("-div.book"),
            ("div.book".to_string(), true)
        );
        assert_eq!(
            strip_list_rule_prefix("+div.book"),
            ("div.book".to_string(), false)
        );
        assert_eq!(
            strip_list_rule_prefix("div.book"),
            ("div.book".to_string(), false)
        );
        assert_eq!(strip_list_rule_prefix("-"), ("".to_string(), true));
    }

    /// `-` 前缀书单：结果倒序；`+` 前缀：正常顺序
    #[test]
    fn test_analyze_html_list_prefix() {
        let html = r#"<div class="book"><h2>书名A</h2><a href="/book/1">详情</a></div>
                       <div class="book"><h2>书名B</h2><a href="/book/2">详情</a></div>"#;
        let rule = SearchRule {
            book_list: Some("div.book".into()),
            name: Some("h2".into()),
            book_url: Some("a@href".into()),
            ..Default::default()
        };
        let src = BookSource {
            book_source_url: "https://a.com".into(),
            ..Default::default()
        };
        let books = analyze_book_list(
            "default",
            html,
            "https://a.com",
            &src,
            &rule,
            "-div.book",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books.len(), 2);
        assert_eq!(
            books[0].name, "书名B",
            "`-` 前缀应倒序: {:?}",
            books[0].name
        );
        assert_eq!(books[1].name, "书名A");

        let books = analyze_book_list(
            "default",
            html,
            "https://a.com",
            &src,
            &rule,
            "+div.book",
            "key",
            &JsBridge::default(),
        );
        assert_eq!(books[0].name, "书名A", "`+` 前缀保持原序");
        assert_eq!(books[1].name, "书名B");
    }

    /// UrlSuffix.retry（legacy AnalyzeUrl UrlOption.retry，AnalyzeUrl.kt:564-573）：
    /// URL option `{...}` 的 retry 键指定该请求重试次数——数字/字符串均可解析；
    /// 缺失/非法 → None（单次请求）；其余后缀键不受影响
    #[test]
    fn test_url_suffix_retry() {
        // 数字字面量
        let (_, s) = split_url_suffix(r#"https://a.com/s,{"retry":3}"#);
        assert_eq!(s.retry, Some(3));
        // legacy setRetry(String)：字符串数字亦可
        let (_, s) =
            split_url_suffix(r#"https://a.com/s,{"retry":"2","charset":"GBK","method":"POST"}"#);
        assert_eq!(s.retry, Some(2));
        assert_eq!(s.charset.as_deref(), Some("GBK"));
        assert_eq!(s.method.as_deref(), Some("POST"));
        // 缺失 / 0 → 不重试
        let (_, s) = split_url_suffix("https://a.com/s");
        assert_eq!(s.retry, None);
        let (_, s) = split_url_suffix(r#"https://a.com/s,{"retry":0}"#);
        assert_eq!(s.retry, Some(0));
        // 非法值容错 → None，不中断后缀解析
        let (_, s) = split_url_suffix(r#"https://a.com/s,{"retry":"abc","bodyJs":"result"}"#);
        assert_eq!(s.retry, None);
        assert_eq!(s.body_js.as_deref(), Some("result"));
    }
}
