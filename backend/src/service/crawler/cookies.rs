//! 书源 Cookie / 登录头 / Header 解析与持久化存储桥接（按用户命名空间隔离）

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::client::FetchResponse;
use super::flaresolverr::FsCookie;

/// 书源 cookie 存取：由 router 启动时注册（底层 Storage；None = 未注册，不附加 cookie）。
/// 全局注册（对齐 js.rs SOURCE_VARS 模式）：Storage 为连接池句柄，Clone 廉价。
static COOKIE_STORAGE: LazyLock<Mutex<Option<crate::storage::Storage>>> =
    LazyLock::new(|| Mutex::new(None));

/// 取当前任务/已注册的 cookie 存储句柄（内部克隆，不跨 await 持锁）。
/// 优先使用任务级依赖注入 `crate::storage::current_task_storage()`（HTTP/SSE 请求作用域零全局锁竞争），
/// 仅在返回 `None`（如独立单测或无作用域后台任务）时回退读取全局 `COOKIE_STORAGE`。
pub(super) fn cookie_storage() -> Option<crate::storage::Storage> {
    crate::storage::current_task_storage().or_else(|| {
        COOKIE_STORAGE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    })
}

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
pub(super) fn merge_cookie_strings(stored: &str, explicit: &str) -> String {
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

/// 短寿命 Set-Cookie（Max-Age ≤ 300s）不是登录态，而是反爬/限流的节流印章
/// （如 bqg.info 的 `ss_search_delay=1; Max-Age=30`）。cookie 表没有过期列，
/// 一旦回存就永久携带，等于把源自己锁死在限流页上，所以这类直接丢弃。
pub(super) fn is_throttle_cookie(raw: &str) -> bool {
    raw.split(';').skip(1).any(|attr| {
        let Some((k, v)) = attr.trim().split_once('=') else {
            return false;
        };
        k.trim().eq_ignore_ascii_case("max-age")
            && v.trim().parse::<i64>().map(|s| s <= 300).unwrap_or(false)
    })
}

/// 从响应头提取 Set-Cookie 的 name=value 对（忽略 Path/Expires 等属性）
pub(super) fn extract_set_cookie_pairs(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        if is_throttle_cookie(value) {
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
pub(super) async fn capture_set_cookies(ns: &str, url: &str, resp: &FetchResponse) {
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
    let storage = cookie_storage()?;
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
    if let Some(storage) = cookie_storage() {
        let _ = storage.set_cookie(ns, &key, cookie).await;
    }
}

/// 按命名空间 + 请求 URL 清除书源 cookie（legado `cookie.removeCookie`/`clearCookie`；
/// 新旧两种键都清——兼容历史 origin 键数据）
pub async fn remove_cookie_for(ns: &str, url: &str) {
    let sub = cookie_subdomain(url);
    let base = base_url_of(url);
    let Some(storage) = cookie_storage() else {
        return;
    };
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
    let storage = cookie_storage()?;
    storage
        .get_login_header_by_base(ns, &base)
        .await
        .ok()
        .flatten()
}

/// 按命名空间 + 书源 key 写入登录头（legacy `source.putLoginHeader`/`removeLoginHeader`；
/// 空值 = 清除；无注册存储时静默 no-op）
pub async fn set_login_header_for(ns: &str, source_url: &str, header: &str) {
    if let Some(storage) = cookie_storage() {
        let _ = storage.set_login_header(ns, source_url, header).await;
    }
}

/// 按命名空间 + 请求 URL 查书源登录态（cookie + user_agent）
///
/// 走 storage 的 cookie 快照缓存（按 ns 的内存快照，写路径立刻失效）——
/// 此前每次抓取都要全表读 `book_source_cookies` 找本源那一条，fan-out 期间是
/// 线上最热的慢 SQL。语义不变（`##` 备地址任一命中、空 cookie 也算命中）。
pub async fn session_for(ns: &str, url: &str) -> Option<(String, String)> {
    let base = base_url_of(url)?;
    let storage = cookie_storage()?;
    storage.get_session_by_base(ns, &base).await.ok().flatten()
}

/// 请求 URL → 书源 source_url（cookie 存储键；按 base 匹配）。
/// 无既有 cookie 行时回退用请求 baseUrl 作为键（get_cookie_by_base 按 base 命中，不影响查找）。
pub(super) async fn resolve_source_url(ns: &str, url: &str) -> Option<String> {
    let base = base_url_of(url)?;
    let Some(storage) = cookie_storage() else {
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

// ==================== cookie / header 工具 ====================

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
        let vars = HashMap::from([
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
pub(super) fn merge_login_header(headers: &mut HashMap<String, String>, login_header: &str) {
    for (k, v) in parse_header(login_header) {
        headers.insert(k, v);
    }
}
