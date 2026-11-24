//! 正文与标题净化替换引擎（兼容 legado ReplaceRule）
//!
//! 复用了 `crate::util::regex::Regex`（std regex 快路径 + fancy-regex 前后瞻/反向引用回退 + LRU 缓存 + 回溯上限），
//! 并自动把 Java/JS 风格 `\uXXXX` 转义规范化为 Rust 正则支持的 `\u{XXXX}`。

use std::borrow::Cow;

use crate::model::ReplaceRule;

/// 净化目标：正文或章节标题
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurifyTarget {
    Content,
    Title,
}

/// 将 Java / JS 正则里的 `\uXXXX`（4 位十六进制）归一为 Rust `regex`/`fancy-regex` 支持的 `\u{XXXX}`；
/// 已经带花括号的 `\u{...}` 或被转义的 `\\uXXXX` 保持不动。
fn normalize_java_unicode_escapes(pattern: &str) -> Cow<'_, str> {
    if !pattern.contains("\\u") {
        return Cow::Borrowed(pattern);
    }
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut i = 0usize;
    let mut changed = false;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                out.push_str("\\\\");
                i += 2;
                continue;
            }
            if i + 5 < bytes.len()
                && bytes[i + 1] == b'u'
                && bytes[i + 2..i + 6].iter().all(|b| b.is_ascii_hexdigit())
            {
                out.push_str("\\u{");
                out.push_str(&pattern[i + 2..i + 6]);
                out.push('}');
                i += 6;
                changed = true;
                continue;
            }
        }
        let ch = pattern[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(pattern)
    }
}

/// 判断规则作用范围 `scope` 是否命中当前书名 / 书源 URL（对齐 legado：空=全部；逗号/分号/换行分隔；支持 `!排除`）
fn scope_matches(scope: Option<&str>, book_name: Option<&str>, source_url: Option<&str>) -> bool {
    let Some(raw) = scope.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let name = book_name.unwrap_or("").trim();
    let src = source_url.unwrap_or("").trim();
    if name.is_empty() && src.is_empty() {
        return true;
    }

    let mut has_include = false;
    let mut included = false;
    for token in raw
        .split([',', '，', ';', '；', '\n'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if let Some(neg) = token.strip_prefix('!').map(str::trim) {
            if !neg.is_empty() && (name.contains(neg) || src.contains(neg)) {
                return false;
            }
        } else {
            has_include = true;
            if name.contains(token) || src.contains(token) {
                included = true;
            }
        }
    }
    if has_include {
        included
    } else {
        true
    }
}

/// 按规则顺序依次净化文本（单条规则语法异常或超限时安全跳过，不中断阅读）
pub fn apply_replace_rules(
    text: &str,
    rules: &[ReplaceRule],
    target: PurifyTarget,
    book_name: Option<&str>,
    source_url: Option<&str>,
) -> String {
    if text.is_empty() || rules.is_empty() {
        return text.to_string();
    }
    let mut current = text.to_string();
    for rule in rules {
        if !rule.enabled || rule.find.is_empty() {
            continue;
        }
        let target_ok = match target {
            PurifyTarget::Content => rule.scope_content,
            PurifyTarget::Title => rule.scope_title,
        };
        if !target_ok {
            continue;
        }
        if !scope_matches(rule.scope.as_deref(), book_name, source_url) {
            continue;
        }

        if !rule.is_regex {
            if current.contains(&rule.find) {
                current = current.replace(&rule.find, &rule.replace);
            }
            continue;
        }

        let norm = normalize_java_unicode_escapes(&rule.find);
        match crate::util::regex::Regex::new(&norm) {
            Ok(re) => {
                if let Cow::Owned(replaced) = re.replace_all(&current, &rule.replace) {
                    current = replaced;
                }
            }
            Err(err) => {
                tracing::debug!(
                    rule_name = %rule.name,
                    error = %err,
                    "跳过无效净化正则"
                );
            }
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regex_rule(name: &str, find: &str, replace: &str) -> ReplaceRule {
        ReplaceRule {
            id: name.to_string(),
            name: name.to_string(),
            find: find.to_string(),
            replace: replace.to_string(),
            is_regex: true,
            enabled: true,
            scope_content: true,
            scope_title: false,
            ..ReplaceRule::default()
        }
    }

    #[test]
    fn normalizes_java_unicode_escapes_and_lookarounds() {
        let rules = vec![
            regex_rule(
                "中文英文名点号",
                r"(?<=[\u4e00-\u9fa5])(\.|．|·|-)(?=[\u4e00-\u9fa5])",
                "•",
            ),
            regex_rule("修正对话", r"「(([^「」]+){1,3})」", "“$1”"),
            regex_rule("修正双引号", r"(?m)“([^“”]+)“([^“”]+(“|$))", "“$1”$2"),
            regex_rule("合并重复感叹号", r"(?<=(！|？))\1+", ""),
        ];

        let input = "克莱恩·莫雷蒂说：「你好！！！」\n“测试“结束";
        let out = apply_replace_rules(input, &rules, PurifyTarget::Content, Some("诡秘之主"), None);
        assert_eq!(out, "克莱恩•莫雷蒂说：“你好！”\n“测试”结束");
    }

    #[test]
    fn respects_scope_include_and_exclude() {
        let mut r = regex_rule("仅诡秘", "广告", "");
        r.scope = Some("诡秘之主".into());
        assert_eq!(
            apply_replace_rules(
                "正文广告",
                &[r.clone()],
                PurifyTarget::Content,
                Some("诡秘之主"),
                None
            ),
            "正文"
        );
        assert_eq!(
            apply_replace_rules(
                "正文广告",
                &[r],
                PurifyTarget::Content,
                Some("大主宰"),
                None
            ),
            "正文广告"
        );

        let mut excl = regex_rule("排除诡秘", "广告", "");
        excl.scope = Some("!诡秘之主".into());
        assert_eq!(
            apply_replace_rules(
                "正文广告",
                &[excl.clone()],
                PurifyTarget::Content,
                Some("诡秘之主"),
                None
            ),
            "正文广告"
        );
        assert_eq!(
            apply_replace_rules(
                "正文广告",
                &[excl],
                PurifyTarget::Content,
                Some("大主宰"),
                None
            ),
            "正文"
        );
    }
}
