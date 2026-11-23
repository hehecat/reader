//! `crawler` 模块单元测试

use super::client::{
    content_type_charset, has_url_encoded, html_meta_charset, retryable_http_error,
    shared_direct_client_cached,
};
use super::cookies::{extract_set_cookie_pairs, is_throttle_cookie};
use super::*;
use std::sync::atomic::Ordering;

/// 全局复用客户端应返回同一静态单例指针（连接池/TLS 配置全进程共享）
#[test]
fn shared_http_clients_are_singletons() {
    assert!(std::ptr::eq(shared_http_client(), shared_http_client()));
    assert!(std::ptr::eq(
        shared_no_redirect_client(),
        shared_no_redirect_client()
    ));
    assert!(!std::ptr::eq(
        shared_http_client(),
        shared_no_redirect_client()
    ));
}

/// 限流印章（Max-Age 很短的 Set-Cookie）不许回存成登录态：cookie 表没有过期列，
/// 存进去就永久携带，源会被自己的节流 cookie 锁死（bqg.info 的 ss_search_delay 事故）。
#[test]
fn throttle_cookies_are_not_persisted() {
    let hdrs = vec![
        (
            "set-cookie".to_string(),
            "ss_search_delay=1; Max-Age=30; Path=/".to_string(),
        ),
        (
            "set-cookie".to_string(),
            "reader_session=abc123; Max-Age=2592000; Path=/".to_string(),
        ),
        (
            "set-cookie".to_string(),
            "plain_cookie=v; Path=/".to_string(),
        ),
    ];
    let pairs = extract_set_cookie_pairs(&hdrs);
    assert_eq!(
        pairs,
        vec![
            ("reader_session".to_string(), "abc123".to_string()),
            ("plain_cookie".to_string(), "v".to_string()),
        ]
    );
    assert!(is_throttle_cookie("a=1; max-age=0; path=/"));
    assert!(!is_throttle_cookie("a=1; Max-Age=3600"));
    assert!(!is_throttle_cookie("a=1; Path=/; HttpOnly"));
}

/// 三态记账的「超时」判定：超时归 Timeout（不写连败），确定性失败归 Fail。
/// 判定宽了会把死源当慢源放过（永远不降级），窄了又会把慢源当死源（本轮要修的病）。
#[test]
fn timeout_detection_separates_slow_from_dead() {
    // 超时类：文本与类型两条路都要认
    assert!(is_timeout_error(&anyhow::anyhow!("operation timed out")));
    assert!(is_timeout_error(&anyhow::anyhow!(
        "error sending request: timed out"
    )));
    assert!(is_timeout_error(&anyhow::anyhow!("单源搜索超时（15s）")));
    assert!(is_timeout_error(&anyhow::anyhow!("deadline has elapsed")));
    // 确定性失败：不是超时（应记 Fail，让它照常累计连败）
    for msg in [
        "connection refused",
        "dns error: failed to lookup address",
        "invalid peer certificate: UnknownIssuer",
        "http 404",
    ] {
        assert!(
            !is_timeout_error(&anyhow::anyhow!("{msg}")),
            "{msg} 不该被判成超时"
        );
    }
    // cause 链里的超时也要认（顶层常是 “error decoding response body”）
    let chained = anyhow::anyhow!("outer").context(anyhow::anyhow!("inner: operation timed out"));
    assert!(is_timeout_error(&chained));
}

fn base_key_inputs() -> ClientCacheKeyInputs {
    ClientCacheKeyInputs {
        timeout_secs: 15,
        redirect_tag: "source",
        env_proxy: String::new(),
        insecure_certs: false,
        ca_file: String::new(),
        pin_gen: 7,
        pin_epoch: 3,
    }
}

/// client 缓存 key 必须对每个「会改变 Client 身份/行为」的配置项都敏感：
/// 漏掉任何一项都会让两种不同配置串用同一个 Client（超时/证书/proxy 静默失效）
#[test]
fn client_cache_key_distinguishes_every_input() {
    let base = base_key_inputs();
    let k0 = client_cache_key(&base);
    assert_eq!(k0, client_cache_key(&base), "同输入必须同 key");

    let mut variants: Vec<ClientCacheKeyInputs> = Vec::new();
    let mut v = base.clone();
    v.timeout_secs = 30;
    variants.push(v);
    let mut v = base.clone();
    v.redirect_tag = "none";
    variants.push(v);
    let mut v = base.clone();
    v.env_proxy = "http://127.0.0.1:7890".into();
    variants.push(v);
    let mut v = base.clone();
    v.insecure_certs = true;
    variants.push(v);
    let mut v = base.clone();
    v.ca_file = "/etc/ca.pem".into();
    variants.push(v);
    let mut v = base.clone();
    v.pin_gen = 8;
    variants.push(v);
    let mut v = base.clone();
    v.pin_epoch = 4;
    variants.push(v);

    let mut seen = std::collections::HashSet::new();
    seen.insert(k0.clone());
    for (i, variant) in variants.iter().enumerate() {
        let k = client_cache_key(variant);
        assert_ne!(k, k0, "第 {i} 项变化没有改变 key");
        assert!(seen.insert(k), "第 {i} 项与之前某项 key 撞了");
    }
}

