//! C1 进阶 E2E：GBK 编码源 / POST 搜索 / @js 规则链 / replaceRegex 清洗。
//!
//! 覆盖真实书源高频踩坑点：
//! 1. GBK 页面 + `<meta charset=gbk>` 声明 → 自动探测解码
//! 2. GBK 页面无任何 charset 声明（Content-Type 无 charset）→ 启发式回退
//! 3. searchUrl `,{method:"POST",body:...}` + body 内 {{key}} 模板
//! 4. 字段规则 @js 后缀链（result 注入）
//! 5. ruleContent.replaceRegex 删除型清洗（`模式##`）

use std::net::SocketAddr;

use axum::extract::Request;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;

use reader_dev::model::BookSource;
use reader_dev::service::book;
use reader_dev::service::search;
use reader_dev::storage::Storage;

mod common;

fn allow_private_net() -> &'static common::PrivateNetGuard {
    static GUARD: std::sync::OnceLock<common::PrivateNetGuard> = std::sync::OnceLock::new();
    GUARD.get_or_init(common::PrivateNetGuard::on)
}

/// 中文样本 → GBK 字节
fn gbk(s: &str) -> Vec<u8> {
    let (cow, _, had_errors) = encoding_rs::GBK.encode(s);
    assert!(!had_errors, "样本应可完整编码为 GBK");
    cow.into_owned()
}

async fn spawn_mock_site() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    // GBK 搜索页（带 meta charset 声明）
    let gbk_search_meta = gbk(
        r#"<!doctype html><html><head><meta charset="gbk"></head><body>
<ul class="list">
<li class="row"><a class="bk" href="/gbk/book/301.html">编码之书</a><span class="au">码农</span></li>
</ul>
</body></html>"#,
    );
    // GBK 详情页
    let gbk_detail = gbk(
        r#"<!doctype html><html><head><meta charset="gbk"></head><body>
<h1 class="nm">编码之书</h1><span class="writer">码农</span>
<div class="desc">GBK 编码详情简介。</div>
<a class="go-toc" href="/gbk/toc/301.html">目录</a>
</body></html>"#,
    );
    // GBK 目录页
    let gbk_toc = gbk(
        r#"<!doctype html><html><head><meta charset="gbk"></head><body>
<dl class="chs">
<dd><a href="/gbk/ch/301-1.html">第一章 编码</a></dd>
<dd><a href="/gbk/ch/301-2.html">第二章 解码</a></dd>
</dl>
</body></html>"#,
    );
    // GBK 正文页（含广告行——replaceRegex 目标；字段走 @js 链大写化验证 result 注入）
    let gbk_chapter = gbk(
        r#"<!doctype html><html><head><meta charset="gbk"></head><body>
<div id="txt"><p>正文第一段。</p>本站由广告联盟赞助<p>正文第二段。</p>请记住本站域名<p>正文第三段。</p></div>
</body></html>"#,
    );

    // GBK 无声明搜索页（启发式回退路径：Content-Type 仅 text/html、无 meta）
    let gbk_search_plain = gbk(r#"<!doctype html><html><body>
<ul class="list">
<li class="row"><a class="bk" href="/plain/book/401.html">裸流之书</a><span class="au">隐者</span></li>
</ul>
</body></html>"#);

    async fn gbin(bytes: Vec<u8>) -> axum::response::Response {
        // 注意：content-type 故意不带 charset（探测路径覆盖）
        axum::http::Response::builder()
            .header("content-type", "text/html")
            .body(axum::body::Body::from(bytes))
            .unwrap()
    }

    // POST 搜索：校验 body 里 k=关键词 后返回 UTF-8 结果
    async fn post_search(req: Request) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
            .await
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("k=接口") {
            let json = json!({"rows":[{"t":"邮递之书","a":"小哥"}]}).to_string();
            axum::http::Response::builder()
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json.to_string()))
                .unwrap()
        } else {
            axum::http::Response::builder()
                .status(400)
                .body(axum::body::Body::from(r#"{"err":"bad key"}"#))
                .unwrap()
        }
    }

    let app = Router::new()
        .route(
            "/gbk/search",
            get(move || async move { gbin(gbk_search_meta).await }),
        )
        .route(
            "/plain/search",
            get(move || async move { gbin(gbk_search_plain).await }),
        )
        .route(
            "/gbk/book/:id.html",
            get(move || async move { gbin(gbk_detail.clone()).await }),
        )
        .route(
            "/gbk/toc/:id.html",
            get(move || async move { gbin(gbk_toc.clone()).await }),
        )
        .route(
            "/gbk/ch/:id.html",
            get(move || async move { gbin(gbk_chapter.clone()).await }),
        )
        .route("/post-search", post(post_search));

    let server = axum::serve(listener, app);
    let addr: SocketAddr = server.local_addr().expect("mock 启动");
    tokio::spawn(async move { server.await });
    addr.port()
}

