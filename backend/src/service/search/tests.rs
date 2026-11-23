//! 搜索模块单元测试

use std::collections::HashMap;
use std::time::Duration;

use crate::model::BookSource;
use crate::parser::js::JsBridge;
use crate::parser::rule::{resolve_get, RuleVars};

use super::engine::{apply_body_js, deadline_from_secs, SEARCH_SOURCE_DEADLINE_DEFAULT_SECS};
use super::extract::analyze_book_list;
use super::*;

/// 任务 #2：deadline env 语义（0 = 关闭；其余 clamp(5,120)；未设置走默认 60）
#[test]
fn test_search_source_deadline_semantics() {
    assert_eq!(deadline_from_secs(0), None, "0 = 关闭");
    assert_eq!(deadline_from_secs(60), Some(Duration::from_secs(60)));
    assert_eq!(
        deadline_from_secs(1),
        Some(Duration::from_secs(5)),
        "过小值抬到 5s 下限"
    );
    assert_eq!(
        deadline_from_secs(999),
        Some(Duration::from_secs(120)),
        "过大值压到 120s 上限"
    );
    assert_eq!(SEARCH_SOURCE_DEADLINE_DEFAULT_SECS, 60);
}

/// E8：字段清洗四件套（legacy formatBookName/Author/wordCountFormat/kind 归一）
#[test]
fn test_field_cleaning_helpers() {
    // nameRegex：剔除作者尾巴
    assert_eq!(format_book_name("诡秘之主 作者：爱潜水的乌贼"), "诡秘之主");
    assert_eq!(format_book_name("某书 乌贼 著"), "某书");
    assert_eq!(format_book_name("  正常书名  "), "正常书名");
    assert_eq!(format_book_name(""), "");
    // authorRegex：「作者：」前缀 / 「著」后缀
    assert_eq!(format_book_author("作者：乌贼"), "乌贼");
    assert_eq!(format_book_author("作 者 : 乌贼"), "乌贼");
    assert_eq!(format_book_author("乌贼 著"), "乌贼");
    assert_eq!(format_book_author("普通作者"), "普通作者");
    // wordCountFormat：纯数字 → N字 / 万字；非数字原样；0 → 空
    assert_eq!(word_count_format("123"), "123字");
    assert_eq!(word_count_format("150000"), "15万字");
    assert_eq!(word_count_format("1234567"), "123.5万字");
    assert_eq!(word_count_format("20000"), "2万字", "尾零去除");
    assert_eq!(word_count_format("0"), "", "0 → 空");
    assert_eq!(word_count_format("约三万"), "约三万");
    assert_eq!(word_count_format(""), "");
    // kind 多值归一：分隔符统一为半角逗号、去空段
    assert_eq!(normalize_kind_list("玄幻, 冒险；科幻"), "玄幻,冒险,科幻");
    assert_eq!(normalize_kind_list("a;;b，，c"), "a,b,c");
    assert_eq!(normalize_kind_list("单值"), "单值");
    assert_eq!(normalize_kind_list(" , , "), "");
}

/// E1/E2：URL 模板 {{js}} 展开（legacy replaceKeyPageJs）+ page 数值语义
#[test]
fn test_expand_url_js_templates() {
    let bridge = crate::parser::js::JsBridge::default();
    let headers = HashMap::new();
    // 数值算术：page+1 = 2（非 "11"）
    assert_eq!(
        expand_url_js_templates(
            "https://a.com/x?p={{page+1}}",
            "k",
            1,
            "https://a.com",
            &headers,
            &bridge
        ),
        "https://a.com/x?p=2"
    );
    // 标准 JS 内建可用（encodeURI）
    assert_eq!(
        expand_url_js_templates(
            "https://a.com/s?q={{encodeURI(key)}}&p={{page}}",
            "abc def",
            3,
            "https://a.com",
            &headers,
            &bridge
        ),
        "https://a.com/s?q=abc%20def&p=3"
    );
    // java.* 桥在模板内可用
    assert_eq!(
        expand_url_js_templates(
            "https://a.com/t?m={{java.md5Encode16('abc')}}",
            "k",
            1,
            "https://a.com",
            &headers,
            &bridge
        ),
        "https://a.com/t?m=3cd24fb0d6963f7d"
    );
    // 求值失败 → 原样保留（安全回退）
    let bad = "https://a.com/x?z={{noSuchFnZZZ()}}";
    assert_eq!(
        expand_url_js_templates(bad, "k", 1, "https://a.com", &headers, &bridge),
        bad
    );
    // 无闭合 {{ → 剩余文本原样保留
    let open = "https://a.com/x?q={{abc";
    assert_eq!(
        expand_url_js_templates(open, "k", 1, "https://a.com", &headers, &bridge),
        open
    );
    // 多个表达式混合展开
    assert_eq!(
        expand_url_js_templates(
            "https://a.com/{{key}}/p{{page+2}}/s{{page*3}}",
            "书名",
            1,
            "https://a.com",
            &headers,
            &bridge
        ),
        "https://a.com/书名/p3/s3"
    );
}

