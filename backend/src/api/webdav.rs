//! WebDAV 服务（/reader3/webdav*，对齐 legacy WebdavController）
//!
//! 根目录：storage/data/{user}/webdav（secure 模式按 Basic 认证用户；非 secure 用 default）
//! 支持：OPTIONS / PROPFIND / GET / PUT / MKCOL / DELETE / MOVE / COPY / LOCK / UNLOCK

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;

use crate::storage::Storage;

/// WebDAV 处理入口（匹配 /reader3/webdav* 任意方法）
///
/// `client_ip`：客户端 IP（router 层解析——直连优先，可信代理白名单内才信 XFF），
/// 用于 P1-1 登录限流键（用户名+IP）。
pub async fn handle(
    storage: &Storage,
    method: Method,
    path: &str,
    headers: &HeaderMap,
    body: axum::body::Bytes,
    client_ip: &str,
) -> Response {
    // 1. OPTIONS 预检（不校验认证——CORS/客户端预检，legacy 修复点）
    if method == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::OK)
            .header("DAV", "1,2")
            .header(
                "Allow",
                "OPTIONS,GET,HEAD,PUT,DELETE,PROPFIND,MKCOL,MOVE,COPY,LOCK,UNLOCK",
            )
            // MS-Author-Via：告知 Office 等客户端本服务支持 DAV 写入语义
            .header("MS-Author-Via", "DAV")
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::empty())
            .unwrap();
    }

    // 2. Basic 认证（P1-1：密码校验接入登录限流）
    let Some((_username, _ns, home)) = authenticate(storage, headers, client_ip).await else {
        return webdav_status(
            StatusCode::UNAUTHORIZED,
            Some(("WWW-Authenticate", "Basic realm=\"reader\"")),
        );
    };

    // 3. 路径解析（webdav 根目录下）
    let Some(file) = resolve_path(&home, path) else {
        return webdav_status(StatusCode::BAD_REQUEST, None);
    };

    match method.as_str() {
        "PROPFIND" => propfind(&file, path, headers).await,
        "GET" | "HEAD" => get_file(&file).await,
        "PUT" => put_file(&file, body).await,
        "MKCOL" => mkcol(&file).await,
        "DELETE" => delete(&file).await,
        "MOVE" => move_copy(&file, &home, headers, false).await,
        "COPY" => move_copy(&file, &home, headers, true).await,
        "LOCK" => lock(headers),
        "UNLOCK" => webdav_status(StatusCode::NO_CONTENT, None),
        _ => webdav_status(StatusCode::METHOD_NOT_ALLOWED, None),
    }
}

/// Basic 认证 → (username, user_namespace, user_home)
/// P1-1：接入登录限流（用户名+IP，与 /reader3/login 同表）——
/// 锁定中拒绝；密码错误/账号不存在计入失败；成功清零。
pub(crate) async fn authenticate(
    storage: &Storage,
    headers: &HeaderMap,
    client_ip: &str,
) -> Option<(String, String, PathBuf)> {
    if !storage.config.secure {
        let home = storage
            .config
            .storage_dir()
            .join("data")
            .join("default")
            .join("webdav");
        return Some(("default".into(), "default".into(), home));
    }
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())?
        .strip_prefix("Basic ")?;
    let decoded = String::from_utf8(base64_decode(auth)).ok()?;
    let (username, password) = decoded.split_once(':')?;
    // P1-1：锁定中直接拒绝（不泄露锁定状态，统一 401）
    if crate::util::login_limit::check_allowed(username, client_ip).is_err() {
        return None;
    }
    let Some(user) = storage.find_user(username).await.ok().flatten() else {
        // 账号不存在 → 计入失败（与 /reader3/login 一致）
        crate::util::login_limit::record_failure(username, client_ip);
        return None;
    };
    if !user.enable_webdav {
        // 未开启 WebDAV 权限：账号有效但被拒——不累计密码失败，避免误锁
        return None;
    }
    if user.disabled {
        // S6：停用账号即使开了 WebDAV 也不能登录
        return None;
    }
    // 统一密码校验：argon2id（PHC）优先，legacy 双 MD5 兼容；MD5 通过时自动升级为 argon2id
    if !crate::util::password::verify_password(storage, &user, password).await {
        crate::util::login_limit::record_failure(username, client_ip);
        return None;
    }
    crate::util::login_limit::reset(username, client_ip);
    let home = storage
        .config
        .storage_dir()
        .join("data")
        .join(username)
        .join("webdav");
    Some((username.to_string(), username.to_string(), home))
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .unwrap_or_default()
}

