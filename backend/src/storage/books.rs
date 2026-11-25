//! 书籍、章节缓存、书架分组、书签、本地书同步与换源候选 DAO

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use anyhow::Result;
use futures::StreamExt;

use crate::model::Book;

use super::{make_snippet, BookContentHit, LruCache, Storage};

/// `books` 表显式列名投影（与 `model::Book` 100% 对齐，跳过旧库残留的死列 `cbz`/`local_pdf`/`pdf`/`raw_json`）
macro_rules! book_select_cols {
    () => {
        "book_url, name, author, origin, origin_name, toc_url, kind, custom_tag, \
         cover_url, custom_cover_url, intro, custom_intro, charset, type, group_name, \
         group_ids, latest_chapter_title, latest_chapter_time, last_check_time, \
         last_check_count, total_chapter_num, dur_chapter_title, dur_chapter_index, \
         dur_chapter_pos, dur_chapter_time, word_count, can_update, order_num, \
         origin_order, use_replace_rule, variable, read_config, is_in_shelf, \
         display_cover, display_intro, local_epub, split_long_chapter, \
         last_check_error, info_html, toc_html, language, publisher, published_at, \
         user_namespace, created_at, local_file, local_file_mtime, local_file_size, \
         local_file_deleted"
    };
}

// ---------------- 章节正文热窗口 LRU 缓存（按 cache_tag + ns + book_url + index 隔离） ----------------

const CHAPTER_CONTENT_CACHE_MAX: usize = 128;

type ChapterContentKey = (u64, String, String, i64);

fn chapter_content_cache() -> &'static parking_lot::Mutex<LruCache<ChapterContentKey, String>> {
    static M: OnceLock<parking_lot::Mutex<LruCache<ChapterContentKey, String>>> = OnceLock::new();
    M.get_or_init(|| parking_lot::Mutex::new(LruCache::new(CHAPTER_CONTENT_CACHE_MAX)))
}

static CHAPTER_CONTENT_GEN: AtomicU64 = AtomicU64::new(0);

pub(super) fn invalidate_chapter_cache_for_ns(cache_tag: u64, ns: &str) {
    let mut m = chapter_content_cache().lock();
    CHAPTER_CONTENT_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, e_ns, _, _), _| !(*tag == cache_tag && e_ns == ns));
}

pub(super) fn invalidate_chapter_cache_for_book(cache_tag: u64, ns: &str, book_url: &str) {
    let mut m = chapter_content_cache().lock();
    CHAPTER_CONTENT_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, e_ns, e_url, _), _| !(*tag == cache_tag && e_ns == ns && e_url == book_url));
}

fn invalidate_chapter_cache_for_book_any_ns(cache_tag: u64, book_url: &str) {
    let mut m = chapter_content_cache().lock();
    CHAPTER_CONTENT_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, _, e_url, _), _| !(*tag == cache_tag && e_url == book_url));
}

pub(super) fn invalidate_all_chapter_content_cache() {
    let mut m = chapter_content_cache().lock();
    CHAPTER_CONTENT_GEN.fetch_add(1, Ordering::Release);
    m.clear();
}

fn put_chapter_content_cache(
    cache_tag: u64,
    ns: &str,
    book_url: &str,
    index: i64,
    content: String,
) {
    let mut m = chapter_content_cache().lock();
    CHAPTER_CONTENT_GEN.fetch_add(1, Ordering::Release);
    m.insert(
        (cache_tag, ns.to_string(), book_url.to_string(), index),
        content,
    );
}

impl Storage {
    // ---------------- 书架书籍 CRUD ----------------