/// build_request_url 集成：{{js}} 先于字面替换执行
#[test]
fn test_build_request_url_expands_js_template() {
    let bridge = crate::parser::js::JsBridge::default();
    let headers = HashMap::new();
    let (url, _suffix) = build_request_url(
        "https://a.com/search?q={{key}}&p={{page+1}}",
        "斗罗",
        4,
        "https://a.com",
        &headers,
        &bridge,
    )
    .unwrap();
    assert_eq!(url, "https://a.com/search?q=斗罗&p=5");
}

#[test]
fn test_field_url_with_vars_resolves_get() {
    let mut vars = RuleVars::new();
    vars.insert("bid".to_string(), "abc".to_string());
    assert_eq!(
        resolve_get("https://x.test/c/@get:{bid}/x", &vars),
        "https://x.test/c/abc/x"
    );
    let out = field_url_with_vars(
        r#"{"u":"/c/1"}"#,
        Some("https://x.test/c/@get:{bid}{{$.u}}"),
        "",
        "http://base",
        &mut vars,
    );
    assert_eq!(
        out, "https://x.test/c/abc/c/1",
        "URL 字段应替换 @get 与 {{}}"
    );
}

#[test]
fn test_build_url_double_brace() {
    let u = build_search_url(
        "/novel/search?q={{key}}&p={{page}}",
        "诡秘",
        2,
        "https://a.com",
    );
    assert_eq!(u, "https://a.com/novel/search?q=诡秘&p=2");
}

#[test]
fn test_build_url_single_brace() {
    let u = build_search_url("https://a.com/s?k={key}", "测试", 1, "https://a.com");
    assert_eq!(u, "https://a.com/s?k=测试");
}

#[test]
fn test_build_url_page_picker() {
    let u = build_search_url("https://a.com/<1,2,3>", "x", 2, "https://a.com");
    assert_eq!(u, "https://a.com/2");
}

#[test]
fn test_absolute() {
    assert_eq!(to_absolute("/b/1", "https://a.com"), "https://a.com/b/1");
    assert_eq!(
        to_absolute("https://x.com/b", "https://a.com"),
        "https://x.com/b"
    );
}

#[test]
fn test_analyze_real_json_list() {
    // 真实猫眼 JSON（15 条）+ 真实规则
    let body = match std::fs::read_to_string("target/cat-eye.json") {
        Ok(b) => b,
        Err(_) => return, // 无测试数据时跳过
    };
    let rule: SearchRule = serde_json::from_value(serde_json::json!({
        "bookList": "$.data[*]", "name": "$.novelName", "author": "$.authorName",
        "intro": "$.summary", "bookUrl": "/novel/{{$.novelId}}?isSearch=1",
        "coverUrl": "$.cover", "wordCount": "$.wordNum"
    }))
    .unwrap();
    // 中间环节：bookList 提取
    let items = crate::parser::rule::apply("$.data[*]", &body);
    println!("bookList items: {}", items.len());
    if let Some(first) = items.first() {
        println!(
            "首项前 100: {}",
            first.chars().take(100).collect::<String>()
        );
    }
    // 直接测字段规则
    let name = field(&items[0], Some("$.novelName"), "");
    println!("field('$.novelName') = {:?}", name);
    let book_url = field(&items[0], Some("/novel/{{$.novelId}}?isSearch=1"), "");
    println!("field(bookUrl 内嵌) = {:?}", book_url);
    let src = BookSource {
        book_source_url: "http://api.jmlldsc.com".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        &body,
        "http://api.jmlldsc.com",
        &src,
        &rule,
        "$.data[*]",
        "诡秘之主",
        &JsBridge::default(),
    );
    println!("真实 JSON 解析: {} 本", books.len());
    assert!(!books.is_empty(), "真实数据解析为空");
    assert_eq!(books[0].name, "诡秘之主");
    assert!(
        books[0].book_url.contains("bY7oM0"),
        "bookUrl 内嵌规则: {}",
        books[0].book_url
    );
}

