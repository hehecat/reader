//! HTTP 客户端池、编码探测、DNS 钉扎、表单编码与底层抓取（`fetch` / `fetch_get` / `fetch_image`）

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::cookies::session_for;
use super::ssrf::{
    validate_public_target, validate_redirect_target, validate_source_redirect_target,
    validate_source_target,
};

/// 默认移动端 UA（书源抓取 / 健康探测 / 全局池化客户端共用）
const DEFAULT_MOBILE_UA: &str = "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36";

/// 抓取响应
#[derive(Debug)]
pub struct FetchResponse {
    pub body: String,
    pub url: String,
    /// 响应头（键小写；Set-Cookie 可能有多个同名项）
    pub headers: Vec<(String, String)>,
    /// HTTP 状态码
    pub status: u16,
}

// ==================== 全局轻量复用 HTTP 客户端 ====================

static SHARED_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(DEFAULT_MOBILE_UA)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("初始化全局共享 HTTP Client 失败")
});

static SHARED_NO_REDIRECT_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(DEFAULT_MOBILE_UA)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("初始化全局无重定向共享 HTTP Client 失败")
});

/// 全局轻量复用 HTTP 客户端（基于 rustls + 连接池复用；不设全局超时，单次请求请用
/// `RequestBuilder::timeout(...)` 覆盖超时），消除健康探测/词典/FlareSolverr/camoufox
/// 每次新建 TLS 配置与连接池的开销。
pub fn shared_http_client() -> &'static reqwest::Client {
    &SHARED_HTTP_CLIENT
}

/// 全局轻量复用无重定向 HTTP 客户端（`redirect::Policy::none()` + 连接池复用，
/// 单次请求用 `RequestBuilder::timeout(...)` 覆盖超时）。
pub fn shared_no_redirect_client() -> &'static reqwest::Client {
    &SHARED_NO_REDIRECT_CLIENT
}

// ==================== 字符集探测与解码 ====================

/// 按 charset 解码字节（GB2312/GBK/UTF-8 等，encoding_rs）。
///
/// charset 为空时自动探测（对齐 legacy EncodingDetectHelp/EncodingDetect）：
/// 1) UTF-8/UTF-16 BOM；
/// 2) HTML `<meta charset=...>` / `<meta http-equiv="Content-Type" content="...; charset=...">`；
/// 3) UTF-8 严格/宽松解码（无替换字符）；
/// 4) GBK 启发式（UTF-8 出现替换字符而 GBK 可完整解码时回退）；
/// 5) 兜底 UTF-8 宽松解码。
pub fn decode_bytes(bytes: &[u8], charset: Option<&str>) -> String {
    match charset {
        Some(c) => {
            let encoding =
                encoding_rs::Encoding::for_label(c.as_bytes()).unwrap_or(encoding_rs::UTF_8);
            let (text, _, _) = encoding.decode(bytes);
            text.into_owned()
        }
        None => detect_and_decode(bytes),
    }
}

