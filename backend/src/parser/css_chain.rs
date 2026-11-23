//! legado 链式 CSS（对齐 AnalyzeByJSoup getElementsSingle / getResultLast / RuleAnalyzer）
//!
//! 支持：
//! - `<js>...</js>` / `@js:` 链（规则结果进 JS——result 变量；委托 rule::apply）
//! - 组合分隔：`&&` 多规则合并 / `||` 首个命中即止 / `%%` 按位交错（对齐 RuleAnalyzer splitRule）
//! - `@` 链式步进：选择器 / children / parent / 索引；末段属性/文本提取
//! - 索引：`tag.p.2` / `tag.p!0`（! = 排除）/ 负数 / `tag.p[1:3]` 区间 / `[!0,2]` 排除集 /
//!   `[-1:0]` 反向（对齐 ElementsSingle.findIndexSet）
//! - 简写：`class.x` / `id.x` / `tag.x` / `text.x`（ownText 包含）/ `children`
//! - 属性选择器 [a=b]/[a$=b]/[a^=b]/[a*=b]（scraper 原生）；[a~=regex]（jsoup 语义=正则匹配）
//! - 末段提取：text/textNodes/ownText/html（去 script/style）/all/href/src/content/任意属性
//! - 单段规则 CSS 解析失败 → 正则回退（legacy 兼容；GAP 153：lookbehind 经 fancy-regex）

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::LazyLock;

use parking_lot::Mutex;
use scraper::{ElementRef, Html, Selector};

use crate::parser::rule;

/// CSS `Selector` 全局 LRU 编译缓存容量（避免列表项/字段反复解析同一选择器）
const SELECTOR_CACHE_MAX: usize = 512;

/// 同线程已解析 `scraper::Html` 4 槽位 LRU 缓存容量（典型命中：外层整页 + 当前条目 + 片段）
const HTML_CACHE_SLOTS: usize = 4;
/// 单条 HTML 缓存上限（4 MiB），防止超大响应长驻线程局部存储
const HTML_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;

struct SelectorCache {
    entries: HashMap<String, (u64, Result<Selector, ()>)>,
    clock: u64,
    #[cfg(test)]
    hits: usize,
}

impl SelectorCache {
    fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(64),
            clock: 0,
            #[cfg(test)]
            hits: 0,
        }
    }

    fn get(&mut self, sel: &str) -> Option<Result<Selector, ()>> {
        let (tick, val) = self.entries.get_mut(sel)?;
        self.clock = self.clock.wrapping_add(1);
        *tick = self.clock;
        #[cfg(test)]
        {
            self.hits += 1;
        }
        Some(val.clone())
    }

    fn insert(&mut self, sel: String, val: Result<Selector, ()>) {
        self.clock = self.clock.wrapping_add(1);
        let tick = self.clock;
        if let Some(entry) = self.entries.get_mut(&sel) {
            entry.0 = tick;
            entry.1 = val;
            return;
        }
        if self.entries.len() >= SELECTOR_CACHE_MAX {
            if let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&victim);
            }
        }
        self.entries.insert(sel, (tick, val));
    }
}

static SELECTOR_CACHE: LazyLock<Mutex<SelectorCache>> =
    LazyLock::new(|| Mutex::new(SelectorCache::new()));

/// 全局 LRU 缓存的 CSS 选择器编译入口（收敛 `css_chain` 与 `bridge_dom` 全部 `Selector::parse`）。
/// 解析失败同样缓存 `Err(())`，避免正则回退规则在列表循环中反复触发选择器解析失败。
pub(crate) fn parse_selector_cached(sel: &str) -> Option<Selector> {
    if let Some(hit) = SELECTOR_CACHE.lock().get(sel) {
        return hit.ok();
    }
    let parsed = Selector::parse(sel).map_err(|_| ());
    SELECTOR_CACHE
        .lock()
        .insert(sel.to_string(), parsed.clone());
    parsed.ok()
}

struct HtmlCacheEntry {
    hash: u64,
    is_fragment: bool,
    raw: String,
    doc: Rc<Html>,
    tick: u64,
}

struct HtmlCache {
    slots: Vec<HtmlCacheEntry>,
    clock: u64,
    #[cfg(test)]
    hits: usize,
}

impl HtmlCache {
    fn new() -> Self {
        Self {
            slots: Vec::with_capacity(HTML_CACHE_SLOTS),
            clock: 0,
            #[cfg(test)]
            hits: 0,
        }
    }

    fn get(&mut self, hash: u64, is_fragment: bool, raw: &str) -> Option<Rc<Html>> {
        for entry in &mut self.slots {
            if entry.hash == hash && entry.is_fragment == is_fragment && entry.raw == raw {
                self.clock = self.clock.wrapping_add(1);
                entry.tick = self.clock;
                #[cfg(test)]
                {
                    self.hits += 1;
                }
                return Some(Rc::clone(&entry.doc));
            }
        }
        None
    }

