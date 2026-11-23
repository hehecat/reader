//! 书源管理 API（CRUD、统计、失效源、换源候选、远程订阅导入、登录态与 Cookie、调试 SSE、公共源发布）

use std::collections::{HashMap, HashSet};

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::auth::{check_manager_auth, resolve_namespace};
use super::router::{param_of, AppState, ReturnData};
use super::sse;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SimpleBookSourceItem {
    book_source_group: Option<String>,
    book_source_name: String,
    book_source_url: String,
    enabled: bool,
    book_source_type: i64,
    published: bool,
}

#[derive(serde::Serialize)]
struct FullBookSourceItem {
    #[serde(flatten)]
    source: crate::model::BookSource,
    published: bool,
}

#[derive(serde::Serialize)]
struct RemoteSourcePreview {
    sources: Vec<crate::model::BookSource>,
    existing: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceStatItem<'a> {
    source_url: &'a str,
    confidence: Option<f64>,
    confidence_lo: Option<f64>,
    attempts: i64,
    effective_samples: i64,
    success_rate: f64,
    avg_latency_ms: i64,
    consec_failures: i64,
    last_failure_at: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InvalidBookSourceItem {
    source_url: String,
    time: i64,
    error: String,
    error_msg: String,
    book_source_url: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceCookieItem<'a> {
    source_url: &'a str,
    cookie: &'a str,
    user_agent: &'a str,
    login_header: &'a str,
    updated_at: i64,
}

/// GET/POST `/reader3/getBookSources`：按命名空间返回书源（用户无书源回退 `default`；`simple=1` 返回轻量变体）
pub async fn get_book_sources(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let simple = params
        .get("simple")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        > 0;
    let published: HashSet<String> = sqlx::query_scalar(
        "SELECT book_source_url FROM book_sources WHERE user_namespace = 'default'",
    )
    .fetch_all(&state.storage.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .collect();
    match state.storage.get_book_sources(&namespace).await {
        Ok(sources) => {
            if simple {
                let out: Vec<SimpleBookSourceItem> = sources
                    .into_iter()
                    .map(|s| {
                        let is_public = published.contains(&s.book_source_url);
                        SimpleBookSourceItem {
                            book_source_group: s.book_source_group,
                            book_source_name: s.book_source_name,
                            book_source_url: s.book_source_url,
                            enabled: s.enabled,
                            book_source_type: s.book_source_type,
                            published: is_public,
                        }
                    })
                    .collect();
                ReturnData::ok_typed(out).into_response()
            } else {
                let out: Vec<FullBookSourceItem> = sources
                    .into_iter()
                    .map(|source| {
                        let is_public = published.contains(&source.book_source_url);
                        FullBookSourceItem {
                            source,
                            published: is_public,
                        }
                    })
                    .collect();
                ReturnData::ok_typed(out).into_response()
            }
        }
        Err(e) => {
            tracing::error!("getBookSources [{namespace}] 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// GET/POST `/reader3/getBookSource`：单个书源
pub async fn get_book_source(
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
    let mut url = param_of(&params, body_json.as_ref(), "bookSourceUrl");
    if url.is_empty() {
        url = param_of(&params, body_json.as_ref(), "url");
    }
    if url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    match state.storage.find_book_source(&namespace, &url).await {
        Ok(Some(s)) => ReturnData::ok_typed(s).into_response(),
        Ok(None) => Json(ReturnData::err("书源不存在")).into_response(),
        Err(_) => Json(ReturnData::err("系统错误")).into_response(),
    }
}

async fn book_source_limit_exceeded(
    storage: &crate::storage::Storage,
    ns: &str,
    candidate_urls: &[&str],
) -> Result<bool, ()> {
    let Some(limit) = storage.book_source_limit_for(ns).await.ok().flatten() else {
        return Ok(false);
    };
    if limit <= 0 {
        return Ok(false);
    }
    let existing_list = storage.get_book_sources(ns).await.map_err(|_| ())?;
    let existing_set: HashSet<&str> = existing_list
        .iter()
        .map(|s| s.book_source_url.as_str())
        .collect();
    let mut seen_new: HashSet<&str> = HashSet::new();
    for &url in candidate_urls {
        if !existing_set.contains(url) {
            seen_new.insert(url);
        }
    }
    match storage.count_book_sources(ns).await {
        Ok(count) => Ok(count + seen_new.len() as i64 > limit),
        Err(_) => Err(()),
    }
}

/// POST `/reader3/saveBookSource`：保存单个书源
pub async fn save_book_source(
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
    let source: crate::model::BookSource =
        match serde_json::from_slice::<Value>(&body).ok().and_then(|v| {
            crate::model::book_source::normalize_book_sources(v)
                .into_iter()
                .next()
        }) {
            Some(s) => s,
            None => return Json(ReturnData::err("参数错误")),
        };
    if source.book_source_url.trim().is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match book_source_limit_exceeded(&state.storage, &namespace, &[&source.book_source_url]).await {
        Ok(true) => return Json(ReturnData::err("超过书源数上限")),
        Ok(false) => {}
        Err(()) => return Json(ReturnData::err("系统错误")),
    }
    match state.storage.save_book_source(&namespace, &source).await {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("saveBookSource 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/saveBookSources`：批量保存书源（支持超大订阅包与宽松字段归一化）
pub async fn save_book_sources(
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
        return Json(ReturnData::err("参数错误（请求体为空或超过大小限制）"));
    };
    let raw_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    let mut sources = crate::model::book_source::normalize_book_sources(raw_json);
    sources.retain(|s| !s.book_source_url.trim().is_empty());
    if sources.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let urls: Vec<&str> = sources.iter().map(|s| s.book_source_url.as_str()).collect();
    match book_source_limit_exceeded(&state.storage, &namespace, &urls).await {
        Ok(true) => return Json(ReturnData::err("超过书源数上限")),
        Ok(false) => {}
        Err(()) => return Json(ReturnData::err("系统错误")),
    }
    match state.storage.save_book_sources(&namespace, &sources).await {
        Ok(_) => Json(ReturnData::ok(json!({ "count": sources.len() }))),
        Err(e) => {
            tracing::error!("saveBookSources 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/saveFromRemoteSource`：远程书源订阅导入（支持 `preview=1`）
pub async fn save_from_remote_source(
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
        return Json(ReturnData::err("请输入远程书源链接")).into_response();
    }
    let headers_map: HashMap<String, String> = HashMap::new();
    let resp = match crate::service::crawler::fetch(&url, &headers_map, 15, "GET", None, None, None)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("saveFromRemoteSource 抓取失败 [{url}]: {e}");
            return Json(ReturnData::err("远程书源链接错误")).into_response();
        }
    };
    let json: Value = match serde_json::from_str(&resp.body) {
        Ok(v) => v,
        Err(_) => return Json(ReturnData::err("书源数据格式错误")).into_response(),
    };
    let sources = crate::model::book_source::normalize_book_sources(json);
    if sources.is_empty() || sources.iter().any(|s| s.book_source_url.trim().is_empty()) {
        return Json(ReturnData::err("书源数据格式错误")).into_response();
    }
    if params.get("preview").map(String::as_str) == Some("1") {
        let mut existing = Vec::new();
        for s in &sources {
            if state
                .storage
                .find_book_source(&namespace, &s.book_source_url)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                existing.push(s.book_source_url.clone());
            }
        }
        return ReturnData::ok_typed(RemoteSourcePreview { sources, existing }).into_response();
    }
    let urls: Vec<&str> = sources.iter().map(|s| s.book_source_url.as_str()).collect();
    match book_source_limit_exceeded(&state.storage, &namespace, &urls).await {
        Ok(true) => return Json(ReturnData::err("超过书源数上限")).into_response(),
        Ok(false) => {}
        Err(()) => return Json(ReturnData::err("系统错误")).into_response(),
    }
    match state.storage.save_book_sources(&namespace, &sources).await {
        Ok(_) => Json(ReturnData::ok(json!({ "count": sources.len() }))).into_response(),
        Err(e) => {
            tracing::error!("saveFromRemoteSource 入库失败: {e}");
            Json(ReturnData::err("保存失败")).into_response()
        }
    }
}

/// POST `/reader3/deleteBookSource`：删除单个书源
pub async fn delete_book_source(
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
    let mut url = param_of(&params, body_json.as_ref(), "bookSourceUrl");
    if url.is_empty() {
        url = param_of(&params, body_json.as_ref(), "url");
    }
    if url.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match state.storage.delete_book_source(&namespace, &url).await {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("deleteBookSource 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/deleteBookSources`：批量删除书源
pub async fn delete_book_sources(
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
    let raw: Vec<Value> = match serde_json::from_slice(&body) {
        Ok(u) => u,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    let urls: Vec<String> = raw
        .into_iter()
        .filter_map(|v| match v {
            Value::String(t) => Some(t),
            Value::Object(m) => m
                .get("bookSourceUrl")
                .or_else(|| m.get("url"))
                .and_then(|u| u.as_str())
                .map(str::to_string),
            _ => None,
        })
        .filter(|u| !u.is_empty())
        .collect();
    let mut deleted = 0u64;
    for url in &urls {
        if let Ok(n) = state.storage.delete_book_source(&namespace, url).await {
            deleted += n;
        }
    }
    Json(ReturnData::ok(json!({ "deleted": deleted })))
}

/// POST `/reader3/setAsDefaultBookSources`：管理员发布/下架公共书源
pub async fn set_as_default_book_sources(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let body_json = body
        .as_ref()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok());
    if let Err(ret) = check_manager_auth(&state, &params, &headers, body_json.as_ref()).await {
        return Json(ret);
    }
    let urls: Vec<String> = body_json
        .as_ref()
        .and_then(|v| v.get("bookSources"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.as_str().map(str::to_string).or_else(|| {
                        item.get("bookSourceUrl")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let mode = param_of(&params, body_json.as_ref(), "mode");
    if mode == "remove" {
        let mut removed = 0i64;
        for url in &urls {
            removed += state
                .storage
                .delete_book_source("default", url)
                .await
                .unwrap_or(0) as i64;
        }
        tracing::info!("公共源下架 {removed} 条（{namespace}）");
        return Json(ReturnData::ok(json!({ "removed": removed })));
    }
    let from = {
        let explicit = param_of(&params, body_json.as_ref(), "from");
        if explicit.trim().is_empty() {
            namespace.clone()
        } else {
            explicit
        }
    };
    let all = state
        .storage
        .get_book_sources(&from)
        .await
        .unwrap_or_default();
    let mut published = 0i64;
    let mut missing: Vec<String> = Vec::new();
    for url in &urls {
        match all.iter().find(|s| &s.book_source_url == url) {
            Some(source) => {
                let mut source = source.clone();
                source.user_namespace = "default".to_string();
                match state.storage.save_book_source("default", &source).await {
                    Ok(_) => published += 1,
                    Err(e) => {
                        tracing::error!("发布公共源失败 [{url}]: {e}");
                        missing.push(url.clone());
                    }
                }
            }
            None => missing.push(url.clone()),
        }
    }
    let _ = state
        .storage
        .set_default_book_sources(&namespace, &urls)
        .await;
    tracing::info!("发布公共源 {published} 条（来源 {from}, 操作者 {namespace}）");
    Json(ReturnData::ok(
        json!({ "published": published, "missing": missing }),
    ))
}

/// GET/POST `/reader3/getSourceStats`：书源置信度统计
pub async fn get_source_stats_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let stats = state
        .storage
        .get_source_stats(&namespace)
        .await
        .unwrap_or_default();
    let list: Vec<SourceStatItem<'_>> = stats
        .iter()
        .map(|st| SourceStatItem {
            source_url: &st.source_url,
            confidence: st.confidence().map(|c| (c * 100.0).round() / 100.0),
            confidence_lo: st.confidence_lo().map(|c| (c * 100.0).round() / 100.0),
            attempts: st.attempts,
            effective_samples: st.effective_samples(),
            success_rate: (st.success_rate() * 100.0).round() / 100.0,
            avg_latency_ms: st.avg_latency_ms(),
            consec_failures: st.consec_failures,
            last_failure_at: st.last_failure_at,
        })
        .collect();
    ReturnData::ok_typed(list).into_response()
}

/// GET/POST `/reader3/getInvalidBookSources`：运行期失效书源快照
pub async fn get_invalid_book_sources(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let arr: Vec<InvalidBookSourceItem> = crate::service::health::invalid_snapshot(&namespace)
        .into_iter()
        .map(|(source_url, time, error)| InvalidBookSourceItem {
            book_source_url: source_url.clone(),
            source_url,
            time,
            error_msg: error.clone(),
            error,
        })
        .collect();
    ReturnData::ok_typed(arr).into_response()
}

/// 搜索命中并入换源候选表
pub(crate) async fn persist_search_candidates(
    storage: &crate::storage::Storage,
    ns: &str,
    books: &[crate::service::search::SearchBook],
) {
    const MAX_BOOKS: usize = 200;
    let mut by_key: HashMap<String, Vec<crate::service::search::SearchBook>> = HashMap::new();
    for b in books.iter().take(MAX_BOOKS) {
        let name = b.name.trim();
        let author = b.author.trim();
        if name.is_empty() || b.origin.is_empty() {
            continue;
        }
        by_key
            .entry(format!("{name}_{author}"))
            .or_default()
            .push(b.clone());
    }
    for (key, fresh) in by_key {
        let old = storage
            .get_book_candidates(ns, &key)
            .await
            .unwrap_or_default();
        let mut merged = fresh;
        for o in old {
            if !merged.iter().any(|n| n.origin == o.origin) {
                merged.push(o);
            }
        }
        if let Err(e) = storage.save_book_candidates(ns, &key, &merged).await {
            tracing::debug!("搜索候选写入失败 [{key}]: {e}");
        }
    }
}

/// GET/POST `/reader3/getAvailableBookSource`：换源候选列表（支持 `refresh>0` 全源重搜）
pub async fn get_available_book_source(
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
        return Json(ReturnData::err("请输入书籍链接")).into_response();
    }
    let refresh = param_of(&params, body_json.as_ref(), "refresh")
        .parse::<i64>()
        .unwrap_or(0);
    let book = match state.storage.find_book(&namespace, &url).await {
        Ok(Some(b)) => b,
        Ok(None) => return Json(ReturnData::err("书籍信息错误")).into_response(),
        Err(e) => {
            tracing::error!("getAvailableBookSource 查询失败 [{url}]: {e}");
            return Json(ReturnData::err("系统错误")).into_response();
        }
    };
    if book.name.is_empty() {
        return Json(ReturnData::err("书籍信息错误")).into_response();
    }
    let key = format!("{}_{}", book.name.trim(), book.author.trim());

    let cached = state
        .storage
        .get_book_candidates(&namespace, &key)
        .await
        .unwrap_or_default();
    if refresh <= 0 {
        return ReturnData::ok_typed(cached).into_response();
    }

    let sources: Vec<crate::model::BookSource> =
        match state.storage.get_book_sources(&namespace).await {
            Ok(s) => s
                .into_iter()
                .filter(|s| s.enabled && s.search_url.is_some())
                .collect(),
            Err(_) => return Json(ReturnData::err("系统错误")).into_response(),
        };
    if sources.is_empty() {
        return ReturnData::ok_typed(Vec::<crate::service::search::SearchBook>::new())
            .into_response();
    }

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    let name = book.name.trim().to_string();
    let ns = namespace.clone();
    let storage = state.storage.clone();
    let mut handles = Vec::with_capacity(sources.len());
    for source in sources {
        let sem = semaphore.clone();
        let key = name.clone();
        let ns = ns.clone();
        let storage = storage.clone();
        let scoped_storage = storage.clone();
        handles.push(tokio::spawn(crate::storage::scope_task_storage(
            scoped_storage,
            async move {
                let _permit = sem.acquire().await;
                crate::service::search::search_one_source(&storage, &ns, &source, &key, 1, 15)
                    .await
                    .unwrap_or_default()
            },
        )));
    }
    const EARLY_HITS: usize = 60;
    let budget = std::time::Duration::from_secs(
        std::env::var("READER_SWITCH_SEARCH_BUDGET_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60),
    );
    let deadline = tokio::time::Instant::now() + budget;
    let mut rest = handles;
    let mut all: Vec<crate::service::search::SearchBook> = Vec::new();
    loop {
        if rest.is_empty() || all.len() >= EARLY_HITS {
            break;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            tracing::warn!("换源全量搜索超预算 {}s，返回已收集", budget.as_secs());
            break;
        }
        match tokio::time::timeout(left, futures::future::select_all(rest)).await {
            Ok((joined, _idx, remaining)) => {
                rest = remaining;
                if let Ok(books) = joined {
                    all.extend(books);
                }
            }
            Err(_) => {
                tracing::warn!("换源全量搜索超预算 {}s，返回已收集", budget.as_secs());
                break;
            }
        }
    }
    let mut seen = HashSet::new();
    let mut matched: Vec<crate::service::search::SearchBook> = all
        .into_iter()
        .filter(|b| crate::service::search::exact_match(b, &name))
        .filter(|b| seen.insert(b.origin.clone()))
        .collect();
    for c in cached {
        if !matched.iter().any(|n| n.origin == c.origin) {
            matched.push(c);
        }
    }
    let _ = state
        .storage
        .save_book_candidates(&namespace, &key, &matched)
        .await;
    ReturnData::ok_typed(matched).into_response()
}

// ---------------- 书源登录与 Cookie ----------------

fn merge_login_params(
    query: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> HashMap<String, String> {
    let mut m = query.clone();
    let Some(body) = body else { return m };
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let Some(s) = val.as_str() {
                    m.insert(k.clone(), s.to_string());
                } else {
                    m.insert(k.clone(), val.to_string());
                }
            }
            return m;
        }
    }
    for (k, v) in url::form_urlencoded::parse(body) {
        m.insert(k.into_owned(), v.into_owned());
    }
    m
}

/// POST/GET `/reader3/loginBookSource`：书源登录
#[axum::debug_handler]
pub async fn login_book_source(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let merged = merge_login_params(&params, body.as_deref());
    let book_source_param = merged.get("bookSource").cloned().unwrap_or_default();
    if book_source_param.is_empty() {
        return Json(ReturnData::err("缺少 bookSource 参数"));
    }
    let Some(source) = resolve_book_source(&state, &namespace, &book_source_param).await else {
        return Json(ReturnData::err("书源不存在（请先导入书源）"));
    };
    if source.login_url.as_deref().unwrap_or("").trim().is_empty() {
        return Json(ReturnData::err("书源未配置 loginUrl"));
    }
    let req = crate::service::login::LoginRequest {
        username: merged.get("username").cloned().unwrap_or_default(),
        password: merged.get("password").cloned().unwrap_or_default(),
        captcha: merged.get("captcha").cloned().unwrap_or_default(),
    };
    let mode = merged.get("mode").cloned().unwrap_or_default();
    let outcome = if mode == "browser" {
        if !crate::service::browser::is_browser_available() {
            return Json(ReturnData::err(
                "浏览器后端不可用（camoufox）——无法使用浏览器自动登录，请配置 READER_CAMOUFOX_URL，或在书源设置粘贴 Cookie",
            ));
        }
        crate::service::login::login_browser(&state.storage, &namespace, &source, &req).await
    } else {
        crate::service::login::login_http(&state.storage, &namespace, &source, &req).await
    };
    match outcome {
        Ok(crate::service::login::LoginOutcome::Success { cookie }) => Json(ReturnData::ok(
            json!({ "success": true, "needCaptcha": false, "cookie": cookie }),
        )),
        Ok(crate::service::login::LoginOutcome::NeedImageCaptcha {
            captcha_url,
            captcha_id,
            message,
        }) => Json(ReturnData::ok(json!({
            "success": false,
            "needCaptcha": true,
            "captchaUrl": captcha_url,
            "captchaId": captcha_id,
            "message": message,
        }))),
        Ok(crate::service::login::LoginOutcome::NeedManualCookie { message }) => {
            Json(ReturnData::ok(
                json!({ "success": false, "needManualCaptcha": true, "message": message }),
            ))
        }
        Ok(crate::service::login::LoginOutcome::Failed { message }) => Json(ReturnData::ok(
            json!({ "success": false, "message": message }),
        )),
        Err(e) => {
            tracing::error!("loginBookSource 失败 [{}]: {e}", source.book_source_name);
            Json(ReturnData::err(e.to_string()))
        }
    }
}

/// POST `/reader3/setBookSourceCookie`：手动设置或清除书源 Cookie
pub async fn set_book_source_cookie(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let merged = merge_login_params(&params, body.as_deref());
    let book_source = merged.get("bookSource").cloned().unwrap_or_default();
    if book_source.is_empty() {
        return Json(ReturnData::err("缺少 bookSource 参数"));
    }
    let cookie = merged.get("cookie").cloned().unwrap_or_default();
    if cookie.trim().is_empty() {
        match state.storage.clear_cookie(&namespace, &book_source).await {
            Ok(_) => Json(ReturnData::ok(json!({ "success": true, "cleared": true }))),
            Err(e) => {
                tracing::error!("setBookSourceCookie 清除失败: {e}");
                Json(ReturnData::err("清除失败"))
            }
        }
    } else {
        match state
            .storage
            .set_cookie(&namespace, &book_source, &cookie)
            .await
        {
            Ok(_) => Json(ReturnData::ok(json!({ "success": true }))),
            Err(e) => {
                tracing::error!("setBookSourceCookie 写入失败: {e}");
                Json(ReturnData::err("保存失败"))
            }
        }
    }
}

/// GET/POST `/reader3/getBookSourceCookie`：读取当前用户全部书源登录态
pub async fn get_book_source_cookie(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    match state.storage.list_cookies(&namespace).await {
        Ok(rows) => {
            let arr: Vec<SourceCookieItem<'_>> = rows
                .iter()
                .map(|r| SourceCookieItem {
                    source_url: &r.source_url,
                    cookie: &r.cookie,
                    user_agent: &r.user_agent,
                    login_header: &r.login_header,
                    updated_at: r.updated_at,
                })
                .collect();
            ReturnData::ok_typed(arr).into_response()
        }
        Err(e) => {
            tracing::error!("getBookSourceCookie [{namespace}] 失败: {e}");
            Json(ReturnData::err("读取失败")).into_response()
        }
    }
}

/// GET/POST `/reader3/bookSourceDebugSSE`：书源调试流（统一走 `api::sse`）
pub async fn book_source_debug_sse(
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
    let action = param_of(&params, body_json.as_ref(), "action");
    let key = param_of(&params, body_json.as_ref(), "key");
    let mut target = param_of(&params, body_json.as_ref(), "chapterUrl");
    if target.is_empty() {
        target = param_of(&params, body_json.as_ref(), "url");
    }
    if !matches!(action.as_str(), "search" | "explore" | "toc" | "content") {
        return sse::sse_error(ReturnData::err(
            "请输入调试动作（search|explore|toc|content）",
        ));
    }
    if action == "search" && key.is_empty() {
        return sse::sse_error(ReturnData::err("请输入搜索关键字"));
    }
    let bs_param = param_of(&params, body_json.as_ref(), "bookSource");
    let Some(source) = resolve_book_source(&state, &namespace, &bs_param).await else {
        return sse::sse_error(ReturnData::err("书源不存在"));
    };

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let ns = namespace.clone();
    let storage = state.storage.clone();
    tokio::spawn(crate::storage::scope_task_storage(storage, async move {
        let _ = tx.send(sse::data_event(&json!({
            "type": "start",
            "message": { "action": action, "bookSource": source.book_source_name },
        })));
        let tx_step = tx.clone();
        let result = tokio::select! {
            _ = tx.closed() => return,
            res = crate::service::debug::run_debug(&ns, &source, &action, &key, &target, move |step| {
                if !tx_step.is_closed() {
                    let _ = tx_step.send(sse::data_event(&json!({ "type": "step", "message": step })));
                }
            }) => res,
        };
        if tx.is_closed() {
            return;
        }
        match result {
            Ok(data) => {
                let _ = tx.send(sse::data_event(&json!({ "type": "result", "data": data })));
            }
            Err(e) => {
                let _ = tx.send(sse::data_event(
                    &json!({ "type": "error", "message": e.to_string() }),
                ));
            }
        }
    }));

    sse::sse_from_unbounded(rx)
}

// ---------------- 书源解析与通用辅助 ----------------

pub(crate) async fn resolve_book_source(
    state: &AppState,
    ns: &str,
    param: &str,
) -> Option<crate::model::BookSource> {
    if param.trim_start().starts_with('{') {
        serde_json::from_str(param).ok()
    } else {
        state
            .storage
            .find_book_source(ns, param)
            .await
            .ok()
            .flatten()
    }
}

/// `bookSource` 缺省时按书籍/章节 URL 域名回算书源
pub(crate) async fn resolve_book_source_for_url(
    state: &AppState,
    ns: &str,
    param: &str,
    book_url: &str,
) -> Option<crate::model::BookSource> {
    if !param.trim().is_empty() {
        return resolve_book_source(state, ns, param).await;
    }
    let host = url::Url::parse(book_url).ok()?.host_str()?.to_string();
    if let Ok(sources) = state.storage.get_book_sources(ns).await {
        if let Some(hit) = sources.into_iter().find(|s| {
            url::Url::parse(&s.book_source_url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
                .is_some_and(|h| h == host)
        }) {
            return Some(hit);
        }
    }
    resolve_book_source(state, ns, param).await
}

/// 书源分组匹配（支持逗号/顿号/空白分隔）
pub(crate) fn book_source_group_matches(group: &str, source_groups: Option<&str>) -> bool {
    if group.is_empty() {
        return true;
    }
    source_groups
        .map(|g| {
            g.split([',', '，', '、', ' ', '\t'])
                .any(|part| part == group)
        })
        .unwrap_or(false)
}

/// 书源使用统计：正文抓取成功计数
pub(crate) async fn bump_source_use(state: &AppState, ns: &str, source: &crate::model::BookSource) {
    if let Err(e) = state
        .storage
        .bump_book_source_use(ns, &source.book_source_url)
        .await
    {
        tracing::debug!("书源使用计数失败 [{}]: {e}", source.book_source_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_matching_handles_multi_delimiters() {
        assert!(book_source_group_matches("", None));
        assert!(book_source_group_matches("玄幻", Some("精品,玄幻、完本")));
        assert!(!book_source_group_matches("科幻", Some("精品,玄幻")));
    }

    #[test]
    fn merge_login_params_prefers_json_and_supports_form() {
        let mut q = HashMap::new();
        q.insert("bookSource".into(), "https://a.test".into());
        let json_body = br#"{"username":"u1","password":"p1"}"#;
        let m = merge_login_params(&q, Some(json_body));
        assert_eq!(m.get("username").map(String::as_str), Some("u1"));
        assert_eq!(
            m.get("bookSource").map(String::as_str),
            Some("https://a.test")
        );

        let form_body = b"username=u2&cookie=a%3D1";
        let m2 = merge_login_params(&q, Some(form_body));
        assert_eq!(m2.get("username").map(String::as_str), Some("u2"));
        assert_eq!(m2.get("cookie").map(String::as_str), Some("a=1"));
    }
}
