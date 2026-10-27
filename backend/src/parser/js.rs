#![allow(deprecated)]
//! JS 规则执行（boa_engine 0.19，对齐 legado AnalyzeByJS / js 规则）
//!
//! v2 支持：
//! - 纯 JS 逻辑 + 注入变量（key/page/result/baseUrl/headerMap 简化）
//! - 书源桥接（JsBridge，对齐 legado jsHelp）：
//!   - `java.put(key, val)` / `java.get(key)`：bridge 生命周期内的临时变量
//!   - `java.log(msg)`：tracing 日志
//!   - `java.headerMap.put/get/size`：请求头 Map（eval 后经 `JsBridge::headers()` 读取）
//!   - `java.encodeURI(str, charset)`：URL 百分号编码（gbk/gb2312/utf-8，encoding_rs）
//!   - `java.ajax(urlOrSpec)`：带书源 cookie 的同步请求（支持 `url,{...}` 后缀），返回响应文本
//!   - `java.startBrowserAwait(url, title, isForeground)`：内置浏览器加载页面并等待完成，
//!     返回 `{body: html, cookies: ["name=value",...], status: 200}`（后端接入
//!     `browser::solve_captcha`——CF 质询/Turnstile/滑块统一求解）
//!   - `java.setContent(html)` / `java.getString(rule)` / `java.getElements(rule)`：
//!     设置当前解析文档 + css_chain 规则求值（文本 / outerHTML 数组）
//!   - `java.getWebViewUA()`：固定浏览器 UA（`JS_WEBVIEW_UA`）
//!   - `source.getKey()`（书源 URL）/ `source.getName()`（书源名）
//!   - `source.put(key, val)` / `source.get(key)`：书源级变量，全局共享、按书源 key
//!     隔离（跨搜索/详情调用可见，底层为全局 `Mutex<HashMap>`）
//!
//! boa 0.19 API 注意：
//! - 变量注入需经 JsString 转换（PropertyKey/JsValue 无 From<&str>，有 From<JsString>）
//! - NativeFunction 注册：需捕获状态的闭包走 `from_closure`（捕获数据不得含需 GC
//!   追踪的类型；std Mutex 无 Trace 实现，无法用 from_copy_closure_with_captures）
//! - JsError 含 Rc/NonNull，非 Send/Sync，不能直接进 anyhow，需 map_err 转字符串
//! - JsValue::to_string(&mut Context) 即规范 ToString（数字/布尔按 String() 语义）

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    Source,
};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::model::BookSource;

/// `java.getWebViewUA()` 返回的固定浏览器 UA（与爬虫默认 UA 同系 Chrome 120 内核；
/// 内置浏览器求解（solve_cf_challenge / 待落地的 solve_captcha）返回真实 UA 时
/// 以浏览器为准，此常量作为 JS 侧固定参考值）
pub const JS_WEBVIEW_UA: &str =
    "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36";

/// source.put/get 书源级变量存储：全局共享（跨搜索/详情调用），
/// 外层 key 为书源 key（URL），内层为该书源的变量表（书源间隔离）
static SOURCE_VARS: LazyLock<Mutex<HashMap<String, HashMap<String, String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `application.getSharedPreferences(name, mode)` 的 Android SharedPreferences 兼容存储。
/// 外层 key = 用户命名空间 + pref 名，内层为 key -> 类型保真的 JSON 值
/// （旧版阅读/legado 书源 Header 脚本常用它保存 token/开关，跨搜索/详情/目录 eval 可见）。
static APP_PREFS: LazyLock<Mutex<HashMap<String, HashMap<String, JsonValue>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// P1-3：source.put 存储上限（防书源脚本无界写入）——单书源最多 1000 条 / 总 1MB（UTF-8 字节），
/// 超限拒绝写入（no-op + warn，不抛错——legado 语义 source.put 无返回值，静默丢弃超限值）
pub const SOURCE_VARS_MAX_ENTRIES: usize = 1000;
pub const SOURCE_VARS_MAX_BYTES: usize = 1024 * 1024;

/// E11 `cache` 对象存储（legacy CacheManager shim）：按用户命名空间隔离、
/// 进程级跨请求持久；saveTime 秒过期（0=永不过期）；value 统一字符串，
/// typed getter 解析失败回退默认值。
static CACHE_STORE: LazyLock<Mutex<HashMap<String, (String, i64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// EG4：JS cache SQLite 持久化句柄（serve() 启动时注册，对齐 COOKIE_STORAGE 模式；
/// None = 未注册（测试/降级）——cache.put/get 仅内存不落库）
static JS_CACHE_STORAGE: LazyLock<Mutex<Option<crate::storage::Storage>>> =
    LazyLock::new(|| Mutex::new(None));

/// 注册 JS cache 持久化存储（启动时调用一次；随后 [`load_js_cache_from_db`] 恢复热缓存）
pub fn register_js_cache_storage(storage: crate::storage::Storage) {
    *JS_CACHE_STORAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(storage);
}

/// 注销 JS cache 持久化存储（回到内存模式；重启模拟/测试用）
pub fn clear_js_cache_storage() {
    *JS_CACHE_STORAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn js_cache_registered() -> Option<crate::storage::Storage> {
    JS_CACHE_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 内存键的命名空间归一（空 ns → "default"；与 SQLite user_namespace 列一致）
fn cache_db_ns(ns: &str) -> &str {
    if ns.is_empty() {
        "default"
    } else {
        ns
    }
}

/// EG4：启动时从 SQLite 加载未过期条目到内存热缓存（serve() 注册后调用一次）。
/// 返回恢复条数；此后 cache.get 命中内存不再读库（miss 读穿透仅作跨进程兜底）。
pub async fn load_js_cache_from_db(storage: &crate::storage::Storage) -> Result<usize> {
    let rows = storage.load_js_cache().await?;
    let mut m = CACHE_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let mut n = 0usize;
    for (ns, key, value, exp) in rows {
        // SQL 已过滤过期；时钟回拨等极端情形双保险再判一次
        if exp > 0 && exp <= cache_now_ms() {
            continue;
        }
        m.insert(cache_store_key(&ns, &key), (value, exp));
        n += 1;
    }
    Ok(n)
}

fn cache_store_key(ns: &str, key: &str) -> String {
    format!("{}\u{1}{key}", if ns.is_empty() { "default" } else { ns })
}

fn cache_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn cache_get_value(ns: &str, key: &str) -> Option<String> {
    let k = cache_store_key(ns, key);
    // ① 热缓存（命中即返回；惰性清除已过期条目）
    {
        let mut m = CACHE_STORE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((v, exp)) = m.get(&k) {
            if *exp > 0 && *exp <= cache_now_ms() {
                m.remove(&k);
            } else {
                return Some(v.clone());
            }
        }
    }
    // ② 读穿透 SQLite（miss 兜底：另一进程写库/手动清内存后仍可取回），
    //    命中后回填热缓存。失败按未命中处理（cache.get 语义为 null → 默认值）。
    let storage = js_cache_registered()?;
    let db_ns = cache_db_ns(ns).to_string();
    let db_key = key.to_string();
    let fut = async move { storage.get_js_cache(&db_ns, &db_key).await };
    let (v, exp) = match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cache.get") {
        Ok(Some(hit)) => hit,
        Ok(None) => return None,
        Err(e) => {
            tracing::debug!("cache.get 读库失败（按未命中处理）: {e}");
            return None;
        }
    };
    if exp > 0 && exp <= cache_now_ms() {
        return None;
    }
    CACHE_STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(k, (v.clone(), exp));
    Some(v)
}

fn cache_put_value(ns: &str, key: &str, value: String, save_time_secs: i64) {
    let k = cache_store_key(ns, key);
    let exp = if save_time_secs > 0 {
        cache_now_ms() + save_time_secs * 1000
    } else {
        0
    };
    CACHE_STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(k, (value.clone(), exp));
    // EG4 同步落库（INSERT OR REPLACE）：失败仅告警不抛错——legacy cache.put 无返回值，
    // 静默降级为内存模式，不影响书源规则继续执行
    if let Some(storage) = js_cache_registered() {
        let db_ns = cache_db_ns(ns).to_string();
        let db_key = key.to_string();
        let fut = async move { storage.put_js_cache(&db_ns, &db_key, &value, exp).await };
        if let Err(e) = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cache.put") {
            tracing::warn!("cache.put 落库失败（本次仅内存）: {e}");
        }
    }
}

/// source.put 核心（纯函数可测）：写入成功返回 true；超限（条数/字节）拒绝返回 false
fn source_put_limited(vars: &mut HashMap<String, String>, key: &str, value: &str) -> bool {
    let adding_new = !vars.contains_key(key);
    if adding_new && vars.len() >= SOURCE_VARS_MAX_ENTRIES {
        return false;
    }
    // 字节上限：当前总量 + 新写入（更新已有 key 同样受字节上限约束——防止反复更新撑破 1MB）
    let total: usize = vars.iter().map(|(k, v)| k.len() + v.len()).sum();
    if total + key.len() + value.len() > SOURCE_VARS_MAX_BYTES {
        return false;
    }
    vars.insert(key.to_string(), value.to_string());
    true
}

/// 书源 JS 桥接：持有书源信息与可被 JS 读写的状态（请求头 / java 临时变量）。
///
/// - 每次搜索/详情流程创建一次（`JsBridge::new(source_key, source_name)`），
///   同流程内多次 `eval_js_with_bridge` 共享 `java.put/get` 与 `java.headerMap`；
/// - `source.put/get` 走全局存储，跨流程/跨 bridge 实例可见（按书源 key 隔离）；
/// - 请求头：`set_headers` 注入初始值，JS 内 `java.headerMap.put` 改写，
///   eval 后 `headers()` 取回用于实际请求。
#[derive(Clone)]
pub struct JsBridge {
    inner: Arc<JsBridgeInner>,
}

struct JsBridgeInner {
    /// 书源 key（URL），`source.getKey()` 返回
    source_key: String,
    /// 书源名称，`source.getName()` 返回
    source_name: String,
    /// 书源登录 URL（JS 代码），`source.loginUrl` 返回（`eval(String(source.loginUrl))` 模式）
    login_url: String,
    /// 书源变量（`source.getVariable()`，legado 书源变量配置）
    source_variable: String,
    /// 书源 JS 库（`source.jsLib`——共享全局作用域，定义 AES_KEY/sign 等常量）
    js_lib: String,
    /// 书源 header（`source.header`，JSON 文本）
    source_header: String,
    /// 用户命名空间（书源 cookie 按用户隔离；空 = 无 cookie 上下文，
    /// `java.ajax`/`java.startBrowserAwait` 不带书源 cookie）
    ns: String,
    /// 请求头：`java.headerMap` 的底层存储（JS 可改写）
    headers: Mutex<HashMap<String, String>>,
    /// `java.put/get` 临时变量（本 bridge 生命周期内共享）
    java_vars: Mutex<HashMap<String, String>>,
    /// `java.setContent` 设置的当前解析文档（`java.getString/getElements` 的解析源）
    doc: Mutex<Option<String>>,
}

/// 手动 Clone（Mutex 无 Clone——快照当前内容；`Arc::try_unwrap` 多引用兜底路径用）
impl Clone for JsBridgeInner {
    fn clone(&self) -> Self {
        let lock = |m: &Mutex<HashMap<String, String>>| {
            Mutex::new(m.lock().unwrap_or_else(|e| e.into_inner()).clone())
        };
        Self {
            source_key: self.source_key.clone(),
            source_name: self.source_name.clone(),
            login_url: self.login_url.clone(),
            source_variable: self.source_variable.clone(),
            js_lib: self.js_lib.clone(),
            source_header: self.source_header.clone(),
            ns: self.ns.clone(),
            headers: lock(&self.headers),
            java_vars: lock(&self.java_vars),
            doc: Mutex::new(self.doc.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        }
    }
}

impl JsBridge {
    /// 创建书源桥接
    pub fn new(source_key: impl Into<String>, source_name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(JsBridgeInner {
                source_key: source_key.into(),
                source_name: source_name.into(),
                login_url: String::new(),
                source_variable: String::new(),
                js_lib: String::new(),
                source_header: String::new(),
                ns: String::new(),
                headers: Mutex::new(HashMap::new()),
                java_vars: Mutex::new(HashMap::new()),
                doc: Mutex::new(None),
            }),
        }
    }

    /// 从书源创建桥接（source.loginUrl/getVariable/header 等扩展字段可用）
    pub fn from_source(source: &BookSource, ns: impl Into<String>) -> Self {
        let bridge = Self::new(&source.book_source_url, &source.book_source_name);
        let mut inner = Arc::try_unwrap(bridge.inner).unwrap_or_else(|arc| (*arc).clone());
        inner.login_url = source.login_url.clone().unwrap_or_default();
        inner.source_variable = source
            .variable
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| source.variable_comment.clone().unwrap_or_default());
        inner.js_lib = source.js_lib.clone().unwrap_or_default();
        inner.source_header = source.header.clone().unwrap_or_default();
        inner.ns = ns.into();
        Self {
            inner: Arc::new(inner),
        }
    }

    /// 设置用户命名空间（书源 cookie 按用户隔离；搜索/详情流程传入当前用户 ns）
    pub fn with_namespace(mut self, ns: impl Into<String>) -> Self {
        // 先取出旧 inner 的可克隆状态，避免借用与赋值冲突
        let source_key = self.inner.source_key.clone();
        let source_name = self.inner.source_name.clone();
        let login_url = self.inner.login_url.clone();
        let source_variable = self.inner.source_variable.clone();
        let js_lib = self.inner.js_lib.clone();
        let source_header = self.inner.source_header.clone();
        let headers = self
            .inner
            .headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let java_vars = self
            .inner
            .java_vars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let doc = self
            .inner
            .doc
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.inner = Arc::new(JsBridgeInner {
            source_key,
            source_name,
            login_url,
            source_variable,
            js_lib,
            source_header,
            ns: ns.into(),
            headers: Mutex::new(headers),
            java_vars: Mutex::new(java_vars),
            doc: Mutex::new(doc),
        });
        self
    }

    /// 用户命名空间
    pub fn ns(&self) -> &str {
        &self.inner.ns
    }

    /// 书源 key（URL）
    pub fn source_key(&self) -> &str {
        &self.inner.source_key
    }

    /// 书源名称
    pub fn source_name(&self) -> &str {
        &self.inner.source_name
    }

    /// 设置初始请求头（JS 中可通过 `java.headerMap` 改写）
    pub fn set_headers(&self, headers: HashMap<String, String>) {
        *self.inner.headers.lock().unwrap_or_else(|e| e.into_inner()) = headers;
    }

    /// 读取请求头（eval 后取 JS 改写结果，用于实际请求）
    pub fn headers(&self) -> HashMap<String, String> {
        self.inner
            .headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Default for JsBridge {
    /// 空 bridge（旧 eval_js 兼容路径）：无书源信息、无请求头
    fn default() -> Self {
        Self::new("", "")
    }
}

/// 执行 JS 代码，返回字符串结果（旧签名，内部使用空 bridge）
///
/// 变量以全局属性注入；结果按 String() 语义转字符串
/// （null/undefined → 空串；数字/布尔 → 字面量，如 "42" / "true"）
pub fn eval_js(code: &str, vars: &HashMap<String, String>) -> Result<String> {
    eval_js_with_bridge(code, vars, &JsBridge::default())
}

/// JS 循环迭代上限（GAP #94：boa 死循环防卡死）。
/// boa 0.19 的 RuntimeLimits 无独立“指令数”上限，循环迭代计数（loop_iteration_limit）
/// 是其等价物：每次循环迭代 +1，超限抛 RangeError。10M 足够正常书源规则（一般 <1K 次）
/// 循环，而 `while(true){}` 这类死循环会在毫秒级内触发。
pub const JS_LOOP_ITERATION_LIMIT: u64 = 10_000_000;

/// 构造受限 Context（runtime limits：循环迭代上限，防死循环）
pub fn new_limited_context() -> Context {
    context_with_limit(JS_LOOP_ITERATION_LIMIT)
}

/// 以指定循环迭代上限构造 Context（测试用小上限快速触发）
fn context_with_limit(loop_limit: u64) -> Context {
    let mut context = Context::default();
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(loop_limit);
    context
}

// 最近一次 JS eval 失败的原始错误消息（线程局部——GAP 156：debug SSE 步骤输出用）。
// 每次 `map_js_error`（eval 失败统一出口）记录；debug 流程在步骤结束后
// `take_last_js_error` 取走并附加到步骤输出（错误消息 + JS 片段前 100 字符）。
// 注意：同步代码内取用（debug 步骤的 eval 与读取之间无 await，同线程安全）；
// 其他并发请求的 eval 在线程局部隔离，互不污染。
thread_local! {
    static LAST_JS_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// 取走（并清空）最近一次 JS eval 失败消息；无则 None。
pub fn take_last_js_error() -> Option<String> {
    LAST_JS_ERROR.with(|c| c.borrow_mut().take())
}

/// 将 boa 错误映射为友好文案：超限 → "JS 执行超限"；同时记录原始消息（debug 输出用）
fn map_js_error(e: boa_engine::JsError) -> anyhow::Error {
    let msg = e.to_string();
    LAST_JS_ERROR.with(|c| *c.borrow_mut() = Some(msg.clone()));
    if msg.to_lowercase().contains("loop iteration limit") {
        anyhow!("JS 执行超限")
    } else {
        anyhow!("JS 执行失败: {msg}")
    }
}

/// 执行 JS 代码并注入书源桥接（java.* / source.*，对齐 legado jsHelp）
pub fn eval_js_with_bridge(
    code: &str,
    vars: &HashMap<String, String>,
    bridge: &JsBridge,
) -> Result<String> {
    eval_js_with_bridge_limited(code, vars, bridge, JS_LOOP_ITERATION_LIMIT)
}

/// 执行 JS 并注入桥接 + **数值型**变量覆盖。
/// legacy AnalyzeUrl.kt:246 `bindings["page"] = page`（Int）——`{{page+1}}` 需要
/// 数值算术而非字符串拼接（"1"+1="11" 错位）；字符串 vars 先注入，数值后注册同名覆盖。
pub fn eval_js_with_bridge_num(
    code: &str,
    vars: &HashMap<String, String>,
    bridge: &JsBridge,
    numbers: &[(&str, i64)],
) -> Result<String> {
    let mut context = context_with_limit(JS_LOOP_ITERATION_LIMIT);
    install_globals(&mut context, bridge)?;
    inject_vars(&mut context, vars)?;
    for (name, n) in numbers {
        context
            .register_global_property(
                JsString::from(*name),
                JsValue::from(*n as i32),
                Attribute::all(),
            )
            .map_err(|e| anyhow!("数值变量注入失败 [{name}]: {e}"))?;
    }
    install_bridge(&mut context, bridge)?;
    auto_set_content(vars, bridge);
    let result = context
        .eval(Source::from_bytes(code.as_bytes()))
        .map_err(map_js_error)?;
    Ok(js_result_to_string(&result, &mut context))
}

/// 指定循环迭代上限的桥接执行（核心；测试用小上限验证超限路径）
fn eval_js_with_bridge_limited(
    code: &str,
    vars: &HashMap<String, String>,
    bridge: &JsBridge,
    loop_limit: u64,
) -> Result<String> {
    // legacy 常见变量默认兜底（调用方未注入时避免 ReferenceError）
    let mut vars = {
        let mut v = vars.clone();
        for k in [
            "urlSearchSeries",
            "urlSearch",
            "url",
            "baseUrl",
            "headerMap",
            "result",
            "key",
            "page",
        ] {
            v.entry(k.to_string()).or_default();
        }
        v
    };
    // E10/AR5：保留键先取出（不以裸字符串注入），inject_vars 后注册类型化绑定全局
    let ctx_bindings = take_context_bindings(&mut vars);
    let mut context = context_with_limit(loop_limit);
    install_globals(&mut context, bridge)?;
    inject_vars(&mut context, &vars)?;
    install_context_bindings(&mut context, &ctx_bindings)?;
    install_bridge(&mut context, bridge)?;
    auto_set_content(&vars, bridge);
    let result = context
        .eval(Source::from_bytes(code.as_bytes()))
        .map_err(map_js_error)?;
    Ok(js_result_to_string(&result, &mut context))
}

/// 执行 JS 并返回结构化结果（serde_json::Value）
///
/// 数组/对象经递归转换（`js_value_to_json`）直接得到 JSON——避免 boa ToString 对
/// 数组元素对象输出 "[object Object]" 导致后续 JSON.parse 为空；若结果为字符串且
/// 可解析为 JSON（如 JS 内 `JSON.stringify(...)` 出口），自动解析为对应结构。
pub fn eval_js_json(code: &str, vars: &HashMap<String, String>) -> Result<JsonValue> {
    eval_js_json_with_bridge(code, vars, &JsBridge::default())
}

/// 带书源桥接的 JSON 版本（同 eval_js_json，注入 java.*/source.*）
/// P1-C5：与 eval_js_json_with_bridge_limited 对齐——install_globals（默认变量/cookie/jsoup/
/// 缓存等 shim）+ auto_set_content（隐式 setContent：result 注入的 JS 规则可直接用
/// java.getString/getElements）
pub fn eval_js_json_with_bridge(
    code: &str,
    vars: &HashMap<String, String>,
    bridge: &JsBridge,
) -> Result<JsonValue> {
    // E10/AR5：保留键先取出（不以裸字符串注入），inject_vars 后注册类型化绑定全局
    let mut vars = vars.clone();
    let ctx_bindings = take_context_bindings(&mut vars);
    let mut context = context_with_limit(JS_LOOP_ITERATION_LIMIT);
    install_globals(&mut context, bridge)?;
    inject_vars(&mut context, &vars)?;
    install_context_bindings(&mut context, &ctx_bindings)?;
    install_bridge(&mut context, bridge)?;
    auto_set_content(&vars, bridge);
    let result = context
        .eval(Source::from_bytes(code.as_bytes()))
        .map_err(map_js_error)?;
    let json = js_value_to_json(&result, &mut context)
        .map_err(|e| anyhow!("JS 结果 JSON 转换失败: {e}"))?;
    // 字符串结果：若为 JSON 文本则解析为结构（兼容 JSON.stringify 出口）
    if let JsonValue::String(s) = &json {
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
            return Ok(parsed);
        }
    }
    Ok(json)
}

/// 指定循环迭代上限的 JSON 桥接执行（测试用小上限验证超限路径）
#[cfg(test)]
fn eval_js_json_with_bridge_limited(
    code: &str,
    vars: &HashMap<String, String>,
    bridge: &JsBridge,
    loop_limit: u64,
) -> Result<JsonValue> {
    let mut vars = vars.clone();
    let ctx_bindings = take_context_bindings(&mut vars);
    let mut context = context_with_limit(loop_limit);
    install_globals(&mut context, bridge)?;
    inject_vars(&mut context, &vars)?;
    install_context_bindings(&mut context, &ctx_bindings)?;
    install_bridge(&mut context, bridge)?;
    auto_set_content(&vars, bridge);
    let result = context
        .eval(Source::from_bytes(code.as_bytes()))
        .map_err(map_js_error)?;
    js_value_to_json(&result, &mut context).map_err(|e| anyhow!("JS 结果 JSON 转换失败: {e}"))
}

/// 执行 JS 表达式并返回 JsValue（供内部使用）
pub fn eval_js_value(code: &str, vars: &HashMap<String, String>) -> Result<JsValue> {
    let mut context = context_with_limit(JS_LOOP_ITERATION_LIMIT);
    install_globals(&mut context, &JsBridge::default())?;
    inject_vars(&mut context, vars)?;
    context
        .eval(Source::from_bytes(code.as_bytes()))
        .map_err(map_js_error)
}

/// 注入变量为全局属性（boa 0.19：key/value 需经 JsString 转换）
fn inject_vars(context: &mut Context, vars: &HashMap<String, String>) -> Result<()> {
    for (k, v) in vars {
        context
            .register_global_property(
                JsString::from(k.as_str()),
                JsValue::from(JsString::from(v.as_str())),
                Attribute::all(),
            )
            .map_err(|e| anyhow!("JS 变量注入失败 [{k}]: {e}"))?;
    }
    Ok(())
}

// E10/AR5：章节/书上下文绑定（legacy AnalyzeRule.kt:650-664 evalJS 注入集对齐）。
// 调用方（rule.rs push_js_context / 服务层直接传 RuleVars 表）以保留键写入 vars，
// 求值前在此展开为正确类型的全局变量：
// - title: 章节标题字符串；chapter: {title, url, index} 对象
// - book: {name, author, bookUrl} 对象；nextChapterUrl: 下一章 URL 字符串
// 缺失的绑定为 undefined（与 legacy null 一致，避免书源脚本 ReferenceError）；
// 已有非 undefined 同名全局（显式 vars 注入）不覆盖。
#[derive(Debug, Default)]
struct ContextBindings {
    chapter_title: Option<String>,
    chapter_url: Option<String>,
    chapter_index: Option<i64>,
    next_chapter_url: Option<String>,
    book_name: Option<String>,
    book_author: Option<String>,
    book_url: Option<String>,
}

fn take_binding(vars: &mut HashMap<String, String>, key: &str) -> Option<String> {
    vars.remove(key).filter(|v| !v.is_empty())
}

/// 从 vars 中取出全部保留键（取出 = 不再作为裸字符串全局注入）
fn take_context_bindings(vars: &mut HashMap<String, String>) -> ContextBindings {
    let mut b = ContextBindings {
        chapter_title: take_binding(vars, crate::parser::rule::RK_CHAPTER_TITLE),
        chapter_url: take_binding(vars, crate::parser::rule::RK_CHAPTER_URL),
        next_chapter_url: take_binding(vars, crate::parser::rule::RK_NEXT_CHAPTER_URL),
        book_name: take_binding(vars, crate::parser::rule::RK_BOOK_NAME),
        book_author: take_binding(vars, crate::parser::rule::RK_BOOK_AUTHOR),
        book_url: take_binding(vars, crate::parser::rule::RK_BOOK_URL),
        chapter_index: None,
    };
    if let Some(idx) = take_binding(vars, crate::parser::rule::RK_CHAPTER_INDEX) {
        b.chapter_index = idx.trim().parse::<i64>().ok();
    }
    b
}

/// 注册绑定全局：同名全局已定义（显式 vars 注入）时不覆盖；未定义时注册
/// （有值 → 绑定值；无值 → undefined，与 legacy null 一致，避免 ReferenceError）
fn bind_global(context: &mut Context, name: &str, value: JsValue) -> Result<()> {
    let exists_defined = context
        .global_object()
        .get(JsString::from(name), context)
        .ok()
        .map(|v| !v.is_undefined())
        .unwrap_or(false);
    if exists_defined {
        return Ok(());
    }
    context
        .register_global_property(JsString::from(name), value, Attribute::all())
        .map_err(|e| anyhow!("JS 绑定注入失败 [{name}]: {e}"))
}

fn install_context_bindings(context: &mut Context, b: &ContextBindings) -> Result<()> {
    let str_or_undefined = |s: &Option<String>| s.as_deref().map(JsString::from).map(JsValue::from);
    // title：章节标题字符串
    let title_val = str_or_undefined(&b.chapter_title).unwrap_or_else(JsValue::undefined);
    // chapter：{title, url, index}（任一属性存在即建对象，缺失属性为 undefined）
    let chapter_val: JsValue =
        if b.chapter_title.is_some() || b.chapter_url.is_some() || b.chapter_index.is_some() {
            ObjectInitializer::new(context)
                .property(
                    JsString::from("title"),
                    str_or_undefined(&b.chapter_title).unwrap_or_else(JsValue::undefined),
                    Attribute::all(),
                )
                .property(
                    JsString::from("url"),
                    str_or_undefined(&b.chapter_url).unwrap_or_else(JsValue::undefined),
                    Attribute::all(),
                )
                .property(
                    JsString::from("index"),
                    b.chapter_index
                        .map(JsValue::from)
                        .unwrap_or_else(JsValue::undefined),
                    Attribute::all(),
                )
                .build()
                .into()
        } else {
            JsValue::undefined()
        };
    // book：{name, author, bookUrl}
    let book_val: JsValue =
        if b.book_name.is_some() || b.book_author.is_some() || b.book_url.is_some() {
            ObjectInitializer::new(context)
                .property(
                    JsString::from("name"),
                    str_or_undefined(&b.book_name).unwrap_or_else(JsValue::undefined),
                    Attribute::all(),
                )
                .property(
                    JsString::from("author"),
                    str_or_undefined(&b.book_author).unwrap_or_else(JsValue::undefined),
                    Attribute::all(),
                )
                .property(
                    JsString::from("bookUrl"),
                    str_or_undefined(&b.book_url).unwrap_or_else(JsValue::undefined),
                    Attribute::all(),
                )
                .build()
                .into()
        } else {
            JsValue::undefined()
        };
    bind_global(context, "title", title_val)?;
    bind_global(context, "chapter", chapter_val)?;
    bind_global(context, "book", book_val)?;
    bind_global(
        context,
        "nextChapterUrl",
        str_or_undefined(&b.next_chapter_url).unwrap_or_else(JsValue::undefined),
    )?;
    Ok(())
}

/// 合法 JS 标识符（书源 variable 顶层键注入全局时过滤非法键名）
fn is_js_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '_' && first != '$' && !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

/// 注册 java / source 全局对象
fn install_bridge(context: &mut Context, bridge: &JsBridge) -> Result<()> {
    let (java, source) = build_bridge_objects(bridge, context)?;
    context
        .register_global_property(JsString::from("java"), java.clone(), Attribute::all())
        .map_err(|e| anyhow!("java 对象注册失败: {e}"))?;
    context
        .register_global_property(JsString::from("source"), source, Attribute::all())
        .map_err(|e| anyhow!("source 对象注册失败: {e}"))?;
    // legado 老书源兼容：顶层函数别名（旧版规则引擎把 JsExtensions 顶层函数直接注入
    // 全局作用域，书源脚本常用 `md5Encode(x)`/`base64Encode(x)` 而无需 java. 前缀）。
    // 仅别名纯字符串函数（无状态、无 IO），避免污染通用名称与 JS 标准内建。
    const TOP_LEVEL_ALIASES: &[&str] = &[
        "md5Encode",
        "md5Encode16",
        "base64Encode",
        "base64DecodeToString",
        "hexDecodeToString",
        "timeFormat",
        "timeFormatUTC",
        "utf8ToGbk",
        "htmlFormat",
        "digestHex",
        "randomUUID",
        "androidId",
    ];
    for name in TOP_LEVEL_ALIASES {
        if let Ok(v) = java.get(JsString::from(*name), context) {
            context
                .register_global_property(JsString::from(*name), v, Attribute::all())
                .map_err(|e| anyhow!("顶层函数别名注册失败 [{name}]: {e}"))?;
        }
    }
    Ok(())
}

/// `application` 兼容层：旧版阅读（Rhino）注入的 Android Application 实例。
/// 书源脚本常见用法：`application.getSharedPreferences("x", 0).getString("k", "")`、
/// `...edit().putString(...).apply()`、`application.getPackageName()`、
/// `application.getFilesDir().getAbsolutePath()`。Reader Dev 无 Android 环境，
/// 提供内存 SharedPreferences（按用户隔离、跨 eval 共享）与虚拟文件路径，
/// 避免 `ReferenceError: application is not defined` 导致目录/正文规则返回空。
fn install_application_shim(context: &mut Context, bridge: &JsBridge) -> Result<JsValue> {
    let package_name = JsString::from("io.legado.app");
    let application = ObjectInitializer::new(context)
        .property(
            JsString::from("packageName"),
            JsValue::from(package_name.clone()),
            Attribute::all(),
        )
        .property(
            JsString::from("versionCode"),
            JsValue::Integer(50006),
            Attribute::all(),
        )
        .property(
            JsString::from("versionName"),
            JsValue::from(JsString::from("5.0.6")),
            Attribute::all(),
        )
        .function(
            bind(bridge, application_get_package_name),
            JsString::from("getPackageName"),
            0,
        )
        .function(
            bind(bridge, application_get_shared_preferences),
            JsString::from("getSharedPreferences"),
            2,
        )
        .function(
            bind(bridge, application_files_dir),
            JsString::from("getFilesDir"),
            0,
        )
        .function(
            bind(bridge, application_cache_dir),
            JsString::from("getCacheDir"),
            0,
        )
        .function(
            bind(bridge, application_external_files_dir),
            JsString::from("getExternalFilesDir"),
            1,
        )
        .function(
            bind(bridge, application_external_cache_dir),
            JsString::from("getExternalCacheDir"),
            0,
        )
        .build();
    // 书源偶尔用 `application.context` 再取 SharedPreferences（旧版 Rhino 场景少见）——
    // 指向自身即可避免 null 解引用。
    application
        .set(
            JsString::from("context"),
            JsValue::from(application.clone()),
            true,
            context,
        )
        .map_err(|e| anyhow!("application.context 设置失败: {e}"))?;
    let app = JsValue::from(application.clone());
    context
        .register_global_property(JsString::from("application"), application, Attribute::all())
        .map_err(|e| anyhow!("application 对象注册失败: {e}"))?;
    Ok(app)
}

/// 旧版阅读 Rhino 环境的 Java/Android 常用全局兼容（书源脚本直接引用 Java 类）。
/// 覆盖常见且会导致 ReferenceError 的用法：URLEncoder/URLDecoder、UUID、
/// java.util.Base64、android.util.Base64、Log、System，以及 context/activity/app 别名。
/// `java.net/java.util/java.lang` 命名空间由 [`attach_java_namespaces`] 合并进桥接对象。
fn install_java_utils_shim(context: &mut Context, application: JsValue) -> Result<()> {
    // 全局 URLEncoder / URLDecoder（书源直接引用 Java 类；java.net.* 走桥接命名空间）
    let url_encoder = ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(java_url_encoder_encode) },
            JsString::from("encode"),
            1,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_url_decoder_decode) },
            JsString::from("decode"),
            1,
        )
        .build();
    let url_decoder = ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(java_url_decoder_decode) },
            JsString::from("decode"),
            1,
        )
        .build();
    context
        .register_global_property(JsString::from("URLEncoder"), url_encoder, Attribute::all())
        .map_err(|e| anyhow!("URLEncoder 全局注册失败: {e}"))?;
    context
        .register_global_property(JsString::from("URLDecoder"), url_decoder, Attribute::all())
        .map_err(|e| anyhow!("URLDecoder 全局注册失败: {e}"))?;

    // 全局 UUID（java.util.UUID 由桥接命名空间提供）
    let uuid = java_uuid_obj(context);
    context
        .register_global_property(
            JsString::from("UUID"),
            JsValue::from(uuid.clone()),
            Attribute::all(),
        )
        .map_err(|e| anyhow!("UUID 全局注册失败: {e}"))?;

    // 全局 System（java.lang.System 由桥接命名空间提供）
    let system = java_lang_system_obj(context);
    context
        .register_global_property(
            JsString::from("System"),
            JsValue::from(system.clone()),
            Attribute::all(),
        )
        .map_err(|e| anyhow!("System 全局注册失败: {e}"))?;

    // android.util.Base64（java.util.Base64 由桥接命名空间提供）
    let android_base64 = ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(java_base64_encode_to_string) },
            JsString::from("encodeToString"),
            1,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_base64_decode_to_string) },
            JsString::from("decode"),
            1,
        )
        .build();
    let android_util = ObjectInitializer::new(context)
        .property(
            JsString::from("Base64"),
            JsValue::from(android_base64),
            Attribute::all(),
        )
        .build();
    let android = ObjectInitializer::new(context)
        .property(
            JsString::from("util"),
            JsValue::from(android_util),
            Attribute::all(),
        )
        .build();
    context
        .register_global_property(JsString::from("android"), android, Attribute::all())
        .map_err(|e| anyhow!("android 命名空间注册失败: {e}"))?;

    // Log（no-op，避免书源调试调用 ReferenceError）
    let log = ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(java_log_noop) },
            JsString::from("v"),
            2,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_log_noop) },
            JsString::from("d"),
            2,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_log_noop) },
            JsString::from("i"),
            2,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_log_noop) },
            JsString::from("w"),
            2,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_log_noop) },
            JsString::from("e"),
            2,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_log_noop) },
            JsString::from("println"),
            1,
        )
        .build();
    context
        .register_global_property(JsString::from("Log"), log, Attribute::all())
        .map_err(|e| anyhow!("Log 全局注册失败: {e}"))?;

    // context/activity/app：旧版书源常从这些对象取 SharedPreferences（与 application 同层）
    for name in ["context", "activity", "app"] {
        context
            .register_global_property(JsString::from(name), application.clone(), Attribute::all())
            .map_err(|e| anyhow!("{name} 全局注册失败: {e}"))?;
    }
    Ok(())
}