    fn insert(&mut self, hash: u64, is_fragment: bool, raw: &str, doc: Rc<Html>) {
        if raw.len() > HTML_CACHE_MAX_BYTES {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        let tick = self.clock;
        for entry in &mut self.slots {
            if entry.hash == hash && entry.is_fragment == is_fragment && entry.raw == raw {
                entry.doc = doc;
                entry.tick = tick;
                return;
            }
        }
        let new_entry = HtmlCacheEntry {
            hash,
            is_fragment,
            raw: raw.to_string(),
            doc,
            tick,
        };
        if self.slots.len() < HTML_CACHE_SLOTS {
            self.slots.push(new_entry);
        } else if let Some((idx, _)) = self.slots.iter().enumerate().min_by_key(|(_, e)| e.tick) {
            self.slots[idx] = new_entry;
        }
    }
}

thread_local! {
    static HTML_CACHE: RefCell<HtmlCache> = RefCell::new(HtmlCache::new());
}

fn hash_html(raw: &str) -> u64 {
    let mut h = DefaultHasher::new();
    raw.hash(&mut h);
    h.finish()
}

fn parse_html_cached(raw: &str, is_fragment: bool) -> Rc<Html> {
    let hash = hash_html(raw);
    if let Some(hit) = HTML_CACHE.with(|c| c.borrow_mut().get(hash, is_fragment, raw)) {
        return hit;
    }
    let parsed = Rc::new(if is_fragment {
        Html::parse_fragment(raw)
    } else {
        Html::parse_document(raw)
    });
    HTML_CACHE.with(|c| {
        c.borrow_mut()
            .insert(hash, is_fragment, raw, Rc::clone(&parsed))
    });
    parsed
}

/// 同线程 4 槽位缓存的 `Html::parse_document`：同一页面/条目连续提取多字段零重复建树。
pub(crate) fn parse_html_document_cached(doc_html: &str) -> Rc<Html> {
    parse_html_cached(doc_html, false)
}

/// 同线程 4 槽位缓存的 `Html::parse_fragment`（供 `bridge_dom` 元素属性/文本/子选择复用）。
pub(crate) fn parse_html_fragment_cached(frag_html: &str) -> Rc<Html> {
    parse_html_cached(frag_html, true)
}

/// 链式 CSS 入口。含 `<js>`/`@js:` 标记时委托规则引擎（类型逐段识别）；
/// 否则走纯 CSS 链。
pub fn css_chain(rule: &str, html: &str) -> Vec<String> {
    if rule::contains_js_marker(rule) {
        return rule::apply(rule, html);
    }
    css_chain_plain(rule, html)
}

/// 纯 CSS 链（无 JS 段）：`##` 尾段剥离 + `&&`/`||`/`%%` 组合
fn css_chain_plain(rule: &str, html: &str) -> Vec<String> {
    let main = rule.split("##").next().unwrap_or(rule).trim();
    if main.is_empty() {
        return vec![];
    }
    let (sep, subs) = rule::split_combined(main);
    let mut groups: Vec<Vec<String>> = Vec::new();
    for sub in subs {
        let r = css_chain_single(sub.trim(), html);
        if !r.is_empty() {
            groups.push(r);
            if sep == Some("||") {
                break;
            }
        }
    }
    match sep {
        // legado %%：按 results[0] 的行号交错取各组同位置元素
        Some("%%") => {
            let mut out = Vec::new();
            if let Some(first) = groups.first() {
                for i in 0..first.len() {
                    for g in &groups {
                        if i < g.len() {
                            out.push(g[i].clone());
                        }
                    }
                }
            }
            out
        }
        _ => groups.into_iter().flatten().collect(),
    }
}

/// `@` 切分（平衡组保护）：引号（'/\"）内与 `[]`/`()` 平衡组内的 `@` 不作分隔符。
///
/// 正则回退规则的 `@` 常位于分组内（如 `(?:@|＠)\S+`）或引号内
/// （如 `a[href*='a@b.com']`），盲切会切碎规则导致解析失败静默空。
fn split_at_chain(rule: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = Vec::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut depth: usize = 0;
    let mut start = 0usize;
    for (i, c) in rule.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '[' | '(' if !in_single && !in_double => depth += 1,
            ']' | ')' if !in_single && !in_double => depth = depth.saturating_sub(1),
            '@' if !in_single && !in_double && depth == 0 => {
                parts.push(&rule[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&rule[start..]);
    parts
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// 单条 CSS 链：`@` 切分；末段为提取器 → 提取；否则全为选择器步进 → 返回元素 HTML
fn css_chain_single(rule: &str, doc_html: &str) -> Vec<String> {
    let doc = parse_html_document_cached(doc_html);
    let parts = split_at_chain(rule);
    if parts.is_empty() {
        return vec![];
    }
    // current 为空 = 整个文档（对齐 legado：elements 初始为 Document 本身）
    let mut current: Vec<ElementRef> = Vec::new();
    let last = parts.len() - 1;
    let mut selector_parse_failed = false;
    for (i, part) in parts.iter().enumerate() {
        if i == last && is_attr_extractor(part) {
            return extract_attr(&doc, &current, part);
        }
        let next = step_elements(&doc, &current, part, &mut selector_parse_failed);
        if next.is_empty() {
            // E13（legacy AnalyzeByJSoup.getResultLast:214-256）：末段选择失败 →
            // 按**任意属性名**提取（img@srcset / video@poster / time@datetime /
            // 自定义 data-* 等——不限于白名单）；提取为空再走原有回退
            if i == last && !current.is_empty() {
                let by_attr = extract_attr(&doc, &current, part);
                if !by_attr.is_empty() {
                    return by_attr;
                }
            }
            // 单段规则：CSS 选择器无法解析（如 legacy 正则规则）→ 正则回退
            if parts.len() == 1 && selector_parse_failed {
                if let Ok(re) = crate::util::regex::Regex::new(&selector_part(part)) {
                    let r: Vec<String> = re
                        .captures_iter(doc_html)
                        .map(|c| {
                            c.get(1)
                                .or_else(|| c.get(0))
                                .map(|m| m.as_str().trim().to_string())
                                .unwrap_or_default()
                        })
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !r.is_empty() {
                        return r;
                    }
                }
            }
            return vec![];
        }
        current = next;
    }
    // 全为选择器（无末段提取）：返回元素 HTML（bookList 场景）
    current.iter().map(|e| e.html()).collect()
}

/// 单段步进（对齐 legado getElementsSingle + findIndexSet）
/// 索引作用于每个当前元素的独立结果集（legado 逐元素语义）
fn step_elements<'a>(
    doc: &'a Html,
    current: &[ElementRef<'a>],
    part: &str,
    selector_parse_failed: &mut bool,
) -> Vec<ElementRef<'a>> {
    let (before_rule, exclude, indices) = parse_index_spec(part);
    let mut out: Vec<ElementRef<'a>> = Vec::new();
    if current.is_empty() {
        out.extend(step_one(
            doc,
            None,
            &before_rule,
            exclude,
            &indices,
            selector_parse_failed,
        ));
    } else {
        for e in current {
            out.extend(step_one(
                doc,
                Some(*e),
                &before_rule,
                exclude,
                &indices,
                selector_parse_failed,
            ));
        }
    }
    out
}

/// 单元素步进：选择 + 索引过滤（beforeRule 关键词分发，对齐 legado）
fn step_one<'a>(
    doc: &'a Html,
    root: Option<ElementRef<'a>>,
    before_rule: &str,
    exclude: bool,
    indices: &[IndexSpec],
    selector_parse_failed: &mut bool,
) -> Vec<ElementRef<'a>> {
    // beforeRule 关键词分发（对齐 legado：children/class/tag/id/text；parent 为扩展）
    let mut els: Vec<ElementRef<'a>> = if before_rule.is_empty() {
        // 索引直接作用于子元素（允许索引作为根）
        children_one(doc, root)
    } else {
        let rules: Vec<&str> = before_rule.split('.').collect();
        match rules[0] {
            "children" => children_one(doc, root),
            "class" if rules.len() > 1 => {
                let (name, pseudo) = extract_jsoup_pseudo(rules[1]);
                let els = if let Some(sel) = parse_selector(
                    &format!(".{}", css_escape_ident(&name)),
                    selector_parse_failed,
                ) {
                    select_one(doc, root, &sel)
                } else {
                    // CSS 类名转义失败（罕见）→ 按 class 属性 token 过滤
                    select_all_one(doc, root)
                        .into_iter()
                        .filter(|e| e.value().classes().any(|c| c == name.as_str()))
                        .collect()
                };
                apply_pseudo_filters(els, &pseudo)
            }
            "id" if rules.len() > 1 => {
                let (name, pseudo) = extract_jsoup_pseudo(rules[1]);
                let els = if let Some(sel) = parse_selector(
                    &format!("#{}", css_escape_ident(&name)),
                    selector_parse_failed,
                ) {
                    select_one(doc, root, &sel)
                } else {
                    select_all_one(doc, root)
                        .into_iter()
                        .filter(|e| e.value().attr("id") == Some(name.as_str()))
                        .collect()
                };
                apply_pseudo_filters(els, &pseudo)
            }
            "tag" if rules.len() > 1 => {
                let (name, pseudo) = extract_jsoup_pseudo(rules[1]);
                let els = if let Some(sel) = parse_selector(&name, selector_parse_failed) {
                    select_one(doc, root, &sel)
                } else {
                    select_all_one(doc, root)
                        .into_iter()
                        .filter(|e| e.value().name() == name.as_str())
                        .collect()
                };
                apply_pseudo_filters(els, &pseudo)
            }
            // text.x → ownText 包含 x（jsoup getElementsContainingOwnText）
            "text" if rules.len() > 1 => select_all_one(doc, root)
                .into_iter()
                .filter(|e| own_text(e).contains(rules[1]))
                .collect(),
            // parent（扩展：取当前元素的父元素）
            "parent" if rules.len() == 1 => match root {
                Some(e) => e.parent().and_then(ElementRef::wrap).into_iter().collect(),
                None => vec![],
            },
            _ => {
                // jsoup ~= 是正则匹配语义（CSS 是单词匹配）——预提取 [attr~=regex] 后置过滤
                let (base, regex_filters) = extract_tilde_attr(before_rule);
                let base = normalize_selector(&base);
                // jsoup 扩展伪类（:eq/:lt/:gt/:contains/:matches）scraper 不支持——
                // 送解析前提取为后置过滤条件，按组合器分段左到右求值
                let plan = plan_css_selector(&base);
                if plan.iter().any(|(_, _, f)| !f.is_empty()) {
                    let mut els = select_staged(doc, root, &plan, selector_parse_failed);
                    if !regex_filters.is_empty() {
                        els.retain(|e| {
                            regex_filters.iter().all(|(attr, re)| {
                                e.attr(attr).map(|v| re.is_match(v)).unwrap_or(false)
                            })
                        });
                    }
                    els
                } else if regex_filters.is_empty() {
                    match parse_selector(&base, selector_parse_failed) {
                        Some(sel) => select_one(doc, root, &sel),
                        None => vec![],
                    }
                } else {
                    let sel = match parse_selector(&base, selector_parse_failed) {
                        Some(s) => s,
                        None => return vec![],
                    };
                    let els = select_one(doc, root, &sel);
                    els.into_iter()
                        .filter(|e| {
                            regex_filters.iter().all(|(attr, re)| {
                                e.attr(attr).map(|v| re.is_match(v)).unwrap_or(false)
                            })
                        })
                        .collect()
                }
            }
        }
    };
    // 索引过滤（对齐 legado：'!' 排除 / '.' 选择 / 无索引原样返回）
    if !indices.is_empty() {
        let set = resolve_index_set(els.len(), indices);
        if exclude {
            let mut kept: Vec<ElementRef<'a>> = Vec::with_capacity(els.len());
            for (i, e) in els.drain(..).enumerate() {
                if !set.contains(&i) {
                    kept.push(e);
                }
            }
            kept
        } else {
            set.iter().filter_map(|&i| els.get(i).copied()).collect()
        }
    } else {
        els
    }
}

/// 索引规格（对齐 legado ElementsSingle.findIndexSet）
#[derive(Debug, Clone)]
enum IndexSpec {
    One(i64),
    Range(Option<i64>, Option<i64>, i64),
}