/// 路径解析（P0-6：组件级归一化防穿越——复用 files::resolve_secure_path：`..` 逐级
/// 弹出、越出 webdav 根即拒绝；绝对路径按相对处理；符号链接逃逸被 canonicalize 拦截）
fn resolve_path(home: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path);
    // 去掉 /reader3/webdav 前缀
    let rel = decoded
        .trim_start_matches("/reader3/webdav")
        .trim_start_matches('/');
    crate::api::files::resolve_secure_path(home, rel)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn webdav_status(code: StatusCode, header: Option<(&str, &str)>) -> Response {
    let mut builder = Response::builder().status(code);
    if let Some((k, v)) = header {
        builder = builder.header(k, v);
    }
    builder.body(Body::empty()).unwrap()
}

/// XML 特殊字符转义（& < > "）——文件名直接拼入 XML 时防注入/打瘫 PROPFIND
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// href 完整 percent-encoding：非 [A-Za-z0-9/._~-] 字节一律 %XX
/// （修复裸中文 UTF-8、`#`/`%`/`?` 截断；'/' 保留为路径分隔符）
fn url_encode_path(s: &str) -> String {
    const SAFE: &[u8] = b"/._~-";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || SAFE.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// RFC 4918 getlastmodified：HTTP-date（RFC 1123，如 `Sun, 23 Aug 2026 04:05:06 GMT`）
fn http_date(t: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    // chrono strftime 与系统 locale 无关，%a/%b 恒为英文缩写
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// PROPFIND：XML 列表（对齐 legacy 语义）
/// Depth 头（RFC 4918 §9.1）：0 仅自身；1 含一级子项；缺省按 1
async fn propfind(file: &Path, request_path: &str, headers: &HeaderMap) -> Response {
    if !file.exists() {
        return webdav_status(StatusCode::NOT_FOUND, None);
    }
    let depth_one = !headers
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == "0")
        .unwrap_or(false);
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n",
    );
    // href 基准：解码后的请求路径重新完整编码（客户端可能裸发 UTF-8 或已编码——归一化）
    let base = url_encode_path(&percent_decode(request_path.trim_end_matches('/')));

    // 自身
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    xml.push_str(&entry_xml(&base, &name, file));
    // 子项（Depth:1 仅一级）
    if depth_one && file.is_dir() {
        if let Ok(entries) = std::fs::read_dir(file) {
            for e in entries.flatten() {
                let child_name = e.file_name().to_string_lossy().into_owned();
                let child_path = e.path();
                let child_url = format!("{base}/{}", url_encode_path(&child_name));
                xml.push_str(&entry_xml(&child_url, &child_name, &child_path));
            }
        }
    }
    xml.push_str("</D:multistatus>");
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(Body::from(xml))
        .unwrap()
}

fn entry_xml(url: &str, name: &str, file: &Path) -> String {
    let modified = file
        .metadata()
        .and_then(|m| m.modified())
        .map(http_date)
        .unwrap_or_default();
    let display = xml_escape(name);
    let href = xml_escape(url);
    if file.is_dir() {
        format!(
            "<D:response><D:href>{}</D:href><D:propstat><D:status>HTTP/1.1 200 OK</D:status><D:prop><D:getlastmodified>{}</D:getlastmodified><D:creationdate>{}</D:creationdate><D:resourcetype><D:collection/></D:resourcetype><D:displayname>{}</D:displayname></D:prop></D:propstat></D:response>\n",
            href, modified, modified, display
        )
    } else {
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        format!(
            "<D:response><D:href>{}</D:href><D:propstat><D:status>HTTP/1.1 200 OK</D:status><D:prop><D:getlastmodified>{}</D:getlastmodified><D:creationdate>{}</D:creationdate><D:resourcetype/><D:displayname>{}</D:displayname><D:getcontentlength>{}</D:getcontentlength></D:prop></D:propstat></D:response>\n",
            href, modified, modified, display, len
        )
    }
}

