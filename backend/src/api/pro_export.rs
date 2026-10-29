//! Pro 独有接口补齐（audit #13 契约对齐）：
//!
//! - `GET/POST /reader3/getAllContents`：整本缓存只读组装器（零网络，缺章占位）
//! - `GET/POST /reader3/searchChapter`：章节标题搜索（contains，{list,lastIndex} 分页）
//! - `GET/POST /reader3/exportToTxt`：服务端 TXT 导出下载（charset 参数支持 GBK 等）
//! - `GET/POST /reader3/exportToEpub`：服务端 EPUB 导出下载
//!
//! 数据源均为 book_chapters 表（目录行 content 为空串——getAllContents 按契约写
//! 「暂无缓存内容。」占位；导出时跳过空章）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::api::router::{param_of, resolve_namespace, AppState, ReturnData};

// ---------------------------------------------------------------- 导出顺序（阅读序）

/// 目录条目（`toc_cache.chapters_json` 的元素，只留排阅读序要用的两个字段）
#[derive(Debug, Clone, PartialEq, Eq)]
struct TocEntry {
    /// 目录标题（阅读序 = 它在目录数组里的下标）
    title: String,
    /// `md5(chapterUrl)`——正好等于书源书 `book_chapters.chapter_index`
    hash: Option<i64>,
}

/// 目录缓存「多旧都能用」：导出只借它的**顺序**，正文另有出处，故取 10 年 ≈ 永不过期
/// （避免 `now - max_age` 溢出：10 年毫秒数远小于 i64::MAX）。
const TOC_HINT_MAX_AGE_MS: i64 = 10 * 365 * 24 * 3600 * 1000;

/// 解析 `toc_cache.chapters_json`（坏 JSON / 坏元素一律跳过，绝不 panic——缓存是好几年攒的）
fn parse_toc_entries(json: &str) -> Vec<TocEntry> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim().to_string();
            if title.is_empty() {
                return None;
            }
            let hash = item
                .get("url")
                .and_then(|v| v.as_str())
                .and_then(toc_entry_hash);
            Some(TocEntry { title, hash })
        })
        .collect()
}

/// 目录条目的章键：离线兜底合成的目录是 `cache://<bookUrl>#<md5>`（尾巴就是 chapter_index），
/// 源站目录是真实章节 URL（重算 `chapter_url_hash` 即可与 chapter_index 对齐）。
fn toc_entry_hash(url: &str) -> Option<i64> {
    if let Some((_, tail)) = url.rsplit_once('#') {
        if let Ok(h) = tail.trim().parse::<i64>() {
            return Some(h);
        }
    }
    (!url.trim().is_empty()).then(|| crate::util::md5::chapter_url_hash(url))
}

/// 标题里的章号：「第12章 xxx」→ 12，「第十二章 xxx」→ 12；没有 → None。
/// 只认「第」开头紧跟的数字（阿拉伯或中文），其它一律不猜——宁可退回原序也不错排。
fn chapter_num_of_title(title: &str) -> Option<i64> {
    let rest = title.trim_start().strip_prefix('第')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        return digits.parse().ok();
    }
    chinese_number(rest)
}

/// 解析中文整数（只覆盖章号会出现的写法：零一二三四五六七八九 + 十百千，含「两」）。
/// 遇到不认识的字符即停；一个数字都没读到返回 None。
fn chinese_number(text: &str) -> Option<i64> {
    fn digit_of(ch: char) -> Option<i64> {
        match ch {
            '零' => Some(0),
            '一' => Some(1),
            '二' | '两' => Some(2),
            '三' => Some(3),
            '四' => Some(4),
            '五' => Some(5),
            '六' => Some(6),
            '七' => Some(7),
            '八' => Some(8),
            '九' => Some(9),
            _ => None,
        }
    }
    let mut total = 0i64;
    let mut cur: Option<i64> = None;
    let mut seen = false;
    for ch in text.chars() {
        let unit = match ch {
            '十' => 10,
            '百' => 100,
            '千' => 1000,
            _ => match digit_of(ch) {
                Some(d) => {
                    cur = Some(d);
                    seen = true;
                    continue;
                }
                None => break,
            },
        };
        // 「十二」= 12：单位前没写数字时按 1 计
        total += cur.take().unwrap_or(1) * unit;
        seen = true;
    }
    seen.then(|| total + cur.unwrap_or(0))
}

