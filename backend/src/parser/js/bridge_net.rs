//! 网络与浏览器桥接：
//! - `cookie` 对象与 `java.getCookie`
//! - `Reload` / `getWbiEnc`（bilibili wbi 签名）
//! - `java.startBrowserAwait`（通过 `service::browser` 门面解耦可选 camoufox 特性）
//! - `java.encodeURI` / `java.ajax` / `java.post` / `java.ajaxAll` / `java.connect` / `java.head`

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{anyhow, Result};
use boa_engine::object::builtins::JsArray;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsArgs, JsResult, JsString, JsValue, NativeFunction};
use serde_json::Value as JsonValue;

use super::runtime::{
    bind, block_on_task, js_native_error, js_value_to_json, js_value_to_string, JsBridge,
    JsBridgeInner, BRIDGE_WAIT_TIMEOUT,
};

/// 注册 `cookie` 全局对象（legado CookieStore shim——按用户命名空间读写爬虫层 cookie）
pub(super) fn install_cookie_object(context: &mut Context, bridge: &JsBridge) -> Result<()> {
    let cookie = ObjectInitializer::new(context)
        .function(
            bind(bridge, cookie_remove),
            JsString::from("removeCookie"),
            1,
        )
        .function(bind(bridge, cookie_get), JsString::from("getCookie"), 1)
        .function(bind(bridge, cookie_get_key), JsString::from("getKey"), 2)
        .function(bind(bridge, cookie_set), JsString::from("setCookie"), 2)
        .function(
            bind(bridge, cookie_replace),
            JsString::from("replaceCookie"),
            2,
        )
        .function(
            bind(bridge, cookie_remove),
            JsString::from("clearCookie"),
            1,
        )
        .build();
    context
        .register_global_property(JsString::from("cookie"), cookie, Attribute::all())
        .map_err(|e| anyhow!("cookie 对象注册失败: {e}"))
}

