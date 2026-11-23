//! 缓存与脚本持久化：目录缓存、书籍详情缓存、JS cache、书级变量、搜索短缓存

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use anyhow::Result;

use super::{CacheInfo, LruCache, Storage};

// ---------------- 目录热窗口 LRU 缓存（按 cache_tag + ns + toc_url 隔离） ----------------

const TOC_CACHE_MAX: usize = 32;

type TocCacheKey = (u64, String, String);

struct TocCacheEntry {
    book_url: String,
    chapters_json: String,
    updated_at: i64,
}

fn toc_cache() -> &'static parking_lot::Mutex<LruCache<TocCacheKey, TocCacheEntry>> {
    static M: OnceLock<parking_lot::Mutex<LruCache<TocCacheKey, TocCacheEntry>>> = OnceLock::new();
    M.get_or_init(|| parking_lot::Mutex::new(LruCache::new(TOC_CACHE_MAX)))
}

static TOC_CACHE_GEN: AtomicU64 = AtomicU64::new(0);

fn invalidate_toc_cache_for_ns(cache_tag: u64, ns: &str) {
    let mut m = toc_cache().lock();
    TOC_CACHE_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, e_ns, _), _| !(*tag == cache_tag && e_ns == ns));
}

pub(super) fn invalidate_toc_cache_for_book_url(cache_tag: u64, ns: &str, book_url: &str) {
    let mut m = toc_cache().lock();
    TOC_CACHE_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, e_ns, e_toc), entry| {
        !(*tag == cache_tag && e_ns == ns && (entry.book_url == book_url || e_toc == book_url))
    });
}

pub(super) fn invalidate_all_toc_cache() {
    let mut m = toc_cache().lock();
    TOC_CACHE_GEN.fetch_add(1, Ordering::Release);
    m.clear();
}

fn ends_with_ascii_ci(s: &str, suffix: &str) -> bool {
    let Some(start) = s.len().checked_sub(suffix.len()) else {
        return false;
    };
    s.is_char_boundary(start) && s[start..].eq_ignore_ascii_case(suffix)
}

impl Storage {
    // ---------------- 缓存统计与清理 ----------------

    /// 缓存统计（按命名空间隔离）
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

    /// 清空某命名空间缓存（type: "toc" / "chapters" / "all"）
    pub async fn clear_cache(&self, ns: &str, cache_type: &str) -> Result<(u64, u64)> {
        let mut toc_deleted = 0u64;
        let mut chapters_deleted = 0u64;
        if cache_type == "toc" || cache_type == "all" {
            let r = sqlx::query("DELETE FROM toc_cache WHERE user_namespace = ?1")
                .bind(ns)
                .execute(&self.pool)
                .await?;
            toc_deleted = r.rows_affected();
            invalidate_toc_cache_for_ns(self.cache_tag, ns);
        }
        if cache_type == "chapters" || cache_type == "all" {
            let r = sqlx::query("DELETE FROM book_chapters WHERE user_namespace = ?1")
                .bind(ns)
                .execute(&self.pool)
                .await?;
            chapters_deleted = r.rows_affected();
            super::books::invalidate_chapter_cache_for_ns(self.cache_tag, ns);
        }
        Ok((toc_deleted, chapters_deleted))
    }

    /// 删除单书缓存（不影响书架 books 行）
    pub async fn delete_book_cache(&self, ns: &str, book_url: &str) -> Result<u64> {
        let r =
            sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
                .bind(book_url)
                .bind(ns)
                .execute(&self.pool)
                .await?;
        super::books::invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        Ok(r.rows_affected())
    }

    /// 单书缓存信息：(章节数, 正文近似大小 sum length(content))
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

    // ---------------- 目录缓存（toc_cache） ----------------

    /// 目录缓存写入（写穿透更新热窗口 LRU）
    pub async fn cache_toc(
        &self,
        ns: &str,
        book_url: &str,
        toc_url: &str,
        chapters_json: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR REPLACE INTO toc_cache (book_url, toc_url, chapters_json, updated_at, user_namespace) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(book_url)
        .bind(toc_url)
        .bind(chapters_json)
        .bind(now)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        let mut m = toc_cache().lock();
        TOC_CACHE_GEN.fetch_add(1, Ordering::Release);
        m.retain(|(tag, e_ns, _), entry| {
            !(*tag == self.cache_tag && e_ns == ns && entry.book_url == book_url)
        });
        m.insert(
            (self.cache_tag, ns.to_string(), toc_url.to_string()),
            TocCacheEntry {
                book_url: book_url.to_string(),
                chapters_json: chapters_json.to_string(),
                updated_at: now,
            },
        );
        Ok(())
    }

