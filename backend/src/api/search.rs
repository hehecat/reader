//! 搜索与书海探索 API（多源 SSE 搜索、换源 SSE 搜索、单源搜索、书海探索）

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde_json::{json, Value};

use super::auth::{require_book_source_permission, resolve_namespace};
use super::router::{param_of, AppState, ReturnData};
use super::sources::{book_source_group_matches, persist_search_candidates, resolve_book_source};
use super::sse;

const SORT_UNKNOWN: f64 = 0.3;
const SSE_RAMP_STEP_MS: u64 = 150;
const SSE_RAMP_FREE_SOURCES: usize = 8;
const SSE_SESSION_IDLE_MS: i64 = 15 * 60 * 1000;
const SSE_SESSION_MAX: usize = 64;

fn json_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

fn int_param_of(
    params: &HashMap<String, String>,
    body: Option<&Value>,
    keys: &[&str],
) -> Option<i64> {
    if let Some(b) = body {
        for k in keys {
            if let Some(v) = b.get(*k).and_then(json_i64) {
                return Some(v);
            }
        }
    }
    for k in keys {
        if let Some(v) = params.get(*k).and_then(|s| s.trim().parse::<i64>().ok()) {
            return Some(v);
        }
    }
    None
}

fn str_param_of(params: &HashMap<String, String>, body: Option<&Value>, keys: &[&str]) -> String {
    if let Some(b) = body {
        for k in keys {
            if let Some(v) = b.get(*k) {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() {
                        return s.to_string();
                    }
                } else if !v.is_null() {
                    return v.to_string();
                }
            }
        }
    }
    for k in keys {
        if let Some(s) = params.get(*k) {
            if !s.is_empty() {
                return s.clone();
            }
        }
    }
    String::new()
}

/// GET/POST `/reader3/searchBook`：单源 JSON 搜索
pub async fn search_book(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body
        .as_ref()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok());
    if let Err(ret) = require_book_source_permission(&state, &namespace).await {
        return Json(ret).into_response();
    }
    let mut key = params.get("key").cloned().unwrap_or_default();
    let mut key_exact_prefix = false;
    if key.starts_with('=') {
        key_exact_prefix = true;
        key.remove(0);
    }
    let page = int_param_of(&params, body_json.as_ref(), &["page"]).unwrap_or(1i64);
    let book_source_param = str_param_of(
        &params,
        body_json.as_ref(),
        &["bookSource", "bookSourceUrl"],
    );
    if let Some(json) = body_json.as_ref() {
        if let Some(v) = json.get("key").and_then(|v| v.as_str()) {
            key = v.to_string();
        }
    }
    if key.is_empty() {
        return Json(ReturnData::err("请输入搜索关键字")).into_response();
    }
    if book_source_param.is_empty() {
        return Json(ReturnData::err("未配置书源")).into_response();
    }
    let Some(source) = resolve_book_source(&state, &namespace, &book_source_param).await else {
        return Json(ReturnData::err("书源不存在")).into_response();
    };

    match crate::service::search::search_one_source(
        &state.storage,
        &namespace,
        &source,
        &key,
        page,
        search_timeout_param(&params, body_json.as_ref()),
    )
    .await
    {
        Ok(books) => {
            let exact = key_exact_prefix || params.get("exact").map(|v| v == "1").unwrap_or(false);
            let books = if exact {
                crate::service::search::filter_exact(books, &key)
            } else {
                books
            };
            ReturnData::ok_typed(books).into_response()
        }
        Err(e) => {
            tracing::error!("搜索失败 [{}]: {e:?}", source.book_source_name);
            Json(ReturnData::err("搜索失败")).into_response()
        }
    }
}

/// GET/POST `/reader3/exploreBook`：书海分类发现（JSON Body 优先，Query 兜底）
const EXPLORE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const EXPLORE_CACHE_MAX: usize = 128;
type ExploreCacheKey = (String, String, String, i64);
type ExploreCacheMap =
    HashMap<ExploreCacheKey, (std::time::Instant, Vec<crate::service::search::SearchBook>)>;

fn explore_cache() -> &'static parking_lot::Mutex<ExploreCacheMap> {
    static C: std::sync::OnceLock<parking_lot::Mutex<ExploreCacheMap>> = std::sync::OnceLock::new();
    C.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