/// 解析 `part` 为 (beforeRule, 排除?, 索引列表)
/// - legacy：`tag.p.2` / `tag.p!0:1` / `tag.p.1:3`（: 分隔多索引）
/// - 括号：`tag.p[1:3]` / `tag.p[!0,2]` / `tag.p[-1:0]` / `tag.p[0]`
fn parse_index_spec(part: &str) -> (String, bool, Vec<IndexSpec>) {
    let rus = part.trim();
    if rus.is_empty() {
        return (String::new(), false, vec![]);
    }
    let bytes = rus.as_bytes();
    let mut indices: Vec<IndexSpec> = Vec::new();
    let mut cur_list: Vec<i64> = Vec::new(); // 当前区间右端/间隔
    let mut l = String::new();
    let mut cur_minus = false;

    let push_number = |l: &mut String, cur_minus: &mut bool, _cur_list: &mut Vec<i64>| {
        let n: i64 = if l.is_empty() {
            0
        } else {
            l.parse().unwrap_or(0)
        };
        let n = if *cur_minus { -n } else { n };
        *l = String::new();
        *cur_minus = false;
        n
    };

    if rus.ends_with(']') {
        // 常规索引写法 [index...]，逆向遍历（可无前置规则）
        let mut pos = rus.len().saturating_sub(2); // 指向 ']' 前一位
        let mut split_exclude = false;
        let mut before_rule: Option<String> = None;
        let mut bracket_indices: Vec<IndexSpec> = Vec::new();
        loop {
            let rl = bytes[pos];
            if rl == b' ' {
                // 跳过空格
            } else if rl.is_ascii_digit() {
                l.insert(0, rl as char);
            } else if rl == b'-' {
                cur_minus = true;
            } else {
                let cur_int: Option<i64> = if l.is_empty() {
                    None
                } else {
                    Some(if cur_minus {
                        -l.parse::<i64>().unwrap_or(0)
                    } else {
                        l.parse().unwrap_or(0)
                    })
                };
                l.clear();
                cur_minus = false;
                match rl {
                    b':' => {
                        cur_list.push(cur_int.unwrap_or(0));
                    }
                    _ => {
                        // 区间或单个索引加入集合
                        if cur_list.is_empty() {
                            match cur_int {
                                None => break, // 是 jsoup 选择器而非索引列表，跳出
                                Some(n) => bracket_indices.push(IndexSpec::One(n)),
                            }
                        } else {
                            let end = cur_list.pop().unwrap_or(0);
                            let step = if cur_list.is_empty() {
                                1
                            } else {
                                cur_list.pop().unwrap_or(1)
                            };
                            cur_list.clear();
                            bracket_indices.push(IndexSpec::Range(cur_int, Some(end), step));
                        }
                        if rl == b'!' {
                            split_exclude = true;
                            // 对齐 legado：读取 '!' 前的字符（跳过空格）判断结构
                            let mut prev = pos.saturating_sub(1);
                            while prev > 0 && bytes[prev] == b' ' {
                                prev -= 1;
                            }
                            if bytes[prev] == b'[' {
                                before_rule = Some(rus[..prev].to_string());
                                break;
                            }
                            if bytes[prev] != b',' {
                                break; // 非索引结构，跳出
                            }
                        }
                        if rl == b'[' {
                            before_rule = Some(rus[..pos].to_string());
                            break;
                        }
                        if rl != b',' {
                            break; // 非索引结构，跳出
                        }
                    }
                }
            }
            if pos == 0 {
                break;
            }
            pos -= 1;
        }
        match before_rule {
            Some(br) => {
                // 逆向扫描压入，需反转恢复书写顺序（对齐 legado 双重反转）
                indices.extend(bracket_indices.into_iter().rev());
                (br, split_exclude, indices)
            }
            None => {
                // 未闭合索引结构（如 a[href] 属性选择器）→ 整体作为选择器
                (rus.to_string(), false, vec![])
            }
        }
    } else {
        // 阅读原本写法，逆向遍历（可无前置规则）
        let mut len = rus.len();
        let mut split_exclude = false;
        let mut before_rule: Option<String> = None;
        let mut legacy_indices: Vec<i64> = Vec::new();
        loop {
            if len == 0 || len > bytes.len() {
                break;
            }
            len -= 1;
            let rl = bytes[len];
            if rl == b' ' {
                continue;
            }
            if rl.is_ascii_digit() {
                l.insert(0, rl as char);
            } else if rl == b'-' {
                cur_minus = true;
            } else if rl == b'!' || rl == b'.' || rl == b':' {
                let n = push_number(&mut l, &mut cur_minus, &mut cur_list);
                legacy_indices.push(n);
                if rl != b':' {
                    split_exclude = rl == b'!';
                    before_rule = Some(rus[..len].to_string());
                    break;
                }
            } else {
                break; // 非索引结构，跳出
            }
        }
        match before_rule {
            Some(br) => {
                // 逆向扫描压入，需反转恢复升序（对齐 legado indexDefault 双重反转）
                indices.extend(legacy_indices.into_iter().rev().map(IndexSpec::One));
                (br, split_exclude, indices)
            }
            None => (rus.to_string(), false, vec![]),
        }
    }
}

/// 解析索引集合并按 legado 语义展开（负数回绕 / 区间 / 反向 / 去重保持书写顺序）
fn resolve_index_set(len: usize, indices: &[IndexSpec]) -> Vec<usize> {
    let mut set: Vec<usize> = Vec::new();
    let push = |set: &mut Vec<usize>, i: usize| {
        if !set.contains(&i) {
            set.push(i);
        }
    };
    let len_i = len as i64;
    for spec in indices {
        match spec {
            IndexSpec::One(it) => {
                if *it >= 0 {
                    if *it < len_i {
                        push(&mut set, *it as usize);
                    }
                } else if len_i >= -*it {
                    push(&mut set, (*it + len_i) as usize);
                }
            }
            IndexSpec::Range(start_x, end_x, step_x) => {
                let mut start = start_x.unwrap_or(0);
                if start < 0 {
                    start += len_i;
                }
                let mut end = end_x.unwrap_or(len_i - 1);
                if end < 0 {
                    end += len_i;
                }
                if (start < 0 && end < 0) || (start >= len_i && end >= len_i) {
                    continue; // start 和 end 同侧越界，无效区间
                }
                if start >= len_i {
                    start = len_i - 1;
                } else if start < 0 {
                    start = 0;
                }
                if end >= len_i {
                    end = len_i - 1;
                } else if end < 0 {
                    end = 0;
                }
                if start == end || *step_x >= len_i {
                    push(&mut set, start as usize);
                    continue;
                }
                let step = if *step_x > 0 {
                    *step_x
                } else if -*step_x < len_i {
                    *step_x + len_i
                } else {
                    1
                };
                if end > start {
                    let mut i = start;
                    while i <= end {
                        push(&mut set, i as usize);
                        i += step;
                    }
                } else {
                    let mut i = start;
                    while i >= end {
                        push(&mut set, i as usize);
                        i -= step;
                    }
                }
            }
        }
    }
    set
}

/// 单元素/文档选择
fn select_one<'a>(
    doc: &'a Html,
    root: Option<ElementRef<'a>>,
    sel: &Selector,
) -> Vec<ElementRef<'a>> {
    match root {
        Some(e) => e.select(sel).collect(),
        None => doc.select(sel).collect(),
    }
}

/// 单元素/文档全部后代（用于关键字过滤）
fn select_all_one<'a>(doc: &'a Html, root: Option<ElementRef<'a>>) -> Vec<ElementRef<'a>> {
    match root {
        Some(e) => e.descendent_elements().collect(),
        None => doc.root_element().descendent_elements().collect(),
    }
}

/// children 步进：无上下文 → 文档根元素（对齐 legado Document.children() = [html]）
fn children_one<'a>(doc: &'a Html, root: Option<ElementRef<'a>>) -> Vec<ElementRef<'a>> {
    match root {
        Some(e) => e.child_elements().collect(),
        None => vec![doc.root_element()],
    }
}

/// 解析 CSS 选择器（经全局 LRU 缓存）；失败置位（单段规则触发正则回退）
fn parse_selector(sel: &str, selector_parse_failed: &mut bool) -> Option<Selector> {
    match parse_selector_cached(sel) {
        Some(s) => Some(s),
        None => {
            *selector_parse_failed = true;
            None
        }
    }
}

/// 末段是否为属性/文本提取器（legado getResultLast 关键字 + 常见属性名）
fn is_attr_extractor(part: &str) -> bool {
    let p = part.to_ascii_lowercase();
    matches!(
        p.as_str(),
        "text"
            | "textnodes"
            | "owntext"
            | "html"
            | "all"
            | "outerhtml"
            | "href"
            | "src"
            | "content"
            | "value"
            | "title"
            | "alt"
            | "name"
            | "id"
            | "class"
            | "data-src"
            | "data-original"
            | "data-url"
            | "data-id"
            | "data-bookid"
            | "data-book"
            | "data-name"
            | "data-href"
            | "data-type"
            | "data-value"
            | "data-title"
            | "data-index"
            | "data-num"
    ) || p.starts_with("data-")
        || p.starts_with("aria-")
}

/// legado 简写转换：class.active → .active；tag.div → div；id.xxx → #xxx
/// （含 CSS 标识符转义——类名可能含特殊字符）
fn normalize_selector(sel: &str) -> String {
    if let Some(c) = sel.strip_prefix("class.") {
        format!(".{}", css_escape_ident(c))
    } else if let Some(c) = sel.strip_prefix("id.") {
        format!("#{}", css_escape_ident(c))
    } else if let Some(c) = sel.strip_prefix("tag.") {
        c.to_string()
    } else {
        sel.to_string()
    }
}

/// 选择器部分的原始文本（正则回退用——去掉索引后缀）
fn selector_part(part: &str) -> String {
    let (before, _, _) = parse_index_spec(part);
    normalize_selector(&before)
}

/// CSS 标识符转义：非 [a-zA-Z0-9_-] 字符 → \XX（码点十六进制）转义
fn css_escape_ident(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        let first = i == 0;
        let ok =
            c.is_ascii_alphanumeric() || c == '-' || c == '_' || (!first && c.is_ascii_digit());
        if ok {
            out.push(c);
        } else {
            out.push('\\');
            out.push_str(&format!("{:X} ", c as u32));
        }
    }
    out
}

