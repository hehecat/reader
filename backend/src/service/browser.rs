//! 浏览器后端统一门面（**camoufox-only**，受可选 Cargo feature `camoufox` 控制）。
//!
//! Rust 内不内嵌浏览器引擎：验证码求解 / 登录表单 / 滑块 / 图片验证码全部委托
//! camoufox HTTP 服务（`scripts/camoufox_solver.py`，见 [`crate::service::camoufox`]）。
//! 保留本模块对外 API（`is_browser_available` / `solve_cf_challenge` / `solve_captcha`
//! / `extract_turnstile_sitekey` / `unsupported_captcha_kind`），crawler 与书源 JS 桥
//! 无需感知后端实现。服务未运行时首次调用自动拉起（`camoufox::ensure_service`）；
//! 当构建关闭 `camoufox` 特性（`--no-default-features`）时，`is_browser_available()`
//! 返回 `false`，求解/登录入口返回明确的未启用错误，纯 Rust HTML 预检函数仍照常工作。

use anyhow::{anyhow, Result};

pub use crate::service::camoufox::{CfSolution, LoginCaptcha, LoginSession};

/// 浏览器是否可用：camoufox 启用即可（服务未启动时首次求解会自动拉起）
pub fn is_browser_available() -> bool {
    crate::service::camoufox::enabled()
}

/// CF 质询/Turnstile 统一求解（书源 JS 桥 `java.startBrowserAwait` 与 crawler 兜底共用）。
/// proxy：书源级代理（None = 环境 READER_CAMOUFOX_PROXY）。
pub async fn solve_cf_challenge(
    _ns: &str,
    url: &str,
    cookies: &[(String, String)],
    max_wait_ms: u64,
    proxy: Option<&str>,
) -> Result<CfSolution> {
    crate::service::camoufox::ensure_service().await?;
    crate::service::camoufox::solve(url, cookies, max_wait_ms, proxy).await
}

/// 统一验证码求解入口（含登录页滑块/图片验证码分支——camoufox 服务端处理）：
/// 与 solve_cf_challenge 等价（camoufox /solve 内部已覆盖 CF 经典/Turnstile/滑块）。
pub async fn solve_captcha(
    ns: &str,
    url: &str,
    cookies: &[(String, String)],
    max_wait_ms: u64,
    proxy: Option<&str>,
) -> Result<CfSolution> {
    solve_cf_challenge(ns, url, cookies, max_wait_ms, proxy).await
}

/// 登录表单会话（mode=browser）：第一步填表+提交+自动滑块/质询；图片验证码 → need_captcha。
/// 返回 Ok(LoginSession)（status 字段区分 ok/need_captcha/timeout/error）。
pub async fn login_start(
    url: &str,
    username: &str,
    password: &str,
    cookies: &[(String, String)],
    proxy: Option<&str>,
    max_wait_ms: u64,
) -> Result<LoginSession> {
    crate::service::camoufox::ensure_service().await?;
    crate::service::camoufox::login_start(url, username, password, cookies, proxy, max_wait_ms)
        .await
}

/// 登录第二步：图片验证码回填（session_id 来自 login_start 的 need_captcha）。
pub async fn login_captcha(
    session_id: &str,
    captcha: &str,
    max_wait_ms: u64,
) -> Result<LoginSession> {
    crate::service::camoufox::ensure_service().await?;
    crate::service::camoufox::login_captcha(session_id, captcha, max_wait_ms).await
}

/// 关闭登录会话
pub async fn login_close(session_id: &str) -> Result<()> {
    crate::service::camoufox::login_close(session_id).await
}

/// 验证码探测（getCaptcha 用）：导航登录页 → 检测验证码类型 + 图片截图
pub async fn probe_captcha(
    url: &str,
    cookies: &[(String, String)],
    proxy: Option<&str>,
) -> Result<serde_json::Value> {
    crate::service::camoufox::ensure_service().await?;
    crate::service::camoufox::probe(url, cookies, proxy).await
}

