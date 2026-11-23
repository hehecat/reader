//! 文件管理 API（`/reader3/file/*`，仅保留前端 `services/webdav.ts` 实际使用的 5 个接口）
//!
//! 保留接口：`list`、`download`、`upload`、`delete`、`delete_multi`。
//!
//! `home` 语义：
//! - `__WEBDAV__`      → `storage/data/{ns}/webdav`（secure 模式需开启 webdav 权限）
//! - `__LOCAL_STORE__` → `storage/localStore`（secure 模式需开启本地书仓权限；写/删需管理员）
//! - `__HOME__` / 空   → `storage/data/{ns}`
//! - `__STORAGE__`     → `storage` 根（需管理员）
//! - 其他              → 非法访问

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use axum::extract::{Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::auth::{login_required, resolve_namespace};
use super::router::{
    check_upload_content_length, read_multipart_field_limited, AppState, ReturnData,
};
use crate::AppConfig;

/// 参数提取：body JSON 优先，GET query 兜底
fn str_param(params: &HashMap<String, String>, body: Option<&Value>, key: &str) -> String {
    if let Some(b) = body {
        if let Some(v) = b.get(key).and_then(|v| v.as_str()) {
            return v.to_string();
        }
    }
    params.get(key).cloned().unwrap_or_default()
}

fn int_param(params: &HashMap<String, String>, body: Option<&Value>, key: &str) -> Option<i64> {
    if let Some(v) = body.and_then(|b| b.get(key)) {
        if let Some(n) = v
            .as_i64()
            .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        {
            return Some(n);
        }
    }
    params.get(key).and_then(|v| v.trim().parse::<i64>().ok())
}

/// 管理权限判定：非 secure 模式恒 true；secure 模式仅管理员
fn manager_ok(config: &AppConfig, user: Option<&crate::model::user::User>) -> bool {
    if !config.secure {
        return true;
    }
    user.map(|u| u.is_admin).unwrap_or(false)
}

fn manager_required() -> ReturnData {
    ReturnData::err("仅管理员可执行该操作")
}

fn resolve_secure_path_from_abs(base_abs: &Path, rel: &str) -> Option<PathBuf> {
    let joined = base_abs.join(rel.trim_start_matches(['/', '\\']));
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.starts_with(base_abs) {
        Some(out)
    } else {
        None
    }
}

/// 安全路径解析（防穿越）：组件级处理 `..`，返回路径保证位于 `base` 内
pub(crate) fn resolve_secure_path(base: &Path, rel: &str) -> Option<PathBuf> {
    let base_abs = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    resolve_secure_path_from_abs(&base_abs, rel)
}

/// `home` 语义解析：返回允许访问的根目录（异步自动建目录）
pub(crate) async fn file_home(
    config: &AppConfig,
    ns: &str,
    home: &str,
    is_save: bool,
    is_delete: bool,
    manager: bool,
    user: Option<&crate::model::User>,
) -> Result<PathBuf, ReturnData> {
    let storage_dir = config.storage_dir();
    let dir = match home {
        "__WEBDAV__" => {
            if config.secure {
                let user = user.ok_or_else(login_required)?;
                if !user.enable_webdav {
                    return Err(ReturnData::err("未开启webdav功能"));
                }
            }
            storage_dir.join("data").join(ns).join("webdav")
        }
        "__LOCAL_STORE__" => {
            if config.secure {
                let user = user.ok_or_else(login_required)?;
                if !user.enable_local_store {
                    return Err(ReturnData::err("未开启本地书仓功能"));
                }
            }
            if (is_save || is_delete) && !manager {
                return Err(manager_required());
            }
            storage_dir.join("localStore")
        }
        "__HOME__" | "" => storage_dir.join("data").join(ns),
        "__STORAGE__" => {
            if !manager {
                return Err(manager_required());
            }
            storage_dir
        }
        _ => return Err(ReturnData::err("非法访问")),
    };
    let _ = tokio::fs::create_dir_all(&dir).await;
    Ok(dir)
}

/// 条目结构（list/upload 零中间 Value 树直接序列化）
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    name: String,
    size: u64,
    path: String,
    last_modified: i64,
    is_directory: bool,
}

fn build_file_entry(
    base_abs: &Path,
    path: &Path,
    is_dir: bool,
    size: u64,
    last_modified: i64,
) -> FileEntry {
    let rel = path
        .strip_prefix(base_abs)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    FileEntry {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size,
        path: format!("/{}", rel.trim_start_matches('/')),
        last_modified,
        is_directory: is_dir,
    }
}

