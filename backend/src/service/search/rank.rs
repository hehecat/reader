//! 搜索结果精准匹配、文本归一化与过滤逻辑

use super::SearchBook;

/// 精确匹配文本归一化：去首尾空白 + 全角 ASCII → 半角（U+FF01..U+FF5E → U+21..U+7E，
/// 全角空格 U+3000 → 半角空格）+ 小写。用于「精确」搜索：大小写/全半角差异不敏感。
pub(crate) fn normalize_search_text(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect::<String>()
        .to_lowercase()
}

/// 单本是否精确命中：书名或作者与 key 严格等值（归一化后；作者为空不参与匹配）
pub(crate) fn exact_match(book: &SearchBook, key: &str) -> bool {
    let q = normalize_search_text(key);
    if q.is_empty() {
        return true;
    }
    let name = normalize_search_text(&book.name);
    let author = normalize_search_text(&book.author);
    name == q || (!author.is_empty() && author == q)
}

/// 精确搜索过滤：书源规则解析后，仅保留书名/作者等值命中的结果（模糊模式不做此过滤）
pub(crate) fn filter_exact(books: Vec<SearchBook>, key: &str) -> Vec<SearchBook> {
    books.into_iter().filter(|b| exact_match(b, key)).collect()
}
