//! 书籍链路单元测试（详情 / 目录 / 正文 / 媒体 / 翻页 / 变量贯通）

use crate::model::book_chapter::BookChapter;

use super::media::collect_urls;
use super::toc::{
    chapters_from_items, clamp_toc_concurrency, dedupe_chapters, next_toc_urls,
    set_toc_budget_secs, set_toc_concurrency, should_retry_toc_with_book_page, split_url_list,
};
use super::*;

/// legado init 语义：先提取上下文（JSONPath/CSS/JS），字段规则相对应用
/// fetch_url 的 legado AnalyzeUrl 后缀支持：js 修改 URL / headers / bodyJs / POST
mod fetch_url_suffix_tests {
    use super::super::*;
    use crate::model::book_source::BookSource;
    use crate::service::crawler::ssrf_allow_private_guard;

    /// 简单 mock：记录请求头，返回固定 body（或按 path 路由）
    async fn mock() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let req = String::from_utf8_lossy(&buf);
                    let path = req
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split(' ')
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    let has_x = req.to_lowercase().contains("x-test");
                    let body = if has_x {
                        format!("BODY-FOR-{path}-WITH-HEADER")
                    } else {
                        format!("BODY-FOR-{path}")
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body,
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)] // 先 Default 再按条件覆盖字段，比一次性构造更贴近可读的分支逻辑
    async fn fetch_url_js_and_headers_suffix() {
        let _ssrf = ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1
        let base = mock().await;
        let mut source = BookSource::default();
        source.book_source_url = format!("{base}/src");
        // js 键返回完整新 URL（eval 注入 baseUrl/key/page/headerMap/result——无 url 变量）
        let url =
            format!("{base}/orig,{{\"js\":\"'{base}/new'\",\"headers\":{{\"X-Test\":\"1\"}}}}");
        let resp = fetch_url("default", &url, &source).await.unwrap();
        assert!(
            resp.body.contains("/new") && resp.body.contains("WITH-HEADER"),
            "js 修改 URL + headers 应生效: {}",
            resp.body
        );
    }

    #[tokio::test]
    async fn fetch_url_body_js() {
        let _ssrf = ssrf_allow_private_guard(true);
        let base = mock().await;
        let source = BookSource::default();
        let url = format!("{base}/x,{{\"bodyJs\":\"result.replace('BODY','TEXT')\"}}");
        let resp = fetch_url("default", &url, &source).await.unwrap();
        assert!(
            resp.body.contains("TEXT-FOR"),
            "bodyJs 应改写响应体: {}",
            resp.body
        );
    }

    #[tokio::test]
    async fn fetch_url_plain_no_suffix() {
        let _ssrf = ssrf_allow_private_guard(true);
        let base = mock().await;
        let source = BookSource::default();
        let resp = fetch_url("default", &format!("{base}/plain"), &source)
            .await
            .unwrap();
        assert_eq!(resp.body, "BODY-FOR-/plain");
    }
}

mod init_rule_tests {
    use super::super::*;
    use crate::model::book_source::BookSource;

    #[allow(clippy::field_reassign_with_default)] // 先 Default 再按条件覆盖字段，比一次性构造更贴近可读的分支逻辑
    fn src_with(book_info: serde_json::Value) -> BookSource {
        let mut s = BookSource::default();
        s.rule_book_info = Some(book_info);
        s
    }

    #[test]
    fn dbg_rule_deser() {
        let v = serde_json::json!({"init": "$.data", "name": "$.novelName", "author": "$.author", "tocUrl": "/novel/{{$.novelId}}/chapters"});
        let r: BookInfoRule = serde_json::from_value(v).unwrap();
        eprintln!("init={:?} name={:?} toc={:?}", r.init, r.name, r.toc_url);
    }

    #[test]
    fn dbg_apply_init() {
        let html = r#"{"code":0,"data":{"novelId":"bY7oM0","novelName":"诡秘之主","author":"爱潜水的乌贼"}}"#;
        let out = crate::parser::rule::apply_init(html, Some("$.data"));
        eprintln!("apply_init($.data) = {out}");
        let v = crate::parser::rule::apply("$.novelName", &out);
        eprintln!("apply($.novelName) = {v:?}");
    }

    #[test]
    fn book_info_init_jsonpath_context() {
        // 猫眼类 JSON API：init=$.data → name/author 在 data 子对象上
        let source = src_with(serde_json::json!({
            "init": "$.data",
            "name": "$.novelName",
            "author": "$.author",
            "tocUrl": "/novel/{{$.novelId}}/chapters"
        }));
        let html = r#"{"code":0,"data":{"novelId":"bY7oM0","novelName":"诡秘之主","author":"爱潜水的乌贼"}}"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://api.jmlldsc.com/novel/bY7oM0?isSearch=1",
            &source,
            "http://api.jmlldsc.com/novel/bY7oM0?isSearch=1",
            None,
        );
        assert_eq!(
            info.name, "诡秘之主",
            "init 后 name 应相对 data 提取: {:?}",
            info.name
        );
        assert_eq!(info.author, "爱潜水的乌贼");
        assert!(
            info.toc_url
                .as_deref()
                .unwrap_or("")
                .ends_with("/novel/bY7oM0/chapters"),
            "tocUrl {{}} 内嵌应展开: {:?}",
            info.toc_url
        );
    }

    #[test]
    fn book_info_init_absent_uses_raw() {
        let source = src_with(serde_json::json!({
            "name": "class.title@text",
            "author": "class.author@text"
        }));
        let html = r#"<html><body><div class="title">书名A</div><div class="author">作者A</div></body></html>"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://x.com",
            &source,
            "http://x.com/b",
            None,
        );
        assert_eq!(info.name, "书名A");
        assert_eq!(info.author, "作者A");
    }

    #[test]
    fn book_info_init_js() {
        let source = src_with(serde_json::json!({
            "init": "@js:JSON.parse(result).data",
            "name": "$.novelName"
        }));
        let html = r#"{"data":{"novelName":"JS书名"}}"#;
        let info = analyze_book_info(
            "default",
            html,
            "http://x.com",
            &source,
            "http://x.com/b",
            None,
        );
        assert_eq!(
            info.name, "JS书名",
            "JS init 后 JSONPath 相对提取: {:?}",
            info.name
        );
    }
}

fn test_source() -> BookSource {
    BookSource {
        book_source_url: "http://127.0.0.1:9999".into(),
        book_source_name: "测试源".into(),
        rule_book_info: Some(serde_json::json!({
            "name": "h1.bookname@text", "author": "p.author@text",
            "intro": "div.intro@text", "coverUrl": "img.cover@src",
            "tocUrl": "/toc"
        })),
        rule_toc: Some(serde_json::json!({
            "chapterList": "ul.chapters@li",
            "chapterName": "a@text", "chapterUrl": "a@href"
        })),
        rule_content: Some(serde_json::json!({
            "content": "div.content@text"
        })),
        ..Default::default()
    }
}

#[test]
fn test_analyze_info() {
    let html = r#"<h1 class="bookname">测试书</h1><p class="author">作者X</p>
        <div class="intro">简介内容</div><img class="cover" src="/cover.jpg">"#;
    let info = analyze_book_info(
        "default",
        html,
        "http://127.0.0.1:9999/book/1",
        &test_source(),
        "http://127.0.0.1:9999/book/1",
        None,
    );
    assert_eq!(info.name, "测试书");
    assert_eq!(info.author, "作者X");
    assert_eq!(info.intro.as_deref(), Some("简介内容"));
    assert_eq!(
        info.cover_url.as_deref(),
        Some("http://127.0.0.1:9999/cover.jpg"),
        "详情封面应转绝对 URL（否则书架显示首字封面）"
    );
    assert_eq!(info.toc_url.as_deref(), Some("http://127.0.0.1:9999/toc"));
}