async fn get_file(file: &Path) -> Response {
    if !file.exists() || !file.is_file() {
        return webdav_status(StatusCode::NOT_FOUND, None);
    }
    match tokio::fs::read(file).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
    }
}

async fn put_file(file: &Path, body: axum::body::Bytes) -> Response {
    let Some(parent) = file.parent() else {
        return webdav_status(StatusCode::CONFLICT, None);
    };
    if !parent.exists() {
        return webdav_status(StatusCode::CONFLICT, None);
    }
    let existed = file.exists();
    match tokio::fs::write(file, &body).await {
        // RFC 4918 §9.7.1：覆盖已存在资源 → 204；新建 → 201
        Ok(_) => webdav_status(
            if existed {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::CREATED
            },
            None,
        ),
        Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
    }
}

async fn mkcol(file: &Path) -> Response {
    if file.exists() {
        return webdav_status(StatusCode::METHOD_NOT_ALLOWED, None);
    }
    // RFC 4918 §9.3：MKCOL 只创建最末一层集合，父集合不存在 → 409 Conflict
    // （不再 create_dir_all 隐式补全中间层）
    match file.parent() {
        Some(p) if p.exists() => {}
        _ => return webdav_status(StatusCode::CONFLICT, None),
    }
    match tokio::fs::create_dir(file).await {
        Ok(_) => webdav_status(StatusCode::CREATED, None),
        Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
    }
}

async fn delete(file: &Path) -> Response {
    if !file.exists() {
        return webdav_status(StatusCode::NOT_FOUND, None);
    }
    if file.is_dir() {
        match tokio::fs::remove_dir_all(file).await {
            Ok(_) => webdav_status(StatusCode::OK, None),
            Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
        }
    } else {
        match tokio::fs::remove_file(file).await {
            Ok(_) => webdav_status(StatusCode::OK, None),
            Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
        }
    }
}

