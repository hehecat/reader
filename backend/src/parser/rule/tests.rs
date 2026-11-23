use super::eval::{apply_depth, expand_inline, json_cache_hits};
use super::*;

#[test]
fn test_parse_prefix() {
    // ## 第二段 @ 开头 → 前缀（兼容 legacy 旧格式）
    let r = parse_rule("div.book##@https://a.com");
    assert_eq!(r.kind, RuleKind::Css);
    assert_eq!(r.prefix.as_deref(), Some("@https://a.com"));
    assert!(r.replace_regex.is_none());
}

#[test]
fn test_parse_legado_flags() {
    assert_eq!(parse_rule("@Json:$.list.name").kind, RuleKind::JsonPath);
    assert_eq!(parse_rule("$.list.name").kind, RuleKind::JsonPath);
    assert_eq!(parse_rule("@XPath://div/a").kind, RuleKind::XPath);
    assert_eq!(parse_rule("//div/a").kind, RuleKind::XPath);
    assert_eq!(parse_rule("@@div.book").kind, RuleKind::Css);
    assert_eq!(parse_rule("a@href").kind, RuleKind::Css);
    // 大小写不敏感标志（legado startsWith(ignoreCase)）
    assert_eq!(parse_rule("@json:$.a").kind, RuleKind::JsonPath);
    assert_eq!(parse_rule("@xpath://a").kind, RuleKind::XPath);
    assert_eq!(parse_rule("@JS:result").kind, RuleKind::Js);
    // 孤立 @ 前缀剥除（链式冗余符号）
    assert_eq!(parse_rule("@class.b@text").kind, RuleKind::Css);
    assert_eq!(parse_rule("@class.b@text").body, "class.b@text");
    // : 前缀 → 正则规则（legado allInOne）
    assert_eq!(parse_rule(":第(.+?)章").kind, RuleKind::Regex);
}

#[test]
fn test_parse_replace_segments() {
    // 三段：替换正则 + 替换串
    let r = parse_rule("a@href##(\\d+)##[$1]");
    assert_eq!(r.replace_regex.as_deref(), Some("(\\d+)"));
    assert_eq!(r.replacement.as_deref(), Some("[$1]"));
    assert!(!r.replace_first);
    // 两段：替换正则，替换串为空（删除匹配）
    let r2 = parse_rule("a@href##\\s+");
    assert_eq!(r2.replace_regex.as_deref(), Some("\\s+"));
    assert!(r2.replacement.is_none());
    // 四段（###）：replaceFirst
    let r3 = parse_rule("##(第.章)##[$1]###");
    assert!(r3.replace_first);
    assert_eq!(r3.body, "");
    assert_eq!(r3.replacement.as_deref(), Some("[$1]"));
}

#[test]
fn test_css_select() {
    let html = r#"<html><body><div class="book"><a href="/1">书名A</a></div><div class="book"><a href="/2">书名B</a></div></body></html>"#;
    let r = apply("div.book a", html);
    assert_eq!(r.len(), 2);
}

#[test]
fn test_regex_fallback() {
    let html = "书名：测试书 作者：张三";
    let r = apply("书名：(.+?)\\s", html);
    assert_eq!(r.first().map(String::as_str), Some("测试书"));
}

/// GAP 153：规则正则支持 lookbehind（fancy-regex 升级路径）
#[test]
fn test_regex_lookbehind() {
    let html = "书名：测试书 作者：张三";
    // 规则主体 lookbehind
    let r = apply("(?<=书名：)\\S+", html);
    assert_eq!(r, vec!["测试书".to_string()]);
    // 替换规则（## 第三段）lookbehind：旧值 (?<=：)(.+?)$ 命中 "测试书"
    let r = apply("(.+?)$##(?<=：)(.+?)$##[$1]", "书名：测试书");
    assert_eq!(r, vec!["书名：[测试书]".to_string()]);
}

#[test]
fn test_regex_case_flag() {
    // 大小写标志：内联 (?i)
    let r = apply("(?i)ABC", "xx abc yy");
    assert_eq!(r, vec!["abc".to_string()]);
    // : 前缀正则规则
    // E16/EG1：全捕获组——每匹配输出 group(0)..group(n) 以 \n 连接
    let r2 = apply(":第(.+?)章", "第一章 第二章");
    assert_eq!(
        r2,
        vec!["第一章\n一".to_string(), "第二章\n二".to_string()],
        "全捕获组：每匹配 group(0)+group(1) 以 \n 连接"
    );
}

#[test]
fn test_regex_chain_replace() {
    // ##pat##rep## 多段替换
    let r = apply("##(第.章)##[$1]", "第一章 第二章");
    assert_eq!(r, vec!["[第一章] [第二章]".to_string()]);
    // 两段：删除匹配
    let r2 = apply("##\\s+", "a b  c");
    assert_eq!(r2, vec!["abc".to_string()]);
    // ### replaceFirst（对齐 legado it.group(0)!!.replaceFirst）：仅提取首个匹配并替换；无匹配 → 空串
    let r3 = apply("##(第.章)##[$1]###", "第一章 第二章");
    assert_eq!(r3, vec!["[第一章]".to_string()]);
    let r4 = apply("##(第.章)##[$1]###", "无匹配文本");
    assert_eq!(r4, vec![String::new()]);
    // 真实书源常见用法：纯正则提取 HTML 字段与封面路径改写
    let block_html = r#"<div class="block"><p>作者：爱潜水的乌贼</p><p>最新章节：<a href="/1.html">第一章</a></p></div>"#;
    assert_eq!(
        apply("##作者.([^<]+)<##$1###", block_html),
        vec!["爱潜水的乌贼".to_string()]
    );
    assert_eq!(
        apply(
            r#"a@href##.+\D((\d+)\d{3})\D##/files/article/image/$2/$1/$1s.jpg###"#,
            r#"<a href="/book/1218284.html">诡秘之主</a>"#
        ),
        vec!["/files/article/image/1218/1218284/1218284s.jpg".to_string()]
    );
    // 主规则 + 替换链（书名去书名号）
    let html = r#"<h2 class="t">《测试书》</h2>"#;
    let r5 = apply("class.t@text##《(.*)》##$1", html);
    assert_eq!(r5, vec!["测试书".to_string()]);
}