/// legacy BookInfo：tocUrl 规则缺失/为空时回退 baseUrl（详情页即目录页）
#[test]
fn test_analyze_info_toc_fallback_base() {
    let mut src = test_source();
    src.rule_book_info = Some(serde_json::json!({
        "name": "h1.bookname@text",
        "author": "p.author@text"
    }));
    let base = "http://127.0.0.1:9999/book/1";
    let html = r#"<h1 class="bookname">测试书</h1><p class="author">作者X</p>"#;
    let info = analyze_book_info("default", html, base, &src, base, None);
    assert_eq!(info.toc_url.as_deref(), Some(base), "tocUrl 应回退 baseUrl");
}

/// legacy canReName：书源规则未声明 canReName 时保留书架已有书名/作者
#[test]
fn test_can_rename_only_with_rule() {
    let mut src = test_source();
    src.rule_book_info = Some(serde_json::json!({
        "name": "h1.bookname@text",
        "author": "p.author@text"
    }));
    let base = "http://127.0.0.1:9999/book/1";
    let html = r#"<h1 class="bookname">规则新名</h1><p class="author">规则作者</p>"#;
    let info = analyze_book_info_with_existing(html, base, &src, base, "书架旧名", "书架作者");
    assert_eq!(info.name, "书架旧名", "无 canReName 不应覆盖书架书名");
    assert_eq!(info.author, "书架作者", "无 canReName 不应覆盖书架作者");

    // canReName 规则非空 → 允许覆盖
    src.rule_book_info = Some(serde_json::json!({
        "name": "h1.bookname@text",
        "author": "p.author@text",
        "canReName": "true"
    }));
    let info = analyze_book_info_with_existing(html, base, &src, base, "书架旧名", "书架作者");
    assert_eq!(info.name, "规则新名");
    assert_eq!(info.author, "规则作者");
}

/// legacy BookChapter.tag：目录规则 updateTime 解析结果写入章节附加信息
#[test]
fn test_chapter_tag_from_update_time() {
    let rule = TocRule {
        chapter_list: Some("@js:[{t:'章A',u:'/x/1',time:'2026-08-08'}]".into()),
        chapter_name: Some("$.t".into()),
        chapter_url: Some("$.u".into()),
        update_time: Some("$.time".into()),
        ..Default::default()
    };
    let items = toc_items(rule.chapter_list.as_deref().unwrap(), "{}");
    assert_eq!(items.len(), 1);
    let mut vars = crate::parser::rule::RuleVars::new();
    let chapters = chapters_from_items(&items, &rule, "https://src.test", 0, &mut vars);
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].tag.as_deref(), Some("2026-08-08"));
}

/// 链表分页串行抓的总预算: 每页 2s 的双页源 + 预算 1s → 第二页前报错, 不再百秒无响应
#[tokio::test]
async fn test_toc_budget_caps_serial_pagination() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let mut req = String::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if req.contains("\r\n\r\n") {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.starts_with("/toc2") {
                    r#"{"ch":[{"t":"第二章","u":"/c/2"}]}"#
                } else {
                    r#"{"ch":[{"t":"第一章","u":"/c/1"}],"next":"/toc2"}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    let mut src = test_source();
    src.book_source_url = format!("http://{addr}/src");
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "@js:$.ch",
        "chapterName": "$.t",
        "chapterUrl": "$.u",
        "nextTocUrl": "$.next"
    }));
    set_toc_budget_secs(1);
    let t0 = std::time::Instant::now();
    let res = analyze_toc("default", &format!("http://{addr}/toc"), &src, 20, None, "").await;
    let elapsed = t0.elapsed();
    set_toc_budget_secs(0);
    let err = res.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        err.contains("预算") && elapsed < std::time::Duration::from_secs(10),
        "应报预算超时且快: err={err} elapsed={elapsed:?}"
    );
}

/// 多页目录 mock：按路径前缀返回 (body, 延迟ms)；`flaky` 路径首次请求直接断连（模拟偶发失败）
async fn serve_toc_pages(
    pages: Vec<(&'static str, &'static str, u64)>,
    flaky: Option<&'static str>,
) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let pages = pages.clone();
            let hits = hits.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let mut req = String::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if req.contains("\r\n\r\n") {
                        break;
                    }
                }
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let Some((_, body, delay)) =
                    pages.iter().find(|(p, _, _)| path.starts_with(*p)).copied()
                else {
                    let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                };
                if Some(path.as_str()) == flaky && hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    // 首次请求直接断开（无响应体）→ 抓取侧得到连接错误
                    drop(sock);
                    return;
                }
                if delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// 并发上限夹取：0/超大值夹回 1..=8，默认 4
#[test]
fn test_toc_concurrency_clamp() {
    assert_eq!(clamp_toc_concurrency(0), 1);
    assert_eq!(clamp_toc_concurrency(1), 1);
    assert_eq!(clamp_toc_concurrency(4), 4);
    assert_eq!(clamp_toc_concurrency(8), 8);
    assert_eq!(clamp_toc_concurrency(99), 8);
}

/// 有界并发抓页必须保序：中间页故意慢，合并结果仍按页序 → 页内序
#[tokio::test]
async fn test_toc_bounded_concurrency_preserves_page_order() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve_toc_pages(
        vec![
            (
                "/p1",
                r#"{"ch":[{"t":"A1","u":"/a1"},{"t":"A2","u":"/a2"}],"next":"/p2"}"#,
                0,
            ),
            ("/p2", r#"{"ch":[{"t":"B1","u":"/b1"}],"next":"/p3"}"#, 300),
            ("/p3", r#"{"ch":[{"t":"C1","u":"/c1"}]}"#, 0),
        ],
        None,
    )
    .await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "$.ch",
        "chapterName": "$.t",
        "chapterUrl": "$.u",
        "nextTocUrl": "$.next"
    }));
    for concurrency in [1usize, 4] {
        set_toc_concurrency(concurrency);
        let chapters = analyze_toc("default", &format!("{base}/p1"), &src, 20, None, "")
            .await
            .unwrap();
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["A1", "A2", "B1", "C1"],
            "并发={concurrency} 时必须按页序合并（实际 {titles:?}）"
        );
        let indexes: Vec<i64> = chapters.iter().map(|c| c.index).collect();
        assert_eq!(indexes, vec![0, 1, 2, 3], "章节 index 需按合并序重排");
    }
    set_toc_concurrency(0);
}

/// 预算到点用已抓到的页产出部分目录（不再整体失败）
#[tokio::test]
async fn test_toc_budget_returns_partial_directory() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve_toc_pages(
        vec![
            ("/p1", r#"{"ch":[{"t":"A1","u":"/a1"}],"next":"/slow"}"#, 0),
            ("/slow", r#"{"ch":[{"t":"Z1","u":"/z1"}]}"#, 3000),
        ],
        None,
    )
    .await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "$.ch",
        "chapterName": "$.t",
        "chapterUrl": "$.u",
        "nextTocUrl": "$.next"
    }));
    set_toc_budget_secs(1);
    set_toc_concurrency(1);
    let t0 = std::time::Instant::now();
    let res = analyze_toc("default", &format!("{base}/p1"), &src, 20, None, "").await;
    let elapsed = t0.elapsed();
    set_toc_budget_secs(0);
    set_toc_concurrency(0);
    let chapters = res.expect("预算到点应返回部分目录而不是失败");
    assert_eq!(chapters.len(), 1, "只应拿到第一页的章节");
    assert_eq!(chapters[0].title, "A1");
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "预算到点后应立即收尾: {elapsed:?}"
    );
}