/// 容量上限：到顶后插入新 key 会整体清空再重建（有上限，key 空间不会无限涨）
#[test]
fn insert_capped_enforces_limit() {
    let mut m: HashMap<String, u8> = HashMap::new();
    for i in 0..3 {
        assert!(!insert_capped(&mut m, format!("k{i}"), i, 3));
    }
    assert_eq!(m.len(), 3);
    // 覆盖已有 key：不清空、返回 false
    assert!(!insert_capped(&mut m, "k1".to_string(), 99, 3));
    assert_eq!(m.len(), 3);
    assert_eq!(m.get("k1"), Some(&99));
    // 插入新 key 且已到顶：清空后只留新 key，返回 true（表示发生过清空）
    assert!(insert_capped(&mut m, "k9".to_string(), 9, 3));
    assert_eq!(m.len(), 1);
    assert_eq!(m.get("k9"), Some(&9));
}

/// shared_direct_client 复用同一个底层 Client（连接池/TLS 会话跨抓取共享）：
/// 同一配置第二次调用不再新增条目；不同 timeout 各占一条。
/// 用私有缓存表断言：不依赖进程级静态表的绝对长度，避免与其它用例并发抓取互相干扰。
#[test]
fn shared_direct_client_is_reused() {
    let cache: std::sync::Mutex<HashMap<String, reqwest::Client>> =
        std::sync::Mutex::new(HashMap::new());
    let key17 = client_cache_key(&ClientCacheKeyInputs::current(17));
    let key18 = client_cache_key(&ClientCacheKeyInputs::current(18));
    assert_ne!(key17, key18, "不同 timeout 必须是不同条目");
    assert_eq!(
        client_cache_key(&ClientCacheKeyInputs::current(17)),
        key17,
        "同一配置的 key 必须稳定"
    );

    let _ = shared_direct_client_cached(&cache, 17).expect("构建 client");
    let n1 = cache.lock().unwrap_or_else(|e| e.into_inner()).len();
    assert_eq!(n1, 1, "首次调用插入一条");
    assert!(cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&key17));

    let _ = shared_direct_client_cached(&cache, 17).expect("构建 client");
    assert_eq!(
        cache.lock().unwrap_or_else(|e| e.into_inner()).len(),
        n1,
        "同一配置不应重复插入"
    );

    let _ = shared_direct_client_cached(&cache, 18).expect("构建 client");
    let m = cache.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(m.len(), 2, "不同 timeout 必须是不同条目");
    assert!(m.contains_key(&key17) && m.contains_key(&key18));
}

/// E6：cookie 域键 = legacy getSubDomain（host[:port] 去最左标签）
#[test]
fn test_cookie_subdomain() {
    assert_eq!(
        cookie_subdomain("https://www.example.com/a?b=1"),
        "example.com"
    );
    assert_eq!(
        cookie_subdomain("http://api.example.com:8080/x"),
        "example.com:8080"
    );
    assert_eq!(
        cookie_subdomain("https://user@sub.a.com/path"),
        "a.com",
        "userinfo 剥离"
    );
    // 单标签（无点）原样
    assert_eq!(
        cookie_subdomain("http://localhost:3000/x"),
        "localhost:3000"
    );
    assert_eq!(cookie_subdomain("https://example.com"), "example.com");
    // IP：多段 → 去首段（legacy 同款怪癖，保持一致）
    assert_eq!(cookie_subdomain("http://192.168.1.1:80/"), "168.1.1:80");
    // 无 scheme 容忍
    assert_eq!(cookie_subdomain("www.example.com/c"), "example.com");
}

/// E4：显式 Cookie 头与存储 cookie 逐键合并（stored 为底、explicit 覆盖）
#[test]
fn test_merge_cookie_strings() {
    assert_eq!(
        merge_cookie_strings("a=1; b=2", "b=9; c=3"),
        "a=1; b=9; c=3"
    );
    assert_eq!(merge_cookie_strings("", "x=1"), "x=1");
    assert_eq!(merge_cookie_strings("x=1", ""), "x=1");
    // 值中含 '=' 的边界
    assert_eq!(merge_cookie_strings("t=aa==", "u=bb=="), "t=aa==; u=bb==");
}

/// E5：Set-Cookie 提取（仅取首个 name=value 对；忽略属性与 Expires）
#[test]
fn test_extract_set_cookie_pairs() {
    let headers = vec![
        (
            "set-cookie".to_string(),
            "sid=abc123; Path=/; HttpOnly".to_string(),
        ),
        ("Set-Cookie".to_string(), "expires stuff".to_string()),
        (
            "Set-Cookie".to_string(),
            "Expires=Wed, 21 Oct 2025 07:28:00 GMT; Path=/".to_string(),
        ),
        ("Content-Type".to_string(), "text/html".to_string()),
    ];
    let pairs = extract_set_cookie_pairs(&headers);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0], ("sid".to_string(), "abc123".to_string()));
}

/// 69shuba 搜索 POST（真实链路复现 builder error——网络不可达时跳过）
#[tokio::test]
async fn fetch_69shuba_post() {
    let mut h = HashMap::new();
    h.insert(
        "User-Agent".to_string(),
        "Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 Chrome/125.0 Mobile Safari/537.36"
            .to_string(),
    );
    h.insert(
        "Referer".to_string(),
        "https://www.69shuba.com/".to_string(),
    );
    h.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded; charset=gbk".to_string(),
    );
    let url = "https://www.69shuba.com/modules/article/search.php";
    let body = "searchkey=%E8%AF%A1%E7%A7%98%E4%B9%8B%E4%B8%BB&searchtype=all&page=1";
    match fetch(url, &h, 30, "POST", Some(body), Some("gbk"), None).await {
        Ok(r) => println!("OK status={} len={}", r.status, r.body.len()),
        Err(e) => println!("ERR: {e:?} source={:?}", e.source().map(|s| s.to_string())),
    }
}