pub async fn explore_book(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    if let Err(ret) = require_book_source_permission(&state, &namespace).await {
        return Json(ret).into_response();
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let url = str_param_of(&params, body_json.as_ref(), &["url", "ruleFindUrl"]);
    if url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let page = int_param_of(&params, body_json.as_ref(), &["page"]).unwrap_or(1);
    let refresh = int_param_of(&params, body_json.as_ref(), &["refresh"]).unwrap_or(0) > 0;
    let timeout_secs = int_param_of(&params, body_json.as_ref(), &["timeout"])
        .unwrap_or(8)
        .clamp(3, 20) as u64;
    let bs_param = str_param_of(
        &params,
        body_json.as_ref(),
        &["bookSource", "bookSourceUrl"],
    );
    let Some(source) = resolve_book_source(&state, &namespace, &bs_param).await else {
        return Json(ReturnData::err("书源不存在")).into_response();
    };
    let cache_key = (
        namespace.clone(),
        source.book_source_url.clone(),
        url.clone(),
        page,
    );
    if !refresh {
        if let Some(cached) = explore_cache()
            .lock()
            .get(&cache_key)
            .filter(|(at, _)| at.elapsed() < EXPLORE_CACHE_TTL)
            .map(|(_, v)| v.clone())
        {
            return ReturnData::ok_typed(cached).into_response();
        }
    }
    let explore_res = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        crate::service::explore::explore_url(&namespace, &url, page, &source),
    )
    .await
    {
        Ok(res) => res,
        Err(_) => Err(anyhow::anyhow!("源站探索响应超时（>{timeout_secs}s）")),
    };
    match explore_res {
        Ok(books) => {
            crate::service::health::clear_source_invalid(&namespace, &source.book_source_url);
            let _ = state
                .storage
                .record_path_outcome(
                    &namespace,
                    &source.book_source_url,
                    "explore",
                    !books.is_empty(),
                )
                .await;
            if !books.is_empty() {
                let mut m = explore_cache().lock();
                if m.len() >= EXPLORE_CACHE_MAX && !m.contains_key(&cache_key) {
                    if let Some(oldest) = m
                        .iter()
                        .min_by_key(|(_, (t, _))| *t)
                        .map(|(k, _)| k.clone())
                    {
                        m.remove(&oldest);
                    }
                }
                m.insert(cache_key, (std::time::Instant::now(), books.clone()));
            }
            ReturnData::ok_typed(books).into_response()
        }
        Err(e) => {
            crate::service::health::mark_source_invalid(
                &namespace,
                &source.book_source_url,
                &e.to_string(),
            );
            let _ = state
                .storage
                .record_path_outcome(&namespace, &source.book_source_url, "explore", false)
                .await;
            tracing::error!("exploreBook 失败 [{url}]: {e}");
            Json(ReturnData::err(format!("探索失败：{e}"))).into_response()
        }
    }
}

