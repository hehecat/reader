//! OPDS 服务（OPDS 1.2 Atom + OPDS 2.0 JSON + OPDS-PSE 进度保存）
//!
//! 端点（统一走 `opds_ns` 认证：非 secure → default；secure → Basic（独立 OPDS 账号
//! 优先 → 系统用户账号）或 accessToken=username:token）：
//!
//! OPDS 1.2（application/atom+xml;profile=opds-catalog）：
//! - GET /opds                      根目录（导航：书架/最近阅读/全部分组/本地书/按来源）
//! - GET /opds/shelf                书架（全部书籍，acquisition feed）
//! - GET /opds/recent               最近阅读（dur_chapter_time > 0）
//! - GET /opds/local                本地书
//! - GET /opds/groups               分组导航
//! - GET /opds/group/{id}           分组内书籍
//! - GET /opds/source               按来源导航
//! - GET /opds/source/{name}        某来源书籍（name 为 base64url）
//! - GET /opds/search?q=            搜索（书名/作者，大小写不敏感）
//! - GET /opds/opensearch.xml       OpenSearch 描述
//!
//! 分页：?startIndex=&maxItems=（默认每页 50，上限 500）
//!
//! 获取/下载：
//! - GET /opds/acquire/{id}         获取正文文本（本地书首章 / 书源书首章）
//! - GET /opds/download/{id}?format= 下载（本地书原文件 txt/epub/zip；书源书 txt 拼接）
//!
//! OPDS 2.0（application/opds+json）：
//! - GET /opds/catalog              根目录（navigation + facets + groups）
//! - GET /opds/catalog/{shelf|recent|local|groups|source|search|group/{id}|source/{name}}
//!
//! OPDS-PSE（Partial Save Entries）：
//! - GET  /opds/save/{bookId}       当前进度 entry（Atom + content JSON；?format=json 输出 JSON）
//! - POST /opds/save/{bookId}       body/query：progress/position/total/chapterIndex/chapterTitle/timestamp
//!   → 写 books.dur_chapter_*

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use serde_json::json;

use crate::model::Book;
use crate::service::local_book::{is_local_book, ImportedBook};
use crate::storage::Storage;

/// 默认每页条数（任务契约：50）
pub const DEFAULT_PAGE_SIZE: usize = 50;
/// 每页上限（防超大响应）
const MAX_PAGE_SIZE: usize = 500;

// ---------------- 分页 ----------------

/// 解析分页参数（startIndex 默认 0；maxItems 默认 50，上限 500）
pub fn parse_page(params: &HashMap<String, String>) -> (usize, usize) {
    let start = params
        .get("startIndex")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let max = params
        .get("maxItems")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .min(MAX_PAGE_SIZE);
    (start, max)
}

/// 分页切片
fn paginate<T: Clone>(items: &[T], start: usize, max: usize) -> (Vec<T>, usize) {
    let total = items.len();
    if start >= total {
        return (Vec::new(), total);
    }
    let end = (start + max).min(total);
    (items[start..end].to_vec(), total)
}

/// OpenSearch 分页链接（first/prev/next），extra 为附加查询（如 &q=）
fn pager_links_xml(base: &str, extra: &str, start: usize, max: usize, total: usize) -> String {
    let mut s = String::new();
    let page_href = |s: usize| abs_href(base, &format!("?startIndex={s}&maxItems={max}{extra}"));
    s.push_str(&format!(
        "    <link rel=\"first\" href=\"{}\" type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\"/>\n",
        xml_escape(&page_href(0))
    ));
    if start > 0 {
        let prev = start.saturating_sub(max);
        s.push_str(&format!(
            "    <link rel=\"previous\" href=\"{}\" type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\"/>\n",
            xml_escape(&page_href(prev))
        ));
    }
    if start + max < total {
        s.push_str(&format!(
            "    <link rel=\"next\" href=\"{}\" type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\"/>\n",
            xml_escape(&page_href(start + max))
        ));
    }
    s
}

// ---------------- 工具 ----------------

/// 相对 href → 绝对 URL（GAP 52：OPDS 链接需绝对地址供外部阅读器使用）：
/// - 已是 http(s) 绝对地址 → 原样返回（封面等外链）
/// - 以 / 开头 → {base}{href}
/// - 其余（如 ?startIndex=...）→ {base}/{href}
pub fn abs_href(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let base = base.trim_end_matches('/');
    if href.starts_with('/') {
        format!("{base}{href}")
    } else if href.starts_with('?') {
        // 查询串直接拼接到 base 末尾（pager 链接：{self}?startIndex=...）
        format!("{base}{href}")
    } else {
        format!("{base}/{href}")
    }
}

/// 自引用 base（GAP 52）：优先 Host 请求头（X-Forwarded-Proto 可切 https），
/// Host 缺失时回退 http://localhost:{port}（与监听端口一致）。
pub fn opds_base(headers: &axum::http::HeaderMap, default_port: u16) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or("").trim().to_string())
        .filter(|s| s == "https" || s == "http")
        .unwrap_or_else(|| "http".to_string());
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("localhost:{default_port}"));
    format!("{scheme}://{host}")
}

/// bookUrl → base64url（URL 安全，无 / 等特殊字符——Path 单段可匹配）
pub fn encode_id(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes())
}

pub fn decode_id(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 毫秒时间戳 → RFC3339（UTC，秒精度）
fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 条目 updated：最新章节时间 > 0 用之，否则创建时间
fn entry_updated(book: &Book) -> String {
    let ms = if book.latest_chapter_time > 0 {
        book.latest_chapter_time
    } else {
        book.created_at
    };
    rfc3339(ms)
}

/// 来源键（书源名优先，缺省用书源 URL）
fn source_key(book: &Book) -> String {
    if book.origin_name.is_empty() {
        book.origin.clone()
    } else {
        book.origin_name.clone()
    }
}

/// 搜索匹配：书名/作者（大小写不敏感）
fn match_query(book: &Book, q: &str) -> bool {
    let ql = q.to_lowercase();
    if ql.is_empty() {
        return true;
    }
    book.name.to_lowercase().contains(&ql) || book.author.to_lowercase().contains(&ql)
}

/// 书架定位书籍（book_id 为 base64url(bookUrl)；解码失败时按原文精确匹配兜底）
async fn find_book(storage: &Storage, ns: &str, book_id: &str) -> Result<Book> {
    let decoded = decode_id(book_id);
    let books = storage.list_books(ns).await?;
    books
        .into_iter()
        .find(|b| b.book_url == decoded || b.book_url == book_id)
        .ok_or_else(|| anyhow::anyhow!("书籍不存在"))
}

/// 条目展示简介（用户修改优先）
fn display_intro(book: &Book) -> String {
    book.custom_intro
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| book.intro.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string()
}

/// 条目展示封面（用户封面优先）
fn display_cover(book: &Book) -> Option<String> {
    book.custom_cover_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| book.cover_url.as_deref().filter(|s| !s.is_empty()))
        .filter(|s| s.starts_with('/') || s.starts_with("http"))
        .map(str::to_string)
}

// ---------------- 本地书原文件 ----------------

/// 本地书原文件定位：
/// - local_file 双轨同步关联（GAP 170：书仓目录 / env READER_LOCAL_BOOK_DIR）优先
/// - local://{uuid} → storage/data/{ns}/opds_files/{uuid}.*（上传时落盘）
/// - storage/ 文件书（legacy）→ 按路径解析（兼容 index.epub 目录形态）
fn resolve_local_file(storage: &Storage, ns: &str, book: &Book) -> Option<PathBuf> {
    if let Some(p) = &book.local_file {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if book.book_url.starts_with("local://") {
        let id = book.book_url.trim_start_matches("local://");
        let dir = storage
            .config
            .storage_dir()
            .join("data")
            .join(ns)
            .join("opds_files");
        let rd = std::fs::read_dir(&dir).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file()
                && p.file_stem()
                    .map(|s| s.to_string_lossy() == id)
                    .unwrap_or(false)
            {
                return Some(p);
            }
        }
        return None;
    }
    if book.book_url.starts_with("storage/") || book.origin == "loc_book" {
        return resolve_storage_file(&storage.config.storage_dir(), &book.book_url);
    }
    None
}

/// 按目标格式过滤的本地原文件定位（format 为空时不过滤）：
/// `local://cccc` 同 stem 的 cccc.txt 与 cccc.epub 并存时，read_dir 顺序不定——
/// 必须按 format 精确匹配扩展名（否则 EPUB 下载可能拿到 TXT 文件，CI/本机 flaky）。
fn resolve_local_file_by_format(
    storage: &Storage,
    ns: &str,
    book: &Book,
    format: &str,
) -> Option<PathBuf> {
    if let Some(p) = &book.local_file {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            let ext = pb.extension().map(|e| e.to_string_lossy().to_lowercase());
            if format.is_empty() || ext.as_deref() == Some(format) || ext.as_deref().is_none() {
                return Some(pb);
            }
        }
    }
    if book.book_url.starts_with("local://") {
        let id = book.book_url.trim_start_matches("local://");
        let dir = storage
            .config
            .storage_dir()
            .join("data")
            .join(ns)
            .join("opds_files");
        let rd = std::fs::read_dir(&dir).ok()?;
        let mut best: Option<PathBuf> = None;
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file()
                || p.file_stem()
                    .map(|s| s.to_string_lossy() != id)
                    .unwrap_or(true)
            {
                continue;
            }
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase());
            if format.is_empty() {
                // 无格式要求：任意扩展名（优先 txt——与旧行为一致）
                if best.is_none() || ext.as_deref() == Some("txt") {
                    best = Some(p);
                }
            } else if ext.as_deref() == Some(format) {
                return Some(p);
            }
        }
        return best;
    }
    if book.book_url.starts_with("storage/") || book.origin == "loc_book" {
        return resolve_storage_file(&storage.config.storage_dir(), &book.book_url);
    }
    None
}