    /// 清理某本书的目录缓存
    pub async fn clear_toc_cache_for_book(&self, ns: &str, book_url: &str) -> Result<u64> {
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
        let mut m = toc_cache().lock();
        TOC_CACHE_GEN.fetch_add(1, Ordering::Release);
        if tail.is_empty() {
            m.retain(|(tag, e_ns, e_toc), entry| {
                !(*tag == self.cache_tag
                    && e_ns == ns
                    && (entry.book_url == book_url || e_toc == book_url))
            });
        } else if tail.contains(['%', '_']) {
            m.retain(|(tag, e_ns, _), _| !(*tag == self.cache_tag && e_ns == ns));
        } else {
            let suffix = format!("/{tail}");
            m.retain(|(tag, e_ns, e_toc), entry| {
                !(*tag == self.cache_tag
                    && e_ns == ns
                    && (entry.book_url == book_url
                        || e_toc == book_url
                        || ends_with_ascii_ci(&entry.book_url, &suffix)
                        || ends_with_ascii_ci(e_toc, &suffix)))
            });
        }
        Ok(r.rows_affected())
    }

    /// 目录缓存读取（超过 max_age_ms 视为未命中；走热窗口 LRU 缓存）
    pub async fn get_toc_cache(
        &self,
        ns: &str,
        toc_url: &str,
        max_age_ms: i64,
    ) -> Result<Option<String>> {
        let cutoff = chrono::Utc::now().timestamp_millis() - max_age_ms;
        let key = (self.cache_tag, ns.to_string(), toc_url.to_string());
        let cached = {
            let mut m = toc_cache().lock();
            m.get(&key)
                .map(|e| (e.updated_at >= cutoff).then(|| e.chapters_json.clone()))
        };
        if let Some(res) = cached {
            return Ok(res);
        }
        let gen = TOC_CACHE_GEN.load(Ordering::Acquire);
        let r: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT book_url, chapters_json, updated_at FROM toc_cache \
             WHERE toc_url = ?1 AND updated_at >= ?2 AND user_namespace = ?3",
        )
        .bind(toc_url)
        .bind(cutoff)
        .bind(ns)
        .fetch_optional(&self.pool)
        .await?;
        let Some((book_url, chapters_json, updated_at)) = r else {
            return Ok(None);
        };
        let mut m = toc_cache().lock();
        if TOC_CACHE_GEN.load(Ordering::Acquire) == gen {
            m.insert(
                key,
                TocCacheEntry {
                    book_url,
                    chapters_json: chapters_json.clone(),
                    updated_at,
                },
            );
        }
        Ok(Some(chapters_json))
    }

    // ---------------- 书籍详情持久缓存（book_info_cache） ----------------

    /// 书籍详情持久缓存读：TTL 内命中返回反序列化值，过期/缺失返回 None
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

    /// 书籍详情持久缓存写
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

    /// 书籍详情持久缓存失效
    pub async fn clear_book_info_cache(&self, ns: &str, book_url: &str) -> Result<()> {
        sqlx::query("DELETE FROM book_info_cache WHERE book_url = ?1 AND user_namespace = ?2")
            .bind(book_url)
            .bind(ns)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---------------- JS cache 对象持久化（js_cache） ----------------

    /// 读取单条 JS cache（value, expiry 毫秒时间戳；0 = 永不过期）
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

    /// 写入单条 JS cache
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

    /// 删除单条 JS cache
    pub async fn delete_js_cache(&self, ns: &str, key: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM js_cache WHERE user_namespace = ?1 AND key = ?2")
            .bind(ns)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 清空命名空间全部 JS cache
    pub async fn clear_js_cache(&self, ns: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM js_cache WHERE user_namespace = ?1")
            .bind(ns)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
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

    // ---------------- @put/@get 书级变量持久化（book_vars_cache） ----------------

    /// 读取书级变量 JSON
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

    /// 写入书级变量 JSON
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

    /// 批量写章节级变量（单事务）
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
        let mut tx = self.begin_immediate().await?;
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

    // ---------------- 搜索结果短缓存（search_cache） ----------------

    /// 兼容调用方（表已在 schema.rs 初始化，此处幂等保底）
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
}
