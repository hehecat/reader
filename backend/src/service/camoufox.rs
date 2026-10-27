//! camoufox 求解/登录后端（**唯一浏览器后端**）：HTTP 调用 `scripts/camoufox_solver.py`
//! （独立 Python 服务，默认端口 8196）——camoufox（Playwright 封装，Firefox 内核 +
//! 真实指纹预设）求解 Cloudflare 质询/Turnstile、登录表单填写 + 滑块拖拽 + 图片验证码
//! 两步流。Rust 内不内嵌浏览器引擎。
//!
//! 环境变量：
//! - `READER_CAMOUFOX_URL`：服务地址（默认 http://127.0.0.1:8196；显式配置 = 连接既有
//!   服务，不自 spawn）
//! - `READER_CAMOUFOX_DISABLE=1`：禁用 camoufox 后端
//! - `READER_CAMOUFOX_UA`：求解用 UA（默认 Chrome/131 Windows——与 CDP 路径一致；
//!   69shuba 等站点有 UA 门禁，Firefox UA 会被 "请使用新版本的Google Chrome" 拦截）
//! - `READER_CAMOUFOX_SCRIPT`：自 spawn 脚本路径（未配置 URL 时自动拉起）
//! - `READER_CAMOUFOX_SPAWN=0`：禁用自 spawn（须显式配置 READER_CAMOUFOX_URL）
//! - `READER_CAMOUFOX_PROXY`：默认代理（socks5://host:port 等住宅代理——机房 IP 解
//!   Turnstile 必需；书源级 proxyUrl 优先覆盖）
//!
//! 求解成功后的 cookie 合并/存库/UA 记录复用 crawler::store_solution_session。

use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

/// 默认求解 UA：Chrome/131 Windows——与 69shuba 实测通过值一致
/// （camoufox 默认 Firefox wire UA 会命中站点 UA 门禁，Chrome wire UA 直过）。
const DEFAULT_SOLVE_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 求解用 UA：`READER_CAMOUFOX_UA` 显式配置优先，默认 Chrome/131 Windows
pub fn solve_ua() -> String {
    std::env::var("READER_CAMOUFOX_UA")
        .map(|v| v.trim().to_string())
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_SOLVE_UA.to_string())
}

/// camoufox 服务地址：`READER_CAMOUFOX_URL`（默认 http://127.0.0.1:8196，尾斜杠去除）
pub fn server_url() -> String {
    std::env::var("READER_CAMOUFOX_URL")
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8196".to_string())
}

/// 服务端口（server_url 解析；自 spawn 用）
fn server_port() -> u16 {
    server_url()
        .rsplit(':')
        .next()
        .and_then(|s| s.trim_end_matches('/').parse().ok())
        .unwrap_or(8196)
}

/// 是否启用 camoufox（`READER_CAMOUFOX_DISABLE=1` 关闭；默认启用——唯一浏览器后端）
pub fn enabled() -> bool {
    std::env::var("READER_CAMOUFOX_DISABLE")
        .map(|v| v.trim() != "1")
        .unwrap_or(true)
}