/// 单页偶发失败重试 1 次：首次断连，重试成功后目录完整
#[tokio::test]
async fn test_toc_page_retries_once_on_transient_failure() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve_toc_pages(
        vec![
            ("/p1", r#"{"ch":[{"t":"A1","u":"/a1"}],"next":"/flaky"}"#, 0),
            ("/flaky", r#"{"ch":[{"t":"B1","u":"/b1"}]}"#, 0),
        ],
        Some("/flaky"),
    )
    .await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "$.ch",
        "chapterName": "$.t",
        "chapterUrl": "$.u",
        "nextTocUrl": "$.next"
    }));
    let chapters = analyze_toc("default", &format!("{base}/p1"), &src, 20, None, "")
        .await
        .unwrap();
    let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, vec!["A1", "B1"], "重试成功后第二页应并入");
}

/// 某页重试后仍失败：停止翻页，返回已抓到的部分目录（不整体失败、不无限重试）
#[tokio::test]
async fn test_toc_stops_after_persistent_page_failure() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve_toc_pages(
        vec![("/p1", r#"{"ch":[{"t":"A1","u":"/a1"}],"next":"/dead"}"#, 0)],
        // /dead 一直断连（mock 里未知路径返回 404，这里靠 next 指向不存在路径）
        None,
    )
    .await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "$.ch",
        "chapterName": "$.t",
        "chapterUrl": "$.u",
        "nextTocUrl": "$.next"
    }));
    let t0 = std::time::Instant::now();
    let chapters = analyze_toc("default", &format!("{base}/p1"), &src, 20, None, "")
        .await
        .expect("第二页失败应返回已抓到的部分目录");
    assert_eq!(chapters.len(), 1, "只保留第一页章节");
    assert!(t0.elapsed() < std::time::Duration::from_secs(5));
}

/// 详情 @put → 目录 @get：书级变量跨 getBookInfo/getChapterList 贯通
#[tokio::test]
async fn test_put_get_flows_into_toc() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve(r#"{"data":[{"t":"第一章","u":"/c/1"}]}"#).await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.rule_book_info = Some(serde_json::json!({
        "init": "@put:{bid:$.book_id}",
        "name": "$.name",
        "coverUrl": "$.cover",
        "tocUrl": format!("{base}/toc")
    }));
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "$.data",
        "chapterName": "$.t",
        "chapterUrl": "https://x.test/c/@get:{bid}{{$.u}}"
    }));
    let book_url = format!("{base}/book/1");
    let html = r#"{"book_id":"abc","name":"书","cover":"/c.jpg"}"#;
    let info = analyze_book_info("default", html, &book_url, &src, &book_url, None);
    let expected_cover = format!("{base}/c.jpg");
    assert_eq!(info.name, "书");
    assert_eq!(info.cover_url.as_deref(), Some(expected_cover.as_str()));
    let toc_url = info.toc_url.clone().unwrap();
    let chapters = analyze_toc("default", &toc_url, &src, 2, None, &book_url)
        .await
        .unwrap();
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].title, "第一章");
    assert_eq!(
        chapters[0].url, "https://x.test/c/abc/c/1",
        "目录规则中 @get 应取详情 @put 的值: {:?}",
        chapters[0].url
    );
}

// ---------------- P1：@put/@get 变量持久化 + 跨阶段贯通 ----------------

/// 串行化 SQLite 注册类测试（BOOK_VARS_STORAGE 为进程级单例——并发注册会互相覆盖句柄）
static BOOK_VARS_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 独立临时库 + 注册全局持久化句柄；收尾必须调 [`teardown_book_vars_db`]
async fn setup_book_vars_db(tag: &str) -> (crate::storage::Storage, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("reader-p1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = crate::AppConfig::from_env();
    config.work_dir = dir.to_string_lossy().into_owned();
    let storage = crate::storage::init(&config).await.unwrap();
    crate::parser::rule::register_book_vars_storage(storage.clone());
    (storage, dir)
}

fn teardown_book_vars_db(storage: crate::storage::Storage, dir: &std::path::Path) {
    crate::parser::rule::clear_book_vars_storage();
    drop(storage);
    let _ = std::fs::remove_dir_all(dir);
}

/// P1-1 详情 @put → 重启模拟（清内存缓存）→ 不跑目录直接取正文——
/// book_url 键从 SQLite 读穿透回填，正文规则中的 @get:{bid} 仍可命中
#[tokio::test]
#[allow(clippy::await_holding_lock)] // 测试里用全局锁串行化 env/端口，跨 await 持有是刻意的
async fn test_detail_put_survives_restart_into_content() {
    let _guard = BOOK_VARS_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let detail_base = serve(r#"{"book_id":"abc","name":"书"}"#).await;
    let ch_base = serve("BID=abc&TAIL").await;

    let ns = format!("p1dc-{}", uuid::Uuid::new_v4());
    let (storage, dir) = setup_book_vars_db("dc").await;
    let mut src = test_source();
    src.book_source_url = format!("{detail_base}/src");
    src.rule_book_info = Some(serde_json::json!({
        "init": "@put:{bid:$.book_id}",
        "name": "$.name"
    }));
    src.rule_content = Some(serde_json::json!({ "content": "^BID=@get:{bid}&(.+)$" }));

    // ① 详情阶段：@put 写入（内存 + SQLite 同步双写）
    let book_url = format!("{detail_base}/book/1");
    analyze_book_info(
        &ns,
        r#"{"book_id":"abc","name":"书"}"#,
        &book_url,
        &src,
        &book_url,
        None,
    );
    let row = storage
        .get_book_vars_cache(&ns, &src.book_source_url, &book_url)
        .await
        .unwrap();
    assert!(
        row.as_deref().map(|j| j.contains("abc")).unwrap_or(false),
        "详情 @put 应已同步落库: {row:?}"
    );

    // ② 重启模拟：清内存缓存（SQLite 保留）
    crate::parser::rule::clear_book_vars_memory_cache_ns(&ns);

    // ③ 不跑目录直接取正文：book_url 级读穿透回填 → @get:{bid} 命中
    let chapter_url = format!("{ch_base}/c/1");
    let content = analyze_content(&ns, &chapter_url, &src, 1, None, None, &book_url)
        .await
        .unwrap();
    assert_eq!(
        content, "TAIL",
        "重启后直接取正文应能读到详情阶段 @put 的变量"
    );

    teardown_book_vars_db(storage, &dir);
}

/// P1-2 目录阶段变量对正文可见（跨阶段）：目录 init @put 的 tid 逐章落盘；
/// 重启模拟后——① 已知章节按章节键读穿透命中；② 全新章节 URL 无章节键，
/// 经 book 级双键合并回退仍命中
#[tokio::test]
#[allow(clippy::await_holding_lock)] // 测试里用全局锁串行化 env/端口，跨 await 持有是刻意的
async fn test_toc_vars_visible_to_content_after_restart() {
    let _guard = BOOK_VARS_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let toc_base =
        serve(r#"{"toc_id":"77","data":[{"t":"第一章","u":"/c/1"},{"t":"第二章","u":"/c/2"}]}"#)
            .await;
    let ch_base = serve("TID=77&TAIL").await;

    let ns = format!("p1tv-{}", uuid::Uuid::new_v4());
    let (storage, dir) = setup_book_vars_db("tv").await;
    let mut src = test_source();
    src.book_source_url = format!("{toc_base}/src");
    src.rule_toc = Some(serde_json::json!({
        "init": "@put:{tid:$.toc_id}",
        "chapterList": "$.data",
        "chapterName": "$.t",
        "chapterUrl": format!("{ch_base}/c/@get:{{tid}}")
    }));
    src.rule_content = Some(serde_json::json!({ "content": "^TID=@get:{tid}&(.+)$" }));

    // ① 目录阶段：tid 写入并逐章 + 目录级落盘
    let toc_url = format!("{toc_base}/toc");
    let chapters = analyze_toc(&ns, &toc_url, &src, 2, None, "").await.unwrap();
    assert_eq!(chapters.len(), 2);
    assert!(
        chapters[0].url.ends_with("/c/77"),
        "章节 URL 应拼入目录阶段 @get 值: {}",
        chapters[0].url
    );
    let ch_row = storage
        .get_book_vars_cache(&ns, &src.book_source_url, &chapters[0].url)
        .await
        .unwrap();
    assert!(
        ch_row.as_deref().map(|j| j.contains("77")).unwrap_or(false),
        "目录阶段应逐章落库: {ch_row:?}"
    );

    // ② 重启模拟
    crate::parser::rule::clear_book_vars_memory_cache_ns(&ns);

    // ③a 已知章节直接取正文：章节键读穿透回填
    let content = analyze_content(&ns, &chapters[0].url, &src, 1, None, None, "")
        .await
        .unwrap();
    assert_eq!(content, "TAIL", "重启后章节键应从 SQLite 回填目录阶段变量");

    // ③b 全新章节 URL（无章节键）：book 级（tocUrl 键）合并回退命中
    let fresh = format!("{ch_base}/c/fresh");
    let content2 = analyze_content(&ns, &fresh, &src, 1, None, None, &toc_url)
        .await
        .unwrap();
    assert_eq!(
        content2, "TAIL",
        "无章节键时应经 book 级双键合并读到目录阶段变量"
    );

    teardown_book_vars_db(storage, &dir);
}

/// P1 rule-API 级回环：两级保存落库 → 清内存 → load_merged 读穿透重组
#[tokio::test]
#[allow(clippy::await_holding_lock)] // 测试里用全局锁串行化 env/端口，跨 await 持有是刻意的
async fn test_book_vars_sqlite_roundtrip_unit() {
    let _guard = BOOK_VARS_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ns = format!("p1rt-{}", uuid::Uuid::new_v4());
    let (storage, dir) = setup_book_vars_db("rt").await;
    let src_key = "https://src.test/rt";
    let mut vars = crate::parser::rule::RuleVars::new();
    vars.insert("token".to_string(), "T0KEN-9".to_string());
    crate::parser::rule::save_book_vars_two_level(
        &ns,
        src_key,
        "https://b.test/book",
        "https://b.test/c/1",
        &vars,
    );

    let leaf = storage
        .get_book_vars_cache(&ns, src_key, "https://b.test/c/1")
        .await
        .unwrap();
    assert!(leaf.is_some(), "leaf 键应落库");
    let root = storage
        .get_book_vars_cache(&ns, src_key, "https://b.test/book")
        .await
        .unwrap();
    assert!(root.is_some(), "root 键应落库");

    crate::parser::rule::clear_book_vars_memory_cache_ns(&ns);
    let merged = crate::parser::rule::load_book_vars_merged(
        &ns,
        src_key,
        "https://b.test/book",
        "https://b.test/c/1",
    );
    assert_eq!(
        merged.get("token").map(String::as_str),
        Some("T0KEN-9"),
        "清内存后应读穿透 SQLite 回填"
    );
    // 上下文字段不随持久化复活
    assert!(merged.chapter_title.is_none() && merged.book_name.is_none());

    teardown_book_vars_db(storage, &dir);
}

#[test]
fn test_analyze_content_from() {
    let html =
        r#"<html><div class="content">第一章正文内容测试。</div><script>干扰</script></html>"#;
    let content = analyze_content_from(html, &test_source());
    assert_eq!(content, "第一章正文内容测试。");
}

#[test]
fn test_analyze_content_replace() {
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@text",
        "replaceRegex": "\\s+## "
    }));
    let html = r#"<div class="content">多   个  空格</div>"#;
    let content = analyze_content_from(html, &src);
    assert_eq!(content, "多个空格");
}

