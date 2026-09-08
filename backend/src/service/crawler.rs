//! HTTP 抓取客户端（reqwest，书源抓取）
//!
//! - `http_get`/`http_post`：书源抓取入口——按用户命名空间 + 请求 URL 的 baseUrl
//!   从 book_source_cookies 表读取书源 cookie 自动附加（登录态独立于系统用户）；
//!   响应命中 Cloudflare 质询特征时自动转 FlareSolverr 解（见 `flaresolverr` 模块说明）。
//! - `fetch`/`fetch_get`：原始抓取（不带 cookie/FS 逻辑），供 RSS/TTS 等非书源场景。

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// 内置浏览器 CF 质询求解的质询等待循环上限（任务要求最多 30s）
pub const CF_SOLVE_MAX_WAIT_MS: u64 = 30_000;

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
fn html_meta_charset(head: &str) -> Option<String> {
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
fn content_type_charset(headers: &[(String, String)]) -> Option<String> {
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

/// 构建共享 HTTP client（直连/图片/TTS 复用）：
/// - 超时与 UA 统一
/// - `READER_DANGER_ACCEPT_INVALID_CERTS=1` → 接受自签名/过期证书（legacy SSLHelper trust-all 对应）
/// - `READER_CA_FILE=/path` → 追加自定义根证书（PEM）
/// - `READER_HTTP_PROXY=http://host:port` → 直连请求统一走该代理（legacy OkHttp 代理语义；
///   书源级 proxy 仍优先用于浏览器求解，不互相覆盖）
pub fn http_client_builder(
    timeout_secs: u64,
    redirect_policy: reqwest::redirect::Policy,
) -> Result<reqwest::Client> {
    build_http_client(timeout_secs, redirect_policy, None)
}

// ---- DNS 钉扎：fake-ip/DNS 劫持环境（clash tun 等）下容器解析到 198.18.x 假 IP 直连必败 →
// 连接层失败时经 DoH(dns.alidns.com, IP 直连免递归)解析真 IP 并按 host 钉扎 10 分钟
static DNS_PINS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<std::net::IpAddr>)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn pin_dns(host: &str, ips: Vec<std::net::IpAddr>) {
    if let Ok(mut m) = DNS_PINS.lock() {
        m.retain(|_, (t, _)| t.elapsed() < std::time::Duration::from_secs(600));
        m.insert(host.to_string(), (std::time::Instant::now(), ips));
    }
}

fn dns_pins() -> Vec<(String, Vec<std::net::IpAddr>)> {
    DNS_PINS
        .lock()
        .map(|m| m.iter().map(|(h, (_, ips))| (h.clone(), ips.clone())).collect())
        .unwrap_or_default()
}

/// DoH 解析 A 记录（AliDNS, 客户端钉扎 223.5.5.5 免被劫持）
async fn doh_resolve(domain: &str) -> Option<Vec<std::net::IpAddr>> {
    use std::net::SocketAddr;
    let client = reqwest::Client::builder()
        .resolve_to_addrs(
            "dns.alidns.com",
            &[SocketAddr::new("223.5.5.5".parse().ok()?, 443)],
        )
        .timeout(Duration::from_secs(6))
        .build()
        .ok()?;
    let v: serde_json::Value = client
        .get(format!("https://dns.alidns.com/resolve?name={domain}&type=A"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let ips: Vec<std::net::IpAddr> = v
        .get("Answer")?
        .as_array()?
        .iter()
        .filter_map(|a| a.get("data")?.as_str()?.parse::<std::net::IpAddr>().ok())
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
        .user_agent("Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36");
    // 应用 DNS 钉扎（fake-ip 环境兜底）：443/80 双端口按序尝试
    for (host, ips) in dns_pins() {
        let mut addrs = Vec::with_capacity(ips.len() * 2);
        for ip in &ips {
            addrs.push(std::net::SocketAddr::new(*ip, 443));
            addrs.push(std::net::SocketAddr::new(*ip, 80));
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
fn ssrf_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        match validate_redirect_target(attempt.url().as_str()) {
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
    let client = build_http_client(0, ssrf_redirect_policy(), Some(&key))?;
    let mut m = PROXY_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() >= PROXY_CLIENT_CACHE_MAX {
        m.clear();
    }
    m.insert(key, client.clone());
    Ok(client)
}

/// 可重试的传输层错误（超时/连接中断/EOF/TLS 握手——不重试 4xx/5xx 业务响应）
fn retryable_http_error(e: &anyhow::Error) -> bool {
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
fn has_url_encoded(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphanumeric() || b"+-_.$:()!*@&#,[]".contains(&c) {
            i += 1;
            continue;
        }
        if c == b'%' && i + 2 < b.len() {
            if b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
                i += 3;
                continue;
            }
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
fn encode_form_fields(fields: &str, charset: &str) -> String {
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
    // 入口目标校验（DNS 解析后——拒绝私网/回环/169.254 等）
    validate_public_target(url).await?;
    // EG5：书源级代理 → 共享代理 Client（按代理串缓存）；否则按次构建直连 Client
    let client = match proxy.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            tracing::debug!("http_fetch 走书源级代理 {p} {method} {url}");
            proxy_http_client(p)?
        }
        None => http_client_builder(timeout_secs, ssrf_redirect_policy())?,
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
    let client = http_client_builder(timeout_secs, reqwest::redirect::Policy::none())?;
    let mut current = url.to_string();
    for _hop in 0..=10 {
        validate_public_target(&current).await?;
        let mut req = client.get(&current);
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

// ==================== SSRF 防护（/assets/proxy 回源目标校验，M1） ====================

/// 测试钩子：允许私网/回环回源目标（仅测试代码设置；生产恒为 false）。
/// 生产代码不读环境变量、无配置入口——所有请求强制校验。
pub static SSRF_ALLOW_PRIVATE: AtomicBool = AtomicBool::new(false);

/// 测试互斥锁：所有读写 `SSRF_ALLOW_PRIVATE` 的测试持同一把锁（串行），
/// 避免并行测试互相干扰（放行态与拦截态断言互斥）。
#[cfg(test)]
static SSRF_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 测试用 RAII 守卫：持全局互斥锁并将私网放行开关置为 `allow`；Drop 时恢复原值。
/// 用法：`let _g = ssrf_allow_private_guard(true);`（mock 服务器绑定 127.0.0.1 的测试）；
/// 拦截断言测试用 `ssrf_allow_private_guard(false)` 持锁确保无并发放行。
#[cfg(test)]
pub struct SsrfAllowGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: bool,
}

/// 取测试守卫（见 [`SsrfAllowGuard`]）
#[cfg(test)]
pub fn ssrf_allow_private_guard(allow: bool) -> SsrfAllowGuard {
    let _lock = SSRF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = SSRF_ALLOW_PRIVATE.swap(allow, Ordering::Relaxed);
    SsrfAllowGuard { _lock, prev }
}

#[cfg(test)]
impl Drop for SsrfAllowGuard {
    fn drop(&mut self) {
        SSRF_ALLOW_PRIVATE.store(self.prev, Ordering::Relaxed);
    }
}

/// 目标 IP 是否为应拦截的私网/回环/链路本地地址：
/// IPv4：127.0.0.0/8（回环）、10/8、172.16/12、192.168/16（私网）、169.254/16（链路本地）、
/// 0.0.0.0（未指定）、255.255.255.255（广播）；
/// IPv6：::1（回环）、fc00::/7（ULA）、fe80::/10（链路本地）、::（未指定）、
/// IPv4 映射地址（::ffff:a.b.c.d）递归按 IPv4 判定。
pub fn is_private_target_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private_target_ip(std::net::IpAddr::V4(v4)))
        }
    }
}

/// 重定向跳转目标校验（同步版——reqwest redirect `Policy::custom` 闭包内调用；
/// 语义与 [`validate_public_target`] 一致：字面 IP 直接判定、域名解析后逐个 IP 校验、
/// 私网/回环/链路本地/未指定/广播一律拒绝、解析失败拒绝；测试钩子
/// `SSRF_ALLOW_PRIVATE` 放行态同样生效）。
pub fn validate_redirect_target(url: &str) -> Result<()> {
    if SSRF_ALLOW_PRIVATE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("重定向目标 URL 非法: {e}"))?;
    // 字面 IP 快速路径（不经 DNS——Host::Ipv6 直接判回环，不依赖系统 IPv6 支持）
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V4(ip)) {
                anyhow::bail!("重定向目标为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Ipv6(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V6(ip)) {
                anyhow::bail!("重定向目标为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Domain(_)) => {}
        None => anyhow::bail!("重定向目标缺少主机名"),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("重定向目标缺少主机名"))?;
    if host.eq_ignore_ascii_case("localhost") {
        anyhow::bail!("重定向目标 localhost 为回环地址，已拦截");
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .map_err(|e| anyhow!("重定向目标域名解析失败（{host}）: {e}"))?;
    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if is_private_target_ip(addr.ip()) {
            anyhow::bail!(
                "重定向目标域名 {host} 解析到内网/回环地址（{}），已拦截",
                addr.ip()
            );
        }
    }
    if !resolved_any {
        anyhow::bail!("重定向目标域名 {host} 无可用地址");
    }
    Ok(())
}

/// 校验回源目标为公网地址（SSRF 防护，M1）：
/// - 字面 IP：直接判定（回环/私网/链路本地/未指定/广播一律拒绝）；
/// - 域名：DNS 解析后逐个 IP 校验（任一解析到私网即拒绝）；localhost 直接拒绝；
/// - 解析失败 / 无地址 → 拒绝。
/// 供 fetch_image 每跳调用（含重定向目标）——/assets/proxy 非 secure 模式同样生效。
pub async fn validate_public_target(url: &str) -> Result<()> {
    if SSRF_ALLOW_PRIVATE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("目标 URL 非法: {e}"))?;
    // 字面 IP 快速路径（不经 DNS——Host::Ipv6 直接判回环，不依赖系统 IPv6 支持）
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V4(ip)) {
                anyhow::bail!("目标地址为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Ipv6(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V6(ip)) {
                anyhow::bail!("目标地址为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Domain(_)) => {}
        None => anyhow::bail!("目标 URL 缺少主机名"),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("目标 URL 缺少主机名"))?;
    if host.eq_ignore_ascii_case("localhost") {
        anyhow::bail!("目标地址 localhost 为回环地址，已拦截");
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let mut resolved_any = false;
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| anyhow!("目标域名解析失败（{host}）: {e}"))?;
    for addr in addrs {
        resolved_any = true;
        if is_private_target_ip(addr.ip()) {
            anyhow::bail!(
                "目标域名 {host} 解析到内网/回环地址（{}），已拦截",
                addr.ip()
            );
        }
    }
    if !resolved_any {
        anyhow::bail!("目标域名 {host} 无可用地址");
    }
    Ok(())
}

// ==================== 书源 cookie（按用户隔离） ====================

/// 书源 cookie 存取：由 router 启动时注册（底层 Storage；None = 未注册，不附加 cookie）。
/// 全局注册（对齐 js.rs SOURCE_VARS 模式）：Storage 为连接池句柄，Clone 廉价。
static COOKIE_STORAGE: LazyLock<Mutex<Option<crate::storage::Storage>>> =
    LazyLock::new(|| Mutex::new(None));

/// 注册书源 cookie 存储（router 初始化时调用一次）
pub fn register_cookie_storage(storage: crate::storage::Storage) {
    *COOKIE_STORAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(storage);
}

/// 测试用：清空注册（回到无 cookie 状态）
pub fn clear_cookie_storage() {
    *COOKIE_STORAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 取请求 URL 的 baseUrl（scheme://host[:port]）——登录头/UA 会话匹配键
pub fn base_url_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{}://{host}{port}", parsed.scheme()))
}

/// legacy NetworkUtils.getSubDomain：cookie 存储域键。
/// host[:port] 去掉最左标签（≥2 个点时），单标签原样；不含 scheme。
/// （legacy CookieStore 内部即按此归一 tag——www/m/接口子域与裸域共享同一份 cookie）
pub(crate) fn cookie_subdomain(url: &str) -> String {
    let authority = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    let authority = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    match (authority.find('.'), authority.rfind('.')) {
        (Some(fd), Some(ld)) if fd != ld => authority[fd + 1..].to_string(),
        _ => authority.to_string(),
    }
}

/// 合并两段 Cookie 串：stored 为底、explicit 逐键覆盖（legacy AnalyzeUrl.setCookie
/// 的 `cookieMap.putAll(customCookieMap)` 语义——显式头同名键优先，非整体替换）
fn merge_cookie_strings(stored: &str, explicit: &str) -> String {
    if stored.is_empty() {
        return explicit.to_string();
    }
    if explicit.is_empty() {
        return stored.to_string();
    }
    let mut pairs = parse_cookie_string(stored);
    for (k, v) in parse_cookie_string(explicit) {
        match pairs.iter_mut().find(|(ek, _)| ek == &k) {
            Some(slot) => slot.1 = v,
            None => pairs.push((k, v)),
        }
    }
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 从响应头提取 Set-Cookie 的 name=value 对（忽略 Path/Expires 等属性）
fn extract_set_cookie_pairs(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let first = value.split(';').next().unwrap_or("");
        if let Some((k, v)) = first.split_once('=') {
            let k = k.trim();
            if !k.is_empty() && !k.eq_ignore_ascii_case("expires") {
                out.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

/// legacy AnalyzeUrl.saveCookieJar / OkHttp CookieJar 语义：
/// 响应 Set-Cookie 按域合并回存（既有 cookie 为底、新值逐键覆盖），
/// 后续同域请求自动携带会话。
async fn capture_set_cookies(ns: &str, url: &str, resp: &FetchResponse) {
    let pairs = extract_set_cookie_pairs(&resp.headers);
    if pairs.is_empty() {
        return;
    }
    let existing = session_for(ns, url).await.unwrap_or_default().0;
    let mut merged_pairs = parse_cookie_string(&existing);
    for (k, v) in pairs {
        match merged_pairs.iter_mut().find(|(ek, _)| ek == &k) {
            Some(slot) => slot.1 = v,
            None => merged_pairs.push((k, v)),
        }
    }
    let merged = merged_pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    if !merged.is_empty() {
        set_cookie_for(ns, url, &merged).await;
    }
}

/// 按命名空间 + 请求 URL 查书源 cookie（无注册/未命中 → None）。
/// 域键为 legacy getSubDomain 子域归一（E6）；未命中时回退旧 origin 键读取
/// （兼容历史会话，写入一律走新键）。
pub async fn cookie_for(ns: &str, url: &str) -> Option<String> {
    let storage = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    let sub = cookie_subdomain(url);
    if let Ok(Some(c)) = storage.get_cookie_by_base(ns, &sub).await {
        return Some(c).filter(|c| !c.is_empty());
    }
    // 兼容回退：旧数据按 origin 键存储
    let base = base_url_of(url)?;
    storage.get_cookie_by_base(ns, &base).await.ok().flatten()
}

/// 按命名空间 + 请求 URL 写入书源 cookie（legado `cookie.setCookie`/`java.getCookie` 后端；
/// 无注册存储时静默 no-op）。域键为 legacy getSubDomain 归一。
pub async fn set_cookie_for(ns: &str, url: &str, cookie: &str) {
    if cookie.trim().is_empty() {
        return;
    }
    let key = cookie_subdomain(url);
    if key.is_empty() {
        return;
    }
    let storage = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(storage) = storage {
        let _ = storage.set_cookie(ns, &key, cookie).await;
    }
}

/// 按命名空间 + 请求 URL 清除书源 cookie（legado `cookie.removeCookie`/`clearCookie`；
/// 新旧两种键都清——兼容历史 origin 键数据）
pub async fn remove_cookie_for(ns: &str, url: &str) {
    let sub = cookie_subdomain(url);
    let base = base_url_of(url);
    let storage = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(storage) = storage else { return };
    if !sub.is_empty() {
        let _ = storage.clear_cookie(ns, &sub).await;
    }
    if let Some(base) = base {
        let _ = storage.clear_cookie(ns, &base).await;
    }
}

/// 按命名空间 + 请求 URL 查书源登录头（legacy `source.getLoginHeader()`；无注册/未命中 → None）
pub async fn login_header_for(ns: &str, url: &str) -> Option<String> {
    let base = base_url_of(url)?;
    let storage = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    storage
        .get_login_header_by_base(ns, &base)
        .await
        .ok()
        .flatten()
}

/// 按命名空间 + 书源 key 写入登录头（legacy `source.putLoginHeader`/`removeLoginHeader`；
/// 空值 = 清除；无注册存储时静默 no-op）
pub async fn set_login_header_for(ns: &str, source_url: &str, header: &str) {
    let storage = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(storage) = storage {
        let _ = storage.set_login_header(ns, source_url, header).await;
    }
}

/// 按命名空间 + 请求 URL 查书源登录态（cookie + user_agent）
pub async fn session_for(ns: &str, url: &str) -> Option<(String, String)> {
    let base = base_url_of(url)?;
    let storage = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT source_url, cookie, user_agent FROM book_source_cookies WHERE user_namespace = ?1",
    )
    .bind(ns)
    .fetch_all(&storage.pool)
    .await
    .ok()?;
    let target = crate::storage::normalize_base(&base)?;
    for (source_url, cookie, ua) in rows {
        // `##` 后缀：主地址/备用地址任一段命中即可（与 book_sources 语义一致）
        let any_match = source_url
            .split("##")
            .any(|part| crate::storage::normalize_base(part) == Some(target.clone()));
        if any_match {
            return Some((cookie, ua));
        }
    }
    None
}

// ==================== 书源抓取（带 cookie + Cloudflare 质询绕过） ====================

/// 书源 GET（自动附加书源 cookie；CF 质询自动转 FlareSolverr；proxy = 书源级代理——
/// 求解浏览器出口，None = 回退环境变量 READER_OBSCURA_PROXY；
/// charset 非空非 utf-8 时 URL query 值按该编码百分号编码——legacy analyzeFields）
pub async fn http_get(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    http_fetch(ns, url, headers, timeout_secs, "GET", None, charset, proxy).await
}

/// 书源 POST（自动附加书源 cookie；CF 质询自动转 FlareSolverr；proxy 同 http_get）
pub async fn http_post(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    http_fetch(ns, url, headers, timeout_secs, "POST", body, charset, proxy).await
}

/// [`http_get`] 带 legacy UrlOption.retry 重试：retry=None/0 → 单次请求；
/// n>0 失败后最多再试 n 次（立即重试，无退避；封顶 10 防呆）
pub async fn http_get_retry(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    charset: Option<&str>,
    proxy: Option<&str>,
    retry: Option<u32>,
) -> Result<FetchResponse> {
    fetch_with_retry(retry, || {
        http_get(ns, url, headers, timeout_secs, charset, proxy)
    })
    .await
}

/// [`http_post`] 重试语义同 [`http_get_retry`]
pub async fn http_post_retry(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
    retry: Option<u32>,
) -> Result<FetchResponse> {
    fetch_with_retry(retry, || {
        http_post(ns, url, headers, timeout_secs, body, charset, proxy)
    })
    .await
}

/// 简单重试循环（legacy AnalyzeUrl UrlOption.retry）：共 retry+1 次尝试，失败即重试
async fn fetch_with_retry<T, F, Fut>(retry: Option<u32>, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let attempts = retry.unwrap_or(0).min(10) as usize + 1;
    let mut attempt = 0usize;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                attempt += 1;
                if attempt >= attempts {
                    return Err(e);
                }
                tracing::debug!("请求失败，重试 {attempt}/{}: {e}", attempts - 1);
            }
        }
    }
}

/// 书源抓取统一入口：cookie 注入 → 直连 → CF 质询检测 → FlareSolverr 兜底
async fn http_fetch(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    method: &str,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    let first = http_fetch_inner(ns, url, headers, timeout_secs, method, body, charset, proxy).await;
    match first {
        Err(e) if proxy.is_none() => {
            let conn_fail = e
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|r| r.is_connect() || r.is_request());
            if !conn_fail {
                return Err(e);
            }
            // fake-ip/DNS 劫持环境（clash tun 等）：连接层失败 → DoH 解析真 IP 钉扎后重试一次
            let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string));
            let resolved = match host {
                Some(h) => doh_resolve(&h).await.map(|ips| (h, ips)),
                None => None,
            };
            match resolved {
                Some((h, ips)) => {
                    tracing::info!("DoH 解析 {h} -> {ips:?}（本地 DNS 疑似劫持/fake-ip）——钉扎重试");
                    pin_dns(&h, ips);
                    http_fetch_inner(ns, url, headers, timeout_secs, method, body, charset, proxy)
                        .await
                }
                None => Err(e),
            }
        }
        other => other,
    }
}


async fn http_fetch_inner(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    method: &str,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    // 0) data URI（legado dataUriRegex：`data:;base64,<payload>`——搜索/详情规则可直接
    //    内嵌 base64 数据，不发起网络请求；搜索 URL 后缀 `{"type":...}` 已被切分）
    if crate::service::search::is_data_uri(url) {
        let payload = url.split_once(',').map(|(_, p)| p).unwrap_or("");
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|e| anyhow!("data URI base64 解码失败: {e}"))?;
        return Ok(FetchResponse {
            body: String::from_utf8_lossy(&bytes).into_owned(),
            url: url.to_string(),
            headers: Vec::new(),
            status: 200,
        });
    }
    // ⓪b charset 表单/query 字段编码（legacy AnalyzeUrl.analyzeFields）：POST 非 JSON
    //    form body 与 GET URL query 的**值**按 charset 百分号编码后发送——书源写中文
    //    原文 + charset=gbk 时若原样发出 UTF-8 字节，服务端按 GBK 解码必然乱码。
    //    charset 为空/utf-8 时不动（保持既有行为）
    let charset_need_encode = charset.is_some_and(|c| {
        let c = c.trim();
        !c.is_empty() && !matches!(c.to_ascii_lowercase().as_str(), "utf-8" | "utf8" | "utf_8")
    });
    let mut url = url.to_string();
    let mut body: Option<String> = body.map(|b| b.to_string());
    let mut force_form_content_type = false;
    if charset_need_encode {
        let cs = charset.unwrap_or_default().trim();
        if method.eq_ignore_ascii_case("POST") {
            if let Some(b) = body.as_ref() {
                // 像 form 才编码（含 & 和 = 且不含 {——JSON/XML body 原样，legacy isJson/isXml 对应）
                if b.contains('&') && b.contains('=') && !b.contains('{') {
                    body = Some(encode_form_fields(b, cs));
                    // legacy 仅在未显式声明 Content-Type 时分析字段；显式头不覆盖，
                    // 但字段值仍按 charset 编码（否则中文原文照发乱码依旧）
                    force_form_content_type = !headers
                        .keys()
                        .any(|k| k.eq_ignore_ascii_case("Content-Type"));
                }
            }
        } else if let Some(qpos) = url.find('?') {
            let end = url[qpos..].find('#').map_or(url.len(), |i| qpos + i);
            let base = url[..qpos].to_string();
            let query = url[qpos + 1..end].to_string();
            let tail = url[end..].to_string();
            if query.contains('=') && !query.contains('{') {
                url = format!("{base}?{}{tail}", encode_form_fields(&query, cs));
            }
        }
    }
    // 浏览器优先路径同样执行 SSRF 入口校验（obscura 侧还默认禁 RFC1918 内网导航，
    // 双保险——否则默认浏览器优先会让私网书源 URL 绕过 fetch 的直连校验）
    validate_public_target(&url).await?;
    // ① 书源 cookie + 记录的 UA（FlareSolverr 返回的 UA 绑定 cookie——部分站点校验 UA 一致性）
    let (session_cookie, stored_ua) = session_for(ns, &url).await.unwrap_or_default();
    // E6：cookie 主读路径走 legacy getSubDomain 域键；旧 origin 键会话作回退
    let cookie = match cookie_for(ns, &url).await {
        Some(c) if !c.is_empty() => c,
        _ => session_cookie,
    };
    let mut req_headers = headers.clone();
    if !cookie.is_empty() {
        // E4（legacy AnalyzeUrl.setCookie）：存储 cookie 为底、显式 Cookie 头逐键覆盖
        // ——不再整体顶掉书源自带的 token 型 Cookie
        let explicit = req_headers
            .get("Cookie")
            .or_else(|| req_headers.get("cookie"))
            .cloned();
        req_headers.remove("Cookie");
        req_headers.remove("cookie");
        let merged_cookie = match explicit {
            Some(e) if !e.trim().is_empty() => merge_cookie_strings(&cookie, &e),
            _ => cookie.clone(),
        };
        req_headers.insert("Cookie".to_string(), merged_cookie);
    }
    if !stored_ua.is_empty()
        && !req_headers.contains_key("User-Agent")
        && !req_headers.contains_key("user-agent")
    {
        req_headers.insert("User-Agent".to_string(), stored_ua);
    }
    // ①b 书源登录头（legacy getHeaderMap(true)：登录成功后 JS 保存的 header 自动附加，
    //    且覆盖源 header 同名键——登录态优先）
    if let Some(login_header) = login_header_for(ns, &url).await {
        merge_login_header(&mut req_headers, &login_header);
    }
    // ⓪b 续：重组后的 form body 未显式 Content-Type 时补 form 头（放在登录头合并后，
    //    避免被覆盖；显式声明过 Content-Type 的书源保持原头不动）
    if force_form_content_type {
        req_headers.insert(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        );
    }

    // ② 浏览器优先（默认开启）：GET 先经内置 obscura 导航，跳过 reqwest 直连。
    //    浏览器后端不可用时自动回退直连（不因缺浏览器而整体不可用）。
    tracing::debug!("http_fetch 直连 {method} {url}");
    let browser_first_get = (browser_first_enabled() || browser_needed(&url))
        && method.eq_ignore_ascii_case("GET")
        && browser_solver_available();
    let resp = if browser_first_get {
        // 浏览器优先模式：跳过 reqwest 直连，直接进入下方浏览器求解链；
        // status=0 是“未直连”哨兵（不伪造 200，避免被当作真实成功响应）
        FetchResponse {
            body: String::new(),
            url: url.to_string(),
            headers: Vec::new(),
            status: 0,
        }
    } else {
        match fetch(
            &url,
            &req_headers,
            timeout_secs,
            method,
            body.as_deref(),
            charset,
            proxy,
        )
        .await
        {
            Ok(r) => {
                // E5（legacy AnalyzeUrl.saveCookieJar / OkHttp CookieJar）：
                // 响应 Set-Cookie 按域合并回存，后续同域请求自动携带会话
                capture_set_cookies(ns, &url, &r).await;
                r
            }
            Err(e) => {
                tracing::error!(
                    "http_fetch 直连失败 {url}: {e:?} source={:?}",
                    e.source().map(|s| s.to_string())
                );
                // fake-ip/DNS 劫持环境：连接层失败 → DoH 解析真 IP 钉扎后重试一次
                // 默认浏览器兜底（READER_BROWSER_FALLBACK_DISABLE=1 关闭）：网络层失败
                // （超时/连接中断/TLS）时内置 obscura 浏览器重试——很多站点的反爬只对
                // 直连 reqwest 指纹生效，浏览器 stealth 指纹可正常访问
                if browser_fallback_enabled()
                    && method.eq_ignore_ascii_case("GET")
                    && should_browser_rescue_error(&e)
                {
                    match solve_cf_builtin(ns, &url, &cookie, proxy).await {
                        Ok((fallback, merged, solved_ua)) => {
                            if cf_browser_available() {
                                mark_browser_needed(&url);
                            }
                            let retry_cookie = merged.clone().unwrap_or_default();
                            let mut retry_headers = headers.clone();
                            if !retry_cookie.is_empty() {
                                retry_headers.insert("Cookie".to_string(), retry_cookie);
                            }
                            if !solved_ua.is_empty()
                                && !retry_headers.contains_key("User-Agent")
                                && !retry_headers.contains_key("user-agent")
                            {
                                retry_headers.insert("User-Agent".to_string(), solved_ua);
                            }
                            if let Ok(retry) = fetch(
                                &url,
                                &retry_headers,
                                timeout_secs,
                                method,
                                body.as_deref(),
                                charset,
                                proxy,
                            )
                            .await
                            {
                                return Ok(retry);
                            }
                            return Ok(fallback);
                        }
                        Err(browser_err) => {
                            tracing::warn!(
                                    "直连失败后浏览器兜底也失败（{url}）: {browser_err:#}——返回直连错误"
                                );
                            return Err(e);
                        }
                    }
                }
                return Err(e);
            }
        }
    };

    // ③ CF 质询检测（503/403 + 特征 HTML）→ 解质询降级链：FlareSolverr（配置了 URL）→
    //    内置浏览器（进程内 CDP，含 Turnstile 分支）→ 求解成功 cookie 合并存库后
    //    **重试原请求**（原 method/body/headers + 新 cookie——POST 场景关键：浏览器求解
    //    只会 GET 首页，重试才能让 POST（如 69shuba search.php 搜索）拿到真实结果）；
    //    重试仍质询/失败 → 用求解结果（浏览器 HTML）兜底返回
    let needs_solve = browser_first_get
        || is_cloudflare_challenge(resp.status, &resp.body)
        || (browser_fallback_enabled() && looks_like_anti_bot(resp.status, &resp.body));
    if needs_solve {
        tracing::debug!("http_fetch 命中 CF 质询 status={} url={url}", resp.status);
        // 调试/日志：质询页 Turnstile sitekey（纯 Rust 解析预检——与浏览器内提取镜像）
        if let Some(sk) = crate::service::browser::extract_turnstile_sitekey(&resp.body) {
            tracing::debug!("CF 质询页含 Turnstile sitekey={sk}（{url}）");
        }
        // 求解：返回兜底响应 + 合并后 cookie 串（内存直传重试——不依赖 storage 注册状态/
        // 并发覆盖）+ 浏览器 UA。浏览器优先模式下求解失败降级直连（默认浏览器优先不能
        // 因为某站点 WAF/浏览器异常就让所有请求失败）；非优先模式保持原“失败即报错”。
        let solved_result: Result<(FetchResponse, Option<String>, String)> = async {
            if let Some(fs) =
                flaresolverr_request(&url, &cookie, method, body.as_deref(), timeout_secs).await?
            {
                // FS 解成功：cookie 与用户原 cookie 按 name 合并后存库（按用户）+ UA 记录
                let fs_pairs: Vec<(String, String)> = fs
                    .cookies
                    .iter()
                    .map(|c| (c.name.clone(), c.value.clone()))
                    .collect();
                let merged =
                    store_solution_session(ns, &url, &cookie, &fs_pairs, &fs.user_agent, None)
                        .await;
                Ok((
                    FetchResponse {
                        body: fs.response,
                        url: if fs.url.is_empty() {
                            url.to_string()
                        } else {
                            fs.url
                        },
                        headers: Vec::new(),
                        status: fs.status,
                    },
                    merged,
                    fs.user_agent,
                ))
            } else {
                // 未配置 FLARESOLVERR_URL → 内置浏览器求解（进程内 CDP，不依赖外部容器；
                // 带书源级代理 proxy——obscura spawn --proxy）
                let solved = solve_cf_builtin(ns, &url, &cookie, proxy).await?;
                if cf_browser_available() {
                    mark_browser_needed(&url);
                }
                Ok(solved)
            }
        }
        .await;
        let (fallback, merged_cookie, solved_ua) = match solved_result {
            Ok(v) => v,
            Err(e) if browser_first_get => {
                log_solve_failure_cooled(&url, &e);
                // 冷却期内 debug 静默降级——避免服务未就绪时每请求刷屏
                return match fetch(
                    &url,
                    &req_headers,
                    timeout_secs,
                    method,
                    body.as_deref(),
                    charset,
                    proxy,
                )
                .await
                {
                    Ok(r) => Ok(r),
                    Err(fetch_err) => Err(fetch_err),
                };
            }
            Err(e) => return Err(e),
        };

        // ④ 重试原请求（原 method/body/headers + 求解后的 cookie——POST 场景关键：
        //    浏览器求解只会 GET 首页，重试才能让 POST（如 69shuba search.php）拿到真实
        //    结果）：优先用内存中的合并 cookie（含 cf_clearance）；无合并结果时回退读库
        let mut retry_cookie = merged_cookie.clone().unwrap_or_default();
        if retry_cookie.is_empty() {
            retry_cookie = session_for(ns, &url).await.unwrap_or_default().0;
        }
        let mut retry_headers = headers.clone();
        if !retry_cookie.is_empty() {
            retry_headers.insert("Cookie".to_string(), retry_cookie);
        }
        if !solved_ua.is_empty()
            && !retry_headers.contains_key("User-Agent")
            && !retry_headers.contains_key("user-agent")
        {
            retry_headers.insert("User-Agent".to_string(), solved_ua);
        }
        // 重试同样带上登录头（CF 求解后的请求保持登录态）
        if let Some(login_header) = login_header_for(ns, &url).await {
            merge_login_header(&mut retry_headers, &login_header);
        }
        if let Ok(retry) = fetch(
            &url,
            &retry_headers,
            timeout_secs,
            method,
            body.as_deref(),
            charset,
            proxy,
        )
        .await
        {
            if !is_cloudflare_challenge(retry.status, &retry.body) {
                return Ok(retry); // 重试拿到真实内容（GET/POST 通用）
            }
            // 重试仍命中质询（cf_clearance 未生效/新质询）→ 兜底用求解结果
        }
        return Ok(fallback);
    }
    Ok(resp)
}