/// 默认代理：`READER_CAMOUFOX_PROXY`（socks5://host:port 等；书源级 proxyUrl 优先覆盖）
pub fn default_proxy() -> Option<String> {
    std::env::var("READER_CAMOUFOX_PROXY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 求解结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CfSolution {
    /// 求解完成后目标页最终 HTML（document.documentElement.outerHTML）
    pub html: String,
    /// 求解后浏览器内该站点全部 cookie（name, value——含 cf_clearance；按 name 排序去重）
    pub cookies: Vec<(String, String)>,
    /// 浏览器真实 UA（与 cf_clearance 绑定：后续抓取需带同一 UA）
    pub user_agent: String,
    /// Turnstile 求解得到的 cf-turnstile-response token（非 Turnstile 质询为 None）
    pub turnstile_token: Option<String>,
    /// Turnstile sitekey（检测时提取——日志/调试用；非 Turnstile 质询为 None）
    pub turnstile_sitekey: Option<String>,
}

/// 图片验证码（截图 + 坐标——前端显示后回填）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginCaptcha {
    pub base64: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 登录会话结果（/login、/login/captcha 统一返回）
#[derive(Debug, Clone)]
pub struct LoginSession {
    /// ok | need_captcha | timeout | error
    pub status: String,
    /// 会话 id（need_captcha 后两步回填 /login/captcha 用）
    pub session_id: Option<String>,
    pub html: String,
    pub cookies: Vec<(String, String)>,
    pub user_agent: String,
    pub turnstile_token: Option<String>,
    /// 最终页 URL（loginCheckJs 的 url 变量）
    pub url: String,
    pub captcha: Option<LoginCaptcha>,
    pub error: Option<String>,
    pub message: Option<String>,
}

impl LoginSession {
    /// cookie 数组 → "a=1; b=2"
    pub fn cookies_to_string(&self) -> String {
        let mut pairs: Vec<(String, String)> = self.cookies.clone();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

// ==================== HTTP 客户端 ====================

fn client(timeout_secs: u64) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| anyhow!("camoufox HTTP 客户端构造失败: {e}"))
}

async fn post_json(path: &str, payload: Value, timeout_secs: u64) -> Result<Value> {
    let base = server_url();
    let c = client(timeout_secs)?;
    let resp = c
        .post(format!("{base}{path}"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            anyhow!(
                "camoufox 服务不可达（{base}{path}）: {e}——启动方式：python scripts/camoufox_solver.py（或设置 READER_CAMOUFOX_URL / READER_CAMOUFOX_SCRIPT 自启动）"
            )
        })?;
    resp.json()
        .await
        .map_err(|e| anyhow!("camoufox 响应解析失败: {e}"))
}

/// 从 {name,value}[] → Vec<(String,String)>
fn cookies_from_json(arr: &Value) -> Vec<(String, String)> {
    arr.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|c| {
                    Some((
                        c.get("name")?.as_str()?.to_string(),
                        c.get("value")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ==================== 服务自 spawn（单可执行程序——首次用到自动拉起 Python 服务） ====================

/// 服务状态缓存：0=未尝试，1=就绪，-1=启动失败（快速失败，不反复空转 20s 轮询）
static SERVICE_STATE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(0);

/// 已 spawn 的 python 子进程（保活 + 检测提前退出）
static SPAWNED_CHILD: LazyLock<Mutex<Option<std::process::Child>>> =
    LazyLock::new(|| Mutex::new(None));

/// 脚本发现：READER_CAMOUFOX_SCRIPT → 本程序同目录 → ./scripts/ → ./（含 .exe 形态）
fn find_script() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("READER_CAMOUFOX_SCRIPT") {
        let p = p.trim();
        if !p.is_empty() {
            let pb = std::path::PathBuf::from(p);
            if pb.exists() {
                return Some(pb);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["camoufox_solver.py", "scripts/camoufox_solver.py"] {
                let pb = dir.join(name);
                if pb.exists() {
                    return Some(pb);
                }
            }
        }
    }
    for name in ["camoufox_solver.py", "scripts/camoufox_solver.py"] {
        let pb = std::path::PathBuf::from(name);
        if pb.exists() {
            return Some(pb);
        }
    }
    None
}

fn python_bin() -> Option<&'static str> {
    if which("python3").is_some() {
        Some("python3")
    } else if which("python").is_some() {
        Some("python")
    } else {
        None
    }
}

/// 确保 camoufox 服务可用：已配置 READER_CAMOUFOX_URL → Ok（连接失败在请求时报错）；
/// 否则发现脚本并 spawn `python3 <script> --port <port>`，等待 /health 就绪。
/// 失败状态缓存——不会反复空转（脚本提前退出 / 超时 → 快速失败并缓存）。
pub async fn ensure_service() -> Result<()> {
    if !enabled() {
        return Err(anyhow!("camoufox 已禁用（READER_CAMOUFOX_DISABLE=1）"));
    }
    if std::env::var("READER_CAMOUFOX_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .is_some()
    {
        return Ok(()); // 连接既有服务；不可达时请求报错
    }
    match SERVICE_STATE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => return Ok(()),
        -1 => return Err(anyhow!("camoufox 求解服务启动失败（已缓存）——请检查 python3 与 camoufox 依赖，或设置 READER_CAMOUFOX_URL")),
        _ => {}
    }
    if std::env::var("READER_CAMOUFOX_SPAWN")
        .map(|v| v.trim() == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        SERVICE_STATE.store(-1, std::sync::atomic::Ordering::Relaxed);
        return Err(anyhow!(
            "camoufox 服务未配置且自 spawn 已禁用（READER_CAMOUFOX_SPAWN=0）——请设置 READER_CAMOUFOX_URL"
        ));
    }
    let script = match find_script() {
        Some(s) => s,
        None => {
            SERVICE_STATE.store(-1, std::sync::atomic::Ordering::Relaxed);
            return Err(anyhow!(
                "找不到 camoufox_solver.py——请设置 READER_CAMOUFOX_SCRIPT（或 READER_CAMOUFOX_URL 连接既有服务）"
            ));
        }
    };
    let python = match python_bin() {
        Some(p) => p,
        None => {
            SERVICE_STATE.store(-1, std::sync::atomic::Ordering::Relaxed);
            return Err(anyhow!(
                "找不到 python3/python——无法自 spawn camoufox_solver.py（请设置 READER_CAMOUFOX_URL 连接既有服务）"
            ));
        }
    };
    let port = server_port();
    let mut cmd = std::process::Command::new(python);
    cmd.arg(&script)
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg("127.0.0.1");
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    tracing::info!(
        "自 spawn camoufox 求解服务: {python} {} --port {port}",
        script.display()
    );
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            SERVICE_STATE.store(-1, std::sync::atomic::Ordering::Relaxed);
            return Err(anyhow!(
                "camoufox_solver.py 启动失败（{python} {}）: {e}",
                script.display()
            ));
        }
    };
    *SPAWNED_CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    // 健康轮询（最长 20s；脚本提前退出 → 快速失败，不空转）
    for _ in 0..40 {
        // 子进程提前退出（如 camoufox 依赖未安装）→ 立即失败
        let exited = SPAWNED_CHILD
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten());
        if exited.is_some() {
            SERVICE_STATE.store(-1, std::sync::atomic::Ordering::Relaxed);
            return Err(anyhow!(
                "camoufox_solver.py 提前退出（可能是 camoufox 依赖未安装——pip install camoufox && camoufox fetch）"
            ));
        }
        if health().await.unwrap_or(false) {
            SERVICE_STATE.store(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    SERVICE_STATE.store(-1, std::sync::atomic::Ordering::Relaxed);
    Err(anyhow!(
        "camoufox 求解服务启动超时（20s）——请检查 python3 与 camoufox 依赖"
    ))
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(if cfg!(windows) { ';' } else { ':' }) {
        let p = std::path::PathBuf::from(dir).join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ==================== /solve（CF/Turnstile/滑块质询求解） ====================

/// 解质询：HTTP 调 camoufox /solve。proxy：书源级代理（None = READER_CAMOUFOX_PROXY）。
pub async fn solve(
    url: &str,
    cookies: &[(String, String)],
    max_wait_ms: u64,
    proxy: Option<&str>,
) -> Result<CfSolution> {
    if !enabled() {
        return Err(anyhow!("camoufox 已禁用（READER_CAMOUFOX_DISABLE=1）"));
    }
    let proxy = proxy
        .filter(|p| !p.trim().is_empty())
        .map(String::from)
        .or_else(default_proxy);
    let payload = json!({
        "url": url,
        "cookies": cookies.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>(),
        "maxWaitMs": max_wait_ms,
        "userAgent": solve_ua(),
        "proxy": proxy,
    });
    // 超时：求解上限 + 20s 余量（导航/提取），封顶 120s——服务不可达时连接拒绝立即返回
    let timeout = max_wait_ms.saturating_add(20).min(120);
    let v = post_json("/solve", payload, timeout).await?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let diag = v.get("diagnostics").cloned().unwrap_or(Value::Null);
        return Err(anyhow!(
            "camoufox 求解失败: {err}{}",
            if diag.is_object() {
                format!("（诊断: {diag}）")
            } else {
                String::new()
            }
        ));
    }
    let html = v
        .get("html")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if html.is_empty() {
        return Err(anyhow!("camoufox 响应缺少 html 字段"));
    }
    Ok(CfSolution {
        html,
        cookies: cookies_from_json(v.get("cookies").unwrap_or(&Value::Null)),
        user_agent: v
            .get("userAgent")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        turnstile_token: v
            .get("turnstileToken")
            .and_then(|x| x.as_str())
            .map(String::from)
            .filter(|s| !s.is_empty()),
        turnstile_sitekey: v
            .get("turnstileSitekey")
            .and_then(|x| x.as_str())
            .map(String::from)
            .filter(|s| !s.is_empty()),
    })
}

// ==================== /login（登录会话：填表/滑块/图片验证码两步流） ====================

fn parse_login_session(v: Value) -> Result<LoginSession> {
    let status = v
        .get("status")
        .and_then(|x| x.as_str())
        .unwrap_or("error")
        .to_string();
    let session_id = v
        .get("sessionId")
        .and_then(|x| x.as_str())
        .map(String::from);
    let captcha = v.get("captcha").map(|c| LoginCaptcha {
        base64: c
            .get("base64")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        x: c.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0),
        y: c.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0),
        w: c.get("w").and_then(|x| x.as_f64()).unwrap_or(0.0),
        h: c.get("h").and_then(|x| x.as_f64()).unwrap_or(0.0),
    });
    Ok(LoginSession {
        status,
        session_id,
        html: v
            .get("html")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        cookies: cookies_from_json(v.get("cookies").unwrap_or(&Value::Null)),
        user_agent: v
            .get("userAgent")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        turnstile_token: v
            .get("turnstileToken")
            .and_then(|x| x.as_str())
            .map(String::from)
            .filter(|s| !s.is_empty()),
        url: v
            .get("url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        captcha,
        error: v.get("error").and_then(|x| x.as_str()).map(String::from),
        message: v.get("message").and_then(|x| x.as_str()).map(String::from),
    })
}

/// 登录第一步：填表单 + 提交 + 自动滑块/质询；图片验证码 → need_captcha（两步回填）。
pub async fn login_start(
    url: &str,
    username: &str,
    password: &str,
    cookies: &[(String, String)],
    proxy: Option<&str>,
    max_wait_ms: u64,
) -> Result<LoginSession> {
    let proxy = proxy
        .filter(|p| !p.trim().is_empty())
        .map(String::from)
        .or_else(default_proxy);
    let payload = json!({
        "url": url,
        "username": username,
        "password": password,
        "cookies": cookies.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>(),
        "maxWaitMs": max_wait_ms,
        "userAgent": solve_ua(),
        "proxy": proxy,
    });
    let v = post_json("/login", payload, max_wait_ms.saturating_add(20).min(120)).await?;
    parse_login_session(v)
}

/// 登录第二步：回填图片验证码 → 重新提交 → 等待（可能再次 need_captcha——验证码错了换图）。
pub async fn login_captcha(
    session_id: &str,
    captcha: &str,
    max_wait_ms: u64,
) -> Result<LoginSession> {
    let payload = json!({
        "sessionId": session_id,
        "captcha": captcha,
        "maxWaitMs": max_wait_ms,
    });
    let v = post_json(
        "/login/captcha",
        payload,
        max_wait_ms.saturating_add(20).min(120),
    )
    .await?;
    parse_login_session(v)
}

/// 关闭登录会话（会话闲置由服务端 10 分钟自动回收；显式关闭更即时）。
pub async fn login_close(session_id: &str) -> Result<()> {
    let v = post_json("/login/close", json!({ "sessionId": session_id }), 15).await?;
    let _ = v;
    Ok(())
}

/// 验证码探测（/probe）：导航登录页 → 检测验证码类型 + 图片截图（getCaptcha 用）。
pub async fn probe(url: &str, cookies: &[(String, String)], proxy: Option<&str>) -> Result<Value> {
    let proxy = proxy
        .filter(|p| !p.trim().is_empty())
        .map(String::from)
        .or_else(default_proxy);
    let payload = json!({
        "url": url,
        "cookies": cookies.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>(),
        "userAgent": solve_ua(),
        "proxy": proxy,
    });
    post_json("/probe", payload, 45).await
}

/// 健康检查（GET /health → 200 + ok:true）——集成测试/探活用
pub async fn health() -> Result<bool> {
    let base = server_url();
    let c = client(5)?;
    let resp = c
        .get(format!("{base}/health"))
        .send()
        .await
        .map_err(|e| anyhow!("camoufox 服务不可达（{base}）: {e}"))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("camoufox /health 响应解析失败: {e}"))?;
    Ok(v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量测试串行锁
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_server_url_default_and_env() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("READER_CAMOUFOX_URL");
        assert_eq!(server_url(), "http://127.0.0.1:8196");
        std::env::set_var("READER_CAMOUFOX_URL", "http://127.0.0.1:9999/");
        assert_eq!(server_url(), "http://127.0.0.1:9999");
        std::env::remove_var("READER_CAMOUFOX_URL");
    }

    #[test]
    fn test_server_port() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("READER_CAMOUFOX_URL");
        assert_eq!(server_port(), 8196);
        std::env::set_var("READER_CAMOUFOX_URL", "http://127.0.0.1:9123/");
        assert_eq!(server_port(), 9123);
        std::env::remove_var("READER_CAMOUFOX_URL");
    }

    #[test]
    fn test_enabled_flags() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("READER_CAMOUFOX_DISABLE");
        assert!(enabled());
        std::env::set_var("READER_CAMOUFOX_DISABLE", "1");
        assert!(!enabled());
        std::env::remove_var("READER_CAMOUFOX_DISABLE");
    }

    #[test]
    fn test_solve_ua_default_and_env() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("READER_CAMOUFOX_UA");
        let d = solve_ua();
        assert!(d.contains("Chrome/"), "默认 UA 应为 Chrome: {d}");
        std::env::set_var(
            "READER_CAMOUFOX_UA",
            "Mozilla/5.0 (X11; Linux x86_64) Firefox/143.0",
        );
        assert_eq!(solve_ua(), "Mozilla/5.0 (X11; Linux x86_64) Firefox/143.0");
        std::env::set_var("READER_CAMOUFOX_UA", "  ");
        assert!(solve_ua().contains("Chrome/"), "空白 env 回退默认");
        std::env::remove_var("READER_CAMOUFOX_UA");
    }

    #[test]
    fn test_default_proxy() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("READER_CAMOUFOX_PROXY");
        assert!(default_proxy().is_none());
        std::env::set_var("READER_CAMOUFOX_PROXY", "socks5://127.0.0.1:1080");
        assert_eq!(default_proxy().as_deref(), Some("socks5://127.0.0.1:1080"));
        std::env::remove_var("READER_CAMOUFOX_PROXY");
    }

    #[test]
    fn test_cookies_from_json() {
        let v = json!([{"name": "a", "value": "1"}, {"name": "b", "value": ""}]);
        assert_eq!(
            cookies_from_json(&v),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "".to_string())
            ]
        );
        assert_eq!(
            cookies_from_json(&Value::Null),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn test_login_session_cookies_to_string() {
        let s = LoginSession {
            status: "ok".into(),
            session_id: None,
            html: String::new(),
            cookies: vec![
                ("b".to_string(), "2".to_string()),
                ("a".to_string(), "1".to_string()),
            ],
            user_agent: String::new(),
            turnstile_token: None,
            url: String::new(),
            captcha: None,
            error: None,
            message: None,
        };
        assert_eq!(s.cookies_to_string(), "a=1; b=2");
    }
}