/// 自动探测编码并解码（见 [`decode_bytes`]）
fn detect_and_decode(bytes: &[u8]) -> String {
    // 1) BOM
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (text, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        return text.into_owned();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (text, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        return text.into_owned();
    }
    // 2) UTF-8 严格解码优先
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    // 3) HTML meta charset（仅扫描文档头，避免大响应全量扫描）
    let head = &bytes[..bytes.len().min(2048)];
    if let Ok(head_str) = std::str::from_utf8(head) {
        if let Some(meta) = html_meta_charset(head_str) {
            if let Some(enc) = encoding_rs::Encoding::for_label(meta.as_bytes()) {
                if !enc.name().eq_ignore_ascii_case("utf-8")
                    && !enc.name().eq_ignore_ascii_case("us-ascii")
                {
                    let (text, _, _) = enc.decode(bytes);
                    return text.into_owned();
                }
            }
        }
    }
    // 4) 宽松 UTF-8：无替换字符 → 直接返回
    let (utf8_text, _, had_errors) = encoding_rs::UTF_8.decode(bytes);
    let utf8_text = utf8_text.into_owned();
    if !had_errors {
        return utf8_text;
    }
    // 5) 统计式探测（legacy ICU4J CharsetDetector 对应——区分 GBK/Big5/Shift_JIS 等
    //    非 UTF-8 编码；仅采用 CJK 候选，避免把中文乱码误判为 Latin 单字节）
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    let guessed = detector.guess(None, false);
    let guessed_name = guessed.name().to_ascii_lowercase();
    let cjk_guess = matches!(
        guessed_name.as_str(),
        "gbk" | "gb18030" | "big5" | "euc-kr" | "shift_jis" | "euc-jp"
    );
    if cjk_guess {
        let (text, _, _) = guessed.decode(bytes);
        return text.into_owned();
    }
    // 6) GBK 启发式（中文站点无 meta 常见编码）
    let (gbk_text, _, gbk_errors) = encoding_rs::GBK.decode(bytes);
    let gbk_text = gbk_text.into_owned();
    if !gbk_errors {
        return gbk_text;
    }
    utf8_text
}

/// 从 HTML 头部提取 `<meta charset=...>` 或 `<meta http-equiv="Content-Type" content="...">`
/// 声明的字符集（大小写/单双引号/空白不敏感；未声明返回 None）
pub(super) fn html_meta_charset(head: &str) -> Option<String> {
    let lower = head.to_ascii_lowercase();
    // <meta charset=...>（可带引号与空白）
    for needle in ["charset=", "charset ="] {
        let mut pos = 0;
        while let Some(rel) = lower[pos..].find(needle) {
            let start = pos + rel + needle.len();
            let rest = lower[start..].trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
            let label = if let Some(q) = quote {
                let inner_start = q.len_utf8();
                let end = rest[inner_start..]
                    .find(q)
                    .map(|i| i + inner_start)
                    .unwrap_or(rest.len());
                rest[inner_start..end].trim().to_string()
            } else {
                rest.chars()
                    .take_while(|c| {
                        !c.is_whitespace() && *c != '"' && *c != '\'' && *c != '/' && *c != '>'
                    })
                    .collect::<String>()
                    .trim()
                    .to_string()
            };
            if !label.is_empty() {
                return Some(label);
            }
            pos = start;
        }
    }
    // <meta http-equiv="Content-Type" content="text/html; charset=...">
    if let Some(ctype_pos) = lower.find("content-type") {
        let after = &lower[ctype_pos..];
        if let Some(content_pos) = after.find("content=") {
            let content = &after[content_pos + 8..];
            if let Some(cs_pos) = content.find("charset=") {
                let rest = &content[cs_pos + 8..];
                let label: String = rest
                    .chars()
                    .take_while(|c| {
                        !c.is_whitespace() && *c != '"' && *c != '\'' && *c != ';' && *c != '>'
                    })
                    .collect();
                if !label.is_empty() {
                    return Some(label);
                }
            }
        }
    }
    None
}

/// 从 HTTP 响应头提取 Content-Type 的 charset（键已小写）
pub(super) fn content_type_charset(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .and_then(|(_, v)| {
            let lower = v.to_ascii_lowercase();
            let mut pos = 0;
            while let Some(rel) = lower[pos..].find("charset=") {
                let start = pos + rel + "charset=".len();
                let label = v[start..]
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim()
                    .to_string();
                if !label.is_empty() {
                    return Some(label);
                }
                pos = start;
            }
            None
        })
}

// ==================== Client 构建与 DNS 钉扎 ====================

/// 构建共享 HTTP client（直连/图片/TTS 复用）：
/// - 超时与 UA 统一
/// - `READER_DANGER_ACCEPT_INVALID_CERTS=1` → 接受自签名/过期证书（legacy SSLHelper trust-all 对应）
/// - `READER_CA_FILE=/path` → 追加自定义根证书（PEM）
/// - `READER_HTTP_PROXY=http://host:port` → 直连请求统一走该代理（legacy OkHttp 代理语义；
///   书源级 proxy 仍优先用于浏览器求解，不互相覆盖）
#[allow(clippy::type_complexity)] // 返回 (body, cookies, ua) 这类元组是内部约定，起别名反而要多跳一层
pub fn http_client_builder(
    timeout_secs: u64,
    redirect_policy: reqwest::redirect::Policy,
) -> Result<reqwest::Client> {
    build_http_client(timeout_secs, redirect_policy, None)
}

// ---- DNS 钉扎：fake-ip/DNS 劫持环境（clash tun 等）下容器解析到 198.18.x 假 IP 直连必败 →
// 连接层失败时经 DoH(dns.alidns.com, IP 直连免递归)解析真 IP 并按 host 钉扎 10 分钟
#[allow(clippy::type_complexity)] // host -> (过期时刻, IP 列表)，起别名反而多跳一层
static DNS_PINS: LazyLock<Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// DNS 钉扎代数：每次新增钉扎 +1。缓存的直连 Client 里 `resolve_to_addrs` 是构建期快照，
/// 新钉扎必须重建 Client 才生效——代数进缓存 key，天然淘汰旧 Client（否则钉扎后重试仍走假 IP）。
static DNS_PIN_GEN: AtomicU64 = AtomicU64::new(0);

/// 钉扎 TTL（与 [`pin_dns`] 的 retain 窗口一致）：缓存 key 带 600s 槽位，槽位翻转即重建 Client，
/// 避免缓存的 Client 把过期 IP 钉死到进程结束。
const DNS_PIN_TTL_SECS: u64 = 600;

pub(super) fn pin_dns(host: &str, ips: Vec<IpAddr>) {
    if let Ok(mut m) = DNS_PINS.lock() {
        m.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(DNS_PIN_TTL_SECS));
        m.insert(host.to_string(), (Instant::now(), ips));
    }
    DNS_PIN_GEN.fetch_add(1, Ordering::Relaxed);
}

