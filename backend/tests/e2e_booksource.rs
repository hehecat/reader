//! C1 端到端引擎集成测试：mock 书站 + 真实书源规则跑通
//! 搜索 → 详情 → 目录（分页）→ 正文（nextContentUrl 分页）全链路。
//!
//! 两个典型书源形态：
//! - CSS 源：HTML 搜索页 + CSS 选择器规则 + {{key}} 模板 + 目录翻页 + 正文翻页
//! - JSON 源：JSONPath 规则 + JSON API
//!
//! mock 站点为进程内 axum 服务（127.0.0.1 随机端口），SSRF 放行走 tests/common 守卫。


use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

use reader_dev::model::BookSource;
use reader_dev::service::book;
use reader_dev::service::search;
use reader_dev::storage::Storage;

mod common;

/// 全局一次性放行私网（OnceLock 永不 Drop——避免并行测试间 RAII 守卫互相恢复）
fn allow_private_net() -> &'static common::PrivateNetGuard {
    static GUARD: std::sync::OnceLock<common::PrivateNetGuard> = std::sync::OnceLock::new();
    GUARD.get_or_init(common::PrivateNetGuard::on)
}

/// 起一个 mock 书站，返回 (端口)
async fn spawn_mock_site() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    // ---- HTML 源页面 ----
    let search_html = r##"<!doctype html><html><body>
<ul class="result-list">
  <li class="item">
    <a class="title-link" href="/book/101.html">测试之书</a>
    <span class="author">张三</span>
    <span class="kind">玄幻 · 完结</span>
    <img class="cover" src="/img/101.jpg"/>
    <p class="intro">这是第一本书的简介。</p>
    <span class="latest">第100章 大结局</span>
  </li>
  <li class="item">
    <a class="title-link" href="/book/102.html">测试之书前传</a>
    <span class="author">李四</span>
  </li>
</ul>
</body></html>"##;

    let detail_html = r##"<!doctype html><html><head><meta charset="utf-8"></head><body>
<h1 class="book-title">测试之书</h1>
<span class="book-author">张三</span>
<div class="book-intro">详情页简介：一段精彩的冒险。</div>
<img class="book-cover" src="/img/cover-101.png"/>
<a class="toc-entry" href="/toc/101/page-1.html">开始阅读</a>
</body></html>"##;

    // 目录两页：page-1 有 3 章 + 下一页；page-2 有 2 章（含卷标题）
    let toc_p1 = r##"<!doctype html><html><body>
<dl class="chapter-list">
  <dt class="volume">第一卷</dt>
  <dd><a href="/chapter/101-1.html">第一章 起点</a></dd>
  <dd><a href="/chapter/101-2.html">第二章 转折</a></dd>
  <dd><a href="/chapter/101-3.html">第三章 高潮</a></dd>
</dl>
<a class="next-page" href="/toc/101/page-2.html">下一页</a>
</body></html>"##;

    let toc_p2 = r##"<!doctype html><html><body>
<dl class="chapter-list">
  <dd><a href="/chapter/101-4.html">第四章 结局(上)</a></dd>
  <dd><a href="/chapter/101-5.html">第五章 结局(下)</a></dd>
