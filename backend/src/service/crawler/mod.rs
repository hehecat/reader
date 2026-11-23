//! HTTP 抓取客户端（reqwest，书源抓取）
//!
//! - `http_get`/`http_post`：书源抓取入口——按用户命名空间 + 请求 URL 的 baseUrl
//!   从 book_source_cookies 表读取书源 cookie 自动附加（登录态独立于系统用户）；
//!   响应命中 Cloudflare 质询特征时自动转 FlareSolverr 或内置浏览器求解。
//! - `fetch`/`fetch_get`：原始抓取（不带 cookie/FS 逻辑），供 RSS/TTS 等非书源场景。

mod client;
mod cookies;
mod flaresolverr;
mod ssrf;
#[cfg(test)]
mod tests;

use anyhow::{anyhow, Result};
use std::collections::HashMap;

#[allow(unused_imports)]
pub(crate) use client::{
    client_cache_key, insert_capped, shared_direct_client, ClientCacheKeyInputs,
};
pub use client::{
    decode_bytes, fetch, fetch_get, fetch_image, http_client_builder, is_timeout_error,
    shared_http_client, shared_no_redirect_client, FetchResponse,
};
#[allow(unused_imports)]
pub(crate) use cookies::cookie_subdomain;
pub use cookies::{
    base_url_of, clear_cookie_storage, cookie_for, login_header_for, merge_fs_cookies,
    merge_turnstile_token, parse_cookie_string, parse_header, register_cookie_storage,
    remove_cookie_for, session_for, set_cookie_for, set_login_header_for,
};
#[cfg(test)]
pub(crate) use flaresolverr::force_cf_browser_available;
pub use flaresolverr::{
    flaresolverr_base, flaresolverr_request, is_cloudflare_challenge, FsCookie, FsSolution,
    CF_SOLVE_MAX_WAIT_MS,
};
pub use ssrf::{
    is_private_target_ip, validate_public_target, validate_redirect_target,
    validate_source_redirect_target, validate_source_target, SOURCE_ALLOW_PRIVATE,
    SSRF_ALLOW_PRIVATE,
};
#[cfg(test)]
pub use ssrf::{ssrf_allow_private_guard, SsrfAllowGuard};

use client::{doh_resolve, encode_form_fields, pin_dns};
use cookies::{capture_set_cookies, merge_cookie_strings, merge_login_header};
use flaresolverr::{
    browser_fallback_enabled, browser_first_enabled, browser_needed, browser_solver_available,
    cf_browser_available, log_solve_failure_cooled, looks_like_anti_bot, mark_browser_needed,
    should_browser_rescue_error, solve_cf_builtin, store_solution_session,
};

// ==================== 书源抓取（带 cookie + Cloudflare 质询绕过） ====================

/// 抓取作用域：决定是否允许「浏览器优先」（`READER_BROWSER_FIRST` + 反爬 host 缓存）。
///
/// `browser_first_enabled()` 默认 true，且 `browser_needed()` 是**进程内全局** host 缓存
/// （30 分钟 TTL，与书源/命名空间/调用场景无关）——只关 `browser_first_enabled()` 会被它
/// 绕过，所以必须按作用域在 `(A || B)` **整条表达式的外层**短路。
///
/// 搜索是广度扇出（一次 full_scan 派发 ~134 源），单次浏览器导航 5~30s，会把单源墙钟
/// 拖到 110s+；书信息/目录/正文是**深度串行**抓取，需要浏览器指纹绕反爬，行为
/// 必须与历史完全一致。CF 求解本身（`CF_SOLVE_MAX_WAIT_MS`）不在本参数管辖范围。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FetchScope {
    /// 书信息/目录/正文/登录/发现/OPDS/调试等一切非搜索路径——行为不变（默认）
    #[default]
    Book,
    /// 搜索路径：浏览器优先整条表达式（含 `browser_needed` 全局缓存）外层短路
    Search,
}