#[test]
fn test_analyze_empty_list_falls_back_to_detail() {
    let mut src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    src.rule_search = Some(serde_json::json!({
        "bookList": "div.none",
        "name": "h1@text"
    }));
    src.rule_book_info = Some(serde_json::json!({
        "name": "h1@text",
        "author": "p@text",
        "tocUrl": "/toc"
    }));
    let rule: SearchRule = serde_json::from_value(src.rule_search.clone().unwrap()).unwrap();
    let html = r#"<h1>书名</h1><p>作者</p>"#;
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com/book/1",
        &src,
        &rule,
        "div.none",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 1, "列表为空应按详情页解析单本");
    assert_eq!(books[0].name, "书名");
    assert_eq!(books[0].author, "作者");
    assert_eq!(books[0].toc_url, "https://a.com/toc");
    assert_eq!(books[0].book_url, "https://a.com/book/1");
}

/// legado BookList：响应 URL 匹配 bookUrlPattern → 直接按详情页解析单本
#[test]
fn test_analyze_single_detail_by_url_pattern() {
    let mut src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    src.book_url_pattern = Some(r"^https://a\.com/book/\d+$".into());
    src.rule_book_info = Some(serde_json::json!({
        "name": "h1@text",
        "author": "p@text",
        "coverUrl": "img@src",
        "tocUrl": "/toc"
    }));
    let rule: SearchRule = SearchRule::default();
    let html = r#"<h1>书名</h1><p>作者</p><img src="/cover.jpg">"#;
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com/book/42",
        &src,
        &rule,
        "div.none",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 1, "bookUrlPattern 命中应按详情页解析");
    assert_eq!(books[0].name, "书名");
    assert_eq!(books[0].author, "作者");
    assert_eq!(books[0].book_url, "https://a.com/book/42");
    assert_eq!(books[0].toc_url, "https://a.com/toc");
    assert_eq!(
        books[0].cover_url.as_deref(),
        Some("https://a.com/cover.jpg")
    );
}

/// legacy getSearchItem：bookUrl 规则结果为空时回退 baseUrl，而不是丢弃条目
#[test]
fn test_analyze_item_empty_book_url_falls_back_base() {
    let mut src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    src.rule_search = Some(serde_json::json!({
        "bookList": "div.book",
        "name": "h2@text",
        "author": "p@text"
    }));
    let rule: SearchRule = serde_json::from_value(src.rule_search.clone().unwrap()).unwrap();
    let html = r#"<div class="book"><h2>书名</h2><p>作者</p></div>"#;
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com/list",
        &src,
        &rule,
        "div.book",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].book_url, "https://a.com/list");
}

/// E10：搜索条目内 src = 当前条目 HTML（legacy AnalyzeRule.kt:661
/// bindings["src"]=content——逐条目解析时 item_html 即"当前文档"）：
/// 条目字段的 <js> 链与 {{js}} 内嵌均可取到当前书元素 HTML（逐条目不同）
#[test]
fn test_analyze_item_src_binding() {
    let html = r#"<div class="book"><h2>书名甲</h2></div><div class="book"><h2>书名乙</h2></div>"#;
    let rule = SearchRule {
        book_list: Some("div.book".into()),
        // <js> 链：src=当前条目 HTML → 长度后缀随条目变化
        name: Some("h2@text<js>result + '#' + src.length</js>".into()),
        // {{js}}：URL 模板内嵌——与 <js> 链同源（同一 vars 的 src）
        book_url: Some("/d{{src.length}}".into()),
        ..Default::default()
    };
    let src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com/list",
        &src,
        &rule,
        "div.book",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 2);
    let n0: usize = books[0].name.rsplit('#').next().unwrap().parse().unwrap();
    let n1: usize = books[1].name.rsplit('#').next().unwrap().parse().unwrap();
    assert!(
        books[0].name.starts_with("书名甲#") && n0 > 0,
        "<js> 应能取到条目 src: {}",
        books[0].name
    );
    assert_ne!(
        books[0].name, books[1].name,
        "不同条目的 src 应为各自元素 HTML"
    );
    // {{js}} 内嵌展开出 URL（/d{长度} → 转绝对），且与 <js> 链取到的同一 src 一致
    assert_eq!(books[0].book_url, format!("https://a.com/d{n0}"));
    assert_eq!(books[1].book_url, format!("https://a.com/d{n1}"));
}

