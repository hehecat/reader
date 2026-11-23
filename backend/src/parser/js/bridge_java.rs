//! 全局环境装配与 `java.*` / `source.*` 核心桥接：
//! - `install_globals`（JS prelude、jsLib、书源变量、默认全局、cookie/cache/org.jsoup/application/Java utils）
//! - `install_bridge` / `build_bridge_objects`（`java.*` / `source.*` / `headerMap` / 顶层函数别名）
//! - `java.put/get/log/logType/toast/timeFormat/randomUUID/androidId/t2s/s2t/getWebViewUA`
//! - `source.getKey/getName/put/get/getVariable/*LoginHeader`
//! - 文件/ZIP 缓存（`cacheFile`/`downloadFile`/`readFile`/`unzipFile` 等）与 TTF 字体反爬解析

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{anyhow, Result};
use base64::Engine as _;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsArgs, JsObject, JsResult, JsString, JsValue, NativeFunction, Source};

use super::bridge_android::{
    attach_java_namespaces, install_application_shim, install_cache_object, install_java_utils_shim,
};
use super::bridge_crypto::{
    gzip_base64, java_aes_base64_decode_to_byte_array, java_aes_base64_decode_to_string,
    java_aes_decode_args_base64_str, java_aes_decode_to_byte_array, java_aes_decode_to_string,
    java_aes_encode_args_base64_str, java_aes_encode_to_base64_byte_array,
    java_aes_encode_to_base64_string, java_aes_encode_to_byte_array, java_aes_encode_to_string,
    java_base64_decode_flags, java_base64_decode_to_byte_array, java_base64_encode_flags,
    java_create_symmetric_crypto, java_des_base64_decode_to_string, java_des_decode_to_string,
    java_des_encode_to_base64_string, java_des_encode_to_string, java_digest_base64_str,
    java_digest_hex, java_escape, java_hex_decode, java_hmac_hex, java_md5_encode,
    java_md5_encode16, java_triple_des_decode_args_base64_str, java_triple_des_decode_str,
    java_triple_des_encode_args_base64_str, java_triple_des_encode_base64_str, java_unescape,
    java_utf8_to_gbk, xgorgon_stub,
};
use super::bridge_dom::{
    java_get_elements, java_get_string, java_html_format, java_set_content, jsoup_parse,
};
use super::bridge_net::{
    get_wbi_enc, install_cookie_object, java_ajax, java_ajax_all, java_connect, java_encode_uri,
    java_get_cookie, java_head, java_http_fetch, java_post, java_start_browser_await, reload_fetch,
};
use super::runtime::{
    bind, block_on_task, bridge_ref, eval_js_with_bridge, is_js_identifier, js_value_to_i64,
    js_value_to_string, js_value_to_u32, map_js_error, register_global_fn, JsBridge, JsBridgeInner,
    BRIDGE_WAIT_TIMEOUT,
};

/// `java.getWebViewUA()` 返回的固定浏览器 UA（与爬虫默认 UA 同系 Chrome 120 内核；
/// 内置浏览器求解（solve_cf_challenge / solve_captcha）返回真实 UA 时
/// 以浏览器为准，此常量作为 JS 侧固定参考值）
pub const JS_WEBVIEW_UA: &str =
    "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36";

/// P1-3：source.put 存储上限（防书源脚本无界写入）——单书源最多 1000 条 / 总 1MB（UTF-8 字节），
/// 超限拒绝写入（no-op + warn，不抛错——legado 语义 source.put 无返回值，静默丢弃超限值）
pub const SOURCE_VARS_MAX_ENTRIES: usize = 1000;
pub const SOURCE_VARS_MAX_BYTES: usize = 1024 * 1024;

/// source.put/get 书源级变量存储：全局共享（跨搜索/详情调用），
/// 外层 key 为书源 key（URL），内层为该书源的变量表（书源间隔离）
static SOURCE_VARS: LazyLock<Mutex<HashMap<String, HashMap<String, String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// source.put 核心（纯函数可测）：写入成功返回 true；超限（条数/字节）拒绝返回 false
pub(super) fn source_put_limited(
    vars: &mut HashMap<String, String>,
    key: &str,
    value: &str,
) -> bool {
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

/// 全局 shim（每个 eval 上下文安装一次，先于 vars 注入——vars 同名覆盖）：
/// - JS prelude：`Map()` 无 new 调用兼容（legado Rhino 允许；boa 0.19 报错）、
///   `cache` 内存对象、`unescape/escape`、`URL/URLSearchParams`
/// - 默认全局变量：baseUrl/base_url/src/type/urlIP（缺省值，避免 ReferenceError）
/// - 全局对象/函数：cookie（removeCookie/getCookie/setCookie）、org.jsoup（Jsoup.parse）、
///   xGorgon（stub）、getWbiEnc（bilibili wbi 签名）、Reload（拉取远程 JS 文本）
pub(super) fn install_globals(context: &mut Context, bridge: &JsBridge) -> Result<()> {
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

    // cookie 对象（legado CookieStore shim）
    install_cookie_object(context, bridge)?;

    // cache 对象（E11：legacy CacheManager shim）
    install_cache_object(context, bridge)?;

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

/// 注册 java / source 全局对象及顶层纯函数别名
pub(super) fn install_bridge(context: &mut Context, bridge: &JsBridge) -> Result<()> {
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

/// 构建 java / source 对象（ObjectInitializer：function 注册方法、property 挂子对象）
fn build_bridge_objects(bridge: &JsBridge, context: &mut Context) -> Result<(JsObject, JsObject)> {
    // java.headerMap：请求头 Map（底层为 bridge.headers，eval 后可读取）
    let mut header_map = ObjectInitializer::new(context);
    header_map
        .function(bind(bridge, header_map_put), JsString::from("put"), 2)
        .function(bind(bridge, header_map_get), JsString::from("get"), 1)
        .function(bind(bridge, header_map_size), JsString::from("size"), 0);
    let header_map = header_map.build();

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
        // E16：legacy 摘要 base64 版 + logType
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

// ---- java.* 核心实现 ----

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
pub(super) fn format_time_millis(millis: i64, pattern: &str, offset_ms: i64) -> String {
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
            'y' => fmt.push_str(if n == 2 { "%y" } else { "%Y" }),
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

/// java.log(msg)：tracing 日志（调试书源规则）
fn java_log(_inner: &JsBridgeInner, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let msg = js_value_to_string(args.get_or_undefined(0), context);
    tracing::info!(target: "reader.js", "[java.log] {}", msg);
    Ok(JsValue::undefined())
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

/// `java.getWebViewUA()`：固定浏览器 UA（见 `JS_WEBVIEW_UA`）
fn java_get_webview_ua(
    _inner: &JsBridgeInner,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(JS_WEBVIEW_UA)))
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

// ---- 文件 / ZIP 缓存 / importScript / webView ----

/// JS 缓存目录（legacy CacheManager 对应；文件 shim 统一落这里）
pub(super) fn js_cache_dir() -> std::path::PathBuf {
    static DIR: LazyLock<std::path::PathBuf> =
        LazyLock::new(|| crate::AppConfig::from_env().storage_dir().join("js-cache"));
    DIR.clone()
}

pub(super) fn sanitize_ns(ns: &str) -> String {
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

/// java.cacheFile(url, saveTime?)：下载 URL（带书源 cookie/header，经 crawler::fetch），
/// **返回响应内容文本**；saveTime>0 时缓存有效期内直接复用本地副本
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

/// java.getFile(path)：返回 Java File 对象 stub（带 `.path` 与 `.exists()`）
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
        let url = path;
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
        let url = s;
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
