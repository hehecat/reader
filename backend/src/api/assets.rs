//! 静态资源、防盗链图片代理、封面代理、EPUB 资源与 SPA Fallback

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::auth::{resolve_current_user, resolve_namespace};
use super::router::{AppState, ReturnData};

/// 上游 `Content-Type` 合法性校验：仅接受 RFC 7230 token 字符集的 `type/subtype[; k=v]`，
/// 拒绝控制字符/CRLF 头注入；非法回退默认 `image/png`。
pub(crate) fn sanitize_proxy_content_type(content_type: Option<&str>) -> String {
    const DEFAULT_CT: &str = "image/png";
    let is_token = |x: &str| -> bool {
        !x.is_empty()
            && x.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
    };
    let Some(ct) = content_type else {
        return DEFAULT_CT.to_string();
    };
    let ct = ct.trim();
    if ct.is_empty() || ct.bytes().any(|b| matches!(b, 0x0A | 0x0D | 0x00 | 0x7F)) {
        return DEFAULT_CT.to_string();
    }
    let mut segs = ct.split(';');
    let media = segs.next().unwrap_or("").trim();
    let mut parts = media.splitn(2, '/');
    let (t, s) = (parts.next(), parts.next());
    if t.map(is_token).unwrap_or(false) && s.map(is_token).unwrap_or(false) {
        let params_ok = segs.all(|p| {
            let p = p.trim();
            let mut kv = p.splitn(2, '=');
            match (kv.next(), kv.next()) {
                (Some(k), Some(v)) => is_token(k) && is_token(v.trim_matches('"')),
                _ => false,
            }
        });
        if params_ok {
            return ct.to_string();
        }
    }
    DEFAULT_CT.to_string()
}

/// GET `/assets/proxy?url=&referer=&fmt=webp&q=80`：封面/正文图片防盗链代理（磁盘 LRU 缓存）
pub async fn assets_proxy(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let url = params.get("url").cloned().unwrap_or_default();
    if url.is_empty() {
        return Json(ReturnData::err("参数错误")).into_response();
    }
    let parsed = match url::Url::parse(&url) {
        Ok(p) if matches!(p.scheme(), "http" | "https") => p,
        _ => return Json(ReturnData::err("参数错误")).into_response(),
    };
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return (StatusCode::UNAUTHORIZED, Json(ret)).into_response(),
    };
    let referer = params.get("referer").cloned();
    let to_webp = params
        .get("fmt")
        .map(|v| v.eq_ignore_ascii_case("webp"))
        .unwrap_or(false);
    let quality: u8 = params.get("q").and_then(|v| v.parse().ok()).unwrap_or(80);
    match state
        .image_cache
        .get_or_fetch(
            &namespace,
            parsed.as_str(),
            referer.as_deref(),
            10,
            5 * 1024 * 1024,
        )
        .await
    {
        Ok((bytes, content_type, status, from_cache)) => {
            let cache_control = if from_cache {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            let is_raster = content_type
                .as_deref()
                .map(|ct| ct.starts_with("image/"))
                .unwrap_or(false);
            let converted = if to_webp && is_raster {
                crate::service::imaging::to_webp(&bytes, quality)
            } else {
                None
            };
            let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let (ct, body_bytes) = match converted {
                Some(webp) => ("image/webp".to_string(), webp),
                None => (sanitize_proxy_content_type(content_type.as_deref()), bytes),
            };
            Response::builder()
                .status(status_code)
                .header(header::CONTENT_TYPE, ct)
                .header(header::CACHE_CONTROL, cache_control)
                .body(Body::from(body_bytes))
                .unwrap_or_else(|e| {
                    tracing::error!("图片代理响应构造失败: {e}");
                    Json(ReturnData::err("系统错误")).into_response()
                })
        }
        Err(e) => {
            tracing::warn!("图片代理失败 [{url}]: {e}");
            Json(ReturnData::err(format!("图片加载失败：{e}"))).into_response()
        }
    }
}

