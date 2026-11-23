//! JS 运行时核心：`JsBridge`、受限 Context、同线程轻量无桥接沙箱复用池、
//! `block_on_task` 异步安全边界与 Boa 值转换工具。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::{
    Context, JsError, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source,
};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::model::BookSource;

use super::bridge_java::{install_bridge, install_globals};

/// JS 循环迭代上限（GAP #94：boa 死循环防卡死）。
/// boa 0.19 的 RuntimeLimits 无独立“指令数”上限，循环迭代计数（loop_iteration_limit）
/// 是其等价物：每次循环迭代 +1，超限抛 RangeError。10M 足够正常书源规则（一般 <1K 次）
/// 循环，而 `while(true){}` 这类死循环会在毫秒级内触发。
pub const JS_LOOP_ITERATION_LIMIT: u64 = 10_000_000;

/// JS 桥接同步等待上限（M6：60s → 10s）——书源规则并发执行时，单次桥接调用对
/// axum worker 线程的阻塞占用最多 10s；超时后调用方立即返回错误、不再等待工作线程
/// （工作线程继续运行至其内部 fetch 超时后自行退出，不占用 worker）。
pub(super) const BRIDGE_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// 无桥接默认全局字符串（`install_globals` 对空 `JsBridge::default()` 注册的 5 个可变基线键）
const DEFAULT_MUTABLE_GLOBALS: &[&str] = &["baseUrl", "base_url", "src", "type", "urlIP"];

/// 书源 JS 桥接：持有书源信息与可被 JS 读写的状态（请求头 / java 临时变量）。
///
/// - 每次搜索/详情流程创建一次（`JsBridge::new(source_key, source_name)`），
///   同流程内多次 `eval_js_with_bridge` 共享 `java.put/get` 与 `java.headerMap`；
/// - `source.put/get` 走全局存储，跨流程/跨 bridge 实例可见（按书源 key 隔离）；
/// - 请求头：`set_headers` 注入初始值，JS 内 `java.headerMap.put` 改写，
///   eval 后 `headers()` 取回用于实际请求。
#[derive(Clone)]
pub struct JsBridge {
    pub(super) inner: Arc<JsBridgeInner>,
}