/// java.net / java.util / java.lang 命名空间（挂到桥接 java 对象，避免被 install_bridge 覆盖）
fn attach_java_namespaces(java: &JsObject, context: &mut Context) -> Result<()> {
    // 用 JS 普通对象挂载（Rust ObjectInitializer 嵌套属性上的方法调用在 boa 0.19 存在
    // this 转换问题——`obj.method()` 报 cannot convert null/undefined to object；
    // JS 对象字面量 + 全局 shim 引用则正常）。
    context
        .register_global_property(
            JsString::from("__reader_java_bridge"),
            JsValue::from(java.clone()),
            Attribute::all(),
        )
        .map_err(|e| anyhow!("java 桥接暂存失败: {e}"))?;
    context
        .eval(Source::from_bytes(
            br#"
            (function () {
              var jb = globalThis.__reader_java_bridge;
              if (!jb) return;
              jb.net = {
                URLEncoder: globalThis.URLEncoder,
                URLDecoder: globalThis.URLDecoder
              };
              jb.util = {
                Base64: {
                  getEncoder: function () {
                    return { encodeToString: function (s) { return android.util.Base64.encodeToString(s, 0); } };
                  },
                  getDecoder: function () {
                    return { decode: function (s) { return android.util.Base64.decode(s, 0); } };
                  }
                },
                UUID: globalThis.UUID
              };
              jb.lang = { System: globalThis.System };
              delete globalThis.__reader_java_bridge;
            })();
            "#,
        ))
        .map_err(map_js_error)?;
    Ok(())
}

fn java_uuid_obj(context: &mut Context) -> JsObject {
    ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(java_uuid_random) },
            JsString::from("randomUUID"),
            0,
        )
        .build()
}

fn java_url_encoder_encode(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    Ok(JsValue::from(JsString::from(out)))
}

fn java_url_decoder_decode(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context).replace('+', " ");
    let out = urlencoding::decode(&s).map(|c| c.into_owned()).unwrap_or(s);
    Ok(JsValue::from(JsString::from(out)))
}

fn java_uuid_random(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(
        uuid::Uuid::new_v4().to_string(),
    )))
}

fn java_system_current_time_millis(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(JsValue::from(now))
}

fn java_system_nano_time(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Ok(JsValue::from(now))
}

fn java_lang_system_obj(context: &mut Context) -> JsObject {
    ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(java_system_current_time_millis) },
            JsString::from("currentTimeMillis"),
            0,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_system_nano_time) },
            JsString::from("nanoTime"),
            0,
        )
        .build()
}

fn java_base64_encode_to_string(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let out = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, s.as_bytes());
    Ok(JsValue::from(JsString::from(out)))
}

fn java_base64_decode_to_string(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s.trim())
        .unwrap_or_default();
    Ok(JsValue::from(JsString::from(
        String::from_utf8_lossy(&bytes).into_owned(),
    )))
}

fn java_log_noop(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn application_get_package_name(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from("io.legado.app")))
}

/// 虚拟目录对象（无 Android 文件系统，返回稳定路径字符串即可满足
/// `...getAbsolutePath()` 拼路径场景）。
fn application_dir(path: &str, context: &mut Context) -> JsResult<JsValue> {
    let path_abs = path.to_string();
    let path_rel = path.to_string();
    let obj = ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(path_abs.clone())))
                })
            },
            JsString::from("getAbsolutePath"),
            0,
        )
        .function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(path_rel.clone())))
                })
            },
            JsString::from("getPath"),
            0,
        )
        .build();
    Ok(obj.into())
}

fn application_files_dir(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    application_dir("/data/user/0/io.legado.app/files", context)
}

fn application_cache_dir(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    application_dir("/data/user/0/io.legado.app/cache", context)
}

fn application_external_files_dir(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    application_dir("/data/user/0/io.legado.app/external_files", context)
}

fn application_external_cache_dir(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    application_dir("/data/user/0/io.legado.app/external_cache", context)
}

/// SharedPreferences/Editor 方法绑定：捕获 pref 名（String 无 GC 追踪类型，from_closure 安全）。
fn bind_pref<F>(inner: Arc<JsBridgeInner>, pref: String, f: F) -> NativeFunction
where
    F: Fn(&JsBridgeInner, &str, &JsValue, &[JsValue], &mut Context) -> JsResult<JsValue> + 'static,
{
    unsafe {
        NativeFunction::from_closure(move |this, args, ctx| f(&inner, &pref, this, args, ctx))
    }
}

fn app_prefs_key(ns: &str, pref: &str) -> String {
    format!("{ns}\u{1f}::{pref}")
}

fn app_prefs_get(inner: &JsBridgeInner, pref: &str, key: &str) -> Option<JsonValue> {
    APP_PREFS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_prefs_key(&inner.ns, pref))
        .and_then(|m| m.get(key))
        .cloned()
}

fn application_get_shared_preferences(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let pref = js_value_to_string(args.get_or_undefined(0), context);
    let pref_bind = pref.clone();
    let sp_inner = Arc::new(JsBridgeInner {
        source_key: inner.source_key.clone(),
        source_name: inner.source_name.clone(),
        login_url: inner.login_url.clone(),
        source_variable: inner.source_variable.clone(),
        js_lib: inner.js_lib.clone(),
        source_header: inner.source_header.clone(),
        ns: inner.ns.clone(),
        headers: Mutex::new(HashMap::new()),
        java_vars: Mutex::new(HashMap::new()),
        doc: Mutex::new(None),
    });
    let sp = ObjectInitializer::new(context)
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_get_string),
            JsString::from("getString"),
            2,
        )
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_get_int),
            JsString::from("getInt"),
            2,
        )
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_get_long),
            JsString::from("getLong"),
            2,
        )
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_get_float),
            JsString::from("getFloat"),
            2,
        )
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_get_boolean),
            JsString::from("getBoolean"),
            2,
        )
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_get_all),
            JsString::from("getAll"),
            0,
        )
        .function(
            bind_pref(Arc::clone(&sp_inner), pref_bind.clone(), sp_contains),
            JsString::from("contains"),
            1,
        )
        .function(
            bind_pref(sp_inner, pref_bind, sp_edit),
            JsString::from("edit"),
            0,
        )
        .build();
    // Android 属性：name / mode（脚本偶发读取）
    sp.set(
        JsString::from("name"),
        JsValue::from(JsString::from(pref)),
        true,
        context,
    )?;
    Ok(sp.into())
}

fn sp_get_string(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let def = js_value_to_string(args.get_or_undefined(1), context);
    let out = match app_prefs_get(inner, pref, &key) {
        Some(JsonValue::String(s)) => s,
        Some(JsonValue::Number(n)) => n.to_string(),
        Some(JsonValue::Bool(b)) => b.to_string(),
        _ => def,
    };
    Ok(JsValue::from(JsString::from(out)))
}

fn sp_get_int(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let def = js_value_to_i64(args.get_or_undefined(1), context);
    let out = match app_prefs_get(inner, pref, &key) {
        Some(JsonValue::Number(n)) => n.as_i64().unwrap_or(def),
        Some(JsonValue::String(s)) => s.parse::<i64>().unwrap_or(def),
        Some(JsonValue::Bool(b)) => i64::from(b),
        _ => def,
    };
    Ok(JsValue::from(out))
}

fn sp_get_long(
    inner: &JsBridgeInner,
    pref: &str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    sp_get_int(inner, pref, this, args, context)
}

fn sp_get_float(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let def = js_value_to_f64(args.get_or_undefined(1), context);
    let out = match app_prefs_get(inner, pref, &key) {
        Some(JsonValue::Number(n)) => n.as_f64().unwrap_or(def),
        Some(JsonValue::String(s)) => s.parse::<f64>().unwrap_or(def),
        Some(JsonValue::Bool(b)) => f64::from(b),
        _ => def,
    };
    Ok(JsValue::from(out))
}

fn sp_get_boolean(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let def = args
        .get(1)
        .map(|v| js_value_to_bool(v, context))
        .unwrap_or(false);
    let out = match app_prefs_get(inner, pref, &key) {
        Some(JsonValue::Bool(b)) => b,
        Some(JsonValue::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(JsonValue::String(s)) => {
            let s = s.trim().to_ascii_lowercase();
            s == "true" || s == "1" || s == "yes"
        }
        _ => def,
    };
    Ok(JsValue::from(out))
}

fn sp_get_all(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let map = APP_PREFS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_prefs_key(&inner.ns, pref))
        .cloned()
        .unwrap_or_default();
    let obj = ObjectInitializer::new(context).build();
    for (k, v) in map {
        obj.set(
            JsString::from(k),
            json_to_js_value(v, context),
            true,
            context,
        )?;
    }
    Ok(obj.into())
}

fn sp_contains(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(app_prefs_get(inner, pref, &key).is_some()))
}

/// SharedPreferences.Editor：方法链式返回自身（put/remove/clear/commit/apply 语义对齐）。
fn sp_edit(
    inner: &JsBridgeInner,
    pref: &str,
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut editor = ObjectInitializer::new(context);
    let editor_inner = Arc::new(JsBridgeInner {
        source_key: inner.source_key.clone(),
        source_name: inner.source_name.clone(),
        login_url: inner.login_url.clone(),
        source_variable: inner.source_variable.clone(),
        js_lib: inner.js_lib.clone(),
        source_header: inner.source_header.clone(),
        ns: inner.ns.clone(),
        headers: Mutex::new(HashMap::new()),
        java_vars: Mutex::new(HashMap::new()),
        doc: Mutex::new(None),
    });
    for (name, f) in [
        (
            "putString",
            sp_put_string
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "putInt",
            sp_put_number
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "putLong",
            sp_put_number
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "putFloat",
            sp_put_number
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "putBoolean",
            sp_put_boolean
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "remove",
            sp_remove
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "clear",
            sp_clear
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "commit",
            sp_commit
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
        (
            "apply",
            sp_apply
                as fn(
                    &JsBridgeInner,
                    &str,
                    &JsValue,
                    &[JsValue],
                    &mut Context,
                ) -> JsResult<JsValue>,
        ),
    ] {
        editor.function(
            bind_pref(Arc::clone(&editor_inner), pref.to_string(), f),
            JsString::from(name),
            if name == "commit" || name == "apply" || name == "clear" {
                0
            } else {
                2
            },
        );
    }
    Ok(editor.build().into())
}

fn sp_put_string(
    inner: &JsBridgeInner,
    pref: &str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_string(args.get_or_undefined(1), context);
    app_prefs_insert(inner, pref.to_string(), key, JsonValue::String(value));
    Ok(this.clone())
}

fn sp_put_number(
    inner: &JsBridgeInner,
    pref: &str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_json(args.get_or_undefined(1), context).unwrap_or(JsonValue::Null);
    let value = match value {
        JsonValue::Number(_) => value,
        JsonValue::String(s) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        _ => JsonValue::Null,
    };
    app_prefs_insert(inner, pref.to_string(), key, value);
    Ok(this.clone())
}

fn sp_put_boolean(
    inner: &JsBridgeInner,
    pref: &str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_bool(args.get_or_undefined(1), context);
    app_prefs_insert(inner, pref.to_string(), key, JsonValue::Bool(value));
    Ok(this.clone())
}

fn sp_remove(
    inner: &JsBridgeInner,
    pref: &str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    if let Some(m) = APP_PREFS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&app_prefs_key(&inner.ns, pref))
    {
        m.remove(&key);
    }
    Ok(this.clone())
}

fn sp_clear(
    inner: &JsBridgeInner,
    pref: &str,
    this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(m) = APP_PREFS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&app_prefs_key(&inner.ns, pref)) { m.clear() }
    Ok(this.clone())
}

fn sp_commit(
    _inner: &JsBridgeInner,
    _pref: &str,
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(true))
}