/// SearchRule.updateTime → SearchBook.updateTime 透传（legacy SearchBook 契约）
#[test]
fn test_analyze_list_update_time() {
    let mut src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    src.rule_search = Some(serde_json::json!({
        "bookList": "div.book",
        "name": "h2@text",
        "bookUrl": "a@href",
        "updateTime": "span.time@text"
    }));
    let rule: SearchRule = serde_json::from_value(src.rule_search.clone().unwrap()).unwrap();
    let html = r#"<div class="book"><h2>书名</h2><a href="/b/1">详情</a><span class="time">2026-08-08</span></div>"#;
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com/list",
        &src,
        &rule,
        "div.book",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].update_time.as_deref(), Some("2026-08-08"));
}

#[test]
fn test_normalize_search_text() {
    // 全角 ASCII → 半角 + 小写 + 去首尾空白
    assert_eq!(normalize_search_text(" ＡＢＣ１２３ "), "abc123");
    assert_eq!(normalize_search_text("ＦｕｌｌＷｉｄｔｈ"), "fullwidth");
    assert_eq!(normalize_search_text("AbC"), "abc");
    // 全角空格 U+3000 → 半角空格；首尾空白（含全角）被 trim 去除
    assert_eq!(normalize_search_text("　全角　空格　"), "全角 空格");
    // 中文与混合内容原样保留（小写化对中文无影响）
    assert_eq!(normalize_search_text("诡秘之主"), "诡秘之主");
}

#[test]
fn test_filter_exact() {
    let mk = |name: &str, author: &str| SearchBook {
        name: name.into(),
        author: author.into(),
        ..Default::default()
    };
    let books = vec![
        mk("诡秘之主", "爱潜水的乌贼"),
        mk("诡秘之主2", "爱潜水的乌贼"),
        mk("ＧＭＴ", "测试作者"),
        mk("凡人修仙传", "忘语"),
        mk("Book ABC", ""),
    ];
    // 书名等值命中（大小写不敏感）
    let hit = filter_exact(books.clone(), "诡秘之主");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].name, "诡秘之主");
    // 包含但不等值（模糊命中）→ 精确下不命中
    let hit = filter_exact(books.clone(), "诡秘");
    assert!(hit.is_empty());
    // 全角书名 + 半角 key：全半角忽略等值命中
    let hit = filter_exact(books.clone(), "gmt");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].name, "ＧＭＴ");
    // 半角书名 + 全角 key
    let hit = filter_exact(books.clone(), "ＢＯＯＫ ａｂｃ");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].name, "Book ABC");
    // 作者等值命中
    let hit = filter_exact(books.clone(), "忘语");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].name, "凡人修仙传");
    // 作者为空：不因空作者误命中；空 key 不过滤
    let hit = filter_exact(books.clone(), "");
    assert_eq!(hit.len(), books.len());
    // 无命中
    let hit = filter_exact(books, "不存在的书");
    assert!(hit.is_empty());
}

#[test]
fn test_analyze_html_list() {
    let html = r#"<div class="book"><h2>书名A</h2><p>作者甲</p><a href="/book/1">详情</a></div>
                   <div class="book"><h2>书名B</h2><p>作者乙</p><a href="/book/2">详情</a></div>"#;
    let rule = SearchRule {
        book_list: Some("div.book".into()),
        name: Some("h2".into()),
        author: Some("p".into()),
        book_url: Some("a@href".into()),
        ..Default::default()
    };
    let src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com",
        &src,
        &rule,
        "div.book",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 2);
    assert_eq!(books[0].name, "书名A");
    assert_eq!(books[0].author, "作者甲");
    assert_eq!(books[0].book_url, "https://a.com/book/1");
}

