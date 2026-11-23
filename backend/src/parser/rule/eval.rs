//! 规则求值执行引擎（init / 链式 JS / 模板 `{{...}}` / CSS / XPath / Regex / JSONPath）
//! 与同线程 4 槽位已解析 `serde_json::Value` LRU 缓存。

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use super::chain::{
    apply_post, contains_js_marker, find_closing_double_brace, parse_rule, split_combined,
    split_js_chain,
};
use super::vars::{push_js_context, resolve_get, split_put, RuleVars};
use super::{Rule, RuleKind};

/// 同线程已解析 `serde_json::Value` 4 槽位 LRU 缓存容量
/// （典型命中：整页响应 JSON + 当前列表条目 JSON 连续多字段提取，零重复反序列化）
const JSON_CACHE_SLOTS: usize = 4;
/// 单条 JSON 文本缓存上限（4 MiB），防止超大响应长驻线程局部存储
const JSON_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;

struct JsonCacheEntry {
    hash: u64,
    raw: String,
    value: Rc<serde_json::Value>,
    tick: u64,
}

struct JsonValueCache {
    slots: Vec<JsonCacheEntry>,
    clock: u64,
    #[cfg(test)]
    hits: usize,
}

impl JsonValueCache {
    fn new() -> Self {
        Self {
            slots: Vec::with_capacity(JSON_CACHE_SLOTS),
            clock: 0,
            #[cfg(test)]
            hits: 0,
        }
    }

    fn get(&mut self, hash: u64, raw: &str) -> Option<Rc<serde_json::Value>> {
        for entry in &mut self.slots {
            if entry.hash == hash && entry.raw == raw {
                self.clock = self.clock.wrapping_add(1);
                entry.tick = self.clock;
                #[cfg(test)]
                {
                    self.hits += 1;
                }
                return Some(Rc::clone(&entry.value));
            }
        }
        None
    }

