//! 鉴权、用户信息、系统信息与管理员用户管理 API

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::LazyLock;
use std::time::Instant;

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::router::{now_millis, param_of, AppState, ReturnData};
use crate::model::User;

/// 进程启动时刻（用于 `/reader3/getSystemInfo` 的 `uptimeSeconds`）
static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// POST `/reader3/login` 请求体
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginBody {
    username: Option<String>,
    password: Option<String>,
    is_login: Option<bool>,
    code: Option<String>,
}

/// POST `/reader3/login`：注册或登录，返回 `format_user`（camelCase）
pub async fn login(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Json<ReturnData> {
    let username = body.username.clone().unwrap_or_default();
    let password = body.password.clone().unwrap_or_default();
    let is_login = body.is_login.unwrap_or(false);

    if username.is_empty() {
        return Json(ReturnData::err("请输入用户名"));
    }
    if password.is_empty() {
        return Json(ReturnData::err("请输入密码"));
    }

    let ip = client_ip(&peer, &headers);
    if let Err(msg) = crate::util::login_limit::check_allowed(&username, &ip) {
        return Json(ReturnData::err(msg));
    }

    let user = match state.storage.find_user(&username).await {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("查询用户 {username} 失败: {e}");
            return Json(ReturnData::err("系统错误"));
        }
    };

    let Some(mut user) = user else {
        if is_login {
            crate::util::login_limit::record_failure(&username, &ip);
            return Json(ReturnData::err("用户不存在"));
        }
        return register(&state, &username, &password, body.code.clone()).await;
    };

    if !is_login {
        return Json(ReturnData::err("用户名已被占用"));
    }
    if !crate::util::password::verify_password(&state.storage, &user, &password).await {
        crate::util::login_limit::record_failure(&username, &ip);
        return Json(ReturnData::err("密码错误"));
    }
    if user.disabled {
        return Json(ReturnData::err("账号已停用, 请联系管理员"));
    }
    crate::util::login_limit::reset(&username, &ip);
    let now = now_millis();
    let token = uuid::Uuid::new_v4().simple().to_string();
    if let Err(e) = state.storage.add_user_token(&username, &token, now).await {
        tracing::error!("更新用户 {username} 会话失败: {e}");
        return Json(ReturnData::err("系统错误"));
    }
    user.token = token;
    user.last_login_at = now;
    tracing::info!("用户登录: {username}");
    Json(ReturnData::ok(format_user(&user)))
}

async fn register(
    state: &AppState,
    username: &str,
    password: &str,
    code: Option<String>,
) -> Json<ReturnData> {
    let config = &state.storage.config;

    if username.len() < 5 {
        return Json(ReturnData::err("用户名不能低于5位"));
    }
    if (password.len() as i64) < config.min_user_password_length {
        return Json(ReturnData::err(format!(
            "密码不能低于{}位",
            config.min_user_password_length
        )));
    }
    if username == "default" {
        return Json(ReturnData::err("用户名不能为非法字符"));
    }
    if !is_valid_username(username) {
        return Json(ReturnData::err("用户名只能由字母和数字组成"));
    }

    if !config.invite_code.is_empty() {
        let code = code.unwrap_or_default();
        if code.is_empty() {
            return Json(ReturnData::err("请输入邀请码"));
        }
        if code != config.invite_code {
            return Json(ReturnData::err("邀请码错误"));
        }
    }

    let count = match state.storage.count_users().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("统计用户数失败: {e}");
            return Json(ReturnData::err("系统错误"));
        }
    };
    let user_limit = config.user_limit.max(1);
    if count >= user_limit {
        return Json(ReturnData::err("超过用户数上限"));
    }

    let salt = random_salt();
    let now = now_millis();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let user = User {
        username: username.to_string(),
        password: crate::util::password::hash_password(password),
        salt,
        token,
        token_map: None,
        is_admin: count == 0,
        disabled: false,
        enable_webdav: config.default_user_enable_webdav,
        enable_local_store: config.default_user_enable_local_store,
        enable_book_source: config.default_user_enable_book_source,
        enable_rss_source: config.default_user_enable_rss_source,
        book_source_limit: config.default_user_book_source_limit,
        book_limit: config.default_user_book_limit,
        last_login_at: now,
        created_at: now,
        user_namespace: username.to_string(),
        raw_json: None,
    };
    if let Err(e) = state.storage.insert_user(&user).await {
        tracing::error!("创建用户 {username} 失败: {e}");
        return Json(ReturnData::err("系统错误"));
    }
    tracing::info!("新用户注册: {username}");
    Json(ReturnData::ok(format_user(&user)))
}

