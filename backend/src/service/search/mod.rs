//! 搜索链路：searchUrl 构造 + 抓取 + ruleSearch 规则应用 → SearchBook
//!
//! 对齐 legacy WebBook.searchBook / BookList.analyzeBookList 语义（v1：无 JS）
//! 模块化拆分：`engine`（请求构造/限流/超时/抓取）、`extract`（列表与字段提取/清洗）、`rank`（精确匹配与归一化）。

mod engine;
mod extract;
mod rank;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub(crate) use engine::{
    build_request_url, concurrent_rate_acquire, is_data_uri, js_vars, split_url_suffix,
};
pub use engine::{build_search_url, search_one_source, to_absolute, UrlSuffix};
#[allow(unused_imports)]
pub(crate) use engine::{
    concurrent_rate_acquire_raw, concurrent_rate_sleep_ms, expand_url_js_templates,
};

pub(crate) use extract::{
    analyze_book_list_for_explore, expand_embedded_with_vars, field, field_url_with_vars,
    field_with_vars, format_book_author, format_book_name, normalize_kind_list,
    opt_field_with_vars, single_search_book, strip_list_rule_prefix, word_count_format,
};
#[allow(unused_imports)]
pub(crate) use extract::{
    expand_embedded, field_with_bridge, field_with_bridge_vars, opt_field, opt_field_with_bridge,
    opt_field_with_bridge_vars,
};

#[allow(unused_imports)]
pub(crate) use rank::normalize_search_text;
pub(crate) use rank::{exact_match, filter_exact};

/// 搜索结果（兼容 legacy SearchBook 字段）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBook {
    pub book_url: String,
    pub origin: String,
    pub origin_name: String,
    #[serde(rename = "type")]
    pub book_type: i64,
    pub name: String,
    pub author: String,
    pub kind: Option<String>,
    pub cover_url: Option<String>,
    pub intro: Option<String>,
    pub word_count: Option<String>,
    pub latest_chapter_title: Option<String>,
    /// 更新时间（legacy SearchBook/BookInfoRule.updateTime 契约；空规则时省略）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
    pub toc_url: String,
    pub time: i64,
    pub variable: Option<String>,
    pub origin_order: i64,
}

/// ruleSearch 结构（legacy BookListRule 字段）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRule {
    pub book_list: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub kind: Option<String>,
    pub intro: Option<String>,
    pub book_url: Option<String>,
    pub cover_url: Option<String>,
    pub word_count: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub score: Option<String>,
    pub comment: Option<String>,
    pub tags: Option<String>,
    pub serial_number: Option<String>,
    pub variable: Option<serde_json::Value>,
}
