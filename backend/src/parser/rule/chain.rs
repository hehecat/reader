//! 规则字符串解析、JS 链与组合分隔切分、前缀与正则替换后处理

use super::{Rule, RuleKind};

/// 解析规则字符串（对齐 legado SourceRule + makeUpRule）
/// - `@@` 前缀去掉（默认规则）
/// - `@CSS:`/`@XPath:`/`@Json:`/`@js:`/`js:` 前缀（大小写不敏感，对齐 legado startsWith(ignoreCase)）
/// - `:` 前缀 → 正则规则（legado allInOne：书籍列表/目录列表专用）
/// - 孤立 `@` 前缀剥除（legado RuleAnalyzer.trim——链式规则中 @ 为冗余符号）
/// - `##` 多段：第二段为替换正则（@ 开头 → legacy 前缀）；第三段为替换串；
///   存在第四段（`###`）→ 仅替换首个匹配
pub fn parse_rule(rule: &str) -> Rule {
    // ## 切分需避开 {{...}} 内嵌规则（legado：evalMatcher 先于 makeUpRule 的 ## 切分）
    let parts = split_hashes(rule);
    let raw_main = parts[0].trim();
    // (规则体, 类型, 是否裸键 Default 模式——仅无显式前缀的兜底分支为 true)
    let (main, kind, default_mode) = if let Some(rest) = raw_main.strip_prefix("@@") {
        (rest.trim().to_string(), RuleKind::Css, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@CSS:") {
        (rest.trim().to_string(), RuleKind::Css, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@XPath:") {
        (rest.trim().to_string(), RuleKind::XPath, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@Json:") {
        (rest.trim().to_string(), RuleKind::JsonPath, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@js:") {
        (rest.trim().to_string(), RuleKind::Js, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "js:") {
        (rest.trim().to_string(), RuleKind::Js, false)
    } else if let Some(rest) = raw_main.strip_prefix(':') {
        // legado allInOne：: 开头整条规则为正则
        (rest.trim().to_string(), RuleKind::Regex, false)
    } else {
        // 孤立 @ 前缀剥除（legado RuleAnalyzer.trim：@ 或空白符）
        let cleaned = raw_main.trim_start_matches('@').trim();
        let kind = detect_kind(cleaned);
        // 仅裸键兜底分支视为 legacy Mode.Default（@CSS:/@@ 等显式前缀已在上方拦截）
        let default_mode = kind == RuleKind::Css;
        (cleaned.to_string(), kind, default_mode)
    };

    let mut prefix = None;
    let mut replace_regex = None;
    let mut replacement = None;
    let mut replace_first = false;
    if parts.len() > 1 {
        let tail = parts[1].trim();
        if tail.starts_with('@') {
            // legacy 前缀格式：规则##@前缀（拼接在结果前）
            prefix = Some(parts[1..].join("##"));
        } else {
            replace_regex = Some(tail.to_string());
            if parts.len() > 2 {
                replacement = Some(parts[2].to_string());
            }
            if parts.len() > 3 {
                replace_first = true;
            }
        }
    }

    Rule {
        kind,
        body: main,
        prefix,
        replace_regex,
        replacement,
        replace_first,
        default_mode,
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// 在 `start..` 内寻找花括号（`{{...}}`）之外的 `<js>` / `@js:` 标记
/// （大小写不敏感，同 find_ci 语义；P3-A：{{}} 内嵌模板中的标记是规则引用而非 JS 段）
fn find_js_markers(rule: &str, start: usize) -> (Option<usize>, Option<usize>) {
    let b = rule.as_bytes();
    let mut i = start;
    let mut depth = 0i32;
    let mut js_tag = None;
    let mut js_at = None;
    while i < rule.len() {
        // 只处理字符边界（{{/}}/@js:/<js> 均为 ASCII；多字节字符逐字跳过）
        if !rule.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if b[i] == b'{' && i + 1 < rule.len() && b[i + 1] == b'{' {
            depth += 1;
            i += 2;
            continue;
        }
        if b[i] == b'}' && i + 1 < rule.len() && b[i + 1] == b'}' && depth > 0 {
            depth -= 1;
            i += 2;
            continue;
        }
        if depth == 0 {
            if js_at.is_none()
                && rule[i..]
                    .get(..4)
                    .is_some_and(|s| s.eq_ignore_ascii_case("@js:"))
            {
                js_at = Some(i);
            }
            if js_tag.is_none()
                && rule[i..]
                    .get(..4)
                    .is_some_and(|s| s.eq_ignore_ascii_case("<js>"))
            {
                js_tag = Some(i);
            }
            if js_at.is_some() && js_tag.is_some() {
                break;
            }
        }
        i += rule[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    (js_tag, js_at)
}

/// 在 `{{` 之后的切片 `after` 中定位配对的外层 `}}` 字节下标：
/// 跳过引号字面量并平衡单层 `{ ... }`（使 `{{try{chapter.title}catch(e){""}}}` 正确闭合到最外层 `}}`）。
pub(crate) fn find_closing_double_brace(after: &str) -> Option<usize> {
    let b = after.as_bytes();
    let mut i = 0usize;
    let mut brace_depth = 0i32;
    let mut quote: u8 = 0;
    let mut esc = false;
    while i < b.len() {
        let c = b[i];
        if esc {
            esc = false;
            i += 1;
            continue;
        }
        if quote != 0 {
            if c == b'\\' {
                esc = true;
            } else if c == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' || c == b'`' {
            quote = c;
            i += 1;
            continue;
        }
        if c == b'{' {
            brace_depth += 1;
            i += 1;
            continue;
        }
        if c == b'}' {
            if brace_depth > 0 {
                brace_depth -= 1;
                i += 1;
                continue;
            }
            if i + 1 < b.len() && b[i + 1] == b'}' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// 按 `##` 切分（避开 `{{...}}` 内嵌规则——其内容可含 `##` 替换链）
fn split_hashes(rule: &str) -> Vec<&str> {
    let b = rule.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            depth += 1;
            i += 2;
            continue;
        }
        if b[i] == b'}' && b[i + 1] == b'}' && depth > 0 {
            depth -= 1;
            i += 2;
            continue;
        }
        if depth == 0 && b[i] == b'#' && b[i + 1] == b'#' {
            parts.push(&rule[start..i]);
            i += 2;
            start = i;
            continue;
        }
        i += 1;
    }
    parts.push(&rule[start..]);
    parts
}

#[allow(clippy::if_same_then_else)] // 不同前缀走向同一 RuleKind 是有意的 legado 语义映射，分开写便于对照
fn detect_kind(body: &str) -> RuleKind {
    let b = body.trim();
    // 对齐 legado SourceRule 类型检测（AnalyzeRule.kt）
    if b.starts_with("@CSS:") {
        RuleKind::Css // @CSS: 显式 CSS
    } else if b.starts_with("@@") {
        RuleKind::Css // @@ 默认规则（去前缀由 parse 处理）
    } else if b.starts_with("@XPath:") {
        RuleKind::XPath
    } else if b.starts_with("@Json:") {
        RuleKind::JsonPath
    } else if b.starts_with("$.") || b.starts_with("$[") || b.starts_with('{') {
        RuleKind::JsonPath // $. / $[ 或 JSON 片段
    } else if b.starts_with(':') {
        RuleKind::Regex // legado allInOne：: 前缀正则规则
    } else if b.starts_with('/') {
        RuleKind::XPath // XPath 特征明显，无需标识头
    } else if b.starts_with("@js:") || b.starts_with("js:") {
        RuleKind::Js
    } else if b.contains("$1") || b.contains("$2") {
        RuleKind::Regex // $N 引用 → 正则
    } else {
        RuleKind::Css
    }
}

// ---------- 组合分隔与 JS 链切分 ----------

/// JS 链段
pub struct JsSeg<'a> {
    pub is_js: bool,
    pub text: &'a str,
}

/// 是否含 JS 标记（<js> 或 @js:，大小写不敏感——对齐 legado JS_PATTERN）
pub fn contains_js_marker(rule: &str) -> bool {
    find_ci(rule, "<js>").is_some() || find_ci(rule, "@js:").is_some()
}

pub(super) fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let nb = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(nb.len())
        .position(|w| w.eq_ignore_ascii_case(nb))
}

/// 按 JS 标记切段（legado JS_PATTERN：`<js>...</js>` 或 `@js:` 贪婪到末尾，均大小写不敏感）
pub fn split_js_chain(rule: &str) -> Vec<JsSeg<'_>> {
    let mut segs: Vec<JsSeg<'_>> = Vec::new();
    let mut start = 0;
    while start < rule.len() {
        // P3-A 修复：{{...}} 内嵌模板内的 @js:/<js> 不视为 JS 段——{{@js:...}} 是
        // 规则引用语法（legado isRule），须留给 expand_inline 处理；原先会被切成
        // 文本段 "{{" + JS 段 "'@js:1+1'}}"（含未闭合 }}）导致求值失败
        let (js_tag, js_at) = find_js_markers(rule, start);
        // <js> 需有闭合 </js> 才成为 JS 段（legado 非贪婪匹配失败则跳过）
        let tag_ok = js_tag.and_then(|p| find_ci(&rule[p + 4..], "</js>").map(|q| (p, p + 4 + q)));
        enum Kind {
            Tag,
            At,
        }
        let cand = match (tag_ok, js_at) {
            (Some((p, _)), Some(q)) if q < p => Some((Kind::At, q, rule.len())),
            (Some((p, end)), _) => Some((Kind::Tag, p, end)),
            (None, Some(q)) => Some((Kind::At, q, rule.len())),
            (None, None) => None,
        };
        match cand {
            None => {
                let t = rule[start..].trim();
                if !t.is_empty() {
                    segs.push(JsSeg {
                        is_js: false,
                        text: t,
                    });
                }
                break;
            }
            Some((kind, seg_start, seg_end)) => {
                let t = rule[start..seg_start].trim();
                if !t.is_empty() {
                    segs.push(JsSeg {
                        is_js: false,
                        text: t,
                    });
                }
                let code = match kind {
                    Kind::Tag => &rule[seg_start + 4..seg_end],
                    Kind::At => &rule[seg_start + 4..],
                };
                let code = code.trim();
                if !code.is_empty() {
                    segs.push(JsSeg {
                        is_js: true,
                        text: code,
                    });
                }
                start = match kind {
                    Kind::Tag => seg_end + 5, // 跳过 </js>（5 字符）
                    Kind::At => rule.len(),
                };
            }
        }
    }
    segs
}

/// 组合分隔切分（对齐 legado RuleAnalyzer.splitRule）：
/// 取最早出现在平衡组（[] / () / {} / 引号）外的 `&&` / `||` / `%%`，
/// 确定分隔符后剩余部分按该分隔符朴素切分（legado 二段切分语义）。
/// 返回 (分隔符, 子规则列表)；无分隔符 → (None, [整条])
pub fn split_combined(rule: &str) -> (Option<&'static str>, Vec<&str>) {
    let b = rule.as_bytes();
    let mut depth_sq = 0i32;
    let mut depth_par = 0i32;
    let mut depth_cur = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    let mut esc = false;
    let mut sep_pos: Option<usize> = None;
    let mut sep_kind: &'static str = "&&";
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if esc {
            esc = false;
            i += 1;
            continue;
        }
        if c == b'\\' {
            esc = true;
            i += 1;
            continue;
        }
        if c == b'\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == b'"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if in_s || in_d {
            i += 1;
            continue;
        }
        match c {
            b'[' => depth_sq += 1,
            b']' => depth_sq -= 1,
            b'(' => depth_par += 1,
            b')' => depth_par -= 1,
            b'{' => depth_cur += 1,
            b'}' => depth_cur -= 1,
            _ if depth_sq == 0 && depth_par == 0 && depth_cur == 0 => {
                let rest = &b[i..];
                if rest.starts_with(b"&&") {
                    sep_pos = Some(i);
                    sep_kind = "&&";
                    break;
                }
                if rest.starts_with(b"||") {
                    sep_pos = Some(i);
                    sep_kind = "||";
                    break;
                }
                if rest.starts_with(b"%%") {
                    sep_pos = Some(i);
                    sep_kind = "%%";
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    match sep_pos {
        None => (None, vec![rule]),
        Some(pos) => {
            let mut subs = vec![&rule[..pos]];
            let rest = &rule[pos + sep_kind.len()..];
            let mut start = 0;
            while let Some(p) = rest[start..].find(sep_kind) {
                subs.push(&rest[start..start + p]);
                start += p + sep_kind.len();
            }
            subs.push(&rest[start..]);
            (Some(sep_kind), subs)
        }
    }
}

/// 应用前缀与替换（legado 语义：前缀拼接 + 正则替换/替换首个）
pub(super) fn apply_post(results: Vec<String>, rule: &Rule) -> Vec<String> {
    if results.is_empty() {
        return results;
    }
    // 列表热路径优化：规则替换正则仅查询/编译一次（走 util::regex 500 条 LRU 缓存），
    // 避免对列表中每个条目重复加锁查缓存。
    let replace_plan = rule
        .replace_regex
        .as_deref()
        .filter(|re| !re.is_empty())
        .map(|re_str| {
            let compiled = crate::util::regex::Regex::new(re_str).ok();
            let rep = rule.replacement.as_deref().unwrap_or("");
            (re_str, compiled, rep)
        });
    results
        .into_iter()
        .map(|mut s| {
            if let Some(prefix) = &rule.prefix {
                if !s.starts_with(prefix.as_str()) {
                    s = format!("{prefix}{s}");
                }
            }
            if let Some((re_str, compiled, rep)) = &replace_plan {
                s = apply_replace_regex(&s, re_str, compiled.as_ref(), rep, rule.replace_first);
            }
            s
        })
        .collect()
}

/// 替换执行（对齐 legado AnalyzeRule.applyReplaceRegex）：
/// - replaceFirst（###）：取首个匹配串 `it.group(0)` 再执行 `replaceFirst(regex, replacement)`
///   （丢弃匹配前后的无关 HTML/文本）；无匹配 → 空串；正则编译失败 → 替换串本身
/// - 普通：全文全部替换；正则编译失败 → 字面串替换
fn apply_replace_regex(
    result: &str,
    re_str: &str,
    compiled: Option<&crate::util::regex::Regex>,
    replacement: &str,
    first: bool,
) -> String {
    if first {
        match compiled {
            Some(re) => match re.find_str(result) {
                Some(matched) => re.replace_first(matched, replacement).into_owned(),
                None => String::new(),
            },
            None => replacement.to_string(),
        }
    } else {
        match compiled {
            Some(re) => re.replace_all(result, replacement).into_owned(),
            None => result.replace(re_str, replacement),
        }
    }
}