fn sp_apply(
    _inner: &JsBridgeInner,
    _pref: &str,
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn app_prefs_insert(inner: &JsBridgeInner, pref: String, key: String, value: JsonValue) {
    APP_PREFS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(app_prefs_key(&inner.ns, &pref))
        .or_default()
        .insert(key, value);
}

fn json_to_js_value(value: JsonValue, _context: &mut Context) -> JsValue {
    match value {
        JsonValue::String(s) => JsValue::from(JsString::from(s)),
        JsonValue::Number(n) => n
            .as_i64()
            .map(JsValue::from)
            .or_else(|| n.as_f64().map(JsValue::from))
            .unwrap_or(JsValue::undefined()),
        JsonValue::Bool(b) => JsValue::from(b),
        _ => JsValue::undefined(),
    }
}

fn js_value_to_f64(v: &JsValue, context: &mut Context) -> f64 {
    match v {
        JsValue::Integer(i) => f64::from(*i),
        JsValue::Rational(r) => *r,
        JsValue::BigInt(b) => b.to_f64(),
        _ => js_value_to_string(v, context).parse::<f64>().unwrap_or(0.0),
    }
}

fn js_value_to_bool(v: &JsValue, context: &mut Context) -> bool {
    match v {
        JsValue::Boolean(b) => *b,
        JsValue::Integer(i) => *i != 0,
        JsValue::Rational(r) => *r != 0.0,
        _ => {
            let s = js_value_to_string(v, context).trim().to_ascii_lowercase();
            s == "true" || s == "1" || s == "yes"
        }
    }
}

/// 规则 JS 求值前自动 setContent（legado AnalyzeByJS 语义：注入 result 的 JS 规则
/// 自动把 result 设为当前解析文档——`java.getString/getElements` 无需先手动 setContent）
fn auto_set_content(vars: &HashMap<String, String>, bridge: &JsBridge) {
    if let Some(result) = vars.get("result") {
        if !result.is_empty() {
            *bridge.inner.doc.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
        }
    }
}

/// 全局 shim（每个 eval 上下文安装一次，先于 vars 注入——vars 同名覆盖）：
/// - JS prelude：`Map()` 无 new 调用兼容（legado Rhino 允许；boa 0.19 报错）、
///   `cache` 内存对象、`unescape/escape`
/// - 默认全局变量：baseUrl/base_url/src/type/urlIP（缺省值，避免 ReferenceError）
/// - 全局对象/函数：cookie（removeCookie/getCookie/setCookie）、org.jsoup（Jsoup.parse）、
///   xGorgon（stub）、getWbiEnc（bilibili wbi 签名）、Reload（拉取远程 JS 文本）
fn install_globals(context: &mut Context, bridge: &JsBridge) -> Result<()> {
    let prelude = r#"
(function () {
  // Map() 无 new：legado Rhino 允许（boa 0.19 报 TypeError）——包装为可无 new 调用
  var NativeMap = Map;
  function MapShim(iterable) {
    var m = new NativeMap();
    if (iterable != null && typeof iterable[Symbol.iterator] === 'function') {
      var it = iterable[Symbol.iterator]();
      var step;
      while (!(step = it.next()).done) { m.set(step.value[0], step.value[1]); }
    }
    return m;
  }
  MapShim.prototype = NativeMap.prototype;
  try { globalThis.Map = MapShim; } catch (e) {}
  // cache：内存 KV（legado cache 全局）
  if (typeof cache === 'undefined') {
    globalThis.cache = (function () {
      var store = new NativeMap();
      return {
        get: function (k) { return store.get(k); },
        set: function (k, v) { store.set(k, v); },
        getFromMemory: function (k) { return store.get(k); },
        putToMemory: function (k, v) { store.set(k, v); },
        deleteMemory: function (k) { store.delete(k); },
        clear: function () { store.clear(); }
      };
    })();
  }
  if (typeof unescape !== 'function') {
    globalThis.unescape = function (s) { return decodeURIComponent(String(s)); };
    globalThis.escape = function (s) { return encodeURIComponent(String(s)); };
  }
  // URL/URLSearchParams：boa 无内置 URL——书源 header/搜索 JS 常用 `new URL(...)`
  // （日志 ReferenceError: URL is not defined）。最小实现覆盖 href/origin/协议/
  // 主机/路径/查询/哈希/searchParams 与相对 URL 拼接。
  function URLSearchParamsImpl(init) {
    this._list = [];
    if (init != null) {
      if (typeof init === 'string') {
        var q = init.charAt(0) === '?' ? init.slice(1) : init;
        if (q) {
          q.split('&').forEach(function (pair) {
            if (!pair) return;
            var eq = pair.indexOf('=');
            var k = eq < 0 ? pair : pair.slice(0, eq);
            var v = eq < 0 ? '' : pair.slice(eq + 1);
            try { k = decodeURIComponent(k.replace(/\+/g, ' ')); } catch (e) {}
            try { v = decodeURIComponent(v.replace(/\+/g, ' ')); } catch (e) {}
            this._list.push([k, v]);
          }, this);
        }
      } else if (typeof init === 'object') {
        var self = this;
        Object.keys(init).forEach(function (k) { self._list.push([k, String(init[k])]); });
      }
    }
  }
  URLSearchParamsImpl.prototype.append = function (k, v) { this._list.push([String(k), String(v)]); };
  URLSearchParamsImpl.prototype.delete = function (k) {
    k = String(k);
    this._list = this._list.filter(function (p) { return p[0] !== k; });
  };
  URLSearchParamsImpl.prototype.get = function (k) {
    k = String(k);
    for (var i = 0; i < this._list.length; i++) {
      if (this._list[i][0] === k) return this._list[i][1];
    }
    return null;
  };
  URLSearchParamsImpl.prototype.getAll = function (k) {
    k = String(k);
    var out = [];
    for (var i = 0; i < this._list.length; i++) {
      if (this._list[i][0] === k) out.push(this._list[i][1]);
    }
    return out;
  };
  URLSearchParamsImpl.prototype.has = function (k) {
    return this.get(String(k)) !== null;
  };
  URLSearchParamsImpl.prototype.set = function (k, v) {
    this.delete(k);
    this.append(k, v);
  };
  URLSearchParamsImpl.prototype.toString = function () {
    var self = this;
    return this._list.map(function (p) {
      return encodeURIComponent(p[0]).replace(/%20/g, '+') + '=' + encodeURIComponent(p[1]).replace(/%20/g, '+');
    }).join('&');
  };
  URLSearchParamsImpl.prototype.forEach = function (fn, thisArg) {
    for (var i = 0; i < this._list.length; i++) {
      fn.call(thisArg || null, this._list[i][1], this._list[i][0], this);
    }
  };
  function URLImpl(url, base) {
    if (!(this instanceof URLImpl)) return new URLImpl(url, base);
    var raw = String(url == null ? '' : url).trim();
    var rawBase = base == null ? '' : String(base).trim();
    var urlRe = /^([a-z][a-z0-9+.-]*):(?:\/\/([^\/?#]*))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/i;
    var m = urlRe.exec(raw);
    var scheme = '', auth = '', path = '', query = '', hash = '';
    if (m) {
      scheme = m[1].toLowerCase();
      auth = m[2] || '';
      path = m[3] || '';
      query = m[4] || '';
      hash = m[5] || '';
    } else if (rawBase) {
      var bm = urlRe.exec(rawBase);
      if (!bm) throw new TypeError('Invalid URL: ' + raw);
      scheme = bm[1].toLowerCase();
      auth = bm[2] || '';
      var bp = bm[3] || '';
      var bq = bm[4] || '';
      if (raw.charAt(0) === '#') {
        path = bp; query = bq; hash = raw.slice(1);
      } else if (raw.charAt(0) === '?') {
        path = bp; query = raw.slice(1); hash = '';
      } else if (raw.indexOf('//') === 0) {
        var rm = /^\/\/([^\/?#]*)([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/.exec(raw);
        if (!rm) throw new TypeError('Invalid URL: ' + raw);
        auth = rm[1]; path = rm[2] || ''; query = rm[3] || ''; hash = rm[4] || '';
      } else {
        var before = raw.split(/[?#]/)[0];
        var baseDir = bp;
        var slash = baseDir.lastIndexOf('/');
        if (slash < 0) baseDir = '/'; else baseDir = baseDir.slice(0, slash + 1);
        path = before.charAt(0) === '/' ? before : baseDir + before;
        var qm = /^[^?#]*\?([^#]*)(?:#(.*))?$/.exec(raw);
        query = qm ? (qm[1] || '') : '';
        hash = qm ? (qm[2] || '') : '';
      }
    } else {
      throw new TypeError('Invalid URL: ' + raw);
    }
    var hostPort = auth;
    var at = hostPort.lastIndexOf('@');
    if (at >= 0) hostPort = hostPort.slice(at + 1);
    var host = hostPort, port = '';
    var colon = hostPort.lastIndexOf(':');
    if (colon >= 0 && hostPort.indexOf(']') < colon) {
      host = hostPort.slice(0, colon);
      port = hostPort.slice(colon + 1);
    }
    this._scheme = scheme;
    this._host = host;
    this._port = port;
    this._path = path || '/';
    this._query = query;
    this._hash = hash;
    this._params = new URLSearchParamsImpl(query);
  }
  function hrefOf(u) {
    var out = u._scheme + ':';
    if (u._host) {
      out += '//' + u._host;
      if (u._port) out += ':' + u._port;
    }
    out += u._path || '/';
    if (u._query) out += '?' + u._query;
    if (u._hash) out += '#' + u._hash;
    return out;
  }
  URLImpl.prototype = {
    get href() { return hrefOf(this); },
    set href(v) { var u = new URLImpl(v); this._scheme = u._scheme; this._host = u._host; this._port = u._port; this._path = u._path; this._query = u._query; this._hash = u._hash; this._params = u._params; },
    get protocol() { return this._scheme + ':'; },
    get host() { return this._host + (this._port ? ':' + this._port : ''); },
    get hostname() { return this._host; },
    get port() { return this._port; },
    get pathname() { return this._path || '/'; },
    get search() { return this._query ? '?' + this._query : ''; },
    get hash() { return this._hash ? '#' + this._hash : ''; },
    get origin() { return this._host ? this._scheme + '://' + this.host : 'null'; },
    get searchParams() { return this._params; },
    toString: function () { return hrefOf(this); },
    toJSON: function () { return hrefOf(this); }
  };
  URLImpl.parse = function (u) { return new URLImpl(u); };
  globalThis.URL = URLImpl;
  globalThis.URLSearchParams = URLSearchParamsImpl;
})();
"#;
    context
        .eval(Source::from_bytes(prelude.as_bytes()))
        .map_err(map_js_error)?;

    // 书源 JS 库（legado SharedJsScope：jsLib 定义 AES_KEY/sign/URL 等共享全局——
    // header/搜索/正文 JS 直接引用不报 ReferenceError）。书源变量顶层键也注入全局
    // （同名被后续 vars 覆盖，语义与 legado bindings 覆盖 prototype 一致）。
    let js_lib = bridge.inner.js_lib.clone();
    if !js_lib.trim().is_empty() {
        context
            .eval(Source::from_bytes(js_lib.as_bytes()))
            .map_err(map_js_error)?;
    }
    if let Ok(variable_map) =
        serde_json::from_str::<serde_json::Value>(&bridge.inner.source_variable)
    {
        if let Some(obj) = variable_map.as_object() {
            for (name, value) in obj {
                if !is_js_identifier(name) {
                    continue;
                }
                let text = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    other => other.to_string(),
                };
                context
                    .register_global_property(
                        JsString::from(name.as_str()),
                        JsValue::from(JsString::from(text)),
                        Attribute::all(),
                    )
                    .map_err(|e| anyhow!("书源变量注入失败 [{name}]: {e}"))?;
            }
        }
    }

    // 默认全局变量（vars 注入同名覆盖——URL 构造/规则 eval 的显式 baseUrl 优先）
    let key = bridge.inner.source_key.clone();
    let host = url::Url::parse(&key)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default();
    for (name, value) in [
        ("baseUrl", key.clone()),
        ("base_url", key.clone()),
        ("src", key),
        ("type", String::new()),
        ("urlIP", host),
    ] {
        context
            .register_global_property(
                JsString::from(name),
                JsValue::from(JsString::from(value)),
                Attribute::all(),
            )
            .map_err(|e| anyhow!("JS 全局注入失败 [{name}]: {e}"))?;
    }

    // cookie 对象（legado CookieStore shim——按用户命名空间读写爬虫层 cookie）
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
        .map_err(|e| anyhow!("cookie 对象注册失败: {e}"))?;

    // cache 对象（E11：legacy CacheManager shim——书源级跨请求持久缓存，按用户隔离）
    let cache_obj = ObjectInitializer::new(context)
        .function(bind(bridge, cache_js_get), JsString::from("get"), 1)
        .function(bind(bridge, cache_js_put), JsString::from("put"), 3)
        .function(bind(bridge, cache_js_get_int), JsString::from("getInt"), 2)
        .function(bind(bridge, cache_js_put_int), JsString::from("putInt"), 3)
        .function(
            bind(bridge, cache_js_get_long),
            JsString::from("getLong"),
            2,
        )
        .function(
            bind(bridge, cache_js_put_long),
            JsString::from("putLong"),
            3,
        )
        .function(
            bind(bridge, cache_js_get_double),
            JsString::from("getDouble"),
            2,
        )
        .function(
            bind(bridge, cache_js_put_double),
            JsString::from("putDouble"),
            3,
        )
        .function(
            bind(bridge, cache_js_get_float),
            JsString::from("getFloat"),
            2,
        )
        .function(
            bind(bridge, cache_js_put_float),
            JsString::from("putFloat"),
            3,
        )
        .function(bind(bridge, cache_js_delete), JsString::from("delete"), 1)
        // 旧 JS 版 cache shim 的方法别名（保持既有书源/测试兼容）
        .function(bind(bridge, cache_js_put), JsString::from("set"), 2)
        .function(
            bind(bridge, cache_js_get),
            JsString::from("getFromMemory"),
            1,
        )
        .function(bind(bridge, cache_js_put), JsString::from("putToMemory"), 2)
        .function(
            bind(bridge, cache_js_delete),
            JsString::from("deleteMemory"),
            1,
        )
        .function(bind(bridge, cache_js_clear), JsString::from("clear"), 0)
        .build();
    context
        .register_global_property(JsString::from("cache"), cache_obj, Attribute::all())
        .map_err(|e| anyhow!("cache 对象注册失败: {e}"))?;

    // org.jsoup.Jsoup.parse(html)：Document/Elements shim（scraper 后端）
    let jsoup = ObjectInitializer::new(context)
        .function(
            unsafe { NativeFunction::from_closure(jsoup_parse) },
            JsString::from("parse"),
            1,
        )
        .build();
    let org = ObjectInitializer::new(context)
        .property(JsString::from("jsoup"), jsoup, Attribute::all())
        .build();
    context
        .register_global_property(JsString::from("org"), org, Attribute::all())
        .map_err(|e| anyhow!("org 对象注册失败: {e}"))?;

    // xGorgon：字节系签名 stub（真实算法不可用——返回空串，避免 ReferenceError）
    register_global_fn(context, "xGorgon", xgorgon_stub, 1)?;
    // gzip(text)：GZip 压缩 → base64（legado 书源常用：`gzip(JSON.stringify(...))`）
    register_global_fn(context, "gzip", gzip_base64, 1)?;
    // getWbiEnc：bilibili wbi 签名（真实实现——nav 密钥 + mixinKey + wts/w_rid）
    register_global_fn(context, "getWbiEnc", get_wbi_enc, 1)?;
    // Reload(url)：拉取远程文本（书源远程 JS 加载模式）
    register_global_fn(context, "Reload", reload_fetch, 1)?;
    // application：旧版阅读 Android 环境兼容（SharedPreferences/文件目录 shim）
    let application = install_application_shim(context, bridge)?;
    // Java/Android 常用全局兼容（URLEncoder/UUID/Base64/Log/System/context 等）
    install_java_utils_shim(context, application)?;
    Ok(())
}

/// 注册全局函数
fn register_global_fn(
    context: &mut Context,
    name: &str,
    f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
    len: usize,
) -> Result<()> {
    let func =
        FunctionObjectBuilder::new(context.realm(), unsafe { NativeFunction::from_closure(f) })
            .name(name)
            .length(len)
            .build();
    context
        .register_global_property(JsString::from(name), func, Attribute::all())
        .map_err(|e| anyhow!("JS 全局函数注册失败 [{name}]: {e}"))
}

/// 解析 cookie 串为键值映射（legado CookieStore.cookieToMap）
fn cookie_map(cookie: &str) -> HashMap<String, String> {
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

// ---- E11 cache 对象处理器（legacy CacheManager shim）----

fn cache_js_get(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    match cache_get_value(&inner.ns, &key) {
        Some(v) => Ok(JsValue::from(JsString::from(v))),
        None => Ok(JsValue::null()),
    }
}

fn cache_js_put(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), _context);
    let value = js_value_to_string(args.get_or_undefined(1), _context);
    let save_time = args
        .get(2)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    cache_put_value(&inner.ns, &key, value, save_time);
    Ok(JsValue::undefined())
}

fn cache_num_get(inner: &JsBridgeInner, args: &[JsValue], default_val: f64) -> f64 {
    let key = js_value_to_string(args.get_or_undefined(0), &mut Context::default());
    let def = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n)
        .unwrap_or(default_val);
    match cache_get_value(&inner.ns, &key) {
        Some(v) => v.trim().parse::<f64>().unwrap_or(def),
        None => def,
    }
}

fn cache_js_get_int(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0) as i32;
    Ok(JsValue::Integer(n))
}

fn cache_js_put_int(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), _context);
    let v = args
        .get(1)
        .and_then(|x| x.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    let save_time = args
        .get(2)
        .and_then(|x| x.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    cache_put_value(&inner.ns, &key, v.to_string(), save_time);
    Ok(JsValue::undefined())
}

fn cache_js_get_long(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0);
    Ok(JsValue::from(n))
}

fn cache_js_put_long(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), _context);
    let v = args
        .get(1)
        .and_then(|x| x.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    let save_time = args
        .get(2)
        .and_then(|x| x.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    cache_put_value(&inner.ns, &key, v.to_string(), save_time);
    Ok(JsValue::undefined())
}

fn cache_js_get_double(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0);
    Ok(JsValue::from(n))
}

fn cache_js_put_double(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), _context);
    let v = args.get(1).and_then(|x| x.as_number()).unwrap_or(0.0);
    let save_time = args
        .get(2)
        .and_then(|x| x.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    cache_put_value(&inner.ns, &key, format!("{v}"), save_time);
    Ok(JsValue::undefined())
}

fn cache_js_get_float(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0);
    Ok(JsValue::from(n))
}

fn cache_js_put_float(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), _context);
    let v = args.get(1).and_then(|x| x.as_number()).unwrap_or(0.0);
    let save_time = args
        .get(2)
        .and_then(|x| x.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    cache_put_value(&inner.ns, &key, format!("{v}"), save_time);
    Ok(JsValue::undefined())
}

fn cache_js_delete(
    inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), _context);
    CACHE_STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&cache_store_key(&inner.ns, &key));
    // EG4：同步删库（失败仅告警——下次启动可能恢复旧值，与内存删除后重 put 的语义一致）
    if let Some(storage) = js_cache_registered() {
        let db_ns = cache_db_ns(&inner.ns).to_string();
        let fut = async move { storage.delete_js_cache(&db_ns, &key).await };
        if let Err(e) = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cache.delete") {
            tracing::warn!("cache.delete 删库失败（仅内存）: {e}");
        }
    }
    Ok(JsValue::undefined())
}

fn cache_js_clear(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let sep = char::from_u32(0x1).unwrap_or('\u{1f}');
    let prefix = format!(
        "{}{sep}",
        if inner.ns.is_empty() {
            "default"
        } else {
            &inner.ns
        }
    );
    CACHE_STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|k, _| !k.starts_with(&prefix));
    // EG4：同步清库（仅本命名空间；失败仅告警）
    if let Some(storage) = js_cache_registered() {
        let db_ns = cache_db_ns(&inner.ns).to_string();
        let fut = async move { storage.clear_js_cache(&db_ns).await };
        if let Err(e) = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "cache.clear") {
            tracing::warn!("cache.clear 清库失败（仅内存）: {e}");
        }
    }
    Ok(JsValue::undefined())
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
    let key = key.clone();
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

/// xGorgon(...)：字节系请求签名 stub（无法复现——返回空串）
fn xgorgon_stub(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from("")))
}

/// 全局 gzip(text)：UTF-8 → GZip → base64（空输入返回空串）
fn gzip_base64(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    use std::io::Write as _;
    let text = js_value_to_string(args.get_or_undefined(0), context);
    if text.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let out = enc
        .write_all(text.as_bytes())
        .and_then(|_| enc.finish())
        .map(|bytes| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes))
        .unwrap_or_default();
    Ok(JsValue::from(JsString::from(out)))
}

/// Reload(url)：拉取远程内容（书源远程 JS 加载：`eval(String(Reload('...')))`）
fn reload_fetch(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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
fn get_wbi_enc(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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

/// 构建 java / source 对象（ObjectInitializer：function 注册方法、property 挂子对象）
fn build_bridge_objects(bridge: &JsBridge, context: &mut Context) -> Result<(JsObject, JsObject)> {
    // java.headerMap：请求头 Map（底层为 bridge.headers，eval 后可读取）
    let mut header_map = ObjectInitializer::new(context);
    header_map
        .function(bind(bridge, header_map_put), JsString::from("put"), 2)
        .function(bind(bridge, header_map_get), JsString::from("get"), 1)
        .function(bind(bridge, header_map_size), JsString::from("size"), 0);
    let header_map = header_map.build();

    // java：put/get（临时变量）、log（tracing）、headerMap（请求头）、
    // encodeURI/ajax/startBrowserAwait/setContent/getString/getElements/getWebViewUA（legacy shim）
    // + 签名/编码/转换 shim：md5Encode/HMacHex/randomUUID/androidId/base64Encode/base64DecodeToString/
    //   hexDecodeToString/t2s/s2t/desEncodeToBase64String/get(url)/connect/head
    let mut java = ObjectInitializer::new(context);
    java.function(bind(bridge, java_put), JsString::from("put"), 2)
        .function(bind(bridge, java_get), JsString::from("get"), 1)
        .function(
            bind(bridge, java_get_cookie),
            JsString::from("getCookie"),
            2,
        )
        .function(bind(bridge, java_log), JsString::from("log"), 1)
        .function(bind(bridge, java_toast), JsString::from("toast"), 1)
        .function(bind(bridge, java_toast), JsString::from("longToast"), 1)
        .function(bind(bridge, java_toast), JsString::from("shortToast"), 1)
        .function(
            bind(bridge, java_time_format),
            JsString::from("timeFormat"),
            1,
        )
        .function(
            bind(bridge, java_time_format_utc),
            JsString::from("timeFormatUTC"),
            3,
        )
        .function(
            unsafe { NativeFunction::from_closure(java_aes_decrypt) },
            JsString::from("aesBase64DecodeToString"),
            4,
        )
        .function(
            bind(bridge, java_encode_uri),
            JsString::from("encodeURI"),
            2,
        )
        .function(bind(bridge, java_ajax), JsString::from("ajax"), 2)
        .function(bind(bridge, java_post), JsString::from("post"), 3)
        .function(bind(bridge, java_ajax_all), JsString::from("ajaxAll"), 2)
        .function(
            bind(bridge, java_start_browser_await),
            JsString::from("startBrowserAwait"),
            3,
        )
        .function(
            bind(bridge, java_set_content),
            JsString::from("setContent"),
            1,
        )
        .function(
            bind(bridge, java_get_string),
            JsString::from("getString"),
            1,
        )
        .function(
            bind(bridge, java_get_elements),
            JsString::from("getElements"),
            1,
        )
        .function(
            bind(bridge, java_get_webview_ua),
            JsString::from("getWebViewUA"),
            0,
        )
        // 签名/编码 shim（legado java.* 常见缺项——按报错消息逐项补齐）
        .function(
            bind(bridge, java_md5_encode),
            JsString::from("md5Encode"),
            1,
        )
        .function(bind(bridge, java_hmac_hex), JsString::from("HMacHex"), 3)
        .function(
            bind(bridge, java_random_uuid),
            JsString::from("randomUUID"),
            0,
        )
        .function(
            bind(bridge, java_android_id),
            JsString::from("androidId"),
            0,
        )
        .function(
            bind(bridge, java_base64_encode_flags),
            JsString::from("base64Encode"),
            2,
        )
        .function(
            bind(bridge, java_base64_decode_flags),
            JsString::from("base64DecodeToString"),
            2,
        )
        // legacy base64Decode 返回 ByteArray（number[]）
        .function(
            bind(bridge, java_base64_decode_to_byte_array),
            JsString::from("base64Decode"),
            2,
        )
        .function(
            bind(bridge, java_base64_decode_to_byte_array),
            JsString::from("base64DecodeToByteArray"),
            2,
        )
        // E16：legacy 摘要 base64 版 + logType（base64 flags 变体已并入原名绑定）
        .function(
            bind(bridge, java_digest_base64_str),
            JsString::from("digestBase64Str"),
            2,
        )
        .function(bind(bridge, java_log_type), JsString::from("logType"), 1)
        .function(
            bind(bridge, java_hex_decode),
            JsString::from("hexDecodeToString"),
            1,
        )
        .function(bind(bridge, java_t2s), JsString::from("t2s"), 1)
        .function(bind(bridge, java_s2t), JsString::from("s2t"), 1)
        .function(
            bind(bridge, java_des_encode_to_base64_string),
            JsString::from("desEncodeToBase64String"),
            4,
        )
        .function(
            bind(bridge, java_create_symmetric_crypto),
            JsString::from("createSymmetricCrypto"),
            3,
        )
        // legado JsExtensions 完整集：编码/加解密/文件/zip/TTF
        .function(bind(bridge, java_escape), JsString::from("escape"), 1)
        .function(bind(bridge, java_unescape), JsString::from("unescape"), 1)
        .function(
            bind(bridge, java_utf8_to_gbk),
            JsString::from("utf8ToGbk"),
            1,
        )
        .function(
            bind(bridge, java_digest_hex),
            JsString::from("digestHex"),
            2,
        )
        .function(
            bind(bridge, java_md5_encode16),
            JsString::from("md5Encode16"),
            1,
        )
        .function(
            bind(bridge, java_aes_decode_to_string),
            JsString::from("aesDecodeToString"),
            4,
        )
        .function(
            bind(bridge, java_aes_base64_decode_to_string),
            JsString::from("aesBase64DecodeToString"),
            4,
        )
        .function(
            bind(bridge, java_aes_encode_to_base64_string),
            JsString::from("aesEncodeToBase64String"),
            4,
        )
        .function(
            bind(bridge, java_aes_encode_to_string),
            JsString::from("aesEncodeToString"),
            4,
        )
        .function(
            bind(bridge, java_aes_decode_to_byte_array),
            JsString::from("aesDecodeToByteArray"),
            4,
        )
        .function(
            bind(bridge, java_aes_base64_decode_to_byte_array),
            JsString::from("aesBase64DecodeToByteArray"),
            4,
        )
        .function(
            bind(bridge, java_aes_encode_to_byte_array),
            JsString::from("aesEncodeToByteArray"),
            4,
        )
        .function(
            bind(bridge, java_aes_encode_to_base64_byte_array),
            JsString::from("aesEncodeToBase64ByteArray"),
            4,
        )
        .function(
            bind(bridge, java_aes_decode_args_base64_str),
            JsString::from("aesDecodeArgsBase64Str"),
            5,
        )
        .function(
            bind(bridge, java_aes_encode_args_base64_str),
            JsString::from("aesEncodeArgsBase64Str"),
            5,
        )
        .function(
            bind(bridge, java_des_decode_to_string),
            JsString::from("desDecodeToString"),
            4,
        )
        .function(
            bind(bridge, java_des_base64_decode_to_string),
            JsString::from("desBase64DecodeToString"),
            4,
        )
        .function(
            bind(bridge, java_des_encode_to_string),
            JsString::from("desEncodeToString"),
            4,
        )
        .function(
            bind(bridge, java_triple_des_decode_str),
            JsString::from("tripleDESDecodeStr"),
            5,
        )
        .function(
            bind(bridge, java_triple_des_decode_args_base64_str),
            JsString::from("tripleDESDecodeArgsBase64Str"),
            5,
        )
        .function(
            bind(bridge, java_triple_des_encode_base64_str),
            JsString::from("tripleDESEncodeBase64Str"),
            5,
        )
        .function(
            bind(bridge, java_triple_des_encode_args_base64_str),
            JsString::from("tripleDESEncodeArgsBase64Str"),
            5,
        )
        .function(
            bind(bridge, java_cache_file),
            JsString::from("cacheFile"),
            2,
        )
        .function(
            bind(bridge, java_download_file),
            JsString::from("downloadFile"),
            2,
        )
        .function(bind(bridge, java_get_file), JsString::from("getFile"), 1)
        .function(bind(bridge, java_read_file), JsString::from("readFile"), 1)
        .function(
            bind(bridge, java_read_txt_file),
            JsString::from("readTxtFile"),
            2,
        )
        .function(
            bind(bridge, java_delete_file),
            JsString::from("deleteFile"),
            1,
        )
        .function(
            bind(bridge, java_unzip_file),
            JsString::from("unzipFile"),
            1,
        )
        .function(
            bind(bridge, java_get_txt_in_folder),
            JsString::from("getTxtInFolder"),
            1,
        )
        .function(
            bind(bridge, java_get_zip_string_content),
            JsString::from("getZipStringContent"),
            3,
        )
        .function(
            bind(bridge, java_get_zip_byte_array_content),
            JsString::from("getZipByteArrayContent"),
            2,
        )
        .function(
            bind(bridge, java_import_script),
            JsString::from("importScript"),
            1,
        )
        .function(bind(bridge, java_web_view), JsString::from("webView"), 3)
        .function(
            bind(bridge, java_html_format),
            JsString::from("htmlFormat"),
            1,
        )
        .function(
            bind(bridge, java_query_base64_ttf),
            JsString::from("queryBase64TTF"),
            1,
        )
        .function(bind(bridge, java_query_ttf), JsString::from("queryTTF"), 1)
        .function(
            bind(bridge, java_replace_font),
            JsString::from("replaceFont"),
            3,
        )
        .function(bind(bridge, java_connect), JsString::from("connect"), 1)
        .function(bind(bridge, java_head), JsString::from("head"), 2)
        .property(JsString::from("headerMap"), header_map, Attribute::all());
    let java = java.build();
    // java.net/java.util/java.lang：旧版书源直接引用 Java 类的命名空间
    attach_java_namespaces(&java, context)?;

    // source：getKey（书源 URL）/ getName（书源名）/ put/get（书源级变量）
    // + key/url（URL 别名）/ loginUrl（登录 JS）/ header（header 文本）/ getVariable（书源变量）
    let key = JsValue::from(JsString::from(bridge.inner.source_key.as_str()));
    let mut source = ObjectInitializer::new(context);
    source
        .function(bind(bridge, source_get_key), JsString::from("getKey"), 0)
        .function(bind(bridge, source_get_name), JsString::from("getName"), 0)
        .function(bind(bridge, source_put), JsString::from("put"), 2)
        .function(bind(bridge, source_get), JsString::from("get"), 1)
        .function(
            bind(bridge, source_put_login_header),
            JsString::from("putLoginHeader"),
            1,
        )
        .function(
            bind(bridge, source_remove_login_header),
            JsString::from("removeLoginHeader"),
            0,
        )
        .function(
            bind(bridge, source_get_login_header),
            JsString::from("getLoginHeader"),
            0,
        )
        .function(
            bind(bridge, source_get_variable),
            JsString::from("getVariable"),
            0,
        )
        .property(JsString::from("key"), key.clone(), Attribute::all())
        .property(JsString::from("url"), key.clone(), Attribute::all())
        .property(JsString::from("bookSourceUrl"), key, Attribute::all())
        .property(
            JsString::from("loginUrl"),
            JsValue::from(JsString::from(bridge.inner.login_url.as_str())),
            Attribute::all(),
        )
        .property(
            JsString::from("header"),
            JsValue::from(JsString::from(bridge.inner.source_header.as_str())),
            Attribute::all(),
        );
    let source = source.build();

    Ok((java, source))
}

/// 将 bridge 状态绑定进 NativeFunction 闭包。
///
/// boa 0.19 的 `from_copy_closure_with_captures` 要求捕获类型实现 `Trace`，
/// 而 `std::sync::Mutex` 无 Trace 实现，故走 `from_closure`。
///
/// # Safety
///
/// `from_closure` 的不安全前提是「捕获变量含需 GC 追踪（Trace）的数据」；
/// 此处仅捕获 `Arc<JsBridgeInner>`，内部全是 String / Mutex<HashMap<String, String>>，
/// 不含 JsValue / JsObject / Gc 等需追踪数据，闭包生命周期由 Arc 管理，无 use-after-free。
fn bind<F>(bridge: &JsBridge, f: F) -> NativeFunction
where
    F: Fn(&JsBridgeInner, &[JsValue], &mut Context) -> JsResult<JsValue> + 'static,
{
    let inner = Arc::clone(&bridge.inner);
    unsafe { NativeFunction::from_closure(move |_this, args, ctx| f(&inner, args, ctx)) }
}

// ---- java.* 实现 ----

/// java.put(key, value)：bridge 生命周期内的临时变量
fn java_put(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_string(args.get_or_undefined(1), context);
    inner
        .java_vars
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, value);
    Ok(JsValue::undefined())
}

/// java.get(key)：读取临时变量，缺失返回 undefined。
/// java.get(url, opts) / java.get(httpUrl)：HTTP GET（legado jsHelp 兼容：
/// 无忧书城 `java.get(su,{}).headers('Location')`）——返回响应对象
fn java_get(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    if args.len() > 1 || key.starts_with("http://") || key.starts_with("https://") {
        return java_http_fetch(inner, &key, "GET", context);
    }
    let value = inner
        .java_vars
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    Ok(value.map_or_else(JsValue::undefined, |s| JsValue::from(JsString::from(s))))
}

/// java.getCookie(url, key?=null)：读取书源 cookie（与 cookie 对象同源）
fn java_get_cookie(
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

/// java.timeFormat(time)：毫秒时间戳 → `yyyy/MM/dd HH:mm`（legado AppConst.dateFormat）
fn java_time_format(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let t = js_value_to_i64(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(format_time_millis(
        t,
        "yyyy/MM/dd HH:mm",
        0,
    ))))
}

/// java.timeFormatUTC(time, format, sh)：按指定格式 + 时区毫秒偏移格式化
/// （legado SimpleTimeZone(sh, "UTC")——sh 为毫秒，如 28800000 = UTC+8）
fn java_time_format_utc(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let t = js_value_to_i64(args.get_or_undefined(0), context);
    let format = js_value_to_string(args.get_or_undefined(1), context);
    let sh = js_value_to_i64(args.get_or_undefined(2), context);
    Ok(JsValue::from(JsString::from(format_time_millis(
        t, &format, sh,
    ))))
}

/// 毫秒时间戳 → Java SimpleDateFormat 风格格式化（支持 yyyy/MM/dd/HH/mm/ss/SSS，
/// 时区偏移毫秒；失败返回空串——legado 解析失败返回 null）
fn format_time_millis(millis: i64, pattern: &str, offset_ms: i64) -> String {
    let Some(dt) = chrono::DateTime::from_timestamp_millis(millis) else {
        return String::new();
    };
    let dt = dt + chrono::Duration::milliseconds(offset_ms);
    let mut fmt = String::new();
    let mut i = 0usize;
    while i < pattern.len() {
        let c = pattern[i..].chars().next().unwrap();
        let n = pattern[i..].chars().take_while(|&x| x == c).count();
        match c {
            'y' => {
                fmt.push_str(if n >= 4 {
                    "%Y"
                } else if n == 2 {
                    "%y"
                } else {
                    "%Y"
                });
            }
            'M' => fmt.push_str(if n >= 2 { "%m" } else { "%-m" }),
            'd' => fmt.push_str(if n >= 2 { "%d" } else { "%-d" }),
            'H' => fmt.push_str(if n >= 2 { "%H" } else { "%-H" }),
            'm' => fmt.push_str(if n >= 2 { "%M" } else { "%-M" }),
            's' => fmt.push_str(if n >= 2 { "%S" } else { "%-S" }),
            'S' => fmt.push_str("%3f"),
            '%' => fmt.push_str("%%"),
            _ => fmt.push(c),
        }
        i += n * c.len_utf8();
    }
    dt.format(&fmt).to_string()
}

/// JsValue → i64（数字直接取；字符串按数字解析；失败 → 0）
fn js_value_to_i64(v: &JsValue, context: &mut Context) -> i64 {
    match v {
        JsValue::Integer(i) => i64::from(*i),
        JsValue::Rational(r) => *r as i64,
        JsValue::BigInt(b) => b.to_f64() as i64,
        _ => js_value_to_string(v, context).parse::<i64>().unwrap_or(0),
    }
}

/// java.log(msg)：tracing 日志（调试书源规则）
/// java.toast/longToast/shortToast：无 UI 环境提示（记日志）
fn java_toast(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let msg = args
        .first()
        .map(|v| js_value_to_string(v, context))
        .unwrap_or_default();
    tracing::debug!("java.toast: {}", msg);
    Ok(JsValue::undefined())
}

fn java_log(_inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let msg = js_value_to_string(args.get_or_undefined(0), context);
    tracing::info!(target: "reader.js", "[java.log] {}", msg);
    Ok(JsValue::undefined())
}

// ---- legacy shim：encodeURI / ajax / startBrowserAwait / setContent / getString / getElements / getWebViewUA ----

/// 构造 JS 异常（NativeFunction 内 throw，书源规则可 catch）
fn js_native_error(msg: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(msg.into()).into()
}

/// JS 桥接同步等待上限（M6：60s → 10s）——书源规则并发执行时，单次桥接调用对
/// axum worker 线程的阻塞占用最多 10s；超时后调用方立即返回错误、不再等待工作线程
/// （工作线程继续运行至其内部 fetch 超时后自行退出，不占用 worker）。
const BRIDGE_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// 同步等待异步任务结果（boa 引擎为同步调用环境；`java.ajax`/`java.startBrowserAwait`
/// 需要等待网络/浏览器结果）。
///
/// 阻塞说明：书源 JS 通常在 tokio 工作线程（axum handler）中执行，本函数用专用
/// worker 线程 + 独立 current_thread tokio runtime 执行异步任务，当前线程经
/// std mpsc `recv_timeout` 阻塞等待（最多 `timeout`，调用方统一传 [`BRIDGE_WAIT_TIMEOUT`]）。
/// 不在 ambient runtime 上直接 spawn——避免 current_thread runtime（如 `#[tokio::test]`）下
/// 「阻塞唯一 worker → 任务永不被轮询」的死锁；阻塞仅限当前执行 JS 的线程，多线程 runtime 的
/// 其他 worker 不受影响。超时路径：调用方立即返回错误，不再等待工作线程回收（M6）。
pub(crate) fn block_on_task<T: Send + 'static>(
    fut: impl Future<Output = Result<T>> + Send + 'static,
    timeout: Duration,
    what: &str,
) -> Result<T> {
    let what = what.to_string();
    let what_msg = what.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Result<T>>();
    std::thread::Builder::new()
        .name(format!("reader-js-{what}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(anyhow!("{what_msg} 内部 runtime 创建失败: {e}")));
                    return;
                }
            };
            let _ = tx.send(rt.block_on(fut));
        })
        .map_err(|e| anyhow!("{what} worker 线程创建失败: {e}"))?;
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(anyhow!("{what} 超时（{}s）", timeout.as_secs()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow!("{what} worker 异常退出"))
        }
    }
}