/// storage/ 文件书定位（兼容 legacy：{name}.epub/ 目录内含 index.epub 等形态）
fn resolve_storage_file(storage_dir: &Path, book_url: &str) -> Option<PathBuf> {
    let path = storage_dir.join(book_url.trim_start_matches("storage/"));
    if path.is_file() {
        return Some(path);
    }
    if path.is_dir() {
        let idx = path.join("index.epub");
        if idx.is_file() {
            return Some(idx);
        }
        let rd = std::fs::read_dir(&path).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() && p.to_string_lossy().to_lowercase().ends_with(".epub") {
                return Some(p);
            }
        }
    }
    let parent = path.parent()?;
    let idx = parent.join("index.epub");
    if idx.is_file() {
        return Some(idx);
    }
    let rd = std::fs::read_dir(parent).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() {
            let lower = p.to_string_lossy().to_lowercase();
            if lower.ends_with(".epub") || lower.ends_with(".txt") {
                return Some(p);
            }
        }
    }
    None
}

/// 本地文件解析（按扩展名分派：EPUB/TXT/MOBI/AZW3/PDF/FB2/DOCX——不联网）
fn parse_local_file(path: &Path) -> Result<ImportedBook> {
    crate::service::local_book::parse_loc_book_path(
        path,
        &[],
        crate::service::local_book::DEFAULT_EPUB_TOC_MODE,
        false,
    )
}

/// 本地书原文件的内容类型（按扩展名）
fn content_type_for_ext(ext: &str) -> &'static str {
    match ext {
        "epub" => "application/epub+zip",
        "zip" | "cbz" => "application/zip",
        "pdf" => "application/pdf",
        "umd" => "application/octet-stream",
        _ => "text/plain; charset=utf-8",
    }
}

/// 本地书是否可下载原文件（存在性检查；返回内容类型与下载 href）
fn local_original_link(storage: &Storage, ns: &str, book: &Book) -> Option<(String, String)> {
    let path = resolve_local_file(storage, ns, book)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext.is_empty() {
        return None;
    }
    let ct = content_type_for_ext(&ext);
    let href = if ext == "txt" {
        // TXT 原文件即 txt 下载
        format!("/opds/download/{}?format=txt", encode_id(&book.book_url))
    } else {
        format!("/opds/download/{}", encode_id(&book.book_url))
    };
    Some((ct.to_string(), href))
}

// ---------------- OPDS 1.2 ----------------

const ATOM_CT: &str = "application/atom+xml;profile=opds-catalog;charset=utf-8";
const NAV_CT: &str = "application/atom+xml;profile=opds-catalog;kind=navigation";

/// feed 开头（含命名空间——xmlns:opds 为 OPDS 2010 catalog，dc/dcterms 元数据）
fn feed_header(id: &str, title: &str, self_href: &str, kind: &str, base: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\" xmlns:opds=\"http://opds-spec.org/2010/catalog\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\">\n\
         \x20 <id>{id}</id>\n  <title>{}</title>\n  <updated>{}</updated>\n  <author><name>reader-dev</name></author>\n\
         \x20 <link rel=\"self\" href=\"{}\" type=\"{}\"/>\n\
         \x20 <link rel=\"start\" href=\"{}\" type=\"{}\"/>\n\
         \x20 <link rel=\"search\" href=\"{}\" type=\"application/opensearchdescription+xml\"/>\n\
         \x20 <link rel=\"search\" href=\"{}\" type=\"{}\"/>\n",
        xml_escape(title),
        now_rfc3339(),
        xml_escape(&abs_href(base, self_href)),
        kind,
        xml_escape(&abs_href(base, "/opds")),
        NAV_CT,
        xml_escape(&abs_href(base, "/opds/opensearch.xml")),
        xml_escape(&abs_href(base, "/opds/search?q={searchTerms}")),
        ATOM_CT,
    )
}

/// 导航条目（根目录/分组/来源导航）
fn nav_entry_xml(id_suffix: &str, title: &str, content: &str, href: &str, base: &str) -> String {
    let mut s = String::from("  <entry>\n");
    s.push_str(&format!(
        "    <title>{}</title>\n    <id>urn:uuid:reader-dev-nav-{id_suffix}</id>\n    <updated>{}</updated>\n",
        xml_escape(title),
        now_rfc3339()
    ));
    if !content.is_empty() {
        s.push_str(&format!(
            "    <content type=\"text\">{}</content>\n",
            xml_escape(content)
        ));
    }
    s.push_str(&format!(
        "    <link rel=\"subsection\" href=\"{}\" type=\"{}\"/>\n",
        xml_escape(&abs_href(base, href)),
        ATOM_CT
    ));
    s.push_str("  </entry>\n");
    s
}

/// 根目录（导航入口：书架/最近阅读/全部分组/本地书/按来源 + OpenSearch）
pub async fn root(storage: &Storage, ns: &str, base: &str) -> Result<String> {
    let books = storage.list_books(ns).await?;
    let total = books.len();
    let recent = books.iter().filter(|b| b.dur_chapter_time > 0).count();
    let local = books
        .iter()
        .filter(|b| is_local_book(&b.book_url, &b.origin))
        .count();
    let groups = storage.list_book_groups_with_count(ns).await?;
    let sources = source_counts(&books);

    let mut xml = feed_header(
        &format!("urn:uuid:reader-dev-root-{ns}"),
        "reader-dev 书架",
        "/opds",
        NAV_CT,
        base,
    );
    xml.push_str(&nav_entry_xml(
        &format!("shelf-{ns}"),
        "书架",
        &format!("全部书籍（{total} 本）"),
        "/opds/shelf",
        base,
    ));
    xml.push_str(&nav_entry_xml(
        &format!("recent-{ns}"),
        "最近阅读",
        &format!("有阅读进度的书籍（{recent} 本）"),
        "/opds/recent",
        base,
    ));
    xml.push_str(&nav_entry_xml(
        &format!("groups-{ns}"),
        "全部分组",
        &format!("{} 个分组", groups.len()),
        "/opds/groups",
        base,
    ));
    xml.push_str(&nav_entry_xml(
        &format!("local-{ns}"),
        "本地书",
        &format!("本地 TXT/EPUB 书籍（{local} 本）"),
        "/opds/local",
        base,
    ));
    xml.push_str(&nav_entry_xml(
        &format!("source-{ns}"),
        "按来源",
        &format!("{} 个书源", sources.len()),
        "/opds/source",
        base,
    ));
    // 分组直接入口（客户端可直达）
    for g in &groups {
        xml.push_str(&nav_entry_xml(
            &format!("group-{}-{ns}", g.id),
            &g.name,
            &format!("{} 本", g.book_count),
            &format!("/opds/group/{}", g.id),
            base,
        ));
    }
    xml.push_str("</feed>");
    Ok(xml)
}

/// 来源 → (名称, 数量)（按 origin_name 分组，缺省用 origin）
fn source_counts(books: &[Book]) -> Vec<(String, usize)> {
    let mut map: Vec<(String, usize)> = Vec::new();
    for b in books {
        let key = source_key(b);
        match map.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => *n += 1,
            None => map.push((key, 1)),
        }
    }
    map.sort_by(|a, b| a.0.cmp(&b.0));
    map
}

