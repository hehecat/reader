//! 搜索结果提取：书单解析、详情页回退、字段规则应用与文本清洗

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::model::BookSource;
use crate::parser::js::JsBridge;
use crate::parser::rule::{
    apply, apply_with_vars, parse_rule, push_js_context, resolve_get, RuleKind, RuleVars,
};

use super::engine::to_absolute;
use super::{SearchBook, SearchRule};

/// legacy AppPattern.nameRegex + BookHelp.formatBookName：
/// 剔除书名中的作者尾巴（"xxx 作者：yyy"/"xxx yyy 著"）
pub(crate) fn format_book_name(name: &str) -> String {
    static NAME_RE: LazyLock<crate::util::regex::Regex> = LazyLock::new(|| {
        crate::util::regex::RegexBuilder::new(r"\s+作\s*者.*|\s+\S+\s+著")
            .build()
            .expect("nameRegex 编译失败")
    });
    NAME_RE
        .replace_all(name, "")
        .trim_matches(|c: char| c <= ' ')
        .to_string()
}

/// legacy AppPattern.authorRegex + BookHelp.formatBookAuthor：
/// 剔除「作者：」前缀与「著」后缀
pub(crate) fn format_book_author(author: &str) -> String {
    static AUTHOR_RE: LazyLock<crate::util::regex::Regex> = LazyLock::new(|| {
        crate::util::regex::RegexBuilder::new(r"^\s*作\s*者[:：\s]+|\s+著")
            .build()
            .expect("authorRegex 编译失败")
    });
    AUTHOR_RE
        .replace_all(author, "")
        .trim_matches(|c: char| c <= ' ')
        .to_string()
}

