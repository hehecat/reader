//! RSS 链路：feed 抓取 → 文章列表；网页正文提取
//!
//! 对齐 legacy Rss.getArticles / getContent：
//! - 列表：GET sortUrl（未配置则 sourceUrl）；ruleArticles 配置时按
//!   RssParserByRule 语义解析（CSS/JSONPath/Regex/JS + ruleTitle/rulePubDate/
//!   ruleDescription/ruleImage/ruleLink，`-` 前缀倒序）；否则 feed-rs 解析 RSS/Atom
//! - 分页：ruleNextPage="PAGE" 或 sortUrl 含 {{page}} 由前端 page 参数推进
//! - 正文：优先文章 content 字段（feed content/summary）；为空且配置 ruleContent 时
//!   按规则提取；否则抓取文章链接用常见正文容器 CSS 选择器提取段落文本

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::model::rss::{RssArticle, RssSource};
use crate::service::crawler;

/// 抓取 feed 并解析为文章列表（含分页参数替换；{{page}} 存在时替换为页码）。
/// `sort_url` 为前端指定分类 URL（legacy sortUrl 多段 `名称::地址` 的其中一段）；
/// 为空时沿用默认语义（多段取第一段）。
pub async fn fetch_articles(
    source: &RssSource,
    page: i64,
    sort_url: Option<&str>,
) -> Result<Vec<RssArticle>> {
    let url = build_feed_url_for(source, page, sort_url);
    // legado concurrentRate：RSS 抓取请求前限速（A2 共享滑窗/间隔）
    crate::service::search::concurrent_rate_acquire_raw(
        "rss",
        &source.source_url,
        source.concurrent_rate().as_deref(),
    )
    .await;
    let headers = crawler::parse_header(source.header().as_deref().unwrap_or(""));
    let resp = crawler::fetch(&url, &headers, 30, "GET", None, None, None)
        .await
        .with_context(|| format!("抓取 RSS 失败: {url}"))?;
    parse_feed_at(&resp.body, source, &resp.url)
}