/// GET/POST `/reader3/searchBookSourceSSE`：流式换源搜索（统一走 `api::sse`）
pub async fn search_book_source_sse(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return sse::sse_error(ret),
    };
    if let Err(ret) = require_book_source_permission(&state, &namespace).await {
        return sse::sse_error(ret);
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let url = param_of(&params, body_json.as_ref(), "url");
    let book_source_param = param_of(&params, body_json.as_ref(), "bookSource");
    if url.is_empty() {
        return sse::sse_error(ReturnData::err("请输入书籍链接"));
    }
    if book_source_param.is_empty() {
        return sse::sse_error(ReturnData::err("未配置书源"));
    }

    let name = match state.storage.find_book(&namespace, &url).await {
        Ok(Some(b)) => b.name,
        _ => {
            let Some(source) = resolve_book_source(&state, &namespace, &book_source_param).await
            else {
                return sse::sse_error(ReturnData::err("书源不存在"));
            };
            match crate::service::book::fetch_book_info(&namespace, &url, &source, None).await {
                Ok(info) => {
                    if info.name.is_empty() {
                        return sse::sse_error(ReturnData::err("获取书籍信息失败"));
                    }
                    info.name
                }
                Err(e) => {
                    tracing::error!("searchBookSourceSSE 获取书名失败 [{url}]: {e}");
                    return sse::sse_error(ReturnData::err("获取书籍信息失败"));
                }
            }
        }
    };
    let key = name.trim().to_string();
    if key.is_empty() {
        return sse::sse_error(ReturnData::err("无法获取书名"));
    }

    let current = book_source_param.trim();
    let sources: Vec<crate::model::BookSource> =
        match state.storage.get_book_sources(&namespace).await {
            Ok(s) => s
                .into_iter()
                .filter(|s| {
                    s.enabled
                        && s.search_url.is_some()
                        && s.book_source_url != current
                        && s.book_source_name != current
                })
                .collect(),
            Err(_) => return sse::sse_error(ReturnData::err("系统错误")),
        };

    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let ns = namespace.clone();
    let storage = state.storage.clone();
    let scoped_storage = storage.clone();
    tokio::spawn(crate::storage::scope_task_storage(
        scoped_storage,
        async move {
            if sources.is_empty() {
                let _ = tx
                    .send(sse::named_event(
                        "end",
                        &json!({ "lastIndex": -1, "isEnd": true }),
                    ))
                    .await;
                return;
            }
            let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(24));
            let mut tasks = futures::stream::FuturesUnordered::new();
            for (i, source) in sources.into_iter().enumerate() {
                let sem = semaphore.clone();
                let key = key.clone();
                let ns = ns.clone();
                let storage = storage.clone();
                tasks.push(Box::pin(async move {
                    let _permit = sem.acquire().await;
                    let t0 = Instant::now();
                    let res = crate::service::search::search_one_source(
                        &storage, &ns, &source, &key, 1, 15,
                    )
                    .await;
                    let lat = t0.elapsed().as_millis() as i64;
                    let (outcome, intro_hits, intro_count) = match &res {
                        Ok(books) => (
                            crate::storage::SearchOutcome::Ok,
                            books
                                .iter()
                                .filter(|b| {
                                    b.intro.as_deref().is_some_and(|t| !t.trim().is_empty())
                                })
                                .count() as i64,
                            books.len() as i64,
                        ),
                        Err(e) => (crate::storage::SearchOutcome::from_error(e), 0, 0),
                    };
                    let surl = source.book_source_url.clone();
                    let st = storage.clone();
                    let scoped_st = st.clone();
                    let nsx = ns.clone();
                    tokio::spawn(crate::storage::scope_task_storage(scoped_st, async move {
                        let _ = st
                            .record_search_outcome_state(
                                &nsx,
                                &surl,
                                outcome,
                                lat,
                                intro_hits,
                                intro_count,
                            )
                            .await;
                    }));
                    (i as i64, res.unwrap_or_default())
                }));
            }

            let mut last = -1i64;
            let mut seen = HashSet::new();
            let ql = key.to_lowercase();
            loop {
                let next = tokio::select! {
                    _ = tx.closed() => return,
                    item = tasks.next() => item,
                };
                let Some((i, books)) = next else {
                    break;
                };
                last = i;
                let matched: Vec<_> = books
                    .into_iter()
                    .filter(|b| {
                        let bl = b.name.to_lowercase();
                        bl.contains(&ql) || ql.contains(&bl)
                    })
                    .filter(|b| seen.insert(b.origin.clone()))
                    .collect();
                let payload = json!({ "lastIndex": i, "data": matched });
                if tx.send(sse::data_event(&payload)).await.is_err() {
                    return;
                }
            }
            let _ = tx
                .send(sse::named_event(
                    "end",
                    &json!({ "lastIndex": last, "isEnd": true }),
                ))
                .await;
        },
    ));

    sse::sse_from_receiver(rx)
}