/// 字段规则 ## 替换链（真实书源：class.title@tag.h2@text##《(.*)》##$1### 形态）：
/// 替换须在 field 层生效（此前 Css 分支绕过 apply_post 丢失替换）
#[test]
fn test_field_css_replace_chain() {
    let html = r#"<div class="book"><h2 class="t">《测试书》</h2><p>作者甲</p></div>"#;
    // 三段替换（去书名号）
    let name = field(html, Some("class.t@text##《(.*)》##$1"), "");
    assert_eq!(name, "测试书");
    // 字段上下文为单本书元素：链式提取 + 替换
    let el = crate::parser::css_chain::css_chain("div.book", html);
    let name2 = field(&el[0], Some("class.t@text##《(.*)》##$1"), "");
    assert_eq!(name2, "测试书");
    // @js: 链字段（提取结果进 JS result 变量）
    let name3 = field(
        &el[0],
        Some("class.t@text@js:result.replace('《','【').replace('》','】')"),
        "",
    );
    assert_eq!(name3, "【测试书】");
    // 纯替换规则（### replaceFirst）——上下文仅含书名
    let h2 = crate::parser::css_chain::css_chain("class.t", html);
    let name4 = field(&h2[0], Some("##《(.*)》##[$1]###"), "");
    assert_eq!(name4, "[测试书]");
}

/// AR2：字段 Css 规则多命中 → 全量非空结果以 "\n" 连接（legacy
/// AnalyzeByJSoup.getString 的 joinToString("\n") 语义），非仅首个；
/// URL 字段不受影响——仍走 getString0 仅取首个
#[test]
fn test_field_css_multi_hit_joined_and_url_first() {
    let html = r#"<div><h2 class="t">书名甲</h2><h2 class="t">书名乙</h2></div>"#;
    // 文本字段：多命中全量连接
    assert_eq!(field(html, Some("class.t@text"), ""), "书名甲\n书名乙");
    // 空命中回退 default 不变
    assert_eq!(field(html, Some("class.missing@text"), "默认"), "默认");
    // URL 字段：多命中仍仅取首个（isUrl → getString0），并转绝对地址
    let urls = r#"<div><a class="l" href="/1">a</a><a class="l" href="/2">b</a></div>"#;
    let mut vars = RuleVars::new();
    assert_eq!(
        field_url_with_vars(urls, Some("class.l@href"), "", "http://base", &mut vars),
        "http://base/1"
    );
}

/// AR3：后缀链提取段为空 → 整链终止返回空（legacy 段空结果为 null，
/// 后续 JS 跳过；此前以空串续喂 JS 得 "0"）
#[test]
fn test_field_empty_extract_terminates_js_suffix() {
    let html = r#"<div><p>正文</p></div>"#;
    // class.missing 无命中 → JS 不执行 → 空串（非 "0"）
    assert_eq!(
        field(html, Some("class.missing@text@js:result.length"), ""),
        ""
    );
    // 命中时链照常工作
    assert_eq!(field(html, Some("tag.p@text@js:result + '!'"), ""), "正文!");
}

/// bookList 修复：JS 返回 JSON.parse(result).data 数组 → 逐本书解析（此前 ToString
/// 输出 "[object Object]" 导致解析为空）
#[test]
fn test_js_book_list_json_parse_array() {
    let body = r#"{"code":0,"data":[
        {"novelName":"书A","authorName":"作者甲","novelId":"id1"},
        {"novelName":"书B","authorName":"作者乙","novelId":"id2"}
    ]}"#;
    let rule = SearchRule {
        book_list: Some("@js:JSON.parse(result).data".into()),
        name: Some("$.novelName".into()),
        author: Some("$.authorName".into()),
        book_url: Some("/novel/{{$.novelId}}".into()),
        ..Default::default()
    };
    let src = BookSource {
        book_source_url: "https://api.test".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        body,
        "https://api.test",
        &src,
        &rule,
        "@js:JSON.parse(result).data",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 2, "JS bookList 应解析出 2 本书");
    assert_eq!(books[0].name, "书A");
    assert_eq!(books[0].author, "作者甲");
    assert_eq!(books[0].book_url, "https://api.test/novel/id1");
    assert_eq!(books[1].name, "书B");
}