/// 提取 `[attr~=regex]`（jsoup 语义：正则匹配，大小写不敏感）→ (基础选择器, 过滤器)
///
/// P2：按 char 扫描（char_indices）——旧实现按字节逐位 `c as char`，非 ASCII
/// （中文选择器/属性值）被拆成乱码字节，且 `sel[i..]`/`inner[i..]` 在非字符边界
/// 切片会 panic。
fn extract_tilde_attr(sel: &str) -> (String, Vec<(String, crate::util::regex::Regex)>) {
    let chars: Vec<(usize, char)> = sel.char_indices().collect();
    let mut base = String::new();
    let mut filters: Vec<(String, crate::util::regex::Regex)> = Vec::new();
    let mut i = 0; // 字符下标
    let mut in_single = false;
    let mut in_double = false;
    while i < chars.len() {
        let (_, c) = chars[i];
        if c == '\'' && !in_double {
            in_single = !in_single;
            base.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            base.push(c);
            i += 1;
            continue;
        }
        if c == '[' && !in_single && !in_double {
            // 找到匹配的 ]
            let mut j = i + 1; // 字符下标
            let mut qs = false;
            let mut qd = false;
            while j < chars.len() {
                let d = chars[j].1;
                if d == '\'' && !qd {
                    qs = !qs;
                } else if d == '"' && !qs {
                    qd = !qd;
                } else if d == ']' && !qs && !qd {
                    break;
                }
                j += 1;
            }
            if j >= chars.len() {
                // 未闭合——原样保留
                base.push_str(&sel[chars[i].0..]);
                break;
            }
            let j_byte = chars[j].0;
            let inner = &sel[chars[i].0 + 1..j_byte];
            if let Some(tilde_pos) = find_attr_op(inner, "~=") {
                let attr = inner[..tilde_pos].trim();
                let val = inner[tilde_pos + 2..].trim();
                let val = val.trim_matches(|c| c == '\'' || c == '"');
                if !attr.is_empty() && !val.is_empty() {
                    if let Ok(re) = crate::util::regex::RegexBuilder::new(val)
                        .case_insensitive(true)
                        .build()
                    {
                        filters.push((attr.to_string(), re));
                        i = j + 1;
                        continue;
                    }
                }
            }
            base.push_str(&sel[chars[i].0..=j_byte]);
            i = j + 1;
            continue;
        }
        base.push(c);
        i += 1;
    }
    (base, filters)
}

/// 在属性选择器内容中查找操作符位置（跳过引号内）；返回字节偏移（字符边界）
fn find_attr_op(inner: &str, op: &str) -> Option<usize> {
    let mut qs = false;
    let mut qd = false;
    for (i, c) in inner.char_indices() {
        if c == '\'' && !qd {
            qs = !qs;
        } else if c == '"' && !qs {
            qd = !qd;
        } else if !qs && !qd && inner[i..].starts_with(op) {
            return Some(i);
        }
    }
    None
}

/// CSS 组合器类型（分段求值用；首段组合器无意义）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comb {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

/// jsoup 扩展伪类提取出的后置过滤条件（scraper 不支持这些伪类，直接解析必失败）
#[derive(Debug, Clone)]
enum PseudoFilter {
    /// :eq(n) —— 兄弟位置等于 n（jsoup 语义：父节点下全部子节点序，含文本节点）
    Eq(i64),
    /// :lt(n) —— 兄弟位置小于 n
    Lt(i64),
    /// :gt(n) —— 兄弟位置大于 n
    Gt(i64),
    /// :contains(t) —— 元素全文包含 t（jsoup 大小写不敏感；存小写形式）
    Contains(String),
    /// :matches(re) —— 元素全文正则匹配（java Pattern.find 语义：任意位置命中）
    Matches(crate::util::regex::Regex),
}

impl PseudoFilter {
    fn matches(&self, el: &ElementRef) -> bool {
        match self {
            Self::Eq(n) => sibling_index(el) as i64 == *n,
            Self::Lt(n) => (sibling_index(el) as i64) < *n,
            Self::Gt(n) => (sibling_index(el) as i64) > *n,
            // jsoup :contains/:matches 作用于元素全文（text() 归一化语义）
            Self::Contains(t) => collapse_ws(&text_without_scripts(el))
                .to_lowercase()
                .contains(t),
            Self::Matches(re) => re.is_match(&collapse_ws(&text_without_scripts(el))),
        }
    }
}

/// 选择器求值计划：(组合器, 段选择器, 该段伪类过滤)
type SelectorPlan = Vec<(Comb, String, Vec<PseudoFilter>)>;

/// 选择器预处理计划：按组合器切段，逐段提取 jsoup 扩展伪类为后置过滤。
/// 无伪类时各段过滤器为空——调用方可据此走 scraper 原生单次解析路径。
fn plan_css_selector(base: &str) -> SelectorPlan {
    split_combinator_segments(base)
        .into_iter()
        .map(|(comb, seg)| {
            let (clean, filters) = extract_jsoup_pseudo(&seg);
            (comb, clean, filters)
        })
        .collect()
}

/// 提取 jsoup 扩展伪类 `:eq(n)/:lt(n)/:gt(n)/:contains(t)/:matches(re)`
/// → (剩余选择器, 后置过滤条件)。参数内引号/嵌套括号/转义不参与识别；
/// 括号未闭合或数值非法时原样保留（保持解析失败 → 正则回退的既有语义）。
fn extract_jsoup_pseudo(sel: &str) -> (String, Vec<PseudoFilter>) {
    let chars: Vec<(usize, char)> = sel.char_indices().collect();
    let mut base = String::new();
    let mut filters: Vec<PseudoFilter> = Vec::new();
    let mut qs = false;
    let mut qd = false;
    let mut i = 0;
    while i < chars.len() {
        let (byte, c) = chars[i];
        if c == '\'' && !qd {
            qs = !qs;
            base.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !qs {
            qd = !qd;
            base.push(c);
            i += 1;
            continue;
        }
        if qs || qd || c != ':' {
            base.push(c);
            i += 1;
            continue;
        }
        let rest = &sel[byte..];
        let name = [":eq(", ":lt(", ":gt(", ":contains(", ":matches("]
            .into_iter()
            .find(|p| rest.starts_with(p));
        let Some(name) = name else {
            base.push(c);
            i += 1;
            continue;
        };
        let arg_start = byte + name.len();
        let Some(arg_len) = scan_paren_close(&sel[arg_start..]) else {
            base.push_str(rest);
            break;
        };
        let end_byte = arg_start + arg_len + 1; // 含 ')'
        while i < chars.len() && chars[i].0 < end_byte {
            i += 1;
        }
        let arg = sel[arg_start..arg_start + arg_len].trim();
        let filter = match name {
            ":eq(" => arg.parse::<i64>().ok().map(PseudoFilter::Eq),
            ":lt(" => arg.parse::<i64>().ok().map(PseudoFilter::Lt),
            ":gt(" => arg.parse::<i64>().ok().map(PseudoFilter::Gt),
            ":contains(" => Some(PseudoFilter::Contains(arg.to_lowercase())),
            ":matches(" => crate::util::regex::Regex::new(arg)
                .ok()
                .map(PseudoFilter::Matches),
            _ => None,
        };
        match filter {
            Some(f) => filters.push(f),
            None => base.push_str(&sel[byte..end_byte]),
        }
    }
    (base, filters)
}

/// 扫描到与起始 `(` 配对的 `)`，返回 `)` 的字节偏移（处理转义/引号/嵌套）；
/// 未闭合返回 None
fn scan_paren_close(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut qs = false;
    let mut qd = false;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' => esc = true,
            '\'' if !qd => qs = !qs,
            '"' if !qs => qd = !qd,
            '(' if !qs && !qd => depth += 1,
            ')' if !qs && !qd => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// 按 CSS 组合器切分为段：(组合器, 段文本)。引号/`[]`/`()` 内的组合符不参与
/// （属性值与伪类参数可含空格及 `>+~`，如 `[title*="a b"]`、`:nth-child(2n+1)`）。
fn split_combinator_segments(sel: &str) -> Vec<(Comb, String)> {
    let chars: Vec<char> = sel.chars().collect();
    let mut out: Vec<(Comb, String)> = Vec::new();
    let mut cur = String::new();
    let mut qs = false;
    let mut qd = false;
    let mut br = 0usize;
    let mut pa = 0usize;
    let mut pending = Comb::Descendant;

    fn flush(out: &mut Vec<(Comb, String)>, cur: &mut String, comb: Comb) {
        let s = cur.trim().to_string();
        cur.clear();
        if !s.is_empty() {
            out.push((comb, s));
        }
    }

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !qd {
            qs = !qs;
            cur.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !qs {
            qd = !qd;
            cur.push(c);
            i += 1;
            continue;
        }
        if qs || qd {
            cur.push(c);
            i += 1;
            continue;
        }
        match c {
            '[' | '(' => {
                if c == '[' {
                    br += 1;
                } else {
                    pa += 1;
                }
                cur.push(c);
            }
            ']' | ')' => {
                if c == ']' {
                    br = br.saturating_sub(1);
                } else {
                    pa = pa.saturating_sub(1);
                }
                cur.push(c);
            }
            '>' | '+' | '~' if br == 0 && pa == 0 => {
                flush(&mut out, &mut cur, pending);
                pending = match c {
                    '>' => Comb::Child,
                    '+' => Comb::Adjacent,
                    _ => Comb::Sibling,
                };
                // 跳过组合符后的空白（a > b）
                while i + 1 < chars.len() && chars[i + 1].is_whitespace() {
                    i += 1;
                }
            }
            c if br == 0 && pa == 0 && c.is_whitespace() => {
                // 向前看：组合符前的空白留给组合符分支；尾随空白丢弃；否则段结束
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j >= chars.len() {
                    // 尾随空白
                } else if matches!(chars[j], '>' | '+' | '~') {
                    // 组合符前导空白：保留 cur 待组合符分支 flush
                } else {
                    flush(&mut out, &mut cur, pending);
                    pending = Comb::Descendant;
                }
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    flush(&mut out, &mut cur, pending);
    out
}

/// 分段求值选择器（对齐 jsoup 左到右组合语义），逐段应用伪类后置过滤。
/// 首段作用域：root 有值 = root 子树（含 root 自身）；无值 = 全文档后代。
fn select_staged<'a>(
    doc: &'a Html,
    root: Option<ElementRef<'a>>,
    plan: &SelectorPlan,
    selector_parse_failed: &mut bool,
) -> Vec<ElementRef<'a>> {
    let mut matched: Option<Vec<ElementRef<'a>>> = None;
    for (comb, sel, filters) in plan {
        let universe: Vec<ElementRef<'a>> = match (&matched, comb) {
            (None, _) => match root {
                Some(e) => std::iter::once(e).chain(e.descendent_elements()).collect(),
                None => doc.root_element().descendent_elements().collect(),
            },
            (Some(ms), Comb::Descendant) => ms
                .iter()
                .flat_map(|e| std::iter::once(*e).chain(e.descendent_elements()))
                .collect(),
            (Some(ms), Comb::Child) => ms.iter().flat_map(|e| e.child_elements()).collect(),
            (Some(ms), Comb::Adjacent) => ms.iter().filter_map(next_element_sibling).collect(),
            (Some(ms), Comb::Sibling) => ms.iter().flat_map(following_element_siblings).collect(),
        };
        let parsed = match parse_selector(sel, selector_parse_failed) {
            Some(s) => s,
            None => return vec![],
        };
        let mut seen: Vec<_> = Vec::new();
        let mut next: Vec<ElementRef<'a>> = Vec::new();
        for el in universe {
            if seen.contains(&el.id()) {
                continue;
            }
            if parsed.matches(&el) && filters.iter().all(|f| f.matches(&el)) {
                seen.push(el.id());
                next.push(el);
            }
        }
        matched = Some(next);
    }
    matched.unwrap_or_default()
}

/// 应用伪类后置过滤（空条件原样返回；class/id/tag 简写路径复用）
fn apply_pseudo_filters<'a>(
    mut els: Vec<ElementRef<'a>>,
    filters: &[PseudoFilter],
) -> Vec<ElementRef<'a>> {
    if !filters.is_empty() {
        els.retain(|e| filters.iter().all(|f| f.matches(e)));
    }
    els
}

