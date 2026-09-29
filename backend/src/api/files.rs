//! F-38 文件管理 API（/reader3/file/*，对齐 legacy FileController）
//!
//! home 语义（legacy FileController.checkAccess）：
//! - `__WEBDAV__`      → storage/data/{ns}/webdav（secure 模式需开启 webdav 权限）
//! - `__LOCAL_STORE__` → storage/localStore（secure 模式需开启本地书仓权限；写/删需管理密码）
//! - `__HOME__`        → storage/data/{ns}
//! - `__STORAGE__`     → storage 根（需管理密码）
//! - 空                → storage/data/{ns}（兼容旧客户端/手动构造 URL）
//! - 其他              → 非法访问
//!
//! 路径安全：resolve_secure_path（组件级 normalize + starts_with 校验，防穿越，
//! 参考 B-3 resolve_storage_path 模式）

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use axum::extract::{Multipart, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::router::{resolve_namespace, AppState, ReturnData};
use crate::AppConfig;

/// 参数提取：body JSON 优先，GET query 兜底（与 router 其余 handler 一致）
fn str_param(params: &HashMap<String, String>, body: Option<&Value>, key: &str) -> String {
    if let Some(b) = body {
        if let Some(v) = b.get(key).and_then(|v| v.as_str()) {
            return v.to_string();
        }
    }
    params.get(key).cloned().unwrap_or_default()
}

/// 管理权限判定：能否访问跨命名空间的管理路径（`__STORAGE__` 等）。
/// - 非 secure 模式 → 恒 true（单用户部署, 无登录概念）
/// - secure 模式 → 仅管理员（账户由部署配置 READER_APP_ADMIN_USERNAME 指定）
fn manager_ok(config: &AppConfig, user: Option<&crate::model::user::User>) -> bool {
    if !config.secure {
        return true;
    }
    user.map(|u| u.is_admin).unwrap_or(false)
}

fn manager_required() -> ReturnData {
    ReturnData::err("仅管理员可执行该操作")
}

fn login_required() -> ReturnData {
    ReturnData {
        is_success: false,
        error_msg: "请登录后使用".to_string(),
        data: json!("NEED_LOGIN"),
    }
}

/// 安全路径解析（防穿越）：rel 视为 base 下相对路径（去除前导 / 或 \，与 legacy
/// removePrefix 一致）；组件级处理 `..`（弹出一级，越出 base 即拒绝）；返回路径保证
/// 位于 base 内
pub(crate) fn resolve_secure_path(base: &Path, rel: &str) -> Option<PathBuf> {
    let base_abs = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
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
    if out.starts_with(&base_abs) {
        Some(out)
    } else {
        None
    }
}

/// home 语义解析（legacy checkAccess）：返回允许访问的根目录（自动建目录）
pub(crate) fn file_home(
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
        "__HOME__" => storage_dir.join("data").join(ns),
        "__STORAGE__" => {
            if !manager {
                return Err(manager_required());
            }
            storage_dir
        }
        "" => storage_dir.join("data").join(ns),
        _ => return Err(ReturnData::err("非法访问")),
    };
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir)
}

/// 条目 JSON（legacy list/upload 返回结构）
fn entry_json(base_abs: &Path, path: &Path, is_dir: bool, size: u64, last_modified: i64) -> Value {
    let rel = path
        .strip_prefix(base_abs)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    json!({
        "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "size": size,
        "path": format!("/{}", rel.trim_start_matches('/')),
        "lastModified": last_modified,
        "isDirectory": is_dir,
    })
}

fn last_modified_millis(path: &Path) -> i64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// GET /reader3/file/list：列目录（path + home 参数）
pub async fn list(
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
        false,
        manager,
        user.as_ref(),
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let Some(file) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("路径不存在"));
    };
    if !file.exists() {
        if path != "/" {
            return Json(ReturnData::err("路径不存在"));
        }
        if std::fs::create_dir_all(&file).is_err() {
            return Json(ReturnData::err("路径不存在"));
        }
    }
    if !file.is_dir() {
        return Json(ReturnData::err("路径不是目录"));
    }
    let base_abs = base.canonicalize().unwrap_or_else(|_| base.clone());
    let mut items: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&file) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let meta = entry.metadata().ok();
            items.push(entry_json(
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
    Json(ReturnData::ok(Value::Array(items)))
}

/// GET /reader3/file/get：读文件（文本内容，data = content 字符串）
pub async fn get(
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
        false,
        manager,
        user.as_ref(),
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let Some(file) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("参数错误"));
    };
    if !file.exists() {
        return Json(ReturnData::err("路径不存在"));
    }
    if !file.is_file() {
        return Json(ReturnData::err("路径不是文件"));
    }
    match tokio::fs::read_to_string(&file).await {
        Ok(content) => Json(ReturnData::ok(Value::String(content))),
        Err(e) => {
            tracing::error!("file/get 读取失败 [{}]: {e}", file.display());
            Json(ReturnData::err("读取失败"))
        }
    }
}

