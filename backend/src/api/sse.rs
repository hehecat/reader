//! 统一 SSE 流封装（基于标准 `axum::response::Sse`）
//!
//! 为全部 4 条存活 SSE 流提供一致的协议头与心跳保证：
//! - `searchBookMultiSSE`（多源流式搜索）
//! - `searchBookSourceSSE`（换源流式搜索）
//! - `cacheBookSSE`（整书后台缓存进度流）
//! - `bookSourceDebugSSE`（书源调试流）
//!
//! 统一特性：
//! - 响应头 `Content-Type: text/event-stream`、`Cache-Control: no-cache`、`X-Accel-Buffering: no`
//! - 15 秒 Keep-Alive 心跳防反代断连
//! - 零 `Response::builder().unwrap()`

use std::convert::Infallible;
use std::time::Duration;

use axum::http::{header, HeaderName, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::Stream;
use serde_json::Value;

use crate::api::router::ReturnData;

/// SSE 心跳间隔（秒）：防止 Nginx / Cloudflare 等反代在慢源抓取间隙切断长连接
pub const SSE_KEEP_ALIVE_SECS: u64 = 15;

/// 构造无名默认 SSE 数据事件（`data: <json>\n\n`，对齐前端 `EventSource.onmessage`）
pub fn data_event(payload: &Value) -> Event {
    Event::default().data(payload.to_string())
}

/// 构造具名 SSE 事件（如 `event: end` / `event: error`）
pub fn named_event(name: &str, payload: &Value) -> Event {
    Event::default().event(name).data(payload.to_string())
}

/// 将事件流包装为标准 `axum::response::Sse` 响应，统一注入：
/// - `Cache-Control: no-cache`
/// - `X-Accel-Buffering: no`（禁止 Nginx 等反代缓冲事件流）
/// - 15s Keep-Alive 心跳
pub fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut resp = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(SSE_KEEP_ALIVE_SECS))
                .text("keep-alive"),
        )
        .into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp
}

/// 有界 `mpsc::Receiver<Event>` → SSE 响应（客户端断开时 drop rx，发送端 `tx.is_closed()` 立即感知）
pub fn sse_from_receiver(rx: tokio::sync::mpsc::Receiver<Event>) -> Response {
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok::<Event, Infallible>(ev), rx))
    });
    sse_response(stream)
}

/// 无界 `mpsc::UnboundedReceiver<Event>` → SSE 响应（适用于同步回调推送，如 `run_debug`）
pub fn sse_from_unbounded(rx: tokio::sync::mpsc::UnboundedReceiver<Event>) -> Response {
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok::<Event, Infallible>(ev), rx))
    });
    sse_response(stream)
}

/// SSE 前置校验失败单事件响应（`event: error` + `data: ReturnData`）
pub fn sse_error(ret: ReturnData) -> Response {
    let payload = serde_json::to_string(&ret).unwrap_or_default();
    let stream = futures::stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("error").data(payload))
    });
    sse_response(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use serde_json::json;

    #[tokio::test]
    async fn sse_error_emits_required_headers_and_event() {
        let resp = sse_error(ReturnData::err("参数错误"));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-cache")
        );
        assert_eq!(
            resp.headers()
                .get("x-accel-buffering")
                .and_then(|v| v.to_str().ok()),
            Some("no")
        );
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/event-stream"), "ct={ct}");
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("event: error"), "body={text}");
        assert!(text.contains("参数错误"), "body={text}");
    }

    #[tokio::test]
    async fn sse_from_receiver_streams_data_and_named_end() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(data_event(&json!({"lastIndex": 0, "data": []})))
                .await;
            let _ = tx
                .send(named_event("end", &json!({"lastIndex": 0, "isEnd": true})))
                .await;
        });
        let resp = sse_from_receiver(rx);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("x-accel-buffering")
                .and_then(|v| v.to_str().ok()),
            Some("no")
        );
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("\"lastIndex\":0"), "body={text}");
        assert!(text.contains("event: end"), "body={text}");
    }
}