fn dns_pins() -> Vec<(String, Vec<IpAddr>)> {
    DNS_PINS
        .lock()
        .map(|m| {
            m.iter()
                .map(|(h, (_, ips))| (h.clone(), ips.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// AliDNS DoH 专用池化 Client（钉扎 223.5.5.5:443，避免每次 DoH 重复初始化 TLS）
static DOH_CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
    let ip: IpAddr = "223.5.5.5".parse().ok()?;
    reqwest::Client::builder()
        .resolve_to_addrs("dns.alidns.com", &[SocketAddr::new(ip, 443)])
        .build()
        .ok()
});

/// DoH 解析 A 记录（AliDNS, 客户端钉扎 223.5.5.5 免被劫持）
pub(super) async fn doh_resolve(domain: &str) -> Option<Vec<IpAddr>> {
    let client = DOH_CLIENT.as_ref()?;
    let v: serde_json::Value = client
        .get(format!(
            "https://dns.alidns.com/resolve?name={domain}&type=A"
        ))
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let ips: Vec<IpAddr> = v
        .get("Answer")?
        .as_array()?
        .iter()
        .filter_map(|a| a.get("data")?.as_str()?.parse::<IpAddr>().ok())
        .collect();
    if ips.is_empty() {
        None
    } else {
        Some(ips)
    }
}

/// 实际构建：EG5 书源级代理（proxyUrl / URL option 的 proxy 键）显式指定时优先生效，
/// 覆盖 READER_HTTP_PROXY 环境变量（书源自带代理语义更精确）。timeout_secs=0 表示
/// 不设全局超时（代理 Client 缓存复用场景——每次请求经 RequestBuilder::timeout 单独限定）。
fn build_http_client(
    timeout_secs: u64,
    redirect_policy: reqwest::redirect::Policy,
    proxy: Option<&str>,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(redirect_policy)
        .user_agent(DEFAULT_MOBILE_UA);
    // 应用 DNS 钉扎（fake-ip 环境兜底）：443/80 双端口按序尝试
    for (host, ips) in dns_pins() {
        let mut addrs = Vec::with_capacity(ips.len() * 2);
        for ip in &ips {
            addrs.push(SocketAddr::new(*ip, 443));
            addrs.push(SocketAddr::new(*ip, 80));
        }
        builder = builder.resolve_to_addrs(&host, &addrs);
    }
    // timeout_secs=0 → 不设全局超时（ClientBuilder 默认无超时；请求级在 fetch_once 内限定）
    if timeout_secs > 0 {
        builder = builder.timeout(Duration::from_secs(timeout_secs));
    }
    let explicit_proxy = proxy.map(str::trim).filter(|p| !p.is_empty());
    if let Some(proxy_url) = explicit_proxy {
        match reqwest::Proxy::all(proxy_url) {
            Ok(p) => builder = builder.proxy(p),
            Err(e) => {
                tracing::warn!("书源级代理配置无效（{proxy_url}）: {e}");
            }
        }
    } else if let Ok(proxy_url) = std::env::var("READER_HTTP_PROXY") {
        let proxy_url = proxy_url.trim().to_string();
        if !proxy_url.is_empty() {
            match reqwest::Proxy::all(&proxy_url) {
                Ok(p) => builder = builder.proxy(p),
                Err(e) => {
                    tracing::warn!("READER_HTTP_PROXY 配置无效（{proxy_url}）: {e}");
                }
            }
        }
    }
    if std::env::var("READER_DANGER_ACCEPT_INVALID_CERTS")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
    {
        tracing::warn!("READER_DANGER_ACCEPT_INVALID_CERTS=1：接受自签名/无效证书（不安全，仅建议内网书源使用）");
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Ok(ca) = std::env::var("READER_CA_FILE") {
        let ca = ca.trim().to_string();
        if !ca.is_empty() {
            match std::fs::read(&ca) {
                Ok(pem) => match reqwest::Certificate::from_pem(&pem) {
                    Ok(cert) => builder = builder.add_root_certificate(cert),
                    Err(e) => {
                        tracing::warn!("READER_CA_FILE 证书解析失败（{ca}）: {e}");
                    }
                },
                Err(e) => {
                    tracing::warn!("READER_CA_FILE 读取失败（{ca}）: {e}");
                }
            }
        }
    }
    builder
        .build()
        .map_err(|e| anyhow!("构建 HTTP client 失败: {e}"))
}

/// SSRF 校验重定向策略（fetch 与代理共享 Client 共用；逐跳校验跳转目标防 302 回内网）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
fn ssrf_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        match validate_redirect_target(attempt.url().as_str()) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(e),
        }
    })
}

