//! `parser::js` 单元测试（原 `js.rs` 内联测试完整迁移 + 沙箱复用池回归测试）

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use base64::Engine as _;
use boa_engine::{JsString, JsValue};

use crate::model::BookSource;

use super::bridge_android::{cache_get_value, cache_put_value, cache_store_key, CACHE_STORE};
use super::bridge_crypto::{aes_crypt, gzip_base64};
use super::bridge_java::{format_time_millis, js_cache_dir, sanitize_ns, source_put_limited};
use super::bridge_net::{cookie_map, force_js_browser_available, register_js_solve_hook};
use super::runtime::{
    context_with_limit, eval_js_json_with_bridge_limited, eval_js_with_bridge_limited,
    BRIDGE_WAIT_TIMEOUT,
};
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
    let json = eval_js_json_with_bridge("cache.set('k', 42); cache.get('k')", &v, &bridge).unwrap();
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
    let json = eval_js_json_with_bridge("application.getFilesDir().getAbsolutePath()", &v, &bridge)
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
        let err = eval_js_with_bridge_limited(code, &v, &JsBridge::default(), 1_000).unwrap_err();
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
    let err = eval_js_json_with_bridge_limited("while(true){}", &v, &JsBridge::default(), 1_000)
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
        eval_js_with_bridge("java.encodeURI(\"a b-c_.!~*'()/?:@&=+$,#\")", &v, &bridge).unwrap(),
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
    let html = r#"<div class="book"><h2>书名A</h2></div><div class="book"><h2>书名B</h2></div>"#;
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
        eval_js_with_bridge("java.base64DecodeToString('YWJj', 0)", &vars(&[]), &bridge).unwrap(),
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
    let err = eval_js_with_bridge("java.ajaxAll('not-an-array')", &vars(&[]), &bridge).unwrap_err();
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
    let mut ctx = new_limited_context();
    let out = gzip_base64(
        &JsValue::undefined(),
        &[JsValue::from(JsString::from("hello world"))],
        &mut ctx,
    )
    .unwrap();
    let b64 = out.as_string().unwrap().to_std_string_escaped();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64).unwrap();
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
    let out = eval_js_with_bridge("AES_KEY + ':' + sign('k')", &HashMap::new(), &bridge).unwrap();
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

/// 第二轮优化回归：无桥接轻量沙箱复用池跨调用严格隔离（临时 vars 清理、隐式全局清理、
/// 顶层 let/const 不触发重复声明错误、异常后自动重建、eval_js_value 正常求值）
#[test]
fn pooled_sandbox_isolates_vars_globals_and_lexical_decls() {
    // 1. 临时 vars 每次借用后清理，不泄漏到下一次调用
    assert_eq!(
        eval_js("customVar + '!'", &vars(&[("customVar", "ok")])).unwrap(),
        "ok!"
    );
    assert_eq!(
        eval_js("typeof customVar", &vars(&[])).unwrap(),
        "undefined"
    );

    // 2. 隐式全局变量（无 var/let/const）在归还前被清理
    assert_eq!(
        eval_js("leakedGlobal = 42; leakedGlobal", &vars(&[])).unwrap(),
        "42"
    );
    assert_eq!(
        eval_js("typeof leakedGlobal", &vars(&[])).unwrap(),
        "undefined"
    );

    // 3. 顶层 let/const 连续多次执行不触发 SyntaxError: redeclaration
    assert_eq!(
        eval_js("let poolTmp = 1; poolTmp + 1", &vars(&[])).unwrap(),
        "2"
    );
    assert_eq!(
        eval_js("let poolTmp = 10; poolTmp + 1", &vars(&[])).unwrap(),
        "11"
    );
    assert_eq!(
        eval_js("const poolConst = 'a'; poolConst", &vars(&[])).unwrap(),
        "a"
    );
    assert_eq!(
        eval_js("const poolConst = 'b'; poolConst", &vars(&[])).unwrap(),
        "b"
    );

    // 4. 异常求值后沙箱自动丢弃并安全恢复
    assert!(eval_js("throw new Error('pool-err')", &vars(&[])).is_err());
    assert_eq!(eval_js("1 + 2", &vars(&[])).unwrap(), "3");

    // 5. eval_js_json 与 eval_js_value 共用复用池且互相隔离
    let j = eval_js_json("({v: itemKey})", &vars(&[("itemKey", "abc")])).unwrap();
    assert_eq!(j["v"], "abc");
    assert_eq!(eval_js("typeof itemKey", &vars(&[])).unwrap(), "undefined");

    let val = eval_js_value("1 + 41", &vars(&[])).unwrap();
    assert_eq!(val.as_number(), Some(42.0));
}