/// E9：replaceRegex 多段链（&&）+ ### 尾标仅首次替换 + 无 ## 段删除语义
#[test]
fn test_analyze_content_replace_multi_segment() {
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@text",
        // 段1：删除"广告"（无 ## → 删除型）；段2：尾部标记替换为【完】；
        // 段3：o→0 仅首次（### 尾标）
        "replaceRegex": "广告&&尾部##【完】&&o##0###"
    }));
    let html = r#"<div class="content">广告正文oogo尾部</div>"#;
    let content = analyze_content_from(html, &src);
    assert_eq!(
        content, "正文0ogo【完】",
        "段序：删广告→尾部换【完】→首个 o 变 0"
    );
}

/// 真实书源回归：replaceRegex 带前导 `##`（如 `##本章未完`、`##pat##rep`）与 `{{chapter.title}}` 模板展开
#[test]
fn test_analyze_content_replace_leading_hashes_and_inline_vars() {
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@text",
        "replaceRegex": r#"##{{try{chapter.title}catch(e){""}}}|\(第\d+/\d+页\)&&##（本章未完[^）]*）&&##错字##正字"#
    }));
    let html =
        r#"<div class="content">第一章 绯红正文错字内容（本章未完，请点击下一页）(第1/3页)</div>"#;
    let mut vars = crate::parser::rule::RuleVars::new();
    vars.chapter_title = Some("第一章 绯红".to_string());
    let content = analyze_content_from_with_vars(html, &src, &mut vars);
    assert_eq!(content, "正文正字内容");
}

/// chapterList JS 规则（JSON.parse(result).data 数组）→ 章节上下文列表
#[test]
fn test_toc_items_js_array() {
    let body = r#"{"data":[{"title":"第一章","href":"/c/1"},{"title":"第二章","href":"/c/2"}]}"#;
    let items = toc_items("@js:JSON.parse(result).data", body);
    assert_eq!(items.len(), 2, "JS chapterList 应解析出 2 项");
    assert!(items[0].contains("第一章"));
    assert!(items[0].contains("/c/1"));
}

/// chapterList JS 数组字面量 + 字段规则 → 章节（title/url 绝对化/index）
#[test]
fn test_toc_js_full_pipeline() {
    let rule = TocRule {
        chapter_list: Some("@js:[{t:'章A',u:'/x/1'},{t:'章B',u:'/x/2'}]".into()),
        chapter_name: Some("$.t".into()),
        chapter_url: Some("$.u".into()),
        ..Default::default()
    };
    let items = toc_items(rule.chapter_list.as_deref().unwrap(), "{}");
    assert_eq!(items.len(), 2);
    let mut vars = crate::parser::rule::RuleVars::new();
    let chapters = chapters_from_items(&items, &rule, "https://src.test", 5, &mut vars);
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].title, "章A");
    assert_eq!(chapters[0].url, "https://src.test/x/1");
    assert_eq!(chapters[0].index, 5);
    assert_eq!(chapters[1].index, 6);
}

/// <js> 包裹 chapterList 兜底
#[test]
fn test_toc_items_js_html_wrapped() {
    let body = r#"{"data":[{"title":"包章","url":"/b/1"}]}"#;
    let items = toc_items("<js>JSON.parse(result).data</js>", body);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("包章"));
}

/// GAP 17b：ruleRelated CSS 链式解析 → relatedBooks
#[test]
fn test_analyze_related_books() {
    let mut src = test_source();
    src.rule_related = Some(serde_json::json!({
        "bookList": "ul.related@li",
        "name": "a.bookname@text",
        "author": "span.author@text",
        "bookUrl": "a@href",
        "coverUrl": "img@src"
    }));
    let html = r#"<ul class="related">
        <li><a class="bookname" href="/r/1">推荐书1</a><span class="author">作者甲</span><img src="/c1.jpg"></li>
        <li><a class="bookname" href="/r/2">推荐书2</a><span class="author">作者乙</span><img src="/c2.jpg"></li>
    </ul>"#;
    let related = analyze_related_books(html, "http://127.0.0.1:9999/book/1", &src);
    assert_eq!(related.len(), 2);
    assert_eq!(related[0].name, "推荐书1");
    assert_eq!(related[0].author, "作者甲");
    assert_eq!(related[0].book_url, "http://127.0.0.1:9999/r/1");
    // coverUrl 经 field_url 绝对化（与 ruleExplore 书单一致）
    assert_eq!(
        related[0].cover_url.as_deref(),
        Some("http://127.0.0.1:9999/c1.jpg")
    );
    assert_eq!(related[1].name, "推荐书2");
    // 无 ruleRelated / 无 bookList → 空
    assert!(analyze_related_books(html, "http://x", &test_source()).is_empty());
    let mut src2 = test_source();
    src2.rule_related = Some(serde_json::json!({"name": "a@text"}));
    assert!(analyze_related_books(html, "http://x", &src2).is_empty());
}