/// 书籍条目 XML（OPDS 1.2：id/title/author/updated/content/封面缩略图/acquisition/partial-save）
fn book_entry_xml(storage: &Storage, ns: &str, book: &Book, base: &str) -> String {
    let id = encode_id(&book.book_url);
    let mut e = String::from("  <entry>\n");
    e.push_str(&format!(
        "    <id>urn:uuid:{id}</id>\n    <title>{}</title>\n",
        xml_escape(&book.name)
    ));
    if !book.author.is_empty() {
        e.push_str(&format!(
            "    <author><name>{}</name></author>\n",
            xml_escape(&book.author)
        ));
    }
    e.push_str(&format!("    <updated>{}</updated>\n", entry_updated(book)));
    let intro = display_intro(book);
    if !intro.is_empty() {
        e.push_str(&format!(
            "    <content type=\"text\">{}</content>\n",
            xml_escape(&intro)
        ));
    }
    // OPDS 1.2 元数据：语言/出版时间/出版社/分类
    if let Some(lang) = book.language.as_deref().filter(|s| !s.is_empty()) {
        e.push_str(&format!(
            "    <dc:language>{}</dc:language>\n",
            xml_escape(lang)
        ));
    }
    if let Some(pub_at) = book.published_at.as_deref().filter(|s| !s.is_empty()) {
        e.push_str(&format!(
            "    <dcterms:published>{}</dcterms:published>\n",
            xml_escape(pub_at)
        ));
    }
    if let Some(publisher) = book.publisher.as_deref().filter(|s| !s.is_empty()) {
        e.push_str(&format!(
            "    <dcterms:publisher>{}</dcterms:publisher>\n",
            xml_escape(publisher)
        ));
    }
    if let Some(kind) = book.kind.as_deref().filter(|s| !s.is_empty()) {
        e.push_str(&format!(
            "    <category term=\"{}\" label=\"{}\"/>\n",
            xml_escape(kind),
            xml_escape(kind)
        ));
    }
    // 封面：缩略图 + 原图（OPDS 1.2 thumbnail relation；相对路径补绝对前缀）
    if let Some(cover) = display_cover(book) {
        e.push_str(&format!(
            "    <link rel=\"http://opds-spec.org/cover/thumbnail\" href=\"{}\" type=\"image/jpeg\"/>\n",
            xml_escape(&abs_href(base, &cover))
        ));
        e.push_str(&format!(
            "    <link rel=\"http://opds-spec.org/cover\" href=\"{}\" type=\"image/jpeg\"/>\n",
            xml_escape(&abs_href(base, &cover))
        ));
    }
    // acquisition：获取正文文本
    e.push_str(&format!(
        "    <link rel=\"http://opds-spec.org/acquisition\" href=\"{}\" type=\"text/plain\" title=\"获取正文\"/>\n",
        xml_escape(&abs_href(base, &format!("/opds/acquire/{id}")))
    ));
    // acquisition：下载 TXT
    e.push_str(&format!(
        "    <link rel=\"http://opds-spec.org/acquisition\" href=\"{}\" type=\"text/plain\" title=\"下载 TXT\"/>\n",
        xml_escape(&abs_href(base, &format!("/opds/download/{id}?format=txt")))
    ));
    // acquisition：本地书原文件（epub/zip）
    if let Some((ct, href)) = local_original_link(storage, ns, book) {
        e.push_str(&format!(
            "    <link rel=\"http://opds-spec.org/acquisition\" href=\"{}\" type=\"{}\" title=\"下载原文件\"/>\n",
            xml_escape(&abs_href(base, &href)),
            xml_escape(&ct)
        ));
    }
    // OPDS-PSE：进度保存
    e.push_str(&format!(
        "    <link rel=\"partial-save\" href=\"{}\" type=\"application/atom+xml;type=entry;profile=opds-save\" title=\"保存进度\"/>\n",
        xml_escape(&abs_href(base, &format!("/opds/save/{id}")))
    ));
    e.push_str("  </entry>\n");
    e
}

/// 书籍 acquisition feed（OPDS 1.2 + OpenSearch 分页元素）
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
async fn books_feed_xml(
    storage: &Storage,
    ns: &str,
    title: &str,
    id_suffix: &str,
    self_href: &str,
    extra_query: &str,
    books: &[Book],
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let (page, total) = paginate(books, start, max);
    let self_abs = abs_href(base, self_href);
    let mut xml = feed_header(
        &format!("urn:uuid:reader-dev-{id_suffix}-{ns}"),
        title,
        self_href,
        ATOM_CT,
        base,
    );
    xml.push_str(&format!(
        "  <opds:totalResults>{total}</opds:totalResults>\n"
    ));
    xml.push_str(&format!("  <opds:startIndex>{start}</opds:startIndex>\n"));
    xml.push_str(&format!("  <opds:itemsPerPage>{max}</opds:itemsPerPage>\n"));
    xml.push_str(&pager_links_xml(&self_abs, extra_query, start, max, total));
    for b in &page {
        xml.push_str(&book_entry_xml(storage, ns, b, base));
    }
    xml.push_str("</feed>");
    Ok(xml)
}

/// 分组/来源导航 feed
fn nav_feed_xml(
    ns: &str,
    title: &str,
    id_suffix: &str,
    self_href: &str,
    entries: Vec<(String, String, String, String)>, // (id_suffix, title, content, href)
    base: &str,
) -> String {
    let mut xml = feed_header(
        &format!("urn:uuid:reader-dev-{id_suffix}-{ns}"),
        title,
        self_href,
        NAV_CT,
        base,
    );
    for (sid, t, c, href) in entries {
        xml.push_str(&nav_entry_xml(&sid, &t, &c, &href, base));
    }
    xml.push_str("</feed>");
    xml
}

