//! 纯净 SQLite Schema 与高频索引初始化（无历史兼容迁移补丁）

use anyhow::Result;
use sqlx::SqlitePool;

/// 建表与索引（幂等 `CREATE TABLE/INDEX IF NOT EXISTS`），并尝试初始化 FTS5 `trigram` 索引。
/// 返回 `true` 表示当前 SQLite 运行环境已就绪 FTS5 `trigram` 倒排索引。
pub(crate) async fn init_schema(pool: &SqlitePool) -> Result<bool> {
    const STATEMENTS: &[&str] = &[
        // ---------------- 用户与系统配置 ----------------
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
            enable_rss_source INTEGER DEFAULT 0,
            book_source_limit INTEGER DEFAULT 0,
            book_limit INTEGER DEFAULT 0,
            is_admin INTEGER DEFAULT 0,
            disabled INTEGER DEFAULT 0,
            last_login_at INTEGER DEFAULT 0,
            created_at INTEGER DEFAULT 0,
            user_namespace TEXT DEFAULT ''
        );
        "#,
        r#"
        CREATE TABLE IF NOT EXISTS system_settings (
            key TEXT PRIMARY KEY,
            value TEXT,
            updated_at INTEGER DEFAULT 0
        );
        "#,
        r#"
        CREATE TABLE IF NOT EXISTS user_config (
            user_namespace TEXT NOT NULL,
            ns TEXT NOT NULL,
            config TEXT,
            updated_at INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, ns)
        );
        "#,
        // ---------------- 书籍、章节、分组与书签 ----------------
        r#"
        CREATE TABLE IF NOT EXISTS books (
            book_url TEXT NOT NULL,
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
            group_ids TEXT NOT NULL DEFAULT '',
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
            display_cover TEXT,
            display_intro TEXT,
            local_epub INTEGER DEFAULT 0,
            split_long_chapter INTEGER DEFAULT 0,
            last_check_error TEXT,
            info_html TEXT,
            toc_html TEXT,
            language TEXT,
            publisher TEXT,
            published_at TEXT,
            user_namespace TEXT NOT NULL DEFAULT '',
            created_at INTEGER DEFAULT 0,
            local_file TEXT,
            local_file_mtime INTEGER DEFAULT 0,
            local_file_size INTEGER DEFAULT 0,
            local_file_deleted INTEGER DEFAULT 0,
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
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
            user_namespace TEXT NOT NULL DEFAULT '',
            raw_json TEXT,
            PRIMARY KEY (book_url, title, user_namespace)
        );
        "#,
        r#"
        CREATE TABLE IF NOT EXISTS book_source_candidates (
            user_namespace TEXT NOT NULL,
            book_key TEXT NOT NULL,
            candidates_json TEXT NOT NULL DEFAULT '[]',
            updated_at INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, book_key)
        );
        "#,
        // ---------------- 书源、统计与登录态 Cookie ----------------
        r#"
        CREATE TABLE IF NOT EXISTS book_sources (
            book_source_url TEXT NOT NULL,
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
            user_namespace TEXT NOT NULL DEFAULT '',
            hidden INTEGER DEFAULT 0,
            raw_json TEXT,
            PRIMARY KEY (book_source_url, user_namespace)
        );
        "#,
        r#"
        CREATE TABLE IF NOT EXISTS source_stats (
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
        );
        "#,
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
        // ---------------- 缓存与脚本持久化 ----------------
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
        r#"
        CREATE TABLE IF NOT EXISTS book_info_cache (
            book_url TEXT NOT NULL,
            user_namespace TEXT NOT NULL,
            info_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (book_url, user_namespace)
        );
        "#,
        r#"
        CREATE TABLE IF NOT EXISTS js_cache (
            user_namespace TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            expiry INTEGER DEFAULT 0,
            PRIMARY KEY (user_namespace, key)
        );
        "#,
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
        r#"
        CREATE TABLE IF NOT EXISTS search_cache (
            user_namespace TEXT NOT NULL,
            cache_key TEXT NOT NULL,
            result_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (user_namespace, cache_key)
        );
        "#,
        // ---------------- 净化规则 / HttpTTS / TXT 目录规则 ----------------
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
        r#"
        CREATE TABLE IF NOT EXISTS http_tts_list (
            url TEXT NOT NULL,
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
            user_namespace TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (url, user_namespace)
        );
        "#,
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
        // ---------------- 高频查询索引 ----------------
        "CREATE INDEX IF NOT EXISTS idx_books_ns ON books(user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_books_ns_name_author ON books(user_namespace, name, author);",
        "CREATE INDEX IF NOT EXISTS idx_books_can_update ON books(can_update) WHERE can_update = 1;",
        "CREATE INDEX IF NOT EXISTS idx_book_chapters_book_ns ON book_chapters(book_url, user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_book_chapters_ns ON book_chapters(user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_book_sources_ns_enabled ON book_sources(user_namespace, enabled, hidden);",
        "CREATE INDEX IF NOT EXISTS idx_bookmarks_book_ns ON bookmarks(book_url, user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_book_groups_ns_order ON book_groups(user_namespace, order_num, id);",
        "CREATE INDEX IF NOT EXISTS idx_replace_rules_ns_enable ON replace_rules(user_namespace, enable, order_num);",
        "CREATE INDEX IF NOT EXISTS idx_http_tts_ns ON http_tts_list(user_namespace, name);",
        "CREATE INDEX IF NOT EXISTS idx_txt_toc_rules_ns ON txt_toc_rules(user_namespace, serial_number, id);",
        "CREATE INDEX IF NOT EXISTS idx_toc_cache_url ON toc_cache(toc_url, user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_toc_cache_ns ON toc_cache(user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_book_vars_ns_url ON book_vars_cache(user_namespace, url);",
        "CREATE INDEX IF NOT EXISTS idx_js_cache_expiry ON js_cache(expiry);",
        "CREATE INDEX IF NOT EXISTS idx_source_stats_ns ON source_stats(user_namespace);",
        "CREATE INDEX IF NOT EXISTS idx_cookies_ns ON book_source_cookies(user_namespace, updated_at DESC);",
    ];

    for stmt in STATEMENTS {
        sqlx::query(stmt).execute(pool).await?;
    }
    Ok(init_book_chapters_fts(pool).await)
}

