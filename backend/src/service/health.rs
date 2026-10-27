//! 书源健康检测（getInvalidBookSources）：运行期失败快照 + 并发 HEAD/首页检测（禁用接口用）

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use crate::model::BookSource;
use crate::service::crawler;

/// 失效快照 TTL：legacy addInvalidBookSource 缓存 600 秒
const INVALID_SNAPSHOT_TTL_MS: i64 = 600 * 1000;

/// 运行期失效书源快照（legacy invalidBookSourceCache 对齐）：
/// key = namespace:source_url，value = (记录时间戳 ms, 错误信息)
static INVALID_SOURCE_SNAPSHOT: LazyLock<Mutex<HashMap<String, (i64, String)>>> =
    LazyLock::new(Default::default);

fn snapshot_key(ns: &str, source_url: &str) -> String {
    format!("{ns}:{source_url}")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// 标记书源运行期失败（搜索/详情/目录/正文实际抓取报错时调用）；
/// 600 秒内 [`invalid_snapshot`] 直接返回该记录，不重新探测。
pub fn mark_source_invalid(ns: &str, source_url: &str, error_msg: &str) {
    let mut map = INVALID_SOURCE_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.insert(
        snapshot_key(ns, source_url),
        (now_ms(), error_msg.to_string()),
    );
}

/// 成功抓取后清除对应源的失败标记
pub fn clear_source_invalid(ns: &str, source_url: &str) {
    let mut map = INVALID_SOURCE_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.remove(&snapshot_key(ns, source_url));
}

/// 判断书源是否处于 600 秒失效期内（搜索等请求前置短路用：
/// legacy searchBookWithSource 发请求前先查 invalidBookSourceCache）
pub fn is_source_invalid(ns: &str, source_url: &str) -> bool {
    let now = now_ms();
    let map = INVALID_SOURCE_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.get(&snapshot_key(ns, source_url))
        .is_some_and(|(ts, _)| now - *ts < INVALID_SNAPSHOT_TTL_MS)
}

/// 读取命名空间下 600 秒内的失败记录（过期条目顺带清理）→ [(sourceUrl, time, errorMsg)]
pub fn invalid_snapshot(ns: &str) -> Vec<(String, i64, String)> {
    let now = now_ms();
    let prefix = format!("{ns}:");
    let mut map = INVALID_SOURCE_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, (ts, _)| now - *ts < INVALID_SNAPSHOT_TTL_MS);
    let mut out: Vec<(String, i64, String)> = map
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(k, (ts, err))| (k[prefix.len()..].to_string(), *ts, err.clone()))
        .collect();
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out
}

/// 单书源健康检测：HEAD 优先（405/501 或失败回退 GET 首页）
/// 返回 (是否可用, 说明)
pub async fn check_source(_ns: &str, source: &BookSource) -> (bool, String) {
    let base = source
        .book_source_url
        .split("##")
        .next()
        .unwrap_or("")
        .trim();
    if base.is_empty() {
        return (false, "书源地址为空".to_string());
    }
    // P1 SSRF：书源地址公网校验（DNS 解析后——拒绝私网/回环/169.254 等）
    if let Err(e) = crate::service::crawler::validate_public_target(base).await {
        return (false, format!("地址校验失败: {e}"));
    }
    let headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36")
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("客户端构建失败: {e}")),
    };
    let req_headers = to_reqwest_headers(&headers);

    // HEAD 优先（轻量）
    let head = client.head(base).headers(req_headers.clone()).send().await;
    if let Ok(resp) = head {
        let status = resp.status().as_u16();
        if status == 405 || status == 501 {
            // 服务器不支持 HEAD → GET 兜底
        } else if status < 400 {
            return (true, format!("HEAD {status}"));
        } else {
            // 4xx/5xx：不视为“网络失效”，但按不可用处理（GET 兜底验证首页）
            let get = client.get(base).headers(req_headers.clone()).send().await;
            return match get {
                Ok(r) if r.status().as_u16() < 400 => {
                    (true, format!("GET {}", r.status().as_u16()))
                }
                Ok(r) => (false, format!("HTTP {}", r.status().as_u16())),
                Err(e) => (false, format!("连接失败: {e}")),
            };
        }
    }
    // GET 兜底
    match client.get(base).headers(req_headers).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status < 400 {
                (true, format!("GET {status}"))
            } else {
                (false, format!("HTTP {status}"))
            }
        }
        Err(e) => (false, format!("连接失败: {e}")),
    }
}