#[test]
fn test_base_url_of() {
    assert_eq!(
        base_url_of("https://a.com/book/1?x=2").as_deref(),
        Some("https://a.com")
    );
    assert_eq!(
        base_url_of("https://a.com:8443/x").as_deref(),
        Some("https://a.com:8443")
    );
    assert_eq!(base_url_of("http://a.com").as_deref(), Some("http://a.com"));
    assert_eq!(base_url_of("not a url"), None);
}

/// HTML meta charset：GBK 页面无显式 charset 参数时自动探测解码
#[test]
fn test_decode_bytes_meta_charset_gbk() {
    let (gbk, _, _) = encoding_rs::GBK.encode("第一章 内容");
    let mut bytes = b"<html><head><meta charset=\"gbk\"></head><body>".to_vec();
    bytes.extend_from_slice(&gbk);
    bytes.extend_from_slice(b"</body></html>");
    let text = decode_bytes(&bytes, None);
    assert!(text.contains("第一章 内容"), "meta gbk 应解码中文: {text}");
}

/// http-equiv Content-Type 声明 charset
#[test]
fn test_decode_bytes_meta_http_equiv() {
    let (gbk, _, _) = encoding_rs::GBK.encode("第二卷");
    let mut bytes =
        br#"<meta http-equiv="Content-Type" content="text/html; charset=GB2312">"#.to_vec();
    bytes.extend_from_slice(&gbk);
    let text = decode_bytes(&bytes, None);
    assert!(text.contains("第二卷"), "http-equiv charset 应生效: {text}");
}

/// UTF-8 BOM 与纯 UTF-8 正常解码
#[test]
fn test_decode_bytes_utf8_bom() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("中文内容".as_bytes());
    assert_eq!(decode_bytes(&bytes, None), "中文内容");
    assert_eq!(decode_bytes("纯UTF8".as_bytes(), None), "纯UTF8");
}

/// 无 meta 的纯 GBK 中文页：UTF-8 有替换字符时启发式回退 GBK
#[test]
fn test_decode_bytes_gbk_heuristic() {
    let (gbk, _, _) = encoding_rs::GBK.encode("这是一段没有meta声明的GBK中文");
    let text = decode_bytes(&gbk, None);
    assert!(
        text.contains("这是") && text.contains("中文"),
        "GBK 启发式应解码: {text}"
    );
}

/// 统计式探测：无 meta 的 Big5 页面应解码为繁体中文（不落入 GBK 启发式）
#[test]
fn test_decode_bytes_big5_statistical() {
    let (big5, _, _) = encoding_rs::BIG5
        .encode("這是一段沒有 meta 聲明的繁體中文內容，用來驗證統計式編碼偵測可以正確辨識 Big5。");
    let text = decode_bytes(&big5, None);
    assert!(
        text.contains("繁體中文內容"),
        "Big5 应经统计探测解码: {text}"
    );
}

/// Content-Type charset 提取（fetch 的 HTTP 头优先级）
#[test]
fn test_content_type_charset_extract() {
    let headers = vec![
        (
            "content-type".to_string(),
            "text/html; charset=gbk".to_string(),
        ),
        ("x-other".to_string(), "text/plain".to_string()),
    ];
    assert_eq!(content_type_charset(&headers).as_deref(), Some("gbk"));
    let headers = vec![(
        "content-type".to_string(),
        "text/html; charset=\"UTF-8\"".to_string(),
    )];
    assert_eq!(content_type_charset(&headers).as_deref(), Some("UTF-8"));
    assert!(content_type_charset(&[]).is_none());
}