/// 兄弟位置（jsoup siblingIndex 语义：父节点下全部子节点序，含文本节点）
fn sibling_index(el: &ElementRef) -> usize {
    let mut idx = 0;
    let mut n = el.prev_sibling();
    while let Some(node) = n {
        idx += 1;
        n = node.prev_sibling();
    }
    idx
}

/// 紧邻的后一个元素兄弟（`A + B` 右侧候选）
fn next_element_sibling<'a>(el: &ElementRef<'a>) -> Option<ElementRef<'a>> {
    let mut n = el.next_sibling();
    while let Some(node) = n {
        if let Some(e) = ElementRef::wrap(node) {
            return Some(e);
        }
        n = node.next_sibling();
    }
    None
}

/// 全部后续元素兄弟（`A ~ B` 右侧候选）
fn following_element_siblings<'a>(el: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    let mut out = Vec::new();
    let mut n = el.next_sibling();
    while let Some(node) = n {
        if let Some(e) = ElementRef::wrap(node) {
            out.push(e);
        }
        n = node.next_sibling();
    }
    out
}

/// 单元素直接文本（ownText 语义：直接文本节点，空白折叠 + 首尾修剪）
fn own_text(el: &ElementRef) -> String {
    let mut s = String::new();
    for child in el.children() {
        if let scraper::node::Node::Text(txt) = child.value() {
            s.push_str(&txt.text);
        }
    }
    collapse_ws(&s)
}

/// 空白折叠（jsoup normaliseWhitespace：连续空白 → 单空格 + 首尾修剪）
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_ws = true;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_ws {
                out.push(' ');
            }
            last_ws = true;
        } else {
            out.push(c);
            last_ws = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// 空白折叠但保留块级换行（`\u{0}` 标记作为段落边界，原文空白仍按 jsoup 折叠）。
/// 书源正文 `@text` 规则常见 `<p>/<div>/<br>` 结构，jsoup 老版本 text() 会把
/// 块级边界折叠成空格，导致 Reader Dev 前端整章无换行；这里在折叠前保留换行。
fn collapse_ws_keep_breaks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_ws = false;
    let mut last_nl = false;
    for c in s.chars() {
        if c == '\u{0}' {
            if !last_nl {
                out.push('\n');
                last_nl = true;
            }
            last_ws = true;
        } else if c.is_whitespace() {
            if !last_ws {
                out.push(' ');
            }
            last_ws = true;
            last_nl = false;
        } else {
            out.push(c);
            last_ws = false;
            last_nl = false;
        }
    }
    while out.ends_with(' ') || out.ends_with('\n') {
        out.pop();
    }
    while out.starts_with(' ') || out.starts_with('\n') {
        out.remove(0);
    }
    out
}

/// 块级元素（文本提取时作为段落边界插入换行）
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "br" | "p"
            | "div"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "tr"
            | "dd"
            | "dt"
            | "hr"
            | "section"
            | "article"
            | "blockquote"
            | "table"
            | "ul"
            | "ol"
            | "pre"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "summary"
            | "details"
            | "figure"
            | "figcaption"
            | "caption"
            | "thead"
            | "tbody"
            | "tfoot"
            | "center"
            | "address"
            | "fieldset"
            | "legend"
            | "menu"
            | "dir"
            | "colgroup"
    )
}