/// 把 `list_cached_chapters` 的行重排成**阅读序**（纯函数，无 IO，不改行内容）。
///
/// 为什么需要：书源书的 `chapter_index` 是 `md5(chapterUrl)`（[`crate::util::md5::chapter_url_hash`]），
/// `list_cached_chapters` 的 `ORDER BY chapter_index` 给的是**哈希序**——直接拿去拼 TXT/EPUB
/// 就是乱序的书。
///
/// 排序键优先级（与 `service::graph::plan` 排章序的口径一致）：
/// 1. 章键（hash）命中目录 → 用目录下标；2. 标题命中目录 → 用目录下标；
/// 3. 标题自带章号 → 按章号升序；4. 都对不上 → 保持入参原序垫底（本地书 `chapter_index`
///    本身就是阅读序）。
///
/// 注意：目录只覆盖部分已缓存章时，「命中目录的」会整体排在「只有章号的」之前（同一份
/// 目录内它们的相对顺序仍正确）；这是 3/4 两档缺乏统一尺度的必然取舍。
fn order_chapters_for_export(
    rows: Vec<(i64, String, String)>,
    toc_order_hint: &[TocEntry],
) -> Vec<(i64, String, String)> {
    if rows.len() < 2 {
        return rows;
    }
    let mut by_hash: HashMap<i64, usize> = HashMap::with_capacity(toc_order_hint.len());
    let mut by_title: HashMap<&str, usize> = HashMap::with_capacity(toc_order_hint.len());
    for (pos, entry) in toc_order_hint.iter().enumerate() {
        if let Some(hash) = entry.hash {
            by_hash.entry(hash).or_insert(pos); // 同 key 取目录里第一条（「先命中者胜」）
        }
        by_title.entry(entry.title.as_str()).or_insert(pos);
    }
    // (rank, pos, 原下标, 行) —— 原下标兜底保证同键时严格稳定
    type Keyed = (u8, i64, usize, (i64, String, String));
    let mut keyed: Vec<Keyed> = rows
        .into_iter()
        .enumerate()
        .map(|(orig, row)| {
            let toc_pos = by_hash
                .get(&row.0)
                .copied()
                .or_else(|| by_title.get(row.1.trim()).copied());
            let (rank, pos) = match toc_pos {
                Some(p) => (0u8, p as i64),
                None => match chapter_num_of_title(&row.1) {
                    Some(n) => (1u8, n),
                    None => (2u8, i64::MAX),
                },
            };
            (rank, pos, orig, row)
        })
        .collect();
    // 末位用原下标：同 rank/pos 时严格稳定（不依赖 sort 的稳定性实现）
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    keyed.into_iter().map(|(_, _, _, row)| row).collect()
}

/// 取本书目录顺序提示（`toc_cache` → [`TocEntry`] 列表）；取不到返回空表。
///
/// 目录缓存的键既可能是 tocUrl，也可能直接是 bookUrl（离线兜底合成目录写的就是 bookUrl，
/// 见 router 的 `synthesize_toc_from_cache` 落库处）——两个候选都试，任一非空即用。
async fn toc_order_hint(
    state: &AppState,
    ns: &str,
    book_url: &str,
    book: Option<&crate::model::book::Book>,
) -> Vec<TocEntry> {
    let mut candidates: Vec<&str> = Vec::with_capacity(2);
    if let Some(toc_url) = book.map(|b| b.toc_url.as_str()) {
        if !toc_url.trim().is_empty() {
            candidates.push(toc_url);
        }
    }
    candidates.push(book_url);
    for candidate in candidates {
        if let Ok(Some(json)) = state
            .storage
            .get_toc_cache(ns, candidate, TOC_HINT_MAX_AGE_MS)
            .await
        {
            let entries = parse_toc_entries(&json);
            if !entries.is_empty() {
                return entries;
            }
        }
    }
    Vec::new()
}

