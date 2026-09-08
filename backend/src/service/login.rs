//! 书源登录（loginUrl + loginCheckJs + 验证码）——登录态独立于系统用户（按用户命名空间存库）
//!
//! 三条路径：
//! 1) **HTTP 直连**（默认）：POST（表单）/GET 执行 loginUrl（支持 {user}/{pass}/{captcha}
//!    及 {{...}} 双花括号占位符；带书源既有 cookie）→ 响应 Set-Cookie 合并存库（按用户）→
//!    执行 loginCheckJs（复用 js shim，vars: cookie/result/url）→ true/false。
//! 2) **浏览器自动**（mode=browser 或 HTTP 流检测到点击类验证码后自动切换）：
//!    headless 浏览器（CDP）填表单、滑块自动拖拽（人类轨迹）、图片验证码截图给前端。
//! 3) **图片验证码**：返回 captchaUrl（页面提取 URL 或浏览器截图 data URI）+ captchaId；
//!    前端输入后重新调用 loginBookSource（captcha 参数，HTTP 流）或 submitCaptcha（浏览器流）。
//!
//! 点击类验证码（滑块/点选）处理策略：
//! - 滑块：浏览器自动拖拽（2 次尝试）；失败/超时（30s）→ "需手动 Cookie" 错误
//! - 点选：无法自动识别目标点 → "需手动 Cookie" 错误（请在浏览器登录后粘贴 Cookie）

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::model::BookSource;
use crate::service::{browser, crawler, search};
use crate::storage::Storage;

/// 登录请求参数（均可选）
#[derive(Debug, Clone, Default)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// 图片验证码文本（前端输入后回传）
    pub captcha: String,
}

/// 登录结果（软性结果；硬错误走 Err）
pub enum LoginOutcome {
    /// 登录成功（cookie 已存库，按用户）
    Success { cookie: String },
    /// 需要图片验证码：captcha_url 给前端（页面提取 URL 或浏览器截图 data URI）
    NeedImageCaptcha {
        captcha_url: String,
        captcha_id: String,
        message: String,
    },
    /// 点击类验证码无法自动处理/失败/超时 → 引导手动 Cookie
    NeedManualCookie { message: String },
    /// 登录失败（loginCheckJs 未通过，无验证码）
    Failed { message: String },
}

// ==================== 占位符 / 表单 / loginCheckJs ====================

/// loginUrl/loginBody 占位符替换：{user}/{pass}/{captcha}/{username}/{password} 及双花括号变体。
/// 双花括号优先（避免 `{{user}}` 被 `{user}` 二次替换错位）。
pub fn replace_login_placeholders(
    s: &str,
    username: &str,
    password: &str,
    captcha: &str,
) -> String {
    let mut out = s.to_string();
    out = out
        .replace("{{user}}", username)
        .replace("{{pass}}", password)
        .replace("{{captcha}}", captcha)
        .replace("{{username}}", username)
        .replace("{{password}}", password);
    out = out
        .replace("{user}", username)
        .replace("{pass}", password)
        .replace("{captcha}", captcha)
        .replace("{username}", username)
        .replace("{password}", password);
    out
}

/// 构建登录表单体（application/x-www-form-urlencoded）：
/// loginUi 字段名优先（password 类型→密码；captcha 相关→验证码；首个其余→用户名），
/// 无 loginUi 时缺省 username/password（+captcha，若提供了验证码参数）。
pub fn build_login_form(source: &BookSource, req: &LoginRequest) -> String {
    let fields: Vec<(String, String)> = source
        .login_ui
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
        .map(|items| {
            items
                .iter()
                .map(|it| {
                    let name = it
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let typ = it
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("text")
                        .to_string();
                    (name, typ)
                })
                .collect()
        })
        .unwrap_or_else(|| {
            let mut d = vec![
                ("username".to_string(), "text".to_string()),
                ("password".to_string(), "password".to_string()),
            ];
            if !req.captcha.is_empty() {
                d.push(("captcha".to_string(), "text".to_string()));
            }
            d
        });
    let mut user_done = false;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (name, typ) in fields {
        if name.is_empty() {
            continue;
        }
        let t = typ.to_lowercase();
        let n = name.to_lowercase();
        let value = if t.contains("password") {
            req.password.clone()
        } else if n.contains("captcha")
            || n.contains("vcode")
            || n.contains("verify")
            || n.contains("checkcode")
            || t.contains("captcha")
            || t.contains("verify")
        {
            req.captcha.clone()
        } else if !user_done {
            user_done = true;
            req.username.clone()
        } else {
            String::new()
        };
        pairs.push((name, value));
    }
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(&k, &v);
    }
    ser.finish()
}

