//! 书籍详情与相关推荐解析（ruleBookInfo / ruleRelated）

use anyhow::Result;
use serde::Deserialize;

use crate::model::book_chapter::BookInfo;
use crate::model::BookSource;

use super::{apply_login_check_js, fetch_url, to_abs};

/// ruleBookInfo 结构（legacy BookInfoRule）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BookInfoRule {
    pub name: Option<String>,
    pub author: Option<String>,
    pub kind: Option<String>,
    pub intro: Option<String>,
    pub cover_url: Option<String>,
    pub toc_url: Option<String>,
    /// 更新时间（legacy BookInfoRule.updateTime）
    pub update_time: Option<String>,
    /// 是否允许用规则结果覆盖非空原有书名/作者（legacy canReName）
    pub can_re_name: Option<String>,
    pub word_count: Option<String>,
    pub last_chapter: Option<String>,
    pub init: Option<String>,
}

/// ruleRelated 结构（GAP 17b：相关推荐——字段与 ruleExplore 一致：bookList + 字段规则）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RelatedRule {
    pub book_list: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub book_url: Option<String>,
    pub cover_url: Option<String>,
}

/// 相关推荐解析（GAP 17b）：ruleRelated 应用详情页 HTML，同 ruleExplore 风格
/// （bookList CSS 链式 + 字段规则）→ [{name, author, bookUrl, coverUrl}]
pub fn analyze_related_books(
    html: &str,
    base_url: &str,
    source: &BookSource,
) -> Vec<crate::model::book_chapter::RelatedBook> {
    let rule: RelatedRule = source
        .rule_related
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(book_list_rule) = rule.book_list.clone() else {
        return vec![];
    };
    // 复用 ruleExplore 书单解析（SearchRule 字段名与 RelatedRule 一致）
    let search_rule = crate::service::search::SearchRule {
        book_list: rule.book_list,
        name: rule.name,
        author: rule.author,
        book_url: rule.book_url,
        cover_url: rule.cover_url,
        ..Default::default()
    };
    crate::service::search::analyze_book_list_for_explore(
        "default",
        html,
        base_url,
        source,
        &search_rule,
        &book_list_rule,
    )
    .into_iter()
    .map(|b| crate::model::book_chapter::RelatedBook {
        name: b.name,
        author: b.author,
        book_url: b.book_url,
        cover_url: b.cover_url,
    })
    .collect()
}

/// 详情解析（ruleBookInfo 字段应用于详情页 HTML）
/// 目录 URL 解析（legacy WebBook.getChapterList 语义）：书页与目录页常是独立端点
/// （如 fq 源的 `/toc/<id>`），book.toc_url 为空时先查 book vars 的 `tocUrl` 缓存（推导过
/// 就不再抓书页），再抓书页按 ruleBookInfo.tocUrl 推导，结果写回 vars 供复用。
/// 推导不出 → 返回 book_url 兜底（部分源的目录确实就在书页）。
pub async fn resolve_toc_url(
    ns: &str,
    source: &crate::model::BookSource,
    book_url: &str,
) -> String {
    if book_url.is_empty() {
        return String::new();
    }
    let mut vars = crate::parser::rule::load_book_vars(ns, &source.book_source_url, book_url);
    let cached = vars.get("tocUrl").cloned().unwrap_or_default();
    if !cached.is_empty() && cached != book_url {
        return cached;
    }
    if let Ok(resp) = fetch_url(ns, book_url, source).await {
        let info = analyze_book_info(ns, &resp.body, &resp.url, source, book_url, None);
        if let Some(t) = info.toc_url {
            if !t.is_empty() && t != book_url {
                vars.insert("tocUrl".to_string(), t.clone());
                crate::parser::rule::save_book_vars(ns, &source.book_source_url, book_url, &vars);
                return t;
            }
        }
    }
    book_url.to_string()
}