/// meta 声明提取：单/双引号与无引号形式
#[test]
fn test_html_meta_charset_forms() {
    assert_eq!(
        html_meta_charset(r#"<meta charset='gb2312'>"#).as_deref(),
        Some("gb2312")
    );
    assert_eq!(
        html_meta_charset(r#"<meta charset = utf-8 >"#).as_deref(),
        Some("utf-8")
    );
    assert_eq!(
        html_meta_charset(r#"<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=GBK">"#)
            .as_deref(),
        Some("gbk")
    );
    assert_eq!(html_meta_charset("<html><head></head></html>"), None);
}

/// fetch：Content-Type charset 覆盖 HTML meta（HTTP 头优先级更高）
#[tokio::test]
async fn test_fetch_content_type_charset_wins() {
    let _ssrf = ssrf_allow_private_guard(true);
    let (gbk, _, _) = encoding_rs::GBK.encode("正文甲");
    let mut body = b"<html><meta charset=\"utf-8\">".to_vec();
    body.extend_from_slice(&gbk);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=gbk\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut out = resp.into_bytes();
        out.extend_from_slice(&body);
        let _ = sock.write_all(&out).await;
    });
    let resp = fetch(
        &format!("http://{addr}/x"),
        &HashMap::new(),
        10,
        "GET",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        resp.body.contains("正文甲"),
        "HTTP charset 应优先于 meta: {}",
        resp.body
    );
}

/// 传输层失败自动重试：前两次连接中断、第三次返回 200（默认重试 2 次）
#[tokio::test]
async fn test_fetch_retries_transient_connection_error() {
    let _ssrf = ssrf_allow_private_guard(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for attempt in 0..3 {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            if attempt < 2 {
                // 模拟连接中断（半响应后关闭 → reqwest EOF）
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nha")
                    .await;
                drop(sock);
            } else {
                let body = "ok";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        }
    });
    let resp = fetch(
        &format!("http://{addr}/retry"),
        &HashMap::new(),
        10,
        "GET",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "ok", "重试后应返回成功响应");
}

/// 可重试错误识别：超时/连接中断/TLS 等传输层错误重试，HTTP 状态错误不重试
#[test]
fn test_retryable_http_error() {
    assert!(retryable_http_error(&anyhow!("operation timed out")));
    assert!(retryable_http_error(&anyhow!(
        "error sending request for url: connection closed before message completed"
    )));
    assert!(retryable_http_error(&anyhow!(
        "error sending request: tls handshake eof"
    )));
    assert!(!retryable_http_error(&anyhow!(
        "HTTP status client error (500 Internal Server Error) for url"
    )));
}

#[test]
fn test_cookie_for_unregistered_is_none() {
    clear_cookie_storage();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = rt.block_on(cookie_for("default", "https://a.com/x"));
    assert!(r.is_none());
}

#[test]
fn test_is_cloudflare_challenge() {
    // 503 + 特征 HTML → true
    assert!(is_cloudflare_challenge(
        503,
        "<html>Just a moment...<script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/jsch/v1\"></script>"
    ));
    assert!(is_cloudflare_challenge(
        503,
        "__cf_chl_opt_tKb6Qe=...; cf-browser-gesture"
    ));
    // 403 + 特征（69shuba 等强质询）→ true
    assert!(is_cloudflare_challenge(
        403,
        "<title>Just a moment...</title> challenge-platform"
    ));
    // Turnstile 特征（challenges.cloudflare.com/turnstile、cf-turnstile、turnstile/api.js）
    assert!(is_cloudflare_challenge(
        503,
        "<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\"></script>"
    ));
    assert!(is_cloudflare_challenge(
        503,
        "<div class=\"cf-turnstile\" data-sitekey=\"0x4AAAAAAA\"></div>"
    ));
    assert!(is_cloudflare_challenge(403, "turnstile/api.js"));
    // 非 503/403 → false（即使含特征）
    assert!(!is_cloudflare_challenge(200, "Just a moment"));
    // 503 无特征 → false（零开销直连路径）
    assert!(!is_cloudflare_challenge(503, "<html>maintenance</html>"));
    assert!(!is_cloudflare_challenge(404, "challenge-platform"));
}

#[test]
fn test_parse_cookie_string() {
    assert_eq!(
        parse_cookie_string("a=1; b=2"),
        vec![("a".into(), "1".into()), ("b".into(), "2".into())]
    );
    assert_eq!(
        parse_cookie_string("a=1;; b="),
        vec![("a".into(), "1".into())]
    );
    assert_eq!(parse_cookie_string(""), Vec::<(String, String)>::new());
}

/// 登录头合并：JSON map 解析 + 覆盖源 header 同名键（legacy getHeaderMap(true)）
#[test]
fn test_merge_login_header() {
    let mut headers = HashMap::new();
    headers.insert("User-Agent".to_string(), "src-ua".to_string());
    headers.insert("X-Token".to_string(), "old".to_string());
    merge_login_header(
        &mut headers,
        r#"{"X-Token":"tok-1","X-Auth":"alice","User-Agent":"login-ua"}"#,
    );
    assert_eq!(headers.get("X-Token").map(String::as_str), Some("tok-1"));
    assert_eq!(headers.get("X-Auth").map(String::as_str), Some("alice"));
    assert_eq!(
        headers.get("User-Agent").map(String::as_str),
        Some("login-ua"),
        "登录头应覆盖源 header 同名键"
    );
    // 空/非 JSON 不破坏原头
    merge_login_header(&mut headers, "");
    assert_eq!(headers.get("X-Token").map(String::as_str), Some("tok-1"));
    merge_login_header(&mut headers, "X-Plain=1");
    assert_eq!(headers.get("X-Plain").map(String::as_str), Some("1"));
}

/// 合并策略：同名 FS 覆盖、不同名保留用户值、顺序稳定（用户序为基底 + FS 新名追加）
#[test]
fn test_merge_fs_cookies() {
    let user = "sid=abc; theme=dark";
    let fs = vec![
        FsCookie {
            name: "cf_clearance".into(),
            value: "xyz".into(),
            domain: None,
            path: None,
        },
        FsCookie {
            name: "theme".into(),
            value: "light".into(),
            domain: None,
            path: None,
        },
    ];
    let merged = merge_fs_cookies(user, &fs);
    assert_eq!(merged, "sid=abc; theme=light; cf_clearance=xyz");
}

#[test]
fn test_merge_fs_cookies_empty_user() {
    let fs = vec![FsCookie {
        name: "cf_clearance".into(),
        value: "xyz".into(),
        domain: None,
        path: None,
    }];
    assert_eq!(merge_fs_cookies("", &fs), "cf_clearance=xyz");
    assert_eq!(merge_fs_cookies("a=1", &[]), "a=1");
    assert_eq!(merge_fs_cookies("", &[]), "");
}

/// Turnstile token 伪 cookie 合并：追加 / 空串 / 同名覆盖（上次求解残留）
#[test]
fn test_merge_turnstile_token() {
    assert_eq!(
        merge_turnstile_token("sid=abc", "tok-1"),
        "sid=abc; cf_turnstile_token=tok-1"
    );
    assert_eq!(
        merge_turnstile_token("", "tok-1"),
        "cf_turnstile_token=tok-1"
    );
    assert_eq!(
        merge_turnstile_token("sid=abc; cf_turnstile_token=old", "new"),
        "sid=abc; cf_turnstile_token=new"
    );
}

#[test]
fn test_flaresolverr_disabled_by_default() {
    // 未配置 FLARESOLVERR_URL → Ok(None)（降级直连，不影响现有路径）
    std::env::remove_var("FLARESOLVERR_URL");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = rt.block_on(flaresolverr_request(
        "https://a.com",
        "a=1",
        "GET",
        None,
        15,
    ));
    assert!(r.is_ok());
    assert!(r.unwrap().is_none());
}

/// 浏览器不可用分支单测：solve_cf_builtin 返回明确错误（不启动浏览器、不发请求）；
/// camoufox 禁用时 solve_cf_builtin 返回明确错误（不发起任何 HTTP 调用）
#[test]
fn test_cf_builtin_browser_unavailable_returns_clear_error() {
    std::env::set_var("READER_CAMOUFOX_DISABLE", "1");
    force_cf_browser_available(Some(false));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = rt.block_on(solve_cf_builtin(
        "default",
        "https://cf.example.com/book/1",
        "sid=abc",
        None,
    ));
    force_cf_browser_available(None);
    std::env::remove_var("READER_CAMOUFOX_DISABLE");
    let err = r.expect_err("浏览器不可用应返回错误");
    assert!(
        err.to_string().contains("camoufox") || err.to_string().contains("禁用"),
        "错误应提示 camoufox 未启用: {err}"
    );
}

/// 微型 HTTP 服务器：返回固定状态/Content-Type/二进制体；可记录收到的请求头
async fn serve_image(
    status: u16,
    content_type: &'static str,
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
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            captured.lock().unwrap().push(req);
            let reason = if status == 200 { "OK" } else { "ERR" };
            let head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut resp = head.into_bytes();
            resp.extend_from_slice(&body);
            let _ = sock.write_all(&resp).await;
        }
    });
    format!("http://{addr}")
}

