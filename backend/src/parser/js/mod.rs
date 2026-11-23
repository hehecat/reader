#![allow(deprecated)]
//! JS 规则执行（boa_engine 0.19，对齐 legado AnalyzeByJS / js 规则）
//!
//! 模块划分：
//! - `runtime`：受限 Context、同线程轻量无桥接沙箱复用池、`JsBridge` 与值转换、`block_on_task`
//! - `bridge_java`：全局环境与 `java.*` / `source.*` 对象装配、文件/ZIP 缓存与 TTF 字体解析
//! - `bridge_android`：`cache` 持久化缓存、`application` / `SharedPreferences` 与 Java/Android 常用类 shim
//! - `bridge_crypto`：摘要 / HMAC / Base64 / Hex / GZip / 对称加解密（AES/DES/3DES）
//! - `bridge_dom`：`java.setContent`/`getString`/`getElements`/`htmlFormat` 与 `org.jsoup` shim
//! - `bridge_net`：`cookie`、`java.ajax`/`post`/`ajaxAll`/`connect`/`head`/`encodeURI`、`startBrowserAwait`、`Reload`/`getWbiEnc`
//! - `tests`：单元测试（`#[cfg(test)]`）

mod bridge_android;
mod bridge_crypto;
mod bridge_dom;
mod bridge_java;
mod bridge_net;
mod runtime;

#[cfg(test)]
mod tests;

pub use bridge_android::{
    clear_js_cache_storage, load_js_cache_from_db, register_js_cache_storage,
};
pub use bridge_java::{JS_WEBVIEW_UA, SOURCE_VARS_MAX_BYTES, SOURCE_VARS_MAX_ENTRIES};
pub(crate) use runtime::block_on_task;
pub use runtime::{
    eval_js, eval_js_json, eval_js_json_with_bridge, eval_js_value, eval_js_with_bridge,
    eval_js_with_bridge_num, js_value_to_json, new_limited_context, take_last_js_error, JsBridge,
    JS_LOOP_ITERATION_LIMIT,
};