/// 解析 cookie 串为键值映射（legado CookieStore.cookieToMap）
pub(super) fn cookie_map(cookie: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in cookie.split(';') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        if !k.is_empty() && !v.is_empty() {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// cookie.removeCookie(url)：清除书源 cookie（按用户命名空间）
fn cookie_remove(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    if !url.is_empty() {
        let ns = inner.ns.clone();
        let fut = async move {
            crate::service::crawler::remove_cookie_for(&ns, &url).await;
            Ok::<_, anyhow::Error>(())
        };
        let _ = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cookie.removeCookie");
    }
    Ok(JsValue::undefined())
}

/// cookie.getCookie(url)：返回书源 cookie 串（无存储/未命中 → 空串）
fn cookie_get(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    if url.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let ns = inner.ns.clone();
    let fut = async move {
        let cookie = crate::service::crawler::cookie_for(&ns, &url)
            .await
            .unwrap_or_default();
        Ok::<_, anyhow::Error>(cookie)
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cookie.getCookie") {
        Ok(cookie) => Ok(JsValue::from(JsString::from(cookie))),
        Err(_) => Ok(JsValue::from(JsString::from(""))),
    }
}

/// cookie.getKey(url, key)：取 cookie 串中指定键值
fn cookie_get_key(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    let key = js_value_to_string(args.get_or_undefined(1), context);
    if url.is_empty() || key.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let ns = inner.ns.clone();
    let fut = async move {
        let cookie = crate::service::crawler::cookie_for(&ns, &url)
            .await
            .unwrap_or_default();
        Ok::<_, anyhow::Error>(cookie_map(&cookie).get(&key).cloned().unwrap_or_default())
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cookie.getKey") {
        Ok(v) => Ok(JsValue::from(JsString::from(v))),
        Err(_) => Ok(JsValue::from(JsString::from(""))),
    }
}

/// cookie.setCookie(url, cookie)：整串覆盖书源 cookie
fn cookie_set(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    let cookie = js_value_to_string(args.get_or_undefined(1), context);
    if !url.is_empty() {
        let ns = inner.ns.clone();
        let fut = async move {
            crate::service::crawler::set_cookie_for(&ns, &url, &cookie).await;
            Ok::<_, anyhow::Error>(())
        };
        let _ = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cookie.setCookie");
    }
    Ok(JsValue::undefined())
}

/// cookie.replaceCookie(url, cookie)：按键合并进书源 cookie（legado CookieStore.replaceCookie）
fn cookie_replace(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    let cookie = js_value_to_string(args.get_or_undefined(1), context);
    if !url.is_empty() {
        let ns = inner.ns.clone();
        let fut = async move {
            let old = crate::service::crawler::cookie_for(&ns, &url)
                .await
                .unwrap_or_default();
            let mut map = cookie_map(&old);
            for (k, v) in cookie_map(&cookie) {
                map.insert(k, v);
            }
            let merged = map
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            crate::service::crawler::set_cookie_for(&ns, &url, &merged).await;
            Ok::<_, anyhow::Error>(())
        };
        let _ = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cookie.replaceCookie");
    }
    Ok(JsValue::undefined())
}

/// java.getCookie(url, key?=null)：读取书源 cookie（与 cookie 对象同源）
pub(super) fn java_get_cookie(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    if url.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let key = js_value_to_string(args.get_or_undefined(1), context);
    let ns = inner.ns.clone();
    let fut = async move {
        let cookie = crate::service::crawler::cookie_for(&ns, &url)
            .await
            .unwrap_or_default();
        let out = if key.is_empty() {
            cookie
        } else {
            cookie_map(&cookie).get(&key).cloned().unwrap_or_default()
        };
        Ok::<_, anyhow::Error>(out)
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.getCookie") {
        Ok(v) => Ok(JsValue::from(JsString::from(v))),
        Err(_) => Ok(JsValue::from(JsString::from(""))),
    }
}

/// Reload(url)：拉取远程内容（书源远程 JS 加载：`eval(String(Reload('...')))`）
pub(super) fn reload_fetch(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    if url.is_empty() {
        return Err(js_native_error("Reload: url 不能为空"));
    }
    let fut_url = url.clone();
    let fut = async move {
        crate::service::crawler::fetch(&fut_url, &HashMap::new(), 15, "GET", None, None, None)
            .await
            .map(|r| r.body)
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "Reload") {
        Ok(body) => Ok(JsValue::from(JsString::from(body))),
        Err(e) => Err(js_native_error(format!("Reload 失败（{url}）: {e}"))),
    }
}

// ---- bilibili wbi 签名（getWbiEnc） ----

/// mixinKey 重排表（bilibili wbi 算法公开常量）
const WBI_MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// wbi 密钥缓存（img_key + sub_key + 获取时间戳；1 小时刷新）
static WBI_KEYS: LazyLock<Mutex<Option<(String, String, u64)>>> =
    LazyLock::new(|| Mutex::new(None));

/// 取 wbi 密钥（nav 接口；带缓存；失败 → None）
fn wbi_keys() -> Option<(String, String)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    {
        let cache = WBI_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((img, sub, ts)) = cache.as_ref() {
            if now - *ts < 3600 {
                return Some((img.clone(), sub.clone()));
            }
        }
    }
    let fut = async {
        let mut headers = HashMap::new();
        headers.insert(
            "User-Agent".to_string(),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0".to_string(),
        );
        let resp = crate::service::crawler::fetch(
            "https://api.bilibili.com/x/web-interface/nav",
            &headers,
            10,
            "GET",
            None,
            None,
            None,
        )
        .await
        .ok()?;
        let json: JsonValue = serde_json::from_str(&resp.body).ok()?;
        let img = json
            .pointer("/data/wbi_img/img_url")
            .and_then(|v| v.as_str())?;
        let sub = json
            .pointer("/data/wbi_img/sub_url")
            .and_then(|v| v.as_str())?;
        Some((wbi_key_of(img), wbi_key_of(sub)))
    };
    let fetched = block_on_task(
        async move { Ok::<_, anyhow::Error>(fut.await) },
        BRIDGE_WAIT_TIMEOUT,
        "getWbiEnc-nav",
    )
    .ok()
    .flatten();
    if let Some((img, sub)) = fetched {
        *WBI_KEYS.lock().unwrap_or_else(|e| e.into_inner()) = Some((img.clone(), sub.clone(), now));
        return Some((img, sub));
    }
    None
}

/// wbi 图片 URL → 密钥（去目录/扩展名）
fn wbi_key_of(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .split('.')
        .next()
        .unwrap_or("")
        .to_string()
}

/// mixinKey = 按重排表取 32 位
fn wbi_mixin_key(orig: &str) -> String {
    let bytes: Vec<char> = orig.chars().collect();
    WBI_MIXIN_KEY_ENC_TAB
        .iter()
        .take(32)
        .filter_map(|&i| bytes.get(i))
        .collect()
}

/// getWbiEnc(paramsObj)：bilibili wbi 签名 → 返回带 wts/w_rid 的 query 串
/// （密钥获取失败 → 返回排序 query + wts，不带 w_rid——不抛异常）
pub(super) fn get_wbi_enc(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let params = match js_value_to_json(args.get_or_undefined(0), context) {
        Ok(JsonValue::Object(map)) => map,
        _ => return Err(js_native_error("getWbiEnc: 参数须为对象")),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut sorted: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (k, v) in params {
        let s = match v {
            JsonValue::String(s) => s,
            JsonValue::Number(n) => n.to_string(),
            JsonValue::Bool(b) => b.to_string(),
            _ => continue,
        };
        if !s.is_empty() {
            sorted.insert(k, s);
        }
    }
    sorted.insert("wts".to_string(), now.to_string());
    let query = sorted
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    if let Some((img, sub)) = wbi_keys() {
        let mixin = wbi_mixin_key(&format!("{img}{sub}"));
        let w_rid = crate::util::md5::md5_encode(&format!("{query}{mixin}"));
        return Ok(JsValue::from(JsString::from(format!(
            "{query}&w_rid={w_rid}"
        ))));
    }
    Ok(JsValue::from(JsString::from(query)))
}

// ---- 内置浏览器求解（java.startBrowserAwait） ----

/// 内置浏览器可用性（`java.startBrowserAwait` 前置检查；测试钩子可强制覆盖）。
/// 统一通过 `service::browser::is_browser_available()` 判断，兼容关闭 `camoufox` Cargo feature。
fn js_browser_available() -> bool {
    #[cfg(test)]
    {
        use std::sync::atomic::Ordering;
        match JS_BROWSER_AVAIL_OVERRIDE.load(Ordering::Relaxed) {
            1 => return true,
            -1 => return false,
            _ => {}
        }
    }
    crate::service::browser::is_browser_available()
}

/// 测试钩子：强制浏览器可用性（Some(true)/Some(false) 强制；None 恢复自动探测）
#[cfg(test)]
pub(super) fn force_js_browser_available(v: Option<bool>) {
    use std::sync::atomic::Ordering;
    JS_BROWSER_AVAIL_OVERRIDE.store(
        match v {
            Some(true) => 1,
            Some(false) => -1,
            None => 0,
        },
        Ordering::Relaxed,
    );
}

#[cfg(test)]
static JS_BROWSER_AVAIL_OVERRIDE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(0);

/// 测试注入：页面求解钩子（返回 (html, cookies, ua)）；None = 走真实浏览器
#[cfg(test)]
pub(super) type SolveHook = dyn Fn(String, Vec<(String, String)>) -> Result<(String, Vec<(String, String)>, String)>
    + Send
    + Sync;

#[cfg(test)]
static JS_SOLVE_HOOK: LazyLock<Mutex<Option<Arc<SolveHook>>>> = LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
pub(super) fn register_js_solve_hook(hook: Option<Arc<SolveHook>>) {
    *JS_SOLVE_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

/// 页面求解（`java.startBrowserAwait` 后端）：带书源 cookie 加载页面并等待完成，
/// 返回 (html, cookies, user_agent)。
///
/// 接入 `browser::solve_captcha`（统一验证码求解入口：CF JS 质询 / Turnstile / 滑块
/// 自动处理；camoufox 服务端——Firefox 内核 + 真实指纹预设）。
#[cfg_attr(test, allow(unused_variables))]
async fn solve_page(
    ns: String,
    url: String,
    cookies: Vec<(String, String)>,
    _title: String,
) -> Result<(String, Vec<(String, String)>, String)> {
    #[cfg(test)]
    {
        let hook = JS_SOLVE_HOOK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            return hook(url, cookies);
        }
        // 测试环境：未注册求解钩子时直接报错——禁止测试路径启动真实浏览器
        Err(anyhow!("测试环境未注册页面求解钩子（不会启动真实浏览器）"))
    }
    #[cfg(not(test))]
    {
        let sol = crate::service::browser::solve_captcha(&ns, &url, &cookies, 60_000, None)
            .await
            .map_err(|e| anyhow!("浏览器加载失败（{url}）: {e:#}"))?;
        Ok((sol.html, sol.cookies, sol.user_agent))
    }
}

/// `java.startBrowserAwait(url, title, isForeground)`：内置浏览器打开 url（vUrl 已含
/// query）并等待加载完成，返回 JS 对象 `{body: 页面 html, cookies: ["name=value",...],
/// status: 200}`（boa 对象构造）。
/// - 带书源既有 cookie（按用户命名空间 + url base 匹配，与 crawler 一致）
/// - headless 环境无前台概念，isForeground 仅兼容占位
/// - 浏览器不可用 / 加载失败 / 超时（60s）→ 抛 JS 异常（书源规则可 catch）
pub(super) fn java_start_browser_await(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    let title = js_value_to_string(args.get_or_undefined(1), context);
    let _is_foreground = args.get_or_undefined(2).to_boolean();
    if url.is_empty() {
        return Err(js_native_error("java.startBrowserAwait: url 不能为空"));
    }
    if !js_browser_available() {
        return Err(js_native_error("java.startBrowserAwait 失败：camoufox 浏览器后端不可用——请配置 READER_CAMOUFOX_URL（或安装 python3 + camoufox，由程序自动拉起 scripts/camoufox_solver.py）".to_string()));
    }
    let ns = inner.ns.clone();
    let fut_url = url.clone();
    let fut = async move {
        // 书源既有 cookie（按用户命名空间；无注册/未命中 → 空）
        let cookie_str = crate::service::crawler::cookie_for(&ns, &fut_url)
            .await
            .unwrap_or_default();
        let cookies = crate::service::crawler::parse_cookie_string(&cookie_str);
        solve_page(ns, fut_url, cookies, title).await
    };
    let (html, cookies, _user_agent) =
        match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.startBrowserAwait") {
            Ok(v) => v,
            Err(e) => {
                return Err(js_native_error(format!(
                    "java.startBrowserAwait 失败（{url}）: {e}"
                )))
            }
        };
    // JS 对象：{body: html, cookies: ["a=1", ...], status: 200}（先构造数组再建对象，
    // 避免 ObjectInitializer 与 from_iter 同时可变借用 context）
    let cookies_arr = JsArray::from_iter(
        cookies
            .into_iter()
            .map(|(k, v)| JsValue::from(JsString::from(format!("{k}={v}")))),
        context,
    );
    let mut obj = ObjectInitializer::new(context);
    obj.property(
        JsString::from("body"),
        JsValue::from(JsString::from(html)),
        Attribute::all(),
    )
    .property(JsString::from("cookies"), cookies_arr, Attribute::all())
    .property(
        JsString::from("status"),
        JsValue::from(200),
        Attribute::all(),
    );
    Ok(obj.build().into())
}

// ---- URL 编码与 HTTP 请求（encodeURI / ajax / post / ajaxAll / connect / head） ----

/// `java.encodeURI(str, charset)`：按 charset（默认 utf-8；gbk/gb2312 走 encoding_rs）
/// 对字符串做 URL 百分号编码。encodeURI 语义：ASCII 字母数字与 `-_.!~*'()` 及
/// `;/?:@&=+$,#` 保留不编码；其余字节逐个 `%XX`（大写）。
pub(super) fn java_encode_uri(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let charset = args
        .get(1)
        .map(|v| js_value_to_string(v, context))
        .unwrap_or_default();
    let encoding =
        encoding_rs::Encoding::for_label(charset.trim().as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (bytes, _, _) = encoding.encode(&s);
    Ok(JsValue::from(JsString::from(percent_encode_uri(&bytes))))
}

/// encodeURI 语义百分号编码（保留 unreserved + `;/?:@&=+$,#`）
fn percent_encode_uri(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_'
                    | b'.'
                    | b'!'
                    | b'~'
                    | b'*'
                    | b'\''
                    | b'('
                    | b')'
                    | b','
                    | b';'
                    | b'/'
                    | b'?'
                    | b':'
                    | b'@'
                    | b'&'
                    | b'='
                    | b'+'
                    | b'$'
                    | b'#'
            )
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `url,{...}` 后缀（同 crawler/search 解析：method/body/charset/headers）
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AjaxSuffix {
    method: Option<String>,
    body: Option<String>,
    charset: Option<String>,
    headers: Option<HashMap<String, String>>,
}

/// 切分 `url,{...}` 后缀：从最后一个「逗号后整段为合法 JSON」的位置切分
/// （对齐 search::split_url_suffix）
fn parse_ajax_suffix(url: &str) -> (String, AjaxSuffix) {
    let mut split: Option<(usize, AjaxSuffix)> = None;
    for (i, ch) in url.char_indices() {
        if ch != ',' {
            continue;
        }
        let rest = url[i + 1..].trim_start();
        if !rest.starts_with('{') {
            continue;
        }
        if let Ok(suffix) = serde_json::from_str::<AjaxSuffix>(rest) {
            split = Some((i, suffix));
        }
    }
    match split {
        Some((i, suffix)) => (url[..i].to_string(), suffix),
        None => (url.to_string(), AjaxSuffix::default()),
    }
}

/// `java.ajax(urlOrSpec)`：带书源 cookie 的同步请求（阻塞等待结果），返回响应体文本。
/// 委托 [`java_ajax_fetch`]（GET 默认；失败返回错误文本——legacy ajax 不抛异常）。
pub(super) fn java_ajax(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url_spec = js_value_to_string(args.get_or_undefined(0), context);
    let timeout_secs = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| (n as u64).clamp(1, 60))
        .unwrap_or(15);
    let (body, _url, _status) = java_ajax_fetch(
        inner,
        &url_spec,
        "GET",
        None,
        timeout_secs,
        "java.ajax",
        true,
    )?;
    Ok(JsValue::from(JsString::from(body)))
}

/// `java.post(url[, body[, timeoutSecs]])`：POST 请求（legado 文档化 API，P3-A 实现）。
/// 复用 [`java_ajax_fetch`] 完整管线：书源 header + cookie + `,{...}` 后缀解析
/// （后缀显式 method 覆盖 POST；body 优先取显式参数，其次后缀 body）。
pub(super) fn java_post(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url_spec = js_value_to_string(args.get_or_undefined(0), context);
    let body = js_value_to_string(args.get_or_undefined(1), context);
    let timeout_secs = args
        .get(2)
        .and_then(|v| v.as_number())
        .map(|n| (n as u64).clamp(1, 60))
        .unwrap_or(15);
    java_ajax_fetch(
        inner,
        &url_spec,
        "POST",
        Some(&body),
        timeout_secs,
        "java.post",
        false,
    )
    .map(|(body, _, _)| JsValue::from(JsString::from(body)))
}

/// `java.ajaxAll(urls[, timeoutSecs])`：逐个同步请求 URL 数组（legacy JsExtensions.kt:65-79）。
/// 每个元素可为 URL 或 `url,{...}` 后缀形式（同 java.ajax）；返回 StrResponse 对象数组
/// （`.body()` 方法 + `.url`/`.code` 属性）；失败元素为错误文本对象（legacy 不抛异常）。
pub(super) fn java_ajax_all(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let arr = args.get_or_undefined(0);
    let obj = arr
        .as_object()
        .ok_or_else(|| js_native_error("java.ajaxAll 参数应为 URL 数组"))?;
    if !obj.is_array() {
        return Err(js_native_error("java.ajaxAll 参数应为 URL 数组"));
    }
    let len = obj
        .get(JsString::from("length"), context)?
        .to_u32(context)?;
    let timeout_secs = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| (n as u64).clamp(1, 60))
        .unwrap_or(15);
    // E12（legacy JsExtensions.kt:65-79）：元素为 StrResponse 对象——.body() 方法 +
    // .url/.code 属性；失败元素为错误文本对象（legacy 不抛异常）
    let mut items = Vec::with_capacity(len as usize);
    for k in 0..len {
        let item = obj.get(k, context)?;
        let spec = js_value_to_string(&item, context);
        let (body, url, status) = java_ajax_fetch(
            inner,
            &spec,
            "GET",
            None,
            timeout_secs,
            "java.ajaxAll",
            true,
        )?;
        items.push(make_str_response(context, body, url, status)?);
    }
    Ok(JsArray::from_iter(items, context).into())
}