/// 书源抓取专用重定向策略（逐跳校验，只受 `SOURCE_ALLOW_PRIVATE` 控制）
fn source_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        match validate_source_redirect_target(attempt.url().as_str()) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(e),
        }
    })
}

/// EG5：按代理串缓存的直连 Client（reqwest Client 内置连接池/TLS 会话——每次请求重建
/// 开销大；不同代理各自实例）。上限防呆：超限整体清空重建（书源级代理数量级很小，
/// 正常不会触达）。
static PROXY_CLIENTS: LazyLock<Mutex<HashMap<String, reqwest::Client>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const PROXY_CLIENT_CACHE_MAX: usize = 32;

/// 取（或建）指定代理的共享直连 Client（无全局超时——单次请求在 [`fetch_once`] 内限时）
fn proxy_http_client(proxy_url: &str) -> Result<reqwest::Client> {
    let key = proxy_url.trim().to_string();
    if let Some(c) = PROXY_CLIENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return Ok(c.clone()); // Client 内部 Arc，clone 廉价
    }
    let client = build_http_client(0, source_redirect_policy(), Some(&key))?;
    let mut m = PROXY_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() >= PROXY_CLIENT_CACHE_MAX {
        m.clear();
    }
    m.insert(key, client.clone());
    Ok(client)
}

// ---- 直连 Client 缓存（无 proxy 分支）：线上书源几乎全部无 proxy，本分支是 100% 走的抓取路径。
// 曾经每次抓取都 `Client::builder()...build()`，连接池与 TLS 会话完全不复用（目录翻页每页重握手，
// 实测同一源经后端 3.3~5.1s vs 浏览器直连 0.8~1.6s）。现在按「会改变 Client 身份/行为」的配置
// 组合缓存，key 覆盖：全局 timeout、重定向策略 tag、READER_HTTP_PROXY 现值、证书相关开关
// （READER_DANGER_ACCEPT_INVALID_CERTS / READER_CA_FILE）、DNS 钉扎代数与钉扎 TTL 槽位。
// UA 不随源变化（builder 内固定），故不进 key；书源级 UA 由每次请求的 header 覆盖。
// 容量上限 64：到顶整体清空重建（`reqwest::Client` 构建很便宜，不做 LRU）；key 空间由
// 上述有限配置组合决定，正常情况下同源不同 timeout 也就几种。
static DIRECT_CLIENTS: LazyLock<Mutex<HashMap<String, reqwest::Client>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const DIRECT_CLIENT_CACHE_MAX: usize = 64;