/// MOVE/COPY（Destination 头）——目标同样经 resolve_secure_path 组件级校验（P0-6）
async fn move_copy(file: &Path, home: &Path, headers: &HeaderMap, copy: bool) -> Response {
    let Some(dest) = headers.get("destination").and_then(|v| v.to_str().ok()) else {
        return webdav_status(StatusCode::BAD_REQUEST, None);
    };
    let dest_path = percent_decode(dest.split('?').next().unwrap_or(dest));
    // Destination 是完整 URL（http://host/reader3/webdav/xxx）——取路径部分
    let dest_path = dest_path
        .split("://")
        .nth(1)
        .and_then(|s| s.split_once('/').map(|(_, p)| format!("/{p}")))
        .unwrap_or(dest_path.clone());
    let rel = dest_path
        .trim_start_matches("/reader3/webdav")
        .trim_start_matches('/');
    // 安全校验：目标必须在 webdav 根内（组件级归一化——防 .. 穿越任意写入）
    let Some(target) = crate::api::files::resolve_secure_path(home, rel) else {
        return webdav_status(StatusCode::FORBIDDEN, None);
    };
    if !file.exists() {
        return webdav_status(StatusCode::NOT_FOUND, None);
    }
    // Overwrite 头（RFC 4918 §10.6）：默认 T；`F` 且目标已存在 → 412 Precondition Failed
    let overwrite = headers
        .get("overwrite")
        .and_then(|v| v.to_str().ok())
        .map(|v| !v.trim().eq_ignore_ascii_case("f"))
        .unwrap_or(true);
    let dest_existed = target.exists();
    if !overwrite && dest_existed {
        return webdav_status(StatusCode::PRECONDITION_FAILED, None);
    }
    // 覆盖成功 → 204；新建 → 201
    let ok_status = |existed: bool| {
        if existed {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::CREATED
        }
    };
    if copy {
        match copy_recursive(file, &target) {
            Ok(_) => webdav_status(ok_status(dest_existed), None),
            Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
        }
    } else {
        match tokio::fs::rename(file, &target).await {
            Ok(_) => webdav_status(ok_status(dest_existed), None),
            Err(_) => webdav_status(StatusCode::INTERNAL_SERVER_ERROR, None),
        }
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// LOCK：返回 lock token（legacy 语义——不真正持锁）
fn lock(headers: &HeaderMap) -> Response {
    let _timeout = headers
        .get("timeout")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("Second-3600")
        .to_string();
    let lock_token = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock><D:locktype><write/></D:locktype><D:lockscope><exclusive/></D:lockscope><D:locktoken><D:href>{}</D:href></D:locktoken><D:depth>infinity</D:depth></D:activelock></D:lockdiscovery></D:prop>",
        lock_token
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("Lock-Token", lock_token)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P0-6：resolve_path 组件级归一化——正常放行、`..` 越出根拒绝（含百分号编码绕过）
    #[test]
    fn test_resolve_path_traversal() {
        let base = std::env::temp_dir().join("reader-webdav-resolve");
        let home = base.join("data/alice/webdav");
        std::fs::create_dir_all(&home).unwrap();
        let home_abs = home.canonicalize().unwrap();

        // 正常：/reader3/webdav 前缀剥离 + 相对解析
        let p = resolve_path(&home, "/reader3/webdav/a/b.txt").unwrap();
        assert_eq!(p, home_abs.join("a/b.txt"));
        let p = resolve_path(&home, "/reader3/webdav").unwrap();
        assert_eq!(p, home_abs);

        // 根内 .. 弹回放行（组件级归一化）
        let p = resolve_path(&home, "/reader3/webdav/a/../b.txt").unwrap();
        assert_eq!(p, home_abs.join("b.txt"));

        // 穿越拒绝：越出 webdav 根一律 None
        assert!(resolve_path(&home, "/reader3/webdav/../escape.txt").is_none());
        assert!(resolve_path(&home, "/reader3/webdav/a/../../escape.txt").is_none());
        assert!(resolve_path(&home, "/reader3/webdav/../../data/bob/secret.txt").is_none());
        // 百分号编码的 .. 同样拒绝（解码后再归一化）
        assert!(resolve_path(&home, "/reader3/webdav/..%2F..%2Fescape.txt").is_none());
        assert!(resolve_path(&home, "/reader3/webdav/a/..%2F..%2Fescape.txt").is_none());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// S6：停用账号即使密码正确也必须被拒（与 OPDS Basic 同口径）
    #[tokio::test]
    async fn test_authenticate_rejects_disabled_user() {
        let dir =
            std::env::temp_dir().join(format!("reader-webdav-disabled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        config.secure = true;
        let storage = crate::storage::init(&config).await.unwrap();
        storage
            .insert_user(&crate::model::User {
                username: "davdis".into(),
                password: crate::util::password::hash_password("pw123456"),
                enable_webdav: true,
                ..Default::default()
            })
            .await
            .unwrap();
        // insert_user 不带 disabled 列（停用是管理动作）→ 走权限更新
        storage
            .update_user_permissions(
                "davdis",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(true),
            )
            .await
            .unwrap();
        use base64::Engine;
        let mut h = axum::http::HeaderMap::new();
        let cred = base64::engine::general_purpose::STANDARD.encode("davdis:pw123456");
        h.insert(
            axum::http::header::AUTHORIZATION,
            format!("Basic {cred}").parse().unwrap(),
        );
        assert!(
            authenticate(&storage, &h, "203.0.113.78").await.is_none(),
            "停用账号不应通过 WebDAV 认证"
        );

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-1：WebDAV Basic 密码校验接入登录限流——
    /// 失败 5 次锁定（同 IP 即使密码正确也拒绝）、成功清零、异 IP 独立计数
    #[tokio::test]
    async fn test_authenticate_login_rate_limit() {
        let dir =
            std::env::temp_dir().join(format!("reader-webdav-ratelimit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        config.secure = true;
        let storage = crate::storage::init(&config).await.unwrap();

        // 系统用户（argon2id 密码）
        storage
            .insert_user(&crate::model::User {
                username: "davetest".into(),
                password: crate::util::password::hash_password("pw123456"),
                enable_webdav: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let basic = |u: &str, p: &str| {
            use base64::Engine;
            let mut h = axum::http::HeaderMap::new();
            let cred = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
            h.insert(
                axum::http::header::AUTHORIZATION,
                format!("Basic {cred}").parse().unwrap(),
            );
            h
        };
        let ip = "203.0.113.77";

        // 正确密码通过（并清零计数）
        assert!(authenticate(&storage, &basic("davetest", "pw123456"), ip)
            .await
            .is_some());
        // 连续 5 次错误密码 → 锁定
        for _ in 0..5 {
            assert!(authenticate(&storage, &basic("davetest", "wrong"), ip)
                .await
                .is_none());
        }
        // 锁定中：正确密码也被拒（统一 401）
        assert!(
            authenticate(&storage, &basic("davetest", "pw123456"), ip)
                .await
                .is_none(),
            "锁定中正确密码也应拒绝"
        );
        // 异 IP 不受影响（独立计数）
        assert!(
            authenticate(&storage, &basic("davetest", "pw123456"), "203.0.113.78")
                .await
                .is_some(),
            "异 IP 应正常通过"
        );
        // 账号不存在也计入失败（与 login 一致）——不存在的用户不锁定已存在用户
        assert!(authenticate(&storage, &basic("ghost", "x"), ip)
            .await
            .is_none());
        assert!(
            authenticate(&storage, &basic("davetest", "pw123456"), "203.0.113.79")
                .await
                .is_some(),
            "ghost 失败不应影响 davetest 其他 IP"
        );

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- RFC 4918 合规修复：单元测试 ----------

    /// P0-1：XML 特殊字符转义
    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("a&b<c>\"d\""), "a&amp;b&lt;c&gt;&quot;d&quot;");
        assert_eq!(xml_escape("普通名字.txt"), "普通名字.txt");
        // & 必须最先转义（避免二次编码）
        assert_eq!(xml_escape("&amp;"), "&amp;amp;");
    }

    /// P1-4：href 完整 percent-encoding（非 [A-Za-z0-9/._~-] 一律 %XX）
    #[test]
    fn test_url_encode_path() {
        assert_eq!(url_encode_path("a b.txt"), "a%20b.txt");
        assert_eq!(
            url_encode_path("中文 书.txt"),
            "%E4%B8%AD%E6%96%87%20%E4%B9%A6.txt"
        );
        assert_eq!(url_encode_path("a#b%c?.txt"), "a%23b%25c%3F.txt");
        assert_eq!(url_encode_path("/dir/sub/file.txt"), "/dir/sub/file.txt");
        assert_eq!(url_encode_path("safe-._~name"), "safe-._~name");
    }

    /// P0-3：getlastmodified 用 HTTP-date（RFC 1123）
    #[test]
    fn test_http_date_rfc1123() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_445_412_480);
        assert_eq!(http_date(t), "Wed, 21 Oct 2015 07:28:00 GMT");
    }

    // ---------- RFC 4918 合规修复：集成测试（non-secure 直连 handle） ----------

    struct DavFixture {
        config: crate::AppConfig,
        home: PathBuf,
    }

    async fn dav_fixture(tag: &str) -> DavFixture {
        let dir =
            std::env::temp_dir().join(format!("reader-webdav-rfc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        config.secure = false;
        let home = config.storage_dir().join("data/default/webdav");
        std::fs::create_dir_all(&home).unwrap();
        DavFixture { config, home }
    }

    async fn dav_call(
        storage: &Storage,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Response {
        let m = Method::from_bytes(method.as_bytes()).unwrap();
        let mut hm = HeaderMap::new();
        for (k, v) in headers {
            let name = axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap();
            hm.insert(name, v.parse().unwrap());
        }
        handle(storage, m, path, &hm, axum::body::Bytes::new(), "127.0.0.1").await
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// P0-1/P0-2/P0-3/P1-4/P1-6：PROPFIND——XML 转义、子项 displayname、
    /// HTTP-date、href percent-encoding、Depth 头
    #[tokio::test]
    async fn test_propfind_compliance() {
        let fx = dav_fixture("propfind").await;
        let storage = crate::storage::init(&fx.config).await.unwrap();

        // 特殊字符文件名（Windows 允许 & # 中文；< > 非法故用 & 覆盖转义路径）
        std::fs::write(fx.home.join("a&b #1.txt"), b"x").unwrap();
        std::fs::write(fx.home.join("中文 书.txt"), b"y").unwrap();
        std::fs::create_dir(fx.home.join("dir")).unwrap();

        // 默认（无 Depth 头）= Depth:1
        let resp = dav_call(&storage, "PROPFIND", "/reader3/webdav/", &[]).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let xml = body_string(resp).await;
        let entries = xml.matches("<D:response>").count();
        assert_eq!(entries, 4, "根 + 3 子项（默认 Depth:1）: {xml}");

        // P0-3：HTTP-date 格式（`Sun, 23 Aug 2026 04:05:06 GMT`，非 ISO8601）
        let lm = xml
            .split("<D:getlastmodified>")
            .nth(1)
            .and_then(|s| s.split("</D:getlastmodified>").next())
            .expect("应有 getlastmodified");
        let parts: Vec<&str> = lm.split(' ').collect();
        assert_eq!(parts.len(), 6, "getlastmodified={lm}");
        assert_eq!(parts[5], "GMT");
        assert_eq!(parts[1].len(), 2, "日两位补零: {lm}");
        assert!(parts[1].bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(parts[2].len(), 3);

        // P0-2：子项 displayname 非空且为真实文件名
        assert!(
            xml.contains("<D:displayname>a&amp;b #1.txt</D:displayname>"),
            "{xml}"
        );
        assert!(
            xml.contains("<D:displayname>中文 书.txt</D:displayname>"),
            "{xml}"
        );
        assert!(
            !xml.contains("<D:displayname></D:displayname>"),
            "不允许空 displayname"
        );

        // P0-1：XML 转义生效
        assert!(xml.contains("&amp;"), "应包含转义后的 &amp;");
        assert!(!xml.contains(">a&b"), "displayname 未转义: {xml}");

        // P1-4：href 完整 percent-encoding（& → %26，空格 → %20，中文 → UTF-8 %XX）
        assert!(
            xml.contains("<D:href>/reader3/webdav/a%26b%20%231.txt</D:href>"),
            "{xml}"
        );
        assert!(
            xml.contains("<D:href>/reader3/webdav/%E4%B8%AD%E6%96%87%20%E4%B9%A6.txt</D:href>"),
            "{xml}"
        );

        // Depth: 0 —— 仅自身一条
        let resp = dav_call(&storage, "PROPFIND", "/reader3/webdav/", &[("Depth", "0")]).await;
        let xml = body_string(resp).await;
        assert_eq!(
            xml.matches("<D:response>").count(),
            1,
            "Depth:0 仅自身: {xml}"
        );

        // Depth: 1 —— 显式等于默认
        let resp = dav_call(&storage, "PROPFIND", "/reader3/webdav/", &[("Depth", "1")]).await;
        let xml = body_string(resp).await;
        assert_eq!(xml.matches("<D:response>").count(), 4);

        // 不存在资源 → 404
        let resp = dav_call(&storage, "PROPFIND", "/reader3/webdav/nope", &[]).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&fx.config.work_dir);
    }

    /// P2-9 / P2-7 / P2-8：PUT 新建 201 覆盖 204；MKCOL 单层创建父缺失 409；
    /// OPTIONS Allow 含 HEAD 且带 MS-Author-Via
    #[tokio::test]
    async fn test_put_mkcol_options_compliance() {
        let fx = dav_fixture("putmkcol").await;
        let storage = crate::storage::init(&fx.config).await.unwrap();

        // PUT 新建 → 201
        let resp = dav_call(&storage, "PUT", "/reader3/webdav/new.txt", &[]).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(std::fs::read(fx.home.join("new.txt")).unwrap(), b"");
        // PUT 已存在 → 204
        let resp = dav_call(&storage, "PUT", "/reader3/webdav/new.txt", &[]).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // 父目录缺失 → PUT 409
        let resp = dav_call(&storage, "PUT", "/reader3/webdav/ghost/x.txt", &[]).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // MKCOL 直接子层 → 201
        let resp = dav_call(&storage, "MKCOL", "/reader3/webdav/coll", &[]).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(fx.home.join("coll").is_dir());
        // 已存在 → 405
        let resp = dav_call(&storage, "MKCOL", "/reader3/webdav/coll", &[]).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        // 单层语义：中间层不存在 → 409（不再 create_dir_all 隐式补全）
        let resp = dav_call(&storage, "MKCOL", "/reader3/webdav/no/such/deep", &[]).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(!fx.home.join("no").exists());

        // OPTIONS：Allow 含 HEAD；MS-Author-Via: DAV
        let resp = dav_call(&storage, "OPTIONS", "/reader3/webdav/", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let allow = resp
            .headers()
            .get("Allow")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            allow.split(',').any(|m| m.trim() == "HEAD"),
            "Allow={allow}"
        );
        assert_eq!(
            resp.headers()
                .get("MS-Author-Via")
                .and_then(|v| v.to_str().ok()),
            Some("DAV")
        );

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&fx.config.work_dir);
    }

    /// P1-5：Overwrite 头——`F` 且目标存在 → 412；覆盖成功 → 204；新建 → 201
    #[tokio::test]
    async fn test_move_copy_overwrite_header() {
        let fx = dav_fixture("overwrite").await;
        let storage = crate::storage::init(&fx.config).await.unwrap();

        std::fs::write(fx.home.join("src.txt"), b"new-content").unwrap();
        std::fs::write(fx.home.join("dst.txt"), b"old-content").unwrap();
        let dest = |name: &str| format!("http://127.0.0.1:1234/reader3/webdav/{name}");

        // COPY + Overwrite:F 且目标存在 → 412，目标不变
        let resp = dav_call(
            &storage,
            "COPY",
            "/reader3/webdav/src.txt",
            &[("Destination", &dest("dst.txt")), ("Overwrite", "F")],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(
            std::fs::read(fx.home.join("dst.txt")).unwrap(),
            b"old-content"
        );

        // COPY 无 Overwrite 头（默认 T）覆盖成功 → 204，内容被替换
        let resp = dav_call(
            &storage,
            "COPY",
            "/reader3/webdav/src.txt",
            &[("Destination", &dest("dst.txt"))],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            std::fs::read(fx.home.join("dst.txt")).unwrap(),
            b"new-content"
        );

        // COPY 到全新目标 → 201
        let resp = dav_call(
            &storage,
            "COPY",
            "/reader3/webdav/src.txt",
            &[("Destination", &dest("fresh.txt"))],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            std::fs::read(fx.home.join("fresh.txt")).unwrap(),
            b"new-content"
        );

        // MOVE 到新目标 → 201；MOVE 覆盖已有目标 → 204
        let resp = dav_call(
            &storage,
            "MOVE",
            "/reader3/webdav/fresh.txt",
            &[("Destination", &dest("moved.txt"))],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(!fx.home.join("fresh.txt").exists());
        let resp = dav_call(
            &storage,
            "MOVE",
            "/reader3/webdav/moved.txt",
            &[("Destination", &dest("src.txt")), ("Overwrite", "T")],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            std::fs::read(fx.home.join("src.txt")).unwrap(),
            b"new-content"
        );
        assert!(!fx.home.join("moved.txt").exists());

        // MOVE + Overwrite:F 且目标存在 → 412，源保留
        std::fs::write(fx.home.join("m2.txt"), b"m2").unwrap();
        let resp = dav_call(
            &storage,
            "MOVE",
            "/reader3/webdav/m2.txt",
            &[("Destination", &dest("src.txt")), ("Overwrite", "F")],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
        assert!(fx.home.join("m2.txt").exists());
        assert_eq!(
            std::fs::read(fx.home.join("src.txt")).unwrap(),
            b"new-content"
        );

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&fx.config.work_dir);
    }
}