/// GAP #88/125：fetch_image——二进制透传 + Content-Type + Referer/书源 cookie 附加
#[tokio::test]
async fn test_fetch_image_binary_and_headers() {
    let _ssrf = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
    clear_cookie_storage();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let png = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3, 4];
    let url = serve_image(200, "image/png", png.clone(), captured.clone()).await;

    let (bytes, content_type, status) = fetch_image(
        "default",
        &url,
        Some("https://src.com/book/1"),
        10,
        5 * 1024 * 1024,
    )
    .await
    .unwrap();
    assert_eq!(bytes, png, "图片字节应原样透传");
    assert_eq!(
        content_type.as_deref(),
        Some("image/png"),
        "Content-Type 透传"
    );
    assert_eq!(status, 200);
    let req = captured.lock().unwrap()[0].clone();
    assert!(
        req.to_lowercase()
            .contains("referer: https://src.com/book/1"),
        "应携带 Referer（防盗链绕过）: {req}"
    );

    // 非 200 状态透传
    let url = serve_image(404, "text/plain", b"nf".to_vec(), captured.clone()).await;
    let (bytes, _, status) = fetch_image("default", &url, None, 10, 5 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(status, 404);
    assert_eq!(bytes, b"nf");
}

/// GAP #88/125：大小上限——Content-Length 预检与流式累计双重拦截
#[tokio::test]
async fn test_fetch_image_size_cap() {
    let _ssrf = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
    clear_cookie_storage();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let body = vec![b'x'; 2048];
    let url = serve_image(
        200,
        "application/octet-stream",
        body.clone(),
        captured.clone(),
    )
    .await;
    // Content-Length 预检：声明 2048 > 上限 100 → 拒绝
    let err = fetch_image("default", &url, None, 10, 100)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("图片超过大小上限"), "{err}");
    // 正常上限内通过
    let (bytes, _, _) = fetch_image("default", &url, None, 10, 4096).await.unwrap();
    assert_eq!(bytes, body);
}

/// 书源抓取校验独立于全局放行：即使 `SSRF_ALLOW_PRIVATE=true`（内网 TTS/图片场景），
/// 书源入口与重定向仍拦截私网/回环——防"全局放行顺带放开书源"导致的 SSRF。
#[tokio::test]
async fn test_source_target_blocked_even_when_global_allows() {
    // 持锁，先把两个开关都置「拦截」，再只翻全局那个：
    // 本用例要守的就是「全局放行不能顺带放行书源」这条独立收紧
    let _g = ssrf_allow_private_guard(false);
    SSRF_ALLOW_PRIVATE.store(true, Ordering::Relaxed);
    for url in [
        "http://127.0.0.1:8085/x.png",
        "http://172.17.0.1:9911/book/1",
        "http://192.168.1.10/x",
    ] {
        assert!(
            validate_public_target(url).await.is_ok(),
            "全局放行应生效: {url}"
        );
        let err = validate_source_target(url).await.unwrap_err();
        assert!(
            err.to_string().contains("已拦截"),
            "书源应拦截 {url}: {err}"
        );
        let err2 = validate_source_redirect_target(url).unwrap_err();
        assert!(
            err2.to_string().contains("已拦截"),
            "书源重定向应拦截 {url}: {err2}"
        );
    }
    // 公网目标不受影响
    assert!(validate_source_target("https://example.com/x")
        .await
        .is_ok());
}