#[test]
fn test_json_path() {
    let json = r#"{"data":{"list":[{"name":"书1"},{"name":"书2"}]}}"#;
    let r = apply("{$.data.list.name}", json);
    assert_eq!(r, vec!["书1".to_string(), "书2".to_string()]);
}

#[test]
fn test_json_content_bare_key_forced_jsonpath() {
    // legacy ar.kt:469（setContent isJSON）：JSON 内容上裸键规则强制走 JsonPath
    let json = r#"{"data":{"list":[{"name":"书1"},{"name":"书2"}]}}"#;
    assert_eq!(apply("data.list.name", json), vec!["书1", "书2"]);
    // 数组根自动展开
    let arr = r#"[{"name":"甲"},{"name":"乙"}]"#;
    assert_eq!(apply("name", arr), vec!["甲", "乙"]);
    // 强制 JsonPath 后 ## 替换链仍生效
    let r = apply("data.name##书##本", r#"{"data":{"name":"书名"}}"#);
    assert_eq!(r, vec!["本名"]);
    // && 组合在裸键下同样可用
    let r2 = apply("a.b&&a.c", r#"{"a":{"b":"1","c":"2"}}"#);
    assert_eq!(r2, vec!["1", "2"]);
}

#[test]
fn test_json_explicit_prefix_unchanged_on_json() {
    let json = r#"{"data":{"name":"书名"}}"#;
    // 显式 $. / @Json: 前缀不受影响
    assert_eq!(apply("$.data.name", json), vec!["书名"]);
    assert_eq!(apply("@Json:$.data.name", json), vec!["书名"]);
    // 显式 @CSS: 固定走 JSoup（legacy @CSS: 分支），JSON 文本上无命中 → 空
    assert!(apply("@CSS:data.name", json).is_empty());
    // 非 JSON 括号文本（解析失败）不触发强制
    assert!(apply("data.name", "{not valid json").is_empty());
}

#[test]
fn test_html_css_unaffected_by_json_detect() {
    let html = r#"<html><body><div class="t">标题</div></body></html>"#;
    assert_eq!(apply("class.t@text", html), vec!["标题"]);
    // 含花括号内容的 HTML 不会被误判为 JSON
    let html2 = r#"<html><body><div class="t">{标题}</div></body></html>"#;
    assert_eq!(apply("class.t@text", html2), vec!["{标题}"]);
}

#[test]
fn test_json_path_recursive() {
    let json = r#"{"a":{"content":"深1","b":{"content":"深2"}},"content":"浅"}"#;
    // $..content 任意深度（DFS 前序：父键先出）
    let r = apply("$..content", json);
    assert_eq!(
        r,
        vec!["浅".to_string(), "深1".to_string(), "深2".to_string()]
    );
}

#[test]
fn test_json_path_indexes() {
    let json = r#"{"data":["a","b","c","d"]}"#;
    assert_eq!(apply("$.data[0]", json), vec!["a".to_string()]);
    assert_eq!(apply("$.data[-1]", json), vec!["d".to_string()]);
    assert_eq!(
        apply("$.data[1:3]", json),
        vec!["b".to_string(), "c".to_string()]
    );
    assert_eq!(
        apply("$.data[0,2]", json),
        vec!["a".to_string(), "c".to_string()]
    );
    assert_eq!(
        apply("$.data[*]", json),
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string()
        ]
    );
    // 对象通配
    let obj = r#"{"x":1,"y":2}"#;
    assert_eq!(apply("$.*", obj), vec!["1".to_string(), "2".to_string()]);
    // 引号键
    let obj2 = r#"{"a b":{"c":7}}"#;
    assert_eq!(apply("$['a b'].c", obj2), vec!["7".to_string()]);
}

/// E14：JsonPath 中部内嵌 `{$.x}` 子引用（模板拼接形态）
#[test]
fn test_json_path_embedded_template() {
    let json = r#"{"a":{"t":"T1"},"b":{"t":"T2"},"n":3}"#;
    // 双内嵌 + 字面连接符
    assert_eq!(apply("{$.a.t}-{$.b.t}", json), vec!["T1-T2".to_string()]);
    // 三内嵌含数字字段
    assert_eq!(
        apply("{$.a.t}({$.n}){$.b.t}", json),
        vec!["T1(3)T2".to_string()]
    );
    // 整体单内嵌保持原语义
    assert_eq!(apply("{$.a.t}", json), vec!["T1".to_string()]);
    // 内嵌 + 后缀字面量
    assert_eq!(apply("$.a.t", json), vec!["T1".to_string()]);
}

#[test]
fn test_json_path_filters() {
    let json = r#"{"list":[{"name":"书1","grade":5,"volume":false},{"name":"书2","grade":1},{"name":"书3","grade":8,"volume":true}]}"#;
    // 存在性过滤（真实书源：[?(@.bookName)]）
    let r = apply("$.list[?(@.name)]", json);
    assert_eq!(r.len(), 3);
    // 数值比较（真实书源：[?(@.grade > 1)]）
    let r2 = apply("$.list[?(@.grade > 1)].name", json);
    assert_eq!(r2, vec!["书1".to_string(), "书3".to_string()]);
    // 布尔比较（真实书源：[?(@.volume == false)]）
    let r3 = apply("$.list[?(@.volume == false)].name", json);
    assert_eq!(r3, vec!["书1".to_string()]);
    // 字符串比较 + && 组合
    let r4 = apply("$.list[?(@.grade > 1 && @.name == '书3')].name", json);
    assert_eq!(r4, vec!["书3".to_string()]);
    // 过滤后对象 JSON 化（bookList 场景）
    let r5 = apply("$.list[?(@.grade > 5)]", json);
    assert_eq!(r5.len(), 1);
    assert!(r5[0].contains("\"name\":\"书3\""));
    // || 组合规则：首个命中
    let r6 = apply("$.list[?(@.grade > 5)].name||$.list[0].name", json);
    assert_eq!(r6, vec!["书3".to_string()]);
    let r7 = apply("$.missing||$.list[0].name", json);
    assert_eq!(r7, vec!["书1".to_string()]);
}

/// JP1：裸存在性真值——键存在即匹配（值为 null/false/"" 也返回 true），
/// 含 `"vip":false` 的列表项不被误丢弃；键不存在仍不匹配
#[test]
fn test_jsonpath_filter_bare_existence() {
    // 正例：false/null/空串值均视为存在
    let json =
        r#"[{"vip":false,"tag":"a"},{"vip":null,"tag":"b"},{"vip":"","tag":"c"},{"tag":"d"}]"#;
    let r = apply("$[?(@.vip)].tag", json);
    assert_eq!(r, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    // 反例：键缺失不匹配
    let r2 = apply("$[?(@.nope)].tag", json);
    assert!(r2.is_empty());
}

/// JP2：缺失路径 `!=` 恒 true（legacy NotEqualsEvaluator 即 !EQ）
#[test]
fn test_jsonpath_filter_missing_not_equals() {
    let json = r#"[{"name":"a","extra":"x"},{"name":"b"}]"#;
    // 正例：a 有 extra='x'（!= 'y' 成立）；b 无 extra 键（缺失 → true）
    let r = apply("$[?(@.extra != 'y')].name", json);
    assert_eq!(r, vec!["a".to_string(), "b".to_string()]);
    // 反例：存在且相等 → false
    let r2 = apply("$[?(@.extra != 'x')].name", json);
    assert_eq!(r2, vec!["b".to_string()]);
}

/// JP3：数值与数字字符串松散相等（legacy NumberNode.equals → BigDecimal 比较）
#[test]
fn test_jsonpath_filter_loose_number_string_eq() {
    let json = r#"[{"n":123,"name":"a"},{"n":456,"name":"b"}]"#;
    // 正例：数值 123 == 字符串 '123'
    let r = apply("$[?(@.n == '123')].name", json);
    assert_eq!(r, vec!["a".to_string()]);
    // 正例（反向）：字符串 "123" == 数值 123
    let j2 = r#"[{"s":"123.5","name":"c"}]"#;
    let r2 = apply("$[?(@.s == 123.5)].name", j2);
    assert_eq!(r2, vec!["c".to_string()]);
    // 反例：非数字字符串仍不相等
    let r3 = apply("$[?(@.n == 'abc')].name", json);
    assert!(r3.is_empty());
}

/// JP4：高频操作符补齐——`=~` 正则 / `in` / `nin` / `size`
#[test]
fn test_jsonpath_filter_extra_operators() {
    // =~：正则匹配（右值 /pattern/ 形式）
    let json = r#"[{"name":"斗破苍穹"},{"name":"凡人修仙传"}]"#;
    let r = apply("$[?(@.name =~ /斗破/)].name", json);
    assert_eq!(r, vec!["斗破苍穹".to_string()]);
    let r2 = apply("$[?(@.name =~ /金庸/)].name", json);
    assert!(r2.is_empty(), "正则不匹配应为空: {r2:?}");
    // in / nin：左值（不）在右值数组中
    let j2 = r#"[{"g":1,"name":"a"},{"g":2,"name":"b"},{"g":3,"name":"c"}]"#;
    assert_eq!(
        apply("$[?(@.g in [2,3])].name", j2),
        vec!["b".to_string(), "c".to_string()]
    );
    assert_eq!(apply("$[?(@.g nin [2,3])].name", j2), vec!["a".to_string()]);
    assert_eq!(
        apply("$[?(@.name in ['a','c'])].name", j2),
        vec!["a".to_string(), "c".to_string()]
    );
    // size：数组/字符串长度等于右值
    let j3 = r#"[{"tags":[1,2,3],"name":"x"},{"tags":[1],"name":"y"},{"title":"abcd","name":"z"}]"#;
    assert_eq!(apply("$[?(@.tags size 3)].name", j3), vec!["x".to_string()]);
    assert_eq!(apply("$[?(@.tags size 1)].name", j3), vec!["y".to_string()]);
    assert_eq!(
        apply("$[?(@.title size 4)].name", j3),
        vec!["z".to_string()]
    );
    let r3 = apply("$[?(@.tags size 9)].name", j3);
    assert!(r3.is_empty(), "长度不等应为空: {r3:?}");
    // 词操作符不误伤路径键名（@.min / @.size 含操作符字样）
    let j4 = r#"[{"min":5},{"size":[1,2]}]"#;
    assert_eq!(apply("$[?(@.min > 3)]", j4).len(), 1);
    assert_eq!(apply("$[?(@.size size 2)]", j4).len(), 1);
}

/// P0-2：JSONPath 段数超限（超深路径）→ 返回错误语义（空结果 + 日志），不栈溢出 abort
#[test]
fn test_jsonpath_depth_limit_segments() {
    let json = r#"{"a":{"a":{"a":1}}}"#;
    // 100 段路径（远超 JSONPATH_MAX_DEPTH=64）
    let path = format!("$.{}", "a.".repeat(100).trim_end_matches('.'));
    let r = apply(&path, json);
    assert!(r.is_empty(), "超深路径应返回空结果而非崩溃: {r:?}");
    // 64 段以内仍正常（不误伤合法规则）
    let ok_path = format!("$.{}", "a.".repeat(3).trim_end_matches('.'));
    assert_eq!(apply(&ok_path, json), vec!["1".to_string()]);
}

/// P0-2：过滤表达式括号嵌套超限 → 空结果，不栈溢出 abort
#[test]
fn test_jsonpath_depth_limit_filter_parens() {
    let json = r#"{"list":[{"name":"书1"}]}"#;
    // 200 层括号嵌套的过滤表达式
    let expr = format!(
        "$.list[?({}@.name{})].name",
        "(".repeat(200),
        ")".repeat(200)
    );
    let r = apply(&expr, json);
    assert!(r.is_empty(), "超深括号过滤应返回空结果而非崩溃: {r:?}");
    // 正常括号仍工作
    let ok = apply("$.list[?((@.name))].name", json);
    assert_eq!(ok, vec!["书1".to_string()]);
}

/// P0-2：深层嵌套 JSON 解析（serde_json 128 层上限之外的输入）→ 解析错误回退空结果，不 abort；
/// 128 层内可解析的深层值 + 递归求值 → 求值深度上限（64）返回空结果，不 abort
#[test]
fn test_jsonpath_deep_nested_value() {
    // 500 层嵌套数组（超过 serde_json 内置 128 层递归上限 → 解析报错 → 回退空结果）
    let deep = format!("{}0{}", "[".repeat(500), "]".repeat(500));
    let r = apply("$[*]", &deep);
    assert!(r.is_empty(), "超深解析应优雅失败而非崩溃: {r:?}");
    let r2 = apply("$..x", &deep);
    assert!(r2.is_empty());
    // 100 层（可解析）嵌套数组：$[*].x 沿数组展开下钻 → 求值深度超限 → 空结果，不 abort
    let deep100 = format!("{}0{}", "[".repeat(100), "]".repeat(100));
    let r3 = apply("$[*].x", &deep100);
    assert!(r3.is_empty(), "深层值求值应受深度上限约束而非崩溃: {r3:?}");
    // 普通深度 JSON 正常
    let normal = r#"{"a":[{"b":{"c":"深"}}]}"#;
    assert_eq!(apply("$.a[0].b.c", normal), vec!["深".to_string()]);
}

#[test]
fn test_json_path_html_embedded() {
    // HTML 内嵌 JSON 行（json_from_html 回退）
    let html = "前文\n{\"data\":{\"name\":\"内嵌\"}}\n后文";
    let r = apply("$.data.name", html);
    assert_eq!(r, vec!["内嵌".to_string()]);
}

#[test]
fn test_js_rule() {
    let html = "abc123";
    // js: / @js: 前缀剥离 + result 变量注入
    assert_eq!(apply("js:result.length", html), vec!["6".to_string()]);
    assert_eq!(
        apply("@js:result.toUpperCase()", html),
        vec!["ABC123".to_string()]
    );
    // JS 失败 → 空结果
    assert!(apply("@js:throw new Error('x')", html).is_empty());
    // JS 返回空串 → 空结果
    assert!(apply("@js:''", html).is_empty());
}

#[test]
fn test_js_chain() {
    let html = r#"<div class="b">abc123</div>"#;
    // <js> 链：规则结果进 JS（result 变量），再进后续规则
    let r = apply("<js>result.toUpperCase()</js>", html);
    assert_eq!(r, vec![html.to_uppercase()]);
    // CSS → JS → 结果
    let r2 = apply("class.b@text@js:result.replace('abc','xyz')", html);
    assert_eq!(r2, vec!["xyz123".to_string()]);
    // <js> 在前 → JS → CSS 链（真实书源：<js>...</js>$.data[*] 形态）
    let r3 = apply("<js>result.replace('abc','xyz')</js>@class.b@text", html);
    assert_eq!(r3, vec!["xyz123".to_string()]);
    // JS 失败 → 整链空
    assert!(apply("class.b@text@js:throw new Error('x')", html).is_empty());
}

/// AR3：链式规则某段结果为空 → 提前终止整链返回空（legado AnalyzeRule.getString：
/// 段空结果为 null，后续所有段跳过；此前以空串续喂下一段，如
/// class.missing@text@js:result.length 误得 "0"）
#[test]
fn test_js_chain_empty_intermediate_terminates() {
    let html = r#"<div class="b">abc123</div>"#;
    // CSS 段无命中 → JS 不执行 → 空（非 "0"）
    assert!(apply("class.missing@text@js:result.length", html).is_empty());
    // 三段链：中段 JS 得空 → 后续段跳过 → 整链空
    assert!(apply("class.b@text<js>''</js>class.b@text", html).is_empty());
    // 对照：各段非空时多段链照常贯通
    let r = apply(
        "class.b@text<js>result.replace('abc','x')</js>@js:result + 'y'",
        html,
    );
    assert_eq!(r, vec!["x123y".to_string()]);
}

#[test]
fn test_inline_js_substitution() {
    let html =
        r#"<html><body><div class="book">书名A</div><div class="book">书名B</div></body></html>"#;
    // {{...}} JS 构建 CSS 选择器，替换回规则后执行
    let r = apply("{{'div.' + 'book'}}", html);
    assert_eq!(r.len(), 2);
    // JS 可读取注入的 result（当前上下文文本），条件返回正则规则
    let html2 = "书名：测试书 作者：张三";
    let rule = r#"{{result.startsWith('书名') ? '书名：(.+?)\\s' : 'div'}}"#;
    let r2 = apply(rule, html2);
    assert_eq!(r2.first().map(String::as_str), Some("测试书"));
    // JS 失败 → 展开为空 → 空结果
    assert!(apply("{{nonexistent.fn()}}", html).is_empty());
    // 未闭合 {{ 原样处理（按 JsonPath 分支解析失败 → 空结果），不 panic
    assert!(apply("{{div.book", html).is_empty());
}

#[test]
fn test_inline_jsonpath_substitution() {
    let json = r#"{"data":{"n":42}}"#;
    // {{$.x}} → JSONPath 提取（非 JS 执行），替换回规则后执行
    let r = apply("@js:{{$.data.n}}", json);
    assert_eq!(r, vec!["42".to_string()]);
    // 提取失败 → 替换为空 → 空结果
    let r2 = apply("@js:{{$.missing}}", json);
    assert!(r2.is_empty());
}

#[test]
fn test_inline_rule_ref_substitution() {
    // {{@@rule}} 规则引用（真实书源：{{@@[name$=update_time]@content##T##🔸}}）
    let html =
        r#"<meta name="update_time" content="2024-01-01"><div class="card"><p>正文</p></div>"#;
    let r = apply("更新时间：{{@@[name$=update_time]@content##-##/}}", html);
    assert_eq!(r, vec!["更新时间：2024/01/01".to_string()]);
    let r2 = apply("{{@@.card@p@text}}", html);
    assert_eq!(r2, vec!["正文".to_string()]);
    // {{//xpath}} 规则引用（真实书源：{{//data[@name='Title']/text()}}）
    let xml = r#"<data><item name="Title">测试书</item></data>"#;
    let r3 = apply("书名：{{//data/item[@name='Title']/text()}}", xml);
    assert_eq!(r3, vec!["书名：测试书".to_string()]);
    // {{$.x}} 多结果换行拼接
    assert_eq!(
        expand_inline(
            "{{$.list.name}}",
            r#"{"list":[{"name":"书1"},{"name":"书2"}]}"#
        ),
        "书1\n书2"
    );
}

/// P2：{{}} 模板替换值含规则控制标记（@js:/##/{{/@// 前缀）——替换后不再递归
/// 重新解析执行（防数据驱动二次执行），按纯文本返回；安全值拼接照常重新解析
#[test]
fn dbg_tmp_rule() {
    let html = "<html><body></body></html>";
    let r = apply_depth("@js:'@js:1+1'", html, 1, None);
    eprintln!("DBG apply_depth = {:?}", r);
    let mut vars = std::collections::HashMap::new();
    vars.insert("result".to_string(), html.to_string());
    eprintln!(
        "DBG eval_js = {:?}",
        crate::parser::js::eval_js("'@js:1+1'", &vars)
    );
    eprintln!("DBG segs len = {}", split_js_chain("@js:'@js:1+1'").len());
}

#[test]
fn test_inline_template_no_double_parse_of_control_values() {
    let html = "<html><body></body></html>";
    // 值以 @js: 开头：旧实现重新解析会再次执行（返回执行结果）；现按纯文本
    let r = apply("{{$.x}}", r#"{"x":"@js:result + '!'"}"#);
    assert_eq!(
        r,
        vec!["@js:result + '!'".to_string()],
        "@js: 前缀值不得二次执行"
    );
    // 值中间含 @js:（后缀链标记）：同样不再执行
    let r = apply("{{$.x}}", r#"{"x":"abc@js:1+1"}"#);
    assert_eq!(r, vec!["abc@js:1+1".to_string()]);
    // 值含 ##：旧实现重新解析会切出新规则链；现按纯文本
    let r = apply("x{{$.a}}y", r###"{"a":"##"}"###);
    assert_eq!(r, vec!["x##y".to_string()]);
    // 值含 {{：不再二次模板展开
    let r = apply("{{$.x}}", r#"{"x":"{{'div'}}"}"#);
    assert_eq!(r, vec!["{{'div'}}".to_string()]);
    // 值以 // 开头：不再按 XPath 规则解析
    let r = apply("{{$.x}}", r#"{"x":"//div"}"#);
    assert_eq!(r, vec!["//div".to_string()]);
    // {{@js:...}} 规则引用：内层求值结果含控制标记 → 外层按纯文本（不二次执行）
    let r = apply("{{@js:'@js:1+1'}}", html);
    assert_eq!(r, vec!["@js:1+1".to_string()]);
    // 前缀/替换段在控制值路径仍应用（##pat##rep）
    let r = apply("{{$.x}}##x##y", r#"{"x":"@js:xx"}"#);
    assert_eq!(r, vec!["@js:yy".to_string()]);
    // 安全值：拼接 CSS/正则仍照常重新解析执行（既有语义不受影响）
    let html2 = r#"<div class="book">书名A</div>"#;
    let r = apply("{{'div.' + 'book'}}", html2);
    assert_eq!(r.len(), 1);
    assert!(r[0].contains("书名A"), "CSS 拼接应重新解析执行: {r:?}");
    // 安全 CSS 值经 JSON 上下文提取（html 为 JSON 文本，无 HTML 可匹配）→
    // 按 legado 语义返回展开文本（{{}} 规则执行无果 → 规则串本身即结果）
    let r = apply("{{$.x}}", r#"{"x":"div.book"}"#);
    assert_eq!(
        r,
        vec!["div.book".to_string()],
        "JSON 上下文安全值按展开文本返回: {r:?}"
    );
}

#[test]
fn test_expand_inline() {
    // 数组下标形式 {{$.a[0]}}
    assert_eq!(
        expand_inline("{{$.list[0]}}", r#"{"list":["书1","书2"]}"#),
        "书1"
    );
    // 上下文非完整 JSON → 逐行提取 JSON 片段（json_from_html 回退）
    assert_eq!(
        expand_inline(
            "{{$.data.name}}",
            "前文\n{\"data\":{\"name\":\"内嵌\"}}\n后文"
        ),
        "内嵌"
    );
    // 未闭合 {{ 原样返回
    assert_eq!(expand_inline("{{div.book", "<html></html>"), "{{div.book");
}

#[test]
fn test_split_combined_basic() {
    let (sep, subs) = split_combined("a&&b&&c");
    assert_eq!(sep, Some("&&"));
    assert_eq!(subs, vec!["a", "b", "c"]);
    let (sep, subs) = split_combined("a||b");
    assert_eq!(sep, Some("||"));
    assert_eq!(subs, vec!["a", "b"]);
    let (sep, _subs) = split_combined("a%%b");
    assert_eq!(sep, Some("%%"));
    // 无分隔符
    let (sep, subs) = split_combined("a.b@c");
    assert_eq!(sep, None);
    assert_eq!(subs, vec!["a.b@c"]);
    // 平衡组内分隔符不参与（属性选择器 / 引号 / {{}} / 过滤表达式）
    let (sep, subs) = split_combined("a[href='x&&y']@b||c");
    assert_eq!(sep, Some("||"));
    assert_eq!(subs, vec!["a[href='x&&y']@b", "c"]);
    let (sep, _subs) = split_combined("[?(@.a == 'x' && @.b)]");
    assert_eq!(sep, None);
    let (sep, subs) = split_combined("{{a&&b}}&&c");
    assert_eq!(sep, Some("&&"));
    assert_eq!(subs, vec!["{{a&&b}}", "c"]);
}

#[test]
fn test_split_js_chain() {
    let segs = split_js_chain("a@href<js>code1</js>b@text@js:code2");
    let kinds: Vec<bool> = segs.iter().map(|s| s.is_js).collect();
    let texts: Vec<&str> = segs.iter().map(|s| s.text).collect();
    assert_eq!(kinds, vec![false, true, false, true]);
    assert_eq!(texts, vec!["a@href", "code1", "b@text", "code2"]);
    // 大小写不敏感
    let segs2 = split_js_chain("<JS>x</JS>");
    assert_eq!(segs2.len(), 1);
    assert!(segs2[0].is_js);
    assert_eq!(segs2[0].text, "x");
    // 未闭合 <js> → 普通段
    let segs3 = split_js_chain("<js>unclosed");
    assert_eq!(segs3.len(), 1);
    assert!(!segs3[0].is_js);
    // @js: 贪婪到末尾（后续 @js: 并入首个代码）
    let segs4 = split_js_chain("x@js:a@js:b");
    assert_eq!(segs4.len(), 2);
    assert_eq!(segs4[1].text, "a@js:b");
    // 无标记 → 单普通段
    let segs5 = split_js_chain("class.a@text");
    assert_eq!(segs5.len(), 1);
    assert!(!segs5[0].is_js);
}

#[test]
fn test_xpath_rules() {
    let xml = r#"<?xml version="1.0"?>
<library>
  <book id="1"><title>三体</title></book>
  <book id="2"><title>流浪地球</title></book>
</library>"#;
    assert_eq!(
        apply("//book/title", xml),
        vec!["三体".to_string(), "流浪地球".to_string()]
    );
    // && 组合
    let r = apply("//book[1]/title&&//book[2]/title", xml);
    assert_eq!(r, vec!["三体".to_string(), "流浪地球".to_string()]);
    // || 首个命中（legado：同一条规则只按最早出现的分隔符切分——|| 单独使用）
    let r2 = apply("//nonexistent||//book[1]/title", xml);
    assert_eq!(r2, vec!["三体".to_string()]);
}

#[test]
fn test_url_kind_kept() {
    // Url 变体保留（无检测路径——孤立 @ 现按 CSS/正则处理）
    let r = parse_rule("@class.a");
    assert_eq!(r.kind, RuleKind::Css);
    assert_eq!(r.body, "class.a");
}

/// F12/AR4：@get:{title}/@get:{bookName} 内建回退（legacy ar.kt:632-645）——
/// 变量表未命中时回退当前章标题/书名；无上下文 → 空串
#[test]
fn test_resolve_get_builtin_fallback() {
    // 无上下文：回退为空串（legacy chapter/book 为 null）
    let vars = RuleVars::new();
    assert_eq!(resolve_get("@get:{title}", &vars), "");
    assert_eq!(resolve_get("@get:{bookName}", &vars), "");

    // 注入实体上下文后命中
    let mut vars = RuleVars::new();
    vars.chapter_title = Some("第一章 测试".to_string());
    vars.book_name = Some("测试书名".to_string());
    assert_eq!(resolve_get("@get:{title}", &vars), "第一章 测试");
    assert_eq!(resolve_get("@get:{bookName}", &vars), "测试书名");

    // URL 模板内混合拼接 + 未命中键仍为空
    assert_eq!(
        resolve_get("https://x.test/@get:{bookName}/c/@get:{title}.json", &vars),
        "https://x.test/测试书名/c/第一章 测试.json"
    );
    assert_eq!(
        resolve_get("@get:{missing}@get:{title}", &vars),
        "第一章 测试"
    );
}

/// F12/AR4：变量表优先于内建回退（legacy getString 先查 varMap）
#[test]
fn test_resolve_get_vars_table_overrides_builtin() {
    let mut vars = RuleVars::new();
    vars.chapter_title = Some("章节标题".to_string());
    vars.book_name = Some("书名".to_string());
    vars.insert("title".to_string(), "@put 手动覆盖".to_string());
    vars.insert("bookName".to_string(), "@put 书名覆盖".to_string());
    assert_eq!(resolve_get("@get:{title}", &vars), "@put 手动覆盖");
    assert_eq!(resolve_get("@get:{bookName}", &vars), "@put 书名覆盖");
    // 其他键不受影响
    assert_eq!(resolve_get("@get:{other}", &vars), "");
}

/// F12/AR4：规则链中 @get:{title}/@get:{bookName} 经 apply_with_vars 求值同样吃到内建回退
/// （apply_single 先 makeUpRule 替换 @get 再执行规则）
#[test]
fn test_apply_with_vars_builtin_fallback() {
    let mut vars = RuleVars::new();
    vars.chapter_title = Some("第2章".to_string());
    vars.book_name = Some("书A".to_string());
    // 正文规则内嵌 @get 替换后成为可用正则
    let out = apply_with_vars("^@get:{bookName}，(.+)", "书A，张三", &mut vars);
    assert_eq!(out, vec!["张三".to_string()]);
    // 无回退上下文时 @get 解析为空串，规则失配
    let mut empty = RuleVars::new();
    let out2 = apply_with_vars("^@get:{bookName}，(.+)", "书A，张三", &mut empty);
    assert!(out2.is_empty() || out2.iter().all(|s| s.is_empty()));
}

/// F12/AR4：内建上下文字段不随 save_book_vars 持久化——跨请求无脏值
#[test]
fn test_book_vars_persistence_strips_context() {
    let mut vars = RuleVars::new();
    vars.insert("bid".to_string(), "42".to_string());
    vars.chapter_title = Some("脏标题".to_string());
    vars.book_name = Some("脏书名".to_string());
    save_book_vars("ns-f12", "src-f12", "https://b.test/1", &vars);
    let loaded = load_book_vars("ns-f12", "src-f12", "https://b.test/1");
    assert_eq!(loaded.get("bid").map(String::as_str), Some("42"));
    assert!(loaded.chapter_title.is_none(), "章标题上下文不应持久化");
    assert!(loaded.book_name.is_none(), "书名上下文不应持久化");
}

/// E10/AR5：push_js_context 把章节/书上下文以保留键写入 JS 变量表——
/// 结构体字段（title/url/next/bookName）+ map 透传（baseUrl/src/额外 `__x__` 键）
#[test]
fn test_push_js_context_reserved_keys() {
    let mut vars = RuleVars::new();
    vars.chapter_title = Some("第1章".to_string());
    vars.chapter_url = Some("https://b.test/c/1".to_string());
    vars.next_chapter_url = Some("https://b.test/c/2".to_string());
    vars.book_name = Some("书名甲".to_string());
    vars.insert("baseUrl".to_string(), "https://b.test/c/1".to_string());
    vars.insert("src".to_string(), "<p>正文</p>".to_string());
    vars.insert("__chapter_index__".to_string(), "3".to_string());
    vars.insert("__book_author__".to_string(), "作者丙".to_string());

    let mut js_vars = std::collections::HashMap::new();
    push_js_context(&mut js_vars, Some(&vars));
    assert_eq!(
        js_vars
            .get(crate::parser::rule::RK_CHAPTER_TITLE)
            .map(String::as_str),
        Some("第1章")
    );
    assert_eq!(
        js_vars
            .get(crate::parser::rule::RK_CHAPTER_URL)
            .map(String::as_str),
        Some("https://b.test/c/1")
    );
    assert_eq!(
        js_vars
            .get(crate::parser::rule::RK_NEXT_CHAPTER_URL)
            .map(String::as_str),
        Some("https://b.test/c/2")
    );
    assert_eq!(
        js_vars
            .get(crate::parser::rule::RK_BOOK_NAME)
            .map(String::as_str),
        Some("书名甲")
    );
    // map 透传：baseUrl/src/路由层反查的 index 与 author
    assert_eq!(
        js_vars.get("baseUrl").map(String::as_str),
        Some("https://b.test/c/1")
    );
    assert_eq!(js_vars.get("src").map(String::as_str), Some("<p>正文</p>"));
    assert_eq!(
        js_vars
            .get(crate::parser::rule::RK_CHAPTER_INDEX)
            .map(String::as_str),
        Some("3")
    );
    assert_eq!(
        js_vars
            .get(crate::parser::rule::RK_BOOK_AUTHOR)
            .map(String::as_str),
        Some("作者丙")
    );

    // None → 不写入任何键
    let mut empty = std::collections::HashMap::new();
    push_js_context(&mut empty, None);
    assert!(empty.is_empty());
}

/// E10/AR5：apply_with_vars 的 JS 规则可访问 chapter/title/book/nextChapterUrl 绑定
/// （push_js_context 贯通 → parser::js 展开为类型化全局；legacy AnalyzeRule evalJS）
#[test]
fn test_apply_with_vars_js_context_bindings() {
    let mut vars = RuleVars::new();
    vars.chapter_title = Some("第三章".to_string());
    vars.chapter_url = Some("https://b.test/c/3".to_string());
    vars.next_chapter_url = Some("https://b.test/c/4".to_string());
    vars.book_name = Some("测试书".to_string());
    // JS 规则：拼接上下文绑定
    let out = apply_with_vars(
        "@js:chapter.title + '|' + chapter.url + '|' + nextChapterUrl + '|' + book.name",
        "ignored",
        &mut vars,
    );
    assert_eq!(
        out,
        vec!["第三章|https://b.test/c/3|https://b.test/c/4|测试书".to_string()]
    );
    // {{}} 内嵌 JS 同样吃到绑定
    let out2 = apply_with_vars("^{{chapter.title}}，(.+)", "第三章，张三", &mut vars);
    assert_eq!(out2, vec!["张三".to_string()]);
}

/// P1 跨阶段合并：book 级作底、章节级覆盖（legacy book→chapter 单 varMap 回退链）
#[test]
fn test_load_book_vars_merged_two_level() {
    let ns = format!("p1m-{}", uuid::Uuid::new_v4());
    let src = "https://src.test/p1m";
    let mut book_level = RuleVars::new();
    book_level.insert("bid".to_string(), "B1".to_string());
    book_level.insert("shared".to_string(), "from-book".to_string());
    save_book_vars(&ns, src, "https://b.test/book", &book_level);
    let mut ch_level = RuleVars::new();
    ch_level.insert("cid".to_string(), "C7".to_string());
    ch_level.insert("shared".to_string(), "from-chapter".to_string());
    save_book_vars(&ns, src, "https://b.test/c/1", &ch_level);

    let merged = load_book_vars_merged(&ns, src, "https://b.test/book", "https://b.test/c/1");
    assert_eq!(
        merged.get("bid").map(String::as_str),
        Some("B1"),
        "book 级作底"
    );
    assert_eq!(
        merged.get("cid").map(String::as_str),
        Some("C7"),
        "章节级并入"
    );
    assert_eq!(
        merged.get("shared").map(String::as_str),
        Some("from-chapter"),
        "同名键章节级覆盖"
    );

    // 空 root / 同键 → 退化为单键读取
    let only_leaf = load_book_vars_merged(&ns, src, "", "https://b.test/c/1");
    assert_eq!(only_leaf.get("cid").map(String::as_str), Some("C7"));
    assert_eq!(only_leaf.get("bid"), None);
}

/// P1 双键保存：save_book_vars_two_level 同时写 root/leaf 两键
#[test]
fn test_save_book_vars_two_level() {
    let ns = format!("p1s-{}", uuid::Uuid::new_v4());
    let src = "https://src.test/p1s";
    let mut vars = RuleVars::new();
    vars.insert("k".to_string(), "v".to_string());
    save_book_vars_two_level(&ns, src, "https://b.test/book", "https://b.test/c/2", &vars);
    assert_eq!(
        load_book_vars(&ns, src, "https://b.test/c/2")
            .get("k")
            .map(String::as_str),
        Some("v")
    );
    assert_eq!(
        load_book_vars(&ns, src, "https://b.test/book")
            .get("k")
            .map(String::as_str),
        Some("v")
    );
    // root == leaf → 只存一份不 panic
    save_book_vars_two_level(
        &ns,
        src,
        "https://b.test/same",
        "https://b.test/same",
        &vars,
    );
    assert_eq!(
        load_book_vars(&ns, src, "https://b.test/same")
            .get("k")
            .map(String::as_str),
        Some("v")
    );
}

/// R3：同线程 4 槽位 `serde_json::Value` 缓存命中与连续字段提取一致性
#[test]
fn test_json_value_cache_hit_consistency() {
    let resp = "  {\"code\":0,\"data\":{\"title\":\"诡秘之主\",\"author\":\"爱潜水的乌贼\",\"cover\":\"/c.jpg\"}} \n";
    let item = r#"{"bookId":"10086","name":"道诡异仙","latest":"第888章"}"#;

    let hits_before = json_cache_hits();

    // 同一 JSON 响应连续提取多字段（含显式 $. 与裸键 Mode.Default 双路径）
    assert_eq!(apply("$.data.title", resp), vec!["诡秘之主".to_string()]);
    assert_eq!(
        apply("$.data.author", resp),
        vec!["爱潜水的乌贼".to_string()]
    );
    assert_eq!(apply("data.cover", resp), vec!["/c.jpg".to_string()]);

    // 交错提取条目 JSON 多字段，4 槽位同时保留外层响应与当前条目
    assert_eq!(apply("$.name", item), vec!["道诡异仙".to_string()]);
    assert_eq!(apply("$.latest", item), vec!["第888章".to_string()]);
    assert_eq!(apply("bookId", item), vec!["10086".to_string()]);
    // 回到外层响应仍命中缓存
    assert_eq!(apply("$.code", resp), vec!["0".to_string()]);

    let hits_after = json_cache_hits();
    assert!(
        hits_after >= hits_before + 6,
        "同线程 JSON Value 缓存应连续命中: before={hits_before}, after={hits_after}"
    );
}