/// POST /reader3/file/save：写文件（body：path + content）
pub async fn save(
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
        true,
        false,
        manager,
        user.as_ref(),
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    let content = str_param(&params, body_json.as_ref(), "content");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let Some(file) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("参数错误"));
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match tokio::fs::write(&file, content).await {
        Ok(_) => Json(ReturnData::ok(Value::String(String::new()))),
        Err(e) => {
            tracing::error!("file/save 写入失败 [{}]: {e}", file.display());
            Json(ReturnData::err("保存失败"))
        }
    }
}

/// POST /reader3/file/mkdir：建目录（body：path + name）
pub async fn mkdir(
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
        true,
        false,
        manager,
        user.as_ref(),
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    let name = str_param(&params, body_json.as_ref(), "name");
    if path.is_empty() || name.is_empty() || name.starts_with('.') {
        return Json(ReturnData::err("参数错误"));
    }
    let Some(parent) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("参数错误"));
    };
    let Some(dir) = resolve_secure_path(&parent, &name) else {
        return Json(ReturnData::err("参数错误"));
    };
    if dir.exists() {
        return Json(ReturnData::err("路径已存在"));
    }
    match std::fs::create_dir_all(&dir) {
        Ok(_) => Json(ReturnData::ok(Value::String(String::new()))),
        Err(e) => {
            tracing::error!("file/mkdir 失败 [{}]: {e}", dir.display());
            Json(ReturnData::err("创建失败"))
        }
    }
}

/// POST /reader3/file/rename：重命名文件/目录（body：path + name；secure 模式书仓写需管理密码）
pub async fn rename(
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
        true,
        false,
        manager,
        user.as_ref(),
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    let name = str_param(&params, body_json.as_ref(), "name");
    if path.is_empty() || name.is_empty() || name.starts_with('.') {
        return Json(ReturnData::err("参数错误"));
    }
    if name.contains('/') || name.contains('\\') {
        return Json(ReturnData::err("名称不能包含路径分隔符"));
    }
    let Some(old_path) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("参数错误"));
    };
    if !old_path.exists() {
        return Json(ReturnData::err("文件不存在"));
    }
    let parent = old_path.parent().unwrap_or(&base);
    let Some(new_path) = resolve_secure_path(parent, &name) else {
        return Json(ReturnData::err("参数错误"));
    };
    if new_path.exists() {
        return Json(ReturnData::err("路径已存在"));
    }
    match std::fs::rename(&old_path, &new_path) {
        Ok(_) => Json(ReturnData::ok(Value::String(String::new()))),
        Err(e) => {
            tracing::error!(
                "file/rename 失败 [{}] → [{}]: {e}",
                old_path.display(),
                new_path.display()
            );
            Json(ReturnData::err("重命名失败"))
        }
    }
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
        "pdf" => "application/pdf",
        "epub" => "application/epub+zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Range 头解析结果：
/// - Full：无头 / 语法非法 / start>end 等无效规格 → 按 RFC 7233 忽略，200 全量
/// - Partial{start,end}：闭区间字节范围（end 已收敛到 total-1）
/// - Unsatisfiable：语法合法但越出文件（start>=total、空文件、-0）→ 416
#[derive(Debug, PartialEq, Eq)]
enum RangeReq {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

/// 解析 `Range: bytes=start-end`（兼容 `bytes=start-` 开放末端与 `bytes=-suffix`
/// 后缀形式）；多区间仅取首段（浏览器/下载器实际均为单区间）
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
        // 后缀形式 bytes=-N：最后 N 字节（N 超长 → 整个文件）
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
                    // last-byte-pos < first-byte-pos 属无效规格 → 忽略头
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

/// GET /reader3/file/download：下载文件（path；stream<=0 附件，>0 内联；
/// Range 断点续传：命中返回 206 + Content-Range 并 seek 只读区间）
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
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret).into_response(),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let Some(file) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("参数错误")).into_response();
    };
    if !file.exists() || !file.is_file() {
        return Json(ReturnData::err("路径不存在")).into_response();
    }
    let stream = params
        .get("stream")
        .and_then(|v| v.parse::<i64>().ok())
        .or_else(|| {
            body_json
                .as_ref()
                .and_then(|b| b.get("stream").and_then(|v| v.as_i64()))
        })
        .unwrap_or(0);
    let total = match std::fs::metadata(&file) {
        Ok(m) => m.len(),
        Err(e) => {
            tracing::error!("file/download 元数据读取失败 [{}]: {e}", file.display());
            return Json(ReturnData::err("读取失败")).into_response();
        }
    };
    let mime = mime_from_ext(
        file.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default(),
    );
    let disposition = (stream <= 0)
        .then(|| file.file_name().map(|n| n.to_string_lossy().into_owned()))
        .flatten()
        .map(|name| format!("attachment; filename={}", urlencoding::encode(&name)));
    // 公共响应头（Content-Type / Cache-Control / Accept-Ranges / 附件 Content-Disposition）
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
            // 只读请求区间：seek 到 start，take(end-start+1)（不整文件载入内存）
            let length = end - start + 1;
            let read = (|| -> std::io::Result<Vec<u8>> {
                use std::io::{Read, Seek, SeekFrom};
                let mut f = std::fs::File::open(&file)?;
                f.seek(SeekFrom::Start(start))?;
                let mut buf = Vec::with_capacity(length as usize);
                f.take(length).read_to_end(&mut buf)?;
                Ok(buf)
            })();
            match read {
                Ok(bytes) => with_common(
                    Response::builder()
                        .status(axum::http::StatusCode::PARTIAL_CONTENT)
                        .header("Content-Range", format!("bytes {start}-{end}/{total}")),
                )
                .body(axum::body::Body::from(bytes))
                .unwrap(),
                Err(e) => {
                    tracing::error!("file/download 区间读取失败 [{}]: {e}", file.display());
                    Json(ReturnData::err("读取失败")).into_response()
                }
            }
        }
        RangeReq::Unsatisfiable => with_common(
            Response::builder()
                .status(axum::http::StatusCode::RANGE_NOT_SATISFIABLE)
                .header("Content-Range", format!("bytes */{total}")),
        )
        .body(axum::body::Body::empty())
        .unwrap(),
        RangeReq::Full => match tokio::fs::read(&file).await {
            Ok(bytes) => with_common(Response::builder().status(axum::http::StatusCode::OK))
                .body(axum::body::Body::from(bytes))
                .unwrap(),
            Err(e) => {
                tracing::error!("file/download 读取失败 [{}]: {e}", file.display());
                Json(ReturnData::err("读取失败")).into_response()
            }
        },
    }
}