    /// 查询某命名空间的书架（手动排序在前，最近活动优先）
    pub async fn list_books(&self, namespace: &str) -> Result<Vec<Book>> {
        let books = sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            ", rowid AS rowid FROM books \
             WHERE user_namespace = ?1 \
             ORDER BY order_num ASC, MAX(dur_chapter_time, created_at) DESC, rowid DESC"
        ))
        .bind(namespace)
        .fetch_all(&self.pool)
        .await?;
        Ok(books)
    }

    /// 全部命名空间书籍总数
    pub async fn count_books(&self) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM books")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 某命名空间书架书籍数
    pub async fn count_books_for_user(&self, ns: &str) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM books WHERE user_namespace = ?1")
            .bind(ns)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    pub async fn find_book(&self, ns: &str, book_url: &str) -> Result<Option<Book>> {
        let book = sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            " FROM books WHERE user_namespace = ?1 AND book_url = ?2"
        ))
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(book)
    }

    /// 按 书名+作者 查书架书
    pub async fn find_book_by_name_author(
        &self,
        ns: &str,
        name: &str,
        author: &str,
    ) -> Result<Option<Book>> {
        let book = sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            ", rowid AS rowid FROM books \
             WHERE user_namespace = ?1 AND name = ?2 AND author = ?3 \
             ORDER BY rowid LIMIT 1"
        ))
        .bind(ns)
        .bind(name)
        .bind(author)
        .fetch_optional(&self.pool)
        .await?;
        Ok(book)
    }

    /// saveBook 全量入架/覆盖（保留既有 local_file 关联）
    pub async fn upsert_book(&self, ns: &str, book: &Book) -> Result<()> {
        let mut b = book.clone();
        b.user_namespace = ns.to_string();
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
             use_replace_rule, variable, read_config, is_in_shelf, display_cover,
             display_intro, local_epub, split_long_chapter, last_check_error, info_html,
             toc_html, language, publisher, published_at, user_namespace, created_at,
             local_file, local_file_mtime, local_file_size, local_file_deleted)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                    ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40,
                    ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49)"#,
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
        .bind(&b.display_cover)
        .bind(&b.display_intro)
        .bind(b.local_epub)
        .bind(b.split_long_chapter)
        .bind(&b.last_check_error)
        .bind(&b.info_html)
        .bind(&b.toc_html)
        .bind(&b.language)
        .bind(&b.publisher)
        .bind(&b.published_at)
        .bind(&b.user_namespace)
        .bind(b.created_at)
        .bind(local_file)
        .bind(b.local_file_mtime)
        .bind(b.local_file_size)
        .bind(b.local_file_deleted)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// saveBook 增量更新（白名单 camelCase 键）
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

    /// 更新书字段（name/author/coverUrl/group）
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
            "UPDATE books SET name = COALESCE(?3, name), author = COALESCE(?4, author), \
             cover_url = COALESCE(?5, cover_url), group_name = COALESCE(?6, group_name) \
             WHERE user_namespace = ?1 AND book_url = ?2",
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

    /// 更新封面 URL
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

    /// 换源持久化：更新 origin/originName/bookUrl/tocUrl，并清理旧 URL 的章节与目录缓存
    #[allow(clippy::too_many_arguments)]
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
        let mut tx = self.begin_immediate().await?;
        if old_url != new_url {
            sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
                .bind(old_url)
                .bind(ns)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM toc_cache WHERE book_url = ?1 AND user_namespace = ?2")
                .bind(old_url)
                .bind(ns)
                .execute(&mut *tx)
                .await?;
        }
        let r = sqlx::query(
            "UPDATE books SET origin = ?3, origin_name = ?4, book_url = ?5, toc_url = ?6, \
             cover_url = CASE WHEN (cover_url IS NULL OR cover_url = '') THEN ?7 ELSE cover_url END \
             WHERE user_namespace = ?1 AND book_url = ?2",
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
        if old_url != new_url {
            invalidate_chapter_cache_for_book(self.cache_tag, ns, old_url);
            super::cache::invalidate_toc_cache_for_book_url(self.cache_tag, ns, old_url);
        }
        Ok(r.rows_affected())
    }
}

/// 跨设备阅读进度写入结果
#[derive(Debug, Clone)]
pub enum ProgressUpdateResult {
    Updated {
        dur_chapter_index: i64,
        dur_chapter_pos: i64,
        dur_chapter_title: String,
        dur_chapter_time: i64,
    },
    Conflict {
        dur_chapter_index: i64,
        dur_chapter_pos: i64,
        dur_chapter_title: String,
        dur_chapter_time: i64,
    },
    NotFound,
}

