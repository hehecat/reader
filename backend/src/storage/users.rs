//! 用户账号、会话 Token、权限与命名空间管理

use anyhow::Result;

use crate::model::User;

use super::{invalidate_cookie_snapshots, Storage};

/// 显式列名投影：跳过已废弃的 `raw_json` 列（兼容无该列的新库，同时避免旧库读取冗余大字段）
macro_rules! user_select_cols {
    () => {
        "username, password, salt, token, token_map, \
         enable_webdav, enable_local_store, enable_book_source, enable_rss_source, \
         book_source_limit, book_limit, is_admin, disabled, last_login_at, created_at, \
         user_namespace, CAST(NULL AS TEXT) AS raw_json"
    };
}

impl Storage {
    /// 按用户名查用户（登录 / token 校验）
    pub async fn find_user(&self, username: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(concat!(
            "SELECT ",
            user_select_cols!(),
            " FROM users WHERE username = ?1"
        ))
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(user)
    }

    /// 用户总数（注册上限校验）
    pub async fn count_users(&self) -> Result<i64> {
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 在线会话数（活跃 token 总数）
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

    /// 新建/覆盖用户
    pub async fn insert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO users
                (username, password, salt, token, enable_webdav, enable_local_store,
                 enable_book_source, enable_rss_source, book_source_limit, book_limit,
                 is_admin, last_login_at, created_at, user_namespace, disabled, token_map)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
        .bind(user.disabled)
        .bind(user.token_map.as_ref().map(|v| v.to_string()))
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

    /// 登录：追加 token 到 token_map（上限 5，最旧被丢），同时刷新主 token 与 last_login_at
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

    /// 登出：从 token_map 移除指定 token（其他设备 token 不受影响）
    pub async fn remove_user_token(&self, username: &str, token: &str) -> Result<u64> {
        let Some(user) = self.find_user(username).await? else {
            return Ok(0);
        };
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

    /// 用户书籍上限（users.book_limit；用户不存在返回 None）
    pub async fn book_limit_for(&self, ns: &str) -> Result<Option<i64>> {
        let limit = sqlx::query_scalar("SELECT book_limit FROM users WHERE username = ?1")
            .bind(ns)
            .fetch_optional(&self.pool)
            .await?;
        Ok(limit)
    }

    /// 用户书源上限（users.book_source_limit；用户不存在返回 None）
    pub async fn book_source_limit_for(&self, ns: &str) -> Result<Option<i64>> {
        let limit = sqlx::query_scalar("SELECT book_source_limit FROM users WHERE username = ?1")
            .bind(ns)
            .fetch_optional(&self.pool)
            .await?;
        Ok(limit)
    }

    /// 退出登录：清空用户 token 与 token_map
    pub async fn logout_user(&self, username: &str) -> Result<u64> {
        let r = sqlx::query("UPDATE users SET token = '', token_map = '[]' WHERE username = ?1")
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 清理不活跃用户（users 行 + 用户级数据行 + 数据目录；except 用户除外）
    pub async fn clear_inactive_users(
        &self,
        before_ms: i64,
        except: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut tx = self.begin_immediate().await?;
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

    /// 全部用户列表（按创建时间排序）
    pub async fn list_users(&self) -> Result<Vec<User>> {
        let users = sqlx::query_as::<_, User>(concat!(
            "SELECT ",
            user_select_cols!(),
            " FROM users ORDER BY created_at, username"
        ))
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
                enable_webdav      = COALESCE(?1, enable_webdav),
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

    /// 管理员数量
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
        let user = sqlx::query_as::<_, User>(concat!(
            "SELECT ",
            user_select_cols!(),
            " FROM users ORDER BY CASE WHEN username = 'admin' THEN 0 ELSE 1 END, created_at, username LIMIT 1"
        ))
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

    /// 删除用户并清理全部用户数据（用户级表行 + storage/data/{username} 目录）
    pub async fn delete_user(&self, username: &str) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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

    /// 批量删除用户（单事务）
    pub async fn delete_users(&self, usernames: &[String]) -> Result<u64> {
        let mut tx = self.begin_immediate().await?;
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

    fn remove_user_data_dir(&self, username: &str) {
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
            Err(e) => tracing::warn!("删除用户数据目录失败 {}: {e}", dir.display()),
        }
    }

    /// 重置用户密码（新 salt + 加密密码；清空 token 与 token_map 使旧会话立即失效）
    pub async fn reset_user_password(
        &self,
        username: &str,
        salt: &str,
        encrypted_password: &str,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE users SET password = ?1, salt = ?2, token = '', token_map = '[]' WHERE username = ?3",
        )
        .bind(encrypted_password)
        .bind(salt)
        .bind(username)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 密码哈希升级（MD5 → argon2id PHC）：仅更新 password 列，不使当前会话失效
    pub async fn upgrade_user_password_hash(&self, username: &str, phc: &str) -> Result<u64> {
        let r = sqlx::query("UPDATE users SET password = ?1 WHERE username = ?2")
            .bind(phc)
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
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
        let default_dir = self.config.storage_dir().join("data").join("default");
        if default_dir.is_dir() && !nss.iter().any(|n| n == "default") {
            nss.push("default".to_string());
        }
        nss
    }
}

/// 删除指定用户命名空间下的全部用户级数据行（事务内调用）
async fn delete_user_rows(tx: &mut super::ImmediateTx, username: &str) -> Result<()> {
    for table in [
        "book_chapters",
        "toc_cache",
        "book_info_cache",
        "books",
        "book_sources",
        "book_source_cookies",
        "book_source_candidates",
        "source_stats",
        "js_cache",
        "book_vars_cache",
        "search_cache",
        "user_config",
        "bookmarks",
        "book_groups",
        "replace_rules",
        "http_tts_list",
        "txt_toc_rules",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE user_namespace = ?1"))
            .bind(username)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}