/// POST `/reader3/logout`：退出当前设备登录
pub async fn logout(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    _body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    if !state.storage.config.secure {
        return Json(ReturnData::err("不支持的操作"));
    }
    let user = match resolve_current_user(&state, &params, &headers).await {
        Ok(u) => u,
        Err(ret) => return Json(ret),
    };
    let username = user.username;
    let token = access_token_of(&params, &headers)
        .and_then(|t| t.split_once(':').map(|(_, tk)| tk.to_string()))
        .unwrap_or_default();
    let result = if token.is_empty() {
        state.storage.logout_user(&username).await
    } else {
        state.storage.remove_user_token(&username, &token).await
    };
    match result {
        Ok(_) => {
            tracing::info!("用户退出登录: {username}");
            Json(ReturnData::ok(Value::Null))
        }
        Err(e) => {
            tracing::error!("logout 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FontAssetEntry {
    name: String,
    size: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInfoPayload {
    user_info: Option<UserView>,
    secure: bool,
    secure_key: bool,
    fonts: Vec<FontAssetEntry>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemInfoPayload<'a> {
    version: &'static str,
    timestamp: i64,
    secure: bool,
    mode: &'a str,
    is_admin: bool,
    invite_required: bool,
    user_limit: i64,
    uptime_seconds: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UserView {
    username: String,
    last_login_at: i64,
    access_token: String,
    enable_webdav: bool,
    enable_local_store: bool,
    enable_book_source: bool,
    enable_rss_source: bool,
    book_source_limit: i64,
    book_limit: i64,
    is_admin: bool,
    created_at: i64,
}

impl UserView {
    fn from_user(user: &User) -> Self {
        Self {
            username: user.username.clone(),
            last_login_at: user.last_login_at,
            access_token: format!("{}:{}", user.username, user.token),
            enable_webdav: user.enable_webdav,
            enable_local_store: user.enable_local_store,
            enable_book_source: user.enable_book_source,
            enable_rss_source: user.enable_rss_source,
            book_source_limit: user.book_source_limit,
            book_limit: user.book_limit,
            is_admin: user.is_admin,
            created_at: user.created_at,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminUserItem<'a> {
    book_source_count: i64,
    book_count: i64,
    username: &'a str,
    enable_webdav: bool,
    enable_local_store: bool,
    enable_book_source: bool,
    enable_rss_source: bool,
    book_source_limit: i64,
    book_limit: i64,
    is_admin: bool,
    disabled: bool,
    last_login_at: i64,
    created_at: i64,
}

/// GET `/reader3/getUserInfo`：当前用户信息 + secure 开关 + 系统内置字体清单
pub async fn get_user_info(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let user = resolve_current_user(&state, &params, &headers).await.ok();
    let user_info = user.as_ref().map(UserView::from_user);
    let fonts_dir = state
        .storage
        .config
        .storage_dir()
        .join("assets")
        .join("fonts");
    let mut fonts: Vec<FontAssetEntry> = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&fonts_dir).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            let p = e.path();
            let meta = match e.metadata().await {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let is_ttf = p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("ttf"))
                .unwrap_or(false);
            if !is_ttf {
                continue;
            }
            fonts.push(FontAssetEntry {
                name: p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string(),
                size: meta.len(),
            });
        }
    }
    ReturnData::ok_typed(UserInfoPayload {
        user_info,
        secure: state.storage.config.secure,
        secure_key: false,
        fonts,
    })
    .into_response()
}

/// GET `/reader3/getSystemInfo`：精简系统信息（含 `/epub/*` Cookie 下发，不含内存/CPU 采样）
pub async fn get_system_info(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let config = &state.storage.config;
    let current_user = resolve_current_user(&state, &params, &headers).await.ok();
    let cookie = system_info_cookie(&state, current_user.as_ref());
    let is_admin = !config.secure || current_user.as_ref().map(|u| u.is_admin).unwrap_or(false);
    let data = SystemInfoPayload {
        version: env!("CARGO_PKG_VERSION"),
        timestamp: now_millis(),
        secure: config.secure,
        mode: &config.mode,
        is_admin,
        invite_required: !config.invite_code.is_empty(),
        user_limit: config.user_limit,
        uptime_seconds: PROCESS_START.elapsed().as_secs(),
    };
    system_info_response(data, cookie)
}

fn system_info_cookie(state: &AppState, user: Option<&User>) -> Option<String> {
    if !state.storage.config.secure {
        return None;
    }
    let user = user?;
    let ttl_secs = state.storage.config.token_ttl_days.max(0) * 86_400;
    let max_age = if ttl_secs > 0 {
        format!("; Max-Age={ttl_secs}")
    } else {
        String::new()
    };
    Some(format!(
        "reader_token={}:{}; Path=/; SameSite=Lax; HttpOnly{max_age}",
        user.username, user.token
    ))
}

fn system_info_response<T: serde::Serialize>(data: T, cookie: Option<String>) -> Response {
    let mut resp = ReturnData::ok_typed(data).into_response();
    if let Some(value) = cookie {
        if let Ok(hv) = HeaderValue::from_str(&value) {
            resp.headers_mut().insert(header::SET_COOKIE, hv);
        }
    }
    resp
}

/// GET/POST `/reader3/getUsers`：管理员获取用户列表
pub async fn get_users(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    if let Err(ret) = resolve_namespace(&state, &params, &headers).await {
        return Json(ret).into_response();
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Err(ret) = check_manager_auth(&state, &params, &headers, body_json.as_ref()).await {
        return Json(ret).into_response();
    }
    match state.storage.list_users().await {
        Ok(users) => {
            let (source_counts, book_counts) = namespace_counts(&state.storage).await;
            let arr: Vec<AdminUserItem<'_>> = users
                .iter()
                .map(|u| AdminUserItem {
                    book_source_count: source_counts.get(&u.username).copied().unwrap_or(0),
                    book_count: book_counts.get(&u.username).copied().unwrap_or(0),
                    username: &u.username,
                    enable_webdav: u.enable_webdav,
                    enable_local_store: u.enable_local_store,
                    enable_book_source: u.enable_book_source,
                    enable_rss_source: u.enable_rss_source,
                    book_source_limit: u.book_source_limit,
                    book_limit: u.book_limit,
                    is_admin: u.is_admin,
                    disabled: u.disabled,
                    last_login_at: u.last_login_at,
                    created_at: u.created_at,
                })
                .collect();
            ReturnData::ok_typed(arr).into_response()
        }
        Err(e) => {
            tracing::error!("getUsers 失败: {e}");
            Json(ReturnData::err("系统错误")).into_response()
        }
    }
}

/// POST `/reader3/addUser`：管理员创建用户
pub async fn add_user(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    if let Err(ret) = resolve_namespace(&state, &params, &headers).await {
        return Json(ret);
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Err(ret) = check_manager_auth(&state, &params, &headers, body_json.as_ref()).await {
        return Json(ret);
    }
    let username = param_of(&params, body_json.as_ref(), "username");
    let password = param_of(&params, body_json.as_ref(), "password");
    if username.is_empty() || password.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let config = &state.storage.config;
    if username.len() < 5 {
        return Json(ReturnData::err("用户名不能低于5位"));
    }
    if (password.len() as i64) < config.min_user_password_length {
        return Json(ReturnData::err(format!(
            "密码不能低于{}位",
            config.min_user_password_length
        )));
    }
    if username == "default" {
        return Json(ReturnData::err("用户名不能为非法字符"));
    }
    if !is_valid_username(&username) {
        return Json(ReturnData::err("用户名只能由字母和数字组成"));
    }
    if state
        .storage
        .find_user(&username)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Json(ReturnData::err("用户名已被占用"));
    }
    let count = match state.storage.count_users().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("统计用户数失败: {e}");
            return Json(ReturnData::err("系统错误"));
        }
    };
    if count >= config.user_limit.max(1) {
        return Json(ReturnData::err("超过用户数上限"));
    }
    let bool_param = |key: &str, default: bool| -> bool {
        if let Some(b) = body_json.as_ref().and_then(|b| b.get(key)) {
            return b.as_bool().unwrap_or(default);
        }
        params
            .get(key)
            .map(|v| v == "true" || v == "1")
            .unwrap_or(default)
    };
    let int_param = |key: &str, default: i64| -> i64 {
        if let Some(v) = body_json.as_ref().and_then(|b| b.get(key)) {
            return v.as_i64().unwrap_or(default);
        }
        params
            .get(key)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(default)
    };
    let salt = random_salt();
    let now = now_millis();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let user = User {
        username: username.clone(),
        password: crate::util::password::hash_password(&password),
        salt,
        token,
        token_map: None,
        is_admin: bool_param("isAdmin", false),
        disabled: false,
        enable_webdav: bool_param("enableWebdav", config.default_user_enable_webdav),
        enable_local_store: bool_param("enableLocalStore", config.default_user_enable_local_store),
        enable_book_source: bool_param("enableBookSource", config.default_user_enable_book_source),
        enable_rss_source: bool_param("enableRssSource", config.default_user_enable_rss_source),
        book_source_limit: int_param("bookSourceLimit", config.default_user_book_source_limit),
        book_limit: int_param("bookLimit", config.default_user_book_limit),
        last_login_at: now,
        created_at: now,
        user_namespace: username.clone(),
        raw_json: None,
    };
    if let Err(e) = state.storage.insert_user(&user).await {
        tracing::error!("addUser 创建用户 {username} 失败: {e}");
        return Json(ReturnData::err("系统错误"));
    }
    tracing::info!("管理员创建用户: {username}");
    Json(ReturnData::ok(format_user(&user)))
}

/// POST `/reader3/updateUser`：更新用户权限与限额
pub async fn update_user(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    if let Err(ret) = resolve_namespace(&state, &params, &headers).await {
        return Json(ret);
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Err(ret) = check_manager_auth(&state, &params, &headers, body_json.as_ref()).await {
        return Json(ret);
    }
    let username = param_of(&params, body_json.as_ref(), "username");
    if username.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let bool_param = |key: &str| -> Option<bool> {
        if let Some(b) = body_json.as_ref().and_then(|b| b.get(key)) {
            return b.as_bool();
        }
        params.get(key).map(|v| v == "true" || v == "1")
    };
    let int_param = |key: &str| -> Option<i64> {
        if let Some(b) = body_json.as_ref().and_then(|b| b.get(key)) {
            return b.as_i64();
        }
        params.get(key).and_then(|v| v.parse::<i64>().ok())
    };
    if bool_param("isAdmin") == Some(false) || bool_param("disabled") == Some(true) {
        if let Ok(Some(target)) = state.storage.find_user(&username).await {
            if target.is_admin && state.storage.count_admins().await.unwrap_or(1) <= 1 {
                return Json(ReturnData::err(if bool_param("disabled") == Some(true) {
                    "不能停用最后一名管理员"
                } else {
                    "不能撤销最后一名管理员"
                }));
            }
        }
    }
    match state
        .storage
        .update_user_permissions(
            &username,
            bool_param("enableWebdav"),
            bool_param("enableLocalStore"),
            bool_param("enableBookSource"),
            bool_param("enableRssSource"),
            int_param("bookSourceLimit"),
            int_param("bookLimit"),
            bool_param("isAdmin"),
            bool_param("disabled"),
        )
        .await
    {
        Ok(0) => Json(ReturnData::err("用户不存在")),
        Ok(_) => Json(ReturnData::ok(Value::Null)),
        Err(e) => {
            tracing::error!("updateUser [{username}] 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

/// POST `/reader3/deleteUser`：管理员删除单个用户（不能删除自己或最后一名管理员）
pub async fn delete_user(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let current_user = match resolve_current_user(&state, &params, &headers).await {
        Ok(u) => u,
        Err(ret) => return Json(ret),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Err(ret) = check_manager_auth(&state, &params, &headers, body_json.as_ref()).await {
        return Json(ret);
    }
    let username = param_of(&params, body_json.as_ref(), "username");
    if username.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    if username == current_user.username {
        return Json(ReturnData::err("不能删除自己"));
    }
    if let Ok(Some(target)) = state.storage.find_user(&username).await {
        if target.is_admin && state.storage.count_admins().await.unwrap_or(1) <= 1 {
            return Json(ReturnData::err("不能删除最后一名管理员"));
        }
    }
    match state.storage.delete_user(&username).await {
        Ok(0) => Json(ReturnData::err("用户不存在")),
        Ok(_) => {
            tracing::info!("deleteUser：删除用户 {username}");
            Json(ReturnData::ok(Value::Null))
        }
        Err(e) => {
            tracing::error!("deleteUser [{username}] 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

/// POST `/reader3/resetUserPassword`：重置用户密码
pub async fn reset_user_password(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    if let Err(ret) = resolve_namespace(&state, &params, &headers).await {
        return Json(ret);
    }
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Err(ret) = check_manager_auth(&state, &params, &headers, body_json.as_ref()).await {
        return Json(ret);
    }
    let username = param_of(&params, body_json.as_ref(), "username");
    let mut password = param_of(&params, body_json.as_ref(), "password");
    if password.is_empty() {
        password = param_of(&params, body_json.as_ref(), "newPassword");
    }
    if username.is_empty() || password.is_empty() {
        return Json(ReturnData::err("参数错误"));
    }
    let salt = random_salt();
    let encrypted = crate::util::password::hash_password(&password);
    match state
        .storage
        .reset_user_password(&username, &salt, &encrypted)
        .await
    {
        Ok(0) => Json(ReturnData::err("用户不存在")),
        Ok(_) => {
            tracing::info!("resetUserPassword：重置用户 {username} 密码");
            Json(ReturnData::ok(Value::Null))
        }
        Err(e) => {
            tracing::error!("resetUserPassword [{username}] 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

fn json_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

fn int_param_of(
    params: &HashMap<String, String>,
    body: Option<&Value>,
    keys: &[&str],
) -> Option<i64> {
    if let Some(b) = body {
        for k in keys {
            if let Some(v) = b.get(*k).and_then(json_i64) {
                return Some(v);
            }
        }
    }
    for k in keys {
        if let Some(v) = params.get(*k).and_then(|s| s.trim().parse::<i64>().ok()) {
            return Some(v);
        }
    }
    None
}

/// POST `/reader3/clearInactiveUsers`：清理不活跃用户（JSON Body 优先，Query 兜底）
pub async fn clear_inactive_users(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Json<ReturnData> {
    let config = &state.storage.config;
    let user = match resolve_current_user(&state, &params, &headers).await {
        Ok(u) => u,
        Err(ret) => return Json(ret),
    };
    let username = user.username;
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if config.secure && !user.is_admin {
        return Json(ReturnData::err("仅管理员可执行该操作"));
    }
    let inactive_day =
        int_param_of(&params, body_json.as_ref(), &["inactiveDay", "days", "day"]).unwrap_or(0);
    let before = now_millis().saturating_sub(inactive_day.saturating_mul(86_400 * 1000));
    match state
        .storage
        .clear_inactive_users(before, Some(&username))
        .await
    {
        Ok(deleted) => {
            tracing::info!(
                "clearInactiveUsers：删除 {} 个不活跃用户: {deleted:?}",
                deleted.len()
            );
            Json(ReturnData::ok(json!({
                "deleted": deleted,
                "count": deleted.len(),
            })))
        }
        Err(e) => {
            tracing::error!("clearInactiveUsers 失败: {e}");
            Json(ReturnData::err("系统错误"))
        }
    }
}

// ---------------- 公共鉴权与权限辅助 ----------------

pub(crate) fn login_required() -> ReturnData {
    ReturnData {
        is_success: false,
        error_msg: "请登录后使用".to_string(),
        data: json!("NEED_LOGIN"),
    }
}

/// 解析命名空间：单用户模式返回 `"default"`，多用户模式校验 `accessToken`
pub(crate) async fn resolve_namespace(
    state: &AppState,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Result<String, ReturnData> {
    if !state.storage.config.secure {
        return Ok("default".to_string());
    }
    let user = resolve_current_user(state, params, headers).await?;
    if user.is_admin && params.get("ns").map(|s| s == "default").unwrap_or(false) {
        return Ok("default".to_string());
    }
    Ok(user.username)
}

/// 解析当前登录用户（secure 模式校验主 token 或多设备 `token_map` + TTL）
pub(crate) async fn resolve_current_user(
    state: &AppState,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Result<User, ReturnData> {
    if !state.storage.config.secure {
        return Err(login_required());
    }
    let Some(access_token) = access_token_of(params, headers) else {
        return Err(login_required());
    };
    let Some((username, token)) = access_token.split_once(':') else {
        return Err(login_required());
    };
    if username.is_empty() || token.is_empty() {
        return Err(login_required());
    }
    match state.storage.find_user(username).await {
        Ok(Some(user)) => {
            let token_ok = (!user.token.is_empty()
                && crate::util::constant_time::ct_eq(&user.token, token))
                || crate::model::user::token_map_valid(&user.token_map, token, now_millis());
            if !token_ok {
                return Err(login_required());
            }
            if user.disabled {
                return Err(ReturnData::err("账号已停用, 请联系管理员"));
            }
            let ttl_days = state.storage.config.token_ttl_days;
            if ttl_days > 0 && now_millis() - user.last_login_at > ttl_days * 86_400_000 {
                return Err(login_required());
            }
            Ok(user)
        }
        _ => Err(login_required()),
    }
}

/// 管理校验：单用户模式放行；多用户模式需已登录管理员
pub(crate) async fn check_manager_auth(
    state: &AppState,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
    _body: Option<&Value>,
) -> Result<(), ReturnData> {
    if !state.storage.config.secure {
        return Ok(());
    }
    let user = resolve_current_user(state, params, headers).await?;
    if !user.is_admin {
        return Err(ReturnData::err("仅管理员可执行该操作"));
    }
    Ok(())
}

/// secure 模式书源权限校验
pub(crate) async fn require_book_source_permission(
    state: &AppState,
    ns: &str,
) -> Result<(), ReturnData> {
    if !state.storage.config.secure || ns == "default" {
        return Ok(());
    }
    match state.storage.find_user(ns).await {
        Ok(Some(u)) if u.enable_book_source => Ok(()),
        Ok(Some(_)) => Err(ReturnData::err("书源功能未开启")),
        _ => Err(login_required()),
    }
}

fn access_token_of(params: &HashMap<String, String>, headers: &HeaderMap) -> Option<String> {
    params
        .get("accessToken")
        .cloned()
        .or_else(|| {
            headers
                .get("accessToken")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_string())
        })
}

pub(crate) fn format_user(user: &User) -> Value {
    json!({
        "username": user.username,
        "lastLoginAt": user.last_login_at,
        "accessToken": format!("{}:{}", user.username, user.token),
        "enableWebdav": user.enable_webdav,
        "enableLocalStore": user.enable_local_store,
        "enableBookSource": user.enable_book_source,
        "enableRssSource": user.enable_rss_source,
        "bookSourceLimit": user.book_source_limit,
        "bookLimit": user.book_limit,
        "isAdmin": user.is_admin,
        "createdAt": user.created_at,
    })
}

async fn namespace_counts(
    storage: &crate::storage::Storage,
) -> (HashMap<String, i64>, HashMap<String, i64>) {
    let source_rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT user_namespace, COUNT(*) FROM book_sources GROUP BY user_namespace")
            .fetch_all(&storage.pool)
            .await
            .unwrap_or_default();
    let book_rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT user_namespace, COUNT(*) FROM books GROUP BY user_namespace")
            .fetch_all(&storage.pool)
            .await
            .unwrap_or_default();
    (
        source_rows.into_iter().collect(),
        book_rows.into_iter().collect(),
    )
}

fn is_valid_username(username: &str) -> bool {
    !username.is_empty() && username.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn random_salt() -> String {
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(8)
        .map(char::from)
        .collect()
}

// ---------------- 客户端 IP 解析（可信代理白名单） ----------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum TrustedNet {
    Ip(IpAddr),
    Cidr { net: IpAddr, prefix: u8 },
}

impl TrustedNet {
    fn matches(&self, ip: IpAddr) -> bool {
        match *self {
            TrustedNet::Ip(net) => net == ip,
            TrustedNet::Cidr { net, prefix } => match (ip, net) {
                (IpAddr::V4(a), IpAddr::V4(b)) => {
                    let p = prefix.min(32);
                    p == 0
                        || (u32::from(a) & (u32::MAX << (32 - p)))
                            == (u32::from(b) & (u32::MAX << (32 - p)))
                }
                (IpAddr::V6(a), IpAddr::V6(b)) => {
                    let p = prefix.min(128);
                    p == 0
                        || (u128::from(a) & (u128::MAX << (128 - p)))
                            == (u128::from(b) & (u128::MAX << (128 - p)))
                }
                _ => false,
            },
        }
    }
}

fn parse_trusted_proxies(raw: &str) -> Vec<TrustedNet> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match s.split_once('/') {
            Some((ip, prefix)) => {
                let net = ip.parse::<IpAddr>().ok()?;
                let prefix = prefix.parse::<u8>().ok()?;
                Some(TrustedNet::Cidr { net, prefix })
            }
            None => s.parse::<IpAddr>().ok().map(TrustedNet::Ip),
        })
        .collect()
}

static TRUSTED_PROXIES: LazyLock<Vec<TrustedNet>> = LazyLock::new(|| {
    parse_trusted_proxies(&std::env::var("READER_TRUSTED_PROXIES").unwrap_or_default())
});

pub(crate) fn client_ip(peer: &SocketAddr, headers: &HeaderMap) -> String {
    client_ip_with(peer, headers, &TRUSTED_PROXIES)
}

fn client_ip_with(peer: &SocketAddr, headers: &HeaderMap, proxies: &[TrustedNet]) -> String {
    let direct = peer.ip().to_string();
    if !proxies.iter().any(|p| p.matches(peer.ip())) {
        return direct;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.parse::<IpAddr>().is_ok())
        .unwrap_or(&direct)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ip_ignores_untrusted_xff_and_trusts_whitelisted_cidr() {
        let peer: SocketAddr = "10.0.0.2:12345".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.5"));

        assert_eq!(client_ip_with(&peer, &headers, &[]), "10.0.0.2");
        let trusted = parse_trusted_proxies("10.0.0.0/24");
        assert_eq!(client_ip_with(&peer, &headers, &trusted), "203.0.113.5");
    }

    #[test]
    fn username_validation_accepts_alphanumeric_only() {
        assert!(is_valid_username("alice01"));
        assert!(!is_valid_username("alice_01"));
        assert!(!is_valid_username(""));
    }
}
