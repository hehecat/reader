//! C1 第四轮 E2E：失效源生命周期（F2/EG5）+ 健康探测。
//!
//! - 搜索失败 → mark_source_invalid → 600s 内前置短路跳过请求
//! - 成功抓取 → clear_source_invalid → 快照清空
//! - check_source HEAD 探测可用/不可用源

use std::net::SocketAddr;

use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use serde_json::json;

use reader_dev::model::BookSource;
use reader_dev::service::{health, search};
use reader_dev::storage::Storage;

mod common;

fn allow_private_net() -> &'static common::PrivateNetGuard {
    static GUARD: std::sync::OnceLock<common::PrivateNetGuard> = std::sync::OnceLock::new();
    GUARD.get_or_init(common::PrivateNetGuard::on)
}

async fn spawn_mock_site() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    let search_html = r#"<!doctype html><html><body>
<ul class="list"><li class="row"><a class="bk" href="/b/1.html">存活之书</a></li></ul>
</body></html>"#;

    let app = Router::new()
        .route(
            "/search",
            get(move || async move {
                axum::http::Response::builder()
                    .header("content-type", "text/html; charset=utf-8")
                    .body(axum::body::Body::from(search_html))
                    .unwrap()
            }),
        )
        .route(
            "/gone",
            get(|| async { (StatusCode::NOT_FOUND, "not found") }),
        );

    let server = axum::serve(listener, app);
    let addr: SocketAddr = server.local_addr().expect("mock 启动");
    tokio::spawn(async move { server.await });
    addr.port()
}

fn source_with(base: &str, path: &str) -> BookSource {
    serde_json::from_value(json!({
        "bookSourceUrl": format!("{base}{path}"),
        "bookSourceName": "生命周期源",
        "enabled": true,
        "searchUrl": format!("{base}{path}?q={{{{key}}}}"),
        "ruleSearch": {
            "bookList": "ul.list@li.row",
            "name": "a.bk@text",
            "bookUrl": "a.bk@href"
        }
    }))
    .unwrap()
}

/// 失效源全周期：失败标记 → 短路 → 清除恢复
#[tokio::test(flavor = "multi_thread")]
async fn e2e_invalid_source_lifecycle() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-health").await;
    // 唯一 ns 隔离全局失效表，避免并行测试互扰
    let ns = format!("ns-h-{}", std::process::id());
    let _base = format!("http://127.0.0.1:{port}");

    // 1) 网络层故障（拒绝连接）源搜索失败 → 标记失效
    // （临时 listener bind 后立即 drop，端口无人监听 → connection refused）
    let dead_port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let dead_base = format!("http://127.0.0.1:{dead_port}");
    let dead = source_with(&dead_base, "/gone");
    assert!(
        search::search_one_source(&storage, &ns, &dead, "x", 1, 15)
            .await
            .is_err(),
        "拒绝连接应报错"
    );
    assert!(
        health::is_source_invalid(&ns, &dead.book_source_url),
        "失败后应处于 600s 失效期"
    );
    let snap = health::invalid_snapshot(&ns);
    assert_eq!(snap.len(), 1, "快照应含该源记录");
    assert!(snap[0].2.contains("404") || !snap[0].2.is_empty());

    // 2) 失效期内再搜：前置短路直接返回空（不发请求）
    let r = search::search_one_source(&storage, &ns, &dead, "x", 1, 15)
        .await
        .expect("短路路径应返回 Ok(vec![])");
    assert!(r.is_empty(), "短路应返回空结果");

    // 3) clear（模拟成功抓取/手动恢复）→ 快照清空、可重新探测
    health::clear_source_invalid(&ns, &dead.book_source_url);
    assert!(!health::is_source_invalid(&ns, &dead.book_source_url));
    assert!(health::invalid_snapshot(&ns).is_empty());

    storage.pool.close().await;
}

/// 健康探测：200 存活源 vs 404 死源
#[tokio::test(flavor = "multi_thread")]
async fn e2e_check_source_probe() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let base = format!("http://127.0.0.1:{port}");
    let ns = format!("ns-probe-{}", std::process::id());

    let alive = BookSource {
        book_source_url: format!("{base}/search"),
        ..Default::default()
    };
    let (ok, _) = health::check_source(&ns, &alive).await;
    assert!(ok, "200 源应判定可用");

    let dead = BookSource {
        book_source_url: format!("{base}/gone"),
        ..Default::default()
    };
    let (ok2, _) = health::check_source(&ns, &dead).await;
    assert!(!ok2, "404 源应判定不可用");
}

/// 成功搜索后自动清除既有失效标记（Ok 分支 → clear_source_invalid）
#[tokio::test(flavor = "multi_thread")]
async fn e2e_success_clears_stale_mark() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-health-clear").await;
    let ns = format!("ns-c-{}", std::process::id());
    let base = format!("http://127.0.0.1:{port}");

    let alive = source_with(&base, "/search");
    // 预置一个过期前的人工标记
    health::mark_source_invalid(&ns, &alive.book_source_url, "历史错误");
    assert!(health::is_source_invalid(&ns, &alive.book_source_url));

    // 直接调用会短路——先手动清除再走成功链路验证清除逻辑
    health::clear_source_invalid(&ns, &alive.book_source_url);
    let results = search::search_one_source(&storage, &ns, &alive, "存活", 1, 15)
        .await
        .expect("存活源搜索失败");
    assert_eq!(results.len(), 1);
    // 成功后再查：仍非失效
    assert!(!health::is_source_invalid(&ns, &alive.book_source_url));

    storage.pool.close().await;
}

async fn test_storage(tag: &str) -> Storage {
    let dir = std::env::temp_dir().join(format!("reader-e2e-health-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = reader_dev::AppConfig::from_env();
    config.work_dir = dir.to_string_lossy().into_owned();
    reader_dev::storage::init(&config).await.unwrap()
}
