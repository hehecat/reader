//! 存储层：SQLite（兼容迁移自 legacy 的 JSON storage）

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::model::{Book, User};
use crate::AppConfig;

pub mod migrate;

/// 单次搜索的结论三态（`source_stats` 的记账口径）。
///
/// 为什么不是 `bool`：**超时不是失败**。老实现只有一个 `ok`，超时被记成普通失败 →
/// `consec_failures++` → 连败 3 次把好源压到 `confidence()=0.05`，再被「死源 6h 跳过」
/// 逻辑滤掉（实测超时源多为 15s 单源预算下的慢源，不是死源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOutcome {
    /// 有结论的成功（拿到响应/解析出结果）
    Ok,
    /// 真失败：网络错误、解析失败、HTTP 业务错误
    Fail,
    /// 超时 / 被窗硬顶截断：不给 `consec_failures` 定罪，但要记 `last_failure_at`
    Timeout,
}

impl SearchOutcome {
    /// 老布尔入口的映射（`true → Ok`、`false → Fail`）——**表达不了超时**
    pub fn from_bool(ok: bool) -> Self {
        if ok {
            Self::Ok
        } else {
            Self::Fail
        }
    }

    /// 抓取/搜索错误 → 结论：网络层超时归 [`SearchOutcome::Timeout`]，其余归 `Fail`
    pub fn from_error(e: &anyhow::Error) -> Self {
        if crate::service::crawler::is_timeout_error(e) {
            Self::Timeout
        } else {
            Self::Fail
        }
    }
}

/// 存储句柄
#[derive(Clone)]
pub struct Storage {
    pub pool: SqlitePool,
    pub config: AppConfig,
    /// 实例标识：cookie 快照缓存按「实例 + 命名空间」分桶，避免多个库/多实例（测试、备份库）
    /// 之间串用快照。同一池的 clone 共享同一标识，符合语义。
    cache_tag: u64,
}

/// 实例标识发号器
static STORAGE_INSTANCE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// 缓存统计（getCacheInfo）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    /// toc_cache 行数
    pub toc_cache_count: i64,
    /// toc_cache 近似大小（sum length(chapters_json)）
    pub toc_cache_size: i64,
    /// book_chapters 行数
    pub chapter_count: i64,
    /// 章节缓存近似大小（sum length(content)）
    pub chapter_size: i64,
    /// 总大小（目录缓存 + 章节缓存）
    pub total_size: i64,
}

/// 全书搜索命中（searchBookContent）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookContentHit {
    pub chapter_index: i64,
    pub title: String,
    /// 命中段落前后截取的摘要
    pub snippet: String,
}

/// 阅读统计单书汇总（getReadingStats.books[]）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingStatBook {
    pub book_url: String,
    /// 书名（书架 books 表关联；不在书架时用 bookUrl）
    pub name: String,
    /// 累计阅读秒数
    pub seconds: i64,
    /// 累计阅读字数
    pub chars: i64,
}

/// 阅读统计（getReadingStats：今日/本周/总计 + 单书汇总）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingStats {
    /// 今日阅读秒数
    pub today: i64,
    /// 近 7 天阅读秒数
    pub week: i64,
    /// 累计阅读秒数
    pub total: i64,
    /// 单书汇总（按秒数降序）
    pub books: Vec<ReadingStatBook>,
}

/// 命中摘要：定位 key 首次出现位置（大小写不敏感），取所在段落 + 前后各 radius 字符，
/// 截断处补省略号、换行压平为空格
fn make_snippet(content: &str, key: &str, radius: usize) -> String {
    let lower = content.to_lowercase();
    let key_lower = key.to_lowercase();
    let Some(pos) = lower.find(&key_lower) else {
        return String::new();
    };
    // 对齐 UTF-8 字符边界（lowercase 极端情形下字节偏移可能漂移）
    let pos = floor_char_boundary(content, pos);
    // 段落边界（最近的前后换行）
    let para_start = content[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let para_end = content[pos..]
        .find('\n')
        .map(|i| pos + i)
        .unwrap_or(content.len());
    let start = para_start.max(pos.saturating_sub(radius));
    let end = (pos + key.len() + radius).min(para_end);
    let start = floor_char_boundary(content, start);
    let end = floor_char_boundary(content, end);
    let mut s = String::new();
    if start > para_start {
        s.push('…');
    }
    s.push_str(&content[start..end]);
    if end < para_end {
        s.push('…');
    }
    s.replace('\n', " ")
}

/// 向左对齐到最近的 UTF-8 字符边界（O(3) 步内收敛）
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// P0-3 旧库迁移：五类 (url 单列主键) 表 → (url, user_namespace) 复合主键
///
/// 背景：book_sources/rss_sources/rss_articles/http_tts_list/source_subs 原以 URL 为主键，
/// secure 多用户下用户 B 保存同 URL 会覆盖用户 A 的行（INSERT OR REPLACE / ON CONFLICT(url)）。
/// 重建为复合主键后同 URL 按用户分行；所有读写路径均已按 user_namespace 过滤，读侧不受影响。
///
/// 幂等：sqlite_master.sql 已含复合主键定义即跳过（新库 CREATE 直接复合键；二次启动不再重建）。
/// 重建步骤（事务内）：按 pragma_table_info 实况列序建新表（保留类型/NOT NULL/默认值/列序——
/// 兼容旧库经 ALTER 追加的列如 book_sources.proxy_url 位于表尾）→ 逐列 INSERT SELECT 复制 →
/// DROP 旧表 → RENAME。
async fn migrate_ns_composite_keys(pool: &SqlitePool) -> Result<()> {
    // (表名, URL 列名)——与上方 CREATE TABLE 的复合主键列序一致
    const TABLES: &[(&str, &str)] = &[
        ("book_sources", "book_source_url"),
        ("rss_sources", "rss_source_url"),
        ("rss_articles", "url"),
        ("http_tts_list", "url"),
        ("source_subs", "url"),
    ];
    for &(table, url_col) in TABLES {
        let sql: Option<String> =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_optional(pool)
                .await?;
        let composite = format!("PRIMARY KEY ({url_col}, user_namespace)");
        // 去引号后匹配（重建表存的是带引号标识符形式）
        let sql_flat = sql.as_deref().map(|s| s.replace('"', ""));
        if sql_flat
            .as_deref()
            .map(|s| s.contains(&composite))
            .unwrap_or(false)
        {
            continue; // 已是复合主键（新库或已重建）
        }
        // 实况列元数据（pragma 序）：name/type/notnull/dflt_value
        let cols: Vec<(String, String, i64, Option<String>)> =
            sqlx::query_as("SELECT name, type, \"notnull\", dflt_value FROM pragma_table_info(?1)")
                .bind(table)
                .fetch_all(pool)
                .await?;
        if cols.is_empty() {
            tracing::warn!("{table} 无列信息，跳过复合主键重建");
            continue;
        }
        let defs: Vec<String> = cols
            .iter()
            .map(|(name, ty, notnull, dflt)| {
                let mut d = format!("\"{name}\" {ty}");
                if *notnull != 0 {
                    d.push_str(" NOT NULL");
                }
                if let Some(v) = dflt {
                    d.push_str(" DEFAULT ");
                    d.push_str(v);
                }
                d
            })
            .collect();
        let col_list = cols
            .iter()
            .map(|(name, ..)| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let tmp = format!("{table}_ns_pk");
        let create = format!(
            "CREATE TABLE \"{tmp}\" ({}, PRIMARY KEY (\"{url_col}\", \"user_namespace\"))",
            defs.join(", ")
        );
        let copy = format!("INSERT INTO \"{tmp}\" ({col_list}) SELECT {col_list} FROM \"{table}\"");
        let mut tx = pool.begin().await?;
        sqlx::query(&create).execute(&mut *tx).await?;
        sqlx::query(&copy).execute(&mut *tx).await?;
        sqlx::query(&format!("DROP TABLE \"{table}\""))
            .execute(&mut *tx)
            .await?;
        sqlx::query(&format!("ALTER TABLE \"{tmp}\" RENAME TO \"{table}\""))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!("{table} 重建为复合主键 ({url_col}, user_namespace)——同 URL 按用户隔离");
    }
    Ok(())
}

/// 连接池上限（env `READER_DB_MAX_CONNECTIONS`，默认 32）。
///
/// 默认从 8 提到 32：搜索 fan-out 并发 24（SSE/非 SSE 都是），每个源的搜索在
/// 「读 cookie 快照 + 读目录缓存 + 写目录/正文缓存」之间要占连接——池 8 时 24 路并发
/// 有 2/3 在排队，线上日志表现为 10~30s 的慢 SQL（排队时长算进 elapsed）。
/// SQLite 侧仍靠 WAL + busy_timeout(5s) 保证写竞争不报错；读多写少的本库其实不怕
/// 连接多，怕的是连接少导致的排队。实测依据见 scripts/dev-backend.sh 压测记录。
fn max_connections() -> u32 {
    std::env::var("READER_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(32)
        .clamp(1, 128)
}

/// 初始化：建目录 + 打开/建库 + 建表
pub async fn init(config: &AppConfig) -> Result<Storage> {
    let storage_dir = config.storage_dir();
    std::fs::create_dir_all(&storage_dir)?;
    let db_path = storage_dir.join("reader.db");

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        // GAP 96：WAL 模式（并发读写不互斥——默认 DELETE journal 下读会阻塞写/写会阻塞读）
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // GAP 96：busy_timeout（锁等待 5s，避免并发写瞬时 SQLITE_BUSY 报错）
        .busy_timeout(std::time::Duration::from_secs(5))
        // 每个正文请求的 save_book_vars、每章缓存写入都吃一次 commit：
        // 实测 FULL 2.65–3.09 ms/commit → NORMAL 0.06–0.09 ms（34–44×）。
        // WAL + NORMAL 的语义是「掉电可能丢最后几秒事务」，阅读站可接受。
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        // 页缓存 64 MB（默认 -2000 = 2 MB）：book_sources 全表扫实测 64.62 → 36.60 ms
        .pragma("cache_size", "-65536")
        // sqlx-sqlite 0.7.4 已知缺陷：语句缓存 + 建表/ALTER 类 DDL 并发时，
        // sqlite 自动重准备后的列数（column_count）与缓存的列元数据不一致 →
        // SqliteRow::current 越界 panic（row.rs:43，见 storage 测试偶发失败）。
        // 本库规模下准备开销可忽略，禁用缓存彻底规避。
        .statement_cache_capacity(0);
    let max_connections = max_connections();
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    // 建表（兼容 legacy 实体）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            username TEXT PRIMARY KEY,
            password TEXT NOT NULL,
            salt TEXT NOT NULL,
            token TEXT DEFAULT '',
            token_map TEXT,
            enable_webdav INTEGER DEFAULT 0,
            enable_local_store INTEGER DEFAULT 0,
            enable_book_source INTEGER DEFAULT 1,
            enable_rss_source INTEGER DEFAULT 1,
            book_source_limit INTEGER DEFAULT 0,
            book_limit INTEGER DEFAULT 0,
            is_admin INTEGER DEFAULT 0,
            last_login_at INTEGER DEFAULT 0,
            created_at INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT '',
            raw_json TEXT
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // OPDS 独立账号等系统设置（键值表）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS system_settings (
            key TEXT PRIMARY KEY,
            value TEXT,
            updated_at INTEGER DEFAULT 0
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 书源登录态 cookie（按用户隔离：user_namespace + source_url 联合主键）
    // user_agent 列：FlareSolverr 返回的 userAgent 与库中不同时一并记录（部分站点 UA 绑定 cookie）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_source_cookies (
            user_namespace TEXT NOT NULL,
            source_url TEXT NOT NULL,
            cookie TEXT NOT NULL DEFAULT '',
            user_agent TEXT NOT NULL DEFAULT '',
            login_header TEXT NOT NULL DEFAULT '',
            updated_at INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, source_url)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // F2 每书换源候选持久化（legacy getUserStorage(ns, name_author, "bookSource")）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_source_candidates (
            user_namespace TEXT NOT NULL,
            book_key TEXT NOT NULL,
            candidates_json TEXT NOT NULL DEFAULT '[]',
            updated_at INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, book_key)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // EG4 JS cache 对象持久化（书源脚本 cache.put/get——登录 token/签名中间量重启不丢；
    // expiry 为 Unix 毫秒时间戳，0 = 永不过期）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS js_cache (
            user_namespace TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            expiry INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, key)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // @put/@get 书级变量持久化（P1：纯内存缓存重启即失——登录态型书源 token 批量失效；
    // vars_json 为 RuleVars map 的 JSON 序列化；双键复用：book_url/toc_url/章节 URL 各存一行）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_vars_cache (
            user_namespace TEXT NOT NULL,
            source_url TEXT NOT NULL,
            url TEXT NOT NULL,
            vars_json TEXT NOT NULL,
            updated_at INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, source_url, url)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 兼容旧库：books 表缺 user_namespace 列时补列
    let cols: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('users')")
        .fetch_all(&pool)
        .await?;
    if !cols.iter().any(|c| c == "user_namespace") {
        sqlx::query("ALTER TABLE users ADD COLUMN user_namespace TEXT DEFAULT ''")
            .execute(&pool)
            .await?;
        tracing::info!("users 表补充 user_namespace 列");
    }

    // 兼容旧库：books.local_epub/local_pdf 曾被声明为 TEXT（Book 模型为 bool），
    // TEXT 亲和性会把 bool 存成文本 '0'/'1'，读取时 bool 解码失败（saveBook/进度/书架读回依赖）。
    // 检测到 TEXT 类型则重建表为 INTEGER（幂等，仅执行一次）。
    let epub_col_type: Option<String> =
        sqlx::query_scalar("SELECT type FROM pragma_table_info('books') WHERE name = 'local_epub'")
            .fetch_optional(&pool)
            .await?;
    if epub_col_type.as_deref() == Some("TEXT") {
        rebuild_books_bool_columns(&pool).await?;
        tracing::info!("books 表重建：local_epub/local_pdf TEXT → INTEGER");
    }

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS books (
            book_url TEXT,
            name TEXT DEFAULT '',
            author TEXT DEFAULT '',
            origin TEXT DEFAULT '',
            origin_name TEXT DEFAULT '',
            toc_url TEXT DEFAULT '',
            kind TEXT,
            custom_tag TEXT,
            cover_url TEXT,
            custom_cover_url TEXT,
            intro TEXT,
            custom_intro TEXT,
            charset TEXT,
            type INTEGER DEFAULT 0,
            group_name INTEGER DEFAULT 0,
            latest_chapter_title TEXT,
            latest_chapter_time INTEGER DEFAULT 0,
            last_check_time INTEGER DEFAULT 0,
            last_check_count INTEGER DEFAULT 0,
            total_chapter_num INTEGER DEFAULT 0,
            dur_chapter_title TEXT,
            dur_chapter_index INTEGER DEFAULT 0,
            dur_chapter_pos INTEGER DEFAULT 0,
            dur_chapter_time INTEGER DEFAULT 0,
            word_count TEXT,
            can_update INTEGER DEFAULT 1,
            order_num INTEGER DEFAULT 0,
            origin_order INTEGER DEFAULT 0,
            use_replace_rule INTEGER DEFAULT 1,
            variable TEXT,
            read_config TEXT,
            is_in_shelf INTEGER DEFAULT 1,
            cbz INTEGER DEFAULT 0,
            display_cover TEXT,
            display_intro TEXT,
            local_epub INTEGER DEFAULT 0,
            local_pdf INTEGER DEFAULT 0,
            pdf INTEGER DEFAULT 0,
            split_long_chapter INTEGER DEFAULT 0,
            last_check_error TEXT,
            info_html TEXT,
            toc_html TEXT,
            user_namespace TEXT DEFAULT '',
            created_at INTEGER DEFAULT 0,
            raw_json TEXT,
            local_file TEXT,
            local_file_mtime INTEGER DEFAULT 0,
            local_file_size INTEGER DEFAULT 0,
            local_file_deleted INTEGER DEFAULT 0,
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_sources (
            book_source_url TEXT,
            book_source_name TEXT DEFAULT '',
            book_source_group TEXT,
            book_source_type INTEGER DEFAULT 0,
            book_url_pattern TEXT,
            custom_order INTEGER DEFAULT 0,
            enabled INTEGER DEFAULT 1,
            enabled_explore INTEGER DEFAULT 1,
            enabled_cookie_jar INTEGER,
            concurrent_rate TEXT,
            js_lib TEXT,
            header TEXT,
            proxy_url TEXT,
            login_url TEXT,
            login_ui TEXT,
            login_check_js TEXT,
            login_js TEXT,
            book_source_comment TEXT,
            variable_comment TEXT,
            last_update_time INTEGER DEFAULT 0,
            respond_time INTEGER DEFAULT 0,
            weight INTEGER DEFAULT 0,
            use_count INTEGER DEFAULT 0,
            use_ts INTEGER DEFAULT 0,
            explore_url TEXT,
            search_url TEXT,
            rule_explore TEXT,
            rule_search TEXT,
            rule_book_info TEXT,
            rule_toc TEXT,
            rule_content TEXT,
            rule_related TEXT,
            search_rule TEXT,
            explore_rule TEXT,
            book_info_rule TEXT,
            toc_rule TEXT,
            content_rule TEXT,
            key TEXT,
            tag TEXT,
            logger TEXT,
            variable TEXT,
            user_namespace TEXT DEFAULT '',
            hidden INTEGER DEFAULT 0,
            raw_json TEXT,
            -- P0-3 按用户隔离：同 URL 不同用户各自成行（旧库由 migrate_ns_composite_keys 重建）
            PRIMARY KEY (book_source_url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 兼容旧库：book_sources 缺 proxy_url 列时补列（幂等——已存在则跳过；
    // 书源级代理（proxyUrl）求解 CF 质询/Turnstile 用）
    let bs_cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('book_sources')")
            .fetch_all(&pool)
            .await?;
    if !bs_cols.iter().any(|c| c == "proxy_url") {
        sqlx::query("ALTER TABLE book_sources ADD COLUMN proxy_url TEXT")
            .execute(&pool)
            .await?;
        tracing::info!("book_sources 表补充 proxy_url 列");
    }

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rss_sources (
            rss_source_url TEXT,
            rss_source_name TEXT DEFAULT '',
            rss_source_group TEXT,
            enabled INTEGER DEFAULT 1,
            user_namespace TEXT DEFAULT '',
            raw_json TEXT,
            -- P0-3 按用户隔离（同 URL 不同用户各自成行）
            PRIMARY KEY (rss_source_url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // RSS 文章缓存（P0-3 复合主键 (url, user_namespace)：每用户独立行/已读标记；content 为 feed 正文/摘要或抓取网页提取的正文）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rss_articles (
            url TEXT,
            source_url TEXT DEFAULT '',
            title TEXT DEFAULT '',
            author TEXT DEFAULT '',
            time INTEGER DEFAULT 0,
            content TEXT,
            cover TEXT,
            read INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT '',
            PRIMARY KEY (url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 兼容旧库：rss_articles 缺 read 列时补列（幂等——已存在则跳过）
    let rss_cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('rss_articles')")
            .fetch_all(&pool)
            .await?;
    if !rss_cols.iter().any(|c| c == "read") {
        sqlx::query("ALTER TABLE rss_articles ADD COLUMN read INTEGER DEFAULT 0")
            .execute(&pool)
            .await?;
        tracing::info!("rss_articles 表补充 read 列");
    }

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_chapters (
            book_url TEXT NOT NULL,
            chapter_index INTEGER NOT NULL,
            title TEXT DEFAULT '',
            content TEXT,
            user_namespace TEXT NOT NULL DEFAULT 'default',
            PRIMARY KEY (book_url, chapter_index, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 书籍详情持久缓存（getBookInfo 成功落盘, TTL 由调用方判定; 补进程内 bookInfoCache
    // 重启即失且书架书不命中的缺口）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_info_cache (
            book_url TEXT NOT NULL,
            user_namespace TEXT NOT NULL,
            info_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // F-10：目录缓存（getBookToc 成功落盘，TTL 5 分钟；book_url 为主键，toc_url 供“同 tocUrl 直读缓存”查找）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS toc_cache (
            book_url TEXT NOT NULL,
            toc_url TEXT DEFAULT '',
            chapters_json TEXT,
            updated_at INTEGER DEFAULT 0,
            user_namespace TEXT NOT NULL DEFAULT 'default',
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;
    // 目录缓存按 toc_url 查（get_toc_cache）而主键是 (book_url, user_namespace)：
    // 没有这条索引就是全表扫，1.9 MB 行实测 8.735 → 5.467 ms。CREATE INDEX IF NOT EXISTS
    // 对老库同样生效（启动时补建）。
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_toc_cache_url ON toc_cache(toc_url, user_namespace)",
    )
    .execute(&pool)
    .await?;

    // 书签（任务规格：PRIMARY KEY (book_url, title)）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS bookmarks (
            book_url TEXT NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            book_name TEXT NOT NULL DEFAULT '',
            book_author TEXT NOT NULL DEFAULT '',
            paragraph_index INTEGER DEFAULT 0,
            chapter_index INTEGER DEFAULT 0,
            chapter_name TEXT NOT NULL DEFAULT '',
            book_text TEXT NOT NULL DEFAULT '',
            content TEXT NOT NULL DEFAULT '',
            created_at INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT '',
            raw_json TEXT,
            PRIMARY KEY (book_url, title)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 书架分组（books.group_name 存分组 id）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_groups (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL DEFAULT '',
            cover TEXT,
            show INTEGER DEFAULT 1,
            order_num INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT ''
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // F-28 替换规则（前端生成字符串 id；order 为 SQLite 关键字 → order_num）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS replace_rules (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            group_name TEXT,
            find TEXT NOT NULL DEFAULT '',
            replace TEXT NOT NULL DEFAULT '',
            scope TEXT,
            scope_title INTEGER DEFAULT 0,
            scope_content INTEGER DEFAULT 1,
            is_regex INTEGER DEFAULT 0,
            timeout_millisecond INTEGER DEFAULT 3000,
            enable INTEGER DEFAULT 1,
            order_num INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT ''
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // F-26 HttpTTS 听书源（url 主键；type 0=在线合成 / 1=本地引擎）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS http_tts_list (
            url TEXT,
            name TEXT NOT NULL DEFAULT '',
            type INTEGER DEFAULT 0,
            content_type TEXT,
            concurrent_rate TEXT,
            login_url TEXT,
            login_ui TEXT,
            header TEXT,
            js_lib TEXT,
            enabled_cookie_jar INTEGER DEFAULT 0,
            login_check_js TEXT,
            last_update_time INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT '',
            -- P0-3 按用户隔离（同 URL 不同用户各自成行）
            PRIMARY KEY (url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 书源订阅（P0-3 复合主键 (url, user_namespace)：raw_json 为抓取到的书源数组 JSON 原文）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS source_subs (
            url TEXT,
            name TEXT NOT NULL DEFAULT '',
            enabled INTEGER DEFAULT 1,
            hidden INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT '',
            raw_json TEXT,
            selected_urls TEXT DEFAULT '[]',
            PRIMARY KEY (url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // P0-3 旧库迁移：五类（url 单列主键）表重建为 (url, user_namespace) 复合主键
    //（幂等：已复合则跳过；重建在全部 CREATE/补列之后，列序以 pragma 实况为准）
    migrate_ns_composite_keys(&pool).await?;

    // 自定义 TXT 目录规则（对齐 legado TxtTocRule：name/rule/serialNumber/enable）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS txt_toc_rules (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            rule TEXT NOT NULL DEFAULT '',
            enable INTEGER DEFAULT 1,
            serial_number INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT ''
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 用户配置（前端设置同步：user_config 表按用户命名空间 + 配置命名空间双主键）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_config (
            user_namespace TEXT NOT NULL,
            ns TEXT NOT NULL,
            config TEXT,
            updated_at INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, ns)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 阅读统计（reading_stats：按用户 + 书 + 日期累计阅读时长/字数）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS reading_stats (
            user_namespace TEXT NOT NULL,
            book_url TEXT NOT NULL,
            date TEXT NOT NULL,
            seconds INTEGER DEFAULT 0,
            chars INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, book_url, date)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 关系图谱 B 阶段（整本批量生成）：任务进度 / 逐批原始输出 / 每书成品。
    // 只增表，不改已有列：旧库升级时直接建空表即可。
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_graph_jobs (
            job_id TEXT PRIMARY KEY,
            book_url TEXT NOT NULL,
            user_namespace TEXT NOT NULL DEFAULT 'default',
            status TEXT NOT NULL,
            total_batches INTEGER NOT NULL DEFAULT 0,
            done_batches  INTEGER NOT NULL DEFAULT 0,
            chapters_scanned INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_graph_batches (
            job_id TEXT NOT NULL,
            batch_no INTEGER NOT NULL,
            raw_json TEXT NOT NULL,
            finished_at INTEGER NOT NULL,
            PRIMARY KEY (job_id, batch_no)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_graphs (
            book_url TEXT NOT NULL,
            user_namespace TEXT NOT NULL DEFAULT 'default',
            data_json TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'batch',
            characters INTEGER NOT NULL DEFAULT 0,
            relations  INTEGER NOT NULL DEFAULT 0,
            chapters_scanned INTEGER NOT NULL DEFAULT 0,
            job_id TEXT,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 增量生成台账：哪些章已经被模型抽过。`chapter_key` = `book_chapters.chapter_index`
    // （书源书是 md5(chapterUrl)，跨「目录重排 / 每次重新编号」稳定，不会因为 TOC 变动错位）。
    // `content_hash` 存正文哈希——老章正文被改写时要重抽，所以哈希变了就等于「没覆盖」。
    // 只增表、不改已有列：旧库升级时建出来的空表 = 全部未覆盖，第一次生成仍是全量。
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_graph_chapters (
            book_url TEXT NOT NULL,
            user_namespace TEXT NOT NULL DEFAULT 'default',
            chapter_key INTEGER NOT NULL,
            ordinal INTEGER NOT NULL DEFAULT 0,
            title TEXT NOT NULL DEFAULT '',
            content_hash INTEGER NOT NULL DEFAULT 0,
            done_at INTEGER,
            job_id TEXT,
            PRIMARY KEY (book_url, user_namespace, chapter_key)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 批次沉淀：一批的原始模型输出 + 它覆盖的 chapter_key 列表（JSON 数组）。
    // 归并吃这本书的**全部** unit（历史 + 本次）：归并是纯代码，不花 token，
    // 历史批留着只会让家族归并更准。
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS book_graph_units (
            book_url TEXT NOT NULL,
            user_namespace TEXT NOT NULL DEFAULT 'default',
            seq INTEGER NOT NULL,
            raw_json TEXT NOT NULL,
            chapter_keys TEXT NOT NULL DEFAULT '[]',
            job_id TEXT,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (book_url, user_namespace, seq)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // 幂等补列（兼容旧库：缺列则 ALTER TABLE 补上）
    let columns = [
        ("users", &["token_map", "raw_json"][..]),
        // legacy bookmark.json 的 content 字段无对应列 → raw_json 原文保底
        ("bookmarks", &["raw_json"][..]),
        ("book_sources", &["rule_related"][..]),
        // GAP 44：旧库缺 rss_source_group 列时补上（新库 CREATE TABLE 已含）
        ("rss_sources", &["rss_source_group"][..]),
        (
            "books",
            &[
                "toc_url",
                "custom_tag",
                "custom_intro",
                "latest_chapter_title",
                "latest_chapter_time",
                "last_check_time",
                "last_check_count",
                "total_chapter_num",
                "word_count",
                "order_num",
                "origin_order",
                "use_replace_rule",
                "variable",
                "read_config",
                "is_in_shelf",
                "cbz",
                "display_cover",
                "display_intro",
                "local_epub",
                "local_pdf",
                "pdf",
                "split_long_chapter",
                "info_html",
                "toc_html",
                "language",
                "publisher",
                "published_at",
                "raw_json",
            ][..],
        ),
    ];
    for (table, cols) in columns {
        for col in cols {
            ensure_column(&pool, table, col).await?;
        }
    }

    // GAP 170 本地书双轨同步列（幂等补列；local_file TEXT + 变更检测/删除标记 INTEGER）
    ensure_column_typed(&pool, "books", "local_file", "TEXT").await?;
    ensure_column_typed(&pool, "books", "local_file_mtime", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "books", "local_file_size", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "books", "local_file_deleted", "INTEGER DEFAULT 0").await?;

    // P0 跨用户缓存隔离：book_chapters / toc_cache 补 user_namespace 列（旧库 ALTER，新库建表已含）
    ensure_column_typed(
        &pool,
        "book_chapters",
        "user_namespace",
        "TEXT NOT NULL DEFAULT 'default'",
    )
    .await?;
    ensure_column_typed(
        &pool,
        "toc_cache",
        "user_namespace",
        "TEXT NOT NULL DEFAULT 'default'",
    )
    .await?;

    // legacy 实体字段幂等补列（旧库升级：缺列则 ALTER TABLE 补上）
    ensure_column_typed(&pool, "bookmarks", "book_name", "TEXT NOT NULL DEFAULT ''").await?;
    ensure_column_typed(
        &pool,
        "bookmarks",
        "book_author",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    ensure_column_typed(
        &pool,
        "bookmarks",
        "chapter_name",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    ensure_column_typed(&pool, "bookmarks", "book_text", "TEXT NOT NULL DEFAULT ''").await?;
    ensure_column_typed(&pool, "bookmarks", "content", "TEXT NOT NULL DEFAULT ''").await?;
    ensure_column_typed(&pool, "book_groups", "cover", "TEXT").await?;
    ensure_column_typed(&pool, "book_groups", "show", "INTEGER DEFAULT 1").await?;
    // legacy 多分组位掩码：books.group_ids 存 JSON 数组（group_name 保留主分组兼容）
    ensure_column_typed(&pool, "books", "group_ids", "TEXT NOT NULL DEFAULT ''").await?;
    ensure_column_typed(&pool, "replace_rules", "group_name", "TEXT").await?;
    ensure_column_typed(&pool, "replace_rules", "scope", "TEXT").await?;
    ensure_column_typed(&pool, "replace_rules", "scope_title", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "replace_rules", "scope_content", "INTEGER DEFAULT 1").await?;
    ensure_column_typed(&pool, "replace_rules", "is_regex", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(
        &pool,
        "replace_rules",
        "timeout_millisecond",
        "INTEGER DEFAULT 3000",
    )
    .await?;
    ensure_column_typed(&pool, "http_tts_list", "content_type", "TEXT").await?;
    ensure_column_typed(&pool, "http_tts_list", "concurrent_rate", "TEXT").await?;
    ensure_column_typed(&pool, "http_tts_list", "login_url", "TEXT").await?;
    ensure_column_typed(&pool, "http_tts_list", "login_ui", "TEXT").await?;
    ensure_column_typed(&pool, "http_tts_list", "header", "TEXT").await?;
    ensure_column_typed(&pool, "http_tts_list", "js_lib", "TEXT").await?;
    ensure_column_typed(
        &pool,
        "http_tts_list",
        "enabled_cookie_jar",
        "INTEGER DEFAULT 0",
    )
    .await?;
    ensure_column_typed(&pool, "http_tts_list", "login_check_js", "TEXT").await?;
    ensure_column_typed(
        &pool,
        "http_tts_list",
        "last_update_time",
        "INTEGER DEFAULT 0",
    )
    .await?;

    // 书源使用统计列（幂等补列：旧库缺 use_count/use_ts 时 ALTER TABLE 补上）
    ensure_column_typed(&pool, "book_sources", "use_count", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "book_sources", "use_ts", "INTEGER DEFAULT 0").await?;
    // 书源 JS 库（旧库升级：book_sources 缺 js_lib 列时补列）
    ensure_column_typed(&pool, "book_sources", "js_lib", "TEXT").await?;
    // 管理员标记（旧库升级：users 缺 is_admin 列时补列）
    ensure_column_typed(&pool, "users", "is_admin", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "users", "disabled", "INTEGER DEFAULT 0").await?;
    // 用户私有删除覆盖标记（旧库升级：book_sources / source_subs 缺 hidden 列时补列）
    ensure_column_typed(&pool, "book_sources", "hidden", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "source_subs", "hidden", "INTEGER DEFAULT 0").await?;
    ensure_column_typed(&pool, "source_subs", "selected_urls", "TEXT DEFAULT '[]'").await?;
    // 书源登录头（legacy putLoginHeader 持久化；旧库缺列时补列）
    ensure_column_typed(
        &pool,
        "book_source_cookies",
        "login_header",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;

    // 旧版本地书入库把 type 写死为 1（音频）——一次性纠正为文本；
    // CBZ 漫画保持 type=2，不受影响。
    sqlx::query("UPDATE books SET type = 0 WHERE origin = 'local' AND type = 1")
        .execute(&pool)
        .await?;

    tracing::info!("storage initialized at {}", db_path.display());

    // JSON → SQLite 迁移（幂等：users 表非空跳过）
    let storage = Storage {
        pool,
        config: config.clone(),
        cache_tag: STORAGE_INSTANCE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    };
    if let Err(e) = crate::storage::migrate::migrate_if_needed(&storage).await {
        tracing::error!("JSON→SQLite 迁移失败（服务继续启动，数据仍保留在 JSON）：{e}");
    }
    // 管理员兜底：旧库/迁移后无管理员时，把最早用户（优先名为 admin）提升为管理员
    if let Err(e) = storage.ensure_admin_user().await {
        tracing::warn!("初始化管理员兜底失败: {e:#}");
    }
    // 一次性纠正旧版注册默认值（全关 + 100/200 → 全开 + 80000/5000）：
    // 仅当用户权限字段仍精确等于旧错误默认时才覆盖；人工改过的字段原样保留。
    if let Err(e) = storage.migrate_user_permission_defaults().await {
        tracing::warn!("用户默认权限一次性迁移失败: {e:#}");
    }
    // source_stats（含 timeouts 三态列的幂等补列）必须建在下面的快照刷新**之前**：
    // ALTER TABLE 之后若还抱着旧读快照，首次 SELECT * 会读不到新列（见下条注释）。
    storage.ensure_source_stats_table().await?;
    storage.ensure_search_cache_table().await?;
    // WAL 快照刷新：WAL 模式下隐式读事务跨语句保持——建表/ALTER/重建期间
    // 执行过 pragma 检查的池连接会持有 DDL 提交前的旧读快照，后续查询可能
    // 读不到新表结构/新数据（sqlx-sqlite 0.7.4 下表现为 row.rs 越界 panic 或
    // 查询返回空——storage 测试偶发）。BEGIN;COMMIT; 结束各连接的隐式读事务，
    // 下次读取强制取新快照（无事务的连接上为安全 no-op；裸 COMMIT 会报错）。
    // 每个池连接都刷一次（上限跟 max_connections 一致，见函数注释）
    let max_conns = storage.pool.options().get_max_connections();
    for _ in 0..max_conns {
        let mut conn = storage.pool.acquire().await?;
        sqlx::query("BEGIN; COMMIT;").execute(&mut *conn).await?;
    }
    Ok(storage)
}

// ---------------- 书源 cookie 快照缓存（抓取热路径） ----------------
//
// 背景：`session_for` / `get_cookie_by_base` / `get_login_header_by_base` 每次抓取都要按
// ns 全表读一遍 `book_source_cookies`（线上 199 行）去找「本源」那一条。按源收窄 SQL 不可行：
// 线上 cookie 行的 `source_url` 是裸 host（如 `016wx.com`）或 `host##备用地址` 形式，而
// 164 个书源里只有 3 个有对应 cookie 行（其余 161 个每次都要「查不到」）——把查询改成按源
// 精查会让「查不到」这个绝大多数情况从 1 次查询变成 3~4 次查询。
// 因此采用「按 ns 缓存整份快照 + 写路径立刻失效」：稳态下一次抓取 0 次查询，
// TTL 10s 兜住任何漏掉的写路径，写路径 invalidation 保证不会拿到旧 cookie（登录态）。

/// ns 快照：`key = 实例标识 + ns`，值 = (写入时刻, 快照)
type CookieSnapshotMap =
    std::collections::HashMap<String, (std::time::Instant, std::sync::Arc<CookieSnapshot>)>;

fn cookie_snapshots() -> &'static std::sync::Mutex<CookieSnapshotMap> {
    static M: std::sync::OnceLock<std::sync::Mutex<CookieSnapshotMap>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 写代数：任何 cookie 写入都 +1（与缓存读写同锁），用于拒绝「写期间读到的旧快照」
static COOKIE_SNAPSHOT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 快照 TTL：写路径已全部失效，TTL 只是兜底（≤30s 约束内的保守取值）
const COOKIE_SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// 容量上限（ns 数量级极小，纯防呆）
const COOKIE_SNAPSHOT_MAX: usize = 32;

/// 容量上限插入（到顶就淘汰**写入时刻最旧的那一条**，其余热条目保留）。
///
/// 语义（逐条对应下面 `insert_capped_evicts_at_capacity` 单测）：
/// - 未到 `max`：直接插入，不淘汰；
/// - 已到 `max` 且 key 已存在：原地覆盖（值 + 时刻），不淘汰、总数不增长（幂等）；
/// - 已到 `max` 且 key 是新键：先淘汰时刻最小的那条再插入，总数恒等于 `max`；
/// - `max == 0`：一律不插入（保持空表），不 panic；
/// - 时刻相同（同一毫秒连插）时按 key 字典序定胜负，保证淘汰结果可复现。
///
/// 返回最后一条被淘汰的 key（没发生淘汰时为 `None`）。
///
/// 抽成纯函数是为了能脱离 sqlite / 全局单例单测：唯一调用点是
/// `Storage::cookie_snapshot` 的快照缓存写入。
fn insert_capped_evict_oldest<V>(
    m: &mut std::collections::HashMap<String, (std::time::Instant, V)>,
    key: String,
    value: V,
    max: usize,
    now: std::time::Instant,
) -> Option<String> {
    if max == 0 {
        return None;
    }
    let mut evicted = None;
    // 用 while（不是 if）：万一表里已经「超额」（历史数据/上限被调小），一次淘汰到位，
    // 绝不留 len > max 的状态。
    while m.len() >= max && !m.contains_key(&key) {
        let Some(victim) = m
            .iter()
            .min_by(|a, b| a.1 .0.cmp(&b.1 .0).then_with(|| a.0.cmp(b.0)))
            .map(|(k, _)| k.clone())
        else {
            break; // 空表：无可淘汰（`max == 0` 已在上面拦住）
        };
        m.remove(&victim);
        evicted = Some(victim);
    }
    m.insert(key, (now, value));
    evicted
}

fn cookie_snapshot_peek(key: &str) -> Option<std::sync::Arc<CookieSnapshot>> {
    let m = cookie_snapshots().lock().unwrap_or_else(|e| e.into_inner());
    match m.get(key) {
        Some((t, snap)) if t.elapsed() < COOKIE_SNAPSHOT_TTL => Some(snap.clone()),
        _ => None,
    }
}

/// 失效全部 cookie 快照——**任何写 `book_source_cookies` 的路径都必须调用**
/// （set/clear cookie、set login_header、删书源、删用户、备份恢复）。
/// 失效放在写事务提交之后调用，保证重新加载时读到已提交的新值。
pub(crate) fn invalidate_cookie_snapshots() {
    let mut m = cookie_snapshots().lock().unwrap_or_else(|e| e.into_inner());
    COOKIE_SNAPSHOT_GEN.fetch_add(1, std::sync::atomic::Ordering::Release);
    m.clear();
}

/// 一份 cookie 快照：预先把 `source_url`（含 `##` 备用地址）按 [`normalize_base`] 摊平成
/// base → 行下标，匹配退化为哈希查找。三张表分别复刻历史逐行扫描的三种语义
/// （「先命中者胜」，都按表顺序取第一条）：
/// - `any`：不过滤（`session_for`：返回 cookie + user_agent，空值也算命中）
/// - `cookie`：跳过 cookie 为空的行（`get_cookie_by_base`）
/// - `header`：跳过 login_header 为空的行（`get_login_header_by_base`）
#[derive(Debug, Default)]
struct CookieSnapshot {
    rows: Vec<(String, String, String)>,
    any: std::collections::HashMap<String, usize>,
    cookie: std::collections::HashMap<String, usize>,
    header: std::collections::HashMap<String, usize>,
}

impl CookieSnapshot {
    fn build(rows: Vec<(String, String, String, String)>) -> Self {
        let mut snap = CookieSnapshot::default();
        snap.rows.reserve(rows.len());
        for (source_url, cookie, user_agent, login_header) in rows {
            let idx = snap.rows.len();
            for part in source_url.split("##") {
                let Some(base) = normalize_base(part) else {
                    continue;
                };
                snap.any.entry(base.clone()).or_insert(idx);
                if !cookie.is_empty() {
                    snap.cookie.entry(base.clone()).or_insert(idx);
                }
                if !login_header.trim().is_empty() {
                    snap.header.entry(base).or_insert(idx);
                }
            }
            snap.rows.push((cookie, user_agent, login_header));
        }
        snap
    }

    /// `session_for` 语义：cookie + user_agent（不筛空）
    fn session(&self, base: &str) -> Option<(String, String)> {
        let row = self.rows.get(*self.any.get(base)?)?;
        Some((row.0.clone(), row.1.clone()))
    }

    /// `get_cookie_by_base` 语义：cookie 非空才算命中
    fn cookie_for(&self, base: &str) -> Option<String> {
        let row = self.rows.get(*self.cookie.get(base)?)?;
        Some(row.0.clone())
    }

    /// `get_login_header_by_base` 语义：login_header 非空才算命中
    fn header_for(&self, base: &str) -> Option<String> {
        let row = self.rows.get(*self.header.get(base)?)?;
        Some(row.2.clone())
    }
}

impl Storage {
    /// 按用户名查用户（登录 / token 校验）
    pub async fn find_user(&self, username: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;
        Ok(user)
    }

    // ---------------- OPDS 设置（system_settings 键值表） ----------------

    /// 系统设置读取（无则 None）
    pub async fn get_system_setting(&self, key: &str) -> Result<Option<String>> {
        let r: Option<(String,)> =
            sqlx::query_as("SELECT value FROM system_settings WHERE key = ?1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(r.map(|x| x.0))
    }

    /// 系统设置写入（INSERT OR REPLACE）
    pub async fn set_system_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO system_settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
        )
        .bind(key)
        .bind(value)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 系统设置删除（返回删除行数）
    pub async fn delete_system_setting(&self, key: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM system_settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// OPDS 独立账号：读取 (username, 存储串 `salt$hash`)。未配置返回 None。
    pub async fn get_opds_account(&self) -> Result<Option<(String, String)>> {
        let username = self.get_system_setting("opds_username").await?;
        let password = self.get_system_setting("opds_password").await?;
        match (username, password) {
            (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => Ok(Some((u, p))),
            _ => Ok(None),
        }
    }

    /// OPDS 独立账号写入（password 为已生成的 `salt$hash` 存储串）
    pub async fn set_opds_account(&self, username: &str, stored_password: &str) -> Result<()> {
        self.set_system_setting("opds_username", username).await?;
        self.set_system_setting("opds_password", stored_password)
            .await?;
        Ok(())
    }

    /// OPDS 独立账号清除（禁用；回退系统账号/token 认证）
    pub async fn clear_opds_account(&self) -> Result<()> {
        self.delete_system_setting("opds_username").await?;
        self.delete_system_setting("opds_password").await?;
        Ok(())
    }

    /// 书源列表（按命名空间；无则回退 default）
    /// 默认排序：weight DESC 优先（权重自动调整后高权重书源靠前），
    /// 权重相同回落 custom_order（legacy 手动排序），再按名称稳定排序。
    pub async fn get_book_sources(&self, ns: &str) -> Result<Vec<crate::model::BookSource>> {
        // 用户自有行 + 未覆盖的 default 系统行合并：
        // - 用户行（含 hidden 删除覆盖）优先，同 URL 时 default 行被覆盖隐藏；
        // - 最终对外过滤 hidden（用户删除的系统源在本命名空间消失，不影响 default）。
        // ORDER BY 交给下面的 Rust 排序：SQL 排 615 行 × 44 列会多一个 TEMP B-TREE，
        // 实测 44.74 → 30.76 ms，而排序语义完全一致（weight DESC, custom_order, name）。
        let mut rows = sqlx::query_as::<_, crate::model::BookSource>(
            "SELECT * FROM book_sources WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        sort_book_sources(&mut rows);
        if ns == "default" {
            return Ok(rows.into_iter().filter(|s| !s.hidden).collect());
        }
        let mut default_rows = sqlx::query_as::<_, crate::model::BookSource>(
            "SELECT * FROM book_sources WHERE user_namespace = 'default'",
        )
        .fetch_all(&self.pool)
        .await?;
        sort_book_sources(&mut default_rows);
        let mut merged: Vec<crate::model::BookSource> = rows;
        let owned: std::collections::HashSet<String> =
            merged.iter().map(|s| s.book_source_url.clone()).collect();
        for s in default_rows {
            if !owned.contains(&s.book_source_url) {
                merged.push(s);
            }
        }
        // 合并后保持原排序语义：weight DESC, custom_order, name
        sort_book_sources(&mut merged);
        Ok(merged.into_iter().filter(|s| !s.hidden).collect())
    }

    /// 书源使用统计自增（搜索/换源/正文抓取成功时调用）：
    /// use_count+1 并刷新 use_ts（原子 UPDATE；未命中行静默忽略，不影响业务）
    pub async fn bump_book_source_use(&self, ns: &str, url: &str) -> Result<()> {
        sqlx::query(
            "UPDATE book_sources SET use_count = use_count + 1, use_ts = ?1 \
             WHERE user_namespace = ?2 AND book_source_url = ?3",
        )
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(ns)
        .bind(url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 按 URL 查书源（精确或前缀匹配，兼容 ##@ 后缀；用户命名空间 + fallback default）
    pub async fn find_book_source(
        &self,
        ns: &str,
        book_source_url: &str,
    ) -> Result<Option<crate::model::BookSource>> {
        let like = format!("{book_source_url}%");
        let r = sqlx::query_as::<_, crate::model::BookSource>(
            "SELECT * FROM book_sources WHERE user_namespace = ?1 AND hidden = 0 \
             AND (book_source_url = ?2 OR book_source_url LIKE ?3)",
        )
        .bind(ns)
        .bind(book_source_url)
        .bind(&like)
        .fetch_optional(&self.pool)
        .await?;
        if r.is_some() || ns == "default" {
            return Ok(r);
        }
        // 用户已有该 URL 的私有行（含 hidden 删除覆盖）时不得回退 default，
        // 否则已删除的系统源仍会被单查接口找回。
        let overlay = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM book_sources WHERE user_namespace = ?1 \
             AND (book_source_url = ?2 OR book_source_url LIKE ?3)",
        )
        .bind(ns)
        .bind(book_source_url)
        .bind(&like)
        .fetch_one(&self.pool)
        .await?;
        if overlay > 0 {
            return Ok(None);
        }
        sqlx::query_as::<_, crate::model::BookSource>(
            "SELECT * FROM book_sources WHERE user_namespace = 'default' AND hidden = 0 \
             AND (book_source_url = ?1 OR book_source_url LIKE ?2)",
        )
        .bind(book_source_url)
        .bind(&like)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 保存书源（INSERT OR REPLACE；raw_json 按 camelCase 重新序列化，与 bookSource.json 字段名一致）
    pub async fn save_book_source(
        &self,
        ns: &str,
        source: &crate::model::BookSource,
    ) -> Result<()> {
        upsert_book_source(&self.pool, ns, source).await
    }

    /// 批量保存书源（单事务：全部成功或全部回滚）
    pub async fn save_book_sources(
        &self,
        ns: &str,
        sources: &[crate::model::BookSource],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for source in sources {
            upsert_book_source(&mut *tx, ns, source).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 删除书源（按 URL 精确匹配；用户命名空间无记录时回退 default——列表回退语义一致，
    /// 否则用户看到的系统书源删除后刷新又会出现）；返回受影响行数
    /// 连带删除该书源的登录态 cookie（按实际目标命名空间）
    pub async fn delete_book_source(&self, ns: &str, url: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "DELETE FROM book_sources WHERE user_namespace = ?1 AND book_source_url = ?2 AND hidden = 0",
        )
        .bind(ns)
        .bind(url)
        .execute(&mut *tx)
        .await?;
        let mut affected = r.rows_affected();
        sqlx::query(
            "DELETE FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(url)
        .execute(&mut *tx)
        .await?;
        if ns != "default" {
            if let Some(default_src) = find_book_source_row(&mut *tx, "default", url).await? {
                // 用户删的是本人自有且 URL 与 default 前缀匹配到的不同行时，不误隐藏系统源；
                // 其余情况复制 hidden 覆盖（删系统源、删与系统源同 URL 的个人副本、幂等重删）
                if !(affected > 0 && default_src.book_source_url != url) {
                    upsert_book_source_hidden(&mut *tx, ns, &default_src, true).await?;
                    affected = 1;
                }
            } else if affected == 0 {
                // 用户已有 hidden 覆盖且 default 行已被管理员删除：保持隐藏（幂等）
                let hidden_rows = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM book_sources WHERE user_namespace = ?1 \
                     AND book_source_url = ?2 AND hidden = 1",
                )
                .bind(ns)
                .bind(url)
                .fetch_one(&mut *tx)
                .await?;
                if hidden_rows > 0 {
                    affected = 1;
                }
            }
        }
        tx.commit().await?;
        invalidate_cookie_snapshots();
        Ok(affected)
    }

    /// 按 URL 查单个书源（管理 API 用；复用 find_book_source 的精确/前缀匹配 + default 回退语义）
    pub async fn get_book_source(
        &self,
        ns: &str,
        url: &str,
    ) -> Result<Option<crate::model::BookSource>> {
        self.find_book_source(ns, url).await
    }

    /// 去重分组列表（兼容 legacy getBookSourceGroups：bookSourceGroup 空格分隔，保序去重；无书源回退 default）
    pub async fn list_book_source_groups(&self, ns: &str) -> Result<Vec<String>> {
        let sources = self.get_book_sources(ns).await?;
        let mut groups: Vec<String> = Vec::new();
        for s in sources {
            let Some(group) = s.book_source_group else {
                continue;
            };
            for part in group.split_whitespace() {
                if !groups.iter().any(|g| g == part) {
                    groups.push(part.to_string());
                }
            }
        }
        Ok(groups)
    }

    /// 启停书源（按 URL 精确匹配；用户命名空间无记录时回退 default——与列表回退语义一致）；
    /// 返回受影响行数
    pub async fn update_book_source_enabled(
        &self,
        ns: &str,
        url: &str,
        enabled: bool,
    ) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "UPDATE book_sources SET enabled = ?1, hidden = 0 WHERE user_namespace = ?2 AND book_source_url = ?3",
        )
        .bind(enabled)
        .bind(ns)
        .bind(url)
        .execute(&mut *tx)
        .await?;
        let mut affected = r.rows_affected();
        if affected == 0 && ns != "default" {
            // 普通用户停用 default 系统书源：复制到本人命名空间后修改 enabled——
            // 个人覆盖，不影响系统配置。
            if let Some(default_src) = find_book_source_row(&mut *tx, "default", url).await? {
                let mut copy = default_src;
                copy.enabled = enabled;
                upsert_book_source_hidden(&mut *tx, ns, &copy, false).await?;
                affected = 1;
            }
        }
        tx.commit().await?;
        Ok(affected)
    }

    /// 清空命名空间全部书源（连带清理书源 cookie）。
    /// 普通用户清空：删除本人自有书源 + 把 default 系统书源全部复制为 hidden 私有覆盖；
    /// 管理员/default：直接删除系统书源。
    pub async fn delete_all_book_sources(&self, ns: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let mut affected = 0u64;
        if ns != "default" {
            let defaults = sqlx::query_as::<_, crate::model::BookSource>(
                "SELECT * FROM book_sources WHERE user_namespace = 'default'",
            )
            .fetch_all(&mut *tx)
            .await?;
            for s in defaults {
                if !s.hidden {
                    upsert_book_source_hidden(&mut *tx, ns, &s, true).await?;
                    affected += 1;
                }
            }
        }
        // 只删本人可见行；hidden 覆盖代表“已删除的系统源”，清空后应继续保持隐藏
        let r = sqlx::query("DELETE FROM book_sources WHERE user_namespace = ?1 AND hidden = 0")
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        affected += r.rows_affected();
        sqlx::query("DELETE FROM book_source_cookies WHERE user_namespace = ?1")
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        invalidate_cookie_snapshots();
        Ok(affected)
    }

    // ---------------- EG4：JS cache 对象持久化（书源脚本 cache.put/get） ----------------

    /// 读取单条 JS cache（value, expiry 毫秒时间戳；0 = 永不过期）。过期与否由调用方判定。
    pub async fn get_js_cache(&self, ns: &str, key: &str) -> Result<Option<(String, i64)>> {
        let r: Option<(String, i64)> = sqlx::query_as(
            "SELECT value, expiry FROM js_cache WHERE user_namespace = ?1 AND key = ?2",
        )
        .bind(ns)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r)
    }

    /// 写入单条 JS cache（INSERT OR REPLACE）
    pub async fn put_js_cache(
        &self,
        ns: &str,
        key: &str,
        value: &str,
        expiry_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO js_cache (user_namespace, key, value, expiry) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(ns)
        .bind(key)
        .bind(value)
        .bind(expiry_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 删除单条 JS cache（返回删除行数）
    pub async fn delete_js_cache(&self, ns: &str, key: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM js_cache WHERE user_namespace = ?1 AND key = ?2")
            .bind(ns)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 清空命名空间全部 JS cache（cache.clear；返回删除行数）
    pub async fn clear_js_cache(&self, ns: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM js_cache WHERE user_namespace = ?1")
            .bind(ns)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ---------------- @put/@get 书级变量持久化（book_vars_cache） ----------------

    /// 读取书级变量 JSON（精确键；无则 None）
    pub async fn get_book_vars_cache(
        &self,
        ns: &str,
        source_url: &str,
        url: &str,
    ) -> Result<Option<String>> {
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT vars_json FROM book_vars_cache \
             WHERE user_namespace = ?1 AND source_url = ?2 AND url = ?3",
        )
        .bind(ns)
        .bind(source_url)
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|x| x.0))
    }

    /// 写入书级变量 JSON（INSERT OR REPLACE）
    pub async fn put_book_vars_cache(
        &self,
        ns: &str,
        source_url: &str,
        url: &str,
        vars_json: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO book_vars_cache \
             (user_namespace, source_url, url, vars_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(ns)
        .bind(source_url)
        .bind(url)
        .bind(vars_json)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------- 搜索结果短缓存 (10min): 同关键词重搜免回源 ----------

    pub async fn ensure_search_cache_table(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS search_cache (
                user_namespace TEXT NOT NULL,
                cache_key TEXT NOT NULL,
                result_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (user_namespace, cache_key)
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_search_cache(
        &self,
        ns: &str,
        key: &str,
        ttl_ms: i64,
    ) -> Result<Option<String>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT result_json, created_at FROM search_cache WHERE user_namespace = ?1 AND cache_key = ?2",
        )
        .bind(ns)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        let Some((json, created)) = row else {
            return Ok(None);
        };
        let now = chrono::Utc::now().timestamp_millis();
        if now - created > ttl_ms {
            return Ok(None);
        }
        Ok(Some(json))
    }

    pub async fn put_search_cache(&self, ns: &str, key: &str, json: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR REPLACE INTO search_cache (user_namespace, cache_key, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(ns)
        .bind(key)
        .bind(json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------- 书源置信度统计 (搜索排序/清理无效源依据) ----------

    /// 滑动全量计数版统计行; confidence() = 质量分(成功率 .55 / 目录 .15 / 正文 .15 /
    /// 丰富度 .15) × 速度因子(0.3 + 0.7×(1-平均延迟/30s)); 冷启动 0.5;
    /// 连败>=3 或 尝试>=5 且成功率<0.2 压到 0.05
    pub async fn ensure_source_stats_table(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS source_stats (
                user_namespace TEXT NOT NULL,
                source_url TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                successes INTEGER NOT NULL DEFAULT 0,
                timeouts INTEGER NOT NULL DEFAULT 0,
                lat_sum_ms INTEGER NOT NULL DEFAULT 0,
                intro_hits INTEGER NOT NULL DEFAULT 0,
                intro_count INTEGER NOT NULL DEFAULT 0,
                toc_attempts INTEGER NOT NULL DEFAULT 0,
                toc_successes INTEGER NOT NULL DEFAULT 0,
                content_attempts INTEGER NOT NULL DEFAULT 0,
                content_successes INTEGER NOT NULL DEFAULT 0,
                consec_failures INTEGER NOT NULL DEFAULT 0,
                last_failure_at INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_namespace, source_url)
            )",
        )
        .execute(&self.pool)
        .await?;
        // 旧库升级：timeouts 三态列（超时计数）幂等补上——列已存在时是 no-op，
        // 连跑两次不会报错、也不会重复加列（`pragma_table_info` 判存在，见 ensure_column_typed）。
        ensure_column_typed(
            &self.pool,
            "source_stats",
            "timeouts",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        Ok(())
    }

    /// 三态记账（**新增调用点必须走这个**，不要再用 bool 表达「超时」）
    ///
    /// 语义严格三分：
    /// - [`SearchOutcome::Ok`]：`attempts+1`、`successes+1`、`consec_failures=0`
    /// - [`SearchOutcome::Fail`]：`attempts+1`、`consec_failures+1`、`last_failure_at=now_ms`
    /// - [`SearchOutcome::Timeout`]：`attempts+1`、`timeouts+1`、`consec_failures` **不变**、
    ///   `last_failure_at=now_ms`
    ///
    /// 为什么超时不能记成失败：老实现用一个 `ok: bool`，超时被记成普通失败 → `consec_failures++`
    /// → 连败 3 次就把好源压到 `confidence()=0.05`，再被「死源 6h 跳过」逻辑过滤掉。
    /// 时间戳一律毫秒（`timestamp_millis()`）；秒/毫秒混用会让 6h 豁免窗口变成 6 秒或 25 天。
    pub async fn record_search_outcome_state(
        &self,
        ns: &str,
        source_url: &str,
        outcome: SearchOutcome,
        lat_ms: i64,
        intro_hits: i64,
        intro_count: i64,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        // 三个互斥增量，SQL 里用它们表达三态，避免「布尔 + CASE」把超时混进连败
        let ok_delta = (outcome == SearchOutcome::Ok) as i64;
        let timeout_delta = (outcome == SearchOutcome::Timeout) as i64;
        let consec_inc = (outcome == SearchOutcome::Fail) as i64;
        let consec_reset = (outcome == SearchOutcome::Ok) as i64;
        // Ok 不动 last_failure_at；Fail/Timeout 都写（死源 6h 豁免靠它才生效）
        let fail_ts = if outcome == SearchOutcome::Ok { 0 } else { now };
        sqlx::query(
            "INSERT INTO source_stats (user_namespace, source_url, attempts, successes, timeouts, lat_sum_ms, intro_hits, intro_count, consec_failures, last_failure_at, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(user_namespace, source_url) DO UPDATE SET
                attempts = attempts + 1,
                successes = successes + ?3,
                timeouts = timeouts + ?4,
                lat_sum_ms = lat_sum_ms + ?5,
                intro_hits = intro_hits + ?6,
                intro_count = intro_count + ?7,
                consec_failures = CASE WHEN ?11 = 1 THEN 0
                                       WHEN ?8 = 1 THEN consec_failures + 1
                                       ELSE consec_failures END,
                last_failure_at = CASE WHEN ?9 > 0 THEN ?9 ELSE last_failure_at END,
                updated_at = ?10",
        )
        .bind(ns)
        .bind(source_url)
        .bind(ok_delta)
        .bind(timeout_delta)
        .bind(lat_ms)
        .bind(intro_hits)
        .bind(intro_count)
        .bind(consec_inc)
        .bind(fail_ts)
        .bind(now)
        .bind(consec_reset)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 向后兼容入口（老签名：`ok: bool`）——只是 [`Self::record_search_outcome_state`] 的薄包装。
    ///
    /// ⚠️ `ok=false` 一律记 `Fail`：**它表达不了超时**。凡是「超时/被窗硬顶截断」的新调用点
    /// 必须显式传 [`SearchOutcome::Timeout`]，否则又会污染 `consec_failures`。
    pub async fn record_search_outcome(
        &self,
        ns: &str,
        source_url: &str,
        ok: bool,
        lat_ms: i64,
        intro_hits: i64,
        intro_count: i64,
    ) -> Result<()> {
        self.record_search_outcome_state(
            ns,
            source_url,
            SearchOutcome::from_bool(ok),
            lat_ms,
            intro_hits,
            intro_count,
        )
        .await
    }

    pub async fn record_path_outcome(
        &self,
        ns: &str,
        source_url: &str,
        kind: &str,
        ok: bool,
    ) -> Result<()> {
        let (ta, ts) = if kind == "toc" {
            (1i64, if ok { 1 } else { 0 })
        } else {
            (0, 0)
        };
        let (ca, cs) = if kind == "content" {
            (1i64, if ok { 1 } else { 0 })
        } else {
            (0, 0)
        };
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO source_stats (user_namespace, source_url, toc_attempts, toc_successes, content_attempts, content_successes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(user_namespace, source_url) DO UPDATE SET
                toc_attempts = toc_attempts + ?3,
                toc_successes = toc_successes + ?4,
                content_attempts = content_attempts + ?5,
                content_successes = content_successes + ?6,
                updated_at = ?7",
        )
        .bind(ns)
        .bind(source_url)
        .bind(ta)
        .bind(ts)
        .bind(ca)
        .bind(cs)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_source_stats(&self, ns: &str) -> Result<Vec<crate::model::SourceStat>> {
        let rows = sqlx::query_as::<_, crate::model::SourceStat>(
            "SELECT * FROM source_stats WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 批量写章节级变量: 单事务多行, 目录循环逐章写入的快路径 (千章级首抓秒级)
    pub async fn put_book_vars_cache_batch(
        &self,
        ns: &str,
        source_url: &str,
        urls: &[String],
        vars_json: &str,
    ) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut tx = self.pool.begin().await?;
        for url in urls {
            sqlx::query(
                "INSERT OR REPLACE INTO book_vars_cache \
                 (user_namespace, source_url, url, vars_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(ns)
            .bind(source_url)
            .bind(url)
            .bind(vars_json)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 启动加载：全部未过期条目 (user_namespace, key, value, expiry)
    pub async fn load_js_cache(&self) -> Result<Vec<(String, String, String, i64)>> {
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT user_namespace, key, value, expiry FROM js_cache \
             WHERE expiry = 0 OR expiry > ?1",
        )
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // ---------------- 书源登录态 cookie（按用户隔离） ----------------

    /// 取本命名空间的 cookie 快照（10s TTL 内存缓存，见 [`CookieSnapshot`]）
    ///
    /// 抓取热路径：每次抓取此前要按 ns 全表读 `book_source_cookies`（线上 199 行）找
    /// 「本源」那一条，fan-out 期间每秒数十次；线上 18 条慢 SQL 全是这条查询。
    async fn cookie_snapshot(&self, ns: &str) -> Result<std::sync::Arc<CookieSnapshot>> {
        let key = format!("{}\u{1}{ns}", self.cache_tag);
        if let Some(snap) = cookie_snapshot_peek(&key) {
            return Ok(snap);
        }
        // 单飞（single-flight）：fan-out 起步的同一瞬间会有几十个抓取同时 miss，
        // 若各自查库就变成「几十个一模一样的全表读」，缓存反而把慢 SQL 变多（实测过）。
        // 用一把异步锁串行化构建：后到者拿到锁后重新 peek，直接命中刚建好的快照。
        static LOAD_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _load = LOAD_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        if let Some(snap) = cookie_snapshot_peek(&key) {
            return Ok(snap);
        }
        let gen = COOKIE_SNAPSHOT_GEN.load(std::sync::atomic::Ordering::Acquire);
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT source_url, cookie, user_agent, login_header FROM book_source_cookies \
             WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        let snap = std::sync::Arc::new(CookieSnapshot::build(rows));
        // 「校验写代数 + 写入」在同一把锁内完成：写路径 invalidate 也在这把锁内 bump 代数，
        // 因此不会出现「读到写前旧数据、却在 invalidate 之后才塞进缓存」的竞态。
        let mut m = cookie_snapshots().lock().unwrap_or_else(|e| e.into_inner());
        if COOKIE_SNAPSHOT_GEN.load(std::sync::atomic::Ordering::Acquire) == gen {
            // 到顶淘汰最旧一条（原先整体 clear()：一次溢出会把其余 ns 的热快照全打掉，
            // 之后每个 ns 都要重查一次全表，见 [`insert_capped_evict_oldest`]）。
            insert_capped_evict_oldest(
                &mut m,
                key,
                snap.clone(),
                COOKIE_SNAPSHOT_MAX,
                std::time::Instant::now(),
            );
        }
        Ok(snap)
    }

    /// 读取书源 cookie（精确 source_url 键；无则 None）
    pub async fn get_cookie(&self, ns: &str, source_url: &str) -> Result<Option<String>> {
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT cookie FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|x| x.0).filter(|c| !c.is_empty()))
    }

    /// 列出当前用户全部书源登录态（Cookie 管理：source_url/cookie/user_agent/login_header/updated_at）
    pub async fn list_cookies(&self, ns: &str) -> Result<Vec<crate::model::CookieRow>> {
        let rows = sqlx::query_as::<_, crate::model::CookieRow>(
            "SELECT source_url, cookie, user_agent, login_header, updated_at \
             FROM book_source_cookies WHERE user_namespace = ?1 \
             ORDER BY updated_at DESC, source_url",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter(|r| {
                !r.cookie.trim().is_empty()
                    || !r.user_agent.trim().is_empty()
                    || !r.login_header.trim().is_empty()
            })
            .collect())
    }

    /// 写入书源 cookie（INSERT OR REPLACE；空值等价清除）
    pub async fn set_cookie(&self, ns: &str, source_url: &str, cookie: &str) -> Result<()> {
        if cookie.trim().is_empty() {
            self.clear_cookie(ns, source_url).await?;
            return Ok(());
        }
        sqlx::query(
            "INSERT OR REPLACE INTO book_source_cookies (user_namespace, source_url, cookie, user_agent, login_header, updated_at)
             VALUES (?1, ?2, ?3, \
                     COALESCE((SELECT user_agent FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2), ''), \
                     COALESCE((SELECT login_header FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2), ''), \
                     ?4)",
        )
        .bind(ns)
        .bind(source_url)
        .bind(cookie)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        invalidate_cookie_snapshots();
        Ok(())
    }

    /// 清除书源 cookie（返回删除行数）
    pub async fn clear_cookie(&self, ns: &str, source_url: &str) -> Result<u64> {
        let r = sqlx::query(
            "DELETE FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .execute(&self.pool)
        .await?;
        invalidate_cookie_snapshots();
        Ok(r.rows_affected())
    }

    /// 记录书源 user_agent（FlareSolverr 返回 UA 与库中不同时更新——部分站点 UA 绑定 cookie）
    pub async fn set_cookie_user_agent(
        &self,
        ns: &str,
        source_url: &str,
        user_agent: &str,
    ) -> Result<()> {
        if user_agent.trim().is_empty() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO book_source_cookies (user_namespace, source_url, cookie, user_agent, updated_at)
             VALUES (?1, ?2, '', ?3, ?4)
             ON CONFLICT(user_namespace, source_url) DO UPDATE SET user_agent = ?3",
        )
        .bind(ns)
        .bind(source_url)
        .bind(user_agent)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        invalidate_cookie_snapshots();
        Ok(())
    }

    /// 读取书源登录态会话（cookie + user_agent）
    pub async fn get_source_session(
        &self,
        ns: &str,
        source_url: &str,
    ) -> Result<Option<(String, String)>> {
        let r: Option<(String, String)> = sqlx::query_as(
            "SELECT cookie, user_agent FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r)
    }

    /// 按 baseUrl 匹配书源 cookie（crawler 抓取用：请求 URL 的 base 与书源
    /// source_url 的 base 一致即命中——source_url 可能带 `##` 备用地址后缀）。
    /// 仅查本命名空间（书源 cookie 按用户隔离）。走 cookie 快照缓存（写路径会立刻失效）。
    pub async fn get_cookie_by_base(&self, ns: &str, base_url: &str) -> Result<Option<String>> {
        let Some(target) = normalize_base(base_url) else {
            return Ok(None);
        };
        Ok(self.cookie_snapshot(ns).await?.cookie_for(&target))
    }

    /// 读取书源登录头（legacy putLoginHeader 持久化，按用户 + source_url 键）
    pub async fn get_login_header(&self, ns: &str, source_url: &str) -> Result<Option<String>> {
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT login_header FROM book_source_cookies \
             WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|x| x.0).filter(|h| !h.trim().is_empty()))
    }

    /// 写入书源登录头（INSERT OR REPLACE 保留既有 cookie/UA；空值等价清除）
    pub async fn set_login_header(&self, ns: &str, source_url: &str, header: &str) -> Result<()> {
        if header.trim().is_empty() {
            let r = sqlx::query(
                "UPDATE book_source_cookies SET login_header = '' \
                 WHERE user_namespace = ?1 AND source_url = ?2",
            )
            .bind(ns)
            .bind(source_url)
            .execute(&self.pool)
            .await?;
            if r.rows_affected() == 0 {
                sqlx::query(
                    "INSERT INTO book_source_cookies \
                     (user_namespace, source_url, cookie, user_agent, login_header, updated_at) \
                     VALUES (?1, ?2, '', '', '', ?3)",
                )
                .bind(ns)
                .bind(source_url)
                .bind(chrono::Utc::now().timestamp_millis())
                .execute(&self.pool)
                .await?;
            }
            invalidate_cookie_snapshots();
            return Ok(());
        }
        sqlx::query(
            "INSERT OR REPLACE INTO book_source_cookies \
             (user_namespace, source_url, cookie, user_agent, login_header, updated_at) \
             VALUES (?1, ?2, \
                     COALESCE((SELECT cookie FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2), ''), \
                     COALESCE((SELECT user_agent FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2), ''), \
                     ?3, ?4)",
        )
        .bind(ns)
        .bind(source_url)
        .bind(header)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        invalidate_cookie_snapshots();
        Ok(())
    }

    /// 按 baseUrl 匹配书源登录头（crawler 抓取用；同 get_cookie_by_base 的 `##` 备地址语义）
    pub async fn get_login_header_by_base(
        &self,
        ns: &str,
        base_url: &str,
    ) -> Result<Option<String>> {
        let Some(target) = normalize_base(base_url) else {
            return Ok(None);
        };
        Ok(self.cookie_snapshot(ns).await?.header_for(&target))
    }

    /// 按 baseUrl 匹配书源会话（cookie + user_agent；`##` 备地址语义，空值也算命中——
    /// 与 crawler `session_for` 的历史语义一致），走 cookie 快照缓存。
    pub async fn get_session_by_base(
        &self,
        ns: &str,
        base_url: &str,
    ) -> Result<Option<(String, String)>> {
        let Some(target) = normalize_base(base_url) else {
            return Ok(None);
        };
        Ok(self.cookie_snapshot(ns).await?.session(&target))
    }

    // ---------------- RSS ----------------

    /// RSS 源列表（按命名空间；无则回退 default，同 get_book_sources 语义）
    pub async fn get_rss_sources(&self, ns: &str) -> Result<Vec<crate::model::RssSource>> {
        let rows = sqlx::query_as::<_, crate::model::RssSource>(
            "SELECT * FROM rss_sources WHERE user_namespace = ?1 ORDER BY rss_source_name",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        if !rows.is_empty() || ns == "default" {
            return Ok(rows);
        }
        sqlx::query_as::<_, crate::model::RssSource>(
            "SELECT * FROM rss_sources WHERE user_namespace = 'default' ORDER BY rss_source_name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 按 URL 查 RSS 源（用户命名空间 + default 回退）
    pub async fn find_rss_source(
        &self,
        ns: &str,
        source_url: &str,
    ) -> Result<Option<crate::model::RssSource>> {
        let r = sqlx::query_as::<_, crate::model::RssSource>(
            "SELECT * FROM rss_sources WHERE user_namespace = ?1 AND rss_source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await?;
        if r.is_some() || ns == "default" {
            return Ok(r);
        }
        sqlx::query_as::<_, crate::model::RssSource>(
            "SELECT * FROM rss_sources WHERE user_namespace = 'default' AND rss_source_url = ?1",
        )
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 保存 RSS 源（INSERT OR REPLACE；raw_json 存完整 JSON 原文）
    pub async fn save_rss_source(&self, ns: &str, source: &crate::model::RssSource) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO rss_sources            (rss_source_url, rss_source_name, rss_source_group, enabled, user_namespace, raw_json)            VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&source.source_url)
        .bind(&source.source_name)
        .bind(&source.source_group)
        .bind(source.enabled)
        .bind(ns)
        .bind(&source.raw_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 删除 RSS 源（按 URL，仅限本命名空间）；返回受影响行数
    pub async fn delete_rss_source(&self, ns: &str, source_url: &str) -> Result<u64> {
        let r = sqlx::query(
            "DELETE FROM rss_sources WHERE user_namespace = ?1 AND rss_source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 批量保存 RSS 文章（单事务，INSERT ... ON CONFLICT 按 url 主键去重更新；
    /// 更新时不触碰 read 列——feed 刷新重新入库不清除已读标记）
    pub async fn save_rss_articles(
        &self,
        ns: &str,
        articles: &[crate::model::RssArticle],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for a in articles {
            sqlx::query(
                "INSERT INTO rss_articles            (url, source_url, title, author, time, content, cover, user_namespace, read)            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)            ON CONFLICT(url, user_namespace) DO UPDATE SET                source_url = excluded.source_url,                title = excluded.title,                author = excluded.author,                time = excluded.time,                content = excluded.content,                cover = excluded.cover,                user_namespace = excluded.user_namespace",
            )
            .bind(&a.url)
            .bind(&a.source_url)
            .bind(&a.title)
            .bind(&a.author)
            .bind(a.time)
            .bind(&a.content)
            .bind(&a.cover)
            .bind(ns)
            .bind(a.read)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 标记 RSS 文章已读/未读（按 (ns, url) 查改——P0-4 命名空间隔离；返回受影响行数）
    pub async fn set_rss_article_read(&self, ns: &str, url: &str, read: bool) -> Result<u64> {
        let r =
            sqlx::query("UPDATE rss_articles SET read = ?2 WHERE url = ?1 AND user_namespace = ?3")
                .bind(url)
                .bind(read)
                .bind(ns)
                .execute(&self.pool)
                .await?;
        Ok(r.rows_affected())
    }

    /// 某 RSS 源全部文章的已读标记（url → read），getRssArticles 返回时合并用
    pub async fn get_rss_article_read_flags(
        &self,
        ns: &str,
        source_url: &str,
    ) -> Result<std::collections::HashMap<String, bool>> {
        let rows: Vec<(String, bool)> = sqlx::query_as(
            "SELECT url, read FROM rss_articles WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// 按 (ns, url) 查 RSS 文章（P0-4 命名空间隔离——getRssArticle 正文/缓存用）
    pub async fn get_rss_article(
        &self,
        ns: &str,
        url: &str,
    ) -> Result<Option<crate::model::RssArticle>> {
        let r = sqlx::query_as::<_, crate::model::RssArticle>(
            "SELECT * FROM rss_articles WHERE url = ?1 AND user_namespace = ?2",
        )
        .bind(url)
        .bind(ns)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r)
    }

    // ---------------- 缓存管理 ----------------

    /// 缓存统计（按命名空间隔离）：toc_cache 行数 / book_chapters 行数 /
    /// 章节正文近似大小（sum length(content)）/ 目录缓存大小（sum length(chapters_json)）/
    /// 总大小（两者之和）——只统计 ns 名下缓存，不泄露其它用户用量
    pub async fn get_cache_info(&self, ns: &str) -> Result<CacheInfo> {
        let toc_cache_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM toc_cache WHERE user_namespace = ?1")
                .bind(ns)
                .fetch_one(&self.pool)
                .await?;
        let toc_cache_size: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(length(chapters_json)), 0) FROM toc_cache WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        let chapter_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM book_chapters WHERE user_namespace = ?1")
                .bind(ns)
                .fetch_one(&self.pool)
                .await?;
        let chapter_size: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(length(content)), 0) FROM book_chapters WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        Ok(CacheInfo {
            toc_cache_count,
            toc_cache_size,
            chapter_count,
            chapter_size,
            total_size: toc_cache_size + chapter_size,
        })
    }

    /// 清空某命名空间缓存（type: "toc" 清目录缓存 / "chapters" 清章节缓存 / "all" 全清）；
    /// 返回 (toc 删除行数, 章节删除行数)——只删 ns 名下缓存，不跨用户清空
    pub async fn clear_cache(&self, ns: &str, cache_type: &str) -> Result<(u64, u64)> {
        let mut toc_deleted = 0u64;
        let mut chapters_deleted = 0u64;
        if cache_type == "toc" || cache_type == "all" {
            let r = sqlx::query("DELETE FROM toc_cache WHERE user_namespace = ?1")
                .bind(ns)
                .execute(&self.pool)
                .await?;
            toc_deleted = r.rows_affected();
        }
        if cache_type == "chapters" || cache_type == "all" {
            let r = sqlx::query("DELETE FROM book_chapters WHERE user_namespace = ?1")
                .bind(ns)
                .execute(&self.pool)
                .await?;
            chapters_deleted = r.rows_affected();
        }
        Ok((toc_deleted, chapters_deleted))
    }

    // ---------------- 全书搜索（本地书） ----------------

    /// 某书在 book_chapters 表中的章节数（本地书判定用）——P0 按命名空间隔离
    pub async fn count_chapters(&self, ns: &str, book_url: &str) -> Result<i64> {
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    /// 删除单书缓存（book_chapters 该 book_url 行——本地书章节 + 书源书正文缓存）；
    /// 不影响书架 books 行。返回删除行数——P0 按命名空间隔离
    pub async fn delete_book_cache(&self, ns: &str, book_url: &str) -> Result<u64> {
        let r =
            sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
                .bind(book_url)
                .bind(ns)
                .execute(&self.pool)
                .await?;
        Ok(r.rows_affected())
    }

    /// 单书缓存信息：(章节数, 正文近似大小 sum length(content))——P0 按命名空间隔离
    pub async fn book_cache_info(&self, ns: &str, book_url: &str) -> Result<(i64, i64)> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        let size: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(length(content)), 0) FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        Ok((count, size))
    }

    /// 全书搜索：book_chapters 正文 LIKE 匹配（key 中 %/_ 转义为字面量），按章节序返回
    /// 命中章节（chapterIndex/title/snippet——命中段落前后截取），最多 limit 条
    pub async fn search_book_content(
        &self,
        ns: &str,
        book_url: &str,
        key: &str,
        limit: i64,
    ) -> Result<Vec<BookContentHit>> {
        let escaped = key
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chapter_index, title, content FROM book_chapters             WHERE book_url = ?1 AND user_namespace = ?2 AND content LIKE ?3 ESCAPE '\\'             ORDER BY chapter_index LIMIT ?4",
        )
        .bind(book_url)
        .bind(ns)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut hits = Vec::with_capacity(rows.len());
        for (chapter_index, title, content) in rows {
            hits.push(BookContentHit {
                chapter_index,
                title,
                snippet: make_snippet(&content, key, 40),
            });
        }
        Ok(hits)
    }

    // ---------------- 书源订阅 ----------------

    /// 订阅列表（按名称排序；用户无订阅回退 default，同书源语义）
    pub async fn get_source_subs(&self, ns: &str) -> Result<Vec<crate::model::SourceSub>> {
        let rows = sqlx::query_as::<_, crate::model::SourceSub>(
            "SELECT * FROM source_subs WHERE user_namespace = ?1 ORDER BY name, url",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        if ns == "default" {
            return Ok(rows.into_iter().filter(|s| !s.hidden).collect());
        }
        let default_rows = sqlx::query_as::<_, crate::model::SourceSub>(
            "SELECT * FROM source_subs WHERE user_namespace = 'default' ORDER BY name, url",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut merged: Vec<crate::model::SourceSub> = rows;
        let owned: std::collections::HashSet<String> =
            merged.iter().map(|s| s.url.clone()).collect();
        for s in default_rows {
            if !owned.contains(&s.url) {
                merged.push(s);
            }
        }
        merged.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.url.cmp(&b.url)));
        Ok(merged.into_iter().filter(|s| !s.hidden).collect())
    }

    /// 按 URL 查订阅（用户命名空间 + default 回退）
    pub async fn find_source_sub(
        &self,
        ns: &str,
        url: &str,
    ) -> Result<Option<crate::model::SourceSub>> {
        let r = sqlx::query_as::<_, crate::model::SourceSub>(
            "SELECT * FROM source_subs WHERE user_namespace = ?1 AND url = ?2 AND hidden = 0",
        )
        .bind(ns)
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;
        if r.is_some() || ns == "default" {
            return Ok(r);
        }
        // 用户已有该订阅的私有行（含 hidden 删除覆盖）时不得回退 default
        let overlay = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM source_subs WHERE user_namespace = ?1 AND url = ?2",
        )
        .bind(ns)
        .bind(url)
        .fetch_one(&self.pool)
        .await?;
        if overlay > 0 {
            return Ok(None);
        }
        sqlx::query_as::<_, crate::model::SourceSub>(
            "SELECT * FROM source_subs WHERE user_namespace = 'default' AND url = ?1 AND hidden = 0",
        )
        .bind(url)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 保存订阅（INSERT OR REPLACE，按 url 主键覆盖；raw_json 存书源数组 JSON 原文）
    pub async fn save_source_sub(
        &self,
        ns: &str,
        url: &str,
        name: &str,
        raw_json: &str,
        selected_urls: &[String],
    ) -> Result<()> {
        let selected_json =
            serde_json::to_string(selected_urls).unwrap_or_else(|_| "[]".to_string());
        sqlx::query(
            "INSERT OR REPLACE INTO source_subs (url, name, enabled, hidden, user_namespace, raw_json, selected_urls)             VALUES (?1, ?2, 1, 0, ?3, ?4, ?5)",
        )
        .bind(url)
        .bind(name)
        .bind(ns)
        .bind(raw_json)
        .bind(selected_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 启停订阅：禁用后定时任务跳过（保留记录与已导入书源）。
    /// 普通用户操作 default 系统订阅时复制到本人命名空间（个人启停覆盖，不影响系统订阅）；
    /// 本人自有订阅直接更新。
    pub async fn set_source_sub_enabled(&self, ns: &str, url: &str, enabled: bool) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        if ns != "default" {
            let owned = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM source_subs WHERE user_namespace = ?1 AND url = ?2",
            )
            .bind(ns)
            .bind(url)
            .fetch_one(&mut *tx)
            .await?;
            if owned == 0 {
                if let Some(default_sub) =
                    self.find_source_sub_row(&mut *tx, "default", url).await?
                {
                    let mut copy = default_sub;
                    copy.user_namespace = ns.to_string();
                    copy.hidden = false;
                    copy.enabled = enabled;
                    sqlx::query(
                        "INSERT OR REPLACE INTO source_subs (url, name, enabled, hidden, user_namespace, raw_json, selected_urls)                         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
                    )
                    .bind(&copy.url)
                    .bind(&copy.name)
                    .bind(enabled as i64)
                    .bind(ns)
                    .bind(&copy.raw_json)
                    .bind(&copy.selected_urls)
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                return Ok(());
            }
        }
        sqlx::query(
            "UPDATE source_subs SET enabled = ?1, hidden = 0 WHERE user_namespace = ?2 AND url = ?3",
        )
        .bind(enabled as i64)
        .bind(ns)
        .bind(url)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 删除订阅（按 url）。普通用户删除 default 系统订阅时复制到本人命名空间并隐藏——
    /// 只对本人生效，系统订阅保留；本人自有订阅则直接删除。返回受影响行数
    pub async fn delete_source_sub(&self, ns: &str, url: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "DELETE FROM source_subs WHERE user_namespace = ?1 AND url = ?2 AND hidden = 0",
        )
        .bind(ns)
        .bind(url)
        .execute(&mut *tx)
        .await?;
        let mut affected = r.rows_affected();
        if ns != "default" {
            if let Some(default_sub) = self.find_source_sub_row(&mut *tx, "default", url).await? {
                let mut copy = default_sub;
                copy.user_namespace = ns.to_string();
                copy.hidden = true;
                sqlx::query(
                    "INSERT OR REPLACE INTO source_subs (url, name, enabled, hidden, user_namespace, raw_json, selected_urls)                      VALUES (?1, ?2, 1, 1, ?3, ?4, ?5)",
                )
                .bind(&copy.url)
                .bind(&copy.name)
                .bind(ns)
                .bind(&copy.raw_json)
                .bind(&copy.selected_urls)
                .execute(&mut *tx)
                .await?;
                affected = 1;
            } else if affected == 0 {
                // 用户已有 hidden 覆盖且 default 订阅已被管理员删除：保持隐藏（幂等）
                let hidden_rows = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM source_subs WHERE user_namespace = ?1 \
                     AND url = ?2 AND hidden = 1",
                )
                .bind(ns)
                .bind(url)
                .fetch_one(&mut *tx)
                .await?;
                if hidden_rows > 0 {
                    affected = 1;
                }
            }
        }
        tx.commit().await?;
        Ok(affected)
    }

    /// 按 URL 查订阅行（供 copy-on-write 复制 default 系统订阅用）
    async fn find_source_sub_row<'e, E>(
        &self,
        executor: E,
        ns: &str,
        url: &str,
    ) -> Result<Option<crate::model::SourceSub>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        sqlx::query_as::<_, crate::model::SourceSub>(
            "SELECT * FROM source_subs WHERE user_namespace = ?1 AND url = ?2",
        )
        .bind(ns)
        .bind(url)
        .fetch_optional(executor)
        .await
        .map_err(Into::into)
    }

    /// 保存章节（本地书）
    pub async fn save_chapters(&self, book_url: &str, chapters: &[(String, String)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT OR REPLACE INTO book_chapters (book_url, chapter_index, title, content) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 本地书章节列表
    pub async fn list_chapters(&self, book_url: &str) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT chapter_index, title FROM book_chapters WHERE book_url = ?1 ORDER BY chapter_index",
        )
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 已缓存章节的「下标 + 标题」（不带正文）——P0 按命名空间隔离。
    /// 用途：目录离线兜底（源站不可达时用已缓存章节合成目录给阅读器）。
    pub async fn list_cached_chapter_headers(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT chapter_index, title FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2 ORDER BY chapter_index",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 形如 `{prefix}*` 的候选 URL（= book_vars_cache 的键）——目录离线兜底的 URL 复原用。
    /// 书源书的 `book_chapters.chapter_index` 是 chapterUrl 的 md5 哈希（不可逆），只有 URL
    /// 原文能重算哈希；而 analyze_toc/analyze_content 都会按章节 URL 写一行变量缓存，
    /// 所以这是全库唯一存着章节 URL 原文的地方。prefix 中的 LIKE 通配符按字面量转义。
    pub async fn list_urls_under_prefix(&self, ns: &str, prefix: &str) -> Result<Vec<String>> {
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT url FROM book_vars_cache WHERE user_namespace = ?1 AND url LIKE ?2 ESCAPE '\\'",
        )
        .bind(ns)
        .bind(format!("{escaped}%"))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 命名空间内某书章节行数（saveBookProgress 越界校验用——ns 安全）
    pub async fn count_book_chapters(&self, ns: &str, book_url: &str) -> Result<i64> {
        let r: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        Ok(r.0)
    }

    /// 本地书章节列表（含字数：SQLite length() 对 TEXT 按字符数统计正文，避免整章内容回传）
    pub async fn list_chapters_with_word_count(
        &self,
        book_url: &str,
    ) -> Result<Vec<(i64, String, i64)>> {
        let rows = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT chapter_index, title, length(content) FROM book_chapters WHERE book_url = ?1 ORDER BY chapter_index",
        )
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 章节正文（P0 跨用户隔离：按 user_namespace 过滤）
    pub async fn get_chapter_content(
        &self,
        ns: &str,
        book_url: &str,
        index: i64,
    ) -> Result<Option<String>> {
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT content FROM book_chapters WHERE book_url = ?1 AND chapter_index = ?2 AND user_namespace = ?3",
        )
        .bind(book_url)
        .bind(index)
        .bind(ns)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|x| x.0))
    }

    /// 单书已缓存章节（含正文；供客户端从服务器拉取离线缓存）——P0 按命名空间隔离
    pub async fn list_cached_chapters(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<(i64, String, String)>> {
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chapter_index, title, content FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2 ORDER BY chapter_index",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 书源书正文缓存写回（chapter_index = chapterUrl md5 哈希；与本地书顺序索引键域不重叠）
    /// P0 跨用户隔离：缓存行按 user_namespace 隔离
    pub async fn cache_chapter_content(
        &self,
        ns: &str,
        book_url: &str,
        index: i64,
        title: &str,
        content: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO book_chapters (book_url, chapter_index, title, content, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(book_url)
        .bind(index)
        .bind(title)
        .bind(content)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 批量写章节正文（单事务）：逐章 autocommit 实测 4.5–4.7 ms/行，
    /// 打包成事务后 0.03–0.11 ms/行（43–139×）。
    pub async fn cache_chapter_contents_batch(
        &self,
        ns: &str,
        book_url: &str,
        items: &[(i64, String, String)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for (index, title, content) in items {
            sqlx::query(
                "INSERT OR REPLACE INTO book_chapters (book_url, chapter_index, title, content, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(book_url)
            .bind(index)
            .bind(title)
            .bind(content)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---------------- 关系图谱 B 阶段（整本批量生成） ----------------
    // 表：book_graph_jobs（任务进度）/ book_graph_batches（逐批原始输出）/ book_graphs（每书成品）

    /// 建任务行（同 job_id 覆盖）：status=queued、进度归零
    pub async fn graph_job_create(
        &self,
        job_id: &str,
        ns: &str,
        book_url: &str,
        total_batches: i64,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR REPLACE INTO book_graph_jobs \
             (job_id, book_url, user_namespace, status, total_batches, done_batches, chapters_scanned, \
              prompt_tokens, completion_tokens, error, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 'queued', ?4, 0, 0, 0, 0, NULL, ?5, ?5)",
        )
        .bind(job_id)
        .bind(book_url)
        .bind(ns)
        .bind(total_batches)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写任务状态（queued/running/done/failed/cancelled）+ 错误信息
    pub async fn graph_job_set_status(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE book_graph_jobs SET status = ?2, error = ?3, updated_at = ?4 WHERE job_id = ?1",
        )
        .bind(job_id)
        .bind(status)
        .bind(error)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写任务进度（批进度 + 已扫章数 + token 估算）
    pub async fn graph_job_set_progress(
        &self,
        job_id: &str,
        done_batches: i64,
        chapters_scanned: i64,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE book_graph_jobs SET done_batches = ?2, chapters_scanned = ?3, \
             prompt_tokens = ?4, completion_tokens = ?5, updated_at = ?6 WHERE job_id = ?1",
        )
        .bind(job_id)
        .bind(done_batches)
        .bind(chapters_scanned)
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 任务快照：(status, total_batches, done_batches, error)
    pub async fn graph_job_snapshot(
        &self,
        job_id: &str,
    ) -> Result<Option<(String, i64, i64, Option<String>)>> {
        let row = sqlx::query_as::<_, (String, i64, i64, Option<String>)>(
            "SELECT status, total_batches, done_batches, error FROM book_graph_jobs WHERE job_id = ?1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// 本书未完成的任务（running/queued）——重启后可续跑 / 避免重复开跑
    pub async fn graph_job_unfinished(&self, ns: &str, book_url: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT job_id FROM book_graph_jobs \
             WHERE user_namespace = ?1 AND book_url = ?2 AND status IN ('queued','running') \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// 逐批原始输出落库（一批一行，续跑时跳过已成功的批）
    pub async fn graph_batch_put(&self, job_id: &str, batch_no: i64, raw_json: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO book_graph_batches (job_id, batch_no, raw_json, finished_at) \
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(job_id)
        .bind(batch_no)
        .bind(raw_json)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读回某任务的全部批次（按 batch_no 升序）
    pub async fn graph_batches(&self, job_id: &str) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT batch_no, raw_json FROM book_graph_batches WHERE job_id = ?1 ORDER BY batch_no",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 成品落库（每书一份，覆盖写）
    #[allow(clippy::too_many_arguments)]
    pub async fn graph_save(
        &self,
        ns: &str,
        book_url: &str,
        data_json: &str,
        characters: i64,
        relations: i64,
        chapters_scanned: i64,
        job_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO book_graphs \
             (book_url, user_namespace, data_json, source, characters, relations, chapters_scanned, job_id, updated_at) \
             VALUES (?1, ?2, ?3, 'batch', ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(book_url)
        .bind(ns)
        .bind(data_json)
        .bind(characters)
        .bind(relations)
        .bind(chapters_scanned)
        .bind(job_id)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 成品里记的「已扫描章数」（没成品返回 0）——用来判断台账是否比成品新
    pub async fn graph_chapters_scanned(&self, ns: &str, book_url: &str) -> Result<i64> {
        let scanned = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT chapters_scanned FROM book_graphs WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        Ok(scanned.unwrap_or(0))
    }

    /// 读成品 JSON（没生成过返回 None）
    pub async fn graph_get(&self, ns: &str, book_url: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT data_json FROM book_graphs WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    // ---- 增量生成：章节覆盖台账 + 批次沉淀 ----

    /// 读台账：该书每个已抽取章的 (chapter_key, ordinal, title, content_hash, done_at)
    /// （`done_at` 是毫秒时间戳，NULL 视作 0 = 最久远，排在最前面被优先补抽）
    pub async fn graph_ledger(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<(i64, i64, String, i64, i64)>> {
        let rows = sqlx::query_as::<_, (i64, i64, String, i64, Option<i64>)>(
            "SELECT chapter_key, ordinal, title, content_hash, done_at FROM book_graph_chapters \
             WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(k, o, t, h, d)| (k, o, t, h, d.unwrap_or(0)))
            .collect())
    }

    /// 台账覆盖章数（「台账覆盖数没变」的判定依据）
    pub async fn graph_ledger_count(&self, ns: &str, book_url: &str) -> Result<i64> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM book_graph_chapters WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    /// 一批成功后**一次性**落 unit + 标台账（同一事务：要么都记、要么都不记）。
    ///
    /// `chapters` = (chapter_key, ordinal, title, content_hash)。台账写 `done_at`/`job_id`，
    /// 重复标同一章是幂等的（`INSERT OR REPLACE`，不会多出行）。
    /// 失败/取消的批不会走到这里，下次自然重抽这一批。
    pub async fn graph_unit_commit(
        &self,
        ns: &str,
        book_url: &str,
        job_id: &str,
        seq: i64,
        raw_json: &str,
        chapters: &[(i64, i64, String, i64)],
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let keys: Vec<serde_json::Value> = chapters
            .iter()
            .map(|(key, ..)| serde_json::json!(key))
            .collect();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR REPLACE INTO book_graph_units \
             (book_url, user_namespace, seq, raw_json, chapter_keys, job_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(book_url)
        .bind(ns)
        .bind(seq)
        .bind(raw_json)
        .bind(serde_json::Value::Array(keys).to_string())
        .bind(job_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        for (key, ordinal, title, content_hash) in chapters {
            sqlx::query(
                "INSERT OR REPLACE INTO book_graph_chapters \
                 (book_url, user_namespace, chapter_key, ordinal, title, content_hash, done_at, job_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(book_url)
            .bind(ns)
            .bind(key)
            .bind(ordinal)
            .bind(title)
            .bind(content_hash)
            .bind(now)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 该书已沉淀的最大 unit 序号（没有 unit 返回 0）
    pub async fn graph_unit_max_seq(&self, ns: &str, book_url: &str) -> Result<i64> {
        let seq = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(seq) FROM book_graph_units WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_one(&self.pool)
        .await?;
        Ok(seq.unwrap_or(0))
    }

    /// 读该书**全部** unit 的原始输出（按 seq 升序）——归并吃历史 + 本次
    pub async fn graph_units(&self, ns: &str, book_url: &str) -> Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT raw_json FROM book_graph_units WHERE user_namespace = ?1 AND book_url = ?2 \
             ORDER BY seq",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 删掉这本书的全部 unit + 台账（「全量重来」用；当前无生产调用点，供排查/测试）
    pub async fn graph_ledger_reset(&self, ns: &str, book_url: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for table in ["book_graph_units", "book_graph_chapters"] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE user_namespace = ?1 AND book_url = ?2"
            ))
            .bind(ns)
            .bind(book_url)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 删除本地书（含章节）
    /// 安全：book_chapters 无命名空间列——按书架归属（books.user_namespace）过滤后再删，
    /// 防止跨用户删他人本地书缓存（P1-C1）
    /// P3-A 标注：当前无生产调用点——本地书删除走 [`Self::delete_book`]（P1-C1 已含章节
    /// 清理）；本函数保留为 storage 层原语（ns 隔离校验 + 测试覆盖），供后续
    /// 本地书专属删除 API 复用。
    pub async fn delete_local_book(&self, ns: &str, book_url: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM book_chapters WHERE book_url = ?1              AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
        )
        .bind(book_url)
        .bind(ns)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM books WHERE user_namespace = ?1 AND book_url = ?2")
            .bind(ns)
            .bind(book_url)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 删除书（书源书或本地书——本地书含章节）
    /// GAP 117：删除后清理 assets/{ns}/covers 下对应封面文件（未被其他书引用时）
    pub async fn delete_book(&self, ns: &str, book_url: &str) -> Result<u64> {
        let cover_url = sqlx::query_scalar::<_, Option<String>>(
            "SELECT cover_url FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        let mut tx = self.pool.begin().await?;
        // P1-C1：book_chapters/toc_cache 无命名空间列——按书架归属过滤后删（跨用户删除防护）
        sqlx::query(
            "DELETE FROM book_chapters WHERE book_url = ?1              AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
        )
        .bind(book_url)
        .bind(ns)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM toc_cache WHERE book_url = ?1              AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
        )
        .bind(book_url)
        .bind(ns)
        .execute(&mut *tx)
        .await?;
        let r = sqlx::query("DELETE FROM books WHERE user_namespace = ?1 AND book_url = ?2")
            .bind(ns)
            .bind(book_url)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if r.rows_affected() > 0 {
            self.cleanup_orphan_cover(ns, cover_url.as_deref()).await;
        }
        Ok(r.rows_affected())
    }

    /// GAP 117：封面文件残留清理——cover_url 形如 /assets/{ns}/covers/{file}，
    /// 且删除后无其他书引用同一路径时删除文件（最佳努力，失败仅告警）
    async fn cleanup_orphan_cover(&self, ns: &str, cover_url: Option<&str>) {
        let Some(cover_url) = cover_url else { return };
        let Some(file) = cover_file_name(ns, cover_url) else {
            return;
        };
        // 其他书（本命名空间内）是否仍引用同一封面
        let refs: i64 = match sqlx::query_scalar(
            "SELECT COUNT(*) FROM books WHERE user_namespace = ?1 AND cover_url = ?2",
        )
        .bind(ns)
        .bind(cover_url)
        .fetch_one(&self.pool)
        .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("cleanup_orphan_cover 查询失败: {e}");
                return;
            }
        };
        if refs > 0 {
            return;
        }
        let path = self
            .config
            .storage_dir()
            .join("assets")
            .join(ns)
            .join("covers")
            .join(&file);
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("封面清理失败 {}: {e}", path.display());
            }
        } else {
            tracing::info!("GAP 117 已清理无引用封面 {}", path.display());
        }
    }

    /// 更新书字段（编辑：name/author/coverUrl/group）
    pub async fn update_book(
        &self,
        ns: &str,
        book_url: &str,
        name: Option<&str>,
        author: Option<&str>,
        cover_url: Option<&str>,
        group: Option<i64>,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE books SET name = COALESCE(?3, name), author = COALESCE(?4, author),              cover_url = COALESCE(?5, cover_url), group_name = COALESCE(?6, group_name)              WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(name)
        .bind(author)
        .bind(cover_url)
        .bind(group)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 按 URL 查书架书（saveBook 新增/编辑判断；不存在返回 None）
    /// 换源持久化（legacy setBookSource）：更新书架书的 origin/originName/bookUrl/tocUrl，
    /// 封面仅原为空时补（legacy editShelfBook 语义）；旧 URL 的章节/目录缓存清理
    /// （新源目录按需重建）。阅读进度在 books 行上天然保留。返回受影响行数。
    #[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
    pub async fn switch_book_source(
        &self,
        ns: &str,
        old_url: &str,
        new_url: &str,
        origin: &str,
        origin_name: &str,
        toc_url: &str,
        cover_url: Option<&str>,
    ) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        if old_url != new_url {
            // 旧 URL 章节表/目录缓存清理——必须在 UPDATE 前做（守卫子查询依赖旧 URL 仍归属本用户）
            sqlx::query(
                "DELETE FROM book_chapters WHERE book_url = ?1                  AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
            )
            .bind(old_url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM toc_cache WHERE book_url = ?1                  AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
            )
            .bind(old_url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        let r = sqlx::query(
            "UPDATE books SET origin = ?3, origin_name = ?4, book_url = ?5, toc_url = ?6,              cover_url = CASE WHEN (cover_url IS NULL OR cover_url = '') THEN ?7 ELSE cover_url END              WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(old_url)
        .bind(origin)
        .bind(origin_name)
        .bind(new_url)
        .bind(toc_url)
        .bind(cover_url)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(r.rows_affected())
    }

    pub async fn find_book(&self, ns: &str, book_url: &str) -> Result<Option<Book>> {
        let book = sqlx::query_as::<_, Book>(
            "SELECT * FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(book)
    }

    /// 按 书名+作者 查书架书（legacy saveBookToShelf 判重键——同书不同 URL 视为同一本）
    pub async fn find_book_by_name_author(
        &self,
        ns: &str,
        name: &str,
        author: &str,
    ) -> Result<Option<Book>> {
        let book = sqlx::query_as::<_, Book>(
            "SELECT books.*, books.rowid AS rowid FROM books              WHERE user_namespace = ?1 AND name = ?2 AND author = ?3              ORDER BY rowid LIMIT 1",
        )
        .bind(ns)
        .bind(name)
        .bind(author)
        .fetch_optional(&self.pool)
        .await?;
        Ok(book)
    }

    /// saveBook 全量入架/覆盖：INSERT OR REPLACE（不存在则新增，存在则全字段更新）
    pub async fn upsert_book(&self, ns: &str, book: &Book) -> Result<()> {
        let mut b = book.clone();
        b.user_namespace = ns.to_string();
        // GAP 170 双轨同步：local_file 为服务端内部字段（客户端 saveBook 不带）——
        // 旧客户端全量覆盖时保留既有文件关联，避免打断文件↔DB 双轨
        let local_file = if b.local_file.is_some() {
            b.local_file.clone()
        } else {
            self.find_book(ns, &b.book_url)
                .await
                .ok()
                .flatten()
                .and_then(|old| old.local_file)
        };
        sqlx::query(
            r#"INSERT OR REPLACE INTO books
            (book_url, name, author, origin, origin_name, toc_url, kind, custom_tag, cover_url,
             custom_cover_url, intro, custom_intro, charset, type, group_name, group_ids,
             latest_chapter_title, latest_chapter_time, last_check_time, last_check_count,
             total_chapter_num, dur_chapter_title, dur_chapter_index, dur_chapter_pos,
             dur_chapter_time, word_count, can_update, order_num, origin_order,
             use_replace_rule, variable, read_config, is_in_shelf, cbz, display_cover,
             display_intro, local_epub, local_pdf, pdf, split_long_chapter,
             last_check_error, info_html, toc_html, language, publisher, published_at,
             user_namespace, created_at, raw_json, local_file, local_file_mtime,
             local_file_size, local_file_deleted)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                    ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40,
                    ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, ?50, ?51, ?52, ?53)"#,
        )
        .bind(&b.book_url)
        .bind(&b.name)
        .bind(&b.author)
        .bind(&b.origin)
        .bind(&b.origin_name)
        .bind(&b.toc_url)
        .bind(&b.kind)
        .bind(&b.custom_tag)
        .bind(&b.cover_url)
        .bind(&b.custom_cover_url)
        .bind(&b.intro)
        .bind(&b.custom_intro)
        .bind(&b.charset)
        .bind(b.book_type)
        .bind(b.group)
        .bind(&b.group_ids)
        .bind(&b.latest_chapter_title)
        .bind(b.latest_chapter_time)
        .bind(b.last_check_time)
        .bind(b.last_check_count)
        .bind(b.total_chapter_num)
        .bind(&b.dur_chapter_title)
        .bind(b.dur_chapter_index)
        .bind(b.dur_chapter_pos)
        .bind(b.dur_chapter_time)
        .bind(&b.word_count)
        .bind(b.can_update)
        .bind(b.order)
        .bind(b.origin_order)
        .bind(b.use_replace_rule)
        .bind(&b.variable)
        .bind(b.read_config.as_ref().map(|v| v.to_string()))
        .bind(b.is_in_shelf)
        .bind(b.cbz)
        .bind(&b.display_cover)
        .bind(&b.display_intro)
        .bind(b.local_epub)
        .bind(b.local_pdf)
        .bind(b.pdf)
        .bind(b.split_long_chapter)
        .bind(&b.last_check_error)
        .bind(&b.info_html)
        .bind(&b.toc_html)
        .bind(&b.language)
        .bind(&b.publisher)
        .bind(&b.published_at)
        .bind(&b.user_namespace)
        .bind(b.created_at)
        .bind(&b.raw_json)
        // GAP 170 双轨同步：local_file 为服务端内部字段（客户端 saveBook 不带）——
        // 旧客户端全量覆盖时保留既有文件关联，避免打断文件↔DB 双轨
        .bind(local_file)
        .bind(b.local_file_mtime)
        .bind(b.local_file_size)
        .bind(b.local_file_deleted)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// saveBook 增量更新：按请求 JSON 中出现的字段（camelCase 键）动态 UPDATE（编辑场景，
    /// 未提供的字段保持不变；列名来自固定映射表，无注入风险）
    pub async fn patch_book(
        &self,
        ns: &str,
        book_url: &str,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64> {
        let mut qb = sqlx::QueryBuilder::new("UPDATE books SET ");
        let mut first = true;
        let mut any = false;
        for (key, value) in patch {
            let Some(col) = BOOK_PATCH_COLUMNS
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, c)| *c)
            else {
                continue;
            };
            if !first {
                qb.push(", ");
            }
            qb.push(col).push(" = ");
            push_book_patch_value(&mut qb, value);
            first = false;
            any = true;
        }
        if !any {
            return Ok(0);
        }
        qb.push(" WHERE user_namespace = ")
            .push_bind(ns)
            .push(" AND book_url = ")
            .push_bind(book_url);
        let r = qb.build().execute(&self.pool).await?;
        Ok(r.rows_affected())
    }

    /// F-8 保存阅读进度（durChapter* 字段；title 为 None 时保持原值）
    pub async fn update_book_progress(
        &self,
        ns: &str,
        book_url: &str,
        title: Option<&str>,
        index: i64,
        pos: i64,
        time: i64,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE books SET dur_chapter_title = COALESCE(?3, dur_chapter_title),              dur_chapter_index = ?4, dur_chapter_pos = ?5, dur_chapter_time = ?6              WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(title)
        .bind(index)
        .bind(pos)
        .bind(time)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// F-10 目录缓存写入（getBookToc 成功后调用）——P0 按命名空间隔离
    pub async fn cache_toc(
        &self,
        ns: &str,
        book_url: &str,
        toc_url: &str,
        chapters_json: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO toc_cache (book_url, toc_url, chapters_json, updated_at, user_namespace)              VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(book_url)
        .bind(toc_url)
        .bind(chapters_json)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(ns)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// F-10 目录缓存读取（同 tocUrl 直读；超过 max_age_ms 视为未命中）——P0 按命名空间隔离
    /// 清理某本书的目录缓存（缓存键可能是 book_url，也可能是解析出的 toc_url）。
    /// 用途：书架刷新拿到新章数后失效旧快照，避免"书架 438 章 / 阅读器只有 436 章"长期打架。
    pub async fn clear_toc_cache_for_book(&self, ns: &str, book_url: &str) -> Result<u64> {
        // 缓存键常是目录端点（`/toc/<id>`）而书是详情页（`/book/<id>`）——单靠等值匹配会漏，
        // 故再用 URL 末段 id 兜底（`%/ <id>` 形式，避免误伤同前缀的其它书）。
        let tail = book_url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        let r = if tail.is_empty() {
            sqlx::query(
                "DELETE FROM toc_cache WHERE user_namespace = ?1 AND (book_url = ?2 OR toc_url = ?2)",
            )
            .bind(ns)
            .bind(book_url)
            .execute(&self.pool)
            .await?
        } else {
            let pattern = format!("%/{tail}");
            sqlx::query(
                "DELETE FROM toc_cache WHERE user_namespace = ?1 \
                 AND (book_url = ?2 OR toc_url = ?2 OR book_url LIKE ?3 OR toc_url LIKE ?3)",
            )
            .bind(ns)
            .bind(book_url)
            .bind(&pattern)
            .execute(&self.pool)
            .await?
        };
        Ok(r.rows_affected())
    }

    pub async fn get_toc_cache(
        &self,
        ns: &str,
        toc_url: &str,
        max_age_ms: i64,
    ) -> Result<Option<String>> {
        let cutoff = chrono::Utc::now().timestamp_millis() - max_age_ms;
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT chapters_json FROM toc_cache WHERE toc_url = ?1 AND updated_at >= ?2 AND user_namespace = ?3",
        )
        .bind(toc_url)
        .bind(cutoff)
        .bind(ns)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|x| x.0))
    }

    /// 保存书签（INSERT OR REPLACE，主键 book_url+title）
    pub async fn save_bookmark(&self, ns: &str, bookmark: &crate::model::Bookmark) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO bookmarks (book_url, title, book_name, book_author,              paragraph_index, chapter_index, chapter_name, book_text, content,              created_at, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&bookmark.book_url)
        .bind(&bookmark.title)
        .bind(&bookmark.book_name)
        .bind(&bookmark.book_author)
        .bind(bookmark.paragraph_index)
        .bind(bookmark.chapter_index)
        .bind(&bookmark.chapter_name)
        .bind(&bookmark.book_text)
        .bind(&bookmark.content)
        .bind(bookmark.created_at)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 某书的书签列表（按创建时间倒序）
    pub async fn list_bookmarks(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<crate::model::Bookmark>> {
        let rows = sqlx::query_as::<_, crate::model::Bookmark>(
            "SELECT * FROM bookmarks WHERE user_namespace = ?1 AND book_url = ?2              ORDER BY created_at DESC, rowid DESC",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 删除书签（book_url + title）；返回受影响行数
    pub async fn delete_bookmark(&self, ns: &str, book_url: &str, title: &str) -> Result<u64> {
        let r = sqlx::query(
            "DELETE FROM bookmarks WHERE user_namespace = ?1 AND book_url = ?2 AND title = ?3",
        )
        .bind(ns)
        .bind(book_url)
        .bind(title)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 分组列表（按 order_num, id 排序）
    pub async fn list_book_groups(&self, ns: &str) -> Result<Vec<crate::model::BookGroup>> {
        let rows = sqlx::query_as::<_, crate::model::BookGroup>(
            "SELECT * FROM book_groups WHERE user_namespace = ?1 ORDER BY order_num, id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 保存分组：id > 0 按 id 覆盖，否则自增新建；返回带 id 的分组
    /// P1-C2：id > 0 时校验归属——该 id 已被其他命名空间占用则拒绝（防跨用户覆写）
    pub async fn save_book_group(
        &self,
        ns: &str,
        group: &crate::model::BookGroup,
    ) -> Result<crate::model::BookGroup> {
        let mut g = group.clone();
        g.user_namespace = ns.to_string();
        if g.id > 0 {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT user_namespace FROM book_groups WHERE id = ?1")
                    .bind(g.id)
                    .fetch_optional(&self.pool)
                    .await?;
            if let Some(owner) = owner {
                if owner != ns {
                    anyhow::bail!("分组不存在或无权操作");
                }
            }
            sqlx::query(
                "INSERT OR REPLACE INTO book_groups (id, name, cover, show, order_num, user_namespace)              VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(g.id)
            .bind(&g.name)
            .bind(&g.cover)
            .bind(g.show)
            .bind(g.order)
            .bind(ns)
            .execute(&self.pool)
            .await?;
        } else {
            let r = sqlx::query(
                "INSERT INTO book_groups (name, cover, show, order_num, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(&g.name)
            .bind(&g.cover)
            .bind(g.show)
            .bind(g.order)
            .bind(ns)
            .execute(&self.pool)
            .await?;
            g.id = r.last_insert_rowid();
        }
        Ok(g)
    }

    /// 书设分组（兼容单值：books.group_name = 分组 id，group_ids = [id]）；返回受影响行数
    pub async fn update_book_group_id(&self, ns: &str, book_url: &str, group: i64) -> Result<u64> {
        self.set_book_groups(ns, book_url, &[group]).await
    }

    /// 多分组设置（legacy saveBookGroupId 位掩码语义）：group_ids 存 JSON 数组，
    /// group_name 同步为首个分组（兼容旧前端/排序）。自动过滤不属于该命名空间的分组 id。
    pub async fn set_book_groups(&self, ns: &str, book_url: &str, ids: &[i64]) -> Result<u64> {
        let valid = self.valid_group_ids(ns, ids).await?;
        let mut unique: Vec<i64> = Vec::with_capacity(valid.len());
        for id in valid {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        unique.sort_unstable();
        let encoded = serde_json::to_string(&unique).unwrap_or_else(|_| "[]".to_string());
        let primary = unique.first().copied().unwrap_or(0);
        let r = sqlx::query(
            "UPDATE books SET group_name = ?3, group_ids = ?4 WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(primary)
        .bind(encoded)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 往书籍追加一个分组（幂等）；返回受影响行数
    pub async fn add_book_group(&self, ns: &str, book_url: &str, group_id: i64) -> Result<u64> {
        let mut ids = self.book_group_ids(ns, book_url).await?;
        if ids.contains(&group_id) {
            return Ok(0);
        }
        ids.push(group_id);
        self.set_book_groups(ns, book_url, &ids).await
    }

    /// 从书籍移除一个分组（group_ids 去除；group_name 若为该分组则改为剩余首项/0）；
    /// 返回受影响行数
    pub async fn remove_book_group(&self, ns: &str, book_url: &str, group_id: i64) -> Result<u64> {
        let mut ids = self.book_group_ids(ns, book_url).await?;
        let before = ids.len();
        ids.retain(|id| *id != group_id);
        if ids.len() == before {
            return Ok(0);
        }
        self.set_book_groups(ns, book_url, &ids).await
    }

    /// 读取书籍当前分组 ID 列表（JSON 数组；无记录/空 → []）
    async fn book_group_ids(&self, ns: &str, book_url: &str) -> Result<Vec<i64>> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT group_ids FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(raw
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
            .unwrap_or_default())
    }

    /// 过滤出属于该命名空间的分组 id（防跨用户/幽灵 id）
    async fn valid_group_ids(&self, ns: &str, ids: &[i64]) -> Result<Vec<i64>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut valid = Vec::with_capacity(ids.len());
        for id in ids {
            let owned: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM book_groups WHERE user_namespace = ?1 AND id = ?2",
            )
            .bind(ns)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
            if owned.is_some() {
                valid.push(*id);
            }
        }
        Ok(valid)
    }

    /// 分组列表（带组内书数统计：books.group_name = 分组 id 计数）
    pub async fn list_book_groups_with_count(
        &self,
        ns: &str,
    ) -> Result<Vec<crate::model::BookGroupWithCount>> {
        let mut rows = sqlx::query_as::<_, (i64, String, Option<String>, bool, i64, i64)>(
            "SELECT g.id, g.name, g.cover, g.show, g.order_num, 0 FROM book_groups g WHERE g.user_namespace = ?1 ORDER BY g.order_num, g.id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        // F7（legacy BookGroupController.onList）：空库播种内置 5 组
        // -1 全部(-10) / -2 本地(-9) / -3 音频(-8) / -4 未分组(-7) / -5 更新错误(-6)
        if rows.is_empty() && !ns.is_empty() {
            let defaults: [(i64, &str, i64); 5] = [
                (-1, "全部", -10),
                (-2, "本地", -9),
                (-3, "音频", -8),
                (-4, "未分组", -7),
                (-5, "更新错误", -6),
            ];
            for (gid, gname, gorder) in defaults {
                let _ = sqlx::query(
                    "INSERT OR IGNORE INTO book_groups (id, name, show, order_num, user_namespace)                      VALUES (?1, ?2, 1, ?3, ?4)",
                )
                .bind(gid)
                .bind(gname)
                .bind(gorder)
                .bind(ns)
                .execute(&self.pool)
                .await;
            }
            rows = sqlx::query_as::<_, (i64, String, Option<String>, bool, i64, i64)>(
                "SELECT g.id, g.name, g.cover, g.show, g.order_num, 0 FROM book_groups g WHERE g.user_namespace = ?1 ORDER BY g.order_num, g.id",
            )
            .bind(ns)
            .fetch_all(&self.pool)
            .await?;
        }
        let mut out = Vec::with_capacity(rows.len());
        for (id, name, cover, show, order, _) in rows {
            let pattern = format!("%\"{id}\"%");
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM books WHERE user_namespace = ?1 AND (group_name = ?2 OR group_ids LIKE ?3)",
            )
            .bind(ns)
            .bind(id)
            .bind(pattern)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
            out.push(crate::model::BookGroupWithCount {
                id,
                name: name.clone(),
                group_id: id,
                group_name: name,
                cover,
                show,
                order,
                order_num: order,
                book_count: count,
            });
        }
        Ok(out)
    }

    /// 分组重命名（仅改 name，保留 order 与 id；不存在返回 0 行）
    pub async fn rename_book_group(&self, ns: &str, id: i64, name: &str) -> Result<u64> {
        let r =
            sqlx::query("UPDATE book_groups SET name = ?3 WHERE user_namespace = ?1 AND id = ?2")
                .bind(ns)
                .bind(id)
                .bind(name)
                .execute(&self.pool)
                .await?;
        Ok(r.rows_affected())
    }

    /// 删除分组（事务：组内书 group_name 置 0 后删分组）；返回删除的分组行数
    pub async fn delete_book_group(&self, ns: &str, id: i64) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE books SET group_name = 0 WHERE user_namespace = ?1 AND group_name = ?2",
        )
        .bind(ns)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        // 多分组：移除 group_ids 中的该 id（事务内逐行处理）
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT book_url, group_ids FROM books WHERE user_namespace = ?1 AND group_ids LIKE ?2",
        )
        .bind(ns)
        .bind(format!("%\"{id}\"%"))
        .fetch_all(&mut *tx)
        .await?;
        for (book_url, raw_ids) in rows {
            let mut ids: Vec<i64> = serde_json::from_str(&raw_ids).unwrap_or_default();
            ids.retain(|g| *g != id);
            let encoded = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
            let primary = ids.first().copied().unwrap_or(0);
            sqlx::query(
                "UPDATE books SET group_ids = ?3, group_name = ?4 WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(&book_url)
            .bind(encoded)
            .bind(primary)
            .execute(&mut *tx)
            .await?;
        }
        let r = sqlx::query("DELETE FROM book_groups WHERE user_namespace = ?1 AND id = ?2")
            .bind(ns)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(r.rows_affected())
    }

    // ---------------- 批量接口（deleteBooks / 分组批量 / 书签批量 / RSS 批量） ----------------

    /// F4 按名称解析 HttpTTS（legacy getHttpTTSByName——textToSpeech type=api 时
    /// voice 参数即听书源名）
    pub async fn get_http_tts_by_name(
        &self,
        ns: &str,
        name: &str,
    ) -> Result<Option<crate::model::HttpTts>> {
        let tts = sqlx::query_as::<_, crate::model::HttpTts>(
            "SELECT * FROM http_tts_list WHERE user_namespace = ?1 AND name = ?2              ORDER BY rowid LIMIT 1",
        )
        .bind(ns)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(tts)
    }

    /// F2 每书换源候选读取（legacy getUserStorage(ns, name_author, "bookSource")）
    pub async fn get_book_candidates(
        &self,
        ns: &str,
        key: &str,
    ) -> Result<Vec<crate::service::search::SearchBook>> {
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT candidates_json FROM book_source_candidates              WHERE user_namespace = ?1 AND book_key = ?2",
        )
        .bind(ns)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some((json,)) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(vec![]),
        }
    }

    /// F2 每书换源候选写入（覆盖式）
    pub async fn save_book_candidates(
        &self,
        ns: &str,
        key: &str,
        cands: &[crate::service::search::SearchBook],
    ) -> Result<()> {
        let json = serde_json::to_string(cands)?;
        sqlx::query(
            "INSERT OR REPLACE INTO book_source_candidates              (user_namespace, book_key, candidates_json, updated_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(ns)
        .bind(key)
        .bind(json)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 批量删除书（事务：每本连带删章节）；返回删除的书行数
    /// 批量删除（GAP 117：删除后清理无引用封面文件）
    pub async fn delete_books(&self, ns: &str, book_urls: &[String]) -> Result<u64> {
        let mut covers: Vec<String> = Vec::new();
        for url in book_urls {
            if let Ok(Some(c)) = sqlx::query_scalar::<_, Option<String>>(
                "SELECT cover_url FROM books WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(url)
            .fetch_one(&self.pool)
            .await
            {
                covers.push(c);
            }
        }
        let mut tx = self.pool.begin().await?;
        let mut deleted = 0u64;
        for url in book_urls {
            // P1-C1：章节/目录缓存删除按书架归属过滤（跨用户删除防护）
            sqlx::query(
                "DELETE FROM book_chapters WHERE book_url = ?1              AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
            )
            .bind(url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM toc_cache WHERE book_url = ?1              AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
            )
            .bind(url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
            let r = sqlx::query("DELETE FROM books WHERE user_namespace = ?1 AND book_url = ?2")
                .bind(ns)
                .bind(url)
                .execute(&mut *tx)
                .await?;
            deleted += r.rows_affected();
        }
        tx.commit().await?;
        for c in covers {
            self.cleanup_orphan_cover(ns, Some(&c)).await;
        }
        Ok(deleted)
    }

    /// 批量追加分组（books.group_ids 追加 group_id；group_name 同步为首项）；返回受影响行数
    pub async fn add_book_group_multi(
        &self,
        ns: &str,
        book_urls: &[String],
        group_id: i64,
    ) -> Result<u64> {
        if book_urls.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut updated = 0u64;
        for url in book_urls {
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT group_ids FROM books WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(url)
            .fetch_optional(&mut *tx)
            .await?;
            let mut ids: Vec<i64> = raw
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            if ids.contains(&group_id) {
                continue;
            }
            ids.push(group_id);
            ids.sort_unstable();
            let encoded = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
            let primary = ids.first().copied().unwrap_or(0);
            let r = sqlx::query(
                "UPDATE books SET group_ids = ?3, group_name = ?4 WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(url)
            .bind(encoded)
            .bind(primary)
            .execute(&mut *tx)
            .await?;
            updated += r.rows_affected();
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// 批量移出分组（group_id=None 清空全部多分组；Some(id) 仅移除该分组）；返回受影响行数
    pub async fn remove_book_group_multi(
        &self,
        ns: &str,
        book_urls: &[String],
        group_id: Option<i64>,
    ) -> Result<u64> {
        if book_urls.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut updated = 0u64;
        for url in book_urls {
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT group_ids FROM books WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(url)
            .fetch_optional(&mut *tx)
            .await?;
            let mut ids: Vec<i64> = raw
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let before = ids.len();
            if let Some(gid) = group_id {
                ids.retain(|g| *g != gid);
            } else {
                ids.clear();
            }
            if ids.len() == before {
                continue;
            }
            ids.sort_unstable();
            let encoded = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
            let primary = ids.first().copied().unwrap_or(0);
            let r = sqlx::query(
                "UPDATE books SET group_ids = ?3, group_name = ?4 WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(url)
            .bind(encoded)
            .bind(primary)
            .execute(&mut *tx)
            .await?;
            updated += r.rows_affected();
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// 分组排序批量保存（order = [(id, order_num)]）；返回更新行数
    pub async fn save_book_group_order(&self, ns: &str, order: &[(i64, i64)]) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let mut updated = 0u64;
        for (id, order_num) in order {
            let r = sqlx::query(
                "UPDATE book_groups SET order_num = ?3 WHERE user_namespace = ?1 AND id = ?2",
            )
            .bind(ns)
            .bind(id)
            .bind(order_num)
            .execute(&mut *tx)
            .await?;
            updated += r.rows_affected();
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// 批量保存书签（单事务，INSERT OR REPLACE 按 book_url+title 主键去重）
    pub async fn save_bookmarks(
        &self,
        ns: &str,
        bookmarks: &[crate::model::Bookmark],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for b in bookmarks {
            sqlx::query(
                "INSERT OR REPLACE INTO bookmarks (book_url, title, book_name, book_author,              paragraph_index, chapter_index, chapter_name, book_text, content,              created_at, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .bind(&b.book_url)
            .bind(&b.title)
            .bind(&b.book_name)
            .bind(&b.book_author)
            .bind(b.paragraph_index)
            .bind(b.chapter_index)
            .bind(&b.chapter_name)
            .bind(&b.book_text)
            .bind(&b.content)
            .bind(b.created_at)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 批量删除书签（book_url + ids=书签标题列表，IN 查询）；返回受影响行数
    pub async fn delete_bookmarks(&self, ns: &str, book_url: &str, ids: &[String]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut qb = sqlx::QueryBuilder::new("DELETE FROM bookmarks WHERE user_namespace = ");
        qb.push_bind(ns)
            .push(" AND book_url = ")
            .push_bind(book_url)
            .push(" AND title IN (");
        let mut sep = qb.separated(", ");
        for id in ids {
            sep.push_bind(id);
        }
        qb.push(")");
        let r = qb.build().execute(&self.pool).await?;
        Ok(r.rows_affected())
    }

    /// 批量保存 RSS 源（单事务，INSERT OR REPLACE 按 rss_source_url 主键覆盖）
    pub async fn save_rss_sources(
        &self,
        ns: &str,
        sources: &[crate::model::RssSource],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for s in sources {
            sqlx::query(
                "INSERT OR REPLACE INTO rss_sources            (rss_source_url, rss_source_name, rss_source_group, enabled, user_namespace, raw_json)            VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(&s.source_url)
            .bind(&s.source_name)
            .bind(&s.source_group)
            .bind(s.enabled)
            .bind(ns)
            .bind(&s.raw_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---------------- 用户配置（user_config 表：按用户 + 配置命名空间） ----------------

    /// 读取用户配置（无则 None）
    pub async fn get_user_config(&self, ns: &str, key: &str) -> Result<Option<String>> {
        let r: Option<(String,)> =
            sqlx::query_as("SELECT config FROM user_config WHERE user_namespace = ?1 AND ns = ?2")
                .bind(ns)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(r.map(|x| x.0))
    }

    /// 保存用户配置（INSERT OR REPLACE）
    pub async fn save_user_config(&self, ns: &str, key: &str, config: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO user_config (user_namespace, ns, config, updated_at)              VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(ns)
        .bind(key)
        .bind(config)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------------- 阅读统计（reading_stats 表：按用户 + 书 + 日期累计） ----------------

    /// 增量累计阅读统计（seconds/chars 累加到当日行；INSERT OR REPLACE 语义不丢历史）
    pub async fn record_reading_stats(
        &self,
        ns: &str,
        book_url: &str,
        date: &str,
        seconds: i64,
        chars: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO reading_stats (user_namespace, book_url, date, seconds, chars)              VALUES (?1, ?2, ?3, ?4, ?5)              ON CONFLICT(user_namespace, book_url, date)              DO UPDATE SET seconds = seconds + excluded.seconds, chars = chars + excluded.chars",
        )
        .bind(ns)
        .bind(book_url)
        .bind(date)
        .bind(seconds)
        .bind(chars)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 阅读统计汇总：今日/近 7 天/累计秒数 + 单书（秒数降序，关联书架书名）
    pub async fn get_reading_stats(&self, ns: &str) -> Result<ReadingStats> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let week_start = (chrono::Utc::now() - chrono::Duration::days(6))
            .format("%Y-%m-%d")
            .to_string();
        let today_s: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(seconds), 0) FROM reading_stats WHERE user_namespace = ?1 AND date = ?2",
        )
        .bind(ns)
        .bind(&today)
        .fetch_one(&self.pool)
        .await?;
        let week_s: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(seconds), 0) FROM reading_stats WHERE user_namespace = ?1 AND date >= ?2",
        )
        .bind(ns)
        .bind(&week_start)
        .fetch_one(&self.pool)
        .await?;
        let total_s: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(seconds), 0) FROM reading_stats WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT book_url, COALESCE(SUM(seconds), 0), COALESCE(SUM(chars), 0)              FROM reading_stats WHERE user_namespace = ?1              GROUP BY book_url ORDER BY 2 DESC",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        let mut books = Vec::with_capacity(rows.len());
        for (book_url, seconds, chars) in rows {
            let name = sqlx::query_scalar::<_, String>(
                "SELECT name FROM books WHERE user_namespace = ?1 AND book_url = ?2",
            )
            .bind(ns)
            .bind(&book_url)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or_else(|| book_url.clone());
            books.push(ReadingStatBook {
                book_url,
                name,
                seconds,
                chars,
            });
        }
        Ok(ReadingStats {
            today: today_s,
            week: week_s,
            total: total_s,
            books,
        })
    }

    // ---------------- 默认书源标记（system_settings：default_book_sources_{ns}） ----------------

    /// 默认书源列表（JSON 数组存 system_settings；无则空）
    pub async fn get_default_book_sources(&self, ns: &str) -> Result<Vec<String>> {
        let key = format!("default_book_sources_{ns}");
        match self.get_system_setting(&key).await? {
            Some(raw) => Ok(serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()),
            None => Ok(Vec::new()),
        }
    }

    /// 设置默认书源标记（覆盖式保存书源 URL 列表）
    pub async fn set_default_book_sources(&self, ns: &str, urls: &[String]) -> Result<()> {
        let key = format!("default_book_sources_{ns}");
        let raw = serde_json::to_string(urls).unwrap_or_else(|_| "[]".to_string());
        self.set_system_setting(&key, &raw).await
    }

    // ---------------- F-28 替换规则 ----------------

    /// 替换规则列表（按 order_num, id 排序；无用户规则回退 default，同书源语义）
    pub async fn get_replace_rules(&self, ns: &str) -> Result<Vec<crate::model::ReplaceRule>> {
        let rows = sqlx::query_as::<_, crate::model::ReplaceRule>(
            "SELECT * FROM replace_rules WHERE user_namespace = ?1 ORDER BY order_num, id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        if !rows.is_empty() || ns == "default" {
            return Ok(rows);
        }
        sqlx::query_as::<_, crate::model::ReplaceRule>(
            "SELECT * FROM replace_rules WHERE user_namespace = 'default' ORDER BY order_num, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 保存单条替换规则（INSERT OR REPLACE，按 id 主键覆盖）
    /// P1-C2：id 已被其他命名空间占用时改插新 id（不覆写他人规则；default 共享规则编辑同理
    /// 转为自己副本）；返回生效 id（可能已改插）
    pub async fn save_replace_rule(
        &self,
        ns: &str,
        rule: &crate::model::ReplaceRule,
    ) -> Result<String> {
        let mut r = rule.clone();
        self.ensure_rule_id_owned("replace_rules", ns, &mut r.id)
            .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO replace_rules (id, name, group_name, find, replace, scope,              scope_title, scope_content, is_regex, timeout_millisecond, enable, order_num, user_namespace)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(&r.id)
        .bind(&r.name)
        .bind(&r.group)
        .bind(&r.find)
        .bind(&r.replace)
        .bind(&r.scope)
        .bind(r.scope_title)
        .bind(r.scope_content)
        .bind(r.is_regex)
        .bind(r.timeout_millisecond)
        .bind(r.enabled)
        .bind(r.order)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        Ok(r.id)
    }

    /// P1-C2：规则 id 归属校验——id 非空且已被其他命名空间占用 → 改插新 uuid（插入新而非覆写）
    /// table 仅为固定字面量（"replace_rules" / "txt_toc_rules"），无注入面
    async fn ensure_rule_id_owned(&self, table: &str, ns: &str, id: &mut String) -> Result<()> {
        if id.trim().is_empty() {
            return Ok(());
        }
        let sql = format!("SELECT user_namespace FROM {table} WHERE id = ?1");
        let owner: Option<String> = sqlx::query_scalar(&sql)
            .bind(&*id)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(owner) = owner {
            if owner != ns {
                *id = format!("rule-{}", uuid::Uuid::new_v4());
            }
        }
        Ok(())
    }

    /// 批量保存替换规则（单事务：全部成功或全部回滚）
    pub async fn save_replace_rules(
        &self,
        ns: &str,
        rules: &[crate::model::ReplaceRule],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for rule in rules {
            let mut r = rule.clone();
            self.ensure_rule_id_owned("replace_rules", ns, &mut r.id)
                .await?;
            sqlx::query(
                "INSERT OR REPLACE INTO replace_rules (id, name, group_name, find, replace, scope,                  scope_title, scope_content, is_regex, timeout_millisecond, enable, order_num, user_namespace)                  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )
            .bind(&r.id)
            .bind(&r.name)
            .bind(&r.group)
            .bind(&r.find)
            .bind(&r.replace)
            .bind(&r.scope)
            .bind(r.scope_title)
            .bind(r.scope_content)
            .bind(r.is_regex)
            .bind(r.timeout_millisecond)
            .bind(r.enabled)
            .bind(r.order)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 删除替换规则（按 id，仅限本命名空间）；返回受影响行数
    pub async fn delete_replace_rule(&self, ns: &str, id: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM replace_rules WHERE user_namespace = ?1 AND id = ?2")
            .bind(ns)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 批量删除替换规则（单事务：全部成功或全部回滚）；返回受影响行数。
    /// 每个 id 同时按 id 或 name 匹配（legacy 数组以 name 匹配规则）
    pub async fn delete_replace_rules(&self, ns: &str, ids: &[String]) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let mut total = 0u64;
        for id in ids {
            let r = sqlx::query(
                "DELETE FROM replace_rules WHERE user_namespace = ?1 AND (id = ?2 OR name = ?2)",
            )
            .bind(ns)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            total += r.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    /// 清空命名空间全部替换规则；返回受影响行数
    pub async fn delete_all_replace_rules(&self, ns: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM replace_rules WHERE user_namespace = ?1")
            .bind(ns)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ---------------- F-26 HttpTTS ----------------

    /// HttpTTS 听书源列表（按名称排序；无用户数据回退 default，同书源语义）
    pub async fn get_http_tts_list(&self, ns: &str) -> Result<Vec<crate::model::HttpTts>> {
        let rows = sqlx::query_as::<_, crate::model::HttpTts>(
            "SELECT * FROM http_tts_list WHERE user_namespace = ?1 ORDER BY name",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        if !rows.is_empty() || ns == "default" {
            return Ok(rows);
        }
        sqlx::query_as::<_, crate::model::HttpTts>(
            "SELECT * FROM http_tts_list WHERE user_namespace = 'default' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 保存 HttpTTS（INSERT OR REPLACE，按 url 主键覆盖）
    pub async fn save_http_tts(&self, ns: &str, tts: &crate::model::HttpTts) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO http_tts_list (url, name, type, content_type, concurrent_rate,              login_url, login_ui, header, js_lib, enabled_cookie_jar, login_check_js,              last_update_time, user_namespace)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(&tts.url)
        .bind(&tts.name)
        .bind(tts.tts_type)
        .bind(&tts.content_type)
        .bind(&tts.concurrent_rate)
        .bind(&tts.login_url)
        .bind(&tts.login_ui)
        .bind(&tts.header)
        .bind(&tts.js_lib)
        .bind(tts.enabled_cookie_jar)
        .bind(&tts.login_check_js)
        .bind(tts.last_update_time)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 批量保存 HttpTTS（单事务：全部成功或全部回滚）
    pub async fn save_http_tts_multi(
        &self,
        ns: &str,
        items: &[crate::model::HttpTts],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for tts in items {
            sqlx::query(
                "INSERT OR REPLACE INTO http_tts_list (url, name, type, content_type, concurrent_rate,              login_url, login_ui, header, js_lib, enabled_cookie_jar, login_check_js,              last_update_time, user_namespace)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )
            .bind(&tts.url)
            .bind(&tts.name)
            .bind(tts.tts_type)
            .bind(&tts.content_type)
            .bind(&tts.concurrent_rate)
            .bind(&tts.login_url)
            .bind(&tts.login_ui)
            .bind(&tts.header)
            .bind(&tts.js_lib)
            .bind(tts.enabled_cookie_jar)
            .bind(&tts.login_check_js)
            .bind(tts.last_update_time)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 删除 HttpTTS（按 url，仅限本命名空间）；返回受影响行数
    pub async fn delete_http_tts(&self, ns: &str, url: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM http_tts_list WHERE user_namespace = ?1 AND url = ?2")
            .bind(ns)
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 批量删除 HttpTTS（按 url 列表，仅限本命名空间；单事务，返回受影响行数）
    pub async fn delete_http_tts_multi(&self, ns: &str, urls: &[String]) -> Result<u64> {
        if urls.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut affected = 0u64;
        for url in urls {
            let r = sqlx::query("DELETE FROM http_tts_list WHERE user_namespace = ?1 AND url = ?2")
                .bind(ns)
                .bind(url)
                .execute(&mut *tx)
                .await?;
            affected += r.rows_affected();
        }
        tx.commit().await?;
        Ok(affected)
    }

    // ---------------- 自定义 TXT 目录规则 ----------------

    /// 用户自定义 TXT 目录规则（按 serial_number, id 排序；仅用户自有，无 default 回退）
    pub async fn get_txt_toc_rules(&self, ns: &str) -> Result<Vec<crate::model::TxtTocRule>> {
        let rows = sqlx::query_as::<_, crate::model::TxtTocRule>(
            "SELECT * FROM txt_toc_rules WHERE user_namespace = ?1 ORDER BY serial_number, id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 保存单条 TXT 目录规则（INSERT OR REPLACE，按 id 主键覆盖）
    /// P1-C2：id 已被其他命名空间占用时改插新 id（不覆写他人规则）
    /// 保存单条 TXT 目录规则（INSERT OR REPLACE，按 id 主键覆盖）
    /// P1-C2：id 已被其他命名空间占用时改插新 id（不覆写他人规则）；返回生效 id（可能已改插）
    pub async fn save_txt_toc_rule(
        &self,
        ns: &str,
        rule: &crate::model::TxtTocRule,
    ) -> Result<String> {
        let mut r = rule.clone();
        // P1-C2：id 已被其他命名空间占用时改插新 id（不覆写他人规则）
        self.ensure_rule_id_owned("txt_toc_rules", ns, &mut r.id)
            .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO txt_toc_rules (id, name, rule, enable, serial_number, user_namespace)              VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&r.id)
        .bind(&r.name)
        .bind(&r.rule)
        .bind(r.enable)
        .bind(r.serial_number)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        Ok(r.id)
    }

    /// 删除 TXT 目录规则（按 id，仅限本命名空间）；返回受影响行数
    pub async fn delete_txt_toc_rule(&self, ns: &str, id: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM txt_toc_rules WHERE user_namespace = ?1 AND id = ?2")
            .bind(ns)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 导入内置默认规则为用户规则（id 固定 default-{i}，幂等可重复导入）；返回导入条数
    pub async fn import_default_txt_toc_rules(&self, ns: &str) -> Result<usize> {
        let defaults = crate::service::local_book::DEFAULT_TOC_RULE_DEFS;
        let mut tx = self.pool.begin().await?;
        for def in defaults {
            sqlx::query(
                "INSERT OR REPLACE INTO txt_toc_rules (id, name, rule, enable, serial_number, user_namespace)                  VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(format!("default-{}", def.serial_number + 1))
            .bind(def.name)
            .bind(def.rule)
            .bind(def.enable)
            .bind(def.serial_number)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(defaults.len())
    }

    // ---------------- getSystemInfo 统计 ----------------

    /// 全部命名空间书籍总数
    pub async fn count_books(&self) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM books")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 全部命名空间书源总数
    pub async fn count_all_book_sources(&self) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM book_sources")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 本地书入库（books + 章节）
    pub async fn save_local_book(
        &self,
        ns: &str,
        info: &crate::model::book_chapter::BookInfo,
        imported: &crate::service::local_book::ImportedBook,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        // INSERT OR REPLACE 会重置未列出列：重导入本地书时保留既有多分组
        let prev_group_ids: Option<String> = sqlx::query_scalar(
            "SELECT group_ids FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(&info.book_url)
        .fetch_optional(&mut *tx)
        .await?;
        let group_ids = prev_group_ids.unwrap_or_else(|| "[]".to_string());
        sqlx::query(
            r#"INSERT OR REPLACE INTO books
            (book_url, name, author, kind, intro, language, publisher, published_at,
             cover_url, toc_url, origin, origin_name, group_name, group_ids, type,
             total_chapter_num, user_namespace, created_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13,?14,?15,?16,?17)"#,
        )
        .bind(&info.book_url)
        .bind(&info.name)
        .bind(&info.author)
        .bind(&info.kind)
        .bind(&info.intro)
        .bind(&info.language)
        .bind(&info.publisher)
        .bind(&info.published_at)
        .bind(&info.cover_url)
        .bind(&info.toc_url)
        .bind(&info.origin)
        .bind(&info.origin_name)
        .bind(group_ids)
        .bind(info.book_type)
        .bind(imported.chapters.len() as i64)
        .bind(ns)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&mut *tx)
        .await?;
        let chapters: Vec<(String, String)> = imported
            .chapters
            .iter()
            .map(|c| (c.title.clone(), c.content.clone()))
            .collect();
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT OR REPLACE INTO book_chapters (book_url, chapter_index, title, content) VALUES (?1,?2,?3,?4)",
            )
            .bind(&info.book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---------------- GAP 171：loc_book → DB 迁移 ----------------

    /// legacy loc_book 文件书迁移：文件解析结果写入 book_chapters（键 = 原 book_url），
    /// books.local_file 记录关联路径（保留原记录 origin 不动，local_file 非空即迁移标记）
    /// 返回是否写入（书不存在返回 false）
    #[allow(clippy::too_many_arguments)] // 抓取/OPDS 内部管线参数，拆结构体会让调用点更长，暂不拆
    pub async fn migrate_loc_book(
        &self,
        ns: &str,
        book_url: &str,
        local_file: Option<&str>,
        mtime: i64,
        size: i64,
        chapters: &[(String, String)],
        cover: Option<&[u8]>,
    ) -> Result<bool> {
        let exists: bool = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_one(&self.pool)
        .await?
            > 0;
        if !exists {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        // 覆盖式重建章节（重复迁移幂等）
        sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1")
            .bind(book_url)
            .execute(&mut *tx)
            .await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT OR REPLACE INTO book_chapters (book_url, chapter_index, title, content) VALUES (?1,?2,?3,?4)",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE books SET local_file = ?3, local_file_mtime = ?4, local_file_size = ?5, local_file_deleted = 0, total_chapter_num = ?6              WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(local_file)
        .bind(mtime)
        .bind(size)
        .bind(chapters.len() as i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        // 封面落盘（与上传导入一致）
        if let Some(cover) = cover {
            if !cover.is_empty() {
                let cover_dir = self
                    .config
                    .storage_dir()
                    .join("assets")
                    .join(ns)
                    .join("covers");
                let _ = std::fs::create_dir_all(&cover_dir);
                let file_id = format!("{}.jpg", uuid::Uuid::new_v4());
                if std::fs::write(cover_dir.join(&file_id), cover).is_ok() {
                    let _ = self
                        .update_book_cover(ns, book_url, &format!("/assets/{ns}/covers/{file_id}"))
                        .await;
                }
            }
        }
        Ok(true)
    }

    /// GAP 171：查全部 legacy loc_book 文件书（migrateLocBook all 用）
    pub async fn list_loc_book_books(&self, ns: &str) -> Result<Vec<crate::model::Book>> {
        sqlx::query_as::<_, crate::model::Book>(
            "SELECT * FROM books WHERE user_namespace = ?1 AND origin = 'loc_book' ORDER BY name",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    // ---------------- GAP 170 本地书双轨同步 ----------------

    /// 查询有 local_file 关联的书（含已删除标记的——文件重现时用于重链/重扫）
    pub async fn list_linked_local_books(&self, ns: &str) -> Result<Vec<Book>> {
        let rows = sqlx::query_as::<_, Book>(
            "SELECT * FROM books WHERE user_namespace = ?1 AND local_file IS NOT NULL AND local_file != ''",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 查询无文件关联的 local:// DB 书（对账任务自动生成 epub 落书仓目录用）
    pub async fn list_local_db_books_without_file(&self, ns: &str) -> Result<Vec<Book>> {
        let rows = sqlx::query_as::<_, Book>(
            "SELECT * FROM books WHERE user_namespace = ?1 AND book_url LIKE 'local://%' AND (local_file IS NULL OR local_file = '')",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 写入本地书文件关联（local_file=None 时仅更新变更检测/删除标记字段）
    pub async fn link_local_file(
        &self,
        ns: &str,
        book_url: &str,
        local_file: Option<&str>,
        mtime: i64,
        size: i64,
        deleted: bool,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE books SET local_file = ?3, local_file_mtime = ?4, local_file_size = ?5, local_file_deleted = ?6              WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(local_file)
        .bind(mtime)
        .bind(size)
        .bind(deleted)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 替换本地书全部章节（重扫用：事务内先删后插——旧章残留清理，新章序从 0 开始）
    /// P1-C1：书必须属于当前命名空间（否则拒绝）——防止跨用户覆写他人章节缓存
    pub async fn replace_chapters(
        &self,
        ns: &str,
        book_url: &str,
        chapters: &[(String, String)],
    ) -> Result<()> {
        let owned: bool = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?
        .is_some();
        if !owned {
            anyhow::bail!("书籍不存在或无权操作");
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM book_chapters WHERE book_url = ?1              AND book_url IN (SELECT book_url FROM books WHERE user_namespace = ?2)",
        )
        .bind(book_url)
        .bind(ns)
        .execute(&mut *tx)
        .await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 本地书章节（含正文，全量——对账任务 epub 生成用）
    pub async fn list_chapters_full(&self, book_url: &str) -> Result<Vec<(i64, String, String)>> {
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chapter_index, title, content FROM book_chapters WHERE book_url = ?1 ORDER BY chapter_index",
        )
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 更新封面 URL（导入后写封面文件）
    pub async fn update_book_cover(
        &self,
        ns: &str,
        book_url: &str,
        cover_url: &str,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE books SET cover_url = ?3 WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(cover_url)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 本地书入库（books + 章节）
    /// 用户总数（注册上限校验）
    pub async fn count_users(&self) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 在线会话数（活跃 token 总数，服务监控用）
    ///
    /// 每用户：token_map 非空 → 其条目数（多设备会话，含主 token）；
    /// 否则主 token 非空 → 1。
    pub async fn count_active_tokens(&self) -> Result<i64> {
        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT token, token_map FROM users")
                .fetch_all(&self.pool)
                .await?;
        let mut n: i64 = 0;
        for (token, token_map) in rows {
            if let Some(map) = token_map {
                let v: Option<serde_json::Value> = serde_json::from_str(&map).ok();
                n += crate::model::user::token_map_list(&v).len() as i64;
            } else if !token.is_empty() {
                n += 1;
            }
        }
        Ok(n)
    }

    /// 新建用户
    pub async fn insert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO users
                (username, password, salt, token, enable_webdav, enable_local_store,
                 enable_book_source, enable_rss_source, book_source_limit, book_limit,
                 is_admin, last_login_at, created_at, user_namespace)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
        )
        .bind(&user.username)
        .bind(&user.password)
        .bind(&user.salt)
        .bind(&user.token)
        .bind(user.enable_webdav)
        .bind(user.enable_local_store)
        .bind(user.enable_book_source)
        .bind(user.enable_rss_source)
        .bind(user.book_source_limit)
        .bind(user.book_limit)
        .bind(user.is_admin)
        .bind(user.last_login_at)
        .bind(user.created_at)
        .bind(&user.user_namespace)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 登录成功：刷新 token + last_login_at
    pub async fn update_user_session(
        &self,
        username: &str,
        token: &str,
        last_login_at: i64,
    ) -> Result<()> {
        sqlx::query("UPDATE users SET token = ?1, last_login_at = ?2 WHERE username = ?3")
            .bind(token)
            .bind(last_login_at)
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---------------- GAP 59 多设备 token（users.token_map） ----------------

    /// 登录：追加 token 到 token_map（对象形态 {token: 过期毫秒}，上限 5，最旧被丢），
    /// 同时刷新主 token 与 last_login_at。过期时间 = now + ttl 天（ttl<=0 永不过期）。
    pub async fn add_user_token(
        &self,
        username: &str,
        token: &str,
        last_login_at: i64,
    ) -> Result<()> {
        let user = self.find_user(username).await?;
        let ttl_days = self.config.token_ttl_days;
        let expire_ms = if ttl_days > 0 {
            last_login_at.saturating_add(ttl_days * 86_400_000)
        } else {
            i64::MAX
        };
        let map_json = crate::model::user::token_map_push(
            &user.as_ref().and_then(|u| u.token_map.clone()),
            token,
            expire_ms,
            last_login_at,
        );
        sqlx::query(
            "UPDATE users SET token = ?1, token_map = ?2, last_login_at = ?3 WHERE username = ?4",
        )
        .bind(token)
        .bind(&map_json)
        .bind(last_login_at)
        .bind(username)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 登出：从 token_map 移除指定 token；若移除的是主 token 则同时清空主 token
    /// （其他设备 token 不受影响——多设备会话互不干扰）。返回受影响行数。
    pub async fn remove_user_token(&self, username: &str, token: &str) -> Result<u64> {
        let user = self.find_user(username).await?;
        let Some(user) = user else { return Ok(0) };
        let (map_json, removed) = crate::model::user::token_map_remove(&user.token_map, token);
        let clear_main = !user.token.is_empty() && user.token == token;
        let main_token = if clear_main { "" } else { user.token.as_str() };
        sqlx::query("UPDATE users SET token = ?1, token_map = ?2 WHERE username = ?3")
            .bind(main_token)
            .bind(&map_json)
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(if removed || clear_main { 1 } else { 0 })
    }

    /// 查询某命名空间的书架（按插入顺序，兼容 legacy bookshelf.json 数组顺序）
    pub async fn list_books(&self, namespace: &str) -> Result<Vec<Book>> {
        let books = sqlx::query_as::<_, Book>(
            r#"
            SELECT books.*, books.rowid AS rowid
            FROM books
            WHERE user_namespace = ?1
            -- 最近活动优先：手动排序（order_num）在前，同序按 max(最近阅读, 加入时间) 倒序——
            -- 刚加入的书（created_at 新）与刚读完的书（dur_chapter_time 更新）都排到最前
            ORDER BY order_num ASC, MAX(dur_chapter_time, created_at) DESC, rowid DESC
            "#,
        )
        .bind(namespace)
        .fetch_all(&self.pool)
        .await?;
        Ok(books)
    }

    // ---------------- F-7 书源数上限 ----------------

    /// 某命名空间现有书源数（仅用户自有书源，不含 default 回退）
    pub async fn count_book_sources(&self, ns: &str) -> Result<i64> {
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM book_sources WHERE user_namespace = ?1 AND hidden = 0",
        )
        .bind(ns)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    /// P1-C4：某命名空间书架书籍数（saveBook 上限校验用）
    pub async fn count_books_for_user(&self, ns: &str) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM books WHERE user_namespace = ?1")
            .bind(ns)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 用户书籍上限（users.book_limit；用户不存在返回 None——非 secure 模式不限制）
    pub async fn book_limit_for(&self, ns: &str) -> Result<Option<i64>> {
        let limit = sqlx::query_scalar("SELECT book_limit FROM users WHERE username = ?1")
            .bind(ns)
            .fetch_optional(&self.pool)
            .await?;
        Ok(limit)
    }

    /// 用户书源上限（users.book_source_limit；用户不存在返回 None——非 secure 模式不限制）
    pub async fn book_source_limit_for(&self, ns: &str) -> Result<Option<i64>> {
        let limit = sqlx::query_scalar("SELECT book_source_limit FROM users WHERE username = ?1")
            .bind(ns)
            .fetch_optional(&self.pool)
            .await?;
        Ok(limit)
    }

    // ---------------- F-25/F-34 用户会话 ----------------

    /// F-25 退出登录：清空用户 token（token 立即失效）
    pub async fn logout_user(&self, username: &str) -> Result<u64> {
        let r = sqlx::query("UPDATE users SET token = '' WHERE username = ?1")
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// F-34 清理不活跃用户：删除 last_login_at < before_ms 的 users 行（简化：仅删用户行，
    /// 用户数据目录/命名空间数据保留；except 用户受保护不删）。返回被删用户名列表
    /// GAP #95：清理不活跃用户（users 行 + 用户级数据行 + 数据目录；except 用户除外）
    pub async fn clear_inactive_users(
        &self,
        before_ms: i64,
        except: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut tx = self.pool.begin().await?;
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT username FROM users WHERE last_login_at < ?1")
                .bind(before_ms)
                .fetch_all(&mut *tx)
                .await?;
        let mut deleted = Vec::new();
        for username in rows {
            if except == Some(username.as_str()) {
                continue;
            }
            sqlx::query("DELETE FROM users WHERE username = ?1")
                .bind(&username)
                .execute(&mut *tx)
                .await?;
            delete_user_rows(&mut tx, &username).await?;
            deleted.push(username);
        }
        tx.commit().await?;
        invalidate_cookie_snapshots();
        for username in &deleted {
            self.remove_user_data_dir(username);
        }
        Ok(deleted)
    }

    // ---------------- F-32 用户管理 ----------------

    /// 全部用户列表（含权限/启用状态；按创建时间排序）
    pub async fn list_users(&self) -> Result<Vec<User>> {
        let users = sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at, username")
            .fetch_all(&self.pool)
            .await?;
        Ok(users)
    }

    /// 更新用户权限/限额（None 字段不更新；用户不存在返回 0 行）
    #[allow(clippy::too_many_arguments)]
    pub async fn update_user_permissions(
        &self,
        username: &str,
        enable_webdav: Option<bool>,
        enable_local_store: Option<bool>,
        enable_book_source: Option<bool>,
        enable_rss_source: Option<bool>,
        book_source_limit: Option<i64>,
        book_limit: Option<i64>,
        is_admin: Option<bool>,
        disabled: Option<bool>,
    ) -> Result<u64> {
        let r = sqlx::query(
            r#"
            UPDATE users SET
                enable_webdav     = COALESCE(?1, enable_webdav),
                enable_local_store = COALESCE(?2, enable_local_store),
                enable_book_source = COALESCE(?3, enable_book_source),
                enable_rss_source  = COALESCE(?4, enable_rss_source),
                book_source_limit  = COALESCE(?5, book_source_limit),
                book_limit         = COALESCE(?6, book_limit),
                is_admin           = COALESCE(?7, is_admin),
                disabled           = COALESCE(?8, disabled)
            WHERE username = ?9
            "#,
        )
        .bind(enable_webdav)
        .bind(enable_local_store)
        .bind(enable_book_source)
        .bind(enable_rss_source)
        .bind(book_source_limit)
        .bind(book_limit)
        .bind(is_admin)
        .bind(disabled)
        .bind(username)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 管理员数量（最后一名管理员禁止撤销/删除——保证系统配置始终可管理）
    pub async fn count_admins(&self) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 无管理员时把最早用户提升为管理员（优先用户名 admin，其次最早创建）
    pub async fn ensure_admin_user(&self) -> Result<()> {
        if self.count_admins().await? > 0 {
            return Ok(());
        }
        let user = sqlx::query_as::<_, User>(
            "SELECT * FROM users ORDER BY CASE WHEN username = 'admin' THEN 0 ELSE 1 END, created_at, username LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        if let Some(user) = user {
            sqlx::query("UPDATE users SET is_admin = 1 WHERE username = ?1")
                .bind(&user.username)
                .execute(&self.pool)
                .await?;
            tracing::info!("无管理员用户，提升 {} 为管理员", user.username);
        }
        Ok(())
    }

    /// 一次性纠正旧版注册默认权限（v5.0.4 及以前：四权限全关 + 书源 100/书籍 200；
    /// 新版默认：全开 + 书源 80000/书籍 5000）。
    ///
    /// 只更新仍精确等于旧错误默认值的用户行——已手动调整过的用户不被覆盖。
    /// v5.2.0 起不再依赖旧标记：即使旧版本已写过 `user_permission_defaults_v500`，
    /// 只要还有精确等于旧错误默认值的行就继续修正（条件本身保证不会覆盖人工改动），
    /// 新标记仅用于跳过完全无残留的场景。
    pub async fn migrate_user_permission_defaults(&self) -> Result<()> {
        const MARKER: &str = "user_permission_defaults_v520";
        if self.get_system_setting(MARKER).await?.is_some() {
            return Ok(());
        }
        let r = sqlx::query(
            "UPDATE users SET \
                 enable_webdav = 1, enable_local_store = 1, \
                 enable_book_source = 1, enable_rss_source = 1, \
                 book_source_limit = 80000, book_limit = 5000 \
             WHERE enable_webdav = 0 AND enable_local_store = 0 \
               AND enable_book_source = 0 AND enable_rss_source = 0 \
               AND book_source_limit = 100 AND book_limit = 200",
        )
        .execute(&self.pool)
        .await?;
        if r.rows_affected() > 0 {
            tracing::info!(
                "一次性修正 {} 个用户的默认权限（旧错误默认 → 全开 + 80000/5000）",
                r.rows_affected()
            );
        }
        self.set_system_setting(MARKER, "1").await?;
        Ok(())
    }

    /// GAP #95：删除用户并清理全部用户数据（用户级表行 + storage/data/{username} 目录）。
    ///
    /// 表：books/book_sources/book_source_cookies/reading_stats/user_config/rss_sources/
    /// rss_articles/bookmarks/book_groups/replace_rules/http_tts_list/source_subs/txt_toc_rules；
    /// 章节/目录缓存仅当该书 URL 不再被其他用户拥有时删除（book_url 为全局主键）；
    /// 目录递归删除（含 webdav/opds_files 等）。用户不存在返回 0。
    pub async fn delete_user(&self, username: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query("DELETE FROM users WHERE username = ?1")
            .bind(username)
            .execute(&mut *tx)
            .await?;
        if r.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(0);
        }
        delete_user_rows(&mut tx, username).await?;
        tx.commit().await?;
        invalidate_cookie_snapshots();
        self.remove_user_data_dir(username);
        Ok(1)
    }

    /// 批量删除用户（单事务：全部用户删除 + 数据清理原子提交，任一失败整体回滚；
    /// 成功后事务外逐个删除用户数据目录）。已存在用户才计入返回数。
    pub async fn delete_users(&self, usernames: &[String]) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let mut deleted = 0u64;
        let mut removed: Vec<String> = Vec::new();
        for username in usernames {
            let r = sqlx::query("DELETE FROM users WHERE username = ?1")
                .bind(username)
                .execute(&mut *tx)
                .await?;
            if r.rows_affected() > 0 {
                delete_user_rows(&mut tx, username).await?;
                deleted += 1;
                removed.push(username.clone());
            }
        }
        tx.commit().await?;
        invalidate_cookie_snapshots();
        for username in removed {
            self.remove_user_data_dir(&username);
        }
        Ok(deleted)
    }

    /// 删除 storage/data/{username} 目录（递归——含 webdav/opds_files 等）；失败仅告警
    fn remove_user_data_dir(&self, username: &str) {
        // 防御：注册已限 ^[a-zA-Z0-9]+$，再挡一层路径穿越
        if username.is_empty()
            || username.contains(['/', '\\'])
            || username.contains("..")
            || !username.chars().all(|c| c.is_ascii_alphanumeric())
        {
            tracing::warn!("跳过用户数据目录清理（非法用户名）: {username}");
            return;
        }
        let dir = self.config.storage_dir().join("data").join(username);
        if !dir.exists() {
            return;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(_) => tracing::info!("已删除用户数据目录: {}", dir.display()),
            Err(e) => tracing::warn!("删除用户数据目录失败（文件占用？）{}: {e}", dir.display()),
        }
    }

    /// 重置用户密码（新 salt + 加密密码；清空 token 使旧会话立即失效）
    pub async fn reset_user_password(
        &self,
        username: &str,
        salt: &str,
        encrypted_password: &str,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE users SET password = ?1, salt = ?2, token = '' WHERE username = ?3",
        )
        .bind(encrypted_password)
        .bind(salt)
        .bind(username)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 密码哈希升级（legacy MD5 → argon2id PHC）：仅更新 password 列，
    /// 不动 salt/token——登录成功路径自动迁移时不能使当前会话失效。
    pub async fn upgrade_user_password_hash(&self, username: &str, phc: &str) -> Result<u64> {
        let r = sqlx::query("UPDATE users SET password = ?1 WHERE username = ?2")
            .bind(phc)
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ---------------- F-35 定时书架更新 ----------------

    /// F-35 可更新书架书（can_update=1，全命名空间）
    pub async fn list_updatable_books(&self) -> Result<Vec<Book>> {
        let books = sqlx::query_as::<_, Book>("SELECT * FROM books WHERE can_update = 1")
            .fetch_all(&self.pool)
            .await?;
        Ok(books)
    }

    /// F-35 回写更新检查结果：最新章节标题/总数/检查时间/检查次数
    pub async fn update_book_update_info(
        &self,
        ns: &str,
        book_url: &str,
        latest_title: Option<&str>,
        total_num: i64,
        checked_at: i64,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE books SET                 latest_chapter_title = COALESCE(?3, latest_chapter_title),                 latest_chapter_time = CASE WHEN ?3 IS NOT NULL THEN ?4 ELSE latest_chapter_time END,                 total_chapter_num = ?5,                 last_check_time = ?6,                 last_check_count = last_check_count + 1                 WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(latest_title)
        .bind(checked_at)
        .bind(total_num)
        .bind(checked_at)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    // ---------------- F-39 WebDAV 备份 ----------------

    /// F-39 用户数据完整快照 zip（legacy backupFileNames 全集 + 书源登录态）写入
    /// storage/data/{ns}/webdav/legado/backup-{ts}.zip；返回 zip 文件路径。
    ///
    /// 增量安全：时间戳命名 + 同秒冲突追加序号——旧备份永不覆盖/删除
    /// （legacy createUserBackup 同语义：新备份 = 旧备份之上叠加当前数据）。
    pub async fn create_backup_zip(&self, ns: &str) -> Result<String> {
        let ts = chrono::Utc::now().format("%Y-%m-%d-%H%M%S");
        let legado = self.webdav_legado_dir(ns);
        let mut stem = format!("backup-{ts}");
        let mut seq = 1;
        while legado.join(format!("{stem}.zip")).exists() {
            stem = format!("backup-{ts}-{seq}");
            seq += 1;
        }
        self.write_backup_zip(ns, &stem).await
    }

    /// webdav 备份目录（storage/data/{ns}/webdav/legado）
    fn webdav_legado_dir(&self, ns: &str) -> std::path::PathBuf {
        self.config
            .storage_dir()
            .join("data")
            .join(ns)
            .join("webdav")
            .join("legado")
    }

    /// 打包 zip 写入 storage/data/{ns}/webdav/legado/{stem}.zip（
    /// GAP #57 自动备份与手动备份共用核心）；返回 zip 文件路径
    pub(crate) async fn write_backup_zip(&self, ns: &str, stem: &str) -> Result<String> {
        let legado = self.webdav_legado_dir(ns);
        std::fs::create_dir_all(&legado)?;
        let zip_path = legado.join(format!("{stem}.zip"));

        // 收集数据（legacy backupFileNames 全集：bookshelf/bookSource/bookmark/bookGroup/
        // rssSources/replaceRule/txtTocRule/userConfig/httpTTS + 书源登录态扩展）。
        // users 表不入包（legacy 同）：凭据不进备份 zip，账号体系由服务端管理。
        let books = self.list_books(ns).await?;
        let sources = self.get_book_sources(ns).await?;
        let bookmarks = sqlx::query_as::<_, crate::model::Bookmark>(
            "SELECT * FROM bookmarks WHERE user_namespace = ?1 ORDER BY created_at DESC, rowid DESC",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        let groups = self.list_book_groups(ns).await?;
        let rss_sources = self.get_rss_sources(ns).await?;
        let replace_rules = self.get_replace_rules(ns).await?;
        let txt_toc_rules = self.get_txt_toc_rules(ns).await?;
        let http_tts_list = self.get_http_tts_list(ns).await?;
        let cookies = self.list_cookies(ns).await?;
        // 用户配置全量（{键: 值} 对象；值原样字符串——restore 按字符串读回，往返无损）
        let config_rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT ns, COALESCE(config, '') FROM user_config WHERE user_namespace = ?1 ORDER BY ns",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        let mut user_configs = serde_json::Map::new();
        for (k, v) in config_rows {
            user_configs.insert(k, serde_json::Value::String(v));
        }
        let user_configs = serde_json::Value::Object(user_configs);

        let file = std::fs::File::create(&zip_path)?;
        let mut writer = zip::ZipWriter::new(file);
        write_zip_entry(
            &mut writer,
            "bookshelf.json",
            &serde_json::to_vec_pretty(&books)?,
        )?;
        write_zip_entry(
            &mut writer,
            "bookSource.json",
            &serde_json::to_vec_pretty(&sources)?,
        )?;
        write_zip_entry(
            &mut writer,
            "bookmark.json",
            &serde_json::to_vec_pretty(&bookmarks)?,
        )?;
        write_zip_entry(
            &mut writer,
            "bookGroup.json",
            &serde_json::to_vec_pretty(&groups)?,
        )?;
        write_zip_entry(
            &mut writer,
            "rssSources.json",
            &serde_json::to_vec_pretty(&rss_sources)?,
        )?;
        write_zip_entry(
            &mut writer,
            "replaceRule.json",
            &serde_json::to_vec_pretty(&replace_rules)?,
        )?;
        write_zip_entry(
            &mut writer,
            "txtTocRule.json",
            &serde_json::to_vec_pretty(&txt_toc_rules)?,
        )?;
        write_zip_entry(
            &mut writer,
            "userConfig.json",
            &serde_json::to_vec_pretty(&user_configs)?,
        )?;
        write_zip_entry(
            &mut writer,
            "httpTTS.json",
            &serde_json::to_vec_pretty(&http_tts_list)?,
        )?;
        write_zip_entry(
            &mut writer,
            "bookSourceCookies.json",
            &serde_json::to_vec_pretty(&cookies)?,
        )?;
        writer.finish()?;

        tracing::info!("备份完成 [{ns}]: {}", zip_path.display());
        Ok(zip_path.to_string_lossy().into_owned())
    }

    // ---------------- GAP #57 自动备份 ----------------

    /// GAP #57：清理自动备份 zip（webdav/legado/auto-*.zip），仅保留最近 keep 份；
    /// 返回删除数（文件名按日期字典序即时间序）
    pub fn prune_auto_backups(&self, ns: &str, keep: usize) -> usize {
        let legado = self.webdav_legado_dir(ns);
        let Ok(entries) = std::fs::read_dir(&legado) else {
            return 0;
        };
        let mut files: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("auto-") && n.ends_with(".zip"))
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        let mut removed = 0usize;
        while files.len() > keep {
            let oldest = files.remove(0);
            match std::fs::remove_file(&oldest) {
                Ok(_) => removed += 1,
                Err(e) => tracing::warn!("删除旧自动备份失败 {}: {e}", oldest.display()),
            }
        }
        removed
    }

    /// 定时任务命名空间集合：非 secure → [default]；secure → 全部用户 + 残留 default 目录
    pub async fn schedule_namespaces(&self) -> Vec<String> {
        if !self.config.secure {
            return vec!["default".to_string()];
        }
        let mut nss: Vec<String> = Vec::new();
        if let Ok(users) = self.list_users().await {
            nss.extend(users.iter().map(|u| u.username.clone()));
        }
        // secure 化之前遗留的 default 数据目录也纳入（存在才处理）
        let default_dir = self.config.storage_dir().join("data").join("default");
        if default_dir.is_dir() && !nss.iter().any(|n| n == "default") {
            nss.push("default".to_string());
        }
        nss
    }

    // ---------------- F-55 备份恢复（restoreFromZip / restoreFromWebdav 共用核心） ----------------

    /// F-55：从备份 zip 字节恢复（restoreFromZip/restoreFromWebdav 共用核心）
    ///
    /// 结构探测：条目在 zip 根 或 config/ 目录下（兼容 legacy 备份 zip 两种布局）；
    /// 书架文件兼容 bookshelf.json / books.json 两个命名。
    ///
    /// 逐项幂等：已存在时 overwrite=true 覆盖、否则跳过（计数进 skipped）；
    /// namespace 一律为当前用户（备份内 user_namespace 不生效）。
    pub async fn restore_backup_zip(
        &self,
        ns: &str,
        zip_bytes: &[u8],
        overwrite: bool,
    ) -> Result<RestoreReport> {
        let cursor = std::io::Cursor::new(zip_bytes.to_vec());
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| anyhow::anyhow!("备份文件不是有效的 zip：{e}"))?;
        // 预读全部条目到内存（zip 读取是同步 IO，避免跨 await 持有 archive）
        let mut entries: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        use std::io::Read;
        for i in 0..archive.len() {
            let mut f = archive.by_index(i)?;
            let name = f.name().to_string();
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes)?;
            entries.insert(name, bytes);
        }

        // 条目读取：根 或 config/ 前缀（legacy 两种布局）
        let entry = |name: &str| -> Option<&Vec<u8>> {
            entries
                .get(name)
                .or_else(|| entries.get(&format!("config/{name}")))
        };

        // 结构探测：一个识别条目都没有 → 拒绝（避免把任意 zip 当备份）
        let recognized = [
            "bookSource.json",
            "bookshelf.json",
            "books.json",
            "bookGroup.json",
            "replaceRule.json",
            "txtTocRule.json",
            "rssSources.json",
            "userConfig.json",
            "httpTTS.json",
            "bookmark.json",
            "bookSourceCookies.json",
        ];
        if !recognized.iter().any(|n| entry(n).is_some()) {
            return Err(anyhow::anyhow!("备份文件中没有可恢复的数据"));
        }

        let mut report = RestoreReport::default();

        // 书源（按 bookSourceUrl 覆盖或跳过）
        if let Some(bytes) = entry("bookSource.json") {
            match serde_json::from_slice::<Vec<crate::model::BookSource>>(bytes) {
                Ok(items) => {
                    for s in items {
                        if s.book_source_url.trim().is_empty() {
                            report.skipped.sources += 1;
                            continue;
                        }
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM book_sources WHERE user_namespace = ?1 AND book_source_url = ?2",
                                    ns,
                                    &s.book_source_url,
                                )
                                .await?
                        {
                            report.skipped.sources += 1;
                        } else {
                            self.save_book_source(ns, &s).await?;
                            report.restored.sources += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] bookSource.json 解析失败: {e}");
                    report.skipped.sources += 1;
                }
            }
        }

        // 书架（按 book_url upsert，namespace=当前用户；bookshelf.json / books.json 兼容）
        if let Some(bytes) = entry("bookshelf.json").or_else(|| entry("books.json")) {
            match serde_json::from_slice::<Vec<Book>>(bytes) {
                Ok(items) => {
                    for mut b in items {
                        if b.book_url.trim().is_empty() {
                            report.skipped.books += 1;
                            continue;
                        }
                        b.user_namespace = ns.to_string();
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM books WHERE user_namespace = ?1 AND book_url = ?2",
                                    ns,
                                    &b.book_url,
                                )
                                .await?
                        {
                            report.skipped.books += 1;
                        } else {
                            self.upsert_book(ns, &b).await?;
                            report.restored.books += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] bookshelf.json 解析失败: {e}");
                    report.skipped.books += 1;
                }
            }
        }

        // 书架分组（按 id 覆盖或跳过；id<=0 时自增新建）
        if let Some(bytes) = entry("bookGroup.json") {
            match serde_json::from_slice::<Vec<crate::model::BookGroup>>(bytes) {
                Ok(items) => {
                    for g in items {
                        if g.name.trim().is_empty() {
                            report.skipped.groups += 1;
                            continue;
                        }
                        if !overwrite && g.id > 0 {
                            let exists: bool = sqlx::query_scalar::<_, i64>(
                                "SELECT 1 FROM book_groups WHERE user_namespace = ?1 AND id = ?2",
                            )
                            .bind(ns)
                            .bind(g.id)
                            .fetch_optional(&self.pool)
                            .await?
                            .is_some();
                            if exists {
                                report.skipped.groups += 1;
                                continue;
                            }
                        }
                        self.save_book_group(ns, &g).await?;
                        report.restored.groups += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] bookGroup.json 解析失败: {e}");
                    report.skipped.groups += 1;
                }
            }
        }

        // 替换规则（按 id）
        if let Some(bytes) = entry("replaceRule.json") {
            match serde_json::from_slice::<Vec<crate::model::ReplaceRule>>(bytes) {
                Ok(items) => {
                    for mut r in items {
                        if r.id.trim().is_empty() {
                            r.id = uuid::Uuid::new_v4().simple().to_string();
                        }
                        if r.name.trim().is_empty() {
                            report.skipped.rules += 1;
                            continue;
                        }
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM replace_rules WHERE user_namespace = ?1 AND id = ?2",
                                    ns,
                                    &r.id,
                                )
                                .await?
                        {
                            report.skipped.rules += 1;
                        } else {
                            self.save_replace_rule(ns, &r).await?;
                            report.restored.rules += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] replaceRule.json 解析失败: {e}");
                    report.skipped.rules += 1;
                }
            }
        }

        // 自定义 TXT 目录规则（按 id）
        if let Some(bytes) = entry("txtTocRule.json") {
            match serde_json::from_slice::<Vec<crate::model::TxtTocRule>>(bytes) {
                Ok(items) => {
                    for mut r in items {
                        if r.id.trim().is_empty() {
                            r.id = uuid::Uuid::new_v4().simple().to_string();
                        }
                        if r.name.trim().is_empty() || r.rule.trim().is_empty() {
                            report.skipped.txt_rules += 1;
                            continue;
                        }
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM txt_toc_rules WHERE user_namespace = ?1 AND id = ?2",
                                    ns,
                                    &r.id,
                                )
                                .await?
                        {
                            report.skipped.txt_rules += 1;
                        } else {
                            self.save_txt_toc_rule(ns, &r).await?;
                            report.restored.txt_rules += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] txtTocRule.json 解析失败: {e}");
                    report.skipped.txt_rules += 1;
                }
            }
        }

        // RSS 源（按 sourceUrl；raw_json 存条目原文，未知字段不丢）
        if let Some(bytes) = entry("rssSources.json") {
            match serde_json::from_slice::<serde_json::Value>(bytes) {
                Ok(serde_json::Value::Array(arr)) => {
                    for v in arr {
                        let mut s: crate::model::RssSource = match serde_json::from_value(v.clone())
                        {
                            Ok(s) => s,
                            Err(_) => {
                                report.skipped.rss += 1;
                                continue;
                            }
                        };
                        if s.source_url.trim().is_empty() || s.source_name.trim().is_empty() {
                            report.skipped.rss += 1;
                            continue;
                        }
                        s.raw_json = Some(v.to_string());
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM rss_sources WHERE user_namespace = ?1 AND rss_source_url = ?2",
                                    ns,
                                    &s.source_url,
                                )
                                .await?
                        {
                            report.skipped.rss += 1;
                        } else {
                            self.save_rss_source(ns, &s).await?;
                            report.restored.rss += 1;
                        }
                    }
                }
                Ok(_) => report.skipped.rss += 1,
                Err(e) => {
                    tracing::warn!("restore [{ns}] rssSources.json 解析失败: {e}");
                    report.skipped.rss += 1;
                }
            }
        }

        // 用户配置（userConfig.json = {键: 值} 对象；或 [{key, value}] 数组）
        if let Some(bytes) = entry("userConfig.json") {
            match serde_json::from_slice::<serde_json::Value>(bytes) {
                Ok(serde_json::Value::Object(map)) => {
                    for (k, v) in map {
                        let raw = match &v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        if !overwrite && self.get_user_config(ns, &k).await?.is_some() {
                            report.skipped.config += 1;
                        } else {
                            self.save_user_config(ns, &k, &raw).await?;
                            report.restored.config += 1;
                        }
                    }
                }
                Ok(serde_json::Value::Array(arr)) => {
                    for v in arr {
                        let Some(k) = v.get("key").and_then(|x| x.as_str()) else {
                            report.skipped.config += 1;
                            continue;
                        };
                        let raw = match v.get("value") {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        if !overwrite && self.get_user_config(ns, k).await?.is_some() {
                            report.skipped.config += 1;
                        } else {
                            self.save_user_config(ns, k, &raw).await?;
                            report.restored.config += 1;
                        }
                    }
                }
                Ok(_) => report.skipped.config += 1,
                Err(e) => {
                    tracing::warn!("restore [{ns}] userConfig.json 解析失败: {e}");
                    report.skipped.config += 1;
                }
            }
        }

        // HttpTTS 听书源（按 url）
        if let Some(bytes) = entry("httpTTS.json") {
            match serde_json::from_slice::<Vec<crate::model::HttpTts>>(bytes) {
                Ok(items) => {
                    for t in items {
                        if t.url.trim().is_empty() || t.name.trim().is_empty() {
                            report.skipped.tts += 1;
                            continue;
                        }
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM http_tts_list WHERE user_namespace = ?1 AND url = ?2",
                                    ns,
                                    &t.url,
                                )
                                .await?
                        {
                            report.skipped.tts += 1;
                        } else {
                            self.save_http_tts(ns, &t).await?;
                            report.restored.tts += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] httpTTS.json 解析失败: {e}");
                    report.skipped.tts += 1;
                }
            }
        }

        // 书签（按 bookUrl+title）——备份 zip 含 bookmark.json，一并恢复
        if let Some(bytes) = entry("bookmark.json") {
            match serde_json::from_slice::<Vec<crate::model::Bookmark>>(bytes) {
                Ok(items) => {
                    for bm in items {
                        if bm.book_url.trim().is_empty() || bm.title.trim().is_empty() {
                            report.skipped.bookmarks += 1;
                            continue;
                        }
                        let exists: bool = sqlx::query_scalar::<_, i64>(
                            "SELECT 1 FROM bookmarks WHERE user_namespace = ?1 AND book_url = ?2 AND title = ?3",
                        )
                        .bind(ns)
                        .bind(&bm.book_url)
                        .bind(&bm.title)
                        .fetch_optional(&self.pool)
                        .await?
                        .is_some();
                        if !overwrite && exists {
                            report.skipped.bookmarks += 1;
                        } else {
                            self.save_bookmark(ns, &bm).await?;
                            report.restored.bookmarks += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] bookmark.json 解析失败: {e}");
                    report.skipped.bookmarks += 1;
                }
            }
        }

        // 书源登录态（按 sourceUrl；cookie/user_agent/login_header 整行恢复）
        if let Some(bytes) = entry("bookSourceCookies.json") {
            match serde_json::from_slice::<Vec<crate::model::CookieRow>>(bytes) {
                Ok(items) => {
                    for c in items {
                        // 空 source_url 或全空登录态 → 无意义行跳过
                        if c.source_url.trim().is_empty()
                            || (c.cookie.trim().is_empty()
                                && c.user_agent.trim().is_empty()
                                && c.login_header.trim().is_empty())
                        {
                            report.skipped.cookies += 1;
                            continue;
                        }
                        if !overwrite
                            && self
                                .table_exists(
                                    "SELECT 1 FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2",
                                    ns,
                                    &c.source_url,
                                )
                                .await?
                        {
                            report.skipped.cookies += 1;
                        } else {
                            sqlx::query(
                                "INSERT OR REPLACE INTO book_source_cookies \
                                 (user_namespace, source_url, cookie, user_agent, login_header, updated_at) \
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            )
                            .bind(ns)
                            .bind(&c.source_url)
                            .bind(&c.cookie)
                            .bind(&c.user_agent)
                            .bind(&c.login_header)
                            .bind(c.updated_at)
                            .execute(&self.pool)
                            .await?;
                            // 恢复会大量改写 cookie：立刻让快照缓存失效（一次性 restore 内
                            // 每行失效的开销只是清一个几项的 HashMap，可忽略）
                            invalidate_cookie_snapshots();
                            report.restored.cookies += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("restore [{ns}] bookSourceCookies.json 解析失败: {e}");
                    report.skipped.cookies += 1;
                }
            }
        }

        tracing::info!(
            "恢复完成 [{ns}] overwrite={overwrite}: restored={:?} skipped={:?}",
            report.restored,
            report.skipped
        );
        Ok(report)
    }

    /// 表存在性探测（幂等判断共用；两参版本）
    async fn table_exists(&self, sql: &str, a: &str, b: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(sql)
            .bind(a)
            .bind(b)
            .fetch_optional(&self.pool)
            .await?
            .is_some())
    }
}

/// 各类目恢复计数（restored/skipped 共用）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreCounts {
    /// 书源（按 bookSourceUrl）
    pub sources: u64,
    /// 书架书（按 book_url）
    pub books: u64,
    /// 书架分组（按 id）
    pub groups: u64,
    /// 替换规则（按 id）
    pub rules: u64,
    /// 自定义 TXT 目录规则（按 id）
    #[serde(rename = "txtRules")]
    pub txt_rules: u64,
    /// RSS 源（按 sourceUrl）
    pub rss: u64,
    /// 用户配置（按配置键）
    pub config: u64,
    /// HttpTTS 听书源（按 url）
    pub tts: u64,
    /// 书签（按 bookUrl+title）
    pub bookmarks: u64,
    /// 书源登录态（按 sourceUrl）
    pub cookies: u64,
}

/// 恢复报告（restoreFromZip / restoreFromWebdav 返回：restored/skipped 各类目计数）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub restored: RestoreCounts,
    pub skipped: RestoreCounts,
}

/// zip 单条目写入（F-39）
fn write_zip_entry(
    writer: &mut zip::ZipWriter<std::fs::File>,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    use std::io::Write;
    writer.start_file(name, zip::write::FileOptions::default())?;
    writer.write_all(bytes)?;
    Ok(())
}

/// 目录解析结果是否可信（够格回写 `total_chapter_num` / `latest_chapter_title`）。
///
/// 源站多页目录若翻页提前终止（末页 next 解析为空或页面结构变化），`analyze_toc`
/// 会返回一份"结构正常但缺章"的目录；一旦拿它回写书架总数，前端进度/未读/「设为读完」
/// 就全部按错值算（绍宋：书架 60 章 / 实际 400 章缓存，读到第 100 章会被算成"已读完"）。
///
/// 判据：解析章数既少于书架已知总数、又少于已缓存章数 → 判为残缺。
/// 两条下界取"或"：卷末总结等让已缓存章数可能多于目录条目（绍宋缓存 400 条 / 目录 395 章），
/// 只把缓存当上界会误杀正常目录；反过来只信 prev 也不行——prev 本身可能已被残缺血洗过。
/// `cached = 0`（一章正文都没缓存）时缓存不构成下界，此时仅凭 prev 判断，否则
/// 「prev 有 400 章、这次只解析出 50 章」会被 `parsed >= 0` 放行。
/// `prev = 0` 的新书没有已知总数，先信任解析结果（交给缓存与阅读器的下界去纠正），
/// 但 `parsed = 0` 永远不可信。
pub(crate) fn plausible_chapter_total(parsed: i64, prev: i64, cached: i64) -> bool {
    parsed > 0 && (parsed >= prev || (cached > 0 && parsed >= cached))
}

/// 这次目录章数是"从哪儿看到的"——决定它够不够格回写 `books.total_chapter_num`。
///
/// 阅读器目录有三条来源，可信度不同（都是「打开书籍就该修好章数」的一部分，
/// 用户不该为章数不一致手动点「刷新目录」）：
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TocWitness {
    /// 真抓到源站目录（`analyze_toc` 成功，空目录 / 残缺已在调用处排除）。
    /// 唯一可**双向**回写的来源：源站真删章（完本精简、作者合并卷）也该跟着降。
    Fetched,
    /// 目录缓存命中（`toc_cache`）。缓存里既可能是上次真抓到的高质量目录，
    /// 也可能是离线兜底合成的目录（见 router 的 `toc_from_cached_chapters`，它写同一个 key），
    /// 两种情况只看 JSON 分不出来，故只允许**只增不减**：比记录多 ⇒ 记录必然陈旧
    /// （残缺目录只会写小值，缓存合成也只会比真相少）；比记录少 ⇒ 无从判断，不写。
    /// 离线兜底（用 `book_chapters` 已缓存正文临时合成的目录）走的就是这一档：
    /// 它天然可能少于真相（正文没缓存齐），所以只能往上修，绝不能降。
    CachedToc,
}

/// 目录观测 → 该把 `total_chapter_num` 写成什么（`None` = 这次不写）。
///
/// 这是「什么时候允许回写章数」的唯一判据（阅读器目录路径 + 夜间书架更新共用
/// `plausible_chapter_total` 这条质量门，这里只是在它上面再叠加"来源可信度"）：
///
/// - `toc_len <= 0`：空目录/解析失败，永远不写（空值入库会把书永久写成 0 章）；
/// - `Fetched`：过 `plausible_chapter_total`（既不少于书架记录、也不少于已缓存章数）
///   才写，双向生效；与记录相同则不产生无意义写；
/// - `CachedToc`（含离线兜底合成）：只在 `toc_len > prev` 时写（理由见 [`TocWitness::CachedToc`]）。
///
/// `cached`（已缓存章数）只在 `Fetched` 下参与判断，其余来源传 0 即可。
pub(crate) fn chapter_total_writeback(
    witness: TocWitness,
    toc_len: i64,
    prev: i64,
    cached: i64,
) -> Option<i64> {
    if toc_len <= 0 {
        return None;
    }
    match witness {
        TocWitness::Fetched => {
            (plausible_chapter_total(toc_len, prev, cached) && toc_len != prev).then_some(toc_len)
        }
        TocWitness::CachedToc => (toc_len > prev).then_some(toc_len),
    }
}

/// F-35：扫描 books 表 can_update=1 的书 → analyze_toc → 回写
/// latest_chapter_title / total_chapter_num（单本失败跳过，不影响其余）
pub async fn run_shelf_update(storage: &Storage) -> Result<usize> {
    let books = storage.list_updatable_books().await?;
    let mut updated = 0usize;
    for book in books {
        // 本地书（local:// 或 storage 文件型）无书源可抓，跳过
        if book.origin == "local"
            || book.book_url.starts_with("local://")
            || book.book_url.ends_with(".txt")
        {
            continue;
        }
        // 书源缺失（用户/系统均无）→ 无法抓取，跳过
        let Ok(Some(source)) = storage
            .find_book_source(&book.user_namespace, &book.origin)
            .await
        else {
            continue;
        };
        // 目录端点常与书页不同(如 fq 的 /toc/<id>): toc_url 为空时按 ruleBookInfo 推导,
        // 与 getChapterList 走同一解析(含 book vars 缓存), 否则会把书页当目录页抓 → 0 章
        let toc_url = if book.toc_url.trim().is_empty() {
            crate::service::book::resolve_toc_url(&book.user_namespace, &source, &book.book_url)
                .await
        } else {
            book.toc_url.clone()
        };
        match crate::service::book::analyze_toc(
            &book.user_namespace,
            &toc_url,
            &source,
            20,
            Some(&book.name),
            &book.book_url,
        )
        .await
        {
            Ok(chapters) if !chapters.is_empty() => {
                let non_volume: Vec<&crate::model::book_chapter::BookChapter> =
                    chapters.iter().filter(|c| !c.is_volume).collect();
                let latest = non_volume.last().map(|c| c.title.clone());
                let total = non_volume.len() as i64;
                // 质量门（与阅读器 getChapterList 同一判据）：残缺目录既不回写章数/最新章节，
                // 也不失效目录缓存——否则 total_chapter_num 被永久毒化成小值。
                // 已缓存章数是第二道锚：prev 一旦被残缺血洗过（绍宋 60）就失效，缓存仍能拦。
                let cached = storage
                    .count_book_chapters(&book.user_namespace, &book.book_url)
                    .await
                    .unwrap_or(0);
                if !plausible_chapter_total(total, book.total_chapter_num, cached) {
                    tracing::warn!(
                        "书架更新跳过章节回写：目录疑似残缺 [{}] {}（解析 {total} 章 vs 书架 {} 章 / 已缓存 {cached} 章）",
                        book.book_url,
                        book.name,
                        book.total_chapter_num
                    );
                    continue;
                }
                let now = chrono::Utc::now().timestamp_millis();
                match storage
                    .update_book_update_info(
                        &book.user_namespace,
                        &book.book_url,
                        latest.as_deref(),
                        total,
                        now,
                    )
                    .await
                {
                    Ok(_) => {
                        updated += 1;
                        // 章数有变化 → 旧目录缓存作废（阅读器下次重取，保持与书架一致）
                        if total != book.total_chapter_num {
                            match storage
                                .clear_toc_cache_for_book(&book.user_namespace, &book.book_url)
                                .await
                            {
                                Ok(n) if n > 0 => tracing::info!(
                                    "章数变化({}→{})，失效目录缓存 {n} 条 [{}]",
                                    book.total_chapter_num,
                                    total,
                                    book.book_url
                                ),
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!("失效目录缓存失败 [{}]: {e:#}", book.book_url)
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("书架更新回写失败 [{}]: {e:#}", book.book_url),
                }
            }
            Ok(_) => {} // 无章节规则/空目录：无可更新内容，跳过
            Err(e) => tracing::warn!("书架更新跳过 [{}]: {e:#}", book.book_url),
        }
    }
    Ok(updated)
}

/// 幂等补列：列不存在则 ALTER TABLE ADD COLUMN（旧库升级用）
/// 规范化 baseUrl（scheme://host[:port]，去尾斜杠/路径/查询）——
/// 书源 cookie 按 base 匹配：请求 https://a.com/book/1 命中 source_url https://a.com
/// GAP #95：删除用户命名空间下的全部用户级数据行（事务内调用）
///
/// 覆盖全部含 user_namespace 列的表；章节/目录缓存（全局 book_url 主键）仅当
/// 该书 URL 不再被其他用户拥有时才删除，避免误删他用户章节。
async fn delete_user_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM book_chapters WHERE book_url IN (
            SELECT book_url FROM books WHERE user_namespace = ?1
              AND book_url NOT IN (SELECT book_url FROM books WHERE user_namespace != ?1))",
    )
    .bind(username)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "DELETE FROM toc_cache WHERE book_url IN (
            SELECT book_url FROM books WHERE user_namespace = ?1
              AND book_url NOT IN (SELECT book_url FROM books WHERE user_namespace != ?1))",
    )
    .bind(username)
    .execute(&mut **tx)
    .await?;
    // 表名单为内部硬编码常量，无注入面
    for table in [
        "books",
        "book_sources",
        "book_source_cookies",
        "reading_stats",
        "user_config",
        "rss_sources",
        "rss_articles",
        "bookmarks",
        "book_groups",
        "replace_rules",
        "http_tts_list",
        "source_subs",
        "txt_toc_rules",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE user_namespace = ?1"))
            .bind(username)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub(crate) fn normalize_base(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // 无 scheme 时补 https://（容忍裸 host 写法）
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let parsed = url::Url::parse(&with_scheme).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{}://{host}{port}", parsed.scheme()))
}

async fn ensure_column(pool: &SqlitePool, table: &str, column: &str) -> anyhow::Result<()> {
    ensure_column_typed(pool, table, column, "TEXT").await
}

/// GAP 117：从 cover_url（/assets/{ns}/covers/{file}）提取文件名；
/// 仅接受纯文件名（无路径分隔符，防穿越）且命名空间匹配时返回
fn cover_file_name(ns: &str, cover_url: &str) -> Option<String> {
    let prefix = format!("/assets/{ns}/covers/");
    let file = cover_url.strip_prefix(&prefix)?;
    if file.is_empty() || file.contains('/') || file.contains('\\') || file.contains("..") {
        return None;
    }
    Some(file.to_string())
}

/// 幂等补列（带列类型；GAP 170：local_file 等双轨同步列按类型补充）
async fn ensure_column_typed(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    sql_type: &str,
) -> anyhow::Result<()> {
    let row: (i64,) = sqlx::query_as(&format!(
        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = '{column}'"
    ))
    .fetch_one(pool)
    .await?;
    if row.0 == 0 {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}");
        sqlx::query(&sql).execute(pool).await?;
        tracing::info!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}");
    }
    Ok(())
}

/// 旧库 books 表重建：local_epub/local_pdf 列类型 TEXT → INTEGER（Book 模型为 bool；
/// TEXT 亲和性会把 bool 写成文本，读回时解码失败）。重建在事务内完成：改名 → 建新表 →
/// 按列名交集动态拷数据（兼容任意旧表形态）→ 删旧表。
async fn rebuild_books_bool_columns(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("ALTER TABLE books RENAME TO books_old")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE books (
            book_url TEXT,
            name TEXT DEFAULT '',
            author TEXT DEFAULT '',
            origin TEXT DEFAULT '',
            origin_name TEXT DEFAULT '',
            toc_url TEXT DEFAULT '',
            kind TEXT,
            custom_tag TEXT,
            cover_url TEXT,
            custom_cover_url TEXT,
            intro TEXT,
            custom_intro TEXT,
            charset TEXT,
            type INTEGER DEFAULT 0,
            group_name INTEGER DEFAULT 0,
            latest_chapter_title TEXT,
            latest_chapter_time INTEGER DEFAULT 0,
            last_check_time INTEGER DEFAULT 0,
            last_check_count INTEGER DEFAULT 0,
            total_chapter_num INTEGER DEFAULT 0,
            dur_chapter_title TEXT,
            dur_chapter_index INTEGER DEFAULT 0,
            dur_chapter_pos INTEGER DEFAULT 0,
            dur_chapter_time INTEGER DEFAULT 0,
            word_count TEXT,
            can_update INTEGER DEFAULT 1,
            order_num INTEGER DEFAULT 0,
            origin_order INTEGER DEFAULT 0,
            use_replace_rule INTEGER DEFAULT 1,
            variable TEXT,
            read_config TEXT,
            is_in_shelf INTEGER DEFAULT 1,
            cbz INTEGER DEFAULT 0,
            display_cover TEXT,
            display_intro TEXT,
            local_epub INTEGER DEFAULT 0,
            local_pdf INTEGER DEFAULT 0,
            pdf INTEGER DEFAULT 0,
            split_long_chapter INTEGER DEFAULT 0,
            last_check_error TEXT,
            info_html TEXT,
            toc_html TEXT,
            user_namespace TEXT DEFAULT '',
            created_at INTEGER DEFAULT 0,
            raw_json TEXT,
            local_file TEXT,
            local_file_mtime INTEGER DEFAULT 0,
            local_file_size INTEGER DEFAULT 0,
            local_file_deleted INTEGER DEFAULT 0,
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
    )
    .execute(&mut *tx)
    .await?;
    // 旧表实际存在的列（与新表按列名交集，保序）→ 动态 INSERT ... SELECT
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('books_old')              WHERE name IN (SELECT name FROM pragma_table_info('books'))",
    )
    .fetch_all(&mut *tx)
    .await?;
    if !cols.is_empty() {
        let quoted: Vec<String> = cols.iter().map(|c| format!("\"{c}\"")).collect();
        let col_list = quoted.join(", ");
        let sql = format!("INSERT INTO books ({col_list}) SELECT {col_list} FROM books_old");
        sqlx::query(&sql).execute(&mut *tx).await?;
    }
    sqlx::query("DROP TABLE books_old")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// 单条书源 upsert（save_book_source / save_book_sources 共用；
/// raw_json 由 serde 按 camelCase 重新序列化，序列化时跳过 user_namespace / raw_json 内部字段）
///
/// 用 INSERT ... ON CONFLICT DO UPDATE 而非 INSERT OR REPLACE：
/// REPLACE 会先删后插，未列出的列（use_count/use_ts 使用统计）会被重置为默认值；
/// DO UPDATE 只覆盖客户端字段，统计列保持不变（客户端保存/导入不会清零计数）。
async fn upsert_book_source<'e, E>(
    executor: E,
    ns: &str,
    source: &crate::model::BookSource,
) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let raw_json = serde_json::to_string(source)?;
    sqlx::query(
        r#"
        INSERT INTO book_sources
            (book_source_url, book_source_name, book_source_group, book_source_type,
             book_url_pattern, custom_order, enabled, enabled_explore, enabled_cookie_jar,
             concurrent_rate, js_lib, header, proxy_url, login_url, login_ui, login_check_js, login_js,
             book_source_comment, variable_comment, last_update_time, respond_time,
             weight, explore_url, search_url, rule_explore, rule_search, rule_book_info,
             rule_toc, rule_content, rule_related, search_rule, explore_rule, book_info_rule, toc_rule,
             content_rule, key, tag, logger, variable, user_namespace, hidden, raw_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29,
                ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42)
        ON CONFLICT(book_source_url, user_namespace) DO UPDATE SET
            book_source_name = excluded.book_source_name,
            book_source_group = excluded.book_source_group,
            book_source_type = excluded.book_source_type,
            book_url_pattern = excluded.book_url_pattern,
            custom_order = excluded.custom_order,
            enabled = excluded.enabled,
            enabled_explore = excluded.enabled_explore,
            enabled_cookie_jar = excluded.enabled_cookie_jar,
            concurrent_rate = excluded.concurrent_rate,
            js_lib = excluded.js_lib,
            header = excluded.header,
            proxy_url = excluded.proxy_url,
            login_url = excluded.login_url,
            login_ui = excluded.login_ui,
            login_check_js = excluded.login_check_js,
            login_js = excluded.login_js,
            book_source_comment = excluded.book_source_comment,
            variable_comment = excluded.variable_comment,
            last_update_time = excluded.last_update_time,
            respond_time = excluded.respond_time,
            weight = excluded.weight,
            explore_url = excluded.explore_url,
            search_url = excluded.search_url,
            rule_explore = excluded.rule_explore,
            rule_search = excluded.rule_search,
            rule_book_info = excluded.rule_book_info,
            rule_toc = excluded.rule_toc,
            rule_content = excluded.rule_content,
            rule_related = excluded.rule_related,
            search_rule = excluded.search_rule,
            explore_rule = excluded.explore_rule,
            book_info_rule = excluded.book_info_rule,
            toc_rule = excluded.toc_rule,
            content_rule = excluded.content_rule,
            key = excluded.key,
            tag = excluded.tag,
            logger = excluded.logger,
            variable = excluded.variable,
            user_namespace = excluded.user_namespace,
            hidden = excluded.hidden,
            raw_json = excluded.raw_json
        "#,
    )
    .bind(&source.book_source_url)
    .bind(&source.book_source_name)
    .bind(&source.book_source_group)
    .bind(source.book_source_type)
    .bind(&source.book_url_pattern)
    .bind(source.custom_order)
    .bind(source.enabled)
    .bind(source.enabled_explore)
    .bind(source.enabled_cookie_jar)
    .bind(&source.concurrent_rate)
    .bind(&source.js_lib)
    .bind(&source.header)
    .bind(&source.proxy_url)
    .bind(&source.login_url)
    .bind(&source.login_ui)
    .bind(&source.login_check_js)
    .bind(&source.login_js)
    .bind(&source.book_source_comment)
    .bind(&source.variable_comment)
    .bind(source.last_update_time)
    .bind(source.respond_time)
    .bind(source.weight)
    .bind(&source.explore_url)
    .bind(&source.search_url)
    .bind(&source.rule_explore)
    .bind(&source.rule_search)
    .bind(&source.rule_book_info)
    .bind(&source.rule_toc)
    .bind(&source.rule_content)
    .bind(&source.rule_related)
    .bind(&source.search_rule)
    .bind(&source.explore_rule)
    .bind(&source.book_info_rule)
    .bind(&source.toc_rule)
    .bind(&source.content_rule)
    .bind(&source.key)
    .bind(&source.tag)
    .bind(&source.logger)
    .bind(&source.variable)
    .bind(ns)
    .bind(source.hidden)
    .bind(raw_json)
    .execute(executor)
    .await?;
    Ok(())
}

/// 按 URL 查书源行（精确或前缀匹配；供 copy-on-write 复制 default 系统书源用）
async fn find_book_source_row<'e, E>(
    executor: E,
    ns: &str,
    url: &str,
) -> Result<Option<crate::model::BookSource>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let like = format!("{url}%");
    sqlx::query_as::<_, crate::model::BookSource>(
        "SELECT * FROM book_sources WHERE user_namespace = ?1 \
         AND (book_source_url = ?2 OR book_source_url LIKE ?3) \
         ORDER BY CASE WHEN book_source_url = ?2 THEN 0 ELSE 1 END, book_source_url \
         LIMIT 1",
    )
    .bind(ns)
    .bind(url)
    .bind(&like)
    .fetch_optional(executor)
    .await
    .map_err(Into::into)
}

/// 复制书源到指定命名空间并设置 hidden 标记（copy-on-write：不触碰原 default 行）
async fn upsert_book_source_hidden<'e, E>(
    executor: E,
    ns: &str,
    source: &crate::model::BookSource,
    hidden: bool,
) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let mut copy = source.clone();
    copy.user_namespace = ns.to_string();
    copy.hidden = hidden;
    upsert_book_source(executor, ns, &copy).await
}

/// saveBook 增量更新字段映射（JSON camelCase 键 → books 表列；固定白名单，防注入）
const BOOK_PATCH_COLUMNS: &[(&str, &str)] = &[
    ("tocUrl", "toc_url"),
    ("origin", "origin"),
    ("originName", "origin_name"),
    ("name", "name"),
    ("author", "author"),
    ("kind", "kind"),
    ("customTag", "custom_tag"),
    ("coverUrl", "cover_url"),
    ("customCoverUrl", "custom_cover_url"),
    ("intro", "intro"),
    ("customIntro", "custom_intro"),
    ("charset", "charset"),
    ("type", "type"),
    ("group", "group_name"),
    ("groupIds", "group_ids"),
    ("latestChapterTitle", "latest_chapter_title"),
    ("latestChapterTime", "latest_chapter_time"),
    ("lastCheckTime", "last_check_time"),
    ("lastCheckCount", "last_check_count"),
    ("totalChapterNum", "total_chapter_num"),
    ("durChapterTitle", "dur_chapter_title"),
    ("durChapterIndex", "dur_chapter_index"),
    ("durChapterPos", "dur_chapter_pos"),
    ("durChapterTime", "dur_chapter_time"),
    ("wordCount", "word_count"),
    ("canUpdate", "can_update"),
    ("order", "order_num"),
    ("originOrder", "origin_order"),
    ("useReplaceRule", "use_replace_rule"),
    ("variable", "variable"),
    ("readConfig", "read_config"),
    ("isInShelf", "is_in_shelf"),
    ("lastCheckError", "last_check_error"),
    ("infoHtml", "info_html"),
    ("tocHtml", "toc_html"),
    ("cbz", "cbz"),
    ("displayCover", "display_cover"),
    ("displayIntro", "display_intro"),
    ("localEpub", "local_epub"),
    ("localPdf", "local_pdf"),
    ("pdf", "pdf"),
    ("splitLongChapter", "split_long_chapter"),
    ("language", "language"),
    ("publisher", "publisher"),
    ("publishedAt", "published_at"),
    ("createdAt", "created_at"),
];

/// 按 JSON value 类型绑定（bool→0/1、数字→int、字符串→text、对象/数组→JSON 文本、null→NULL）
fn push_book_patch_value(qb: &mut sqlx::QueryBuilder<'_, sqlx::Sqlite>, value: &serde_json::Value) {
    match value {
        serde_json::Value::Bool(b) => {
            qb.push_bind(if *b { 1i64 } else { 0i64 });
        }
        serde_json::Value::Number(n) => {
            qb.push_bind(n.as_i64().unwrap_or(0));
        }
        serde_json::Value::String(s) => {
            qb.push_bind(s.clone());
        }
        serde_json::Value::Null => {
            qb.push_bind(Option::<String>::None);
        }
        other => {
            qb.push_bind(other.to_string());
        }
    }
}

impl Storage {
    /// 书籍详情持久缓存读: TTL 内命中返回反序列化值, 过期/缺失返回 None.
    pub async fn get_book_info_cache(
        &self,
        ns: &str,
        book_url: &str,
        ttl_ms: i64,
    ) -> Result<Option<serde_json::Value>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT info_json, updated_at FROM book_info_cache WHERE book_url = ?1 AND user_namespace = ?2",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some((json, ts)) if chrono::Utc::now().timestamp_millis() - ts <= ttl_ms => {
                Ok(serde_json::from_str(&json).ok())
            }
            _ => Ok(None),
        }
    }

    /// 书籍详情持久缓存写 (INSERT OR REPLACE).
    pub async fn set_book_info_cache(
        &self,
        ns: &str,
        book_url: &str,
        info: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO book_info_cache (book_url, user_namespace, info_json, updated_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(book_url)
        .bind(ns)
        .bind(info.to_string())
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 书籍详情持久缓存失效 (换源/删缓存时调用).
    pub async fn clear_book_info_cache(&self, ns: &str, book_url: &str) -> Result<()> {
        sqlx::query("DELETE FROM book_info_cache WHERE book_url = ?1 AND user_namespace = ?2")
            .bind(book_url)
            .bind(ns)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// 书源默认排序：weight DESC、custom_order、name（原 SQL ORDER BY 的等价实现）
fn sort_book_sources(v: &mut [crate::model::BookSource]) {
    v.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.custom_order.cmp(&b.custom_order))
            .then_with(|| a.book_source_name.cmp(&b.book_source_name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BookSource;

    /// 独立临时目录初始化存储（避免污染真实 storage/reader.db）
    async fn test_storage(tag: &str) -> Storage {
        let dir =
            std::env::temp_dir().join(format!("reader-storage-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        init(&config).await.expect("测试存储初始化失败")
    }

    /// 释放连接池并清理临时目录
    async fn cleanup(storage: Storage, tag: &str) {
        storage.pool.close().await;
        let dir =
            std::env::temp_dir().join(format!("reader-storage-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn source(url: &str, name: &str, group: Option<&str>) -> BookSource {
        BookSource {
            book_source_url: url.into(),
            book_source_name: name.into(),
            book_source_group: group.map(|g| g.to_string()),
            search_url: Some(format!("{url}/search?q={{{{key}}}}")),
            rule_search: Some(serde_json::json!({ "bookList": "$.data" })),
            enabled: true,
            enabled_explore: true,
            custom_order: 1,
            ..Default::default()
        }
    }

    /// 保存 → 查询 → 覆盖保存 → 删除 往返；raw_json camelCase 与 bookSource.json 一致
    #[tokio::test]
    async fn test_save_get_delete_roundtrip() {
        let storage = test_storage("roundtrip").await;
        let mut s = source("https://a.com", "A源", Some("小说 玄幻"));
        storage.save_book_source("default", &s).await.unwrap();

        let got = storage
            .get_book_source("default", "https://a.com")
            .await
            .unwrap()
            .expect("保存后应能查到");
        assert_eq!(got.book_source_name, "A源");
        assert_eq!(got.book_source_group.as_deref(), Some("小说 玄幻"));
        assert_eq!(got.user_namespace, "default");
        assert_eq!(
            got.rule_search,
            Some(serde_json::json!({ "bookList": "$.data" }))
        );

        // raw_json：camelCase、含规则字段、可反序列化回 BookSource
        let raw = got.raw_json.as_deref().expect("raw_json 应已写入");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(
            v.get("bookSourceUrl").is_some(),
            "raw_json 应为 camelCase: {raw}"
        );
        assert!(
            v.get("book_source_url").is_none(),
            "raw_json 不应含 snake_case: {raw}"
        );
        assert!(v.get("bookSourceName").is_some());
        assert_eq!(v["enabled"], serde_json::Value::Bool(true));
        let roundtrip: BookSource = serde_json::from_str(raw).unwrap();
        assert_eq!(roundtrip.book_source_url, "https://a.com");

        // 覆盖保存（改名 + 禁用）→ INSERT OR REPLACE 生效
        s.book_source_name = "A源v2".into();
        s.enabled = false;
        storage.save_book_source("default", &s).await.unwrap();
        let got2 = storage
            .get_book_source("default", "https://a.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got2.book_source_name, "A源v2");
        assert!(!got2.enabled);

        // 删除 → 查不到
        let affected = storage
            .delete_book_source("default", "https://a.com")
            .await
            .unwrap();
        assert_eq!(affected, 1);
        assert!(storage
            .get_book_source("default", "https://a.com")
            .await
            .unwrap()
            .is_none());

        cleanup(storage, "roundtrip").await;
    }

    /// update_book_source_enabled：单条切换 + 不存在返回 0 行
    #[tokio::test]
    async fn test_update_enabled() {
        let storage = test_storage("enabled").await;
        storage
            .save_book_source("default", &source("https://a.com", "A", None))
            .await
            .unwrap();
        storage
            .save_book_source("default", &source("https://b.com", "B", None))
            .await
            .unwrap();

        let affected = storage
            .update_book_source_enabled("default", "https://a.com", false)
            .await
            .unwrap();
        assert_eq!(affected, 1);
        let a = storage
            .get_book_source("default", "https://a.com")
            .await
            .unwrap()
            .unwrap();
        assert!(!a.enabled, "A 应被禁用");
        let b = storage
            .get_book_source("default", "https://b.com")
            .await
            .unwrap()
            .unwrap();
        assert!(b.enabled, "B 应保持启用");

        // 不存在的 URL → 0 行
        let none = storage
            .update_book_source_enabled("default", "https://nope.com", true)
            .await
            .unwrap();
        assert_eq!(none, 0);

        cleanup(storage, "enabled").await;
    }

    /// 批量事务保存 + 分组去重列表（含 default 回退）
    #[tokio::test]
    async fn test_batch_save_and_groups() {
        let storage = test_storage("batch").await;
        let sources = vec![
            source("https://a.com", "A", Some("小说 玄幻")),
            source("https://b.com", "B", Some("玄幻")),
            source("https://c.com", "C", None),
            source("https://d.com", "D", Some("")),
        ];
        storage
            .save_book_sources("default", &sources)
            .await
            .unwrap();

        let all = storage.get_book_sources("default").await.unwrap();
        assert_eq!(all.len(), 4);
        assert!(
            all.iter().all(|s| s.raw_json.is_some()),
            "批量保存应写入 raw_json"
        );

        // 保序去重；空串/None 分组不产生条目
        let groups = storage.list_book_source_groups("default").await.unwrap();
        assert_eq!(groups, vec!["小说", "玄幻"]);
        // 无书源命名空间回退 default 的分组
        let groups_fb = storage.list_book_source_groups("ghost").await.unwrap();
        assert_eq!(groups_fb, vec!["小说", "玄幻"]);

        cleanup(storage, "batch").await;
    }

    /// P0-3：按用户主键隔离——用户 B 保存同 URL 不覆盖用户 A（五类表逐项验证）
    #[tokio::test]
    async fn test_ns_isolation_same_url() {
        let storage = test_storage("nsiso").await;

        // 书源：A/B 同 URL 各自成行；同用户重复保存仍按 (url, ns) 覆盖更新
        let mut sa = source("https://a.com/src", "A的书源", None);
        storage.save_book_source("alice", &sa).await.unwrap();
        let mut sb = source("https://a.com/src", "B的书源", None);
        sb.enabled = false;
        storage.save_book_source("bob", &sb).await.unwrap();
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM book_sources WHERE book_source_url = ?1")
                .bind("https://a.com/src")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(n, 2, "同 URL 书源应按用户分行，而非覆盖");
        let (name, en, ns): (String, i64, String) = sqlx::query_as(
            "SELECT book_source_name, enabled, user_namespace FROM book_sources \
             WHERE book_source_url = ?1 AND user_namespace = ?2",
        )
        .bind("https://a.com/src")
        .bind("alice")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!((name.as_str(), en, ns.as_str()), ("A的书源", 1, "alice"));
        // 用户 A 再保存（改名）→ 仅覆盖自己那行
        sa.book_source_name = "A的书源v2".into();
        storage.save_book_source("alice", &sa).await.unwrap();
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM book_sources WHERE book_source_url = ?1")
                .bind("https://a.com/src")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(n, 2, "同用户覆盖更新不应新增行");
        let name: String = sqlx::query_scalar(
            "SELECT book_source_name FROM book_sources WHERE user_namespace = ?1 AND book_source_url = ?2",
        )
        .bind("alice")
        .bind("https://a.com/src")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(name, "A的书源v2");
        let (name_b, en_b): (String, i64) = sqlx::query_as(
            "SELECT book_source_name, enabled FROM book_sources WHERE user_namespace = ?1 AND book_source_url = ?2",
        )
        .bind("bob")
        .bind("https://a.com/src")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(
            (name_b.as_str(), en_b),
            ("B的书源", 0),
            "B 的行不受 A 更新影响"
        );

        // RSS 源
        storage
            .save_rss_source(
                "alice",
                &crate::model::RssSource {
                    source_url: "https://rss.example/x".into(),
                    source_name: "A的RSS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_rss_source(
                "bob",
                &crate::model::RssSource {
                    source_url: "https://rss.example/x".into(),
                    source_name: "B的RSS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM rss_sources WHERE rss_source_url = ?1")
                .bind("https://rss.example/x")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(n, 2);
        let (name, ns): (String, String) = sqlx::query_as(
            "SELECT rss_source_name, user_namespace FROM rss_sources WHERE user_namespace = ?1 AND rss_source_url = ?2",
        )
        .bind("alice")
        .bind("https://rss.example/x")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!((name.as_str(), ns.as_str()), ("A的RSS", "alice"));

        // HttpTTS
        storage
            .save_http_tts(
                "alice",
                &crate::model::HttpTts {
                    url: "https://tts.example/x".into(),
                    name: "A的TTS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_http_tts(
                "bob",
                &crate::model::HttpTts {
                    url: "https://tts.example/x".into(),
                    name: "B的TTS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM http_tts_list WHERE url = ?1")
            .bind("https://tts.example/x")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(n, 2);
        let name: String = sqlx::query_scalar(
            "SELECT name FROM http_tts_list WHERE user_namespace = ?1 AND url = ?2",
        )
        .bind("bob")
        .bind("https://tts.example/x")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(name, "B的TTS");

        // 书源订阅
        storage
            .save_source_sub("alice", "https://sub.example/x", "A的订阅", "[]", &[])
            .await
            .unwrap();
        storage
            .save_source_sub("bob", "https://sub.example/x", "B的订阅", "[]", &[])
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_subs WHERE url = ?1")
            .bind("https://sub.example/x")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(n, 2);
        let name: String = sqlx::query_scalar(
            "SELECT name FROM source_subs WHERE user_namespace = ?1 AND url = ?2",
        )
        .bind("alice")
        .bind("https://sub.example/x")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(name, "A的订阅");

        // RSS 文章：同 feed 同 URL 文章按用户分行；已读标记互不影响
        storage
            .save_rss_articles(
                "alice",
                &[crate::model::RssArticle {
                    url: "https://art.example/1".into(),
                    title: "A标题".into(),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
        storage
            .save_rss_articles(
                "bob",
                &[crate::model::RssArticle {
                    url: "https://art.example/1".into(),
                    title: "B标题".into(),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rss_articles WHERE url = ?1")
            .bind("https://art.example/1")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(n, 2, "同 URL 文章应按用户分行");
        let title: String = sqlx::query_scalar(
            "SELECT title FROM rss_articles WHERE user_namespace = ?1 AND url = ?2",
        )
        .bind("alice")
        .bind("https://art.example/1")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(title, "A标题", "B 的刷新不应覆盖 A 的文章行");
        // A 标记已读 → B 的行保持未读
        assert_eq!(
            storage
                .set_rss_article_read("alice", "https://art.example/1", true)
                .await
                .unwrap(),
            1
        );
        let (read_a, read_b): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT read FROM rss_articles WHERE user_namespace = 'alice' AND url = ?1), \
                    (SELECT read FROM rss_articles WHERE user_namespace = 'bob' AND url = ?1)",
        )
        .bind("https://art.example/1")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(read_a, 1);
        assert_eq!(read_b, 0, "A 的已读标记不应影响 B");

        cleanup(storage, "nsiso").await;
    }

    /// P0-3：旧库（url 单列主键）重建为复合主键——数据保留 + 幂等
    #[tokio::test]
    async fn test_ns_composite_key_rebuild() {
        let dir =
            std::env::temp_dir().join(format!("reader-storage-test-{}-olddb", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        std::fs::create_dir_all(config.storage_dir()).unwrap();
        let db = config.storage_dir().join("reader.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(8)
            .connect(&format!("sqlite://{}?mode=rwc", db.display()))
            .await
            .unwrap();
        // 旧库结构：rss_sources url 单列主键——同 URL 只能存一行
        //（旧实况：用户 B 保存同 URL 会覆盖 A 的行；这里模拟 A 的行被保留的场景）
        sqlx::query(
            "CREATE TABLE rss_sources (rss_source_url TEXT PRIMARY KEY, rss_source_name TEXT DEFAULT '', \
             rss_source_group TEXT, enabled INTEGER DEFAULT 1, user_namespace TEXT DEFAULT '', raw_json TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO rss_sources (rss_source_url, rss_source_name, enabled, user_namespace) \
             VALUES ('https://r/x', 'A的RSS', 1, 'alice')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // 重建 → 复合主键生效：数据/列保留
        migrate_ns_composite_keys(&pool).await.unwrap();
        let (name, en): (String, i64) = sqlx::query_as(
            "SELECT rss_source_name, enabled FROM rss_sources WHERE user_namespace = 'alice'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((name.as_str(), en), ("A的RSS", 1));
        // 表结构确为复合主键
        let sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'rss_sources'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            sql.replace('"', "")
                .contains("PRIMARY KEY (rss_source_url, user_namespace)"),
            "{sql}"
        );
        // 重建后用户 B 保存同 URL → 新行共存（旧库覆盖 bug 已消除）
        sqlx::query(
            "INSERT OR REPLACE INTO rss_sources (rss_source_url, rss_source_name, enabled, user_namespace) \
             VALUES ('https://r/x', 'B的RSS', 0, 'bob')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM rss_sources WHERE rss_source_url = 'https://r/x'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 2, "重建后同 URL 应按用户分行");
        let (name_b, ns_b): (String, String) = sqlx::query_as(
            "SELECT rss_source_name, user_namespace FROM rss_sources WHERE rss_source_url = 'https://r/x' AND user_namespace = 'bob'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((name_b.as_str(), ns_b.as_str()), ("B的RSS", "bob"));
        let (name_a, en_a): (String, i64) = sqlx::query_as(
            "SELECT rss_source_name, enabled FROM rss_sources WHERE rss_source_url = 'https://r/x' AND user_namespace = 'alice'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (name_a.as_str(), en_a),
            ("A的RSS", 1),
            "A 的行不受 B 保存影响"
        );
        // 幂等：再跑一次不报错、不丢数据、不重复
        migrate_ns_composite_keys(&pool).await.unwrap();
        let n2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rss_sources")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n2, 2, "二次迁移应跳过");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 书源使用统计：bump 原子自增 use_count/use_ts；命名空间/URL 隔离；
    /// 客户端重新保存（全字段 upsert）不清零统计；serde 输出不携带统计字段
    #[tokio::test]
    async fn test_book_source_use_stats() {
        let storage = test_storage("usestats").await;
        let s = source("https://stats.com", "统计源", None);
        storage.save_book_source("default", &s).await.unwrap();

        let got = storage
            .get_book_source("default", "https://stats.com")
            .await
            .unwrap()
            .expect("保存后应能查到");
        assert_eq!(got.use_count, 0, "初始计数为 0");
        assert_eq!(got.use_ts, 0, "初始时间戳为 0");

        // 两次自增：use_count=2 且 use_ts 刷新为当前毫秒时间戳
        storage
            .bump_book_source_use("default", "https://stats.com")
            .await
            .unwrap();
        storage
            .bump_book_source_use("default", "https://stats.com")
            .await
            .unwrap();
        let got = storage
            .get_book_source("default", "https://stats.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.use_count, 2);
        assert!(got.use_ts > 0);

        // 不存在的源 / 其他命名空间：静默忽略不报错
        storage
            .bump_book_source_use("default", "https://nope.com")
            .await
            .unwrap();
        storage
            .bump_book_source_use("ghost", "https://stats.com")
            .await
            .unwrap();
        let got = storage
            .get_book_source("default", "https://stats.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.use_count, 2, "无关 bump 不应影响计数");

        // 客户端重新保存（save_book_source 全字段覆盖）不清零统计
        let mut s2 = source("https://stats.com", "统计源改名", None);
        s2.custom_order = 5;
        storage.save_book_source("default", &s2).await.unwrap();
        let got = storage
            .get_book_source("default", "https://stats.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.use_count, 2, "客户端保存不应重置使用统计");
        assert_eq!(got.book_source_name, "统计源改名", "覆盖保存仍应生效");

        // 统计字段不外泄：serde 输出与 raw_json 均不含 useCount/useTs
        let json = serde_json::to_value(&got).unwrap();
        assert!(json.get("useCount").is_none(), "序列化不应含 useCount");
        assert!(json.get("useTs").is_none(), "序列化不应含 useTs");
        assert!(
            !got.raw_json.as_deref().unwrap_or("").contains("useCount"),
            "raw_json 不应含统计字段"
        );

        cleanup(storage, "usestats").await;
    }

    /// getBookSources 默认排序：weight DESC 优先，同权重回落 custom_order
    #[tokio::test]
    async fn test_book_sources_weight_order() {
        let storage = test_storage("weightorder").await;
        let mut a = source("https://w1.com", "A", None);
        a.weight = 10;
        a.custom_order = 0;
        let mut b = source("https://w2.com", "B", None);
        b.weight = 100;
        b.custom_order = 3;
        let mut c = source("https://w3.com", "C", None);
        c.weight = 10;
        c.custom_order = 1;
        storage
            .save_book_sources("default", &[a, b, c])
            .await
            .unwrap();

        let all = storage.get_book_sources("default").await.unwrap();
        let urls: Vec<&str> = all.iter().map(|s| s.book_source_url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://w2.com", "https://w1.com", "https://w3.com"],
            "weight DESC 优先，同权重回落 custom_order"
        );
        cleanup(storage, "weightorder").await;
    }

    /// 旧库兼容：books.local_epub/local_pdf 为 TEXT 类型时，init 应重建为 INTEGER 且数据无损读回
    #[tokio::test]
    async fn test_legacy_books_text_bool_rebuild() {
        let dir = std::env::temp_dir().join(format!(
            "reader-storage-test-{}-legacyrebuild",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();

        // 1. 模拟旧库：books 表 local_epub/local_pdf 为 TEXT，写入一行（含 TEXT '1'）
        let db_path = dir.join("storage").join("reader.db");
        std::fs::create_dir_all(dir.join("storage")).unwrap();
        {
            let opts = SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(opts)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE books (
                    book_url TEXT PRIMARY KEY, name TEXT DEFAULT '', author TEXT DEFAULT '',
                    origin TEXT DEFAULT '', origin_name TEXT DEFAULT '', toc_url TEXT DEFAULT '',
                    local_epub TEXT, local_pdf TEXT, pdf INTEGER DEFAULT 0,
                    user_namespace TEXT DEFAULT '')",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO books (book_url, name, local_epub, local_pdf, user_namespace)              VALUES ('https://old.com/a', '旧书', '1', '0', 'default')",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        }

        // 2. init → 检测 TEXT → 重建
        let storage = init(&config).await.expect("含旧 books 表的库应能初始化");
        let book = storage
            .find_book("default", "https://old.com/a")
            .await
            .unwrap()
            .expect("重建后旧数据应保留");
        assert_eq!(book.name, "旧书");
        assert!(book.local_epub, "TEXT '1' 应迁移为 true");
        assert!(!book.local_pdf, "TEXT '0' 应迁移为 false");
        // 重建后写入/读回 bool 正常
        storage
            .update_book_progress("default", "https://old.com/a", Some("第1章"), 0, 0, 1)
            .await
            .unwrap();
        let again = storage
            .find_book("default", "https://old.com/a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.dur_chapter_title.as_deref(), Some("第1章"));
        assert!(again.local_epub);

        // 3. 幂等：再次 init 不报错、不重复重建
        let storage2 = init(&config).await.unwrap();
        assert!(storage2
            .find_book("default", "https://old.com/a")
            .await
            .unwrap()
            .is_some());

        storage.pool.close().await;
        storage2.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 命名空间隔离：列表/删除/启停互不串；空命名空间回退 default
    #[tokio::test]
    async fn test_namespace_isolation() {
        let storage = test_storage("ns").await;
        storage
            .save_book_source("default", &source("https://a.com", "默认源", None))
            .await
            .unwrap();
        storage
            .save_book_source("alice", &source("https://b.com", "爱丽丝源", None))
            .await
            .unwrap();

        let alice = storage.get_book_sources("alice").await.unwrap();
        assert_eq!(
            alice.len(),
            2,
            "用户自有源 + 未覆盖的 default 系统源合并显示"
        );
        assert!(
            alice.iter().any(|s| s.book_source_name == "爱丽丝源")
                && alice.iter().any(|s| s.book_source_name == "默认源")
        );
        // 无书源命名空间回退 default
        let bob = storage.get_book_sources("bob").await.unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].book_source_name, "默认源");
        // 删除只影响本命名空间
        assert_eq!(
            storage
                .delete_book_source("alice", "https://b.com")
                .await
                .unwrap(),
            1
        );
        assert!(storage
            .get_book_source("alice", "https://b.com")
            .await
            .unwrap()
            .is_none());
        assert!(storage
            .get_book_source("default", "https://a.com")
            .await
            .unwrap()
            .is_some());
        // 启停：本命名空间记录优先；普通用户停用 default 系统书源 → 个人覆盖副本，
        // default 系统行保持启用（copy-on-write）
        assert_eq!(
            storage
                .update_book_source_enabled("alice", "https://a.com", false)
                .await
                .unwrap(),
            1
        );
        assert!(
            storage
                .get_book_source("default", "https://a.com")
                .await
                .unwrap()
                .unwrap()
                .enabled,
            "普通用户停用不应改动 default 系统书源"
        );
        let alice = storage.get_book_sources("alice").await.unwrap();
        assert_eq!(alice.len(), 1, "个人覆盖副本覆盖了同 URL 的 default 系统源");
        assert!(
            !alice
                .iter()
                .find(|s| s.book_source_url == "https://a.com")
                .unwrap()
                .enabled,
            "个人覆盖副本已停用"
        );
        // 再启回：个人副本启用（default 系统行保持启用）
        assert_eq!(
            storage
                .update_book_source_enabled("alice", "https://a.com", true)
                .await
                .unwrap(),
            1
        );
        let alice = storage.get_book_sources("alice").await.unwrap();
        assert_eq!(alice.len(), 1, "个人副本启用后仍覆盖 default 系统源");
        assert!(
            alice
                .iter()
                .find(|s| s.book_source_url == "https://a.com")
                .unwrap()
                .enabled
        );
        // 删除：普通用户删除 default 系统书源 → 个人 hidden 覆盖，default 系统行保留
        assert_eq!(
            storage
                .delete_book_source("alice", "https://a.com")
                .await
                .unwrap(),
            1
        );
        let alice_list = storage.get_book_sources("alice").await.unwrap();
        assert!(
            !alice_list
                .iter()
                .any(|s| s.book_source_url == "https://a.com"),
            "普通用户列表不再显示已删除的 default 源"
        );
        assert!(
            storage
                .get_book_source("default", "https://a.com")
                .await
                .unwrap()
                .is_some(),
            "default 系统书源不应被普通用户删除"
        );

        cleanup(storage, "ns").await;
    }

    /// 构造书架书（默认值 + 关键字段）
    fn shelf_book(url: &str, name: &str) -> Book {
        Book {
            book_url: url.into(),
            name: name.into(),
            author: "作者A".into(),
            origin: "https://src.com".into(),
            origin_name: "源A".into(),
            toc_url: format!("{url}/toc"),
            book_type: 1,
            can_update: true,
            is_in_shelf: true,
            ..Default::default()
        }
    }
    /// setBookSource 换源持久化：字段更新 + 进度保留 + 旧章节/目录缓存清理
    #[tokio::test]
    async fn test_switch_book_source() {
        let storage = test_storage("sbs").await;
        let old_url = "https://old.com/a";
        let new_url = "https://new.com/b";
        storage
            .upsert_book("default", &shelf_book(old_url, "书A"))
            .await
            .unwrap();
        storage
            .update_book_progress("default", old_url, Some("第1章"), 0, 123, 1111)
            .await
            .unwrap();
        // 旧 URL 章节缓存（换源后应清理）
        storage
            .cache_chapter_content("default", old_url, 0, "第1章", "旧内容")
            .await
            .unwrap();
        let n = storage
            .switch_book_source(
                "default",
                old_url,
                new_url,
                "https://newsrc.com",
                "新源",
                "https://new.com/b/toc",
                Some("https://cover/new.png"),
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(
            storage
                .find_book("default", old_url)
                .await
                .unwrap()
                .is_none(),
            "旧 URL 行不存在"
        );
        let got = storage
            .find_book("default", new_url)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.origin, "https://newsrc.com");
        assert_eq!(got.origin_name, "新源");
        assert_eq!(got.toc_url, "https://new.com/b/toc");
        // 进度保留（换源核心语义）
        assert_eq!(got.dur_chapter_index, 0);
        assert_eq!(got.dur_chapter_pos, 123);
        // 封面原为空 → 补上
        assert_eq!(got.cover_url.as_deref(), Some("https://cover/new.png"));
        // 旧章节缓存已清理
        assert!(
            storage
                .get_chapter_content("default", old_url, 0)
                .await
                .unwrap()
                .is_none(),
            "旧 URL 章节缓存应被清理"
        );
        cleanup(storage, "ns").await;
    }

    /// F-8/F-9：upsert 新增 → find → patch 增量 → upsert 覆盖 → 进度保存 全链路
    #[tokio::test]
    async fn test_book_save_progress_flow() {
        let storage = test_storage("book").await;
        let url = "https://book.com/a";
        assert!(
            storage.find_book("default", url).await.unwrap().is_none(),
            "初始不在书架"
        );

        // F-9 新增入架（全量 INSERT）
        let mut book = shelf_book(url, "书名");
        book.total_chapter_num = 100;
        storage.upsert_book("default", &book).await.unwrap();
        let got = storage
            .find_book("default", url)
            .await
            .unwrap()
            .expect("入架后应可查到");
        assert_eq!(got.name, "书名");
        assert_eq!(got.origin, "https://src.com");
        assert_eq!(got.total_chapter_num, 100);
        assert_eq!(got.user_namespace, "default");
        assert_eq!(got.toc_url, format!("{url}/toc"), "upsert 应持久化 toc_url");

        // F-9 编辑：增量 patch（name/coverUrl/group），未提供字段保持不变
        let patch: serde_json::Map<String, serde_json::Value> = serde_json::json!({
            "name": "书名v2",
            "coverUrl": "https://cover.com/x.jpg",
            "group": 3,
        })
        .as_object()
        .unwrap()
        .clone();
        let affected = storage.patch_book("default", url, &patch).await.unwrap();
        assert_eq!(affected, 1);
        let got2 = storage.find_book("default", url).await.unwrap().unwrap();
        assert_eq!(got2.name, "书名v2");
        assert_eq!(got2.cover_url.as_deref(), Some("https://cover.com/x.jpg"));
        assert_eq!(got2.group, 3);
        assert_eq!(got2.total_chapter_num, 100, "未提供的字段应保持原值");
        // 未知键忽略 + 空 patch → 0 行
        let junk: serde_json::Map<String, serde_json::Value> =
            serde_json::json!({ "unknownKey": 1 })
                .as_object()
                .unwrap()
                .clone();
        assert_eq!(storage.patch_book("default", url, &junk).await.unwrap(), 0);
        // 不存在的书 patch → 0 行
        assert_eq!(
            storage
                .patch_book("default", "https://nope.com", &patch)
                .await
                .unwrap(),
            0
        );

        // F-9 覆盖：upsert 全字段更新
        let mut book2 = shelf_book(url, "书名v3");
        book2.total_chapter_num = 200;
        storage.upsert_book("default", &book2).await.unwrap();
        let got3 = storage.find_book("default", url).await.unwrap().unwrap();
        assert_eq!(got3.name, "书名v3");
        assert_eq!(got3.total_chapter_num, 200);
        assert_eq!(
            got3.toc_url,
            format!("{url}/toc"),
            "全量覆盖后 toc_url 不应被清空"
        );

        // F-8 进度保存
        let affected = storage
            .update_book_progress("default", url, Some("第3章"), 2, 1234, 5678)
            .await
            .unwrap();
        assert_eq!(affected, 1);
        let got4 = storage.find_book("default", url).await.unwrap().unwrap();
        assert_eq!(got4.dur_chapter_title.as_deref(), Some("第3章"));
        assert_eq!(got4.dur_chapter_index, 2);
        assert_eq!(got4.dur_chapter_pos, 1234);
        assert_eq!(got4.dur_chapter_time, 5678);
        // title=None 保持原值
        storage
            .update_book_progress("default", url, None, 3, 0, 9999)
            .await
            .unwrap();
        let got5 = storage.find_book("default", url).await.unwrap().unwrap();
        assert_eq!(got5.dur_chapter_title.as_deref(), Some("第3章"));
        assert_eq!(got5.dur_chapter_index, 3);
        // 书架外的书 → 0 行
        assert_eq!(
            storage
                .update_book_progress("default", "https://nope.com", Some("x"), 0, 0, 0)
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "book").await;
    }

    /// 本地书入库：books.type 使用传入 book_type（回归：旧代码写死 1=音频，
    /// 导致本地 EPUB 全部按音频分支读取失败）
    #[tokio::test]
    async fn test_save_local_book_type_preserved() {
        let storage = test_storage("locbooktype").await;
        let imported = crate::service::local_book::ImportedBook {
            meta: Default::default(),
            chapters: vec![crate::service::local_book::Chapter {
                title: "第一章".into(),
                content: "正文".into(),
            }],
            cover: None,
            format: "epub".into(),
        };
        let mut info = crate::model::book_chapter::BookInfo {
            book_url: "local://sample".into(),
            name: "本地书".into(),
            origin: "local".into(),
            ..Default::default()
        };
        info.book_type = 0;
        storage
            .save_local_book("default", &info, &imported)
            .await
            .unwrap();
        let book = storage
            .find_book("default", "local://sample")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(book.book_type, 0, "文本本地书 type 应为 0");
        assert_eq!(book.total_chapter_num, 1, "本地书总章数应写入");
        assert_eq!(
            storage
                .count_chapters("default", "local://sample")
                .await
                .unwrap(),
            1,
            "章节应入库"
        );

        // 漫画本地书保留 2
        let mut comic = info.clone();
        comic.book_url = "local://comic".into();
        comic.book_type = 2;
        storage
            .save_local_book("default", &comic, &imported)
            .await
            .unwrap();
        let book = storage
            .find_book("default", "local://comic")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(book.book_type, 2, "CBZ 漫画 type 应为 2");
        assert_eq!(book.total_chapter_num, 1, "漫画本地书总章数应写入");

        cleanup(storage, "locbooktype").await;
    }

    /// F-10：目录缓存写入 → 命中 → 过期未命中
    #[tokio::test]
    async fn test_toc_cache_roundtrip() {
        let storage = test_storage("toccache").await;
        let toc_url = "https://book.com/toc";
        assert!(
            storage
                .get_toc_cache("default", toc_url, 300_000)
                .await
                .unwrap()
                .is_none(),
            "未缓存时应未命中"
        );

        storage
            .cache_toc(
                "default",
                toc_url,
                toc_url,
                r#"[{"title":"第一章","url":"https://book.com/1"}]"#,
            )
            .await
            .unwrap();
        let cached = storage
            .get_toc_cache("default", toc_url, 300_000)
            .await
            .unwrap()
            .expect("缓存后应命中");
        assert!(cached.contains("第一章"));
        // 同 book_url 覆盖写
        storage
            .cache_toc("default", toc_url, toc_url, r#"[{"title":"新目录"}]"#)
            .await
            .unwrap();
        let cached2 = storage
            .get_toc_cache("default", toc_url, 300_000)
            .await
            .unwrap()
            .unwrap();
        assert!(cached2.contains("新目录"));
        // 过期（把 updated_at 置 0）→ 未命中
        sqlx::query("UPDATE toc_cache SET updated_at = 0 WHERE book_url = ?1")
            .bind(toc_url)
            .execute(&storage.pool)
            .await
            .unwrap();
        assert!(
            storage
                .get_toc_cache("default", toc_url, 300_000)
                .await
                .unwrap()
                .is_none(),
            "TTL 过期应未命中"
        );

        cleanup(storage, "toccache").await;
    }

    /// 书签：保存 → 列表 → 覆盖保存（同 title）→ 删除
    #[tokio::test]
    async fn test_bookmark_roundtrip() {
        let storage = test_storage("bookmark").await;
        let url = "https://book.com/a";
        let bm = crate::model::Bookmark {
            book_url: url.into(),
            title: "标记1".into(),
            book_name: "三体".into(),
            book_author: "刘慈欣".into(),
            paragraph_index: 42,
            chapter_index: 3,
            chapter_name: "第一章".into(),
            book_text: "这是书签内容".into(),
            content: "备注A".into(),
            created_at: 1000,
            ..Default::default()
        };
        storage.save_bookmark("default", &bm).await.unwrap();
        storage
            .save_bookmark(
                "default",
                &crate::model::Bookmark {
                    book_url: url.into(),
                    title: "标记2".into(),
                    paragraph_index: 7,
                    chapter_index: 1,
                    created_at: 2000,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let list = storage.list_bookmarks("default", url).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "标记2", "按创建时间倒序");
        assert_eq!(list[1].paragraph_index, 42);
        assert_eq!(list[1].book_name, "三体");
        assert_eq!(list[1].book_author, "刘慈欣");
        assert_eq!(list[1].chapter_name, "第一章");
        assert_eq!(list[1].book_text, "这是书签内容");
        assert_eq!(list[1].content, "备注A", "legacy Bookmark.content 应持久化");
        // 他书/他命名空间隔离
        assert!(storage
            .list_bookmarks("default", "https://other.com")
            .await
            .unwrap()
            .is_empty());
        assert!(storage
            .list_bookmarks("alice", url)
            .await
            .unwrap()
            .is_empty());

        // 同 title 覆盖保存
        storage
            .save_bookmark(
                "default",
                &crate::model::Bookmark {
                    book_url: url.into(),
                    title: "标记1".into(),
                    paragraph_index: 99,
                    chapter_index: 3,
                    created_at: 3000,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let list2 = storage.list_bookmarks("default", url).await.unwrap();
        assert_eq!(list2.len(), 2);
        assert_eq!(list2[0].paragraph_index, 99);

        // 删除
        assert_eq!(
            storage
                .delete_bookmark("default", url, "标记1")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage.list_bookmarks("default", url).await.unwrap().len(),
            1
        );
        assert_eq!(
            storage
                .delete_bookmark("default", url, "不存在")
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "bookmark").await;
    }

    /// 分组：新建（自增 id）→ 列表 → 按 id 覆盖 → 书设分组
    #[tokio::test]
    async fn test_book_group_flow() {
        let storage = test_storage("bookgroup").await;
        let g1 = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    name: "玄幻".into(),
                    cover: Some("https://covers/x.png".into()),
                    show: false,
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(g1.id > 0, "新建应返回自增 id");
        let g2 = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    name: "言情".into(),
                    order: 2,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(g2.id > g1.id);

        let list = storage.list_book_groups("default").await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "玄幻", "按 order 排序");
        assert_eq!(
            list[0].cover.as_deref(),
            Some("https://covers/x.png"),
            "分组封面应持久化"
        );
        assert!(!list[0].show, "分组显隐开关应持久化");
        assert!(list[1].show, "缺省 show 应为 true");
        // 命名空间隔离
        assert!(storage.list_book_groups("alice").await.unwrap().is_empty());

        // 按 id 覆盖（改名 + 排序）
        let updated = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    id: g1.id,
                    name: "玄幻v2".into(),
                    order: 5,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.id, g1.id);
        let list2 = storage.list_book_groups("default").await.unwrap();
        assert_eq!(list2.len(), 2);
        assert_eq!(list2[1].name, "玄幻v2");

        // 书设分组（books.group_name）
        let url = "https://book.com/a";
        storage
            .upsert_book("default", &shelf_book(url, "书名"))
            .await
            .unwrap();
        assert_eq!(
            storage
                .update_book_group_id("default", url, g1.id)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .find_book("default", url)
                .await
                .unwrap()
                .unwrap()
                .group,
            g1.id
        );
        assert_eq!(
            storage
                .update_book_group_id("default", "https://nope.com", g1.id)
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "bookgroup").await;
    }

    /// RSS 源：保存（含 raw_json 原文）→ 查询 → 覆盖保存 → 删除；命名空间回退 default
    #[tokio::test]
    async fn test_rss_source_roundtrip() {
        let storage = test_storage("rsssrc").await;
        let s = crate::model::RssSource {
            source_url: "https://feed.example.com/rss".into(),
            source_name: "示例源".into(),
            source_group: Some("科技".into()),
            enabled: true,
            raw_json: Some(
                r#"{"sourceUrl":"https://feed.example.com/rss","sourceName":"示例源","sourceGroup":"科技","enabled":true,"sortUrl":null,"ruleContent":"css.article"}"#
                    .into(),
            ),
            ..Default::default()
        };
        storage.save_rss_source("default", &s).await.unwrap();

        let got = storage
            .find_rss_source("default", "https://feed.example.com/rss")
            .await
            .unwrap()
            .expect("保存后应能查到");
        assert_eq!(got.source_name, "示例源");
        assert_eq!(got.source_group.as_deref(), Some("科技"));
        assert!(got.enabled);
        assert_eq!(got.user_namespace, "default");
        let raw: serde_json::Value =
            serde_json::from_str(got.raw_json.as_deref().expect("raw_json 应已写入")).unwrap();
        assert_eq!(raw["sourceUrl"], "https://feed.example.com/rss");
        assert_eq!(raw["ruleContent"], "css.article", "raw_json 应保留完整字段");

        // 覆盖保存（改名 + 禁用）→ INSERT OR REPLACE 生效
        let mut s2 = s.clone();
        s2.source_name = "示例源v2".into();
        s2.enabled = false;
        storage.save_rss_source("default", &s2).await.unwrap();
        let got2 = storage
            .find_rss_source("default", "https://feed.example.com/rss")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got2.source_name, "示例源v2");
        assert!(!got2.enabled);

        // 列表（default 直接返回；其他命名空间回退 default）
        let list = storage.get_rss_sources("default").await.unwrap();
        assert_eq!(list.len(), 1);
        let fb = storage.get_rss_sources("ghost").await.unwrap();
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].source_name, "示例源v2", "无源命名空间回退 default");

        // 删除 → 查不到
        assert_eq!(
            storage
                .delete_rss_source("default", "https://feed.example.com/rss")
                .await
                .unwrap(),
            1
        );
        assert!(storage
            .find_rss_source("default", "https://feed.example.com/rss")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            storage
                .delete_rss_source("default", "https://feed.example.com/rss")
                .await
                .unwrap(),
            0,
            "重复删除影响 0 行"
        );

        cleanup(storage, "rsssrc").await;
    }

    /// RSS 文章：批量保存（按 url 去重）→ 按 url 查询；命名空间隔离
    #[tokio::test]
    async fn test_rss_articles_roundtrip() {
        let storage = test_storage("rssart").await;
        let article = |url: &str, title: &str, time: i64| crate::model::RssArticle {
            url: url.into(),
            source_url: "https://feed.example.com/rss".into(),
            title: title.into(),
            author: "作者".into(),
            time,
            content: Some("正文".into()),
            cover: Some("https://img.example.com/1.jpg".into()),
            ..Default::default()
        };
        let articles = vec![
            article("https://feed.example.com/a", "甲", 1000),
            article("https://feed.example.com/b", "乙", 2000),
        ];
        storage
            .save_rss_articles("default", &articles)
            .await
            .unwrap();

        let got = storage
            .get_rss_article("default", "https://feed.example.com/a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.title, "甲");
        assert_eq!(got.source_url, "https://feed.example.com/rss");
        assert_eq!(got.time, 1000);
        assert_eq!(got.content.as_deref(), Some("正文"));
        assert_eq!(got.cover.as_deref(), Some("https://img.example.com/1.jpg"));
        assert_eq!(got.user_namespace, "default");

        // 同 url 覆盖（刷新 feed 时去重更新）
        storage
            .save_rss_articles(
                "default",
                &[article("https://feed.example.com/a", "甲v2", 3000)],
            )
            .await
            .unwrap();
        let again = storage
            .get_rss_article("default", "https://feed.example.com/a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.title, "甲v2");
        assert_eq!(again.time, 3000);
        assert_eq!(
            storage
                .get_rss_article("default", "https://feed.example.com/b")
                .await
                .unwrap()
                .unwrap()
                .title,
            "乙"
        );

        // 已读标记：标记已读 → 重新入库（feed 刷新）不清除
        storage
            .set_rss_article_read("default", "https://feed.example.com/a", true)
            .await
            .unwrap();
        storage
            .save_rss_articles(
                "default",
                &[article("https://feed.example.com/a", "甲v3", 4000)],
            )
            .await
            .unwrap();
        let marked = storage
            .get_rss_article("default", "https://feed.example.com/a")
            .await
            .unwrap()
            .unwrap();
        assert!(marked.read, "重新入库不应清除已读标记");
        assert_eq!(marked.title, "甲v3");
        // 标回未读
        storage
            .set_rss_article_read("default", "https://feed.example.com/a", false)
            .await
            .unwrap();
        assert!(
            !storage
                .get_rss_article("default", "https://feed.example.com/a")
                .await
                .unwrap()
                .unwrap()
                .read
        );
        // 已读标记批量查询（仅本命名空间 + 本源的 url）
        let flags = storage
            .get_rss_article_read_flags("default", "https://feed.example.com/rss")
            .await
            .unwrap();
        assert_eq!(flags.len(), 2);
        assert!(!flags["https://feed.example.com/a"]);
        assert!(!flags["https://feed.example.com/b"]);
        let other = storage
            .get_rss_article_read_flags("other", "https://feed.example.com/rss")
            .await
            .unwrap();
        assert!(other.is_empty(), "其他命名空间看不到该源的已读标记");

        // P0-4 跨命名空间拒绝：get 查不到（按 (ns, url) 查）、set 影响 0 行
        assert!(
            storage
                .get_rss_article("other", "https://feed.example.com/a")
                .await
                .unwrap()
                .is_none(),
            "其他命名空间不应读到本命名空间文章"
        );
        assert_eq!(
            storage
                .set_rss_article_read("other", "https://feed.example.com/a", true)
                .await
                .unwrap(),
            0,
            "其他命名空间标记已读应影响 0 行"
        );
        let untouched = storage
            .get_rss_article("default", "https://feed.example.com/a")
            .await
            .unwrap()
            .unwrap();
        assert!(!untouched.read, "跨命名空间标记不得改动他人已读状态");

        // 不存在的 url
        assert!(storage
            .get_rss_article("default", "https://feed.example.com/nope")
            .await
            .unwrap()
            .is_none());

        cleanup(storage, "rssart").await;
    }

    /// F-7：书源计数（不含 default 回退）+ 用户书源上限读取
    #[tokio::test]
    async fn test_book_source_limit_helpers() {
        let storage = test_storage("bslimit").await;
        storage
            .insert_user(&User {
                username: "alice".into(),
                book_source_limit: 5,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            storage.book_source_limit_for("alice").await.unwrap(),
            Some(5)
        );
        assert_eq!(storage.book_source_limit_for("ghost").await.unwrap(), None);
        for i in 0..3 {
            storage
                .save_book_source("alice", &source(&format!("https://s{i}.com"), "S", None))
                .await
                .unwrap();
        }
        assert_eq!(storage.count_book_sources("alice").await.unwrap(), 3);
        assert_eq!(
            storage.count_book_sources("default").await.unwrap(),
            0,
            "计数不含 default 回退"
        );
        cleanup(storage, "bslimit").await;
    }

    /// F-25：logout 清空 token，重复 logout 影响 0 行
    #[tokio::test]
    async fn test_logout_user() {
        let storage = test_storage("logout").await;
        storage
            .insert_user(&User {
                username: "alice".into(),
                token: "t1".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(storage.logout_user("alice").await.unwrap(), 1);
        assert!(storage
            .find_user("alice")
            .await
            .unwrap()
            .unwrap()
            .token
            .is_empty());
        assert_eq!(storage.logout_user("ghost").await.unwrap(), 0);
        cleanup(storage, "logout").await;
    }

    /// GAP 59：登录追加 token（token_map 上限 5、去重）；登出仅移除当前设备 token
    #[tokio::test]
    async fn test_add_remove_user_token() {
        let storage = test_storage("tokenmap").await;
        storage
            .insert_user(&User {
                username: "alice".into(),
                token: "t0".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // 追加 6 个 token → 上限 5，最旧被丢
        for i in 1..=6 {
            storage
                .add_user_token("alice", &format!("tok-{i}"), 1000 + i)
                .await
                .unwrap();
        }
        let user = storage.find_user("alice").await.unwrap().unwrap();
        assert_eq!(user.token, "tok-6", "主 token 为最新");
        let list = crate::model::user::token_map_list(&user.token_map);
        assert_eq!(
            list,
            vec!["tok-2", "tok-3", "tok-4", "tok-5", "tok-6"],
            "上限 5 且最旧被丢"
        );
        // 重新登录同一 token → 去重（不重复计数）
        storage
            .add_user_token("alice", "tok-5", 9999)
            .await
            .unwrap();
        let user = storage.find_user("alice").await.unwrap().unwrap();
        let list = crate::model::user::token_map_list(&user.token_map);
        assert_eq!(list.iter().filter(|t| *t == "tok-5").count(), 1, "去重");
        assert_eq!(list.len(), 5);
        // 登出：移除设备 token（主 token 保持）
        storage.remove_user_token("alice", "tok-4").await.unwrap();
        let user = storage.find_user("alice").await.unwrap().unwrap();
        assert_eq!(user.token, "tok-5", "主 token 不受影响");
        let list = crate::model::user::token_map_list(&user.token_map);
        assert!(!list.contains(&"tok-4".to_string()));
        assert_eq!(list.len(), 4);
        // 登出主 token → 主 token 清空但 map 其余保留
        storage.remove_user_token("alice", "tok-5").await.unwrap();
        let user = storage.find_user("alice").await.unwrap().unwrap();
        assert!(user.token.is_empty(), "主 token 清空");
        let list = crate::model::user::token_map_list(&user.token_map);
        assert_eq!(list.len(), 3, "其余设备 token 保留");
        // 不存在的 token → 无影响
        assert_eq!(storage.remove_user_token("alice", "nope").await.unwrap(), 0);
        // legacy 对象形态 token_map 兼容（键即 token）——直接 UPDATE 注入旧形态数据
        storage
            .insert_user(&User {
                username: "legacy".into(),
                token: "main".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        sqlx::query("UPDATE users SET token_map = ?1 WHERE username = 'legacy'")
            .bind(serde_json::json!({"old-1": 1700000000000i64}).to_string())
            .execute(&storage.pool)
            .await
            .unwrap();
        storage
            .add_user_token("legacy", "new-1", 2000)
            .await
            .unwrap();
        let user = storage.find_user("legacy").await.unwrap().unwrap();
        let list = crate::model::user::token_map_list(&user.token_map);
        assert!(list.contains(&"old-1".to_string()) && list.contains(&"new-1".to_string()));
        cleanup(storage, "tokenmap").await;
    }

    /// F-34：不活跃用户清理（GAP #95：users 行 + 用户级数据行 + 数据目录；except 保护）
    #[tokio::test]
    async fn test_clear_inactive_users() {
        let storage = test_storage("inactive").await;
        let mk = |name: &str, last: i64| User {
            username: name.into(),
            last_login_at: last,
            ..Default::default()
        };
        storage.insert_user(&mk("old", 1000)).await.unwrap();
        storage.insert_user(&mk("mid", 5000)).await.unwrap();
        storage.insert_user(&mk("new", 9999)).await.unwrap();
        // old 用户残留数据（书 + 数据目录文件）——应一并清理
        storage
            .upsert_book(
                "old",
                &crate::model::Book {
                    book_url: "https://old.com/b".into(),
                    name: "旧书".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let dir = storage
            .config
            .storage_dir()
            .join("data")
            .join("old")
            .join("webdav");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.txt"), "x").unwrap();

        let deleted = storage.clear_inactive_users(6000, None).await.unwrap();
        assert_eq!(deleted, vec!["old", "mid"]);
        assert!(storage.find_user("old").await.unwrap().is_none());
        assert!(storage.find_user("mid").await.unwrap().is_none());
        assert!(storage.find_user("new").await.unwrap().is_some());
        // 用户级数据行 + 目录已清理
        assert!(storage.list_books("old").await.unwrap().is_empty());
        assert!(!storage
            .config
            .storage_dir()
            .join("data")
            .join("old")
            .exists());

        // except 用户受保护
        let deleted = storage
            .clear_inactive_users(99999, Some("new"))
            .await
            .unwrap();
        assert!(deleted.is_empty());
        assert!(storage.find_user("new").await.unwrap().is_some());
        cleanup(storage, "inactive").await;
    }

    /// GAP #95：deleteUser 全量清理——用户级表行 + 数据目录；共享书章节保留
    #[tokio::test]
    async fn test_delete_user_cleans_all_data() {
        let storage = test_storage("deluser2").await;
        // alice（被删）与 bob（保留）
        storage
            .insert_user(&User {
                username: "alice".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage
            .insert_user(&User {
                username: "bob".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // 共享书（双方都有）+ alice 独有书
        let shared = crate::model::Book {
            book_url: "https://s.com/shared".into(),
            name: "共享".into(),
            ..Default::default()
        };
        let alice_only = crate::model::Book {
            book_url: "https://a.com/only".into(),
            name: "独有".into(),
            ..Default::default()
        };
        storage.upsert_book("alice", &shared).await.unwrap();
        storage.upsert_book("bob", &shared).await.unwrap();
        storage.upsert_book("alice", &alice_only).await.unwrap();
        // 章节（共享书章节两用户共用；独有书章节仅 alice）
        for (url, idx) in [("https://s.com/shared", 0), ("https://a.com/only", 0)] {
            sqlx::query("INSERT INTO book_chapters (book_url, chapter_index, title, content) VALUES (?1, ?2, '章', '文')")
                .bind(url)
                .bind(idx)
                .execute(&storage.pool)
                .await
                .unwrap();
        }
        // 用户级数据铺满各表
        storage
            .save_book_source(
                "alice",
                &crate::model::BookSource {
                    book_source_url: "https://a.com/src".into(),
                    book_source_name: "源".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .set_cookie("alice", "https://a.com/src", "sid=1")
            .await
            .unwrap();
        storage
            .save_user_config("alice", "reader", "{}")
            .await
            .unwrap();
        storage
            .record_reading_stats("alice", "https://a.com/only", "2025-01-01", 60, 1000)
            .await
            .unwrap();
        storage
            .save_bookmark(
                "alice",
                &crate::model::Bookmark {
                    book_url: "https://a.com/only".into(),
                    title: "书签".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_source_sub("alice", "https://a.com/sub", "订阅", "[]", &[])
            .await
            .unwrap();
        storage
            .save_rss_source(
                "alice",
                &crate::model::RssSource {
                    source_url: "https://a.com/rss".into(),
                    source_name: "RSS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_rss_articles(
                "alice",
                &[crate::model::RssArticle {
                    url: "https://a.com/rss/1".into(),
                    title: "文".into(),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
        // 数据目录（含 webdav 子目录）
        let alice_dir = storage.config.storage_dir().join("data").join("alice");
        std::fs::create_dir_all(alice_dir.join("webdav").join("legado")).unwrap();
        std::fs::write(
            alice_dir.join("webdav").join("legado").join("backup-1.zip"),
            "zip",
        )
        .unwrap();
        std::fs::create_dir_all(alice_dir.join("opds_files")).unwrap();
        std::fs::write(alice_dir.join("opds_files").join("f.txt"), "f").unwrap();

        // 删除
        assert_eq!(storage.delete_user("alice").await.unwrap(), 1);
        // users 行
        assert!(storage.find_user("alice").await.unwrap().is_none());
        // 用户级表行清空
        assert!(storage.list_books("alice").await.unwrap().is_empty());
        assert!(storage.get_book_sources("alice").await.unwrap().is_empty());
        assert!(storage
            .get_cookie("alice", "https://a.com/src")
            .await
            .unwrap()
            .is_none());
        let cfg: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_config WHERE user_namespace = 'alice'")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(cfg, 0);
        let stats: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM reading_stats WHERE user_namespace = 'alice'")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(stats, 0);
        let bm: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM bookmarks WHERE user_namespace = 'alice'")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(bm, 0);
        let subs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM source_subs WHERE user_namespace = 'alice'")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(subs, 0);
        let rss_src: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM rss_sources WHERE user_namespace = 'alice'")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(rss_src, 0);
        let rss_art: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM rss_articles WHERE user_namespace = 'alice'")
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(rss_art, 0);
        // 章节：独有书章节删除；共享书章节保留（bob 仍拥有）
        let shared_ch: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM book_chapters WHERE book_url = 'https://s.com/shared'",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(shared_ch, 1, "共享书章节应保留");
        let only_ch: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM book_chapters WHERE book_url = 'https://a.com/only'",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(only_ch, 0, "独有书章节应删除");
        // bob 数据不受影响
        assert!(storage
            .find_book("bob", "https://s.com/shared")
            .await
            .unwrap()
            .is_some());
        // 目录递归删除（含 webdav/opds_files）
        assert!(!alice_dir.exists(), "数据目录应被递归删除");

        // 不存在用户 → 0；非法用户名 → 不 panic、不删目录
        assert_eq!(storage.delete_user("ghost").await.unwrap(), 0);
        assert_eq!(storage.delete_user("../evil").await.unwrap(), 0);
        cleanup(storage, "deluser2").await;
    }

    /// GAP #57：自动备份（auto-YYYYMMDD.zip 写入 + 同日幂等 + 保留最近 7 份）
    #[tokio::test]
    async fn test_auto_backup_and_prune() {
        let storage = test_storage("autobk").await;
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: "https://a.com/b".into(),
                    name: "书".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let legado = storage
            .config
            .storage_dir()
            .join("data")
            .join("default")
            .join("webdav")
            .join("legado");

        // 首次：生成 auto-YYYYMMDD.zip
        assert_eq!(
            crate::service::schedule::run_auto_backup(&storage)
                .await
                .unwrap(),
            1
        );
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let auto_file = legado.join(format!("auto-{today}.zip"));
        assert!(
            auto_file.exists(),
            "自动备份文件应生成: {}",
            auto_file.display()
        );

        // 同日再跑：幂等跳过（不重复生成）
        let files_after_first: Vec<_> = std::fs::read_dir(&legado)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            crate::service::schedule::run_auto_backup(&storage)
                .await
                .unwrap(),
            0
        );
        let files_after_second: Vec<_> = std::fs::read_dir(&legado)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            files_after_first.len(),
            files_after_second.len(),
            "同日不应重复备份"
        );

        // 保留最近 7 份：伪造 12 个历史 auto-*.zip → prune 后剩 7（今天的保留）
        for i in 1..=12 {
            std::fs::write(legado.join(format!("auto-202501{i:02}.zip")), "old").unwrap();
        }
        let removed = storage.prune_auto_backups("default", 7);
        assert_eq!(removed, 6);
        let remaining: Vec<String> = std::fs::read_dir(&legado)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.starts_with("auto-") && n.ends_with(".zip"))
            .collect();
        assert_eq!(remaining.len(), 7);
        assert!(
            remaining.iter().any(|n| *n == format!("auto-{today}.zip")),
            "今天的备份应保留"
        );
        assert!(
            remaining.iter().all(|n| n.as_str() >= "auto-20250107.zip"),
            "只保留最新的 7 份: {remaining:?}"
        );

        // 手动备份不受影响（backup-*.zip 不参与 auto 清理）
        storage.create_backup_zip("default").await.unwrap();
        let removed = storage.prune_auto_backups("default", 7);
        assert_eq!(removed, 0);
        cleanup(storage, "autobk").await;
    }

    /// F-32：用户管理——列表/权限更新/删除/重置密码
    #[tokio::test]
    async fn test_user_management() {
        let storage = test_storage("usermgmt").await;
        storage
            .insert_user(&User {
                username: "alice".into(),
                password: "p1".into(),
                salt: "s1".into(),
                token: "tok".into(),
                enable_webdav: false,
                enable_book_source: true,
                book_source_limit: 10,
                book_limit: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        storage
            .insert_user(&User {
                username: "bob".into(),
                password: "p2".into(),
                salt: "s2".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        // 列表：含全部用户与启用状态
        let users = storage.list_users().await.unwrap();
        assert_eq!(users.len(), 2);
        let alice = users.iter().find(|u| u.username == "alice").unwrap();
        assert!(!alice.enable_webdav && alice.enable_book_source);
        assert_eq!(alice.book_source_limit, 10);

        // 部分字段更新（None 不覆盖）
        let n = storage
            .update_user_permissions(
                "alice",
                Some(true),
                None,
                Some(false),
                None,
                Some(99),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
        let alice = storage.find_user("alice").await.unwrap().unwrap();
        assert!(alice.enable_webdav, "enable_webdav 应更新为 true");
        assert!(
            !alice.enable_book_source,
            "enable_book_source 应更新为 false"
        );
        assert_eq!(alice.book_source_limit, 99);
        assert_eq!(alice.book_limit, 20, "未提供的字段应保持原值");
        assert!(!alice.enable_local_store);
        // 不存在的用户 → 0 行
        assert_eq!(
            storage
                .update_user_permissions(
                    "ghost",
                    Some(true),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap(),
            0
        );

        // 删除
        assert_eq!(storage.delete_user("bob").await.unwrap(), 1);
        assert!(storage.find_user("bob").await.unwrap().is_none());
        assert_eq!(storage.delete_user("ghost").await.unwrap(), 0);

        // 重置密码：新密码可校验、token 清空
        let salt = "newsalt";
        let encrypted = crate::util::md5::gen_encrypted_password("新密码123", salt);
        assert_eq!(
            storage
                .reset_user_password("alice", salt, &encrypted)
                .await
                .unwrap(),
            1
        );
        let alice = storage.find_user("alice").await.unwrap().unwrap();
        assert_eq!(alice.password, encrypted);
        assert_eq!(alice.salt, salt);
        assert!(alice.token.is_empty(), "重置密码后旧 token 应失效");
        assert_eq!(
            storage
                .reset_user_password("ghost", salt, &encrypted)
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "usermgmt").await;
    }

    /// F-35：可更新书扫描（仅 can_update=1）+ 更新信息回写（含 None 标题不覆盖 latest_chapter_time）
    #[tokio::test]
    async fn test_updatable_books_and_update_info() {
        let storage = test_storage("shelfupd").await;
        let mut b1 = shelf_book("https://book.com/a", "A");
        b1.can_update = true;
        let mut b2 = shelf_book("https://book.com/b", "B");
        b2.can_update = false;
        storage.upsert_book("default", &b1).await.unwrap();
        storage.upsert_book("default", &b2).await.unwrap();

        let updatable = storage.list_updatable_books().await.unwrap();
        assert_eq!(updatable.len(), 1);
        assert_eq!(updatable[0].book_url, "https://book.com/a");

        let affected = storage
            .update_book_update_info("default", "https://book.com/a", Some("第99章"), 99, 123456)
            .await
            .unwrap();
        assert_eq!(affected, 1);
        let book = storage
            .find_book("default", "https://book.com/a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(book.latest_chapter_title.as_deref(), Some("第99章"));
        assert_eq!(book.total_chapter_num, 99);
        assert_eq!(book.latest_chapter_time, 123456);
        assert_eq!(book.last_check_time, 123456);
        assert_eq!(book.last_check_count, 1);

        // 无最新章节（None）→ 标题/时间保持原值，仅检查计数 +1
        storage
            .update_book_update_info("default", "https://book.com/a", None, 99, 888888)
            .await
            .unwrap();
        let book = storage
            .find_book("default", "https://book.com/a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(book.latest_chapter_title.as_deref(), Some("第99章"));
        assert_eq!(book.latest_chapter_time, 123456);
        assert_eq!(book.last_check_time, 888888);
        assert_eq!(book.last_check_count, 2);
        // 不存在的书 → 0 行
        assert_eq!(
            storage
                .update_book_update_info("default", "https://nope.com", Some("x"), 1, 1)
                .await
                .unwrap(),
            0
        );
        cleanup(storage, "shelfupd").await;
    }

    /// F-39：备份 zip 打包（legacy backupFileNames 全集 + 登录态；路径在 webdav/legado 下）
    #[tokio::test]
    async fn test_backup_zip() {
        let storage = test_storage("backup").await;
        storage
            .upsert_book("default", &shelf_book("https://book.com/a", "备份书"))
            .await
            .unwrap();
        storage
            .save_book_source("default", &source("https://s.com", "源A", None))
            .await
            .unwrap();

        let path = storage.create_backup_zip("default").await.unwrap();
        let zip_path = std::path::PathBuf::from(&path);
        assert!(zip_path.exists(), "zip 文件应已生成: {path}");
        let name = zip_path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("backup-") && name.ends_with(".zip"),
            "文件名应为 backup-*.zip: {name}"
        );
        assert!(
            zip_path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n == "legado")
                .unwrap_or(false),
            "zip 应在 webdav/legado 下: {path}"
        );

        // 再次立即备份：同秒也不得覆盖上一份（增量安全——旧备份保留）
        let path2 = storage.create_backup_zip("default").await.unwrap();
        assert_ne!(path, path2, "两次备份路径应不同（同秒追加序号）");
        assert!(zip_path.exists(), "旧备份不应被新备份覆盖/删除");
        assert!(std::path::PathBuf::from(&path2).exists());

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        for expect in [
            "bookshelf.json",
            "bookSource.json",
            "bookmark.json",
            "bookGroup.json",
            "rssSources.json",
            "replaceRule.json",
            "txtTocRule.json",
            "userConfig.json",
            "httpTTS.json",
            "bookSourceCookies.json",
        ] {
            assert!(
                names.iter().any(|n| n == expect),
                "zip 应含 {expect}: {names:?}"
            );
        }
        let mut entry = archive.by_name("bookshelf.json").unwrap();
        let mut content = String::new();
        use std::io::Read;
        entry.read_to_string(&mut content).unwrap();
        assert!(content.contains("备份书"), "bookshelf.json 应含书架书");
        drop(entry);
        let mut entry = archive.by_name("bookSource.json").unwrap();
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        assert!(content.contains("源A"), "bookSource.json 应含书源");

        cleanup(storage, "backup").await;
    }

    /// 备份→清空→恢复 往返一致性：全部类目数据经 zip 快照后逐表比对一致
    #[tokio::test]
    async fn test_backup_restore_roundtrip() {
        use crate::model::{BookGroup, Bookmark, HttpTts, ReplaceRule, RssSource, TxtTocRule};
        let storage = test_storage("bkroundtrip").await;

        // 种子数据（覆盖备份 zip 全部条目类目）
        let mut book = shelf_book("https://book.com/r1", "往返书");
        book.author = "作者甲".into();
        book.dur_chapter_index = 7;
        book.dur_chapter_pos = 42;
        book.dur_chapter_title = Some("第七章".into());
        storage.upsert_book("default", &book).await.unwrap();
        storage
            .save_book_group(
                "default",
                &BookGroup {
                    id: 9,
                    name: "分组九".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_bookmark(
                "default",
                &Bookmark {
                    book_url: "https://book.com/r1".into(),
                    title: "书签甲".into(),
                    chapter_index: 3,
                    content: "好句".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_book_source(
                "default",
                &source("https://s-rt.com", "往返源", Some("小说")),
            )
            .await
            .unwrap();
        storage
            .save_rss_source(
                "default",
                &RssSource {
                    source_url: "https://rss-rt.com/feed".into(),
                    source_name: "往返RSS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_replace_rule(
                "default",
                &ReplaceRule {
                    id: "rt1".into(),
                    name: "往返规则".into(),
                    find: "旧".into(),
                    replace: "新".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_txt_toc_rule(
                "default",
                &TxtTocRule {
                    id: "rtt1".into(),
                    name: "往返TXT规则".into(),
                    rule: r"^第[一二三四五六七八九十]+章".into(),
                    serial_number: 2,
                    enable: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_http_tts(
                "default",
                &HttpTts {
                    url: "https://tts-rt.com/api".into(),
                    name: "往返TTS".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_user_config("default", "theme", "dark")
            .await
            .unwrap();
        storage
            .save_user_config("default", "fontSize", "18")
            .await
            .unwrap();
        storage
            .set_cookie("default", "https://s-rt.com", "SID=abc123")
            .await
            .unwrap();

        // 快照原值（清空前）
        let exp_books = storage.list_books("default").await.unwrap();
        let exp_groups = storage.list_book_groups("default").await.unwrap();
        let exp_sources = storage.get_book_sources("default").await.unwrap();
        let exp_rss = storage.get_rss_sources("default").await.unwrap();
        let exp_rules = storage.get_replace_rules("default").await.unwrap();
        let exp_txt = storage.get_txt_toc_rules("default").await.unwrap();
        let exp_tts = storage.get_http_tts_list("default").await.unwrap();
        let exp_cookies = storage.list_cookies("default").await.unwrap();
        let exp_cfg_theme = storage.get_user_config("default", "theme").await.unwrap();
        let exp_cfg_font = storage
            .get_user_config("default", "fontSize")
            .await
            .unwrap();
        let exp_bookmarks = storage
            .list_bookmarks("default", "https://book.com/r1")
            .await
            .unwrap();

        let zip_path = storage.create_backup_zip("default").await.unwrap();
        let zip_bytes = std::fs::read(&zip_path).unwrap();

        // 清空命名空间全部用户数据（模拟换机/丢库）
        for table in [
            "books",
            "book_groups",
            "bookmarks",
            "book_sources",
            "rss_sources",
            "replace_rules",
            "txt_toc_rules",
            "http_tts_list",
            "user_config",
            "book_source_cookies",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE user_namespace = ?1"))
                .bind("default")
                .execute(&storage.pool)
                .await
                .unwrap();
        }
        assert!(storage.list_books("default").await.unwrap().is_empty());
        assert!(storage
            .get_book_sources("default")
            .await
            .unwrap()
            .is_empty());

        // 恢复 → 全部 restored
        let report = storage
            .restore_backup_zip("default", &zip_bytes, true)
            .await
            .unwrap();
        assert_eq!(report.restored.books, 1);
        assert_eq!(report.restored.groups, 1);
        assert_eq!(report.restored.bookmarks, 1);
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.restored.rss, 1);
        assert_eq!(report.restored.rules, 1);
        assert_eq!(report.restored.txt_rules, 1);
        assert_eq!(report.restored.tts, 1);
        assert_eq!(report.restored.config, 2);
        assert_eq!(report.restored.cookies, 1);
        assert_eq!(report.skipped.books + report.skipped.sources, 0);

        // 逐表与快照比对一致（模型未派生 PartialEq → 以 JSON 序列化结果比对，
        // 即备份 zip 条目的实际载荷语义；books 剔除 list_books 附加的易变 rowid）
        fn json_of<T: serde::Serialize>(v: &T) -> serde_json::Value {
            serde_json::to_value(v).unwrap()
        }
        fn strip_rowid(books: &[Book]) -> serde_json::Value {
            let mut v = serde_json::to_value(books).unwrap();
            if let serde_json::Value::Array(arr) = &mut v {
                for item in arr {
                    item.as_object_mut().map(|o| o.remove("rowid"));
                }
            }
            v
        }
        assert_eq!(
            strip_rowid(&storage.list_books("default").await.unwrap()),
            strip_rowid(&exp_books)
        );
        assert_eq!(
            json_of(&storage.list_book_groups("default").await.unwrap()),
            json_of(&exp_groups)
        );
        assert_eq!(
            json_of(&storage.get_book_sources("default").await.unwrap()),
            json_of(&exp_sources)
        );
        assert_eq!(
            json_of(&storage.get_rss_sources("default").await.unwrap()),
            json_of(&exp_rss)
        );
        assert_eq!(
            json_of(&storage.get_replace_rules("default").await.unwrap()),
            json_of(&exp_rules)
        );
        assert_eq!(
            json_of(&storage.get_txt_toc_rules("default").await.unwrap()),
            json_of(&exp_txt)
        );
        assert_eq!(
            json_of(&storage.get_http_tts_list("default").await.unwrap()),
            json_of(&exp_tts)
        );
        assert_eq!(
            json_of(&storage.list_cookies("default").await.unwrap()),
            json_of(&exp_cookies)
        );
        assert_eq!(
            storage.get_user_config("default", "theme").await.unwrap(),
            exp_cfg_theme
        );
        assert_eq!(
            storage
                .get_user_config("default", "fontSize")
                .await
                .unwrap(),
            exp_cfg_font
        );
        assert_eq!(
            json_of(
                &storage
                    .list_bookmarks("default", "https://book.com/r1")
                    .await
                    .unwrap()
            ),
            json_of(&exp_bookmarks)
        );
        // 阅读进度细节（dur 三字段）不丢
        let got_book = storage
            .find_book("default", "https://book.com/r1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got_book.dur_chapter_index, 7);
        assert_eq!(got_book.dur_chapter_pos, 42);
        assert_eq!(got_book.dur_chapter_title.as_deref(), Some("第七章"));

        cleanup(storage, "bkroundtrip").await;
    }

    /// 测试用最小备份 zip 构造（条目名 → 内容）
    fn make_backup_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buf);
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::FileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        drop(writer);
        buf.into_inner()
    }

    /// F-55：备份 zip 恢复（最小备份 zip → 各表断言；幂等 skip / overwrite）
    #[tokio::test]
    async fn test_restore_backup_zip() {
        let storage = test_storage("restore").await;
        let zip = make_backup_zip(&[
            (
                "bookSource.json",
                r#"[{"bookSourceUrl":"https://s1.com","bookSourceName":"源1","bookSourceGroup":"小说"}]"#,
            ),
            (
                "bookshelf.json",
                r#"[{"bookUrl":"https://b1.com","name":"书1","author":"甲","origin":"https://s1.com","group":3}]"#,
            ),
            (
                "bookGroup.json",
                r#"[{"id":3,"name":"我的分组","order":1}]"#,
            ),
            (
                "replaceRule.json",
                r#"[{"id":"r1","name":"规则1","find":"甲","replace":"乙","enabled":true,"order":0}]"#,
            ),
            (
                "txtTocRule.json",
                r#"[{"id":"t1","name":"TXT规则","rule":"^第.*章","serialNumber":1}]"#,
            ),
            (
                "rssSources.json",
                r#"[{"sourceUrl":"https://rss.com/feed","sourceName":"RSS源","header":"UA:1"}]"#,
            ),
            ("userConfig.json", r#"{"theme":"dark","fontSize":"18"}"#),
            (
                "httpTTS.json",
                r#"[{"url":"https://tts.com/api","name":"TTS源","type":0}]"#,
            ),
            (
                "bookmark.json",
                r#"[{"bookUrl":"https://b1.com","title":"书签1","chapterIndex":2,"paragraphIndex":10}]"#,
            ),
        ]);

        // 首次恢复：全部 restored
        let report = storage
            .restore_backup_zip("default", &zip, false)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.restored.books, 1);
        assert_eq!(report.restored.groups, 1);
        assert_eq!(report.restored.rules, 1);
        assert_eq!(report.restored.txt_rules, 1);
        assert_eq!(report.restored.rss, 1);
        assert_eq!(report.restored.config, 2);
        assert_eq!(report.restored.tts, 1);
        assert_eq!(report.restored.bookmarks, 1);
        assert_eq!(report.skipped.sources, 0);

        // 各表断言
        let sources = storage.get_book_sources("default").await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].book_source_name, "源1");
        assert_eq!(sources[0].book_source_group.as_deref(), Some("小说"));
        assert_eq!(sources[0].user_namespace, "default");
        let books = storage.list_books("default").await.unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].name, "书1");
        assert_eq!(books[0].group, 3, "分组 id 应原样恢复");
        assert_eq!(books[0].user_namespace, "default");
        let groups = storage.list_book_groups("default").await.unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, 3);
        assert_eq!(groups[0].name, "我的分组");
        let rules = storage.get_replace_rules("default").await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].find, "甲");
        let txt_rules = storage.get_txt_toc_rules("default").await.unwrap();
        assert_eq!(txt_rules.len(), 1);
        assert_eq!(txt_rules[0].rule, "^第.*章");
        let rss = storage.get_rss_sources("default").await.unwrap();
        assert_eq!(rss.len(), 1);
        assert_eq!(rss[0].source_name, "RSS源");
        assert_eq!(
            rss[0].raw_json.as_deref().map(|r| r.contains("\"header\"")),
            Some(true),
            "RSS raw_json 应保留未知字段"
        );
        assert_eq!(
            storage
                .get_user_config("default", "theme")
                .await
                .unwrap()
                .as_deref(),
            Some("dark")
        );
        assert_eq!(
            storage
                .get_user_config("default", "fontSize")
                .await
                .unwrap()
                .as_deref(),
            Some("18")
        );
        let tts = storage.get_http_tts_list("default").await.unwrap();
        assert_eq!(tts.len(), 1);
        assert_eq!(tts[0].url, "https://tts.com/api");
        let bookmarks = storage
            .list_bookmarks("default", "https://b1.com")
            .await
            .unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].chapter_index, 2);

        // 幂等：overwrite=false 再恢复 → 全部 skipped
        let report = storage
            .restore_backup_zip("default", &zip, false)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 0);
        assert_eq!(report.skipped.sources, 1);
        assert_eq!(report.skipped.books, 1);
        assert_eq!(report.skipped.groups, 1);
        assert_eq!(report.skipped.rules, 1);
        assert_eq!(report.skipped.txt_rules, 1);
        assert_eq!(report.skipped.rss, 1);
        assert_eq!(report.skipped.config, 2);
        assert_eq!(report.skipped.tts, 1);
        assert_eq!(report.skipped.bookmarks, 1);
        assert_eq!(report.restored.config, 0);

        // overwrite=true → 全部覆盖 restored
        let report = storage
            .restore_backup_zip("default", &zip, true)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.restored.books, 1);
        assert_eq!(report.restored.groups, 1);
        assert_eq!(report.restored.rules, 1);
        assert_eq!(report.restored.txt_rules, 1);
        assert_eq!(report.restored.rss, 1);
        assert_eq!(report.restored.config, 2);
        assert_eq!(report.restored.tts, 1);
        assert_eq!(report.restored.bookmarks, 1);
        assert_eq!(report.skipped.sources, 0);
        assert_eq!(
            storage.get_book_sources("default").await.unwrap().len(),
            1,
            "覆盖不应产生重复行"
        );

        // 命名空间隔离：恢复进 alice 命名空间互不影响 default
        // （book_sources 表主键为 book_source_url（全局），跨用户恢复需用不同 URL）
        storage
            .insert_user(&User {
                username: "alice".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let zip_alice = make_backup_zip(&[
            (
                "bookSource.json",
                r#"[{"bookSourceUrl":"https://s5.com","bookSourceName":"源5"}]"#,
            ),
            (
                "bookshelf.json",
                r#"[{"bookUrl":"https://b5.com","name":"书5"}]"#,
            ),
        ]);
        let report = storage
            .restore_backup_zip("alice", &zip_alice, false)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.restored.books, 1);
        assert_eq!(
            storage.get_book_sources("alice").await.unwrap().len(),
            2,
            "恢复的用户源 + 未覆盖的 default 系统源合并显示"
        );
        assert_eq!(
            storage.get_book_sources("default").await.unwrap().len(),
            1,
            "default 不受影响"
        );
        assert_eq!(
            storage.list_books("default").await.unwrap().len(),
            1,
            "default 书架不受影响"
        );

        cleanup(storage, "restore").await;
    }

    /// F-55：legacy 布局兼容——config/ 目录 + books.json 命名；非法 zip / 无识别条目报错
    #[tokio::test]
    async fn test_restore_backup_zip_legacy_layout() {
        let storage = test_storage("restorelegacy").await;
        let zip = make_backup_zip(&[
            (
                "config/bookSource.json",
                r#"[{"bookSourceUrl":"https://s2.com","bookSourceName":"源2"}]"#,
            ),
            (
                "config/books.json",
                r#"[{"bookUrl":"https://b2.com","name":"书2"}]"#,
            ),
            (
                "config/bookGroup.json",
                r#"[{"id":7,"name":"legacy分组","order":0}]"#,
            ),
        ]);
        let report = storage
            .restore_backup_zip("default", &zip, false)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.restored.books, 1);
        assert_eq!(report.restored.groups, 1);
        let books = storage.list_books("default").await.unwrap();
        assert_eq!(books[0].name, "书2");
        let groups = storage.list_book_groups("default").await.unwrap();
        assert_eq!(groups[0].id, 7);

        // 条目同时存在于根与 config/ 时优先根（当前布局覆盖 legacy）
        let zip2 = make_backup_zip(&[
            (
                "bookSource.json",
                r#"[{"bookSourceUrl":"https://s3.com","bookSourceName":"根源"}]"#,
            ),
            (
                "config/bookSource.json",
                r#"[{"bookSourceUrl":"https://s4.com","bookSourceName":"config源"}]"#,
            ),
        ]);
        let report = storage
            .restore_backup_zip("default", &zip2, true)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 1);
        assert_eq!(
            storage.get_book_sources("default").await.unwrap()[0].book_source_url,
            "https://s3.com"
        );

        // 无效条目（空 url/空名）计入 skipped
        let zip3 = make_backup_zip(&[
            (
                "bookSource.json",
                r#"[{"bookSourceName":"没URL"},{"bookSourceUrl":"https://ok.com","bookSourceName":"OK"}]"#,
            ),
            ("bookshelf.json", r#"[{"name":"没URL"}]"#),
            ("httpTTS.json", r#"[{"name":"没URL"}]"#),
        ]);
        let report = storage
            .restore_backup_zip("default", &zip3, false)
            .await
            .unwrap();
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.skipped.sources, 1);
        assert_eq!(report.skipped.books, 1);
        assert_eq!(report.skipped.tts, 1);

        // 非 zip 字节 → Err
        assert!(storage
            .restore_backup_zip("default", b"not a zip file", false)
            .await
            .is_err());
        // 合法 zip 但无识别条目 → Err
        let empty = make_backup_zip(&[("readme.txt", "hi")]);
        assert!(storage
            .restore_backup_zip("default", &empty, false)
            .await
            .is_err());

        cleanup(storage, "restorelegacy").await;
    }

    /// F-35：定时任务主循环（本地书/无书源书跳过；网络书源缺失时静默跳过不报错）
    #[tokio::test]
    async fn test_run_shelf_update_skips() {
        let storage = test_storage("shelfrun").await;
        // 本地书（跳过）
        storage
            .upsert_book("default", &shelf_book("local://abc", "本地书"))
            .await
            .unwrap();
        // 无 tocUrl（跳过）
        let mut b = shelf_book("https://book.com/notoc", "无目录");
        b.toc_url = String::new();
        storage.upsert_book("default", &b).await.unwrap();
        // 无书源（跳过）
        storage
            .upsert_book("default", &shelf_book("https://book.com/nosrc", "无源"))
            .await
            .unwrap();

        // 不应报错、不应更新任何书
        assert_eq!(run_shelf_update(&storage).await.unwrap(), 0);
        let book = storage
            .find_book("default", "https://book.com/nosrc")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(book.last_check_count, 0);
        cleanup(storage, "shelfrun").await;
    }

    /// 夜间书架更新的目录质量门：残缺目录（比已知总数和已缓存章数都少）不得回写章数
    #[test]
    fn test_plausible_chapter_total() {
        // 残缺：解析 60 章 < 书架 395 / 已缓存 400（绍宋真实数据）→ 拒绝
        assert!(!plausible_chapter_total(60, 395, 400));
        // 正常增长：解析 400 ≥ 书架 395 → 接受
        assert!(plausible_chapter_total(400, 395, 400));
        // 持平续更：解析 400 > 395，即使缓存只有 100 章也接受
        assert!(plausible_chapter_total(396, 396, 100));
        // 卷末总结使缓存多于目录：解析 395 == 书架 395，缓存 400 → 接受
        assert!(plausible_chapter_total(395, 395, 400));
        // prev=0（新书首抓，无已知总数）→ 信任解析结果，交给缓存/阅读器下界纠正
        assert!(plausible_chapter_total(300, 0, 0));
        assert!(plausible_chapter_total(300, 0, 400));
        // 只有 prev 一道锚也要拦
        assert!(!plausible_chapter_total(50, 400, 0));
        // 空解析永远不可信
        assert!(!plausible_chapter_total(0, 0, 0));
        assert!(!plausible_chapter_total(0, 400, 400));
        // 负值（异常数据）同样不可信
        assert!(!plausible_chapter_total(-1, 0, 0));
    }

    /// 章数自愈：真目录抓到多少就回写多少（双向），兜底/缓存命中不得把章数写小。
    #[test]
    fn test_chapter_total_writeback() {
        // ① 真目录 1747 vs 记录 1745（《玄鉴仙族》线上实况）→ 写 1747
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 1747, 1745, 12),
            Some(1747)
        );
        // ② 兜底目录（离线合成）400 章 vs 记录 1747 → 不写（无论缓存多少章）
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 400, 1747, 400),
            None
        );
        // ③ 真目录比记录少且不是残缺（完本删章：1747 → 1740，缓存 300）→ 照样写
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 1740, 1747, 300),
            Some(1740)
        );
        // ④ 章数 0 → 不写（任何来源）
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 0, 1747, 0),
            None
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 0, 1747, 0),
            None
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 0, 0, 0),
            None
        );
        // ⑤ 真目录比记录少且明显少于已缓存章数 → 判为残缺，不写（质量门下界）
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 50, 1747, 400),
            None
        );
        // ⑥ 真目录与记录一致 → 不产生无意义写
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 1747, 1747, 1747),
            None
        );
        // ⑦ 目录缓存命中只允许"只增不减"：1800 > 1747 写，1745 < 1747 不写
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 1800, 1747, 0),
            Some(1800)
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 1745, 1747, 0),
            None
        );
        // ⑧ 新书 prev=0：真目录直接写；缓存/兜底来源走"只增"，也能把这个 0 往上修
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 300, 0, 0),
            Some(300)
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 300, 0, 0),
            Some(300)
        );
    }

    /// F-28：替换规则 CRUD 往返 + 命名空间隔离 + default 回退
    #[tokio::test]
    async fn test_replace_rules_roundtrip() {
        let storage = test_storage("replrule").await;
        use crate::model::ReplaceRule;
        let rule = |id: &str, name: &str, order: i64| ReplaceRule {
            id: id.into(),
            name: name.into(),
            group: Some("通用".into()),
            find: format!("找{name}"),
            replace: format!("替{name}"),
            scope: Some("content".into()),
            scope_title: false,
            scope_content: true,
            is_regex: true,
            timeout_millisecond: 5000,
            enabled: true,
            order,
            ..Default::default()
        };

        // 保存两条（order 逆序）→ 按 order_num 排序返回
        storage
            .save_replace_rule("default", &rule("r1", "一", 2))
            .await
            .unwrap();
        storage
            .save_replace_rule("default", &rule("r2", "二", 1))
            .await
            .unwrap();
        let list = storage.get_replace_rules("default").await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "r2", "应按 order_num 排序");
        assert_eq!(list[1].id, "r1");
        assert_eq!(list[0].find, "找二");
        assert_eq!(list[0].group.as_deref(), Some("通用"));
        assert_eq!(list[0].scope.as_deref(), Some("content"));
        assert!(list[0].is_regex);
        assert_eq!(list[0].timeout_millisecond, 5000);
        assert_eq!(list[0].user_namespace, "default");

        // 覆盖保存（同 id）
        let mut r = rule("r1", "一v2", 2);
        r.enabled = false;
        storage.save_replace_rule("default", &r).await.unwrap();
        let list = storage.get_replace_rules("default").await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(!list[1].enabled);
        assert_eq!(list[1].name, "一v2");

        // 批量保存（事务）
        storage
            .save_replace_rules("default", &[rule("r3", "三", 3), rule("r4", "四", 4)])
            .await
            .unwrap();
        assert_eq!(storage.get_replace_rules("default").await.unwrap().len(), 4);

        // 删除
        assert_eq!(
            storage.delete_replace_rule("default", "r3").await.unwrap(),
            1
        );
        assert_eq!(
            storage
                .delete_replace_rule("default", "ghost")
                .await
                .unwrap(),
            0
        );
        assert_eq!(storage.get_replace_rules("default").await.unwrap().len(), 3);

        // 命名空间隔离：alice 无规则时回退 default
        assert_eq!(storage.get_replace_rules("alice").await.unwrap().len(), 3);
        storage
            .save_replace_rule("alice", &rule("a1", "爱丽丝", 0))
            .await
            .unwrap();
        let alice = storage.get_replace_rules("alice").await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].name, "爱丽丝");
        // 删除只影响本命名空间
        assert_eq!(storage.delete_replace_rule("alice", "r1").await.unwrap(), 0);
        assert_eq!(storage.get_replace_rules("default").await.unwrap().len(), 3);

        cleanup(storage, "replrule").await;
    }

    /// F-26：HttpTTS CRUD 往返 + 命名空间隔离 + default 回退
    #[tokio::test]
    async fn test_http_tts_roundtrip() {
        let storage = test_storage("httptts").await;
        use crate::model::HttpTts;
        let tts = |url: &str, name: &str, ty: i64| HttpTts {
            url: url.into(),
            name: name.into(),
            tts_type: ty,
            content_type: Some("audio/mpeg".into()),
            concurrent_rate: Some("0".into()),
            login_url: Some("https://login.example.com".into()),
            login_ui: Some(r#"[{"type":"input"}]"#.into()),
            header: Some(r#"{"X-Token":"a"}"#.into()),
            js_lib: Some("lib.js".into()),
            enabled_cookie_jar: Some(true),
            login_check_js: Some("java.ajax('x')".into()),
            last_update_time: 1700000000000,
            ..Default::default()
        };

        storage
            .save_http_tts("default", &tts("https://tts.example.com/a", "引擎甲", 0))
            .await
            .unwrap();
        storage
            .save_http_tts("default", &tts("https://tts.example.com/b", "引擎乙", 1))
            .await
            .unwrap();
        let list = storage.get_http_tts_list("default").await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "引擎乙", "应按名称排序");
        assert_eq!(list[0].tts_type, 1);
        assert_eq!(list[1].url, "https://tts.example.com/a");
        assert_eq!(list[1].content_type.as_deref(), Some("audio/mpeg"));
        assert_eq!(list[1].concurrent_rate.as_deref(), Some("0"));
        assert_eq!(
            list[1].login_url.as_deref(),
            Some("https://login.example.com")
        );
        assert_eq!(list[1].login_ui.as_deref(), Some(r#"[{"type":"input"}]"#));
        assert_eq!(list[1].header.as_deref(), Some(r#"{"X-Token":"a"}"#));
        assert_eq!(list[1].js_lib.as_deref(), Some("lib.js"));
        assert_eq!(list[1].enabled_cookie_jar, Some(true));
        assert_eq!(list[1].login_check_js.as_deref(), Some("java.ajax('x')"));
        assert_eq!(list[1].last_update_time, 1700000000000);

        // 同 url 覆盖
        storage
            .save_http_tts("default", &tts("https://tts.example.com/a", "引擎甲v2", 0))
            .await
            .unwrap();
        let list = storage.get_http_tts_list("default").await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|t| t.name == "引擎甲v2"));

        // 删除
        assert_eq!(
            storage
                .delete_http_tts("default", "https://tts.example.com/a")
                .await
                .unwrap(),
            1
        );
        assert_eq!(storage.get_http_tts_list("default").await.unwrap().len(), 1);

        // 命名空间隔离 + default 回退
        assert_eq!(
            storage.get_http_tts_list("alice").await.unwrap().len(),
            1,
            "空命名空间回退 default"
        );
        storage
            .save_http_tts("alice", &tts("https://tts.example.com/x", "爱丽丝引擎", 0))
            .await
            .unwrap();
        assert_eq!(storage.get_http_tts_list("alice").await.unwrap().len(), 1);
        assert_eq!(
            storage
                .delete_http_tts("alice", "https://tts.example.com/b")
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "httptts").await;
    }

    /// 自定义 TXT 目录规则：保存/排序/删除/导入默认规则 + 命名空间隔离
    #[tokio::test]
    async fn test_txt_toc_rules_flow() {
        let storage = test_storage("txttoc").await;
        use crate::model::TxtTocRule;
        let rule = |id: &str, name: &str, re: &str, sn: i64| TxtTocRule {
            id: id.into(),
            name: name.into(),
            rule: re.into(),
            enable: true,
            serial_number: sn,
            ..Default::default()
        };

        // 初始无用户规则
        assert!(storage
            .get_txt_toc_rules("default")
            .await
            .unwrap()
            .is_empty());

        // 保存（乱序 serialNumber → 按序返回）
        storage
            .save_txt_toc_rule("default", &rule("t1", "自定义A", r"^第.+章$", 5))
            .await
            .unwrap();
        storage
            .save_txt_toc_rule("default", &rule("t2", "自定义B", r"^楔子$", 1))
            .await
            .unwrap();
        let list = storage.get_txt_toc_rules("default").await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "t2", "应按 serial_number 排序");
        assert_eq!(list[1].name, "自定义A");
        assert_eq!(list[1].user_namespace, "default");

        // 覆盖 + 禁用
        let mut r = rule("t1", "自定义Av2", r"^第.+章$", 5);
        r.enable = false;
        storage.save_txt_toc_rule("default", &r).await.unwrap();
        let list = storage.get_txt_toc_rules("default").await.unwrap();
        assert!(!list[1].enable);

        // 删除
        assert_eq!(
            storage.delete_txt_toc_rule("default", "t2").await.unwrap(),
            1
        );
        assert_eq!(storage.get_txt_toc_rules("default").await.unwrap().len(), 1);

        // 导入默认规则（幂等）
        let count = storage
            .import_default_txt_toc_rules("default")
            .await
            .unwrap();
        assert_eq!(
            count,
            crate::service::local_book::DEFAULT_TOC_RULE_DEFS.len()
        );
        let list = storage.get_txt_toc_rules("default").await.unwrap();
        let default_ids = list.iter().filter(|r| r.id.starts_with("default-")).count();
        assert_eq!(
            default_ids,
            crate::service::local_book::DEFAULT_TOC_RULE_DEFS.len()
        );
        assert_eq!(
            storage
                .import_default_txt_toc_rules("default")
                .await
                .unwrap(),
            count,
            "重复导入不新增"
        );
        assert_eq!(
            storage.get_txt_toc_rules("default").await.unwrap().len(),
            list.len()
        );

        // 命名空间隔离：alice 无规则（不查 default）
        assert!(storage.get_txt_toc_rules("alice").await.unwrap().is_empty());

        cleanup(storage, "txttoc").await;
    }

    /// getSystemInfo 统计：用户数/书数/书源数（全命名空间）
    #[tokio::test]
    async fn test_system_info_counts() {
        let storage = test_storage("sysinfo").await;
        storage
            .insert_user(&User {
                username: "alice".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage
            .upsert_book("default", &shelf_book("https://book.com/a", "A"))
            .await
            .unwrap();
        storage
            .upsert_book("alice", &shelf_book("https://book.com/b", "B"))
            .await
            .unwrap();
        storage
            .save_book_source("default", &source("https://s.com", "源A", None))
            .await
            .unwrap();
        storage
            .save_book_source("alice", &source("https://s2.com", "源B", None))
            .await
            .unwrap();

        assert_eq!(storage.count_users().await.unwrap(), 1);
        assert_eq!(storage.count_books().await.unwrap(), 2);
        assert_eq!(storage.count_all_book_sources().await.unwrap(), 2);
        cleanup(storage, "sysinfo").await;
    }

    /// 在线会话计数：主 token + token_map 多设备（服务监控）
    #[tokio::test]
    async fn test_count_active_tokens() {
        let storage = test_storage("activetok").await;
        assert_eq!(
            storage.count_active_tokens().await.unwrap(),
            0,
            "无用户 → 0"
        );

        // 无 token 用户
        storage
            .insert_user(&User {
                username: "guest".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // 单设备：仅主 token
        storage
            .insert_user(&User {
                username: "single".into(),
                token: "t-main".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // 多设备：add_user_token 推入 token_map（主 token = 最近一个）
        storage
            .insert_user(&User {
                username: "multi".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage
            .add_user_token("multi", "t-dev1", 1000)
            .await
            .unwrap();
        storage
            .add_user_token("multi", "t-dev2", 2000)
            .await
            .unwrap();

        assert_eq!(
            storage.count_active_tokens().await.unwrap(),
            3,
            "单设备 1 + 多设备 2"
        );

        // 登出（清主 token）后多设备仍在线
        storage.logout_user("single").await.unwrap();
        assert_eq!(storage.count_active_tokens().await.unwrap(), 2);
        cleanup(storage, "activetok").await;
    }

    /// 缓存管理：getCacheInfo 统计（toc_cache 行数 / book_chapters 行数 / sum length 大小）+
    /// clearCache 按 type 清空（toc / chapters / all）
    #[tokio::test]
    async fn test_cache_info_and_clear() {
        let storage = test_storage("cache").await;

        // 空库：全零
        let info = storage.get_cache_info("default").await.unwrap();
        assert_eq!(info.toc_cache_count, 0);
        assert_eq!(info.chapter_count, 0);
        assert_eq!(info.chapter_size, 0);
        assert_eq!(info.total_size, 0);

        // 写入目录缓存 2 条 + 章节 3 条
        storage
            .cache_toc(
                "default",
                "https://book.com/a",
                "https://book.com/toc",
                "[{\"title\":\"第一章\"}]",
            )
            .await
            .unwrap();
        storage
            .cache_toc(
                "default",
                "https://book.com/b",
                "https://book.com/toc2",
                "[{\"title\":\"第二章\"}]",
            )
            .await
            .unwrap();
        storage
            .save_chapters(
                "local://book1",
                &[
                    ("第一章".to_string(), "正文一甲乙丙丁".to_string()),
                    ("第二章".to_string(), "正文二戊己庚辛壬癸".to_string()),
                    ("第三章".to_string(), "正文三子丑寅卯".to_string()),
                ],
            )
            .await
            .unwrap();

        let info = storage.get_cache_info("default").await.unwrap();
        assert_eq!(info.toc_cache_count, 2);
        assert_eq!(
            info.toc_cache_size, 34,
            "SQLite length() 按字符计，两条各 17 字符"
        );
        assert_eq!(info.chapter_count, 3);
        assert_eq!(info.chapter_size, 23, "7+9+7 字符");
        assert_eq!(info.total_size, info.toc_cache_size + info.chapter_size);

        // 只清 toc
        let (toc_del, chap_del) = storage.clear_cache("default", "toc").await.unwrap();
        assert_eq!(toc_del, 2);
        assert_eq!(chap_del, 0);
        let info = storage.get_cache_info("default").await.unwrap();
        assert_eq!(info.toc_cache_count, 0);
        assert_eq!(info.chapter_count, 3, "章节缓存不受影响");

        // 只清 chapters
        let (toc_del, chap_del) = storage.clear_cache("default", "chapters").await.unwrap();
        assert_eq!(toc_del, 0);
        assert_eq!(chap_del, 3);
        let info = storage.get_cache_info("default").await.unwrap();
        assert_eq!(info.chapter_count, 0);
        assert_eq!(info.total_size, 0);

        // all：全清（再写入后验证）
        storage
            .cache_toc(
                "default",
                "https://book.com/a",
                "https://book.com/toc",
                "[]",
            )
            .await
            .unwrap();
        storage
            .save_chapters(
                "local://book1",
                &[("第四章".to_string(), "正文四".to_string())],
            )
            .await
            .unwrap();
        let (toc_del, chap_del) = storage.clear_cache("default", "all").await.unwrap();
        assert_eq!(toc_del, 1);
        assert_eq!(chap_del, 1);
        let info = storage.get_cache_info("default").await.unwrap();
        assert_eq!(info.toc_cache_count, 0);
        assert_eq!(info.chapter_count, 0);
        assert_eq!(info.total_size, 0);

        // 未知 type：不删任何表
        let (toc_del, chap_del) = storage.clear_cache("default", "unknown").await.unwrap();
        assert_eq!(toc_del, 0);
        assert_eq!(chap_del, 0);

        cleanup(storage, "cache").await;
    }

    /// issue #1：缓存统计/清理按命名空间隔离——清 ns_a 不影响 ns_b 的目录与章节缓存
    #[tokio::test]
    async fn test_cache_namespace_isolation() {
        let storage = test_storage("cachensiso").await;
        storage
            .cache_toc("ns_a", "https://book.com/a", "https://book.com/toc-a", "[]")
            .await
            .unwrap();
        storage
            .cache_toc("ns_b", "https://book.com/b", "https://book.com/toc-b", "[]")
            .await
            .unwrap();
        storage
            .cache_chapter_content("ns_a", "https://book.com/a", 1, "第一章", "正文A")
            .await
            .unwrap();
        storage
            .cache_chapter_content("ns_b", "https://book.com/b", 1, "第一章", "正文B")
            .await
            .unwrap();

        // 统计互不串号
        let info_a = storage.get_cache_info("ns_a").await.unwrap();
        let info_b = storage.get_cache_info("ns_b").await.unwrap();
        assert_eq!((info_a.toc_cache_count, info_a.chapter_count), (1, 1));
        assert_eq!((info_b.toc_cache_count, info_b.chapter_count), (1, 1));

        // 清 ns_a（all）→ 只删 ns_a 的 1 toc + 1 chapter
        let (toc_del, chap_del) = storage.clear_cache("ns_a", "all").await.unwrap();
        assert_eq!((toc_del, chap_del), (1, 1));

        // ns_b 数据仍在
        let info_b = storage.get_cache_info("ns_b").await.unwrap();
        assert_eq!(info_b.toc_cache_count, 1, "清 ns_a 不应影响 ns_b 目录缓存");
        assert_eq!(info_b.chapter_count, 1, "清 ns_a 不应影响 ns_b 章节缓存");
        assert_eq!(
            info_b.total_size,
            info_b.toc_cache_size + info_b.chapter_size
        );
        assert_eq!(
            storage
                .get_chapter_content("ns_b", "https://book.com/b", 1)
                .await
                .unwrap()
                .as_deref(),
            Some("正文B"),
            "ns_b 正文缓存应保留"
        );

        // ns_a 已清空
        let info_a = storage.get_cache_info("ns_a").await.unwrap();
        assert_eq!((info_a.toc_cache_count, info_a.chapter_count), (0, 0));
        assert_eq!(info_a.total_size, 0);

        cleanup(storage, "cachensiso").await;
    }

    /// 单书缓存：book_cache_info（章节数+大小）/ delete_book_cache（仅删该书，不动书架）
    #[tokio::test]
    async fn test_book_cache_info_and_delete() {
        let storage = test_storage("bookcache").await;
        // 书 A：本地书章节 + 书源书正文缓存（md5 键）混合
        storage
            .save_chapters(
                "https://book.com/a",
                &[
                    ("第一章".to_string(), "正文一二三四五".to_string()),
                    ("第二章".to_string(), "正文六七八九十".to_string()),
                ],
            )
            .await
            .unwrap();
        storage
            .cache_chapter_content(
                "default",
                "https://book.com/a",
                crate::util::md5::chapter_url_hash("https://book.com/c/3"),
                "第三章",
                "正文三",
            )
            .await
            .unwrap();
        storage
            .save_chapters(
                "https://book.com/b",
                &[("第一章".to_string(), "另一本".to_string())],
            )
            .await
            .unwrap();
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: "https://book.com/a".into(),
                    name: "测试书".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 信息统计：A 共 3 章（本地 2 + 缓存 1），size = 7+7+3 字符
        let (count, size) = storage
            .book_cache_info("default", "https://book.com/a")
            .await
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(size, 17);
        // 无缓存书 → 0
        let (count, size) = storage
            .book_cache_info("default", "https://ghost.com")
            .await
            .unwrap();
        assert_eq!((count, size), (0, 0));

        // 删单书缓存：只删 A 的章节，B 不受影响、书架行保留
        let deleted = storage
            .delete_book_cache("default", "https://book.com/a")
            .await
            .unwrap();
        assert_eq!(deleted, 3);
        assert_eq!(
            storage
                .count_chapters("default", "https://book.com/a")
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            storage
                .count_chapters("default", "https://book.com/b")
                .await
                .unwrap(),
            1
        );
        assert!(
            storage
                .find_book("default", "https://book.com/a")
                .await
                .unwrap()
                .is_some(),
            "删除缓存不应影响书架"
        );
        // 再删空书 → 0 行
        assert_eq!(
            storage
                .delete_book_cache("default", "https://ghost.com")
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "bookcache").await;
    }

    /// GAP 96：连接初始化后 WAL 模式 + busy_timeout 生效（幂等：重复 init 不报错且保持 WAL）
    #[tokio::test]
    async fn test_wal_and_busy_timeout() {
        let storage = test_storage("wal").await;
        // journal_mode 返回当前模式字符串（wal 持久化于库文件头）
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "应启用 WAL 模式，实际 {mode}");
        // busy_timeout 毫秒值
        let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(timeout, 5000, "busy_timeout 应为 5000ms");
        // WAL 副作用文件存在（reader.db-wal 惰性创建——写一次触发）
        storage
            .save_chapters(
                "local://waltest",
                &[("第一章".to_string(), "正文".to_string())],
            )
            .await
            .unwrap();
        let db_path = storage.config.storage_dir().join("reader.db");
        assert!(db_path.exists());

        // 幂等：关闭后重新 init 同一目录，仍为 WAL 且 busy_timeout 生效
        let dir = storage.config.work_dir.clone();
        storage.pool.close().await;
        let mut config = AppConfig::from_env();
        config.work_dir = dir.clone();
        let reopened = init(&config).await.unwrap();
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&reopened.pool)
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "重开库应保持 WAL");
        let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&reopened.pool)
            .await
            .unwrap();
        assert_eq!(timeout, 5000);
        cleanup(reopened, "wal").await;
    }

    /// GAP 96 回归：WAL 模式下池连接不持有跨语句旧读快照——
    /// 连接 A 建立读快照后，连接 B 写入，A 再次读取必须看到新数据
    #[tokio::test]
    async fn test_wal_no_stale_snapshot_across_connections() {
        let storage = test_storage("snaprepro").await;
        // 连接 A：先读一次（若 WAL 隐式读事务跨语句保持，此处建立旧快照）
        let mut conn_a = storage.pool.acquire().await.unwrap();
        let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM books")
            .fetch_one(&mut *conn_a)
            .await
            .unwrap();
        // 连接 B：写入
        let mut conn_b = storage.pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO books (book_url, name, user_namespace) VALUES (?1, ?2, 'default')",
        )
        .bind("https://snap.com/a")
        .bind("快照书")
        .execute(&mut *conn_b)
        .await
        .unwrap();
        drop(conn_b);
        // 连接 A：再次读取——必须能看到写入（快照已刷新）
        let found: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM books WHERE book_url = ?1 AND user_namespace = 'default'",
        )
        .bind("https://snap.com/a")
        .fetch_optional(&mut *conn_a)
        .await
        .unwrap();
        assert_eq!(
            found.map(|r| r.0),
            Some("快照书".to_string()),
            "WAL 下旧读快照导致跨连接读不到新数据"
        );
        drop(conn_a);
        cleanup(storage, "snaprepro").await;
    }

    /// 全书搜索：LIKE 匹配 + 命中摘要（前后截取）+ %/_ 转义 + 章节序 + limit
    #[tokio::test]
    async fn test_search_book_content() {
        let storage = test_storage("search").await;
        storage
            .save_chapters(
                "local://book1",
                &[
                    ("第一章".to_string(), "这是第一章的正文，关键词出现了。".to_string()),
                    ("第二章".to_string(), "本章没有匹配内容。".to_string()),
                    ("第三章".to_string(), "在很久很久以前，有一个非常非常长的开头铺垫，它洋洋洒洒写了很多很多字，然后关键词在这里再次出现，后面还有一点内容。".to_string()),
                ],
            )
            .await
            .unwrap();
        storage
            .save_chapters(
                "local://book2",
                &[("第一章".to_string(), "另一本书里的关键词。".to_string())],
            )
            .await
            .unwrap();

        // 命中两章，按章节序返回
        let hits = storage
            .search_book_content("default", "local://book1", "关键词", 50)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chapter_index, 0);
        assert_eq!(hits[0].title, "第一章");
        assert!(hits[0].snippet.contains("关键词"));
        assert!(hits[0].snippet.starts_with("这是第一章"));
        assert_eq!(hits[1].chapter_index, 2);
        assert!(hits[1].snippet.contains("关键词"));
        assert!(
            hits[1].snippet.starts_with("…"),
            "超长段落应截断补省略号: {}",
            hits[1].snippet
        );

        // 其他书不串
        let hits = storage
            .search_book_content("default", "local://book2", "关键词", 50)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "第一章");

        // 无命中 / 书不存在
        assert!(storage
            .search_book_content("default", "local://book1", "不存在词", 50)
            .await
            .unwrap()
            .is_empty());
        assert!(storage
            .search_book_content("default", "local://ghost", "关键词", 50)
            .await
            .unwrap()
            .is_empty());

        // 大小写不敏感（ASCII）
        storage
            .save_chapters(
                "local://book3",
                &[("Ch1".to_string(), "Hello World here".to_string())],
            )
            .await
            .unwrap();
        let hits = storage
            .search_book_content("default", "local://book3", "world", 50)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("World"));

        // limit 生效
        let hits = storage
            .search_book_content("default", "local://book1", "关键词", 1)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        // %/_ 作为字面量转义（不当作 LIKE 通配符）
        storage
            .save_chapters(
                "local://book4",
                &[
                    ("C1".to_string(), "进度5_0%完成。".to_string()),
                    ("C2".to_string(), "完全没有任何特殊符号的一章。".to_string()),
                ],
            )
            .await
            .unwrap();
        let hits = storage
            .search_book_content("default", "local://book4", "5_0%", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "% 应转义为字面量");
        assert_eq!(hits[0].title, "C1");
        let hits = storage
            .search_book_content("default", "local://book4", "5_", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "_ 应转义为字面量");
        let hits = storage
            .search_book_content("default", "local://book4", "%", 10)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "% 转义后只匹配含字面 % 的行（未转义会匹配全部）"
        );
        assert_eq!(hits[0].title, "C1");
        let hits = storage
            .search_book_content("default", "local://book4", "_", 10)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "_ 转义后只匹配含字面 _ 的行（未转义会匹配全部）"
        );
        assert_eq!(hits[0].title, "C1");
        assert_eq!(
            storage
                .count_chapters("default", "local://book4")
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            storage
                .count_chapters("default", "local://ghost")
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "search").await;
    }

    /// 书源订阅：CRUD 往返 + 命名空间隔离 + default 回退
    #[tokio::test]
    async fn test_source_sub_crud() {
        let storage = test_storage("subs").await;
        let raw = r#"[{"bookSourceUrl":"https://s1.com","bookSourceName":"源1"}]"#;

        // 保存 → 查询往返（raw_json 原文保留）
        storage
            .save_source_sub("default", "https://sub.com/all.json", "全部书源", raw, &[])
            .await
            .unwrap();
        let list = storage.get_source_subs("default").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].url, "https://sub.com/all.json");
        assert_eq!(list[0].name, "全部书源");
        assert_eq!(list[0].raw_json.as_deref(), Some(raw));
        assert_eq!(list[0].user_namespace, "default");

        // 覆盖保存（改名）
        storage
            .save_source_sub(
                "default",
                "https://sub.com/all.json",
                "全部书源v2",
                raw,
                &[],
            )
            .await
            .unwrap();
        let list = storage.get_source_subs("default").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "全部书源v2");

        // 按 URL 查找
        let sub = storage
            .find_source_sub("default", "https://sub.com/all.json")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sub.name, "全部书源v2");
        assert!(storage
            .find_source_sub("default", "https://sub.com/ghost")
            .await
            .unwrap()
            .is_none());

        // 命名空间隔离 + default 回退
        assert_eq!(
            storage.get_source_subs("alice").await.unwrap().len(),
            1,
            "alice 无订阅回退 default"
        );
        assert!(storage
            .find_source_sub("alice", "https://sub.com/all.json")
            .await
            .unwrap()
            .is_some());
        storage
            .save_source_sub("alice", "https://sub.com/a.json", "爱丽丝订阅", raw, &[])
            .await
            .unwrap();
        let alice = storage.get_source_subs("alice").await.unwrap();
        assert_eq!(
            alice.len(),
            2,
            "自有订阅 + 未覆盖的 default 系统订阅合并显示"
        );
        assert!(
            alice.iter().any(|s| s.name == "爱丽丝订阅")
                && alice.iter().any(|s| s.name == "全部书源v2")
        );

        // 删除：本命名空间记录优先；普通用户删除 default 系统订阅 → 个人 hidden 覆盖，
        // default 系统订阅保留（copy-on-write）
        assert_eq!(
            storage
                .delete_source_sub("alice", "https://sub.com/all.json")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .delete_source_sub("alice", "https://sub.com/a.json")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage.get_source_subs("alice").await.unwrap().len(),
            0,
            "alice 删除后列表清空（hidden 覆盖）"
        );
        assert!(
            !storage.get_source_subs("default").await.unwrap().is_empty(),
            "default 系统订阅不应被普通用户删除"
        );

        cleanup(storage, "subs").await;
    }

    /// copy-on-write：普通用户删除/停用 default 系统书源只生成个人覆盖副本；
    /// default 系统行不受影响，其他用户仍回退系统源
    #[tokio::test]
    async fn test_user_source_overlay_does_not_touch_default() {
        let storage = test_storage("overlay").await;
        let default_src = source("https://sys.com", "系统源", None);
        storage
            .save_book_source("default", &default_src)
            .await
            .unwrap();

        // 普通用户停用 → 个人 disabled 副本；default 仍启用
        storage
            .update_book_source_enabled("alice", "https://sys.com", false)
            .await
            .unwrap();
        let alice = storage.get_book_sources("alice").await.unwrap();
        assert_eq!(alice.len(), 1);
        assert!(!alice[0].enabled);
        assert_eq!(alice[0].user_namespace, "alice");
        assert!(
            storage
                .get_book_source("default", "https://sys.com")
                .await
                .unwrap()
                .unwrap()
                .enabled,
            "default 系统书源保持启用"
        );

        // 普通用户删除 → 个人 hidden 覆盖；default 系统行保留；bob 仍能看到系统源
        storage
            .delete_book_source("alice", "https://sys.com")
            .await
            .unwrap();
        assert!(
            !storage
                .get_book_sources("alice")
                .await
                .unwrap()
                .iter()
                .any(|s| s.book_source_url == "https://sys.com"),
            "alice 列表隐藏系统源"
        );
        assert!(
            storage
                .get_book_sources("bob")
                .await
                .unwrap()
                .iter()
                .any(|s| s.book_source_url == "https://sys.com"),
            "bob 仍回退系统源"
        );
        assert!(
            storage
                .get_book_source("default", "https://sys.com")
                .await
                .unwrap()
                .is_some(),
            "default 系统行未被删除"
        );

        // 订阅同理：普通用户删除 default 订阅 → 个人 hidden 覆盖，default 保留
        storage
            .save_source_sub(
                "default",
                "https://sub.example/all.json",
                "系统订阅",
                "[]",
                &[],
            )
            .await
            .unwrap();
        storage
            .delete_source_sub("alice", "https://sub.example/all.json")
            .await
            .unwrap();
        assert!(
            storage.get_source_subs("alice").await.unwrap().is_empty(),
            "alice 订阅列表隐藏系统订阅"
        );
        assert!(
            !storage.get_source_subs("default").await.unwrap().is_empty(),
            "default 系统订阅保留"
        );

        cleanup(storage, "overlay").await;
    }

    /// 管理员兜底：无管理员时最早用户（优先名为 admin）自动提升
    #[tokio::test]
    async fn test_ensure_admin_user() {
        let storage = test_storage("admins").await;
        storage
            .insert_user(&crate::model::User {
                username: "alice".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage
            .insert_user(&crate::model::User {
                username: "bob".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(storage.count_admins().await.unwrap(), 0);
        storage.ensure_admin_user().await.unwrap();
        assert_eq!(storage.count_admins().await.unwrap(), 1);
        assert!(
            storage.find_user("alice").await.unwrap().unwrap().is_admin,
            "最早用户被提升为管理员"
        );
        cleanup(storage, "admins").await;
    }

    /// 一次性迁移旧注册默认权限：仅修正仍精确等于旧错误默认值（全关 + 100/200）的用户；
    /// 手动改过的不动；system_settings 标记保证只执行一次
    #[tokio::test]
    async fn test_migrate_user_permission_defaults() {
        let storage = test_storage("permdefaults").await;
        // 旧默认值（v5.0.4 及以前注册）→ 应被覆盖为全开 + 80000/5000
        storage
            .insert_user(&User {
                username: "legacy_old".into(),
                enable_webdav: false,
                enable_local_store: false,
                enable_book_source: false,
                enable_rss_source: false,
                book_source_limit: 100,
                book_limit: 200,
                ..Default::default()
            })
            .await
            .unwrap();
        // 人工改过权限（书源仍开）→ 保持不动
        storage
            .insert_user(&User {
                username: "legacy_edited".into(),
                enable_webdav: false,
                enable_local_store: false,
                enable_book_source: true,
                enable_rss_source: false,
                book_source_limit: 100,
                book_limit: 200,
                ..Default::default()
            })
            .await
            .unwrap();
        // 人工改过上限 → 保持不动
        storage
            .insert_user(&User {
                username: "legacy_custom_limit".into(),
                enable_webdav: false,
                enable_local_store: false,
                enable_book_source: false,
                enable_rss_source: false,
                book_source_limit: 999,
                book_limit: 200,
                ..Default::default()
            })
            .await
            .unwrap();

        // init 阶段已无用户跑过一次迁移并写入标记；测试单独清掉标记以模拟升级现场
        storage
            .delete_system_setting("user_permission_defaults_v500")
            .await
            .unwrap();
        storage
            .delete_system_setting("user_permission_defaults_v520")
            .await
            .unwrap();
        storage.migrate_user_permission_defaults().await.unwrap();

        let old = storage.find_user("legacy_old").await.unwrap().unwrap();
        assert!(old.enable_webdav && old.enable_local_store);
        assert!(old.enable_book_source && old.enable_rss_source);
        assert_eq!(old.book_source_limit, 80000);
        assert_eq!(old.book_limit, 5000);

        let edited = storage.find_user("legacy_edited").await.unwrap().unwrap();
        assert!(!edited.enable_webdav, "人工改过的权限不应被覆盖");
        assert!(edited.enable_book_source);
        assert_eq!(edited.book_source_limit, 100);

        let custom = storage
            .find_user("legacy_custom_limit")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(custom.book_source_limit, 999, "人工改过的上限不应被覆盖");
        assert!(!custom.enable_book_source);

        // 标记已写入：第二次执行不再修改（新注册全开用户保持不动）
        storage
            .insert_user(&User {
                username: "fresh_new".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage.migrate_user_permission_defaults().await.unwrap();
        let fresh = storage.find_user("fresh_new").await.unwrap().unwrap();
        assert!(!fresh.enable_book_source, "第二次迁移不应再触碰用户");

        cleanup(storage, "permdefaults").await;
    }

    /// 书源书正文缓存：chapterUrl md5 哈希键写入 → 同键读取；与本地书顺序索引键域不重叠；覆盖写
    #[tokio::test]
    async fn test_chapter_content_cache_roundtrip() {
        let storage = test_storage("chapcache").await;
        let book_url = "https://book.com/a";
        let url1 = "https://book.com/1.html";
        let url2 = "https://book.com/2.html";
        let idx1 = crate::util::md5::chapter_url_hash(url1);
        let idx2 = crate::util::md5::chapter_url_hash(url2);
        assert!(idx1 > 0 && idx2 > 0, "哈希恒为正");
        assert_ne!(idx1, idx2, "不同 chapterUrl 哈希不同");

        // 写入 → 同 chapterUrl 直读
        storage
            .cache_chapter_content("default", book_url, idx1, "第一章", "第一章正文内容。")
            .await
            .unwrap();
        let got = storage
            .get_chapter_content("default", book_url, idx1)
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("第一章正文内容。"));
        assert_eq!(
            storage
                .get_chapter_content("default", book_url, idx2)
                .await
                .unwrap(),
            None,
            "未缓存键应无命中"
        );

        // 覆盖写（同一 chapterUrl 再次缓存更新正文）
        storage
            .cache_chapter_content("default", book_url, idx1, "第一章", "更新后的正文。")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_chapter_content("default", book_url, idx1)
                .await
                .unwrap()
                .as_deref(),
            Some("更新后的正文。")
        );

        // 与本地书顺序索引共存：哈希键域（~2^60）不重叠 0..n
        storage
            .save_chapters(book_url, &[("本地1".to_string(), "本地内容1".to_string())])
            .await
            .unwrap();
        assert_eq!(
            storage.count_chapters("default", book_url).await.unwrap(),
            2,
            "缓存行 + 本地行共存"
        );
        assert_eq!(
            storage
                .get_chapter_content("default", book_url, 0)
                .await
                .unwrap()
                .as_deref(),
            Some("本地内容1")
        );
        assert_eq!(
            storage
                .get_chapter_content("default", book_url, idx1)
                .await
                .unwrap()
                .as_deref(),
            Some("更新后的正文。")
        );

        // 不同书同 chapterUrl → 按 book_url 隔离
        storage
            .cache_chapter_content(
                "default",
                "https://book.com/b",
                idx1,
                "第一章",
                "B 书正文。",
            )
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_chapter_content("default", "https://book.com/a", idx1)
                .await
                .unwrap()
                .as_deref(),
            Some("更新后的正文。")
        );
        assert_eq!(
            storage
                .get_chapter_content("default", "https://book.com/b", idx1)
                .await
                .unwrap()
                .as_deref(),
            Some("B 书正文。")
        );

        cleanup(storage, "chapcache").await;
    }

    /// P0 跨用户缓存隔离：userA 写正文/目录缓存，userB 同 bookUrl 不得命中
    #[tokio::test]
    async fn test_chapter_cache_namespace_isolation() {
        let storage = test_storage("nsiso").await;
        let book_url = "https://shared.com/book";
        let ch_url = "https://shared.com/ch1.html";
        let idx = crate::util::md5::chapter_url_hash(ch_url);

        storage
            .cache_chapter_content("userA", book_url, idx, "第一章", "A 的正文")
            .await
            .unwrap();
        storage
            .cache_toc(
                "userA",
                book_url,
                "https://shared.com/toc",
                r#"[{"title":"A目录"}]"#,
            )
            .await
            .unwrap();

        // 同命名空间命中
        assert_eq!(
            storage
                .get_chapter_content("userA", book_url, idx)
                .await
                .unwrap()
                .as_deref(),
            Some("A 的正文")
        );
        assert!(storage
            .get_toc_cache("userA", "https://shared.com/toc", 86_400_000)
            .await
            .unwrap()
            .is_some());

        // 他命名空间不可见（不串扰）
        assert_eq!(
            storage
                .get_chapter_content("userB", book_url, idx)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_toc_cache("userB", "https://shared.com/toc", 86_400_000)
                .await
                .unwrap(),
            None
        );

        // 删除仅影响本命名空间
        storage.delete_book_cache("userB", book_url).await.unwrap();
        assert_eq!(
            storage
                .get_chapter_content("userA", book_url, idx)
                .await
                .unwrap()
                .as_deref(),
            Some("A 的正文")
        );
        storage.delete_book_cache("userA", book_url).await.unwrap();
        assert_eq!(
            storage
                .get_chapter_content("userA", book_url, idx)
                .await
                .unwrap(),
            None
        );

        cleanup(storage, "nsiso").await;
    }

    /// 分组收尾：带书数列表 / 重命名保留 order / 删除分组组内书置 0 + 命名空间隔离
    #[tokio::test]
    async fn test_book_group_count_rename_delete() {
        let storage = test_storage("grpfin").await;
        let g1 = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    name: "玄幻".into(),
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let g2 = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    name: "言情".into(),
                    order: 2,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // 书：g1 两本、g2 一本、未分组一本（group 0 不计入任何组）
        storage
            .upsert_book("default", &shelf_book("https://b.com/1", "书1"))
            .await
            .unwrap();
        storage
            .upsert_book("default", &shelf_book("https://b.com/2", "书2"))
            .await
            .unwrap();
        storage
            .upsert_book("default", &shelf_book("https://b.com/3", "书3"))
            .await
            .unwrap();
        storage
            .upsert_book("default", &shelf_book("https://b.com/4", "书4"))
            .await
            .unwrap();
        storage
            .update_book_group_id("default", "https://b.com/1", g1.id)
            .await
            .unwrap();
        storage
            .update_book_group_id("default", "https://b.com/2", g1.id)
            .await
            .unwrap();
        storage
            .update_book_group_id("default", "https://b.com/3", g2.id)
            .await
            .unwrap();

        // 带书数列表（bookCount + orderNum 别名）
        let list = storage
            .list_book_groups_with_count("default")
            .await
            .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "玄幻");
        assert_eq!(list[0].book_count, 2);
        assert_eq!(list[0].order, 1);
        assert_eq!(list[0].order_num, 1);
        assert_eq!(list[1].name, "言情");
        assert_eq!(list[1].book_count, 1);

        // 重命名：仅改 name，order/id 保留；不存在返回 0 行
        assert_eq!(
            storage
                .rename_book_group("default", g1.id, "玄幻v2")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .rename_book_group("default", 9999, "幽灵")
                .await
                .unwrap(),
            0
        );
        let list = storage
            .list_book_groups_with_count("default")
            .await
            .unwrap();
        assert_eq!(list[0].name, "玄幻v2");
        assert_eq!(list[0].order, 1, "重命名保留 order");
        assert_eq!(list[0].id, g1.id, "重命名保留 id");
        assert_eq!(list[0].book_count, 2, "重命名不影响书数");

        // 删除 g1：组内书置 0，组删除；g2 与书不受影响
        assert_eq!(
            storage.delete_book_group("default", g1.id).await.unwrap(),
            1
        );
        assert_eq!(
            storage.delete_book_group("default", g1.id).await.unwrap(),
            0,
            "重复删除 0 行"
        );
        let list = storage
            .list_book_groups_with_count("default")
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "言情");
        let b1 = storage
            .find_book("default", "https://b.com/1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b1.group, 0, "组内书应置 0（未分组）");
        let b2 = storage
            .find_book("default", "https://b.com/2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b2.group, 0);
        let b3 = storage
            .find_book("default", "https://b.com/3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b3.group, g2.id, "其他组书不受影响");

        // 命名空间隔离：alice 删除不了 default 的分组
        assert_eq!(storage.delete_book_group("alice", g2.id).await.unwrap(), 0);
        assert_eq!(storage.list_book_groups("default").await.unwrap().len(), 1);
        assert_eq!(
            storage
                .rename_book_group("alice", g2.id, "越权改名")
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "grpfin").await;
    }

    // ---------------- 书源登录态 cookie（按用户隔离） ----------------

    #[tokio::test]
    async fn test_cookie_roundtrip_and_namespace_isolation() {
        let storage = test_storage("cookie").await;

        // 初始无 cookie
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            None
        );

        // 写入 → 读回
        storage
            .set_cookie("default", "https://a.com", "sid=abc; token=xyz")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            Some("sid=abc; token=xyz".to_string())
        );

        // 覆盖写
        storage
            .set_cookie("default", "https://a.com", "sid=def")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            Some("sid=def".to_string())
        );

        // 按用户隔离：alice 读不到 default 的 cookie
        assert_eq!(
            storage.get_cookie("alice", "https://a.com").await.unwrap(),
            None
        );
        storage
            .set_cookie("alice", "https://a.com", "sid=alice")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            Some("sid=def".to_string())
        );
        assert_eq!(
            storage.get_cookie("alice", "https://a.com").await.unwrap(),
            Some("sid=alice".to_string())
        );

        // 清除
        assert_eq!(
            storage
                .clear_cookie("alice", "https://a.com")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage.get_cookie("alice", "https://a.com").await.unwrap(),
            None
        );
        assert_eq!(
            storage
                .clear_cookie("alice", "https://a.com")
                .await
                .unwrap(),
            0
        );

        cleanup(storage, "cookie").await;
    }

    #[tokio::test]
    async fn test_cookie_by_base_matching() {
        let storage = test_storage("cookiebase").await;
        storage
            .set_cookie("default", "https://a.com", "sid=abc")
            .await
            .unwrap();
        // `##` 备用地址后缀：主地址命中
        storage
            .set_cookie("default", "https://b.com##https://b2.com", "sid=bbb")
            .await
            .unwrap();

        // 请求 URL 的 base 命中书源 source_url base（含端口/路径差异）
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://a.com")
                .await
                .unwrap(),
            Some("sid=abc".to_string())
        );
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://a.com/book/1?x=2")
                .await
                .unwrap(),
            Some("sid=abc".to_string())
        );
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://b2.com/path")
                .await
                .unwrap(),
            Some("sid=bbb".to_string())
        );
        // 不匹配
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://c.com")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_cookie_by_base("alice", "https://a.com")
                .await
                .unwrap(),
            None
        );
        // 端口不同不命中
        storage
            .set_cookie("default", "https://d.com:8443", "sid=dd")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://d.com")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://d.com:8443/x")
                .await
                .unwrap(),
            Some("sid=dd".to_string())
        );

        cleanup(storage, "cookiebase").await;
    }

    #[tokio::test]
    async fn test_cookie_user_agent_record() {
        let storage = test_storage("cookieua").await;
        storage
            .set_cookie("default", "https://a.com", "sid=1")
            .await
            .unwrap();
        storage
            .set_cookie_user_agent("default", "https://a.com", "fs-ua/1.0")
            .await
            .unwrap();
        let (cookie, ua) = storage
            .get_source_session("default", "https://a.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cookie, "sid=1");
        assert_eq!(ua, "fs-ua/1.0");
        // set_cookie 覆盖不丢 UA
        storage
            .set_cookie("default", "https://a.com", "sid=2")
            .await
            .unwrap();
        let (cookie, ua) = storage
            .get_source_session("default", "https://a.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cookie, "sid=2");
        assert_eq!(ua, "fs-ua/1.0");
        cleanup(storage, "cookieua").await;
    }

    #[tokio::test]
    async fn test_login_header_roundtrip_and_base_matching() {
        let storage = test_storage("loginheader").await;
        // 无 → None
        assert_eq!(
            storage
                .get_login_header("default", "https://a.com")
                .await
                .unwrap(),
            None
        );

        storage
            .set_login_header(
                "default",
                "https://a.com",
                r#"{"X-Auth-Token":"tok-1","X-User":"alice"}"#,
            )
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_login_header("default", "https://a.com")
                .await
                .unwrap(),
            Some(r#"{"X-Auth-Token":"tok-1","X-User":"alice"}"#.to_string())
        );

        // 按 base 命中（请求 URL 带路径/查询；`##` 备地址命中）
        storage
            .set_login_header("default", "https://b.com##https://b2.com", r#"{"X-B":"1"}"#)
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_login_header_by_base("default", "https://a.com/book/1?x=2")
                .await
                .unwrap(),
            Some(r#"{"X-Auth-Token":"tok-1","X-User":"alice"}"#.to_string())
        );
        assert_eq!(
            storage
                .get_login_header_by_base("default", "https://b2.com/path")
                .await
                .unwrap(),
            Some(r#"{"X-B":"1"}"#.to_string())
        );
        assert_eq!(
            storage
                .get_login_header_by_base("default", "https://c.com")
                .await
                .unwrap(),
            None
        );

        // 用户隔离
        assert_eq!(
            storage
                .get_login_header("alice", "https://a.com")
                .await
                .unwrap(),
            None
        );

        // set_cookie 覆盖不丢登录头（同一行）
        storage
            .set_cookie("default", "https://a.com", "sid=abc")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_login_header("default", "https://a.com")
                .await
                .unwrap(),
            Some(r#"{"X-Auth-Token":"tok-1","X-User":"alice"}"#.to_string())
        );

        // 空值 = 清除
        storage
            .set_login_header("default", "https://a.com", "")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_login_header("default", "https://a.com")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_login_header_by_base("default", "https://a.com")
                .await
                .unwrap(),
            None
        );
        // cookie 行仍保留
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            Some("sid=abc".to_string())
        );

        cleanup(storage, "loginheader").await;
    }

    #[tokio::test]
    async fn test_delete_book_source_cleans_cookie() {
        let storage = test_storage("cookiedel").await;
        storage
            .set_cookie("default", "https://a.com", "sid=1")
            .await
            .unwrap();
        let s = source("https://a.com", "A源", None);
        storage.save_book_source("default", &s).await.unwrap();

        assert_eq!(
            storage
                .delete_book_source("default", "https://a.com")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            None
        );

        // delete_all 清理
        storage
            .set_cookie("default", "https://a.com", "sid=2")
            .await
            .unwrap();
        storage
            .set_cookie("default", "https://b.com", "sid=3")
            .await
            .unwrap();
        storage
            .save_book_source("default", &source("https://a.com", "A", None))
            .await
            .unwrap();
        storage
            .save_book_source("default", &source("https://b.com", "B", None))
            .await
            .unwrap();
        storage.delete_all_book_sources("default").await.unwrap();
        assert_eq!(
            storage
                .get_cookie("default", "https://a.com")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_cookie("default", "https://b.com")
                .await
                .unwrap(),
            None
        );

        cleanup(storage, "cookiedel").await;
    }

    #[test]
    fn test_normalize_base() {
        assert_eq!(
            normalize_base("https://a.com").as_deref(),
            Some("https://a.com")
        );
        assert_eq!(
            normalize_base("https://a.com/").as_deref(),
            Some("https://a.com")
        );
        assert_eq!(
            normalize_base("https://a.com/book/1?x=2").as_deref(),
            Some("https://a.com")
        );
        assert_eq!(
            normalize_base("https://a.com:8443/x").as_deref(),
            Some("https://a.com:8443")
        );
        assert_eq!(
            normalize_base("http://a.com").as_deref(),
            Some("http://a.com")
        );
        assert_eq!(normalize_base("a.com").as_deref(), Some("https://a.com"));
        assert!(normalize_base("").is_none());
    }

    // ---------------- GAP 44：RSS 分组 ----------------

    /// RSS 源 group 往返：保存（含 sourceGroup）→ 列表/查找返回 group；覆盖保存更新 group；
    /// 删除后无残留
    #[tokio::test]
    async fn test_rss_source_group_roundtrip() {
        let storage = test_storage("rssgroup").await;
        let mut s = crate::model::RssSource {
            source_url: "https://r.com/feed.xml".into(),
            source_name: "科技资讯".into(),
            source_group: Some("科技".into()),
            enabled: true,
            ..Default::default()
        };
        storage.save_rss_source("default", &s).await.unwrap();
        let got = storage
            .get_rss_sources("default")
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.source_url == "https://r.com/feed.xml")
            .expect("保存后应能查到");
        assert_eq!(
            got.source_group.as_deref(),
            Some("科技"),
            "group 应落库并读回"
        );
        assert_eq!(got.source_name, "科技资讯");

        // 覆盖保存：改 group
        s.source_group = Some("新闻 综合".into());
        storage.save_rss_source("default", &s).await.unwrap();
        let got = storage
            .find_rss_source("default", "https://r.com/feed.xml")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.source_group.as_deref(), Some("新闻 综合"));

        // 删除
        storage
            .delete_rss_source("default", "https://r.com/feed.xml")
            .await
            .unwrap();
        assert!(storage
            .get_rss_sources("default")
            .await
            .unwrap()
            .iter()
            .all(|x| x.source_url != "https://r.com/feed.xml"));

        cleanup(storage, "rssgroup").await;
    }

    // ---------------- GAP 117：删除书清理封面文件 ----------------

    /// 封面残留清理：删除唯一引用书的封面文件被移除；多书共享封面时保留；
    /// 无封面/路径不匹配不报错
    #[tokio::test]
    async fn test_delete_book_cleans_orphan_cover() {
        let storage = test_storage("coverclean").await;
        let cover_dir = storage.config.storage_dir().join("assets/default/covers");
        std::fs::create_dir_all(&cover_dir).unwrap();
        std::fs::write(cover_dir.join("a1.jpg"), b"cover-a").unwrap();
        std::fs::write(cover_dir.join("a2.jpg"), b"cover-b").unwrap();
        let cover_a = "/assets/default/covers/a1.jpg";
        let cover_b = "/assets/default/covers/a2.jpg";

        // 两本书共享 cover_a；一本书独享 cover_b
        for (url, cover) in [
            ("https://b1.com", cover_a),
            ("https://b2.com", cover_a),
            ("https://b3.com", cover_b),
        ] {
            storage
                .upsert_book(
                    "default",
                    &crate::model::Book {
                        book_url: url.into(),
                        name: url.into(),
                        cover_url: Some(cover.into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }

        // 删 b1（cover_a 仍被 b2 引用 → 文件保留）
        storage
            .delete_book("default", "https://b1.com")
            .await
            .unwrap();
        assert!(cover_dir.join("a1.jpg").exists(), "共享封面不应删除");
        // 删 b2 → cover_a 无引用 → 文件清理
        storage
            .delete_book("default", "https://b2.com")
            .await
            .unwrap();
        assert!(!cover_dir.join("a1.jpg").exists(), "无引用封面应清理");
        // 删 b3 → cover_b 清理
        storage
            .delete_book("default", "https://b3.com")
            .await
            .unwrap();
        assert!(!cover_dir.join("a2.jpg").exists());

        // 批量删除也清理
        std::fs::write(cover_dir.join("a3.jpg"), b"cover-c").unwrap();
        for url in ["https://c1.com", "https://c2.com"] {
            storage
                .upsert_book(
                    "default",
                    &crate::model::Book {
                        book_url: url.into(),
                        name: url.into(),
                        cover_url: Some("/assets/default/covers/a3.jpg".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        storage
            .delete_books(
                "default",
                &["https://c1.com".into(), "https://c2.com".into()],
            )
            .await
            .unwrap();
        assert!(!cover_dir.join("a3.jpg").exists(), "批量删除后封面应清理");

        // 封面路径穿越样式（.. / 反斜杠）→ 不删除任何文件
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: "https://d1.com".into(),
                    name: "d".into(),
                    cover_url: Some("/assets/default/covers/../a1.jpg".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .delete_book("default", "https://d1.com")
            .await
            .unwrap();

        cleanup(storage, "coverclean").await;
    }

    // ---------------- P1-C1：跨用户缓存删除防护 ----------------

    /// 跨用户 delete_book：书属 default，alice 删不掉章节/目录缓存/书架行；
    /// default 自己删则全部清除（含 toc_cache）
    #[tokio::test]
    async fn test_delete_book_cache_ns_scoped() {
        let storage = test_storage("delns").await;
        let url = "https://book.com/x";
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: url.into(),
                    name: "书X".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_chapters(url, &[("第一章".into(), "正文一".into())])
            .await
            .unwrap();
        storage
            .cache_toc("default", url, url, "[{ \"t\": 1 }]")
            .await
            .unwrap();
        assert_eq!(storage.count_chapters("default", url).await.unwrap(), 1);
        assert_eq!(
            storage
                .get_toc_cache("default", url, 86_400_000)
                .await
                .unwrap()
                .as_deref(),
            Some("[{ \"t\": 1 }]")
        );

        // 他人命名空间删除：书架行/章节/目录缓存均不受影响
        let r = storage.delete_book("alice", url).await.unwrap();
        assert_eq!(r, 0, "跨用户不应删除书架行");
        assert!(storage.find_book("default", url).await.unwrap().is_some());
        assert_eq!(
            storage.count_chapters("default", url).await.unwrap(),
            1,
            "章节缓存应保留"
        );
        assert!(
            storage
                .get_toc_cache("default", url, 86_400_000)
                .await
                .unwrap()
                .is_some(),
            "目录缓存应保留"
        );

        // 本人删除：书架行 + 章节 + 目录缓存全部清除
        let r = storage.delete_book("default", url).await.unwrap();
        assert_eq!(r, 1);
        assert!(storage.find_book("default", url).await.unwrap().is_none());
        assert_eq!(storage.count_chapters("default", url).await.unwrap(), 0);
        assert!(storage
            .get_toc_cache("default", url, 86_400_000)
            .await
            .unwrap()
            .is_none());
        cleanup(storage, "delns").await;
    }

    /// 跨用户 delete_books 批量：仅删本命名空间书籍及其缓存
    #[tokio::test]
    async fn test_delete_books_cache_ns_scoped() {
        let storage = test_storage("delnsb").await;
        let url = "https://book.com/y";
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: url.into(),
                    name: "书Y".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_chapters(url, &[("第一章".into(), "正文".into())])
            .await
            .unwrap();
        storage.cache_toc("default", url, url, "[]").await.unwrap();

        // alice 批量删（含他人书 URL）→ 0 行，缓存保留
        let r = storage
            .delete_books("alice", &[url.to_string()])
            .await
            .unwrap();
        assert_eq!(r, 0);
        assert_eq!(storage.count_chapters("default", url).await.unwrap(), 1);
        assert!(storage
            .get_toc_cache("default", url, 86_400_000)
            .await
            .unwrap()
            .is_some());

        // default 本人批量删 → 全清
        let r = storage
            .delete_books("default", &[url.to_string()])
            .await
            .unwrap();
        assert_eq!(r, 1);
        assert_eq!(storage.count_chapters("default", url).await.unwrap(), 0);
        assert!(storage
            .get_toc_cache("default", url, 86_400_000)
            .await
            .unwrap()
            .is_none());
        cleanup(storage, "delnsb").await;
    }

    /// 跨用户 delete_local_book：他人删不动，本人删（含章节）成功
    #[tokio::test]
    async fn test_delete_local_book_ns_scoped() {
        let storage = test_storage("delloc").await;
        let url = "local:///books/测试.mobi";
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: url.into(),
                    name: "本地书".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_chapters(url, &[("第一章".into(), "正文".into())])
            .await
            .unwrap();

        storage.delete_local_book("alice", url).await.unwrap();
        assert!(
            storage.find_book("default", url).await.unwrap().is_some(),
            "跨用户不应删除本地书"
        );
        assert_eq!(storage.count_chapters("default", url).await.unwrap(), 1);

        storage.delete_local_book("default", url).await.unwrap();
        assert!(storage.find_book("default", url).await.unwrap().is_none());
        assert_eq!(storage.count_chapters("default", url).await.unwrap(), 0);
        cleanup(storage, "delloc").await;
    }

    /// 跨用户 replace_chapters：非本人书拒绝（不删不插）；本人书正常替换
    #[tokio::test]
    async fn test_replace_chapters_ns_scoped() {
        let storage = test_storage("repns").await;
        let url = "local:///books/替换.mobi";
        storage
            .upsert_book(
                "default",
                &crate::model::Book {
                    book_url: url.into(),
                    name: "本地书".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_chapters(url, &[("旧章".into(), "旧正文".into())])
            .await
            .unwrap();

        // alice 替换 → 拒绝，旧章保留
        let err = storage
            .replace_chapters("alice", url, &[("新章".into(), "新正文".into())])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("无权"), "应拒绝跨用户替换: {err}");
        let list = storage.list_chapters(url).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1, "旧章");

        // default 本人替换 → 成功
        storage
            .replace_chapters("default", url, &[("新章".into(), "新正文".into())])
            .await
            .unwrap();
        let list = storage.list_chapters(url).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1, "新章");
        assert_eq!(
            storage
                .get_chapter_content("default", url, 0)
                .await
                .unwrap()
                .as_deref(),
            Some("新正文")
        );
        cleanup(storage, "repns").await;
    }

    // ---------------- P1-C2：book_groups / replace_rules / txt_toc_rules 归属校验 ----------------

    /// 跨用户 save_book_group（同 id）：拒绝，他人分组不被覆写；本人覆盖正常
    #[tokio::test]
    async fn test_save_book_group_cross_ns_rejected() {
        let storage = test_storage("grpns").await;
        let g = storage
            .save_book_group(
                "alice",
                &crate::model::BookGroup {
                    name: "alice的分组".into(),
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // default 用同 id 保存 → 拒绝
        let err = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    id: g.id,
                    name: "劫持".into(),
                    order: 9,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("无权"), "跨用户分组保存应拒绝: {err}");
        let list = storage.list_book_groups("alice").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "alice的分组", "他人分组不应被覆写");
        // alice 本人覆盖 → 成功
        storage
            .save_book_group(
                "alice",
                &crate::model::BookGroup {
                    id: g.id,
                    name: "改名".into(),
                    order: 2,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let list = storage.list_book_groups("alice").await.unwrap();
        assert_eq!(list[0].name, "改名");
        // default 新建（id=0）不受影响
        let g2 = storage
            .save_book_group(
                "default",
                &crate::model::BookGroup {
                    name: "default的分组".into(),
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(g2.id > 0 && g2.id != g.id);
        cleanup(storage, "grpns").await;
    }

    /// 跨用户 save_replace_rule：他人 id → 改插新 id（不覆写他人规则）
    #[tokio::test]
    async fn test_save_replace_rule_cross_ns_new_id() {
        let storage = test_storage("rulens").await;
        storage
            .save_replace_rule(
                "alice",
                &crate::model::ReplaceRule {
                    id: "rule-shared".into(),
                    name: "alice规则".into(),
                    find: "广告".into(),
                    replace: "".into(),
                    enabled: true,
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // default 用同 id 保存 → 成功但换新 id，alice 规则原样
        storage
            .save_replace_rule(
                "default",
                &crate::model::ReplaceRule {
                    id: "rule-shared".into(),
                    name: "default规则".into(),
                    find: "弹窗".into(),
                    replace: "".into(),
                    enabled: true,
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let alice_rules = storage.get_replace_rules("alice").await.unwrap();
        assert_eq!(alice_rules.len(), 1);
        assert_eq!(alice_rules[0].id, "rule-shared", "他人规则不应被覆写");
        assert_eq!(alice_rules[0].name, "alice规则");
        let default_rules = storage.get_replace_rules("default").await.unwrap();
        assert_eq!(default_rules.len(), 1);
        assert_ne!(default_rules[0].id, "rule-shared", "应改插新 id");
        assert_eq!(default_rules[0].name, "default规则");
        // 本人同 id 覆盖 → 保持原 id
        storage
            .save_replace_rule(
                "alice",
                &crate::model::ReplaceRule {
                    id: "rule-shared".into(),
                    name: "alice规则v2".into(),
                    find: "广告".into(),
                    replace: "".into(),
                    enabled: true,
                    order: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let alice_rules = storage.get_replace_rules("alice").await.unwrap();
        assert_eq!(alice_rules.len(), 1);
        assert_eq!(alice_rules[0].id, "rule-shared");
        assert_eq!(alice_rules[0].name, "alice规则v2");
        cleanup(storage, "rulens").await;
    }

    /// 跨用户 save_replace_rules 批量：同样改插新 id
    #[tokio::test]
    async fn test_save_replace_rules_batch_cross_ns_new_id() {
        let storage = test_storage("rulesns").await;
        storage
            .save_replace_rules(
                "alice",
                &[
                    crate::model::ReplaceRule {
                        id: "r1".into(),
                        name: "A1".into(),
                        find: "f1".into(),
                        ..Default::default()
                    },
                    crate::model::ReplaceRule {
                        id: "r2".into(),
                        name: "A2".into(),
                        find: "f2".into(),
                        ..Default::default()
                    },
                ],
            )
            .await
            .unwrap();
        storage
            .save_replace_rules(
                "default",
                &[
                    crate::model::ReplaceRule {
                        id: "r1".into(),
                        name: "D1".into(),
                        find: "f1".into(),
                        ..Default::default()
                    },
                    crate::model::ReplaceRule {
                        id: "r3".into(),
                        name: "D3".into(),
                        find: "f3".into(),
                        ..Default::default()
                    },
                ],
            )
            .await
            .unwrap();
        let alice = storage.get_replace_rules("alice").await.unwrap();
        assert_eq!(alice.len(), 2);
        assert!(alice.iter().all(|r| r.id == "r1" || r.id == "r2"));
        let default = storage.get_replace_rules("default").await.unwrap();
        assert_eq!(default.len(), 2);
        assert!(default.iter().any(|r| r.id == "r3"), "r3 新 id 保留");
        assert!(
            default.iter().all(|r| r.id != "r1"),
            "r1 应改插新 id（不覆写 alice）"
        );
        cleanup(storage, "rulesns").await;
    }

    /// 跨用户 save_txt_toc_rule：他人 id → 改插新 id
    #[tokio::test]
    async fn test_save_txt_toc_rule_cross_ns_new_id() {
        let storage = test_storage("tocns").await;
        storage
            .save_txt_toc_rule(
                "alice",
                &crate::model::TxtTocRule {
                    id: "toc-1".into(),
                    name: "alice目录规则".into(),
                    rule: "^第.*章$".into(),
                    enable: true,
                    serial_number: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_txt_toc_rule(
                "default",
                &crate::model::TxtTocRule {
                    id: "toc-1".into(),
                    name: "default目录规则".into(),
                    rule: "^第.*节$".into(),
                    enable: true,
                    serial_number: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let alice = storage.get_txt_toc_rules("alice").await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].id, "toc-1");
        assert_eq!(alice[0].name, "alice目录规则");
        let default = storage.get_txt_toc_rules("default").await.unwrap();
        assert_eq!(default.len(), 1);
        assert_ne!(default[0].id, "toc-1", "应改插新 id");
        assert_eq!(default[0].name, "default目录规则");
        cleanup(storage, "tocns").await;
    }

    // ---------------- P1-C4：book_limit 辅助 ----------------

    #[tokio::test]
    async fn test_book_limit_for_and_count() {
        let storage = test_storage("bklimit").await;
        // 无用户行 → None（非 secure 模式不限制）
        assert_eq!(storage.book_limit_for("default").await.unwrap(), None);
        assert_eq!(storage.count_books_for_user("default").await.unwrap(), 0);
        // 建用户 + 书
        storage
            .insert_user(&crate::model::User {
                username: "alice".into(),
                book_limit: 3,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(storage.book_limit_for("alice").await.unwrap(), Some(3));
        for i in 0..2 {
            storage
                .upsert_book(
                    "alice",
                    &crate::model::Book {
                        book_url: format!("https://b{i}.com"),
                        name: format!("书{i}"),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        assert_eq!(storage.count_books_for_user("alice").await.unwrap(), 2);
        assert_eq!(storage.count_books_for_user("default").await.unwrap(), 0);
        cleanup(storage, "bklimit").await;
    }

    // ---------------- 缓存容量上限淘汰（insert_capped_evict_oldest） ----------------

    /// `t` 秒前的时刻（构造「谁更旧」可控的输入，避免依赖真实时钟精度）
    fn at(secs_ago: u64) -> std::time::Instant {
        std::time::Instant::now() - std::time::Duration::from_secs(secs_ago)
    }

    type CapMap = std::collections::HashMap<String, (std::time::Instant, u32)>;

    /// 未满不淘汰 / 满了插新淘汰最旧 / 重复 key 幂等 / 总数恰好等于上限 /
    /// 被淘汰的是最旧的且最近写的还在 / max=0 与空表不 panic。
    #[test]
    fn insert_capped_evicts_at_capacity() {
        // 1) 未满：不淘汰，返回 None
        let mut m: CapMap = std::collections::HashMap::new();
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "a".into(), 1, 3, at(10)),
            None
        );
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "b".into(), 2, 3, at(9)),
            None
        );
        assert_eq!(m.len(), 2, "未到上限时不该淘汰");

        // 2) 重复插入同一 key：幂等——不增长、不淘汰、值被覆盖
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "a".into(), 11, 3, at(8)),
            None,
            "覆盖已有 key 不该淘汰任何条目"
        );
        assert_eq!(m.len(), 2, "重复 key 不该让总数增长");
        assert_eq!(m.get("a").map(|(_, v)| *v), Some(11), "重复 key 应覆盖旧值");

        // 3) 填满到上限：仍不淘汰
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "c".into(), 3, 3, at(7)),
            None
        );
        assert_eq!(m.len(), 3);

        // 4) 满时插新：淘汰最旧的 "b"（at(9) 是现存里最旧），总数恰好等于上限
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "d".into(), 4, 3, at(0)),
            Some("b".to_string()),
            "应淘汰写入时刻最旧的那条"
        );
        assert_eq!(m.len(), 3, "淘汰后总数必须恰好等于上限");
        assert!(!m.contains_key("b"), "最旧的应被淘汰");
        assert!(m.contains_key("a") && m.contains_key("c"), "其余条目应保留");
        assert_eq!(m.get("d").map(|(_, v)| *v), Some(4), "新写的必须还在");
        // 最近写的（"d"）不会是淘汰对象：再插一次，被淘汰的应是当前最旧的 "a"(at(8))
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "e".into(), 5, 3, at(0)),
            Some("a".to_string())
        );
        assert!(m.contains_key("d"), "最近写的条目不该被下一轮淘汰");
        assert_eq!(m.len(), 3);

        // 5) 边界：max = 0 → 不插入、不 panic（保持空表，也不淘汰已有内容之外的任何东西）
        let mut zero: CapMap = std::collections::HashMap::new();
        assert_eq!(
            insert_capped_evict_oldest(&mut zero, "a".into(), 1, 0, at(0)),
            None
        );
        assert!(zero.is_empty(), "max=0 时不该插入");
        // 空表 + max=1：正常插入，不淘汰
        let mut one: CapMap = std::collections::HashMap::new();
        assert_eq!(
            insert_capped_evict_oldest(&mut one, "a".into(), 1, 1, at(1)),
            None
        );
        assert_eq!(one.len(), 1);
        // 单条上限再插 → 淘汰唯一一条，长度仍是 1
        assert_eq!(
            insert_capped_evict_oldest(&mut one, "b".into(), 2, 1, at(0)),
            Some("a".to_string())
        );
        assert_eq!(one.len(), 1);
    }

    /// 同一时刻连续插入（`Instant` 相同）时淘汰结果可复现：按 key 字典序定胜负，
    /// 且总数仍恰好等于上限（真实调用点在同一毫秒内连插多条就会走到这里）。
    #[test]
    fn insert_capped_same_instant_is_deterministic() {
        let now = std::time::Instant::now();
        let mut m: CapMap = std::collections::HashMap::new();
        insert_capped_evict_oldest(&mut m, "k1".into(), 1, 2, now);
        insert_capped_evict_oldest(&mut m, "k2".into(), 2, 2, now);
        assert_eq!(
            insert_capped_evict_oldest(&mut m, "k3".into(), 3, 2, now),
            Some("k1".to_string()),
            "同刻插入按 key 定序：k1 < k2，淘汰 k1"
        );
        assert!(m.contains_key("k2") && m.contains_key("k3"));
        assert_eq!(m.len(), 2);
    }

    /// 超额历史状态（len > max）一次淘汰到位，不留 len > max；
    /// 且上限被调小时也只会收敛到 max，不会 panic。
    #[test]
    fn insert_capped_shrinks_overfull_map() {
        let mut m: CapMap = std::collections::HashMap::new();
        for i in 0..5 {
            m.insert(format!("old{i}"), (at(100 - i), i as u32));
        }
        assert_eq!(m.len(), 5);
        // max=2：插新键时循环淘汰，直到 len < 2 再插入 → 最终恰好 2 条
        insert_capped_evict_oldest(&mut m, "new".into(), 99, 2, at(0));
        assert_eq!(m.len(), 2, "超额表应一次收敛到上限");
        assert!(m.contains_key("new"));
        assert!(m.contains_key("old4"), "最近写的旧条目应留下");
    }

    // ---------------- 三态记账（SearchOutcome） ----------------

    /// 取单源统计行（不存在则 panic，便于断言）
    async fn stat_of(storage: &Storage, ns: &str, url: &str) -> crate::model::SourceStat {
        storage
            .get_source_stats(ns)
            .await
            .unwrap()
            .into_iter()
            .find(|st| st.source_url == url)
            .unwrap_or_else(|| panic!("{url} 应有统计行"))
    }

    /// Ok / Fail / Timeout 三种 outcome 的 SQL 结果。
    ///
    /// 关键断言（本轮改造的全部意义）：**Timeout 不 ++ `consec_failures`，但必须写
    /// `last_failure_at`**——前者防止把慢源误判成死源，后者是「死源 6h 豁免」的判据。
    #[tokio::test]
    async fn test_record_search_outcome_three_states() {
        let storage = test_storage("outcome3").await;
        let ns = "default";
        let url = "https://three-state.test";

        // ① Ok：attempts+1 / successes+1 / consec_failures 归零 / 不动 last_failure_at
        storage
            .record_search_outcome_state(ns, url, SearchOutcome::Ok, 1200, 3, 5)
            .await
            .unwrap();
        let st = stat_of(&storage, ns, url).await;
        assert_eq!(
            (st.attempts, st.successes, st.timeouts, st.consec_failures),
            (1, 1, 0, 0)
        );
        assert_eq!(st.last_failure_at, 0, "Ok 不写 last_failure_at");
        assert_eq!((st.intro_hits, st.intro_count), (3, 5));
        assert_eq!(st.lat_sum_ms, 1200);

        // ② Fail：attempts+1 / consec_failures+1 / last_failure_at=now_ms，不动 successes
        let before = chrono::Utc::now().timestamp_millis();
        storage
            .record_search_outcome_state(ns, url, SearchOutcome::Fail, 800, 0, 0)
            .await
            .unwrap();
        let st = stat_of(&storage, ns, url).await;
        assert_eq!(
            (st.attempts, st.successes, st.timeouts, st.consec_failures),
            (2, 1, 0, 1)
        );
        assert!(st.last_failure_at >= before, "Fail 必须写 last_failure_at");
        assert!(
            st.last_failure_at > 1_000_000_000_000,
            "last_failure_at 必须是毫秒（秒级时间戳 ~1e9，毫秒 ~1.7e12）"
        );

        // ③ Timeout：attempts+1 / timeouts+1 / successes 不变 /
        //    **consec_failures 不动** / last_failure_at 必须写 now_ms
        let before = chrono::Utc::now().timestamp_millis();
        storage
            .record_search_outcome_state(ns, url, SearchOutcome::Timeout, 15_000, 0, 0)
            .await
            .unwrap();
        let st = stat_of(&storage, ns, url).await;
        assert_eq!(
            (st.attempts, st.successes, st.timeouts, st.consec_failures),
            (3, 1, 1, 1),
            "Timeout: attempts/timeouts +1，successes 与 consec_failures 都不变"
        );
        assert!(
            st.last_failure_at >= before,
            "Timeout 也必须写 last_failure_at（6h 死源豁免靠它）"
        );
    }

    /// 连续超时不累积连败：`consec_failures` 一直不涨，因此
    /// `confidence()` 不会被压到 0.05，源也不会被「死源 6h 跳过」滤掉。
    ///
    /// 改造前这里是：3 次超时 = 3 次失败 → `consec_failures=3` → confidence 0.05 → 6h 不搜。
    #[tokio::test]
    async fn test_timeout_does_not_poison_consec_failures() {
        let storage = test_storage("outcome-timeout").await;
        let ns = "default";
        let url = "https://slow-but-alive.test";

        // 先成功一次打底，再连续 5 次超时
        storage
            .record_search_outcome_state(ns, url, SearchOutcome::Ok, 900, 1, 1)
            .await
            .unwrap();
        for _ in 0..5 {
            storage
                .record_search_outcome_state(ns, url, SearchOutcome::Timeout, 15_000, 0, 0)
                .await
                .unwrap();
        }
        let st = stat_of(&storage, ns, url).await;
        assert_eq!(st.attempts, 6);
        assert_eq!(st.successes, 1);
        assert_eq!(st.timeouts, 5);
        assert_eq!(st.consec_failures, 0, "超时不计连败");

        // 对照：一次真失败才 +1
        storage
            .record_search_outcome_state(ns, url, SearchOutcome::Fail, 100, 0, 0)
            .await
            .unwrap();
        let st = stat_of(&storage, ns, url).await;
        assert_eq!(st.consec_failures, 1, "只有 Fail 计连败");
        // 再成功一次归零
        storage
            .record_search_outcome_state(ns, url, SearchOutcome::Ok, 100, 1, 1)
            .await
            .unwrap();
        let st = stat_of(&storage, ns, url).await;
        assert_eq!(st.consec_failures, 0, "Ok 清零连败");
    }

    /// 向后兼容入口：老 `bool` 签名仍然编译且行为不变（true→Ok / false→Fail）
    #[tokio::test]
    async fn test_record_search_outcome_bool_compat() {
        let storage = test_storage("outcome-bool").await;
        let ns = "default";
        let url = "https://bool-compat.test";
        storage
            .record_search_outcome(ns, url, true, 500, 0, 0)
            .await
            .unwrap();
        storage
            .record_search_outcome(ns, url, false, 500, 0, 0)
            .await
            .unwrap();
        let st = stat_of(&storage, ns, url).await;
        assert_eq!((st.attempts, st.successes, st.timeouts), (2, 1, 0));
        assert_eq!(st.consec_failures, 1, "bool=false 仍记 Fail");
        assert!(st.last_failure_at > 0);
        // bool 入口表达不了超时：from_bool 只有 Ok/Fail
        assert_eq!(SearchOutcome::from_bool(true), SearchOutcome::Ok);
        assert_eq!(SearchOutcome::from_bool(false), SearchOutcome::Fail);
    }

    /// 迁移幂等：对**已有生产库形态**（source_stats 无 timeouts 列）补列——
    /// 连跑两次不报错、列不重复加、老数据不丢、默认值 0。
    #[tokio::test]
    async fn test_source_stats_timeouts_migration_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "reader-storage-migrate-{}-timeouts",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("legacy.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        // 旧生产库的表结构：没有 timeouts 列
        sqlx::query(
            "CREATE TABLE source_stats (
                user_namespace TEXT NOT NULL,
                source_url TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                successes INTEGER NOT NULL DEFAULT 0,
                lat_sum_ms INTEGER NOT NULL DEFAULT 0,
                intro_hits INTEGER NOT NULL DEFAULT 0,
                intro_count INTEGER NOT NULL DEFAULT 0,
                toc_attempts INTEGER NOT NULL DEFAULT 0,
                toc_successes INTEGER NOT NULL DEFAULT 0,
                content_attempts INTEGER NOT NULL DEFAULT 0,
                content_successes INTEGER NOT NULL DEFAULT 0,
                consec_failures INTEGER NOT NULL DEFAULT 0,
                last_failure_at INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_namespace, source_url)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO source_stats (user_namespace, source_url, attempts, successes, consec_failures)
             VALUES ('default', 'https://legacy.test', 7, 5, 2)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let storage = Storage {
            pool: pool.clone(),
            config: AppConfig::from_env(),
            cache_tag: 0,
        };
        // 第一次补列；第二次（模拟服务再次启动）必须是 no-op
        storage.ensure_source_stats_table().await.unwrap();
        storage.ensure_source_stats_table().await.unwrap();

        // 列恰好一列，不重复
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('source_stats') WHERE name = 'timeouts'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(cols.len(), 1, "timeouts 列应恰好一列");
        // 老数据不丢，新列默认 0
        let stats = storage.get_source_stats("default").await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].attempts, 7);
        assert_eq!(stats[0].successes, 5);
        assert_eq!(stats[0].consec_failures, 2);
        assert_eq!(stats[0].timeouts, 0);
        // 补列后照常能记账
        storage
            .record_search_outcome_state(
                "default",
                "https://legacy.test",
                SearchOutcome::Timeout,
                15_000,
                0,
                0,
            )
            .await
            .unwrap();
        let stats = storage.get_source_stats("default").await.unwrap();
        assert_eq!((stats[0].attempts, stats[0].timeouts), (8, 1));

        // 全新建库（init 路径）同样带列且幂等
        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