/// legacy StringUtils.wordCountFormat：纯数字 → "N字"；>10000 → "#.#万字"
/// （一位小数、去尾零）；非数字原样；数字 ≤0 → 空
pub(crate) fn word_count_format(wc: &str) -> String {
    if !wc.is_empty() && wc.bytes().all(|b| b.is_ascii_digit()) {
        let Ok(n) = wc.parse::<i64>() else {
            return String::new();
        };
        if n > 10_000 {
            let w = n as f64 / 10_000.0;
            let s = format!("{w:.1}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
            return format!("{s}万字");
        }
        if n > 0 {
            return format!("{n}字");
        }
        return String::new();
    }
    wc.to_string()
}

/// legacy kind 多值归一：getStringList 按 [,;，；] 拆分 → joinToString(",")，
/// 空段丢弃、段内去空白
pub(crate) fn normalize_kind_list(kind: &str) -> String {
    kind.split([',', ';', '，', '；'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// 发现页解析（无 key；无书源桥接——发现流程暂不注入 cookie 上下文）
pub(crate) fn analyze_book_list_for_explore(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    rule: &SearchRule,
    book_list_rule: &str,
) -> Vec<SearchBook> {
    analyze_book_list_impl(
        ns,
        body,
        base_url,
        source,
        rule,
        book_list_rule,
        "",
        base_url,
        None,
    )
}

/// 解析书单（对齐 legacy BookList.analyzeBookList v1：无 JS/无变量）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub(super) fn analyze_book_list(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    rule: &SearchRule,
    book_list_rule: &str,
    _key: &str,
    bridge: &JsBridge,
) -> Vec<SearchBook> {
    analyze_book_list_impl(
        ns,
        body,
        base_url,
        source,
        rule,
        book_list_rule,
        _key,
        base_url,
        Some(bridge),
    )
}

#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub(super) fn analyze_book_list_impl(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    rule: &SearchRule,
    book_list_rule: &str,
    _key: &str,
    request_url: &str,
    bridge: Option<&JsBridge>,
) -> Vec<SearchBook> {
    // legado BookList：响应 URL 匹配 bookUrlPattern → 按详情页规则解析为单本
    if let Some(pat) = source
        .book_url_pattern
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        let matched = crate::util::regex::Regex::new(pat)
            .map(|r| r.is_match(base_url))
            .unwrap_or(false);
        if matched {
            return single_detail_search_book(ns, body, base_url, source, request_url);
        }
    }
    // legado BookList：列表规则前缀 `-` = 结果倒序；`+` = 去掉前缀（兼容旧写法）
    let (list_rule, reverse) = strip_list_rule_prefix(book_list_rule);
    let list_rule = list_rule.as_str();
    // bookList 规则类型检测
    let parsed = parse_rule(list_rule);
    let mut items: Vec<String> = match parsed.kind {
        RuleKind::Css => css_items(list_rule, body),
        RuleKind::JsonPath => apply(list_rule, body),
        RuleKind::Regex => apply(list_rule, body),
        RuleKind::Js => js_book_list(list_rule, body, base_url, bridge),
        _ => vec![],
    };
    // JS 规则（<js> 或 @js: 开头——eval 返回 JSON 书单数组）
    if items.is_empty()
        && (list_rule.contains("<js>") || list_rule.trim_start().starts_with("@js:"))
    {
        items = js_book_list(list_rule, body, base_url, bridge);
    }
    if reverse {
        items.reverse();
    }
    // legado BookList：列表为空且书源未配置 bookUrlPattern → 按详情页规则解析单本
    if items.is_empty()
        && source
            .book_url_pattern
            .as_deref()
            .map(|p| p.trim().is_empty())
            .unwrap_or(true)
    {
        return single_detail_search_book(ns, body, base_url, source, request_url);
    }

    items
        .into_iter()
        .enumerate()
        .filter_map(|(idx, item_html)| {
            // legado：同一本书条目共用一个 AnalyzeRule——@put 跨字段存入、@get 后置字段读取
            let mut vars = RuleVars::new();
            // E10/AR5：搜索无章节上下文，仅绑定 baseUrl（JS 字段规则可用）
            vars.insert("baseUrl".to_string(), base_url.to_string());
            // E10：src = 当前条目 HTML（legacy AnalyzeRule.kt:661 bindings["src"]=content——
            // 逐条目解析时 item_html 即"当前文档"，条目内 <js>/{{js}} 的 src 可取当前元素 HTML）
            vars.insert("src".to_string(), item_html.clone());
            let mut book = SearchBook {
                origin: source.book_source_url.clone(),
                origin_name: source.book_source_name.clone(),
                origin_order: source.custom_order,
                // 非文本书源（音频/漫画等）：bookSourceType 透传到搜索结果的 type——
                // 入架后 books.book_type 即带类型，阅读器按此分派渲染
                book_type: source.book_source_type.clamp(0, 4),
                time: chrono::Utc::now().timestamp_millis(),
                ..Default::default()
            };
            // 字段规则（在每本书元素上下文中应用）
            book.name = field_with_bridge_vars(
                &item_html,
                rule.name.as_deref(),
                &book.name,
                bridge,
                &mut vars,
            );
            // legacy BookList.kt:168 formatBookName——剔除书名中的作者尾巴等脏数据
            book.name = format_book_name(&book.name);
            if book.name.is_empty() {
                return None;
            }
            // legacy BookList.kt:173 formatBookAuthor——剔除「作者：」前缀/「著」后缀
            book.author =
                field_with_bridge_vars(&item_html, rule.author.as_deref(), "", bridge, &mut vars);
            book.author = format_book_author(&book.author);
            // legacy BookList.kt:178 kind=getStringList(...).joinToString(",")——多值归一
            book.kind =
                opt_field_with_bridge_vars(&item_html, rule.kind.as_deref(), bridge, &mut vars)
                    .map(|k| normalize_kind_list(&k));
            book.intro =
                opt_field_with_bridge_vars(&item_html, rule.intro.as_deref(), bridge, &mut vars);
            book.cover_url = rule
                .cover_url
                .as_deref()
                .map(|r| field_url_with_vars(&item_html, Some(r), "", base_url, &mut vars))
                .filter(|v| !v.is_empty());
            book.word_count = opt_field_with_bridge_vars(
                &item_html,
                rule.word_count.as_deref(),
                bridge,
                &mut vars,
            )
            .map(|w| word_count_format(&w));
            book.latest_chapter_title = opt_field_with_bridge_vars(
                &item_html,
                rule.last_chapter.as_deref(),
                bridge,
                &mut vars,
            );
            book.update_time = opt_field_with_bridge_vars(
                &item_html,
                rule.update_time.as_deref(),
                bridge,
                &mut vars,
            )
            .filter(|s| !s.trim().is_empty());
            let book_url = field_url_with_vars(
                &item_html,
                rule.book_url.as_deref(),
                "",
                base_url,
                &mut vars,
            );
            if book_url.is_empty() {
                // legacy getSearchItem：bookUrl 为空时回退当前响应 URL
                book.book_url = base_url.to_string();
            } else {
                book.book_url = book_url;
            }
            // 详情页 URL 规则（bookUrlPattern 正则应匹配——v1 记录即可）
            if book.name.is_empty() {
                return None;
            }
            // 搜索阶段 tocUrl 留空（进入详情时获取）
            let _ = idx;
            book.toc_url = String::new();
            Some(book)
        })
        .collect()
}

/// 详情页单本解析结果 → SearchBook（legacy BookList.getInfoItem → Book.toSearchBook）
pub(crate) fn single_search_book(
    info: crate::model::book_chapter::BookInfo,
    source: &BookSource,
    book_url: &str,
) -> SearchBook {
    SearchBook {
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: source.book_source_name.clone(),
        origin_order: source.custom_order,
        book_type: source.book_source_type.clamp(0, 4),
        name: info.name,
        author: info.author,
        kind: info.kind,
        cover_url: info.cover_url,
        intro: info.intro,
        word_count: info.word_count,
        latest_chapter_title: info.latest_chapter_title,
        update_time: info.update_time,
        toc_url: info.toc_url.unwrap_or_default(),
        time: chrono::Utc::now().timestamp_millis(),
        variable: None,
    }
}

/// legado BookList.getInfoItem：按 ruleBookInfo 解析当前响应为单本（name 非空才返回）
fn single_detail_search_book(
    ns: &str,
    body: &str,
    base_url: &str,
    source: &BookSource,
    request_url: &str,
) -> Vec<SearchBook> {
    let info =
        crate::service::book::analyze_book_info(ns, body, base_url, source, request_url, None);
    if info.name.is_empty() {
        return vec![];
    }
    vec![single_search_book(info, source, request_url)]
}

/// legado 列表规则前缀（BookList/BookChapterList/RssParserByRule 共用语义）：
/// `-` → 结果倒序；`+` → 仅去前缀（旧版兼容写法）；其余原样。
pub(crate) fn strip_list_rule_prefix(rule: &str) -> (String, bool) {
    let trimmed = rule.trim_start();
    if let Some(rest) = trimmed.strip_prefix('-') {
        (rest.trim_start().to_string(), true)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (rest.trim_start().to_string(), false)
    } else {
        (rule.to_string(), false)
    }
}

/// CSS 书单：链式 CSS（legado）→ 元素 html 列表
/// JS 书单规则（legado `<js>代码</js>` 或 `@js:代码`——eval 返回 JSON 数组，每项为书对象）
fn js_book_list(rule: &str, body: &str, base_url: &str, bridge: Option<&JsBridge>) -> Vec<String> {
    // 提取 JS 代码
    let code = if rule.trim_start().starts_with("@js:") {
        rule.trim_start()[4..].to_string()
    } else if let Some(start) = rule.find("<js>") {
        let rest = &rule[start + 4..];
        let end = rest.find("</js>").unwrap_or(rest.len());
        rest[..end].to_string()
    } else {
        return vec![];
    };
    // 执行（注入 result=响应体、key/page）并直接取结构化结果：
    // 数组/对象经递归 JSON 转换（js_value_to_json）——避免 boa ToString 对数组
    // 元素对象输出 "[object Object]" 导致 JSON.parse 解析为空（bookList 修复核心）
    let mut vars = HashMap::new();
    vars.insert("result".to_string(), body.to_string());
    vars.insert("key".to_string(), String::new());
    vars.insert("page".to_string(), "1".to_string());
    vars.insert("baseUrl".to_string(), base_url.to_string());
    // 带书源桥接执行（搜索流程：java.ajax/startBrowserAwait/setContent 等可用）
    let result = match bridge {
        Some(b) => crate::parser::js::eval_js_json_with_bridge(&code, &vars, b),
        None => crate::parser::js::eval_js_json(&code, &vars),
    };
    let Ok(result) = result else {
        return vec![];
    };
    // 数组：每项书对象/字符串 → 上下文（JSON 文本）；单对象兼容
    match result {
        serde_json::Value::Array(list) => list
            .iter()
            .map(|item| match item {
                serde_json::Value::Object(_) => item.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect(),
        serde_json::Value::Object(_) => vec![result.to_string()],
        _ => vec![],
    }
}

fn css_items(rule: &str, body: &str) -> Vec<String> {
    crate::parser::css_chain::css_chain(rule, body)
}

/// URL 型字段规则（legado isUrl 语义）：展开内嵌后若是路径/URL 直接拼接，否则走规则解析
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
fn field_url(context: &str, rule: Option<&str>, default: &str, base: &str) -> String {
    field_url_impl(context, rule, default, base, None)
}

/// [`field_url`] 带变量版本（@get 引用已存变量——搜索条目内跨字段贯通）
pub(crate) fn field_url_with_vars(
    context: &str,
    rule: Option<&str>,
    default: &str,
    base: &str,
    vars: &mut RuleVars,
) -> String {
    field_url_impl(context, rule, default, base, Some(vars))
}

fn field_url_impl(
    context: &str,
    rule: Option<&str>,
    default: &str,
    base: &str,
    mut vars: Option<&mut RuleVars>,
) -> String {
    let Some(rule) = rule else {
        return default.to_string();
    };
    let expanded = expand_embedded_impl(rule, context, vars.as_deref_mut());
    // legado makeUpRule：@get 在类型检测/URL 直判前替换
    // （URL 字段可拼 `https://x/...@get:{id}`——{{}} 内嵌已在上一步展开）
    let expanded = match vars.as_deref() {
        Some(v) => resolve_get(&expanded, v),
        None => expanded,
    };
    // URL 型：路径或完整 URL → 直接返回（相对转绝对）；// 开头是 XPath 不在此列
    if expanded.starts_with('/') && !expanded.starts_with("//") {
        return to_absolute(&expanded, base);
    }
    if expanded.starts_with("http://") || expanded.starts_with("https://") {
        return expanded;
    }
    // 规则解析（CSS/JSONPath/Regex 等）；结果为相对路径时转绝对
    // （legado isUrl：URL 字段走 getString0 仅取首个）
    let v = field_impl(context, Some(&expanded), default, None, vars, true);
    if v.starts_with('/') && !v.starts_with("//") {
        to_absolute(&v, base)
    } else {
        v
    }
}

/// 展开 {{$.xxx}} 内嵌规则（legado：{{}} 内为 JSONPath/JS，v1 支持 JSONPath）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn expand_embedded(rule: &str, context: &str) -> String {
    expand_embedded_impl(rule, context, None)
}

/// [`expand_embedded`] 带变量版本：先替换 `@get:{key}`（legado makeUpRule）
pub(crate) fn expand_embedded_with_vars(rule: &str, context: &str, vars: &mut RuleVars) -> String {
    let resolved = resolve_get(rule, vars);
    expand_embedded_impl(&resolved, context, Some(vars))
}

#[allow(clippy::while_let_loop)] // 显式 loop + let-else 便于多处 break，改成 while let 反而难读
fn expand_embedded_impl(rule: &str, context: &str, mut vars: Option<&mut RuleVars>) -> String {
    if !rule.contains("{{") {
        return rule.to_string();
    }
    let mut result = rule.to_string();
    loop {
        let Some(start) = result.find("{{") else {
            break;
        };
        let Some(end_rel) = crate::parser::rule::find_closing_double_brace(&result[start + 2..])
        else {
            break;
        };
        let end = start + 2 + end_rel;
        let inner = &result[start + 2..end];
        let mut replacement = String::new();
        if inner.starts_with("$.") || inner.starts_with("$[") || inner.starts_with('{') {
            // JSONPath 内嵌：从上下文（可能是 JSON 对象文本）提取
            let values = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(inner, context, v),
                None => apply(inner, context),
            };
            if let Some(v) = values.first() {
                replacement = v.clone();
            }
        } else if inner.starts_with('@') || inner.starts_with("//") {
            // 规则引用（legado isRule：@ 开头 / // 开头 → 递归按规则求值）
            let values = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(inner, context, v),
                None => apply(inner, context),
            };
            replacement = values.join("\n");
        } else {
            // E10：JS 表达式（legado {{js}} 模板——注入 result=上下文 + 章节/书上下文，
            // 条目内 src=当前元素 HTML 可用；求值失败 → 空串）
            replacement = crate::parser::rule::inline_js(inner.trim(), context, vars.as_deref());
        }
        result.replace_range(start..=end + 1, &replacement);
    }
    result
}