/// 从 legacy sortUrl 多段文本（&&/换行分隔，每段 `名称::地址`）取首个有效 URL
fn first_sort_segment(sort_url: &str) -> Option<&str> {
    for seg in sort_url
        .split(['\n', '&'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some((_, u)) = seg.split_once("::") {
            if !u.trim().is_empty() {
                return Some(u.trim());
            }
        }
    }
    None
}

/// 构造抓取 URL：sortUrl 多段（&&/换行分隔，每段 name::url）取第一段 URL；
/// 前端指定 `sort_url` 时（完整 URL 或 `名称::地址` 段）优先使用该段；
/// 无有效段用 sourceUrl。{{page}} 存在时替换为页码。
pub fn build_feed_url_for(source: &RssSource, page: i64, sort_url: Option<&str>) -> String {
    let mut url = source.source_url.clone();
    if let Some(requested) = sort_url.filter(|s| !s.trim().is_empty()) {
        url = first_sort_segment(requested)
            .unwrap_or(requested.trim())
            .to_string();
    } else if let Some(sort_url) = source.sort_url().filter(|s| !s.trim().is_empty()) {
        if let Some(u) = first_sort_segment(&sort_url) {
            url = u.to_string();
        }
    }
    if url.contains("{{page}}") {
        url = url.replace("{{page}}", &page.to_string());
    }
    url
}

/// 默认抓取 URL（无前端分类指定）：sortUrl 多段取第一段；无有效段用 sourceUrl
pub fn build_feed_url(source: &RssSource, page: i64) -> String {
    build_feed_url_for(source, page, None)
}

/// 解析 feed XML → 文章列表（纯函数，单测直接调用）
pub fn parse_feed(xml: &str, source: &RssSource) -> Result<Vec<RssArticle>> {
    parse_feed_at(xml, source, &source.source_url)
}

/// 按实际抓取 URL 为相对链接基准解析 feed
fn parse_feed_at(xml: &str, source: &RssSource, base_url: &str) -> Result<Vec<RssArticle>> {
    // legado RssParserByRule：ruleArticles 非空 → 自定义规则解析
    if let Some(rule) = source.rule_articles().filter(|r| !r.trim().is_empty()) {
        return Ok(parse_feed_by_rule(xml, source, base_url, &rule));
    }
    let feed = feed_rs::parser::parse(xml.as_bytes())
        .with_context(|| format!("解析 RSS 失败: {}", source.source_url))?;
    let mut articles: Vec<RssArticle> = feed
        .entries
        .iter()
        .map(|e| article_from_entry(e, source))
        .collect();
    // 按发布时间倒序（无时间戳的排最后，保持 feed 顺序）
    articles.sort_by_key(|a| std::cmp::Reverse(a.time));
    Ok(articles)
}

/// RssParserByRule 语义：ruleArticles 定位条目 + 字段规则提取
fn parse_feed_by_rule(
    xml: &str,
    source: &RssSource,
    base_url: &str,
    rule_articles: &str,
) -> Vec<RssArticle> {
    let (list_rule, reverse) = crate::service::search::strip_list_rule_prefix(rule_articles);
    let items = crate::service::book::toc_items(&list_rule, xml);
    let mut vars = crate::parser::rule::RuleVars::new();
    let mut articles: Vec<RssArticle> = Vec::with_capacity(items.len());
    for item in items {
        let title = crate::service::search::field_with_vars(
            &item,
            source.rule_title().as_deref(),
            "",
            &mut vars,
        );
        if title.is_empty() {
            continue;
        }
        let link = crate::service::search::field_url_with_vars(
            &item,
            source.rule_link().as_deref(),
            "",
            base_url,
            &mut vars,
        );
        let pub_date = crate::service::search::field_with_vars(
            &item,
            source.rule_pub_date().as_deref(),
            "",
            &mut vars,
        );
        let description = crate::service::search::opt_field_with_vars(
            &item,
            source.rule_description().as_deref(),
            &mut vars,
        );
        let cover = crate::service::search::field_url_with_vars(
            &item,
            source.rule_image().as_deref(),
            "",
            base_url,
            &mut vars,
        );
        let cover = if cover.is_empty() { None } else { Some(cover) };
        articles.push(RssArticle {
            url: link,
            source_url: source.source_url.clone(),
            title,
            author: String::new(),
            time: parse_rss_datetime(&pub_date),
            content: description,
            cover,
            read: false,
            user_namespace: source.user_namespace.clone(),
        });
    }
    if reverse {
        articles.reverse();
    }
    articles
}

/// legacy rulePubDate 字符串 → 毫秒时间戳（宽松解析；失败返回 0）
fn parse_rss_datetime(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    // 纯数字：10 位秒 / 13 位毫秒
    if let Ok(n) = s.parse::<i64>() {
        return if n < 10_000_000_000 { n * 1000 } else { n };
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp_millis();
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
        return dt.timestamp_millis();
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
        "%Y年%m月%d日 %H:%M",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return naive.and_utc().timestamp_millis();
        }
    }
    if let Ok(naive) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(dt) = naive.and_hms_opt(0, 0, 0) {
            return dt.and_utc().timestamp_millis();
        }
    }
    0
}