/// 执行 loginCheckJs（空脚本 = 默认成功，legacy 语义）。
/// 注入 vars：cookie（合并后 cookie 串）/result（响应体）/url（最终 URL）。
/// 返回 true = 已登录。
pub fn check_login(js: &str, cookie: &str, result: &str, url: &str) -> Result<bool> {
    let js = js.trim();
    if js.is_empty() {
        return Ok(true);
    }
    let mut vars = HashMap::new();
    vars.insert("cookie".to_string(), cookie.to_string());
    vars.insert("result".to_string(), result.to_string());
    vars.insert("url".to_string(), url.to_string());
    let r = crate::parser::js::eval_js(js, &vars)?;
    let r = r.trim();
    Ok(r.eq_ignore_ascii_case("true") || r == "1")
}

/// 无 loginCheckJs 时的失败标记嗅探：站点常对任意账密回 200 + 会话 Cookie,
/// 仅凭状态码会「假成功」——先扫常见失败文案并回带片段
fn login_failure_marker(body: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "密码错误",
        "密码不正确",
        "用户名错误",
        "账号或密码错误",
        "用户名或密码错误",
        "账号不存在",
        "用户名不存在",
        "账户不存在",
        "登录失败",
        "验证码错误",
        "incorrect password",
        "invalid credentials",
        "wrong password",
        "login failed",
    ];
    let lower = body.to_lowercase();
    for m in MARKERS {
        if let Some(pos) = lower.find(m) {
            let snippet: String = body[pos..].chars().take(30).collect();
            let snippet = snippet.split('<').next().unwrap_or("").trim();
            return Some(format!("站点提示登录失败: {snippet}"));
        }
    }
    None
}

/// Set-Cookie 合并（响应多个 Set-Cookie + 用户既有 cookie）：
/// 按 name 合并——新 Set-Cookie 覆盖同名、空值删除、其余保留；顺序稳定（既有为基底 + 新名追加）
pub fn merge_cookie(existing: &str, set_cookies: &[String]) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, String> = HashMap::new();
    for (k, v) in crawler::parse_cookie_string(existing) {
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.insert(k, v);
    }
    for sc in set_cookies {
        let first = sc.split(';').next().unwrap_or("").trim();
        let Some((k, v)) = first.split_once('=') else {
            continue;
        };
        let k = k.trim().to_string();
        if k.is_empty() {
            continue;
        }
        let v = v.trim();
        if v.is_empty() {
            // 空值 = 删除该 cookie
            map.remove(&k);
            order.retain(|x| x != &k);
            continue;
        }
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.insert(k, v.to_string());
    }
    order
        .into_iter()
        .filter_map(|k| map.get(&k).map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join("; ")
}

// ==================== 验证码特征（页面 HTML 启发式） ====================

/// 点击类验证码检测（页面特征匹配）：返回 Some("slider"|"click")。
/// 命中即认为需浏览器/手动处理（不做 OCR、不做 headless 之外的破解）。
pub fn detect_click_captcha(html: &str) -> Option<&'static str> {
    let lower = html.to_lowercase();
    let slider_markers = [
        "geetest",
        "极验",
        "gt.js",
        "gt4",
        "滑块",
        "滑动验证",
        "slide-verify",
        "slider-verify",
        "tcaptcha",
        "nc_1_n1z",
        "aliyun",
        "阿里云验证码",
        "拖动滑块",
        "拼图",
        "jigsaw",
        "dx-captcha",
        "顶象",
        "dragverify",
        "slidercaptcha",
    ];
    if slider_markers.iter().any(|m| lower.contains(m)) {
        return Some("slider");
    }
    let click_markers = [
        "点选",
        "click-verify",
        "clickcaptcha",
        "verify-point",
        "字符点选",
        "语序点选",
        "points-verify",
    ];
    if click_markers.iter().any(|m| lower.contains(m)) {
        return Some("click");
    }
    None
}