/// 并发检测全部书源（并发上限 96——6900+ 书源时 8 并发会拖到小时级并触发前端 15s 超时）；
/// 返回不可用列表 [(书源, 原因)]
pub async fn find_invalid(ns: &str, sources: &[BookSource]) -> Vec<(BookSource, String)> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(96));
    let mut handles = Vec::with_capacity(sources.len());
    for source in sources {
        let sem = semaphore.clone();
        let ns = ns.to_string();
        let source = source.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            check_source(&ns, &source).await
        }));
    }
    let mut invalid = Vec::new();
    for (source, h) in sources.iter().zip(handles) {
        if let Ok((ok, reason)) = h.await {
            if !ok {
                invalid.push((source.clone(), reason));
            }
        }
    }
    invalid
}

/// HashMap<String,String> → http::HeaderMap（非法头名/值跳过）
fn to_reqwest_headers(headers: &HashMap<String, String>) -> http::HeaderMap {
    let mut hm = http::HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(k), Ok(v)) = (
            http::header::HeaderName::try_from(k.as_str()),
            http::header::HeaderValue::try_from(v.as_str()),
        ) {
            hm.insert(k, v);
        }
    }
    hm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 运行期失效快照：标记 → 快照返回；清除 → 消失；命名空间隔离
    #[test]
    fn test_invalid_snapshot_mark_clear() {
        let ns = "snap-test-a";
        mark_source_invalid(ns, "http://a.com", "连接失败");
        let snap = invalid_snapshot(ns);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, "http://a.com");
        assert!(snap[0].1 > 0);
        assert!(snap[0].2.contains("连接失败"));
        // 命名空间隔离
        assert!(invalid_snapshot("snap-test-b").is_empty());
        // 前置短路查询：标记期内为 true，清除后为 false，跨命名空间不受影响
        assert!(is_source_invalid(ns, "http://a.com"));
        assert!(!is_source_invalid("snap-test-b", "http://a.com"));
        // 成功抓取清除
        clear_source_invalid(ns, "http://a.com");
        assert!(invalid_snapshot(ns).is_empty());
        assert!(!is_source_invalid(ns, "http://a.com"));
    }

    /// 过期条目不返回（600 秒 TTL）
    #[test]
    fn test_invalid_snapshot_expires() {
        let ns = "snap-test-expire";
        let key = snapshot_key(ns, "http://old.com");
        INVALID_SOURCE_SNAPSHOT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key,
                (now_ms() - INVALID_SNAPSHOT_TTL_MS - 1000, "过期".into()),
            );
        assert!(invalid_snapshot(ns).is_empty(), "超过 600 秒的记录应被清理");
        assert!(!is_source_invalid(ns, "http://old.com"), "过期记录不短路");
    }

    #[test]
    fn test_to_reqwest_headers_skips_invalid() {
        let mut map = HashMap::new();
        map.insert("User-Agent".to_string(), "UA1".to_string());
        map.insert("bad header name".to_string(), "x".to_string());
        let hm = to_reqwest_headers(&map);
        assert_eq!(hm.len(), 1);
        assert_eq!(hm.get("user-agent").unwrap(), "UA1");
    }

    #[tokio::test]
    async fn test_check_source_empty_url() {
        let src = BookSource::default();
        let (ok, reason) = check_source("default", &src).await;
        assert!(!ok);
        assert!(reason.contains("书源地址为空"));
    }

    #[tokio::test]
    async fn test_check_source_unreachable() {
        // 127.0.0.1:1 连接拒绝 → 不可用（快速失败）
        let src = BookSource {
            book_source_url: "http://127.0.0.1:1".into(),
            book_source_name: "坏源".into(),
            ..Default::default()
        };
        let (ok, reason) = check_source("default", &src).await;
        assert!(!ok, "不可达应判定不可用: {reason}");
    }

    /// P1 SSRF：书源健康检测地址拒绝私网/回环（DNS 解析后校验，错误返回）
    #[tokio::test]
    async fn test_check_source_rejects_private_url() {
        let _g = crate::service::crawler::ssrf_allow_private_guard(false);
        for url in [
            "http://127.0.0.1:8080",
            "http://10.0.0.1",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let src = BookSource {
                book_source_url: url.into(),
                book_source_name: "私网源".into(),
                ..Default::default()
            };
            let (ok, reason) = check_source("default", &src).await;
            assert!(!ok, "私网地址应判定不可用");
            assert!(reason.contains("已拦截"), "应报内网拦截（{url}）: {reason}");
        }
    }
}