/// `SOURCE_ALLOW_PRIVATE` 开关：显式放行时书源可访问私网，关闭后恢复拦截
#[tokio::test]
async fn test_source_allow_private_switch() {
    let _g = ssrf_allow_private_guard(false); // 持锁 + 全局拦截态
    let prev = SOURCE_ALLOW_PRIVATE.swap(true, Ordering::Relaxed);
    let allowed = validate_source_target("http://127.0.0.1:1/x").await.is_ok();
    SOURCE_ALLOW_PRIVATE.store(prev, Ordering::Relaxed);
    let blocked = validate_source_target("http://127.0.0.1:1/x")
        .await
        .is_err();
    assert!(allowed, "SOURCE_ALLOW_PRIVATE=true 时应放行私网书源");
    assert!(blocked, "开关关闭后应恢复拦截");
}

/// M1 SSRF：私网/回环/链路本地字面 IP 一律拒绝（127.0.0.1、10/8、172.16/12、
/// 192.168/16、169.254/16、::1、fc00::/7、0.0.0.0、IPv4 映射回环）
#[tokio::test]
async fn test_ssrf_rejects_private_literal_ips() {
    let _g = ssrf_allow_private_guard(false); // 持锁：确保无测试并发放行私网
    for (url, label) in [
        ("http://127.0.0.1:8085/reader3/getSystemInfo", "回环 127/8"),
        ("http://127.1.2.3/x.png", "回环 127/8 非 .0.1"),
        ("http://10.0.0.6/x.png", "私网 10/8"),
        ("http://172.16.5.5/x.png", "私网 172.16/12"),
        ("http://172.31.255.255/x.png", "私网 172.16/12 上界"),
        ("http://192.168.1.1/x.png", "私网 192.168/16"),
        (
            "http://169.254.169.254/latest/meta-data",
            "链路本地（云元数据）",
        ),
        ("http://0.0.0.0/x.png", "未指定"),
        ("http://[::1]:8085/x.png", "IPv6 回环"),
        ("http://[fc00::1]/x.png", "IPv6 ULA fc00::/7"),
        ("http://[fd12:3456::1]/x.png", "IPv6 ULA fd00::/8"),
        ("http://[::ffff:127.0.0.1]/x.png", "IPv4 映射回环"),
        ("http://localhost:8085/x.png", "localhost 字面量"),
    ] {
        let err = validate_public_target(url).await.unwrap_err();
        assert!(
            err.to_string().contains("已拦截"),
            "{label}（{url}）应被拦截: {err}"
        );
    }
}

/// M1 SSRF：公网地址放行——字面公网 IP 与公网域名（DNS 解析后校验）
#[tokio::test]
async fn test_ssrf_allows_public_targets() {
    let _g = ssrf_allow_private_guard(false);
    validate_public_target("https://8.8.8.8/x.png")
        .await
        .expect("公网字面 IP 应放行");
    validate_public_target("https://1.1.1.1/x.png")
        .await
        .expect("公网字面 IP 应放行");
    // 公网域名：DNS 解析后应为公网地址（example.com 固定公网；离线环境跳过该断言）
    if tokio::net::lookup_host(("example.com", 443)).await.is_ok() {
        validate_public_target("https://example.com/x.png")
            .await
            .expect("公网域名解析后应放行");
    }
}

/// M1 SSRF：fetch_image 整体拒绝私网目标（不经 DNS 也拦截）——代理端点唯一回源入口
#[tokio::test]
async fn test_fetch_image_rejects_private_url() {
    let _g = ssrf_allow_private_guard(false);
    let err = fetch_image("default", "http://127.0.0.1:1/x.png", None, 3, 1024)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("已拦截"),
        "私网 URL 应在发请求前被拦截: {err}"
    );
}

/// M1 SSRF 回归：手动重定向跟进仍正常（Policy::none + 每跳校验）——302 → 公网目标可拉取；
/// 跳转目标为私网时由 validate_public_target 拦截（该函数本身有独立单测）
#[tokio::test]
async fn test_fetch_image_follows_redirect() {
    let _g = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
                                             // 目标服务器：返回 png
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = target.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let body = [0x89u8, b'P', b'N', b'G', 7, 7, 7];
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut resp = head.into_bytes();
        resp.extend_from_slice(&body);
        let _ = sock.write_all(&resp).await;
    });
    // 入口服务器：302 → 目标
    let entry = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let entry_addr = entry.local_addr().unwrap();
    let loc = format!("http://{target_addr}/final.png");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = entry.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = sock.write_all(head.as_bytes()).await;
    });
    let (bytes, ct, status) = fetch_image(
        "default",
        &format!("http://{entry_addr}/start.png"),
        None,
        5,
        1024,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(ct.as_deref(), Some("image/png"));
    assert_eq!(bytes, vec![0x89, b'P', b'N', b'G', 7, 7, 7]);
}

/// P1 SSRF 全覆盖：fetch 入口拒绝私网目标（http_get/http_post 书源抓取、
/// java.ajax 等 JS shim、rss/schedule 订阅抓取均经 fetch——统一生效）
#[tokio::test]
async fn test_fetch_rejects_private_url() {
    let _g = ssrf_allow_private_guard(false);
    let headers = HashMap::new();
    // fetch 直调：回环 / 私网 / 链路本地（169.254 云元数据）
    for url in [
        "http://127.0.0.1:1/x",
        "http://10.0.0.1/x",
        "http://192.168.1.1/x",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]:1/x",
    ] {
        let err = fetch(url, &headers, 3, "GET", None, None, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("已拦截"),
            "fetch 应拦截私网目标（{url}）: {err}"
        );
    }
    // http_get / http_post 同链路生效
    let err = http_get("default", "http://127.0.0.1:1/x", &headers, 3, None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("已拦截"), "http_get 应拦截: {err}");
    let err = http_post(
        "default",
        "http://127.0.0.1:1/x",
        &headers,
        3,
        Some("a=1"),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("已拦截"),
        "http_post 应拦截: {err}"
    );
}