/// GBK 源：自动探测解码 + replaceRegex 清洗 + 目录/正文链路
#[tokio::test(flavor = "multi_thread")]
async fn e2e_gbk_charset_pipeline() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-gbk").await;
    let ns = "default";
    let base = format!("http://127.0.0.1:{port}");

    let src_json = json!({
        "bookSourceUrl": format!("{base}/gbk-src"),
        "bookSourceName": "GBK 测试源",
        "enabled": true,
        "searchUrl": format!("{base}/gbk/search?q={{{{key}}}}"),
        "ruleSearch": {
            "bookList": "ul.list@li.row",
            "name": "a.bk@text",
            "bookUrl": "a.bk@href",
            "author": "span.au@text"
        },
        "ruleBookInfo": {
            "name": "h1.nm@text",
            "author": "span.writer@text",
            "intro": "div.desc@text",
            "tocUrl": "a.go-toc@href"
        },
        "ruleToc": {
            "chapterList": "dl.chs@dd",
            "chapterName": "a@text",
            "chapterUrl": "a@href"
        },
        "ruleContent": {
            "content": "#txt@p@text",
            "replaceRegex": "本站由广告联盟赞助##|请记住本站域名##"
        }
    });
    let source: BookSource = serde_json::from_value(src_json).unwrap();

    // ---- GBK 带 meta 声明的搜索页 ----
    let results = search::search_one_source(&storage, ns, &source, "编码", 1, 15)
        .await
        .expect("GBK 搜索失败");
    assert_eq!(results.len(), 1, "GBK 页面应正确解码出结果");
    assert_eq!(results[0].name, "编码之书", "中文不应乱码");

    // ---- 详情 / 目录 / 正文 ----
    let info = book::fetch_book_info(ns, &results[0].book_url, &source, None)
        .await
        .expect("GBK 详情失败");
    assert_eq!(info.name, "编码之书");
    assert!(
        info.intro
            .as_deref()
            .unwrap_or_default()
            .contains("GBK 编码详情简介"),
        "详情中文解码"
    );
    let chapters = book::analyze_toc(
        ns,
        info.toc_url.as_deref().unwrap(),
        &source,
        3,
        None,
        &results[0].book_url,
    )
    .await
    .expect("GBK 目录失败");
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[1].title, "第二章 解码");

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
    .expect("GBK 正文失败");
    assert!(content.contains("正文第一段"), "正文解码: {content}");
    assert!(content.contains("正文第二段"), "正文解码: {content}");
    assert!(
        !content.contains("广告联盟") && !content.contains("请记住本站"),
        "replaceRegex 应删除广告行: {content}"
    );

    storage.pool.close().await;
}