pub(super) struct JsBridgeInner {
    /// 书源 key（URL），`source.getKey()` 返回
    pub(super) source_key: String,
    /// 书源名称，`source.getName()` 返回
    pub(super) source_name: String,
    /// 书源登录 URL（JS 代码），`source.loginUrl` 返回（`eval(String(source.loginUrl))` 模式）
    pub(super) login_url: String,
    /// 书源变量（`source.getVariable()`，legado 书源变量配置）
    pub(super) source_variable: String,
    /// 书源 JS 库（`source.jsLib`——共享全局作用域，定义 AES_KEY/sign 等常量）
    pub(super) js_lib: String,
    /// 书源 header（`source.header`，JSON 文本）
    pub(super) source_header: String,
    /// 用户命名空间（书源 cookie 按用户隔离；空 = 无 cookie 上下文，
    /// `java.ajax`/`java.startBrowserAwait` 不带书源 cookie）
    pub(super) ns: String,
    /// 请求头：`java.headerMap` 的底层存储（JS 可改写）
    pub(super) headers: Mutex<HashMap<String, String>>,
    /// `java.put/get` 临时变量（本 bridge 生命周期内共享）
    pub(super) java_vars: Mutex<HashMap<String, String>>,
    /// `java.setContent` 设置的当前解析文档（`java.getString/getElements` 的解析源）
    pub(super) doc: Mutex<Option<String>>,
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
        let mut inner = (*self.inner).clone();
        inner.ns = ns.into();
        self.inner = Arc::new(inner);
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

/// 构造受限 Context（runtime limits：循环迭代上限，防死循环）
pub fn new_limited_context() -> Context {
    context_with_limit(JS_LOOP_ITERATION_LIMIT)
}

/// 以指定循环迭代上限构造 Context（测试用小上限快速触发）
pub(super) fn context_with_limit(loop_limit: u64) -> Context {
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
    static LAST_JS_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 取走（并清空）最近一次 JS eval 失败消息；无则 None。
pub fn take_last_js_error() -> Option<String> {
    LAST_JS_ERROR.with(|c| c.borrow_mut().take())
}

/// 将 boa 错误映射为友好文案：超限 → "JS 执行超限"；同时记录原始消息（debug 输出用）
pub(super) fn map_js_error(e: boa_engine::JsError) -> anyhow::Error {
    let msg = e.to_string();
    LAST_JS_ERROR.with(|c| *c.borrow_mut() = Some(msg.clone()));
    if msg.to_lowercase().contains("loop iteration limit") {
        anyhow!("JS 执行超限")
    } else {
        anyhow!("JS 执行失败: {msg}")
    }
}

/// 异步安全阻塞边界：处于 Tokio 多线程运行时工作线程时自动套一层 `block_in_place`，
/// 避免重度桥接求值或同步等待阻塞调度器；处于 current_thread（如 `#[tokio::test]`）
/// 或非 Tokio 线程时直接同步执行，不触发 panic。
fn run_in_blocking_boundary<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// 同步等待异步任务结果（boa 引擎为同步调用环境；`java.ajax`/`java.startBrowserAwait`
/// 需要等待网络/浏览器结果）。
///
/// 阻塞说明：书源 JS 通常在 tokio 工作线程（axum handler）中执行，本函数用专用
/// worker 线程 + 独立 current_thread tokio runtime 执行异步任务，当前线程经
/// `run_in_blocking_boundary` + std mpsc `recv_timeout` 阻塞等待（最多 `timeout`，
/// 调用方统一传 [`BRIDGE_WAIT_TIMEOUT`]）。
/// 不在 ambient runtime 上直接 spawn——避免 current_thread runtime（如 `#[tokio::test]`）下
/// 「阻塞唯一 worker → 任务永不被轮询」的死锁；超时路径调用方立即返回错误（M6）。
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
    let waited = run_in_blocking_boundary(|| rx.recv_timeout(timeout));
    match waited {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(anyhow!("{what} 超时（{}s）", timeout.as_secs()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow!("{what} worker 异常退出"))
        }
    }
}

// ---- 同线程轻量无桥接 JS 沙箱复用池 ----

/// 线程局部复用的无状态 Boa 沙箱：预注册全部内置工具函数与空 bridge，
/// 避免列表解析 30~100 条目时每字段从零初始化整套 Boa 引擎与宿主函数。
struct PooledSandbox {
    context: Context,
    bridge: JsBridge,
    baseline_keys: HashSet<String>,
}

impl PooledSandbox {
    fn new() -> Result<Self> {
        let mut context = new_limited_context();
        let bridge = JsBridge::default();
        install_globals(&mut context, &bridge)?;
        install_bridge(&mut context, &bridge)?;
        let mut baseline_keys = HashSet::new();
        let global = context.global_object();
        if let Ok(keys) = global.own_property_keys(&mut context) {
            for key in keys {
                if let PropertyKey::String(s) = key {
                    baseline_keys.insert(s.to_std_string_escaped());
                }
            }
        }
        Ok(Self {
            context,
            bridge,
            baseline_keys,
        })
    }

    /// 除 5 个默认可变字符串全局外，其余基线全局均视为受保护键（若 vars 同名覆盖则走独立 Context）
    fn is_protected_key(&self, key: &str) -> bool {
        self.baseline_keys.contains(key) && !DEFAULT_MUTABLE_GLOBALS.contains(&key)
    }

