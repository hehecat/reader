//! Android / 旧版阅读环境兼容层：
//! - `cache` 持久化缓存（内存热缓存 + SQLite 读写穿透）
//! - `application` / `SharedPreferences` / `Editor`
//! - `URLEncoder` / `URLDecoder` / `UUID` / `System` / `android.util.Base64` / `Log` / `java.net/util/lang`

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{anyhow, Result};
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsArgs, JsObject, JsResult, JsString, JsValue, NativeFunction, Source};
use serde_json::Value as JsonValue;

use super::runtime::{
    bind, block_on_task, js_value_to_bool, js_value_to_f64, js_value_to_i64, js_value_to_json,
    js_value_to_string, json_to_js_value, map_js_error, JsBridge, JsBridgeInner,
    BRIDGE_WAIT_TIMEOUT,
};

/// `application.getSharedPreferences(name, mode)` 的 Android SharedPreferences 兼容存储。
/// 外层 key = 用户命名空间 + pref 名，内层为 key -> 类型保真的 JSON 值
/// （旧版阅读/legado 书源 Header 脚本常用它保存 token/开关，跨搜索/详情/目录 eval 可见）。
static APP_PREFS: LazyLock<Mutex<HashMap<String, HashMap<String, JsonValue>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// E11 `cache` 对象存储（legacy CacheManager shim）：按用户命名空间隔离、
/// 进程级跨请求持久；saveTime 秒过期（0=永不过期）；value 统一字符串，
/// typed getter 解析失败回退默认值。
pub(super) static CACHE_STORE: LazyLock<Mutex<HashMap<String, (String, i64)>>> =
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
    crate::storage::current_task_storage().or_else(|| {
        JS_CACHE_STORAGE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    })
}

/// EG4 写穿透落库：内存热缓存已同步更新，同进程立即可读；
/// 处于多线程 Tokio runtime 时通过 `handle.spawn` 异步后台落库，零阻塞解析线程；
/// CurrentThread（单测 runtime）或非 Tokio 线程保持 `block_on_task` 同步落库以保证单测确定性。
fn spawn_or_block_cache_write<T: Send + 'static>(
    fut: impl std::future::Future<Output = Result<T>> + Send + 'static,
    what: &'static str,
    err_msg: &'static str,
) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            handle.spawn(async move {
                if let Err(e) = fut.await {
                    tracing::warn!("{err_msg}: {e}");
                }
            });
        }
        _ => {
            if let Err(e) = block_on_task(fut, BRIDGE_WAIT_TIMEOUT, what) {
                tracing::warn!("{err_msg}: {e}");
            }
        }
    }
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

pub(super) fn cache_store_key(ns: &str, key: &str) -> String {
    format!("{}\u{1}{key}", if ns.is_empty() { "default" } else { ns })
}

fn cache_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn cache_get_value(ns: &str, key: &str) -> Option<String> {
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

pub(super) fn cache_put_value(ns: &str, key: &str, value: String, save_time_secs: i64) {
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
    // EG4 写穿透落库（INSERT OR REPLACE）：内存已更新；多线程 runtime 异步落库，失败仅告警不抛错
    if let Some(storage) = js_cache_registered() {
        let db_ns = cache_db_ns(ns).to_string();
        let db_key = key.to_string();
        let fut = async move { storage.put_js_cache(&db_ns, &db_key, &value, exp).await };
        spawn_or_block_cache_write(fut, "cache.put", "cache.put 落库失败（本次仅内存）");
    }
}

/// 注册 `cache` 全局对象（E11：legacy CacheManager shim——书源级跨请求持久缓存，按用户隔离）
pub(super) fn install_cache_object(context: &mut Context, bridge: &JsBridge) -> Result<()> {
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
        .map_err(|e| anyhow!("cache 对象注册失败: {e}"))
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
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let value = js_value_to_string(args.get_or_undefined(1), context);
    let save_time = args
        .get(2)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    cache_put_value(&inner.ns, &key, value, save_time);
    Ok(JsValue::undefined())
}

fn cache_num_get(
    inner: &JsBridgeInner,
    args: &[JsValue],
    default_val: f64,
    context: &mut Context,
) -> f64 {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    let def = args
        .get(1)
        .and_then(|v| v.as_number())
        .unwrap_or(default_val);
    match cache_get_value(&inner.ns, &key) {
        Some(v) => v.trim().parse::<f64>().unwrap_or(def),
        None => def,
    }
}

fn cache_js_get_int(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0, context) as i32;
    Ok(JsValue::Integer(n))
}