/// 内置浏览器可用性（`java.startBrowserAwait` 前置检查；测试钩子可强制覆盖）。
/// GAP 175：camoufox 后端启用（默认）时视为可用——求解链会在 CDP 失败/不可用时
/// 自动走 camoufox（HTTP 后端），前置检查不再因缺浏览器直接拦截。
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
    crate::service::browser::is_browser_available() || crate::service::camoufox::enabled()
}

/// 测试钩子：强制浏览器可用性（Some(true)/Some(false) 强制；None 恢复自动探测）
#[cfg(test)]
fn force_js_browser_available(v: Option<bool>) {
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
type SolveHook = dyn Fn(String, Vec<(String, String)>) -> Result<(String, Vec<(String, String)>, String)>
    + Send
    + Sync;

#[cfg(test)]
static JS_SOLVE_HOOK: LazyLock<Mutex<Option<Arc<SolveHook>>>> = LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
fn register_js_solve_hook(hook: Option<Arc<SolveHook>>) {
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
fn java_start_browser_await(
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

/// `java.encodeURI(str, charset)`：按 charset（默认 utf-8；gbk/gb2312 走 encoding_rs）
/// 对字符串做 URL 百分号编码。encodeURI 语义：ASCII 字母数字与 `-_.!~*'()` 及
/// `;/?:@&=+$,#` 保留不编码；其余字节逐个 `%XX`（大写）。
fn java_encode_uri(
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
fn java_ajax(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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
/// 复用 [`java_ajax_impl`] 完整管线：书源 header + cookie + `,{...}` 后缀解析
/// （后缀显式 method 覆盖 POST；body 优先取显式参数，其次后缀 body）。
fn java_post(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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
fn java_ajax_all(
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
    let body_for_fn = body.clone();
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

/// `java.setContent(html)`：设置当前解析文档（后续 getString/getElements 的解析源）
fn java_set_content(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let html = js_value_to_string(args.get_or_undefined(0), context);
    *inner.doc.lock().unwrap_or_else(|e| e.into_inner()) = Some(html);
    Ok(JsValue::undefined())
}

/// 当前文档（未 setContent → 明确错误提示）
fn current_doc(inner: &JsBridgeInner) -> JsResult<String> {
    inner
        .doc
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| js_native_error("尚未设置文档内容：请先调用 java.setContent(html)"))
}

/// 规则末段是否为属性/文本提取器（与 css_chain::is_attr_extractor 对齐）
fn rule_ends_with_extractor(rule: &str) -> bool {
    rule.split('@')
        .next_back()
        .map(|s| {
            matches!(
                s.trim(),
                "text"
                    | "textNodes"
                    | "ownText"
                    | "html"
                    | "all"
                    | "href"
                    | "src"
                    | "value"
                    | "data-src"
                    | "data-original"
                    | "data-url"
            )
        })
        .unwrap_or(false)
}

/// `java.getString(rule)`：对已存文档用 css_chain 规则求值，返回首个结果文本。
/// 规则末段是 `@text/@href` 等提取器时返回提取值；否则结果是元素 outerHTML，
/// 转文本（对齐 legado getString 的 getText 语义）；无匹配返回空串。
fn java_get_string(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let rule = js_value_to_string(args.get_or_undefined(0), context);
    let doc = current_doc(inner)?;
    let results = crate::parser::css_chain::css_chain(&rule, &doc);
    let text = match results.first() {
        Some(first) => {
            if rule_ends_with_extractor(&rule) {
                first.clone()
            } else {
                // 元素 HTML → 文本（对齐 search::field 语义）
                let f = scraper::Html::parse_fragment(first);
                let t = f
                    .root_element()
                    .text()
                    .collect::<String>()
                    .trim()
                    .to_string();
                if t.is_empty() {
                    first.clone()
                } else {
                    t
                }
            }
        }
        None => String::new(),
    };
    Ok(JsValue::from(JsString::from(text)))
}

/// `java.getElements(rule)`：对已存文档用 css_chain 规则求值。
/// - 规则带 `@` 提取器（@text/@href 等）→ 返回提取值字符串数组（原语义）
/// - 纯选择器规则 → 返回元素对象数组（jsoup 语义：`el.select(css)`/`el.text()`/`el.attr(name)`）
fn java_get_elements(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let rule = js_value_to_string(args.get_or_undefined(0), context);
    let doc = current_doc(inner)?;
    let results = crate::parser::css_chain::css_chain(&rule, &doc);
    if rule_ends_with_extractor(&rule) {
        let arr = JsArray::from_iter(
            results
                .into_iter()
                .map(|s| JsValue::from(JsString::from(s))),
            context,
        );
        return Ok(arr.into());
    }
    // 元素对象数组（jsoup Elements：select/text/attr/first/get/size/eachAttr/eachText/html/val）
    jsoup_elements_from_htmls(results, context)
}

/// `java.getWebViewUA()`：固定浏览器 UA（见 `JS_WEBVIEW_UA`）
fn java_get_webview_ua(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(JS_WEBVIEW_UA)))
}

// ---- 签名/编码/转换 shim（legado java.* 常见缺项） ----

/// java.md5Encode(str)：md5 十六进制（小写）
fn java_md5_encode(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(crate::util::md5::md5_encode(
        &s,
    ))))
}

/// java.HMacHex(data, algo, key)：HMAC 十六进制（HmacMD5/HmacSHA1/HmacSHA256/HmacSHA512）
fn java_hmac_hex(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_string(args.get_or_undefined(0), context);
    let algo = js_value_to_string(args.get_or_undefined(1), context);
    let key = js_value_to_string(args.get_or_undefined(2), context);
    use hmac::{Mac, SimpleHmac};
    let out: String = match algo.to_lowercase().as_str() {
        "hmacmd5" => hex::encode(
            SimpleHmac::<md5::Md5>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        "hmacsha1" => hex::encode(
            SimpleHmac::<sha1::Sha1>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        "hmacsha256" => hex::encode(
            SimpleHmac::<sha2::Sha256>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        "hmacsha512" => hex::encode(
            SimpleHmac::<sha2::Sha512>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        other => return Err(js_native_error(format!("HMacHex: 不支持的算法 {other}"))),
    };
    Ok(JsValue::from(JsString::from(out)))
}

/// java.randomUUID()：UUID v4 对象（toString() → 连字符小写）
fn java_random_uuid(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let u = uuid::Uuid::new_v4().to_string();
    let mut obj = ObjectInitializer::new(context);
    obj.function(
        unsafe {
            NativeFunction::from_closure(move |_this, _args, _ctx| {
                Ok(JsValue::from(JsString::from(u.clone())))
            })
        },
        JsString::from("toString"),
        0,
    );
    Ok(obj.build().into())
}

/// java.androidId()：稳定设备 ID（legado Android 环境常量）
fn java_android_id(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from("9774d56d682e549c")))
}

/// java.base64Encode(str)
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
fn java_base64_encode(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes()),
    )))
}

/// E16：Android Base64 flags → base64 引擎
/// （NO_PADDING=1 / NO_WRAP=2 / CRLF=4 / URL_SAFE=8；解码恒宽松——容忍换行/缺省填充）
fn b64_engine(flags: i64) -> base64::engine::GeneralPurpose {
    use base64::alphabet;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let alpha = if flags & 8 != 0 {
        alphabet::URL_SAFE
    } else {
        alphabet::STANDARD
    };
    let cfg = GeneralPurposeConfig::new()
        .with_encode_padding(flags & 1 == 0)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&alpha, cfg)
}

/// 手动 76 列折行（Android DEFAULT/CRLF 换行形态；NO_WRAP 不折）
fn b64_wrap(s: String, flags: i64) -> String {
    if flags & 2 != 0 || s.len() <= 76 {
        return s;
    }
    let eol = if flags & 4 != 0 { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(s.len() + s.len() / 76 * eol.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + 76).min(bytes.len());
        if i > 0 {
            out.push_str(eol);
        }
        out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or_default());
        i = end;
    }
    out
}

/// 解码前剥离空白（Android DEFAULT 输出带 \n——解码侧恒容忍）
fn b64_strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// java.base64Encode(str[, flags])
fn java_base64_encode_flags(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let flags = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    let encoded = b64_engine(flags).encode(s.as_bytes());
    Ok(JsValue::from(JsString::from(b64_wrap(encoded, flags))))
}

/// java.base64Decode(str[, flags]) → 文本（UTF-8 lossy）
fn java_base64_decode_flags(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let flags = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    match b64_engine(flags).decode(b64_strip_ws(&s).as_bytes()) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(_) => Err(js_native_error("java.base64Decode: base64 解码失败")),
    }
}

/// java.base64DecodeToByteArray(str[, flags]) → number[]（每元素 0-255）
fn java_base64_decode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let flags = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    match b64_engine(flags).decode(b64_strip_ws(&s).as_bytes()) {
        Ok(bytes) => {
            let arr = JsArray::from_iter(bytes.iter().map(|b| JsValue::from(*b as i32)), context);
            Ok(arr.into())
        }
        Err(_) => Err(js_native_error(
            "java.base64DecodeToByteArray: base64 解码失败",
        )),
    }
}

/// java.digestBase64Str(data, algorithm)：摘要 → base64（digestHex 的 base64 版）
fn java_digest_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    use base64::Engine as _;
    use sha1::Digest as _;
    let data = js_value_to_string(args.get_or_undefined(0), context);
    let algo = js_value_to_string(args.get_or_undefined(1), context).to_ascii_lowercase();
    let digest: Vec<u8> = match algo.as_str() {
        "md5" | "md-5" => hex::decode(crate::util::md5::md5_encode(&data)).unwrap_or_default(),
        "sha1" | "sha-1" => sha1::Sha1::digest(data.as_bytes()).to_vec(),
        "sha256" | "sha-256" => sha2::Sha256::digest(data.as_bytes()).to_vec(),
        "sha512" | "sha-512" => sha2::Sha512::digest(data.as_bytes()).to_vec(),
        other => {
            return Err(js_native_error(format!(
                "digestBase64Str: 不支持的算法 {other}"
            )))
        }
    };
    Ok(JsValue::from(JsString::from(
        base64::engine::general_purpose::STANDARD.encode(digest),
    )))
}

/// java.logType(any)：记录并返回值的 JS 类型名（调试用 stub）
fn java_log_type(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let v = args.get_or_undefined(0);
    let t = if v.is_null() {
        "null"
    } else if v.is_undefined() {
        "undefined"
    } else if v.is_string() {
        "string"
    } else if v.is_boolean() {
        "boolean"
    } else if v.is_number() {
        "number"
    } else if v.is_object() {
        "object"
    } else {
        "unknown"
    };
    tracing::debug!("java.logType: {t}");
    Ok(JsValue::from(JsString::from(t)))
}

/// java.base64DecodeToString(str)
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
fn java_base64_decode(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    match base64::engine::general_purpose::STANDARD.decode(s.as_bytes()) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(_) => Err(js_native_error(
            "java.base64DecodeToString: base64 解码失败",
        )),
    }
}

/// java.hexDecodeToString(str)：十六进制 → 文本
fn java_hex_decode(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    match hex::decode(s.trim()) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(_) => Err(js_native_error("java.hexDecodeToString: 十六进制解码失败")),
    }
}

/// java.t2s(str)：繁体 → 简体（zhconv 表）
fn java_t2s(_inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(zhconv::zhconv(
        &s,
        zhconv::Variant::ZhHans,
    ))))
}

/// java.s2t(str)：简体 → 繁体
fn java_s2t(_inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(zhconv::zhconv(
        &s,
        zhconv::Variant::ZhHant,
    ))))
}

// ---- 加密/编码 shim（legado JsExtensions / EncoderUtils 完整集） ----

/// Java transformation 字符串解析（AES/DES/DESede + ECB/CBC + PKCS5/7/NoPadding）
fn parse_transformation(transformation: &str) -> Result<(String, String, String), String> {
    let parts: Vec<&str> = transformation.split('/').collect();
    if parts.len() != 3 {
        return Err(format!("不支持的 transformation: {transformation}"));
    }
    let algo = parts[0].to_ascii_uppercase();
    let mode = parts[1].to_ascii_uppercase();
    let pad = parts[2].to_ascii_uppercase();
    let algo_ok = matches!(
        algo.as_str(),
        "AES" | "DES" | "DESEDE" | "TRIPLEDES" | "3DES"
    );
    let mode_ok = matches!(mode.as_str(), "ECB" | "CBC");
    let pad_ok = matches!(
        pad.as_str(),
        "PKCS5PADDING" | "PKCS7PADDING" | "PKCS5" | "PKCS7" | "NOPADDING" | "NO"
    );
    if !algo_ok || !mode_ok || !pad_ok {
        return Err(format!("不支持的 transformation: {transformation}"));
    }
    Ok((algo, mode, pad))
}

fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad = block_size - (data.len() % block_size);
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(pad as u8, pad));
    out
}

fn pkcs7_unpad(data: &[u8]) -> Option<Vec<u8>> {
    let last = *data.last()? as usize;
    if last == 0 || last > data.len() {
        return None;
    }
    if data[data.len() - last..]
        .iter()
        .all(|&b| b as usize == last)
    {
        Some(data[..data.len() - last].to_vec())
    } else {
        None
    }
}