/// feed-rs Entry → RssArticle
fn article_from_entry(entry: &feed_rs::model::Entry, source: &RssSource) -> RssArticle {
    let url = entry
        .links
        .iter()
        .find(|l| l.rel.as_deref().unwrap_or("alternate") == "alternate")
        .map(|l| l.href.clone())
        .or_else(|| entry.links.first().map(|l| l.href.clone()))
        .or_else(|| {
            // guid 为 URL 时兜底
            if entry.id.starts_with("http://") || entry.id.starts_with("https://") {
                Some(entry.id.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let title = entry
        .title
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_default();
    // 作者：feed-rs 的 RSS2 <author> 解析为 name="author" + email=原文（如“作者乙”），
    // 因此 name 为占位符“author”或空时回退 email；Atom/dc:creator 直接用 name
    let author = entry
        .authors
        .first()
        .map(|a| {
            if a.name.is_empty() || a.name == "author" {
                a.email.clone().unwrap_or_else(|| a.name.clone())
            } else {
                a.name.clone()
            }
        })
        .unwrap_or_default();
    // 发布时间：published 优先，缺省用 updated；无时间戳置 0
    let time = entry
        .published
        .or(entry.updated)
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    // 正文：content.body 优先，缺省用 summary
    let content = entry
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()));
    // 配图：media 组第一张图
    let cover = entry
        .media
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|c| c.url.as_ref().map(|u| u.to_string()));
    RssArticle {
        url,
        source_url: source.source_url.clone(),
        title,
        author,
        time,
        content,
        cover,
        read: false,
        user_namespace: String::new(),
    }
}

/// 抓取网页正文（简单 CSS 选择器：常见正文容器 → 段落文本；兜底 body 全文）
pub async fn fetch_web_content(url: &str) -> Result<String> {
    let headers = HashMap::new();
    let resp = crawler::fetch(url, &headers, 30, "GET", None, None, None)
        .await
        .with_context(|| format!("抓取文章页面失败: {url}"))?;
    let text = extract_web_content(&resp.body);
    if text.is_empty() {
        anyhow::bail!("网页正文提取为空: {url}");
    }
    Ok(text)
}

/// 抓取 RSS 文章正文：ruleContent 配置时按规则提取；否则 CSS 启发式
pub async fn fetch_article_content(source: &RssSource, url: &str) -> Result<String> {
    if let Some(rule) = source.rule_content().filter(|r| !r.trim().is_empty()) {
        let headers = crawler::parse_header(source.header().as_deref().unwrap_or(""));
        let resp = crawler::fetch(url, &headers, 30, "GET", None, None, None)
            .await
            .with_context(|| format!("抓取文章页面失败: {url}"))?;
        let mut vars = crate::parser::rule::RuleVars::new();
        let text = crate::service::search::field_with_vars(&resp.body, Some(&rule), "", &mut vars);
        if text.trim().is_empty() {
            anyhow::bail!("RSS 正文规则提取为空: {url}");
        }
        return Ok(text);
    }
    fetch_web_content(url).await
}

/// 从 HTML 提取正文（纯函数，单测直接调用）
pub fn extract_web_content(html: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    // 常见正文容器（按优先级）
    const CANDIDATES: &[&str] = &[
        "article",
        ".article-content",
        ".post-content",
        ".entry-content",
        ".article",
        "#content",
        ".content",
        "main",
    ];
    for sel in CANDIDATES {
        let Ok(selector) = scraper::Selector::parse(sel) else {
            continue;
        };
        if let Some(node) = doc.select(&selector).next() {
            let text = visible_text(&node);
            if !text.is_empty() {
                return text;
            }
        }
    }
    // 兜底：body 全部可见文本
    if let Ok(selector) = scraper::Selector::parse("body") {
        if let Some(node) = doc.select(&selector).next() {
            return visible_text(&node);
        }
    }
    String::new()
}

/// 收集子树内可见文本（跳过 script/style），按行合并去空白
fn visible_text(root: &scraper::ElementRef<'_>) -> String {
    let mut out = Vec::new();
    collect_visible_text(root, &mut out);
    clean_text(&out.join("\n"))
}

/// 递归收集文本节点，跳过 script/style 子树
fn collect_visible_text(elem: &scraper::ElementRef<'_>, out: &mut Vec<String>) {
    if elem.value().name() == "script" || elem.value().name() == "style" {
        return;
    }
    for child in elem.children() {
        match child.value() {
            scraper::node::Node::Text(t) => out.push(t.text.to_string()),
            scraper::node::Node::Element(_) => {
                if let Some(e) = scraper::ElementRef::wrap(child) {
                    collect_visible_text(&e, out);
                }
            }
            _ => {}
        }
    }
}

/// 合并文本行：去空白、去空行、按行拼接
fn clean_text(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> RssSource {
        RssSource {
            source_url: "https://example.com/feed.xml".into(),
            source_name: "测试源".into(),
            enabled: true,
            ..Default::default()
        }
    }

    const SAMPLE_RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>测试频道</title>
    <link>https://example.com</link>
    <description>测试</description>
    <item>
      <title>第一篇</title>
      <link>https://example.com/1</link>
      <guid>https://example.com/1</guid>
      <author>作者甲</author>
      <pubDate>Wed, 01 Jan 2025 00:00:00 GMT</pubDate>
      <description><![CDATA[<p>第一篇文章摘要</p>]]></description>
    </item>
    <item>
      <title>第二篇</title>
      <link>https://example.com/2</link>
      <guid>guid-2</guid>
      <author>作者乙</author>
      <pubDate>Thu, 02 Jan 2025 00:00:00 GMT</pubDate>
      <description>第二篇摘要</description>
    </item>
  </channel>
</rss>"#;

    /// feed 解析：标题 / 链接 / 作者 / 时间 / 摘要提取，且按时间倒序
    #[test]
    fn test_parse_feed_extracts_articles() {
        let articles = parse_feed(SAMPLE_RSS, &source()).expect("RSS 解析应成功");
        assert_eq!(articles.len(), 2);
        // 按发布时间倒序：第二篇（01-02）在前
        assert_eq!(articles[0].title, "第二篇");
        assert_eq!(articles[0].url, "https://example.com/2");
        assert_eq!(articles[0].author, "作者乙");
        assert_eq!(articles[0].time, 1735776000000);
        assert_eq!(articles[0].content.as_deref(), Some("第二篇摘要"));
        assert_eq!(articles[1].title, "第一篇");
        assert_eq!(articles[1].author, "作者甲");
        assert_eq!(
            articles[1].content.as_deref(),
            Some("<p>第一篇文章摘要</p>")
        );
        assert_eq!(articles[1].source_url, "https://example.com/feed.xml");
    }

    /// 无链接的条目用 http(s) guid 兜底；无时间戳置 0
    #[test]
    fn test_parse_feed_fallbacks() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <item><title>无链接</title><guid isPermaLink="true">https://example.com/guid-only</guid></item>
  <item><title>纯文本guid</title><guid>abc</guid><description>摘要</description></item>
</channel></rss>"#;
        let articles = parse_feed(xml, &source()).expect("解析应成功");
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].url, "https://example.com/guid-only");
        assert_eq!(articles[1].url, "", "非 URL guid 不应作为链接");
        assert_eq!(articles[1].time, 0);
        assert_eq!(articles[1].content.as_deref(), Some("摘要"));
    }

    /// Atom feed 解析
    #[test]
    fn test_parse_atom_feed() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom 频道</title>
  <entry>
    <title>Atom文章</title>
    <link href="https://example.com/atom/1"/>
    <id>tag:example.com,2025:1</id>
    <author><name>作者丙</name></author>
    <published>2025-03-01T08:00:00Z</published>
    <content type="html">&lt;p&gt;Atom正文&lt;/p&gt;</content>
  </entry>