/// GET/POST `/reader3/searchBookMultiSSE`：多书源流式搜索（统一走 `api::sse`）
pub async fn search_book_multi_sse(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let mut key = params.get("key").cloned().unwrap_or_default();
    let mut key_exact_prefix = false;
    if key.starts_with('=') {
        key_exact_prefix = true;
        key.remove(0);
    }
    let mut group = params.get("bookSourceGroup").cloned().unwrap_or_default();
    let mut single_source_url = params.get("bookSourceUrl").cloned().unwrap_or_default();
    let mut exact = params.get("exact").map(|v| v == "1").unwrap_or(false);
    let mut last_index = params
        .get("lastIndex")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(-1);
    let mut search_size = params
        .get("searchSize")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let mut concurrent_count = params
        .get("concurrentCount")
        .and_then(|v| v.parse::<usize>().ok());
    let body_json = body
        .as_ref()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok());
    if let Some(json) = body_json.as_ref() {
        if let Some(v) = json.get("key").and_then(|v| v.as_str()) {
            key = v.to_string();
        }
        if let Some(v) = json.get("bookSourceGroup").and_then(|v| v.as_str()) {
            group = v.to_string();
        }
        if let Some(v) = json.get("bookSourceUrl").and_then(|v| v.as_str()) {
            single_source_url = v.to_string();
        }
        if let Some(v) = json.get("exact") {
            exact = v.as_u64() == Some(1)
                || v.as_str()
                    .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
        }
        if let Some(v) = json.get("lastIndex").and_then(|v| v.as_i64()) {
            last_index = v;
        }
        if let Some(v) = json.get("searchSize").and_then(|v| v.as_u64()) {
            search_size = v as usize;
        }
        if let Some(v) = json.get("concurrentCount").and_then(|v| v.as_u64()) {
            concurrent_count = Some(v as usize);
        }
    }
    if key_exact_prefix {
        exact = true;
    }

    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return sse::sse_error(ret),
    };
    if let Err(ret) = require_book_source_permission(&state, &namespace).await {
        return sse::sse_error(ret);
    }
    if key.is_empty() {
        return sse::sse_error(ReturnData::err("请输入搜索关键字"));
    }
    let sources = match state.storage.get_book_sources(&namespace).await {
        Ok(s) => s,
        Err(_) => return sse::sse_error(ReturnData::err("系统错误")),
    };
    let mut sources: Vec<crate::model::BookSource> = sources
        .into_iter()
        .filter(|s| {
            s.enabled
                && s.search_url.is_some()
                && !crate::service::health::is_source_invalid(&namespace, &s.book_source_url)
        })
        .filter(|s| book_source_group_matches(&group, s.book_source_group.as_deref()))
        .filter(|s| single_source_url.is_empty() || s.book_source_url == single_source_url)
        .collect();
    let stats_rows = state
        .storage
        .get_source_stats(&namespace)
        .await
        .unwrap_or_default();
    {
        // 三级分层排序，避免 Wilson 下界把 1/1~3/3 已验证好源压在未探测死源（0.5）下面：
        // - Tier 2：已有真实成功证据且搜索成功率 ≥ 40% → 稳居最前，按 confidence_lo 降序 + 延迟升序
        // - Tier 1：尚未探测过的新源 → 排在已验证好源之后，自然填补首批窗口剩余槽位
        // - Tier 0：已测但失败/超时居多的弱源 → 沉底，不再挤占首批 50 源
        let mut map: HashMap<String, (u8, f64, i64)> = HashMap::new();
        for st in &stats_rows {
            let has_ok = st.successes > 0 || st.toc_successes > 0 || st.content_successes > 0;
            let tier = if st.effective_samples() == 0 {
                1
            } else if has_ok && (st.attempts == 0 || st.success_rate() >= 0.4) {
                2
            } else {
                0
            };
            map.insert(
                st.source_url.clone(),
                (
                    tier,
                    st.confidence_lo().unwrap_or(SORT_UNKNOWN),
                    st.avg_latency_ms(),
                ),
            );
        }
        if !map.is_empty() {
            sources.sort_by(|a, b| {
                let (ta, ca, la) =
                    map.get(&a.book_source_url)
                        .copied()
                        .unwrap_or((1, SORT_UNKNOWN, 3000));
                let (tb, cb, lb) =
                    map.get(&b.book_source_url)
                        .copied()
                        .unwrap_or((1, SORT_UNKNOWN, 3000));
                tb.cmp(&ta)
                    .then_with(|| cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal))
                    .then_with(|| la.cmp(&lb))
            });
        }
    }
    let include_dead = params.get("all").map(|v| v == "1").unwrap_or(false);
    if !include_dead && single_source_url.is_empty() {
        let now = chrono::Utc::now().timestamp_millis();
        let dead = dead_source_urls(&stats_rows, now);
        if !dead.is_empty() {
            sources.retain(|s| !dead.contains(&s.book_source_url));
        }
    }
    if sources.is_empty() {
        return sse::sse_error(ReturnData::err("未配置书源"));
    }
    if last_index >= sources.len() as i64 - 1 {
        return sse::sse_error(ReturnData::err("没有更多了"));
    }
    search_size = search_size.max(1);
    let concurrent_count = effective_concurrent_count(concurrent_count);
    let timeout_secs = search_timeout_param(&params, body_json.as_ref());

    let sched_key = sched_session_key(&namespace, &group, &key, &single_source_url);
    let (sources, marks) = sched_begin(&sched_key, sources, last_index < 0);
    let session_urls: Vec<String> = sources.iter().map(|s| s.book_source_url.clone()).collect();
    let total = sources.len() as i64;
    let start = (last_index + 1).max(0) as usize;
    let end = (start + search_size).min(sources.len());
    let stat_by_url: HashMap<&str, SchedStat> = stats_rows
        .iter()
        .filter_map(|st| SchedStat::from_row(st).map(|s| (st.source_url.as_str(), s)))
        .collect();
    let stat_vec: Vec<Option<SchedStat>> = session_urls
        .iter()
        .map(|u| stat_by_url.get(u.as_str()).copied())
        .collect();
    let (window, next_from_planned) = plan_window(start, end, &marks, last_index);
    let dispatch: Vec<usize> = order_window(start, end, &stat_vec)
        .into_iter()
        .filter(|i| window.contains(i))
        .collect();
    tracing::info!(
        "SSE 窗口 lastIndex={} start={} end={} plan={} dispatch={} 已结论={} 窗口预算={}s 超时={}s",
        last_index,
        start,
        end,
        window.len(),
        dispatch.len(),
        marks.done.len(),
        sse_window_budget_secs(),
        timeout_secs
    );

    let (tx, rx) = tokio::sync::mpsc::channel(concurrent_count.clamp(4, 64));
    let ns = namespace.clone();
    let storage = state.storage.clone();
    let scoped_storage = storage.clone();
    tokio::spawn(crate::storage::scope_task_storage(
        scoped_storage,
        async move {
            let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrent_count));
            let mut tasks = futures::stream::FuturesUnordered::new();
            let conn_t0 = Instant::now();
            for (offset, &idx) in dispatch.iter().enumerate() {
                let source = sources[idx].clone();
                let sem = sem.clone();
                let key = key.clone();
                let ns = ns.clone();
                let storage = storage.clone();
                tracing::info!(
                    "SSE 派发源 idx={} offset={} url={} t_ms={}",
                    idx,
                    offset,
                    source.book_source_url,
                    conn_t0.elapsed().as_millis()
                );
                let ramp_ms = sse_ramp_delay_ms(offset);
                tasks.push(Box::pin(async move {
                    if ramp_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(ramp_ms)).await;
                    }
                    let _permit = sem.acquire().await;
                    let t0 = Instant::now();
                    let res = crate::service::search::search_one_source(
                        &storage,
                        &ns,
                        &source,
                        &key,
                        1,
                        timeout_secs,
                    )
                    .await;
                    let lat = t0.elapsed().as_millis() as i64;
                    let (outcome, intro_hits, intro_count) = match &res {
                        Ok(books) => (
                            crate::storage::SearchOutcome::Ok,
                            books
                                .iter()
                                .filter(|b| {
                                    b.intro.as_deref().is_some_and(|t| !t.trim().is_empty())
                                })
                                .count() as i64,
                            books.len() as i64,
                        ),
                        Err(e) => (crate::storage::SearchOutcome::from_error(e), 0, 0),
                    };
                    let ok = outcome == crate::storage::SearchOutcome::Ok;
                    let surl = source.book_source_url.clone();
                    let st = storage.clone();
                    let scoped_st = st.clone();
                    let nsx = ns.clone();
                    tokio::spawn(crate::storage::scope_task_storage(scoped_st, async move {
                        let _ = st
                            .record_search_outcome_state(
                                &nsx,
                                &surl,
                                outcome,
                                lat,
                                intro_hits,
                                intro_count,
                            )
                            .await;
                    }));
                    let books = res.unwrap_or_default();
                    let books = if exact {
                        crate::service::search::filter_exact(books, &key)
                    } else {
                        books
                    };
                    let mut unique = Vec::with_capacity(books.len());
                    let mut local_seen = HashSet::new();
                    for b in books {
                        let k = format!("{}_{}", b.name.trim(), b.author.trim());
                        if k.is_empty() {
                            continue;
                        }
                        if local_seen.insert(k) {
                            unique.push(b);
                        }
                    }
                    let payload = json!({ "lastIndex": idx as i64, "data": unique });
                    (idx as i64, payload, unique, ok)
                }));
            }

            let window_budget = Duration::from_secs(sse_window_budget_secs());
            let window_t0 = Instant::now();
            let mut discovered: Vec<crate::service::search::SearchBook> = Vec::new();
            let mut done: HashSet<usize> = HashSet::new();
            let full_scan = end >= total.max(0) as usize;
            let hard_cap = sse_hard_cap(window_budget, timeout_secs);
            let window_deadline = tokio::time::Instant::from_std(window_t0) + hard_cap;
            let mut results: Vec<(usize, bool)> = Vec::new();
            let mut hard_capped = false;
            let mut client_gone = false;

            loop {
                if !full_scan
                    && sse_should_close(done.len(), window_t0.elapsed(), window_budget, hard_cap)
                {
                    hard_capped = window_t0.elapsed() >= hard_cap;
                    break;
                }
                let (i, payload, found, ok) = if full_scan {
                    tokio::select! {
                        biased;
                        next = tasks.next() => match next {
                            Some(v) => v,
                            None => break,
                        },
                        _ = tx.closed() => {
                            client_gone = true;
                            break;
                        }
                    }
                } else {
                    tokio::select! {
                        biased;
                        next = tasks.next() => match next {
                            Some(v) => v,
                            None => break,
                        },
                        _ = tx.closed() => {
                            client_gone = true;
                            break;
                        }
                        _ = tokio::time::sleep_until(window_deadline) => {
                            hard_capped = true;
                            break;
                        }
                    }
                };
                done.insert(i as usize);
                results.push((i as usize, ok));
                discovered.extend(found);
                if tx.send(sse::data_event(&payload)).await.is_err() {
                    client_gone = true;
                    break;
                }
            }

            while let Some(Some((i, payload, found, ok))) =
                futures::FutureExt::now_or_never(tasks.next())
            {
                let i = i as usize;
                done.insert(i);
                results.push((i, ok));
                discovered.extend(found);
                if !client_gone {
                    let _ = tx.send(sse::data_event(&payload)).await;
                }
            }
            let unfinished = record_unfinished_timeouts(
                &storage,
                &ns,
                &session_urls,
                &dispatch,
                &done,
                window_t0.elapsed().as_millis() as i64,
            )
            .await;
            sched_finish(&sched_key, &session_urls, &results, last_index);
            let mut concluded = marks.done.clone();
            concluded.extend(done.iter().copied());
            let next_from = sse_next_from(start, end, &concluded, last_index, hard_capped)
                .min(next_from_planned)
                .max(last_index);
            tracing::info!(
            "SSE 窗口收口 lastIndex={} 计划派={} 实际完成={} 补记超时={} 硬顶={} next_from={} 计划游标={} 命中={}",
            last_index,
            dispatch.len(),
            done.len(),
            unfinished,
            hard_capped,
            next_from,
            next_from_planned,
            discovered.len()
        );
            if !discovered.is_empty() {
                persist_search_candidates(&storage, &ns, &discovered).await;
            }
            if !client_gone {
                let end_payload =
                    json!({ "lastIndex": next_from, "isEnd": next_from >= total - 1 });
                let _ = tx.send(sse::named_event("end", &end_payload)).await;
            }
        },
    ));

    sse::sse_from_receiver(rx)
}