/// 手动 CBC（兼容 cipher 0.4 / 0.5 两代 block cipher trait）
fn cbc_apply(
    encrypt_block: impl Fn(&[u8]) -> Result<Vec<u8>, String>,
    decrypt_block: impl Fn(&[u8]) -> Result<Vec<u8>, String>,
    data: &[u8],
    iv: &[u8],
    block_size: usize,
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    if data.is_empty() || !data.len().is_multiple_of(block_size) {
        return Err("CBC 数据长度必须为分块大小的整数倍".into());
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = iv.to_vec();
    for chunk in data.chunks(block_size) {
        if encrypt {
            let xored: Vec<u8> = chunk.iter().zip(prev.iter()).map(|(a, b)| a ^ b).collect();
            let enc = encrypt_block(&xored)?;
            out.extend_from_slice(&enc);
            prev = enc;
        } else {
            let dec = decrypt_block(chunk)?;
            let plain: Vec<u8> = dec.iter().zip(prev.iter()).map(|(a, b)| a ^ b).collect();
            out.extend_from_slice(&plain);
            prev = chunk.to_vec();
        }
    }
    Ok(out)
}

/// AES 加解密（ECB/CBC + PKCS5/7/NoPadding；key 16/24/32，iv 前 16 字节）
fn aes_crypt(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
    let (_, mode, pad) = parse_transformation(transformation)?;
    let padding = !matches!(pad.as_str(), "NOPADDING" | "NO");
    let key_len = match key.len() {
        16 | 24 | 32 => key.len(),
        _ => return Err(format!("AES key 长度必须为 16/24/32，实际 {}", key.len())),
    };
    let iv_bytes = if iv.is_empty() {
        vec![0u8; 16]
    } else {
        iv.iter().take(16).copied().collect::<Vec<u8>>()
    };
    let data = if encrypt && padding {
        pkcs7_pad(data, 16)
    } else {
        data.to_vec()
    };
    if data.is_empty() || data.len() % 16 != 0 {
        return Err("AES 数据长度不合法".into());
    }
    let block_enc = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(16);
        match key_len {
            16 => {
                let c = aes::Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes128>::clone_from_slice(block);
                c.encrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            24 => {
                let c = aes::Aes192::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes192>::clone_from_slice(block);
                c.encrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            32 => {
                let c = aes::Aes256::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes256>::clone_from_slice(block);
                c.encrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            _ => return Err("AES key 长度不合法".into()),
        }
        Ok(out)
    };
    let block_dec = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(16);
        match key_len {
            16 => {
                let c = aes::Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes128>::clone_from_slice(block);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            24 => {
                let c = aes::Aes192::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes192>::clone_from_slice(block);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            32 => {
                let c = aes::Aes256::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes256>::clone_from_slice(block);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            _ => return Err("AES key 长度不合法".into()),
        }
        Ok(out)
    };
    let raw = if mode == "CBC" {
        cbc_apply(block_enc, block_dec, &data, &iv_bytes, 16, encrypt)?
    } else {
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            let r = if encrypt {
                block_enc(chunk)?
            } else {
                block_dec(chunk)?
            };
            out.extend_from_slice(&r);
        }
        out
    };
    if encrypt || !padding {
        Ok(raw)
    } else {
        pkcs7_unpad(&raw).ok_or_else(|| "AES 解密填充校验失败".into())
    }
}

/// DES/DESede 加解密（ECB/CBC + PKCS5/7/NoPadding；cipher 0.5 block trait）
fn des_crypt(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    use des::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
    let (algo, mode, pad) = parse_transformation(transformation)?;
    let padding = !matches!(pad.as_str(), "NOPADDING" | "NO");
    let is_3des = matches!(algo.as_str(), "DESEDE" | "TRIPLEDES" | "3DES");
    let key_len = if is_3des { 24 } else { 8 };
    if key.len() != key_len {
        return Err(format!(
            "{} key 长度必须为 {key_len}，实际 {}",
            if is_3des { "3DES" } else { "DES" },
            key.len()
        ));
    }
    let iv_bytes = if iv.is_empty() {
        vec![0u8; 8]
    } else {
        iv.iter().take(8).copied().collect::<Vec<u8>>()
    };
    let data = if encrypt && padding {
        pkcs7_pad(data, 8)
    } else {
        data.to_vec()
    };
    if data.is_empty() || data.len() % 8 != 0 {
        return Err("DES 数据长度不合法".into());
    }
    let block_enc = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(8);
        if is_3des {
            let c = des::TdesEde3::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::TdesEde3>::clone_from_slice(block);
            c.encrypt_block(&mut b);
            out.extend_from_slice(&b);
        } else {
            let c = des::Des::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::Des>::clone_from_slice(block);
            c.encrypt_block(&mut b);
            out.extend_from_slice(&b);
        }
        Ok(out)
    };
    let block_dec = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(8);
        if is_3des {
            let c = des::TdesEde3::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::TdesEde3>::clone_from_slice(block);
            c.decrypt_block(&mut b);
            out.extend_from_slice(&b);
        } else {
            let c = des::Des::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::Des>::clone_from_slice(block);
            c.decrypt_block(&mut b);
            out.extend_from_slice(&b);
        }
        Ok(out)
    };
    let raw = if mode == "CBC" {
        cbc_apply(block_enc, block_dec, &data, &iv_bytes, 8, encrypt)?
    } else {
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(8) {
            let r = if encrypt {
                block_enc(chunk)?
            } else {
                block_dec(chunk)?
            };
            out.extend_from_slice(&r);
        }
        out
    };
    if encrypt || !padding {
        Ok(raw)
    } else {
        pkcs7_unpad(&raw).ok_or_else(|| "DES 解密填充校验失败".into())
    }
}

/// 通用对称加解密分发（AES/DES/DESede）
fn symmetric_crypt(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let (algo, _, _) = parse_transformation(transformation)?;
    match algo.as_str() {
        "AES" => aes_crypt(data, key, transformation, iv, encrypt),
        _ => des_crypt(data, key, transformation, iv, encrypt),
    }
}

/// `java.createSymmetricCrypto` 返回的 cipher 对象状态（`setIv` 可改写 iv）
struct SymmetricCryptoState {
    transformation: String,
    key: Vec<u8>,
    iv: Vec<u8>,
}

/// 对称密钥长度归一化：AES 16/24/32、DES 8、3DES 24；
/// 短键补零，16 字节 3DES 键按 TDEA-2 复制前 8 字节
fn normalize_symmetric_key(algo: &str, key: &[u8]) -> Vec<u8> {
    let len = match algo {
        "AES" => {
            if key.len() >= 32 {
                32
            } else if key.len() >= 24 {
                24
            } else {
                16
            }
        }
        "DES" => 8,
        _ => 24,
    };
    let mut out = vec![0u8; len];
    let n = key.len().min(len);
    out[..n].copy_from_slice(&key[..n]);
    if algo != "AES" && algo != "DES" && key.len() == 16 {
        out[16..24].copy_from_slice(&key[..8]);
    }
    out
}

fn normalize_symmetric_iv(algo: &str, iv: &[u8]) -> Vec<u8> {
    let block = if algo == "AES" { 16 } else { 8 };
    let mut out = vec![0u8; block];
    let n = iv.len().min(block);
    out[..n].copy_from_slice(&iv[..n]);
    out
}

/// legado `decrypt(data)` 的 data 编码识别：
/// ByteArray 原样，字符串按 `isHex`（偶数位全 hex）→ hex，否则 base64
fn symmetric_data_to_bytes(v: &JsValue, context: &mut Context) -> Vec<u8> {
    if v.as_object().is_some() {
        return js_value_to_bytes(v, context);
    }
    let s = js_value_to_string(v, context);
    let s = s.trim();
    let is_hex = !s.is_empty() && s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex {
        return hex::decode(s).unwrap_or_default();
    }
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .unwrap_or_default()
}

fn symmetric_cipher_do(
    state: &SymmetricCryptoState,
    data: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let (algo, _, _) = parse_transformation(&state.transformation)?;
    let key = normalize_symmetric_key(&algo, &state.key);
    let iv = normalize_symmetric_iv(&algo, &state.iv);
    symmetric_crypt(data, &key, &state.transformation, &iv, encrypt)
}

fn js_byte_array(bytes: Vec<u8>, context: &mut Context) -> JsValue {
    JsArray::from_iter(
        bytes.into_iter().map(|b| JsValue::from(u32::from(b))),
        context,
    )
    .into()
}

/// `java.createSymmetricCrypto(transformation, key, iv)`：
/// legado JsEncodeUtils 对称加密对象（decrypt/decryptStr/encrypt/encryptBase64/encryptHex/setIv）
fn java_create_symmetric_crypto(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let transformation = js_value_to_string(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let iv = js_value_to_bytes(args.get_or_undefined(2), context);
    let state = Arc::new(Mutex::new(SymmetricCryptoState {
        transformation,
        key,
        iv,
    }));

    let decrypt_state = Arc::clone(&state);
    let decrypt = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = decrypt_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = symmetric_data_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, false)
                .map(|bytes| js_byte_array(bytes, ctx))
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.decrypt: {e}")))
        })
    };

    let decrypt_str_state = Arc::clone(&state);
    let decrypt_str = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = decrypt_str_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = symmetric_data_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, false)
                .map(js_bytes_to_js_string)
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.decryptStr: {e}")))
        })
    };

    let encrypt_state = Arc::clone(&state);
    let encrypt = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = encrypt_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = js_value_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, true)
                .map(|bytes| js_byte_array(bytes, ctx))
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.encrypt: {e}")))
        })
    };

    let encrypt_b64_state = Arc::clone(&state);
    let encrypt_b64 = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = encrypt_b64_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = js_value_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, true)
                .map(|bytes| {
                    JsValue::from(JsString::from(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ))
                })
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.encryptBase64: {e}")))
        })
    };

    let encrypt_hex_state = Arc::clone(&state);
    let encrypt_hex = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = encrypt_hex_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = js_value_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, true)
                .map(|bytes| JsValue::from(JsString::from(hex::encode(bytes))))
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.encryptHex: {e}")))
        })
    };

    let set_iv_state = Arc::clone(&state);
    let set_iv = unsafe {
        NativeFunction::from_closure(move |this, args, ctx| {
            let iv = js_value_to_bytes(args.get_or_undefined(0), ctx);
            set_iv_state.lock().unwrap_or_else(|e| e.into_inner()).iv = iv;
            Ok(this.clone())
        })
    };

    let obj = ObjectInitializer::new(context)
        .function(decrypt, JsString::from("decrypt"), 1)
        .function(decrypt_str, JsString::from("decryptStr"), 1)
        .function(encrypt, JsString::from("encrypt"), 1)
        .function(encrypt_b64, JsString::from("encryptBase64"), 1)
        .function(encrypt_hex, JsString::from("encryptHex"), 1)
        .function(set_iv, JsString::from("setIv"), 1)
        .build();
    Ok(obj.into())
}

/// Java `escape`：ASCII 字母数字保留，其他 <256 用 %XX，>=256 用 %uXXXX
fn java_escape_impl(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let code = c as u32;
        if (code >= b'0' as u32 && code <= b'9' as u32)
            || (code >= b'a' as u32 && code <= b'z' as u32)
            || (code >= b'A' as u32 && code <= b'Z' as u32)
        {
            out.push(c);
        } else if code < 0x100 {
            out.push_str(&format!("%{code:02X}"));
        } else {
            out.push_str(&format!("%u{code:04X}"));
        }
    }
    out
}

/// Java `unescape`：%XX / %uXXXX
fn java_unescape_impl(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && bytes[i + 1] == b'u' {
            let hex = s.get(i + 2..i + 6).unwrap_or("");
            if let Ok(code) = u32::from_str_radix(hex, 16) {
                if let Some(c) = char::from_u32(code) {
                    out.push(c);
                    i += 6;
                    continue;
                }
            }
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = s.get(i + 1..i + 3).unwrap_or("");
            if let Ok(code) = u8::from_str_radix(hex, 16) {
                out.push(code as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// legacy utf8ToGbk：UTF-8 字符串 → GBK 字节 → 按 UTF-8 解码（书源签名场景）
fn utf8_to_gbk_lossy(s: &str) -> String {
    let (gbk_bytes, _, _) = encoding_rs::GBK.encode(s);
    String::from_utf8_lossy(&gbk_bytes).into_owned()
}

/// JS 缓存目录（legacy CacheManager 对应；文件 shim 统一落这里）
fn js_cache_dir() -> std::path::PathBuf {
    static DIR: LazyLock<std::path::PathBuf> =
        LazyLock::new(|| crate::AppConfig::from_env().storage_dir().join("js-cache"));
    DIR.clone()
}

/// java.cacheFile(url, saveTime?)：下载 URL（带书源 cookie/header，经 crawler::fetch），
/// **返回响应内容文本**；saveTime>0 时缓存有效期内直接复用本地副本
/// （legacy JsExtensions.kt:148-159——此前误实现为返回文件名）
fn java_cache_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    if url.is_empty() || (!url.starts_with("http://") && !url.starts_with("https://")) {
        return Ok(JsValue::from(JsString::from("")));
    }
    let save_time: u64 = js_value_to_string(args.get_or_undefined(1), context)
        .trim()
        .parse()
        .unwrap_or(0);
    let ns = inner.ns.clone();
    let fut = async move {
        let dir = js_cache_dir().join(sanitize_ns(&ns));
        std::fs::create_dir_all(&dir).map_err(|e| anyhow!("{e}"))?;
        let name = format!("{}.dat", crate::util::md5::md5_encode(&url));
        let path = dir.join(&name);
        // legacy：saveTime>0 且本地副本未过期 → 直接返回缓存内容
        if save_time > 0 {
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(modified) = meta.modified() {
                    let age = modified.elapsed().map(|d| d.as_secs()).unwrap_or(u64::MAX);
                    if age < save_time {
                        let cached = std::fs::read_to_string(&path).unwrap_or_default();
                        if !cached.is_empty() {
                            return Ok::<_, anyhow::Error>(cached);
                        }
                    }
                }
            }
        }
        let resp =
            crate::service::crawler::fetch(&url, &Default::default(), 15, "GET", None, None, None)
                .await
                .map_err(|e| anyhow!("{e}"))?;
        std::fs::write(&path, &resp.body).map_err(|e| anyhow!("{e}"))?;
        Ok(resp.body)
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.cacheFile") {
        Ok(content) => Ok(JsValue::from(JsString::from(content))),
        Err(_) => Ok(JsValue::from(JsString::from(""))),
    }
}

/// java.downloadFile(content, url)：content 写入缓存文件（文件名 md5(url) + ".txt"），
/// 返回相对路径字符串（可传给 readFile/readTxtFile 读回）
/// （legacy JsExtensions.kt:181-195——legacy 以 hex 解码 content、扩展名取自 URL type；
/// 此处按缓存 shim 坐标系简化为原文直写 + 固定 .txt，往返语义一致）
fn java_download_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let content = js_value_to_string(args.get_or_undefined(0), context);
    let url = js_value_to_string(args.get_or_undefined(1), context);
    if url.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let dir = js_cache_dir().join(sanitize_ns(&inner.ns));
    let name = format!("{}.txt", crate::util::md5::md5_encode(&url));
    if std::fs::create_dir_all(&dir).is_ok()
        && std::fs::write(dir.join(&name), content.as_bytes()).is_ok()
    {
        Ok(JsValue::from(JsString::from(name)))
    } else {
        Ok(JsValue::from(JsString::from("")))
    }
}

fn sanitize_ns(ns: &str) -> String {
    if ns.is_empty() {
        "default".to_string()
    } else {
        ns.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
    }
}

fn js_cache_path(ns: &str, path: &str) -> std::path::PathBuf {
    let clean = path
        .trim_start_matches('/')
        .replace("..", "_")
        .replace(['\\', ':'], "_");
    js_cache_dir().join(sanitize_ns(ns)).join(clean)
}

/// java.readFile(path)：读取缓存文件 → UTF-8 lossy 字符串
fn java_read_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = js_value_to_string(args.get_or_undefined(0), context);
    let bytes = std::fs::read(js_cache_path(&inner.ns, &path)).unwrap_or_default();
    Ok(JsValue::from(JsString::from(
        String::from_utf8_lossy(&bytes).into_owned(),
    )))
}

/// java.readTxtFile(path, charsetName?)：读取缓存文件（按编码解码，默认探测）
fn java_read_txt_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = js_value_to_string(args.get_or_undefined(0), context);
    let charset = js_value_to_string(args.get_or_undefined(1), context);
    let bytes = std::fs::read(js_cache_path(&inner.ns, &path)).unwrap_or_default();
    let text = if charset.is_empty() {
        crate::service::crawler::decode_bytes(&bytes, None)
    } else {
        let enc = match charset.to_ascii_lowercase().as_str() {
            "gbk" | "gb2312" | "gb18030" => encoding_rs::GBK,
            "big5" => encoding_rs::BIG5,
            "utf-16" | "utf16" => encoding_rs::UTF_16LE,
            _ => encoding_rs::UTF_8,
        };
        let (s, _, _) = enc.decode(&bytes);
        s.into_owned()
    };
    Ok(JsValue::from(JsString::from(text)))
}

/// java.deleteFile(path)：删除缓存文件
fn java_delete_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = js_value_to_string(args.get_or_undefined(0), context);
    let _ = std::fs::remove_file(js_cache_path(&inner.ns, &path));
    Ok(JsValue::undefined())
}

/// java.getFile(path)：返回 Java File 对象 stub（master 无 java.io.File 类）。
/// 带 `.path`（缓存命名空间目录下解析后的绝对路径，与 readFile/downloadFile 同坐标系）
/// 与 `.exists()`（实时探测）；大多数书源只用它取路径字符串
/// （legacy JsExtensions.kt:350-358——相对路径基于 cacheDir 解析后 new File(aPath)）
fn java_get_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = js_value_to_string(args.get_or_undefined(0), context);
    let abs = js_cache_path(&inner.ns, &path);
    let path_str = abs.to_string_lossy().into_owned();
    let obj = ObjectInitializer::new(context)
        .property(
            JsString::from("path"),
            JsValue::from(JsString::from(path_str)),
            Attribute::all(),
        )
        .function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| Ok(JsValue::from(abs.exists())))
            },
            JsString::from("exists"),
            0,
        )
        .build();
    Ok(obj.into())
}

/// java.unzipFile(zipPath)：解压缓存 zip，返回解压目录相对路径
fn java_unzip_file(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let zip_path = js_value_to_string(args.get_or_undefined(0), context);
    let out_dir = js_cache_dir().join(sanitize_ns(&inner.ns)).join("unzip");
    let zip_abs = js_cache_path(&inner.ns, &zip_path);
    let Ok(file) = std::fs::File::open(&zip_abs) else {
        return Ok(JsValue::from(JsString::from("")));
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Ok(JsValue::from(JsString::from("")));
    };
    let _ = std::fs::remove_dir_all(&out_dir);
    let _ = std::fs::create_dir_all(&out_dir);
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().replace('\\', "/");
        let target = out_dir.join(name.trim_start_matches('/'));
        if entry.is_dir() {
            let _ = std::fs::create_dir_all(&target);
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::File::create(&target) {
            let _ = std::io::copy(&mut entry, &mut f);
        }
    }
    let _ = std::fs::remove_file(&zip_abs);
    Ok(JsValue::from(JsString::from("unzip")))
}

/// java.getTxtInFolder(path)：拼接解压目录全部文本
fn java_get_txt_in_folder(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = js_value_to_string(args.get_or_undefined(0), context);
    let dir = if path.is_empty() {
        js_cache_dir().join(sanitize_ns(&inner.ns)).join("unzip")
    } else {
        js_cache_path(&inner.ns, &path)
    };
    let mut parts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Ok(bytes) = std::fs::read(&p) {
                    parts.push(crate::service::crawler::decode_bytes(&bytes, None));
                }
            }
        }
    }
    Ok(JsValue::from(JsString::from(parts.join("\n"))))
}

/// java.getZipStringContent(urlOrHex, path, charset?)：读取 zip 内单文件
fn java_get_zip_string_content(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    let path = js_value_to_string(args.get_or_undefined(1), context);
    let charset = js_value_to_string(args.get_or_undefined(2), context);
    let bytes = match java_get_zip_bytes(url, path, inner) {
        Some(b) => b,
        None => return Ok(JsValue::from(JsString::from(""))),
    };
    let text = if charset.is_empty() {
        crate::service::crawler::decode_bytes(&bytes, None)
    } else {
        let enc = match charset.to_ascii_lowercase().as_str() {
            "gbk" | "gb2312" | "gb18030" => encoding_rs::GBK,
            "big5" => encoding_rs::BIG5,
            _ => encoding_rs::UTF_8,
        };
        let (s, _, _) = enc.decode(&bytes);
        s.into_owned()
    };
    Ok(JsValue::from(JsString::from(text)))
}

/// java.getZipByteArrayContent(urlOrHex, path)：读取 zip 内单文件字节（返回 lossy 字符串）
fn java_get_zip_byte_array_content(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    let path = js_value_to_string(args.get_or_undefined(1), context);
    let bytes = java_get_zip_bytes(url, path, inner).unwrap_or_default();
    Ok(JsValue::from(JsString::from(
        String::from_utf8_lossy(&bytes).into_owned(),
    )))
}

fn java_get_zip_bytes(url: String, path: String, _inner: &JsBridgeInner) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = if url.starts_with("http://") || url.starts_with("https://") {
        let fut = async move {
            crate::service::crawler::fetch(&url, &Default::default(), 15, "GET", None, None, None)
                .await
                .map(|r| r.body.as_bytes().to_vec())
                .map_err(|e| anyhow!("{e}"))
        };
        block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.getZip").ok()?
    } else {
        hex::decode(url.trim()).ok()?
    };
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if entry.name() == path {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

/// java.importScript(path)：拉取远程 JS，**返回脚本源码文本**
/// （legacy JsExtensions.kt:125-133——书源拿到源码后自行 eval/拼接；
/// 此前误实现为 eval 后回传最后表达式值）
#[allow(unused_variables)]
fn java_import_script(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = js_value_to_string(args.get_or_undefined(0), context);
    if path.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let code = if path.starts_with("http://") || path.starts_with("https://") {
        let url = path.clone();
        let fut = async move {
            crate::service::crawler::fetch(&url, &Default::default(), 15, "GET", None, None, None)
                .await
                .map(|r| String::from_utf8_lossy(r.body.as_bytes()).into_owned())
                .map_err(|e| anyhow!("{e}"))
        };
        block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.importScript").unwrap_or_default()
    } else {
        java_read_txt_file(inner, args, context)
            .ok()
            .and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()))
            .unwrap_or_default()
    };
    // E12：返回脚本原文（legacy 语义——eval 由书源自行负责）
    Ok(JsValue::from(JsString::from(code)))
}

/// java.webView(html, url, js)：无 Android WebView，直接 eval 附加 JS；html 为空时抓取 url
fn java_web_view(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let html = js_value_to_string(args.get_or_undefined(0), context);
    let url = js_value_to_string(args.get_or_undefined(1), context);
    let js = js_value_to_string(args.get_or_undefined(2), context);
    let body = if !html.is_empty() {
        html
    } else if !url.is_empty() {
        let url2 = url.clone();
        let fut = async move {
            crate::service::crawler::fetch(&url2, &Default::default(), 15, "GET", None, None, None)
                .await
                .map(|r| String::from_utf8_lossy(r.body.as_bytes()).into_owned())
                .map_err(|e| anyhow!("{e}"))
        };
        block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.webView").unwrap_or_default()
    } else {
        String::new()
    };
    if js.trim().is_empty() {
        return Ok(JsValue::from(JsString::from(body)));
    }
    let mut vars = HashMap::new();
    vars.insert("result".to_string(), body.clone());
    vars.insert("baseUrl".to_string(), url);
    let bridge = bridge_ref(inner);
    let out = eval_js_with_bridge(&js, &vars, &bridge).unwrap_or_default();
    if out.is_empty() {
        Ok(JsValue::from(JsString::from(body)))
    } else {
        Ok(JsValue::from(JsString::from(out)))
    }
}

fn bridge_ref(inner: &JsBridgeInner) -> JsBridge {
    JsBridge {
        inner: Arc::new(JsBridgeInner {
            source_key: inner.source_key.clone(),
            source_name: inner.source_name.clone(),
            login_url: inner.login_url.clone(),
            source_variable: inner.source_variable.clone(),
            js_lib: inner.js_lib.clone(),
            source_header: inner.source_header.clone(),
            ns: inner.ns.clone(),
            headers: Mutex::new(
                inner
                    .headers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            ),
            java_vars: Mutex::new(
                inner
                    .java_vars
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            ),
            doc: Mutex::new(inner.doc.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        }),
    }
}

/// java.htmlFormat(str)：HTML → 纯文本（保留图片占位/换行语义，同正文清洗）
fn java_html_format(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(
        crate::service::book::html_content_to_text(&s),
    )))
}

/// JsValue → 字节（字符串 UTF-8 / 数字数组 / Uint8Array 兼容）
fn js_value_to_bytes(v: &JsValue, context: &mut Context) -> Vec<u8> {
    if let Some(arr) = v.as_object() {
        let len_key = JsString::from("length");
        if let Ok(raw_len) = arr.get(len_key, context) {
            if let Ok(len) = raw_len.to_u32(context) {
                let mut out = Vec::with_capacity(len as usize);
                for i in 0..len {
                    if let Ok(raw_b) = arr.get(i, context) {
                        if let Ok(b) = raw_b.to_u32(context) {
                            out.push(b as u8);
                        }
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    js_value_to_string(v, context).into_bytes()
}

/// 通用对称加解密（transformation 四参数格式）
fn crypt_with_transformation(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    symmetric_crypt(data, key, transformation, iv, encrypt)
}

/// mode/padding 分开的 Hutool 风格调用（AES/3DES）
fn crypt_with_mode_padding(
    data: &[u8],
    key: &[u8],
    algo: &str,
    mode: &str,
    padding: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let transformation = format!("{algo}/{mode}/{padding}");
    symmetric_crypt(data, key, &transformation, iv, encrypt)
}

fn js_bytes_to_js_string(bytes: Vec<u8>) -> JsValue {
    JsValue::from(JsString::from(String::from_utf8_lossy(&bytes).into_owned()))
}

/// 字节 → number[]（legacy ByteArray 语义，每元素 0-255）
fn js_bytes_to_js_array(bytes: Vec<u8>, context: &mut Context) -> JsValue {
    JsArray::from_iter(bytes.iter().map(|b| JsValue::from(*b as i32)), context).into()
}

fn js_base64_opt(args: &[JsValue], idx: usize, context: &mut Context) -> Vec<u8> {
    let s = js_value_to_string(args.get_or_undefined(idx), context);
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .unwrap_or_default()
}

/// java.escape / unescape / utf8ToGbk / digestHex / md5Encode16
fn java_escape(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(java_escape_impl(&s))))
}

fn java_unescape(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(java_unescape_impl(&s))))
}

fn java_utf8_to_gbk(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(utf8_to_gbk_lossy(&s))))
}

fn java_digest_hex(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_string(args.get_or_undefined(0), context);
    let algo = js_value_to_string(args.get_or_undefined(1), context).to_ascii_lowercase();
    use sha1::Digest as _;
    let out = match algo.as_str() {
        "md5" | "md-5" => crate::util::md5::md5_encode(&data),
        "sha1" | "sha-1" => hex::encode(sha1::Sha1::digest(data.as_bytes())),
        "sha256" | "sha-256" => hex::encode(sha2::Sha256::digest(data.as_bytes())),
        "sha512" | "sha-512" => hex::encode(sha2::Sha512::digest(data.as_bytes())),
        other => return Err(js_native_error(format!("digestHex: 不支持的算法 {other}"))),
    };
    Ok(JsValue::from(JsString::from(out)))
}

fn java_md5_encode16(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(
        crate::util::md5::md5_encode(&s)[8..24].to_string(),
    )))
}

// ---- AES 系列（legacy JsExtensions 同名） ----

fn java_aes_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.aesDecodeToString: {e}"))),
    }
}

fn java_aes_base64_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!(
            "java.aesBase64DecodeToString: {e}"
        ))),
    }
}

fn java_aes_encode_to_base64_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.aesEncodeToBase64String: {e}"
        ))),
    }
}

fn java_aes_encode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.aesEncodeToString: {e}"))),
    }
}

fn java_aes_decode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "AES", &mode, &padding, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.aesDecodeArgsBase64Str: {e}"))),
    }
}

fn java_aes_encode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "AES", &mode, &padding, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!("java.aesEncodeArgsBase64Str: {e}"))),
    }
}

// ---- AES 系列 *ToByteArray 变体（legacy JsExtensions 同名；输出 number[]） ----

/// java.aesDecodeToByteArray(data, key, transformation, iv) → number[]
fn java_aes_decode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_array(bytes, context)),
        Err(e) => Err(js_native_error(format!("java.aesDecodeToByteArray: {e}"))),
    }
}

/// java.aesBase64DecodeToByteArray(data, key, transformation, iv)：先 base64 解码再解密 → number[]
fn java_aes_base64_decode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_array(bytes, context)),
        Err(e) => Err(js_native_error(format!(
            "java.aesBase64DecodeToByteArray: {e}"
        ))),
    }
}

/// java.aesEncodeToByteArray(data, key, transformation, iv) → number[]
fn java_aes_encode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(js_bytes_to_js_array(bytes, context)),
        Err(e) => Err(js_native_error(format!("java.aesEncodeToByteArray: {e}"))),
    }
}

/// java.aesEncodeToBase64ByteArray(data, key, transformation, iv)：加密后 base64 化，
/// 返回 base64 文本的字节（legacy encryptAES2Base64——aesEncodeToBase64String 即其 UTF-8 视图）
fn java_aes_encode_to_base64_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok(js_bytes_to_js_array(b64.into_bytes(), context))
        }
        Err(e) => Err(js_native_error(format!(
            "java.aesEncodeToBase64ByteArray: {e}"
        ))),
    }
}

// ---- DES 系列 ----

fn java_des_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.desDecodeToString: {e}"))),
    }
}

fn java_des_base64_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!(
            "java.desBase64DecodeToString: {e}"
        ))),
    }
}

fn java_des_encode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.desEncodeToString: {e}"))),
    }
}

fn java_des_encode_to_base64_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.desEncodeToBase64String: {e}"
        ))),
    }
}

// ---- 3DES 系列（Hutool mode/padding 风格） ----

fn java_triple_des_decode_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_value_to_bytes(args.get_or_undefined(4), context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.tripleDESDecodeStr: {e}"))),
    }
}

fn java_triple_des_decode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!(
            "java.tripleDESDecodeArgsBase64Str: {e}"
        ))),
    }
}

fn java_triple_des_encode_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_value_to_bytes(args.get_or_undefined(4), context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.tripleDESEncodeBase64Str: {e}"
        ))),
    }
}

fn java_triple_des_encode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.tripleDESEncodeArgsBase64Str: {e}"
        ))),
    }
}

// ---- TTF 字体解析（legacy QueryTTF / queryTTF / replaceFont） ----

/// 字形轮廓点集（坐标 + on-curve 标志），用于跨字体同形字形匹配
#[derive(Debug, Clone, PartialEq)]
struct TtfOutline {
    points: Vec<(f32, f32, bool)>,
}

struct TtfShim {
    /// Unicode 码点 → glyph id（cmap）
    codes: HashMap<u32, u32>,
    /// glyph id → 轮廓
    glyphs: HashMap<u32, TtfOutline>,
}

struct TtfOutlineCollector {
    points: Vec<(f32, f32, bool)>,
}