/// GET/POST /reader3/getAllContents：纯缓存只读组装器，零网络。
/// 响应 data：{name, author, total, cachedCount, chapters: [{chapterIndex,title,content}]}
pub async fn get_all_contents(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    if book_url.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let book = state
        .storage
        .find_book(&namespace, &book_url)
        .await
        .ok()
        .flatten();
    match state
        .storage
        .list_cached_chapters(&namespace, &book_url)
        .await
    {
        Ok(rows) => {
            const PLACEHOLDER: &str = "暂无缓存内容。";
            // 契约按阅读序给章节（rows 来自 ORDER BY chapter_index = 哈希序）
            let hint = toc_order_hint(&state, &namespace, &book_url, book.as_ref()).await;
            let rows = order_chapters_for_export(rows, &hint);
            let chapters: Vec<serde_json::Value> = rows
                .iter()
                .map(|(idx, title, content)| {
                    json!({
                        "chapterIndex": idx,
                        "title": title,
                        // Pro 语义：未缓存章写占位符
                        "content": if content.trim().is_empty() { PLACEHOLDER } else { content },
                    })
                })
                .collect();
            let cached = rows.iter().filter(|(_, _, c)| !c.trim().is_empty()).count();
            Json(ReturnData::ok(json!({
                "name": book.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
                "author": book.as_ref().map(|b| b.author.clone()).unwrap_or_default(),
                "total": chapters.len(),
                "cachedCount": cached,
                "chapters": chapters,
            })))
        }
        Err(e) => {
            tracing::error!("getAllContents [{namespace}/{book_url}] 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

/// GET/POST /reader3/searchChapter：章节标题 contains 匹配（大小写敏感，对齐 Pro
/// indexOf 语义）。分页：fromIndex 起始扫描位置、pageSize 单页条数（默认 100 上限 500）。
/// 响应 data：{list:[{chapterIndex,title}], lastIndex}——还有更多时 lastIndex=下一扫描
/// 起点（最后命中 index+1），扫尽为 -1。
pub async fn search_chapter(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let key = param_of(&params, body_json.as_ref(), "key");
    if key.is_empty() {
        return Json(ReturnData::err("请输入搜索关键字"));
    }
    let book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    if book_url.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let from_index: i64 = param_of(&params, body_json.as_ref(), "fromIndex")
        .parse()
        .unwrap_or(0);
    let page_size: usize = param_of(&params, body_json.as_ref(), "pageSize")
        .parse::<usize>()
        .unwrap_or(100)
        .clamp(1, 500);

    match state
        .storage
        .list_cached_chapters(&namespace, &book_url)
        .await
    {
        Ok(rows) => {
            // 搜索结果同样按阅读序返回（rows 原生是哈希序）——否则上下页顺序跳来跳去
            let book = state
                .storage
                .find_book(&namespace, &book_url)
                .await
                .ok()
                .flatten();
            let hint = toc_order_hint(&state, &namespace, &book_url, book.as_ref()).await;
            let rows = order_chapters_for_export(rows, &hint);
            let hits: Vec<(i64, String)> = rows
                .iter()
                .filter(|(idx, title, _)| *idx >= from_index && title.contains(key.as_str()))
                .map(|(idx, title, _)| (*idx, title.clone()))
                .collect();
            let has_more = hits.len() > page_size;
            let page: Vec<serde_json::Value> = hits
                .iter()
                .take(page_size)
                .map(|(idx, title)| json!({"chapterIndex": idx, "title": title}))
                .collect();
            let last_index = if has_more {
                page.last()
                    .and_then(|v| v.get("chapterIndex").and_then(|x| x.as_i64()))
                    .map(|i| i + 1)
                    .unwrap_or(-1)
            } else {
                -1
            };
            Json(ReturnData::ok(
                json!({ "list": page, "lastIndex": last_index }),
            ))
        }
        Err(e) => {
            tracing::error!("searchChapter [{namespace}/{book_url}] 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

/// 共享：取书 + 已缓存正文 → 导出章节列表（空 content 跳过——目录行不导出）
async fn export_chapters(
    state: &AppState,
    ns: &str,
    book_url: &str,
) -> anyhow::Result<(
    Option<crate::model::book::Book>,
    Vec<crate::service::export_book::ExportChapter>,
)> {
    let book = state.storage.find_book(ns, book_url).await.ok().flatten();
    let rows = state.storage.list_cached_chapters(ns, book_url).await?;
    // TXT/EPUB 必须按阅读序拼（rows 原生是哈希序，见 order_chapters_for_export）
    let hint = toc_order_hint(state, ns, book_url, book.as_ref()).await;
    let chapters = order_chapters_for_export(rows, &hint)
        .into_iter()
        .filter(|(_, _, c)| !c.trim().is_empty())
        .map(|(_, title, content)| crate::service::export_book::ExportChapter { title, content })
        .collect();
    Ok((book, chapters))
}

fn export_common_params(
    params: &HashMap<String, String>,
    body_json: Option<&serde_json::Value>,
) -> (String, String) {
    (
        param_of(params, body_json, "bookUrl"),
        param_of(params, body_json, "charset"),
    )
}

/// GET/POST /reader3/exportToTxt：服务端 TXT 导出下载。charset 参数默认 UTF-8
/// （Pro appConfig.exportCharset 对齐；GBK 等经 encode_txt 转码，不可映射字符报错）。
pub async fn export_to_txt(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let (book_url, charset) = export_common_params(&params, body_json.as_ref());
    if book_url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let (book, chapters) = match export_chapters(&state, &namespace, &book_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("exportToTxt [{namespace}/{book_url}] 失败: {e}");
            return Json(ReturnData::err("系统错误")).into_response();
        }
    };
    let name = book
        .as_ref()
        .map(|b| b.name.clone())
        .unwrap_or_else(|| book_url.clone());
    let txt = crate::service::export_book::build_txt(&name, &chapters);
    let encoding: &str = if charset.is_empty() {
        "utf-8"
    } else {
        &charset
    };
    let (bytes, _) = match crate::service::export_book::encode_txt(&txt, encoding) {
        Ok(v) => v,
        Err(e) => return Json(ReturnData::err(format!("编码不支持: {e}"))).into_response(),
    };
    file_download(bytes, &format!("{name}.txt"), "text/plain; charset=utf-8")
}

/// GET/POST /reader3/exportToEpub：服务端 EPUB 导出下载。元数据 publisher="Legado" /
/// language="zh" 对齐 Pro epublib 产物约定；简介/分类透传书籍信息。
pub async fn export_to_epub(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let (book_url, _charset) = export_common_params(&params, body_json.as_ref());
    if book_url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let (book, chapters) = match export_chapters(&state, &namespace, &book_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("exportToEpub [{namespace}/{book_url}] 失败: {e}");
            return Json(ReturnData::err("系统错误")).into_response();
        }
    };
    let name = book
        .as_ref()
        .map(|b| b.name.clone())
        .unwrap_or_else(|| book_url.clone());
    let author = book.as_ref().map(|b| b.author.clone()).unwrap_or_default();
    let meta = crate::service::export_book::EpubMeta {
        description: book.as_ref().and_then(|b| b.intro.clone()),
        language: Some("zh".into()),
        published_at: None,
        publisher: Some("Legado".into()),
        subject: book.as_ref().and_then(|b| b.kind.clone()),
        ..Default::default()
    };
    let bytes = crate::service::export_book::build_epub_full(&name, &author, &meta, &chapters);
    file_download(bytes, &format!("{name}.epub"), "application/epub+zip")
}

fn file_download(bytes: Vec<u8>, filename: &str, content_type: &str) -> Response {
    // RFC 6266/5987：非 ASCII 文件名走 filename*（URL 编码），ASCII 回退名兜底
    let encoded: String = urlencoding::encode(filename).into_owned();
    let ascii_fallback: String = if filename.is_ascii() {
        filename.replace('"', "'")
    } else {
        "download".to_string()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{encoded}"),
        )
        .body(axum::body::Body::from(bytes))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    async fn test_state(tag: &str) -> (AppState, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-proexport-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        config.token_ttl_days = 0;
        let storage = crate::storage::init(&config).await.unwrap();
        let image_cache = crate::service::image_cache::ImageCache::with_capacity(
            dir.join("storage").join("cache").join("images"),
            1024 * 1024,
        );
        (
            AppState {
                storage,
                image_cache,
            },
            dir,
        )
    }

    fn q(pairs: &[(&str, &str)]) -> Query<HashMap<String, String>> {
        Query(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn empty_headers() -> HeaderMap {
        HeaderMap::new()
    }

    /// 种子：书架书 + 目录（2 章空正文行）+ 缓存第 1 章正文
    async fn seed(state: &AppState) {
        state
            .storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: "https://src/book1".into(),
                    name: "测试书名".into(),
                    author: "作者甲".into(),
                    origin: "https://src".into(),
                    intro: Some("简介文本".into()),
                    kind: Some("玄幻".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // 目录两章（content 空）
        state
            .storage
            .cache_chapter_content("default", "https://src/book1", 0, "第一章", "")
            .await
            .unwrap();
        state
            .storage
            .cache_chapter_content("default", "https://src/book1", 1, "第二章", "")
            .await
            .unwrap();
        // 缓存第 1 章正文
        state
            .storage
            .cache_chapter_content(
                "default",
                "https://src/book1",
                0,
                "第一章",
                "第一章的完整正文。",
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_get_all_contents_placeholder() {
        let (state, _dir) = test_state("allcontents").await;
        seed(&state).await;
        let ret = get_all_contents(
            State(state.clone()),
            q(&[("bookUrl", "https://src/book1")]),
            empty_headers(),
            None,
        )
        .await;
        assert!(ret.is_success, "{:?}", ret.error_msg);
        let data = ret.0.data.clone();
        let chapters = data.get("chapters").and_then(|v| v.as_array()).unwrap();
        assert_eq!(chapters.len(), 2);
        // 第 0 章已缓存 → 正文；第 1 章未缓存 → 占位符
        assert_eq!(
            chapters[0].get("content").and_then(|v| v.as_str()),
            Some("第一章的完整正文。")
        );
        assert_eq!(
            chapters[1].get("content").and_then(|v| v.as_str()),
            Some("暂无缓存内容。")
        );
        assert_eq!(data.get("name").and_then(|v| v.as_str()), Some("测试书名"));
        assert_eq!(data.get("cachedCount").and_then(|v| v.as_i64()), Some(1));
    }

    #[tokio::test]
    async fn test_search_chapter_pagination() {
        let (state, _dir) = test_state("searchchapter").await;
        for i in 0..5 {
            state
                .storage
                .cache_chapter_content("default", "u:b", i, &format!("番外{i}篇"), "x")
                .await
                .unwrap();
        }
        state
            .storage
            .cache_chapter_content("default", "u:b", 10, "无关章节", "y")
            .await
            .unwrap();

        // 首页 pageSize=3 → 3 条 + lastIndex=下一起点
        let ret = search_chapter(
            State(state.clone()),
            q(&[("bookUrl", "u:b"), ("key", "番外"), ("pageSize", "3")]),
            empty_headers(),
            None,
        )
        .await;
        let data = ret.0.data.clone();
        let list = data.get("list").and_then(|v| v.as_array()).unwrap();
        assert_eq!(list.len(), 3);
        let last = data.get("lastIndex").and_then(|v| v.as_i64()).unwrap();
        assert!(last > 0);

        // 从 lastIndex 续拉 → 剩余 2 条 + lastIndex=-1（扫尽）
        let ret2 = search_chapter(
            State(state.clone()),
            q(&[
                ("bookUrl", "u:b"),
                ("key", "番外"),
                ("pageSize", "3"),
                ("fromIndex", &last.to_string()),
            ]),
            empty_headers(),
            None,
        )
        .await;
        let data2 = ret2.0.data.clone();
        let list2 = data2.get("list").and_then(|v| v.as_array()).unwrap();
        assert_eq!(list2.len(), 2);
        assert_eq!(data2.get("lastIndex").and_then(|v| v.as_i64()), Some(-1));

        // 无命中
        let ret3 = search_chapter(
            State(state),
            q(&[("bookUrl", "u:b"), ("key", "不存在")]),
            empty_headers(),
            None,
        )
        .await;
        let data3 = ret3.0.data.clone();
        assert_eq!(
            data3.get("list").and_then(|v| v.as_array()).unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn test_export_txt_utf8_and_gbk() {
        let (state, _dir) = test_state("exptxt").await;
        seed(&state).await;
        // 默认 UTF-8：响应体含中文标题与正文
        let resp = export_to_txt(
            State(state.clone()),
            q(&[("bookUrl", "https://src/book1")]),
            empty_headers(),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let cd = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cd.contains(".txt"));
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("测试书名"));
        assert!(text.contains("第一章的完整正文。"));

        // GBK 转码：可解码且内容一致
        let resp2 = export_to_txt(
            State(state.clone()),
            q(&[("bookUrl", "https://src/book1"), ("charset", "gbk")]),
            empty_headers(),
            None,
        )
        .await;
        let bytes2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
            .await
            .unwrap();
        let (decoded, _, _) = encoding_rs::GBK.decode(&bytes2);
        // 仅已缓存的第一章导出（目录行/未缓存章跳过）
        assert!(decoded.contains("第一章的完整正文。"));
        assert!(!decoded.contains("第二章"));

        // 不支持的编码 → 错误 JSON
        let resp3 = export_to_txt(
            State(state),
            q(&[
                ("bookUrl", "https://src/book1"),
                ("charset", "not-a-charset"),
            ]),
            empty_headers(),
            None,
        )
        .await;
        assert_eq!(resp3.status(), StatusCode::OK); // Json 错误包仍 200（ReturnData 契约）
    }

    #[tokio::test]
    async fn test_export_epub_zip() {
        let (state, _dir) = test_state("expepub").await;
        seed(&state).await;
        let resp = export_to_epub(
            State(state),
            q(&[("bookUrl", "https://src/book1")]),
            empty_headers(),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ct, "application/epub+zip");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        // zip 本地文件头魔数 PK\x03\x04 + mimetype 首条目（Stored 未压缩）
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        // 解压校验：mimetype 内容正确、仅已缓存章导出（目录空行跳过）
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut mimetype = String::new();
        use std::io::Read;
        archive
            .by_name("mimetype")
            .unwrap()
            .read_to_string(&mut mimetype)
            .unwrap();
        assert_eq!(mimetype, "application/epub+zip");
        let mut found_content = false;
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            let mut buf = String::new();
            let _ = f.read_to_string(&mut buf);
            if buf.contains("第一章的完整正文。") {
                found_content = true;
            }
        }
        assert!(found_content, "EPUB 内应含已缓存章正文");
    }

    // ---------------- 导出阅读序（order_chapters_for_export） ----------------

    fn row(idx: i64, title: &str, content: &str) -> (i64, String, String) {
        (idx, title.to_string(), content.to_string())
    }

    fn titles(rows: &[(i64, String, String)]) -> Vec<String> {
        rows.iter().map(|(_, t, _)| t.clone()).collect()
    }

    /// 有目录顺序：按目录下标重排（行原序是 chapter_index 哈希序）
    #[test]
    fn test_order_chapters_for_export_uses_toc_order() {
        // 入参按哈希序：11 → 22 → 33
        let rows = vec![
            row(11, "第一章", "甲"),
            row(22, "第二章", "乙"),
            row(33, "第三章", "丙"),
        ];
        // 目录顺序与哈希序相反
        let hint = vec![
            TocEntry {
                title: "第三章".into(),
                hash: Some(33),
            },
            TocEntry {
                title: "第二章".into(),
                hash: Some(22),
            },
            TocEntry {
                title: "第一章".into(),
                hash: Some(11),
            },
        ];
        let out = order_chapters_for_export(rows.clone(), &hint);
        assert_eq!(titles(&out), vec!["第三章", "第二章", "第一章"]);
        // 行内容原样保留（接口返回结构不变）
        assert_eq!(out[0].2, "丙");
        assert_eq!(out[0].0, 33, "chapterIndex 不该被重编号");

        // 目录条目只有标题（hash 取不到，如 url 是相对路径/空）→ 按标题命中，同样生效
        let by_title_hint = vec![
            TocEntry {
                title: "第二章".into(),
                hash: None,
            },
            TocEntry {
                title: "第一章".into(),
                hash: None,
            },
            TocEntry {
                title: "第三章".into(),
                hash: None,
            },
        ];
        let out = order_chapters_for_export(rows, &by_title_hint);
        assert_eq!(titles(&out), vec!["第二章", "第一章", "第三章"]);
    }

    /// 无目录：按标题里的阿拉伯章号升序，无章号的（卷末总结等）垫尾
    #[test]
    fn test_order_chapters_for_export_falls_back_to_title_number() {
        let rows = vec![
            row(5, "第十章 收束", "甲"),
            row(3, "第二章 启程", "乙"),
            row(9, "卷末总结", "丙"),
            row(7, "第一章 开端", "丁"),
        ];
        let out = order_chapters_for_export(rows, &[]);
        assert_eq!(
            titles(&out),
            vec!["第一章 开端", "第二章 启程", "第十章 收束", "卷末总结"]
        );
    }

    /// 目录与章号都取不到：保持原序（稳定），不 panic
    #[test]
    fn test_order_chapters_for_export_keeps_original_order_without_hints() {
        let rows = vec![
            row(900, "楔子", "甲"),
            row(100, "尾声", "乙"),
            row(500, "番外一", "丙"),
        ];
        let out = order_chapters_for_export(rows.clone(), &[]);
        assert_eq!(titles(&out), titles(&rows), "无任何线索时必须保持入参原序");
        // 目录里只有一条无关标题 → 同样不改变顺序（不 panic、不乱排）
        let unrelated = vec![TocEntry {
            title: "别的书第一章".into(),
            hash: Some(1),
        }];
        let out = order_chapters_for_export(rows.clone(), &unrelated);
        assert_eq!(titles(&out), titles(&rows));
    }

    /// 边界：空表 / 单行 / 目录为空 / 坏 JSON 都不 panic
    #[test]
    fn test_order_chapters_for_export_boundaries() {
        assert!(order_chapters_for_export(Vec::new(), &[]).is_empty());
        let single = vec![row(42, "唯一章", "甲")];
        assert_eq!(
            titles(&order_chapters_for_export(single.clone(), &[])),
            vec!["唯一章"]
        );
        // 坏 JSON → 空目录（跳过而不 panic）
        assert!(parse_toc_entries("不是 json").is_empty());
        assert!(parse_toc_entries("{}").is_empty());
        // 元素缺 title / title 空 → 跳过
        assert!(parse_toc_entries(r#"[{"url":"http://a/1.html"},{"title":"  "}]"#).is_empty());
        // 正常元素：hash 从 #tail 与真实 URL 两种来源都能取到
        let parsed = parse_toc_entries(
            r#"[{"title":"第一章","url":"cache://http://b/1.html#777"},
                {"title":"第二章","url":"http://b/2.html"}]"#,
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].hash, Some(777));
        assert_eq!(
            parsed[1].hash,
            Some(crate::util::md5::chapter_url_hash("http://b/2.html"))
        );
        // 章号解析：中文数字/无「第」都不猜
        assert_eq!(chapter_num_of_title("第12章 x"), Some(12));
        assert_eq!(chapter_num_of_title("第十二章 x"), Some(12));
        assert_eq!(chapter_num_of_title("第十一章 x"), Some(11));
        assert_eq!(chapter_num_of_title("第二十三章 x"), Some(23));
        assert_eq!(chapter_num_of_title("第一百零五章 x"), Some(105));
        assert_eq!(chapter_num_of_title("序章"), None);
        assert_eq!(chapter_num_of_title("十二章 x"), None, "没有「第」前缀不猜");
        assert_eq!(chapter_num_of_title("第x章"), None);
    }

    /// 端到端：目录缓存存在时，getAllContents / exportToTxt 都按阅读序输出
    /// （种子故意让 chapter_index 的哈希序与阅读序相反）
    #[tokio::test]
    async fn test_export_order_follows_toc_cache() {
        let (state, _dir) = test_state("exportorder").await;
        let book_url = "https://src/orderbook";
        state
            .storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: book_url.into(),
                    name: "乱序书".into(),
                    origin: "https://src".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // chapter_index 哈希序：300(第二章) < 600(第三章) < 900(第一章)
        state
            .storage
            .cache_chapter_content("default", book_url, 900, "第一章", "正文一。")
            .await
            .unwrap();
        state
            .storage
            .cache_chapter_content("default", book_url, 300, "第二章", "正文二。")
            .await
            .unwrap();
        state
            .storage
            .cache_chapter_content("default", book_url, 600, "第三章", "正文三。")
            .await
            .unwrap();
        // 目录缓存（key 用 bookUrl，与离线兜底合成目录一致）
        state
            .storage
            .cache_toc(
                "default",
                book_url,
                book_url,
                r#"[{"title":"第一章","url":"cache://b#900"},
                    {"title":"第二章","url":"cache://b#300"},
                    {"title":"第三章","url":"cache://b#600"}]"#,
            )
            .await
            .unwrap();

        // getAllContents：chapters 按阅读序
        let ret = get_all_contents(
            State(state.clone()),
            q(&[("bookUrl", book_url)]),
            empty_headers(),
            None,
        )
        .await;
        assert!(ret.is_success, "{:?}", ret.error_msg);
        let data = ret.0.data.clone();
        let chapters = data.get("chapters").and_then(|v| v.as_array()).unwrap();
        let got: Vec<&str> = chapters
            .iter()
            .map(|c| c.get("title").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(got, vec!["第一章", "第二章", "第三章"]);

        // exportToTxt：正文按阅读序拼接
        let resp = export_to_txt(
            State(state),
            q(&[("bookUrl", book_url)]),
            empty_headers(),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let p1 = text.find("正文一。").expect("应含第一章正文");
        let p2 = text.find("正文二。").expect("应含第二章正文");
        let p3 = text.find("正文三。").expect("应含第三章正文");
        assert!(p1 < p2 && p2 < p3, "导出正文必须按阅读序: {text}");
    }
}