/// 图片验证码 URL 提取：页面 `<img>` 中 src/id/class/alt 含验证码特征者取其 src（相对路径拼绝对）。
pub fn extract_image_captcha_url(html: &str, base_url: &str) -> Option<String> {
    let re = regex::Regex::new(r"<img[^>]*>").expect("static regex");
    for cap in re.captures_iter(html) {
        let tag = cap.get(0)?.as_str();
        let ctx = tag.to_lowercase();
        let has_feature = [
            "captcha",
            "vcode",
            "verify",
            "yzm",
            "checkcode",
            "验证码",
            "randimg",
            "kaptcha",
        ]
        .iter()
        .any(|k| ctx.contains(k));
        if !has_feature {
            continue;
        }
        for attr in ["src", "data-src", "data-original"] {
            let attr_re = regex::Regex::new(&format!(r#"{attr}\s*=\s*["']([^"']+)["']"#))
                .expect("static regex");
            if let Some(m) = attr_re.captures(tag) {
                let url = m.get(1)?.as_str();
                if url.starts_with("data:") {
                    return Some(url.to_string());
                }
                return Some(search::to_absolute(url, base_url));
            }
        }
    }
    None
}

// ==================== 验证码会话缓存（内存，5 分钟过期） ====================

struct CaptchaSession {
    ns: String,
    source_url: String,
    kind: String,
    username: String,
    password: String,
    created: Instant,
    /// camoufox 登录会话 id（图片验证码两步流第二步回填用；HTTP 流验证码为 None）
    browser_session: Option<String>,
}

static CAPTCHA_SESSIONS: LazyLock<Mutex<HashMap<String, CaptchaSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const CAPTCHA_TTL: Duration = Duration::from_secs(300);

fn new_captcha_session(ns: &str, source: &BookSource, kind: &str, req: &LoginRequest) -> String {
    new_captcha_session_impl(ns, source, kind, req, None)
}

/// 浏览器流验证码会话：携带 camoufox 会话 id（/login/captcha 两步回填）
fn new_captcha_session_browser(
    ns: &str,
    source: &BookSource,
    kind: &str,
    req: &LoginRequest,
    browser_session: String,
) -> String {
    new_captcha_session_impl(ns, source, kind, req, Some(browser_session))
}

fn new_captcha_session_impl(
    ns: &str,
    source: &BookSource,
    kind: &str,
    req: &LoginRequest,
    browser_session: Option<String>,
) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let mut guard = CAPTCHA_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    // 过期清理
    guard.retain(|_, s| s.created.elapsed() < CAPTCHA_TTL);
    guard.insert(
        id.clone(),
        CaptchaSession {
            ns: ns.to_string(),
            source_url: source.book_source_url.clone(),
            kind: kind.to_string(),
            username: req.username.clone(),
            password: req.password.clone(),
            created: Instant::now(),
            browser_session,
        },
    );
    id
}

fn get_captcha_session(id: &str) -> Option<CaptchaSession> {
    let mut guard = CAPTCHA_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, s| s.created.elapsed() < CAPTCHA_TTL);
    guard.remove(id)
}

// ==================== HTTP 直连登录流 ====================