impl ttf_parser::OutlineBuilder for TtfOutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.points.push((x, y, true));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.points.push((x, y, true));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.points.push((x1, y1, false));
        self.points.push((x, y, true));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.points.push((x1, y1, false));
        self.points.push((x2, y2, false));
        self.points.push((x, y, true));
    }
    fn close(&mut self) {}
}

fn parse_ttf(bytes: &[u8]) -> Option<TtfShim> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let mut codes = HashMap::new();
    let mut glyphs = HashMap::new();
    // 枚举 Unicode cmap 中所有码点（Face::cmap 子表）
    let cmap = face.tables().cmap?;
    for subtable in cmap.subtables {
        subtable.codepoints(|cp| {
            if let Some(gid) = subtable.glyph_index(cp) {
                codes.insert(cp, gid.0 as u32);
            }
        });
    }
    let mut visited = std::collections::HashSet::new();
    for &gid in codes.values() {
        if !visited.insert(gid) {
            continue;
        }
        let mut collector = TtfOutlineCollector { points: Vec::new() };
        if face
            .outline_glyph(ttf_parser::GlyphId(gid as u16), &mut collector)
            .is_some()
        {
            glyphs.insert(
                gid,
                TtfOutline {
                    points: collector.points,
                },
            );
        }
    }
    Some(TtfShim { codes, glyphs })
}

static TTF_REGISTRY: LazyLock<Mutex<HashMap<u64, Arc<TtfShim>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static TTF_NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn register_ttf(shim: TtfShim) -> u64 {
    let id = TTF_NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    TTF_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(shim));
    id
}

fn ttf_object(id: u64, context: &mut Context) -> JsResult<JsValue> {
    let mut obj = ObjectInitializer::new(context);
    obj.function(
        unsafe { NativeFunction::from_closure(move |_t, args, ctx| ttf_in_limit(id, args, ctx)) },
        JsString::from("inLimit"),
        1,
    )
    .function(
        unsafe { NativeFunction::from_closure(move |_t, args, ctx| ttf_get_glyf(id, args, ctx)) },
        JsString::from("getGlyfByCode"),
        1,
    )
    .function(
        unsafe { NativeFunction::from_closure(move |_t, args, ctx| ttf_get_code(id, args, ctx)) },
        JsString::from("getCodeByGlyf"),
        1,
    )
    .property(
        JsString::from("__ttfId"),
        JsValue::from(id as f64),
        Attribute::all(),
    );
    Ok(obj.build().into())
}

fn ttf_get(id: u64) -> Option<Arc<TtfShim>> {
    TTF_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
}

fn ttf_in_limit(id: u64, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = js_value_to_u32(args.get_or_undefined(0), context);
    let Some(shim) = ttf_get(id) else {
        return Ok(JsValue::from(false));
    };
    Ok(JsValue::from(shim.codes.contains_key(&code)))
}

fn ttf_get_glyf(id: u64, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = js_value_to_u32(args.get_or_undefined(0), context);
    let Some(shim) = ttf_get(id) else {
        return Ok(JsValue::from(0));
    };
    Ok(JsValue::from(
        shim.codes.get(&code).copied().unwrap_or(0) as f64
    ))
}

fn ttf_get_code(id: u64, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let gid = js_value_to_u32(args.get_or_undefined(0), context);
    let Some(shim) = ttf_get(id) else {
        return Ok(JsValue::from(0));
    };
    let code = shim
        .codes
        .iter()
        .find(|(_, g)| **g == gid)
        .map(|(c, _)| *c)
        .unwrap_or(0);
    Ok(JsValue::from(code as f64))
}

fn js_value_to_u32(v: &JsValue, context: &mut Context) -> u32 {
    match v {
        JsValue::Integer(i) => (*i).max(0) as u32,
        JsValue::Rational(r) => (*r).max(0.0) as u32,
        JsValue::BigInt(b) => b.to_f64().max(0.0) as u32,
        _ => js_value_to_string(v, context).parse::<u32>().unwrap_or(0),
    }
}

/// java.queryBase64TTF(base64)：base64 字体 → TTF 对象
fn java_query_base64_ttf(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .unwrap_or_default();
    let Some(shim) = parse_ttf(&bytes) else {
        return Ok(JsValue::null());
    };
    ttf_object(register_ttf(shim), context)
}

/// java.queryTTF(str)：支持 URL / base64 / 本地缓存文件
fn java_query_ttf(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    if s.is_empty() {
        return Ok(JsValue::null());
    }
    let bytes: Option<Vec<u8>> = if s.starts_with("http://") || s.starts_with("https://") {
        let url = s.clone();
        let fut = async move {
            crate::service::crawler::fetch(&url, &Default::default(), 15, "GET", None, None, None)
                .await
                .map(|r| r.body.as_bytes().to_vec())
                .map_err(|e| anyhow!("{e}"))
        };
        block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "java.queryTTF").ok()
    } else if s.contains("storage/") {
        let p = crate::AppConfig::from_env().storage_dir().join(
            s.trim_start_matches('/')
                .replace("..", "_")
                .replace(['\\', ':'], "_"),
        );
        std::fs::read(p).ok()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .ok()
    };
    let Some(bytes) = bytes else {
        return Ok(JsValue::null());
    };
    let Some(shim) = parse_ttf(&bytes) else {
        return Ok(JsValue::null());
    };
    ttf_object(register_ttf(shim), context)
}

/// java.replaceFont(text, font1, font2)：用 font2 中同形字符替换 font1 中字符
fn java_replace_font(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let text = js_value_to_string(args.get_or_undefined(0), context);
    let id1 = js_ttf_id(args.get_or_undefined(1), context);
    let id2 = js_ttf_id(args.get_or_undefined(2), context);
    let (Some(f1), Some(f2)) = (id1.and_then(ttf_get), id2.and_then(ttf_get)) else {
        return Ok(JsValue::from(JsString::from(text)));
    };
    // font2：轮廓 → 首个同形码点
    let mut outline_to_code: HashMap<u32, u32> = HashMap::new();
    for (code, gid) in &f2.codes {
        if let Some(outline) = f2.glyphs.get(gid) {
            outline_to_code
                .entry(hash_outline(outline))
                .or_insert(*code);
        }
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let code = c as u32;
        let replaced = f1
            .codes
            .get(&code)
            .and_then(|gid| f1.glyphs.get(gid))
            .and_then(|outline| outline_to_code.get(&hash_outline(outline)))
            .and_then(|new_code| char::from_u32(*new_code));
        if let Some(nc) = replaced {
            out.push(nc);
        } else {
            out.push(c);
        }
    }
    Ok(JsValue::from(JsString::from(out)))
}

/// 轮廓的稳定哈希（坐标量化到 0.1 单位）
fn hash_outline(o: &TtfOutline) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (x, y, on) in &o.points {
        ((x * 10.0).round() as i64).hash(&mut hasher);
        ((y * 10.0).round() as i64).hash(&mut hasher);
        on.hash(&mut hasher);
    }
    hasher.finish() as u32
}

fn js_ttf_id(v: &JsValue, context: &mut Context) -> Option<u64> {
    let obj = v.as_object()?;
    let key = JsString::from("__ttfId");
    let raw = obj.get(key, context).ok()?;
    let n = raw.as_number()?;
    Some(n as u64)
}

/// 同步 HTTP 请求（java.get(url)/connect(url)/head(url) 共用）：返回响应对象
fn java_http_fetch(
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
        let url = final_url.clone();
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
            NativeFunction::from_closure(move |_t, _a, _c| {
                // cookies()：空对象（cookie 由爬虫层管理）
                Ok(ObjectInitializer::new(_c).build().into())
            })
        },
        JsString::from("cookies"),
        0,
    );
    {
        let body = body.clone();
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
fn java_connect(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    java_http_fetch(inner, &url, "GET", context)
}

/// java.head(url, headers)：HTTP HEAD（cookies() 兼容）
fn java_head(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = js_value_to_string(args.get_or_undefined(0), context);
    java_http_fetch(inner, &url, "HEAD", context)
}

// ---- org.jsoup shim（scraper 后端） ----

/// org.jsoup.Jsoup.parse(html)：Document 对象（select/text/title/html/toString）
fn jsoup_parse(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let html = js_value_to_string(_args.get_or_undefined(0), context);
    jsoup_document(&html, context)
}

/// Document 对象
fn jsoup_document(html: &str, context: &mut Context) -> JsResult<JsValue> {
    let html = html.to_string();
    let mut doc = ObjectInitializer::new(context);
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let css = js_value_to_string(args.get_or_undefined(0), ctx);
                    jsoup_select(&h, &css, ctx)
                })
            },
            JsString::from("select"),
            1,
        );
    }
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    let parsed = scraper::Html::parse_document(&h);
                    let txt = parsed.root_element().text().collect::<String>();
                    Ok(JsValue::from(JsString::from(txt)))
                })
            },
            JsString::from("text"),
            0,
        );
    }
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    let parsed = scraper::Html::parse_document(&h);
                    let title = scraper::Selector::parse("title")
                        .ok()
                        .and_then(|sel| parsed.select(&sel).next())
                        .map(|e| e.text().collect::<String>())
                        .unwrap_or_default();
                    Ok(JsValue::from(JsString::from(title)))
                })
            },
            JsString::from("title"),
            0,
        );
    }
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(h.clone())))
                })
            },
            JsString::from("toString"),
            0,
        );
    }
    Ok(doc.build().into())
}

/// Elements 选择（css 选择器 → 元素对象数组 + jsoup 方法）
fn jsoup_select(html: &str, css: &str, context: &mut Context) -> JsResult<JsValue> {
    let parsed = scraper::Html::parse_fragment(html);
    let Ok(selector) = scraper::Selector::parse(css) else {
        return Ok(JsArray::new(context).into()); // 非法选择器 → 空 Elements（不抛错）
    };
    let htmls: Vec<String> = parsed.select(&selector).map(|e| e.html()).collect();
    jsoup_elements_from_htmls(htmls, context)
}

/// 由元素 HTML 列表构建 jsoup Elements（数组 + attr/text/first/get/size/eachAttr/eachText/html/val）
fn jsoup_elements_from_htmls(htmls: Vec<String>, context: &mut Context) -> JsResult<JsValue> {
    let arr = JsArray::new(context);
    for h in &htmls {
        let el = jsoup_element(h, context)?;
        arr.push(el, context)?;
    }
    let htmls = Arc::new(htmls);
    // attr(name)：首元素属性
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("attr"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx);
                    Ok(JsValue::from(JsString::from(first_attr(&htmls, &name))))
                })
            })
            .name("attr")
            .length(1)
            .build(),
            true,
            context,
        )?;
    }
    // text()：首元素文本
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("text"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    Ok(JsValue::from(JsString::from(first_text(&htmls))))
                })
            })
            .name("text")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // first()：首元素对象（无 → null）
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("first"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, ctx| match htmls.first() {
                    Some(h) => jsoup_element(h, ctx),
                    None => Ok(JsValue::null()),
                })
            })
            .name("first")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // get(i)：第 i 个元素对象（越界 → null）
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("get"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let i = args.get_or_undefined(0).to_i32(ctx).unwrap_or(0);
                    match htmls.get(i.max(0) as usize) {
                        Some(h) => jsoup_element(h, ctx),
                        None => Ok(JsValue::null()),
                    }
                })
            })
            .name("get")
            .length(1)
            .build(),
            true,
            context,
        )?;
    }
    // size()：数量
    {
        let n = htmls.len() as i32;
        arr.set(
            JsString::from("size"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| Ok(JsValue::from(n)))
            })
            .name("size")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // eachAttr(name)：各元素属性值数组
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("eachAttr"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx);
                    let vals: Vec<JsValue> = htmls
                        .iter()
                        .map(|h| JsValue::from(JsString::from(attr_of(h, &name))))
                        .collect();
                    Ok(JsArray::from_iter(vals, ctx).into())
                })
            })
            .name("eachAttr")
            .length(1)
            .build(),
            true,
            context,
        )?;
    }
    // eachText()：各元素文本数组
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("eachText"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, ctx| {
                    let vals: Vec<JsValue> = htmls
                        .iter()
                        .map(|h| JsValue::from(JsString::from(text_of(h))))
                        .collect();
                    Ok(JsArray::from_iter(vals, ctx).into())
                })
            })
            .name("eachText")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // html()：首元素 innerHTML
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("html"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    Ok(JsValue::from(JsString::from(inner_html_of(
                        htmls.first().map(String::as_str).unwrap_or(""),
                    ))))
                })
            })
            .name("html")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // val()：首元素 value 属性/文本
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("val"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    let v = htmls
                        .first()
                        .map(|h| {
                            let a = attr_of(h, "value");
                            if a.is_empty() {
                                text_of(h)
                            } else {
                                a
                            }
                        })
                        .unwrap_or_default();
                    Ok(JsValue::from(JsString::from(v)))
                })
            })
            .name("val")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // toString()/outerHtml()：全部元素 HTML 拼接
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("toString"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(htmls.join(""))))
                })
            })
            .name("toString")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    Ok(arr.into())
}

/// jsoup 元素对象（select/attr/text/html/val/ownText/toString）
fn jsoup_element(html: &str, context: &mut Context) -> JsResult<JsValue> {
    let html = html.to_string();
    let mut el = ObjectInitializer::new(context);
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let css = js_value_to_string(args.get_or_undefined(0), ctx);
                    jsoup_select(&h, &css, ctx)
                })
            },
            JsString::from("select"),
            1,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx);
                    Ok(JsValue::from(JsString::from(attr_of(&h, &name))))
                })
            },
            JsString::from("attr"),
            1,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(text_of(&h))))
                })
            },
            JsString::from("text"),
            0,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(inner_html_of(&h))))
                })
            },
            JsString::from("html"),
            0,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    let v = attr_of(&h, "value");
                    Ok(JsValue::from(JsString::from(if v.is_empty() {
                        text_of(&h)
                    } else {
                        v
                    })))
                })
            },
            JsString::from("val"),
            0,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(h.clone())))
                })
            },
            JsString::from("toString"),
            0,
        );
    }
    Ok(el.build().into())
}

/// 元素 HTML → 首个元素属性值
fn attr_of(html: &str, name: &str) -> String {
    let parsed = scraper::Html::parse_fragment(html);
    parsed.root_element().attr(name).unwrap_or("").to_string()
}

/// 元素 HTML → 文本
fn text_of(html: &str) -> String {
    let parsed = scraper::Html::parse_fragment(html);
    parsed
        .root_element()
        .text()
        .collect::<String>()
        .trim()
        .to_string()
}

/// 元素 HTML → innerHTML
fn inner_html_of(html: &str) -> String {
    let parsed = scraper::Html::parse_fragment(html);
    parsed.root_element().inner_html()
}

/// 首个元素属性值（Elements.attr）
fn first_attr(htmls: &[String], name: &str) -> String {
    htmls.first().map(|h| attr_of(h, name)).unwrap_or_default()
}

/// 首个元素文本（Elements.text）
fn first_text(htmls: &[String]) -> String {
    htmls.first().map(|h| text_of(h)).unwrap_or_default()
}

// ---- java.headerMap.* 实现（请求头 Map）----

/// java.headerMap.put(key, value)
fn header_map_put(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_string(args.get_or_undefined(1), context);
    inner
        .headers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, value);
    Ok(JsValue::undefined())
}

/// java.headerMap.get(key)
fn header_map_get(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = inner
        .headers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    Ok(value.map_or_else(JsValue::undefined, |s| JsValue::from(JsString::from(s))))
}

/// java.headerMap.size()
fn header_map_size(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let size = inner
        .headers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .len();
    Ok(JsValue::from(size as i32))
}

// ---- source.* 实现 ----

/// source.getKey()：书源 key（URL）
fn source_get_key(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(inner.source_key.as_str())))
}

/// source.getName()：书源名称
fn source_get_name(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(inner.source_name.as_str())))
}

/// source.put(key, value)：书源级变量（全局存储，按书源 key 隔离，
/// 跨搜索/详情调用可见）。P1-3：条数/字节上限，超限拒绝写入（见 source_put_limited）。
fn source_put(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_string(args.get_or_undefined(1), context);
    let mut map = SOURCE_VARS.lock().unwrap_or_else(|e| e.into_inner());
    let vars = map.entry(inner.source_key.clone()).or_default();
    if !source_put_limited(vars, &key, &value) {
        tracing::warn!(
            "source.put 超限拒绝（书源 {}，key={key:?}，value_len={}）——单书源上限 {SOURCE_VARS_MAX_ENTRIES} 条 / {SOURCE_VARS_MAX_BYTES} 字节",
            inner.source_key,
            value.len()
        );
    }
    Ok(JsValue::undefined())
}

/// source.get(key)：读取书源级变量，缺失返回 undefined
fn source_get(inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = SOURCE_VARS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&inner.source_key)
        .and_then(|m| m.get(&key).cloned());
    Ok(value.map_or_else(JsValue::undefined, |s| JsValue::from(JsString::from(s))))
}

/// source.getVariable()：书源变量（legado 书源变量配置；无 → 空串）
fn source_get_variable(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(
        inner.source_variable.as_str(),
    )))
}

/// source.putLoginHeader(header)：保存登录头（JSON 文本；legacy 登录成功后自动附加到抓取请求）。
/// 按用户命名空间 + 书源 key 存库；无 cookie 上下文（ns 空）时静默 no-op。
fn source_put_login_header(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let header = js_value_to_string(args.get_or_undefined(0), context);
    if !inner.ns.is_empty() {
        let ns = inner.ns.clone();
        let source_url = inner.source_key.clone();
        let fut = async move {
            crate::service::crawler::set_login_header_for(&ns, &source_url, &header).await;
            Ok::<_, anyhow::Error>(())
        };
        let _ = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "source.putLoginHeader");
    }
    Ok(JsValue::undefined())
}

/// source.removeLoginHeader()：清除登录头
fn source_remove_login_header(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    if !inner.ns.is_empty() {
        let ns = inner.ns.clone();
        let source_url = inner.source_key.clone();
        let fut = async move {
            crate::service::crawler::set_login_header_for(&ns, &source_url, "").await;
            Ok::<_, anyhow::Error>(())
        };
        let _ = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "source.removeLoginHeader");
    }
    Ok(JsValue::undefined())
}

/// source.getLoginHeader()：读取已保存的登录头（无 → 空串）
fn source_get_login_header(
    inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    if inner.ns.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let ns = inner.ns.clone();
    let source_url = inner.source_key.clone();
    let fut = async move {
        let header = crate::service::crawler::login_header_for(&ns, &source_url)
            .await
            .unwrap_or_default();
        Ok::<_, anyhow::Error>(header)
    };
    match block_on_task(fut, BRIDGE_WAIT_TIMEOUT, "source.getLoginHeader") {
        Ok(h) => Ok(JsValue::from(JsString::from(h))),
        Err(_) => Ok(JsValue::from(JsString::from(""))),
    }
}

/// JsValue → 字符串（对齐 String() 语义：数字/布尔 → 字面量；
/// null/undefined → 空串，对齐 legado 空结果语义；对象 → toString()）
fn js_value_to_string(v: &JsValue, context: &mut Context) -> String {
    match v {
        JsValue::Null | JsValue::Undefined => String::new(),
        _ => v
            .to_string(context)
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default(),
    }
}

/// eval 结果出口：数组/对象 → JSON 文本（避免 ToString 的 "[object Object]"
/// 使下游 JSON 解析为空）；其余按 String() 语义（数字/布尔字面量、null/undefined 空串）
fn js_result_to_string(v: &JsValue, context: &mut Context) -> String {
    match v {
        JsValue::Null | JsValue::Undefined => String::new(),
        JsValue::Object(_) => js_value_to_json(v, context)
            .map(|j| j.to_string())
            .unwrap_or_default(),
        _ => js_value_to_string(v, context),
    }
}

/// JsValue → serde_json::Value 递归转换（数组/对象/基本类型全支持）
///
/// 背景：boa ToString 对数组输出元素 Join（对象元素为 "[object Object]"），
/// 经 JSON.parse 必然解析为空。此处对齐 JSON.stringify 语义（Undefined/BigInt/
/// Symbol → null，不 panic——区别于 boa `JsValue::to_json` 对 Undefined 的 todo!）。
pub fn js_value_to_json(v: &JsValue, context: &mut Context) -> JsResult<JsonValue> {
    match v {
        JsValue::Null | JsValue::Undefined => Ok(JsonValue::Null),
        JsValue::Boolean(b) => Ok(JsonValue::Bool(*b)),
        JsValue::String(s) => Ok(JsonValue::String(s.to_std_string_escaped())),
        JsValue::Rational(r) => Ok(serde_json::json!(*r)),
        JsValue::Integer(i) => Ok(serde_json::json!(*i)),
        // BigInt/Symbol：JSON.stringify 语义（BigInt 抛错、Symbol 忽略）——此处收敛为 null
        JsValue::BigInt(_) | JsValue::Symbol(_) => Ok(JsonValue::Null),
        JsValue::Object(obj) => {
            if obj.is_array() {
                // 数组：按 length 逐元素（对齐 JSON.stringify 语义）
                let len = obj
                    .get(JsString::from("length"), context)?
                    .to_u32(context)?;
                let mut arr = Vec::with_capacity(len as usize);
                for k in 0..len {
                    let val = obj.get(k, context)?;
                    arr.push(js_value_to_json(&val, context)?);
                }
                Ok(JsonValue::Array(arr))
            } else {
                // 对象：own_property_keys 遍历（Symbol 键跳过）
                let mut map = JsonMap::new();
                for key in obj.own_property_keys(context)? {
                    let k = match &key {
                        PropertyKey::String(s) => s.to_std_string_escaped(),
                        PropertyKey::Index(i) => i.get().to_string(),
                        PropertyKey::Symbol(_) => continue,
                    };
                    let val = obj.get(key, context)?;
                    map.insert(k, js_value_to_json(&val, context)?);
                }
                Ok(JsonValue::Object(map))
            }
        }
    }
}

/// java.aesBase64DecodeToString(data, key, mode, iv)：AES/CBC/PKCS5 解密（书源加密 URL 常见）
fn java_aes_decrypt(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let data = args
        .first()
        .map(|v| js_value_to_string(v, context))
        .unwrap_or_default();
    let key = args
        .get(1)
        .map(|v| js_value_to_string(v, context))
        .unwrap_or_default();
    let iv = args
        .get(3)
        .map(|v| js_value_to_string(v, context))
        .unwrap_or_default();
    let decrypted = aes_base64_decode_to_string(&data, &key, &iv);
    Ok(JsValue::from(JsString::from(decrypted)))
}

