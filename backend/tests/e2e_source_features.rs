//! C1 第三轮 E2E：书源 Cookie 注入 / 自定义 header 透传 / loginCheckJs。
//!
//! - 回显端点校验：`source.header` 合并注入、cookie store 注入
//! - `set_cookie_for` → 后续同源请求自动携带 Cookie
//! - `loginCheckJs` 返回非 true/false 时抓取判定需登录并替换内容

use std::net::SocketAddr;
use std::sync::Mutex;

use axum::extract::Request as AxumRequest;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use reader_dev::service::crawler;
use reader_dev::storage::Storage;

mod common;

fn allow_private_net() -> &'static common::PrivateNetGuard {
    static GUARD: std::sync::OnceLock<common::PrivateNetGuard> = std::sync::OnceLock::new();
    GUARD.get_or_init(common::PrivateNetGuard::on)
}

/// 捕获最近一次请求的关键头（Cookie/User-Agent/X-Custom）
static CAPTURED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

async fn spawn_mock_site() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    async fn echo_headers(req: AxumRequest) -> impl IntoResponse {
        for key in ["cookie", "user-agent", "x-custom"] {
            if let Some(v) = req.headers().get(key) {
                if let Ok(vs) = v.to_str() {
                    CAPTURED
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((key.to_string(), vs.to_string()));
                }
            }
        }
        axum::http::Response::builder()
            .header("content-type", "text/html; charset=utf-8")
            .body(axum::body::Body::from(
                r#"<!doctype html><html><body><p class="ok">echo-ok</p></body></html>"#,
            ))
            .unwrap()
    }

    let app = Router::new().route("/echo", get(echo_headers));

    let server = axum::serve(listener, app);
    let addr: SocketAddr = server.local_addr().expect("mock 启动");
    tokio::spawn(async move { server.await });
    addr.port()
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// T1/T2：书源自定义 header + cookie store 注入（回显断言）
#[tokio::test(flavor = "multi_thread")]
async fn e2e_header_and_cookie_injection() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-cookie").await;
    crawler::register_cookie_storage(storage.clone());
    let ns = "default";
    let base = base_url(port);

    crawler::set_cookie_for(ns, &format!("{base}/echo"), "session=abc123; theme=dark").await;

    let source = reader_dev::model::BookSource {
        book_source_url: format!("{base}/src"),
        book_source_name: "回显源".into(),
        enabled: true,
        header: Some(r#"{"X-Custom": "custom-val", "User-Agent": "ReaderTestUA/1.0"}"#.into()),
        ..Default::default()
    };

    let url = base.clone() + "/echo";
    let resp = reader_dev::service::book::fetch_url(ns, &url, &source)
        .await
        .expect("回显抓取失败");
    assert!(resp.body.contains("echo-ok"));

    let captured = CAPTURED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let cookie_hit = captured
        .iter()
        .any(|(k, v)| k == "cookie" && v.contains("session=abc123"));
    let ua_hit = captured
        .iter()
        .any(|(k, v)| k == "user-agent" && v == "ReaderTestUA/1.0");
    let custom_hit = captured
        .iter()
        .any(|(k, v)| k == "x-custom" && v == "custom-val");
    assert!(ua_hit, "source.header 的 User-Agent 应透传: {captured:?}");
    assert!(custom_hit, "source.header 的自定义头应透传: {captured:?}");
    assert!(
        cookie_hit,
        "cookie store 中该源的 cookie 应自动注入请求: {captured:?}"
    );
    storage.pool.close().await;
}

/// T3：loginCheckJs——JS 判定页面需要登录时 fetch 链路给出明确信号
#[tokio::test(flavor = "multi_thread")]
async fn e2e_login_check_js_flow() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-loginjs").await;
    let ns = "default";
    let base = base_url(port);

    let mut source = reader_dev::model::BookSource {
        book_source_url: format!("{base}/login-src"),
        book_source_name: "登录检测源".into(),
        enabled: true,
        ..Default::default()
    };
    source.login_check_js =
        Some(r#"String(result).indexOf("登录已过期") !== -1 ? "NEED_LOGIN" : "false""#.into());

    let expired_html = "<html><body>登录已过期，请重新登录</body></html>";
    let replaced =
        reader_dev::service::book::apply_login_check_js(ns, &source, expired_html, &base, None)
            .await;
    assert_eq!(replaced, "NEED_LOGIN", "命中时应替换为 JS 输出: {replaced}");

    let ok_html = "<html><body>正文正常</body></html>";
    let passthrough =
        reader_dev::service::book::apply_login_check_js(ns, &source, ok_html, &base, None).await;
    assert_eq!(passthrough, ok_html, "未命中应原样透传");

    source.login_check_js = Some("\"true\"".into());
    let passthrough_true =
        reader_dev::service::book::apply_login_check_js(ns, &source, ok_html, &base, None).await;
    assert_eq!(passthrough_true, ok_html);
    storage.pool.close().await;
}

async fn test_storage(tag: &str) -> Storage {
    let dir = std::env::temp_dir().join(format!("reader-e2e-feat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = reader_dev::AppConfig::from_env();
    config.work_dir = dir.to_string_lossy().into_owned();
    reader_dev::storage::init(&config).await.unwrap()
}