/// POST /reader3/file/upload：上传（multipart：home/path 字段 + file 字段，可多文件）
pub async fn upload(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Json<ReturnData> {
    let ns = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret),
    };
    let mut home = params.get("home").cloned().unwrap_or_default();
    let mut path = params.get("path").cloned().unwrap_or_default();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let max_bytes = state.storage.config.upload_max_bytes();
    let max_mb = state.storage.config.upload_max_mb;
    // GAP 62：Content-Length 预检（超限 → 明确错误）
    if let Some(msg) = crate::api::router::check_upload_content_length(&headers, max_bytes, max_mb)
    {
        return Json(ReturnData::err(msg));
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
                    // GAP 62：显式字段大小上限（超限 → 明确错误）
                    match crate::api::router::read_multipart_field_limited(
                        &mut field, max_bytes, max_mb,
                    )
                    .await
                    {
                        Ok(bytes) => files.push((name, bytes)),
                        Err(msg) => return Json(ReturnData::err(msg)),
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
        return Json(ReturnData::err("请上传文件"));
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
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let Some(target_dir) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("路径不存在"));
    };
    if !target_dir.is_dir() {
        return Json(ReturnData::err("路径不存在"));
    }
    let base_abs = base.canonicalize().unwrap_or_else(|_| base.clone());
    let mut items: Vec<Value> = Vec::new();
    for (name, bytes) in files {
        // 文件名只取 basename（防路径穿越：../x、a/b 均收敛为安全名）
        let safe_name = Path::new(&name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if safe_name.is_empty() || safe_name.starts_with('.') {
            continue;
        }
        let Some(dest) = resolve_secure_path(&target_dir, &safe_name) else {
            continue;
        };
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&dest, &bytes).is_err() {
            continue;
        }
        items.push(entry_json(
            &base_abs,
            &dest,
            false,
            bytes.len() as u64,
            last_modified_millis(&dest),
        ));
    }
    Json(ReturnData::ok(Value::Array(items)))
}

/// POST /reader3/file/delete：删除文件/目录（path，递归）
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
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let path = str_param(&params, body_json.as_ref(), "path");
    if path.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let Some(file) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("参数错误"));
    };
    if !file.exists() {
        return Json(ReturnData::err("路径不存在"));
    }
    let result = if file.is_dir() {
        std::fs::remove_dir_all(&file)
    } else {
        std::fs::remove_file(&file)
    };
    match result {
        Ok(_) => Json(ReturnData::ok(Value::String(String::new()))),
        Err(e) => {
            tracing::error!("file/delete 失败 [{}]: {e}", file.display());
            Json(ReturnData::err("删除失败"))
        }
    }
}