// ---------------- 多源搜索调度与纯函数 ----------------

fn effective_concurrent_count(v: Option<usize>) -> usize {
    v.unwrap_or(24).clamp(1, 128)
}

fn search_timeout_param(params: &HashMap<String, String>, body_json: Option<&Value>) -> u64 {
    let raw = param_of(params, body_json, "timeout")
        .parse::<i64>()
        .unwrap_or(15);
    raw.clamp(3, 60) as u64
}

fn sse_window_budget_secs() -> u64 {
    static B: OnceLock<u64> = OnceLock::new();
    *B.get_or_init(|| {
        std::env::var("READER_SEARCH_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v: &u64| *v > 0)
            .unwrap_or(10)
    })
}

fn sse_ramp_delay_ms(offset: usize) -> u64 {
    (offset.saturating_sub(SSE_RAMP_FREE_SOURCES) as u64).saturating_mul(SSE_RAMP_STEP_MS)
}

fn sse_should_close(
    done: usize,
    elapsed: Duration,
    window_budget: Duration,
    hard_cap: Duration,
) -> bool {
    elapsed >= hard_cap || (done >= 8 && elapsed > window_budget)
}

fn sse_hard_cap(window_budget: Duration, timeout_secs: u64) -> Duration {
    window_budget
        .mul_f64(1.5)
        .max(Duration::from_secs(timeout_secs.saturating_add(2)))
}

async fn record_unfinished_timeouts(
    storage: &crate::storage::Storage,
    ns: &str,
    urls: &[String],
    dispatched: &[usize],
    done: &HashSet<usize>,
    waited_ms: i64,
) -> usize {
    let mut n = 0usize;
    for &idx in dispatched {
        if done.contains(&idx) {
            continue;
        }
        let Some(url) = urls.get(idx) else { continue };
        let _ = storage
            .record_search_outcome_state(
                ns,
                url,
                crate::storage::SearchOutcome::Timeout,
                waited_ms,
                0,
                0,
            )
            .await;
        n += 1;
    }
    n
}

fn sse_next_from(
    start: usize,
    end: usize,
    done: &HashSet<usize>,
    last_index: i64,
    hard_capped: bool,
) -> i64 {
    if hard_capped {
        return (end as i64 - 1).max(last_index);
    }
    match (start..end).find(|i| !done.contains(i)) {
        Some(i) => {
            let cand = (i as i64 - 1).max(last_index);
            if cand <= last_index && !done.is_empty() {
                (i as i64).min(end as i64 - 1)
            } else {
                cand
            }
        }
        None => (end as i64 - 1).max(last_index),
    }
}

fn dead_source_urls(stats: &[crate::model::SourceStat], now_ms: i64) -> HashSet<String> {
    stats
        .iter()
        .filter(|st| st.is_dead(now_ms))
        .map(|st| st.source_url.clone())
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct SchedStat {
    success_rate: f64,
    avg_latency_ms: i64,
}

impl SchedStat {
    fn from_row(st: &crate::model::SourceStat) -> Option<Self> {
        (st.attempts > 0).then(|| Self {
            success_rate: st.success_rate(),
            avg_latency_ms: st.avg_latency_ms(),
        })
    }
}

fn sched_rank(s: Option<SchedStat>) -> u8 {
    match s {
        None => 1,
        Some(x) if x.success_rate > 0.0 => 0,
        Some(_) => 2,
    }
}

fn sched_slot(stats: &[Option<SchedStat>], i: usize) -> Option<SchedStat> {
    stats.get(i).copied().flatten()
}

fn order_window(start: usize, end: usize, stats: &[Option<SchedStat>]) -> Vec<usize> {
    let mut idx: Vec<usize> = (start..end.max(start)).collect();
    idx.sort_by(|&a, &b| {
        let (sa, sb) = (sched_slot(stats, a), sched_slot(stats, b));
        sched_rank(sa)
            .cmp(&sched_rank(sb))
            .then_with(|| match (sa, sb) {
                (Some(x), Some(y)) => y
                    .success_rate
                    .partial_cmp(&x.success_rate)
                    .unwrap_or(std::cmp::Ordering::Equal),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| match (sa, sb) {
                (Some(x), Some(y)) => x.avg_latency_ms.cmp(&y.avg_latency_ms),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.cmp(&b))
    });
    idx
}

#[derive(Debug, Default, Clone)]
struct SchedMarks {
    done: HashSet<usize>,
    failed: HashSet<usize>,
    retried: HashSet<usize>,
    failed_at: HashMap<usize, i64>,
}

fn plan_window(
    start: usize,
    end: usize,
    done_sources: &SchedMarks,
    last_index: i64,
) -> (Vec<usize>, i64) {
    let end = end.max(start);
    let win: Vec<usize> = (start..end)
        .filter(|&i| {
            if !done_sources.done.contains(&i) {
                return true;
            }
            done_sources.failed.contains(&i)
                && !done_sources.retried.contains(&i)
                && (i as i64) > last_index
                && done_sources
                    .failed_at
                    .get(&i)
                    .is_some_and(|&c| last_index < c)
        })
        .collect();
    let next = (end as i64 - 1).max(last_index);
    (win, next)
}

#[derive(Debug, Clone)]
struct SchedSession {
    urls: Vec<String>,
    done: HashSet<String>,
    failed: HashSet<String>,
    retried: HashSet<String>,
    failed_at: HashMap<String, i64>,
    touched_ms: i64,
}

static SSE_SESSIONS: OnceLock<Mutex<HashMap<String, SchedSession>>> = OnceLock::new();

fn sse_sessions() -> &'static Mutex<HashMap<String, SchedSession>> {
    SSE_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sched_session_key(ns: &str, group: &str, key: &str, single_url: &str) -> String {
    format!("{ns}\u{1}{group}\u{1}{key}\u{1}{single_url}")
}

fn sched_begin(
    session_key: &str,
    sources: Vec<crate::model::BookSource>,
    fresh: bool,
) -> (Vec<crate::model::BookSource>, SchedMarks) {
    let now = chrono::Utc::now().timestamp_millis();
    let mut guard = sse_sessions().lock().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, s| now - s.touched_ms <= SSE_SESSION_IDLE_MS);
    if fresh || !guard.contains_key(session_key) {
        if guard.len() >= SSE_SESSION_MAX {
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, s)| s.touched_ms)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest);
            }
        }
        guard.insert(
            session_key.to_string(),
            SchedSession {
                urls: sources.iter().map(|s| s.book_source_url.clone()).collect(),
                done: Default::default(),
                failed: Default::default(),
                retried: Default::default(),
                failed_at: Default::default(),
                touched_ms: now,
            },
        );
    }
    let Some(sess) = guard.get_mut(session_key) else {
        return (sources, SchedMarks::default());
    };
    let mut by_url: HashMap<&str, usize> = HashMap::new();
    for (i, s) in sources.iter().enumerate() {
        by_url.entry(s.book_source_url.as_str()).or_insert(i);
    }
    let mut used = vec![false; sources.len()];
    let mut ordered: Vec<crate::model::BookSource> = Vec::with_capacity(sources.len());
    for url in &sess.urls {
        if let Some(&i) = by_url.get(url.as_str()) {
            if !used[i] {
                used[i] = true;
                ordered.push(sources[i].clone());
            }
        }
    }
    for (i, s) in sources.iter().enumerate() {
        if !used[i] {
            ordered.push(s.clone());
        }
    }
    sess.urls = ordered.iter().map(|s| s.book_source_url.clone()).collect();
    sess.touched_ms = now;
    let mut marks = SchedMarks::default();
    for (i, url) in sess.urls.iter().enumerate() {
        if sess.done.contains(url) {
            marks.done.insert(i);
        }
        if sess.failed.contains(url) {
            marks.failed.insert(i);
        }
        if sess.retried.contains(url) {
            marks.retried.insert(i);
        }
        if let Some(&c) = sess.failed_at.get(url) {
            marks.failed_at.insert(i, c);
        }
    }
    (ordered, marks)
}

