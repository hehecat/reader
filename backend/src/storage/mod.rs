//! 存储层：SQLite（模块化拆分：schema / users / books / sources / cache / misc）

mod books;
mod cache;
mod misc;
mod schema;
mod sources;
mod users;

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::AppConfig;

/// 失效 cookie 快照与全部命名空间级热缓存（用户删除等跨表清理路径调用）
pub(crate) fn invalidate_cookie_snapshots() {
    sources::invalidate_cookie_snapshots_only();
    sources::invalidate_all_book_sources_cache();
    misc::invalidate_all_replace_rules_cache();
    books::invalidate_all_chapter_content_cache();
    cache::invalidate_all_toc_cache();
}

/// 轻量 LRU 缓存（用于 Storage 进程内热窗口缓存；配合 `parking_lot::Mutex` 使用）
pub(super) struct LruCache<K, V> {
    cap: usize,
    tick: u64,
    map: std::collections::HashMap<K, (u64, V)>,
}

impl<K: Eq + std::hash::Hash + Clone, V> LruCache<K, V> {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            cap,
            tick: 0,
            map: std::collections::HashMap::with_capacity(cap.min(64)),
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    pub(super) fn get(&mut self, key: &K) -> Option<&V> {
        let tick = self.next_tick();
        let entry = self.map.get_mut(key)?;
        entry.0 = tick;
        Some(&entry.1)
    }

    pub(super) fn insert(&mut self, key: K, value: V) {
        if self.cap == 0 {
            return;
        }
        let tick = self.next_tick();
        if let Some(entry) = self.map.get_mut(&key) {
            entry.0 = tick;
            entry.1 = value;
            return;
        }
        while self.map.len() >= self.cap {
            let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.map.remove(&victim);
        }
        self.map.insert(key, (tick, value));
    }

    pub(super) fn retain(&mut self, mut f: impl FnMut(&K, &V) -> bool) {
        self.map.retain(|k, (_, v)| f(k, v));
    }

    pub(super) fn clear(&mut self) {
        self.map.clear();
    }
}

/// 单次搜索的结论三态（`source_stats` 的记账口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOutcome {
    /// 有结论的成功（拿到响应/解析出结果）
    Ok,
    /// 真失败：网络错误、解析失败、HTTP 业务错误
    Fail,
    /// 超时 / 被窗硬顶截断：不给 `consec_failures` 定罪，但记 `last_failure_at`
    Timeout,
}

impl SearchOutcome {
    pub fn from_bool(ok: bool) -> Self {
        if ok {
            Self::Ok
        } else {
            Self::Fail
        }
    }

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
    /// 实例标识：cookie 快照缓存按「实例 + 命名空间」分桶
    pub(super) cache_tag: u64,
    /// 当前 SQLite 环境是否已就绪 FTS5 `trigram` 倒排索引
    pub fts_trigram_ready: bool,
}

static STORAGE_INSTANCE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

tokio::task_local! {
    static TASK_STORAGE: Storage;
}

/// 将 `Storage` 绑定到当前异步请求/任务上下文（零全局状态依赖注入）
pub async fn scope_task_storage<F: std::future::Future>(storage: Storage, fut: F) -> F::Output {
    TASK_STORAGE.scope(storage, fut).await
}

/// 获取当前异步任务绑定的 `Storage`（未处于请求作用域时返回 `None`，供单测兜底）
pub fn current_task_storage() -> Option<Storage> {
    TASK_STORAGE.try_with(Clone::clone).ok()
}

/// 缓存统计（getCacheInfo）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    pub toc_cache_count: i64,
    pub toc_cache_size: i64,
    pub chapter_count: i64,
    pub chapter_size: i64,
    pub total_size: i64,
}

/// 全书搜索命中（searchBookContent）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookContentHit {
    pub chapter_index: i64,
    pub title: String,
    pub snippet: String,
}

