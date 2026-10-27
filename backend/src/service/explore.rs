//! 发现/探索（ruleExplore）：exploreUrl 集合 + 书单解析
//!
//! 对齐 legacy WebBook.exploreBook：URL 列表 → 抓取 → ruleExplore 字段 → SearchBook

use anyhow::Result;

use crate::model::BookSource;
use crate::service::crawler;
use crate::service::search::SearchBook;

/// 探索条目（title + url）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExploreEntry {
    pub title: String,
    pub url: String,
    /// book=书单分类（探索加载）/ link=外部链接（点击打开）
    #[serde(default)]
    pub r#type: String,
}

/// 判断分类类型：外部链接（群/导入/渠道/发布等）vs 书单
fn entry_type(title: &str, url: &str) -> String {
    let t = title.to_lowercase();
    let keywords = [
        "导入",
        "群",
        "发布",
        "渠道",
        "交流",
        "更新",
        "关注",
        "频道",
        "公众号",
    ];
    if keywords.iter().any(|k| t.contains(k)) {
        return "link".to_string();
    }
    let domains = [
        "qm.qq.com",
        "bilibili.com",
        "mp.weixin.qq.com",
        "shuyuan-api",
        "yckceo.com",
        "t.me",
    ];
    if domains.iter().any(|d| url.contains(d)) {
        return "link".to_string();
    }
    "book".to_string()
}

/// 解析 exploreUrl（legado 语义）：
/// - `@js:代码`：执行 JS（返回 JSON.stringify([{title,url},...])）→ 解析条目
/// - 普通多行 URL：每行一个条目（title 从 URL 尾部提取）
#[allow(clippy::collapsible_match)] // 先解包 Ok 再解包 Array，分层写更清楚
pub fn parse_explore_entries(explore_url: &str) -> Vec<ExploreEntry> {
    let mut entries = Vec::new();
    let lines: Vec<&str> = explore_url.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        // @js: 格式：同行（@js:代码）或独立行（@js: 后所有行为代码——legado 常见）
        if line == "@js:" || line.starts_with("@js:") {
            let code = if line == "@js:" {
                // 独立行：后续所有行拼接为代码
                let rest = &lines[i + 1..];
                i = lines.len();
                rest.join(
                    "
",
                )
            } else {
                i += 1;
                line[4..].to_string()
            };
            // eval 直接取结构化结果（数组/对象递归 JSON 转换——避免 ToString 的
            // "[object Object]" 导致条目解析为空；JSON.stringify 字符串出口自动解析）
            if let Ok(list) = crate::parser::js::eval_js_json_with_bridge(
                &code,
                &Default::default(),
                &crate::parser::js::JsBridge::default(),
            ) {
                if let serde_json::Value::Array(items) = list {
                    for item in items {
                        let title = item
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let url = item
                            .get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !url.is_empty() {
                            entries.push(ExploreEntry {
                                title: title.clone(),
                                url: url.clone(),
                                r#type: entry_type(&title, &url),
                            });
                        }
                    }
                    i = lines.len();
                    continue;
                }
            }
            continue;
        }
        // JSON 数组格式：[{"title":"...","url":"..."}, ...]（inline 或跨行）
        if line.starts_with('[') || line.starts_with('{') {
            // 收集到匹配的 ]（多行 JSON）
            let mut json_str = line.to_string();
            let mut j = i + 1;
            while !json_str.trim_end().ends_with(']') && j < lines.len() {
                json_str.push('\n');
                json_str.push_str(lines[j]);
                j += 1;
            }
            i = j;
            if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
                for item in list {
                    let title = item
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let url = item
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !url.is_empty() {
                        entries.push(ExploreEntry {
                            title: title.clone(),
                            url: url.clone(),
                            r#type: entry_type(&title, &url),
                        });
                    }
                }
                continue;
            }
            continue;
        }
        // "标题::URL" 格式（legado 常见）
        if let Some((title, url)) = line.split_once("::") {
            let title = title.trim().to_string();
            let url = url.trim().to_string();
            if !url.is_empty() {
                entries.push(ExploreEntry {
                    title: title.clone(),
                    url: url.clone(),
                    r#type: entry_type(&title, &url),
                });
                i += 1;
                continue;
            }
        }
        // 普通 URL 行：title 从尾部提取
        let title = url_title(line);
        entries.push(ExploreEntry {
            title: title.clone(),
            url: line.to_string(),
            r#type: entry_type(&title, line),
        });
        i += 1;
    }
    entries
}