/// 钉扎 TTL 槽位起点（见 [`DNS_PIN_TTL_SECS`]）
static CLIENT_CACHE_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// 直连 Client 缓存 key 的全部输入（纯数据，便于单测 key 区分度）
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientCacheKeyInputs {
    pub timeout_secs: u64,
    pub redirect_tag: &'static str,
    pub env_proxy: String,
    pub insecure_certs: bool,
    pub ca_file: String,
    pub pin_gen: u64,
    pub pin_epoch: u64,
}

impl ClientCacheKeyInputs {
    /// 直连抓取用的输入（读进程环境 + 钉扎代数/槽位）
    pub(super) fn current(timeout_secs: u64) -> Self {
        Self::current_with_redirect(timeout_secs, "source")
    }

    fn current_with_redirect(timeout_secs: u64, redirect_tag: &'static str) -> Self {
        let env = |k: &str| std::env::var(k).unwrap_or_default().trim().to_string();
        Self {
            timeout_secs,
            redirect_tag,
            env_proxy: env("READER_HTTP_PROXY"),
            insecure_certs: env("READER_DANGER_ACCEPT_INVALID_CERTS") == "1",
            ca_file: env("READER_CA_FILE"),
            pin_gen: DNS_PIN_GEN.load(Ordering::Relaxed),
            pin_epoch: CLIENT_CACHE_EPOCH.elapsed().as_secs() / DNS_PIN_TTL_SECS,
        }
    }
}

/// 缓存 key（纯函数；任何一项变化都必须产生不同 key，否则会串用身份不同的 Client）
pub(crate) fn client_cache_key(i: &ClientCacheKeyInputs) -> String {
    format!(
        "t={}\u{1}r={}\u{1}p={}\u{1}i={}\u{1}ca={}\u{1}g={}\u{1}e={}",
        i.timeout_secs,
        i.redirect_tag,
        i.env_proxy,
        u8::from(i.insecure_certs),
        i.ca_file,
        i.pin_gen,
        i.pin_epoch
    )
}

/// 容量上限插入：到顶就整体清空（简单防呆，不做 LRU）
pub(crate) fn insert_capped<V>(
    m: &mut HashMap<String, V>,
    key: String,
    value: V,
    max: usize,
) -> bool {
    if m.len() >= max && !m.contains_key(&key) {
        m.clear();
        m.insert(key, value);
        return true;
    }
    m.insert(key, value);
    false
}

/// 取（或建）无 proxy 直连抓取用的共享 Client——连接池/TLS 会话跨请求复用。
///
/// 构建失败不缓存、按原路径直接返回错误（调用方 [`fetch`] 另有降级：缓存路径整体失败时
/// 退回「每请求新建」的原逻辑），保证缓存不引入新的失败点。
pub(crate) fn shared_direct_client(timeout_secs: u64) -> Result<reqwest::Client> {
    shared_direct_client_cached(&DIRECT_CLIENTS, timeout_secs)
}

/// 缓存本体与静态表解耦：单测注入私有表，断言不再受其它用例并发抓取影响
pub(super) fn shared_direct_client_cached(
    cache: &Mutex<HashMap<String, reqwest::Client>>,
    timeout_secs: u64,
) -> Result<reqwest::Client> {
    let key = client_cache_key(&ClientCacheKeyInputs::current(timeout_secs));
    if let Some(c) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(c.clone()); // Client 内部 Arc，clone 廉价
    }
    let client = build_http_client(timeout_secs, source_redirect_policy(), None)?;
    let mut m = cache.lock().unwrap_or_else(|e| e.into_inner());
    insert_capped(&mut m, key, client.clone(), DIRECT_CLIENT_CACHE_MAX);
    Ok(client)
}