    fn insert(&mut self, hash: u64, raw: &str, value: Rc<serde_json::Value>) {
        if raw.len() > JSON_CACHE_MAX_BYTES {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        let tick = self.clock;
        for entry in &mut self.slots {
            if entry.hash == hash && entry.raw == raw {
                entry.value = value;
                entry.tick = tick;
                return;
            }
        }
        let new_entry = JsonCacheEntry {
            hash,
            raw: raw.to_string(),
            value,
            tick,
        };
        if self.slots.len() < JSON_CACHE_SLOTS {
            self.slots.push(new_entry);
        } else if let Some((idx, _)) = self.slots.iter().enumerate().min_by_key(|(_, e)| e.tick) {
            self.slots[idx] = new_entry;
        }
    }
}

thread_local! {
    static JSON_VALUE_CACHE: RefCell<JsonValueCache> = RefCell::new(JsonValueCache::new());
}

fn hash_json(raw: &str) -> u64 {
    let mut h = DefaultHasher::new();
    raw.hash(&mut h);
    h.finish()
}

/// 同线程 4 槽位缓存的 `serde_json::Value` 解析入口（自动 trim，对齐 `is_json_text` 与 JSONPath 键一致性）。
/// 仅缓存 `Ok(Rc<Value>)`，解析失败不挤占有效槽位；解析发生在 `RefCell` 借用之外以防重入 panic。
fn parse_json_value_cached(text: &str) -> serde_json::Result<Rc<serde_json::Value>> {
    let trimmed = text.trim();
    let hash = hash_json(trimmed);
    if let Some(hit) = JSON_VALUE_CACHE.with(|c| c.borrow_mut().get(hash, trimmed)) {
        return Ok(hit);
    }
    let val = Rc::new(parse_json_value(trimmed)?);
    JSON_VALUE_CACHE.with(|c| c.borrow_mut().insert(hash, trimmed, Rc::clone(&val)));
    Ok(val)
}

#[cfg(test)]
pub(super) fn json_cache_hits() -> usize {
    JSON_VALUE_CACHE.with(|c| c.borrow().hits)
}

/// legado init 规则：先提取上下文（CSS/JSONPath/正则/JS），后续字段规则在其上相对应用。
/// 提取为空 → 返回原上下文（不阻断解析链）。JS 规则注入 result=原文。
pub fn apply_init(context: &str, init: Option<&str>) -> String {
    apply_init_impl(context, init, None)
}

/// [`apply_init`] 带变量版本（@put/@get 贯通同一本书的详情/目录/正文流程）
pub fn apply_init_with_vars(context: &str, init: Option<&str>, vars: &mut RuleVars) -> String {
    apply_init_impl(context, init, Some(vars))
}

fn apply_init_impl(context: &str, init: Option<&str>, vars: Option<&mut RuleVars>) -> String {
    let Some(r) = init else {
        return context.to_string();
    };
    let r = r.trim();
    if r.is_empty() {
        return context.to_string();
    }
    let parsed = parse_rule(r);
    let out = match parsed.kind {
        RuleKind::Js => {
            let mut js_vars = std::collections::HashMap::new();
            js_vars.insert("result".to_string(), context.to_string());
            // E10/AR5：init 段 JS 同样注入章节/书上下文（legacy evalJS 全量绑定）
            push_js_context(&mut js_vars, vars.as_deref());
            crate::parser::js::eval_js(&parsed.body, &js_vars).unwrap_or_default()
        }
        _ => apply_depth(r, context, 0, vars)
            .into_iter()
            .next()
            .unwrap_or_default(),
    };
    if out.is_empty() {
        context.to_string()
    } else {
        out
    }
}

/// 对文档执行规则，返回结果列表（含 <js>/@js: 链）
pub fn apply(rule: &str, html: &str) -> Vec<String> {
    apply_depth(rule, html, 0, None)
}

/// [`apply`] 带变量版本：同一流程内多次 apply 共享 `@put`/`@get` 变量
pub fn apply_with_vars(rule: &str, html: &str, vars: &mut RuleVars) -> Vec<String> {
    apply_depth(rule, html, 0, Some(vars))
}

/// 链式执行（legado splitSourceRule：先按 JS 标记切段，逐段顺序执行、结果管道传递）
pub(super) fn apply_depth(
    rule: &str,
    html: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> Vec<String> {
    let segs = split_js_chain(rule);
    if segs.len() == 1 && !segs[0].is_js {
        return apply_single(segs[0].text, html, depth, vars);
    }
    let mut result: Option<Vec<String>> = None;
    for seg in segs {
        // AR3：某段结果为空 → 提前终止整链返回空（legado AnalyzeRule.getString：
        // 段空结果为 null，循环内 result?.let 使后续所有段跳过，最终得空串；
        // 原实现以空串续喂下一段，如 class.missing@text@js:result.length 误得 "0"）
        if result.as_ref().is_some_and(|r| r.is_empty()) {
            return vec![];
        }
        let input = result
            .as_ref()
            .map(|r| r.join("\n"))
            .unwrap_or_else(|| html.to_string());
        if seg.is_js {
            // JS 段：{{...}} 先展开（legado makeUpRule 对 JS 规则同样处理），再以 result 执行
            let code = expand_inline_depth_checked(seg.text, &input, depth, vars.as_deref_mut()).0;
            let mut js_vars = std::collections::HashMap::new();
            js_vars.insert("result".to_string(), input);
            js_vars.insert("key".to_string(), String::new());
            js_vars.insert("page".to_string(), "1".to_string());
            js_vars.insert("baseUrl".to_string(), String::new());
            js_vars.insert("urlSearchSeries".to_string(), String::new());
            js_vars.insert("urlSearch".to_string(), String::new());
            js_vars.insert("url".to_string(), String::new());
            // E10/AR5：章节/书上下文绑定（legacy AnalyzeRule evalJS）
            push_js_context(&mut js_vars, vars.as_deref());
            match crate::parser::js::eval_js(&code, &js_vars) {
                Ok(s) => {
                    // 空串结果 → 空列表；下一轮循环检测到空结果即终止整链（AR3）
                    result = Some(if s.is_empty() { vec![] } else { vec![s] });
                }
                Err(_) => return vec![], // legado：JS 失败 result=null → 整链终止为空
            }
        } else {
            result = Some(apply_single(seg.text, &input, depth, vars.as_deref_mut()));
        }
    }
    result.unwrap_or_default()
}

/// 单条规则执行（parse + {{}} 展开 + 类型分发 + 前缀/替换）
fn apply_single(
    rule_str: &str,
    html: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> Vec<String> {
    // legado SourceRule：先分离 @put（移除并求值存入变量），再 makeUpRule 替换 @get
    let (cleaned, puts) = split_put(rule_str);
    if let Some(v) = vars.as_deref_mut() {
        if !puts.is_empty() {
            apply_put_vars(&puts, html, v, depth);
        }
        let cleaned = resolve_get(&cleaned, v);
        let rule = parse_rule(&cleaned);
        return apply_rule_inner(&rule, html, depth, vars);
    }
    let rule = parse_rule(&cleaned);
    apply_rule_inner(&rule, html, depth, None)
}

/// @put 值求值（legado putRule → getString(value)）：对当前上下文按规则取首个结果；
/// JSON 上下文允许裸字段名（如 `@put:{bid:bookId}`——legado isJSON 下按 JSONPath 处理）
fn apply_put_vars(puts: &[(String, String)], html: &str, vars: &mut RuleVars, depth: usize) {
    for (k, v) in puts {
        let val = if v.trim().is_empty() {
            String::new()
        } else {
            let resolved = resolve_get(v, vars);
            let first = if parse_json_value_cached(html).is_ok() {
                json_path_single(&resolved, html).into_iter().next()
            } else {
                apply_depth(&resolved, html, depth + 1, Some(vars))
                    .into_iter()
                    .next()
            };
            first.unwrap_or_default()
        };
        vars.insert(k.clone(), val);
    }
}

fn apply_rule_inner(
    rule: &Rule,
    html: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> Vec<String> {
    // {{...}} 内嵌表达式：先展开，再重新解析执行（类型可能变化，如 {{$.x}} 拼接出 CSS）
    let rule = if depth < 4 && rule.body.contains("{{") {
        let (expanded, unsafe_value) =
            expand_inline_depth_checked(&rule.body, html, depth, vars.as_deref_mut());
        if expanded != rule.body {
            if unsafe_value {
                // P2：模板替换值本身含规则控制标记（## 段切分 / {{ 二次模板 / @js:<js>
                // JS 标记 / @、// 规则前缀）——视为纯文本结果，不再重新解析执行
                // （防数据驱动二次执行：书内容可借 {{$.x}} 注入 @js: 代码被再次 eval）。
                // 原规则的前缀/替换段仍应用。
                return apply_post(vec![expanded], rule);
            }
            // 重建完整规则串（保留 ##前缀/##替换段）后重新解析
            let mut full = expanded.clone();
            if let Some(p) = &rule.prefix {
                full.push_str("##");
                full.push_str(p);
            } else if let Some(re) = &rule.replace_regex {
                full.push_str("##");
                full.push_str(re);
                if let Some(rep) = &rule.replacement {
                    full.push_str("##");
                    full.push_str(rep);
                    if rule.replace_first {
                        full.push_str("##");
                    }
                }
            }
            let r = apply_single(&full, html, depth + 1, vars.as_deref_mut());
            if r.is_empty() && !expanded.trim().is_empty() {
                // legado：含 {{}} 的规则展开后即结果文本（{{}} 使规则进入 Regex 模式 →
                // 规则串本身即结果）。执行无果时返回展开文本（前缀/替换仍应用）
                return apply_post(vec![expanded], rule);
            }
            return r;
        }
        rule
    } else {
        rule
    };
    // 空规则：纯替换规则（##pat##rep###）→ 直接对输入应用替换；否则空结果
    if rule.body.trim().is_empty() {
        if rule.replace_regex.is_some() || rule.prefix.is_some() {
            return apply_post(vec![html.to_string()], rule);
        }
        return vec![];
    }
    // legacy ar.kt:468-471（setContent isJSON）：内容为 JSON 时，无显式前缀的裸键规则段
    // 强制走 JsonPath 提取（Mode.Default → Mode.Json）；@CSS:/@@/@XPath:/@Json:/js:/
    // $. 等有类型特征的规则不受影响
    let results = if rule.default_mode && rule.kind == RuleKind::Css && is_json_text(html) {
        json_path(rule, html)
    } else {
        match rule.kind {
            RuleKind::Css => css_select(rule, html),
            RuleKind::JsonPath => json_path(rule, html),
            RuleKind::Regex => regex_match(&rule.body, html),
            RuleKind::XPath => xpath_select_rules(rule, html),
            RuleKind::Js => {
                // JS 规则：注入 result/key/page/baseUrl 环境 + 章节/书上下文（E10/AR5）
                let mut js_vars = std::collections::HashMap::new();
                js_vars.insert("result".to_string(), html.to_string());
                js_vars.insert("key".to_string(), String::new());
                js_vars.insert("page".to_string(), "1".to_string());
                js_vars.insert("baseUrl".to_string(), String::new());
                js_vars.insert("urlSearchSeries".to_string(), String::new());
                js_vars.insert("urlSearch".to_string(), String::new());
                js_vars.insert("url".to_string(), String::new());
                push_js_context(&mut js_vars, vars.as_deref());
                match crate::parser::js::eval_js(&rule.body, &js_vars) {
                    Ok(s) if !s.is_empty() => vec![s],
                    _ => vec![],
                }
            }
        }
    };
    // 前缀/替换处理（legado：@@/替换在结果上应用）
    apply_post(results, rule)
}

/// 展开 `{{...}}`（返回展开串 + 是否含规则控制值）：
/// - `{{$.xxx}}` / `{{$[n]}}`：JSONPath 从当前上下文文本提取（复用 json_path 逻辑）
/// - `{{@rule}}` / `{{//xpath}}`：规则引用（legado isRule：@ 开头或 // 开头）
/// - 其他内容：作为 JS 执行（注入 result=上下文文本 / key / page），结果替换回规则
/// - 提取失败 / JS 报错 / 结果为空 → 替换为空串；未闭合的 `{{` → 原样返回
/// - 第二个返回值供调用方决定是否安全地重新解析（P2：含控制标记的值不再二次解析）
fn expand_inline_depth_checked(
    body: &str,
    text: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> (String, bool) {
    let mut out = String::new();
    let mut rest = body;
    let mut unsafe_value = false;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = match find_closing_double_brace(after) {
            Some(e) => e,
            None => return (body.to_string(), false), // 未闭合：不处理
        };
        let expr = after[..end].trim();
        let replaced = if expr.starts_with("$.") || expr.starts_with("$[") {
            inline_json_path(expr, text)
        } else if expr.starts_with('@') || expr.starts_with("//") {
            // legado isRule：@ 开头（@@/@CSS:/@XPath:/@Json:/@js:）或 // → 作为规则递归求值
            apply_depth(expr, text, depth + 1, vars.as_deref_mut()).join("\n")
        } else {
            inline_js(expr, text, vars.as_deref())
        };
        if is_rule_control_value(&replaced) {
            unsafe_value = true;
        }
        out.push_str(&replaced);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    (out, unsafe_value)
}

/// 模板替换值是否含规则控制标记（重新解析会改变语义 / 触发二次执行）：
/// - `##`：段切分（值可拆出新的规则链）；`{{`：二次模板展开
/// - `@js:`/`<js>`（任意位置，大小写不敏感）：JS 代码执行标记
/// - 以 `@` / `//` 开头：规则引用/类型前缀（值被当作规则而非文本）
fn is_rule_control_value(v: &str) -> bool {
    v.contains("##")
        || v.contains("{{")
        || contains_js_marker(v)
        || v.starts_with('@')
        || v.starts_with("//")
}

/// 展开 `{{...}}`（测试便捷入口，无递归深度）
#[cfg(test)]
pub(super) fn expand_inline(body: &str, text: &str) -> String {
    expand_inline_depth_checked(body, text, 0, None).0
}

/// 内嵌 JSONPath：`{{$.a.b}}` / `{{$[0].c}}` → 从上下文文本提取
/// （多结果以换行拼接；无结果 → 空串）
fn inline_json_path(expr: &str, text: &str) -> String {
    let path = if let Some(p) = expr.strip_prefix('$') {
        p // "$.a" → ".a"；"$..a" → "..a"（保留递归标记）；"$[0]" → "[0]"
    } else if let Some(p) = expr.strip_prefix('.') {
        p
    } else {
        expr
    };
    let mut results = vec![];
    match parse_json_value_cached(text) {
        Ok(v) => walk_json(&v, path, &mut results),
        Err(_) => results = json_from_html(path, text),
    }
    if results.is_empty() {
        String::new()
    } else {
        results.join("\n")
    }
}

/// 内嵌 JS：`{{expr}}` → 执行（注入 result=上下文文本 / key / page），失败 → 空串
/// （pub(crate)：search.rs 字段路径的 `{{js}}` 展开复用同一语义）
pub(crate) fn inline_js(expr: &str, text: &str, vars: Option<&RuleVars>) -> String {
    let mut js_vars = std::collections::HashMap::new();
    js_vars.insert("result".to_string(), text.to_string());
    js_vars.insert("key".to_string(), String::new());
    js_vars.insert("page".to_string(), "1".to_string());
    // E10/AR5：{{}} 内嵌 JS 同样绑定章节/书上下文
    push_js_context(&mut js_vars, vars);
    crate::parser::js::eval_js(expr, &js_vars).unwrap_or_default()
}

/// CSS 选择器执行（legado 链式：<js> 链 + &&/||/%% 组合 + @ 链 + 末段属性）
fn css_select(rule: &Rule, html: &str) -> Vec<String> {
    crate::parser::css_chain::css_chain(&rule.body, html)
}

/// XPath 执行（legado AnalyzeByXPath：&&/||/%% 组合）
fn xpath_select_rules(rule: &Rule, html: &str) -> Vec<String> {
    let (sep, subs) = split_combined(&rule.body);
    let mut groups: Vec<Vec<String>> = Vec::new();
    for sub in subs {
        let r = crate::parser::xpath::xpath_select(sub.trim(), html);
        if !r.is_empty() {
            groups.push(r);
            if sep == Some("||") {
                break;
            }
        }
    }
    merge_groups(sep, groups)
}

/// Regex 执行（legado：规则整体当正则，提取 group 1 或全匹配）
/// GAP 153：经 fancy-regex 兼容层编译（支持 lookbehind）；编译失败记日志并返回空
/// E16/EG1：正则匹配（legacy AnalyzeByRegex 对齐）
/// - `&&` 多链：逐条顺序过滤（每条 find_iter 全匹配拼接作为下一条输入）
/// - 末级：captures() 取所有非空捕获组 join("\n") 返回（不只 group(1)||group(0)）
fn regex_match(pattern: &str, text: &str) -> Vec<String> {
    // EG1：&& 多链——legacy AnalyzeByRegex.kt:31-116 按链切分顺序过滤
    if pattern.contains("&&") {
        let mut current = text.to_string();
        for reg in pattern.split("&&") {
            let reg = reg.trim();
            if reg.is_empty() {
                continue;
            }
            match crate::util::regex::Regex::new(reg) {
                Ok(re) => {
                    let mut buf = String::new();
                    for caps in re.captures_iter(&current) {
                        if let Some(m) = caps.get(0) {
                            buf.push_str(m.as_str());
                        }
                    }
                    current = buf;
                }
                Err(e) => {
                    tracing::warn!("正则多链段编译失败（跳过该条）: {e}");
                }
            }
        }
        return if current.is_empty() {
            vec![]
        } else {
            vec![current]
        };
    }

    let re = match crate::util::regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("正则规则编译失败（规则引擎返回空）: {e}");
            return vec![];
        }
    };
    re.captures_iter(text)
        .map(|caps| {
            // EG1：全捕获组提取（group(0)..group(n)），非只 group(1)||group(0)
            let mut parts: Vec<String> = Vec::new();
            let mut gi = 0usize;
            while caps.get(gi).is_some() {
                if let Some(m) = caps.get(gi) {
                    let v = m.as_str().trim().to_string();
                    if !v.is_empty() {
                        parts.push(v);
                    }
                }
                gi += 1;
            }
            if parts.is_empty() {
                String::new()
            } else {
                parts.join("\n")
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// JSONPath 执行（对齐 legado AnalyzeByJSonPath：&&/||/%% 组合 + 完整路径语法）
fn json_path(rule: &Rule, text: &str) -> Vec<String> {
    let (sep, subs) = split_combined(&rule.body);
    let mut groups: Vec<Vec<String>> = Vec::new();
    for sub in subs {
        let r = json_path_single(sub, text);
        if !r.is_empty() {
            groups.push(r);
            if sep == Some("||") {
                break;
            }
        }
    }
    merge_groups(sep, groups)
}

fn merge_groups(sep: Option<&'static str>, groups: Vec<Vec<String>>) -> Vec<String> {
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

/// 单条 JSONPath（输入可能是 JSON 文本或 HTML 中的 JSON 片段）。
/// E14（legacy AnalyzeByJSonPath.kt:38,84 innerRule）：规则中部可内嵌多个 `{$.x}`
/// 子引用——先全部求值替换再拼串（`{$.a.t}-{$.b.t}` → "V1-V2"）；整体恰为一个
/// 内嵌段时走原单路径语义。
fn json_path_single(body: &str, text: &str) -> Vec<String> {
    let trimmed = body.trim();
    // 收集顶层内嵌段（{ 开头、配对 } 结尾、内容形如 $.x / .x / $[i]）
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let bytes = trimmed.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel) = trimmed[i + 1..].find('}') {
                let end = i + 1 + rel + 1;
                let inner = &trimmed[i + 1..end - 1];
                if inner.starts_with("$.") || inner.starts_with("$[") || inner.starts_with('.') {
                    spans.push((i, end));
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    let whole_span =
        spans.len() == 1 && spans[0].0 == 0 && spans[0].1 == trimmed.len() && !spans.is_empty();
    if spans.is_empty() || whole_span {
        return json_path_single_inner(trimmed, text);
    }
    // 模板模式：字面文本与子路径求值结果交替拼接（多值以 \n 连接）
    let mut out = String::new();
    let mut cursor = 0usize;
    for (s, e) in spans {
        out.push_str(&trimmed[cursor..s]);
        let sub_body = format!("{{{}}}", &trimmed[s + 1..e - 1]);
        out.push_str(&json_path_single(&sub_body, text).join("\n"));
        cursor = e;
    }
    out.push_str(&trimmed[cursor..]);
    vec![out]
}

/// 原 json_path_single 主体（单一路径读取；经同线程 4 槽位 JSON 缓存零重复反序列化）
fn json_path_single_inner(body: &str, text: &str) -> Vec<String> {
    // 提取 body 内路径：{$.list.xxx} 或 {.list.xxx}
    let inner = body.trim().trim_start_matches('{').trim_end_matches('}');
    let json = match parse_json_value_cached(text) {
        Ok(v) => v,
        Err(_) => {
            // HTML 中可能内嵌 JSON（如 <script>），尝试按行提取
            return json_from_html(inner, text);
        }
    };
    let path = if let Some(p) = inner.strip_prefix('$') {
        p // "$.a" → ".a"；"$..a" → "..a"（保留递归标记）；"$[0]" → "[0]"
    } else if let Some(p) = inner.strip_prefix('.') {
        p
    } else {
        inner
    };
    let mut results = vec![];
    walk_json(&json, path, &mut results);
    results
}

/// 在 HTML 中查找形如 `{"...` 的 JSON 片段尝试解析
fn json_from_html(path: &str, html: &str) -> Vec<String> {
    let mut results = vec![];
    // 简单策略：按行找包含 { 的片段
    for line in html.lines() {
        let line = line.trim();
        if line.starts_with('{') && line.ends_with('}') {
            if let Ok(v) = parse_json_value_cached(line) {
                let mut r = vec![];
                walk_json(&v, path, &mut r);
                results.extend(r);
            }
        }
    }
    results
}

// ---------- JSONPath 路径引擎 ----------

/// JSONPath 求值递归深度上限（段数 + 值嵌套 + 过滤括号嵌套共用）：
/// 恶意/病态规则（超深路径、超深嵌套括号、超深数组）超限时按“规则错误”处理
/// （记日志、返回空结果），绝不递归到栈溢出 abort。
const JSONPATH_MAX_DEPTH: usize = 64;

/// 深度超限错误（内部传播；顶层 walk_json 转日志 + 空结果）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JsonPathDepthExceeded;

/// serde_json 解析：经 serde_stacker 把 serde 递归移到堆上（解析机制本身不栈溢出）；
/// serde_json 内置 128 层递归上限保留——超限返回解析错误（上层回退 json_from_html/空结果），
/// 不会 abort；求值侧另有 JSONPATH_MAX_DEPTH=64 兜底（Value 深度超限的 drop 递归风险也因此排除）。
fn parse_json_value(text: &str) -> serde_json::Result<serde_json::Value> {
    let mut inner = serde_json::Deserializer::from_str(text);
    let de = serde_stacker::Deserializer::new(&mut inner);
    <serde_json::Value as serde::Deserialize>::deserialize(de)
}

/// 内容是否为 JSON（legacy AnalyzeRule.setContent 的 isJSON 判定，
/// StringExtensions.kt:16 首字符 `{`/`[` + 尾字符配对；此处再经整体解析确认，
/// 比纯首尾括号检查更严——非 JSON 花括号文本不误触发裸键强制 JsonPath）。
fn is_json_text(text: &str) -> bool {
    let t = text.trim();
    let bracketed =
        (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'));
    bracketed && parse_json_value_cached(t).is_ok()
}

#[derive(Debug, Clone, PartialEq)]
enum JSeg {
    Key(String),
    RecKey(String),
    Wildcard,
    Index(i64),
    Slice(Option<i64>, Option<i64>, i64),
    Multi(Vec<JItem>),
    Filter(String),
    QuotedKey(String),
}

#[derive(Debug, Clone, PartialEq)]
enum JItem {
    I(i64),
    S(Option<i64>, Option<i64>, i64),
}

/// 简化 JSONPath 遍历（支持 .a.b / [0] / [-1] / [*] / $..递归 / [?()] 过滤 / 切片）
fn walk_json(value: &serde_json::Value, path: &str, out: &mut Vec<String>) {
    let segs = tokenize_json_path(path);
    let mut found: Vec<&serde_json::Value> = Vec::new();
    if let Err(JsonPathDepthExceeded) = eval_segments(value, &segs, 0, &mut found) {
        tracing::warn!(
            "JSONPath 求值深度超限（>{JSONPATH_MAX_DEPTH}），按空结果处理（路径: {path}）"
        );
        return;
    }
    for v in found {
        push_json_value(v, out);
    }
}

fn push_json_value(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                push_json_value(item, out);
            }
        }
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Null => {}
        other => out.push(other.to_string()), // 数字/布尔/对象（JSON 序列化）
    }
}

fn tokenize_json_path(path: &str) -> Vec<JSeg> {
    let b = path.as_bytes();
    let mut segs = Vec::new();
    let mut i = 0;
    if b.first() == Some(&b'$') {
        i = 1;
    }
    while i < b.len() {
        match b[i] {
            b'.' => {
                if i + 1 < b.len() && b[i + 1] == b'.' {
                    // 递归下降 ..name
                    i += 2;
                    let (name, ni) = read_name(path, i);
                    segs.push(JSeg::RecKey(name));
                    i = ni;
                } else if i + 1 < b.len() && b[i + 1] == b'*' {
                    segs.push(JSeg::Wildcard);
                    i += 2;
                } else {
                    i += 1;
                    let (name, ni) = read_name(path, i);
                    if !name.is_empty() {
                        segs.push(JSeg::Key(name));
                    }
                    i = ni;
                }
            }
            b'[' => {
                let (content, ni) = read_bracket(path, i);
                segs.push(parse_bracket(&content));
                i = ni;
            }
            b'*' => {
                segs.push(JSeg::Wildcard);
                i += 1;
            }
            _ => {
                let (name, ni) = read_name(path, i);
                segs.push(JSeg::Key(name));
                i = ni;
            }
        }
    }
    segs
}

fn read_name(path: &str, mut i: usize) -> (String, usize) {
    let b = path.as_bytes();
    let start = i;
    while i < b.len() && b[i] != b'.' && b[i] != b'[' {
        i += 1;
    }
    (path[start..i].to_string(), i)
}

/// 读取 `[...]` 内容（引号与括号平衡）
fn read_bracket(path: &str, start: usize) -> (String, usize) {
    let b = path.as_bytes();
    let mut i = start + 1;
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                b'[' | b'(' => depth += 1,
                b']' => {
                    if depth == 0 {
                        return (path[start + 1..i].to_string(), i + 1);
                    }
                    depth -= 1;
                }
                b')' => depth -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    (path[start + 1..].to_string(), b.len())
}

fn parse_bracket(content: &str) -> JSeg {
    let c = content.trim();
    if let Some(rest) = c.strip_prefix("?(") {
        if let Some(expr) = rest.strip_suffix(')') {
            return JSeg::Filter(expr.to_string());
        }
    }
    if c.starts_with('\'') || c.starts_with('"') {
        let q = c.chars().next().unwrap();
        if c.len() >= 2 && c.ends_with(q) {
            return JSeg::QuotedKey(c[1..c.len() - 1].to_string());
        }
    }
    if c == "*" {
        return JSeg::Wildcard;
    }
    let items: Vec<&str> = split_top_level(c, ",");
    if items.is_empty() {
        return JSeg::Key(String::new());
    }
    let parsed: Vec<JItem> = items
        .iter()
        .map(|it| {
            let it = it.trim();
            if it.contains(':') {
                let parts: Vec<&str> = it.split(':').collect();
                JItem::S(
                    parts
                        .first()
                        .copied()
                        .filter(|s| !s.is_empty())
                        .and_then(|x| x.trim().parse::<i64>().ok()),
                    parts
                        .get(1)
                        .copied()
                        .filter(|s| !s.is_empty())
                        .and_then(|x| x.trim().parse::<i64>().ok()),
                    parts
                        .get(2)
                        .copied()
                        .filter(|s| !s.is_empty())
                        .and_then(|x| x.trim().parse::<i64>().ok())
                        .unwrap_or(1),
                )
            } else {
                JItem::I(it.parse::<i64>().unwrap_or(0))
            }
        })
        .collect();
    if parsed.len() == 1 {
        match parsed.into_iter().next().unwrap() {
            JItem::I(n) => JSeg::Index(n),
            JItem::S(s, e, st) => JSeg::Slice(s, e, st),
        }
    } else {
        JSeg::Multi(parsed)
    }
}

fn eval_segments<'v>(
    value: &'v serde_json::Value,
    segs: &[JSeg],
    depth: usize,
    out: &mut Vec<&'v serde_json::Value>,
) -> Result<(), JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    if segs.is_empty() {
        out.push(value);
        return Ok(());
    }
    match &segs[0] {
        JSeg::Key(name) | JSeg::QuotedKey(name) => match value {
            serde_json::Value::Object(map) => {
                if let Some(v) = map.get(name) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            serde_json::Value::Array(arr) => {
                // 数组自动展开（对齐 v1 行为）
                for item in arr {
                    eval_segments(item, segs, depth + 1, out)?;
                }
            }
            _ => {}
        },
        JSeg::RecKey(name) => {
            let mut found: Vec<&'v serde_json::Value> = Vec::new();
            collect_rec(value, name, depth + 1, &mut found)?;
            for v in found {
                eval_segments(v, &segs[1..], depth + 1, out)?;
            }
        }
        JSeg::Wildcard => match value {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    eval_segments(item, &segs[1..], depth + 1, out)?;
                }
            }
            serde_json::Value::Object(map) => {
                for v in map.values() {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            _ => {}
        },
        JSeg::Index(n) => match value {
            serde_json::Value::Array(arr) => {
                if let Some(v) = norm_index(*n, arr.len()).and_then(|idx| arr.get(idx)) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(v) = map.get(&n.to_string()) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            _ => {}
        },
        JSeg::Slice(s, e, st) => {
            if let serde_json::Value::Array(arr) = value {
                for v in slice_items(arr, *s, *e, *st) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
        }
        JSeg::Multi(items) => {
            if let serde_json::Value::Array(arr) = value {
                for it in items {
                    match it {
                        JItem::I(n) => {
                            if let Some(v) = norm_index(*n, arr.len()).and_then(|idx| arr.get(idx))
                            {
                                eval_segments(v, &segs[1..], depth + 1, out)?;
                            }
                        }
                        JItem::S(s, e, st) => {
                            for v in slice_items(arr, *s, *e, *st) {
                                eval_segments(v, &segs[1..], depth + 1, out)?;
                            }
                        }
                    }
                }
            }
        }
        JSeg::Filter(expr) => match value {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    if eval_filter(expr, item, depth + 1)? {
                        eval_segments(item, &segs[1..], depth + 1, out)?;
                    }
                }
            }
            other => {
                if eval_filter(expr, other, depth + 1)? {
                    eval_segments(other, &segs[1..], depth + 1, out)?;
                }
            }
        },
    }
    Ok(())
}

fn norm_index(n: i64, len: usize) -> Option<usize> {
    let len_i = len as i64;
    let idx = if n < 0 { n + len_i } else { n };
    if idx >= 0 && idx < len_i {
        Some(idx as usize)
    } else {
        None
    }
}

/// $..name：任意深度收集键 name 的值（深度受限，防栈溢出 abort）
fn collect_rec<'v>(
    value: &'v serde_json::Value,
    name: &str,
    depth: usize,
    out: &mut Vec<&'v serde_json::Value>,
) -> Result<(), JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    match value {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get(name) {
                out.push(v);
            }
            for v in map.values() {
                collect_rec(v, name, depth + 1, out)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_rec(v, name, depth + 1, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Python 风格切片（end 排除；负数回绕；step 可为负反向）
fn slice_items(
    arr: &[serde_json::Value],
    s: Option<i64>,
    e: Option<i64>,
    st: i64,
) -> Vec<&serde_json::Value> {
    let len = arr.len() as i64;
    let step = if st == 0 { 1 } else { st };
    let mut start = s.unwrap_or(if step > 0 { 0 } else { len - 1 });
    if start < 0 {
        start += len;
    }
    let mut end = e.unwrap_or(if step > 0 { len } else { -(len + 1) });
    if end < 0 {
        end += len;
    }
    let mut out = Vec::new();
    if step > 0 {
        let mut i = start.max(0);
        while i < end.min(len) {
            out.push(&arr[i as usize]);
            i += step;
        }
    } else {
        let mut i = start.min(len - 1);
        while i > end && i >= 0 {
            out.push(&arr[i as usize]);
            i += step;
        }
    }
    out
}

// ---------- [?()] 过滤表达式 ----------

fn eval_filter(
    expr: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    for part in split_top_level(expr, "||") {
        if eval_and(part, item, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn eval_and(
    expr: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    for part in split_top_level(expr, "&&") {
        if !eval_primary(part, item, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn eval_primary(
    expr: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    let s = expr.trim();
    if s.is_empty() {
        return Ok(false);
    }
    if let Some(rest) = s.strip_prefix('(') {
        if let Some(inner) = rest.strip_suffix(')') {
            return eval_filter(inner, item, depth + 1);
        }
    }
    if let Some(rest) = s.strip_prefix('!') {
        return Ok(!eval_primary(rest, item, depth + 1)?);
    }
    for op in ["==", "!=", "<=", ">=", "<", ">"] {
        if let Some(pos) = find_op_top(s, op) {
            let lhs = s[..pos].trim();
            let rhs = s[pos + op.len()..].trim();
            let lv = eval_filter_path(lhs, item, depth + 1)?;
            let rv = parse_filter_literal(rhs);
            return Ok(compare_filter(lv, rv.as_ref(), op));
        }
    }
    // 高频操作符（=~ / in / nin / size）：词边界匹配，避免误伤路径键名
    if let Some((op, pos)) = find_word_op_top(s) {
        let lhs = s[..pos].trim();
        let rhs = s[pos + op.len()..].trim();
        return eval_word_op(op, lhs, rhs, item, depth + 1);
    }
    // 无比较：裸存在性（legacy：键存在即匹配——值为 null/false/"" 也返回 true）
    Ok(eval_filter_path(s, item, depth + 1)?.is_some())
}

/// 过滤内路径求值：@ / @.a.b / @['a']（取首个结果）
fn eval_filter_path<'v>(
    path: &str,
    item: &'v serde_json::Value,
    depth: usize,
) -> Result<Option<&'v serde_json::Value>, JsonPathDepthExceeded> {
    let p = path.trim();
    if p == "@" {
        return Ok(Some(item));
    }
    let p = p.strip_prefix('@').unwrap_or(p);
    let segs = tokenize_json_path(p);
    let mut found: Vec<&'v serde_json::Value> = Vec::new();
    eval_segments(item, &segs, depth, &mut found)?;
    Ok(found.into_iter().next())
}

fn parse_filter_literal(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        || (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
    {
        return Some(serde_json::Value::String(s[1..s.len() - 1].to_string()));
    }
    match s {
        "true" => return Some(serde_json::Value::Bool(true)),
        "false" => return Some(serde_json::Value::Bool(false)),
        "null" => return Some(serde_json::Value::Null),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(serde_json::json!(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(serde_json::json!(f));
    }
    Some(serde_json::Value::String(s.to_string()))
}

fn compare_filter(
    lv: Option<&serde_json::Value>,
    rv: Option<&serde_json::Value>,
    op: &str,
) -> bool {
    match op {
        // legacy NotEqualsEvaluator 即 !EQ：路径缺失（取不到值）→ true
        "!=" => match (lv, rv) {
            (Some(l), Some(r)) => !json_eq(l, r),
            _ => true,
        },
        _ => {
            let (Some(l), Some(r)) = (lv, rv) else {
                return false; // 其余操作符缺失值比较恒 false
            };
            match op {
                "==" => json_eq(l, r),
                "<" | "<=" | ">" | ">=" => match (l.as_f64(), r.as_f64()) {
                    (Some(a), Some(b)) => cmp_num(a, b, op),
                    _ => match (l.as_str(), r.as_str()) {
                        (Some(a), Some(b)) => cmp_str(a, b, op),
                        _ => false,
                    },
                },
                _ => false,
            }
        }
    }
}

fn json_eq(l: &serde_json::Value, r: &serde_json::Value) -> bool {
    match (l, r) {
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            a.as_f64().unwrap_or(0.0) == b.as_f64().unwrap_or(0.0)
        }
        // legacy NumberNode.equals 接受 StringNode → 数值与数字字符串松散相等
        (serde_json::Value::Number(a), serde_json::Value::String(b))
        | (serde_json::Value::String(b), serde_json::Value::Number(a)) => {
            match (a.as_f64(), b.parse::<f64>()) {
                (Some(x), Ok(y)) => x == y,
                _ => false,
            }
        }
        (serde_json::Value::String(a), serde_json::Value::String(b)) => a == b,
        (serde_json::Value::Bool(a), serde_json::Value::Bool(b)) => a == b,
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        _ => false,
    }
}

fn cmp_num(a: f64, b: f64, op: &str) -> bool {
    match op {
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => false,
    }
}

fn cmp_str(a: &str, b: &str, op: &str) -> bool {
    match op {
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => false,
    }
}

/// 顶层切分（引号/括号内不切）
fn split_top_level<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                _ if depth == 0 && s.is_char_boundary(i) && s[i..].starts_with(sep) => {
                    parts.push(&s[start..i]);
                    i += sep.len();
                    start = i;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

fn find_op_top(s: &str, op: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut in_s = false;
    let mut in_d = false;
    while i + op.len() <= b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d && s.is_char_boundary(i) && s[i..].starts_with(op) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 词操作符查找（=~ / in / nin / size）：顶层、引号外，且两侧须为空白/端点——
/// 防止误伤含这些词的路径键名（如 @.min、@.size）
fn find_word_op_top(s: &str) -> Option<(&'static str, usize)> {
    let b = s.as_bytes();
    let mut best: Option<(&'static str, usize)> = None;
    let mut in_s = false;
    let mut in_d = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d && s.is_char_boundary(i) {
            for op in ["=~", "nin", "size", "in"] {
                if s[i..].starts_with(op)
                    && (i == 0 || (b[i - 1] as char).is_whitespace())
                    && (i + op.len() == b.len() || (b[i + op.len()] as char).is_whitespace())
                {
                    // 首字符互异无前缀冲突；取最早出现者
                    if best.is_none_or(|(_, p)| i < p) {
                        best = Some((op, i));
                    }
                    break;
                }
            }
        }
        i += 1;
    }
    best
}

/// 高频操作符求值：
/// - `=~`：正则匹配（右值 /pattern/ 形式）
/// - `in`/`nin`：左值（不）在右值数组中
/// - `size`：数组/字符串长度等于右值
fn eval_word_op(
    op: &str,
    lhs: &str,
    rhs: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    let lv = eval_filter_path(lhs, item, depth)?;
    match op {
        "=~" => {
            let Some(v) = lv else { return Ok(false) };
            let text = match v {
                serde_json::Value::String(t) => t.clone(),
                other => other.to_string(),
            };
            let pat = rhs
                .strip_prefix('/')
                .and_then(|p| p.strip_suffix('/'))
                .unwrap_or(rhs);
            match crate::util::regex::Regex::new(pat) {
                Ok(re) => Ok(re.is_match(&text)),
                Err(e) => {
                    tracing::warn!("过滤表达式 =~ 正则编译失败（{pat}）：{e}");
                    Ok(false)
                }
            }
        }
        "in" => Ok(lv.is_some_and(|v| parse_literal_array(rhs).iter().any(|e| json_eq(v, e)))),
        "nin" => Ok(!lv.is_some_and(|v| parse_literal_array(rhs).iter().any(|e| json_eq(v, e)))),
        "size" => {
            let Some(v) = lv else { return Ok(false) };
            let n = match v {
                serde_json::Value::Array(a) => a.len(),
                serde_json::Value::String(t) => t.chars().count(),
                _ => return Ok(false),
            };
            let rv = parse_filter_literal(rhs);
            Ok(rv
                .as_ref()
                .map(|r| json_eq(&serde_json::json!(n), r))
                .unwrap_or(false))
        }
        _ => Ok(false),
    }
}

/// 解析右值数组字面量（[1,2,3] / ['a','b'] / ('a','b')），非数组 → 空
fn parse_literal_array(s: &str) -> Vec<serde_json::Value> {
    let t = s.trim();
    let t = match (t.starts_with('('), t.ends_with(')')) {
        (true, true) => t[1..t.len() - 1].trim(),
        _ => t,
    };
    if !(t.starts_with('[') && t.ends_with(']')) {
        return Vec::new();
    }
    split_top_level(&t[1..t.len() - 1], ",")
        .iter()
        .filter_map(|p| parse_filter_literal(p))
        .collect()
}
