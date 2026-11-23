//! 书籍 API 单元测试（目录缓存合成、API 纯 JSON Body 契约与 Query 向后兼容）

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::chapter::{cache_url_of, parse_cache_url, synthesize_toc_from_cache};

async fn test_app(
    tag: &str,
    secure: bool,
) -> (axum::Router, crate::storage::Storage, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "reader-books-test-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = crate::AppConfig::from_env();
    cfg.work_dir = dir.to_string_lossy().into_owned();
    cfg.secure = secure;
    cfg.mode = if secure {
        "multi".to_string()
    } else {
        "single".to_string()
    };
    let storage = crate::storage::init(&cfg).await.unwrap();
    let app = crate::api::router::router(cfg, storage.clone());
    (app, storage, dir)
}

async fn post_json(app: &axum::Router, uri: &str, body: Value, auth: Option<&str>) -> Value {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(tok) = auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {tok}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_empty(app: &axum::Router, uri: &str, auth: Option<&str>) -> Value {
    let mut builder = Request::builder().method(Method::POST).uri(uri);
    if let Some(tok) = auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {tok}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn synthesize_toc_from_cache_orders_by_tail_id_and_chapter_num() {
    let u1 = "https://b.test/ch/1001.html".to_string();
    let u2 = "https://b.test/ch/1002.html".to_string();
    let h1 = crate::util::md5::chapter_url_hash(&u1);
    let h2 = crate::util::md5::chapter_url_hash(&u2);
    let toc = synthesize_toc_from_cache(
        &[(h2, "第2章 续".into()), (h1, "第1章 起".into())],
        &[u1.clone(), u2.clone()],
        "https://b.test/book/1",
    );
    assert_eq!(toc.len(), 2);
    assert_eq!(toc[0].url, u1);
    assert_eq!(toc[1].url, u2);
    assert_eq!(toc[0].index, 0);
    assert_eq!(toc[1].index, 1);
}

#[test]
fn cache_url_roundtrip() {
    let u = cache_url_of("https://a.test/b#1", 42);
    assert_eq!(parse_cache_url(&u), Some(("https://a.test/b#1", 42)));
    assert_eq!(parse_cache_url("https://a.test/b"), None);
}

#[tokio::test]
async fn save_book_progress_accepts_pure_json_body_and_query_fallback() {
    let (app, _storage, dir) = test_app("progress", false).await;
    let book_url = "https://book.test/contract-1";

    let saved = post_json(
        &app,
        "/reader3/saveBook",
        json!({
            "bookUrl": book_url,
            "name": "契约测试书",
            "author": "测试作者",
            "origin": "https://book.test",
            "originName": "测试源"
        }),
        None,
    )
    .await;
    assert_eq!(saved["isSuccess"], true);

    // 1. 纯 JSON Body（标准数字型 + 精简字段名 url/index/pos/title）
    let res = post_json(
        &app,
        "/reader3/saveBookProgress",
        json!({
            "url": book_url,
            "index": 3,
            "pos": 120,
            "title": "第4章 纯JSON"
        }),
        None,
    )
    .await;
    assert_eq!(res["isSuccess"], true);
    let shelf = get_json(&app, "/reader3/getBookshelf").await;
    assert_eq!(shelf["data"][0]["durChapterIndex"], 3);
    assert_eq!(shelf["data"][0]["durChapterPos"], 120);
    assert_eq!(shelf["data"][0]["durChapterTitle"], "第4章 纯JSON");

    // 2. 纯 JSON Body（字符串型数字字段，且 Body 优先于 Query）
    let res = post_json(
        &app,
        "/reader3/saveBookProgress?index=1&pos=1",
        json!({
            "url": book_url,
            "index": "5",
            "pos": "42",
            "durChapterTitle": "第6章 字符串数值"
        }),
        None,
    )
    .await;
    assert_eq!(res["isSuccess"], true);
    let shelf = get_json(&app, "/reader3/getBookshelf").await;
    assert_eq!(shelf["data"][0]["durChapterIndex"], 5);
    assert_eq!(shelf["data"][0]["durChapterPos"], 42);
    assert_eq!(shelf["data"][0]["durChapterTitle"], "第6章 字符串数值");

    // 3. 向后兼容：纯 Query 参数兜底
    let res = post_empty(
        &app,
        "/reader3/saveBookProgress?bookUrl=https%3A%2F%2Fbook.test%2Fcontract-1&durChapterIndex=7&durChapterPos=9&durChapterTitle=%E7%AC%AC8%E7%AB%A0",
        None,
    )
    .await;
    assert_eq!(res["isSuccess"], true);
    let shelf = get_json(&app, "/reader3/getBookshelf").await;
    assert_eq!(shelf["data"][0]["durChapterIndex"], 7);
    assert_eq!(shelf["data"][0]["durChapterPos"], 9);
    assert_eq!(shelf["data"][0]["durChapterTitle"], "第8章");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn book_group_endpoints_accept_pure_json_body_and_query_fallback() {
    let (app, _storage, dir) = test_app("groups", false).await;
    let book_url = "https://book.test/group-1";

    let saved = post_json(
        &app,
        "/reader3/saveBook",
        json!({
            "bookUrl": book_url,
            "name": "分组测试书",
            "author": "作者",
            "origin": "https://book.test",
            "originName": "测试源"
        }),
        None,
    )
    .await;
    assert_eq!(saved["isSuccess"], true);

    let g1 = post_json(
        &app,
        "/reader3/saveBookGroup",
        json!({ "name": "玄幻" }),
        None,
    )
    .await;
    let g2 = post_json(
        &app,
        "/reader3/saveBookGroup",
        json!({ "name": "科幻" }),
        None,
    )
    .await;
    let g3 = post_json(
        &app,
        "/reader3/saveBookGroup",
        json!({ "name": "历史" }),
        None,
    )
    .await;
    let id1 = g1["data"]["id"].as_i64().unwrap();
    let id2 = g2["data"]["id"].as_i64().unwrap();
    let id3 = g3["data"]["id"].as_i64().unwrap();

    // saveBookGroupId：纯 JSON Body（url + 数字 groupId）
    let res = post_json(
        &app,
        "/reader3/saveBookGroupId",
        json!({ "url": book_url, "groupId": id1 }),
        None,
    )
    .await;
    assert_eq!(res["isSuccess"], true);
    let shelf = get_json(&app, "/reader3/getBookshelf").await;
    assert_eq!(shelf["data"][0]["group"], id1);

    // saveBookGroupId：纯 JSON Body（字符串 groupId，优先于 Query）
    let res = post_json(
        &app,
        &format!("/reader3/saveBookGroupId?groupId={id1}"),
        json!({ "bookUrl": book_url, "groupId": id2.to_string() }),
        None,
    )
    .await;
    assert_eq!(res["isSuccess"], true);
    let shelf = get_json(&app, "/reader3/getBookshelf").await;
    assert_eq!(shelf["data"][0]["group"], id2);

    // deleteBookGroup：纯 JSON Body（{"id": id1} 与 {"groupId": "id2"}）
    let del1 = post_json(&app, "/reader3/deleteBookGroup", json!({ "id": id1 }), None).await;
    assert_eq!(del1["isSuccess"], true);
    let del2 = post_json(
        &app,
        "/reader3/deleteBookGroup",
        json!({ "groupId": id2.to_string() }),
        None,
    )
    .await;
    assert_eq!(del2["isSuccess"], true);

    // deleteBookGroup：纯 Query 参数兜底
    let del3 = post_empty(&app, &format!("/reader3/deleteBookGroup?id={id3}"), None).await;
    assert_eq!(del3["isSuccess"], true);

    let groups = get_json(&app, "/reader3/getBookGroups").await;
    let user_group_count = groups["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["id"].as_i64().unwrap_or(0) > 0)
        .count();
    assert_eq!(user_group_count, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn explore_book_and_clear_inactive_users_accept_pure_json_body() {
    let (app, storage, dir) = test_app("contract-other", true).await;
    let now = chrono::Utc::now().timestamp_millis();
    let day_ms = 86_400_000i64;

    let make_user =
        |username: &str, token: &str, is_admin: bool, last_login_at: i64| crate::model::User {
            username: username.to_string(),
            password: "hash".to_string(),
            salt: "salt".to_string(),
            token: token.to_string(),
            token_map: None,
            is_admin,
            disabled: false,
            enable_webdav: true,
            enable_local_store: true,
            enable_book_source: true,
            enable_rss_source: true,
            book_source_limit: 0,
            book_limit: 0,
            last_login_at,
            created_at: last_login_at,
            user_namespace: username.to_string(),
            raw_json: None,
        };
    storage
        .insert_user(&make_user("admin01", "admintok", true, now))
        .await
        .unwrap();
    storage
        .insert_user(&make_user("stale01", "tok1", false, now - 60 * day_ms))
        .await
        .unwrap();
    storage
        .insert_user(&make_user("fresh01", "tok2", false, now - 5 * day_ms))
        .await
        .unwrap();
    let admin_auth = "admin01:admintok";

    // exploreBook：纯 JSON Body（bookSourceUrl + ruleFindUrl + 字符串 page）
    let explore = post_json(
        &app,
        "/reader3/exploreBook",
        json!({
            "bookSourceUrl": "https://missing-source.test",
            "ruleFindUrl": "https://missing-source.test/rank/{{page}}",
            "page": "2"
        }),
        Some(admin_auth),
    )
    .await;
    assert_eq!(explore["isSuccess"], false);
    assert_eq!(explore["errorMsg"], "书源不存在");

    // clearInactiveUsers：纯 JSON Body {"days": 30} 仅清理超过 30 天未登录的 stale01
    let cleared = post_json(
        &app,
        "/reader3/clearInactiveUsers",
        json!({ "days": 30 }),
        Some(admin_auth),
    )
    .await;
    assert_eq!(cleared["isSuccess"], true);
    assert_eq!(cleared["data"]["count"], 1);
    assert!(storage.find_user("stale01").await.unwrap().is_none());
    assert!(storage.find_user("fresh01").await.unwrap().is_some());

    // clearInactiveUsers：兼容 {"day": "1"} 字符串数值格式，清理 5 天前登录的 fresh01
    let cleared2 = post_json(
        &app,
        "/reader3/clearInactiveUsers",
        json!({ "day": "1" }),
        Some(admin_auth),
    )
    .await;
    assert_eq!(cleared2["isSuccess"], true);
    assert_eq!(cleared2["data"]["count"], 1);
    assert!(storage.find_user("fresh01").await.unwrap().is_none());
    assert!(storage.find_user("admin01").await.unwrap().is_some());

    let _ = std::fs::remove_dir_all(&dir);
}
