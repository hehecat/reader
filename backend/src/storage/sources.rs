//! 书源 CRUD、置信度统计（source_stats）、默认书源标记与登录态 Cookie 快照缓存

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Result;

use super::{normalize_base, LruCache, SearchOutcome, Storage};

// ---------------- 书源列表与单源索引热缓存（按 cache_tag + ns 隔离） ----------------

const BOOK_SOURCES_CACHE_MAX: usize = 64;

struct BookSourcesEntry {
    /// 已合并 default、过滤 hidden、按权重排序的书源列表
    list: Arc<Vec<crate::model::BookSource>>,
    /// book_source_url → BookSource（O(1) 精确查找）
    by_url: HashMap<String, crate::model::BookSource>,
    /// 当前命名空间自有可见书源 URL（用于 find_book_source 区分自有源与 default 回退）
    owned_visible: HashSet<String>,
    /// 当前命名空间 hidden=1 覆盖 URL（保持 find_book_source 的 overlay 语义）
    hidden_urls: Vec<String>,
}

type BookSourcesCache = LruCache<(u64, String), Arc<BookSourcesEntry>>;

fn book_sources_cache() -> &'static parking_lot::Mutex<BookSourcesCache> {
    static M: OnceLock<parking_lot::Mutex<BookSourcesCache>> = OnceLock::new();
    M.get_or_init(|| parking_lot::Mutex::new(LruCache::new(BOOK_SOURCES_CACHE_MAX)))
}

static BOOK_SOURCES_GEN: AtomicU64 = AtomicU64::new(0);

/// 失效指定 Storage 实例（cache_tag）下全部命名空间的书源缓存（覆盖 default 公共源跨 ns 可见性）
pub(super) fn invalidate_book_sources_cache(cache_tag: u64) {
    let mut m = book_sources_cache().lock();
    BOOK_SOURCES_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, _), _| *tag != cache_tag);
}

/// 失效全部实例的书源缓存（用户删除等跨表清理路径）
pub(super) fn invalidate_all_book_sources_cache() {
    let mut m = book_sources_cache().lock();
    BOOK_SOURCES_GEN.fetch_add(1, Ordering::Release);
    m.clear();
}

/// ASCII 大小写不敏感前缀匹配（对齐 SQLite `LIKE '{prefix}%'` 对 URL 前缀的语义）
fn ascii_prefix_eq(candidate: &str, prefix: &str) -> bool {
    candidate.len() >= prefix.len()
        && candidate.is_char_boundary(prefix.len())
        && candidate[..prefix.len()].eq_ignore_ascii_case(prefix)
}

// ---------------- 书源 cookie 快照缓存（抓取热路径） ----------------

type CookieSnapshotMap =
    std::collections::HashMap<String, (std::time::Instant, std::sync::Arc<CookieSnapshot>)>;

fn cookie_snapshots() -> &'static parking_lot::Mutex<CookieSnapshotMap> {
    static M: std::sync::OnceLock<parking_lot::Mutex<CookieSnapshotMap>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

static COOKIE_SNAPSHOT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const COOKIE_SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(10);
const COOKIE_SNAPSHOT_MAX: usize = 32;

pub(super) fn insert_capped_evict_oldest<V>(
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
    while m.len() >= max && !m.contains_key(&key) {
        let Some(victim) = m
            .iter()
            .min_by(|a, b| a.1 .0.cmp(&b.1 .0).then_with(|| a.0.cmp(b.0)))
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        m.remove(&victim);
        evicted = Some(victim);
    }
    m.insert(key, (now, value));
    evicted
}

fn cookie_snapshot_peek(key: &str) -> Option<std::sync::Arc<CookieSnapshot>> {
    let m = cookie_snapshots().lock();
    match m.get(key) {
        Some((t, snap)) if t.elapsed() < COOKIE_SNAPSHOT_TTL => Some(snap.clone()),
        _ => None,
    }
}

/// 失效全部 cookie 快照——任何写 `book_source_cookies` 的路径提交后均应调用
pub(super) fn invalidate_cookie_snapshots_only() {
    let mut m = cookie_snapshots().lock();
    COOKIE_SNAPSHOT_GEN.fetch_add(1, Ordering::Release);
    m.clear();
}

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

    fn session(&self, base: &str) -> Option<(String, String)> {
        let row = self.rows.get(*self.any.get(base)?)?;
        Some((row.0.clone(), row.1.clone()))
    }

    fn cookie_for(&self, base: &str) -> Option<String> {
        let row = self.rows.get(*self.cookie.get(base)?)?;
        Some(row.0.clone())
    }

    fn header_for(&self, base: &str) -> Option<String> {
        let row = self.rows.get(*self.header.get(base)?)?;
        Some(row.2.clone())
    }
}

impl Storage {
    // ---------------- 书源 CRUD ----------------