/// GET `/reader3/cover?path=<图片URL>`：封面代理（复用 `image_cache`）
pub async fn book_cover_legacy(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let path = params.get("path").cloned().unwrap_or_default();
    if path.is_empty() {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(_) => return (StatusCode::NOT_FOUND, "").into_response(),
    };
    let referer = path.rsplit_once('/').map(|(base, _)| base.to_string());
    match state
        .image_cache
        .get_or_fetch(&namespace, &path, referer.as_deref(), 10, 5 * 1024 * 1024)
        .await
    {
        Ok((bytes, content_type, status, _from_cache))
            if (200..300).contains(&status) && !bytes.is_empty() =>
        {
            let ct = sanitize_proxy_content_type(content_type.as_deref());
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ct)
                .header(header::CACHE_CONTROL, "max-age=86400")
                .body(Body::from(bytes))
                .unwrap_or_else(|_| (StatusCode::NOT_FOUND, "").into_response())
        }
        _ => (StatusCode::NOT_FOUND, "").into_response(),
    }
}

/// GET `/epub/*rest`：`storage/data/**` EPUB 章节 HTML（含 `__API_ROOT__` 注入）
pub async fn epub_asset(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    serve_data_file(&state, &headers, "/epub/", uri.path()).await
}

/// SPA fallback：未匹配 `/reader3/*` 返回 JSON 404；
/// 其余先按统一来源（磁盘 `web_root` 若有真实 `index.html` 则优先磁盘，否则走内嵌）查找精确静态文件；
/// 静态资源路径（`/static/*`、`/fonts/*` 或带扩展名文件）缺失时必须返回真实 404，绝不回退 `index.html`（否则会污染浏览器/SW 缓存导致 chunk 失效死循环）；
/// 仅无扩展名的 SPA 路由回退 `index.html`。
pub async fn fallback_handler(
    State(state): State<AppState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Response {
    let path = uri.path();
    tracing::debug!("fallback: {} {}", method, path);
    if path.starts_with("/reader3") {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "isSuccess": false,
                "errorMsg": "接口不存在",
                "data": null
            })),
        )
            .into_response();
    }

    let rel = path.trim_start_matches('/');
    let is_root_or_html = rel.is_empty() || rel == "index.html" || rel.ends_with("/index.html");
    let no_cache_file = is_root_or_html || rel == "sw.js" || rel == "manifest.webmanifest";
    let is_asset_request = is_static_asset_path(rel);

    let web_root = std::path::PathBuf::from(&state.storage.config.web_root);
    let disk_ready = web_root.join("index.html").is_file();

    if disk_ready {
        let lookup = if rel.is_empty() { "index.html" } else { rel };
        let file = web_root.join(lookup);
        if let (Ok(file_abs), Ok(root_abs)) = (file.canonicalize(), web_root.canonicalize()) {
            if file_abs.starts_with(&root_abs) && file_abs.is_file() {
                if let Ok(bytes) = tokio::fs::read(&file_abs).await {
                    return build_static_response(bytes.into(), mime_for(&file_abs), no_cache_file);
                }
            }
        }
        // 磁盘前端完整存在时，缺失静态资源直接 404（禁止跨版本混取旧内嵌文件或回落 index.html）
        if is_asset_request {
            return StatusCode::NOT_FOUND.into_response();
        }
        if let Ok(bytes) = tokio::fs::read(web_root.join("index.html")).await {
            return build_static_response(bytes.into(), "text/html; charset=utf-8", true);
        }
    }

    if let Some((bytes, mime)) = crate::web_assets::get_exact(rel) {
        return build_static_response(bytes.into(), mime, no_cache_file);
    }
    if is_asset_request {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some((bytes, mime)) = crate::web_assets::index_html() {
        return build_static_response(bytes.into(), mime, true);
    }
    StatusCode::NOT_FOUND.into_response()
}

/// 判定是否为静态文件请求（缺失必须返回 404，不可回退 SPA `index.html`）
fn is_static_asset_path(rel: &str) -> bool {
    if rel.is_empty() {
        return false;
    }
    if rel.starts_with("static/") || rel.starts_with("fonts/") || rel.starts_with("assets/") {
        return true;
    }
    let last_segment = rel.rsplit('/').next().unwrap_or("");
    last_segment.contains('.') && !last_segment.ends_with(".html")
}

