//! 辅助功能 API：净化替换规则、缓存管理与整书缓存 SSE、全书搜索、离线词典、自定义字体、TTS/HttpTTS、WebDAV 备份恢复

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::auth::resolve_namespace;
use super::books::{resolve_loc_book_file, txt_toc_rule_regexes};
use super::router::{param_of, read_multipart_field_limited, AppState, ReturnData};
use super::sse;

// ==================== 净化替换规则 ====================

/// GET/POST `/reader3/getReplaceRules`
pub async fn get_replace_rules(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    match state.storage.get_replace_rules(&namespace).await {
        Ok(rules) => ReturnData::ok_typed(rules).into_response(),
        Err(e) => {
            tracing::error!("getReplaceRules [{namespace}] 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// 净化规则语义去重键：同一用户命名空间下，查找/替换/作用范围与匹配模式完全一致视为同一规则
fn replace_rule_dedup_key(
    r: &crate::model::ReplaceRule,
) -> (String, String, String, bool, bool, bool) {
    (
        r.find.clone(),
        r.replace.clone(),
        r.scope.as_deref().unwrap_or("").trim().to_string(),
        r.scope_title,
        r.scope_content,
        r.is_regex,
    )
}

/// POST `/reader3/saveReplaceRule`
pub async fn save_replace_rule(
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
    let mut rule: crate::model::ReplaceRule = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    if rule.name.trim().is_empty() {
        return Json(ReturnData::err("名称不能为空"));
    }
    if rule.find.trim().is_empty() {
        return Json(ReturnData::err("规则不能为空"));
    }
    let is_new = rule.id.trim().is_empty();
    if is_new {
        let existing = state
            .storage
            .get_replace_rules(&namespace)
            .await
            .unwrap_or_default();
        let key = replace_rule_dedup_key(&rule);
        if let Some(dup) = existing
            .iter()
            .find(|r| r.user_namespace == namespace && replace_rule_dedup_key(r) == key)
        {
            // 语义重复：复用已有规则 ID 覆盖更新，避免重复条目
            rule.id = dup.id.clone();
            if rule.order == 0 {
                rule.order = dup.order;
            }
        } else {
            rule.id = format!("rule-{}", uuid::Uuid::new_v4());
            if rule.order == 0 {
                let max_order = existing.iter().map(|r| r.order).max().unwrap_or(0);
                rule.order = max_order + 1;
            }
        }
    }
    rule.user_namespace = namespace.clone();
    match state.storage.save_replace_rule(&namespace, &rule).await {
        Ok(id) => Json(ReturnData::ok(json!({ "id": id }))),
        Err(e) => {
            tracing::error!("saveReplaceRule 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/saveReplaceRules`
pub async fn save_replace_rules(
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
    let raw_rules: Vec<crate::model::ReplaceRule> =
        if let Ok(list) = serde_json::from_slice::<Vec<crate::model::ReplaceRule>>(&body) {
            list
        } else if let Ok(val) = serde_json::from_slice::<Value>(&body) {
            let inner = val
                .get("rules")
                .or_else(|| val.get("data"))
                .cloned()
                .unwrap_or(Value::Null);
            match serde_json::from_value::<Vec<crate::model::ReplaceRule>>(inner) {
                Ok(list) => list,
                Err(_) => return Json(ReturnData::err("参数错误")),
            }
        } else {
            return Json(ReturnData::err("参数错误"));
        };
    if raw_rules.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let existing = state
        .storage
        .get_replace_rules(&namespace)
        .await
        .unwrap_or_default();
    let mut existing_by_key: HashMap<(String, String, String, bool, bool, bool), String> = existing
        .iter()
        .filter(|r| r.user_namespace == namespace)
        .map(|r| (replace_rule_dedup_key(r), r.id.clone()))
        .collect();
    let mut seen_in_batch = std::collections::HashSet::new();
    let mut rules: Vec<crate::model::ReplaceRule> = Vec::with_capacity(raw_rules.len());
    for mut rule in raw_rules {
        if rule.name.trim().is_empty() || rule.find.trim().is_empty() {
            return Json(ReturnData::err("参数错误"));
        }
        let key = replace_rule_dedup_key(&rule);
        if !seen_in_batch.insert(key.clone()) {
            continue;
        }
        if let Some(existing_id) = existing_by_key.get(&key) {
            rule.id = existing_id.clone();
        } else if rule.id.trim().is_empty() {
            rule.id = format!("rule-{}", uuid::Uuid::new_v4());
            existing_by_key.insert(key, rule.id.clone());
        }
        rule.user_namespace = namespace.clone();
        rules.push(rule);
    }
    match state.storage.save_replace_rules(&namespace, &rules).await {
        Ok(_) => Json(ReturnData::ok(json!({ "count": rules.len() }))),
        Err(e) => {
            tracing::error!("saveReplaceRules 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/deleteReplaceRule`
pub async fn delete_replace_rule(
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
    let id = param_of(&params, body_json.as_ref(), "id");
    if id.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match state.storage.delete_replace_rule(&namespace, &id).await {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("deleteReplaceRule 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/deleteReplaceRules`
pub async fn delete_replace_rules(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let Some(json) = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return Json(ReturnData::err("参数错误"));
    };
    let run = |ids: Vec<String>| {
        let state = &state;
        let namespace = &namespace;
        async move {
            if ids.is_empty() {
                return Json(ReturnData::err("参数错误"));
            }
            match state.storage.delete_replace_rules(namespace, &ids).await {
                Ok(n) => Json(ReturnData::ok(json!({ "count": n }))),
                Err(e) => {
                    tracing::error!("deleteReplaceRules 失败: {e}");
                    Json(ReturnData::err("删除失败"))
                }
            }
        }
    };
    match &json {
        Value::Array(arr) => {
            let ids: Vec<String> = arr
                .iter()
                .filter_map(|v| {
                    v.get("id")
                        .and_then(|x| x.as_str())
                        .map(str::to_string)
                        .or_else(|| v.get("name").and_then(|x| x.as_str()).map(str::to_string))
                })
                .collect();
            run(ids).await
        }
        Value::Object(obj) => {
            if obj.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
                return match state.storage.delete_all_replace_rules(&namespace).await {
                    Ok(n) => Json(ReturnData::ok(json!({ "count": n }))),
                    Err(e) => {
                        tracing::error!("deleteReplaceRules(all) 失败: {e}");
                        Json(ReturnData::err("删除失败"))
                    }
                };
            }
            let ids: Vec<String> = obj
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            run(ids).await
        }
        _ => Json(ReturnData::err("参数错误")),
    }
}

// ==================== 缓存统计、清理与整书缓存 SSE ====================

const CACHE_INFO_TTL: Duration = Duration::from_secs(30);
type CacheInfoStore = tokio::sync::Mutex<HashMap<String, (Instant, crate::storage::CacheInfo)>>;

fn cache_info_store() -> &'static CacheInfoStore {
    static STORE: OnceLock<CacheInfoStore> = OnceLock::new();
    STORE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// GET/POST `/reader3/getCacheInfo`
pub async fn get_cache_info(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let cached_hit = {
        let store = cache_info_store().lock().await;
        store
            .get(&namespace)
            .filter(|(at, _)| at.elapsed() < CACHE_INFO_TTL)
            .map(|(_, info)| info.clone())
    };
    if let Some(info) = cached_hit {
        return ReturnData::ok_typed(info).into_response();
    }
    match state.storage.get_cache_info(&namespace).await {
        Ok(info) => {
            cache_info_store()
                .lock()
                .await
                .insert(namespace.clone(), (Instant::now(), info.clone()));
            ReturnData::ok_typed(info).into_response()
        }
        Err(e) => {
            tracing::error!("getCacheInfo 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// POST `/reader3/clearCache`
pub async fn clear_cache(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let mut cache_type = params.get("type").cloned().unwrap_or_default();
    if let Some(body) = body {
        if let Ok(json) = serde_json::from_slice::<Value>(&body) {
            if let Some(v) = json.get("type").and_then(|v| v.as_str()) {
                cache_type = v.to_string();
            }
        }
    }
    if cache_type.is_empty() {
        cache_type = "all".to_string();
    }
    if cache_type != "toc" && cache_type != "chapters" && cache_type != "all" {
        return Json(ReturnData::err("参数错误"));
    }
    match state.storage.clear_cache(&namespace, &cache_type).await {
        Ok((toc_deleted, chapters_deleted)) => {
            cache_info_store().lock().await.remove(&namespace);
            Json(ReturnData::ok(json!({
                "deletedToc": toc_deleted,
                "deletedChapters": chapters_deleted,
            })))
        }
        Err(e) => {
            tracing::error!("clearCache 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

/// GET/POST `/reader3/deleteBookCache`
pub async fn delete_book_cache(
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
    let mut url = param_of(&params, body_json.as_ref(), "url");
    if url.is_empty() {
        url = param_of(&params, body_json.as_ref(), "bookUrl");
    }
    if url.is_empty() {
        return Json(ReturnData::err("请输入书籍链接"));
    }
    let book = match state.storage.find_book(&namespace, &url).await {
        Ok(Some(b)) => b,
        Ok(None) => return Json(ReturnData::err("请先加入书架")),
        Err(e) => {
            tracing::error!("deleteBookCache 查询失败 [{url}]: {e}");
            return Json(ReturnData::err("系统错误"));
        }
    };
    if crate::service::local_book::is_local_book(&book.book_url, &book.origin) {
        return Json(ReturnData::err("本地书籍无需删除缓存"));
    }
    let _ = state.storage.clear_book_info_cache(&namespace, &url).await;
    match state.storage.delete_book_cache(&namespace, &url).await {
        Ok(_) => {
            cache_info_store().lock().await.remove(&namespace);
            Json(ReturnData::ok(json!("")))
        }
        Err(e) => {
            tracing::error!("deleteBookCache 失败 [{url}]: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ShelfBookCacheItem {
    #[serde(flatten)]
    book: crate::model::Book,
    cache_chapter_count: i64,
    cache_size: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ShelfBookCacheDetail {
    #[serde(flatten)]
    book: crate::model::Book,
    cache_chapter_count: i64,
    cache_size: i64,
    on_shelf: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OffShelfBookCacheDetail<'a> {
    book_url: &'a str,
    toc_url: &'a str,
    origin: &'static str,
    origin_name: &'static str,
    name: &'static str,
    author: &'static str,
    #[serde(rename = "type")]
    book_type: i64,
    cache_chapter_count: i64,
    cache_size: i64,
    on_shelf: bool,
}

/// GET/POST `/reader3/getShelfBookWithCacheInfo`
pub async fn get_shelf_book_with_cache_info(
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
    let url = param_of(&params, body_json.as_ref(), "url");
    if url.is_empty() {
        let books = match state.storage.list_books(&namespace).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("getShelfBookWithCacheInfo 列表失败: {e}");
                return Json(ReturnData::err("系统错误")).into_response();
            }
        };
        let mut out: Vec<ShelfBookCacheItem> = Vec::with_capacity(books.len());
        for book in books {
            let (cache_chapter_count, cache_size) = state
                .storage
                .book_cache_info(&namespace, &book.book_url)
                .await
                .unwrap_or((0, 0));
            out.push(ShelfBookCacheItem {
                book,
                cache_chapter_count,
                cache_size,
            });
        }
        return ReturnData::ok_typed(out).into_response();
    }
    let (cache_chapter_count, cache_size) = state
        .storage
        .book_cache_info(&namespace, &url)
        .await
        .unwrap_or((0, 0));
    let book = match state.storage.find_book(&namespace, &url).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return ReturnData::ok_typed(OffShelfBookCacheDetail {
                book_url: &url,
                toc_url: &url,
                origin: "",
                origin_name: "",
                name: "",
                author: "",
                book_type: 0,
                cache_chapter_count,
                cache_size,
                on_shelf: false,
            })
            .into_response();
        }
        Err(e) => {
            tracing::error!("getShelfBookWithCacheInfo 失败 [{url}]: {e}");
            return Json(ReturnData::err("系统错误")).into_response();
        }
    };
    ReturnData::ok_typed(ShelfBookCacheDetail {
        book,
        cache_chapter_count,
        cache_size,
        on_shelf: true,
    })
    .into_response()
}

/// GET/POST `/reader3/cacheBookSSE`：整书缓存进度 SSE 流（统一走 `api::sse`，支持变化去重与断连即停）
pub async fn cache_book_sse(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return sse::sse_error(ret),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let url = param_of(&params, body_json.as_ref(), "url");
    let task_id = param_of(&params, body_json.as_ref(), "taskId");
    let has_task_id = !task_id.is_empty();
    let task_id = if has_task_id { task_id } else { url.clone() };
    if task_id.is_empty() {
        return sse::sse_error(ReturnData::err("参数错误"));
    }
    let progress = match crate::service::cache_job::progress_of_key(&namespace, &task_id) {
        Some(p) => p,
        None if !has_task_id && !url.is_empty() => {
            match state.storage.find_book(&namespace, &url).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return sse::sse_error(ReturnData::err("书籍不存在（请先加入书架）"));
                }
                Err(e) => {
                    tracing::error!("cacheBookSSE 查询失败 [{url}]: {e}");
                    return sse::sse_error(ReturnData::err("系统错误"));
                }
            }
            crate::service::cache_job::start(&namespace, &url, state.storage.clone())
        }
        None => return sse::sse_error(ReturnData::err("缓存任务不存在")),
    };

    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let storage = state.storage.clone();
    tokio::spawn(crate::storage::scope_task_storage(storage, async move {
        let mut last_snapshot: Option<(usize, usize, String, bool, bool, Option<String>)> = None;
        loop {
            if tx.is_closed() {
                return;
            }
            let (snap, payload, done) = {
                let p = progress.lock().unwrap_or_else(|e| e.into_inner());
                let snap = (
                    p.cached,
                    p.total,
                    p.title.clone(),
                    p.finished,
                    p.cancelled,
                    p.error.clone(),
                );
                let payload = json!({
                    "cached": p.cached,
                    "total": p.total,
                    "title": p.title,
                    "finished": p.finished,
                    "cancelled": p.cancelled,
                    "error": p.error,
                    "cachedCount": p.cached,
                    "successCount": p.cached,
                    "failedCount": 0,
                });
                (snap, payload, p.finished || p.cancelled)
            };
            // 首帧、进度变化或终态时推送，空闲期间由 axum Sse 15s keep-alive 维持连接
            let changed = last_snapshot.as_ref() != Some(&snap);
            if changed || done {
                last_snapshot = Some(snap);
                if tx.send(sse::data_event(&payload)).await.is_err() {
                    return;
                }
            }
            if done {
                break;
            }
            tokio::select! {
                _ = tx.closed() => return,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }));

    sse::sse_from_receiver(rx)
}

/// GET/POST `/reader3/cancelCacheBook`
pub async fn cancel_cache_book(
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
    let url = param_of(&params, body_json.as_ref(), "url");
    let task_id = param_of(&params, body_json.as_ref(), "taskId");
    let task_id = if !task_id.is_empty() { task_id } else { url };
    if task_id.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let cancelled = crate::service::cache_job::cancel_key(&namespace, &task_id);
    Json(ReturnData::ok(json!({ "cancelled": cancelled })))
}

// ==================== 全书正文搜索 ====================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FileBookContentHit {
    chapter_index: usize,
    title: String,
    snippet: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrichedBookContentHit<'a> {
    chapter_index: i64,
    title: &'a str,
    snippet: &'a str,
    chapter_title: &'a str,
    result_text: &'a str,
    query: &'a str,
    result_count: i64,
    result_count_within_chapter: i64,
    page_index: i64,
    query_index_in_result: i64,
    query_index_in_chapter: i64,
}

/// GET/POST `/reader3/searchBookContent`
pub async fn search_book_content(
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
    let mut key = param_of(&params, body_json.as_ref(), "key");
    if key.is_empty() {
        key = param_of(&params, body_json.as_ref(), "keyword");
    }
    let book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    if key.is_empty() {
        return Json(ReturnData::err("请输入搜索关键字")).into_response();
    }
    if book_url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let shelf = state
        .storage
        .find_book(&namespace, &book_url)
        .await
        .ok()
        .flatten();
    let has_chapters = state
        .storage
        .count_chapters(&namespace, &book_url)
        .await
        .unwrap_or(0)
        > 0;
    if shelf.is_none() && !has_chapters {
        return Json(ReturnData::err("书籍不存在")).into_response();
    }
    if let Some(book) = &shelf {
        if book.origin == "loc_book" && book.book_url.starts_with("storage/") {
            return match search_file_book_content(&state, &namespace, book, &key).await {
                Ok(hits) => ReturnData::ok_typed(hits).into_response(),
                Err(e) => {
                    tracing::warn!("searchBookContent 文件书失败 [{book_url}]: {e}");
                    Json(ReturnData::err("搜索失败")).into_response()
                }
            };
        }
    }
    match state
        .storage
        .search_book_content(&namespace, &book_url, &key, 100)
        .await
    {
        Ok(mut hits) => {
            // 在线书章节正文以 `chapter_url_hash(&chapter.url)` 作为 `book_chapters.chapter_index` 存储；
            // 若命中大于常规章节序号（或非本地书），通过已缓存目录反解真实 0-based `chapterIndex` 并按章节升序排列。
            let is_local = shelf
                .as_ref()
                .is_some_and(|b| crate::service::local_book::is_local_book(&b.book_url, &b.origin))
                || book_url.starts_with("local://");
            if !is_local && !hits.is_empty() {
                let mut toc_candidates = Vec::new();
                if let Some(b) = &shelf {
                    if !b.toc_url.is_empty() {
                        toc_candidates.push(b.toc_url.clone());
                    }
                }
                if !toc_candidates.iter().any(|u| u == &book_url) {
                    toc_candidates.push(book_url.clone());
                }
                let mut toc_chapters: Option<Vec<crate::model::book_chapter::BookChapter>> = None;
                for cand in &toc_candidates {
                    if let Ok(Some(raw)) = state
                        .storage
                        .get_toc_cache(&namespace, cand, 365 * 24 * 3600 * 1000)
                        .await
                    {
                        if let Ok(parsed) = serde_json::from_str::<
                            Vec<crate::model::book_chapter::BookChapter>,
                        >(&raw)
                        {
                            if !parsed.is_empty() {
                                toc_chapters = Some(parsed);
                                break;
                            }
                        }
                    }
                }
                if let Some(chapters) = toc_chapters {
                    let mut hash_to_idx: HashMap<i64, i64> = HashMap::with_capacity(chapters.len());
                    let mut title_to_idx: HashMap<&str, i64> =
                        HashMap::with_capacity(chapters.len());
                    for (pos, ch) in chapters.iter().enumerate() {
                        let real_idx = if ch.index >= 0 { ch.index } else { pos as i64 };
                        hash_to_idx.insert(crate::util::md5::chapter_url_hash(&ch.url), real_idx);
                        if !ch.title.is_empty() {
                            title_to_idx.entry(ch.title.as_str()).or_insert(real_idx);
                        }
                    }
                    for h in &mut hits {
                        if let Some(&idx) = hash_to_idx.get(&h.chapter_index) {
                            h.chapter_index = idx;
                        } else if h.chapter_index > 1_000_000 {
                            if let Some(&idx) = title_to_idx.get(h.title.as_str()) {
                                h.chapter_index = idx;
                            }
                        }
                    }
                    hits.sort_by_key(|h| h.chapter_index);
                }
            }
            let total = hits.len() as i64;
            let enriched: Vec<EnrichedBookContentHit<'_>> = hits
                .iter()
                .enumerate()
                .map(|(i, h)| EnrichedBookContentHit {
                    chapter_index: h.chapter_index,
                    title: &h.title,
                    snippet: &h.snippet,
                    chapter_title: &h.title,
                    result_text: &h.snippet,
                    query: &key,
                    result_count: total,
                    result_count_within_chapter: 1,
                    page_index: i as i64,
                    query_index_in_result: i as i64,
                    query_index_in_chapter: 0,
                })
                .collect();
            ReturnData::ok_typed(enriched).into_response()
        }
        Err(e) => {
            tracing::error!("searchBookContent 失败 [{book_url}]: {e}");
            Json(ReturnData::err("搜索失败")).into_response()
        }
    }
}

async fn search_file_book_content(
    state: &AppState,
    namespace: &str,
    book: &crate::model::book::Book,
    key: &str,
) -> anyhow::Result<Vec<FileBookContentHit>> {
    let storage_dir = state.storage.config.storage_dir();
    let book_url = book.book_url.clone();
    let toc_url = book.toc_url.clone();
    let user_rules = txt_toc_rule_regexes(state, namespace).await;
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        let path = resolve_loc_book_file(&storage_dir, &book_url)
            .ok_or_else(|| anyhow::anyhow!("文件不存在"))?;
        let path_lower = path.to_string_lossy().to_lowercase();
        let imported = if path_lower.ends_with(".epub") {
            let bytes = std::fs::read(&path)?;
            crate::service::local_book::parse_epub(&bytes, &toc_url)?
        } else {
            crate::service::local_book::parse_txt_file_with_rules(&path, &user_rules)?
        };
        let key_lower = key.to_lowercase();
        let mut hits: Vec<FileBookContentHit> = Vec::new();
        for (i, ch) in imported.chapters.iter().enumerate() {
            if hits.len() >= 100 {
                break;
            }
            let title = if ch.title.is_empty() {
                format!("第{}章", i + 1)
            } else {
                ch.title.clone()
            };
            let matched_in_title = ch.title.to_lowercase().contains(&key_lower);
            let content_lower = ch.content.to_lowercase();
            let pos = if matched_in_title {
                Some(0usize)
            } else {
                content_lower.find(&key_lower)
            };
            if let Some(p) = pos {
                let content = &ch.content;
                let start = content.floor_char_boundary(p.saturating_sub(30));
                let end = content.floor_char_boundary((p + key.len() + 50).min(content.len()));
                let snippet = if start > 0 { "…" } else { "" }.to_string()
                    + &ch.content[start..end]
                    + if end < ch.content.len() { "…" } else { "" };
                hits.push(FileBookContentHit {
                    chapter_index: i,
                    title,
                    snippet,
                });
            }
        }
        Ok(hits)
    })
    .await
    .map_err(|e| anyhow::anyhow!("全文搜索任务异常: {e}"))?
}

// ==================== 离线词典 ====================

#[derive(serde::Serialize)]
struct DictLookupPayload {
    status: crate::service::dict::DictStatus,
    entries: Vec<crate::service::dict::DictEntry>,
}

/// GET/POST `/reader3/dictLookup`
pub async fn dict_lookup(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    if let Err(ret) = resolve_namespace(&state, &params, &headers).await {
        return Json(ret).into_response();
    }
    let body_json = body
        .as_ref()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok());
    let word = param_of(&params, body_json.as_ref(), "word");
    if word.trim().is_empty() {
        return Json(ReturnData::err("请输入查询词")).into_response();
    }
    let storage_dir = state.storage.config.storage_dir();
    match crate::service::dict::lookup(&storage_dir, &word).await {
        Ok(result) => ReturnData::ok_typed(DictLookupPayload {
            status: result.status,
            entries: result.entries,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!("词典查询失败: {e}");
            Json(ReturnData::err("词典查询失败")).into_response()
        }
    }
}

// ==================== 自定义字体 ====================

const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "woff", "woff2"];
const FONT_MAX_MB: u64 = 40;

fn fonts_dir(state: &AppState, ns: &str) -> PathBuf {
    state
        .storage
        .config
        .storage_dir()
        .join("data")
        .join(ns)
        .join("fonts")
}

fn font_mime(ext: &str) -> &'static str {
    match ext {
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        _ => "font/woff2",
    }
}

/// POST `/reader3/uploadFont`
pub async fn upload_font(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name = String::new();
    loop {
        match multipart.next_field().await {
            Ok(Some(mut field)) => {
                if field.name() == Some("file") {
                    file_name = field.file_name().unwrap_or("font.ttf").to_string();
                    match read_multipart_field_limited(
                        &mut field,
                        FONT_MAX_MB as usize * 1024 * 1024,
                        FONT_MAX_MB as i64,
                    )
                    .await
                    {
                        Ok(b) => file_bytes = Some(b),
                        Err(msg) => return Json(ReturnData::err(msg)),
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("uploadFont multipart 读取失败: {e}");
                break;
            }
        }
    }
    let Some(bytes) = file_bytes else {
        return Json(ReturnData::err("未收到文件"));
    };
    if bytes.is_empty() {
        return Json(ReturnData::err("文件为空"));
    }
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default();
    if !FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Json(ReturnData::err("仅支持 TTF/OTF/WOFF/WOFF2 字体文件"));
    }
    let stem = file_name
        .rsplit_once('.')
        .map(|(n, _)| n)
        .unwrap_or("font")
        .trim()
        .to_string();
    let stem = if stem.is_empty() {
        "font".to_string()
    } else {
        stem
    };
    let id = format!(
        "{}-{:x}",
        stem.chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>(),
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.len().hash(&mut h);
            bytes[..64.min(bytes.len())].hash(&mut h);
            h.finish()
        }
    );
    let dir = fonts_dir(&state, &namespace);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return Json(ReturnData::err(format!("创建字体目录失败: {e}")));
    }
    let path = dir.join(format!("{id}.{ext}"));
    if let Err(e) = tokio::fs::write(&path, &bytes).await {
        return Json(ReturnData::err(format!("写入字体失败: {e}")));
    }
    Json(ReturnData::ok(json!({
        "id": id,
        "name": file_name,
        "family": format!("ReaderCustom-{id}"),
        "size": bytes.len(),
    })))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FontListItem {
    id: String,
    name: String,
    family: String,
    size: u64,
    updated_at: i64,
}

/// GET `/reader3/getFontList`
pub async fn get_font_list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let dir = fonts_dir(&state, &namespace);
    let mut list: Vec<FontListItem> = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((id, ext)) = name.rsplit_once('.') else {
                continue;
            };
            if !FONT_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                continue;
            }
            let meta = entry.metadata().await.ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let updated = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            list.push(FontListItem {
                id: id.to_string(),
                family: format!("ReaderCustom-{id}"),
                name,
                size,
                updated_at: updated,
            });
        }
    }
    list.sort_by_key(|item| item.updated_at);
    ReturnData::ok_typed(list).into_response()
}

/// GET `/reader3/getFont?id=`
pub async fn get_font(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let id = params.get("id").cloned().unwrap_or_default();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let dir = fonts_dir(&state, &namespace);
    for ext in FONT_EXTENSIONS {
        let path = dir.join(format!("{id}.{ext}"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, font_mime(ext))
                .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

/// POST `/reader3/deleteFont`
pub async fn delete_font(
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
    let id = param_of(&params, body_json.as_ref(), "id");
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Json(ReturnData::err("参数错误"));
    }
    let dir = fonts_dir(&state, &namespace);
    let mut deleted = false;
    for ext in FONT_EXTENSIONS {
        let path = dir.join(format!("{id}.{ext}"));
        if tokio::fs::remove_file(&path).await.is_ok() {
            deleted = true;
        }
    }
    if deleted {
        Json(ReturnData::ok(json!({ "deleted": true })))
    } else {
        Json(ReturnData::err("字体不存在"))
    }
}

// ==================== TTS 与 HttpTTS ====================

/// GET/POST `/reader3/tts`
pub async fn tts_synthesize(
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
    let text = param_of(&params, body_json.as_ref(), "text");
    if text.trim().is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let engine = param_of(&params, body_json.as_ref(), "engine");
    let voice = param_of(&params, body_json.as_ref(), "voice");
    let voice = if voice.is_empty() {
        crate::service::tts::DEFAULT_VOICE.to_string()
    } else {
        voice
    };
    let rate = param_of(&params, body_json.as_ref(), "rate");
    let rate = if rate.is_empty() {
        "+0%".to_string()
    } else {
        rate
    };
    let pitch = param_of(&params, body_json.as_ref(), "pitch");
    let pitch = if pitch.is_empty() {
        "+0Hz".to_string()
    } else {
        pitch
    };
    let volume = param_of(&params, body_json.as_ref(), "volume");
    let volume = if volume.is_empty() {
        "+0%".to_string()
    } else {
        volume
    };
    let style = param_of(&params, body_json.as_ref(), "style");
    let style = if style.is_empty() { None } else { Some(style) };

    let type_param = param_of(&params, body_json.as_ref(), "type");
    let engine_eff = if !type_param.is_empty() {
        type_param
    } else {
        engine
    };
    let base64_flag = param_of(&params, body_json.as_ref(), "base64") == "1";

    if !base64_flag {
        // 默认音频播放模式：走真流式透传（极低首字延迟，附带 X-Accel-Buffering: no）
        let stream_result = match engine_eff.as_str() {
            "edge" => {
                match crate::service::tts::edge_synthesize_stream(
                    &text,
                    &voice,
                    &rate,
                    &pitch,
                    &volume,
                    style.as_deref(),
                )
                .await
                {
                    Ok(s) => Ok((s, "audio/mpeg".to_string())),
                    Err(e) if voice != crate::service::tts::DEFAULT_VOICE || style.is_some() => {
                        tracing::warn!(
                            "Edge TTS 流式合成失败（voice={voice}），回退默认音色重试: {e}"
                        );
                        crate::service::tts::edge_synthesize_stream(
                            &text,
                            crate::service::tts::DEFAULT_VOICE,
                            &rate,
                            &pitch,
                            &volume,
                            None,
                        )
                        .await
                        .map(|s| (s, "audio/mpeg".to_string()))
                    }
                    Err(e) => Err(e),
                }
            }
            "textToSpeechCn" => match crate::service::tts::text_to_speech_cn(&text, &voice).await {
                Ok(url) => {
                    return Response::builder()
                        .status(StatusCode::FOUND)
                        .header(header::LOCATION, url)
                        .body(Body::empty())
                        .unwrap_or_else(|_| Json(ReturnData::err("合成失败")).into_response());
                }
                Err(e) => {
                    tracing::warn!("textToSpeechCn 合成失败，流式回退 edge: {e}");
                    crate::service::tts::edge_synthesize_stream(
                        &text,
                        crate::service::tts::DEFAULT_VOICE,
                        "+0%",
                        "+0Hz",
                        "+0%",
                        None,
                    )
                    .await
                    .map(|s| (s, "audio/mpeg".to_string()))
                }
            },
            _ => {
                let Some(tts) = state
                    .storage
                    .get_http_tts_by_name(&namespace, &voice)
                    .await
                    .ok()
                    .flatten()
                else {
                    return Json(ReturnData::err("听书源不存在")).into_response();
                };
                let rate_f = rate.parse::<f64>().unwrap_or(0.5);
                let speed = (5.0 + (rate_f - 0.5) * 30.0) as i64;
                crate::service::tts::http_tts_api_synthesize_stream(&namespace, &tts, &text, speed)
                    .await
                    .map(|(stream, ct)| (stream, ct.unwrap_or_else(|| "audio/mpeg".to_string())))
            }
        };

        return match stream_result {
            Ok((stream, mime)) => audio_stream_response(stream, &mime),
            Err(e) => {
                tracing::warn!("tts 流式合成失败 [{engine_eff}]: {e}");
                Json(ReturnData::err("合成失败")).into_response()
            }
        };
    }

    // base64=1 模式：走缓冲版编码 Base64 JSON
    let buffered_result = match engine_eff.as_str() {
        "edge" => {
            match crate::service::tts::edge_synthesize(
                &text,
                &voice,
                &rate,
                &pitch,
                &volume,
                style.as_deref(),
            )
            .await
            {
                Ok(audio) => Ok(audio),
                Err(e) if voice != crate::service::tts::DEFAULT_VOICE || style.is_some() => {
                    tracing::warn!("Edge TTS 合成失败（voice={voice}），回退默认音色重试: {e}");
                    crate::service::tts::edge_synthesize(
                        &text,
                        crate::service::tts::DEFAULT_VOICE,
                        &rate,
                        &pitch,
                        &volume,
                        None,
                    )
                    .await
                }
                Err(e) => Err(e),
            }
        }
        "textToSpeechCn" => match crate::service::tts::text_to_speech_cn(&text, &voice).await {
            Ok(url) => {
                return Response::builder()
                    .status(StatusCode::FOUND)
                    .header(header::LOCATION, url)
                    .body(Body::empty())
                    .unwrap_or_else(|_| Json(ReturnData::err("合成失败")).into_response());
            }
            Err(e) => {
                tracing::warn!("textToSpeechCn 合成失败，回退 edge: {e}");
                crate::service::tts::edge_synthesize(
                    &text,
                    crate::service::tts::DEFAULT_VOICE,
                    "+0%",
                    "+0Hz",
                    "+0%",
                    None,
                )
                .await
            }
        },
        _ => {
            let Some(tts) = state
                .storage
                .get_http_tts_by_name(&namespace, &voice)
                .await
                .ok()
                .flatten()
            else {
                return Json(ReturnData::err("听书源不存在")).into_response();
            };
            let rate_f = rate.parse::<f64>().unwrap_or(0.5);
            let speed = (5.0 + (rate_f - 0.5) * 30.0) as i64;
            crate::service::tts::http_tts_api_synthesize(&namespace, &tts, &text, speed)
                .await
                .map(|(bytes, _ct)| bytes)
        }
    };

    match buffered_result {
        Ok(audio) => audio_base64_response(audio),
        Err(e) => {
            tracing::warn!("tts 合成失败 [{engine_eff}]: {e}");
            Json(ReturnData::err("合成失败")).into_response()
        }
    }
}

fn audio_stream_response(
    stream: crate::service::tts::AudioByteStream,
    content_type: &str,
) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Json(ReturnData::err("响应构造失败")).into_response())
}

fn audio_base64_response(audio: Vec<u8>) -> Response {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&audio);
    ReturnData::ok_typed(encoded).into_response()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpTtsItem<'a> {
    id: &'a str,
    url: &'a str,
    name: &'a str,
    #[serde(rename = "type")]
    tts_type: i64,
    content_type: Option<&'a str>,
    concurrent_rate: Option<&'a str>,
    login_url: Option<&'a str>,
    login_ui: Option<&'a str>,
    header: Option<&'a str>,
    js_lib: Option<&'a str>,
    enabled_cookie_jar: Option<bool>,
    login_check_js: Option<&'a str>,
    last_update_time: i64,
}

/// GET/POST `/reader3/getHttpTTSList`
pub async fn get_http_tts_list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    match state.storage.get_http_tts_list(&namespace).await {
        Ok(list) => {
            let arr: Vec<HttpTtsItem<'_>> = list
                .iter()
                .map(|tts| HttpTtsItem {
                    id: &tts.url,
                    url: &tts.url,
                    name: &tts.name,
                    tts_type: tts.tts_type,
                    content_type: tts.content_type.as_deref(),
                    concurrent_rate: tts.concurrent_rate.as_deref(),
                    login_url: tts.login_url.as_deref(),
                    login_ui: tts.login_ui.as_deref(),
                    header: tts.header.as_deref(),
                    js_lib: tts.js_lib.as_deref(),
                    enabled_cookie_jar: tts.enabled_cookie_jar,
                    login_check_js: tts.login_check_js.as_deref(),
                    last_update_time: tts.last_update_time,
                })
                .collect();
            ReturnData::ok_typed(arr).into_response()
        }
        Err(e) => {
            tracing::error!("getHttpTTSList [{namespace}] 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// POST `/reader3/saveHttpTTS`
pub async fn save_http_tts(
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
    let mut tts: crate::model::HttpTts = match serde_json::from_value(json.clone()) {
        Ok(t) => t,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    if tts.url.trim().is_empty() {
        if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
            tts.url = id.to_string();
        }
    }
    if tts.url.trim().is_empty() {
        return Json(ReturnData::err("链接不能为空"));
    }
    if tts.name.trim().is_empty() {
        return Json(ReturnData::err("名称不能为空"));
    }
    tts.user_namespace = namespace.clone();
    match state.storage.save_http_tts(&namespace, &tts).await {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("saveHttpTTS 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/deleteHttpTTS`
pub async fn delete_http_tts(
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
    let mut url = param_of(&params, body_json.as_ref(), "url");
    if url.is_empty() {
        url = param_of(&params, body_json.as_ref(), "id");
    }
    if url.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match state.storage.delete_http_tts(&namespace, &url).await {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("deleteHttpTTS 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

// ==================== WebDAV 备份与恢复 ====================

/// POST `/reader3/backupToWebdav`
pub async fn backup_to_webdav(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    if state.storage.config.secure {
        let user = match state.storage.find_user(&namespace).await {
            Ok(Some(u)) => u,
            _ => return Json(ReturnData::err("请登录后使用")),
        };
        if !user.enable_webdav {
            return Json(ReturnData::err("未开启webdav功能"));
        }
    }
    match state.storage.create_backup_zip(&namespace).await {
        Ok(path) => Json(ReturnData::ok(json!({ "path": path }))),
        Err(e) => {
            tracing::error!("backupToWebdav 失败 [{namespace}]: {e}");
            Json(ReturnData::err("备份失败"))
        }
    }
}

/// POST `/reader3/restoreFromWebdav`
pub async fn restore_from_webdav(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    if state.storage.config.secure {
        let user = match state.storage.find_user(&namespace).await {
            Ok(Some(u)) => u,
            _ => return Json(ReturnData::err("请登录后使用")).into_response(),
        };
        if !user.enable_webdav {
            return Json(ReturnData::err("未开启webdav功能")).into_response();
        }
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let path = param_of(&params, body_json.as_ref(), "path");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let mut overwrite = params
        .get("overwrite")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if let Some(v) = body_json
        .as_ref()
        .and_then(|b| b.get("overwrite"))
        .and_then(|v| v.as_bool())
    {
        overwrite = v;
    }
    let webdav_root = state
        .storage
        .config
        .storage_dir()
        .join("data")
        .join(&namespace)
        .join("webdav");
    let Some(file) = super::files::resolve_secure_path(&webdav_root, &path) else {
        return Json(ReturnData::err("参数错误")).into_response();
    };
    if !tokio::fs::metadata(&file)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return Json(ReturnData::err("路径不存在")).into_response();
    }
    let bytes = match tokio::fs::read(&file).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("restoreFromWebdav 读取失败 [{}]: {e}", file.display());
            return Json(ReturnData::err("读取失败")).into_response();
        }
    };
    match state
        .storage
        .restore_backup_zip(&namespace, &bytes, overwrite)
        .await
    {
        Ok(report) => ReturnData::ok_typed(report).into_response(),
        Err(e) => {
            tracing::error!("restoreFromWebdav 失败 [{namespace}] {path}: {e}");
            Json(ReturnData::err(format!("恢复失败：{e}"))).into_response()
        }
    }
}