/// GAP 17b：getBookInfo 完整链路——analyze_book_info 返回 relatedBooks
#[test]
fn test_analyze_info_includes_related_books() {
    let mut src = test_source();
    src.rule_related = Some(serde_json::json!({
        "bookList": "ul.related@li",
        "name": "a@text",
        "bookUrl": "a@href"
    }));
    let html = r#"<h1 class="bookname">测试书</h1><p class="author">作者X</p>
        <ul class="related"><li><a href="/r/9">推荐书9</a></li></ul>"#;
    let info = analyze_book_info(
        "default",
        html,
        "http://127.0.0.1:9999/book/1",
        &src,
        "http://127.0.0.1:9999/book/1",
        None,
    );
    assert_eq!(info.related_books.len(), 1);
    assert_eq!(info.related_books[0].name, "推荐书9");
    assert_eq!(info.related_books[0].book_url, "http://127.0.0.1:9999/r/9");
}

/// GAP 153：sourceRegex/replaceRegex 支持 lookbehind（regex crate 不支持）
#[test]
fn test_analyze_content_lookbehind_regex() {
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@text",
        "sourceRegex": "(?<=广告：)\\S+",
        "replaceRegex": "(?<=第).+(?=章)##X"
    }));
    let html = r#"<div class="content">正文：第一章 测试内容 广告：烦人</div>"#;
    let content = analyze_content_from(html, &src);
    // sourceRegex（lookbehind）移除 "烦人"；replaceRegex（lookbehind+lookahead）把 "一章 " 替换为 X
    assert_eq!(content, "正文：第X章 测试内容 广告：");
}

/// 书源正文含 HTML 标签（<p>/<br>/&nbsp; 等）时转纯文本：
/// 换行元素 → \n、实体解码、其余标签剥离（前端纯文本渲染，原样透传会显示标签字面量）。
#[test]
fn test_analyze_content_converts_html_to_text() {
    let mut src = test_source();
    // @html 提取：<p>/<br> → 换行
    src.rule_content = Some(serde_json::json!({ "content": "div.content@html" }));
    let html = r#"<div class="content"><p>第一段</p><br><p>第二段</p></div>"#;
    let content = analyze_content_from(html, &src);
    assert_eq!(content, "第一段\n第二段", "HTML 应转纯文本段落: {content}");

    // 扩充块级标签（header/main/pre 等）与 CR 换行归一
    src.rule_content = Some(serde_json::json!({ "content": "div.content@html" }));
    let html = "<div class=\"content\"><header>标题</header><main><pre>第一段\r\n第二段</pre></main></div>";
    let content = analyze_content_from(html, &src);
    assert_eq!(
        content, "标题\n第一段\n第二段",
        "扩充块级标签与 CR 归一: {content}"
    );

    // &nbsp;/&amp;/数字实体解码 + 非换行标签剥离
    src.rule_content = Some(serde_json::json!({ "content": "div.content@html" }));
    let html = r#"<div class="content">甲&nbsp;乙 &amp; 丙 <span>保留</span><script>去掉</script>&#65;</div>"#;
    let content = analyze_content_from(html, &src);
    assert_eq!(
        content, "甲 乙 & 丙 保留A",
        "实体应解码、非换行标签应剥离: {content}"
    );

    // 无 @ 的裸选择器（legacy 兼容）→ 取纯文本（仅此处剥离，规则显式 @html 时保留）
    src.rule_content = Some(serde_json::json!({ "content": "div.content" }));
    let content = analyze_content_from(html, &src);
    assert!(!content.contains("<"), "裸选择器取文本: {content}");
    assert!(content.contains("甲") && content.contains("乙"));

    // 清洗（sourceRegex/replaceRegex）在 HTML → 文本前作用于原样内容
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@html",
        "replaceRegex": "第一段##甲段"
    }));
    let html = r#"<div class="content"><p>第一段</p><br><p>第二段</p></div>"#;
    let content = analyze_content_from(html, &src);
    assert_eq!(
        content, "甲段\n第二段",
        "replaceRegex 后再转纯文本: {content}"
    );

    // 纯文本正文原样返回（无 HTML/实体不引入差异）
    let content = html_content_to_text("纯文本 1 < 2 & 3");
    assert_eq!(content, "纯文本 1 < 2 & 3");
}

/// 用户报告：部分书源返回无任何换行的纯文本正文，整章连成一大段。
/// 长纯文本按句末标点补换行（短文本/已有换行不处理）。
#[test]
fn test_html_content_to_text_smart_paragraph_breaks() {
    let long = format!(
        "{}。{}。{}。",
        "他抬头看了看窗外的夜色".repeat(10),
        "远处传来隐约的钟声".repeat(10),
        "故事从这里正式开始".repeat(10)
    );
    let content = html_content_to_text(&long);
    assert!(
        content.contains('\n'),
        "长纯文本应按句末标点补换行: {}",
        &content[..content.len().min(80)]
    );
    // 已有换行不二次处理
    let with_nl = "第一段。\n第二段。";
    assert_eq!(html_content_to_text(with_nl), with_nl);
    // 短文本不处理
    let short = "短正文没有换行。";
    assert_eq!(html_content_to_text(short), short);
}

/// 用户报告：正文混入站点内嵌的正则脚本（`(本章未完|记住网址|加入书签)`）。
/// script/style/noscript/template/iframe/head 的内容对正文不可见，必须整段剔除。
#[test]
fn test_html_content_to_text_skips_invisible_containers() {
    let html = concat!(
        r#"<div class="content"><p>正文第一段。</p>"#,
        r#"<script>var x = "(本章未完.*继续阅读)|记住.*网址.*com|『加入书签，方便阅读』";</script>"#,
        r#"<style>.ad{display:none}</style>"#,
        r#"<noscript>请开启 JavaScript</noscript>"#,
        r#"<p>正文第二段。</p></div>"#
    );
    let out = html_content_to_text(html);
    assert!(
        !out.contains("本章未完") && !out.contains("记住") && !out.contains("加入书签"),
        "脚本/样式/无脚本内容不应泄漏到正文: {out}"
    );
    assert!(!out.contains("display:none"), "样式内容不应泄漏: {out}");
    assert!(!out.contains("JavaScript"), "noscript 内容不应泄漏: {out}");
    assert!(
        out.contains("正文第一段。") && out.contains("正文第二段。"),
        "正文应保留: {out}"
    );
}