/// 书源登录（默认 HTTP 流）。点击类验证码命中且浏览器可用 → 自动切换浏览器流。
pub async fn login_http(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    req: &LoginRequest,
) -> Result<LoginOutcome> {
    let login_url = source
        .login_url
        .as_deref()
        .ok_or_else(|| anyhow!("书源未配置 loginUrl"))?;
    // `,{...}` 后缀（method/body/charset/headers，对齐搜索链路）
    let (raw_url, suffix) = search::split_url_suffix(login_url);
    let url = replace_login_placeholders(&raw_url, &req.username, &req.password, &req.captcha);

    let mut req_headers = source
        .header
        .as_deref()
        .map(crawler::parse_header)
        .unwrap_or_default();
    if let Some(extra) = &suffix.headers {
        for (k, v) in extra {
            req_headers.insert(k.clone(), v.clone());
        }
    }

    let method = suffix.method.as_deref().unwrap_or("GET").to_string();
    let body = if let Some(b) = &suffix.body {
        Some(replace_login_placeholders(
            b,
            &req.username,
            &req.password,
            &req.captcha,
        ))
    } else if method.eq_ignore_ascii_case("POST") {
        Some(build_login_form(source, req))
    } else {
        None
    };

    let resp = if method.eq_ignore_ascii_case("POST") {
        req_headers.insert(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        );
        crawler::http_post_retry(
            ns,
            &url,
            &req_headers,
            20,
            body.as_deref(),
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await?
    } else {
        crawler::http_get_retry(
            ns,
            &url,
            &req_headers,
            20,
            suffix.charset.as_deref(),
            source.proxy_url.as_deref(),
            suffix.retry,
        )
        .await?
    };

    // Set-Cookie 合并存库（按用户）
    let set_cookies: Vec<String> = resp
        .headers
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .map(|(_, v)| v.clone())
        .collect();
    let existing = storage.get_cookie(ns, &source.book_source_url).await?;
    let merged = merge_cookie(existing.as_deref().unwrap_or(""), &set_cookies);
    if !merged.is_empty() {
        storage
            .set_cookie(ns, &source.book_source_url, &merged)
            .await?;
    }

    // loginCheckJs; 缺失时启发式防假成功: 无失败标记 + 合并 Cookie 回访 loginUrl 不再展示登录表单
    let has_check_js = source.login_check_js.as_deref().is_some_and(|j| !j.trim().is_empty());
    let ok = match &source.login_check_js {
        Some(js) if !js.trim().is_empty() => check_login(js, &merged, &resp.body, &resp.url)?,
        _ => {
            if login_failure_marker(&resp.body).is_some() {
                false
            } else {
                let mut probe_headers = req_headers.clone();
                if !merged.is_empty() {
                    probe_headers.insert("Cookie".to_string(), merged.clone());
                }
                match crawler::http_get_retry(
                    ns,
                    &url,
                    &probe_headers,
                    20,
                    suffix.charset.as_deref(),
                    source.proxy_url.as_deref(),
                    suffix.retry,
                )
                .await
                {
                    Ok(pr) => {
                        let still_form =
                            pr.body.contains("type=\"password\"") || pr.body.contains("type='password'");
                        login_failure_marker(&pr.body).is_none() && !still_form
                    }
                    Err(_) => false,
                }
            }
        }
    };
    if ok {
        return Ok(LoginOutcome::Success { cookie: merged });
    }

    // 失败 → 验证码判定
    if let Some(kind) = detect_click_captcha(&resp.body) {
        // 点击类验证码：浏览器可用 → 自动切换浏览器流（滑块自动拖）；否则手动 Cookie
        if browser::is_browser_available() {
            tracing::info!(
                "书源 [{}] 检测到{kind}验证码——切换浏览器自动登录",
                source.book_source_name
            );
            return login_browser(storage, ns, source, req).await;
        }
        let kind_cn = if kind == "slider" { "滑块" } else { "点选" };
        return Ok(LoginOutcome::NeedManualCookie {
            message: format!(
                "检测到{kind_cn}验证码：请在浏览器登录该书源后，在书源设置粘贴 Cookie（配置 camoufox 服务后可使用浏览器自动登录）"
            ),
        });
    }
    // 图片验证码：页面含 captcha 图片 → captchaUrl 给前端
    if let Some(captcha_url) = extract_image_captcha_url(&resp.body, &resp.url) {
        let captcha_id = new_captcha_session(ns, source, "image", req);
        return Ok(LoginOutcome::NeedImageCaptcha {
            captcha_url,
            captcha_id,
            message: "需要图片验证码".to_string(),
        });
    }
    // loginUrl 规则含 {captcha} 占位符且首轮未带验证码 → 同样走图片验证码流程
    if raw_url.contains("{captcha}") && req.captcha.is_empty() {
        let captcha_id = new_captcha_session(ns, source, "image", req);
        return Ok(LoginOutcome::NeedImageCaptcha {
            captcha_url: extract_image_captcha_url(&resp.body, &resp.url).unwrap_or_default(),
            captcha_id,
            message: "需要图片验证码（loginUrl 含 {captcha} 占位符）".to_string(),
        });
    }
    Ok(LoginOutcome::Failed {
        message: if has_check_js {
            "登录失败：loginCheckJs 未通过".to_string()
        } else {
            login_failure_marker(&resp.body).unwrap_or_else(|| {
                "无法校验登录结果：登录请求未返回 Cookie 且书源未配 loginCheckJs——请在工作台补 loginCheckJs, 或改用手动 Cookie".to_string()
            })
        },
    })
}

// ==================== 浏览器自动登录流（camoufox /login 会话） ====================

/// 浏览器自动登录（mode=browser；HTTP 流检测到点击类验证码时自动调用）。
/// 30s 总超时；滑块/质询由 camoufox 服务端自动处理；图片验证码 → 两步流回填；
/// 点选/失败/超时 → "需手动 Cookie"。
pub async fn login_browser(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    req: &LoginRequest,
) -> Result<LoginOutcome> {
    let login_url = source
        .login_url
        .as_deref()
        .ok_or_else(|| anyhow!("书源未配置 loginUrl"))?;
    let (raw_url, _suffix) = search::split_url_suffix(login_url);
    let url = replace_login_placeholders(&raw_url, &req.username, &req.password, &req.captcha);

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        browser_login_inner(storage, ns, source, &url, req),
    )
    .await;
    match result {
        Ok(r) => r,
        Err(_) => Ok(LoginOutcome::NeedManualCookie {
            message: "浏览器自动登录超时（30s）——请在浏览器登录该书源后，在书源设置粘贴 Cookie"
                .to_string(),
        }),
    }
}