/// P1 SSRF：重定向跳转目标同步校验（Policy::custom 闭包）——私网/回环/非法 URL
/// 拒绝；公网字面 IP 与公网域名放行（离线环境跳过域名断言）
#[test]
fn test_validate_redirect_target() {
    let _g = ssrf_allow_private_guard(false);
    for (url, label) in [
        ("http://127.0.0.1/x", "回环"),
        ("http://10.0.0.1/x", "私网 10/8"),
        ("http://172.16.0.1/x", "私网 172.16/12"),
        ("http://169.254.169.254/latest/meta-data", "链路本地"),
        ("http://[::1]/x", "IPv6 回环"),
        ("http://[fc00::1]/x", "IPv6 ULA"),
        ("http://localhost/x", "localhost 字面量"),
        ("not a url", "非法 URL"),
    ] {
        let err = validate_redirect_target(url).unwrap_err();
        assert!(
            err.to_string().contains("已拦截") || err.to_string().contains("非法"),
            "{label}（{url}）应被拦截: {err}"
        );
    }
    validate_redirect_target("https://8.8.8.8/x").expect("公网字面 IP 应放行");
    validate_redirect_target("https://1.1.1.1/x").expect("公网字面 IP 应放行");
    // 公网域名：DNS 解析后应为公网地址（离线环境跳过）
    if std::net::ToSocketAddrs::to_socket_addrs(&("example.com", 443)).is_ok() {
        validate_redirect_target("https://example.com/x").expect("公网域名解析后应放行");
    }
}

/// P1 SSRF：fetch 重定向正常跟进（Policy::custom 不破坏合法跳转）——
/// 302 → 公网目标可拉取（mock 绑定 127.0.0.1，持放行守卫）
#[tokio::test]
async fn test_fetch_follows_redirect() {
    let _g = ssrf_allow_private_guard(true); // mock 服务器绑定 127.0.0.1
                                             // 目标服务器：返回 200 文本
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = target.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let body = "redirected-ok";
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut resp = head.into_bytes();
        resp.extend_from_slice(body.as_bytes());
        let _ = sock.write_all(&resp).await;
    });
    // 入口服务器：302 → 目标
    let entry = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let entry_addr = entry.local_addr().unwrap();
    let loc = format!("http://{target_addr}/final");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = entry.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = sock.write_all(head.as_bytes()).await;
    });
    let headers = HashMap::new();
    let resp = fetch(
        &format!("http://{entry_addr}/start"),
        &headers,
        5,
        "GET",
        None,
        None,
        None,
    )
    .await
    .expect("302 应自动跟进");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "redirected-ok");
    assert!(
        resp.url.ends_with("/final"),
        "最终 URL 应为跳转后地址: {}",
        resp.url
    );
}

/// 反爬域名自动优先：成功标记后同主机 30 分钟窗口内 GET 优先浏览器。
#[test]
fn test_browser_needed_marks_host() {
    mark_browser_needed("https://Example.COM/some/path");
    assert!(browser_needed("https://example.com/other"));
    assert!(!browser_needed("https://other.example.com/"));
}

/// #3 回归：搜索作用域必须把整条 `(browser_first_enabled || browser_needed)` 短路成
/// false——即使 host 已在**进程内全局**缓存里（只关 env 开关救不了这种情况）。
#[test]
fn test_fetch_scope_search_never_browser_first() {
    let url = "https://scope-test.example.com/search?q=1";
    mark_browser_needed(url);
    assert!(
        browser_needed(url),
        "前置：host 已在全局缓存（30min TTL）里"
    );
    assert!(
        !browser_first_for(FetchScope::Search, "GET", url),
        "搜索作用域下 GET 也不得走浏览器优先"
    );
    // 非 GET 本来就 false（行为不变）
    assert!(!browser_first_for(FetchScope::Book, "POST", url));
    assert!(!browser_first_for(FetchScope::Search, "POST", url));
    // 书信息/正文作用域：GET 的判定与历史表达式逐位一致
    assert_eq!(
        browser_first_for(FetchScope::Book, "GET", url),
        (browser_first_enabled() || browser_needed(url)) && browser_solver_available()
    );
}

// ==================== charset 表单/query 字段编码（legacy analyzeFields） ====================

/// hasUrlEncoded 对齐：全保留字符或合法 %XX → 已编码；裸中文/杂散 % → 未编码
#[test]
fn test_has_url_encoded() {
    assert!(has_url_encoded("%E8%AF%A1%E7%A7%98%E4%B9%8B%E4%B8%BB"));
    assert!(has_url_encoded("abc123+-_.$:()!*@&#,[]"));
    assert!(has_url_encoded(""));
    assert!(!has_url_encoded("诡秘之主"));
    assert!(!has_url_encoded("a b"));
    assert!(!has_url_encoded("50%"));
    assert!(!has_url_encoded("100%%"));
}