impl FetchScope {
    /// 该作用域是否允许「浏览器优先」，也决定是否向反爬 host 缓存 pre-warm 写入
    fn browser_first_allowed(self) -> bool {
        matches!(self, Self::Book)
    }
}

/// 浏览器优先判定（纯函数，便于单测）：`scope` 在最外层短路**整条**表达式——
/// 搜索作用域直接 false，连 `browser_needed()` 的进程内 host 缓存都不读。
fn browser_first_for(scope: FetchScope, method: &str, url: &str) -> bool {
    scope.browser_first_allowed()
        && method.eq_ignore_ascii_case("GET")
        && (browser_first_enabled() || browser_needed(url))
        && browser_solver_available()
}

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
    http_get_scoped(
        FetchScope::Book,
        ns,
        url,
        headers,
        timeout_secs,
        charset,
        proxy,
    )
    .await
}

/// [`http_get`] 的作用域版本：搜索路径传 [`FetchScope::Search`] 关闭浏览器优先
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub async fn http_get_scoped(
    scope: FetchScope,
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    http_fetch(
        scope,
        ns,
        url,
        headers,
        timeout_secs,
        "GET",
        None,
        charset,
        proxy,
    )
    .await
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
    http_post_scoped(
        FetchScope::Book,
        ns,
        url,
        headers,
        timeout_secs,
        body,
        charset,
        proxy,
    )
    .await
}

/// [`http_post`] 的作用域版本（POST 本就不会浏览器优先，scope 只影响 CF 求解后的 pre-warm）
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub async fn http_post_scoped(
    scope: FetchScope,
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    http_fetch(
        scope,
        ns,
        url,
        headers,
        timeout_secs,
        "POST",
        body,
        charset,
        proxy,
    )
    .await
}

/// [`http_get`] 带 legacy UrlOption.retry 重试：retry=None/0 → 单次请求；
/// n>0 失败后最多再试 n 次（立即重试，无退避；封顶 10 防呆）
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub async fn http_get_retry(
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    charset: Option<&str>,
    proxy: Option<&str>,
    retry: Option<u32>,
) -> Result<FetchResponse> {
    http_get_retry_scoped(
        FetchScope::Book,
        ns,
        url,
        headers,
        timeout_secs,
        charset,
        proxy,
        retry,
    )
    .await
}

/// [`http_get_retry`] 的作用域版本（搜索路径传 [`FetchScope::Search`]）
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub async fn http_get_retry_scoped(
    scope: FetchScope,
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    charset: Option<&str>,
    proxy: Option<&str>,
    retry: Option<u32>,
) -> Result<FetchResponse> {
    fetch_with_retry(retry, || {
        http_get_scoped(scope, ns, url, headers, timeout_secs, charset, proxy)
    })
    .await
}

/// [`http_post`] 重试语义同 [`http_get_retry`]
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
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
    http_post_retry_scoped(
        FetchScope::Book,
        ns,
        url,
        headers,
        timeout_secs,
        body,
        charset,
        proxy,
        retry,
    )
    .await
}

/// [`http_post_retry`] 的作用域版本（搜索路径传 [`FetchScope::Search`]）
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub async fn http_post_retry_scoped(
    scope: FetchScope,
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
        http_post_scoped(scope, ns, url, headers, timeout_secs, body, charset, proxy)
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
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
async fn http_fetch(
    scope: FetchScope,
    ns: &str,
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    method: &str,
    body: Option<&str>,
    charset: Option<&str>,
    proxy: Option<&str>,
) -> Result<FetchResponse> {
    let first = http_fetch_inner(
        scope,
        ns,
        url,
        headers,
        timeout_secs,
        method,
        body,
        charset,
        proxy,
    )
    .await;
    match first {
        Err(e) if proxy.is_none() => {
            let conn_fail = e
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|r| r.is_connect() || r.is_request());
            if !conn_fail {
                return Err(e);
            }
            // fake-ip/DNS 劫持环境（clash tun 等）：连接层失败 → DoH 解析真 IP 钉扎后重试一次
            let host = url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string));
            let resolved = match host {
                Some(h) => doh_resolve(&h).await.map(|ips| (h, ips)),
                None => None,
            };
            match resolved {
                Some((h, ips)) => {
                    tracing::info!(
                        "DoH 解析 {h} -> {ips:?}（本地 DNS 疑似劫持/fake-ip）——钉扎重试"
                    );
                    pin_dns(&h, ips);
                    http_fetch_inner(
                        scope,
                        ns,
                        url,
                        headers,
                        timeout_secs,
                        method,
                        body,
                        charset,
                        proxy,
                    )
                    .await
                }
                None => Err(e),
            }
        }
        other => other,
    }
}