/// 从 URL 提取分类名（尾部路径段/查询参数，解码）
fn url_title(url: &str) -> String {
    let cleaned = url.split(['?', '&', '#']).next().unwrap_or(url);
    let seg = cleaned
        .trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(cleaned);
    let decoded = percent_decode(seg);
    if !decoded.is_empty() && decoded != "/" {
        return decoded;
    }
    // 查询参数 name/type/id
    for param in ["name", "type", "id"] {
        for pair in url.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == param && !v.is_empty() {
                    return percent_decode(v);
                }
            }
        }
    }
    url.to_string()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 构造分页探索 URL（GAP #51：服务端解析书源规则分页变量 {{page}}/{page}）
pub fn build_explore_url(url: &str, page: i64) -> String {
    url.replace("{{page}}", &page.to_string())
        .replace("{page}", &page.to_string())
}

/// 单页发现：抓取 + 解析（复用搜索的 SearchRule 语义）
///
/// GAP #51：page 参数由服务端替换书源分页变量（{{page}}/{page}，URL 与 POST body）
pub async fn explore_url(
    ns: &str,
    url: &str,
    page: i64,
    source: &BookSource,
) -> Result<Vec<SearchBook>> {
    // URL 模板（{{page}}/{page}）→ 页码
    let url = build_explore_url(url, page);
    // 相对 URL 拼书源 baseUrl (legado 语义: 带/前缀与不带前缀的相对路径都要绝对化)
    let raw_url =
        if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("//") {
            url.to_string()
        } else {
            let base = source
                .book_source_url
                .split("##")
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            if url.starts_with('/') {
                format!("{base}{url}")
            } else {
                format!("{base}/{url}")
            }
        };
    // URL 后缀（,{...}：charset/method/body——对齐搜索链路）
    let (final_url, suffix) = crate::service::search::split_url_suffix(&raw_url);
    let mut headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();
    if let Some(extra) = &suffix.headers {
        for (k, v) in extra {
            headers.insert(k.clone(), v.clone());
        }
    }
    // legado concurrentRate：发现请求前限速（A2 共享滑窗/间隔）
    crate::service::search::concurrent_rate_acquire(ns, source).await;
    let post_body = suffix.body.as_ref().map(|b| build_explore_url(b, page));
    // 书源抓取（自动带书源 cookie——按用户命名空间）
    let method = suffix.method.as_deref().unwrap_or("GET");
    // A1 webView：探索 URL option webView=true 时经浏览器渲染（失败回退 HTTP）
    let resp = if suffix.web_view == Some(true) {
        match crate::service::browser::solve_cf_challenge(
            ns,
            &final_url,
            &[],
            15_000,
            source.proxy_url.as_deref(),
        )
        .await
        {
            Ok(sol) => Ok(crawler::FetchResponse {
                body: sol.html,
                url: final_url.clone(),
                headers: Vec::new(),
                status: 200,
            }),
            Err(e) => {
                tracing::warn!("webView 探索渲染失败，回退 HTTP [{final_url}]: {e}");
                if method.eq_ignore_ascii_case("POST") {
                    crawler::http_post_retry(
                        ns,
                        &final_url,
                        &headers,
                        15,
                        post_body.as_deref(),
                        suffix.charset.as_deref(),
                        source.proxy_url.as_deref(),
                        suffix.retry,
                    )
                    .await
                } else {
                    crawler::http_get_retry(
                        ns,
                        &final_url,
                        &headers,
                        15,
                        suffix.charset.as_deref(),
                        source.proxy_url.as_deref(),
                        suffix.retry,
                    )
                    .await
                }
                .map_err(|e| anyhow::anyhow!("抓取失败（{}）: {}", final_url, e))
            }
        }
    } else if method.eq_ignore_ascii_case("POST") {
        crawler::http_post_retry(
            ns,
            &final_url,
            &headers,
            15,
            post_body.as_deref(),
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await
    } else {
        crawler::http_get_retry(
            ns,
            &final_url,
            &headers,
            15,
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await
    }
    .map_err(|e| anyhow::anyhow!("抓取失败（{}）: {}", final_url, e))?;
    // legado WebBook.exploreBook：发现页抓取后执行 loginCheckJs
    let body =
        crate::service::book::apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;

    // legado BookList：explore ruleBookList 为空 → 回退 ruleSearch
    let rule = explore_rule(source);
    // legado BookList：响应 URL 匹配 bookUrlPattern → 按详情页规则解析为单本
    if let Some(pat) = source
        .book_url_pattern
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        let matched = crate::util::regex::Regex::new(pat)
            .map(|r| r.is_match(&resp.url))
            .unwrap_or(false);
        if matched {
            let info =
                crate::service::book::analyze_book_info(ns, &body, &resp.url, source, &url, None);
            if !info.name.is_empty() {
                return Ok(vec![crate::service::search::single_search_book(
                    info, source, &url,
                )]);
            }
        }
    }
    let Some(book_list_rule) = rule.book_list.clone() else {
        return Ok(vec![]);
    };
    let books = crate::service::search::analyze_book_list_for_explore(
        ns,
        &body,
        &resp.url,
        source,
        &rule,
        &book_list_rule,
    );
    Ok(books)
}

/// 探索规则：ruleExplore.bookList 为空时回退 ruleSearch（legacy BookList 语义）
fn explore_rule(source: &BookSource) -> crate::service::search::SearchRule {
    let explore: crate::service::search::SearchRule = source
        .rule_explore
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if explore
        .book_list
        .as_deref()
        .is_some_and(|r| !r.trim().is_empty())
    {
        return explore;
    }
    source
        .rule_search
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// GAP 141：内置探索源清单（JSON 原文，与 bookSource.json 同构——可直接 saveBookSources 导入）
///
/// 验证状态（2026-08 网络实测）：
/// - 新笔趣阁 biquge365.net：探索分类 /sort/{n}_1/、书籍详情、章节目录 ul.info、正文 div.txt 单页全流程可用
/// - 看泡书屋 kpshu.cc：探索分类 /list{n}/、搜索 /search.php、书籍详情、目录 div.book_list、正文 article.font_max
///   （章节正文分页时仅取首页——nextContentUrl 缺失避免串章；站点结构变更时用户可自行修正规则）
pub const BUILTIN_EXPLORE_SOURCES: &[&str] = &[
    // 新笔趣阁（biquge365.net）
    r#"{
  "bookSourceUrl": "https://www.biquge365.net",
  "bookSourceName": "新笔趣阁（内置）",
  "bookSourceGroup": "内置探索",
  "enabled": true,
  "enabledExplore": true,
  "customOrder": 900,
  "exploreUrl": "玄幻魔法::https://www.biquge365.net/sort/1_1/\n仙侠修真::https://www.biquge365.net/sort/2_1/\n都市言情::https://www.biquge365.net/sort/3_1/\n网游动漫::https://www.biquge365.net/sort/4_1/\n科幻小说::https://www.biquge365.net/sort/5_1/\n恐怖灵异::https://www.biquge365.net/sort/6_1/\n历史军事::https://www.biquge365.net/sort/7_1/\n其他小说::https://www.biquge365.net/sort/8_1/",
  "searchUrl": "https://www.biquge365.net/s.php,{method:POST,body:type=articlename&s={{key}}}",
  "ruleExplore": {
    "bookList": "ul.gengxin@li",
    "name": "span.name a@text",
    "author": "span.zuo@text",
    "bookUrl": "span.name a@href"
  },
  "ruleSearch": {
    "bookList": "ul.search li:not(.fen)@li",
    "name": "span.name a@text",
    "author": "span.zuo@text",
    "bookUrl": "span.name a@href"
  },
  "ruleBookInfo": {
    "name": "h1@text",
    "author": "div.xinxi span.x1 a@text",
    "kind": "div.xinxi span.x1.1@text",
    "intro": "div.x3@text",
    "coverUrl": "div.zhutu img@src",
    "tocUrl": "div.gongneng a@href"
  },
  "ruleToc": {
    "chapterList": "ul.info@li",
    "chapterName": "a@text",
    "chapterUrl": "a@href"
  },
  "ruleContent": {
    "content": "div.txt@html",
    "replaceRegex": "一秒记住【笔趣阁】.*?！|（请记住看台湾小说认准台湾小说网.*?）##"
  }
}"#,
    // 看泡书屋（kpshu.cc）
    r#"{
  "bookSourceUrl": "http://www.kpshu.cc",
  "bookSourceName": "看泡书屋（内置）",
  "bookSourceGroup": "内置探索",
  "enabled": true,
  "enabledExplore": true,
  "customOrder": 901,
  "exploreUrl": "玄幻小说::http://www.kpshu.cc/list1/\n武侠小说::http://www.kpshu.cc/list2/\n都市小说::http://www.kpshu.cc/list3/\n历史小说::http://www.kpshu.cc/list4/\n网游小说::http://www.kpshu.cc/list5/\n科幻小说::http://www.kpshu.cc/list6/\n言情小说::http://www.kpshu.cc/list7/\n其他小说::http://www.kpshu.cc/list8/",
  "searchUrl": "http://www.kpshu.cc/search.php?q={{key}}&p=1",
  "ruleExplore": {
    "bookList": "div.row dl@dl",
    "name": "dd h3 a@text",
    "author": "dd.book_other span a@text",
    "bookUrl": "dd h3 a@href",
    "coverUrl": "dt a img@src"
  },
  "ruleSearch": {
    "bookList": "div.row dl@dl",
    "name": "dd h3 a@text",
    "author": "dd.book_other span a@text",
    "bookUrl": "dd h3 a@href",
    "coverUrl": "dt a img@src"
  },
  "ruleBookInfo": {
    "name": "h1@text",
    "author": "div.options li a@text",
    "intro": "div.intro@text",
    "coverUrl": "div.book_info img@src"
  },
  "ruleToc": {
    "chapterList": "div.book_list ul.row@li",
    "chapterName": "a@text",
    "chapterUrl": "a@href"
  },
  "ruleContent": {
    "content": "article.font_max@html"
  }
}"#,
];