/// 内置浏览器兜底开关（默认开启：直连失败/反爬特征时自动浏览器导航重试；
/// `READER_BROWSER_FALLBACK_DISABLE=1` 关闭；浏览器优先关闭后此开关仍按反爬特征兜底）
fn browser_fallback_enabled() -> bool {
    std::env::var("READER_BROWSER_FALLBACK_DISABLE")
        .map(|v| v.trim() != "1")
        .unwrap_or(true)
}

/// 求解失败日志熔断：60 秒内同源失败只 warn 一次，其余降为 debug
/// （真实环境教训：camoufox 服务不可用时每个请求一条 WARN 刷屏数千行）
static SOLVE_FAIL_COOLDOWN_UNTIL: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

fn log_solve_failure_cooled(url: &str, e: &dyn std::fmt::Display) {
    use std::sync::atomic::Ordering;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let until = SOLVE_FAIL_COOLDOWN_UNTIL.load(Ordering::Relaxed);
    if now >= until {
        // 首条/冷却结束：warn 并续期 60s
        SOLVE_FAIL_COOLDOWN_UNTIL.store(now + 60_000, Ordering::Relaxed);
        tracing::warn!("浏览器求解失败（{url}），60s 内降级直连并静默: {e:#}");
    } else {
        tracing::debug!("浏览器求解失败（{url}）[冷却静默]: {e:#}");
    }
}