/// StrResponse 兼容对象（legacy ajaxAll 元素形态）：
/// `.body()` 方法返回响应文本；`.url`/`.code` 为属性
fn make_str_response(
    context: &mut Context,
    body: String,
    url: String,
    status: u16,
) -> JsResult<JsValue> {
    let body_for_fn = body;
    let obj = ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure(move |_this, _args, _ctx| {
                    Ok(JsValue::from(JsString::from(body_for_fn.clone())))
                })
            },
            JsString::from("body"),
            0,
        )
        .property(
            JsString::from("url"),
            JsValue::from(JsString::from(url)),
            Attribute::all(),
        )
        .property(
            JsString::from("code"),
            JsValue::Integer(status as i32),
            Attribute::all(),
        )
        .build();
    Ok(JsValue::from(obj))
}

/// java.ajax / java.post / java.ajaxAll 共享实现：
/// - 支持 `url,{...}` 后缀（method/body/charset/headers，同 crawler 解析）
/// - url 为空 → 用书源 URL（baseUrl）兜底（legado 语义；兼容 `java.ajax(source.key)` 类写法）
/// - 请求头基底为 `java.headerMap`（书源 header）+ 后缀 headers + 书源 cookie
/// - 可选超时秒数（legado callTimeout 兼容；默认 15s，上限 60s）
/// - 返回 (响应体文本, 最终 URL, HTTP 状态码)；soft_fail=true 时失败返回错误文本
///   （legacy ajax 不抛异常——书源自行判断内容有效性），false 时抛 JS 异常
#[allow(clippy::too_many_arguments)]
fn java_ajax_fetch(
    inner: &JsBridgeInner,
    url_spec: &str,
    default_method: &str,
    body_override: Option<&str>,
    timeout_secs: u64,
    op: &str,
    soft_fail: bool,
) -> JsResult<(String, String, u16)> {
    let (mut url, suffix) = parse_ajax_suffix(url_spec);
    if url.is_empty() {
        // 空 url 兜底：书源 URL（legado：java.ajax 空参/undefined → 书源地址）
        url = inner.source_key.clone();
    }
    let ns = inner.ns.clone();
    // 请求头基底：java.headerMap（书源 header，JS 可改写）——async 块前克隆（'static）
    let headers_base = inner
        .headers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let fut_url = url.clone();
    let method = suffix
        .method
        .clone()
        .unwrap_or_else(|| default_method.to_string());
    let body = body_override
        .map(|b| b.to_string())
        .or_else(|| suffix.body.clone());
    let fut = async move {
        let mut headers = headers_base;
        if let Some(extra) = &suffix.headers {
            for (k, v) in extra {
                headers.insert(k.clone(), v.clone());
            }
        }
        // 书源 cookie（按用户命名空间；无注册/未命中 → 不带）
        if let Some(cookie_str) = crate::service::crawler::cookie_for(&ns, &fut_url).await {
            if !cookie_str.is_empty() {
                headers.insert("Cookie".to_string(), cookie_str);
            }
        }
        let resp = crate::service::crawler::fetch(
            &fut_url,
            &headers,
            timeout_secs,
            &method,
            body.as_deref(),
            suffix.charset.as_deref(),
            None,
        )
        .await?;
        Ok::<_, anyhow::Error>((resp.body, resp.url, resp.status))
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, op) {
        Ok((body, final_url, status)) => Ok((body, final_url, status)),
        Err(e) => {
            let msg = format!("{op} 失败（{url}）: {e}");
            if soft_fail {
                // legacy ajax：失败返回错误文本不抛异常
                Ok((msg, url, 0))
            } else {
                Err(js_native_error(msg))
            }
        }
    }
}

