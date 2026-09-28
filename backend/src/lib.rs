//! reader-dev 核心库（Rust 重构）
//!
//! API 兼容 legacy 分支（Kotlin）的 `/reader3/*` 接口，数据兼容（JSON storage → SQLite 迁移）。

pub mod api;
pub mod middleware;
pub mod model;
pub mod parser;
pub mod service;
pub mod storage;
pub mod util;
pub mod web_assets;

use std::net::SocketAddr;

/// 应用配置（env / .env，兼容 READER_APP_* 前缀）
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// 工作目录（storage 根，兼容 READER_APP_WORKDIR）
    pub work_dir: String,
    /// 服务端口
    pub port: u16,
    /// 是否启用登录鉴权（多用户）
    pub secure: bool,
    /// 管理密码
    pub secure_key: String,
    /// 用户上限
    pub user_limit: i64,
    /// 用户书籍上限
    pub user_book_limit: i64,
    /// 正文缓存开关（legacy READER_APP_CACHECHAPTERCONTENT 对齐，默认 true）：
    /// false 时 getBookContent 抓取成功不落 book_chapters（saveBookContent/cacheBookOnServer 不受控）
    pub cache_chapter_content: bool,
    /// 邀请码
    pub invite_code: String,
    /// 最小密码长度
    pub min_user_password_length: i64,
    /// token 有效期（天，GAP 118；<=0 表示永不过期）
    pub token_ttl_days: i64,
    /// 前端静态资源根（构建产物 dist 目录）
    pub web_root: String,
    /// 默认新用户权限（legacy AppConfig 默认）
    pub default_user_enable_webdav: bool,
    pub default_user_enable_local_store: bool,
    pub default_user_enable_book_source: bool,
    pub default_user_enable_rss_source: bool,
    pub default_user_book_source_limit: i64,
    pub default_user_book_limit: i64,
    /// GAP 62 上传大小上限（MB，env READER_UPLOAD_MAX_MB，默认 100）
    pub upload_max_mb: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let work_dir = std::env::var("READER_APP_WORKDIR").unwrap_or_default();
        let port = std::env::var("READER_SERVER_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);
        Self {
            work_dir,
            port,
            cache_chapter_content: env_flag_default("READER_APP_CACHECHAPTERCONTENT", true),
            secure: env_flag("READER_APP_SECURE"),
            secure_key: std::env::var("READER_APP_SECUREKEY").unwrap_or_default(),
            user_limit: env_i64("READER_APP_USERLIMIT", 500_000),
            user_book_limit: env_i64("READER_APP_USERBOOKLIMIT", 500_000),
            invite_code: std::env::var("READER_APP_INVITECODE").unwrap_or_default(),
            min_user_password_length: env_i64("READER_APP_MINUSERPASSWORDLENGTH", 8),
            token_ttl_days: env_i64("READER_TOKEN_TTL_DAYS", 30),
            web_root: std::env::var("READER_APP_WEB_ROOT")
                .unwrap_or_else(|_| "web-ui/dist".to_string()),
            // 注册默认权限：书源/RSS 开（核心阅读能力），WebDAV 备份与本地存储默认关
            // （涉及服务器侧数据导出/落盘，按需由管理员在用户管理里开启）
            default_user_enable_webdav: env_flag_default(
                "READER_APP_DEFAULTUSERENABLEWEBDAV",
                false,
            ),
            default_user_enable_local_store: env_flag_default(
                "READER_APP_DEFAULTUSERENABLELOCALSTORE",
                false,
            ),
            default_user_enable_book_source: env_flag_default(
                "READER_APP_DEFAULTUSERENABLEBOOKSOURCE",
                true,
            ),
            default_user_enable_rss_source: env_flag_default(
                "READER_APP_DEFAULTUSERENABLERSSSOURCE",
                true,
            ),
            default_user_book_source_limit: env_i64("READER_APP_DEFAULTUSERBOOKSOURCELIMIT", 80000),
            default_user_book_limit: env_i64("READER_APP_DEFAULTUSERBOOKLIMIT", 5000),
            upload_max_mb: env_i64("READER_UPLOAD_MAX_MB", 100),
        }
    }

    /// GAP 62：上传大小上限字节数（multipart DefaultBodyLimit 用）
    pub fn upload_max_bytes(&self) -> usize {
        (self.upload_max_mb.max(1) * 1024 * 1024) as usize
    }

    /// storage 根目录（workDir 下的 storage）
    pub fn storage_dir(&self) -> std::path::PathBuf {
        let base = if self.work_dir.is_empty() {
            std::path::PathBuf::from(".")
        } else {
            std::path::PathBuf::from(&self.work_dir)
        };
        base.join("storage")
    }

    /// 启动服务（axum）
    pub async fn serve(self) -> anyhow::Result<()> {
        // legacy env 兼容提示：remote-webview 远程抓取池未移植（master 内置 camoufox）
        if std::env::var("READER_APP_REMOTEWEBVIEWAPI").is_ok() {
            tracing::warn!(
                "检测到 READER_APP_REMOTEWEBVIEWAPI：master 版 webView 抓取使用内置 camoufox，该变量仅提示不生效"
            );
        }

        let storage = storage::init(&self).await?;
        // EG4：JS cache（书源脚本 cache.put/get）SQLite 持久化——重启后登录 token/
        // 签名中间量不丢。注册句柄 + 启动恢复未过期条目到内存热缓存。
        crate::parser::js::register_js_cache_storage(storage.clone());
        // P1：@put/@get 书级变量 SQLite 持久化——登录态型书源 @put:{token:...}
        // 重启后不再批量失效（内存 miss 读穿透回填）
        crate::parser::rule::register_book_vars_storage(storage.clone());
        match crate::parser::js::load_js_cache_from_db(&storage).await {
            Ok(n) if n > 0 => tracing::info!("JS cache 已从库恢复 {n} 条"),
            Ok(_) => {}
            Err(e) => tracing::warn!("JS cache 启动恢复失败（本次运行仅内存）: {e:#}"),
        }
        // GAP 170 本地书双轨同步仓：启动对账 + 书仓目录文件监听（notify，300ms 去抖）
        service::local_sync::spawn_local_sync(storage.clone());
        // F-35 定时书架更新检查（每 10 分钟）+ GAP #101 订阅/RSS 自动刷新
        service::schedule::spawn_schedule_jobs(storage.clone());
        // GAP #57 自动备份（每天 READER_AUTO_BACKUP_HOUR 03:00 默认）
        service::schedule::spawn_auto_backup_job(storage.clone());
        let app = api::router(self.clone(), storage);
        // GAP 60：静态资源 Cache-Control（hash 文件名 30 天 / index.html no-cache）。
        // 挂载点说明：router.rs 被并行修改（git status 未提交改动）——避免冲突，
        // 在 app 构造处（lib.rs serve()，main.rs 无 app 构造代码）挂载，效果等同 router 内层。
        let app = app.layer(crate::middleware::cache_control::CacheControlLayer);
        // GAP 60 备注（实测确认）：router.rs 内的 CompressionLayer 只覆盖其调用前注册的路由
        // （/assets、/health 等）——fallback 静态资源（web-ui/dist）与后注册的 /reader3、/opds
        // 路由未覆盖（axum Router::layer 只包装调用时已注册的路由）。此处外层再挂一层兜底：
        // 已带 Content-Encoding 的响应自动跳过，SSE（text/event-stream）/音视频默认排除，无副作用。
        let app = app.layer(tower_http::compression::CompressionLayer::new());
        // GAP 62：multipart 超限（DefaultBodyLimit 413）→ 替换为明确的 JSON 错误（最外层兜底）
        let app = app.layer(crate::middleware::upload_limit::UploadLimitLayer {
            max_mb: self.upload_max_mb,
        });
        // 服务监控：请求计数（最外层——413/404/静态资源同样计入）
        let app = app.layer(crate::middleware::stats::StatsLayer);
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        tracing::info!("reader-dev (Rust) listening on {addr}");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        // M3：启用 ConnectInfo（直连对端 IP）——登录限流不再信任可伪造的 X-Forwarded-For
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await?;
        Ok(())
    }
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| flag_from_str(&v))
        .unwrap_or(false)
}