async fn browser_login_inner(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    url: &str,
    req: &LoginRequest,
) -> Result<LoginOutcome> {
    // P1 SSRF：登录 URL 公网校验后才允许浏览器导航（camoufox 服务端同样只走公网）
    crate::service::crawler::validate_public_target(url).await?;

    // 既有 cookie 注入（保持会话连续性）
    let cookie_str = storage
        .get_cookie(ns, &source.book_source_url)
        .await?
        .unwrap_or_default();
    let cookie_pairs = crawler::parse_cookie_string(&cookie_str);
    // 代理：书源级 proxyUrl 优先（机房 IP 解 Turnstile 需住宅代理）
    let proxy = source.proxy_url.as_deref();

    let sess = browser::login_start(
        url,
        &req.username,
        &req.password,
        &cookie_pairs,
        proxy,
        60_000,
    )
    .await
    .map_err(|e| anyhow!("camoufox 登录失败（{url}）: {e:#}"))?;

    login_session_to_outcome(storage, ns, source, req, &sess).await
}

/// camoufox LoginSession → LoginOutcome（登录成功判定 / 两步验证码 / 手动 Cookie）
async fn login_session_to_outcome(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    req: &LoginRequest,
    sess: &browser::LoginSession,
) -> Result<LoginOutcome> {
    match sess.status.as_str() {
        "ok" => {
            let cookie_str = sess.cookies_to_string();
            let html = &sess.html;
            let page_url = &sess.url;
            let has_check_js = source.login_check_js.as_deref().is_some_and(|j| !j.trim().is_empty());
            let ok = match &source.login_check_js {
                Some(js) if !js.trim().is_empty() => check_login(js, &cookie_str, html, page_url)?,
                _ => {
                    let still_form =
                        html.contains("type=\"password\"") || html.contains("type='password'");
                    login_failure_marker(html).is_none() && !cookie_str.is_empty() && !still_form
                }
            };
            if ok {
                if !cookie_str.is_empty() {
                    storage
                        .set_cookie(ns, &source.book_source_url, &cookie_str)
                        .await?;
                }
                tracing::info!("书源 [{}] 浏览器自动登录成功", source.book_source_name);
                return Ok(LoginOutcome::Success { cookie: cookie_str });
            }
            if detect_click_captcha(html).is_some() {
                return Ok(LoginOutcome::NeedManualCookie {
                    message:
                        "浏览器自动登录未通过验证——请在浏览器登录该书源后，在书源设置粘贴 Cookie"
                            .to_string(),
                });
            }
            Ok(LoginOutcome::Failed {
                message: if has_check_js {
                    "浏览器登录失败：loginCheckJs 未通过".to_string()
                } else {
                    login_failure_marker(html).unwrap_or_else(|| {
                        "无法校验登录结果：浏览器会话无 Cookie 且书源未配 loginCheckJs——请补 loginCheckJs 或改用手动 Cookie".to_string()
                    })
                },
            })
        }
        "need_captcha" => {
            let Some(captcha) = sess.captcha.clone() else {
                return Ok(LoginOutcome::NeedManualCookie {
                    message:
                        "浏览器登录需要图片验证码但截图缺失——请在浏览器登录该书源后粘贴 Cookie"
                            .to_string(),
                });
            };
            let data_uri = format!("data:image/png;base64,{}", captcha.base64);
            let captcha_id = new_captcha_session_browser(
                ns,
                source,
                "image",
                req,
                sess.session_id.clone().unwrap_or_default(),
            );
            Ok(LoginOutcome::NeedImageCaptcha {
                captcha_url: data_uri,
                captcha_id,
                message: "需要图片验证码（浏览器截图）".to_string(),
            })
        }
        "timeout" | "error" => Ok(LoginOutcome::NeedManualCookie {
            message: sess.error.clone().unwrap_or_else(|| {
                "浏览器自动登录失败——请在浏览器登录该书源后粘贴 Cookie".to_string()
            }),
        }),
        _ => Ok(LoginOutcome::Failed {
            message: "浏览器登录失败：未知状态".to_string(),
        }),
    }
}

