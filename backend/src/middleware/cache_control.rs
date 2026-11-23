//! 静态资源 Cache-Control 中间件（GAP 60）
//!
//! 为前端静态资源与封面资源设置浏览器缓存策略（hash 文件名长缓存、index.html no-cache），
//! API 响应与已带 `Cache-Control` 的响应不做任何修改：
//!
//! | 路径 | Cache-Control |
//! | --- | --- |
//! | `/static/**`、`/fonts/**`（前端构建产物，hash 文件名，内容不可变） | `public, max-age=2592000, immutable`（30 天） |
//! | `/assets/**`（封面等用户资源，legacy 以 md5 文件名存储——同名即同内容） | `public, max-age=2592000`（30 天） |
//! | `/`、`*.html`、无扩展名路径（SPA 路由——实际返回 index.html） | `no-cache` |
//! | 其他带扩展名的前端文件（favicon 等非 hash 文件） | `public, max-age=86400`（1 天） |
//! | `/reader3/**`、`/health`（API） | 不修改 |

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{header, HeaderValue, Request, Response};
use tower::{Layer, Service};

/// hash 文件名静态资源：30 天 + immutable
const CACHE_30D_IMMUTABLE: &str = "public, max-age=2592000, immutable";
/// /assets 用户资源：30 天（不标 immutable——资源可能被覆盖）
const CACHE_30D: &str = "public, max-age=2592000";
/// index.html / SPA 路由：no-cache（每次回源校验，保证发版立即可见）
const NO_CACHE: &str = "no-cache";
/// 其他非 hash 前端文件：1 天
const CACHE_1D: &str = "public, max-age=86400";

/// 依据请求路径决定 Cache-Control 策略（None = 不修改响应）
fn cache_policy(path: &str) -> Option<&'static str> {
    // API 路径一律不动
    if path.starts_with("/reader3") || path == "/health" {
        return None;
    }
    if path.starts_with("/static/") || path.starts_with("/fonts/") {
        return Some(CACHE_30D_IMMUTABLE);
    }
    if path.starts_with("/assets/") {
        return Some(CACHE_30D);
    }
    // SPA：/、index.html、sw.js 与无扩展名路径不长缓存，保证发版与 SW 更新即时生效
    let last_segment = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if path == "/" || path == "/sw.js" || path.ends_with(".html") || !last_segment.contains('.') {
        return Some(NO_CACHE);
    }
    // 其他带扩展名的前端文件（favicon.ico 等）
    Some(CACHE_1D)
}

/// 挂载用 Layer：`router.layer(CacheControlLayer)`
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheControlLayer;

impl<S> Layer<S> for CacheControlLayer {
    type Service = CacheControl<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CacheControl { inner }
    }
}

/// 为响应追加 Cache-Control 的 Service 包装
#[derive(Debug, Clone)]
pub struct CacheControl<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for CacheControl<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let path = req.uri().path().to_string();
        let policy = cache_policy(&path);
        let fut = self.inner.call(req);
        Box::pin(async move {
            let mut resp = fut.await?;
            // P1-9：仅 2xx 成功响应打缓存头——404/5xx 等错误响应不打
            // （否则错误页/缺失资源会被浏览器或中间代理长缓存，掩盖后续修复）
            if resp.status().is_success() {
                if let Some(policy) = policy {
                    // 已有 Cache-Control（如未来 ServeDir 显式配置）不覆盖
                    if !resp.headers().contains_key(header::CACHE_CONTROL) {
                        resp.headers_mut()
                            .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
                    }
                }
            }
            Ok(resp)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_build_assets_get_long_cache() {
        assert_eq!(
            cache_policy("/static/js/app.3f2a9c.js"),
            Some(CACHE_30D_IMMUTABLE)
        );
        assert_eq!(
            cache_policy("/static/css/index.a1b2c3.css"),
            Some(CACHE_30D_IMMUTABLE)
        );
        assert_eq!(
            cache_policy("/fonts/iconfont.woff2"),
            Some(CACHE_30D_IMMUTABLE)
        );
    }

    #[test]
    fn assets_user_resources_get_30d() {
        assert_eq!(cache_policy("/assets/covers/abc123.jpg"), Some(CACHE_30D));
        assert_eq!(cache_policy("/assets/books/def456.epub"), Some(CACHE_30D));
    }

    #[test]
    fn index_html_and_spa_routes_are_no_cache() {
        assert_eq!(cache_policy("/"), Some(NO_CACHE));
        assert_eq!(cache_policy("/index.html"), Some(NO_CACHE));
        assert_eq!(cache_policy("/sw.js"), Some(NO_CACHE));
        assert_eq!(cache_policy("/login"), Some(NO_CACHE));
        assert_eq!(cache_policy("/reader"), Some(NO_CACHE));
    }

    #[test]
    fn api_paths_untouched() {
        assert_eq!(cache_policy("/reader3/getBookshelf"), None);
        assert_eq!(cache_policy("/reader3/file/list"), None);
        assert_eq!(cache_policy("/health"), None);
    }

    #[test]
    fn other_root_files_get_short_cache() {
        assert_eq!(cache_policy("/favicon.ico"), Some(CACHE_1D));
        assert_eq!(cache_policy("/logo.png"), Some(CACHE_1D));
    }

    /// P1-9：响应状态码检查——仅 2xx 打缓存头；404/5xx 错误响应不打
    #[tokio::test]
    async fn error_responses_get_no_cache_control() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::{ServiceBuilder, ServiceExt};

        // 最小后端：按请求路径返回指定状态码
        let echo_status = tower::service_fn(|req: Request<Body>| async move {
            let path = req.uri().path();
            let status = if path.contains("missing") {
                StatusCode::NOT_FOUND
            } else if path.contains("broken") {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::OK
            };
            Ok::<_, std::convert::Infallible>(
                axum::response::Response::builder()
                    .status(status)
                    .body(Body::empty())
                    .unwrap(),
            )
        });

        let mut svc = ServiceBuilder::new()
            .layer(CacheControlLayer)
            .service(echo_status);

        // 200 → 打缓存头（/assets 路径 30 天）
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::builder()
                    .uri("/assets/covers/a.jpg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            CACHE_30D
        );

        // 404（静态资源缺失）→ 不打缓存头
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::builder()
                    .uri("/assets/missing.jpg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(
            !resp.headers().contains_key(header::CACHE_CONTROL),
            "404 响应不应带 Cache-Control"
        );

        // 500 → 不打缓存头
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::builder()
                    .uri("/broken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !resp.headers().contains_key(header::CACHE_CONTROL),
            "5xx 响应不应带 Cache-Control"
        );
    }

    /// P1-9：SPA 路径 404（无匹配前端资源）同样不打 no-cache 头
    #[tokio::test]
    async fn spa_404_gets_no_cache_control() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::{ServiceBuilder, ServiceExt};

        let not_found = tower::service_fn(|_: Request<Body>| async move {
            Ok::<_, std::convert::Infallible>(
                axum::response::Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap(),
            )
        });
        let mut svc = ServiceBuilder::new()
            .layer(CacheControlLayer)
            .service(not_found);
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::builder()
                    .uri("/some/spa/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(
            !resp.headers().contains_key(header::CACHE_CONTROL),
            "SPA 兜底 404 不应带 Cache-Control（路径策略原本是 no-cache）"
        );
    }
}