    fn reset_bridge_state(&self) {
        self.bridge
            .inner
            .java_vars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.bridge
            .inner
            .headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self
            .bridge
            .inner
            .doc
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 清理本次求值注入的临时 `vars` / 上下文绑定与脚本动态写入的全局属性，
    /// 重置默认全局字符串与 runtime limits；若存在不可删除属性（如顶层 var/function）返回 false。
    fn cleanup(&mut self) -> bool {
        self.reset_bridge_state();
        self.context
            .runtime_limits_mut()
            .set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
        let global = self.context.global_object();
        let Ok(keys) = global.own_property_keys(&mut self.context) else {
            return false;
        };
        for key in keys {
            let is_baseline = matches!(
                &key,
                PropertyKey::String(s) if self.baseline_keys.contains(&s.to_std_string_escaped())
            );
            if !is_baseline {
                match global.delete_property_or_throw(key, &mut self.context) {
                    Ok(true) => {}
                    _ => return false,
                }
            }
        }
        for name in DEFAULT_MUTABLE_GLOBALS {
            if self
                .context
                .register_global_property(
                    JsString::from(*name),
                    JsValue::from(JsString::from("")),
                    Attribute::all(),
                )
                .is_err()
            {
                return false;
            }
        }
        self.context.clear_kept_objects();
        true
    }
}

// 关键 TLS 析构顺序保证：
// `SANDBOX_POOL_READY` / `SANDBOX_IN_USE` 均为无析构器的 `Cell<bool>`。
// 线程首次进入池化路径时，先执行 `PooledSandbox::new()` 触发 `boa_gc` 内部 `BOA_GC`
// 线程局部初始化并注册其析构器，然后才首次访问 `SANDBOX_POOL` 注册池析构器。
// 线程退出按 LIFO 顺序先销毁 `SANDBOX_POOL`（释放 `Context` 持有的 `Gc`），
// 再销毁 `BOA_GC`，彻底避免线程退出期 TLS 析构倒序问题。
thread_local! {
    static SANDBOX_POOL_READY: Cell<bool> = const { Cell::new(false) };
    static SANDBOX_IN_USE: Cell<bool> = const { Cell::new(false) };
    static SANDBOX_POOL: RefCell<Option<PooledSandbox>> = const { RefCell::new(None) };
}

struct PoolLeaseGuard;

impl Drop for PoolLeaseGuard {
    fn drop(&mut self) {
        let _ = SANDBOX_IN_USE.try_with(|flag| flag.set(false));
    }
}

/// 静态扫描脚本是否可能污染全局词法环境（顶层 `let`/`const`/`class`/`var`/`function`）
/// 或原型链/动态 eval。命中时本次仍正常执行，但执行后丢弃沙箱而不归还复用池，
/// 防止跨次调用触发 `SyntaxError: redeclaration of lexical binding` 或原型污染。
fn may_pollute_global_scope(code: &str) -> bool {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;

    while i < len {
        let b = bytes[i];
        // 行注释 //
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 块注释 /* ... */
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            let mut closed = false;
            while i + 1 < len {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return true;
            }
            continue;
        }
        // 字符串 / 模板字面量
        if b == b'\'' || b == b'"' || b == b'`' {
            let quote = b;
            i += 1;
            let mut closed = false;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return true;
            }
            continue;
        }
        match b {
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                let start = i;
                i += 1;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
                {
                    i += 1;
                }
                let ident = &code[start..i];
                if matches!(
                    ident,
                    "prototype" | "__proto__" | "eval" | "globalThis" | "Function"
                ) {
                    return true;
                }
                if brace_depth == 0
                    && paren_depth == 0
                    && matches!(
                        ident,
                        "let" | "const" | "class" | "var" | "function" | "import" | "export"
                    )
                {
                    return true;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    false
}

/// 借用同线程无桥接轻量沙箱执行求值；递归重入、TLS 销毁期、或 `vars` 覆盖受保护基线键时
/// 自动安全回退到独立 `new_limited_context()`。
fn with_pooled_sandbox<T>(
    code: &str,
    vars: &HashMap<String, String>,
    fallback: impl FnOnce() -> Result<T>,
    f: impl FnOnce(&mut Context, &JsBridge) -> Result<T>,
) -> Result<T> {
    let reentrant = SANDBOX_IN_USE.try_with(|flag| flag.get()).unwrap_or(true);
    if reentrant {
        return fallback();
    }
    if SANDBOX_IN_USE.try_with(|flag| flag.set(true)).is_err() {
        return fallback();
    }
    let _lease = PoolLeaseGuard;

    let pool_ready = SANDBOX_POOL_READY.try_with(|r| r.get()).unwrap_or(false);
    let mut sandbox = if pool_ready {
        let taken = SANDBOX_POOL
            .try_with(|slot| slot.try_borrow_mut().ok().and_then(|mut opt| opt.take()))
            .ok()
            .flatten();
        match taken {
            Some(sb) => sb,
            None => PooledSandbox::new()?,
        }
    } else {
        let sb = PooledSandbox::new()?;
        let _ = SANDBOX_POOL_READY.try_with(|r| r.set(true));
        sb
    };

    // vars 若覆盖 java/source/Map 等受保护基线键，归还干净沙箱并走独立上下文以保持 100% 语义一致
    if vars.keys().any(|k| sandbox.is_protected_key(k)) {
        let _ = SANDBOX_POOL.try_with(|slot| {
            if let Ok(mut opt) = slot.try_borrow_mut() {
                *opt = Some(sandbox);
            }
        });
        return fallback();
    }

    sandbox.reset_bridge_state();
    sandbox
        .context
        .runtime_limits_mut()
        .set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);

    let dirty_script = may_pollute_global_scope(code);
    let outcome = f(&mut sandbox.context, &sandbox.bridge);

    if outcome.is_ok() && !dirty_script && sandbox.cleanup() {
        let _ = SANDBOX_POOL.try_with(|slot| {
            if let Ok(mut opt) = slot.try_borrow_mut() {
                *opt = Some(sandbox);
            }
        });
    }
    outcome
}

/// 为 `eval_js` / `eval_js_with_bridge_limited` 补齐 legacy 常见默认变量（避免 ReferenceError）
fn with_default_eval_vars(vars: &HashMap<String, String>) -> HashMap<String, String> {
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
}

// ---- 对外求值入口 ----

/// 执行 JS 代码，返回字符串结果（无桥接高频热路径：复用同线程轻量 Boa 沙箱）
///
/// 变量以全局属性注入；结果按 String() 语义转字符串
/// （null/undefined → 空串；数字/布尔 → 字面量，如 "42" / "true"）
pub fn eval_js(code: &str, vars: &HashMap<String, String>) -> Result<String> {
    with_pooled_sandbox(
        code,
        vars,
        || eval_js_with_bridge_limited(code, vars, &JsBridge::default(), JS_LOOP_ITERATION_LIMIT),
        |context, bridge| {
            let mut vars = with_default_eval_vars(vars);
            let ctx_bindings = take_context_bindings(&mut vars);
            inject_vars(context, &vars)?;
            install_context_bindings(context, &ctx_bindings)?;
            auto_set_content(&vars, bridge);
            let result = context
                .eval(Source::from_bytes(code.as_bytes()))
                .map_err(map_js_error)?;
            Ok(js_result_to_string(&result, context))
        },
    )
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
    run_in_blocking_boundary(|| {
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
    })
}

/// 指定循环迭代上限的桥接执行（核心；测试用小上限验证超限路径）
pub(super) fn eval_js_with_bridge_limited(
    code: &str,
    vars: &HashMap<String, String>,
    bridge: &JsBridge,
    loop_limit: u64,
) -> Result<String> {
    run_in_blocking_boundary(|| {
        let mut vars = with_default_eval_vars(vars);
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
    })
}

/// 执行 JS 并返回结构化结果（serde_json::Value；无桥接高频热路径复用同线程轻量沙箱）
///
/// 数组/对象经递归转换（`js_value_to_json`）直接得到 JSON——避免 boa ToString 对
/// 数组元素对象输出 "[object Object]" 导致后续 JSON.parse 为空；若结果为字符串且
/// 可解析为 JSON（如 JS 内 `JSON.stringify(...)` 出口），自动解析为对应结构。
pub fn eval_js_json(code: &str, vars: &HashMap<String, String>) -> Result<JsonValue> {
    with_pooled_sandbox(
        code,
        vars,
        || eval_js_json_with_bridge(code, vars, &JsBridge::default()),
        |context, bridge| {
            let mut vars = vars.clone();
            let ctx_bindings = take_context_bindings(&mut vars);
            inject_vars(context, &vars)?;
            install_context_bindings(context, &ctx_bindings)?;
            auto_set_content(&vars, bridge);
            let result = context
                .eval(Source::from_bytes(code.as_bytes()))
                .map_err(map_js_error)?;
            let json = js_value_to_json(&result, context)
                .map_err(|e| anyhow!("JS 结果 JSON 转换失败: {e}"))?;
            if let JsonValue::String(s) = &json {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
                    return Ok(parsed);
                }
            }
            Ok(json)
        },
    )
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
    run_in_blocking_boundary(|| {
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
    })
}

/// 指定循环迭代上限的 JSON 桥接执行（测试用小上限验证超限路径）
#[cfg(test)]
pub(super) fn eval_js_json_with_bridge_limited(
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

/// 执行 JS 表达式并返回 JsValue（无桥接热路径复用同线程轻量沙箱）
pub fn eval_js_value(code: &str, vars: &HashMap<String, String>) -> Result<JsValue> {
    with_pooled_sandbox(
        code,
        vars,
        || {
            let mut context = context_with_limit(JS_LOOP_ITERATION_LIMIT);
            let bridge = JsBridge::default();
            install_globals(&mut context, &bridge)?;
            install_bridge(&mut context, &bridge)?;
            inject_vars(&mut context, vars)?;
            context
                .eval(Source::from_bytes(code.as_bytes()))
                .map_err(map_js_error)
        },
        |context, _bridge| {
            inject_vars(context, vars)?;
            context
                .eval(Source::from_bytes(code.as_bytes()))
                .map_err(map_js_error)
        },
    )
}

// ---- 变量/上下文绑定与公共辅助 ----

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
pub(super) fn is_js_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '_' && first != '$' && !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
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

/// 注册全局函数
pub(super) fn register_global_fn(
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
pub(super) fn bind<F>(bridge: &JsBridge, f: F) -> NativeFunction
where
    F: Fn(&JsBridgeInner, &[JsValue], &mut Context) -> JsResult<JsValue> + 'static,
{
    let inner = Arc::clone(&bridge.inner);
    unsafe { NativeFunction::from_closure(move |_this, args, ctx| f(&inner, args, ctx)) }
}

/// 构造 JS 异常（NativeFunction 内 throw，书源规则可 catch）
pub(super) fn js_native_error(msg: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(msg.into()).into()
}

pub(super) fn bridge_ref(inner: &JsBridgeInner) -> JsBridge {
    JsBridge {
        inner: Arc::new(inner.clone()),
    }
}

// ---- JsValue 转换辅助 ----

pub(super) fn json_to_js_value(value: JsonValue, _context: &mut Context) -> JsValue {
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

/// JsValue → i64（数字直接取；字符串按数字解析；失败 → 0）
pub(super) fn js_value_to_i64(v: &JsValue, context: &mut Context) -> i64 {
    match v {
        JsValue::Integer(i) => i64::from(*i),
        JsValue::Rational(r) => *r as i64,
        JsValue::BigInt(b) => b.to_f64() as i64,
        _ => js_value_to_string(v, context).parse::<i64>().unwrap_or(0),
    }
}

pub(super) fn js_value_to_f64(v: &JsValue, context: &mut Context) -> f64 {
    match v {
        JsValue::Integer(i) => f64::from(*i),
        JsValue::Rational(r) => *r,
        JsValue::BigInt(b) => b.to_f64(),
        _ => js_value_to_string(v, context).parse::<f64>().unwrap_or(0.0),
    }
}

pub(super) fn js_value_to_u32(v: &JsValue, context: &mut Context) -> u32 {
    match v {
        JsValue::Integer(i) => (*i).max(0) as u32,
        JsValue::Rational(r) => (*r).max(0.0) as u32,
        JsValue::BigInt(b) => b.to_f64().max(0.0) as u32,
        _ => js_value_to_string(v, context).parse::<u32>().unwrap_or(0),
    }
}

pub(super) fn js_value_to_bool(v: &JsValue, context: &mut Context) -> bool {
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

/// JsValue → 字节（字符串 UTF-8 / 数字数组 / Uint8Array 兼容）
pub(super) fn js_value_to_bytes(v: &JsValue, context: &mut Context) -> Vec<u8> {
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

pub(super) fn js_bytes_to_js_string(bytes: Vec<u8>) -> JsValue {
    JsValue::from(JsString::from(String::from_utf8_lossy(&bytes).into_owned()))
}

/// 字节 → number[]（legacy ByteArray 语义，每元素 0-255）
pub(super) fn js_bytes_to_js_array(bytes: Vec<u8>, context: &mut Context) -> JsValue {
    JsArray::from_iter(bytes.iter().map(|b| JsValue::from(*b as i32)), context).into()
}

pub(super) fn js_byte_array(bytes: Vec<u8>, context: &mut Context) -> JsValue {
    JsArray::from_iter(
        bytes.into_iter().map(|b| JsValue::from(u32::from(b))),
        context,
    )
    .into()
}

/// JsValue → 字符串（对齐 String() 语义：数字/布尔 → 字面量；
/// null/undefined → 空串，对齐 legado 空结果语义；对象 → toString()）
pub(super) fn js_value_to_string(v: &JsValue, context: &mut Context) -> String {
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
