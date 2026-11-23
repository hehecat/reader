//! 书架与书籍信息 API：`get_bookshelf`、`save_book`、`delete_book`、`delete_books`、`save_book_progress`、`set_book_source`、`get_book_info`

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::helpers::{ensure_book_quota, int_param_of, opt_str_param_of, str_param_of};
use super::local::migrate_local_book_file;
use crate::api::auth::resolve_namespace;
use crate::api::router::{now_millis, param_of, AppState, ReturnData};
use crate::api::sources::{resolve_book_source, resolve_book_source_for_url};

// ==================== 进程内书籍详情缓存 ====================

const BOOK_INFO_CACHE_CAP: usize = 200;
const BOOK_INFO_CACHE_TTL_MS: u64 = 10 * 60 * 1000;

#[derive(Default)]
struct BookInfoCacheStore {
    map: HashMap<String, (crate::model::book_chapter::BookInfo, Instant)>,
    order: VecDeque<String>,
}

impl BookInfoCacheStore {
    fn get(&mut self, ns: &str, url: &str) -> Option<crate::model::book_chapter::BookInfo> {
        let key = format!("{ns}:{url}");
        match self.map.get(&key) {
            Some((info, at)) if at.elapsed().as_millis() <= BOOK_INFO_CACHE_TTL_MS as u128 => {
                Some(info.clone())
            }
            Some(_) => {
                self.map.remove(&key);
                None
            }
            None => None,
        }
    }