impl Storage {
    /// 原子保存阅读进度（支持服务端单调时钟 + 乐观并发冲突检测，完全兼容旧客户端）
    ///
    /// - 当 `base_time` 为 `Some(base)` 且 `!force` 时：若库中 `dur_chapter_time > base`
    ///   且位置与本次上报不同，则判定为其他设备已更新，拒绝覆盖并返回 `Conflict`。
    /// - 当 `base_time` 为 `None` 且 `!force` 时：兼容旧版 Legado / reader3 协议，
    ///   较旧客户端时间戳不覆盖较新进度。
    #[allow(clippy::too_many_arguments)]
    pub async fn save_book_progress_checked(
        &self,
        ns: &str,
        book_url: &str,
        title: Option<&str>,
        index: i64,
        pos: i64,
        base_time: Option<i64>,
        force: bool,
        client_time: Option<i64>,
        server_now: i64,
    ) -> Result<ProgressUpdateResult> {
        let mut tx = self.begin_immediate().await?;
        let row: Option<(i64, i64, String, i64)> = sqlx::query_as(
            "SELECT COALESCE(dur_chapter_index, 0), COALESCE(dur_chapter_pos, 0), \
             COALESCE(dur_chapter_title, ''), COALESCE(dur_chapter_time, 0) \
             FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((cur_index, cur_pos, cur_title, cur_time)) = row else {
            return Ok(ProgressUpdateResult::NotFound);
        };

        // 现代跨设备客户端：带有效 base_time(>0) 时做乐观并发检测，防止后台未退出的旧设备覆盖新进度
        if let Some(base) = base_time {
            if !force && base > 0 && cur_time > base && (cur_index != index || cur_pos != pos) {
                return Ok(ProgressUpdateResult::Conflict {
                    dur_chapter_index: cur_index,
                    dur_chapter_pos: cur_pos,
                    dur_chapter_title: cur_title,
                    dur_chapter_time: cur_time,
                });
            }
        } else if !force {
            // 兼容旧客户端：若显式传了早于当前库记录的 client_time，则保持原进度不回退
            if let Some(ct) = client_time {
                if ct > 0 && ct < cur_time {
                    return Ok(ProgressUpdateResult::Updated {
                        dur_chapter_index: cur_index,
                        dur_chapter_pos: cur_pos,
                        dur_chapter_title: cur_title,
                        dur_chapter_time: cur_time,
                    });
                }
            }
        }

        // 使用服务端单调递增时钟，彻底消除多设备本地时钟偏差
        let next_time = if base_time.is_some() || force {
            server_now.max(cur_time.saturating_add(1))
        } else {
            server_now.max(client_time.unwrap_or(0)).max(cur_time)
        };
        let next_title = title.unwrap_or(cur_title.as_str()).to_string();

        sqlx::query(
            "UPDATE books SET dur_chapter_title = COALESCE(?3, dur_chapter_title), \
             dur_chapter_index = ?4, dur_chapter_pos = ?5, dur_chapter_time = ?6 \
             WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .bind(title)
        .bind(index)
        .bind(pos)
        .bind(next_time)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(ProgressUpdateResult::Updated {
            dur_chapter_index: index,
            dur_chapter_pos: pos,
            dur_chapter_title: next_title,
            dur_chapter_time: next_time,
        })
    }
}

impl Storage {
    /// 保存阅读进度（较旧时间戳不覆盖新进度）
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
            "UPDATE books SET dur_chapter_title = COALESCE(?3, dur_chapter_title), \
             dur_chapter_index = ?4, dur_chapter_pos = ?5, dur_chapter_time = ?6 \
             WHERE user_namespace = ?1 AND book_url = ?2 \
             AND COALESCE(dur_chapter_time, 0) <= ?6",
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

    /// 删除本地书（按 ns 隔离删章节与书籍行）
    pub async fn delete_local_book(&self, ns: &str, book_url: &str) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
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
        invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        Ok(())
    }