/// 取（或建）无重定向直连共享 Client（用于 `fetch_image` 等手动逐跳 SSRF 校验场景，
/// 复用连接池/TLS 且同步响应 `READER_HTTP_PROXY` / 证书 / DNS 钉扎配置）。
fn shared_no_redirect_direct_client(timeout_secs: u64) -> Result<reqwest::Client> {
    let key = client_cache_key(&ClientCacheKeyInputs::current_with_redirect(
        timeout_secs,
        "none",
    ));
    if let Some(c) = DIRECT_CLIENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return Ok(c.clone());
    }
    let client = build_http_client(timeout_secs, reqwest::redirect::Policy::none(), None)?;
    let mut m = DIRECT_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    insert_capped(&mut m, key, client.clone(), DIRECT_CLIENT_CACHE_MAX);
    Ok(client)
}

// ==================== 错误分类与重试配置 ====================

/// 可重试的传输层错误（超时/连接中断/EOF/TLS 握手——不重试 4xx/5xx 业务响应）
pub(super) fn retryable_http_error(e: &anyhow::Error) -> bool {
    // 顶层消息可能是 "error decoding response body"，真正的 EOF/断连在 cause 链里——
    // 用 {:#} 输出完整错误链再匹配，避免漏判导致传输失败不重试。
    let lower = format!("{e:#}").to_ascii_lowercase();
    // 证书类错误为确定性失败——重试必然同样失败（真实环境刷屏教训：
    // invalid peer certificate / UnknownIssuer / certificate expired 各重试 2 次纯浪费）
    if [
        "invalid peer certificate",
        "unknownissuer",
        "certificate expired",
        "certificate is not valid",
        "bad signature",
        "not valid for name",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        return false;
    }
    [
        "operation timed out",
        "timed out",
        "connection reset",
        "connection closed",
        "connection refused",
        "eof",
        "broken pipe",
        "unexpected eof",
        "end of file",
        "reading a body",
        "tls",
        "error sending request",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// 是否「超时」类错误——三态账本用（`storage::SearchOutcome::Timeout`）。
///
/// 与 [`retryable_http_error`] 的区别：那个判「值不值得重试」，这个只判「是不是慢」。
/// 超时**不写 `consec_failures`**（超时源多半是慢源，按连败定罪会把好源压成死源）；
/// 连接被拒/DNS 失败/证书错误这类确定性失败仍记 `Fail`。
pub fn is_timeout_error(e: &anyhow::Error) -> bool {
    // 优先用类型信息：reqwest 的超时 / tokio 的 deadline，避免字符串匹配误判
    if e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>()
            .map(|re| re.is_timeout())
            .unwrap_or(false)
            || c.downcast_ref::<tokio::time::error::Elapsed>().is_some()
    }) {
        return true;
    }
    // 兜底匹配错误链文本（自定义报文里常有 “timed out”）
    let lower = format!("{e:#}").to_ascii_lowercase();
    [
        "operation timed out",
        "timed out",
        "deadline has elapsed",
        "timeout",
        "超时",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// 重试次数（`READER_HTTP_RETRIES`，默认 2；0 = 不重试）
fn http_retry_count() -> usize {
    std::env::var("READER_HTTP_RETRIES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(2)
}

// ==================== charset 表单/query 字段编码（legacy AnalyzeUrl.analyzeFields） ====================

/// 值是否已是 URL 编码形态（legacy NetworkUtils.hasUrlEncoded 逐字符对齐）：
/// 全部字符属于不需编码集合 [A-Za-z0-9+-_.$:()!*@&#,[]] 或合法 %XX 序列时视为已编码
pub(super) fn has_url_encoded(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphanumeric() || b"+-_.$:()!*@&#,[]".contains(&c) {
            i += 1;
            continue;
        }
        if c == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            i += 3;
            continue;
        }
        return false;
    }
    true
}

/// URLEncoder.encode(value, charset) 对应：值按目标 charset 编码为字节后逐字节转义
/// （[A-Za-z0-9.*_-] 保留、空格→+、其余 %XX 大写十六进制——Java URLEncoder 语义）
fn url_encode_with_charset(v: &str, encoding: &'static encoding_rs::Encoding) -> String {
    let (bytes, _, _) = encoding.encode(v);
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes.iter() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// JavaScript escape() 风格编码（legacy EncoderUtils.escape 对齐）：
/// 字母数字保留；码点 <0x10 → %0x、<0x100 → %x、其余 → %uxxxx（小写十六进制）
fn js_escape_value(v: &str) -> String {
    let mut out = String::new();
    for c in v.chars() {
        let code = c as u32;
        if code < 128 && c.is_ascii_alphanumeric() {
            out.push(c);
        } else if code < 16 {
            out.push_str(&format!("%0{code:x}"));
        } else if code < 256 {
            out.push_str(&format!("%{code:x}"));
        } else {
            out.push_str(&format!("%u{code:x}"));
        }
    }
    out
}

/// legacy AnalyzeUrl.analyzeFields：按 `&` 切 k=v 对，仅对**值**做编码后重组
/// （键不编码；首个 `=` 后的全部内容为值）。已含 %XX 序列的值视为已编码不再重复编码；
/// charset="escape" 用 JS escape() 风格；其余用 URLEncoder.encode(value, charset) 语义
/// （GBK 等非 UTF-8 编码经 encoding_rs 出字节再百分号转义，空格→+）
pub(super) fn encode_form_fields(fields: &str, charset: &str) -> String {
    let label = charset.trim();
    let is_escape = label.eq_ignore_ascii_case("escape");
    let encoding = if is_escape {
        None
    } else {
        Some(encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8))
    };
    fields
        .split('&')
        .filter(|seg| !seg.trim().is_empty())
        .map(|seg| match seg.split_once('=') {
            Some((k, v)) => {
                let value = if has_url_encoded(v) {
                    v.to_string()
                } else if is_escape {
                    js_escape_value(v)
                } else {
                    url_encode_with_charset(v, encoding.unwrap_or(encoding_rs::UTF_8))
                };
                format!("{k}={value}")
            }
            None => seg.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

// ==================== 原始抓取（fetch / fetch_get / fetch_image） ====================

/// 抓取（GET/POST，支持 header JSON；charset 指定时转码）
///
/// P1 SSRF 全覆盖：**入口 URL 与每个重定向跳转目标均做公网校验**（DNS 解析后——
/// 拒绝私网/回环/链路本地（含 169.254 云元数据）/未指定地址，错误返回）。
/// http_get/http_post（书源抓取）、java.ajax 等 JS shim、rss/schedule 订阅抓取
/// 全部经本函数出网——统一生效。传输层失败自动重试（默认 2 次，指数退避）。
///
/// EG5：proxy = 书源级代理（proxyUrl / URL option proxy 键，由 http_fetch 透传）——
/// 直连 reqwest 同样走该代理（legacy 书源代理作用于全部请求的语义）；None = 不带代理。
pub async fn fetch(
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    method: &str,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    // 入口目标校验（DNS 解析后——拒绝私网/回环/169.254 等；书源专用开关控制）
    validate_source_target(url).await?;
    // EG5：书源级代理 → 共享代理 Client（按代理串缓存）；否则共享直连 Client
    // （按配置组合缓存，复用连接池/TLS 会话）
    let client = match proxy.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            tracing::debug!("http_fetch 走书源级代理 {p} {method} {url}");
            proxy_http_client(p)?
        }
        // 缓存路径整体失败时降级回「每请求新建」的原逻辑（缓存不引入新的失败点）
        None => shared_direct_client(timeout_secs)
            .or_else(|_| http_client_builder(timeout_secs, source_redirect_policy()))?,
    };
    let retries = http_retry_count();
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..=retries {
        match fetch_once(&client, url, headers, method, body, charset, timeout_secs).await {
            Ok(r) => return Ok(r),
            Err(e) => {
                if attempt >= retries || !retryable_http_error(&e) {
                    return Err(e);
                }
                tracing::warn!(
                    "http_fetch 传输失败第 {}/{} 次重试 {url}: {e:#}",
                    attempt + 1,
                    retries
                );
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("http_fetch 重试耗尽")))
}

/// 单次 HTTP 请求（不含重试；供 [`fetch`] 循环调用）。
/// timeout_secs 在请求级限定（代理共享 Client 无全局超时；直连 Client 的全局超时同值被覆盖，语义不变）
async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    method: &str,
    body: Option<&str>,
    charset: Option<&str>,
    timeout_secs: u64,
) -> Result<FetchResponse> {
    let method = if method.eq_ignore_ascii_case("POST") {
        reqwest::Method::POST
    } else {
        reqwest::Method::GET
    };
    let mut req = client
        .request(method, url)
        .timeout(Duration::from_secs(timeout_secs.max(1)));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    if let Some(b) = body {
        req = req.body(b.to_string());
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let bytes = resp.bytes().await?;
    // charset 优先级：URL 后缀显式 charset > HTTP Content-Type > HTML meta/自动探测
    let charset = match charset {
        Some(c) => Some(c.to_string()),
        None => content_type_charset(&resp_headers),
    };
    let body = decode_bytes(&bytes, charset.as_deref());
    Ok(FetchResponse {
        body,
        url: final_url,
        headers: resp_headers,
        status,
    })
}

/// 兼容旧签名（GET）
pub async fn fetch_get(
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
) -> Result<FetchResponse> {
    fetch(url, headers, timeout_secs, "GET", None, None, None).await
}

/// 图片代理抓取（GAP #88/125）：二进制安全 + 限流下载 + SSRF 防护
///
/// - 自动附加书源登录态（cookie + 记录的 UA，按用户命名空间）与 Referer（防盗链绕过）
/// - SSRF 防护（M1）：每跳（含重定向）DNS 解析后校验目标为公网地址——私网/回环/链路本地
///   一律拒绝；非 secure 模式同样生效（本函数是 /assets/proxy 唯一回源入口）
/// - 超时 timeout_secs；Content-Length 超限直接拒绝，流式读取累计超 max_bytes 截断报错
/// - 返回 (图片字节, Content-Type, HTTP 状态码)
pub async fn fetch_image(
    ns: &str,
    url: &str,
    referer: Option<&str>,
    timeout_secs: u64,
    max_bytes: u64,
) -> Result<(Vec<u8>, Option<String>, u16)> {
    use futures::StreamExt;

    // 禁用自动重定向：手动逐跳跟进并在每跳都做 SSRF 校验（防 302 跳回内网）
    let client = shared_no_redirect_direct_client(timeout_secs)
        .or_else(|_| http_client_builder(timeout_secs, reqwest::redirect::Policy::none()))?;
    let mut current = url.to_string();
    for _hop in 0..=10 {
        validate_public_target(&current).await?;
        let mut req = client
            .get(&current)
            .timeout(Duration::from_secs(timeout_secs.max(1)));
        if let Some(r) = referer.filter(|r| !r.trim().is_empty()) {
            req = req.header("Referer", r);
        }
        // 书源登录态（cookie + UA）按用户命名空间附加
        let (cookie, stored_ua) = session_for(ns, &current).await.unwrap_or_default();
        if !cookie.is_empty() {
            req = req.header("Cookie", cookie);
        }
        if !stored_ua.is_empty() {
            req = req.header("User-Agent", stored_ua);
        }
        let resp = req.send().await?;
        let status = resp.status();
        // 重定向：手动跟进（每跳重新校验目标）
        if status.is_redirection() {
            if let Some(loc) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            {
                let next = url::Url::parse(&current)?.join(loc)?;
                current = next.to_string();
                continue;
            }
            // 无 Location 的重定向状态：空体透传
            return Ok((Vec::new(), None, status.as_u16()));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        // Content-Length 预检（超限拒绝，避免无谓下载）
        if resp.content_length().is_some_and(|cl| cl > max_bytes) {
            anyhow::bail!("图片超过大小上限");
        }
        // 流式读取 + 累计上限（服务端不守 Content-Length 时兜底）
        let mut bytes: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > max_bytes as usize {
                anyhow::bail!("图片超过大小上限");
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok((bytes, content_type, status.as_u16()));
    }
    anyhow::bail!("图片重定向次数过多")
}
