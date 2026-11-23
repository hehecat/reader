//! 书架分组与书签 API：`get_book_groups`、`save_book_group`、`delete_book_group`、`save_book_group_id`、`save_bookmark`、`get_bookmarks`、`delete_bookmark`、`delete_bookmarks`

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::helpers::{int_param_of, str_param_of};
use crate::api::auth::resolve_namespace;
use crate::api::router::{now_millis, param_of, AppState, ReturnData};

/// GET/POST `/reader3/getBookGroups`
pub async fn get_book_groups(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    match state.storage.list_book_groups_with_count(&namespace).await {
        Ok(groups) => ReturnData::ok_typed(groups).into_response(),
        Err(e) => {
            tracing::error!("getBookGroups 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// POST `/reader3/saveBookGroup`
pub async fn save_book_group(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let Some(body) = body else {
        return Json(ReturnData::err("参数错误")).into_response();
    };
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(ReturnData::err("参数错误")).into_response(),
    };
    let group: crate::model::BookGroup = match serde_json::from_value(v.clone()) {
        Ok(g) => g,
        Err(_) => return Json(ReturnData::err("参数错误")).into_response(),
    };
    if group.name.is_empty() {
        return Json(ReturnData::err("分组名称不能为空")).into_response();
    }
    if group.id > 0 && v.get("order").is_none() && v.get("orderNum").is_none() {
        return match state
            .storage
            .rename_book_group(&namespace, group.id, &group.name)
            .await
        {
            Ok(0) => Json(ReturnData::err("分组不存在")).into_response(),
            Ok(_) => {
                let saved = state
                    .storage
                    .list_book_groups(&namespace)
                    .await
                    .ok()
                    .and_then(|list| list.into_iter().find(|g| g.id == group.id));
                ReturnData::ok_typed(saved.unwrap_or(group)).into_response()
            }
            Err(e) => {
                tracing::error!("saveBookGroup 重命名失败: {e}");
                Json(ReturnData::err("保存失败")).into_response()
            }
        };
    }
    match state.storage.save_book_group(&namespace, &group).await {
        Ok(saved) => ReturnData::ok_typed(saved).into_response(),
        Err(e) => {
            tracing::error!("saveBookGroup 失败: {e}");
            Json(ReturnData::err("保存失败")).into_response()
        }
    }
}

/// POST `/reader3/deleteBookGroup`：删除书架分组（JSON Body 优先，Query 兜底）
pub async fn delete_book_group(
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
    let id = int_param_of(&params, body_json.as_ref(), &["id", "groupId"]).unwrap_or(-1);
    if id <= 0 {
        return Json(ReturnData::err("参数错误"));
    }
    match state.storage.delete_book_group(&namespace, id).await {
        Ok(0) => Json(ReturnData::err("分组不存在")),
        Ok(_) => Json(ReturnData::ok(json!(""))),
        Err(e) => {
            tracing::error!("deleteBookGroup [{id}] 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/saveBookGroupId`：设置书籍分组（JSON Body 优先，Query 兜底）
pub async fn save_book_group_id(
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
    let book_url = str_param_of(&params, body_json.as_ref(), &["bookUrl", "url"]);
    let group = int_param_of(&params, body_json.as_ref(), &["groupId", "group"]).unwrap_or(-1);
    if book_url.is_empty() || group < 0 {
        return Json(ReturnData::err("参数错误"));
    }
    match state
        .storage
        .update_book_group_id(&namespace, &book_url, group)
        .await
    {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("saveBookGroupId 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST `/reader3/saveBookmark`
pub async fn save_bookmark(
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
    let mut bookmark: crate::model::Bookmark = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Json(ReturnData::err("参数错误")),
    };
    if bookmark.book_url.is_empty() || bookmark.title.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    bookmark.user_namespace = namespace.clone();
    if bookmark.created_at == 0 {
        bookmark.created_at = now_millis();
    }
    match state.storage.save_bookmark(&namespace, &bookmark).await {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("saveBookmark 失败: {e}");
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// GET/POST `/reader3/getBookmarks`
pub async fn get_bookmarks(
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
    let book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    if book_url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    match state.storage.list_bookmarks(&namespace, &book_url).await {
        Ok(bookmarks) => ReturnData::ok_typed(bookmarks).into_response(),
        Err(e) => {
            tracing::error!("getBookmarks 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// POST `/reader3/deleteBookmark`
pub async fn delete_bookmark(
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
    let book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    let title = param_of(&params, body_json.as_ref(), "title");
    if book_url.is_empty() || title.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match state
        .storage
        .delete_bookmark(&namespace, &book_url, &title)
        .await
    {
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("deleteBookmark 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/deleteBookmarks`
pub async fn delete_bookmarks(
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
    let book_url = json
        .get("bookUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ids: Vec<String> = json
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if book_url.is_empty() || ids.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    match state
        .storage
        .delete_bookmarks(&namespace, &book_url, &ids)
        .await
    {
        Ok(count) => Json(ReturnData::ok(json!({ "count": count }))),
        Err(e) => {
            tracing::error!("deleteBookmarks 失败: {e}");
            Json(ReturnData::err("删除失败"))
        }
    }
}