/// 从 HTML 提取 Turnstile sitekey（纯 Rust 解析——预检/日志用）。
/// 依次尝试 ① data-sitekey 属性；② iframe src 的 sitekey query 参数；
/// ③ turnstile/api.js 脚本 URL 的 sitekey query 参数。未命中 → None。
pub fn extract_turnstile_sitekey(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    // ① data-sitekey 属性（.cf-turnstile 容器或任意元素）
    for attr in ["data-sitekey=", "data-sitekey = "] {
        let mut from = 0usize;
        while let Some(idx) = lower[from..].find(attr) {
            let start = from + idx + attr.len();
            let rest = &html[start..];
            if let Some(v) = rest
                .strip_prefix('"')
                .and_then(|r| r.split('"').next())
                .or_else(|| rest.strip_prefix('\'').and_then(|r| r.split('\'').next()))
            {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
            from = start + 1;
        }
    }
    // ② iframe src / ③ 脚本 src 的 sitekey query 参数（URL 编码值原样返回）
    for (open, close) in [("<iframe", ">"), ("<script", ">")] {
        let mut from = 0usize;
        while let Some(idx) = lower[from..].find(open) {
            let tag_start = from + idx;
            let Some(tag_end) = lower[tag_start..].find(close) else {
                break;
            };
            let tag = &html[tag_start..tag_start + tag_end];
            let tag_lower = tag.to_lowercase();
            let is_turnstile = tag_lower.contains("challenges.cloudflare.com")
                || tag_lower.contains("turnstile/api.js");
            if is_turnstile {
                if let Some(sk) = url_query_param(tag, "sitekey") {
                    return Some(sk);
                }
            }
            from = tag_start + tag_end + 1;
        }
    }
    None
}

/// 不支持的验证码类型检测（HTML 特征字符串；与 python 服务端 /solve 镜像——
/// 供单测断言/预检）：reCAPTCHA（g-recaptcha/recaptcha/api.js）→ Some("reCAPTCHA")；
/// hCaptcha（h-captcha/hcaptcha.com）→ Some("hCaptcha")；未命中 → None
pub fn unsupported_captcha_kind(body: &str) -> Option<&'static str> {
    let b = body.to_lowercase();
    if b.contains("g-recaptcha") || b.contains("recaptcha/api.js") {
        return Some("reCAPTCHA");
    }
    if b.contains("h-captcha") || b.contains("hcaptcha.com") || b.contains("/hcaptcha") {
        return Some("hCaptcha");
    }
    None
}

/// 从 URL/标签文本提取 query 参数值（简单解析——无 URL 解析依赖；未命中 → None）
fn url_query_param(text: &str, key: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let mut from = 0usize;
    while let Some(idx) = lower[from..].find(&format!("{key}=")) {
        let start = from + idx + key.len() + 1;
        let rest = &text[start..];
        let v: String = rest
            .chars()
            .take_while(|c| *c != '&' && *c != '\"' && *c != '\'' && !c.is_whitespace())
            .collect();
        if !v.is_empty() {
            return Some(v);
        }
        from = start + 1;
    }
    None
}

/// evaluate_in_session 已随 obscura 会话移除：camoufox 下页内交互走 /solve 的
/// post 参数（fetch/navigate 表单链路）或 /login 会话——保留占位以兼容潜在调用方。
pub async fn evaluate_in_session(_ns: &str, _expression: &str) -> Result<serde_json::Value> {
    Err(anyhow!(
        "evaluate_in_session 已移除（camoufox-only）——页内交互请走 /solve post 参数或 /login 会话"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_turnstile_sitekey() {
        // ① data-sitekey 属性
        assert_eq!(
            extract_turnstile_sitekey(
                r#"<div class="cf-turnstile" data-sitekey="0x4AAAAAAA-mockkey"></div>"#
            )
            .as_deref(),
            Some("0x4AAAAAAA-mockkey")
        );
        // 属性名大小写不敏感 + 单引号
        assert_eq!(
            extract_turnstile_sitekey(r#"<DIV DATA-SITEKEY='0x4BBBBB'>"#).as_deref(),
            Some("0x4BBBBB")
        );
        // ② iframe src query
        assert_eq!(
            extract_turnstile_sitekey(
                r#"<iframe src="https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/orchestrate/turnstile/v1?x=1&sitekey=0x4CCCCC&y=2">"#
            )
            .as_deref(),
            Some("0x4CCCCC")
        );
        // ③ turnstile/api.js 脚本 URL query
        assert_eq!(
            extract_turnstile_sitekey(
                r#"<script src="https://challenges.cloudflare.com/turnstile/v0/api.js?sitekey=0x4DDDDD" async defer></script>"#
            )
            .as_deref(),
            Some("0x4DDDDD")
        );
        // 非 turnstile iframe 的 sitekey 不命中
        assert_eq!(
            extract_turnstile_sitekey(r#"<iframe src="https://other.com/x?sitekey=0x9999">"#),
            None
        );
        assert_eq!(extract_turnstile_sitekey("<html>hello</html>"), None);
        assert_eq!(extract_turnstile_sitekey(""), None);
    }

    #[test]
    fn test_unsupported_captcha_kind() {
        assert_eq!(
            unsupported_captcha_kind("<div class=\"g-recaptcha\" data-sitekey=\"x\"></div>"),
            Some("reCAPTCHA")
        );
        assert_eq!(
            unsupported_captcha_kind(
                "<script src=\"https://www.google.com/recaptcha/api.js\"></script>"
            ),
            Some("reCAPTCHA")
        );
        assert_eq!(
            unsupported_captcha_kind("<div class=\"h-captcha\" data-sitekey=\"x\"></div>"),
            Some("hCaptcha")
        );
        assert_eq!(
            unsupported_captcha_kind("<DIV CLASS=\"G-RECAPTCHA\">"),
            Some("reCAPTCHA")
        );
        assert_eq!(
            unsupported_captcha_kind("<div class=\"cf-turnstile\"></div>"),
            None
        );
    }
}