async fn last_modified_millis(path: &Path) -> i64 {
    tokio::fs::metadata(path)
        .await
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// GET `/reader3/file/list`：列目录（`path` + `home` 参数）
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let ns = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let home = str_param(&params, body_json.as_ref(), "home");
    let user = state.storage.find_user(&ns).await.ok().flatten();
    let manager = manager_ok(&state.storage.config, user.as_ref());
    let base = match file_home(
        &state.storage.config,
        &ns,
        &home,
        false,
        false,
        manager,
        user.as_ref(),
    )
    .await
    {
        Ok(b) => b,
        Err(ret) => return Json(ret).into_response(),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let base_abs = tokio::fs::canonicalize(&base)
        .await
        .unwrap_or_else(|_| base.clone());
    let Some(file) = resolve_secure_path_from_abs(&base_abs, &path) else {
        return Json(ReturnData::err("路径不存在")).into_response();
    };
    match tokio::fs::metadata(&file).await {
        Ok(meta) => {
            if !meta.is_dir() {
                return Json(ReturnData::err("路径不是目录")).into_response();
            }
        }
        Err(_) => {
            if path != "/" {
                return Json(ReturnData::err("路径不存在")).into_response();
            }
            if tokio::fs::create_dir_all(&file).await.is_err() {
                return Json(ReturnData::err("路径不存在")).into_response();
            }
        }
    }
    let mut items: Vec<FileEntry> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&file).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let meta = entry.metadata().await.ok();
            items.push(build_file_entry(
                &base_abs,
                &entry.path(),
                meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                meta.as_ref().map(|m| m.len()).unwrap_or(0),
                meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            ));
        }
    }
    ReturnData::ok_typed(items).into_response()
}

/// 扩展名 → Content-Type（download 预览支持；未知类型回退 octet-stream）
fn mime_from_ext(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "epub" => "application/epub+zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Range 头解析结果
#[derive(Debug, PartialEq, Eq)]
enum RangeReq {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

/// 解析 `Range: bytes=start-end`（兼容 `bytes=start-` 与 `bytes=-suffix`）
fn parse_range_header(raw: Option<&str>, total: u64) -> RangeReq {
    let Some(spec) = raw.and_then(|s| s.trim().strip_prefix("bytes=")) else {
        return RangeReq::Full;
    };
    let Some(first) = spec.trim().split(',').next() else {
        return RangeReq::Full;
    };
    let Some((start_s, end_s)) = first.trim().split_once('-') else {
        return RangeReq::Full;
    };
    let (start, end) = if start_s.trim().is_empty() {
        let Ok(suffix) = end_s.trim().parse::<u64>() else {
            return RangeReq::Full;
        };
        if suffix == 0 || total == 0 {
            return RangeReq::Unsatisfiable;
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let Ok(start) = start_s.trim().parse::<u64>() else {
            return RangeReq::Full;
        };
        let end = match end_s.trim().parse::<u64>() {
            Ok(e) => {
                if e < start {
                    return RangeReq::Full;
                }
                e.min(total.saturating_sub(1))
            }
            Err(_) => {
                if !end_s.trim().is_empty() {
                    return RangeReq::Full;
                }
                total.saturating_sub(1)
            }
        };
        if start >= total {
            return RangeReq::Unsatisfiable;
        }
        (start, end)
    };
    RangeReq::Partial { start, end }
}

/// GET `/reader3/file/download`：下载文件（支持 `Range` 断点续传）
pub async fn download(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let ns = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let home = str_param(&params, body_json.as_ref(), "home");
    let user = state.storage.find_user(&ns).await.ok().flatten();
    let manager = manager_ok(&state.storage.config, user.as_ref());
    let base = match file_home(
        &state.storage.config,
        &ns,
        &home,
        false,
        false,
        manager,
        user.as_ref(),
    )
    .await
    {
        Ok(b) => b,
        Err(ret) => return Json(ret).into_response(),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let base_abs = tokio::fs::canonicalize(&base)
        .await
        .unwrap_or_else(|_| base.clone());
    let Some(file) = resolve_secure_path_from_abs(&base_abs, &path) else {
        return Json(ReturnData::err("参数错误")).into_response();
    };
    let meta = match tokio::fs::metadata(&file).await {
        Ok(m) if m.is_file() => m,
        _ => return Json(ReturnData::err("路径不存在")).into_response(),
    };
    let stream = int_param(&params, body_json.as_ref(), "stream").unwrap_or(0);
    let total = meta.len();
    let mime = mime_from_ext(
        file.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default(),
    );
    let disposition = (stream <= 0)
        .then(|| file.file_name().map(|n| n.to_string_lossy().into_owned()))
        .flatten()
        .map(|name| format!("attachment; filename={}", urlencoding::encode(&name)));
    let with_common = |builder: axum::http::response::Builder| {
        let builder = builder
            .header("Cache-Control", "86400")
            .header("Accept-Ranges", "bytes");
        let builder = if let Some(cd) = &disposition {
            builder.header("Content-Disposition", cd)
        } else {
            builder
        };
        builder.header("Content-Type", mime)
    };
    let range_raw = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok());
    match parse_range_header(range_raw, total) {
        RangeReq::Partial { start, end } => {
            let length = end - start + 1;
            let read = async {
                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                let mut f = tokio::fs::File::open(&file).await?;
                f.seek(std::io::SeekFrom::Start(start)).await?;
                let mut buf = Vec::with_capacity(length as usize);
                f.take(length).read_to_end(&mut buf).await?;
                Ok::<Vec<u8>, std::io::Error>(buf)
            }
            .await;
            match read {
                Ok(bytes) => with_common(
                    Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header("Content-Range", format!("bytes {start}-{end}/{total}")),
                )
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| Json(ReturnData::err("响应构造失败")).into_response()),
                Err(e) => {
                    tracing::error!("file/download 区间读取失败 [{}]: {e}", file.display());
                    Json(ReturnData::err("读取失败")).into_response()
                }
            }
        }
        RangeReq::Unsatisfiable => with_common(
            Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header("Content-Range", format!("bytes */{total}")),
        )
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::RANGE_NOT_SATISFIABLE.into_response()),
        RangeReq::Full => match tokio::fs::read(&file).await {
            Ok(bytes) => with_common(Response::builder().status(StatusCode::OK))
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| Json(ReturnData::err("响应构造失败")).into_response()),
            Err(e) => {
                tracing::error!("file/download 读取失败 [{}]: {e}", file.display());
                Json(ReturnData::err("读取失败")).into_response()
            }
        },
    }
}