</feed>"#;
        let articles = parse_feed(xml, &source()).expect("Atom 解析应成功");
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Atom文章");
        assert_eq!(articles[0].url, "https://example.com/atom/1");
        assert_eq!(articles[0].author, "作者丙");
        assert_eq!(articles[0].time, 1740816000000);
        assert_eq!(articles[0].content.as_deref(), Some("<p>Atom正文</p>"));
    }

    /// 无效 feed：返回错误
    #[test]
    fn test_parse_feed_invalid() {
        let err = parse_feed("<html>不是feed</html>", &source());
        assert!(err.is_err(), "非 feed 内容应报错");
    }

    /// sortUrl 构造：无 sortUrl 用 sourceUrl；多段取第一段；无 :: 的段按 legacy 丢弃；{{page}} 替换
    /// （sortUrl 从 raw_json 读取）
    #[test]
    fn test_build_feed_url() {
        let mut s = source();
        assert_eq!(build_feed_url(&s, 1), "https://example.com/feed.xml");
        s.raw_json = Some(
            r#"{"sortUrl":"列表::https://example.com/list\n详情::https://example.com/detail"}"#
                .into(),
        );
        assert_eq!(build_feed_url(&s, 1), "https://example.com/list");
        // 前端指定分类段（legacy `名称::地址` 格式）→ 用该段
        assert_eq!(
            build_feed_url_for(&s, 1, Some("详情::https://example.com/detail")),
            "https://example.com/detail"
        );
        // 前端直接传完整 URL（含 {{page}}）→ 替换页码
        assert_eq!(
            build_feed_url_for(&s, 2, Some("https://example.com/page/{{page}}.xml")),
            "https://example.com/page/2.xml"
        );
        assert_eq!(
            build_feed_url_for(&s, 3, Some("分页::https://example.com/page/{{page}}.xml")),
            "https://example.com/page/3.xml"
        );
        // 无 :: 的段被丢弃 → 回退 sourceUrl（legacy sortUrls 语义）
        s.raw_json = Some(r#"{"sortUrl":"https://example.com/page/{{page}}.xml"}"#.into());
        assert_eq!(build_feed_url(&s, 3), "https://example.com/feed.xml");
        // 带 :: 且含 {{page}} → 替换页码
        s.raw_json = Some(r#"{"sortUrl":"分页::https://example.com/page/{{page}}.xml"}"#.into());
        assert_eq!(build_feed_url(&s, 3), "https://example.com/page/3.xml");
        // sourceUrl 本身含 {{page}} → 替换
        s.raw_json = None;
        s.source_url = "https://example.com/feed/{{page}}.xml".into();
        assert_eq!(build_feed_url(&s, 2), "https://example.com/feed/2.xml");
    }

    /// 正文提取：优先常见容器，忽略脚本/样式，按行合并
    #[test]
    fn test_extract_web_content_selectors() {
        let html = r#"<html><head><style>.x{}</style></head><body>
            <nav>导航</nav>
            <article class="article-content">
                <h1>标题</h1>
                <p>第一段</p>
                <p>第二段</p>
            </article>
            <footer>页脚</footer>
        </body></html>"#;
        let text = extract_web_content(html);
        assert!(text.contains("第一段"));
        assert!(text.contains("第二段"));
        assert!(!text.contains("导航"), "容器外文本不应混入");
        assert!(!text.contains("页脚"));
        assert!(!text.contains("style"), "style 文本不应混入");
        // 无正文容器 → body 兜底
        let bare = extract_web_content("<html><body><p>只有一段</p></body></html>");
        assert_eq!(bare, "只有一段");
        // 完全无文本
        assert_eq!(
            extract_web_content("<html><body><script>var a=1;</script></body></html>"),
            ""
        );
    }

    /// 微型 HTTP 服务器：固定响应体（同 book/crawler 测试模式）
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

    /// RssParserByRule：ruleArticles 定位 + 标题/链接/时间/摘要/配图规则提取，相对链接绝对化
    #[test]
    fn test_parse_feed_by_rule_extracts_articles() {
        let xml = r#"<div class="feed">
            <div class="item">
                <h2 class="t">文章A</h2>
                <a class="l" href="/a">链接</a>
                <span class="d">2025-01-01 10:00:00</span>
                <p class="desc">摘要A</p>
                <img class="i" src="/img-a.jpg">
            </div>
            <div class="item">
                <h2 class="t">文章B</h2>
                <a class="l" href="/b">链接</a>
                <span class="d">2025-01-02 08:30:00</span>
                <p class="desc">摘要B</p>
            </div>
        </div>"#;
        let mut s = source();
        s.raw_json = Some(
            r#"{"ruleArticles":"div.item","ruleTitle":"h2.t@text","ruleLink":"a.l@href",
               "rulePubDate":"span.d@text","ruleDescription":"p.desc@text","ruleImage":"img.i@src"}"#
                .into(),
        );
        let articles = parse_feed(xml, &s).expect("规则解析应成功");
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].title, "文章A");
        assert_eq!(articles[0].url, "https://example.com/a");
        assert_eq!(articles[0].time, 1735725600000, "2025-01-01 10:00 UTC");
        assert_eq!(articles[0].content.as_deref(), Some("摘要A"));
        assert_eq!(
            articles[0].cover.as_deref(),
            Some("https://example.com/img-a.jpg")
        );
        assert_eq!(articles[1].title, "文章B");
        assert_eq!(articles[1].cover, None, "无配图规则命中 → None");
    }

    /// RssParserByRule：`-` 前缀列表规则倒序
    #[test]
    fn test_parse_feed_by_rule_reverse() {
        let xml = r#"<div class="item"><h2>文章一</h2><a href="/1">1</a></div>
                      <div class="item"><h2>文章二</h2><a href="/2">2</a></div>"#;
        let mut s = source();
        s.raw_json = Some(
            r#"{"ruleArticles":"-div.item","ruleTitle":"h2@text","ruleLink":"a@href"}"#.into(),
        );
        let articles = parse_feed(xml, &s).expect("解析应成功");
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].title, "文章二", "`-` 前缀应倒序");
        assert_eq!(articles[1].title, "文章一");
    }

    /// rulePubDate 宽松解析：RFC 2822 / RFC 3339 / 常见日期时间 / 时间戳
    #[test]
    fn test_parse_rss_datetime_formats() {
        assert_eq!(
            parse_rss_datetime("Wed, 01 Jan 2025 00:00:00 GMT"),
            1735689600000
        );
        assert_eq!(parse_rss_datetime("2025-01-02T08:30:00Z"), 1735806600000);
        assert_eq!(parse_rss_datetime("2025-03-01 12:00:00"), 1740830400000);
        assert_eq!(parse_rss_datetime("1735689600"), 1735689600000);
        assert_eq!(parse_rss_datetime("1735689600000"), 1735689600000);
        assert_eq!(parse_rss_datetime("2025-04-05"), 1743811200000);
        assert_eq!(parse_rss_datetime("无法解析"), 0);
        assert_eq!(parse_rss_datetime(""), 0);
    }

    /// ruleContent 规则提取文章正文（优先于 CSS 启发式）
    #[tokio::test]
    async fn test_fetch_article_content_rule() {
        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);
        let base = serve(
            r#"<html><article class="content"><p>规则正文内容</p><script>干扰</script></article></html>"#,
        )
        .await;
        let mut s = source();
        s.raw_json = Some(r#"{"ruleContent":"article.content@text"}"#.into());
        let content = fetch_article_content(&s, &format!("{base}/article/1"))
            .await
            .unwrap();
        assert_eq!(content, "规则正文内容");
    }
}