/// 各类目恢复计数（restored/skipped 共用）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreCounts {
    pub sources: u64,
    pub books: u64,
    pub groups: u64,
    pub rules: u64,
    #[serde(rename = "txtRules")]
    pub txt_rules: u64,
    pub config: u64,
    pub tts: u64,
    pub bookmarks: u64,
    pub cookies: u64,
}

/// 恢复报告（restoreFromWebdav 返回）
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub restored: RestoreCounts,
    pub skipped: RestoreCounts,
}

/// 连接池上限（env `READER_DB_MAX_CONNECTIONS`，默认 24；WAL 模式下读可高并发，避免 SSE 搜书期间挤占登录/读请求）
fn max_connections() -> u32 {
    std::env::var("READER_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(24)
        .clamp(1, 64)
}

/// 初始化：建目录 + 打开/建库（WAL + foreign_keys + 16MB 页缓存）+ 纯净建表与索引
pub async fn init(config: &AppConfig) -> Result<Storage> {
    let storage_dir = config.storage_dir();
    tokio::fs::create_dir_all(&storage_dir).await?;
    let db_path = storage_dir.join("reader.db");

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(15))
        .foreign_keys(true)
        .pragma("cache_size", "-16384")
        .pragma("recursive_triggers", "ON")
        .statement_cache_capacity(0);

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections())
        .connect_with(options)
        .await?;

    let fts_trigram_ready = schema::init_schema(&pool).await?;

    tracing::info!(
        "storage initialized at {} (fts5_trigram={fts_trigram_ready})",
        db_path.display()
    );

    let storage = Storage {
        pool,
        config: config.clone(),
        cache_tag: STORAGE_INSTANCE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        fts_trigram_ready,
    };
    if let Err(e) = storage.ensure_admin_user().await {
        tracing::warn!("初始化管理员兜底失败: {e:#}");
    }
    Ok(storage)
}

/// WAL 模式下立即获取写锁的事务守卫（`BEGIN IMMEDIATE`）：
/// 避免 `BEGIN DEFERRED` 在读快照后升级写锁触发 SQLite `SQLITE_BUSY_SNAPSHOT (517)` 忽略 `busy_timeout`。
pub(super) struct ImmediateTx {
    conn: Option<sqlx::pool::PoolConnection<sqlx::Sqlite>>,
}

impl std::ops::Deref for ImmediateTx {
    type Target = sqlx::SqliteConnection;

    fn deref(&self) -> &Self::Target {
        self.conn.as_deref().expect("ImmediateTx connection active")
    }
}

impl std::ops::DerefMut for ImmediateTx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn
            .as_deref_mut()
            .expect("ImmediateTx connection active")
    }
}

impl ImmediateTx {
    pub(super) async fn commit(mut self) -> Result<()> {
        if let Some(conn) = self.conn.as_deref_mut() {
            sqlx::query("COMMIT").execute(conn).await?;
        }
        // 仅在 COMMIT 明确成功后归还连接池；若 COMMIT 失败或 Future 被取消，Drop 会关闭底层连接并释放写锁
        let _ = self.conn.take();
        Ok(())
    }
}

impl Drop for ImmediateTx {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // 未提交提前退出时直接关闭底层连接，SQLite 自动回滚未结事务且不污染连接池
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = conn.close().await;
                });
            }
        }
    }
}

impl Storage {
    /// 开启 `BEGIN IMMEDIATE` 写事务，遵循连接级 `busy_timeout` 排队等待而不抛 517 快照冲突
    pub(super) async fn begin_immediate(&self) -> Result<ImmediateTx> {
        let conn = self.pool.acquire().await?;
        // 先套入守卫再执行 BEGIN IMMEDIATE，确保若在 await 期间被取消也不会把带未结事务的连接放回连接池
        let mut tx = ImmediateTx { conn: Some(conn) };
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *tx).await?;
        Ok(tx)
    }
}