#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
async fn http_fetch_inner(
    scope: FetchScope,
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
    // 浏览器优先路径同样执行 SSRF 入口校验（camoufox 侧还默认禁 RFC1918 内网导航，
    // 双保险——否则默认浏览器优先会让私网书源 URL 绕过 fetch 的直连校验）
    validate_source_target(&url).await?;
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

    // ② 浏览器优先（默认开启）：GET 先经内置 camoufox 导航，跳过 reqwest 直连。
    //    浏览器后端不可用时自动回退直连（不因缺浏览器而整体不可用）。
    tracing::debug!("http_fetch 直连 {method} {url}");
    let browser_first_get = browser_first_for(scope, method, &url);
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
                // 默认浏览器兜底（READER_BROWSER_FALLBACK_DISABLE=1 关闭）：网络层失败
                // （超时/连接中断/TLS）时内置 camoufox 浏览器重试——很多站点的反爬只对
                // 直连 reqwest 指纹生效，浏览器 stealth 指纹可正常访问
                if browser_fallback_enabled()
                    && method.eq_ignore_ascii_case("GET")
                    && should_browser_rescue_error(&e)
                {
                    match solve_cf_builtin(ns, &url, &cookie, proxy).await {
                        Ok((fallback, merged, solved_ua)) => {
                            // 搜索作用域不写全局 host 缓存：否则一次搜索把书信息/正文路径
                            // 也拖进浏览器优先（30 分钟 TTL 的双向污染）
                            if cf_browser_available() && scope.browser_first_allowed() {
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
    //    内置浏览器（含 Turnstile 分支）→ 求解成功 cookie 合并存库后**重试原请求**
    //    （原 method/body/headers + 新 cookie——POST 场景关键）；
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
        // 求解耗时（日志用：求解失败必须能看出「跑了多久/预算多少」）
        let solve_t0 = std::time::Instant::now();
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
                // 未配置 FLARESOLVERR_URL → 内置浏览器求解（带书源级代理 proxy）
                let solved = solve_cf_builtin(ns, &url, &cookie, proxy).await?;
                // 同上：搜索作用域不污染全局 host 缓存
                if cf_browser_available() && scope.browser_first_allowed() {
                    mark_browser_needed(&url);
                }
                Ok(solved)
            }
        }
        .await;
        let solve_waited_ms = solve_t0.elapsed().as_millis();
        let (fallback, merged_cookie, solved_ua) = match solved_result {
            Ok(v) => v,
            Err(e) if browser_first_get => {
                log_solve_failure_cooled(&url, &e, solve_waited_ms, true);
                // 冷却期内 debug 静默降级——避免服务未就绪时每请求刷屏
                return fetch(
                    &url,
                    &req_headers,
                    timeout_secs,
                    method,
                    body.as_deref(),
                    charset,
                    proxy,
                )
                .await;
            }
            Err(e) => {
                log_solve_failure_cooled(&url, &e, solve_waited_ms, false);
                return Err(e);
            }
        };

        // ④ 重试原请求（原 method/body/headers + 求解后的 cookie——POST 场景关键）
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