/// GAP 109：contentReplace/replaceRegex 在 ruleContent 解析已应用——
/// 构造含广告行的正文，断言净化（广告行移除、正文保留）。
/// （legacy 的 contentReplace 对应 ruleContent.replaceRegex：`模式##替换`，
/// 逐条 replace_all；sourceRegex 为删除型清洗，同样在解析期应用）
#[test]
fn test_analyze_content_replace_regex_cleans_ad_lines() {
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@html",
        // 广告行 + 推广行 → 替换为空（净化）
        "replaceRegex": "【广告】.*?。|（推广）.*?。##"
    }));
    let html = concat!(
        r#"<div class="content"><p>正文第一段。</p>"#,
        r#"<p>【广告】本书由某某网首发，请支持正版。</p>"#,
        r#"<p>（推广）加群领福利。</p>"#,
        r#"<p>正文第二段。</p></div>"#
    );
    let content = analyze_content_from(html, &src);
    assert!(
        !content.contains("【广告】"),
        "广告行应被 replaceRegex 清除: {content}"
    );
    assert!(!content.contains("（推广）"), "推广行应被清除: {content}");
    assert!(
        content.contains("正文第一段。") && content.contains("正文第二段。"),
        "正文应保留: {content}"
    );

    // sourceRegex（删除型）同样应用于正文
    src.rule_content = Some(serde_json::json!({
        "content": "div.content@text",
        "sourceRegex": "【广告】[^】]*"
    }));
    let html = r#"<div class="content">正文甲。【广告】这是一条广告】正文乙。</div>"#;
    let content = analyze_content_from(html, &src);
    assert!(
        !content.contains("广告"),
        "sourceRegex 应删除广告片段: {content}"
    );
    assert!(content.contains("正文甲。") && content.contains("正文乙。"));
}

// ---------------- 非文本内容分派（音频/漫画/视频/文件） ----------------

/// 微型 HTTP 服务器：固定响应体（同 crawler 测试模式）
async fn serve(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for _ in 0..10 {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut resp = head.into_bytes();
            resp.extend_from_slice(body.as_bytes());
            let _ = sock.write_all(&resp).await;
        }
    });
    format!("http://{addr}")
}

/// 音频书：ruleContent.content 提取音频 URL → analyze_media_url 返回音频流直链
#[tokio::test]
async fn test_analyze_media_url_audio() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
    let base =
        serve(r#"<html><div class="player"><audio src="/stream/1.mp3"></audio></div></html>"#)
            .await;
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "div.player audio@src"
    }));
    let url = analyze_media_url(
        "default",
        &format!("{base}/chapter/1"),
        &src,
        None,
        None,
        "",
    )
    .await
    .unwrap();
    assert_eq!(
        url,
        format!("{base}/stream/1.mp3"),
        "音频 URL 应提取并绝对化"
    );
    // contentType 映射
    assert_eq!(audio_content_type(&url), "audio/mpeg");
    assert_eq!(
        audio_content_type("https://x/a.m3u8?t=1"),
        "application/vnd.apple.mpegurl"
    );
    assert_eq!(audio_content_type("https://x/a.m4a"), "audio/mp4");
}

/// 音频书：无 ruleContent（章节 URL 即音频流直链）→ 原样返回章节 URL
#[tokio::test]
async fn test_analyze_media_url_direct_chapter() {
    let mut src = test_source();
    src.rule_content = None;
    let url = analyze_media_url(
        "default",
        "https://cdn.example.com/audio/42.m4a",
        &src,
        None,
        None,
        "",
    )
    .await
    .unwrap();
    assert_eq!(url, "https://cdn.example.com/audio/42.m4a");
}

/// 漫画书：CSS 规则命中多个图片节点 → images 列表（绝对化 + 去重）
#[tokio::test]
async fn test_analyze_comic_images_css() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
    let base = serve(
        r#"<html><div class="imgs"><img src="/p/1.jpg"><img src="/p/2.jpg"><img src="/p/1.jpg"></div></html>"#,
    )
    .await;
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({ "content": "div.imgs img@src" }));
    let images = analyze_comic_images("default", &format!("{base}/comic/1"), &src, None, None, "")
        .await
        .unwrap();
    assert_eq!(
        images,
        vec![format!("{base}/p/1.jpg"), format!("{base}/p/2.jpg")],
        "应提取全部图片且去重保序、相对转绝对"
    );
}

/// 漫画书：@js: 规则返回 JSON 字符串数组 → images 列表
#[tokio::test]
async fn test_analyze_comic_images_js_array() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
    let base = serve(r#"{"data":["/a/1.webp","/a/2.webp"]}"#).await;
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({
        "content": "@js:JSON.parse(result).data"
    }));
    let images = analyze_comic_images("default", &format!("{base}/comic/2"), &src, None, None, "")
        .await
        .unwrap();
    assert_eq!(
        images,
        vec![format!("{base}/a/1.webp"), format!("{base}/a/2.webp")],
        "JS 数组应逐个提取并绝对化"
    );
}

/// 漫画书：无规则且章节 URL 即图片直链 → 单图列表；有规则但提取不到 → 空列表
#[tokio::test]
async fn test_analyze_comic_images_direct() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true); // mock 绑定 127.0.0.1（P1 SSRF 校验放行，仅测试）
                                                                         // 有规则但页面无匹配 → 空列表
    let base = serve(r#"<html><div class="imgs"></div></html>"#).await;
    let mut src = test_source();
    src.rule_content = Some(serde_json::json!({ "content": "div.imgs img@src" }));
    let images = analyze_comic_images("default", &format!("{base}/comic/3"), &src, None, None, "")
        .await
        .unwrap();
    assert!(images.is_empty(), "有规则但提取不到 → 空列表");

    // 无规则且章节 URL 即图片直链 → 单图列表（不抓取）
    let mut src2 = test_source();
    src2.rule_content = None;
    let images2 = analyze_comic_images(
        "default",
        "https://img.example.com/comic/5.jpg",
        &src2,
        None,
        None,
        "",
    )
    .await
    .unwrap();
    assert_eq!(
        images2,
        vec!["https://img.example.com/comic/5.jpg".to_string()]
    );
}

/// collect_urls 纯函数：URL 文本 / JSON 字符串数组 / 对象数组 / 空
#[test]
fn test_collect_urls() {
    let mut out = Vec::new();
    collect_urls("https://a.mp3", &mut out);
    collect_urls(r#"["https://b.jpg","/c.png"]"#, &mut out);
    collect_urls(
        r#"[{"url":"https://d.webp"},{"src":"https://e.avif"}]"#,
        &mut out,
    );
    collect_urls("  ", &mut out);
    assert_eq!(
        out,
        vec![
            "https://a.mp3",
            "https://b.jpg",
            "/c.png",
            "https://d.webp",
            "https://e.avif"
        ]
    );
    // 对象数组取 url/src/href 首命中
    let mut out2 = Vec::new();
    collect_urls(r#"[{"name":"x","href":"/h/1"}]"#, &mut out2);
    assert_eq!(out2, vec!["/h/1"]);
}

/// 目录字段规则：isVolume/isVip 命中、卷章节空 URL 用标题+序号、普通章节空 URL 用 base
#[test]
fn test_chapters_from_items_volume_vip_fallback() {
    let rule = TocRule {
        chapter_list: Some("$.data".into()),
        chapter_name: Some("$.t".into()),
        chapter_url: Some("$.u".into()),
        is_volume: Some("$.vol".into()),
        chapter_vip: Some("$.vip".into()),
        ..Default::default()
    };
    let items = vec![
        r#"{"t":"第一卷 风云","u":"","vol":"1","vip":"0"}"#.to_string(),
        r#"{"t":"第一章","u":"/c/1","vol":"0","vip":"true"}"#.to_string(),
        r#"{"t":"第二章","u":"","vol":"0","vip":"false"}"#.to_string(),
    ];
    let mut vars = crate::parser::rule::RuleVars::new();
    let chapters = chapters_from_items(&items, &rule, "https://src.test/toc", 0, &mut vars);
    assert_eq!(chapters.len(), 3);
    assert!(chapters[0].is_volume, "isVolume 规则命中应为卷");
    assert_eq!(chapters[0].url, "第一卷 风云0", "卷章节空 URL 用标题+序号");
    assert_eq!(chapters[1].title, "\u{1F512}第一章", "isVip 命中加锁前缀");
    assert_eq!(chapters[1].url, "https://src.test/c/1");
    assert!(!chapters[2].is_volume);
    assert_eq!(
        chapters[2].url, "https://src.test/toc",
        "普通章节空 URL 用 base"
    );
}

/// 目录去重：重复 (title,url,is_volume) 保序去重
#[test]
fn test_dedupe_chapters() {
    let ch = |title: &str, url: &str, vol: bool| BookChapter {
        title: title.into(),
        url: url.into(),
        tag: None,
        is_volume: vol,
        index: 0,
    };
    let input = vec![
        ch("章A", "/a", false),
        ch("章B", "/b", false),
        ch("章A", "/a", false),
        ch("卷X", "/v", true),
    ];
    let out = dedupe_chapters(input);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].title, "章A");
    assert_eq!(out[1].title, "章B");
    assert_eq!(out[2].title, "卷X");
}

