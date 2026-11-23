//! 杂项数据与备份恢复：系统设置、用户配置、净化替换规则、HttpTTS、TXT 目录规则、Zip 备份与恢复

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Result;

use crate::model::Book;

use super::{LruCache, RestoreReport, Storage};

// ---------------- 净化替换规则热缓存（按 cache_tag + ns 隔离） ----------------

const REPLACE_RULES_CACHE_MAX: usize = 64;

type ReplaceRulesCache = LruCache<(u64, String), Arc<Vec<crate::model::ReplaceRule>>>;

fn replace_rules_cache() -> &'static parking_lot::Mutex<ReplaceRulesCache> {
    static M: OnceLock<parking_lot::Mutex<ReplaceRulesCache>> = OnceLock::new();
    M.get_or_init(|| parking_lot::Mutex::new(LruCache::new(REPLACE_RULES_CACHE_MAX)))
}

static REPLACE_RULES_GEN: AtomicU64 = AtomicU64::new(0);

/// 失效指定 Storage 实例下全部命名空间的净化规则缓存（覆盖 default 规则回退可见性）
pub(super) fn invalidate_replace_rules_cache(cache_tag: u64) {
    let mut m = replace_rules_cache().lock();
    REPLACE_RULES_GEN.fetch_add(1, Ordering::Release);
    m.retain(|(tag, _), _| *tag != cache_tag);
}

/// 失效全部实例的净化规则缓存（用户删除等跨表清理路径）
pub(super) fn invalidate_all_replace_rules_cache() {
    let mut m = replace_rules_cache().lock();
    REPLACE_RULES_GEN.fetch_add(1, Ordering::Release);
    m.clear();
}

impl Storage {
    // ---------------- 系统设置（system_settings） ----------------