/// POST /reader3/file/deleteMulti：批量删除文件/目录（legacy 对齐）。
/// body：`{"paths":["/a","/b"], "home": "..."}`（兼容 legacy `path` 数组键）；
/// 逐路径静默跳过不存在/非法（防穿越拒绝）路径；目录递归删除。
/// GET+POST /reader3/file/parse：递归扫描目录中的书籍文件（P0 路由补齐，
/// legacy FileController.parse 对齐）。
/// - 扩展名白名单：txt/epub/umd/cbz/pdf；跳过隐藏文件
/// - import=0 → 返回 [{name,size,path,lastModified,book}]（book 为解析出的元数据）
/// - import>0 → 直接入架（upsert 到书架，origin=loc_book），返回 [{name}]
pub async fn parse(
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
    let path = {
        let p = str_param(&params, body_json.as_ref(), "path");
        if p.is_empty() {
            "/".to_string()
        } else {
            p
        }
    };
    let import = {
        let raw = str_param(&params, body_json.as_ref(), "import");
        raw.trim().parse::<i64>().unwrap_or(0)
    };
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
    ) {
        Ok(b) => b,
        Err(ret) => return Json(ret),
    };
    let Some(directory) = resolve_secure_path(&base, &path) else {
        return Json(ReturnData::err("路径不存在"));
    };
    if !directory.exists() {
        return Json(ReturnData::err("路径不存在"));
    }
    if !directory.is_dir() {
        return Json(ReturnData::err("路径不是目录"));
    }

    const BOOK_EXTS: &[&str] = &["txt", "epub", "umd", "cbz", "pdf"];
    let storage_root = state.storage.config.storage_dir();
    let mut out: Vec<Value> = Vec::new();

    // 递归遍历（迭代栈避免深目录递归）
    let mut stack = vec![directory.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !p.is_file() {
                continue;
            }
            let ext = crate::service::local_book::file_ext(&name);
            if !BOOK_EXTS.contains(&ext.as_str()) {
                continue;
            }
            // book_url：相对 storage 根的 POSIX 路径（resolve_loc_book_file 兼容该形态）
            let rel_from_storage = p
                .strip_prefix(&storage_root)
                .map(|rp| rp.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| p.to_string_lossy().replace('\\', "/"));
            let (bname, bauthor) = crate::service::local_book::analyze_name_author(&name);
            let display_name = if bname.is_empty() {
                std::path::Path::new(&name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(name.clone())
            } else {
                bname
            };
            if import > 0 {
                let book = crate::model::Book {
                    book_url: rel_from_storage.clone(),
                    name: display_name.clone(),
                    author: bauthor,
                    origin: "loc_book".into(),
                    origin_name: String::new(),
                    toc_url: rel_from_storage.clone(),
                    book_type: crate::service::local_book::local_book_type(&ext),
                    is_in_shelf: true,
                    ..Default::default()
                };
                if state.storage.upsert_book(&ns, &book).await.is_ok() {
                    out.push(json!({ "name": name }));
                }
            } else {
                let meta = entry.metadata().ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let last_modified = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let rel_to_base = p
                    .strip_prefix(&directory)
                    .map(|rp| format!("/{}", rp.to_string_lossy().replace('\\', "/")))
                    .unwrap_or_else(|_| format!("/{name}"));
                out.push(json!({
                    "name": name,
                    "size": size,
                    "path": rel_to_base,
                    "lastModified": last_modified,
                    "book": {
                        "bookUrl": rel_from_storage,
                        "name": display_name,
                        "author": bauthor,
                        "origin": "loc_book",
                        "tocUrl": rel_from_storage,
                        "type": crate::service::local_book::local_book_type(&ext),
                    },
                }));
            }
        }
    }
    Json(ReturnData::ok(Value::Array(out)))
}

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
    ) {
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
    let mut deleted = 0u64;
    let mut failed = 0u64;
    for path in paths {
        if path.is_empty() {
            continue;
        }
        // 非法（防穿越）路径静默跳过（legacy：resolveSecurePath 失败即跳过）
        let Some(file) = resolve_secure_path(&base, &path) else {
            continue;
        };
        if !file.exists() {
            continue;
        }
        let result = if file.is_dir() {
            std::fs::remove_dir_all(&file)
        } else {
            std::fs::remove_file(&file)
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
    use axum::body::Bytes;

    async fn test_state(tag: &str) -> (AppState, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("reader-files-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        // 既有认证测试不关心过期——默认禁用（GAP 118 过期由 router.rs 专用测试单独覆盖）
        config.token_ttl_days = 0;
        let storage = crate::storage::init(&config).await.unwrap();
        // 图片代理磁盘缓存：独立临时目录 + 固定 1MB 容量（不受宿主 env READER_IMAGE_CACHE_MB 影响）
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

    async fn cleanup(state: AppState, dir: std::path::PathBuf) {
        state.storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 路径安全：正常/穿越/绝对路径/.. 弹回 四类
    #[test]
    fn test_resolve_secure_path() {
        let base = std::env::temp_dir().join("reader-files-resolve");
        let _ = std::fs::create_dir_all(&base);
        let base_abs = base.canonicalize().unwrap();
        assert_eq!(
            resolve_secure_path(&base, "a/b.txt").unwrap(),
            base_abs.join("a/b.txt")
        );
        assert_eq!(
            resolve_secure_path(&base, "a/../b.txt").unwrap(),
            base_abs.join("b.txt"),
            ".. 弹回 base 内应放行"
        );
        assert_eq!(
            resolve_secure_path(&base, "/a.txt").unwrap(),
            base_abs.join("a.txt"),
            "绝对路径按相对处理（legacy removePrefix）"
        );
        assert!(
            resolve_secure_path(&base, "../escape.txt").is_none(),
            "越出 base 应拒绝"
        );
        assert!(
            resolve_secure_path(&base, "a/../../escape.txt").is_none(),
            "多次 .. 越出 base 应拒绝"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// home 语义：非 secure 全通过；secure 下权限/管理密码门槛
    #[test]
    fn test_file_home_semantics() {
        let dir = std::env::temp_dir().join("reader-files-home");
        let mut config = AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        config.secure = false;
        let storage_dir = config.storage_dir();

        assert_eq!(
            file_home(&config, "default", "", false, false, true, None).unwrap(),
            storage_dir.join("data/default"),
            "空 home 回退用户目录"
        );
        assert_eq!(
            file_home(&config, "alice", "__HOME__", false, false, true, None).unwrap(),
            storage_dir.join("data/alice")
        );
        assert_eq!(
            file_home(&config, "alice", "__WEBDAV__", false, false, true, None).unwrap(),
            storage_dir.join("data/alice/webdav")
        );
        assert_eq!(
            file_home(
                &config,
                "alice",
                "__LOCAL_STORE__",
                false,
                false,
                true,
                None
            )
            .unwrap(),
            storage_dir.join("localStore")
        );
        assert_eq!(
            file_home(&config, "alice", "__STORAGE__", false, false, true, None).unwrap(),
            storage_dir
        );
        assert!(
            file_home(&config, "alice", "__OTHER__", false, false, true, None).is_err(),
            "未知 home 应拒绝"
        );

        // secure：__STORAGE__ 与 __LOCAL_STORE__ 写/删需管理密码
        config.secure = true;
        config.secure_key = "secret".into();
        let user_ok = crate::model::User {
            username: "alice".into(),
            enable_webdav: true,
            enable_local_store: true,
            ..Default::default()
        };
        assert!(file_home(&config, "alice", "__STORAGE__", false, false, false, None).is_err());
        assert!(file_home(&config, "alice", "__STORAGE__", false, false, true, None).is_ok());
        assert!(file_home(
            &config,
            "alice",
            "__LOCAL_STORE__",
            true,
            false,
            false,
            Some(&user_ok)
        )
        .is_err());
        assert!(file_home(
            &config,
            "alice",
            "__LOCAL_STORE__",
            false,
            true,
            false,
            Some(&user_ok)
        )
        .is_err());
        assert!(file_home(
            &config,
            "alice",
            "__LOCAL_STORE__",
            true,
            false,
            true,
            Some(&user_ok)
        )
        .is_ok());
        // secure：未开启 webdav/本地书仓 → 拒绝
        let user = crate::model::User {
            username: "alice".into(),
            enable_webdav: false,
            enable_local_store: true,
            ..Default::default()
        };
        assert!(file_home(
            &config,
            "alice",
            "__WEBDAV__",
            false,
            false,
            true,
            Some(&user)
        )
        .is_err());
        let user = crate::model::User {
            username: "alice".into(),
            enable_webdav: true,
            enable_local_store: false,
            ..Default::default()
        };
        assert!(file_home(
            &config,
            "alice",
            "__LOCAL_STORE__",
            false,
            false,
            true,
            Some(&user)
        )
        .is_err());
        assert!(file_home(
            &config,
            "alice",
            "__WEBDAV__",
            false,
            false,
            true,
            Some(&user_ok)
        )
        .is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全链路：save → get → list → mkdir → delete（非 secure，default 命名空间）
    #[tokio::test]
    async fn test_file_handler_roundtrip() {
        let (state, dir) = test_state("roundtrip").await;
        let headers = HeaderMap::new();

        // save 写文件
        let body = Bytes::from(r#"{"path":"/docs/a.txt","content":"hello"}"#);
        let ret = save(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success, "save 应成功: {}", ret.0.error_msg);

        // get 读回
        let params: HashMap<String, String> = [("path".into(), "/docs/a.txt".into())]
            .into_iter()
            .collect();
        let ret = get(State(state.clone()), Query(params), headers.clone(), None).await;
        assert!(ret.0.is_success);
        assert_eq!(ret.0.data, json!("hello"));

        // rename 文件：/docs/a.txt → /docs/b.txt
        let body = Bytes::from(r#"{"path":"/docs/a.txt","name":"b.txt"}"#);
        let ret = rename(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success, "rename 应成功: {}", ret.0.error_msg);
        let params: HashMap<String, String> = [("path".into(), "/docs/b.txt".into())]
            .into_iter()
            .collect();
        let ret = get(State(state.clone()), Query(params), headers.clone(), None).await;
        assert!(ret.0.is_success);
        assert_eq!(ret.0.data, json!("hello"));
        let params: HashMap<String, String> = [("path".into(), "/docs/a.txt".into())]
            .into_iter()
            .collect();
        let ret = get(State(state.clone()), Query(params), headers.clone(), None).await;
        assert!(!ret.0.is_success, "旧路径应已不存在");

        // list 列目录（含 docs）
        let ret = list(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            None,
        )
        .await;
        assert!(ret.0.is_success);
        let arr = ret.0.data.as_array().unwrap();
        assert!(
            arr.iter().any(|v| v["name"] == "docs"),
            "列表应含 docs: {arr:?}"
        );

        // mkdir + 重复建报路径已存在
        let body = Bytes::from(r#"{"path":"/","name":"sub"}"#);
        let ret = mkdir(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success);
        let body = Bytes::from(r#"{"path":"/","name":"sub"}"#);
        let ret = mkdir(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(!ret.0.is_success);
        assert_eq!(ret.0.error_msg, "路径已存在");

        // 穿越路径拒绝（save 到 ../ 外）
        let body = Bytes::from(r#"{"path":"/../../evil.txt","content":"x"}"#);
        let ret = save(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(!ret.0.is_success, "穿越路径应拒绝");

        // delete 目录（递归）
        let body = Bytes::from(r#"{"path":"/docs"}"#);
        let ret = delete(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success);
        let params: HashMap<String, String> = [("path".into(), "/docs/a.txt".into())]
            .into_iter()
            .collect();
        let ret = get(State(state.clone()), Query(params), headers, None).await;
        assert!(!ret.0.is_success);
        assert_eq!(ret.0.error_msg, "路径不存在");

        cleanup(state, dir).await;
    }

    /// file/deleteMulti：批量删文件+目录；legacy `path` 键兼容；防穿越跳过；缺 paths 报参数错误
    #[tokio::test]
    async fn test_file_delete_multi() {
        let (state, dir) = test_state("delmulti").await;
        let headers = HeaderMap::new();

        // 准备：a.txt / b.txt / docs/c.txt / 外部 escape.txt
        let base = state
            .storage
            .config
            .storage_dir()
            .join("data")
            .join("default");
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::write(base.join("a.txt"), "a").unwrap();
        std::fs::write(base.join("b.txt"), "b").unwrap();
        std::fs::write(base.join("docs/c.txt"), "c").unwrap();
        let outside = dir.join("escape.txt");
        std::fs::write(&outside, "x").unwrap();

        // {paths} 批量删除：文件 + 目录 + 不存在的路径（跳过）+ 防穿越路径（跳过）
        let body = Bytes::from(
            r#"{"paths":["/a.txt","/docs","/missing.txt","/../escape.txt"],"home":""}"#,
        );
        let ret = delete_multi(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success, "deleteMulti 应成功: {}", ret.0.error_msg);
        assert_eq!(
            ret.0.data["deleted"], 2,
            "应删除 a.txt + docs 目录: {:?}",
            ret.0.data
        );
        assert!(!base.join("a.txt").exists());
        assert!(!base.join("docs").exists());
        assert!(outside.exists(), "外部文件不应被删");
        assert!(base.join("b.txt").exists());

        // legacy `path` 键（数组）兼容
        let body = Bytes::from(r#"{"path":["/b.txt"],"home":""}"#);
        let ret = delete_multi(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success);
        assert_eq!(ret.0.data["deleted"], 1);
        assert!(!base.join("b.txt").exists());

        // 缺 paths/path 键 → 参数错误；空数组 → 参数错误
        let body = Bytes::from(r#"{"home":""}"#);
        let ret = delete_multi(
            State(state.clone()),
            Query(HashMap::new()),
            headers.clone(),
            Some(body),
        )
        .await;
        assert!(!ret.0.is_success);
        assert_eq!(ret.0.error_msg, "参数错误");
        let body = Bytes::from(r#"{"paths":[],"home":""}"#);
        let ret = delete_multi(
            State(state.clone()),
            Query(HashMap::new()),
            headers,
            Some(body),
        )
        .await;
        assert!(!ret.0.is_success);
        assert_eq!(ret.0.error_msg, "参数错误");

        cleanup(state, dir).await;
    }

    /// P0-8：manager_ok 收紧——仅 secure_key 已配置且请求携带正确 secureKey 才具备管理权限；
    /// 管理路径权限：非 secure 恒真（单用户无登录概念）；secure 仅管理员
    /// （管理密码已废弃——是管理员即可直接改配置）
    #[test]
    fn test_manager_ok() {
        let mut config = AppConfig::from_env();
        let admin = crate::model::User {
            username: "admin".into(),
            is_admin: true,
            ..Default::default()
        };
        let plain = crate::model::User {
            username: "alice".into(),
            ..Default::default()
        };

        // 非 secure（单用户）：恒 manager
        config.secure = false;
        assert!(manager_ok(&config, None));
        assert!(manager_ok(&config, Some(&plain)));

        // secure：匿名/普通用户拒绝, 仅管理员放行（无论 secure_key 是否配置）
        config.secure = true;
        config.secure_key = "".into();
        assert!(!manager_ok(&config, None), "匿名不得访问管理路径");
        assert!(!manager_ok(&config, Some(&plain)), "普通用户不得访问管理路径");
        assert!(manager_ok(&config, Some(&admin)), "管理员可访问管理路径");
        config.secure_key = "sk-123".into();
        assert!(!manager_ok(&config, None));
        assert!(!manager_ok(&config, Some(&plain)));
        assert!(manager_ok(&config, Some(&admin)));
    }

    /// 全链路：secure 模式下写 __STORAGE__（跨命名空间管理路径）仅管理员放行;
    /// 匿名与普通用户一律拒绝
    #[tokio::test]
    async fn test_manager_gate_secure_storage_write() {
        let (mut state, dir) = test_state("mgrgate").await;
        state.storage.config.secure = true;
        state.storage.config.secure_key = "".into(); // 未配置 secure_key（P0-8 场景）
        state
            .storage
            .insert_user(&crate::model::User {
                username: "alice".into(),
                token: "tok".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let auth: HashMap<String, String> = [("accessToken".into(), "alice:tok".into())]
            .into_iter()
            .collect();

        // 普通用户：写 __STORAGE__ 拒绝（哪怕带 secureKey 也不再放行）
        let body = Bytes::from(r#"{"path":"/pwn.txt","content":"x","home":"__STORAGE__"}"#);
        let ret = save(
            State(state.clone()),
            Query(auth.clone()),
            HeaderMap::new(),
            Some(body),
        )
        .await;
        assert!(!ret.0.is_success, "普通用户写 __STORAGE__ 应拒绝");
        assert_eq!(ret.0.error_msg, "仅管理员可执行该操作");
        assert!(!state.storage.config.storage_dir().join("pwn.txt").exists());

        // 匿名：同样拒绝
        let body = Bytes::from(r#"{"path":"/pwn.txt","content":"x","home":"__STORAGE__"}"#);
        let ret = save(
            State(state.clone()),
            Query(HashMap::new()),
            HeaderMap::new(),
            Some(body),
        )
        .await;
        assert!(!ret.0.is_success, "匿名写 __STORAGE__ 应拒绝");

        // 管理员：放行
        state
            .storage
            .insert_user(&crate::model::User {
                username: "root".into(),
                token: "rtok".into(),
                is_admin: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let admin_auth: HashMap<String, String> =
            [("accessToken".into(), "root:rtok".into())]
                .into_iter()
                .collect();
        let body = Bytes::from(r#"{"path":"/pwn.txt","content":"x","home":"__STORAGE__"}"#);
        let ret = save(
            State(state.clone()),
            Query(admin_auth),
            HeaderMap::new(),
            Some(body),
        )
        .await;
        assert!(ret.0.is_success, "管理员应放行: {}", ret.0.error_msg);
        assert!(state.storage.config.storage_dir().join("pwn.txt").exists());

        cleanup(state, dir).await;
    }

    /// MIME 推断：常见扩展名 + 大小写归一 + 未知回退 octet-stream
    #[test]
    fn test_mime_from_ext() {
        assert_eq!(mime_from_ext("txt"), "text/plain");
        assert_eq!(mime_from_ext("HTML"), "text/html");
        assert_eq!(mime_from_ext("Htm"), "text/html");
        assert_eq!(mime_from_ext("css"), "text/css");
        assert_eq!(mime_from_ext("js"), "application/javascript");
        assert_eq!(mime_from_ext("json"), "application/json");
        assert_eq!(mime_from_ext("jpg"), "image/jpeg");
        assert_eq!(mime_from_ext("JPEG"), "image/jpeg");
        assert_eq!(mime_from_ext("png"), "image/png");
        assert_eq!(mime_from_ext("gif"), "image/gif");
        assert_eq!(mime_from_ext("webp"), "image/webp");
        assert_eq!(mime_from_ext("svg"), "image/svg+xml");
        assert_eq!(mime_from_ext("pdf"), "application/pdf");
        assert_eq!(mime_from_ext("epub"), "application/epub+zip");
        assert_eq!(mime_from_ext("mp3"), "audio/mpeg");
        assert_eq!(mime_from_ext("mp4"), "video/mp4");
        assert_eq!(mime_from_ext("xyz"), "application/octet-stream");
        assert_eq!(mime_from_ext(""), "application/octet-stream");
    }

    /// Range 头解析：完整/开放末端/后缀/越界 416/非法忽略/空文件
    #[test]
    fn test_parse_range_header() {
        // 无头 / 非 bytes 单位 / 语法非法 → 忽略头（200 全量）
        assert_eq!(parse_range_header(None, 100), RangeReq::Full);
        assert_eq!(parse_range_header(Some("items=0-5"), 100), RangeReq::Full);
        assert_eq!(parse_range_header(Some("bytes=abc"), 100), RangeReq::Full);
        // 完整形式 bytes=start-end（end 超长收敛到 total-1）
        assert_eq!(
            parse_range_header(Some("bytes=2-5"), 100),
            RangeReq::Partial { start: 2, end: 5 }
        );
        assert_eq!(
            parse_range_header(Some("bytes=95-200"), 100),
            RangeReq::Partial { start: 95, end: 99 },
            "end 超出文件长度应截断到 total-1"
        );
        // 开放末端 bytes=start-
        assert_eq!(
            parse_range_header(Some("bytes=10-"), 100),
            RangeReq::Partial { start: 10, end: 99 }
        );
        // 后缀形式 bytes=-suffix
        assert_eq!(
            parse_range_header(Some("bytes=-7"), 100),
            RangeReq::Partial { start: 93, end: 99 }
        );
        assert_eq!(
            parse_range_header(Some("bytes=-500"), 100),
            RangeReq::Partial { start: 0, end: 99 },
            "后缀超过文件长度应返回整个文件"
        );
        // 合法语法但不可满足 → 416
        assert_eq!(
            parse_range_header(Some("bytes=100-"), 100),
            RangeReq::Unsatisfiable
        );
        assert_eq!(
            parse_range_header(Some("bytes=300-400"), 100),
            RangeReq::Unsatisfiable
        );
        assert_eq!(
            parse_range_header(Some("bytes=0-"), 0),
            RangeReq::Unsatisfiable
        );
        assert_eq!(
            parse_range_header(Some("bytes=2-"), 0),
            RangeReq::Unsatisfiable
        );
        assert_eq!(
            parse_range_header(Some("bytes=-0"), 100),
            RangeReq::Unsatisfiable
        );
        // 无效规格 → 忽略（200）
        assert_eq!(
            parse_range_header(Some("bytes=5-2"), 100),
            RangeReq::Full,
            "start>end 属无效规格应忽略"
        );
        // 多区间取首段；容忍空白
        assert_eq!(
            parse_range_header(Some("bytes=2-5,10-20"), 100),
            RangeReq::Partial { start: 2, end: 5 }
        );
        assert_eq!(
            parse_range_header(Some(" bytes=2-5 "), 100),
            RangeReq::Partial { start: 2, end: 5 }
        );
    }

    /// download 全链路：MIME 推断（含内联 stream=1）、Range 206 分片、开放末端/
    /// 后缀分片、越界 416、无 Range 200 全量 + Accept-Ranges
    #[tokio::test]
    async fn test_download_mime_and_range() {
        let (state, dir) = test_state("dlrange").await;
        let base = state
            .storage
            .config
            .storage_dir()
            .join("data")
            .join("default");
        std::fs::create_dir_all(&base).unwrap();
        let content: Vec<u8> = (0..=255u8).collect();
        std::fs::write(base.join("data.bin"), &content).unwrap();

        let q = || {
            Query(
                [("path".to_string(), "/data.bin".to_string())]
                    .into_iter()
                    .collect::<HashMap<String, String>>(),
            )
        };
        let body_of = |resp: Response| async move {
            axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec()
        };

        // 无 Range：200 全量 + octet-stream（未知扩展名）+ Accept-Ranges + attachment
        let resp = download(State(state.clone()), q(), HeaderMap::new(), None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h["Content-Type"], "application/octet-stream");
        assert_eq!(h["Accept-Ranges"], "bytes");
        assert_eq!(h["Content-Disposition"], "attachment; filename=data.bin");
        assert_eq!(&*body_of(resp).await, &content[..]);

        // 中段区间：206 + Content-Range + 精确字节切片（seek 只读区间）
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::RANGE,
            axum::http::HeaderValue::from_static("bytes=10-19"),
        );
        let resp = download(State(state.clone()), q(), h, None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()["Content-Range"], "bytes 10-19/256");
        assert_eq!(&*body_of(resp).await, &content[10..20]);

        // 开放末端 bytes=250-
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::RANGE,
            axum::http::HeaderValue::from_static("bytes=250-"),
        );
        let resp = download(State(state.clone()), q(), h, None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()["Content-Range"], "bytes 250-255/256");
        assert_eq!(&*body_of(resp).await, &content[250..]);

        // 后缀形式 bytes=-6 → 最后 6 字节
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::RANGE,
            axum::http::HeaderValue::from_static("bytes=-6"),
        );
        let resp = download(State(state.clone()), q(), h, None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()["Content-Range"], "bytes 250-255/256");
        assert_eq!(&*body_of(resp).await, &content[250..]);

        // 非法 Range 头 → 忽略，仍 200 全量
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::RANGE,
            axum::http::HeaderValue::from_static("bytes=abc"),
        );
        let resp = download(State(state.clone()), q(), h, None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // 越界起点 → 416 + Content-Range: bytes */total
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::RANGE,
            axum::http::HeaderValue::from_static("bytes=300-400"),
        );
        let resp = download(State(state.clone()), q(), h, None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(resp.headers()["Content-Range"], "bytes */256");

        // MIME 经 handler 生效：sample.txt + stream=1 内联（无 attachment）→ text/plain
        std::fs::write(base.join("sample.txt"), b"hello range").unwrap();
        let params: HashMap<String, String> = [
            ("path".to_string(), "/sample.txt".to_string()),
            ("stream".to_string(), "1".to_string()),
        ]
        .into_iter()
        .collect();
        let resp = download(State(state.clone()), Query(params), HeaderMap::new(), None).await;
        let h = resp.headers();
        assert_eq!(h["Content-Type"], "text/plain");
        assert!(
            h.get("Content-Disposition").is_none(),
            "内联不应带 attachment"
        );
        assert_eq!(
            &*axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
            b"hello range"
        );

        cleanup(state, dir).await;
    }
}