/// 浏览器优先模式：默认开启（`READER_BROWSER_FIRST` 未设置时所有 GET 先经内置
/// obscura 浏览器导航，最大限度减少验证码/WAF 拦截；代价是速度与资源占用）。
/// 显式 `READER_BROWSER_FIRST=0`/`false`/`off` 可关闭，恢复“直连优先、反爬兜底”。
/// 浏览器不可用或求解失败时自动降级直连，不会因缺浏览器导致抓取全部失败。
fn browser_first_enabled() -> bool {
    std::env::var("READER_BROWSER_FIRST")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            !matches!(v.as_str(), "" | "0" | "false" | "off" | "no")
        })
        .unwrap_or(true)
}

/// 浏览器求解后端是否可用：camoufox 启用即可（服务未启动时首次求解自动拉起）。
/// 不可用时浏览器优先自动回退直连。
fn browser_solver_available() -> bool {
    crate::service::browser::is_browser_available()
}

/// 反爬域名自动优先缓存：内置浏览器成功解过一次质询后，该域名 30 分钟内 GET 直接
/// 优先浏览器导航，减少验证码重复出现；普通站点完全不受影响。
const BROWSER_NEEDED_TTL: Duration = Duration::from_secs(30 * 60);

static BROWSER_NEEDED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