/// AES-128-CBC/PKCS7 解密（key/iv 取前 16 字节）
fn aes_base64_decode_to_string(data: &str, key: &str, iv: &str) -> String {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    use base64::Engine;
    let ciphertext = match base64::engine::general_purpose::STANDARD.decode(data.trim()) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    if ciphertext.is_empty() {
        return String::new();
    }
    let key_bytes: Vec<u8> = key.as_bytes().iter().take(16).copied().collect();
    let iv_bytes: Vec<u8> = iv.as_bytes().iter().take(16).copied().collect();
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    let Ok(dec) = Aes128CbcDec::new_from_slices(&key_bytes, &iv_bytes) else {
        return String::new();
    };
    let buf = ciphertext;
    match dec.decrypt_padded_vec_mut::<block_padding::Pkcs7>(&buf) {
        Ok(pt) => String::from_utf8_lossy(&pt).into_owned(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M6：block_on_task 超时后立即返回错误（不再无限等待）——永不完成的任务在短超时内返回
    #[test]
    fn test_block_on_task_timeout_returns_promptly() {
        let start = std::time::Instant::now();
        let fut = async {
            let _ = std::future::pending::<Result<(), anyhow::Error>>().await;
            Ok(())
        };
        let err =
            block_on_task(fut, std::time::Duration::from_millis(200), "test-pending").unwrap_err();
        assert!(err.to_string().contains("超时"), "应为超时错误: {err}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "超时后应立即返回（不再等待工作线程）"
        );
    }

    /// M6：正常完成的任务仍能取回结果（超时上限下调不影响正常路径）
    #[test]
    fn test_block_on_task_returns_result() {
        let fut = async { Ok::<_, anyhow::Error>(42u32) };
        let r = block_on_task(fut, std::time::Duration::from_secs(2), "test-ok").unwrap();
        assert_eq!(r, 42);
    }

    /// M6：桥接等待上限为 10s（worker 阻塞窗口封顶）
    #[test]
    fn test_bridge_wait_timeout_is_10s() {
        assert_eq!(BRIDGE_WAIT_TIMEOUT, std::time::Duration::from_secs(10));
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ---- 旧行为兼容 ----

    #[test]
    fn eval_js_string_concat() {
        // key 注入 + 字符串拼接
        let v = vars(&[("key", "a")]);
        assert_eq!(eval_js("key + 'x'", &v).unwrap(), "ax");
    }

    #[test]
    fn eval_js_number_boolean_to_string() {
        let v = vars(&[]);
        // 数字 → 字符串
        assert_eq!(eval_js("1 + 2", &v).unwrap(), "3");
        assert_eq!(eval_js("6 * 7", &v).unwrap(), "42");
        assert_eq!(eval_js("3.14", &v).unwrap(), "3.14");
        // 布尔 → 字符串
        assert_eq!(eval_js("true", &v).unwrap(), "true");
        assert_eq!(eval_js("1 > 2", &v).unwrap(), "false");
    }

    #[test]
    fn eval_js_injected_vars() {
        let v = vars(&[
            ("result", "hello"),
            ("page", "2"),
            ("baseUrl", "https://a.com"),
        ]);
        assert_eq!(eval_js("result + page", &v).unwrap(), "hello2");
        assert_eq!(eval_js("baseUrl.length", &v).unwrap(), "13");
    }

    // ---- E10/AR5：章节/书上下文绑定（legacy AnalyzeRule.kt:650-664 注入集） ----

    /// 保留键 → JS 类型化全局：title/chapter{title,url,index}/book{name,author,bookUrl}/
    /// nextChapterUrl 均可在 JS 中直接访问；保留键本身不以裸名暴露
    #[test]
    fn eval_js_context_bindings_injected() {
        let v = vars(&[
            (crate::parser::rule::RK_CHAPTER_TITLE, "第一章 测试"),
            (
                crate::parser::rule::RK_CHAPTER_URL,
                "https://a.test/c/1.html",
            ),
            (crate::parser::rule::RK_CHAPTER_INDEX, "2"),
            (
                crate::parser::rule::RK_NEXT_CHAPTER_URL,
                "https://a.test/c/2.html",
            ),
            (crate::parser::rule::RK_BOOK_NAME, "测试书名"),
            (crate::parser::rule::RK_BOOK_AUTHOR, "作者甲"),
            (crate::parser::rule::RK_BOOK_URL, "https://a.test/book/1"),
        ]);
        assert_eq!(eval_js("title", &v).unwrap(), "第一章 测试");
        assert_eq!(eval_js("chapter.title", &v).unwrap(), "第一章 测试");
        assert_eq!(
            eval_js("chapter.url", &v).unwrap(),
            "https://a.test/c/1.html"
        );
        // index 为数值型绑定（可算术）
        assert_eq!(eval_js("String(chapter.index)", &v).unwrap(), "2");
        assert_eq!(eval_js("String(chapter.index + 1)", &v).unwrap(), "3");
        assert_eq!(
            eval_js("nextChapterUrl", &v).unwrap(),
            "https://a.test/c/2.html"
        );
        assert_eq!(eval_js("book.name", &v).unwrap(), "测试书名");
        assert_eq!(eval_js("book.author", &v).unwrap(), "作者甲");
        assert_eq!(
            eval_js("book.bookUrl", &v).unwrap(),
            "https://a.test/book/1"
        );
        // 保留键不以裸字符串全局暴露
        assert_eq!(
            eval_js("typeof __chapter_title__ + ',' + typeof __book_name__", &v).unwrap(),
            "undefined,undefined"
        );
    }

    /// 缺失上下文 → undefined（与 legacy null 一致，脚本 typeof 判断不 ReferenceError）
    #[test]
    fn eval_js_context_bindings_missing_undefined() {
        let v = vars(&[]);
        assert_eq!(eval_js("typeof title", &v).unwrap(), "undefined");
        assert_eq!(eval_js("typeof chapter", &v).unwrap(), "undefined");
        assert_eq!(eval_js("typeof book", &v).unwrap(), "undefined");
        assert_eq!(eval_js("typeof nextChapterUrl", &v).unwrap(), "undefined");
    }

    /// 部分上下文：仅章节（无书信息）→ chapter 对象可用、book 为 undefined；
    /// 仅书信息 → 反之；显式 vars 同名变量优先于绑定（bind_global 不覆盖非 undefined）
    #[test]
    fn eval_js_context_bindings_partial_and_priority() {
        let v = vars(&[
            (crate::parser::rule::RK_CHAPTER_TITLE, "只有标题"),
            (crate::parser::rule::RK_CHAPTER_INDEX, "0"),
        ]);
        assert_eq!(eval_js("chapter.title", &v).unwrap(), "只有标题");
        assert_eq!(eval_js("typeof chapter.url", &v).unwrap(), "undefined");
        assert_eq!(eval_js("typeof book", &v).unwrap(), "undefined");

        let v2 = vars(&[
            (crate::parser::rule::RK_BOOK_NAME, "只有书名"),
            (crate::parser::rule::RK_BOOK_AUTHOR, "作者乙"),
        ]);
        assert_eq!(eval_js("typeof chapter", &v2).unwrap(), "undefined");
        assert_eq!(
            eval_js("book.name + '/' + book.author", &v2).unwrap(),
            "只有书名/作者乙"
        );

        let v3 = vars(&[
            ("title", "显式变量优先"),
            (crate::parser::rule::RK_CHAPTER_TITLE, "上下文标题"),
        ]);
        assert_eq!(eval_js("title", &v3).unwrap(), "显式变量优先");
    }

    /// JSON 版求值同样注入上下文绑定（eval_js_json_with_bridge 路径）
    #[test]
    fn eval_js_json_context_bindings_injected() {
        let v = vars(&[
            (crate::parser::rule::RK_CHAPTER_TITLE, "JSON 章节"),
            (crate::parser::rule::RK_BOOK_NAME, "JSON 书"),
        ]);
        let out = eval_js_json("JSON.stringify([title, book.name])", &v).unwrap();
        assert_eq!(out.to_string(), r#"["JSON 章节","JSON 书"]"#);
    }

    #[test]
    fn eval_js_null_undefined_to_empty() {
        let v = vars(&[]);
        assert_eq!(eval_js("undefined", &v).unwrap(), "");
        assert_eq!(eval_js("null", &v).unwrap(), "");
    }

    #[test]
    fn eval_js_error_returns_err() {
        let v = vars(&[]);
        assert!(eval_js("throw new Error('boom')", &v).is_err());
        assert!(eval_js("let let = 1", &v).is_err());
    }

    #[test]
    fn eval_js_backward_compat_default_bridge() {
        // 旧签名内部走空 bridge：java/source 可用但不跨调用保留
        let v = vars(&[]);
        assert_eq!(
            eval_js("java.log('x'); java.put('a', 'b')", &v).unwrap(),
            ""
        );
        assert_eq!(eval_js("java.get('a')", &v).unwrap(), "");
        assert_eq!(eval_js("source.getKey()", &v).unwrap(), "");
    }

    #[test]
    fn eval_js_top_level_aliases() {
        // legacy 老书源直接调用顶层函数（无 java. 前缀）
        let v = vars(&[]);
        let md5 = eval_js("md5Encode('abc')", &v).unwrap();
        assert_eq!(md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            eval_js("md5Encode16('abc')", &v).unwrap(),
            "3cd24fb0d6963f7d"
        );
        let b64 = eval_js("base64Encode('abc')", &v).unwrap();
        assert_eq!(b64, "YWJj");
        assert_eq!(eval_js("base64DecodeToString('YWJj')", &v).unwrap(), "abc");
        // 与 java.* 桥等价
        assert_eq!(eval_js("java.md5Encode('abc')", &v).unwrap(), md5);
        assert!(!eval_js("typeof randomUUID === 'function'", &v)
            .unwrap()
            .is_empty());
    }

    // ---- java.* shim ----

    #[test]
    fn bridge_java_put_get_roundtrip() {
        let bridge = JsBridge::new("https://src.test", "测试源");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge("java.put('k1', 'v1'); java.get('k1')", &v, &bridge).unwrap(),
            "v1"
        );
        // 同 bridge 跨调用共享
        assert_eq!(
            eval_js_with_bridge("java.get('k1')", &v, &bridge).unwrap(),
            "v1"
        );
    }

    #[test]
    fn bridge_java_get_missing_is_undefined() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge("java.get('nope')", &v, &bridge).unwrap(),
            ""
        );
    }

    #[test]
    fn bridge_java_vars_isolated_per_bridge() {
        let b1 = JsBridge::new("", "");
        let b2 = JsBridge::new("", "");
        let v = vars(&[]);
        eval_js_with_bridge("java.put('k', 'from-b1')", &v, &b1).unwrap();
        assert_eq!(eval_js_with_bridge("java.get('k')", &v, &b2).unwrap(), "");
    }

    #[test]
    fn bridge_java_log_no_error() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge("java.log('hello ' + 1)", &v, &bridge).unwrap(),
            ""
        );
    }

    // ---- source.* shim ----

    #[test]
    fn bridge_source_put_get_cross_call() {
        // 跨 eval 调用/跨 bridge 实例：同书源 key 共享（全局存储）
        let b1 = JsBridge::new("https://src-x.test/book", "源A");
        let b2 = JsBridge::new("https://src-x.test/book", "源A");
        let v = vars(&[]);
        eval_js_with_bridge("source.put('page', '2')", &v, &b1).unwrap();
        assert_eq!(
            eval_js_with_bridge("source.get('page')", &v, &b2).unwrap(),
            "2"
        );
    }

    #[test]
    fn bridge_source_vars_isolated_by_source_key() {
        let a = JsBridge::new("https://a.test", "A");
        let b = JsBridge::new("https://b.test", "B");
        let v = vars(&[]);
        eval_js_with_bridge("source.put('x', '1')", &v, &a).unwrap();
        assert_eq!(eval_js_with_bridge("source.get('x')", &v, &b).unwrap(), "");
        assert_eq!(eval_js_with_bridge("source.get('x')", &v, &a).unwrap(), "1");
    }

    #[test]
    fn bridge_source_key_and_name() {
        let bridge = JsBridge::new("https://src.test", "测试源");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge("source.getKey() + '|' + source.getName()", &v, &bridge).unwrap(),
            "https://src.test|测试源"
        );
    }

    // ---- java.headerMap shim ----

    #[test]
    fn bridge_header_map_put_get() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge(
                "java.headerMap.put('User-Agent', 'ua/1'); java.headerMap.get('User-Agent')",
                &v,
                &bridge
            )
            .unwrap(),
            "ua/1"
        );
        assert_eq!(
            eval_js_with_bridge("java.headerMap.size()", &v, &bridge).unwrap(),
            "1"
        );
        // eval 后 Rust 侧可读取改写后的请求头
        assert_eq!(
            bridge.headers().get("User-Agent").map(String::as_str),
            Some("ua/1")
        );
    }

    #[test]
    fn bridge_initial_headers_visible_in_js() {
        let bridge = JsBridge::new("", "");
        bridge.set_headers(vars(&[("Referer", "https://r.test")]));
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge("java.headerMap.get('Referer')", &v, &bridge).unwrap(),
            "https://r.test"
        );
    }

    // ---- java.createSymmetricCrypto（legado 对称加密对象） ----

    #[test]
    fn create_symmetric_crypto_aes_roundtrip() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        let js = r#"
            const c = java.createSymmetricCrypto("AES/CBC/PKCS5Padding", "0123456789abcdef", "fedcba9876543210");
            const b64 = c.encryptBase64("你好 Reader");
            c.decryptStr(b64);
        "#;
        assert_eq!(eval_js_with_bridge(js, &v, &bridge).unwrap(), "你好 Reader");
    }

    #[test]
    fn legacy_aes_base64_decode_to_string_known_answer() {
        // Python 已知答案：AES-128-CBC/PKCS7，key=0123456789abcdef，iv=fedcba9876543210
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge(
                r#"java.aesBase64DecodeToString("78SsqOio6VGktE4eStDdPw==", "0123456789abcdef", "AES/CBC/PKCS5Padding", "fedcba9876543210")"#,
                &v,
                &bridge
            )
            .unwrap(),
            "你好 Reader"
        );
    }

    #[test]
    fn aes_crypt_known_answer() {
        let key = b"0123456789abcdef";
        let iv = b"fedcba9876543210";
        let data = base64::engine::general_purpose::STANDARD
            .decode("78SsqOio6VGktE4eStDdPw==")
            .unwrap();
        let out = aes_crypt(&data, key, "AES/CBC/PKCS5Padding", iv, false).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out),
            "你好 Reader",
            "aes_crypt 直调失败: {}",
            String::from_utf8_lossy(&out)
        );
        let enc = aes_crypt(
            "你好 Reader".as_bytes(),
            key,
            "AES/CBC/PKCS5Padding",
            iv,
            true,
        )
        .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(&enc),
            "78SsqOio6VGktE4eStDdPw==",
            "aes_crypt 加密不匹配: {}",
            base64::engine::general_purpose::STANDARD.encode(&enc)
        );
    }

    #[test]
    fn create_symmetric_crypto_hex_decrypt_and_bytes() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        let js = r#"
            const c = java.createSymmetricCrypto("AES/ECB/PKCS5Padding", "0123456789abcdef");
            const hex = Array.from(c.encrypt("abc")).map(b => b.toString(16).padStart(2, "0")).join("");
            c.decryptStr(hex) + "|" + JSON.stringify(c.decrypt(c.encrypt("abc")));
        "#;
        assert_eq!(
            eval_js_with_bridge(js, &v, &bridge).unwrap(),
            "abc|[97,98,99]"
        );
    }

    #[test]
    fn create_symmetric_crypto_des_and_set_iv() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        let js = r#"
            const c = java.createSymmetricCrypto("DES/CBC/PKCS5Padding", "12345678", "87654321");
            const b64 = c.encryptBase64("正文");
            const first = c.decryptStr(b64);
            c.setIv("0000000000000000");
            const b642 = c.encryptBase64("正文2");
            first + "|" + c.decryptStr(b642);
        "#;
        assert_eq!(eval_js_with_bridge(js, &v, &bridge).unwrap(), "正文|正文2");
    }

    // ---- 纯 JS 兼容 ----

    #[test]
    fn bridge_pure_js_still_works() {
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[("key", "a")]);
        assert_eq!(eval_js_with_bridge("key + 'x'", &v, &bridge).unwrap(), "ax");
        assert_eq!(eval_js_with_bridge("1 + 2", &v, &bridge).unwrap(), "3");
        assert!(eval_js_with_bridge("throw new Error('x')", &v, &bridge).is_err());
    }

    // ---- 数组/对象序列化（bookList 修复） ----

    #[test]
    fn eval_js_array_to_json_string() {
        // 数组结果：eval 字符串出口应输出 JSON 文本而非 "[object Object]"
        let v = vars(&[]);
        assert_eq!(
            eval_js("[{a:1},{b:2}]", &v).unwrap(),
            r#"[{"a":1},{"b":2}]"#
        );
        // 对象结果同样 JSON 化
        assert_eq!(
            eval_js("({name:'A',url:'u'})", &v).unwrap(),
            r#"{"name":"A","url":"u"}"#
        );
        // 字符串/数字/布尔/null 语义不变
        assert_eq!(eval_js("JSON.stringify([1,2])", &v).unwrap(), "[1,2]");
        assert_eq!(eval_js("1+2", &v).unwrap(), "3");
        assert_eq!(eval_js("null", &v).unwrap(), "");
    }

    #[test]
    fn eval_js_json_array_from_parse() {
        // JSON.parse(result).data 数组 → 直接结构化返回（bookList 核心修复场景）
        let v = vars(&[(
            "result",
            r#"{"data":[{"name":"书A","url":"/a"},{"name":"书B","url":"/b"}]}"#,
        )]);
        let json = eval_js_json("JSON.parse(result).data", &v).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "书A");
        assert_eq!(arr[1]["url"], "/b");
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn eval_js_json_object_and_scalars() {
        let v = vars(&[]);
        // 对象
        let json = eval_js_json("({x:1,y:'s'})", &v).unwrap();
        assert_eq!(json["x"], 1);
        assert_eq!(json["y"], "s");
        // 字符串内 JSON 自动解析（JSON.stringify 出口）
        let json = eval_js_json("JSON.stringify([{n:1}])", &v).unwrap();
        assert_eq!(json.as_array().unwrap()[0]["n"], 1);
        // 标量
        assert_eq!(eval_js_json("42", &v).unwrap(), serde_json::json!(42));
        assert_eq!(eval_js_json("3.14", &v).unwrap(), serde_json::json!(3.14));
        assert_eq!(eval_js_json("true", &v).unwrap(), serde_json::json!(true));
        assert_eq!(eval_js_json("'str'", &v).unwrap(), serde_json::json!("str"));
        // undefined/bigint → null（不 panic，区别于 boa to_json 的 todo!）
        assert_eq!(
            eval_js_json("undefined", &v).unwrap(),
            serde_json::json!(null)
        );
        assert_eq!(eval_js_json("1n", &v).unwrap(), serde_json::json!(null));
    }

    #[test]
    fn eval_js_json_with_bridge_roundtrip() {
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[]);
        let json = eval_js_json_with_bridge(
            "java.put('k','v'); JSON.parse('{\"list\":[{\"n\":1}]}').list",
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(json.as_array().unwrap()[0]["n"], 1);
    }

    /// P1-C5：eval_js_json_with_bridge 与 eval_js_with_bridge_limited 对齐——
    /// install_globals（默认变量/unescape/cache 等 shim）+ 隐式 setContent（result 变量
    /// 自动成为 java 解析文档——搜索/探索 JS 规则无需手动 java.setContent）
    #[test]
    fn eval_js_json_with_bridge_globals_and_implicit_set_content() {
        let bridge = JsBridge::new("https://src.test", "源");
        // 隐式 setContent：vars.result 注入文档后 java.getString 直接可用
        let v = vars(&[("result", "<p>你好，世界</p>")]);
        let json = eval_js_json_with_bridge("java.getString('p@text')", &v, &bridge).unwrap();
        assert_eq!(json, serde_json::json!("你好，世界"), "应隐式 setContent");
        // install_globals 的全局 shim（cache/unescape 等）可用
        let json =
            eval_js_json_with_bridge("cache.set('k', 42); cache.get('k')", &v, &bridge).unwrap();
        assert_eq!(json, serde_json::json!(42), "cache shim 应可用");
        let json = eval_js_json_with_bridge("unescape('a%20b')", &v, &bridge).unwrap();
        assert_eq!(json, serde_json::json!("a b"), "unescape shim 应可用");
        // 无 result 变量时与受限路径一致（显式 setContent 仍可用）
        let v2 = vars(&[]);
        let json = eval_js_json_with_bridge(
            "java.setContent('<p>显式</p>'); java.getString('p@text')",
            &v2,
            &bridge,
        )
        .unwrap();
        assert_eq!(json, serde_json::json!("显式"));
    }

    // ---- application Android 兼容层（旧版阅读书源 Header 脚本） ----

    #[test]
    fn application_shim_package_and_dir() {
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[]);
        let json = eval_js_json_with_bridge("application.getPackageName()", &v, &bridge).unwrap();
        assert_eq!(json, serde_json::json!("io.legado.app"));
        let json =
            eval_js_json_with_bridge("application.getFilesDir().getAbsolutePath()", &v, &bridge)
                .unwrap();
        assert!(
            json.as_str().unwrap().ends_with("/files"),
            "虚拟文件目录应可用: {json}"
        );
    }

    #[test]
    fn application_shared_preferences_persist_across_evals() {
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[]);
        eval_js_with_bridge(
            "application.getSharedPreferences('sp_test', 0).edit().putString('token', 'abc').commit()",
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(
            eval_js_with_bridge(
                "application.getSharedPreferences('sp_test', 0).getString('token', '')",
                &v,
                &bridge
            )
            .unwrap(),
            "abc"
        );
        // 默认值路径
        assert_eq!(
            eval_js_with_bridge(
                "application.getSharedPreferences('sp_test', 0).getString('missing', 'def')",
                &v,
                &bridge
            )
            .unwrap(),
            "def"
        );
    }

    #[test]
    fn application_shared_preferences_typed_values() {
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[]);
        eval_js_with_bridge(
            "var e = application.getSharedPreferences('sp_typed', 0).edit(); \
             e.putInt('count', 7); e.putBoolean('on', true); e.putString('name', 'n'); e.commit()",
            &v,
            &bridge,
        )
        .unwrap();
        let code = "var sp = application.getSharedPreferences('sp_typed', 0); \
                    sp.getInt('count', 0) + '|' + sp.getBoolean('on', false) + '|' + sp.getString('name', '')";
        assert_eq!(eval_js_with_bridge(code, &v, &bridge).unwrap(), "7|true|n");
    }

    #[test]
    fn application_shim_no_reference_error_for_legacy_header() {
        // 旧版书源 Header 脚本常见写法：读 SharedPreferences 后再请求，不应 ReferenceError
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[]);
        let code = r#"
            var cfg = application.getSharedPreferences('reader_cfg', 0);
            var token = cfg.getString('token', '');
            var dir = application.getFilesDir().getAbsolutePath();
            JSON.stringify({ token: token, dir: dir, pkg: application.getPackageName() });
        "#;
        let json = eval_js_json_with_bridge(code, &v, &bridge).unwrap();
        assert_eq!(json["token"], "");
        assert_eq!(json["pkg"], "io.legado.app");
        assert!(json["dir"].as_str().unwrap().ends_with("/files"));
    }

    #[test]
    fn application_shim_java_utils() {
        let bridge = JsBridge::new("https://src.test", "源");
        let v = vars(&[]);
        assert_eq!(
            eval_js_with_bridge("URLEncoder.encode('a b&c')", &v, &bridge).unwrap(),
            "a+b%26c"
        );
        assert_eq!(
            eval_js_with_bridge("URLDecoder.decode('a+b%26c')", &v, &bridge).unwrap(),
            "a b&c"
        );
        let uuid = eval_js_with_bridge("UUID.randomUUID()", &v, &bridge).unwrap();
        assert_eq!(uuid.len(), 36, "UUID 应为标准 v4 格式: {uuid}");
        assert!(uuid.chars().filter(|c| *c == '-').count() == 4);
        let now = eval_js_with_bridge(
            "typeof System.currentTimeMillis() === 'number'",
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(now, "true");
        // java 桥接命名空间（install_bridge 覆盖后仍可用）
        assert_eq!(
            eval_js_with_bridge("java.net.URLEncoder.encode('x y')", &v, &bridge).unwrap(),
            "x+y"
        );
        assert_eq!(
            eval_js_with_bridge("java.util.UUID.randomUUID().length", &v, &bridge).unwrap(),
            "36"
        );
        assert_eq!(
            eval_js_with_bridge("java.lang.System.currentTimeMillis() > 0", &v, &bridge).unwrap(),
            "true"
        );
        assert_eq!(
            eval_js_with_bridge(
                "java.util.Base64.getEncoder().encodeToString('abc')",
                &v,
                &bridge
            )
            .unwrap(),
            "YWJj"
        );
        assert_eq!(
            eval_js_with_bridge("android.util.Base64.decode('YWJj')", &v, &bridge).unwrap(),
            "abc"
        );
        // Log 与 context/activity/app 别名不抛 ReferenceError
        let json = eval_js_json_with_bridge(
            "Log.i('tag', 'msg'); JSON.stringify({c: context.getPackageName(), a: activity.getPackageName(), p: app.getPackageName()})",
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(json["c"], "io.legado.app");
        assert_eq!(json["a"], "io.legado.app");
        assert_eq!(json["p"], "io.legado.app");
    }

    /// GAP #94：死循环 JS 触发循环迭代上限 → 报“JS 执行超限”（而非卡死）
    #[test]
    fn eval_js_infinite_loop_hits_limit() {
        let v = vars(&[]);
        // 小上限快速触发（1K 次迭代）；正式入口用 10M 上限，同一映射路径
        for code in ["while(true){}", "for(;;){}"] {
            let err =
                eval_js_with_bridge_limited(code, &v, &JsBridge::default(), 1_000).unwrap_err();
            assert!(
                err.to_string().contains("JS 执行超限"),
                "死循环应报超限: {}（实际: {err}）",
                code
            );
        }
        // 正常循环不受影响
        assert_eq!(
            eval_js("let s=0; for(let i=0;i<100;i++){s+=i} s", &v).unwrap(),
            "4950"
        );
        // 超限同样作用于 JSON 出口
        let err =
            eval_js_json_with_bridge_limited("while(true){}", &v, &JsBridge::default(), 1_000)
                .unwrap_err();
        assert!(err.to_string().contains("JS 执行超限"));
    }

    /// 受限 Context：单独设置小上限可快速触发（10M 上限下死循环也能在合理时间内触发）
    #[test]
    fn limited_context_small_limit_quickly_triggers() {
        let mut ctx = context_with_limit(1_000);
        let err = ctx
            .eval(boa_engine::Source::from_bytes(b"while(true){}".as_slice()))
            .unwrap_err();
        assert!(err
            .to_string()
            .to_lowercase()
            .contains("loop iteration limit"));
    }

    // ---- legacy shim：java.encodeURI ----

    #[test]
    fn bridge_java_encode_uri_gbk_and_utf8() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        // GBK：中文 → GBK 字节百分号编码（中=D6D0 文=CEC4）
        assert_eq!(
            eval_js_with_bridge("java.encodeURI('中文', 'GBK')", &v, &bridge).unwrap(),
            "%D6%D0%CE%C4"
        );
        // gb2312 别名等价
        assert_eq!(
            eval_js_with_bridge("java.encodeURI('中文', 'gb2312')", &v, &bridge).unwrap(),
            "%D6%D0%CE%C4"
        );
        // 默认 utf-8：中=E4B8AD 文=E69687
        assert_eq!(
            eval_js_with_bridge("java.encodeURI('中文')", &v, &bridge).unwrap(),
            "%E4%B8%AD%E6%96%87"
        );
        // 显式 utf-8 与空 charset 均回退默认
        assert_eq!(
            eval_js_with_bridge("java.encodeURI('中', 'utf-8')", &v, &bridge).unwrap(),
            "%E4%B8%AD"
        );
        assert_eq!(
            eval_js_with_bridge("java.encodeURI('中', '')", &v, &bridge).unwrap(),
            "%E4%B8%AD"
        );
        // encodeURI 语义：ASCII 字母数字与保留集不编码；空格 → %20
        assert_eq!(
            eval_js_with_bridge("java.encodeURI(\"a b-c_.!~*'()/?:@&=+$,#\")", &v, &bridge)
                .unwrap(),
            "a%20b-c_.!~*'()/?:@&=+$,#"
        );
    }

    // ---- legacy shim：java.setContent / getString / getElements ----

    #[test]
    fn bridge_java_set_content_get_string() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        let html = r#"<div class="book"><h2>书名A</h2><a href="/b/1">详情</a></div><div class="book"><h2>书名B</h2></div>"#;
        let js_html = serde_json::to_string(html).unwrap();
        // @text 提取器
        assert_eq!(
            eval_js_with_bridge(
                &format!("java.setContent({js_html}); java.getString('div.book@h2@text')"),
                &v,
                &bridge
            )
            .unwrap(),
            "书名A"
        );
        // 无 @ 提取器的选择器规则：元素 HTML → 文本
        assert_eq!(
            eval_js_with_bridge(
                &format!("java.setContent({js_html}); java.getString('div.book@h2')"),
                &v,
                &bridge
            )
            .unwrap(),
            "书名A"
        );
        // 索引选择器（.1 → 第 2 个）
        assert_eq!(
            eval_js_with_bridge(
                &format!("java.setContent({js_html}); java.getString('div.book.1@h2@text')"),
                &v,
                &bridge
            )
            .unwrap(),
            "书名B"
        );
        // 无匹配 → 空串
        assert_eq!(
            eval_js_with_bridge(
                &format!("java.setContent({js_html}); java.getString('div.none@h2@text')"),
                &v,
                &bridge
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn bridge_java_get_elements_returns_outer_html_array() {
        let bridge = JsBridge::new("", "");
        let html =
            r#"<div class="book"><h2>书名A</h2></div><div class="book"><h2>书名B</h2></div>"#;
        let js_html = serde_json::to_string(html).unwrap();
        let json = eval_js_json_with_bridge(
            &format!("java.setContent({js_html}); java.getElements('div.book')"),
            &vars(&[]),
            &bridge,
        )
        .unwrap();
        let arr = json.as_array().expect("getElements 应返回数组");
        assert_eq!(arr.len(), 2, "应匹配 2 个 div.book");
        // 新语义：无提取器时返回元素对象（jsoup Elements 风格——可调 .html()/.text()）
        assert!(
            arr[0].is_object(),
            "元素应为对象（含 html/text 等方法）: {}",
            arr[0]
        );
        // 带 @html 提取器 → 字符串数组（outerHTML）
        let js_html2 = serde_json::to_string(html).unwrap();
        let json2 = eval_js_json_with_bridge(
            &format!("java.setContent({js_html2}); java.getElements('div.book@html')"),
            &vars(&[]),
            &bridge,
        )
        .unwrap();
        let arr2 = json2.as_array().expect("应返回数组");
        assert_eq!(arr2.len(), 2);
        assert!(
            arr2[0]
                .as_str()
                .unwrap()
                .contains("<div class=\"book\"><h2>书名A</h2></div>"),
            "元素应返回 outerHTML: {}",
            arr2[0]
        );
        assert!(arr2[1].as_str().unwrap().contains("书名B"));
    }

    #[test]
    fn bridge_java_doc_requires_set_content() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        let err = eval_js_with_bridge("java.getString('div@text')", &v, &bridge).unwrap_err();
        assert!(
            err.to_string().contains("setContent"),
            "应提示先调用 setContent: {err}"
        );
        let err = eval_js_with_bridge("java.getElements('div')", &v, &bridge).unwrap_err();
        assert!(err.to_string().contains("setContent"), "{err}");
    }

    #[test]
    fn bridge_java_get_webview_ua() {
        let bridge = JsBridge::new("", "");
        let ua = eval_js_with_bridge("java.getWebViewUA()", &vars(&[]), &bridge).unwrap();
        assert_eq!(ua, JS_WEBVIEW_UA);
        assert!(ua.contains("Chrome/120"), "UA 应含 Chrome 标识: {ua}");
    }

    // ---- legacy shim：java.startBrowserAwait（缺浏览器明确报错 / 成功路径 / 求解失败）----

    /// startBrowserAwait 测试串行化（全局求解钩子/浏览器可用性覆盖为共享状态，
    /// 并行测试会互相踩踏——见 solve_error_propagates 钩子泄漏到 returns_js_object）
    static SOLVE_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 浏览器不可用：返回明确错误（不启动浏览器、不发请求）
    #[test]
    fn bridge_java_start_browser_await_browser_unavailable() {
        let _guard = SOLVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        force_js_browser_available(Some(false));
        let bridge = JsBridge::new("https://src.test", "源").with_namespace("default");
        let err = eval_js_with_bridge(
            "java.startBrowserAwait('https://a.test/book/1', '标题', true)",
            &vars(&[]),
            &bridge,
        )
        .unwrap_err();
        force_js_browser_available(None);
        assert!(
            err.to_string().contains("浏览器"),
            "应提示浏览器不可用: {err}"
        );
    }

    /// 成功路径（求解钩子注入）：返回 {body, cookies, status} JS 对象
    #[test]
    fn bridge_java_start_browser_await_returns_js_object() {
        let _guard = SOLVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        force_js_browser_available(Some(true));
        register_js_solve_hook(Some(Arc::new(|url, cookies| {
            assert_eq!(url, "https://a.test/book/1?q=1");
            assert!(cookies.is_empty(), "无 cookie 存储时应为空");
            Ok((
                "<html><body>hello-page</body></html>".to_string(),
                vec![
                    ("cf_clearance".to_string(), "xyz".to_string()),
                    ("sid".to_string(), "1".to_string()),
                ],
                "Mozilla/5.0 test UA".to_string(),
            ))
        })));
        let bridge = JsBridge::new("https://src.test", "源").with_namespace("default");
        let r = eval_js_with_bridge(
            "var w = java.startBrowserAwait('https://a.test/book/1?q=1', '标题', false); w.body + '|' + w.status + '|' + w.cookies.length + '|' + w.cookies[0]",
            &vars(&[]),
            &bridge,
        );
        register_js_solve_hook(None);
        force_js_browser_available(None);
        assert_eq!(
            r.unwrap(),
            "<html><body>hello-page</body></html>|200|2|cf_clearance=xyz"
        );
    }

    /// 求解失败：错误信息透传（含 url 上下文）
    #[test]
    fn bridge_java_start_browser_await_solve_error_propagates() {
        let _guard = SOLVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        force_js_browser_available(Some(true));
        register_js_solve_hook(Some(Arc::new(|_, _| Err(anyhow!("模拟浏览器求解失败")))));
        let bridge = JsBridge::new("", "").with_namespace("default");
        let err = eval_js_with_bridge(
            "java.startBrowserAwait('https://a.test/x', 't', true)",
            &vars(&[]),
            &bridge,
        )
        .unwrap_err();
        register_js_solve_hook(None);
        force_js_browser_available(None);
        assert!(
            err.to_string().contains("模拟浏览器求解失败"),
            "求解错误应透传: {err}"
        );
    }

    // ---- legacy shim：java.ajax（本地 HTTP 服务器端到端）----

    /// 微型 HTTP 服务器：返回固定响应体；捕获收到的请求（方法/路径/body）
    async fn serve_echo(
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
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                captured.lock().unwrap().push(req);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut resp = head.into_bytes();
                resp.extend_from_slice(&body);
                let _ = sock.write_all(&resp).await;
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn bridge_java_ajax_get_returns_body() {
        // P1 SSRF：java.ajax 走 crawler::fetch（入口公网校验）——mock 绑定 127.0.0.1，
        // 持放行守卫（仅测试代码可设置）
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let url = rt.block_on(serve_echo(b"hello-ajax".to_vec(), captured.clone()));
        let bridge = JsBridge::new("", "").with_namespace("default");
        let r = eval_js_with_bridge(&format!("java.ajax('{url}/x?a=1')"), &vars(&[]), &bridge);
        assert_eq!(r.unwrap(), "hello-ajax");
        // 无 cookie 存储注册：请求不应带 Cookie 头
        let req = captured.lock().unwrap()[0].to_lowercase();
        assert!(!req.contains("cookie:"), "不应带 Cookie 头: {req}");
    }

    #[test]
    fn bridge_java_ajax_post_suffix_gbk() {
        // P1 SSRF：mock 绑定 127.0.0.1，持放行守卫
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        // POST + `,{...}` 后缀（method/body/charset）：GBK 字节响应正确解码
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let gbk = {
            let (bytes, _, _) = encoding_rs::GBK.encode("中文响应");
            bytes.into_owned()
        };
        let url = rt.block_on(serve_echo(gbk, captured.clone()));
        let bridge = JsBridge::new("", "").with_namespace("default");
        let code = format!(
            "java.ajax('{url}/p,{{\"method\":\"POST\",\"body\":\"k=v\",\"charset\":\"GBK\"}}')"
        );
        let r = eval_js_with_bridge(&code, &vars(&[]), &bridge);
        assert_eq!(r.unwrap(), "中文响应", "GBK 响应应按 charset 解码");
        let req = captured.lock().unwrap()[0].clone();
        assert!(req.starts_with("POST /p"), "应 POST 到 /p: {req}");
        assert!(req.contains("k=v"), "应携带 body: {req}");
    }

    /// E16：base64 flags 变体 / ByteArray 解码 / digestBase64Str / logType
    #[test]
    fn test_base64_flags_and_digest() {
        let bridge = JsBridge::default();
        // URL_SAFE：'+'→'-'
        assert_eq!(
            eval_js_with_bridge("java.base64Encode('a?b', 8)", &vars(&[]), &bridge).unwrap(),
            "YT9i"
        );
        // NO_PADDING：去掉 '='
        assert_eq!(
            eval_js_with_bridge("java.base64Encode('ab', 1)", &vars(&[]), &bridge).unwrap(),
            "YWI"
        );
        // NO_WRAP：默认折行 vs 不折（>76 字符输入）
        let long_in = "x".repeat(120);
        let wrapped = eval_js_with_bridge(
            &format!("java.base64Encode('{}')", long_in),
            &vars(&[]),
            &bridge,
        )
        .unwrap();
        assert!(wrapped.contains('\n'), "DEFAULT 应折行");
        let nowrap = eval_js_with_bridge(
            &format!("java.base64Encode('{}', 2)", long_in),
            &vars(&[]),
            &bridge,
        )
        .unwrap();
        assert!(!nowrap.contains('\n'), "NO_WRAP 不应折行");
        // 解码容忍换行与缺省填充
        assert_eq!(
            eval_js_with_bridge("java.base64DecodeToString('YWJj', 0)", &vars(&[]), &bridge)
                .unwrap(),
            "abc"
        );
        // base64Decode → number[]
        assert_eq!(
            eval_js_with_bridge(
                "JSON.stringify(java.base64Decode('YWJj'))",
                &vars(&[]),
                &bridge
            )
            .unwrap(),
            "[97,98,99]"
        );
        // digestBase64Str
        assert_eq!(
            eval_js_with_bridge("java.digestBase64Str('abc','md5')", &vars(&[]), &bridge).unwrap(),
            "kAFQmDzST7DWlj99KOF/cg=="
        );
        // logType
        assert_eq!(
            eval_js_with_bridge("java.logType('s')", &vars(&[]), &bridge).unwrap(),
            "string"
        );
    }

    /// E11：cache 对象 shim——跨 eval 持久、typed 存取、命名空间隔离
    #[test]
    fn bridge_cache_object() {
        let bridge = JsBridge::new("", "").with_namespace("default");
        // put/get 字符串往返（跨两次 eval 可见——进程级持久）
        let r = eval_js_with_bridge(
            "cache.put('k', 'v', 60); cache.get('k')",
            &vars(&[]),
            &bridge,
        )
        .unwrap();
        assert_eq!(r, "v");
        // getInt 默认值 + putInt
        assert_eq!(
            eval_js_with_bridge("cache.getInt('n', 7)", &vars(&[]), &bridge).unwrap(),
            "7"
        );
        eval_js_with_bridge("cache.putInt('n', 5, 0)", &vars(&[]), &bridge).unwrap();
        assert_eq!(
            eval_js_with_bridge("cache.getInt('n', 7)", &vars(&[]), &bridge).unwrap(),
            "5"
        );
        // getLong/getDouble
        eval_js_with_bridge("cache.putLong('ts', 1700000000000, 0)", &vars(&[]), &bridge).unwrap();
        assert_eq!(
            eval_js_with_bridge("cache.getLong('ts')", &vars(&[]), &bridge).unwrap(),
            "1700000000000"
        );
        // delete 后回默认
        eval_js_with_bridge("cache.delete('n')", &vars(&[]), &bridge).unwrap();
        assert_eq!(
            eval_js_with_bridge("cache.getInt('n', 7)", &vars(&[]), &bridge).unwrap(),
            "7"
        );
        // 命名空间隔离：其他 ns 读不到 default 写入的 k
        let other = JsBridge::new("", "").with_namespace("other");
        let r = eval_js_with_bridge("cache.get('k')", &vars(&[]), &other).unwrap();
        assert_eq!(r, "", "其他命名空间不应读到该 key（null → 空串）");
    }

    /// EG4：cache.put 落库 → 清空内存（模拟重启）→ 启动加载恢复；过期条目不恢复；
    /// miss 读穿透可从 SQLite 回填热缓存
    #[tokio::test]
    async fn eg4_js_cache_sqlite_roundtrip() {
        // 独立临时库（唯一 ns 防并发测试串扰）
        let dir = std::env::temp_dir().join(format!("reader-eg4-js-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = crate::storage::init(&config).await.unwrap();
        register_js_cache_storage(storage.clone());

        let ns = format!("eg4-{}", uuid::Uuid::new_v4());
        // ① put：内存 + SQLite 双写（put 为同步等待落库）
        cache_put_value(&ns, "token", "abc123".to_string(), 600);
        cache_put_value(&ns, "dead", "old-value".to_string(), 1);
        let row = storage.get_js_cache(&ns, "token").await.unwrap();
        assert_eq!(row.unwrap().0, "abc123", "put 应已同步落库");

        // ② 模拟重启：清空本 ns 内存 + 等过期 + 注销存储
        let prefix = format!("{ns}\u{1}");
        CACHE_STORE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k, _| !k.starts_with(&prefix));
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        clear_js_cache_storage();

        // ③ 重新注册 + 启动加载（等价新进程启动路径）
        register_js_cache_storage(storage.clone());
        let restored = load_js_cache_from_db(&storage).await.unwrap();
        assert!(restored >= 1, "至少应恢复 token 一条（实际 {restored}）");
        assert_eq!(
            cache_get_value(&ns, "token"),
            Some("abc123".to_string()),
            "重启后登录 token 应从 SQLite 恢复"
        );
        assert_eq!(cache_get_value(&ns, "dead"), None, "已过期条目不得被恢复");

        // ④ 读穿透：删掉热缓存后再读——应从 SQLite 回填并再次命中
        CACHE_STORE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cache_store_key(&ns, "token"));
        assert_eq!(
            cache_get_value(&ns, "token"),
            Some("abc123".to_string()),
            "miss 应读穿透 SQLite 回填"
        );

        // 收尾：注销全局注册（避免其他测试写入本临时库），清理临时目录（失败忽略——Windows 句柄延迟）
        clear_js_cache_storage();
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 连接失败：legacy ajax 语义——**返回错误文本**不抛异常（书源自行判断内容有效性）
    #[test]
    fn bridge_java_ajax_error_returns_text() {
        let bridge = JsBridge::new("", "").with_namespace("default");
        // 127.0.0.1:1 大概率无服务 → 快速连接失败
        let r = eval_js_with_bridge("java.ajax('http://127.0.0.1:1/nope')", &vars(&[]), &bridge)
            .expect("ajax 失败应返回错误文本而非抛异常");
        assert!(
            r.contains("java.ajax 失败"),
            "失败应返回含 op 名的错误文本: {r}"
        );
        // post 保持抛异常语义（legacy connect/post 链路）
        let err = eval_js_with_bridge(
            "java.post('http://127.0.0.1:1/nope', 'a=1')",
            &vars(&[]),
            &bridge,
        )
        .unwrap_err();
        assert!(err.to_string().contains("java.post 失败"), "{err}");
    }

    /// P3-A：java.post——POST 请求（复用 ajax 管线：书源 header/cookie + 显式 body）
    #[test]
    fn bridge_java_post_sends_post_with_body() {
        // P1 SSRF：mock 绑定 127.0.0.1，持放行守卫
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let url = rt.block_on(serve_echo(b"post-ok".to_vec(), captured.clone()));
        let bridge = JsBridge::new("", "").with_namespace("default");
        let code = format!("java.post('{url}/p', 'a=1&b=2')");
        let r = eval_js_with_bridge(&code, &vars(&[]), &bridge);
        assert_eq!(r.unwrap(), "post-ok");
        let req = captured.lock().unwrap()[0].clone();
        assert!(req.starts_with("POST /p"), "应 POST 到 /p: {req}");
        assert!(req.contains("a=1&b=2"), "应携带 body: {req}");
    }

    /// P3-A：java.ajaxAll——URL 数组逐个 GET，返回 StrResponse 对象数组（.body()/.url/.code）
    #[test]
    fn bridge_java_ajax_all_returns_bodies() {
        // P1 SSRF：mock 绑定 127.0.0.1，持放行守卫
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let url = rt.block_on(serve_echo(b"hello-ajax".to_vec(), captured.clone()));
        let bridge = JsBridge::new("", "").with_namespace("default");
        let code = format!(
            "var r = java.ajaxAll(['{url}/a', '{url}/b']); \
             r.length + '|' + r[0].body() + '|' + r[1].body() + '|' + r[0].code"
        );
        let r = eval_js_with_bridge(&code, &vars(&[]), &bridge);
        assert_eq!(r.unwrap(), "2|hello-ajax|hello-ajax|200");
        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 2, "应请求 2 次");
        assert!(reqs[0].starts_with("GET /a"), "req0: {}", reqs[0]);
        assert!(reqs[1].starts_with("GET /b"), "req1: {}", reqs[1]);
    }

    /// P3-A：java.ajaxAll 非数组参数 → 抛 JS 异常（明确错误）
    #[test]
    fn bridge_java_ajax_all_rejects_non_array() {
        let bridge = JsBridge::new("", "").with_namespace("default");
        let err =
            eval_js_with_bridge("java.ajaxAll('not-an-array')", &vars(&[]), &bridge).unwrap_err();
        assert!(err.to_string().contains("URL 数组"), "应报参数错误: {err}");
    }

    /// P1-3：source.put 条数上限——第 1001 条拒绝，已有 key 更新不受条数限制
    #[test]
    fn test_source_put_entry_limit() {
        let mut vars = HashMap::new();
        for i in 0..SOURCE_VARS_MAX_ENTRIES {
            assert!(
                source_put_limited(&mut vars, &format!("k{i}"), "v"),
                "前 {SOURCE_VARS_MAX_ENTRIES} 条应写入"
            );
        }
        assert_eq!(vars.len(), SOURCE_VARS_MAX_ENTRIES);
        // 新 key 超限拒绝
        assert!(!source_put_limited(&mut vars, "overflow", "v"));
        assert_eq!(vars.len(), SOURCE_VARS_MAX_ENTRIES, "拒绝后不增长");
        // 已有 key 更新仍允许（不计新条数）
        assert!(source_put_limited(&mut vars, "k0", "new-value"));
        assert_eq!(vars.get("k0").unwrap(), "new-value");
    }

    /// P1-3：source.put 字节上限——单值超 1MB 拒绝；累加超限拒绝（含更新已有 key）
    #[test]
    fn test_source_put_byte_limit() {
        let mut vars = HashMap::new();
        // 600KB 值写入成功
        let big = "x".repeat(600 * 1024);
        assert!(source_put_limited(&mut vars, "a", &big));
        // 再写 600KB → 总量超 1MB → 拒绝
        assert!(!source_put_limited(&mut vars, "b", &big));
        assert!(!vars.contains_key("b"));
        // 更新已有 key 撑破上限同样拒绝（值保持原样）
        assert!(!source_put_limited(&mut vars, "a", &"y".repeat(700 * 1024)));
        assert_eq!(vars.get("a").unwrap().len(), big.len());
        // 小值写入正常
        assert!(source_put_limited(&mut vars, "c", "small"));
    }

    /// P1-3：source.put 空 key/空值正常（legado 语义兼容）
    #[test]
    fn test_source_put_empty_ok() {
        let mut vars = HashMap::new();
        assert!(source_put_limited(&mut vars, "", ""));
        assert!(source_put_limited(&mut vars, "k", ""));
        assert_eq!(vars.get("k").unwrap(), "");
    }

    /// java.timeFormat / timeFormatUTC：legado 时间格式化（UTC+8 毫秒偏移）
    #[test]
    fn test_format_time_millis() {
        // 1970-01-01 00:00:00 UTC + 8h
        assert_eq!(
            format_time_millis(0, "yyyy-MM-dd HH:mm:ss", 28_800_000),
            "1970-01-01 08:00:00"
        );
        assert_eq!(
            format_time_millis(0, "yyyy/MM/dd HH:mm", 0),
            "1970/01/01 00:00"
        );
        // 单字符令牌（无补零）+ 字面量
        assert_eq!(format_time_millis(0, "y-M-d H:m", 0), "1970-1-1 0:0");
        // 毫秒
        assert_eq!(format_time_millis(1234, "HH:mm:ss.SSS", 0), "00:00:01.234");
        // 非法时间戳 → 空串（legado 返回 null）
        assert_eq!(format_time_millis(i64::MAX, "yyyy", 0), "");
    }

    /// 全局 gzip：UTF-8 → GZip → base64（可逆）
    #[test]
    fn test_gzip_base64_roundtrip() {
        let mut ctx = Context::default();
        let out = gzip_base64(
            &JsValue::undefined(),
            &[JsValue::from(JsString::from("hello world"))],
            &mut ctx,
        )
        .unwrap();
        let b64 = out.as_string().unwrap().to_std_string_escaped();
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64).unwrap();
        use std::io::Read as _;
        let mut dec = flate2::read::GzDecoder::new(bytes.as_slice());
        let mut text = String::new();
        dec.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello world");
        // 空输入 → 空串
        let empty = gzip_base64(
            &JsValue::undefined(),
            &[JsValue::from(JsString::from(""))],
            &mut ctx,
        )
        .unwrap();
        assert_eq!(empty.as_string().unwrap().to_std_string_escaped(), "");
    }

    /// cookie 键值解析（legado CookieStore.cookieToMap）
    #[test]
    fn test_cookie_map_parses() {
        let map = cookie_map("a=1; b = 2; c=");
        assert_eq!(map.get("a").map(String::as_str), Some("1"));
        assert_eq!(map.get("b").map(String::as_str), Some("2"));
        assert!(!map.contains_key("c"));
    }

    /// jsLib（书源共享 JS 作用域）在 header/搜索/正文 JS 前执行——AES_KEY/sign 等全局可用
    #[test]
    fn test_js_lib_globals_injected() {
        let src = BookSource {
            book_source_url: "https://a.com".into(),
            book_source_name: "A源".into(),
            js_lib: Some("const AES_KEY = 'abc'; function sign(x){ return x + '-s'; }".to_string()),
            ..Default::default()
        };
        let bridge = JsBridge::from_source(&src, "default");
        let out =
            eval_js_with_bridge("AES_KEY + ':' + sign('k')", &HashMap::new(), &bridge).unwrap();
        assert_eq!(out, "abc:k-s");
    }

    /// 书源 variable 顶层键注入全局（header JS 直接引用 API_KEY 等变量）
    #[test]
    fn test_source_variable_top_level_injected() {
        let src = BookSource {
            book_source_url: "https://a.com".into(),
            book_source_name: "A源".into(),
            variable: Some(serde_json::json!({"API_KEY": "xyz", "num": 3})),
            ..Default::default()
        };
        let bridge = JsBridge::from_source(&src, "default");
        let out = eval_js_with_bridge("API_KEY + ':' + num", &HashMap::new(), &bridge).unwrap();
        assert_eq!(out, "xyz:3");
    }

    /// URL/URLSearchParams 全局 shim：相对 URL 拼接、searchParams 读取
    #[test]
    fn test_url_and_url_search_params_available() {
        let out = eval_js_json(
            r#"(() => {
                const u = new URL('bookajax/s?q=1', 'https://a.com/path/x');
                const p = new URLSearchParams('a=1&b=%E4%B8%AD');
                return u.href + '|' + p.get('b');
            })()"#,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(out, "https://a.com/path/bookajax/s?q=1|中");
    }

    /// 补齐：java.downloadFile——写缓存返回相对路径名，readFile/readTxtFile 可读回；
    /// 同 URL 重写覆盖；getFile stub 的 path/exists 与缓存坐标系一致
    #[test]
    fn bridge_java_download_file_roundtrip_and_get_file() {
        // 唯一命名空间避免并行测试互扰（sanitize_ns 只留字母数字）
        let ns = format!(
            "dlt{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let bridge = JsBridge::new("", "").with_namespace(ns.clone());
        let v = vars(&[]);
        // 写入 → 相对路径名 → readFile/readTxtFile 往返
        let r = eval_js_with_bridge(
            "var p = java.downloadFile('下载内容', 'https://src.test/a/b'); \
             p + '|' + java.readFile(p) + '|' + java.readTxtFile(p)",
            &v,
            &bridge,
        )
        .unwrap();
        let parts: Vec<&str> = r.splitn(3, '|').collect();
        assert_eq!(parts.len(), 3, "{r}");
        assert!(
            parts[0].ends_with(".txt") && !parts[0].contains(['/', '\\']),
            "应返回相对文件名: {}",
            parts[0]
        );
        assert_eq!(parts[1], "下载内容");
        assert_eq!(parts[2], "下载内容");
        // 空 URL → 空串（legacy AnalyzeUrl.type 缺失语义）
        assert_eq!(
            eval_js_with_bridge("java.downloadFile('x', '')", &v, &bridge).unwrap(),
            ""
        );
        // 同 URL 重写覆盖；getFile stub：path 含该文件名、exists 实时为 true；
        // 不存在的路径 exists 为 false
        let r2 = eval_js_with_bridge(
            "var p = java.downloadFile('v2', 'https://src.test/a/b'); \
             var f = java.getFile(p); \
             java.readFile(p) + '|' + f.path.endsWith(p) + '|' + f.exists() + '|' + java.getFile('no-such.bin').exists()",
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(r2, "v2|true|true|false");
        let _ = std::fs::remove_dir_all(js_cache_dir().join(sanitize_ns(&ns)));
    }

    /// 补齐：aes 系列 *ToByteArray 变体——输出 number[]，与 *String 版共用底层加解密
    #[test]
    fn bridge_java_aes_to_byte_array_variants() {
        let bridge = JsBridge::new("", "");
        let v = vars(&[]);
        let key = "0123456789abcdef";
        let iv = "fedcba9876543210";
        let t = "AES/CBC/PKCS5Padding";
        // 1) 已知答案：aesBase64DecodeToByteArray（先 base64 解码再解密）→ 明文 UTF-8 字节
        let expected: Vec<String> = "你好 Reader"
            .as_bytes()
            .iter()
            .map(|b| b.to_string())
            .collect();
        let r = eval_js_with_bridge(
            &format!(
                r#"JSON.stringify(java.aesBase64DecodeToByteArray("78SsqOio6VGktE4eStDdPw==", "{key}", "{t}", "{iv}"))"#
            ),
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(r, format!("[{}]", expected.join(",")));
        // 2) aesEncodeToByteArray / aesDecodeToByteArray：密文 number[] 直接回传可解回原文
        let r = eval_js_with_bridge(
            &format!(
                "java.aesDecodeToByteArray(java.aesEncodeToByteArray('abc', '{key}', '{t}', '{iv}'), '{key}', '{t}', '{iv}').join(',')"
            ),
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(r, "97,98,99");
        // 3) aesEncodeToBase64ByteArray：加密后 base64 化的字节——
        //    还原字符串与 aesEncodeToBase64String 一致，经 base64DecodeToByteArray 解回与
        //    aesEncodeToByteArray 密文一致
        let plain = "你好 Reader";
        let r = eval_js_with_bridge(
            &format!(
                "var b64 = String.fromCharCode.apply(null, java.aesEncodeToBase64ByteArray('{plain}', '{key}', '{t}', '{iv}')); \
                 b64 === java.aesEncodeToBase64String('{plain}', '{key}', '{t}', '{iv}') \
                   && JSON.stringify(java.base64DecodeToByteArray(b64)) === JSON.stringify(java.aesEncodeToByteArray('{plain}', '{key}', '{t}', '{iv}'))"
            ),
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(r, "true");
        // 4) 已知答案锚定：还原出的字符串即密文 base64（与 legacy encryptAES2Base64 一致，
        //    aesEncodeToBase64String 就是它的 UTF-8 视图）
        let r = eval_js_with_bridge(
            &format!(
                "String.fromCharCode.apply(null, java.aesEncodeToBase64ByteArray('{plain}', '{key}', '{t}', '{iv}'))"
            ),
            &v,
            &bridge,
        )
        .unwrap();
        assert_eq!(r, "78SsqOio6VGktE4eStDdPw==");
    }
}