/// GAP 141：解析内置探索源 → BookSource 列表（JSON 非法项跳过并告警）
pub fn builtin_explore_sources() -> Vec<crate::model::BookSource> {
    let mut out = Vec::with_capacity(BUILTIN_EXPLORE_SOURCES.len());
    for raw in BUILTIN_EXPLORE_SOURCES {
        match serde_json::from_str::<crate::model::BookSource>(raw) {
            Ok(mut s) => {
                s.raw_json = Some((*raw).to_string());
                out.push(s);
            }
            Err(e) => tracing::warn!("内置探索源解析失败（跳过）: {e}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_urls() {
        let urls = "https://a.com/list\n#注释\nhttps://b.com/{{page}}\n";
        let parsed = parse_explore_entries(urls);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[1].url.contains("{{page}}"));
        // @js: 代码行生成条目
        let js = "@js:JSON.stringify([{title:'分类A',url:'https://a.com/x'}])";
        let parsed = parse_explore_entries(js);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "分类A");
    }

    /// exploreUrl JS 返回数组字面量（非 JSON.stringify 字符串）——此前 ToString
    /// 输出 "[object Object]" 导致条目解析为空
    #[test]
    fn test_parse_js_entries_array_literal() {
        let js =
            "@js:[{title:'分类X',url:'https://a.com/x'},{title:'分类Y',url:'https://a.com/y'}]";
        let parsed = parse_explore_entries(js);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].title, "分类X");
        assert_eq!(parsed[0].url, "https://a.com/x");
        assert_eq!(parsed[1].title, "分类Y");
        assert_eq!(parsed[1].url, "https://a.com/y");
        // JSON.parse 数组出口
        let js = "@js:JSON.parse('[{\"title\":\"类P\",\"url\":\"https://a.com/p\"}]')";
        let parsed = parse_explore_entries(js);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "类P");
        // 无 url 条目丢弃
        let js = "@js:[{title:'空',url:''}]";
        assert!(parse_explore_entries(js).is_empty());
    }

    /// GAP #51：分页变量替换（{{page}}/{page} 双格式，URL 与 POST body 一致）
    #[test]
    fn test_build_explore_url_page() {
        assert_eq!(
            build_explore_url("https://a.com/list/{{page}}", 3),
            "https://a.com/list/3"
        );
        assert_eq!(
            build_explore_url("https://a.com/list/{page}", 2),
            "https://a.com/list/2"
        );
        assert_eq!(
            build_explore_url("https://a.com/list?p={{page}}", 7),
            "https://a.com/list?p=7"
        );
        // 无占位符：原样返回
        assert_eq!(
            build_explore_url("https://a.com/list", 5),
            "https://a.com/list"
        );
    }

    /// GAP 141：内置探索源——JSON 合法、探索入口可解析且非空、ruleExplore/ruleToc/
    /// ruleContent 规则完整（bookList 定位 + 字段规则），可直接导入书源库使用
    #[test]
    fn test_builtin_explore_sources_parse() {
        let sources = builtin_explore_sources();
        assert!(
            sources.len() >= 2,
            "内置探索源应 >= 2 个（当前 {}）",
            sources.len()
        );
        for s in &sources {
            assert!(!s.book_source_url.is_empty(), "书源 URL 必填");
            assert!(!s.book_source_name.is_empty());
            assert!(s.enabled);
            assert!(s.enabled_explore, "探索源应启用 enabledExplore");
            // 探索入口：解析出条目且非空
            let entries = parse_explore_entries(s.explore_url.as_deref().unwrap_or(""));
            assert!(
                entries.len() >= 4,
                "{} 探索分类应 >= 4（当前 {}）",
                s.book_source_name,
                entries.len()
            );
            assert!(entries.iter().all(|e| e.r#type == "book"), "分类应为书单型");
            // 规则完整性：ruleExplore bookList + 字段规则；ruleToc/ruleContent 齐备
            let explore: crate::service::search::SearchRule =
                serde_json::from_value(s.rule_explore.clone().unwrap()).unwrap();
            assert!(explore.book_list.is_some(), "ruleExplore.bookList 必填");
            assert!(
                explore.name.is_some() && explore.book_url.is_some(),
                "name/bookUrl 字段规则必填"
            );
            let toc: crate::service::book::TocRule =
                serde_json::from_value(s.rule_toc.clone().unwrap()).unwrap();
            assert!(
                toc.chapter_list.is_some()
                    && toc.chapter_name.is_some()
                    && toc.chapter_url.is_some()
            );
            let content: crate::service::book::ContentRule =
                serde_json::from_value(s.rule_content.clone().unwrap()).unwrap();
            assert!(content.content.is_some(), "ruleContent.content 必填");
        }
    }

    /// legado BookList：ruleExplore.bookList 为空 → 回退 ruleSearch
    #[test]
    #[allow(clippy::field_reassign_with_default)] // 先 Default 再按条件覆盖字段，比一次性构造更贴近可读的分支逻辑
    fn test_explore_rule_falls_back_to_search() {
        let mut source = crate::model::BookSource::default();
        source.rule_explore = Some(serde_json::json!({ "bookList": "" }));
        source.rule_search = Some(serde_json::json!({ "bookList": "ul.list li" }));
        let rule = explore_rule(&source);
        assert_eq!(rule.book_list.as_deref(), Some("ul.list li"));

        // ruleExplore 有 bookList 时优先用探索规则
        source.rule_explore = Some(serde_json::json!({ "bookList": "div.explore li" }));
        let rule = explore_rule(&source);
        assert_eq!(rule.book_list.as_deref(), Some("div.explore li"));

        // 两边都空 → 默认空规则
        source.rule_explore = None;
        source.rule_search = None;
        assert!(explore_rule(&source).book_list.is_none());
    }

    /// bookUrlPattern 单详情：BookInfo → SearchBook 字段映射（legacy Book.toSearchBook）
    #[test]
    #[allow(clippy::field_reassign_with_default)] // 先 Default 再按条件覆盖字段，比一次性构造更贴近可读的分支逻辑
    fn test_single_search_book_mapping() {
        let mut source = crate::model::BookSource::default();
        source.book_source_url = "https://a.com".into();
        source.book_source_name = "A源".into();
        source.custom_order = 7;
        source.book_source_type = 0;
        let info = crate::model::book_chapter::BookInfo {
            name: "书名".into(),
            author: "作者".into(),
            kind: Some("玄幻".into()),
            intro: Some("简介".into()),
            cover_url: Some("https://a.com/c.jpg".into()),
            word_count: Some("100万".into()),
            latest_chapter_title: Some("第1章".into()),
            toc_url: Some("https://a.com/toc".into()),
            book_url: "https://a.com/book/1".into(),
            origin: "https://a.com".into(),
            origin_name: "A源".into(),
            book_type: 0,
            ..Default::default()
        };
        let book =
            crate::service::search::single_search_book(info, &source, "https://a.com/book/1");
        assert_eq!(book.name, "书名");
        assert_eq!(book.author, "作者");
        assert_eq!(book.kind.as_deref(), Some("玄幻"));
        assert_eq!(book.cover_url.as_deref(), Some("https://a.com/c.jpg"));
        assert_eq!(book.toc_url, "https://a.com/toc");
        assert_eq!(book.origin, "https://a.com");
        assert_eq!(book.origin_name, "A源");
        assert_eq!(book.origin_order, 7);
        assert_eq!(book.book_url, "https://a.com/book/1");
        assert_eq!(book.book_type, 0);
    }
}
