//! HTTP 路由装配层：`AppState`、`ReturnData`、公共请求辅助与全站路由表
//!
//! 业务 handler 已按领域拆分至同级子模块：
//! - `assets`: 图片代理、封面、EPUB 资源与 SPA fallback
//! - `auth`: 登录/登出/用户信息/系统信息/用户管理
//! - `books`: 书架、详情、目录、正文、进度、换源、分组、书签、本地书导入
//! - `sources`: 书源管理、统计、失效源、远程订阅导入、书源登录态与调试 SSE
//! - `search`: 多源/换源 SSE 搜索、单源搜索、书海探索
//! - `features`: 净化规则、缓存与整书缓存 SSE、全书搜索、词典、字体、TTS/HttpTTS、WebDAV 备份恢复
//! - `files`: 文件列表/下载/上传/删除
//! - `sse`: 统一标准 `axum::response::Sse` 封装（含 `X-Accel-Buffering: no` 与 15s 心跳）

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::{json, Value};

use crate::api::{assets, auth, books, features, files, search, sources};
use crate::storage::Storage;

/// 统一 API 返回结构（`isSuccess` / `errorMsg` / `data`，camelCase；默认 `T = Value` 保持 100% 向后兼容）
#[derive(Debug, serde::Serialize)]
pub struct ReturnData<T = Value> {
    #[serde(rename = "isSuccess")]
    pub is_success: bool,
    #[serde(rename = "errorMsg")]
    pub error_msg: String,
    pub data: T,
}

impl ReturnData<Value> {
    pub fn ok(data: Value) -> Self {
        Self {
            is_success: true,
            error_msg: String::new(),
            data,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            is_success: false,
            error_msg: msg.into(),
            data: Value::Null,
        }
    }
}

impl<T: serde::Serialize> ReturnData<T> {
    /// 零中间 `serde_json::Value` 树构造强类型 JSON 成功响应
    pub fn ok_typed(data: T) -> Json<Self> {
        Json(Self {
            is_success: true,
            error_msg: String::new(),
            data,
        })
    }
}

/// 路由共享状态
#[derive(Clone)]
pub struct AppState {
    pub storage: Storage,
    /// 图片代理磁盘缓存（`storage/cache/images`，LRU 容量控制 + 并发去重）
    pub image_cache: Arc<crate::service::image_cache::ImageCache>,
}

/// 目录缓存 TTL（默认 24h，环境变量 `READER_TOC_CACHE_TTL_MS` 可覆盖，单位 ms）
pub(crate) fn toc_cache_ttl_ms() -> i64 {
    static TTL: OnceLock<i64> = OnceLock::new();
    *TTL.get_or_init(|| {
        std::env::var("READER_TOC_CACHE_TTL_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(24 * 60 * 60 * 1000)
    })
}

/// 当前 UTC 毫秒时间戳
pub(crate) fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 参数提取：JSON body 优先，query 兜底
pub(crate) fn param_of(
    params: &HashMap<String, String>,
    body: Option<&Value>,
    key: &str,
) -> String {
    if let Some(b) = body {
        if let Some(v) = b.get(key) {
            if let Some(s) = v.as_str() {
                return s.to_string();
            }
            if !v.is_null() {
                return v.to_string();
            }
        }
    }
    params.get(key).cloned().unwrap_or_default()
}

/// multipart 上传字段限流读取（超限或流中断返回可读错误）
pub(crate) async fn read_multipart_field_limited(
    field: &mut axum::extract::multipart::Field<'_>,
    max_bytes: usize,
    max_mb: i64,
) -> Result<Vec<u8>, String> {
    let limit_msg = || {
        format!(
            "文件过大：超过上传大小上限（{} MB，可用环境变量 READER_UPLOAD_MAX_MB 调整）",
            max_mb
        )
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                out.extend_from_slice(&chunk);
                if out.len() > max_bytes {
                    return Err(limit_msg());
                }
            }
            Ok(None) => break,
            Err(e) => {
                return Err(format!(
                    "上传被中断（请求体超限或连接异常），字段接收不完整: {e}（可用环境变量 READER_UPLOAD_MAX_MB 调整）"
                ))
            }
        }
    }
    if out.len() > max_bytes {
        return Err(limit_msg());
    }
    Ok(out)
}