    /// 删除单书（含章节/目录缓存，并清理无引用本地封面）
    pub async fn delete_book(&self, ns: &str, book_url: &str) -> Result<u64> {
        let cover_url = sqlx::query_scalar::<_, Option<String>>(
            "SELECT cover_url FROM books WHERE user_namespace = ?1 AND book_url = ?2",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        let mut tx = self.begin_immediate().await?;
        sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
            .bind(book_url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM toc_cache WHERE book_url = ?1 AND user_namespace = ?2")
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
        invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        super::cache::invalidate_toc_cache_for_book_url(self.cache_tag, ns, book_url);
        if r.rows_affected() > 0 {
            self.cleanup_orphan_cover(ns, cover_url.as_deref()).await;
        }
        Ok(r.rows_affected())
    }

    /// 批量删除书（事务，连带删章节/目录缓存并清理无引用封面）
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
        let mut tx = self.begin_immediate().await?;
        let mut deleted = 0u64;
        for url in book_urls {
            sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
                .bind(url)
                .bind(ns)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM toc_cache WHERE book_url = ?1 AND user_namespace = ?2")
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
        for url in book_urls {
            invalidate_chapter_cache_for_book(self.cache_tag, ns, url);
            super::cache::invalidate_toc_cache_for_book_url(self.cache_tag, ns, url);
        }
        for c in covers {
            self.cleanup_orphan_cover(ns, Some(&c)).await;
        }
        Ok(deleted)
    }

    async fn cleanup_orphan_cover(&self, ns: &str, cover_url: Option<&str>) {
        let Some(cover_url) = cover_url else { return };
        let Some(file) = cover_file_name(ns, cover_url) else {
            return;
        };
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
        if let Err(e) = tokio::fs::remove_file(&path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("封面清理失败 {}: {e}", path.display());
            }
        }
    }

    /// 可更新书架书（can_update = 1，全命名空间）
    pub async fn list_updatable_books(&self) -> Result<Vec<Book>> {
        let books = sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            " FROM books WHERE can_update = 1"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(books)
    }

    /// 回写定时更新检查结果
    pub async fn update_book_update_info(
        &self,
        ns: &str,
        book_url: &str,
        latest_title: Option<&str>,
        total_num: i64,
        checked_at: i64,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE books SET \
             latest_chapter_title = COALESCE(?3, latest_chapter_title), \
             latest_chapter_time = CASE WHEN ?3 IS NOT NULL THEN ?4 ELSE latest_chapter_time END, \
             total_chapter_num = ?5, \
             last_check_time = ?6, \
             last_check_count = last_check_count + 1 \
             WHERE user_namespace = ?1 AND book_url = ?2",
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

    // ---------------- 章节读写与全文搜索 ----------------

    /// 保存章节（默认命名空间）
    pub async fn save_chapters(&self, book_url: &str, chapters: &[(String, String)]) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
                 title = excluded.title, content = excluded.content",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        invalidate_chapter_cache_for_book_any_ns(self.cache_tag, book_url);
        Ok(())
    }

    /// 保存章节（按命名空间隔离）
    pub async fn save_chapters_ns(
        &self,
        ns: &str,
        book_url: &str,
        chapters: &[(String, String)],
    ) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
                 title = excluded.title, content = excluded.content",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        Ok(())
    }

    /// 替换本地书全部章节（事务内先删后插）
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
        let mut tx = self.begin_immediate().await?;
        sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
            .bind(book_url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
                 title = excluded.title, content = excluded.content",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        Ok(())
    }

