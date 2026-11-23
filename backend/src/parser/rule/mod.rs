//! legado 规则引擎 v2：规则字符串解析 + CSS/JSONPath/Regex/XPath/JS 执行
//!
//! 规则语法（对齐 legado analyzeRule / AnalyzeByJSoup / AnalyzeByJSonPath / RuleAnalyzer）：
//! - 三段式：`规则体##替换正则##替换串`（## 分隔；第三段 `##` 后跟 `#` → 仅替换首个匹配）
//! - 纯替换规则：`##pat##rep###`（规则体为空 → 直接对输入应用替换）
//! - 类型检测：`{...}` JSONPath / `//` XPath / `@js:`|`js:` JS / `:` 正则（列表规则）/ 其余 CSS 或 Regex
//! - JS 链：`<js>...</js>` / `@js:`（贪婪到末尾）——规则结果进 JS（result 变量），再进后续规则
//! - `{{...}}` 内嵌表达式：`{{$.x}}`/`{{$[n]}}` JSONPath 提取；`{{@rule}}`/`{{//xpath}}`
//!   规则引用（legado isRule）；其余按 JS 执行（注入 result/key/page），结果替换回规则
//! - 组合分隔：`&&` 合并 / `||` 首个命中 / `%%` 按位交错（CSS/JSONPath/XPath 均支持）
//! - JSONPath v2：`$..` 递归下降 / `[?()]` 过滤（@ 属性、比较、&&/||）/ `[-1]` / 切片 / 通配
//! - 结果：字符串列表（legado 返回字符串列表语义）

mod chain;
mod eval;
#[cfg(test)]
mod tests;
mod vars;

pub(crate) use chain::find_closing_double_brace;
pub use chain::{contains_js_marker, parse_rule, split_combined, split_js_chain, JsSeg};
pub(crate) use eval::inline_js;
pub use eval::{apply, apply_init, apply_init_with_vars, apply_with_vars};
pub use vars::{
    clear_book_vars_memory_cache, clear_book_vars_memory_cache_ns, clear_book_vars_storage,
    load_book_vars, load_book_vars_merged, register_book_vars_storage, registered_storage,
    resolve_get, save_book_vars, save_book_vars_batch, save_book_vars_two_level, RuleVars,
};
pub(crate) use vars::{
    push_js_context, RK_BOOK_AUTHOR, RK_BOOK_NAME, RK_BOOK_URL, RK_CHAPTER_INDEX, RK_CHAPTER_TITLE,
    RK_CHAPTER_URL, RK_NEXT_CHAPTER_URL,
};

/// 解析后的规则
#[derive(Debug, Clone)]
pub struct Rule {
    /// 规则类型
    pub kind: RuleKind,
    /// 规则主体（类型检测前的原始文本，## 尾段已剥离）
    pub body: String,
    /// `##@前缀`（legacy 旧格式：结果前缀拼接，可选）
    pub prefix: Option<String>,
    /// `##` 第二段（替换正则，可选）
    pub replace_regex: Option<String>,
    /// `##` 第三段（替换串，可选；无第三段 = 替换为空串）
    pub replacement: Option<String>,
    /// `###` 标志（仅替换首个匹配；无匹配 → 空串，legado replaceFirst）
    pub replace_first: bool,
    /// 无显式类型前缀的裸键规则（legacy Mode.Default）：内容为 JSON 时强制走
    /// JsonPath 提取（legacy AnalyzeRule.SourceRule init 的 `isJSON ||` 分支，
    /// ar.kt:469——真实书源常省略 `$.` 前缀写 `data.list.name`）
    pub default_mode: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleKind {
    Css,
    JsonPath,
    Regex,
    XPath, // v2 支持（sxd-xpath）
    Js,    // v2 支持（boa）
           // P3-A：Url 变体已删除——无检测路径（detect_kind 无分支产生它），
           // 匹配臂与 url_replace 为不可达死代码；legacy @url 规则现落入 Css 分支。
}
