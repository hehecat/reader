//! 搜索引擎：单源请求 URL 构造、后缀解析、并发速率限制、端到端超时控制与多源调度执行

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Deserialize;
use url::Url;

use crate::model::BookSource;
use crate::parser::js::JsBridge;
use crate::service::crawler;
use crate::storage::Storage;

use super::extract::analyze_book_list_impl;
use super::{SearchBook, SearchRule};

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
    pub headers: Option<HashMap<String, String>>,
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
pub(super) fn apply_body_js(
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
static RATE_LIMITERS: LazyLock<parking_lot::Mutex<HashMap<String, Arc<RateWindow>>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

struct RateWindow {
    /// n/window → limit=n；纯数字间隔 → limit=1、window=interval
    limit: u64,
    window: Duration,
    hits: tokio::sync::Mutex<VecDeque<Instant>>,
}

impl RateWindow {
    async fn acquire(&self) {
        loop {
            let wait = {
                let mut g = self.hits.lock().await;
                let now = Instant::now();
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
fn parse_rate(rate: &str) -> Option<(u64, Duration)> {
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
        let mut table = RATE_LIMITERS.lock();
        table
            .entry(key)
            .or_insert_with(|| {
                Arc::new(RateWindow {
                    limit,
                    window,
                    hits: tokio::sync::Mutex::new(VecDeque::new()),
                })
            })
            .clone()
    };
    win.acquire().await;
}

/// 单源端到端 deadline 默认值（秒）。
///
/// 60s 是共识档：45s 会把量产 ~40~48s 的 12 个源整体截断（命中 111 → 99），60s 只
/// 截断「110s 级」尾部源（实测命中 111 → 110）。
pub(super) const SEARCH_SOURCE_DEADLINE_DEFAULT_SECS: u64 = 60;

/// 单源端到端 deadline：`READER_SEARCH_SOURCE_DEADLINE_SECS`
/// - 未设置/解析失败 → 默认 60s
/// - `0` → 关闭（None = 不包 `tokio::time::timeout`，回到改造前行为）
/// - 其余 → clamp(5, 120)：5s 以下等于人为制造超时，120s 以上没有意义（已知最长 ~120s）
fn search_source_deadline() -> Option<Duration> {
    let secs = std::env::var("READER_SEARCH_SOURCE_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(SEARCH_SOURCE_DEADLINE_DEFAULT_SECS);
    deadline_from_secs(secs)
}

/// env 秒数 → deadline（纯函数，便于单测；0 = None = 关闭）
pub(super) fn deadline_from_secs(secs: u64) -> Option<Duration> {
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs.clamp(5, 120)))
    }
}

/// 执行单个书源搜索；搜索成功（命中 ≥1 条）时记录书源使用统计（use_count+1）
///
/// legacy 对齐：抓取报错时标记运行期失效快照（getInvalidBookSources 600 秒内直接返回），
/// 成功则清除该源标记。
///
/// 外层再包一层端到端 deadline（见 [`search_source_deadline`]）：超时以 `Elapsed` 错误
/// 返回，派发处 `SearchOutcome::from_error` 会把它归成 `Timeout`（只记 timeouts，不计连败）。
pub async fn search_one_source(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    key: &str,
    page: i64,
    timeout_secs: u64,
) -> Result<Vec<SearchBook>> {
    // 单源端到端 deadline（#2）：单次 HTTP 的 timeout 参数是**乘性**的——
    // `for attempt in 0..=retries`（默认 2）× DoH 钉扎重跑（2 次）≈ 4 次 60s，
    // 再叠 CF 求解预算 30s，于是 timeout=15 也可能跑出 110~120s。把 timeout 调小没用
    // （实测 timeout=3 仍 45s），只能在整条单源链路外面加一层绝对上界。
    let Some(deadline) = search_source_deadline() else {
        return search_one_source_unbounded(storage, ns, source, key, page, timeout_secs).await;
    };
    match tokio::time::timeout(
        deadline,
        search_one_source_unbounded(storage, ns, source, key, page, timeout_secs),
    )
    .await
    {
        Ok(res) => res,
        Err(elapsed) => {
            // 保留 Elapsed 本体（`is_timeout_error` 靠 downcast 归类 Timeout，
            // 派发处 SearchOutcome::from_error → 记 attempts+1/timeouts+1，连败不涨）
            tracing::warn!(
                "搜索单源 deadline 超时 url={} deadline={}s ns={ns} book_source={} —— 按 Timeout 记账（不计连败）",
                source.book_source_url,
                deadline.as_secs(),
                source.book_source_name,
            );
            Err(anyhow::Error::new(elapsed).context(format!(
                "搜索单源端到端超时（deadline {}s，READER_SEARCH_SOURCE_DEADLINE_SECS=0 可关闭）",
                deadline.as_secs()
            )))
        }
    }
}

/// 单源完整搜索（无端到端 deadline 版本，逻辑与历史完全一致）
async fn search_one_source_unbounded(
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
                    crawler::http_post_retry_scoped(
                        crawler::FetchScope::Search,
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
                    crawler::http_get_retry_scoped(
                        crawler::FetchScope::Search,
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
        // 搜索作用域（#3）：整条浏览器优先表达式在外层短路——搜索是广度扇出，
        // 不能让 134 个源各自去赌一次 5~30s 的浏览器导航
        crawler::http_post_retry_scoped(
            crawler::FetchScope::Search,
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
        crawler::http_get_retry_scoped(
            crawler::FetchScope::Search,
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