    /// 加载并缓存 `(cache_tag, ns)` 的书源列表与 URL 索引（写穿透失效）
    async fn load_book_sources_entry(&self, ns: &str) -> Result<Arc<BookSourcesEntry>> {
        let key = (self.cache_tag, ns.to_string());
        if let Some(entry) = book_sources_cache().lock().get(&key).cloned() {
            return Ok(entry);
        }
        let gen = BOOK_SOURCES_GEN.load(Ordering::Acquire);
        let mut rows = sqlx::query_as::<_, crate::model::BookSource>(
            "SELECT * FROM book_sources WHERE user_namespace = ?1",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        sort_book_sources(&mut rows);

        let mut owned_visible: HashSet<String> = HashSet::with_capacity(rows.len());
        let mut hidden_urls: Vec<String> = Vec::new();
        for s in &rows {
            if s.hidden {
                hidden_urls.push(s.book_source_url.clone());
            } else {
                owned_visible.insert(s.book_source_url.clone());
            }
        }

        let visible: Vec<crate::model::BookSource> = if ns == "default" {
            rows.into_iter().filter(|s| !s.hidden).collect()
        } else {
            let mut default_rows = sqlx::query_as::<_, crate::model::BookSource>(
                "SELECT * FROM book_sources WHERE user_namespace = 'default'",
            )
            .fetch_all(&self.pool)
            .await?;
            sort_book_sources(&mut default_rows);
            let mut merged: Vec<crate::model::BookSource> = rows;
            let owned: HashSet<String> = merged.iter().map(|s| s.book_source_url.clone()).collect();
            for s in default_rows {
                if !owned.contains(&s.book_source_url) {
                    merged.push(s);
                }
            }
            sort_book_sources(&mut merged);
            merged.into_iter().filter(|s| !s.hidden).collect()
        };

        let mut by_url: HashMap<String, crate::model::BookSource> =
            HashMap::with_capacity(visible.len());
        for s in &visible {
            by_url.insert(s.book_source_url.clone(), s.clone());
        }
        let entry = Arc::new(BookSourcesEntry {
            list: Arc::new(visible),
            by_url,
            owned_visible,
            hidden_urls,
        });
        let mut m = book_sources_cache().lock();
        if BOOK_SOURCES_GEN.load(Ordering::Acquire) == gen {
            m.insert(key, entry.clone());
        }
        Ok(entry)
    }

    /// 书源列表（按命名空间，与 default 系统行合并，隐藏用户覆盖删除的系统源；走内存缓存）
    pub async fn get_book_sources(&self, ns: &str) -> Result<Vec<crate::model::BookSource>> {
        let entry = self.load_book_sources_entry(ns).await?;
        Ok((*entry.list).clone())
    }

    /// 书源使用统计自增
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
        invalidate_book_sources_cache(self.cache_tag);
        Ok(())
    }

    /// 按 URL 查书源（精确或前缀匹配，用户命名空间 + fallback default；走内存索引）
    pub async fn find_book_source(
        &self,
        ns: &str,
        book_source_url: &str,
    ) -> Result<Option<crate::model::BookSource>> {
        let entry = self.load_book_sources_entry(ns).await?;
        // 1) default 命名空间或当前 ns 自有可见源：O(1) 精确命中
        if ns == "default" || entry.owned_visible.contains(book_source_url) {
            if let Some(s) = entry.by_url.get(book_source_url) {
                return Ok(Some(s.clone()));
            }
        }
        // 2) 当前 ns 自有可见源前缀匹配（对齐原 SQL `LIKE '{url}%'`）
        if !entry.owned_visible.is_empty() {
            if let Some(s) = entry.list.iter().find(|s| {
                entry.owned_visible.contains(&s.book_source_url)
                    && ascii_prefix_eq(&s.book_source_url, book_source_url)
            }) {
                return Ok(Some(s.clone()));
            }
        }
        if ns == "default" {
            return Ok(None);
        }
        // 3) 非 default 命名空间 overlay 语义：存在 hidden=1 覆盖行匹配时不再回退 default
        if entry
            .hidden_urls
            .iter()
            .any(|u| u == book_source_url || ascii_prefix_eq(u, book_source_url))
        {
            return Ok(None);
        }
        // 4) 回退 default 公共源：先精确 O(1)，再前缀匹配
        if let Some(s) = entry.by_url.get(book_source_url) {
            return Ok(Some(s.clone()));
        }
        Ok(entry
            .list
            .iter()
            .find(|s| ascii_prefix_eq(&s.book_source_url, book_source_url))
            .cloned())
    }

    /// 按 URL 查单个书源（复用 find_book_source 语义）
    pub async fn get_book_source(
        &self,
        ns: &str,
        url: &str,
    ) -> Result<Option<crate::model::BookSource>> {
        self.find_book_source(ns, url).await
    }

    /// 保存书源（保留 use_count/use_ts 统计列）
    pub async fn save_book_source(
        &self,
        ns: &str,
        source: &crate::model::BookSource,
    ) -> Result<()> {
        upsert_book_source(&self.pool, ns, source).await?;
        invalidate_book_sources_cache(self.cache_tag);
        Ok(())
    }