/// POST `/reader3/file/upload`：上传文件（multipart：`home`/`path` + `file`，支持多文件）
pub async fn upload(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let ns = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let mut home = params.get("home").cloned().unwrap_or_default();
    let mut path = params.get("path").cloned().unwrap_or_default();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let max_bytes = state.storage.config.upload_max_bytes();
    let max_mb = state.storage.config.upload_max_mb;
    if let Some(msg) = check_upload_content_length(&headers, max_bytes, max_mb) {
        return Json(ReturnData::err(msg)).into_response();
    }
    loop {
        match multipart.next_field().await {
            Ok(Some(mut field)) => match field.name() {
                Some("home") => {
                    if let Ok(v) = field.text().await {
                        home = v;
                    }
                }
                Some("path") => {
                    if let Ok(v) = field.text().await {
                        path = v;
                    }
                }
                Some("file") => {
                    let name = field.file_name().unwrap_or("file").to_string();
                    match read_multipart_field_limited(&mut field, max_bytes, max_mb).await {
                        Ok(bytes) => files.push((name, bytes)),
                        Err(msg) => return Json(ReturnData::err(msg)).into_response(),
                    }
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("files upload multipart 读取失败: {e}");
                break;
            }
        }
    }
    if files.is_empty() {
        return Json(ReturnData::err("请上传文件")).into_response();
    }
    let user = state.storage.find_user(&ns).await.ok().flatten();
    let manager = manager_ok(&state.storage.config, user.as_ref());
    let base = match file_home(
        &state.storage.config,
        &ns,
        &home,
        true,
        false,
        manager,
        user.as_ref(),
    )
    .await
    {
        Ok(b) => b,
        Err(ret) => return Json(ret).into_response(),
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let base_abs = tokio::fs::canonicalize(&base)
        .await
        .unwrap_or_else(|_| base.clone());
    let Some(target_dir) = resolve_secure_path_from_abs(&base_abs, &path) else {
        return Json(ReturnData::err("路径不存在")).into_response();
    };
    if !tokio::fs::metadata(&target_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Json(ReturnData::err("路径不存在")).into_response();
    }
    let target_dir_abs = tokio::fs::canonicalize(&target_dir)
        .await
        .unwrap_or_else(|_| target_dir.clone());
    let mut items: Vec<FileEntry> = Vec::new();
    for (name, bytes) in files {
        let safe_name = Path::new(&name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if safe_name.is_empty() || safe_name.starts_with('.') {
            continue;
        }
        let Some(dest) = resolve_secure_path_from_abs(&target_dir_abs, &safe_name) else {
            continue;
        };
        if let Some(parent) = dest.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&dest, &bytes).await.is_err() {
            continue;
        }
        items.push(build_file_entry(
            &base_abs,
            &dest,
            false,
            bytes.len() as u64,
            last_modified_millis(&dest).await,
        ));
    }
    ReturnData::ok_typed(items).into_response()
}

/// POST `/reader3/file/delete`：删除文件或目录（`path`，递归）
pub async fn delete(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let ns = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let home = str_param(&params, body_json.as_ref(), "home");
    let user = state.storage.find_user(&ns).await.ok().flatten();
    let manager = manager_ok(&state.storage.config, user.as_ref());
    let base = match file_home(
        &state.storage.config,
        &ns,
        &home,
        false,
        true,
        manager,
        user.as_ref(),
    )
    .await
    {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let base_abs = tokio::fs::canonicalize(&base)
        .await
        .unwrap_or_else(|_| base.clone());
    let Some(file) = resolve_secure_path_from_abs(&base_abs, &path) else {
        return Json(ReturnData::err("参数错误"));
    };
    let meta = match tokio::fs::metadata(&file).await {
        Ok(m) => m,
        Err(_) => return Json(ReturnData::err("路径不存在")),
    };
    let result = if meta.is_dir() {
        tokio::fs::remove_dir_all(&file).await
    } else {
        tokio::fs::remove_file(&file).await
    };
    match result {
        Ok(_) => Json(ReturnData::ok(Value::String(String::new()))),
        Err(e) => {
            tracing::error!("file/delete 失败 [{}]: {e}", file.display());
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST `/reader3/file/deleteMulti`：批量删除文件/目录
pub async fn delete_multi(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let ns = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let home = str_param(&params, body_json.as_ref(), "home");
    let user = state.storage.find_user(&ns).await.ok().flatten();
    let manager = manager_ok(&state.storage.config, user.as_ref());
    let base = match file_home(
        &state.storage.config,
        &ns,
        &home,
        false,
        true,
        manager,
        user.as_ref(),
    )
    .await
    {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let paths: Vec<String> = match &body_json {
        Some(Value::Object(obj)) => {
            let arr = obj
                .get("paths")
                .or_else(|| obj.get("path"))
                .and_then(|v| v.as_array());
            match arr {
                Some(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
                None => return Json(ReturnData::err("参数错误")),
            }
        }
        _ => return Json(ReturnData::err("参数错误")),
    };
    if paths.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let base_abs = tokio::fs::canonicalize(&base)
        .await
        .unwrap_or_else(|_| base.clone());
    let mut deleted = 0u64;
    let mut failed = 0u64;
    for path in paths {
        if path.is_empty() {
            continue;
        }
        let Some(file) = resolve_secure_path_from_abs(&base_abs, &path) else {
            continue;
        };
        let Ok(meta) = tokio::fs::metadata(&file).await else {
            continue;
        };
        let result = if meta.is_dir() {
            tokio::fs::remove_dir_all(&file).await
        } else {
            tokio::fs::remove_file(&file).await
        };
        match result {
            Ok(_) => deleted += 1,
            Err(e) => {
                tracing::error!("file/deleteMulti 失败 [{}]: {e}", file.display());
                failed += 1;
            }
        }
    }
    Json(ReturnData::ok(
        json!({ "deleted": deleted, "failed": failed }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_secure_path_blocks_traversal() {
        let dir = std::env::temp_dir().join(format!("reader-files-sec-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(resolve_secure_path(&dir, "/sub/a.zip").is_some());
        assert!(resolve_secure_path(&dir, "../../etc/passwd").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_range_header_handles_common_cases() {
        assert_eq!(parse_range_header(None, 100), RangeReq::Full);
        assert_eq!(
            parse_range_header(Some("bytes=0-9"), 100),
            RangeReq::Partial { start: 0, end: 9 }
        );
        assert_eq!(
            parse_range_header(Some("bytes=90-"), 100),
            RangeReq::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range_header(Some("bytes=-10"), 100),
            RangeReq::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range_header(Some("bytes=200-300"), 100),
            RangeReq::Unsatisfiable
        );
    }
}