/// bookList 修复：JS 直接返回数组字面量（非字符串出口）
#[test]
fn test_js_book_list_array_literal() {
    let rule = SearchRule {
        book_list: Some("@js:[{name:'直A',url:'/a'},{name:'直B',url:'/b'}]".into()),
        name: Some("$.name".into()),
        book_url: Some("$.url".into()),
        ..Default::default()
    };
    let src = BookSource {
        book_source_url: "https://api.test".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        "{}",
        "https://api.test",
        &src,
        &rule,
        "@js:[{name:'直A',url:'/a'},{name:'直B',url:'/b'}]",
        "",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 2);
    assert_eq!(books[0].name, "直A");
    assert_eq!(books[0].book_url, "https://api.test/a");
}

/// bookList 兼容：JS 内 JSON.stringify 返回字符串数组——自动解析
#[test]
fn test_js_book_list_stringify_backward_compat() {
    let rule = SearchRule {
        book_list: Some("@js:JSON.stringify([{name:'串A',url:'/s/a'}])".into()),
        name: Some("$.name".into()),
        book_url: Some("$.url".into()),
        ..Default::default()
    };
    let src = BookSource {
        book_source_url: "https://api.test".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        "{}",
        "https://api.test",
        &src,
        &rule,
        "@js:JSON.stringify([{name:'串A',url:'/s/a'}])",
        "",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].name, "串A");
}

/// bookList：<js> 包裹形式
#[test]
fn test_js_book_list_html_wrapped() {
    let rule = SearchRule {
        book_list: Some("<js>JSON.parse(result).data</js>".into()),
        name: Some("$.name".into()),
        book_url: Some("$.url".into()),
        ..Default::default()
    };
    let body = r#"{"data":[{"name":"包A","url":"/w/1"}]}"#;
    let src = BookSource {
        book_source_url: "https://api.test".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        body,
        "https://api.test",
        &src,
        &rule,
        "<js>JSON.parse(result).data</js>",
        "",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].name, "包A");
}

/// 字段规则：@js: 前缀（analyze 系列字段出口——result 注入上下文执行）
#[test]
fn test_field_js_rule() {
    let ctx = r#"{"data":{"name":"字段书","author":"字段作者"}}"#;
    assert_eq!(
        field(ctx, Some("@js:JSON.parse(result).data.name"), ""),
        "字段书"
    );
    assert_eq!(
        field(ctx, Some("@js:JSON.parse(result).data.author"), ""),
        "字段作者"
    );
    // 失败回退默认值
    assert_eq!(
        field(ctx, Some("@js:JSON.parse(result).data.missing.x"), "默认"),
        "默认"
    );
}