    /// 批量保存书源（单事务）
    pub async fn save_book_sources(
        &self,
        ns: &str,
        sources: &[crate::model::BookSource],
    ) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        for source in sources {
            upsert_book_source(&mut *tx, ns, source).await?;
        }
        tx.commit().await?;
        invalidate_book_sources_cache(self.cache_tag);
        Ok(())
    }

    /// 删除书源（连带清理 cookie；非 default 用户删除系统源时写入 hidden 覆盖）
    pub async fn delete_book_source(&self, ns: &str, url: &str) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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
                if !(affected > 0 && default_src.book_source_url != url) {
                    upsert_book_source_hidden(&mut *tx, ns, &default_src, true).await?;
                    affected = 1;
                }
            } else if affected == 0 {
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
        invalidate_book_sources_cache(self.cache_tag);
        invalidate_cookie_snapshots_only();
        Ok(affected)
    }

    /// 去重分组列表
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

    /// 启停书源（非 default 用户修改系统源时 copy-on-write）
    pub async fn update_book_source_enabled(
        &self,
        ns: &str,
        url: &str,
        enabled: bool,
    ) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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
            if let Some(mut copy) = find_book_source_row(&mut *tx, "default", url).await? {
                copy.enabled = enabled;
                upsert_book_source_hidden(&mut *tx, ns, &copy, false).await?;
                affected = 1;
            }
        }
        tx.commit().await?;
        invalidate_book_sources_cache(self.cache_tag);
        Ok(affected)
    }

    /// 清空命名空间全部书源（连带清理 cookie）
    pub async fn delete_all_book_sources(&self, ns: &str) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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
        invalidate_book_sources_cache(self.cache_tag);
        invalidate_cookie_snapshots_only();
        Ok(affected)
    }

    /// 某命名空间自有书源数
    pub async fn count_book_sources(&self, ns: &str) -> Result<i64> {
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM book_sources WHERE user_namespace = ?1 AND hidden = 0",
        )
        .bind(ns)
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

    // ---------------- 书源置信度统计（source_stats） ----------------

    /// 兼容调用方（表已在 schema.rs 初始化，此处幂等保底）
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
        Ok(())
    }

    /// 三态记账：Ok / Fail / Timeout
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
        let ok_delta = (outcome == SearchOutcome::Ok) as i64;
        let timeout_delta = (outcome == SearchOutcome::Timeout) as i64;
        let consec_inc = (outcome == SearchOutcome::Fail) as i64;
        let consec_reset = (outcome == SearchOutcome::Ok) as i64;
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

    /// 兼容布尔入口（`true → Ok`、`false → Fail`）
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

    // ---------------- 书源登录态 Cookie（按用户隔离） ----------------

    async fn cookie_snapshot(&self, ns: &str) -> Result<std::sync::Arc<CookieSnapshot>> {
        let key = format!("{}\u{1}{ns}", self.cache_tag);
        if let Some(snap) = cookie_snapshot_peek(&key) {
            return Ok(snap);
        }
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
        let mut m = cookie_snapshots().lock();
        if COOKIE_SNAPSHOT_GEN.load(std::sync::atomic::Ordering::Acquire) == gen {
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

    /// 读取书源 cookie（精确 source_url 键）
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

    /// 列出当前用户全部书源登录态
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

    /// 写入书源 cookie（空值等价清除）
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
        invalidate_cookie_snapshots_only();
        Ok(())
    }

    /// 清除书源 cookie
    pub async fn clear_cookie(&self, ns: &str, source_url: &str) -> Result<u64> {
        let r = sqlx::query(
            "DELETE FROM book_source_cookies WHERE user_namespace = ?1 AND source_url = ?2",
        )
        .bind(ns)
        .bind(source_url)
        .execute(&self.pool)
        .await?;
        invalidate_cookie_snapshots_only();
        Ok(r.rows_affected())
    }

    /// 记录书源 user_agent
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
        invalidate_cookie_snapshots_only();
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

    /// 按 baseUrl 匹配书源 cookie（走快照缓存）
    pub async fn get_cookie_by_base(&self, ns: &str, base_url: &str) -> Result<Option<String>> {
        let Some(target) = normalize_base(base_url) else {
            return Ok(None);
        };
        Ok(self.cookie_snapshot(ns).await?.cookie_for(&target))
    }

    /// 读取书源登录头
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

    /// 写入书源登录头（保留既有 cookie/UA；空值等价清除）
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
            invalidate_cookie_snapshots_only();
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
        invalidate_cookie_snapshots_only();
        Ok(())
    }

    /// 按 baseUrl 匹配书源登录头（走快照缓存）
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

    /// 按 baseUrl 匹配书源会话（cookie + user_agent，走快照缓存）
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
}

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

fn sort_book_sources(v: &mut [crate::model::BookSource]) {
    v.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.custom_order.cmp(&b.custom_order))
            .then_with(|| a.book_source_name.cmp(&b.book_source_name))
    });
}
