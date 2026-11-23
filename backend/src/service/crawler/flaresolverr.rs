//! Cloudflare 质询检测、FlareSolverr 求解与内置浏览器反爬兜底

use anyhow::{anyhow, Result};
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicI8;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::client::{shared_http_client, FetchResponse};
use super::cookies::{
    cookie_storage, merge_fs_cookies, merge_turnstile_token, parse_cookie_string,
    resolve_source_url,
};

/// 内置浏览器 CF 质询求解的质询等待循环上限（任务要求最多 30s）
pub const CF_SOLVE_MAX_WAIT_MS: u64 = 30_000;

// ==================== 反爬兜底与浏览器优先缓存 ====================

/// 内置浏览器兜底开关（默认开启：直连失败/反爬特征时自动浏览器导航重试；
/// `READER_BROWSER_FALLBACK_DISABLE=1` 关闭；浏览器优先关闭后此开关仍按反爬特征兜底）
pub(super) fn browser_fallback_enabled() -> bool {
    std::env::var("READER_BROWSER_FALLBACK_DISABLE")
        .map(|v| v.trim() != "1")
        .unwrap_or(true)
}

/// 求解失败日志熔断：60 秒内同源失败只 warn 一次，其余降为 debug
/// （真实环境教训：camoufox 服务不可用时每个请求一条 WARN 刷屏数千行）
static SOLVE_FAIL_COOLDOWN_UNTIL: AtomicI64 = AtomicI64::new(0);

/// `waited_ms` = 本次求解实际耗时；`degraded` = 失败后是降级直连还是直接返回错误。
/// 日志必须同时带 url / 耗时 / 预算（[`CF_SOLVE_MAX_WAIT_MS`]）/ 失败原因——否则
/// 「求解卡了 30s 然后放弃」和「浏览器进程没起来（1ms 内报错）」在日志里长得一样。
pub(super) fn log_solve_failure_cooled(
    url: &str,
    e: &anyhow::Error,
    waited_ms: u128,
    degraded: bool,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let until = SOLVE_FAIL_COOLDOWN_UNTIL.load(Ordering::Relaxed);
    let action = if degraded {
        "降级直连并静默"
    } else {
        "直接返回错误"
    };
    if now >= until {
        // 首条/冷却结束：warn 并续期 60s
        SOLVE_FAIL_COOLDOWN_UNTIL.store(now + 60_000, Ordering::Relaxed);
        tracing::warn!(
            "CF 求解失败（{url}）耗时 {waited_ms}ms / 预算 {CF_SOLVE_MAX_WAIT_MS}ms，{action}: {e:#}"
        );
    } else {
        tracing::debug!(
            "CF 求解失败（{url}）耗时 {waited_ms}ms / 预算 {CF_SOLVE_MAX_WAIT_MS}ms，{action}[冷却静默]: {e:#}"
        );
    }
}

/// 浏览器优先模式：默认开启（`READER_BROWSER_FIRST` 未设置时所有 GET 先经内置
/// camoufox 浏览器导航，最大限度减少验证码/WAF 拦截；代价是速度与资源占用）。
/// 显式 `READER_BROWSER_FIRST=0`/`false`/`off` 可关闭，恢复“直连优先、反爬兜底”。
/// 浏览器不可用或求解失败时自动降级直连，不会因缺浏览器导致抓取全部失败。
pub(super) fn browser_first_enabled() -> bool {
    std::env::var("READER_BROWSER_FIRST")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            !matches!(v.as_str(), "" | "0" | "false" | "off" | "no")
        })
        .unwrap_or(true)
}

/// 浏览器求解后端是否可用：camoufox 启用且未处于自启动失败熔断态即可（服务未启动时首次求解自动拉起）。
/// 不可用时浏览器优先自动回退直连。
pub(super) fn browser_solver_available() -> bool {
    crate::service::browser::is_browser_available()
        && !crate::service::camoufox::spawn_failed_cached()
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

pub(super) fn mark_browser_needed(url: &str) {
    let host = host_of_url(url);
    let mut map = BROWSER_NEEDED.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(host, Instant::now());
    if map.len() > 256 {
        map.retain(|_, at| at.elapsed() < BROWSER_NEEDED_TTL);
    }
}

pub(super) fn browser_needed(url: &str) -> bool {
    let host = host_of_url(url);
    let map = BROWSER_NEEDED.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&host)
        .is_some_and(|at| at.elapsed() < BROWSER_NEEDED_TTL)
}

/// 直连失败是否值得浏览器兜底：仅网络层错误（超时/连接中断/TLS/DNS 解析），
/// HTTP 业务错误（404 等）不启动浏览器
pub(super) fn should_browser_rescue_error(e: &anyhow::Error) -> bool {
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
pub(super) fn looks_like_anti_bot(status: u16, body: &str) -> bool {
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
    let resp = shared_http_client()
        .post(format!("{base}/v1"))
        .timeout(Duration::from_secs(60))
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

// ==================== 内置浏览器 CF 质询求解 ====================

/// 浏览器可用性探测（CF 内置求解前置检查；测试钩子可强制覆盖）
pub(super) fn cf_browser_available() -> bool {
    #[cfg(test)]
    {
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
static CF_BROWSER_AVAIL_OVERRIDE: AtomicI8 = AtomicI8::new(0);

/// CF 质询求解（camoufox 后端——唯一浏览器后端）：
/// - 浏览器不可用 → 明确错误（提示配置 camoufox）
/// - 成功：solution.html 作为响应正文；cookies 与用户 cookie 按 name 合并后存库（按用户）
///   + 浏览器 UA 记录（与 cf_clearance 绑定）
///
/// proxy：书源级代理（None = READER_CAMOUFOX_PROXY）——机房 IP 解 Turnstile 需住宅代理
/// 返回 (兜底响应, 合并后 cookie 串（含 turnstile_token 伪 cookie——重试直接用）, 浏览器 UA)
pub(super) async fn solve_cf_builtin(
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
pub(super) async fn store_solution_session(
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
    if let Some(storage) = cookie_storage() {
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