/// 字段规则应用（上下文为单本书元素 html；无书源桥接——每次 eval 独立空 bridge）
pub(crate) fn field(context: &str, rule: Option<&str>, default: &str) -> String {
    field_impl(context, rule, default, None, None, false)
}

/// 字段规则应用（带书源桥接：搜索流程共享 ns bridge，java.* 可用）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn field_with_bridge(
    context: &str,
    rule: Option<&str>,
    default: &str,
    bridge: Option<&JsBridge>,
) -> String {
    field_impl(context, rule, default, bridge, None, false)
}

/// [`field`] 带变量版本（@put/@get 条目级贯通）
pub(crate) fn field_with_vars(
    context: &str,
    rule: Option<&str>,
    default: &str,
    vars: &mut RuleVars,
) -> String {
    field_impl(context, rule, default, None, Some(vars), false)
}

/// [`field_with_bridge`] 带变量版本
pub(crate) fn field_with_bridge_vars(
    context: &str,
    rule: Option<&str>,
    default: &str,
    bridge: Option<&JsBridge>,
    vars: &mut RuleVars,
) -> String {
    field_impl(context, rule, default, bridge, Some(vars), false)
}

fn field_impl(
    context: &str,
    rule: Option<&str>,
    default: &str,
    bridge: Option<&JsBridge>,
    mut vars: Option<&mut RuleVars>,
    is_url: bool,
) -> String {
    let Some(rule) = rule else {
        return default.to_string();
    };
    // legado 内嵌规则：{{$.xxx}} 从上下文提取替换（v1 支持 JSONPath 内嵌）
    let rule = expand_embedded_impl(rule, context, vars.as_deref_mut());
    // @js: 后缀链（legado）：`提取规则@js:code` → 先提取，结果注入 result 执行 JS
    // （如猫眼章节 URL：$.path@js:java.aesBase64DecodeToString(...)）
    if let Some((main_part, js_code)) = rule.split_once("@js:") {
        let main_part = main_part.trim();
        if !main_part.is_empty() {
            let extracted = if main_part.starts_with("$.") || main_part.starts_with('{') {
                match vars.as_deref_mut() {
                    Some(v) => apply_with_vars(main_part, context, v),
                    None => apply(main_part, context),
                }
            } else if main_part.starts_with("//") {
                crate::parser::xpath::xpath_select(main_part, context)
            } else {
                crate::parser::css_chain::css_chain(main_part, context)
            };
            // AR3：提取段为空 → 整条后缀链终止（legado：段空结果为 null，
            // getString 循环 result?.let 跳过后续 JS，最终返回空串而非以空串续喂）
            let first = match extracted.into_iter().find(|s| !s.is_empty()) {
                Some(s) => s,
                None => return default.to_string(),
            };
            let mut js_env = HashMap::new();
            js_env.insert("result".to_string(), first);
            // E10：@js: 后缀链同样绑定章节/书上下文（baseUrl/src——legacy evalJS 全量注入）
            push_js_context(&mut js_env, vars.as_deref());
            let s = match bridge {
                Some(b) => crate::parser::js::eval_js_with_bridge(js_code.trim(), &js_env, b),
                None => crate::parser::js::eval_js(js_code.trim(), &js_env),
            };
            if let Ok(s) = s {
                if !s.is_empty() {
                    return s;
                }
            }
            return default.to_string();
        }
    }
    let r = parse_rule(&rule);
    match r.kind {
        RuleKind::Css => {
            // 链式 CSS（legado：class./tag./@text/@href 等）；经 apply 执行以保留
            // ##替换链 / <js> 链（apply_post：替换正则/替换串/### 首个匹配）
            let v = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(&rule, context, v),
                None => apply(&rule, context),
            };
            if is_url {
                // legado getString0（isUrl=true）：URL 字段仍仅取首个结果（AR2 多命中
                // 连接不适用于 URL 字段）
                if let Some(first) = v.first() {
                    // 无 @ 的单选择器规则：元素 HTML → 取文本（兼容旧书源写法）
                    if !r.body.contains('@') {
                        // jsoup text() 语义：跳过 script/style 子树
                        let txt = visible_text(first);
                        if !txt.is_empty() {
                            return txt;
                        }
                    }
                    return first.clone();
                }
                return default.to_string();
            }
            // AR2：legacy AnalyzeByJSoup.getString 为全量语义——所有非空结果以 "\n"
            // 连接（getStringList(...).joinToString("\n")），非仅首个命中
            let items: Vec<String> = v.into_iter().filter(|s| !s.is_empty()).collect();
            if items.is_empty() {
                return default.to_string();
            }
            // 无 @ 的单选择器规则：元素 HTML → 取文本（兼容旧书源写法）
            if !r.body.contains('@') {
                // jsoup text() 语义：跳过 script/style 子树
                let texts: Vec<String> = items
                    .iter()
                    .map(|s| visible_text(s))
                    .filter(|t| !t.is_empty())
                    .collect();
                if !texts.is_empty() {
                    return texts.join("\n");
                }
            }
            items.join("\n")
        }
        RuleKind::JsonPath => {
            let v = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(&rule, context, v),
                None => apply(&rule, context),
            };
            // AR2b：与 CSS 分支同为 legacy getString 全量语义——多值（JSON 数组
            // 展开的正文段落等）以 "\n" 连接，非仅首个命中
            let items: Vec<String> = v.into_iter().filter(|s| !s.is_empty()).collect();
            if items.is_empty() {
                return default.to_string();
            }
            items.join("\n")
        }
        RuleKind::Regex => {
            let v = match vars.as_deref_mut() {
                Some(v) => apply_with_vars(&rule, context, v),
                None => apply(&rule, context),
            };
            v.into_iter().next().unwrap_or_else(|| default.to_string())
        }
        RuleKind::Js => {
            // 纯 JS 字段规则（@js:/js: 前缀——parse_rule 已剥前缀，body 即代码）：
            // 注入 result=上下文执行；数组/对象结果自动 JSON 化（js_result_to_string）
            let mut js_env = HashMap::new();
            js_env.insert("result".to_string(), context.to_string());
            js_env.insert("key".to_string(), String::new());
            js_env.insert("page".to_string(), "1".to_string());
            // E10：字段级 JS 同样绑定章节/书上下文（baseUrl/src——legacy evalJS 全量注入）
            push_js_context(&mut js_env, vars.as_deref());
            let s = match bridge {
                Some(b) => crate::parser::js::eval_js_with_bridge(&r.body, &js_env, b),
                None => crate::parser::js::eval_js(&r.body, &js_env),
            }
            .unwrap_or_default();
            if s.is_empty() {
                default.to_string()
            } else {
                s
            }
        }
        _ => default.to_string(),
    }
}