/// `-` 前缀目录规则：JSONPath 目录倒序
#[tokio::test]
async fn test_analyze_toc_reverse_prefix() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve(r#"{"data":[{"t":"第一章","u":"/c/1"},{"t":"第二章","u":"/c/2"}]}"#).await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "-$.data",
        "chapterName": "$.t",
        "chapterUrl": "$.u"
    }));
    let chapters = analyze_toc("default", &format!("{base}/toc"), &src, 2, None, "")
        .await
        .unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].title, "第二章", "`-` 前缀目录应倒序");
    assert_eq!(chapters[1].title, "第一章");
    assert_eq!(chapters[0].index, 0);
    assert_eq!(chapters[1].index, 1);
}

/// loginCheckJs 自动执行：true/空保持响应体；false 保持并继续；其他返回值重写响应体
#[tokio::test]
async fn test_apply_login_check_js() {
    let src = test_source();
    // 无 loginCheckJs → 原样
    assert_eq!(
        apply_login_check_js("default", &src, "<html>正文</html>", "https://a.com", None).await,
        "<html>正文</html>"
    );

    // true → 原样
    let mut src = test_source();
    src.login_check_js = Some("true".into());
    assert_eq!(
        apply_login_check_js("default", &src, "BODY", "https://a.com", None).await,
        "BODY"
    );
    // false → 原样 + 不中断
    src.login_check_js = Some("false".into());
    assert_eq!(
        apply_login_check_js("default", &src, "BODY", "https://a.com", None).await,
        "BODY"
    );
    // JS 重写响应体
    src.login_check_js = Some("result.replace('A', 'B')".into());
    assert_eq!(
        apply_login_check_js("default", &src, "AAA", "https://a.com", None).await,
        "BAA"
    );
    // JS 失败 → 原样
    src.login_check_js = Some("throw new Error('x')".into());
    assert_eq!(
        apply_login_check_js("default", &src, "BODY", "https://a.com", None).await,
        "BODY"
    );
}

/// 详情链路：fetch_book_info 执行 loginCheckJs 后解析（JS 重写详情页）
#[tokio::test]
async fn test_fetch_book_info_applies_login_check_js() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let base = serve(r#"<html><h1 class="bookname">旧名</h1></html>"#).await;
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    src.login_check_js = Some("result.replace('旧名', '新名')".into());
    let info = fetch_book_info("default", &format!("{base}/book/1"), &src, None)
        .await
        .unwrap();
    assert_eq!(info.name, "新名", "详情链路应执行 loginCheckJs 重写");
}