/// GBK 表单编码：中文值 → GBK 字节 percent 序列；ASCII 值原样；空格→+
#[test]
fn test_encode_form_fields_gbk() {
    assert_eq!(
        encode_form_fields("key=中文值&other=data", "gbk"),
        "key=%D6%D0%CE%C4%D6%B5&other=data"
    );
    // 空格转 +（Java URLEncoder 语义）；+ 字面量转 %2B
    assert_eq!(encode_form_fields("q=a b+c", "gbk"), "q=a+b%2Bc");
    // 键不编码、无值段与空段容忍
    assert_eq!(encode_form_fields("中文=值", "gb2312"), "中文=%D6%B5");
    assert_eq!(encode_form_fields("a=&b=2&", "gbk"), "a=&b=2");
}

/// 已含 %XX 的值不重复编码（预编码 body + charset=gbk 常见——69shuba 等）
#[test]
fn test_encode_form_fields_skip_pre_encoded() {
    let body = "searchkey=%E8%AF%A1%E7%A7%98%E4%B9%8B%E4%B8%BB&searchtype=all&page=1";
    assert_eq!(encode_form_fields(body, "gbk"), body);
}

/// charset="escape"：JS escape() 风格（仅字母数字保留、小写十六进制、非 ASCII %uxxxx）
#[test]
fn test_encode_form_fields_escape() {
    assert_eq!(encode_form_fields("q=中 abc", "escape"), "q=%u4e2d%20abc");
    assert_eq!(encode_form_fields("k=中*._", "escape"), "k=%u4e2d%2a%2e%5f");
}

/// POST 中文原文 body + charset=gbk → 实际发出的 body 为 GBK percent 编码，
/// 且自动补 Content-Type: application/x-www-form-urlencoded
#[tokio::test]
async fn test_http_post_form_body_encoded_by_charset_gbk() {
    let _ssrf = ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1
    clear_cookie_storage();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cap = captured.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        cap.lock()
            .unwrap()
            .push(String::from_utf8_lossy(&buf[..n]).to_string());
        let body = "ok";
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(head.as_bytes()).await;
    });
    let mut headers = HashMap::new();
    headers.insert("User-Agent".to_string(), "test-agent".to_string());
    let resp = http_post(
        "default",
        &format!("http://{addr}/search"),
        &headers,
        10,
        Some("key=中文值&other=data"),
        Some("gbk"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(resp.status, 200);
    let req = captured.lock().unwrap()[0].clone();
    assert!(
        req.contains("\r\n\r\nkey=%D6%D0%CE%C4%D6%B5&other=data"),
        "POST body 应为 GBK percent 编码序列: {req}"
    );
    assert!(
        req.to_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "应自动补 form Content-Type: {req}"
    );
}

/// GET query 同样处理：URL ? 后参数值按 charset 编码
#[tokio::test]
async fn test_http_get_query_encoded_by_charset_gbk() {
    let _ssrf = ssrf_allow_private_guard(true);
    clear_cookie_storage();
    // 强制浏览器不可用：GET 走直连（避免测试机装了 camoufox 时被浏览器优先截胡）
    force_cf_browser_available(Some(false));
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cap = captured.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        cap.lock()
            .unwrap()
            .push(String::from_utf8_lossy(&buf[..n]).to_string());
        let body = "ok";
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(head.as_bytes()).await;
    });
    let resp = http_get(
        "default",
        &format!("http://{addr}/s?wd=中文&p=1"),
        &HashMap::new(),
        10,
        Some("gbk"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(resp.status, 200);
    let req = captured.lock().unwrap()[0].clone();
    force_cf_browser_available(None);
    assert!(
        req.starts_with("GET /s?wd=%D6%D0%CE%C4&p=1 "),
        "GET query 值应按 GBK percent 编码: {req}"
    );
}

/// charset 缺省 / utf-8 / JSON body 不做任何改写（保持既有行为）
#[tokio::test]
async fn test_http_post_charset_untouched_cases() {
    let _ssrf = ssrf_allow_private_guard(true);
    clear_cookie_storage();
    for (body, charset) in [
        ("{\"key\": \"中文\"}", Some("gbk")),
        ("key=中文&other=data", None),
        ("key=中文&other=data", Some("utf-8")),
        ("key=中文&other=data", Some("")),
    ] {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cap = captured.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            cap.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_string());
            let body = "ok";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
        });
        http_post(
            "default",
            &format!("http://{addr}/x"),
            &HashMap::new(),
            10,
            Some(body),
            charset,
            None,
        )
        .await
        .unwrap();
        let req = captured.lock().unwrap()[0].clone();
        assert!(
            req.contains(body),
            "charset={charset:?} 时 body 应原样发送: {req}"
        );
    }
}

/// UrlOption.retry 重试循环（legacy AnalyzeUrl.kt:564-573）：
/// None/0 → 单次请求；n>0 → 共 n+1 次尝试，成功即止；失败次数耗尽返回末次错误
#[tokio::test]
async fn test_fetch_with_retry_loop() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    let counter = Arc::new(AtomicUsize::new(0));
    // 首次成功：retry=3 也仅请求一次
    let c = counter.clone();
    fetch_with_retry(Some(3), || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok::<(), anyhow::Error>(())
        }
    })
    .await
    .unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    // 永远失败：retry=2 → 共 3 次，返回错误
    let c = counter.clone();
    assert!(fetch_with_retry(Some(2), || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(anyhow!("boom"))
        }
    })
    .await
    .is_err());
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    // retry=None / Some(0)：均单次请求
    for r in [None, Some(0)] {
        let c = counter.clone();
        assert!(fetch_with_retry(r, || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(anyhow!("boom"))
            }
        })
        .await
        .is_err());
    }
    assert_eq!(counter.load(Ordering::SeqCst), 6);
}