/// 书架 feed（全部书籍）
pub async fn shelf(
    storage: &Storage,
    ns: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books = storage.list_books(ns).await?;
    books_feed_xml(
        storage,
        ns,
        "书架",
        "shelf",
        "/opds/shelf",
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// 最近阅读 feed（dur_chapter_time > 0，按最近阅读时间倒序——list_books 默认序）
pub async fn recent(
    storage: &Storage,
    ns: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| b.dur_chapter_time > 0)
        .collect();
    books_feed_xml(
        storage,
        ns,
        "最近阅读",
        "recent",
        "/opds/recent",
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// 本地书 feed
pub async fn local(
    storage: &Storage,
    ns: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| is_local_book(&b.book_url, &b.origin))
        .collect();
    books_feed_xml(
        storage,
        ns,
        "本地书",
        "local",
        "/opds/local",
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// 分组导航 feed
pub async fn groups(storage: &Storage, ns: &str, base: &str) -> Result<String> {
    let groups = storage.list_book_groups_with_count(ns).await?;
    let entries = groups
        .iter()
        .map(|g| {
            (
                format!("group-{}-{ns}", g.id),
                g.name.clone(),
                format!("{} 本", g.book_count),
                format!("/opds/group/{}", g.id),
            )
        })
        .collect();
    Ok(nav_feed_xml(
        ns,
        "全部分组",
        "groups",
        "/opds/groups",
        entries,
        base,
    ))
}

/// 分组内书籍 feed
pub async fn group(
    storage: &Storage,
    ns: &str,
    id: i64,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let groups = storage.list_book_groups(ns).await?;
    let name = groups
        .iter()
        .find(|g| g.id == id)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| format!("分组 {id}"));
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| b.group == id)
        .collect();
    books_feed_xml(
        storage,
        ns,
        &name,
        &format!("group-{id}"),
        &format!("/opds/group/{id}"),
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// 按来源导航 feed
pub async fn sources(storage: &Storage, ns: &str, base: &str) -> Result<String> {
    let books = storage.list_books(ns).await?;
    let entries = source_counts(&books)
        .into_iter()
        .map(|(name, n)| {
            (
                format!("source-{}-{ns}", encode_id(&name)),
                name.clone(),
                format!("{n} 本"),
                format!("/opds/source/{}", encode_id(&name)),
            )
        })
        .collect();
    Ok(nav_feed_xml(
        ns,
        "按来源",
        "source",
        "/opds/source",
        entries,
        base,
    ))
}

/// 某来源书籍 feed（name 为 base64url）
pub async fn source(
    storage: &Storage,
    ns: &str,
    name_b64: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let name = decode_id(name_b64);
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| source_key(b) == name)
        .collect();
    let title = if name.is_empty() {
        "来源".to_string()
    } else {
        name.clone()
    };
    books_feed_xml(
        storage,
        ns,
        &format!("来源：{title}"),
        "source",
        &format!("/opds/source/{name_b64}"),
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// 搜索 feed（书名/作者）
pub async fn search(
    storage: &Storage,
    ns: &str,
    q: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| match_query(b, q))
        .collect();
    let extra = format!("&q={}", urlencoding::encode(q));
    books_feed_xml(
        storage,
        ns,
        &format!("搜索：{q}"),
        "search",
        "/opds/search",
        &extra,
        &books,
        start,
        max,
        base,
    )
    .await
}

/// OpenSearch 描述 XML
pub fn open_search_xml(base: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <OpenSearchDescription xmlns=\"http://a9.com/-/spec/opensearch/1.1/\">\n\
         \x20 <ShortName>reader-dev</ShortName>\n\
         \x20 <Description>reader-dev 书架搜索（书名/作者）</Description>\n\
         \x20 <Tags>book reader opds</Tags>\n\
         \x20 <Url type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\" template=\"{}\"/>\n\
         \x20 <Url type=\"application/opds+json\" template=\"{}\"/>\n\
         </OpenSearchDescription>\n",
        xml_escape(&abs_href(base, "/opds/search?q={searchTerms}")),
        xml_escape(&abs_href(base, "/opds/catalog/search?q={searchTerms}")),
    )
}

// ---------------- OPDS 2.0 ----------------

/// Link 对象（OPDS 2.0）
fn link_obj(href: &str, ct: &str, title: Option<&str>, rel: Option<&str>) -> serde_json::Value {
    let mut l = serde_json::Map::new();
    l.insert("href".into(), json!(href));
    l.insert("type".into(), json!(ct));
    if let Some(t) = title {
        l.insert("title".into(), json!(t));
    }
    if let Some(r) = rel {
        l.insert("rel".into(), json!(r));
    }
    serde_json::Value::Object(l)
}

/// 导航对象（OPDS 2.0）
fn nav_obj(title: &str, href: &str) -> serde_json::Value {
    json!({
        "metadata": { "title": title },
        "links": [ link_obj(href, "application/opds+json", None, Some("subsection")) ],
    })
}

/// OPDS 2.0 根目录（navigation + facets + groups）
pub async fn catalog_json(storage: &Storage, ns: &str, base: &str) -> Result<String> {
    let books = storage.list_books(ns).await?;
    let groups = storage.list_book_groups_with_count(ns).await?;
    let sources = source_counts(&books);
    let total = books.len();

    let navigation = vec![
        nav_obj("书架", &abs_href(base, "/opds/catalog/shelf")),
        nav_obj("最近阅读", &abs_href(base, "/opds/catalog/recent")),
        nav_obj("本地书", &abs_href(base, "/opds/catalog/local")),
        nav_obj("全部分组", &abs_href(base, "/opds/catalog/groups")),
        nav_obj("按来源", &abs_href(base, "/opds/catalog/source")),
    ];

    let group_links: Vec<serde_json::Value> = groups
        .iter()
        .map(|g| {
            link_obj(
                &abs_href(base, &format!("/opds/catalog/group/{}", g.id)),
                "application/opds+json",
                Some(&g.name),
                Some("subsection"),
            )
        })
        .collect();
    let source_links: Vec<serde_json::Value> = sources
        .iter()
        .map(|(name, _)| {
            link_obj(
                &abs_href(base, &format!("/opds/catalog/source/{}", encode_id(name))),
                "application/opds+json",
                Some(name),
                Some("subsection"),
            )
        })
        .collect();
    let facets = vec![
        json!({ "metadata": { "title": "分组" }, "links": group_links }),
        json!({ "metadata": { "title": "来源" }, "links": source_links }),
    ];

    let group_objs: Vec<serde_json::Value> = groups
        .iter()
        .map(|g| {
            json!({
                "metadata": { "title": g.name, "numberOfItems": g.book_count },
                "links": [ link_obj(&abs_href(base, &format!("/opds/catalog/group/{}", g.id)), "application/opds+json", None, Some("self")) ],
            })
        })
        .collect();

    let catalog = json!({
        "metadata": {
            "id": format!("urn:uuid:reader-dev-catalog-{ns}"),
            "title": "reader-dev",
            "updated": now_rfc3339(),
            "numberOfItems": total,
        },
        "links": [
            link_obj(&abs_href(base, "/opds/catalog"), "application/opds+json", None, Some("self")),
            link_obj(&abs_href(base, "/opds/catalog"), "application/opds+json", None, Some("start")),
            link_obj(&abs_href(base, "/opds/opensearch.xml"), "application/opensearchdescription+xml", Some("OpenSearch 描述"), Some("search")),
            link_obj(&abs_href(base, "/opds/catalog/search?q={searchTerms}"), "application/opds+json", None, Some("search")),
        ],
        "navigation": navigation,
        "facets": facets,
        "groups": group_objs,
    });
    Ok(catalog.to_string())
}

/// 书籍 Publication 对象（OPDS 2.0：metadata + links + images）
fn publication_json(storage: &Storage, ns: &str, book: &Book, base: &str) -> serde_json::Value {
    let id = encode_id(&book.book_url);
    let mut metadata = serde_json::Map::new();
    metadata.insert("identifier".into(), json!(format!("urn:uuid:{id}")));
    metadata.insert("title".into(), json!(book.name));
    if !book.author.is_empty() {
        metadata.insert("authors".into(), json!([{ "name": book.author }]));
    }
    if let Some(pub_at) = book.published_at.as_deref().filter(|s| !s.is_empty()) {
        metadata.insert("published".into(), json!(pub_at));
    }
    if let Some(lang) = book.language.as_deref().filter(|s| !s.is_empty()) {
        metadata.insert("language".into(), json!(lang));
    }
    if let Some(publisher) = book.publisher.as_deref().filter(|s| !s.is_empty()) {
        metadata.insert("publisher".into(), json!(publisher));
    }
    if let Some(kind) = book.kind.as_deref().filter(|s| !s.is_empty()) {
        metadata.insert(
            "categories".into(),
            json!([{ "term": kind, "label": kind }]),
        );
    }
    let intro = display_intro(book);
    if !intro.is_empty() {
        metadata.insert("description".into(), json!(intro));
    }
    metadata.insert("updated".into(), json!(entry_updated(book)));

    let mut links = vec![
        link_obj(
            &abs_href(base, &format!("/opds/acquire/{id}")),
            "text/plain",
            Some("获取正文"),
            Some("http://opds-spec.org/acquisition"),
        ),
        link_obj(
            &abs_href(base, &format!("/opds/download/{id}?format=txt")),
            "text/plain",
            Some("下载 TXT"),
            Some("http://opds-spec.org/acquisition"),
        ),
        link_obj(
            &abs_href(base, &format!("/opds-save?bookId={id}")),
            "application/atom+xml;type=entry;profile=opds-save",
            Some("保存进度"),
            Some("partial-save"),
        ),
    ];
    let mut images: Vec<serde_json::Value> = Vec::new();
    if let Some(cover) = display_cover(book) {
        let cover_abs = abs_href(base, &cover);
        links.push(link_obj(
            &cover_abs,
            "image/jpeg",
            None,
            Some("http://opds-spec.org/cover"),
        ));
        images.push(json!({ "href": cover_abs, "type": "image/jpeg" }));
    }
    if let Some((ct, href)) = local_original_link(storage, ns, book) {
        links.push(link_obj(
            &abs_href(base, &href),
            &ct,
            Some("下载原文件"),
            Some("http://opds-spec.org/acquisition"),
        ));
    }

    json!({
        "metadata": serde_json::Value::Object(metadata),
        "links": links,
        "images": images,
    })
}

/// OPDS 2.0 acquisition feed（分页：metadata.numberOfItems + next/prev links）
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
async fn feed_json(
    storage: &Storage,
    ns: &str,
    title: &str,
    id_suffix: &str,
    self_href: &str,
    extra_query: &str,
    books: &[Book],
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let (page, total) = paginate(books, start, max);
    let self_abs = abs_href(base, self_href);
    let mut links = vec![
        link_obj(&self_abs, "application/opds+json", None, Some("self")),
        link_obj(
            &abs_href(base, "/opds/catalog"),
            "application/opds+json",
            None,
            Some("start"),
        ),
        link_obj(
            &abs_href(base, "/opds/opensearch.xml"),
            "application/opensearchdescription+xml",
            None,
            Some("search"),
        ),
        link_obj(
            &abs_href(base, "/opds/catalog/search?q={searchTerms}"),
            "application/opds+json",
            None,
            Some("search"),
        ),
    ];
    let page_href = |s: usize| {
        abs_href(
            &self_abs,
            &format!("?startIndex={s}&maxItems={max}{extra_query}"),
        )
    };
    links.push(link_obj(
        &page_href(0),
        "application/opds+json",
        None,
        Some("first"),
    ));
    if start > 0 {
        links.push(link_obj(
            &page_href(start.saturating_sub(max)),
            "application/opds+json",
            None,
            Some("previous"),
        ));
    }
    if start + max < total {
        links.push(link_obj(
            &page_href(start + max),
            "application/opds+json",
            None,
            Some("next"),
        ));
    }

    let feed = json!({
        "metadata": {
            "id": format!("urn:uuid:reader-dev-{id_suffix}-{ns}"),
            "title": title,
            "updated": now_rfc3339(),
            "numberOfItems": total,
        },
        "links": links,
        "publications": page.iter().map(|b| publication_json(storage, ns, b, base)).collect::<Vec<_>>(),
    });
    Ok(feed.to_string())
}

/// OPDS 2.0 导航 feed（分组/来源目录）
fn nav_feed_json(
    ns: &str,
    title: &str,
    id_suffix: &str,
    self_href: &str,
    entries: Vec<(String, String)>,
    base: &str,
) -> String {
    let navigation: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|(name, href)| {
            json!({
                "metadata": { "title": name },
                "links": [ link_obj(&abs_href(base, &href), "application/opds+json", None, Some("subsection")) ],
            })
        })
        .collect();
    json!({
        "metadata": { "id": format!("urn:uuid:reader-dev-{id_suffix}-{ns}"), "title": title, "updated": now_rfc3339() },
        "links": [ link_obj(&abs_href(base, self_href), "application/opds+json", None, Some("self")) ],
        "navigation": navigation,
    })
    .to_string()
}

/// OPDS 2.0 书架 feed
pub async fn shelf_json(
    storage: &Storage,
    ns: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books = storage.list_books(ns).await?;
    feed_json(
        storage,
        ns,
        "书架",
        "shelf",
        "/opds/catalog/shelf",
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// OPDS 2.0 最近阅读 feed
pub async fn recent_json(
    storage: &Storage,
    ns: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| b.dur_chapter_time > 0)
        .collect();
    feed_json(
        storage,
        ns,
        "最近阅读",
        "recent",
        "/opds/catalog/recent",
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// OPDS 2.0 本地书 feed
pub async fn local_json(
    storage: &Storage,
    ns: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| is_local_book(&b.book_url, &b.origin))
        .collect();
    feed_json(
        storage,
        ns,
        "本地书",
        "local",
        "/opds/catalog/local",
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// OPDS 2.0 分组导航
pub async fn groups_json(storage: &Storage, ns: &str, base: &str) -> Result<String> {
    let groups = storage.list_book_groups_with_count(ns).await?;
    let entries = groups
        .iter()
        .map(|g| (g.name.clone(), format!("/opds/catalog/group/{}", g.id)))
        .collect();
    Ok(nav_feed_json(
        ns,
        "全部分组",
        "groups",
        "/opds/catalog/groups",
        entries,
        base,
    ))
}

/// OPDS 2.0 分组内书籍 feed
pub async fn group_json(
    storage: &Storage,
    ns: &str,
    id: i64,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let groups = storage.list_book_groups(ns).await?;
    let name = groups
        .iter()
        .find(|g| g.id == id)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| format!("分组 {id}"));
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| b.group == id)
        .collect();
    feed_json(
        storage,
        ns,
        &name,
        &format!("group-{id}"),
        &format!("/opds/catalog/group/{id}"),
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// OPDS 2.0 按来源导航
pub async fn sources_json(storage: &Storage, ns: &str, base: &str) -> Result<String> {
    let books = storage.list_books(ns).await?;
    let entries = source_counts(&books)
        .into_iter()
        .map(|(name, _)| {
            (
                name.clone(),
                format!("/opds/catalog/source/{}", encode_id(&name)),
            )
        })
        .collect();
    Ok(nav_feed_json(
        ns,
        "按来源",
        "source",
        "/opds/catalog/source",
        entries,
        base,
    ))
}

/// OPDS 2.0 某来源书籍 feed
pub async fn source_json(
    storage: &Storage,
    ns: &str,
    name_b64: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let name = decode_id(name_b64);
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| source_key(b) == name)
        .collect();
    let title = if name.is_empty() {
        "来源".to_string()
    } else {
        name.clone()
    };
    feed_json(
        storage,
        ns,
        &format!("来源：{title}"),
        "source",
        &format!("/opds/catalog/source/{name_b64}"),
        "",
        &books,
        start,
        max,
        base,
    )
    .await
}

/// OPDS 2.0 搜索 feed
pub async fn search_json(
    storage: &Storage,
    ns: &str,
    q: &str,
    start: usize,
    max: usize,
    base: &str,
) -> Result<String> {
    let books: Vec<Book> = storage
        .list_books(ns)
        .await?
        .into_iter()
        .filter(|b| match_query(b, q))
        .collect();
    let extra = format!("&q={}", urlencoding::encode(q));
    feed_json(
        storage,
        ns,
        &format!("搜索：{q}"),
        "search",
        "/opds/catalog/search",
        &extra,
        &books,
        start,
        max,
        base,
    )
    .await
}

// ---------------- 获取 / 下载 ----------------

/// 获取正文文本（书源书：目录 → 首章正文；本地书：首章）
pub async fn acquire(storage: &Storage, ns: &str, book_id: &str) -> Result<(String, Vec<u8>)> {
    let book = find_book(storage, ns, book_id).await?;
    let fname = format!("{}.txt", book.name);

    // 本地书（local://：章节表；storage/ 文件书：解析文件）
    if book.book_url.starts_with("local://") {
        let chapters = storage.list_chapters(&book.book_url).await?;
        for (idx, _) in chapters.iter().take(20) {
            if let Some(content) = storage
                .get_chapter_content(ns, &book.book_url, *idx)
                .await?
            {
                if !content.trim().is_empty() {
                    return Ok((fname, content.into_bytes()));
                }
            }
        }
        return Err(anyhow::anyhow!("本地书无章节内容"));
    }
    if is_local_book(&book.book_url, &book.origin) {
        if let Some(path) = resolve_local_file(storage, ns, &book) {
            let imported = parse_local_file(&path)?;
            if let Some(first) = imported
                .chapters
                .iter()
                .find(|c| !c.content.trim().is_empty())
            {
                return Ok((fname, first.content.clone().into_bytes()));
            }
        }
        return Err(anyhow::anyhow!("本地书文件不存在或无可读章节"));
    }

    // 书源书：目录 → 首章正文
    let source = storage
        .find_book_source(ns, &book.origin)
        .await?
        .ok_or_else(|| anyhow::anyhow!("书源不存在"))?;
    let toc_url = if book.toc_url.is_empty() {
        book.book_url.clone()
    } else {
        book.toc_url.clone()
    };
    let chapters = crate::service::book::analyze_toc(
        ns,
        &toc_url,
        &source,
        10,
        Some(&book.name),
        &book.book_url,
    )
    .await?;
    for ch in chapters.iter() {
        if ch.is_volume || ch.url.is_empty() {
            continue;
        }
        let content = crate::service::book::analyze_content(
            ns,
            &ch.url,
            &source,
            5,
            Some(&ch.title),
            Some(&book.name),
            &book.book_url,
        )
        .await?;
        if !content.trim().is_empty() {
            return Ok((fname, content.into_bytes()));
        }
    }
    Err(anyhow::anyhow!("未获取到正文"))
}

/// 下载（返回 (文件名, 字节, Content-Type)）：
/// - 本地书：原文件字节（txt/epub/zip）；format=txt 且原文件为 EPUB 时正文拼接
/// - 书源书：目录 + 正文拼接 TXT（限 max_chapters 防超时）
pub async fn download(
    storage: &Storage,
    ns: &str,
    book_id: &str,
    format: &str,
    max_chapters: Option<usize>,
) -> Result<(String, Vec<u8>, String)> {
    let book = find_book(storage, ns, book_id).await?;

    if is_local_book(&book.book_url, &book.origin) {
        // 原文件优先（local:// 上传落盘 / storage/ legacy 文件书）——按 format 精确匹配扩展名
        if let Some(path) = resolve_local_file_by_format(storage, ns, &book, format) {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if format == "txt" && !ext.is_empty() && ext != "txt" {
                // EPUB 等 → 正文拼接（不联网）
                let imported = parse_local_file(&path)?;
                let txt = imported
                    .chapters
                    .iter()
                    .map(|c| c.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                return Ok((
                    format!("{}.txt", book.name),
                    txt.into_bytes(),
                    "text/plain; charset=utf-8".to_string(),
                ));
            }
            let bytes = std::fs::read(&path)?;
            let name = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.{}", book.name, ext));
            let ct = content_type_for_ext(&ext).to_string();
            return Ok((name, bytes, ct));
        }
        // 无原文件（旧数据导入的 local:// 书）：章节拼接
        if book.book_url.starts_with("local://") {
            let chapters = storage.list_chapters(&book.book_url).await?;
            let mut txt = format!("{}\n{}\n\n", book.name, book.author);
            for (idx, title) in chapters {
                if let Some(content) = storage.get_chapter_content(ns, &book.book_url, idx).await? {
                    txt.push_str(&format!("\n{title}\n\n{content}"));
                }
            }
            return Ok((
                format!("{}.txt", book.name),
                txt.into_bytes(),
                "text/plain; charset=utf-8".to_string(),
            ));
        }
        return Err(anyhow::anyhow!("本地书文件不存在"));
    }

    // 书源书：TXT 导出（联网拼接）
    if format != "txt" {
        return Err(anyhow::anyhow!("书源书仅支持 txt 格式导出"));
    }
    let max_chapters = max_chapters.unwrap_or(usize::MAX);
    let source = storage
        .find_book_source(ns, &book.origin)
        .await?
        .ok_or_else(|| anyhow::anyhow!("书源不存在"))?;
    let toc_url = if book.toc_url.is_empty() {
        book.book_url.clone()
    } else {
        book.toc_url.clone()
    };
    let chapters = crate::service::book::analyze_toc(
        ns,
        &toc_url,
        &source,
        20,
        Some(&book.name),
        &book.book_url,
    )
    .await?;
    let mut txt = String::new();
    txt.push_str(&format!("{}\n{}\n\n", book.name, book.author));
    let mut count = 0usize;
    for ch in chapters.iter().take(max_chapters) {
        if ch.is_volume || ch.url.is_empty() {
            continue;
        }
        match crate::service::book::analyze_content(
            ns,
            &ch.url,
            &source,
            5,
            Some(&ch.title),
            Some(&book.name),
            &book.book_url,
        )
        .await
        {
            Ok(content) => {
                txt.push_str(&format!("\n{}\n\n{}", ch.title, content));
                count += 1;
            }
            Err(e) => {
                tracing::warn!("下载章节失败 {}: {e}", ch.title);
            }
        }
    }
    tracing::info!("OPDS 下载 [{ns}] {}：{count} 章", book.name);
    Ok((
        format!("{}.txt", book.name),
        txt.into_bytes(),
        "text/plain; charset=utf-8".to_string(),
    ))
}

// ---------------- OPDS-PSE（Partial Save Entries） ----------------

/// 当前进度 JSON（GET /opds/save/{bookId} 的 content）
pub async fn save_entry_json(
    storage: &Storage,
    ns: &str,
    book_id: &str,
) -> Result<serde_json::Value> {
    let book = find_book(storage, ns, book_id).await?;
    Ok(json!({
        "bookUrl": book.book_url,
        "bookId": book_id,
        "title": book.name,
        "chapterIndex": book.dur_chapter_index,
        "chapterTitle": book.dur_chapter_title,
        "position": book.dur_chapter_pos,
        "progress": serde_json::Value::Null,
        "total": serde_json::Value::Null,
        "timestamp": book.dur_chapter_time,
    }))
}

/// 当前进度 Atom entry（OPDS-PSE：entry + link rel=partial-save + content JSON）
pub async fn save_entry_xml(
    storage: &Storage,
    ns: &str,
    book_id: &str,
    base: &str,
) -> Result<String> {
    let book = find_book(storage, ns, book_id).await?;
    let payload = save_entry_json(storage, ns, book_id).await?;
    let updated = if book.dur_chapter_time > 0 {
        rfc3339(book.dur_chapter_time)
    } else {
        entry_updated(&book)
    };
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <entry xmlns=\"http://www.w3.org/2005/Atom\" xmlns:opds=\"http://opds-spec.org/2010/catalog\">\n\
         \x20 <id>urn:uuid:{}</id>\n  <title>{}</title>\n  <updated>{updated}</updated>\n",
        xml_escape(book_id),
        xml_escape(&book.name)
    )
    .to_string()
        + &format!(
            "  <author><name>{}</name></author>\n",
            xml_escape(&book.author)
        )
        + &format!(
            "  <link rel=\"partial-save\" href=\"{}\" type=\"application/atom+xml;type=entry;profile=opds-save\"/>\n",
            xml_escape(&abs_href(base, &format!("/opds/save/{book_id}")))
        )
        + "  <category term=\"http://opds-spec.org/save/progress\" label=\"阅读进度\"/>\n"
        + &format!(
            "  <content type=\"application/json\">{}</content>\n</entry>\n",
            xml_escape(&payload.to_string())
        ))
}

/// 应用进度保存（POST /opds/save/{bookId} → books.dur_chapter_*）
/// progress/total 无对应列：仅用于响应回显（position/total 可推算 progress）。
#[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
pub async fn apply_save(
    storage: &Storage,
    ns: &str,
    book_id: &str,
    progress: Option<f64>,
    position: Option<i64>,
    total: Option<i64>,
    chapter_index: Option<i64>,
    chapter_title: Option<String>,
    timestamp: Option<i64>,
) -> Result<serde_json::Value> {
    let book = find_book(storage, ns, book_id).await?;
    let index = chapter_index.unwrap_or(book.dur_chapter_index);
    let title = chapter_title.or_else(|| book.dur_chapter_title.clone());
    let pos = position.unwrap_or(book.dur_chapter_pos);
    let time = timestamp
        .filter(|t| *t > 0)
        .unwrap_or_else(|| Utc::now().timestamp_millis());

    storage
        .update_book_progress(ns, &book.book_url, title.as_deref(), index, pos, time)
        .await?;

    let progress = progress.or_else(|| {
        total.and_then(|t| {
            if t > 0 {
                Some(pos as f64 / t as f64)
            } else {
                None
            }
        })
    });
    tracing::info!(
        "OPDS-PSE 保存进度 [{ns}] {}：章 {index} pos {pos}",
        book.name
    );
    Ok(json!({
        "isSuccess": true,
        "bookUrl": book.book_url,
        "bookId": book_id,
        "title": book.name,
        "chapterIndex": index,
        "chapterTitle": title,
        "position": pos,
        "progress": progress,
        "total": total,
        "timestamp": time,
    }))
}

// ---------------- 测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BookGroup;

    /// 测试自引用 base（GAP 52：链接均为绝对 URL）
    const TEST_BASE: &str = "http://reader.example.com";

    async fn test_state(tag: &str) -> (Storage, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("reader-opds-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = crate::storage::init(&config).await.unwrap();
        (storage, dir)
    }

    async fn cleanup(storage: Storage, dir: std::path::PathBuf) {
        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn book(url: &str, name: &str, author: &str) -> Book {
        Book {
            book_url: url.into(),
            name: name.into(),
            author: author.into(),
            created_at: 1_700_000_000_000,
            ..Default::default()
        }
    }

    async fn seed(storage: &Storage, ns: &str, books: Vec<Book>) {
        for b in books {
            storage.upsert_book(ns, &b).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_encode_decode_roundtrip() {
        let url = "https://example.com/书/目录?x=1&y=2/卷";
        assert_eq!(decode_id(&encode_id(url)), url);
        assert_eq!(decode_id("!!!not-base64!!!"), "");
    }

    #[tokio::test]
    async fn test_abs_href_and_opds_base() {
        // abs_href：/ 开头拼接；? 查询拼接；http 原样；base 尾斜杠容忍
        assert_eq!(
            abs_href("http://a.com", "/opds/shelf"),
            "http://a.com/opds/shelf"
        );
        assert_eq!(
            abs_href("http://a.com/", "/opds/shelf"),
            "http://a.com/opds/shelf"
        );
        assert_eq!(
            abs_href("http://a.com/opds", "?startIndex=1&maxItems=50"),
            "http://a.com/opds?startIndex=1&maxItems=50"
        );
        assert_eq!(
            abs_href("http://a.com", "https://cdn.com/c.jpg"),
            "https://cdn.com/c.jpg"
        );
        // opds_base：Host 头优先（默认 http）
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            axum::http::header::HOST,
            "reader.example.com".parse().unwrap(),
        );
        assert_eq!(opds_base(&h, 8080), "http://reader.example.com");
        // X-Forwarded-Proto: https 切换 scheme
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        assert_eq!(opds_base(&h, 8080), "https://reader.example.com");
        // Host 缺失 → localhost:{port} 默认
        assert_eq!(
            opds_base(&axum::http::HeaderMap::new(), 8080),
            "http://localhost:8080"
        );
    }

    #[tokio::test]
    async fn test_root_navigation_structure() {
        let (storage, dir) = test_state("root").await;
        let ns = "default";
        seed(
            &storage,
            ns,
            vec![
                book("https://a.com/1", "书一", "作者甲"),
                book("local://1111", "本地书", "作者乙"),
            ],
        )
        .await;
        let xml = root(&storage, ns, TEST_BASE).await.unwrap();
        // 命名空间 + 导航类型
        assert!(xml.contains("xmlns:opds=\"http://opds-spec.org/2010/catalog\""));
        assert!(xml.contains("kind=navigation"));
        // 导航入口（绝对 URL）
        for href in [
            "/opds/shelf",
            "/opds/recent",
            "/opds/groups",
            "/opds/local",
            "/opds/source",
        ] {
            assert!(
                xml.contains(&format!("href=\"{TEST_BASE}{href}\"")),
                "缺少导航入口 {href}"
            );
        }
        // OpenSearch 链接 + 搜索模板（绝对 URL）
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/opensearch.xml\"")));
        assert!(xml.contains("type=\"application/opensearchdescription+xml\""));
        assert!(xml.contains(&format!(
            "href=\"{TEST_BASE}/opds/search?q={{searchTerms}}\""
        )));
        // self / start 绝对
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds\"")));
        // 无书籍条目（导航 feed 不应出现 acquisition entry）
        assert!(!xml.contains("<link rel=\"http://opds-spec.org/acquisition\""));
        // 不应残留相对链接（除 xmlns/type 外）
        assert!(!xml.contains("href=\"/opds"), "链接应全部绝对化");
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_shelf_entries_structure() {
        let (storage, dir) = test_state("shelf").await;
        let ns = "default";
        let mut b = book("https://a.com/1", "三体 & 黑暗森林", "刘慈欣");
        b.cover_url = Some("/assets/default/covers/1.jpg".into());
        b.language = Some("zh".into());
        b.published_at = Some("2008-01-01".into());
        b.publisher = Some("重庆出版社".into());
        b.kind = Some("科幻".into());
        b.intro = Some("文化大革命后期 <地球往事>".into());
        b.dur_chapter_index = 3;
        b.dur_chapter_title = Some("第三章".into());
        b.dur_chapter_time = 1_700_100_000_000;
        b.latest_chapter_time = 1_700_100_000_000;
        seed(&storage, ns, vec![b]).await;

        let xml = shelf(&storage, ns, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>1</opds:totalResults>"));
        assert!(xml.contains("<opds:startIndex>0</opds:startIndex>"));
        assert!(xml.contains("<opds:itemsPerPage>50</opds:itemsPerPage>"));
        // id/title 转义
        assert!(xml.contains("<id>urn:uuid:"));
        assert!(xml.contains("三体 &amp; 黑暗森林"));
        // author / content 摘要 / updated
        assert!(xml.contains("<author><name>刘慈欣</name></author>"));
        assert!(xml.contains("文化大革命后期 &lt;地球往事&gt;"));
        assert!(xml.contains("<updated>2023-11-16T"));
        // 封面缩略图（相对路径 → 绝对 URL）
        assert!(xml.contains("rel=\"http://opds-spec.org/cover/thumbnail\""));
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/assets/default/covers/1.jpg\"")));
        // acquisition：获取 + 下载 TXT（绝对 URL）
        let id = encode_id("https://a.com/1");
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/acquire/{id}\"")));
        assert!(xml.contains(&format!(
            "href=\"{TEST_BASE}/opds/download/{id}?format=txt\""
        )));
        // OPDS-PSE 链接
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/save/{id}\"")));
        assert!(xml.contains("rel=\"partial-save\""));
        // 元数据
        assert!(xml.contains("<dc:language>zh</dc:language>"));
        assert!(xml.contains("<dcterms:published>2008-01-01</dcterms:published>"));
        assert!(xml.contains("<dcterms:publisher>重庆出版社</dcterms:publisher>"));
        assert!(xml.contains("<category term=\"科幻\" label=\"科幻\"/>"));
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_pagination_50_per_page() {
        let (storage, dir) = test_state("page").await;
        let ns = "default";
        let books: Vec<Book> = (0..120)
            .map(|i| book(&format!("https://a.com/{i}"), &format!("书{i}"), "作者"))
            .collect();
        seed(&storage, ns, books).await;

        // 第一页：50 条 + next（绝对 URL）
        let xml = shelf(&storage, ns, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>120</opds:totalResults>"));
        assert_eq!(xml.matches("  <entry>\n").count(), 50);
        assert!(xml.contains(&format!(
            "href=\"{TEST_BASE}/opds/shelf?startIndex=50&amp;maxItems=50\""
        )));
        assert!(xml.contains("rel=\"first\""));
        assert!(!xml.contains("rel=\"previous\""));

        // 第二页：50 条，prev + next
        let xml = shelf(&storage, ns, 50, 50, TEST_BASE).await.unwrap();
        assert_eq!(xml.matches("  <entry>\n").count(), 50);
        assert!(xml.contains("rel=\"previous\""));
        assert!(xml.contains("rel=\"next\""));

        // 第三页：20 条，无 next
        let xml = shelf(&storage, ns, 100, 50, TEST_BASE).await.unwrap();
        assert_eq!(xml.matches("  <entry>\n").count(), 20);
        assert!(xml.contains("rel=\"previous\""));
        assert!(!xml.contains("rel=\"next\""));

        // 越界页：0 条
        let xml = shelf(&storage, ns, 500, 50, TEST_BASE).await.unwrap();
        assert_eq!(xml.matches("  <entry>\n").count(), 0);
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_search_by_name_and_author() {
        let (storage, dir) = test_state("search").await;
        let ns = "default";
        seed(
            &storage,
            ns,
            vec![
                book("https://a.com/1", "哈利波特与魔法石", "J.K. Rowling"),
                book("https://a.com/2", "三体", "刘慈欣"),
                book("https://a.com/3", "乡村教师", "刘慈欣"),
            ],
        )
        .await;
        // 搜书名
        let xml = search(&storage, ns, "哈利", 0, 50, TEST_BASE)
            .await
            .unwrap();
        assert!(xml.contains("<opds:totalResults>1</opds:totalResults>"));
        assert!(xml.contains("哈利波特与魔法石"));
        assert!(!xml.contains("三体"));
        // 搜作者（ASCII 大小写不敏感）
        let xml = search(&storage, ns, "j.k. rowling", 0, 50, TEST_BASE)
            .await
            .unwrap();
        assert!(xml.contains("<opds:totalResults>1</opds:totalResults>"));
        assert!(xml.contains("哈利波特与魔法石"));
        // 搜中文作者：命中两本
        let xml = search(&storage, ns, "刘慈欣", 0, 50, TEST_BASE)
            .await
            .unwrap();
        assert!(xml.contains("<opds:totalResults>2</opds:totalResults>"));
        assert!(xml.contains("三体"));
        assert!(xml.contains("乡村教师"));
        // 无结果
        let xml = search(&storage, ns, "不存在", 0, 50, TEST_BASE)
            .await
            .unwrap();
        assert!(xml.contains("<opds:totalResults>0</opds:totalResults>"));
        assert!(!xml.contains("<entry>"));
        // 搜索分页链接带 q（绝对 URL）
        let xml = search(&storage, ns, "刘慈欣", 0, 1, TEST_BASE)
            .await
            .unwrap();
        assert!(xml.contains(&format!(
            "{TEST_BASE}/opds/search?startIndex=1&amp;maxItems=1&amp;q=%E5%88%98%E6%85%88%E6%AC%A3"
        )));
        // 特殊字符 q：feed title / 分页链接均需 XML 转义
        let xml = search(&storage, ns, "<a&b>", 0, 50, TEST_BASE)
            .await
            .unwrap();
        assert!(xml.contains("&lt;a&amp;b&gt;"));
        assert!(!xml.contains("<title>搜索：<a"));
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_groups_feed_and_group_feed() {
        let (storage, dir) = test_state("group").await;
        let ns = "default";
        let g1 = storage
            .save_book_group(
                ns,
                &BookGroup {
                    name: "玄幻".into(),
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let g2 = storage
            .save_book_group(
                ns,
                &BookGroup {
                    name: "言情".into(),
                    order: 2,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut b1 = book("https://a.com/1", "斗破苍穹", "天蚕土豆");
        b1.group = g1.id;
        let mut b2 = book("https://a.com/2", "何以笙箫默", "顾漫");
        b2.group = g2.id;
        seed(&storage, ns, vec![b1, b2]).await;

        // 分组导航（绝对 URL）
        let xml = groups(&storage, ns, TEST_BASE).await.unwrap();
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/group/{}\"", g1.id)));
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/group/{}\"", g2.id)));
        assert!(xml.contains("玄幻"));
        assert!(xml.contains("言情"));

        // 分组 feed：只含本组书
        let xml = group(&storage, ns, g1.id, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>1</opds:totalResults>"));
        assert!(xml.contains("斗破苍穹"));
        assert!(!xml.contains("何以笙箫默"));

        // 未知分组：0 条
        let xml = group(&storage, ns, 9999, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>0</opds:totalResults>"));
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_local_and_source_feeds() {
        let (storage, dir) = test_state("local").await;
        let ns = "default";
        let mut s1 = book("https://s1.com/book1", "网络书A", "丙");
        s1.origin = "https://s1.com".into();
        s1.origin_name = "源一".into();
        let mut s2 = book("https://s2.com/book2", "网络书B", "丁");
        s2.origin = "https://s2.com".into();
        s2.origin_name = "源二".into();
        seed(
            &storage,
            ns,
            vec![
                book("local://aaaa", "本地TXT", "甲"),
                book("storage/books/old.txt", "旧本地书", "乙"),
                s1,
                s2,
            ],
        )
        .await;
        // 本地书 feed
        let xml = local(&storage, ns, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>2</opds:totalResults>"));
        assert!(xml.contains("本地TXT"));
        assert!(xml.contains("旧本地书"));
        assert!(!xml.contains("网络书A"));
        // 来源导航（按 origin_name 分组）
        let xml = sources(&storage, ns, TEST_BASE).await.unwrap();
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/source/\"")));
        // 来源 feed（key = origin_name）
        let b64 = encode_id("源一");
        let xml = source(&storage, ns, &b64, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>1</opds:totalResults>"));
        assert!(xml.contains("网络书A"));
        assert!(!xml.contains("网络书B"));
        // 来源 feed（无 origin_name 时回退 origin）
        let b64 = encode_id("https://s2.com");
        let xml = source(&storage, ns, &b64, 0, 50, TEST_BASE).await.unwrap();
        assert!(xml.contains("<opds:totalResults>0</opds:totalResults>"));
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_open_search_description() {
        let xml = open_search_xml(TEST_BASE);
        assert!(
            xml.contains("<OpenSearchDescription xmlns=\"http://a9.com/-/spec/opensearch/1.1/\">")
        );
        assert!(xml.contains("<ShortName>reader-dev</ShortName>"));
        assert!(xml.contains(&format!(
            "template=\"{TEST_BASE}/opds/search?q={{searchTerms}}\""
        )));
        assert!(xml.contains(&format!(
            "template=\"{TEST_BASE}/opds/catalog/search?q={{searchTerms}}\""
        )));
    }

    #[tokio::test]
    async fn test_catalog2_root_json() {
        let (storage, dir) = test_state("c2root").await;
        let ns = "default";
        let g = storage
            .save_book_group(
                ns,
                &BookGroup {
                    name: "玄幻".into(),
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut b = book("https://a.com/1", "斗破苍穹", "天蚕土豆");
        b.group = g.id;
        b.cover_url = Some("/assets/c.jpg".into());
        seed(&storage, ns, vec![b]).await;

        let json_str = catalog_json(&storage, ns, TEST_BASE).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["metadata"]["title"], "reader-dev");
        assert_eq!(v["metadata"]["numberOfItems"], 1);
        // navigation 5 项（绝对 URL）
        let nav = v["navigation"].as_array().unwrap();
        assert_eq!(nav.len(), 5);
        assert_eq!(nav[0]["metadata"]["title"], "书架");
        assert_eq!(
            nav[0]["links"][0]["href"],
            format!("{TEST_BASE}/opds/catalog/shelf")
        );
        // facets：分组 + 来源
        let facets = v["facets"].as_array().unwrap();
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0]["metadata"]["title"], "分组");
        assert_eq!(
            facets[0]["links"][0]["href"],
            format!("{TEST_BASE}/opds/catalog/group/{}", g.id)
        );
        // groups：numberOfItems
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["metadata"]["numberOfItems"], 1);
        // links：opensearch + search 模板 + self（绝对 URL）
        let links = v["links"].as_array().unwrap();
        assert!(links
            .iter()
            .any(|l| l["href"] == format!("{TEST_BASE}/opds/opensearch.xml")));
        assert!(links
            .iter()
            .any(|l| l["href"] == format!("{TEST_BASE}/opds/catalog/search?q={{searchTerms}}")));
        assert!(links
            .iter()
            .any(|l| l["rel"] == "self" && l["href"] == format!("{TEST_BASE}/opds/catalog")));
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_catalog2_shelf_json() {
        let (storage, dir) = test_state("c2shelf").await;
        let ns = "default";
        let books: Vec<Book> = (0..120)
            .map(|i| book(&format!("https://a.com/{i}"), &format!("书{i}"), "作者"))
            .collect();
        seed(&storage, ns, books).await;

        let json_str = shelf_json(&storage, ns, 0, 50, TEST_BASE).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["metadata"]["numberOfItems"], 120);
        assert_eq!(v["metadata"]["title"], "书架");
        let pubs = v["publications"].as_array().unwrap();
        assert_eq!(pubs.len(), 50);
        // 排序为 dur_chapter_time DESC, rowid DESC → 最后插入的书在最前
        let p0 = &pubs[0];
        assert!(p0["metadata"]["title"].as_str().unwrap().starts_with("书"));
        assert!(p0["metadata"]["identifier"]
            .as_str()
            .unwrap()
            .starts_with("urn:uuid:"));
        assert_eq!(p0["metadata"]["authors"][0]["name"], "作者");
        // links：acquisition（获取/下载，绝对 URL）+ partial-save
        let links = p0["links"].as_array().unwrap();
        assert!(links
            .iter()
            .any(|l| l["rel"] == "http://opds-spec.org/acquisition"
                && l["type"] == "text/plain"
                && l["href"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{TEST_BASE}/opds/acquire/"))));
        assert!(links.iter().any(|l| l["rel"] == "partial-save"
            && l["href"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{TEST_BASE}/opds-save?bookId="))));
        // images 数组（无封面书为空）
        assert!(p0["images"].is_array());
        // 分页 next link（绝对 URL）
        let feed_links = v["links"].as_array().unwrap();
        assert!(feed_links.iter().any(|l| l["rel"] == "next"
            && l["href"] == format!("{TEST_BASE}/opds/catalog/shelf?startIndex=50&maxItems=50")));
        assert!(feed_links
            .iter()
            .any(|l| l["rel"] == "self" && l["href"] == format!("{TEST_BASE}/opds/catalog/shelf")));
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_acquire_local_first_chapter() {
        let (storage, dir) = test_state("acquire").await;
        let ns = "default";
        // local:// 书：章节表
        let info = crate::model::book_chapter::BookInfo {
            book_url: "local://bbbb".into(),
            name: "本地书".into(),
            author: "作者".into(),
            origin: "local".into(),
            origin_name: "本地书".into(),
            ..Default::default()
        };
        let imported = ImportedBook {
            meta: Default::default(),
            chapters: vec![
                crate::service::local_book::Chapter {
                    title: "第一章".into(),
                    content: "第一章正文".into(),
                },
                crate::service::local_book::Chapter {
                    title: "第二章".into(),
                    content: "第二章正文".into(),
                },
            ],
            cover: None,
            format: "txt".into(),
        };
        storage.save_local_book(ns, &info, &imported).await.unwrap();

        let (fname, bytes) = acquire(&storage, ns, &encode_id("local://bbbb"))
            .await
            .unwrap();
        assert_eq!(fname, "本地书.txt");
        assert_eq!(String::from_utf8(bytes).unwrap(), "第一章正文");
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_download_local_txt_concat_and_original_file() {
        let (storage, dir) = test_state("dl").await;
        let ns = "default";
        let info = crate::model::book_chapter::BookInfo {
            book_url: "local://cccc".into(),
            name: "本地书".into(),
            author: "作者".into(),
            origin: "local".into(),
            origin_name: "本地书".into(),
            ..Default::default()
        };
        let imported = ImportedBook {
            meta: Default::default(),
            chapters: vec![
                crate::service::local_book::Chapter {
                    title: "第一章".into(),
                    content: "正文一".into(),
                },
                crate::service::local_book::Chapter {
                    title: "第二章".into(),
                    content: "正文二".into(),
                },
            ],
            cover: None,
            format: "txt".into(),
        };
        storage.save_local_book(ns, &info, &imported).await.unwrap();

        // 无原文件 → 章节拼接
        let (name, bytes, ct) = download(&storage, ns, &encode_id("local://cccc"), "txt", None)
            .await
            .unwrap();
        assert_eq!(name, "本地书.txt");
        assert_eq!(ct, "text/plain; charset=utf-8");
        let txt = String::from_utf8(bytes).unwrap();
        assert!(txt.contains("正文一") && txt.contains("正文二"));

        // 落盘原文件（模拟上传时保存）→ 原样返回
        let opds_dir = storage
            .config
            .storage_dir()
            .join("data")
            .join(ns)
            .join("opds_files");
        std::fs::create_dir_all(&opds_dir).unwrap();
        std::fs::write(opds_dir.join("cccc.txt"), "原始TXT内容").unwrap();
        let (name, bytes, ct) = download(&storage, ns, &encode_id("local://cccc"), "txt", None)
            .await
            .unwrap();
        assert_eq!(name, "cccc.txt");
        assert_eq!(ct, "text/plain; charset=utf-8");
        assert_eq!(String::from_utf8(bytes).unwrap(), "原始TXT内容");

        // EPUB 原文件：application/epub+zip
        let epub_bytes = b"PK\x03\x04fake-epub".to_vec();
        std::fs::write(opds_dir.join("cccc.epub"), &epub_bytes).unwrap();
        let (_, bytes, ct) = download(&storage, ns, &encode_id("local://cccc"), "epub", None)
            .await
            .unwrap();
        assert_eq!(ct, "application/epub+zip");
        assert_eq!(bytes, epub_bytes);
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_pse_get_and_post() {
        let (storage, dir) = test_state("pse").await;
        let ns = "default";
        let mut b = book("https://a.com/1", "三体", "刘慈欣");
        b.dur_chapter_index = 2;
        b.dur_chapter_title = Some("第二章".into());
        b.dur_chapter_pos = 500;
        b.dur_chapter_time = 1_700_100_000_000;
        seed(&storage, ns, vec![b]).await;
        let id = encode_id("https://a.com/1");

        // GET：Atom entry 结构（partial-save 链接绝对 URL）
        let xml = save_entry_xml(&storage, ns, &id, TEST_BASE).await.unwrap();
        assert!(xml.contains("<entry xmlns=\"http://www.w3.org/2005/Atom\""));
        assert!(xml.contains(&format!("<id>urn:uuid:{id}</id>")));
        assert!(xml.contains(&format!("href=\"{TEST_BASE}/opds/save/{id}\"")));
        assert!(xml.contains("rel=\"partial-save\""));
        assert!(xml.contains("type=\"application/atom+xml;type=entry;profile=opds-save\""));
        assert!(xml.contains("<content type=\"application/json\">"));
        // content 内 JSON 被 XML 转义（" → &quot;）
        assert!(xml.contains("&quot;chapterIndex&quot;:2"));
        assert!(xml.contains("&quot;position&quot;:500"));

        // GET：JSON 格式
        let j = save_entry_json(&storage, ns, &id).await.unwrap();
        assert_eq!(j["chapterIndex"], 2);
        assert_eq!(j["chapterTitle"], "第二章");
        assert_eq!(j["timestamp"], 1_700_100_000_000i64);

        // POST：写 dur_chapter_*
        let resp = apply_save(
            &storage,
            ns,
            &id,
            Some(0.35),
            Some(700),
            Some(2000),
            Some(3),
            Some("第三章".into()),
            Some(1_700_200_000_000),
        )
        .await
        .unwrap();
        assert_eq!(resp["isSuccess"], true);
        assert_eq!(resp["progress"], 0.35);
        let book = storage
            .find_book(ns, "https://a.com/1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(book.dur_chapter_index, 3);
        assert_eq!(book.dur_chapter_title.as_deref(), Some("第三章"));
        assert_eq!(book.dur_chapter_pos, 700);
        assert_eq!(book.dur_chapter_time, 1_700_200_000_000);

        // 仅 position/total → 推算 progress
        let resp = apply_save(
            &storage,
            ns,
            &id,
            None,
            Some(1000),
            Some(4000),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!((resp["progress"].as_f64().unwrap() - 0.25).abs() < 1e-9);

        // 不存在书籍 → 错误
        assert!(apply_save(
            &storage,
            ns,
            &encode_id("https://nope.com"),
            None,
            None,
            None,
            None,
            None,
            None
        )
        .await
        .is_err());
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn test_system_settings_roundtrip() {
        let (storage, dir) = test_state("settings").await;
        assert!(storage
            .get_system_setting("opds_username")
            .await
            .unwrap()
            .is_none());
        storage
            .set_system_setting("opds_username", "reader")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_system_setting("opds_username")
                .await
                .unwrap()
                .as_deref(),
            Some("reader")
        );
        storage
            .delete_system_setting("opds_username")
            .await
            .unwrap();
        assert!(storage
            .get_system_setting("opds_username")
            .await
            .unwrap()
            .is_none());
        // OPDS 账号
        let stored = crate::util::sha256::store_password("secret");
        storage.set_opds_account("opds", &stored).await.unwrap();
        let (u, p) = storage.get_opds_account().await.unwrap().unwrap();
        assert_eq!(u, "opds");
        assert!(crate::util::sha256::verify_password("secret", &p));
        storage.clear_opds_account().await.unwrap();
        assert!(storage.get_opds_account().await.unwrap().is_none());
        cleanup(storage, dir).await;
    }
}