</dl>
</body></html>"##;

    // 正文页：每章两段 + nextContentUrl 翻一页
    let chapter_page = |title: &str, p1: &str, p2: &str, next: Option<&str>| -> String {
        let next_html = match next {
            Some(u) => format!(r#"<a class="next-content" href="{u}">下一页</a>"#),
            None => String::new(),
        };
        format!(
            r#"<!doctype html><html><body>
<h2 class="ch-title">{title}</h2>
<div id="content"><p>{p1}</p><br/><p>{p2}</p></div>
{next_html}
</body></html>"#
        )
    };

    // ---- JSON 源 ----
    let json_search = json!({
        "code": 0,
        "data": {
            "list": [
                {"bid": "/japi/detail/201", "bname": "接口之书", "author": "王五", "cat": "科幻", "cover": "/jimg/201.png", "desc": "JSON 源简介"},
                {"bid": "/japi/detail/202", "bname": "无关的书", "author": "赵六", "cat": "", "cover": "", "desc": ""}
            ]
        }
    })
    .to_string();
    let json_detail = json!({
        "info": {"bname": "接口之书", "author": "王五", "intro": "详情：接口之书的完整介绍。", "cover": "/jimg/d201.png"}
    })
    .to_string();
    let json_detail_empty = json!({
        "info": {"bname": "无关的书", "author": "赵六", "intro": "", "cover": ""}
    })
    .to_string();
    let json_toc: Vec<Value> = (1..=4)
        .map(|i| json!({"cname": format!("接口第{i}章"), "curl": format!("/japi/chapter/201/{i}")}))
        .collect();
    let json_toc = json!({ "chapters": json_toc }).to_string();
    let json_chapter = json!({"content": ["接口正文第一段。", "接口正文第二段。"]}).to_string();

    async fn text_response(body: impl Into<String>) -> axum::response::Response {
        axum::http::Response::builder()
            .header("content-type", "text/html; charset=utf-8")
            .body(axum::body::Body::from(body.into()))
            .unwrap()
            .into_response()
    }

    let app = Router::new()
        .route(
            "/search",
            get(move || async move { text_response(search_html).await }),
        )
        .route(
            "/book/101.html",
            get(move || async move { text_response(detail_html).await }),
        )
        .route(
            "/toc/101/page-1.html",
            get(move || async move { text_response(toc_p1).await }),
        )
        .route(
            "/toc/101/page-2.html",
            get(move || async move { text_response(toc_p2).await }),
        )
        .route(
            "/chapter/:id.html",
            get(
                move |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    // id 形如 "101-3"（第 3 章第一页）或 "101-3-2"（第二页）
                    let id = id.trim_end_matches(".html");
                    let segs: Vec<&str> = id.split('-').collect();
                    let ch: i64 = segs.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let is_page_two = segs.get(2).is_some();
                    if is_page_two {
                        text_response(chapter_page(
                            &format!("第{ch}章"),
                            &format!("第{ch}章第二页内容。"),
                            "",
                            None,
                        ))
                        .await
                    } else {
                        text_response(chapter_page(
                            &format!("第{ch}章"),
                            &format!("第{ch}章第一页内容。"),
                            "",
                            Some(&format!("/chapter/101-{ch}-2.html")),
                        ))
                        .await
                    }
                },
            ),
        )
        .route(
            "/japi/search",
            get(move || async move {
                axum::http::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(json_search))
                    .unwrap()
            }),
        )
        .route(
            "/japi/detail/:bid",
            get(
                move |axum::extract::Path(bid): axum::extract::Path<String>| async move {
                    let body = if bid == "201" {
                        json_detail.clone()
                    } else {
                        json_detail_empty.clone()
                    };
                    axum::http::Response::builder()
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body))
                        .unwrap()
                },
            ),
        )
        .route(
            "/japi/toc/201",
            get(move || async move {
                axum::http::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(json_toc))
                    .unwrap()
            }),
        )
        .route(
            "/japi/chapter/201/:n",
            get(move || async move {
                axum::http::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(json_chapter.clone()))
                    .unwrap()
            }),
        );

    let server = axum::serve(listener, app);
    let addr = server.local_addr().expect("mock 站点启动失败");
    tokio::spawn(async move { server.await });
    addr.port()
}