#[test]
fn test_js_search_url_prefix() {
    // @js: 前缀：JS 返回值作为搜索 URL（注入 key/page/baseUrl）
    let headers = HashMap::new();
    let (url, suffix) = build_request_url(
        r#"@js:baseUrl + "/s?q=" + key + "&p=" + page"#,
        "测试书",
        2,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(url, "https://a.com/s?q=测试书&p=2");
    assert!(suffix.js.is_none() && suffix.body_js.is_none());
}

#[test]
fn test_js_search_url_wrapped_tag() {
    // `<js>…</js>` 整体包裹：JS 返回值作为搜索 URL（legado JS_PATTERN）
    let headers = HashMap::new();
    let (url, suffix) = build_request_url(
        r#"<js>baseUrl + "/s?q=" + key + "&p=" + page</js>"#,
        "测试书",
        2,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(url, "https://a.com/s?q=测试书&p=2");
    assert!(suffix.js.is_none() && suffix.body_js.is_none());
}

#[test]
fn test_js_search_url_wrapped_tag_with_suffix() {
    // `<js>` 后的 `,{...}` 后缀保留并解析（charset/headers 等）
    let headers = HashMap::new();
    let (url, suffix) = build_request_url(
        r#"<js>baseUrl + "/search?key=" + key</js>,{"charset":"GBK","headers":{"X-A":"1"}}"#,
        "测试",
        1,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(url, "https://a.com/search?key=测试");
    assert_eq!(suffix.charset.as_deref(), Some("GBK"));
    assert_eq!(
        suffix
            .headers
            .as_ref()
            .and_then(|h| h.get("X-A"))
            .map(String::as_str),
        Some("1")
    );
}

#[test]
fn test_build_search_url_relative_path_without_slash() {
    // `bookajax/search.do?keyword=...` 这类无 scheme 相对 URL → base 目录拼接
    let u = build_search_url(
        "bookajax/search.do?keyword=文明乐园",
        "文明乐园",
        1,
        "https://a.com",
    );
    assert!(
        u.starts_with("https://a.com/bookajax/search.do?keyword="),
        "相对路径应拼到 base 目录: {u}"
    );
}

#[test]
fn test_build_search_url_protocol_relative() {
    let u = build_search_url("//cdn.example.com/s?q={{key}}", "k", 1, "https://a.com");
    assert_eq!(u, "https://cdn.example.com/s?q=k");
}

#[test]
fn test_data_uri_detection() {
    assert!(is_data_uri("data:;base64,eyJhIjoxfQ=="));
    assert!(is_data_uri("data:text/plain;base64,QQ=="));
    assert!(!is_data_uri("https://a.com/s"));
    assert!(!is_data_uri("data:text/plain,hello"));
}

#[test]
fn test_js_search_url_header_map_json() {
    // headerMap 以 JSON 字符串注入，JS 可读取
    let headers = HashMap::from([("User-Agent".to_string(), "UA1".to_string())]);
    let (url, _) = build_request_url(
        r#"@js:baseUrl + "/s?h=" + (JSON.parse(headerMap)["User-Agent"] ? "yes" : "no")"#,
        "k",
        1,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(url, "https://a.com/s?h=yes");
}

#[test]
fn test_url_suffix_js_modifies_url() {
    // `,{"js":...}` 后缀：JS 修改 URL（注入 key/page/result 为空字符串/baseUrl）
    let headers = HashMap::new();
    let (url, suffix) = build_request_url(
        r#"https://a.com/search?q={{key}},{"js":"baseUrl + '/mod?k=' + key + '&p=' + page"}"#,
        "测试",
        1,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(url, "https://a.com/mod?k=测试&p=1");
    // js 键已消费：不再出现在后缀中，bodyJs 保留为空
    assert!(suffix.js.is_none());
}

#[test]
fn test_url_suffix_body_js_rewrites_body() {
    // bodyJs：对响应体执行 JS 后作为新响应体（注入 result=原响应体）
    let suffix: UrlSuffix =
        serde_json::from_str(r#"{"bodyJs":"result.replace('A','B')"}"#).unwrap();
    let headers = HashMap::new();
    let body = apply_body_js(
        "AAA",
        &suffix,
        "k",
        1,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(body, "BAA");
}

#[test]
fn test_url_suffix_parse_ignores_unknown_keys() {
    // 其他键（method 等）忽略；js/bodyJs 同时解析
    let (url, suffix) = split_url_suffix(
        r#"https://a.com/s,{"js":"baseUrl","method":"POST","bodyJs":"result + '!'"}"#,
    );
    assert_eq!(url, "https://a.com/s");
    assert_eq!(suffix.js.as_deref(), Some("baseUrl"));
    assert_eq!(suffix.body_js.as_deref(), Some("result + '!'"));
}

#[test]
fn test_url_without_suffix_unchanged() {
    let headers = HashMap::new();
    let (url, suffix) = build_request_url(
        "https://a.com/s?q={{key}}",
        "k",
        1,
        "https://a.com",
        &headers,
        &JsBridge::default(),
    )
    .unwrap();
    assert_eq!(url, "https://a.com/s?q=k");
    assert!(suffix.js.is_none() && suffix.body_js.is_none());
}

#[test]
fn test_concurrent_rate_sleep_ms() {
    assert_eq!(concurrent_rate_sleep_ms(Some("1000")), 1000);
    assert_eq!(concurrent_rate_sleep_ms(Some("20/60000")), 3000);
    assert_eq!(concurrent_rate_sleep_ms(Some(" 500 ")), 500);
    assert_eq!(concurrent_rate_sleep_ms(Some("abc")), 0);
    assert_eq!(concurrent_rate_sleep_ms(None), 0);
}

/// A2：共享滑窗——10 协程抢 3/300ms 窗口，总耗时 ≥ (10/3-1)*300ms 且窗口内不超发
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_rate_window_shared() {
    let src = BookSource {
        book_source_url: format!("test://rate-window-{}", std::process::id()),
        concurrent_rate: Some("3/300".into()),
        ..Default::default()
    };
    // 唯一 ns 隔离其他测试
    let ns = format!("ns-{}", std::process::id());
    let start = std::time::Instant::now();
    let mut futs = Vec::new();
    for _ in 0..10 {
        futs.push(concurrent_rate_acquire(&ns, &src));
    }
    for f in futs {
        f.await;
    }
    let elapsed = start.elapsed();
    // 10 次请求 / 3 每窗 → 至少等 2 个完整窗口 ≈ 600ms（余量放宽到 550）
    assert!(
        elapsed >= std::time::Duration::from_millis(550),
        "共享滑窗应串行化超窗请求，实际 {elapsed:?}"
    );
}

/// A2：纯数字间隔——同源两次 acquire 至少间隔该毫秒
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_rate_interval_shared() {
    let src = BookSource {
        book_source_url: format!("test://rate-interval-{}", std::process::id()),
        concurrent_rate: Some("150".into()),
        ..Default::default()
    };
    let ns = format!("ns-i-{}", std::process::id());
    let start = std::time::Instant::now();
    concurrent_rate_acquire(&ns, &src).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    concurrent_rate_acquire(&ns, &src).await;
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(140),
        "第二次 acquire 应补足最小间隔"
    );
}

/// legado 列表规则前缀：`-` 倒序、`+` 去前缀、其余原样
#[test]
fn test_strip_list_rule_prefix() {
    assert_eq!(
        strip_list_rule_prefix("-div.book"),
        ("div.book".to_string(), true)
    );
    assert_eq!(
        strip_list_rule_prefix("+div.book"),
        ("div.book".to_string(), false)
    );
    assert_eq!(
        strip_list_rule_prefix("div.book"),
        ("div.book".to_string(), false)
    );
    assert_eq!(strip_list_rule_prefix("-"), ("".to_string(), true));
}

/// `-` 前缀书单：结果倒序；`+` 前缀：正常顺序
#[test]
fn test_analyze_html_list_prefix() {
    let html = r#"<div class="book"><h2>书名A</h2><a href="/book/1">详情</a></div>
                   <div class="book"><h2>书名B</h2><a href="/book/2">详情</a></div>"#;
    let rule = SearchRule {
        book_list: Some("div.book".into()),
        name: Some("h2".into()),
        book_url: Some("a@href".into()),
        ..Default::default()
    };
    let src = BookSource {
        book_source_url: "https://a.com".into(),
        ..Default::default()
    };
    let books = analyze_book_list(
        "default",
        html,
        "https://a.com",
        &src,
        &rule,
        "-div.book",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books.len(), 2);
    assert_eq!(
        books[0].name, "书名B",
        "`-` 前缀应倒序: {:?}",
        books[0].name
    );
    assert_eq!(books[1].name, "书名A");

    let books = analyze_book_list(
        "default",
        html,
        "https://a.com",
        &src,
        &rule,
        "+div.book",
        "key",
        &JsBridge::default(),
    );
    assert_eq!(books[0].name, "书名A", "`+` 前缀保持原序");
    assert_eq!(books[1].name, "书名B");
}

/// UrlSuffix.retry（legacy AnalyzeUrl UrlOption.retry，AnalyzeUrl.kt:564-573）：
/// URL option `{...}` 的 retry 键指定该请求重试次数——数字/字符串均可解析；
/// 缺失/非法 → None（单次请求）；其余后缀键不受影响
#[test]
fn test_url_suffix_retry() {
    // 数字字面量
    let (_, s) = split_url_suffix(r#"https://a.com/s,{"retry":3}"#);
    assert_eq!(s.retry, Some(3));
    // legacy setRetry(String)：字符串数字亦可
    let (_, s) =
        split_url_suffix(r#"https://a.com/s,{"retry":"2","charset":"GBK","method":"POST"}"#);
    assert_eq!(s.retry, Some(2));
    assert_eq!(s.charset.as_deref(), Some("GBK"));
    assert_eq!(s.method.as_deref(), Some("POST"));
    // 缺失 / 0 → 不重试
    let (_, s) = split_url_suffix("https://a.com/s");
    assert_eq!(s.retry, None);
    let (_, s) = split_url_suffix(r#"https://a.com/s,{"retry":0}"#);
    assert_eq!(s.retry, Some(0));
    // 非法值容错 → None，不中断后缀解析
    let (_, s) = split_url_suffix(r#"https://a.com/s,{"retry":"abc","bodyJs":"result"}"#);
    assert_eq!(s.retry, None);
    assert_eq!(s.body_js.as_deref(), Some("result"));
}