fn build_static_response(body: Body, mime: &str, no_cache: bool) -> Response {
    let mut b = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime);
    b = if no_cache {
        b.header(header::CACHE_CONTROL, "no-cache")
            .header("Surrogate-Control", "no-store")
    } else {
        b.header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
    };
    b.body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn mime_for(file: &std::path::Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()) {
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

async fn data_file_access_allowed(
    state: &AppState,
    headers: &HeaderMap,
    rel: &std::path::Path,
) -> bool {
    if !state.storage.config.secure {
        return true;
    }
    let Some(ns) = rel.iter().next().and_then(|s| s.to_str()) else {
        return false;
    };
    if ns == "default" {
        return false;
    }
    let mut params = HashMap::new();
    if let Some(token) = cookie_value(headers, "reader_token") {
        params.insert("accessToken".to_string(), token);
    }
    match resolve_current_user(state, &params, headers).await {
        Ok(user) => user.is_admin || user.username == ns,
        Err(_) => false,
    }
}

async fn serve_data_file(
    state: &AppState,
    headers: &HeaderMap,
    prefix: &str,
    uri_path: &str,
) -> Response {
    let Some(rel) = uri_path.strip_prefix(prefix).and_then(safe_data_rel_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !data_file_access_allowed(state, headers, &rel).await {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let root = state.storage.config.storage_dir().join("data");
    let file = root.join(&rel);
    let (Ok(root_abs), Ok(file_abs)) = (root.canonicalize(), file.canonicalize()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !file_abs.starts_with(&root_abs) || !file_abs.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let bytes = match tokio::fs::read(&file_abs).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let ext = file_abs
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "html" | "htm" | "xhtml") {
        let html = String::from_utf8_lossy(&bytes);
        let injected = inject_api_root_script(&html, &request_base_url(headers));
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(injected))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_for(&file_abs))
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn safe_data_rel_path(tail: &str) -> Option<std::path::PathBuf> {
    let mut rel = std::path::PathBuf::new();
    for seg in tail.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        let decoded = urlencoding::decode(seg).ok()?;
        let d = decoded.as_ref();
        if d == ".."
            || d.contains('/')
            || d.contains('\\')
            || d.contains(':')
            || d.contains('\0')
            || d.bytes().any(|b| b < 0x20)
        {
            return None;
        }
        rel.push(d);
    }
    if rel.as_os_str().is_empty() {
        None
    } else {
        Some(rel)
    }
}

fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| {
        h[i..i + n.len()]
            .iter()
            .zip(n)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

fn inject_api_root_script(html: &str, base_url: &str) -> String {
    let escaped = base_url.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("<script>window.__API_ROOT__=\"{escaped}\"</script>");
    match find_ascii_ci(html, "</body>") {
        Some(pos) => format!("{}{}{}", &html[..pos], script, &html[pos..]),
        None => format!("{html}{script}"),
    }
}

fn request_base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    if !host_ok {
        return String::new();
    }
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|p| *p == "http" || *p == "https")
        .unwrap_or("http");
    format!("{proto}://{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_proxy_content_type_rejects_crlf_and_accepts_valid() {
        assert_eq!(
            sanitize_proxy_content_type(Some("image/jpeg")),
            "image/jpeg"
        );
        assert_eq!(
            sanitize_proxy_content_type(Some("image/jpeg\r\nX-Evil: 1")),
            "image/png"
        );
        assert_eq!(sanitize_proxy_content_type(None), "image/png");
    }

    #[test]
    fn safe_data_rel_path_blocks_traversal() {
        assert!(safe_data_rel_path("alice/book/ch1.html").is_some());
        assert!(safe_data_rel_path("alice/../bob/secret.txt").is_none());
        assert!(safe_data_rel_path("alice/%2e%2e/bob").is_none());
        assert!(safe_data_rel_path("").is_none());
    }

    #[test]
    fn static_asset_path_distinguishes_chunks_from_spa_routes() {
        assert!(is_static_asset_path("static/ShelfPage-abc123.js"));
        assert!(is_static_asset_path("static/index-xyz.css"));
        assert!(is_static_asset_path("sw.js"));
        assert!(is_static_asset_path("favicon.svg"));
        assert!(!is_static_asset_path(""));
        assert!(!is_static_asset_path("reader"));
        assert!(!is_static_asset_path("sources"));
        assert!(!is_static_asset_path("index.html"));
    }
}