fn cache_js_put_int(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
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
    context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0, context);
    Ok(JsValue::from(n))
}

fn cache_js_put_long(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
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
    context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0, context);
    Ok(JsValue::from(n))
}

fn cache_js_put_double(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
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
    context: &mut Context,
) -> JsResult<JsValue> {
    let n = cache_num_get(inner, args, 0.0, context);
    Ok(JsValue::from(n))
}

fn cache_js_put_float(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
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
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = js_value_to_string(args.get_or_undefined(0), context);
    CACHE_STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&cache_store_key(&inner.ns, &key));
    // EG4：写穿透删库（多线程 runtime 异步落库，失败仅告警）
    if let Some(storage) = js_cache_registered() {
        let db_ns = cache_db_ns(&inner.ns).to_string();
        let fut = async move { storage.delete_js_cache(&db_ns, &key).await };
        spawn_or_block_cache_write(fut, "cache.delete", "cache.delete 删库失败（仅内存）");
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
    // EG4：写穿透清库（仅本命名空间；多线程 runtime 异步落库，失败仅告警）
    if let Some(storage) = js_cache_registered() {
        let db_ns = cache_db_ns(&inner.ns).to_string();
        let fut = async move { storage.clear_js_cache(&db_ns).await };
        spawn_or_block_cache_write(fut, "cache.clear", "cache.clear 清库失败（仅内存）");
    }
    Ok(JsValue::undefined())
}

// ---- application / SharedPreferences 兼容层 ----

/// `application` 兼容层：旧版阅读（Rhino）注入的 Android Application 实例。
/// 书源脚本常见用法：`application.getSharedPreferences("x", 0).getString("k", "")`、
/// `...edit().putString(...).apply()`、`application.getPackageName()`、
/// `application.getFilesDir().getAbsolutePath()`。Reader Dev 无 Android 环境，
/// 提供内存 SharedPreferences（按用户隔离、跨 eval 共享）与虚拟文件路径，
/// 避免 `ReferenceError: application is not defined` 导致目录/正文规则返回空。
pub(super) fn install_application_shim(
    context: &mut Context,
    bridge: &JsBridge,
) -> Result<JsValue> {
    let package_name = JsString::from("io.legado.app");
    let application = ObjectInitializer::new(context)
        .property(
            JsString::from("packageName"),
            JsValue::from(package_name),
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
pub(super) fn install_java_utils_shim(context: &mut Context, application: JsValue) -> Result<()> {
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
            JsValue::from(uuid),
            Attribute::all(),
        )
        .map_err(|e| anyhow!("UUID 全局注册失败: {e}"))?;

    // 全局 System（java.lang.System 由桥接命名空间提供）
    let system = java_lang_system_obj(context);
    context
        .register_global_property(
            JsString::from("System"),
            JsValue::from(system),
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
pub(super) fn attach_java_namespaces(java: &JsObject, context: &mut Context) -> Result<()> {
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

type PrefHandler =
    fn(&JsBridgeInner, &str, &JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// SharedPreferences/Editor 方法绑定：捕获 pref 名（String 无 GC 追踪类型，from_closure 安全）。
fn bind_pref(inner: Arc<JsBridgeInner>, pref: String, f: PrefHandler) -> NativeFunction {
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
    let sp_inner = Arc::new(inner.clone());
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
    let editor_inner = Arc::new(inner.clone());
    for (name, f) in [
        ("putString", sp_put_string as PrefHandler),
        ("putInt", sp_put_number as PrefHandler),
        ("putLong", sp_put_number as PrefHandler),
        ("putFloat", sp_put_number as PrefHandler),
        ("putBoolean", sp_put_boolean as PrefHandler),
        ("remove", sp_remove as PrefHandler),
        ("clear", sp_clear as PrefHandler),
        ("commit", sp_commit as PrefHandler),
        ("apply", sp_apply as PrefHandler),
    ] {
        editor.function(
            bind_pref(Arc::clone(&editor_inner), pref.to_string(), f),
            JsString::from(name),
            if matches!(name, "commit" | "apply" | "clear") {
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
        .get_mut(&app_prefs_key(&inner.ns, pref))
    {
        m.clear();
    }
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