/// 命中摘要：定位 key 首次出现位置（大小写不敏感），取所在段落 + 前后各 radius 字符
pub(super) fn make_snippet(content: &str, key: &str, radius: usize) -> String {
    let lower = content.to_lowercase();
    let key_lower = key.to_lowercase();
    let Some(pos) = lower.find(&key_lower) else {
        return String::new();
    };
    let pos = floor_char_boundary(content, pos);
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

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 规范化 baseUrl（scheme://host[:port]，去尾斜杠/路径/查询）
pub(crate) fn normalize_base(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
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

/// 目录解析结果是否可信（够格回写 `total_chapter_num` / `latest_chapter_title`）
pub(crate) fn plausible_chapter_total(parsed: i64, prev: i64, cached: i64) -> bool {
    parsed > 0 && (parsed >= prev || (cached > 0 && parsed >= cached))
}

/// 目录观测来源可信度
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TocWitness {
    Fetched,
    CachedToc,
}

/// 目录观测 → 该把 `total_chapter_num` 写成什么（`None` = 这次不写）
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

/// 扫描 books 表 can_update=1 的书 → analyze_toc → 回写 latest_chapter_title / total_chapter_num
pub async fn run_shelf_update(storage: &Storage) -> Result<usize> {
    let books = storage.list_updatable_books().await?;
    let mut updated = 0usize;
    for book in books {
        if book.origin == "local"
            || book.book_url.starts_with("local://")
            || book.book_url.ends_with(".txt")
        {
            continue;
        }
        let Ok(Some(source)) = storage
            .find_book_source(&book.user_namespace, &book.origin)
            .await
        else {
            continue;
        };
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
                        if total != book.total_chapter_num {
                            if let Err(e) = storage
                                .clear_toc_cache_for_book(&book.user_namespace, &book.book_url)
                                .await
                            {
                                tracing::warn!("失效目录缓存失败 [{}]: {e:#}", book.book_url);
                            }
                        }
                    }
                    Err(e) => tracing::warn!("书架更新回写失败 [{}]: {e:#}", book.book_url),
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("书架更新跳过 [{}]: {e:#}", book.book_url),
        }
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Book, BookGroup, BookSource, Bookmark, ReplaceRule, User};

    async fn test_storage(tag: &str) -> (Storage, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("reader-storage-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = init(&config).await.unwrap();
        (storage, dir)
    }

    async fn cleanup(storage: Storage, dir: std::path::PathBuf) {
        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn init_enables_wal_foreign_keys_and_indexes() {
        let (storage, dir) = test_storage("pragmas").await;
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(fk, 1);
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let idx_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%'",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert!(idx_count >= 15, "预期建立高频索引，实际 {idx_count}");
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn users_books_sources_and_backup_roundtrip() {
        let (storage, dir) = test_storage("roundtrip").await;
        storage
            .insert_user(&User {
                username: "alice".into(),
                password: "hash".into(),
                salt: "salt".into(),
                enable_book_source: true,
                book_limit: 500,
                book_source_limit: 1000,
                user_namespace: "alice".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage.ensure_admin_user().await.unwrap();
        assert_eq!(storage.count_admins().await.unwrap(), 1);

        let src = BookSource {
            book_source_url: "https://example.com".into(),
            book_source_name: "示例源".into(),
            enabled: true,
            ..Default::default()
        };
        storage.save_book_source("alice", &src).await.unwrap();
        assert_eq!(storage.get_book_sources("alice").await.unwrap().len(), 1);

        storage
            .upsert_book(
                "alice",
                &Book {
                    book_url: "https://example.com/b/1".into(),
                    name: "凡人".into(),
                    author: "忘语".into(),
                    origin: "https://example.com".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .save_chapters_ns(
                "alice",
                "https://example.com/b/1",
                &[("第一章".into(), "正文关键字内容".into())],
            )
            .await
            .unwrap();
        let hits = storage
            .search_book_content("alice", "https://example.com/b/1", "关键字", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("关键字"));

        storage
            .save_bookmark(
                "alice",
                &Bookmark {
                    book_url: "https://example.com/b/1".into(),
                    title: "摘录".into(),
                    content: "备注".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let grp = storage
            .save_book_group(
                "alice",
                &BookGroup {
                    name: "仙侠".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(grp.id > 0);

        storage
            .save_replace_rule(
                "alice",
                &ReplaceRule {
                    id: "r1".into(),
                    name: "去广告".into(),
                    find: "广告".into(),
                    replace: String::new(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let zip_path = storage.create_backup_zip("alice").await.unwrap();
        let zip_bytes = std::fs::read(&zip_path).unwrap();
        let report = storage
            .restore_backup_zip("bob", &zip_bytes, false)
            .await
            .unwrap();
        assert_eq!(report.restored.books, 1);
        assert_eq!(report.restored.sources, 1);
        assert_eq!(report.restored.bookmarks, 1);
        assert_eq!(report.restored.rules, 1);
        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn source_stats_three_state_and_cookie_snapshot() {
        let (storage, dir) = test_storage("stats-cookie").await;
        storage
            .record_search_outcome_state(
                "default",
                "https://s1.test",
                SearchOutcome::Timeout,
                5000,
                0,
                0,
            )
            .await
            .unwrap();
        storage
            .record_search_outcome_state(
                "default",
                "https://s1.test",
                SearchOutcome::Fail,
                100,
                0,
                0,
            )
            .await
            .unwrap();
        let stats = storage.get_source_stats("default").await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].timeouts, 1);
        assert_eq!(stats[0].consec_failures, 1);

        storage
            .set_cookie("default", "https://s1.test##https://s2.test", "sid=abc")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_cookie_by_base("default", "https://s2.test/chapter/1")
                .await
                .unwrap()
                .as_deref(),
            Some("sid=abc")
        );
        cleanup(storage, dir).await;
    }

    #[test]
    fn chapter_total_writeback_rules() {
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 0, 100, 50),
            None
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 40, 100, 80),
            None
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::Fetched, 120, 100, 80),
            Some(120)
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 80, 100, 0),
            None
        );
        assert_eq!(
            chapter_total_writeback(TocWitness::CachedToc, 120, 100, 0),
            Some(120)
        );
    }

    #[test]
    fn insert_capped_evicts_oldest() {
        let mut m = std::collections::HashMap::new();
        let t0 = std::time::Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(10);
        let t2 = t0 + std::time::Duration::from_millis(20);
        assert_eq!(
            sources::insert_capped_evict_oldest(&mut m, "a".into(), 1, 2, t0),
            None
        );
        assert_eq!(
            sources::insert_capped_evict_oldest(&mut m, "b".into(), 2, 2, t1),
            None
        );
        assert_eq!(
            sources::insert_capped_evict_oldest(&mut m, "c".into(), 3, 2, t2),
            Some("a".to_string())
        );
        assert_eq!(m.len(), 2);
        assert!(!m.contains_key("a"));
    }

    #[test]
    fn lru_cache_evicts_least_recently_used() {
        let mut lru: LruCache<&str, i32> = LruCache::new(2);
        lru.insert("a", 1);
        lru.insert("b", 2);
        // 访问 a 刷新活跃度 → 淘汰应选中 b
        assert_eq!(lru.get(&"a"), Some(&1));
        lru.insert("c", 3);
        assert_eq!(lru.get(&"a"), Some(&1));
        assert_eq!(lru.get(&"b"), None);
        assert_eq!(lru.get(&"c"), Some(&3));
    }

    #[tokio::test]
    async fn hot_caches_isolated_by_instance_and_invalidated_on_write() {
        let (st1, dir1) = test_storage("hot-cache-1").await;
        let (st2, dir2) = test_storage("hot-cache-2").await;

        // 1) 书源缓存：default 公共源对非 default 用户可见 + 实例间完全隔离 + 写穿透失效
        let src_a = BookSource {
            book_source_url: "https://src-a.test".into(),
            book_source_name: "源A".into(),
            enabled: true,
            ..Default::default()
        };
        st1.save_book_source("default", &src_a).await.unwrap();
        assert_eq!(st1.get_book_sources("alice").await.unwrap().len(), 1);
        assert_eq!(
            st1.get_book_source("alice", "https://src-a.test")
                .await
                .unwrap()
                .map(|s| s.book_source_name),
            Some("源A".to_string())
        );
        // 独立实例 st2 不受 st1 缓存串扰
        assert!(st2.get_book_sources("alice").await.unwrap().is_empty());
        // 修改 enabled / 删除后立即失效
        st1.update_book_source_enabled("alice", "https://src-a.test", false)
            .await
            .unwrap();
        assert!(
            !st1.get_book_source("alice", "https://src-a.test")
                .await
                .unwrap()
                .unwrap()
                .enabled
        );
        st1.delete_book_source("alice", "https://src-a.test")
            .await
            .unwrap();
        assert!(st1
            .get_book_source("alice", "https://src-a.test")
            .await
            .unwrap()
            .is_none());

        // 2) 净化规则缓存：default 回退 + 写入/删除失效 + 实例隔离
        let rule = ReplaceRule {
            id: "r-hot".into(),
            name: "去广告".into(),
            find: "AD".into(),
            replace: String::new(),
            ..Default::default()
        };
        st1.save_replace_rule("default", &rule).await.unwrap();
        assert_eq!(st1.get_replace_rules("alice").await.unwrap().len(), 1);
        assert!(st2.get_replace_rules("alice").await.unwrap().is_empty());
        st1.delete_replace_rule("default", "r-hot").await.unwrap();
        assert!(st1.get_replace_rules("alice").await.unwrap().is_empty());

        // 3) 章节正文与目录热窗口 LRU：写入、命中、更新与清理一致性
        st1.cache_chapter_content("alice", "https://b.test/1", 0, "第一章", "内容V1")
            .await
            .unwrap();
        assert_eq!(
            st1.get_chapter_content("alice", "https://b.test/1", 0)
                .await
                .unwrap()
                .as_deref(),
            Some("内容V1")
        );
        assert!(st2
            .get_chapter_content("alice", "https://b.test/1", 0)
            .await
            .unwrap()
            .is_none());
        st1.cache_chapter_content("alice", "https://b.test/1", 0, "第一章", "内容V2")
            .await
            .unwrap();
        assert_eq!(
            st1.get_chapter_content("alice", "https://b.test/1", 0)
                .await
                .unwrap()
                .as_deref(),
            Some("内容V2")
        );
        st1.delete_book_cache("alice", "https://b.test/1")
            .await
            .unwrap();
        assert!(st1
            .get_chapter_content("alice", "https://b.test/1", 0)
            .await
            .unwrap()
            .is_none());

        st1.cache_toc(
            "alice",
            "https://b.test/1",
            "https://b.test/1/toc",
            r#"[{"title":"第一章"}]"#,
        )
        .await
        .unwrap();
        assert_eq!(
            st1.get_toc_cache("alice", "https://b.test/1/toc", 60_000)
                .await
                .unwrap()
                .as_deref(),
            Some(r#"[{"title":"第一章"}]"#)
        );
        assert!(st2
            .get_toc_cache("alice", "https://b.test/1/toc", 60_000)
            .await
            .unwrap()
            .is_none());
        let info = st1.get_cache_info("alice").await.unwrap();
        assert_eq!(info.toc_cache_count, 1);
        assert_eq!(info.chapter_count, 0);

        st1.clear_toc_cache_for_book("alice", "https://b.test/1")
            .await
            .unwrap();
        assert!(st1
            .get_toc_cache("alice", "https://b.test/1/toc", 60_000)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            st1.get_cache_info("alice").await.unwrap().toc_cache_count,
            0
        );

        cleanup(st1, dir1).await;
        cleanup(st2, dir2).await;
    }

    #[tokio::test]
    async fn search_book_content_fts5_trigram_like_and_sync() {
        let (storage, dir) = test_storage("fts5-trigram").await;
        assert!(
            storage.fts_trigram_ready,
            "bundled SQLite 应支持 FTS5 trigram 索引"
        );

        let book_a = "https://fts.test/book-a";
        let book_b = "https://fts.test/book-b";
        storage
            .upsert_book(
                "alice",
                &Book {
                    book_url: book_a.into(),
                    name: "修仙传".into(),
                    author: "墨客".into(),
                    origin: "https://fts.test".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        storage
            .save_chapters_ns(
                "alice",
                book_a,
                &[
                    ("第一章".into(), "青云山巅剑气纵横，初遇九霄雷劫。".into()),
                    (
                        "第二章".into(),
                        "师尊说道：\"玄天剑诀\" 乃金丹大道之基，StarLight 映照长夜。".into(),
                    ),
                    ("第三章".into(), "再临九霄雷劫，以剑破苍穹。".into()),
                ],
            )
            .await
            .unwrap();
        // 同库不同书 / 不同命名空间干扰项
        storage
            .save_chapters_ns(
                "alice",
                book_b,
                &[("他书".into(), "九霄雷劫他书内容".into())],
            )
            .await
            .unwrap();
        storage
            .save_chapters_ns("bob", book_a, &[("他户".into(), "九霄雷劫他户内容".into())])
            .await
            .unwrap();

        // 1) >= 3 字中文关键词：走 FTS5 trigram 倒排索引 + 命名空间/书籍隔离 + chapter_index 排序
        let hits = storage
            .search_book_content("alice", book_a, "九霄雷劫", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chapter_index, 0);
        assert_eq!(hits[1].chapter_index, 2);
        assert!(hits[0].snippet.contains("九霄雷劫"));

        // limit 截断生效
        let limited = storage
            .search_book_content("alice", book_a, "九霄雷劫", 1)
            .await
            .unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].chapter_index, 0);

        // 含双引号短语 & 英文大小写不敏感（均走 FTS5 trigram）
        let quoted = storage
            .search_book_content("alice", book_a, "\"玄天剑诀\"", 10)
            .await
            .unwrap();
        assert_eq!(quoted.len(), 1);
        assert_eq!(quoted[0].chapter_index, 1);

        let eng = storage
            .search_book_content("alice", book_a, "starlight", 10)
            .await
            .unwrap();
        assert_eq!(eng.len(), 1);
        assert_eq!(eng[0].chapter_index, 1);

        // 2) 1~2 字短中文关键词：走流式 LIKE 路径
        let short1 = storage
            .search_book_content("alice", book_a, "剑", 10)
            .await
            .unwrap();
        assert_eq!(short1.len(), 3);
        let short2 = storage
            .search_book_content("alice", book_a, "雷劫", 10)
            .await
            .unwrap();
        assert_eq!(short2.len(), 2);
        assert_eq!(short2[0].chapter_index, 0);
        assert_eq!(short2[1].chapter_index, 2);

        // 3) 章节更新（AFTER UPDATE 触发器）：旧词立即失效，新词立即可查
        storage
            .cache_chapter_content(
                "alice",
                book_a,
                0,
                "第一章",
                "青云山巅风平浪静，已证紫霄神雷。",
            )
            .await
            .unwrap();
        let after_upd_old = storage
            .search_book_content("alice", book_a, "九霄雷劫", 10)
            .await
            .unwrap();
        assert_eq!(after_upd_old.len(), 1);
        assert_eq!(after_upd_old[0].chapter_index, 2);
        let after_upd_new = storage
            .search_book_content("alice", book_a, "紫霄神雷", 10)
            .await
            .unwrap();
        assert_eq!(after_upd_new.len(), 1);
        assert_eq!(after_upd_new[0].chapter_index, 0);

        // 4) replace_chapters（先删后插）& delete_book_cache（AFTER DELETE 触发器）同步一致性
        storage
            .replace_chapters(
                "alice",
                book_a,
                &[("新章".into(), "替换后的全新太虚剑意".into())],
            )
            .await
            .unwrap();
        assert!(storage
            .search_book_content("alice", book_a, "九霄雷劫", 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            storage
                .search_book_content("alice", book_a, "太虚剑意", 10)
                .await
                .unwrap()
                .len(),
            1
        );

        storage.delete_book_cache("alice", book_a).await.unwrap();
        assert!(storage
            .search_book_content("alice", book_a, "太虚剑意", 10)
            .await
            .unwrap()
            .is_empty());

        // 验证 FTS5 外部内容表完整性（零脏 rowid）
        sqlx::query("INSERT INTO book_chapters_fts(book_chapters_fts) VALUES('integrity-check')")
            .execute(&storage.pool)
            .await
            .unwrap();

        cleanup(storage, dir).await;
    }

    #[tokio::test]
    async fn fts5_backfill_and_clean_schema_without_dead_columns() {
        let (storage, dir) = test_storage("fts5-backfill-schema").await;

        // 1) 新建 books / users 表不包含已废弃死列与冗余 raw_json 列
        let book_cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('books')")
                .fetch_all(&storage.pool)
                .await
                .unwrap();
        for removed in ["cbz", "local_pdf", "pdf", "raw_json"] {
            assert!(
                !book_cols.iter().any(|c| c == removed),
                "books 表不应再包含废弃列 {removed}"
            );
        }
        let user_cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('users')")
                .fetch_all(&storage.pool)
                .await
                .unwrap();
        assert!(
            !user_cols.iter().any(|c| c == "raw_json"),
            "users 表不应再包含冗余列 raw_json"
        );
        // 无 raw_json 列新库上用户读写（显式列投影 + CAST(NULL AS TEXT) AS raw_json）
        storage
            .insert_user(&User {
                username: "clean_u".into(),
                password: "h".into(),
                salt: "s".into(),
                user_namespace: "clean_u".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let u = storage.find_user("clean_u").await.unwrap().unwrap();
        assert_eq!(u.username, "clean_u");
        assert!(u.raw_json.is_none());
        assert_eq!(storage.list_users().await.unwrap().len(), 1);

        // 2) 外部备份 JSON 若含旧字段 cbz/localPdf/pdf，反序列化应静默忽略并兼容入库
        let legacy_book: Book = serde_json::from_str(
            r#"{"bookUrl":"https://legacy.test/1","name":"旧备份书","author":"佚名","origin":"https://legacy.test","cbz":true,"localPdf":true,"pdf":true}"#,
        )
        .unwrap();
        storage.upsert_book("alice", &legacy_book).await.unwrap();
        let loaded = storage
            .find_book("alice", "https://legacy.test/1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.name, "旧备份书");
        // patch_book 收到旧字段名时静默跳过，不触发 SQL 列不存在错误
        let patch: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"name":"改名书","cbz":true,"localPdf":false,"pdf":true}"#)
                .unwrap();
        assert_eq!(
            storage
                .patch_book("alice", "https://legacy.test/1", &patch)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .find_book("alice", "https://legacy.test/1")
                .await
                .unwrap()
                .unwrap()
                .name,
            "改名书"
        );

        // 3) 模拟存量库首次升级：先移除 FTS 表与触发器，写入存量章节，再调用 init_schema 触发自动回填
        for sql in [
            "DROP TRIGGER IF EXISTS book_chapters_fts_ai",
            "DROP TRIGGER IF EXISTS book_chapters_fts_ad",
            "DROP TRIGGER IF EXISTS book_chapters_fts_au",
            "DROP TABLE IF EXISTS book_chapters_fts",
        ] {
            sqlx::query(sql).execute(&storage.pool).await.unwrap();
        }
        storage
            .save_chapters_ns(
                "alice",
                "https://legacy.test/1",
                &[("存量章".into(), "升级前已入库的古老秘籍正文".into())],
            )
            .await
            .unwrap();

        let ready = schema::init_schema(&storage.pool).await.unwrap();
        assert!(ready);
        let hits = storage
            .search_book_content("alice", "https://legacy.test/1", "古老秘籍", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("古老秘籍"));

        cleanup(storage, dir).await;
    }
}