/// 环境变量布尔读取（未配置时返回 default——注册默认权限用）
fn env_flag_default(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => flag_from_str(&v),
        Err(_) => default,
    }
}

/// 环境变量布尔值解析（纯函数可测）：true/1/yes/on（大小写不敏感）→ true，其余 → false
fn flag_from_str(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P1-8：env_flag 布尔解析——true/1/yes/on（大小写不敏感）→ true，其余/缺失 → false
    #[test]
    fn test_flag_from_str() {
        for v in ["true", "TRUE", "True", "1", "yes", "on", "On"] {
            assert!(flag_from_str(v), "{v} 应为 true");
        }
        for v in ["false", "0", "no", "off", "", "2", "tru", "true "] {
            assert!(!flag_from_str(v), "{v:?} 应为 false");
        }
    }

    /// P1-8：默认用户权限 env 正确读取——未配置默认全开；显式 false 关闭
    #[test]
    fn test_default_user_env_flags() {
        // 缺失（默认）→ true
        std::env::remove_var("READER_APP_DEFAULTUSERENABLEBOOKSOURCE");
        std::env::remove_var("READER_APP_DEFAULTUSERENABLERSSSOURCE");
        assert!(env_flag_default(
            "READER_APP_DEFAULTUSERENABLEBOOKSOURCE",
            true
        ));
        assert!(env_flag_default(
            "READER_APP_DEFAULTUSERENABLERSSSOURCE",
            true
        ));
        // 显式 false → false
        std::env::set_var("READER_APP_DEFAULTUSERENABLEBOOKSOURCE", "false");
        assert!(!env_flag_default(
            "READER_APP_DEFAULTUSERENABLEBOOKSOURCE",
            true
        ));
        // 显式 true → true
        std::env::set_var("READER_APP_DEFAULTUSERENABLEBOOKSOURCE", "true");
        assert!(env_flag_default(
            "READER_APP_DEFAULTUSERENABLEBOOKSOURCE",
            true
        ));
        // 清理
        std::env::remove_var("READER_APP_DEFAULTUSERENABLEBOOKSOURCE");
    }

    /// P1-8：AppConfig::from_env 将 env 落到默认用户权限字段（未配置默认全开）
    #[test]
    fn test_from_env_default_user_flags() {
        std::env::remove_var("READER_APP_DEFAULTUSERENABLEWEBDAV");
        std::env::remove_var("READER_APP_DEFAULTUSERENABLELOCALSTORE");
        std::env::remove_var("READER_APP_DEFAULTUSERENABLEBOOKSOURCE");
        std::env::remove_var("READER_APP_DEFAULTUSERENABLERSSSOURCE");
        let cfg = AppConfig::from_env();
        assert!(cfg.default_user_enable_webdav);
        assert!(cfg.default_user_enable_local_store);
        assert!(cfg.default_user_enable_book_source);
        assert!(cfg.default_user_enable_rss_source);
        assert_eq!(cfg.default_user_book_source_limit, 80000);
        assert_eq!(cfg.default_user_book_limit, 5000);
        // 显式 false 可关闭
        std::env::set_var("READER_APP_DEFAULTUSERENABLEBOOKSOURCE", "false");
        let cfg2 = AppConfig::from_env();
        assert!(!cfg2.default_user_enable_book_source);
    }
}