fn css_source(port: u16) -> BookSource {
    let base = format!("http://127.0.0.1:{port}");
    let json_src = json!({
        "bookSourceUrl": format!("{base}/css"),
        "bookSourceName": "CSS 测试源",
        "enabled": true,
        "searchUrl": format!("{base}/search?q={{{{key}}}}"),
        "ruleSearch": {
            "bookList": "ul.result-list@li.item",
            "name": "a.title-link@text",
            "bookUrl": "a.title-link@href",
            "author": "span.author@text",
            "kind": "span.kind@text",
            "coverUrl": "img.cover@src",
            "intro": "p.intro@text",
            "lastChapter": "span.latest@text"
        },
        "ruleBookInfo": {
            "name": "h1.book-title@text",
            "author": "span.book-author@text",
            "intro": "div.book-intro@text",
            "coverUrl": "img.book-cover@src",
            "tocUrl": "a.toc-entry@href"
        },
        "ruleToc": {
            "chapterList": "dl.chapter-list@dd",
            "chapterName": "a@text",
            "chapterUrl": "a@href",
            "nextTocUrl": "a.next-page@href"
        },
        "ruleContent": {
            "content": "#content@p@text",
            "nextContentUrl": "a.next-content@href"
        }
    });
    serde_json::from_value(json_src).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_css_source_full_pipeline() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-css").await;
    let ns = "default";

    // ---- 1) 搜索 ----
    let source = css_source(port);
    let results = search::search_one_source(&storage, ns, &source, "测试", 1, 15)
        .await
        .expect("搜索失败");
    assert_eq!(results.len(), 2, "应命中两条搜索结果");
    assert_eq!(results[0].name, "测试之书");
    assert_eq!(results[0].author, "张三");
    assert!(
        results[0].book_url.contains("/book/101.html"),
        "bookUrl 应为相对链接补全后的绝对地址: {}",
        results[0].book_url
    );
    assert_eq!(
        results[0].latest_chapter_title.as_deref(),
        Some("第100章 大结局")
    );

    // ---- 2) 详情 ----
    let book_url = results[0].book_url.clone();
    let info = book::fetch_book_info(ns, &book_url, &source, None)
        .await
        .expect("详情失败");
    assert_eq!(info.name, "测试之书");
    assert_eq!(info.author, "张三");
    assert!(
        info.intro.unwrap_or_default().contains("精彩的冒险"),
        "详情简介应来自详情页规则"
    );
    let toc_url = info.toc_url.clone().expect("详情应解析出 tocUrl");
    assert!(
        toc_url.contains("/toc/101/page-1.html"),
        "tocUrl: {toc_url}"
    );

    // ---- 3) 目录（含翻页）----
    let chapters = book::analyze_toc(ns, &toc_url, &source, 10, None, &book_url)
        .await
        .expect("目录失败");
    assert_eq!(
        chapters.len(),
        5,
        "目录翻页后共 5 章，实际 {}",
        chapters.len()
    );
    assert_eq!(chapters[0].title, "第一章 起点");
    assert_eq!(chapters[4].title, "第五章 结局(下)");
    assert!(
        chapters[0].url.contains("/chapter/101-1.html"),
        "章节 URL 补全"
    );

    // ---- 4) 正文（含 nextContentUrl 翻页合并）----
    let content = book::analyze_content(ns, &chapters[0].url, &source, 5, None, None, &book_url)
        .await
        .expect("正文失败");
    assert!(
        content.contains("第1章第一页内容") && content.contains("第1章第二页内容"),
        "正文应合并翻页两页内容，实际: {content}"
    );
    assert!(
        !content.contains("<p>") && !content.contains("</html>"),
        "正文应为净化纯文本"
    );

    storage.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_json_source_full_pipeline() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-json").await;
    let ns = "default";
    let base = format!("http://127.0.0.1:{port}");

    let json_src = json!({
        "bookSourceUrl": format!("{base}/jsonapi"),
        "bookSourceName": "JSON 测试源",
        "enabled": true,
        "searchUrl": format!("{base}/japi/search?key={{{{key}}}}"),
        "ruleSearch": {
            "checkKeyWord": "接口",
            "bookList": "$.data.list[*]",
            "name": "$.bname",
            "bookUrl": "$.bid",
            "author": "$.author",
            "kind": "$.cat",
            "coverUrl": "$.cover",
            "intro": "$.desc"
        },
        "ruleBookInfo": {
            "init": "$.info",
            "name": "$.bname",
            "author": "$.author",
            "intro": "$.intro",
            "coverUrl": "$.cover",
            "tocUrl": format!("{base}/japi/toc/201")
        },
        "ruleToc": {
            "chapterList": "$.chapters[*]",
            "chapterName": "$.cname",
            "chapterUrl": "$.curl"
        },
        "ruleContent": {
            "content": "$.content"
        }
    });
    let source: BookSource = serde_json::from_value(json_src).unwrap();

    // 搜索
    let results = search::search_one_source(&storage, ns, &source, "接口", 1, 15)
        .await
        .expect("JSON 搜索失败");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].name, "接口之书");

    // 详情（tocUrl 为固定地址）
    let info = book::fetch_book_info(ns, &results[0].book_url, &source, None)
        .await
        .expect("JSON 详情失败");
    assert_eq!(info.name, "接口之书");
    let toc_url = info.toc_url.expect("JSON 源 tocUrl");
    assert!(toc_url.contains("/japi/toc/201"));

    // 目录
    let chapters = book::analyze_toc(ns, &toc_url, &source, 5, None, &results[0].book_url)
        .await
        .expect("JSON 目录失败");
    assert_eq!(chapters.len(), 4);
    assert_eq!(chapters[0].title, "接口第1章");

    // 正文
    let content = book::analyze_content(
        ns,
        &chapters[0].url,
        &source,
        3,
        None,
        None,
        &results[0].book_url,
    )
    .await
    .expect("JSON 正文失败");
    assert!(
        content.contains("接口正文第一段") && content.contains("接口正文第二段"),
        "JSON 数组正文应拼接: {content}"
    );

    storage.pool.close().await;
}

/// 临时目录存储（与单元测试同模式，避免污染真实数据）
async fn test_storage(tag: &str) -> Storage {
    let dir = std::env::temp_dir().join(format!("reader-e2e-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = reader_dev::AppConfig::from_env();
    config.work_dir = dir.to_string_lossy().into_owned();
    reader_dev::storage::init(&config).await.unwrap()
}