// ==================== getCaptcha / submitCaptcha（浏览器流，图片验证码） ====================

/// POST /reader3/getCaptcha：触发登录页 → camoufox /probe 检测验证码 →
/// {captchaType: image|slider|click|none, captchaUrl(data URI), captchaId, pageUrl}
pub async fn get_captcha(storage: &Storage, ns: &str, source: &BookSource) -> Result<Value> {
    if !browser::is_browser_available() {
        return Err(anyhow!(
            "浏览器后端未启用（camoufox）——请在书源设置粘贴 Cookie（配置 camoufox 服务后可使用浏览器自动登录）"
        ));
    }
    let login_url = source
        .login_url
        .as_deref()
        .ok_or_else(|| anyhow!("书源未配置 loginUrl"))?;
    let (raw_url, _suffix) = search::split_url_suffix(login_url);
    let url = replace_login_placeholders(&raw_url, "", "", "");
    // P1 SSRF：登录 URL 公网校验后才允许浏览器导航
    crate::service::crawler::validate_public_target(&url).await?;

    let cookie_str = storage
        .get_cookie(ns, &source.book_source_url)
        .await?
        .unwrap_or_default();
    let cookie_pairs = crawler::parse_cookie_string(&cookie_str);
    let probe = browser::probe_captcha(&url, &cookie_pairs, source.proxy_url.as_deref())
        .await
        .map_err(|e| anyhow!("验证码探测失败（{url}）: {e:#}"))?;
    let kind = probe
        .get("captchaType")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();
    let page_url = probe
        .get("pageUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match kind.as_str() {
        "image" => {
            let b64 = probe
                .get("captcha")
                .and_then(|c| c.get("base64"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let captcha_url = if b64.is_empty() {
                String::new()
            } else {
                format!("data:image/png;base64,{b64}")
            };
            let captcha_id = new_captcha_session(ns, source, "image", &LoginRequest::default());
            Ok(json!({
                "captchaType": "image",
                "captchaUrl": captcha_url,
                "captchaId": captcha_id,
                "pageUrl": page_url,
                "message": "需要图片验证码",
            }))
        }
        "slider" => Ok(json!({
            "captchaType": "slider",
            "pageUrl": page_url,
            "message": "检测到滑块验证码——请重新调用登录（camoufox 自动处理）",
        })),
        "click" => Ok(json!({
            "captchaType": "click",
            "pageUrl": page_url,
            "message": "检测到点选类验证码（无法自动识别）——请在浏览器登录该书源后粘贴 Cookie",
        })),
        _ => Ok(json!({ "captchaType": "none", "message": "未检测到验证码" })),
    }
}

/// POST /reader3/submitCaptcha：图片验证码文本回填。
/// - 浏览器流验证码（captchaId 携带 camoufox 会话）→ /login/captcha 两步回填
/// - HTTP 流验证码 → 重跑 HTTP 登录（带 captcha 占位符）
/// 成功 → cookie 存库 → loginCheckJs → {isLogin}
pub async fn submit_captcha(
    storage: &Storage,
    ns: &str,
    source: &BookSource,
    captcha_id: &str,
    captcha_text: &str,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Value> {
    let session = get_captcha_session(captcha_id)
        .ok_or_else(|| anyhow!("验证码会话已过期（5 分钟），请重新获取"))?;
    if session.ns != ns || session.source_url != source.book_source_url {
        return Err(anyhow!("验证码会话与书源不匹配"));
    }
    if session.kind != "image" {
        return Err(anyhow!("该验证码会话不是图片验证码，无法提交文本"));
    }
    if captcha_text.trim().is_empty() {
        return Err(anyhow!("请输入验证码"));
    }
    let req = LoginRequest {
        username: username.unwrap_or(&session.username).to_string(),
        password: username
            .map(|_| password.unwrap_or(&session.password).to_string())
            .unwrap_or_else(|| session.password.clone()),
        captcha: captcha_text.trim().to_string(),
    };
    let fut = async {
        if let Some(browser_sid) = session.browser_session.clone() {
            // 浏览器两步流：camoufox /login/captcha（会话内回填）
            let sess = browser::login_captcha(&browser_sid, &req.captcha, 60_000)
                .await
                .map_err(|e| anyhow!("camoufox 验证码回填失败: {e:#}"))?;
            login_session_to_outcome(storage, ns, source, &req, &sess).await
        } else {
            // HTTP 流：重跑 HTTP 登录（带 captcha）
            login_http(storage, ns, source, &req).await
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(30), fut).await;
    match result {
        Ok(Ok(outcome)) => Ok(outcome_to_json(outcome)),
        Ok(Err(e)) => Err(e),
        Err(_) => Ok(json!({
            "isLogin": false, "needManualCaptcha": true,
            "message": "验证码提交超时（30s）——请在浏览器登录该书源后，在书源设置粘贴 Cookie"
        })),
    }
}

/// LoginOutcome → submitCaptcha 响应 JSON
fn outcome_to_json(outcome: LoginOutcome) -> Value {
    match outcome {
        LoginOutcome::Success { cookie } => {
            json!({ "isLogin": true, "cookie": cookie, "needCaptcha": false })
        }
        LoginOutcome::NeedImageCaptcha {
            captcha_url,
            captcha_id,
            message,
        } => json!({
            "isLogin": false, "needCaptcha": true, "captchaUrl": captcha_url,
            "captchaId": captcha_id, "message": message
        }),
        LoginOutcome::NeedManualCookie { message } => json!({
            "isLogin": false, "needManualCaptcha": true, "message": message
        }),
        LoginOutcome::Failed { message } => json!({
            "isLogin": false, "message": message
        }),
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn source_with_login(login_url: &str, login_check_js: &str) -> BookSource {
        BookSource {
            book_source_url: "https://src.test".to_string(),
            book_source_name: "测试源".to_string(),
            login_url: Some(login_url.to_string()),
            login_check_js: if login_check_js.is_empty() {
                None
            } else {
                Some(login_check_js.to_string())
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_replace_placeholders() {
        // 双花括号优先 + 单花括号 + 各字段
        assert_eq!(
            replace_login_placeholders(
                "https://a.com/login?u={{user}}&p={{pass}}&c={{captcha}}",
                "u1",
                "p1",
                "c1"
            ),
            "https://a.com/login?u=u1&p=p1&c=c1"
        );
        assert_eq!(
            replace_login_placeholders(
                "https://a.com/login?u={user}&p={pass}&c={captcha}",
                "u1",
                "p1",
                "c1"
            ),
            "https://a.com/login?u=u1&p=p1&c=c1"
        );
        // 未提供字段 → 空串
        assert_eq!(
            replace_login_placeholders("https://a.com/login?c={captcha}", "", "", ""),
            "https://a.com/login?c="
        );
        // username/password 别名
        assert_eq!(
            replace_login_placeholders(
                "https://a.com/{{username}}/{{password}}",
                "alice",
                "pw",
                ""
            ),
            "https://a.com/alice/pw"
        );
    }

    #[test]
    fn test_check_login() {
        // 空脚本 = 成功（legacy 语义）
        assert!(check_login("", "a=1", "body", "https://a.com").unwrap());
        // true/1 → 成功
        assert!(check_login(
            "result.indexOf('ok') >= 0",
            "a=1",
            "ok body",
            "https://a.com"
        )
        .unwrap());
        assert!(!check_login(
            "result.indexOf('ok') >= 0",
            "a=1",
            "bad body",
            "https://a.com"
        )
        .unwrap());
        assert!(check_login(
            "cookie.indexOf('sid') >= 0",
            "sid=1; a=2",
            "x",
            "https://a.com"
        )
        .unwrap());
        assert!(!check_login("cookie.indexOf('sid') >= 0", "a=2", "x", "https://a.com").unwrap());
        // 布尔表达式直返
        assert!(check_login("true", "", "", "").unwrap());
        assert!(!check_login("false", "", "", "").unwrap());
    }

    #[test]
    fn test_merge_cookie() {
        // 新 Set-Cookie 覆盖同名、不同名保留、空值删除、顺序稳定
        let merged = merge_cookie(
            "sid=old; theme=dark",
            &[
                "sid=new; Path=/; HttpOnly".to_string(),
                "token=abc".to_string(),
            ],
        );
        assert_eq!(merged, "sid=new; theme=dark; token=abc");
        // 空值删除
        let merged = merge_cookie(
            "sid=old; theme=dark",
            &["sid=; Expires=Thu, 01 Jan 1970".to_string()],
        );
        assert_eq!(merged, "theme=dark");
        // 无既有 + 无 Set-Cookie
        assert_eq!(merge_cookie("", &[]), "");
        // 仅既有
        assert_eq!(merge_cookie("a=1", &[]), "a=1");
    }

    #[test]
    fn test_build_login_form() {
        let src = source_with_login("https://a.com/login", "");
        let req = LoginRequest {
            username: "u1".into(),
            password: "p1".into(),
            captcha: "".into(),
        };
        assert_eq!(build_login_form(&src, &req), "username=u1&password=p1");
        // 带验证码 → 追加 captcha 字段
        let req = LoginRequest {
            username: "u1".into(),
            password: "p1".into(),
            captcha: "c1".into(),
        };
        assert_eq!(
            build_login_form(&src, &req),
            "username=u1&password=p1&captcha=c1"
        );
        // loginUi 字段名优先
        let mut src2 = src.clone();
        src2.login_ui = Some(r#"[{"name":"loginName","type":"text"},{"name":"loginPassword","type":"password"},{"name":"vcode","type":"text"}]"#.into());
        let req = LoginRequest {
            username: "u2".into(),
            password: "p2".into(),
            captcha: "v2".into(),
        };
        assert_eq!(
            build_login_form(&src2, &req),
            "loginName=u2&loginPassword=p2&vcode=v2"
        );
    }

    #[test]
    fn test_detect_click_captcha() {
        assert_eq!(
            detect_click_captcha("<html>geetest slider</html>"),
            Some("slider")
        );
        assert_eq!(
            detect_click_captcha("<html>滑动验证</html>"),
            Some("slider")
        );
        assert_eq!(detect_click_captcha("<html>点选验证</html>"), Some("click"));
        assert_eq!(detect_click_captcha("<html>normal page</html>"), None);
        // 图片验证码页（img captcha）不算点击类
        assert_eq!(
            detect_click_captcha(r#"<img src="/captcha.png" alt="验证码">"#),
            None
        );
    }

    #[test]
    fn test_extract_image_captcha_url() {
        let html =
            r#"<html><img src="/captcha.png"><img id="vcode" src="https://a.com/c.png"></html>"#;
        assert_eq!(
            extract_image_captcha_url(html, "https://a.com/login").as_deref(),
            Some("https://a.com/captcha.png")
        );
        // 相对路径拼绝对
        let html = r#"<img class="captcha-img" data-src="/api/code?t=1">"#;
        assert_eq!(
            extract_image_captcha_url(html, "https://a.com/login").as_deref(),
            Some("https://a.com/api/code?t=1")
        );
        // 无验证码图 → None
        assert_eq!(
            extract_image_captcha_url("<img src='/logo.png'>", "https://a.com"),
            None
        );
    }

    #[test]
    fn test_captcha_session_ttl_and_match() {
        let src = source_with_login("https://a.com/login", "");
        let req = LoginRequest {
            username: "u".into(),
            password: "p".into(),
            captcha: "".into(),
        };
        let id = new_captcha_session("default", &src, "image", &req);
        let s = get_captcha_session(&id).unwrap();
        assert_eq!(s.ns, "default");
        assert_eq!(s.source_url, "https://src.test");
        assert_eq!(s.kind, "image");
        // 二次获取（已移除）→ None
        assert!(get_captcha_session(&id).is_none());
        // 未知 id → None
        assert!(get_captcha_session("nope").is_none());
    }
}

/// 定位非 Send 类型：tokio::spawn 要求 future Send（axum Handler 同约束）
#[cfg(test)]
mod send_tests {
    use super::*;

    async fn test_storage() -> Storage {
        let dir = std::env::temp_dir().join(format!("reader-login-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        crate::storage::init(&config).await.unwrap()
    }

    #[tokio::test]
    async fn test_login_futures_are_send() {
        let storage = test_storage().await;
        let src = BookSource {
            book_source_url: "https://a.com".into(),
            book_source_name: "A".into(),
            login_url: Some("https://a.com/login".into()),
            ..Default::default()
        };
        let s2 = storage.clone();
        let src2 = src.clone();
        tokio::spawn(async move {
            let _ = login_http(&s2, "default", &src2, &LoginRequest::default()).await;
        });
        let s3 = storage.clone();
        let src3 = src.clone();
        tokio::spawn(async move {
            let _ = login_browser(&s3, "default", &src3, &LoginRequest::default()).await;
        });
        storage.pool.close().await;
        let dir = std::env::temp_dir().join(format!("reader-login-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