fn host_of_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_else(|| url.to_string())
}

fn mark_browser_needed(url: &str) {
    let host = host_of_url(url);
    let mut map = BROWSER_NEEDED.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(host, Instant::now());
    if map.len() > 256 {
        map.retain(|_, at| at.elapsed() < BROWSER_NEEDED_TTL);
    }
}

fn browser_needed(url: &str) -> bool {
    let host = host_of_url(url);
    let map = BROWSER_NEEDED.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&host)
        .map_or(false, |at| at.elapsed() < BROWSER_NEEDED_TTL)
}

/// 直连失败是否值得浏览器兜底：仅网络层错误（超时/连接中断/TLS/DNS 解析），
/// HTTP 业务错误（404 等）不启动浏览器
fn should_browser_rescue_error(e: &anyhow::Error) -> bool {
    let lower = e.to_string().to_ascii_lowercase();
    [
        "operation timed out",
        "timed out",
        "connection closed",
        "connection reset",
        "connection refused",
        "send request",
        "client error",
        "tls",
        "dns",
        "ssl",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

/// 反爬/验证码页面特征（200/403/429 等——非 CF 专有：人机验证、JS 挑战、WAF 拦截页）。
/// 命中后走浏览器求解链（等待 JS challenge/Turnstile → cookie 合并 → 重试原请求）
fn looks_like_anti_bot(status: u16, body: &str) -> bool {
    let lower = body.to_lowercase();
    let hints = [
        "人机验证",
        "安全验证",
        "访问验证",
        "验证码",
        "滑动验证",
        "拖动验证",
        "请完成验证",
        "checking your browser",
        "verify you are human",
        "verify you're human",
        "attention required",
        "captcha",
        "challenge",
        "managed challenge",
        "enable javascript and cookies to continue",
        "browser check",
        "geetest",
        "hcaptcha",
        "recaptcha",
        "tencent captcha",
        "qcloud captcha",
        "滑块验证",
        "拖动滑块",
        "安全组件",
        "环境检测",
        "浏览器环境",
        "__jsl_clearance",
        "acl.qq.com",
        "sec-captcha",
        "anti-bot",
        "antibot",
    ];
    if hints.iter().any(|h| lower.contains(h)) {
        return true;
    }
    // 403/429 的短响应（WAF 拦截页/JS 挑战）也兜底浏览器
    (status == 403 || status == 429) && body.len() < 4096
}

/// 请求 URL → 书源 source_url（cookie 存储键；按 base 匹配）。
/// 无既有 cookie 行时回退用请求 baseUrl 作为键（get_cookie_by_base 按 base 命中，不影响查找）。
async fn resolve_source_url(ns: &str, url: &str) -> Option<String> {
    let base = base_url_of(url)?;
    let Some(storage) = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return Some(base);
    };
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT source_url FROM book_source_cookies WHERE user_namespace = ?1")
            .bind(ns)
            .fetch_all(&storage.pool)
            .await
            .ok()?;
    let target = crate::storage::normalize_base(&base)?;
    rows.into_iter()
        .find(|su| {
            su.split("##")
                .any(|part| crate::storage::normalize_base(part) == Some(target.clone()))
        })
        .or(Some(base))
}

// ==================== Cloudflare 质询检测 ====================

/// Cloudflare 质询特征检测（503/403 + HTML 特征；未命中返回 false——零开销直连）
pub fn is_cloudflare_challenge(status: u16, body: &str) -> bool {
    if status != 503 && status != 403 {
        return false;
    }
    let body = body.to_lowercase();
    [
        "cf-browser-gesture",
        "challenge-platform",
        "__cf_chl",
        "cf-chl-",
        "just a moment",
        "cf_chl_opt",
        "challenge-running",
        // Turnstile 验证码特征（challenges.cloudflare.com/turnstile 资源、.cf-turnstile
        // 容器、turnstile/api.js 脚本）
        "challenges.cloudflare.com/turnstile",
        "cf-turnstile",
        "turnstile/api.js",
    ]
    .iter()
    .any(|m| body.contains(m))
}

// ==================== FlareSolverr（CF 质询解） ====================

/// FS 返回的 cookie（数组项：name/value/domain/path/...）
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct FsCookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// FS 解结果
pub struct FsSolution {
    pub response: String,
    pub cookies: Vec<FsCookie>,
    pub user_agent: String,
    /// FS 返回的最终 URL（CF 重定向后；空则回退请求 URL）
    pub url: String,
    /// FS 返回的最终 HTTP 状态（缺省 200）
    pub status: u16,
}

/// FlareSolverr 请求配置（环境变量 FLARESOLVERR_URL，默认空 = 禁用）
pub fn flaresolverr_base() -> Option<String> {
    let v = std::env::var("FLARESOLVERR_URL")
        .unwrap_or_default()
        .trim()
        .to_string();
    if v.is_empty() {
        None
    } else {
        Some(v.trim_end_matches('/').to_string())
    }
}

/// 请求 FlareSolverr：`POST {base}/v1`（cmd=request.get，带书源 cookie 数组保持会话连续性）。
/// - 未配置 FLARESOLVERR_URL → Ok(None)（降级直连结果）
/// - FS 错误/超时（60s）→ Err（明确报错，含 FS 地址提示）
pub async fn flaresolverr_request(
    url: &str,
    cookie: &str,
    method: &str,
    body: Option<&str>,
    _timeout_secs: u64,
) -> Result<Option<FsSolution>> {
    let Some(base) = flaresolverr_base() else {
        return Ok(None);
    };
    // 用户 cookie（"a=1; b=2"）→ FS cookies 数组（name/value/domain/path）
    let cookies: Vec<serde_json::Value> = parse_cookie_string(cookie)
        .into_iter()
        .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
        .collect();
    let mut payload = serde_json::json!({
        "cmd": "request.get",
        "url": url,
        "maxTimeout": 60000,
        "cookies": cookies,
    });
    if method.eq_ignore_ascii_case("POST") {
        payload["cmd"] = serde_json::json!("request.post");
        if let Some(b) = body {
            payload["postData"] = serde_json::json!(b);
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let resp = client
        .post(format!("{base}/v1"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| anyhow!("FlareSolverr 请求失败（{base}）: {e}"))?;
    let status = resp.status().as_u16();
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("FlareSolverr 响应解析失败（{base}）: {e}"))?;
    if json.get("status").and_then(|s| s.as_str()) != Some("ok") {
        let msg = json
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        return Err(anyhow!(
            "FlareSolverr 解质询失败（{base}，HTTP {status}）: {msg}"
        ));
    }
    let solution = json.get("solution").cloned().unwrap_or_default();
    let response = solution
        .get("response")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    let user_agent = solution
        .get("userAgent")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let final_url = solution
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let status = solution
        .get("status")
        .and_then(|s| s.as_u64())
        .unwrap_or(200)
        .min(u16::MAX as u64) as u16;
    let cookies: Vec<FsCookie> =
        serde_json::from_value(solution.get("cookies").cloned().unwrap_or_default())
            .unwrap_or_default();
    Ok(Some(FsSolution {
        response,
        cookies,
        user_agent,
        url: final_url,
        status,
    }))
}

// ==================== 内置浏览器 CF 质询求解（进程内 CDP） ====================

/// 浏览器可用性探测（CF 内置求解前置检查；测试钩子可强制覆盖）
fn cf_browser_available() -> bool {
    #[cfg(test)]
    {
        use std::sync::atomic::Ordering;
        match CF_BROWSER_AVAIL_OVERRIDE.load(Ordering::Relaxed) {
            1 => return true,
            -1 => return false,
            _ => {}
        }
    }
    crate::service::browser::is_browser_available()
}

/// 测试钩子：强制浏览器可用性（Some(true)/Some(false) 强制；None 恢复自动探测）
#[cfg(test)]
pub(crate) fn force_cf_browser_available(v: Option<bool>) {
    use std::sync::atomic::Ordering;
    CF_BROWSER_AVAIL_OVERRIDE.store(
        match v {
            Some(true) => 1,
            Some(false) => -1,
            None => 0,
        },
        Ordering::Relaxed,
    );
}

#[cfg(test)]
static CF_BROWSER_AVAIL_OVERRIDE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(0);

/// CF 质询求解（camoufox 后端——唯一浏览器后端）：
/// - 浏览器不可用 → 明确错误（提示配置 camoufox）
/// - 成功：solution.html 作为响应正文；cookies 与用户 cookie 按 name 合并后存库（按用户）
///   + 浏览器 UA 记录（与 cf_clearance 绑定）
/// proxy：书源级代理（None = READER_CAMOUFOX_PROXY）——机房 IP 解 Turnstile 需住宅代理
/// 返回 (兜底响应, 合并后 cookie 串（含 turnstile_token 伪 cookie——重试直接用）, 浏览器 UA)
async fn solve_cf_builtin(
    ns: &str,
    url: &str,
    user_cookie: &str,
    proxy: Option<&str>,
) -> Result<(FetchResponse, Option<String>, String)> {
    let cookies = parse_cookie_string(user_cookie);
    let solution =
        crate::service::browser::solve_cf_challenge(ns, url, &cookies, CF_SOLVE_MAX_WAIT_MS, proxy)
            .await
            .map_err(|e| anyhow!("解 CF 质询失败（{url}）: {e:#}"))?;
    if let Some(sk) = &solution.turnstile_sitekey {
        tracing::info!("Turnstile 求解命中 sitekey={sk}（{url}）");
    }
    let merged = store_solution_session(
        ns,
        url,
        user_cookie,
        &solution.cookies,
        &solution.user_agent,
        solution.turnstile_token.as_deref(),
    )
    .await;
    Ok((
        FetchResponse {
            body: solution.html,
            url: url.to_string(),
            headers: Vec::new(),
            status: 200,
        },
        merged,
        solution.user_agent,
    ))
}

/// 解质询成功后持久化（按用户）：cookies 与用户原 cookie 按 name 合并存库 + UA 记录 +
/// Turnstile token 随 cookie 串存（书源级按用户）。返回合并后的 cookie 串
/// （Some——调用方重试原请求直接用；None = 无新信息）。存储失败仅告警（不影响响应）。
async fn store_solution_session(
    ns: &str,
    url: &str,
    user_cookie: &str,
    solution_cookies: &[(String, String)],
    user_agent: &str,
    turnstile_token: Option<&str>,
) -> Option<String> {
    if solution_cookies.is_empty() && user_agent.is_empty() && turnstile_token.is_none() {
        return None;
    }
    let fs_cookies: Vec<FsCookie> = solution_cookies
        .iter()
        .map(|(n, v)| FsCookie {
            name: n.clone(),
            value: v.clone(),
            domain: None,
            path: None,
        })
        .collect();
    let mut merged = merge_fs_cookies(user_cookie, &fs_cookies);
    // Turnstile token 随 cookie 串存库（书源级按用户）——选择随 cookie 串而非新增表列：
    // book_source_cookies 已按 (user_namespace, source_url) 隔离，伪 cookie 名
    // cf_turnstile_token 不会与真实 cookie 冲突（服务端忽略未知 cookie）；token 短时效
    // （约 5 分钟、单次有效）主要作求解记录，下次求解按 name 覆盖刷新。
    if let Some(token) = turnstile_token.filter(|t| !t.trim().is_empty()) {
        merged = merge_turnstile_token(&merged, token);
    }
    // 注意：先解引用再 clone 出 Storage（句柄 Clone 廉价）——MutexGuard 不能跨 await 存活
    // （非 Send——router 的 tokio::spawn 会因此编译失败）
    let storage_opt: Option<crate::storage::Storage> = COOKIE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(storage) = storage_opt {
        if let Some(su) = resolve_source_url(ns, url).await {
            if !merged.is_empty() {
                let _ = storage.set_cookie(ns, &su, &merged).await;
            }
            // UA 与库中不同则一并记录（部分站点 UA 绑定 cookie）
            if !user_agent.is_empty() {
                let need_update = match storage.get_source_session(ns, &su).await {
                    Ok(Some((_, old_ua))) => old_ua != user_agent,
                    _ => true,
                };
                if need_update {
                    let _ = storage.set_cookie_user_agent(ns, &su, user_agent).await;
                }
            }
        }
    }
    Some(merged)
}

// ==================== cookie 工具（合并策略见下） ====================

/// 解析 "a=1; b=2" cookie 串 → (name, value) 对（跳过空/损坏项）
pub fn parse_cookie_string(cookie: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in cookie.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if !k.is_empty() && !v.is_empty() {
                out.push((k.to_string(), v.to_string()));
            }
        }
    }
    out
}

/// 合并 FlareSolverr cookie 与用户原 cookie（**按 name 合并**）：
/// - 同名：FS 值覆盖用户值（cf_clearance 等质询 cookie 以 FS 为准）
/// - 不同名：保留用户值
/// - 顺序：按用户原 cookie 顺序为基底，FS 新增 name 依次追加（顺序稳定）
/// - 序列化 "a=1; b=2; cf_clearance=..." 存库（按用户）
pub fn merge_fs_cookies(user_cookie: &str, fs_cookies: &[FsCookie]) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, String> = HashMap::new();
    for (k, v) in parse_cookie_string(user_cookie) {
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.insert(k, v);
    }
    for c in fs_cookies {
        if c.name.is_empty() {
            continue;
        }
        if !map.contains_key(&c.name) {
            order.push(c.name.clone());
        }
        map.insert(c.name.clone(), c.value.clone());
    }
    order
        .into_iter()
        .filter_map(|k| map.get(&k).map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 将 Turnstile token 作为伪 cookie（cf_turnstile_token）并入 cookie 串：
/// 同名（上次求解残留）按 name 覆盖——token 单次有效，新求解必然刷新。
pub fn merge_turnstile_token(cookie_str: &str, token: &str) -> String {
    let mut pairs: Vec<(String, String)> = parse_cookie_string(cookie_str)
        .into_iter()
        .filter(|(k, _)| k != "cf_turnstile_token")
        .collect();
    pairs.push(("cf_turnstile_token".to_string(), token.to_string()));
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 解析书源 header 字段（legacy：`<js>` 模板先执行（返回 JSON）→ JSON 字符串或 key=value 行）
pub fn parse_header(header: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut header = header.trim().to_string();
    if header.is_empty() {
        return map;
    }
    // `<js>...</js>` 模板：执行 JS（注入 key/page/baseUrl/result 空串/headerMap）→ 返回 JSON 字符串
    if header.starts_with("<js>") || header.starts_with("js:") || header.starts_with("@js:") {
        let code = header
            .trim_start_matches("<js>")
            .trim_end_matches("</js>")
            .trim_start_matches("js:")
            .trim_start_matches("@js:")
            .trim()
            .to_string();
        let vars = std::collections::HashMap::from([
            ("key".to_string(), String::new()),
            ("page".to_string(), "1".to_string()),
            ("baseUrl".to_string(), String::new()),
            ("result".to_string(), String::new()),
            ("headerMap".to_string(), "{}".to_string()),
        ]);
        match crate::parser::js::eval_js(&code, &vars) {
            Ok(json) => header = json,
            Err(e) => {
                tracing::warn!("书源 header JS 执行失败: {e}");
                return map;
            }
        }
    }
    let header = header.as_str();
    // 尝试 JSON（兼容单引号 JSON：'key': 'value' → 标准 JSON）
    if header.starts_with('{') {
        let normalized = if header.contains('\'') && !header.contains('"') {
            header.replace('\'', "\"")
        } else {
            header.to_string()
        };
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&normalized) {
            if let Some(obj) = v.as_object() {
                for (k, val) in obj {
                    if let Some(s) = val.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        map.insert(k.clone(), val.to_string());
                    }
                }
                return map;
            }
        }
    }
    // key=value 行
    for line in header.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// 合并书源登录头（legacy `getHeaderMap(true)` 语义：登录头覆盖源 header 同名键）
fn merge_login_header(headers: &mut HashMap<String, String>, login_header: &str) {
    for (k, v) in parse_header(login_header) {
        headers.insert(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// E6：cookie 域键 = legacy getSubDomain（host[:port] 去最左标签）
    #[test]
    fn test_cookie_subdomain() {
        assert_eq!(
            cookie_subdomain("https://www.example.com/a?b=1"),
            "example.com"
        );
        assert_eq!(
            cookie_subdomain("http://api.example.com:8080/x"),
            "example.com:8080"
        );
        assert_eq!(
            cookie_subdomain("https://user@sub.a.com/path"),
            "a.com",
            "userinfo 剥离"
        );
        // 单标签（无点）原样
        assert_eq!(
            cookie_subdomain("http://localhost:3000/x"),
            "localhost:3000"
        );
        assert_eq!(cookie_subdomain("https://example.com"), "example.com");
        // IP：多段 → 去首段（legacy 同款怪癖，保持一致）
        assert_eq!(cookie_subdomain("http://192.168.1.1:80/"), "168.1.1:80");
        // 无 scheme 容忍
        assert_eq!(cookie_subdomain("www.example.com/c"), "example.com");
    }

    /// E4：显式 Cookie 头与存储 cookie 逐键合并（stored 为底、explicit 覆盖）
    #[test]
    fn test_merge_cookie_strings() {
        assert_eq!(
            merge_cookie_strings("a=1; b=2", "b=9; c=3"),
            "a=1; b=9; c=3"
        );
        assert_eq!(merge_cookie_strings("", "x=1"), "x=1");
        assert_eq!(merge_cookie_strings("x=1", ""), "x=1");
        // 值中含 '=' 的边界
        assert_eq!(merge_cookie_strings("t=aa==", "u=bb=="), "t=aa==; u=bb==");
    }

    /// E5：Set-Cookie 提取（仅取首个 name=value 对；忽略属性与 Expires）
    #[test]
    fn test_extract_set_cookie_pairs() {
        let headers = vec![
            (
                "set-cookie".to_string(),
                "sid=abc123; Path=/; HttpOnly".to_string(),
            ),
            ("Set-Cookie".to_string(), "expires stuff".to_string()),
            (
                "Set-Cookie".to_string(),
                "Expires=Wed, 21 Oct 2025 07:28:00 GMT; Path=/".to_string(),
            ),
            ("Content-Type".to_string(), "text/html".to_string()),
        ];
        let pairs = extract_set_cookie_pairs(&headers);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("sid".to_string(), "abc123".to_string()));
    }

    /// 69shuba 搜索 POST（真实链路复现 builder error——网络不可达时跳过）
    #[tokio::test]
    async fn fetch_69shuba_post() {
        use std::collections::HashMap;
        let mut h = HashMap::new();
        h.insert(
            "User-Agent".to_string(),
            "Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 Chrome/125.0 Mobile Safari/537.36"
                .to_string(),
        );
        h.insert(
            "Referer".to_string(),
            "https://www.69shuba.com/".to_string(),
        );
        h.insert(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded; charset=gbk".to_string(),
        );
        let url = "https://www.69shuba.com/modules/article/search.php";
        let body = "searchkey=%E8%AF%A1%E7%A7%98%E4%B9%8B%E4%B8%BB&searchtype=all&page=1";
        match fetch(url, &h, 30, "POST", Some(body), Some("gbk"), None).await {
            Ok(r) => println!("OK status={} len={}", r.status, r.body.len()),
            Err(e) => println!("ERR: {e:?} source={:?}", e.source().map(|s| s.to_string())),
        }
    }

    #[test]
    fn test_base_url_of() {
        assert_eq!(
            base_url_of("https://a.com/book/1?x=2").as_deref(),
            Some("https://a.com")
        );
        assert_eq!(
            base_url_of("https://a.com:8443/x").as_deref(),
            Some("https://a.com:8443")
        );
        assert_eq!(base_url_of("http://a.com").as_deref(), Some("http://a.com"));
        assert_eq!(base_url_of("not a url"), None);
    }

    /// HTML meta charset：GBK 页面无显式 charset 参数时自动探测解码
    #[test]
    fn test_decode_bytes_meta_charset_gbk() {
        let (gbk, _, _) = encoding_rs::GBK.encode("第一章 内容");
        let mut bytes = b"<html><head><meta charset=\"gbk\"></head><body>".to_vec();
        bytes.extend_from_slice(&gbk);
        bytes.extend_from_slice(b"</body></html>");
        let text = decode_bytes(&bytes, None);
        assert!(text.contains("第一章 内容"), "meta gbk 应解码中文: {text}");
    }

    /// http-equiv Content-Type 声明 charset
    #[test]
    fn test_decode_bytes_meta_http_equiv() {
        let (gbk, _, _) = encoding_rs::GBK.encode("第二卷");
        let mut bytes =
            br#"<meta http-equiv="Content-Type" content="text/html; charset=GB2312">"#.to_vec();
        bytes.extend_from_slice(&gbk);
        let text = decode_bytes(&bytes, None);
        assert!(text.contains("第二卷"), "http-equiv charset 应生效: {text}");
    }

    /// UTF-8 BOM 与纯 UTF-8 正常解码
    #[test]
    fn test_decode_bytes_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("中文内容".as_bytes());
        assert_eq!(decode_bytes(&bytes, None), "中文内容");
        assert_eq!(decode_bytes("纯UTF8".as_bytes(), None), "纯UTF8");
    }

    /// 无 meta 的纯 GBK 中文页：UTF-8 有替换字符时启发式回退 GBK
    #[test]
    fn test_decode_bytes_gbk_heuristic() {
        let (gbk, _, _) = encoding_rs::GBK.encode("这是一段没有meta声明的GBK中文");
        let text = decode_bytes(&gbk, None);
        assert!(
            text.contains("这是") && text.contains("中文"),
            "GBK 启发式应解码: {text}"
        );
    }

    /// 统计式探测：无 meta 的 Big5 页面应解码为繁体中文（不落入 GBK 启发式）
    #[test]
    fn test_decode_bytes_big5_statistical() {
        let (big5, _, _) = encoding_rs::BIG5.encode(
            "這是一段沒有 meta 聲明的繁體中文內容，用來驗證統計式編碼偵測可以正確辨識 Big5。",
        );
        let text = decode_bytes(&big5, None);
        assert!(
            text.contains("繁體中文內容"),
            "Big5 应经统计探测解码: {text}"
        );
    }

    /// Content-Type charset 提取（fetch 的 HTTP 头优先级）
    #[test]
    fn test_content_type_charset_extract() {
        let headers = vec![
            (
                "content-type".to_string(),
                "text/html; charset=gbk".to_string(),
            ),
            ("x-other".to_string(), "text/plain".to_string()),
        ];
        assert_eq!(content_type_charset(&headers).as_deref(), Some("gbk"));
        let headers = vec![(
            "content-type".to_string(),
            "text/html; charset=\"UTF-8\"".to_string(),
        )];
        assert_eq!(content_type_charset(&headers).as_deref(), Some("UTF-8"));
        assert!(content_type_charset(&[]).is_none());
    }

    /// meta 声明提取：单/双引号与无引号形式
    #[test]
    fn test_html_meta_charset_forms() {
        assert_eq!(
            html_meta_charset(r#"<meta charset='gb2312'>"#).as_deref(),
            Some("gb2312")
        );
        assert_eq!(
            html_meta_charset(r#"<meta charset = utf-8 >"#).as_deref(),
            Some("utf-8")
        );
        assert_eq!(
            html_meta_charset(
                r#"<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=GBK">"#
            )
            .as_deref(),
            Some("gbk")
        );
        assert_eq!(html_meta_charset("<html><head></head></html>"), None);
    }

    /// fetch：Content-Type charset 覆盖 HTML meta（HTTP 头优先级更高）
    #[tokio::test]
    async fn test_fetch_content_type_charset_wins() {
        let _ssrf = ssrf_allow_private_guard(true);
        let (gbk, _, _) = encoding_rs::GBK.encode("正文甲");
        let mut body = b"<html><meta charset=\"utf-8\">".to_vec();
        body.extend_from_slice(&gbk);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=gbk\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut out = resp.into_bytes();
            out.extend_from_slice(&body);
            let _ = sock.write_all(&out).await;
        });
        let resp = fetch(
            &format!("http://{addr}/x"),
            &HashMap::new(),
            10,
            "GET",
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            resp.body.contains("正文甲"),
            "HTTP charset 应优先于 meta: {}",
            resp.body
        );
    }

    /// 传输层失败自动重试：前两次连接中断、第三次返回 200（默认重试 2 次）
    #[tokio::test]
    async fn test_fetch_retries_transient_connection_error() {
        let _ssrf = ssrf_allow_private_guard(true);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for attempt in 0..3 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                if attempt < 2 {
                    // 模拟连接中断（半响应后关闭 → reqwest EOF）
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nha")
                        .await;
                    drop(sock);
                } else {
                    let body = "ok";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                }
            }
        });
        let resp = fetch(
            &format!("http://{addr}/retry"),
            &HashMap::new(),
            10,
            "GET",
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "ok", "重试后应返回成功响应");
    }

    /// 可重试错误识别：超时/连接中断/TLS 等传输层错误重试，HTTP 状态错误不重试
    #[test]
    fn test_retryable_http_error() {
        assert!(retryable_http_error(&anyhow!("operation timed out")));
        assert!(retryable_http_error(&anyhow!(
            "error sending request for url: connection closed before message completed"
        )));
        assert!(retryable_http_error(&anyhow!(
            "error sending request: tls handshake eof"
        )));
        assert!(!retryable_http_error(&anyhow!(
            "HTTP status client error (500 Internal Server Error) for url"
        )));
    }

    #[test]
    fn test_cookie_for_unregistered_is_none() {
        clear_cookie_storage();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r = rt.block_on(cookie_for("default", "https://a.com/x"));
        assert!(r.is_none());
    }

    #[test]
    fn test_is_cloudflare_challenge() {
        // 503 + 特征 HTML → true
        assert!(is_cloudflare_challenge(
            503,
            "<html>Just a moment...<script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/jsch/v1\"></script>"
        ));
        assert!(is_cloudflare_challenge(
            503,
            "__cf_chl_opt_tKb6Qe=...; cf-browser-gesture"
        ));
        // 403 + 特征（69shuba 等强质询）→ true
        assert!(is_cloudflare_challenge(
            403,
            "<title>Just a moment...</title> challenge-platform"
        ));
        // Turnstile 特征（challenges.cloudflare.com/turnstile、cf-turnstile、turnstile/api.js）
        assert!(is_cloudflare_challenge(
            503,
            "<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\"></script>"
        ));
        assert!(is_cloudflare_challenge(
            503,
            "<div class=\"cf-turnstile\" data-sitekey=\"0x4AAAAAAA\"></div>"
        ));
        assert!(is_cloudflare_challenge(403, "turnstile/api.js"));
        // 非 503/403 → false（即使含特征）
        assert!(!is_cloudflare_challenge(200, "Just a moment"));
        // 503 无特征 → false（零开销直连路径）
        assert!(!is_cloudflare_challenge(503, "<html>maintenance</html>"));
        assert!(!is_cloudflare_challenge(404, "challenge-platform"));
    }

    #[test]
    fn test_parse_cookie_string() {
        assert_eq!(
            parse_cookie_string("a=1; b=2"),
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
        assert_eq!(
            parse_cookie_string("a=1;; b="),
            vec![("a".into(), "1".into())]
        );
        assert_eq!(parse_cookie_string(""), Vec::<(String, String)>::new());
    }

    /// 登录头合并：JSON map 解析 + 覆盖源 header 同名键（legacy getHeaderMap(true)）
    #[test]
    fn test_merge_login_header() {
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), "src-ua".to_string());
        headers.insert("X-Token".to_string(), "old".to_string());
        merge_login_header(
            &mut headers,
            r#"{"X-Token":"tok-1","X-Auth":"alice","User-Agent":"login-ua"}"#,
        );
        assert_eq!(headers.get("X-Token").map(String::as_str), Some("tok-1"));
        assert_eq!(headers.get("X-Auth").map(String::as_str), Some("alice"));
        assert_eq!(
            headers.get("User-Agent").map(String::as_str),
            Some("login-ua"),
            "登录头应覆盖源 header 同名键"
        );
        // 空/非 JSON 不破坏原头
        merge_login_header(&mut headers, "");
        assert_eq!(headers.get("X-Token").map(String::as_str), Some("tok-1"));
        merge_login_header(&mut headers, "X-Plain=1");
        assert_eq!(headers.get("X-Plain").map(String::as_str), Some("1"));
    }

    /// 合并策略：同名 FS 覆盖、不同名保留用户值、顺序稳定（用户序为基底 + FS 新名追加）
    #[test]
    fn test_merge_fs_cookies() {
        let user = "sid=abc; theme=dark";
        let fs = vec![
            FsCookie {
                name: "cf_clearance".into(),
                value: "xyz".into(),
                domain: None,
                path: None,
            },
            FsCookie {
                name: "theme".into(),
                value: "light".into(),
                domain: None,
                path: None,
            },
        ];
        let merged = merge_fs_cookies(user, &fs);
        assert_eq!(merged, "sid=abc; theme=light; cf_clearance=xyz");
    }

    #[test]
    fn test_merge_fs_cookies_empty_user() {
        let fs = vec![FsCookie {
            name: "cf_clearance".into(),
            value: "xyz".into(),
            domain: None,
            path: None,
        }];
        assert_eq!(merge_fs_cookies("", &fs), "cf_clearance=xyz");
        assert_eq!(merge_fs_cookies("a=1", &[]), "a=1");
        assert_eq!(merge_fs_cookies("", &[]), "");
    }

    /// Turnstile token 伪 cookie 合并：追加 / 空串 / 同名覆盖（上次求解残留）
    #[test]
    fn test_merge_turnstile_token() {
        assert_eq!(
            merge_turnstile_token("sid=abc", "tok-1"),
            "sid=abc; cf_turnstile_token=tok-1"
        );
        assert_eq!(
            merge_turnstile_token("", "tok-1"),
            "cf_turnstile_token=tok-1"
        );
        assert_eq!(
            merge_turnstile_token("sid=abc; cf_turnstile_token=old", "new"),
            "sid=abc; cf_turnstile_token=new"
        );
    }

    #[test]
    fn test_flaresolverr_disabled_by_default() {
        // 未配置 FLARESOLVERR_URL → Ok(None)（降级直连，不影响现有路径）
        std::env::remove_var("FLARESOLVERR_URL");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r = rt.block_on(flaresolverr_request(
            "https://a.com",
            "a=1",
            "GET",
            None,
            15,
        ));
        assert!(r.is_ok());
        assert!(r.unwrap().is_none());
    }

    /// 浏览器不可用分支单测：solve_cf_builtin 返回明确错误（不启动浏览器、不发请求）；
    /// camoufox 禁用时 solve_cf_builtin 返回明确错误（不发起任何 HTTP 调用）
    #[test]
    fn test_cf_builtin_browser_unavailable_returns_clear_error() {
        std::env::set_var("READER_CAMOUFOX_DISABLE", "1");
        force_cf_browser_available(Some(false));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r = rt.block_on(solve_cf_builtin(
            "default",
            "https://cf.example.com/book/1",
            "sid=abc",
            None,
        ));
        force_cf_browser_available(None);
        std::env::remove_var("READER_CAMOUFOX_DISABLE");
        let err = r.err().expect("浏览器不可用应返回错误");
        assert!(
            err.to_string().contains("camoufox") || err.to_string().contains("禁用"),
            "错误应提示 camoufox 未启用: {err}"
        );
    }

    /// 微型 HTTP 服务器：返回固定状态/Content-Type/二进制体；可记录收到的请求头
    async fn serve_image(
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
        captured: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for _ in 0..10 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                captured.lock().unwrap().push(req);
                let reason = if status == 200 { "OK" } else { "ERR" };
                let head = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut resp = head.into_bytes();
                resp.extend_from_slice(&body);
                let _ = sock.write_all(&resp).await;
            }
        });
        format!("http://{addr}")
    }

    /// GAP #88/125：fetch_image——二进制透传 + Content-Type + Referer/书源 cookie 附加
    #[tokio::test]
    async fn test_fetch_image_binary_and_headers() {
        let _ssrf = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
        clear_cookie_storage();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let png = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3, 4];
        let url = serve_image(200, "image/png", png.clone(), captured.clone()).await;

        let (bytes, content_type, status) = fetch_image(
            "default",
            &url,
            Some("https://src.com/book/1"),
            10,
            5 * 1024 * 1024,
        )
        .await
        .unwrap();
        assert_eq!(bytes, png, "图片字节应原样透传");
        assert_eq!(
            content_type.as_deref(),
            Some("image/png"),
            "Content-Type 透传"
        );
        assert_eq!(status, 200);
        let req = captured.lock().unwrap()[0].clone();
        assert!(
            req.to_lowercase()
                .contains("referer: https://src.com/book/1"),
            "应携带 Referer（防盗链绕过）: {req}"
        );

        // 非 200 状态透传
        let url = serve_image(404, "text/plain", b"nf".to_vec(), captured.clone()).await;
        let (bytes, _, status) = fetch_image("default", &url, None, 10, 5 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(status, 404);
        assert_eq!(bytes, b"nf");
    }

    /// GAP #88/125：大小上限——Content-Length 预检与流式累计双重拦截
    #[tokio::test]
    async fn test_fetch_image_size_cap() {
        let _ssrf = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
        clear_cookie_storage();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let body = vec![b'x'; 2048];
        let url = serve_image(
            200,
            "application/octet-stream",
            body.clone(),
            captured.clone(),
        )
        .await;
        // Content-Length 预检：声明 2048 > 上限 100 → 拒绝
        let err = fetch_image("default", &url, None, 10, 100)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("图片超过大小上限"), "{err}");
        // 正常上限内通过
        let (bytes, _, _) = fetch_image("default", &url, None, 10, 4096).await.unwrap();
        assert_eq!(bytes, body);
    }

    /// M1 SSRF：私网/回环/链路本地字面 IP 一律拒绝（127.0.0.1、10/8、172.16/12、
    /// 192.168/16、169.254/16、::1、fc00::/7、0.0.0.0、IPv4 映射回环）
    #[tokio::test]
    async fn test_ssrf_rejects_private_literal_ips() {
        let _g = ssrf_allow_private_guard(false); // 持锁：确保无测试并发放行私网
        for (url, label) in [
            ("http://127.0.0.1:8085/reader3/getSystemInfo", "回环 127/8"),
            ("http://127.1.2.3/x.png", "回环 127/8 非 .0.1"),
            ("http://10.0.0.6/x.png", "私网 10/8"),
            ("http://172.16.5.5/x.png", "私网 172.16/12"),
            ("http://172.31.255.255/x.png", "私网 172.16/12 上界"),
            ("http://192.168.1.1/x.png", "私网 192.168/16"),
            (
                "http://169.254.169.254/latest/meta-data",
                "链路本地（云元数据）",
            ),
            ("http://0.0.0.0/x.png", "未指定"),
            ("http://[::1]:8085/x.png", "IPv6 回环"),
            ("http://[fc00::1]/x.png", "IPv6 ULA fc00::/7"),
            ("http://[fd12:3456::1]/x.png", "IPv6 ULA fd00::/8"),
            ("http://[::ffff:127.0.0.1]/x.png", "IPv4 映射回环"),
            ("http://localhost:8085/x.png", "localhost 字面量"),
        ] {
            let err = validate_public_target(url).await.unwrap_err();
            assert!(
                err.to_string().contains("已拦截"),
                "{label}（{url}）应被拦截: {err}"
            );
        }
    }

    /// M1 SSRF：公网地址放行——字面公网 IP 与公网域名（DNS 解析后校验）
    #[tokio::test]
    async fn test_ssrf_allows_public_targets() {
        let _g = ssrf_allow_private_guard(false);
        validate_public_target("https://8.8.8.8/x.png")
            .await
            .expect("公网字面 IP 应放行");
        validate_public_target("https://1.1.1.1/x.png")
            .await
            .expect("公网字面 IP 应放行");
        // 公网域名：DNS 解析后应为公网地址（example.com 固定公网；离线环境跳过该断言）
        if tokio::net::lookup_host(("example.com", 443)).await.is_ok() {
            validate_public_target("https://example.com/x.png")
                .await
                .expect("公网域名解析后应放行");
        }
    }

    /// M1 SSRF：fetch_image 整体拒绝私网目标（不经 DNS 也拦截）——代理端点唯一回源入口
    #[tokio::test]
    async fn test_fetch_image_rejects_private_url() {
        let _g = ssrf_allow_private_guard(false);
        let err = fetch_image("default", "http://127.0.0.1:1/x.png", None, 3, 1024)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("已拦截"),
            "私网 URL 应在发请求前被拦截: {err}"
        );
    }

    /// M1 SSRF 回归：手动重定向跟进仍正常（Policy::none + 每跳校验）——302 → 公网目标可拉取；
    /// 跳转目标为私网时由 validate_public_target 拦截（该函数本身有独立单测）
    #[tokio::test]
    async fn test_fetch_image_follows_redirect() {
        let _g = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
                                                 // 目标服务器：返回 png
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = target.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = [0x89u8, b'P', b'N', b'G', 7, 7, 7];
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut resp = head.into_bytes();
            resp.extend_from_slice(&body);
            let _ = sock.write_all(&resp).await;
        });
        // 入口服务器：302 → 目标
        let entry = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let entry_addr = entry.local_addr().unwrap();
        let loc = format!("http://{target_addr}/final.png");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = entry.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = sock.write_all(head.as_bytes()).await;
        });
        let (bytes, ct, status) = fetch_image(
            "default",
            &format!("http://{entry_addr}/start.png"),
            None,
            5,
            1024,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(ct.as_deref(), Some("image/png"));
        assert_eq!(bytes, vec![0x89, b'P', b'N', b'G', 7, 7, 7]);
    }

    /// P1 SSRF 全覆盖：fetch 入口拒绝私网目标（http_get/http_post 书源抓取、
    /// java.ajax 等 JS shim、rss/schedule 订阅抓取均经 fetch——统一生效）
    #[tokio::test]
    async fn test_fetch_rejects_private_url() {
        let _g = ssrf_allow_private_guard(false);
        let headers = HashMap::new();
        // fetch 直调：回环 / 私网 / 链路本地（169.254 云元数据）
        for url in [
            "http://127.0.0.1:1/x",
            "http://10.0.0.1/x",
            "http://192.168.1.1/x",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]:1/x",
        ] {
            let err = fetch(url, &headers, 3, "GET", None, None, None)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("已拦截"),
                "fetch 应拦截私网目标（{url}）: {err}"
            );
        }
        // http_get / http_post 同链路生效
        let err = http_get("default", "http://127.0.0.1:1/x", &headers, 3, None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("已拦截"), "http_get 应拦截: {err}");
        let err = http_post(
            "default",
            "http://127.0.0.1:1/x",
            &headers,
            3,
            Some("a=1"),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("已拦截"),
            "http_post 应拦截: {err}"
        );
    }

    /// P1 SSRF：重定向跳转目标同步校验（Policy::custom 闭包）——私网/回环/非法 URL
    /// 拒绝；公网字面 IP 与公网域名放行（离线环境跳过域名断言）
    #[test]
    fn test_validate_redirect_target() {
        let _g = ssrf_allow_private_guard(false);
        for (url, label) in [
            ("http://127.0.0.1/x", "回环"),
            ("http://10.0.0.1/x", "私网 10/8"),
            ("http://172.16.0.1/x", "私网 172.16/12"),
            ("http://169.254.169.254/latest/meta-data", "链路本地"),
            ("http://[::1]/x", "IPv6 回环"),
            ("http://[fc00::1]/x", "IPv6 ULA"),
            ("http://localhost/x", "localhost 字面量"),
            ("not a url", "非法 URL"),
        ] {
            let err = validate_redirect_target(url).unwrap_err();
            assert!(
                err.to_string().contains("已拦截") || err.to_string().contains("非法"),
                "{label}（{url}）应被拦截: {err}"
            );
        }
        validate_redirect_target("https://8.8.8.8/x").expect("公网字面 IP 应放行");
        validate_redirect_target("https://1.1.1.1/x").expect("公网字面 IP 应放行");
        // 公网域名：DNS 解析后应为公网地址（离线环境跳过）
        if std::net::ToSocketAddrs::to_socket_addrs(&("example.com", 443)).is_ok() {
            validate_redirect_target("https://example.com/x").expect("公网域名解析后应放行");
        }
    }

    /// P1 SSRF：fetch 重定向正常跟进（Policy::custom 不破坏合法跳转）——
    /// 302 → 公网目标可拉取（mock 绑定 127.0.0.1，持放行守卫）
    #[tokio::test]
    async fn test_fetch_follows_redirect() {
        let _g = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
                                                 // 目标服务器：返回 200 文本
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = target.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = "redirected-ok";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut resp = head.into_bytes();
            resp.extend_from_slice(body.as_bytes());
            let _ = sock.write_all(&resp).await;
        });
        // 入口服务器：302 → 目标
        let entry = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let entry_addr = entry.local_addr().unwrap();
        let loc = format!("http://{target_addr}/final");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = entry.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = sock.write_all(head.as_bytes()).await;
        });
        let headers = HashMap::new();
        let resp = fetch(
            &format!("http://{entry_addr}/start"),
            &headers,
            5,
            "GET",
            None,
            None,
            None,
        )
        .await
        .expect("302 应自动跟进");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "redirected-ok");
        assert!(
            resp.url.ends_with("/final"),
            "最终 URL 应为跳转后地址: {}",
            resp.url
        );
    }

    /// 反爬域名自动优先：成功标记后同主机 30 分钟窗口内 GET 优先浏览器。
    #[test]
    fn test_browser_needed_marks_host() {
        mark_browser_needed("https://Example.COM/some/path");
        assert!(browser_needed("https://example.com/other"));
        assert!(!browser_needed("https://other.example.com/"));
    }

    // ==================== charset 表单/query 字段编码（legacy analyzeFields） ====================

    /// hasUrlEncoded 对齐：全保留字符或合法 %XX → 已编码；裸中文/杂散 % → 未编码
    #[test]
    fn test_has_url_encoded() {
        assert!(has_url_encoded("%E8%AF%A1%E7%A7%98%E4%B9%8B%E4%B8%BB"));
        assert!(has_url_encoded("abc123+-_.$:()!*@&#,[]"));
        assert!(has_url_encoded(""));
        assert!(!has_url_encoded("诡秘之主"));
        assert!(!has_url_encoded("a b"));
        assert!(!has_url_encoded("50%"));
        assert!(!has_url_encoded("100%%"));
    }

    /// GBK 表单编码：中文值 → GBK 字节 percent 序列；ASCII 值原样；空格→+
    #[test]
    fn test_encode_form_fields_gbk() {
        assert_eq!(
            encode_form_fields("key=中文值&other=data", "gbk"),
            "key=%D6%D0%CE%C4%D6%B5&other=data"
        );
        // 空格转 +（Java URLEncoder 语义）；+ 字面量转 %2B
        assert_eq!(encode_form_fields("q=a b+c", "gbk"), "q=a+b%2Bc");
        // 键不编码、无值段与空段容忍
        assert_eq!(encode_form_fields("中文=值", "gb2312"), "中文=%D6%B5");
        assert_eq!(encode_form_fields("a=&b=2&", "gbk"), "a=&b=2");
    }

    /// 已含 %XX 的值不重复编码（预编码 body + charset=gbk 常见——69shuba 等）
    #[test]
    fn test_encode_form_fields_skip_pre_encoded() {
        let body = "searchkey=%E8%AF%A1%E7%A7%98%E4%B9%8B%E4%B8%BB&searchtype=all&page=1";
        assert_eq!(encode_form_fields(body, "gbk"), body);
    }

    /// charset="escape"：JS escape() 风格（仅字母数字保留、小写十六进制、非 ASCII %uxxxx）
    #[test]
    fn test_encode_form_fields_escape() {
        assert_eq!(encode_form_fields("q=中 abc", "escape"), "q=%u4e2d%20abc");
        assert_eq!(encode_form_fields("k=中*._", "escape"), "k=%u4e2d%2a%2e%5f");
    }

    /// POST 中文原文 body + charset=gbk → 实际发出的 body 为 GBK percent 编码，
    /// 且自动补 Content-Type: application/x-www-form-urlencoded
    #[tokio::test]
    async fn test_http_post_form_body_encoded_by_charset_gbk() {
        let _ssrf = ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1
        clear_cookie_storage();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cap = captured.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            cap.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_string());
            let body = "ok";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
        });
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), "test-agent".to_string());
        let resp = http_post(
            "default",
            &format!("http://{addr}/search"),
            &headers,
            10,
            Some("key=中文值&other=data"),
            Some("gbk"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(resp.status, 200);
        let req = captured.lock().unwrap()[0].clone();
        assert!(
            req.contains("\r\n\r\nkey=%D6%D0%CE%C4%D6%B5&other=data"),
            "POST body 应为 GBK percent 编码序列: {req}"
        );
        assert!(
            req.to_lowercase()
                .contains("content-type: application/x-www-form-urlencoded"),
            "应自动补 form Content-Type: {req}"
        );
    }

    /// GET query 同样处理：URL ? 后参数值按 charset 编码
    #[tokio::test]
    async fn test_http_get_query_encoded_by_charset_gbk() {
        let _ssrf = ssrf_allow_private_guard(true);
        clear_cookie_storage();
        // 强制浏览器不可用：GET 走直连（避免测试机装了 camoufox 时被浏览器优先截胡）
        force_cf_browser_available(Some(false));
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cap = captured.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            cap.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_string());
            let body = "ok";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
        });
        let resp = http_get(
            "default",
            &format!("http://{addr}/s?wd=中文&p=1"),
            &HashMap::new(),
            10,
            Some("gbk"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(resp.status, 200);
        let req = captured.lock().unwrap()[0].clone();
        force_cf_browser_available(None);
        assert!(
            req.starts_with("GET /s?wd=%D6%D0%CE%C4&p=1 "),
            "GET query 值应按 GBK percent 编码: {req}"
        );
    }

    /// charset 缺省 / utf-8 / JSON body 不做任何改写（保持既有行为）
    #[tokio::test]
    async fn test_http_post_charset_untouched_cases() {
        let _ssrf = ssrf_allow_private_guard(true);
        clear_cookie_storage();
        for (body, charset) in [
            ("{\"key\": \"中文\"}", Some("gbk")),
            ("key=中文&other=data", None),
            ("key=中文&other=data", Some("utf-8")),
            ("key=中文&other=data", Some("")),
        ] {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let cap = captured.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                cap.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let body = "ok";
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
            });
            http_post(
                "default",
                &format!("http://{addr}/x"),
                &HashMap::new(),
                10,
                Some(body),
                charset,
                None,
            )
            .await
            .unwrap();
            let req = captured.lock().unwrap()[0].clone();
            assert!(
                req.contains(body),
                "charset={charset:?} 时 body 应原样发送: {req}"
            );
        }
    }

    /// UrlOption.retry 重试循环（legacy AnalyzeUrl.kt:564-573）：
    /// None/0 → 单次请求；n>0 → 共 n+1 次尝试，成功即止；失败次数耗尽返回末次错误
    #[tokio::test]
    async fn test_fetch_with_retry_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let counter = Arc::new(AtomicUsize::new(0));
        // 首次成功：retry=3 也仅请求一次
        let c = counter.clone();
        fetch_with_retry(Some(3), || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<(), anyhow::Error>(())
            }
        })
        .await
        .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        // 永远失败：retry=2 → 共 3 次，返回错误
        let c = counter.clone();
        assert!(fetch_with_retry(Some(2), || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(anyhow!("boom"))
            }
        })
        .await
        .is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 4);
        // retry=None / Some(0)：均单次请求
        for r in [None, Some(0)] {
            let c = counter.clone();
            assert!(fetch_with_retry(r, || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(anyhow!("boom"))
                }
            })
            .await
            .is_err());
        }
        assert_eq!(counter.load(Ordering::SeqCst), 6);
    }
}