/// 尝试创建轻量 FTS5 `trigram` 外部内容表与同步触发器；
/// 存量库中 `book_chapters` 非空且 FTS 索引为空时一次性自动回填。
/// 运行环境不支持 `fts5` / `trigram` 时静默降级返回 `false`，不影响核心启动。
async fn init_book_chapters_fts(pool: &SqlitePool) -> bool {
    const FTS_STATEMENTS: &[&str] = &[
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS book_chapters_fts USING fts5(
            book_url UNINDEXED,
            user_namespace UNINDEXED,
            chapter_index UNINDEXED,
            title UNINDEXED,
            content,
            content='book_chapters',
            content_rowid='rowid',
            tokenize='trigram'
        );
        "#,
        r#"
        CREATE TRIGGER IF NOT EXISTS book_chapters_fts_ai AFTER INSERT ON book_chapters BEGIN
            INSERT INTO book_chapters_fts(rowid, book_url, user_namespace, chapter_index, title, content)
            VALUES (new.rowid, new.book_url, new.user_namespace, new.chapter_index, new.title, new.content);
        END;
        "#,
        r#"
        CREATE TRIGGER IF NOT EXISTS book_chapters_fts_ad AFTER DELETE ON book_chapters BEGIN
            INSERT INTO book_chapters_fts(book_chapters_fts, rowid, book_url, user_namespace, chapter_index, title, content)
            VALUES ('delete', old.rowid, old.book_url, old.user_namespace, old.chapter_index, old.title, old.content);
        END;
        "#,
        r#"
        CREATE TRIGGER IF NOT EXISTS book_chapters_fts_au AFTER UPDATE ON book_chapters BEGIN
            INSERT INTO book_chapters_fts(book_chapters_fts, rowid, book_url, user_namespace, chapter_index, title, content)
            VALUES ('delete', old.rowid, old.book_url, old.user_namespace, old.chapter_index, old.title, old.content);
            INSERT INTO book_chapters_fts(rowid, book_url, user_namespace, chapter_index, title, content)
            VALUES (new.rowid, new.book_url, new.user_namespace, new.chapter_index, new.title, new.content);
        END;
        "#,
    ];

    for stmt in FTS_STATEMENTS {
        if let Err(e) = sqlx::query(stmt).execute(pool).await {
            tracing::warn!("FTS5 trigram 索引初始化失败，全书搜索将降级为流式 LIKE: {e}");
            return false;
        }
    }

    let has_chapters = sqlx::query_scalar::<_, i64>("SELECT EXISTS(SELECT 1 FROM book_chapters)")
        .fetch_one(pool)
        .await
        .unwrap_or(0)
        != 0;
    if has_chapters {
        let fts_has_rows =
            sqlx::query_scalar::<_, i64>("SELECT EXISTS(SELECT 1 FROM book_chapters_fts_docsize)")
                .fetch_one(pool)
                .await
                .unwrap_or(0)
                != 0;
        if !fts_has_rows {
            if let Err(e) = sqlx::query(
                "INSERT INTO book_chapters_fts(rowid, book_url, user_namespace, chapter_index, title, content) \
                 SELECT rowid, book_url, user_namespace, chapter_index, title, content FROM book_chapters",
            )
            .execute(pool)
            .await
            {
                tracing::warn!("FTS5 trigram 存量章节回填失败，降级为流式 LIKE: {e}");
                return false;
            }
            tracing::info!("FTS5 trigram 存量章节索引回填完成");
        }
    }
    true
}