    pub async fn get_system_setting(&self, key: &str) -> Result<Option<String>> {
        let r: Option<(String,)> =
            sqlx::query_as("SELECT value FROM system_settings WHERE key = ?1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(r.map(|x| x.0))
    }

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

    pub async fn delete_system_setting(&self, key: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM system_settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ---------------- 用户配置（user_config） ----------------

    pub async fn get_user_config(&self, ns: &str, key: &str) -> Result<Option<String>> {
        let r: Option<(String,)> =
            sqlx::query_as("SELECT config FROM user_config WHERE user_namespace = ?1 AND ns = ?2")
                .bind(ns)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(r.map(|x| x.0))
    }

    pub async fn save_user_config(&self, ns: &str, key: &str, config: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_config (user_namespace, ns, config, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(user_namespace, ns) DO UPDATE SET \
             config = excluded.config, updated_at = excluded.updated_at \
             WHERE excluded.updated_at >= user_config.updated_at",
        )
        .bind(ns)
        .bind(key)
        .bind(config)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------------- 净化替换规则（replace_rules） ----------------

    pub async fn get_replace_rules(&self, ns: &str) -> Result<Vec<crate::model::ReplaceRule>> {
        let key = (self.cache_tag, ns.to_string());
        if let Some(cached) = replace_rules_cache().lock().get(&key).cloned() {
            return Ok((*cached).clone());
        }
        let gen = REPLACE_RULES_GEN.load(Ordering::Acquire);
        let rows = sqlx::query_as::<_, crate::model::ReplaceRule>(
            "SELECT * FROM replace_rules WHERE user_namespace = ?1 ORDER BY order_num, id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        let final_rows = if !rows.is_empty() || ns == "default" {
            rows
        } else {
            sqlx::query_as::<_, crate::model::ReplaceRule>(
                "SELECT * FROM replace_rules WHERE user_namespace = 'default' ORDER BY order_num, id",
            )
            .fetch_all(&self.pool)
            .await?
        };
        let arc = Arc::new(final_rows);
        let mut m = replace_rules_cache().lock();
        if REPLACE_RULES_GEN.load(Ordering::Acquire) == gen {
            m.insert(key, arc.clone());
        }
        Ok((*arc).clone())
    }

    pub async fn save_replace_rule(
        &self,
        ns: &str,
        rule: &crate::model::ReplaceRule,
    ) -> Result<String> {
        let mut r = rule.clone();
        self.ensure_rule_id_owned("replace_rules", ns, &mut r.id)
            .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO replace_rules (id, name, group_name, find, replace, scope, \
             scope_title, scope_content, is_regex, timeout_millisecond, enable, order_num, user_namespace) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
        invalidate_replace_rules_cache(self.cache_tag);
        Ok(r.id)
    }

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

    pub async fn save_replace_rules(
        &self,
        ns: &str,
        rules: &[crate::model::ReplaceRule],
    ) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        for rule in rules {
            let mut r = rule.clone();
            self.ensure_rule_id_owned("replace_rules", ns, &mut r.id)
                .await?;
            sqlx::query(
                "INSERT OR REPLACE INTO replace_rules (id, name, group_name, find, replace, scope, \
                 scope_title, scope_content, is_regex, timeout_millisecond, enable, order_num, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
        invalidate_replace_rules_cache(self.cache_tag);
        Ok(())
    }

    pub async fn delete_replace_rule(&self, ns: &str, id: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM replace_rules WHERE user_namespace = ?1 AND id = ?2")
            .bind(ns)
            .bind(id)
            .execute(&self.pool)
            .await?;
        invalidate_replace_rules_cache(self.cache_tag);
        Ok(r.rows_affected())
    }

    pub async fn delete_replace_rules(&self, ns: &str, ids: &[String]) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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
        invalidate_replace_rules_cache(self.cache_tag);
        Ok(total)
    }

    pub async fn delete_all_replace_rules(&self, ns: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM replace_rules WHERE user_namespace = ?1")
            .bind(ns)
            .execute(&self.pool)
            .await?;
        invalidate_replace_rules_cache(self.cache_tag);
        Ok(r.rows_affected())
    }

    // ---------------- HttpTTS 听书源（http_tts_list） ----------------

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

    pub async fn get_http_tts_by_name(
        &self,
        ns: &str,
        name: &str,
    ) -> Result<Option<crate::model::HttpTts>> {
        let tts = sqlx::query_as::<_, crate::model::HttpTts>(
            "SELECT * FROM http_tts_list WHERE user_namespace = ?1 AND name = ?2 ORDER BY rowid LIMIT 1",
        )
        .bind(ns)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(tts)
    }

    pub async fn save_http_tts(&self, ns: &str, tts: &crate::model::HttpTts) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO http_tts_list (url, name, type, content_type, concurrent_rate, \
             login_url, login_ui, header, js_lib, enabled_cookie_jar, login_check_js, \
             last_update_time, user_namespace) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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

    pub async fn save_http_tts_multi(
        &self,
        ns: &str,
        items: &[crate::model::HttpTts],
    ) -> Result<()> {
        let mut tx = self.begin_immediate().await?;
        for tts in items {
            sqlx::query(
                "INSERT OR REPLACE INTO http_tts_list (url, name, type, content_type, concurrent_rate, \
                 login_url, login_ui, header, js_lib, enabled_cookie_jar, login_check_js, \
                 last_update_time, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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

    pub async fn delete_http_tts(&self, ns: &str, url: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM http_tts_list WHERE user_namespace = ?1 AND url = ?2")
            .bind(ns)
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn delete_http_tts_multi(&self, ns: &str, urls: &[String]) -> Result<u64> {
        if urls.is_empty() {
            return Ok(0);
        }
        let mut tx = self.begin_immediate().await?;
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

    // ---------------- 自定义 TXT 目录规则（txt_toc_rules） ----------------

    pub async fn get_txt_toc_rules(&self, ns: &str) -> Result<Vec<crate::model::TxtTocRule>> {
        let rows = sqlx::query_as::<_, crate::model::TxtTocRule>(
            "SELECT * FROM txt_toc_rules WHERE user_namespace = ?1 ORDER BY serial_number, id",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn save_txt_toc_rule(
        &self,
        ns: &str,
        rule: &crate::model::TxtTocRule,
    ) -> Result<String> {
        let mut r = rule.clone();
        self.ensure_rule_id_owned("txt_toc_rules", ns, &mut r.id)
            .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO txt_toc_rules (id, name, rule, enable, serial_number, user_namespace) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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

    pub async fn delete_txt_toc_rule(&self, ns: &str, id: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM txt_toc_rules WHERE user_namespace = ?1 AND id = ?2")
            .bind(ns)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn import_default_txt_toc_rules(&self, ns: &str) -> Result<usize> {
        let defaults = crate::service::local_book::DEFAULT_TOC_RULE_DEFS;
        let mut tx = self.begin_immediate().await?;
        for def in defaults {
            sqlx::query(
                "INSERT OR REPLACE INTO txt_toc_rules (id, name, rule, enable, serial_number, user_namespace) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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

    // ---------------- WebDAV 备份与恢复 ----------------

    pub async fn create_backup_zip(&self, ns: &str) -> Result<String> {
        let ts = chrono::Utc::now().format("%Y-%m-%d-%H%M%S");
        let legado = self.webdav_legado_dir(ns);
        let mut stem = format!("backup-{ts}");
        let mut seq = 1;
        while tokio::fs::try_exists(legado.join(format!("{stem}.zip")))
            .await
            .unwrap_or(false)
        {
            stem = format!("backup-{ts}-{seq}");
            seq += 1;
        }
        self.write_backup_zip(ns, &stem).await
    }

    fn webdav_legado_dir(&self, ns: &str) -> std::path::PathBuf {
        self.config
            .storage_dir()
            .join("data")
            .join(ns)
            .join("webdav")
            .join("legado")
    }

    pub(crate) async fn write_backup_zip(&self, ns: &str, stem: &str) -> Result<String> {
        let legado = self.webdav_legado_dir(ns);
        tokio::fs::create_dir_all(&legado).await?;
        let zip_path = legado.join(format!("{stem}.zip"));

        let books = self.list_books(ns).await?;
        let sources = self.get_book_sources(ns).await?;
        let bookmarks = sqlx::query_as::<_, crate::model::Bookmark>(
            "SELECT * FROM bookmarks WHERE user_namespace = ?1 ORDER BY created_at DESC, rowid DESC",
        )
        .bind(ns)
        .fetch_all(&self.pool)
        .await?;
        let groups = self.list_book_groups(ns).await?;
        let replace_rules = self.get_replace_rules(ns).await?;
        let txt_toc_rules = self.get_txt_toc_rules(ns).await?;
        let http_tts_list = self.get_http_tts_list(ns).await?;
        let cookies = self.list_cookies(ns).await?;
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

        let zip_path_for_task = zip_path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = std::fs::File::create(&zip_path_for_task)?;
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
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("备份写入任务异常: {e}"))??;

        tracing::info!("备份完成 [{ns}]: {}", zip_path.display());
        Ok(zip_path.to_string_lossy().into_owned())
    }

    /// 清理自动备份 zip，保留最近 keep 份
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

    /// 从备份 zip 字节恢复（兼容根路径与 `config/` 子目录布局）
    pub async fn restore_backup_zip(
        &self,
        ns: &str,
        zip_bytes: &[u8],
        overwrite: bool,
    ) -> Result<RestoreReport> {
        let zip_vec = zip_bytes.to_vec();
        let entries: std::collections::HashMap<String, Vec<u8>> =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let cursor = std::io::Cursor::new(zip_vec);
                let mut archive = zip::ZipArchive::new(cursor)
                    .map_err(|e| anyhow::anyhow!("备份文件不是有效的 zip：{e}"))?;
                let mut map: std::collections::HashMap<String, Vec<u8>> =
                    std::collections::HashMap::new();
                use std::io::Read;
                for i in 0..archive.len() {
                    let mut f = archive.by_index(i)?;
                    let name = f.name().to_string();
                    let mut bytes = Vec::new();
                    f.read_to_end(&mut bytes)?;
                    map.insert(name, bytes);
                }
                Ok(map)
            })
            .await
            .map_err(|e| anyhow::anyhow!("备份解压任务异常: {e}"))??;

        let entry = |name: &str| -> Option<&Vec<u8>> {
            entries
                .get(name)
                .or_else(|| entries.get(&format!("config/{name}")))
        };

        let recognized = [
            "bookSource.json",
            "bookshelf.json",
            "books.json",
            "bookGroup.json",
            "replaceRule.json",
            "txtTocRule.json",
            "userConfig.json",
            "httpTTS.json",
            "bookmark.json",
            "bookSourceCookies.json",
        ];
        if !recognized.iter().any(|n| entry(n).is_some()) {
            return Err(anyhow::anyhow!("备份文件中没有可恢复的数据"));
        }

        let mut report = RestoreReport::default();

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

        if let Some(bytes) = entry("bookGroup.json") {
            match serde_json::from_slice::<Vec<crate::model::BookGroup>>(bytes) {
                Ok(items) => {
                    for mut g in items {
                        if g.name.trim().is_empty() {
                            report.skipped.groups += 1;
                            continue;
                        }
                        if g.id > 0 {
                            let owner: Option<String> = sqlx::query_scalar(
                                "SELECT user_namespace FROM book_groups WHERE id = ?1",
                            )
                            .bind(g.id)
                            .fetch_optional(&self.pool)
                            .await?;
                            match owner.as_deref() {
                                Some(o) if o == ns => {
                                    if !overwrite {
                                        report.skipped.groups += 1;
                                        continue;
                                    }
                                }
                                Some(_) => {
                                    // 该 id 已被其他命名空间占用，恢复到当前用户时分配新 id
                                    g.id = 0;
                                }
                                None => {}
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

        if let Some(bytes) = entry("bookSourceCookies.json") {
            match serde_json::from_slice::<Vec<crate::model::CookieRow>>(bytes) {
                Ok(items) => {
                    for c in items {
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
                            super::sources::invalidate_cookie_snapshots_only();
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

    async fn table_exists(&self, sql: &str, a: &str, b: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(sql)
            .bind(a)
            .bind(b)
            .fetch_optional(&self.pool)
            .await?
            .is_some())
    }
}

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