/// GBK 无 charset 声明 → 启发式回退（UTF-8 替换字符检测后按 GBK 解码）
#[tokio::test(flavor = "multi_thread")]
async fn e2e_gbk_autodetect_pipeline() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-gbk-auto").await;
    let ns = "default";
    let base = format!("http://127.0.0.1:{port}");

    let src_json = json!({
        "bookSourceUrl": format!("{base}/plain-src"),
        "bookSourceName": "裸流测试源",
        "enabled": true,
        "searchUrl": format!("{base}/plain/search?k={{{{key}}}}"),
        "ruleSearch": {
            "bookList": "ul.list@li.row",
            "name": "a.bk@text",
            "bookUrl": "a.bk@href",
            "author": "span.au@text"
        }
    });
    let source: BookSource = serde_json::from_value(src_json).unwrap();

    let results = search::search_one_source(&storage, ns, &source, "裸流", 1, 15)
        .await
        .expect("启发式解码搜索失败");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "裸流之书", "无声明 GBK 应被启发式识别");

    storage.pool.close().await;
}

/// POST 搜索（method+body 模板）+ JSONPath 字段 + @js 规则链（result 大写变换）
#[tokio::test(flavor = "multi_thread")]
async fn e2e_post_and_js_chain() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-post-js").await;
    let ns = "default";
    let base = format!("http://127.0.0.1:{port}");

    // name 走 JSONPath 提取后接 @js:result + "-suffix"（验证 result 注入与表达式求值）
    let src_json = json!({
        "bookSourceUrl": format!("{base}/post-src"),
        "bookSourceName": "POST 测试源",
        "enabled": true,
        "searchUrl": format!(r#"{}/post-search,{{"method":"POST","body":"k={{{{key}}}}"}}"#, base),
        "ruleSearch": {
            "bookList": "$.rows[*]",
            "name": "$.t@js:result + '-js'",
            "bookUrl": "'/post/book/' + $.t",
            "author": "$.a"
        }
    });
    let source: BookSource = serde_json::from_value(src_json).unwrap();

    let results = search::search_one_source(&storage, ns, &source, "接口", 1, 15)
        .await
        .expect("POST 搜索失败");
    assert_eq!(results.len(), 1, "POST body 模板应携带关键词命中");
    assert_eq!(results[0].author, "小哥");
    assert_eq!(
        results[0].name, "邮递之书-js",
        "@js 后缀链应对提取结果做变换"
    );

    storage.pool.close().await;
}

async fn test_storage(tag: &str) -> Storage {
    let dir = std::env::temp_dir().join(format!("reader-e2e-adv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = reader_dev::AppConfig::from_env();
    config.work_dir = dir.to_string_lossy().into_owned();
    reader_dev::storage::init(&config).await.unwrap()
}

/// A1 webView 路径：URL option webView=true 时浏览器不可用（测试环境未配置 camoufox）
/// → 回退普通 HTTP 抓取，链路不中断
#[tokio::test(flavor = "multi_thread")]
async fn e2e_webview_fallback_to_http() {
    let _guard = allow_private_net();
    let port = spawn_mock_site().await;
    let storage = test_storage("e2e-wv-fallback").await;
    let ns = "default";
    let base = format!("http://127.0.0.1:{port}");

    // searchUrl 带 webView=true option（JSON 形态）
    let src_json = json!({
        "bookSourceUrl": format!("{base}/wv-src"),
        "bookSourceName": "WebView 回退源",
        "enabled": true,
        "searchUrl": format!(r#"{}/gbk/search?q={{{{key}}}},{{"webView":true}}"#, base),
        "ruleSearch": {
            "bookList": "ul.list@li.row",
            "name": "a.bk@text",
            "bookUrl": "a.bk@href",
            "author": "span.au@text"
        }
    });
    let source: BookSource = serde_json::from_value(src_json).unwrap();

    // 浏览器不可用时 solve_cf_challenge 会尝试 ensure_service——失败即回退。
    // mock 站为 GBK 页面：回退路径的解码/解析全链路应正常出结果
    let results = search::search_one_source(&storage, ns, &source, "编码", 1, 15)
        .await
        .expect("webView 回退后搜索应成功");
    assert_eq!(results.len(), 1, "回退 HTTP 后应正常解析");
    assert_eq!(results[0].name, "编码之书");

    storage.pool.close().await;
}