/// F12/AR4：`book_name` 为解析前已知的书名（搜索结果/书架）——@get:{bookName} 回退源
#[allow(clippy::too_many_arguments)]
pub fn analyze_book_info(
    ns: &str,
    html: &str,
    base_url: &str,
    source: &BookSource,
    book_url: &str,
    book_name: Option<&str>,
) -> BookInfo {
    let rule: BookInfoRule = source
        .rule_book_info
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // legado init：先提取详情上下文（如 $.data），字段规则相对应用
    // @put/@get 变量随本书流程贯通（legado Book.putVariable）——详情→目录共享
    let mut vars = crate::parser::rule::load_book_vars(ns, &source.book_source_url, book_url);
    vars.book_name = book_name.map(str::to_string);
    // E10/AR5：详情页真实 URL → JS 求值绑定 baseUrl（搜索场景无章节上下文）
    vars.insert("baseUrl".to_string(), base_url.to_string());
    let html = crate::parser::rule::apply_init_with_vars(html, rule.init.as_deref(), &mut vars);
    let html = html.as_str();
    // tocUrl 规则（legacy BookInfo.tocUrl 为完整字段规则）：
    // ① 选择器形态（CSS/XPath/JSONPath/@js 链）→ field 全量求值；
    // ② 求值为空时回退 v1 直接路径/URL 拼接 + {{}} 内嵌模板展开
    let toc_url = rule
        .toc_url
        .as_deref()
        .map(|r| {
            let evaluated = crate::service::search::field_with_vars(html, Some(r), "", &mut vars);
            if !evaluated.is_empty() {
                evaluated
            } else {
                crate::service::search::expand_embedded_with_vars(r, html, &mut vars)
            }
        })
        .filter(|r| !r.is_empty())
        .map(|r| to_abs(&r, base_url));

    let info = BookInfo {
        // legacy BookInfo.kt:61-90：name/author 经 formatBookName/Author 清洗，
        // kind 多值归一，wordCount 走 wordCountFormat
        name: crate::service::search::format_book_name(&crate::service::search::field_with_vars(
            html,
            rule.name.as_deref(),
            "",
            &mut vars,
        )),
        author: crate::service::search::format_book_author(
            &crate::service::search::field_with_vars(html, rule.author.as_deref(), "", &mut vars),
        ),
        kind: crate::service::search::opt_field_with_vars(html, rule.kind.as_deref(), &mut vars)
            .map(|k| crate::service::search::normalize_kind_list(&k)),
        intro: crate::service::search::opt_field_with_vars(html, rule.intro.as_deref(), &mut vars),
        update_time: crate::service::search::opt_field_with_vars(
            html,
            rule.update_time.as_deref(),
            &mut vars,
        ),
        cover_url: crate::service::search::opt_field_with_vars(
            html,
            rule.cover_url.as_deref(),
            &mut vars,
        )
        .map(|c| to_abs(&c, base_url)),
        // legacy BookInfo：tocUrl 为空时用 baseUrl（详情页即目录页）
        toc_url: toc_url.clone().or_else(|| Some(base_url.to_string())),
        word_count: crate::service::search::opt_field_with_vars(
            html,
            rule.word_count.as_deref(),
            &mut vars,
        )
        .map(|w| crate::service::search::word_count_format(&w)),
        latest_chapter_title: crate::service::search::opt_field_with_vars(
            html,
            rule.last_chapter.as_deref(),
            &mut vars,
        ),
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: source.book_source_name.clone(),
        language: None,
        publisher: None,
        published_at: None,
        related_books: analyze_related_books(html, base_url, source),
        book_type: source.book_source_type,
    };
    // 目录流程（getBookToc）只带 tocUrl，无 bookUrl——按两个键都存，保证命中
    crate::parser::rule::save_book_vars(ns, &source.book_source_url, book_url, &vars);
    if let Some(t) = &toc_url {
        crate::parser::rule::save_book_vars(ns, &source.book_source_url, t, &vars);
    }
    info
}

/// legacy BookInfo.canReName 语义：书源规则未声明 canReName 时，保留书架已有
/// 书名/作者（避免详情页刷新或换源把用户自定义的名称/作者覆盖为空）。
pub fn merge_existing_identity(
    info: &mut BookInfo,
    source: &BookSource,
    existing_name: &str,
    existing_author: &str,
) {
    if existing_name.is_empty() && existing_author.is_empty() {
        return;
    }
    let rule: BookInfoRule = source
        .rule_book_info
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let can_re_name = rule
        .can_re_name
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !can_re_name {
        if !existing_name.is_empty() {
            info.name = existing_name.to_string();
        }
        if !existing_author.is_empty() {
            info.author = existing_author.to_string();
        }
    }
}

/// 带已有书架身份的详情解析（legacy WebBook 更新详情时 canReName=true——
/// 是否真正覆盖由书源 canReName 规则决定）
pub fn analyze_book_info_with_existing(
    html: &str,
    base_url: &str,
    source: &BookSource,
    book_url: &str,
    existing_name: &str,
    existing_author: &str,
) -> BookInfo {
    let mut info = analyze_book_info(
        "default",
        html,
        base_url,
        source,
        book_url,
        Some(existing_name).filter(|n| !n.is_empty()),
    );
    merge_existing_identity(&mut info, source, existing_name, existing_author);
    info
}

/// 详情抓取 + loginCheckJs + ruleBookInfo 解析（router 详情/换源取书名共用）
/// F12/AR4：`book_name` 为解析前已知的书名（搜索结果/书架）——@get:{bookName} 回退源
///
/// legacy 对齐：抓取报错标记运行期失效快照（getInvalidBookSources 600 秒内直接返回），
/// 成功则清除该源标记。
pub async fn fetch_book_info(
    ns: &str,
    url: &str,
    source: &BookSource,
    book_name: Option<&str>,
) -> Result<BookInfo> {
    match fetch_book_info_impl(ns, url, source, book_name).await {
        Ok(v) => {
            crate::service::health::clear_source_invalid(ns, &source.book_source_url);
            Ok(v)
        }
        Err(e) => {
            crate::service::health::mark_source_invalid(
                ns,
                &source.book_source_url,
                &e.to_string(),
            );
            Err(e)
        }
    }
}

async fn fetch_book_info_impl(
    ns: &str,
    url: &str,
    source: &BookSource,
    book_name: Option<&str>,
) -> Result<BookInfo> {
    let mut resp = fetch_url(ns, url, source).await?;
    resp.body = apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;
    Ok(analyze_book_info(
        ns, &resp.body, &resp.url, source, url, book_name,
    ))
}