/// 同步 HTTP 请求（java.get(url)/connect(url)/head(url) 共用）：返回响应对象
pub(super) fn java_http_fetch(
    inner: &JsBridgeInner,
    url: &str,
    method: &str,
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = url.to_string();
    if url.is_empty() {
        return Err(js_native_error("java HTTP 请求: url 不能为空"));
    }
    let ns = inner.ns.clone();
    let headers_base = inner
        .headers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let method = method.to_string();
    let fut_method = method.clone();
    let fut_url = url.clone();
    let fut = async move {
        let mut headers = headers_base;
        if let Some(cookie_str) = crate::service::crawler::cookie_for(&ns, &fut_url).await {
            if !cookie_str.is_empty() {
                headers.insert("Cookie".to_string(), cookie_str);
            }
        }
        crate::service::crawler::fetch(&fut_url, &headers, 15, &fut_method, None, None, None).await
    };
    let resp = match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java-http") {
        Ok(r) => r,
        Err(e) => return Err(js_native_error(format!("java {method} 失败（{url}）: {e}"))),
    };
    http_response_object(resp, context)
}

/// 构造 HTTP 响应 JS 对象（legado okhttp 语义子集）：
/// raw() → {request(){url()}, code()}；header(name)；headers(name)→数组；body()/html()；
/// json()；code()；url()；cookies()→{}；toString() → body
fn http_response_object(
    resp: crate::service::crawler::FetchResponse,
    context: &mut Context,
) -> JsResult<JsValue> {
    let body = resp.body.clone();
    let final_url = resp.url.clone();
    let status = resp.status;
    let headers: Vec<(String, String)> = resp.headers.clone();
    let headers_arc = Arc::new(headers);

    let mut raw = ObjectInitializer::new(context);
    {
        let url = final_url.clone();
        let code = status;
        raw.function(
            unsafe {
                NativeFunction::from_closure(move |_this, _args, ctx| {
                    let mut req = ObjectInitializer::new(ctx);
                    let u = url.clone();
                    req.function(
                        NativeFunction::from_closure(move |_t, _a, _c| {
                            Ok(JsValue::from(JsString::from(u.clone())))
                        }),
                        JsString::from("url"),
                        0,
                    );
                    let c = code;
                    req.function(
                        NativeFunction::from_closure(move |_t, _a, _c| Ok(JsValue::from(c))),
                        JsString::from("code"),
                        0,
                    );
                    Ok(req.build().into())
                })
            },
            JsString::from("request"),
            0,
        );
    }
    raw.function(
        unsafe { NativeFunction::from_closure(move |_t, _a, _c| Ok(JsValue::from(status))) },
        JsString::from("code"),
        0,
    );
    let raw = raw.build();

    let mut obj = ObjectInitializer::new(context);
    obj.property(JsString::from("raw"), raw, Attribute::all());
    {
        let headers = Arc::clone(&headers_arc);
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx).to_lowercase();
                    let v = headers
                        .iter()
                        .find(|(k, _)| k == &name)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    Ok(JsValue::from(JsString::from(v)))
                })
            },
            JsString::from("header"),
            1,
        );
    }
    {
        let headers = Arc::clone(&headers_arc);
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx).to_lowercase();
                    let vals: Vec<JsValue> = headers
                        .iter()
                        .filter(|(k, _)| k == &name)
                        .map(|(_, v)| JsValue::from(JsString::from(v.clone())))
                        .collect();
                    Ok(JsArray::from_iter(vals, ctx).into())
                })
            },
            JsString::from("headers"),
            1,
        );
    }
    {
        let body = body.clone();
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(body.clone())))
                })
            },
            JsString::from("body"),
            0,
        );
    }
    {
        let body = body.clone();
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(body.clone())))
                })
            },
            JsString::from("html"),
            0,
        );
    }
    {
        let body = body.clone();
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, ctx| {
                    match serde_json::from_str::<JsonValue>(&body) {
                        Ok(json) => JsValue::from_json(&json, ctx)
                            .map_err(|e| js_native_error(format!("json(): {e}"))),
                        Err(_) => Err(js_native_error("json(): 响应体不是合法 JSON")),
                    }
                })
            },
            JsString::from("json"),
            0,
        );
    }
    {
        let url = final_url;
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(url.clone())))
                })
            },
            JsString::from("url"),
            0,
        );
    }
    obj.function(
        unsafe {
            NativeFunction::from_closure(move |_t, _a, ctx| {
                // cookies()：空对象（cookie 由爬虫层管理）
                Ok(ObjectInitializer::new(ctx).build().into())
            })
        },
        JsString::from("cookies"),
        0,
    );
    {
        obj.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(body.clone())))
                })
            },
            JsString::from("toString"),
            0,
        );
    }
    Ok(obj.build().into())
}

/// java.connect(url)：HTTP GET（legado okhttp 连接对象——raw().request().url() 链）
pub(super) fn java_connect(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    java_http_fetch(inner, &url, "GET", context)
}

/// java.head(url, headers)：HTTP HEAD（cookies() 兼容）
pub(super) fn java_head(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    java_http_fetch(inner, &url, "HEAD", context)
}