/// 上传 `Content-Length` 预检
pub(crate) fn check_upload_content_length(
    headers: &HeaderMap,
    max_bytes: usize,
    max_mb: i64,
) -> Option<String> {
    let cl = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())?;
    if cl > max_bytes {
        Some(format!(
            "文件过大：超过上传大小上限（{} MB，可用环境变量 READER_UPLOAD_MAX_MB 调整）",
            max_mb
        ))
    } else {
        None
    }
}

/// 通用 500 JSON 兜底
pub fn internal_error(err: anyhow::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "isSuccess": false, "errorMsg": err.to_string(), "data": null })),
    )
        .into_response()
}

/// 构建全站 Axum 路由（响应压缩统一由 `lib.rs` 外层 `CompressionLayer` 处理）
pub fn router(config: crate::AppConfig, storage: Storage) -> axum::Router {
    crate::service::crawler::register_cookie_storage(storage.clone());
    let image_cache = crate::service::image_cache::ImageCache::new(&config);
    let state = AppState {
        storage,
        image_cache,
    };

    let assets_dir = config.storage_dir().join("assets");
    let assets_service = tower_http::services::ServeDir::new(assets_dir);
    let upload_limit = config.upload_max_bytes();

    axum::Router::new()
        // ---- 健康检查与静态/媒体资源 ----
        .route("/health", get(health))
        .nest_service("/assets", assets_service)
        .route("/assets/proxy", get(assets::assets_proxy))
        .route("/epub/*rest", get(assets::epub_asset))
        .route("/reader3/cover", get(assets::book_cover_legacy))
        // ---- 鉴权、用户信息、系统信息、管理员用户管理 ----
        .route("/reader3/login", post(auth::login))
        .route("/reader3/logout", post(auth::logout))
        .route("/reader3/getUserInfo", get(auth::get_user_info))
        .route("/reader3/getSystemInfo", get(auth::get_system_info))
        .route(
            "/reader3/getUsers",
            get(auth::get_users).post(auth::get_users),
        )
        .route("/reader3/addUser", post(auth::add_user))
        .route("/reader3/updateUser", post(auth::update_user))
        .route("/reader3/deleteUser", post(auth::delete_user))
        .route(
            "/reader3/resetUserPassword",
            post(auth::reset_user_password),
        )
        .route(
            "/reader3/clearInactiveUsers",
            post(auth::clear_inactive_users),
        )
        // ---- 书架、书籍阅读、分组、书签、本地书上传预览 ----
        .route("/reader3/getBookshelf", get(books::get_bookshelf))
        .route("/reader3/saveBook", post(books::save_book))
        .route("/reader3/deleteBook", post(books::delete_book))
        .route("/reader3/deleteBooks", post(books::delete_books))
        .route("/reader3/saveBookProgress", post(books::save_book_progress))
        .route(
            "/reader3/getBookInfo",
            get(books::get_book_info).post(books::get_book_info),
        )
        .route(
            "/reader3/getChapterList",
            get(books::get_chapter_list).post(books::get_chapter_list),
        )
        .route(
            "/reader3/getBookContent",
            get(books::get_book_content).post(books::get_book_content),
        )
        .route(
            "/reader3/setBookSource",
            get(books::set_book_source).post(books::set_book_source),
        )
        .route(
            "/reader3/getChapterListByRule",
            get(books::get_chapter_list_by_rule).post(books::get_chapter_list_by_rule),
        )
        .route(
            "/reader3/getBookGroups",
            get(books::get_book_groups).post(books::get_book_groups),
        )
        .route("/reader3/saveBookGroup", post(books::save_book_group))
        .route("/reader3/deleteBookGroup", post(books::delete_book_group))
        .route("/reader3/saveBookGroupId", post(books::save_book_group_id))
        .route(
            "/reader3/getBookmarks",
            get(books::get_bookmarks).post(books::get_bookmarks),
        )
        .route("/reader3/saveBookmark", post(books::save_bookmark))
        .route("/reader3/deleteBookmark", post(books::delete_bookmark))
        .route("/reader3/deleteBookmarks", post(books::delete_bookmarks))
        .route(
            "/reader3/uploadLocalBook",
            post(books::upload_local_book).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/reader3/importBookPreview",
            post(books::import_book_preview).layer(DefaultBodyLimit::max(upload_limit)),
        )
        // ---- 书源管理与调试 SSE ----
        .route(
            "/reader3/getBookSources",
            get(sources::get_book_sources).post(sources::get_book_sources),
        )
        .route(
            "/reader3/getBookSource",
            get(sources::get_book_source).post(sources::get_book_source),
        )
        .route(
            "/reader3/saveBookSource",
            post(sources::save_book_source).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/reader3/saveBookSources",
            post(sources::save_book_sources).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/reader3/deleteBookSource",
            post(sources::delete_book_source),
        )
        .route(
            "/reader3/deleteBookSources",
            post(sources::delete_book_sources),
        )
        .route(
            "/reader3/setAsDefaultBookSources",
            post(sources::set_as_default_book_sources),
        )
        .route(
            "/reader3/getSourceStats",
            get(sources::get_source_stats_handler).post(sources::get_source_stats_handler),
        )
        .route(
            "/reader3/getInvalidBookSources",
            get(sources::get_invalid_book_sources).post(sources::get_invalid_book_sources),
        )
        .route(
            "/reader3/getAvailableBookSource",
            get(sources::get_available_book_source).post(sources::get_available_book_source),
        )
        .route(
            "/reader3/saveFromRemoteSource",
            post(sources::save_from_remote_source),
        )
        .route(
            "/reader3/loginBookSource",
            get(sources::login_book_source).post(sources::login_book_source),
        )
        .route(
            "/reader3/getBookSourceCookie",
            get(sources::get_book_source_cookie).post(sources::get_book_source_cookie),
        )
        .route(
            "/reader3/setBookSourceCookie",
            post(sources::set_book_source_cookie),
        )
        .route(
            "/reader3/bookSourceDebugSSE",
            get(sources::book_source_debug_sse)
                .post(sources::book_source_debug_sse)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        // ---- 搜索与书海探索 ----
        .route(
            "/reader3/searchBookMultiSSE",
            get(search::search_book_multi_sse).post(search::search_book_multi_sse),
        )
        .route(
            "/reader3/searchBookSourceSSE",
            get(search::search_book_source_sse).post(search::search_book_source_sse),
        )
        .route(
            "/reader3/searchBook",
            get(search::search_book).post(search::search_book),
        )
        .route(
            "/reader3/exploreBook",
            get(search::explore_book).post(search::explore_book),
        )
        // ---- 净化替换规则 ----
        .route(
            "/reader3/getReplaceRules",
            get(features::get_replace_rules).post(features::get_replace_rules),
        )
        .route(
            "/reader3/saveReplaceRule",
            post(features::save_replace_rule),
        )
        .route(
            "/reader3/saveReplaceRules",
            post(features::save_replace_rules).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/reader3/deleteReplaceRule",
            post(features::delete_replace_rule),
        )
        .route(
            "/reader3/deleteReplaceRules",
            post(features::delete_replace_rules),
        )
        // ---- 缓存管理、整书缓存 SSE 与全书正文搜索 ----
        .route(
            "/reader3/getCacheInfo",
            get(features::get_cache_info).post(features::get_cache_info),
        )
        .route("/reader3/clearCache", post(features::clear_cache))
        .route(
            "/reader3/deleteBookCache",
            get(features::delete_book_cache).post(features::delete_book_cache),
        )
        .route(
            "/reader3/getShelfBookWithCacheInfo",
            get(features::get_shelf_book_with_cache_info)
                .post(features::get_shelf_book_with_cache_info),
        )
        .route(
            "/reader3/cacheBookSSE",
            get(features::cache_book_sse).post(features::cache_book_sse),
        )
        .route(
            "/reader3/cancelCacheBook",
            get(features::cancel_cache_book).post(features::cancel_cache_book),
        )
        .route(
            "/reader3/searchBookContent",
            get(features::search_book_content).post(features::search_book_content),
        )
        // ---- 离线词典与自定义字体 ----
        .route(
            "/reader3/dictLookup",
            get(features::dict_lookup).post(features::dict_lookup),
        )
        .route(
            "/reader3/uploadFont",
            post(features::upload_font).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/reader3/getFontList", get(features::get_font_list))
        .route("/reader3/getFont", get(features::get_font))
        .route("/reader3/deleteFont", post(features::delete_font))
        // ---- TTS 与 HttpTTS ----
        .route(
            "/reader3/tts",
            get(features::tts_synthesize).post(features::tts_synthesize),
        )
        .route(
            "/reader3/getHttpTTSList",
            get(features::get_http_tts_list).post(features::get_http_tts_list),
        )
        .route("/reader3/saveHttpTTS", post(features::save_http_tts))
        .route("/reader3/deleteHttpTTS", post(features::delete_http_tts))
        // ---- WebDAV 备份恢复与文件管理 ----
        .route("/reader3/backupToWebdav", post(features::backup_to_webdav))
        .route(
            "/reader3/restoreFromWebdav",
            post(features::restore_from_webdav),
        )
        .route("/reader3/file/list", get(files::list))
        .route("/reader3/file/download", get(files::download))
        .route(
            "/reader3/file/upload",
            post(files::upload).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/reader3/file/delete", post(files::delete))
        .route("/reader3/file/deleteMulti", post(files::delete_multi))
        // ---- SPA Fallback ----
        .fallback(assets::fallback_handler)
        // 全局请求级 Storage 依赖注入（为每条 HTTP/SSE 调用链绑定 task-local Storage）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            inject_task_storage,
        ))
        .with_state(state)
}

/// 请求级 `Storage` 依赖注入中间件：用 `scope_task_storage` 包裹每条 HTTP/SSE 请求
async fn inject_task_storage(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    crate::storage::scope_task_storage(state.storage.clone(), next.run(req)).await
}

async fn health() -> &'static str {
    "ok!"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    async fn test_app(tag: &str) -> axum::Router {
        let dir =
            std::env::temp_dir().join(format!("reader-router-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = crate::AppConfig::from_env();
        cfg.work_dir = dir.to_string_lossy().into_owned();
        cfg.secure = false;
        cfg.mode = "single".to_string();
        let storage = crate::storage::init(&cfg).await.unwrap();
        router(cfg, storage)
    }

    #[tokio::test]
    async fn health_and_system_info_smoke() {
        let app = test_app("smoke").await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&bytes[..], b"ok!");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/reader3/getSystemInfo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["isSuccess"], true);
        assert!(v["data"]["version"].is_string());
        assert!(v["data"]["uptimeSeconds"].is_number());
        assert!(v["data"].get("memory").is_none());

        // 已删除的历史路由应落入 /reader3/* JSON 404
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/reader3/getRssSources")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn inject_task_storage_and_typed_return_data_work() {
        #[derive(serde::Serialize)]
        struct Sample {
            id: i64,
            name: &'static str,
        }

        let dir =
            std::env::temp_dir().join(format!("reader-router-scope-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = crate::AppConfig::from_env();
        cfg.work_dir = dir.to_string_lossy().into_owned();
        let storage = crate::storage::init(&cfg).await.unwrap();
        let state = AppState {
            storage: storage.clone(),
            image_cache: crate::service::image_cache::ImageCache::new(&cfg),
        };
        let app = axum::Router::new()
            .route(
                "/probe",
                get(|| async {
                    let has_storage = crate::storage::current_task_storage().is_some();
                    ReturnData::ok_typed(Sample {
                        id: i64::from(has_storage),
                        name: "typed",
                    })
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                inject_task_storage,
            ))
            .with_state(state);

        assert!(crate::storage::current_task_storage().is_none());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["isSuccess"], true);
        assert_eq!(v["errorMsg"], "");
        assert_eq!(v["data"]["id"], 1);
        assert_eq!(v["data"]["name"], "typed");

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