    fn put(&mut self, ns: &str, url: &str, info: crate::model::book_chapter::BookInfo) {
        let key = format!("{ns}:{url}");
        if !self.map.contains_key(&key) {
            self.order.push_back(key.clone());
            while self.order.len() > BOOK_INFO_CACHE_CAP {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
        self.map.insert(key, (info, Instant::now()));
    }
}

static BOOK_INFO_CACHE: LazyLock<Mutex<BookInfoCacheStore>> =
    LazyLock::new(|| Mutex::new(BookInfoCacheStore::default()));

fn book_info_cache_get(ns: &str, url: &str) -> Option<crate::model::book_chapter::BookInfo> {
    BOOK_INFO_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(ns, url)
}

fn book_info_cache_put(ns: &str, url: &str, info: crate::model::book_chapter::BookInfo) {
    BOOK_INFO_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .put(ns, url, info)
}

// ==================== 书架 CRUD ====================

#[derive(serde::Serialize)]
struct BookshelfItem<'a> {
    #[serde(flatten)]
    book: &'a crate::model::Book,
    #[serde(rename = "groupIds")]
    group_ids: Vec<i64>,
}

fn parse_group_ids(raw: &str) -> Vec<i64> {
    serde_json::from_str(raw).unwrap_or_else(|_| {
        raw.split([',', '，', '、'])
            .filter_map(|s| s.trim().parse::<i64>().ok())
            .collect()
    })
}

/// GET `/reader3/getBookshelf`
pub async fn get_bookshelf(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let refresh = params.get("refresh").map(|v| v == "1").unwrap_or(false);
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    if refresh {
        match crate::storage::run_shelf_update(&state.storage).await {
            Ok(n) => tracing::info!("getBookshelf refresh=1 更新 {n} 本"),
            Err(e) => tracing::warn!("getBookshelf refresh=1 书架更新失败: {e:#}"),
        }
    }
    match state.storage.list_books(&namespace).await {
        Ok(books) => {
            tracing::info!("getBookshelf [{}]: {} 本", namespace, books.len());
            let items: Vec<BookshelfItem<'_>> = books
                .iter()
                .map(|book| BookshelfItem {
                    book,
                    group_ids: parse_group_ids(&book.group_ids),
                })
                .collect();
            ReturnData::ok_typed(items).into_response()
        }
        Err(e) => {
            tracing::error!("查询书架 [{namespace}] 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// POST `/reader3/saveBook`：入架或增量编辑
pub async fn save_book(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let Some(body) = body else {
        return Json(ReturnData::err("参数错误"));
    };
    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    let mut book: crate::model::Book = match serde_json::from_value(body_json.clone()) {
        Ok(b) => b,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    let mut book_url = if book.book_url.is_empty() {
        param_of(&params, Some(&body_json), "bookUrl")
    } else {
        book.book_url.clone()
    };
    if book_url.is_empty() {
        return Json(ReturnData::err("书籍链接不能为空"));
    }

    let mut existing = state
        .storage
        .find_book(&namespace, &book_url)
        .await
        .ok()
        .flatten();
    if existing.is_none() && !book.name.is_empty() {
        existing = state
            .storage
            .find_book_by_name_author(&namespace, &book.name, &book.author)
            .await
            .ok()
            .flatten();
    }
    let exists = existing.is_some();
    if !exists {
        if let Err(ret) = ensure_book_quota(&state, &namespace, &book_url).await {
            return Json(ret);
        }
    }
    let mut loc_migrated = false;
    if (!exists || existing.as_ref().is_some_and(|ex| ex.book_url != book_url))
        && crate::service::local_book::is_local_book(&book_url, &book.origin)
    {
        if let Some(new_url) = migrate_local_book_file(
            &state.storage.config.storage_dir(),
            &namespace,
            &book.name,
            &book.author,
            &book_url,
        )
        .await
        {
            tracing::info!("saveBook 本地书迁移: {book_url} → {new_url}");
            book_url = new_url.clone();
            book.book_url = new_url.clone();
            book.toc_url = new_url;
            loc_migrated = true;
        }
    }

    let result = if let Some(ex) = existing {
        let mut patch = body_json.as_object().cloned().unwrap_or_default();
        for k in [
            "durChapterIndex",
            "durChapterPos",
            "durChapterTime",
            "durChapterTitle",
        ] {
            patch.remove(k);
        }
        if loc_migrated {
            if let Some(v) = patch.get_mut("tocUrl") {
                *v = Value::String(book.toc_url.clone());
            }
        }
        if ex.book_url != book_url {
            let origin_new = if book.origin.is_empty() {
                &ex.origin
            } else {
                &book.origin
            };
            let origin_name_new = if book.origin_name.is_empty() {
                &ex.origin_name
            } else {
                &book.origin_name
            };
            let toc_new = if book.toc_url.is_empty() {
                book_url.clone()
            } else {
                book.toc_url.clone()
            };
            let _ = state
                .storage
                .switch_book_source(
                    &namespace,
                    &ex.book_url,
                    &book_url,
                    origin_new,
                    origin_name_new,
                    &toc_new,
                    None,
                )
                .await;
        }
        state
            .storage
            .patch_book(&namespace, &book_url, &patch)
            .await
    } else {
        let mut b = book;
        b.book_url = book_url.clone();
        b.user_namespace = namespace.clone();
        b.is_in_shelf = true;
        if b.created_at == 0 {
            b.created_at = now_millis();
        }
        if b.dur_chapter_time == 0 {
            b.dur_chapter_time = now_millis();
        }
        if let Some(cov) = b.cover_url.clone() {
            if cov.starts_with("http://") || cov.starts_with("https://") {
                match crate::service::crawler::fetch_image(
                    &namespace,
                    &cov,
                    None,
                    15,
                    5 * 1024 * 1024,
                )
                .await
                {
                    Ok((bytes, content_type, _)) if !bytes.is_empty() => {
                        let cov_path = cov.split('?').next().unwrap_or(&cov);
                        let ext = crate::service::local_book::file_ext(cov_path);
                        if let Some((ext, data)) = crate::service::imaging::normalize_cover(
                            &ext,
                            content_type.as_deref(),
                            bytes,
                        )
                        .await
                        {
                            let md5 = crate::util::md5::md5_encode(&cov);
                            let dir = state
                                .storage
                                .config
                                .storage_dir()
                                .join("assets")
                                .join(&namespace)
                                .join("covers");
                            if tokio::fs::create_dir_all(&dir).await.is_ok() {
                                let fname = format!("{md5}.{ext}");
                                if tokio::fs::write(dir.join(&fname), &data).await.is_ok() {
                                    b.cover_url =
                                        Some(format!("/assets/{namespace}/covers/{fname}"));
                                }
                            }
                        }
                    }
                    _ => tracing::debug!("saveBook 封面下载失败（保留原 URL）: {cov}"),
                }
            }
        }
        state
            .storage
            .upsert_book(&namespace, &b)
            .await
            .map(|_| 1u64)
    };
    match result {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("saveBook 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/deleteBook`
pub async fn delete_book(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut book_url = str_param_of(&params, body_json.as_ref(), &["bookUrl", "url"]);
    if book_url.is_empty() {
        let name = param_of(&params, body_json.as_ref(), "name");
        let author = param_of(&params, body_json.as_ref(), "author");
        if !name.is_empty() {
            if let Ok(Some(b)) = state
                .storage
                .find_book_by_name_author(&namespace, &name, &author)
                .await
            {
                book_url = b.book_url;
            }
        }
    }
    if book_url.is_empty() {
        return Json(ReturnData::err("书架书籍不存在"));
    }
    match state.storage.delete_book(&namespace, &book_url).await {
        Ok(0) => Json(ReturnData::err("书架书籍不存在")),
        Ok(_) => Json(ReturnData::ok(json!("删除书籍成功"))),
        Err(e) => {
            tracing::error!("deleteBook 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/deleteBooks`
pub async fn delete_books(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let Some(body) = body else {
        return Json(ReturnData::err("参数错误"));
    };
    let json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    let mut urls: Vec<String> = json
        .get("bookUrls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        if let Some(arr) = json.as_array() {
            for item in arr {
                if let Some(u) = item.get("bookUrl").and_then(|v| v.as_str()) {
                    if !u.is_empty() {
                        urls.push(u.to_string());
                        continue;
                    }
                }
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let author = item.get("author").and_then(|v| v.as_str()).unwrap_or("");
                if !name.is_empty() {
                    if let Ok(Some(b)) = state
                        .storage
                        .find_book_by_name_author(&namespace, name, author)
                        .await
                    {
                        urls.push(b.book_url);
                    }
                }
            }
        }
    }
    if urls.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match state.storage.delete_books(&namespace, &urls).await {
        Ok(count) => Json(ReturnData::ok(json!({ "count": count }))),
        Err(e) => {
            tracing::error!("deleteBooks 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/saveBookProgress`：保存阅读进度（JSON Body 优先，Query 兜底）
pub async fn save_book_progress(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut book_url = str_param_of(&params, body_json.as_ref(), &["bookUrl", "url"]);
    if book_url.is_empty() {
        book_url = body_json
            .as_ref()
            .and_then(|b| b.get("searchBook"))
            .and_then(|s| s.get("bookUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    if book_url.is_empty() {
        return Json(ReturnData::ok(Value::Null));
    }
    let index =
        int_param_of(&params, body_json.as_ref(), &["durChapterIndex", "index"]).unwrap_or(0);
    let pos = int_param_of(&params, body_json.as_ref(), &["durChapterPos", "pos"]).unwrap_or(0);
    let time = int_param_of(&params, body_json.as_ref(), &["durChapterTime", "time"])
        .unwrap_or_else(now_millis);
    if index < 0 {
        return Json(ReturnData::err("章节不存在"));
    }
    let title = opt_str_param_of(&params, body_json.as_ref(), &["durChapterTitle", "title"]);
    let exists = state
        .storage
        .find_book(&namespace, &book_url)
        .await
        .ok()
        .flatten()
        .is_some();
    match state
        .storage
        .update_book_progress(&namespace, &book_url, title.as_deref(), index, pos, time)
        .await
    {
        Ok(0) if exists => Json(ReturnData::ok(Value::Null)),
        Ok(0) => Json(ReturnData::err("书籍未加入书架")),
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("saveBookProgress 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

// ==================== 换源与书籍详情 ====================

/// GET/POST `/reader3/setBookSource`：换源持久化
pub async fn set_book_source(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    let new_url = param_of(&params, body_json.as_ref(), "newUrl");
    let source_url = param_of(&params, body_json.as_ref(), "bookSourceUrl");
    if book_url.is_empty() {
        return Json(ReturnData::err("书籍链接不能为空")).into_response();
    }
    if new_url.is_empty() {
        return Json(ReturnData::err("新源书籍链接不能为空")).into_response();
    }
    if source_url.is_empty() {
        return Json(ReturnData::err("书源链接不能为空")).into_response();
    }

    let mut book = match state.storage.find_book(&namespace, &book_url).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            let name = param_of(&params, body_json.as_ref(), "name");
            let author = param_of(&params, body_json.as_ref(), "author");
            let fallback = if name.is_empty() {
                None
            } else {
                state
                    .storage
                    .find_book_by_name_author(&namespace, &name, &author)
                    .await
                    .ok()
                    .flatten()
            };
            match fallback {
                Some(b) => {
                    book_url = b.book_url.clone();
                    b
                }
                None => return Json(ReturnData::err("书籍信息错误")).into_response(),
            }
        }
        Err(_) => return Json(ReturnData::err("系统错误")).into_response(),
    };
    let Some(source) = resolve_book_source(&state, &namespace, &source_url).await else {
        return Json(ReturnData::err("书源信息错误")).into_response();
    };
    let info = match crate::service::book::fetch_book_info(
        &namespace,
        &new_url,
        &source,
        Some(&book.name),
    )
    .await
    {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("setBookSource 获取新书详情失败 [{new_url}]: {e}");
            return Json(ReturnData::err("书源信息错误")).into_response();
        }
    };
    let toc_url_new = info
        .toc_url
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| new_url.clone());
    let origin_new = if info.origin.is_empty() {
        source.book_source_url.clone()
    } else {
        info.origin.clone()
    };
    match state
        .storage
        .switch_book_source(
            &namespace,
            &book_url,
            &new_url,
            &origin_new,
            &info.origin_name,
            &toc_url_new,
            info.cover_url.as_deref(),
        )
        .await
    {
        Ok(0) => Json(ReturnData::err("书籍信息错误")).into_response(),
        Ok(_) => {
            let _ = state
                .storage
                .clear_book_info_cache(&namespace, &book_url)
                .await;
            let info_value = serde_json::to_value(info.clone()).unwrap_or(Value::Null);
            let _ = state
                .storage
                .set_book_info_cache(&namespace, &new_url, &info_value)
                .await;
            book.origin = origin_new;
            book.origin_name = info.origin_name;
            book.book_url = new_url.clone();
            book.toc_url = toc_url_new.clone();
            if book.cover_url.as_deref().map(str::is_empty).unwrap_or(true) {
                book.cover_url = info.cover_url;
            }
            if let Ok(chapters) = crate::service::book::analyze_toc(
                &namespace,
                &toc_url_new,
                &source,
                20,
                Some(&book.name),
                &new_url,
            )
            .await
            {
                if !chapters.is_empty() {
                    if let Ok(json) = serde_json::to_string(&chapters) {
                        let _ = state
                            .storage
                            .cache_toc(&namespace, &new_url, &toc_url_new, &json)
                            .await;
                    }
                }
            }
            ReturnData::ok_typed(book).into_response()
        }
        Err(e) => {
            tracing::error!("setBookSource 更新失败 [{book_url}]: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// GET/POST `/reader3/getBookInfo`
pub async fn get_book_info(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut url = param_of(&params, body_json.as_ref(), "url");
    if url.is_empty() {
        url = body_json
            .as_ref()
            .and_then(|b| b.get("searchBook"))
            .and_then(|s| s.get("bookUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    if url.is_empty() {
        return Json(ReturnData::err("请输入书籍链接")).into_response();
    }
    let books = match state.storage.list_books(&namespace).await {
        Ok(b) => b,
        Err(_) => return Json(ReturnData::err("系统错误")).into_response(),
    };
    let shelf_match = books.iter().find(|b| b.book_url == url);
    if let Some(book) =
        shelf_match.filter(|b| crate::service::local_book::is_local_book(&url, b.origin.as_str()))
    {
        let info = crate::model::book_chapter::BookInfo {
            name: book.name.clone(),
            author: book.author.clone(),
            kind: book.kind.clone(),
            intro: book.intro.clone(),
            cover_url: book
                .custom_cover_url
                .clone()
                .or_else(|| book.cover_url.clone()),
            toc_url: Some(if book.toc_url.is_empty() {
                book.book_url.clone()
            } else {
                book.toc_url.clone()
            }),
            book_url: book.book_url.clone(),
            origin: book.origin.clone(),
            origin_name: book.origin_name.clone(),
            language: book.language.clone(),
            publisher: book.publisher.clone(),
            published_at: book.published_at.clone(),
            ..Default::default()
        };
        return ReturnData::ok_typed(info).into_response();
    }
    if url.starts_with("local://") || url.ends_with(".txt") {
        return Json(ReturnData::err("未找到这本书（可能不在书架中）")).into_response();
    }
    if let Ok(Some(cached)) = state
        .storage
        .get_book_info_cache(&namespace, &url, 6 * 3600 * 1000)
        .await
    {
        return Json(ReturnData::ok(cached)).into_response();
    }
    if shelf_match.is_none() {
        if let Some(cached) = book_info_cache_get(&namespace, &url) {
            return ReturnData::ok_typed(cached).into_response();
        }
    }
    let mut bs_param = param_of(&params, body_json.as_ref(), "bookSource");
    if bs_param.is_empty() {
        if let Ok(sources) = state.storage.get_book_sources(&namespace).await {
            for s in sources {
                if !s.enabled {
                    continue;
                }
                if let Some(pat) = &s.book_url_pattern {
                    if !pat.is_empty()
                        && crate::util::regex::Regex::new(pat)
                            .map(|r| r.is_match(&url))
                            .unwrap_or(false)
                    {
                        bs_param = s.book_source_url.clone();
                        break;
                    }
                }
            }
        }
    }
    let Some(source) = resolve_book_source_for_url(&state, &namespace, &bs_param, &url).await
    else {
        return Json(ReturnData::err("书源不存在")).into_response();
    };
    match crate::service::book::fetch_book_info(
        &namespace,
        &url,
        &source,
        shelf_match.map(|b| b.name.as_str()),
    )
    .await
    {
        Ok(mut info) => {
            if let Some(shelf) = shelf_match {
                if !crate::service::local_book::is_local_book(&url, &shelf.origin) {
                    crate::service::book::merge_existing_identity(
                        &mut info,
                        &source,
                        &shelf.name,
                        &shelf.author,
                    );
                }
            } else {
                book_info_cache_put(&namespace, &url, info.clone());
            }
            let info_value = serde_json::to_value(&info).unwrap_or(Value::Null);
            let _ = state
                .storage
                .set_book_info_cache(&namespace, &url, &info_value)
                .await;
            ReturnData::ok_typed(info).into_response()
        }
        Err(e) => {
            tracing::error!("getBookInfo 失败 [{url}]: {e}");
            Json(ReturnData::err("获取详情失败")).into_response()
        }
    }
}