fn sched_finish(session_key: &str, urls: &[String], results: &[(usize, bool)], last_index: i64) {
    let now = chrono::Utc::now().timestamp_millis();
    let mut guard = sse_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let Some(sess) = guard.get_mut(session_key) else {
        return;
    };
    for &(i, ok) in results {
        let Some(url) = urls.get(i) else { continue };
        if ok {
            sess.done.insert(url.clone());
            sess.failed.remove(url);
            sess.retried.remove(url);
            sess.failed_at.remove(url);
        } else {
            if sess.failed.contains(url) {
                sess.retried.insert(url.clone());
            }
            sess.failed.insert(url.clone());
            sess.done.insert(url.clone());
            sess.failed_at.insert(url.clone(), last_index);
        }
    }
    sess.touched_ms = now;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_window_prioritizes_fast_healthy_over_unknown_over_failing() {
        let stats = vec![
            Some(SchedStat {
                success_rate: 0.0,
                avg_latency_ms: 100,
            }),
            None,
            Some(SchedStat {
                success_rate: 0.9,
                avg_latency_ms: 500,
            }),
            Some(SchedStat {
                success_rate: 0.9,
                avg_latency_ms: 120,
            }),
        ];
        assert_eq!(order_window(0, 4, &stats), vec![3, 2, 1, 0]);
    }

    #[test]
    fn sse_next_from_advances_cleanly() {
        let mut done = HashSet::new();
        done.insert(0);
        done.insert(1);
        assert_eq!(sse_next_from(0, 4, &done, -1, false), 1);
        done.insert(2);
        done.insert(3);
        assert_eq!(sse_next_from(0, 4, &done, -1, false), 3);
        assert_eq!(sse_next_from(0, 4, &HashSet::new(), -1, true), 3);
    }
}