    /// 章节列表（按命名空间隔离）
    pub async fn list_chapters_ns(&self, ns: &str, book_url: &str) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT chapter_index, title FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2 ORDER BY chapter_index",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 章节列表（不按 ns 过滤，供全站管理/测试使用）
    pub async fn list_chapters(&self, book_url: &str) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT chapter_index, title FROM book_chapters WHERE book_url = ?1 ORDER BY chapter_index",
        )
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 已缓存章节的「下标 + 标题」（离线兜底）
    pub async fn list_cached_chapter_headers(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<(i64, String)>> {
        self.list_chapters_ns(ns, book_url).await
    }

    /// 前缀匹配 book_vars_cache 中的候选 URL（离线目录复原用）
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

    /// 某书章节数（按命名空间隔离）
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

    /// 命名空间内某书章节行数（同 count_chapters）
    pub async fn count_book_chapters(&self, ns: &str, book_url: &str) -> Result<i64> {
        self.count_chapters(ns, book_url).await
    }

    /// 章节列表（含字数，按命名空间隔离）
    pub async fn list_chapters_with_word_count_ns(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<(i64, String, i64)>> {
        let rows = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT chapter_index, title, length(content) FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2 ORDER BY chapter_index",
        )
        .bind(book_url)
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 章节列表（含字数）
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

    /// 单章正文（按命名空间隔离；走热窗口 LRU 缓存）
    pub async fn get_chapter_content(
        &self,
        ns: &str,
        book_url: &str,
        index: i64,
    ) -> Result<Option<String>> {
        let key = (self.cache_tag, ns.to_string(), book_url.to_string(), index);
        if let Some(cached) = chapter_content_cache().lock().get(&key).cloned() {
            return Ok(Some(cached));
        }
        let gen = CHAPTER_CONTENT_GEN.load(Ordering::Acquire);
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT content FROM book_chapters WHERE book_url = ?1 AND chapter_index = ?2 AND user_namespace = ?3",
        )
        .bind(book_url)
        .bind(index)
        .bind(ns)
        .fetch_optional(&self.pool)
        .await?;
        let Some((content,)) = r else {
            return Ok(None);
        };
        let mut m = chapter_content_cache().lock();
        if CHAPTER_CONTENT_GEN.load(Ordering::Acquire) == gen {
            m.insert(key, content.clone());
        }
        Ok(Some(content))
    }

    /// 单书已缓存章节（含正文，按命名空间隔离）
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

    /// 本地书章节（含正文，按命名空间隔离）
    pub async fn list_chapters_full_ns(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<(i64, String, String)>> {
        self.list_cached_chapters(ns, book_url).await
    }

    /// 本地书章节（含正文，全量）
    pub async fn list_chapters_full(&self, book_url: &str) -> Result<Vec<(i64, String, String)>> {
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chapter_index, title, content FROM book_chapters WHERE book_url = ?1 ORDER BY chapter_index",
        )
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 单章正文缓存写回（写穿透更新热窗口 LRU）
    pub async fn cache_chapter_content(
        &self,
        ns: &str,
        book_url: &str,
        index: i64,
        title: &str,
        content: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO book_chapters (book_url, chapter_index, title, content, user_namespace) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
             title = excluded.title, content = excluded.content",
        )
        .bind(book_url)
        .bind(index)
        .bind(title)
        .bind(content)
        .bind(ns)
        .execute(&self.pool)
        .await?;
        put_chapter_content_cache(self.cache_tag, ns, book_url, index, content.to_string());
        Ok(())
    }

    /// 批量写章节正文（单事务）
    pub async fn cache_chapter_contents_batch(
        &self,
        ns: &str,
        book_url: &str,
        items: &[(i64, String, String)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut tx = self.begin_immediate().await?;
        for (index, title, content) in items {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
                 title = excluded.title, content = excluded.content",
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
        invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        Ok(())
    }

    /// 全书正文搜索（`>= 3` 字符走 FTS5 `trigram` 倒排索引，1~2 字符或无 FTS 走流式 LIKE；
    /// 两条路径均通过异步流式游标逐行抽取摘要后立即释放正文，避免全书正文驻留内存）
    pub async fn search_book_content(
        &self,
        ns: &str,
        book_url: &str,
        key: &str,
        limit: i64,
    ) -> Result<Vec<BookContentHit>> {
        if key.is_empty() || limit <= 0 {
            return Ok(Vec::new());
        }
        let (sql, param) =
            if self.fts_trigram_ready && key.chars().count() >= 3 && !key.contains('\0') {
                (
                    "SELECT c.chapter_index, COALESCE(c.title, ''), COALESCE(c.content, '') \
                 FROM book_chapters_fts \
                 JOIN book_chapters c ON c.rowid = book_chapters_fts.rowid \
                 WHERE c.book_url = ?1 AND c.user_namespace = ?2 AND book_chapters_fts MATCH ?3 \
                 ORDER BY c.chapter_index LIMIT ?4",
                    fts5_phrase_query(key),
                )
            } else {
                let escaped = key
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                (
                    "SELECT chapter_index, COALESCE(title, ''), COALESCE(content, '') \
                 FROM book_chapters \
                 WHERE book_url = ?1 AND user_namespace = ?2 AND content LIKE ?3 ESCAPE '\\' \
                 ORDER BY chapter_index LIMIT ?4",
                    format!("%{escaped}%"),
                )
            };
        let mut hits = Vec::new();
        let mut rows = sqlx::query_as::<_, (i64, String, String)>(sql)
            .bind(book_url)
            .bind(ns)
            .bind(&param)
            .bind(limit)
            .fetch(&self.pool);
        while let Some(row) = rows.next().await {
            let (chapter_index, title, content) = row?;
            let snippet = make_snippet(&content, key, 40);
            drop(content);
            hits.push(BookContentHit {
                chapter_index,
                title,
                snippet,
            });
        }
        Ok(hits)
    }

    // ---------------- 本地书与目录同步 ----------------

    /// 本地书入库（books + 章节）
    pub async fn save_local_book(
        &self,
        ns: &str,
        info: &crate::model::book_chapter::BookInfo,
        imported: &crate::service::local_book::ImportedBook,
    ) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
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
        for (i, c) in imported.chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
                 title = excluded.title, content = excluded.content",
            )
            .bind(&info.book_url)
            .bind(i as i64)
            .bind(&c.title)
            .bind(&c.content)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        invalidate_chapter_cache_for_book(self.cache_tag, ns, &info.book_url);
        Ok(())
    }

    /// 查询有 local_file 关联的书（含已删除标记）
    pub async fn list_linked_local_books(&self, ns: &str) -> Result<Vec<Book>> {
        let rows = sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            " FROM books WHERE user_namespace = ?1 AND local_file IS NOT NULL AND local_file != ''"
        ))
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 查询无文件关联的 local:// DB 书
    pub async fn list_local_db_books_without_file(&self, ns: &str) -> Result<Vec<Book>> {
        let rows = sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            " FROM books WHERE user_namespace = ?1 AND book_url LIKE 'local://%' AND (local_file IS NULL OR local_file = '')"
        ))
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 查全部 loc_book 文件书
    pub async fn list_loc_book_books(&self, ns: &str) -> Result<Vec<Book>> {
        sqlx::query_as::<_, Book>(concat!(
            "SELECT ",
            book_select_cols!(),
            " FROM books WHERE user_namespace = ?1 AND origin = 'loc_book' ORDER BY name"
        ))
        .bind(ns)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// 写入本地书文件关联
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
            "UPDATE books SET local_file = ?3, local_file_mtime = ?4, local_file_size = ?5, local_file_deleted = ?6 \
             WHERE user_namespace = ?1 AND book_url = ?2",
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

    /// loc_book 文件书迁移写入
    #[allow(clippy::too_many_arguments)]
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
        let mut tx = self.begin_immediate().await?;
        sqlx::query("DELETE FROM book_chapters WHERE book_url = ?1 AND user_namespace = ?2")
            .bind(book_url)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        for (i, (title, content)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_chapters (book_url, chapter_index, title, content, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_url, chapter_index, user_namespace) DO UPDATE SET \
                 title = excluded.title, content = excluded.content",
            )
            .bind(book_url)
            .bind(i as i64)
            .bind(title)
            .bind(content)
            .bind(ns)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE books SET local_file = ?3, local_file_mtime = ?4, local_file_size = ?5, local_file_deleted = 0, total_chapter_num = ?6 \
             WHERE user_namespace = ?1 AND book_url = ?2",
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
        invalidate_chapter_cache_for_book(self.cache_tag, ns, book_url);
        if let Some(cover) = cover {
            if !cover.is_empty() {
                let cover_dir = self
                    .config
                    .storage_dir()
                    .join("assets")
                    .join(ns)
                    .join("covers");
                let _ = tokio::fs::create_dir_all(&cover_dir).await;
                let file_id = format!("{}.jpg", uuid::Uuid::new_v4());
                if tokio::fs::write(cover_dir.join(&file_id), cover)
                    .await
                    .is_ok()
                {
                    let _ = self
                        .update_book_cover(ns, book_url, &format!("/assets/{ns}/covers/{file_id}"))
                        .await;
                }
            }
        }
        Ok(true)
    }

    // ---------------- 书签 CRUD ----------------

    /// 保存书签
    pub async fn save_bookmark(&self, ns: &str, bookmark: &crate::model::Bookmark) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO bookmarks (book_url, title, book_name, book_author, \
             paragraph_index, chapter_index, chapter_name, book_text, content, \
             created_at, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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

    /// 批量保存书签
    pub async fn save_bookmarks(
        &self,
        ns: &str,
        bookmarks: &[crate::model::Bookmark],
    ) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        for b in bookmarks {
            sqlx::query(
                "INSERT OR REPLACE INTO bookmarks (book_url, title, book_name, book_author, \
                 paragraph_index, chapter_index, chapter_name, book_text, content, \
                 created_at, user_namespace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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

    /// 某书的书签列表（按创建时间倒序）
    pub async fn list_bookmarks(
        &self,
        ns: &str,
        book_url: &str,
    ) -> Result<Vec<crate::model::Bookmark>> {
        let rows = sqlx::query_as::<_, crate::model::Bookmark>(
            "SELECT * FROM bookmarks WHERE user_namespace = ?1 AND book_url = ?2 \
             ORDER BY created_at DESC, rowid DESC",
        )
        .bind(ns)
        .bind(book_url)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 删除单条书签
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

    /// 批量删除书签
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

    // ---------------- 书架分组 CRUD ----------------

    /// 分组列表
    pub async fn list_book_groups(&self, ns: &str) -> Result<Vec<crate::model::BookGroup>> {
        let rows = sqlx::query_as::<_, crate::model::BookGroup>(
            "SELECT * FROM book_groups WHERE user_namespace = ?1 ORDER BY order_num, id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 保存分组（id > 0 校验归属后覆盖，否则自增新建）
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
                "INSERT OR REPLACE INTO book_groups (id, name, cover, show, order_num, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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

    /// 书设分组（兼容单值）
    pub async fn update_book_group_id(&self, ns: &str, book_url: &str, group: i64) -> Result<u64> {
        self.set_book_groups(ns, book_url, &[group]).await
    }

    /// 多分组设置
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

    pub async fn add_book_group(&self, ns: &str, book_url: &str, group_id: i64) -> Result<u64> {
        let mut ids = self.book_group_ids(ns, book_url).await?;
        if ids.contains(&group_id) {
            return Ok(0);
        }
        ids.push(group_id);
        self.set_book_groups(ns, book_url, &ids).await
    }

    pub async fn remove_book_group(&self, ns: &str, book_url: &str, group_id: i64) -> Result<u64> {
        let mut ids = self.book_group_ids(ns, book_url).await?;
        let before = ids.len();
        ids.retain(|id| *id != group_id);
        if ids.len() == before {
            return Ok(0);
        }
        self.set_book_groups(ns, book_url, &ids).await
    }

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

    /// 分组列表（含组内书数统计，空库自动播种默认分组）
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
                    "INSERT OR IGNORE INTO book_groups (id, name, show, order_num, user_namespace) \
                     VALUES (?1, ?2, 1, ?3, ?4)",
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

    pub async fn delete_book_group(&self, ns: &str, id: i64) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
        sqlx::query(
            "UPDATE books SET group_name = 0 WHERE user_namespace = ?1 AND group_name = ?2",
        )
        .bind(ns)
        .bind(id)
        .execute(&mut *tx)
        .await?;
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

    pub async fn add_book_group_multi(
        &self,
        ns: &str,
        book_urls: &[String],
        group_id: i64,
    ) -> Result<u64> {
        if book_urls.is_empty() {
            return Ok(0);
        }
        let mut tx = self.begin_immediate().await?;
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

    pub async fn remove_book_group_multi(
        &self,
        ns: &str,
        book_urls: &[String],
        group_id: Option<i64>,
    ) -> Result<u64> {
        if book_urls.is_empty() {
            return Ok(0);
        }
        let mut tx = self.begin_immediate().await?;
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

    pub async fn save_book_group_order(&self, ns: &str, order: &[(i64, i64)]) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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

    // ---------------- 换源候选 ----------------

    pub async fn get_book_candidates(
        &self,
        ns: &str,
        key: &str,
    ) -> Result<Vec<crate::service::search::SearchBook>> {
        let r: Option<(String,)> = sqlx::query_as(
            "SELECT candidates_json FROM book_source_candidates \
             WHERE user_namespace = ?1 AND book_key = ?2",
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

    pub async fn save_book_candidates(
        &self,
        ns: &str,
        key: &str,
        cands: &[crate::service::search::SearchBook],
    ) -> Result<()> {
        let json = serde_json::to_string(cands)?;
        sqlx::query(
            "INSERT OR REPLACE INTO book_source_candidates \
             (user_namespace, book_key, candidates_json, updated_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(ns)
        .bind(key)
        .bind(json)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// 构造 FTS5 短语查询：将关键词内双引号转义为 `""` 后包裹为 `"..."`，避免操作符注入
fn fts5_phrase_query(key: &str) -> String {
    let mut q = String::with_capacity(key.len() + 2);
    q.push('"');
    for ch in key.chars() {
        if ch == '"' {
            q.push('"');
        }
        q.push(ch);
    }
    q.push('"');
    q
}

fn cover_file_name(ns: &str, cover_url: &str) -> Option<String> {
    let prefix = format!("/assets/{ns}/covers/");
    let file = cover_url.strip_prefix(&prefix)?;
    if file.is_empty() || file.contains('/') || file.contains('\\') || file.contains("..") {
        return None;
    }
    Some(file.to_string())
}

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
    ("displayCover", "display_cover"),
    ("displayIntro", "display_intro"),
    ("localEpub", "local_epub"),
    ("splitLongChapter", "split_long_chapter"),
    ("language", "language"),
    ("publisher", "publisher"),
    ("publishedAt", "published_at"),
    ("createdAt", "created_at"),
];

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