/// 属性/文本提取（对齐 legado getResultLast）
fn extract_attr<'a>(doc: &'a Html, current: &[ElementRef<'a>], attr: &str) -> Vec<String> {
    let attr_l = attr.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    // 无上下文 → 根元素（legado：Document 本身）
    let items: Vec<ElementRef<'a>> = if current.is_empty() {
        // 属性提取器（href/src 等）：取文档中第一个元素——legacy 元素级上下文语义
        //（裸 "href" 规则用于章节目录：item 上下文是 a 元素 HTML——应从 a 取属性而非根）
        match attr_l.as_str() {
            "text" | "textnodes" | "owntext" | "html" | "all" | "outerhtml" => {
                vec![doc.root_element()]
            }
            _ => doc
                .root_element()
                .descendants()
                .filter_map(|n| match n.value() {
                    // 找第一个含该属性的元素（parse_document 包裹 html/body——首个后代是
                    // body 等无属性节点，须跳过；legacy 元素级上下文语义）
                    scraper::node::Node::Element(e) if e.attr(&attr_l).is_some() => {
                        ElementRef::wrap(n)
                    }
                    _ => None,
                })
                .take(1)
                .collect::<Vec<ElementRef<'a>>>(),
        }
    } else {
        current.to_vec()
    };
    for el in items {
        match attr_l.as_str() {
            "text" => {
                // legado Jsoup text()：跳过 script/style 子树（scraper text() 会混入脚本内容）
                // 块级元素保留换行（Reader Dev 纯文本阅读器需要段落边界）
                let t = collapse_ws_keep_breaks(&text_without_scripts(&el));
                if !t.is_empty() {
                    out.push(t);
                }
            }
            "textnodes" => {
                let tn: Vec<String> = el
                    .children()
                    .filter_map(|n| match n.value() {
                        scraper::node::Node::Text(txt) => {
                            let t = txt.text.trim().to_string();
                            if t.is_empty() {
                                None
                            } else {
                                Some(t)
                            }
                        }
                        _ => None,
                    })
                    .collect();
                if !tn.is_empty() {
                    out.push(tn.join("\n"));
                }
            }
            "owntext" => {
                let t = own_text(&el);
                if !t.is_empty() {
                    out.push(t);
                }
            }
            "html" => {
                // legado：先移除 script/style 再取 outerHTML
                let h = html_without_scripts(&el);
                if !h.is_empty() {
                    out.push(h);
                }
            }
            "all" | "outerhtml" => {
                let h = el.html();
                if !h.is_empty() {
                    out.push(h);
                }
            }
            _ => {
                // 属性（href/src/content/data-*…）：去空白 + 去重（legado attr 分支）
                if let Some(v) = el.value().attr(&attr_l) {
                    let v = v.trim().to_string();
                    if !v.is_empty() && !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
        }
    }
    out
}

/// 元素可见文本（jsoup text() 语义 + 块级换行保留）：跳过 script/style 子树，
/// 块级元素/`<br>` 之间插入换行（jsoup 老版本 text() 折叠为空格，正文会丢失段落）
fn text_without_scripts(el: &ElementRef) -> String {
    let name = el.value().name();
    if name == "script" || name == "style" {
        return String::new();
    }
    let mut s = String::new();
    for child in el.children() {
        match child.value() {
            scraper::node::Node::Text(txt) => s.push_str(&txt.text),
            scraper::node::Node::Element(_) => {
                if let Some(e) = ElementRef::wrap(child) {
                    let child_name = e.value().name();
                    let block = is_block_tag(child_name);
                    if block && !s.is_empty() && !s.ends_with('\u{0}') {
                        // 内部标记：与原文空白区分，折叠后再统一转 \n
                        s.push('\u{0}');
                    }
                    let inner = text_without_scripts(&e);
                    s.push_str(&inner);
                    if block && !inner.is_empty() && !s.ends_with('\u{0}') {
                        s.push('\u{0}');
                    }
                }
            }
            _ => {}
        }
    }
    s
}

/// 元素 outerHTML，但移除内部 script/style（legado @html 语义）
fn html_without_scripts(el: &ElementRef) -> String {
    let mut frag = Html::parse_fragment(&el.html());
    let Some(sel) = parse_selector_cached("script, style") else {
        return el.html();
    };
    let ids: Vec<_> = frag.select(&sel).map(|e| e.id()).collect();
    for id in ids {
        if let Some(mut n) = frag.tree.get_mut(id) {
            n.detach();
        }
    }
    let h = frag.html();
    // parse_fragment 包裹的 html/body 外壳剥除
    let h = h.strip_prefix("<html><body>").unwrap_or(&h);
    h.strip_suffix("</body></html>").unwrap_or(h).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// E13：末段任意属性提取（白名单外属性——legacy getResultLast attr() 兜底）
    #[test]
    fn test_last_segment_arbitrary_attr() {
        // srcset/poster/datetime 均不在白名单
        let html = r#"<video poster="p.jpg"></video><img srcset="s.jpg 1x"><time datetime="2025-01-02"></time>"#;
        assert_eq!(
            css_chain("video@poster", html),
            vec!["p.jpg".to_string()],
            "poster 应按任意属性提取"
        );
        assert_eq!(
            css_chain("img@srcset", html),
            vec!["s.jpg 1x".to_string()],
            "srcset 应按任意属性提取"
        );
        assert_eq!(
            css_chain("time@datetime", html),
            vec!["2025-01-02".to_string()],
            "datetime 应按任意属性提取"
        );
        // 属性确实不存在 → 空（不影响原有正则回退路径）
        assert!(css_chain("video@nope", html).is_empty());
        // 白名单内行为不变
        assert_eq!(
            css_chain("img@data-src", r#"<img data-src="/d.jpg">"#),
            vec!["/d.jpg".to_string()]
        );
    }

    /// 裸属性提取器（legacy 章节目录：chapterUrl=\"href\" 应用于单个 a 元素上下文）——
    /// 从文档第一个元素取属性，而非根元素（根无 href 导致 url 全空）
    #[test]
    fn test_bare_attr_extractor_uses_first_element() {
        let html = r#"<a href="/novel/471547/read_1.html" title="第1章">第1章 世界</a>"#;
        let v = crate::parser::css_chain::css_chain("href", html);
        assert_eq!(
            v.first().map(|s| s.as_str()),
            Some("/novel/471547/read_1.html"),
            "裸 href 应取第一个元素属性: {v:?}"
        );
        // 多元素：取第一个
        let html2 = r#"<a href="/a/1.html">A</a><a href="/b/2.html">B</a>"#;
        let v2 = crate::parser::css_chain::css_chain("href", html2);
        assert_eq!(v2.first().map(|s| s.as_str()), Some("/a/1.html"));
        // src 同理
        let html3 = r#"<img src="/img/cover.jpg">"#;
        let v3 = crate::parser::css_chain::css_chain("src", html3);
        assert_eq!(v3.first().map(|s| s.as_str()), Some("/img/cover.jpg"));
    }

    #[test]
    fn test_chain_booklist() {
        let html = r#"<ul class="ItemListbody"><li><a href="/b/1">书1</a></li><li><a href="/b/2">书2</a></li></ul>"#;
        let r = css_chain("ul.ItemListbody@li", html);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn test_chain_field_text() {
        let html = r#"<li><span>书名</span><dd>作者</dd></li>"#;
        let r = css_chain("li@span@text", html);
        assert_eq!(r, vec!["书名".to_string()]);
    }

    #[test]
    fn test_chain_index() {
        let html = r#"<li><dd>甲</dd><dd>乙</dd></li>"#;
        let r = css_chain("li@dd.1@text", html);
        assert_eq!(r, vec!["乙".to_string()]);
    }

    #[test]
    fn test_chain_href() {
        let html = r#"<li><a href="/book/9">x</a></li>"#;
        let r = css_chain("li@a@href", html);
        assert_eq!(r, vec!["/book/9".to_string()]);
    }

    #[test]
    fn test_chain_and_rules() {
        let html = r#"<p>甲</p><span>乙</span>"#;
        let r = css_chain("p@text&&span@text", html);
        assert_eq!(r, vec!["甲".to_string(), "乙".to_string()]);
    }

    #[test]
    fn test_legado_shortcuts() {
        let html = r#"<div class="active"><span>书名</span></div><div class="active"><span>书2</span></div>"#;
        let r = css_chain("class.active@span@text", html);
        assert_eq!(r, vec!["书名".to_string(), "书2".to_string()]);
        let html2 = r#"<ul id="list"><li>x</li></ul>"#;
        let r2 = css_chain("id.list@li@text", html2);
        assert_eq!(r2, vec!["x".to_string()]);
    }

    #[test]
    fn test_chain_own_text() {
        let html = r#"<div>直接<span>子</span></div>"#;
        let r = css_chain("div@ownText", html);
        assert_eq!(r, vec!["直接".to_string()]);
        // jsoup ownText：直接文本节点按原文空白折叠拼接（元素子节点跳过）
        let html2 = r#"<div>a <b>x</b> c</div>"#;
        let r2 = css_chain("div@ownText", html2);
        assert_eq!(r2, vec!["a c".to_string()]);
        let html3 = r#"<div>a<b>x</b>c</div>"#;
        assert_eq!(css_chain("div@ownText", html3), vec!["ac".to_string()]);
    }

    #[test]
    fn test_chain_text_normalization() {
        // 纯文本空白折叠为单空格（无块级边界时不引入换行）
        let html = "<div>书名：\n    测试书\t 作者</div>";
        let r = css_chain("div@text", html);
        assert_eq!(r, vec!["书名： 测试书 作者".to_string()]);
    }

    #[test]
    fn test_chain_text_keeps_block_newlines() {
        // 块级元素/`<br>` 必须保留为段落边界（Reader Dev 纯文本渲染依赖 \n）
        let html = r#"<div id="content"><p>第一段</p><p>第二段</p><br><p>第三段</p></div>"#;
        let r = css_chain("id.content@text", html);
        assert_eq!(r, vec!["第一段\n第二段\n第三段".to_string()]);
        // 行内元素不产生换行
        let html2 = r#"<div><span>甲</span><span>乙</span></div>"#;
        let r2 = css_chain("div@text", html2);
        assert_eq!(r2, vec!["甲乙".to_string()]);
        // script/style 跳过且不产生空段
        let html3 =
            r#"<div id="c"><script>var x=1;</script><p>一</p><style>.a{}</style><p>二</p></div>"#;
        let r3 = css_chain("id.c@text", html3);
        assert_eq!(r3, vec!["一\n二".to_string()]);
    }

    #[test]
    fn test_chain_text_nodes() {
        let html = r#"<div>甲<span>子</span>乙</div>"#;
        let r = css_chain("div@textNodes", html);
        assert_eq!(r, vec!["甲\n乙".to_string()]);
    }

    #[test]
    fn test_chain_html_extractor_removes_scripts() {
        let html =
            r#"<div class="c">正文<script>var x=1;</script><style>.a{}</style><p>内容</p></div>"#;
        let r = css_chain("class.c@html", html);
        assert_eq!(r.len(), 1);
        assert!(
            !r[0].contains("<script>"),
            "html 提取应移除 script: {}",
            r[0]
        );
        assert!(!r[0].contains("<style>"), "html 提取应移除 style: {}", r[0]);
        assert!(r[0].contains("<p>内容</p>"));
        // all 保留 script/style
        let r2 = css_chain("class.c@all", html);
        assert!(r2[0].contains("<script>"));
    }

    #[test]
    fn test_chain_attr_extractors() {
        let html =
            r#"<meta name="description" content="简介内容"><img data-src="/a.jpg" src="/b.jpg">"#;
        assert_eq!(
            css_chain("meta@content", html),
            vec!["简介内容".to_string()]
        );
        assert_eq!(css_chain("img@data-src", html), vec!["/a.jpg".to_string()]);
        assert_eq!(css_chain("img@src", html), vec!["/b.jpg".to_string()]);
        // 属性去重
        let html2 = r#"<a href="/x">1</a><a href="/x">2</a>"#;
        assert_eq!(css_chain("a@href", html2), vec!["/x".to_string()]);
    }

    #[test]
    fn test_chain_children() {
        let html = r#"<div class="w"><span>一</span><b>二</b></div>"#;
        // children 步进 = 直接子元素
        let r = css_chain("class.w@children@text", html);
        assert_eq!(r, vec!["一".to_string(), "二".to_string()]);
        // 真实书源：@children@children@class.x 深链（children 是容器，class.x 在其中）
        let html2 = r#"<div class="a"><div><div><span class="m">元1</span><span class="m">元2</span></div></div></div>"#;
        let r2 = css_chain("class.a@children@children@class.m@text", html2);
        assert_eq!(r2, vec!["元1".to_string(), "元2".to_string()]);
    }

    #[test]
    fn test_chain_parent() {
        let html = r#"<ul><li>一</li><li>二</li></ul>"#;
        // 每个 li 的父元素（ul）HTML——两个 li 共享同一父 → 2 份（元素列表语义）
        let r = css_chain("li@parent", html);
        assert_eq!(r.len(), 2);
        assert!(r[0].starts_with("<ul>"));
        assert_eq!(r[0], r[1]);
        // 链式：父元素文本（li 为块级 → 换行分隔）
        let r2 = css_chain("li@parent@text", html);
        assert_eq!(r2, vec!["一\n二".to_string(), "一\n二".to_string()]);
    }

    #[test]
    fn test_index_exclusion_legacy() {
        // tag.p!0 = 排除第一个（真实书源：@@class.smallreadbody!0@span@text）
        let html = r#"<div class="x"><p>一</p><p>二</p><p>三</p></div>"#;
        let r = css_chain("class.x@tag.p!0@text", html);
        assert_eq!(r, vec!["二".to_string(), "三".to_string()]);
        // 多索引排除 !0:1
        let r2 = css_chain("class.x@tag.p!0:1@text", html);
        assert_eq!(r2, vec!["三".to_string()]);
        // 负索引排除（div!-1 = 排除最后一个）
        let r3 = css_chain("class.x@tag.p!-1@text", html);
        assert_eq!(r3, vec!["一".to_string(), "二".to_string()]);
    }

    #[test]
    fn test_index_negative_and_bracket() {
        let html = r#"<ul><li>一</li><li>二</li><li>三</li><li>四</li></ul>"#;
        // 负索引（真实书源：tag.em.-1@text / tag.tr.-2）
        assert_eq!(css_chain("ul@li.-1@text", html), vec!["四".to_string()]);
        // [n] 索引
        assert_eq!(css_chain("ul@li[1]@text", html), vec!["二".to_string()]);
        // 区间 [1:3] → 1,2,3（legado Kotlin 区间 end 包含）
        assert_eq!(
            css_chain("ul@li[1:3]@text", html),
            vec!["二".to_string(), "三".to_string(), "四".to_string()]
        );
        // 排除集 [!0,2]
        assert_eq!(
            css_chain("ul@li[!0,2]@text", html),
            vec!["二".to_string(), "四".to_string()]
        );
        // 反向 [-1:0]（legado 特殊用法：任意位置让列表反向）
        assert_eq!(
            css_chain("ul@li[-1:0]@text", html),
            vec![
                "四".to_string(),
                "三".to_string(),
                "二".to_string(),
                "一".to_string()
            ]
        );
        // 步进 [0:4:2] → 0,2
        assert_eq!(
            css_chain("ul@li[0:4:2]@text", html),
            vec!["一".to_string(), "三".to_string()]
        );
    }

    #[test]
    fn test_attr_selectors() {
        let html = r#"<a href="http://a.com/1">1</a><a href="https://b.com/2">2</a><a href="/rel">3</a><a class="x y">4</a><a class="xy">5</a>"#;
        // [a=b]
        assert_eq!(
            css_chain("a[href='http://a.com/1']@text", html),
            vec!["1".to_string()]
        );
        // [a^=b] 前缀
        assert_eq!(
            css_chain("a[href^='https']@text", html),
            vec!["2".to_string()]
        );
        // [a$=b] 后缀
        assert_eq!(
            css_chain("a[href$='.com/2']@text", html),
            vec!["2".to_string()]
        );
        // [a*=b] 包含
        assert_eq!(
            css_chain("a[href*='b.com']@text", html),
            vec!["2".to_string()]
        );
        // [a~=b]：jsoup 语义 = 正则匹配（大小写不敏感）
        assert_eq!(
            css_chain("a[class~='x']@text", html),
            vec!["4".to_string(), "5".to_string()]
        );
        assert_eq!(
            css_chain("a[class~='^xy$']@text", html),
            vec!["5".to_string()]
        );
    }

    #[test]
    fn test_tag_chain() {
        let html = r#"<ul><li><a href="/1">链接1</a></li><li><a href="/2">链接2</a></li></ul>"#;
        // tag.li@tag.a 链（真实书源：tag.span.1@a@text / tag.strong@a@text）
        let r = css_chain("tag.ul@tag.li@tag.a@href", html);
        assert_eq!(r, vec!["/1".to_string(), "/2".to_string()]);
    }

    #[test]
    fn test_or_rules() {
        let html = r#"<div class="a">甲</div>"#;
        // || 首个命中即止（真实书源：@@class.h5-4con@html||class.error_msg@text）
        assert_eq!(
            css_chain("class.a@text||class.missing@text", html),
            vec!["甲".to_string()]
        );
        assert_eq!(
            css_chain("class.missing@text||class.a@text", html),
            vec!["甲".to_string()]
        );
        // || 与 ## 替换共存：替换由 rule.rs apply_post 在结果上应用
    }

    #[test]
    fn test_percent_interleave() {
        // 真实书源：class.bookinfo@a.1@text%%class.bookinfo@span.0@text
        // 索引逐元素作用：每个 bookinfo 内 a[1]、span[0]
        let html = r#"<div class="bookinfo"><a>甲书</a><a>甲书2</a><span>甲作者</span></div><div class="bookinfo"><a>乙书</a><a>乙书2</a><span>乙作者</span></div>"#;
        let r = css_chain("class.bookinfo@a.1@text%%class.bookinfo@span.0@text", html);
        assert_eq!(
            r,
            vec![
                "甲书2".to_string(),
                "甲作者".to_string(),
                "乙书2".to_string(),
                "乙作者".to_string()
            ]
        );
    }

    #[test]
    fn test_comma_group_selector() {
        // 真实书源：@@h3,li —— CSS 选择器组
        let html = r#"<h3>标题</h3><li>项</li>"#;
        let r = css_chain("h3,li@text", html);
        assert_eq!(r, vec!["标题".to_string(), "项".to_string()]);
    }

    #[test]
    fn test_text_keyword_contains_own_text() {
        // text.x → ownText 包含 x（jsoup getElementsContainingOwnText）
        let html = r#"<div class="l"><span>首页</span><span>书城</span></div>"#;
        let r = css_chain("class.l@text.首页@text", html);
        assert_eq!(r, vec!["首页".to_string()]);
    }

    #[test]
    fn test_single_selector_returns_elements() {
        let html = r#"<div class="book"><a>书名A</a></div><div class="book"><a>书名B</a></div>"#;
        let r = css_chain("div.book", html);
        assert_eq!(r.len(), 2);
        assert!(r[0].starts_with("<div class=\"book\">"));
    }

    #[test]
    fn test_css_parse_failure_regex_fallback_only() {
        // 合法 CSS 但无匹配 → 空（不再触发正则回退）
        let html = r#"<div>书名：测试书</div>"#;
        assert!(css_chain("span.nope", html).is_empty());
        // 非法 CSS（legacy 正则规则）→ 正则回退
        let html2 = "书名：测试书 作者：张三";
        let r = css_chain("书名：(.+?)\\s", html2);
        assert_eq!(r, vec!["测试书".to_string()]);
    }

    /// GAP 153：CSS 解析失败回退正则时支持 lookbehind（fancy-regex 升级）
    #[test]
    fn test_chain_regex_fallback_lookbehind() {
        let html = "书名：测试书 作者：张三";
        let r = css_chain("(?<=书名：)\\S+", html);
        assert_eq!(r, vec!["测试书".to_string()]);
    }

    #[test]
    fn test_chain_js_segment() {
        // <js> 链：规则结果进 JS（result 变量），再进后续规则
        let html = r#"<div class="b">abc123</div>"#;
        // 提取后进 JS（@js: 链）
        let r2 = css_chain("class.b@text@js:result.replace('abc','xyz')", html);
        assert_eq!(r2, vec!["xyz123".to_string()]);
        // <js> 在前：JS 处理（result=原文档）后接 CSS 链
        let r = css_chain("<js>result.replace('abc','xyz')</js>@class.b@text", html);
        assert_eq!(r, vec!["xyz123".to_string()]);
        // @js: 贪婪匹配到末尾（legado JS_PATTERN 同语义）——第二个 @js: 并入首个 JS 代码 → JS 语法错误 → 空
        let r3 = css_chain(
            "class.b@text@js:result.replace('abc','xyz')@js:result.toUpperCase()",
            html,
        );
        assert!(r3.is_empty());
    }

    // ==================== P2：非 ASCII 按 char 处理（中文选择器/属性值不按字节拆） ====================

    /// extract_tilde_attr：中文属性值（~= 正则）——旧实现 find_attr_op 按字节切片
    /// 在非字符边界 panic；按 char 后正常提取过滤
    #[test]
    fn test_extract_tilde_attr_chinese_value() {
        // 中文正则值（旧实现：find_attr_op 内 inner[i..] 在汉字中间切片 → panic）
        let (base, filters) = extract_tilde_attr("div[title~=中文]");
        assert_eq!(base, "div");
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].0, "title");
        assert!(filters[0].1.is_match("中文内容"), "正则应匹配中文");
        assert!(!filters[0].1.is_match("英文"));

        // 中文选择器（base）原样保留——不再按字节拆成乱码
        let (base, filters) = extract_tilde_attr("div.书[title~=ok]");
        assert_eq!(base, "div.书");
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].0, "title");

        // 无 ~= 的普通属性选择器原样保留（含中文）
        let (base, filters) = extract_tilde_attr("div[data-名=值]");
        assert_eq!(base, "div[data-名=值]");
        assert!(filters.is_empty());

        // 未闭合 [ 原样保留（含中文）不 panic
        let (base, filters) = extract_tilde_attr("div[title~=中文");
        assert_eq!(base, "div[title~=中文");
        assert!(filters.is_empty());

        // 引号内 ~= 不参与
        let (base, filters) = extract_tilde_attr("div[data-x='a~=b']");
        assert_eq!(base, "div[data-x='a~=b']");
        assert!(filters.is_empty());
    }

    /// css_chain 端到端：中文属性正则过滤正常出结果（旧实现此路径 panic）
    #[test]
    fn test_css_chain_chinese_tilde_filter() {
        let html =
            r#"<div title="中文内容"><p>命中</p></div><div title="English"><p>不命中</p></div>"#;
        let r = css_chain("div[title~=中文]@p@text", html);
        assert_eq!(r, vec!["命中".to_string()]);
        // 中文属性值 + 中文选择器链
        let html2 = r#"<div class="书架"><span data-名="好书">甲</span></div>"#;
        let r2 = css_chain("div.书架 span[data-名~=好书]@text", html2);
        assert_eq!(r2, vec!["甲".to_string()]);
    }

    // ==================== P1：@ 切分平衡组保护 ====================

    /// 正则回退规则中 @ 位于 (...) 平衡组内不应被切碎
    /// （旧实现盲切：`(?:@|＠)\S+` → "(?:" + "|＠)\S+" 两段 → 静默空）
    #[test]
    fn test_split_at_protected_in_balanced_group() {
        let html = "<p>联系 @张三 或 @李四 </p>";
        let r = css_chain("(?:@|＠)\\S+", html);
        assert_eq!(r, vec!["@张三".to_string(), "@李四".to_string()]);
    }

    /// 引号内的 @ 不切分（属性选择器含 @ 字面量）
    #[test]
    fn test_split_at_protected_in_quotes() {
        let html = r#"<a href="mailto:a@b.com">邮</a>"#;
        assert_eq!(
            css_chain("a[href*='a@b.com']@text", html),
            vec!["邮".to_string()]
        );
    }

    // ==================== P1：jsoup 扩展伪类预处理 ====================

    /// :eq/:lt/:gt —— jsoup 兄弟位置语义；scraper 不支持，须提取为后置过滤
    #[test]
    fn test_jsoup_eq_lt_gt_pseudo() {
        let html = "<ul><li>零</li><li>一</li><li>二</li><li>三</li></ul>";
        assert_eq!(css_chain("ul@li:eq(1)@text", html), vec!["一".to_string()]);
        assert_eq!(
            css_chain("ul@li:lt(2)@text", html),
            vec!["零".to_string(), "一".to_string()]
        );
        assert_eq!(css_chain("ul@li:gt(2)@text", html), vec!["三".to_string()]);
    }

    /// 组合器中间段带伪类：分段左到右求值（eq 过滤 li 而非最终结果）
    #[test]
    fn test_jsoup_pseudo_mid_segment() {
        let html = r#"<div class="m"><ul><li><a>a0</a></li><li><a>a1</a></li></ul></div>"#;
        assert_eq!(
            css_chain("div.m ul li:eq(1) a@text", html),
            vec!["a1".to_string()]
        );
    }

    /// :contains 大小写不敏感包含 / :matches 正则（find 语义）
    #[test]
    fn test_jsoup_contains_matches_pseudo() {
        let html = r#"<p>第一章 开始</p><p>第二章 发展</p><p>尾声</p>"#;
        assert_eq!(
            css_chain("p:contains(开始)@text", html),
            vec!["第一章 开始".to_string()]
        );
        assert_eq!(
            css_chain("p:matches(^第.章)@text", html),
            vec!["第一章 开始".to_string(), "第二章 发展".to_string()]
        );
        // 大小写不敏感（needle 大写、文本小写仍命中）
        assert_eq!(
            css_chain("p:contains(ABC)@text", r#"<p>abc</p>"#),
            vec!["abc".to_string()]
        );
    }

    /// 关键字简写带伪类：tag.li:eq(1) / class.item:eq(0)
    #[test]
    fn test_jsoup_pseudo_with_shortcuts() {
        let html = "<ul><li>零</li><li>一</li></ul>";
        assert_eq!(
            css_chain("ul@tag.li:eq(1)@text", html),
            vec!["一".to_string()]
        );
        let html2 = r#"<div class="item">甲</div><div class="item">乙</div>"#;
        assert_eq!(
            css_chain("class.item:eq(0)@text", html2),
            vec!["甲".to_string()]
        );
    }

    /// R3：CSS Selector 全局 LRU 缓存 + 同线程 HTML DOM 4 槽位缓存命中与结果一致性
    #[test]
    fn test_selector_and_dom_cache_hit_consistency() {
        let page = r#"<div class="card"><h2 class="title-r3">修真聊天群</h2><span class="author-r3">圣骑士的传说</span><a href="/book/99">详情</a></div>"#;
        let item =
            r#"<li class="item-r3"><a href="/c/1">第一章</a><em class="meta-r3">免费</em></li>"#;

        let sel_hits_before = SELECTOR_CACHE.lock().hits;
        let html_hits_before = HTML_CACHE.with(|c| c.borrow().hits);

        // 对同一外层页面连续提取 3 个字段 + 对同一条目片段连续提取 2 个字段，交错验证多槽位共存
        let t1 = css_chain("h2.title-r3@text", page);
        let a1 = css_chain("span.author-r3@text", page);
        let u1 = css_chain("div.card@a@href", page);
        let c1 = css_chain("li.item-r3@a@text", item);
        let m1 = css_chain("li.item-r3@em.meta-r3@text", item);

        // 再次用相同选择器与相同文档查询，必命中 Selector 缓存与 HTML DOM 缓存
        let t2 = css_chain("h2.title-r3@text", page);
        let a2 = css_chain("span.author-r3@text", page);
        let c2 = css_chain("li.item-r3@a@text", item);
        // 片段缓存独立于 document 缓存，且同一片段二次解析必命中
        let f1 = parse_html_fragment_cached(item);
        let f2 = parse_html_fragment_cached(item);
        assert!(Rc::ptr_eq(&f1, &f2), "同线程相同片段应复用同一 Rc<Html>");

        // 非法选择器同样被缓存，不抛错且保持正则回退一致
        let bad_sel = "书名-r3：(.+?)\\s";
        assert!(parse_selector_cached(bad_sel).is_none());
        assert!(parse_selector_cached(bad_sel).is_none());

        assert_eq!(t1, vec!["修真聊天群".to_string()]);
        assert_eq!(t1, t2);
        assert_eq!(a1, vec!["圣骑士的传说".to_string()]);
        assert_eq!(a1, a2);
        assert_eq!(u1, vec!["/book/99".to_string()]);
        assert_eq!(c1, vec!["第一章".to_string()]);
        assert_eq!(c1, c2);
        assert_eq!(m1, vec!["免费".to_string()]);

        let sel_hits_after = SELECTOR_CACHE.lock().hits;
        let html_hits_after = HTML_CACHE.with(|c| c.borrow().hits);
        assert!(
            sel_hits_after > sel_hits_before,
            "Selector LRU 缓存应命中: before={sel_hits_before}, after={sel_hits_after}"
        );
        assert!(
            html_hits_after >= html_hits_before + 6,
            "同线程 HTML DOM 缓存应连续命中: before={html_hits_before}, after={html_hits_after}"
        );
    }
}