/// concurrentRate：详情/目录/正文共用 fetch_url 入口，请求前按毫秒限速
#[tokio::test]
async fn test_fetch_url_applies_concurrent_rate() {
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let times = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i64>::new()));
    let times2 = times.clone();
    tokio::spawn(async move {
        for _ in 0..2 {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let times2 = times2.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                times2.lock().unwrap().push(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as i64,
                );
                let body = "ok";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    let mut src = test_source();
    src.concurrent_rate = Some("80".into());
    let url = format!("http://{addr}/x");
    // A2 语义：concurrentRate 限速在 acquire 准入层生效（service::search
    // test_concurrent_rate_interval_shared / _window_shared 两测已验证时序）。
    // 此处仅冒烟验证带 rate 的抓取链路正常（注：不能在此断言端到端 gap——
    // reqwest 冷启动可能超过窗口期使第二次 acquire 免等，属正确放行）。
    let _ = fetch_url("default", &url, &src).await.unwrap();
    let _ = fetch_url("default", &url, &src).await.unwrap();
    let recorded = times.lock().unwrap();
    assert_eq!(recorded.len(), 2);
}

// ---- nextTocUrl 多 URL（legado 多 URL 语义）----

fn toc_vars() -> crate::parser::rule::RuleVars {
    let mut vars = crate::parser::rule::RuleVars::new();
    vars.book_name = Some("书".to_string());
    vars
}

/// CSS 规则命中多个元素 → 全部取出，按命中顺序即页序（`option@value` 全套页码）
#[test]
fn test_next_toc_urls_multi_elements() {
    let html = r#"<div class="pages">
        <select onchange="go()">
            <option value="/50551/1.html" selected>1</option>
            <option value="/50551/2.html">2</option>
            <option value="/50551/3.html">3</option>
        </select></div>"#;
    let mut vars = toc_vars();
    let urls = next_toc_urls(
        html,
        "option@value",
        "https://m.example.com/book/50551/",
        &test_source(),
        "default",
        &mut vars,
    );
    assert_eq!(
        urls,
        vec![
            "https://m.example.com/50551/1.html",
            "https://m.example.com/50551/2.html",
            "https://m.example.com/50551/3.html",
        ],
        "option@value 应一次给出全套页码（相对路径按 base 绝对化）"
    );
}

/// 多元素链接（`.page-item a@href`）同样全量取出
#[test]
fn test_next_toc_urls_multi_links() {
    let html = r#"<ul>
        <li class="page-item"><a href="?page=2">2</a></li>
        <li class="page-item"><a href="?page=3">3</a></li>
        <li class="page-item"><a href="?page=4">4</a></li>
    </ul>"#;
    let mut vars = toc_vars();
    let urls = next_toc_urls(
        html,
        ".page-item a@href",
        "https://x.test/book/1/",
        &test_source(),
        "default",
        &mut vars,
    );
    assert_eq!(
        urls,
        vec![
            "https://x.test/book/1/?page=2",
            "https://x.test/book/1/?page=3",
            "https://x.test/book/1/?page=4",
        ]
    );
}

/// `@js:` 返回**数组**（legado：JS 可返回 URL 数组）→ 展开成多个 URL
#[test]
fn test_next_toc_urls_js_returns_array() {
    let js = "var list=[];\nfor(var i=2;i<=4;i++){ list.push(baseUrl+'/p'+i+'.html') }\nlist;";
    let mut vars = toc_vars();
    vars.insert(
        "baseUrl".to_string(),
        "https://m.example.com/toc".to_string(),
    );
    let urls = next_toc_urls(
        "<html>共 4 页</html>",
        &format!("@js:{js}"),
        "https://m.example.com/toc",
        &test_source(),
        "default",
        &mut vars,
    );
    assert_eq!(
        urls,
        vec![
            "https://m.example.com/toc/p2.html",
            "https://m.example.com/toc/p3.html",
            "https://m.example.com/toc/p4.html",
        ]
    );
}

/// `@js:` 返回**字符串**（单个 URL）→ 与改造前一致，只有一个地址
#[test]
fn test_next_toc_urls_js_returns_single_string() {
    let mut vars = toc_vars();
    vars.insert(
        "baseUrl".to_string(),
        "https://m.example.com/toc".to_string(),
    );
    let urls = next_toc_urls(
        "<html/>",
        "@js:baseUrl+'/p2.html'",
        "https://m.example.com/toc",
        &test_source(),
        "default",
        &mut vars,
    );
    assert_eq!(urls, vec!["https://m.example.com/toc/p2.html".to_string()]);
}

/// 单 URL 向后兼容：`text.下一页@href` 只命中一个 → 与 field_url_with_vars 同值
#[test]
fn test_next_toc_urls_single_url_backward_compatible() {
    let html = r#"<div><a href="/50551/2.html">下一页</a></div>"#;
    let base = "https://m.roushuwu.com/book/50551/";
    let rule = "text.下一页@href";
    let mut vars_a = toc_vars();
    let got = next_toc_urls(html, rule, base, &test_source(), "default", &mut vars_a);
    // 原单 URL 路径（改造前 analyze_toc_impl 的取法）逐字节对照
    let mut vars_b = toc_vars();
    let legacy = {
        let next =
            crate::service::search::field_url_with_vars(html, Some(rule), "", base, &mut vars_b);
        let next = expand_next_url(&next, base, &test_source(), "default");
        to_abs(&next, base)
    };
    assert_eq!(legacy, "https://m.roushuwu.com/50551/2.html");
    assert_eq!(got, vec![legacy], "单 URL 必须与改造前完全一致");
}

/// 无命中的单 URL 规则 → 空（不造假地址，翻页链正常收尾）
#[test]
fn test_next_toc_urls_empty_when_no_match() {
    let mut vars = toc_vars();
    let urls = next_toc_urls(
        "<div>没有下一页</div>",
        "text.下一页@href",
        "https://x.test/toc",
        &test_source(),
        "default",
        &mut vars,
    );
    assert!(urls.is_empty(), "无命中应为空: {urls:?}");
}

/// 相对 URL 解析：`/` 开头按根、无前导 `/` 按当前路径解析（legado toAbsolute）
#[test]
fn test_next_toc_urls_relative_resolution() {
    let html = r#"<select>
        <option value="2.html">2</option>
        <option value="/book/50551/3.html">3</option>
        <option value="https://cdn.x.test/b/4.html">4</option>
    </select>"#;
    let mut vars = toc_vars();
    let urls = next_toc_urls(
        html,
        "option@value",
        "https://m.example.com/book/50551/1.html",
        &test_source(),
        "default",
        &mut vars,
    );
    assert_eq!(
        urls,
        vec![
            "https://m.example.com/book/50551/2.html",
            "https://m.example.com/book/50551/3.html",
            // 已是绝对 URL 的保持不变
            "https://cdn.x.test/b/4.html",
        ]
    );
}

/// 多值拆分：JSON 数组文本（JS 数组序列化出口）与换行拼接两种形态
#[test]
fn test_split_url_list_forms() {
    assert_eq!(
        split_url_list(r#"["https://a/1","https://a/2"]"#),
        vec!["https://a/1".to_string(), "https://a/2".to_string()]
    );
    assert_eq!(
        split_url_list("https://a/1\n\n https://a/2 \n"),
        vec!["https://a/1".to_string(), "https://a/2".to_string()]
    );
    // 单值（含逗号的 URL 不被误拆）→ 向后兼容
    assert_eq!(
        split_url_list("https://a/1?x=1,2"),
        vec!["https://a/1?x=1,2".to_string()]
    );
    assert!(split_url_list("   ").is_empty());
}

/// 目录回退判定：只有「0 章 + 书页地址有效且不同」才补抓书页
#[test]
fn test_should_retry_toc_with_book_page() {
    let empty: Vec<BookChapter> = vec![];
    let one = vec![BookChapter {
        title: "第1章".to_string(),
        url: "https://a/1".to_string(),
        index: 0,
        ..Default::default()
    }];
    // 0 章 + 地址不同 → 回退
    assert!(should_retry_toc_with_book_page(
        "https://toc/1",
        "https://book/1",
        &empty
    ));
    // 已有章节 → 不回退
    assert!(!should_retry_toc_with_book_page(
        "https://toc/1",
        "https://book/1",
        &one
    ));
    // 同一个地址 / 空书页地址 → 不回退（避免同一页重复抓）
    assert!(!should_retry_toc_with_book_page(
        "https://book/1",
        "https://book/1",
        &empty
    ));
    assert!(!should_retry_toc_with_book_page(
        "https://toc/1",
        "  ",
        &empty
    ));
}

/// 多 URL 目录：一次解析出多 URL → 并发抓、按页序合并、同 URL 只抓一次
#[tokio::test]
async fn test_toc_multi_url_fanout_order_and_dedupe() {
    use std::sync::{Arc, Mutex};
    let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // 记录每个被请求的路径（去重断言用）
    let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hits_srv = hits.clone();
    // 每个请求的 [开始, 结束]（算并发重叠峰值 = 真实在飞峰值）
    let spans: Arc<Mutex<Vec<(std::time::Instant, std::time::Instant)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let spans_srv = spans.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let hits = hits_srv.clone();
            let spans = spans_srv.clone();
            tokio::spawn(async move {
                let t_start = std::time::Instant::now();
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                hits.lock().unwrap().push(path.clone());
                let name = path
                    .trim_start_matches('/')
                    .replace(|c: char| !c.is_alphanumeric(), "");
                // 每页固定 2 章，标题/URL 带页号（页序可辨）
                let body = format!(
                    r#"{{"ch":[{{"t":"{name}A","u":"/{name}a"}},{{"t":"{name}B","u":"/{name}b"}}]}}"#
                );
                // 让三页同时在飞（延迟 > 单页 RTT）
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
                spans
                    .lock()
                    .unwrap()
                    .push((t_start, std::time::Instant::now()));
            });
        }
    });
    let base = format!("http://{addr}");
    let mut src = test_source();
    src.book_source_url = format!("{base}/src");
    // nextTocUrl = @js: 返回 URL 数组（本次要支持的形态）：首页给 [自身, p2, p3, p4, p2]
    src.rule_toc = Some(serde_json::json!({
        "chapterList": "$.ch",
        "chapterName": "$.t",
        "chapterUrl": "$.u",
        "nextTocUrl": "@js:var list=[];if(baseUrl.slice(-2)=='p1'){var u=baseUrl.slice(0,-2);list=[baseUrl,u+'p2',u+'p3',u+'p4',u+'p2']};list"
    }));
    let t0 = std::time::Instant::now();
    let chapters = analyze_toc("default", &format!("{base}/p1"), &src, 20, None, "")
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    let got = hits.lock().unwrap().clone();
    let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        titles,
        vec!["p1A", "p1B", "p2A", "p2B", "p3A", "p3B", "p4A", "p4B"],
        "多 URL 目录必须按页序（提取顺序）合并; hits={got:?}"
    );
    let urls: Vec<&str> = chapters.iter().map(|c| c.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            format!("{base}/p1a"),
            format!("{base}/p1b"),
            format!("{base}/p2a"),
            format!("{base}/p2b"),
            format!("{base}/p3a"),
            format!("{base}/p3b"),
            format!("{base}/p4a"),
            format!("{base}/p4b"),
        ]
    );
    let p2_hits = got.iter().filter(|p| p.starts_with("/p2")).count();
    assert_eq!(p2_hits, 1, "同一 URL 只抓一次（去重，含首页自指）: {got:?}");
    assert_eq!(got.len(), 4, "只抓 p1..p4 各一次: {got:?}");
    // 并发重叠峰值：3 个后续页同时在飞（每页 200ms 延迟）→ 峰值必须 >1
    let spans = spans.lock().unwrap().clone();
    let mut events: Vec<(std::time::Instant, i32)> = Vec::new();
    for (s, e) in &spans {
        events.push((*s, 1));
        events.push((*e, -1));
    }
    events.sort();
    let (mut cur, mut peak) = (0i32, 0i32);
    for (_, d) in events {
        cur += d;
        peak = peak.max(cur);
    }
    assert!(
        peak >= 2,
        "多 URL 必须真的并发（在飞峰值应 >1，实际 {peak}; 墙钟 {elapsed:?}）"
    );
}