/// 元素 HTML 的可见文本（jsoup text()：跳过 script/style 子树 + trim）
fn visible_text(html: &str) -> String {
    let doc = scraper::Html::parse_fragment(html);
    collect_visible_text(doc.root_element())
}

fn collect_visible_text(el: scraper::ElementRef<'_>) -> String {
    if el.value().name() == "script" || el.value().name() == "style" {
        return String::new();
    }
    let mut s = String::new();
    for child in el.children() {
        match child.value() {
            scraper::node::Node::Text(txt) => s.push_str(&txt.text),
            scraper::node::Node::Element(_) => {
                if let Some(e) = scraper::ElementRef::wrap(child) {
                    s.push_str(&collect_visible_text(e));
                }
            }
            _ => {}
        }
    }
    s.trim().to_string()
}

#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn opt_field(context: &str, rule: Option<&str>) -> Option<String> {
    opt_field_with_bridge(context, rule, None)
}

#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
pub(crate) fn opt_field_with_bridge(
    context: &str,
    rule: Option<&str>,
    bridge: Option<&JsBridge>,
) -> Option<String> {
    let v = field_with_bridge(context, rule, "", bridge);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// [`opt_field`] 带变量版本
pub(crate) fn opt_field_with_vars(
    context: &str,
    rule: Option<&str>,
    vars: &mut RuleVars,
) -> Option<String> {
    let v = field_with_vars(context, rule, "", vars);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// [`opt_field_with_bridge`] 带变量版本
pub(crate) fn opt_field_with_bridge_vars(
    context: &str,
    rule: Option<&str>,
    bridge: Option<&JsBridge>,
    vars: &mut RuleVars,
) -> Option<String> {
    let v = field_with_bridge_vars(context, rule, "", bridge, vars);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}
