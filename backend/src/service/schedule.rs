//! 定时任务（书架章节更新检查 + 每日自动备份）
//!
//! - `spawn_schedule_jobs`：书架更新循环（每 10 分钟检查可更新书籍并回写最新章节）
//! - `spawn_auto_backup_job`：每天 READER_AUTO_BACKUP_HOUR（默认 03:00）自动备份
//!   各命名空间到 webdav/legado/auto-YYYYMMDD.zip，保留最近 7 份

use std::time::Duration;

use anyhow::Result;
use chrono::Timelike;

use crate::storage::Storage;

/// 自动备份保留份数（最近 7 份）
const AUTO_BACKUP_KEEP: usize = 7;

/// 启动定时任务（书架更新；在 lib.rs serve 时调用一次）
pub fn spawn_schedule_jobs(storage: Storage) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match crate::storage::run_shelf_update(&storage).await {
                Ok(n) => tracing::info!("书架更新检查完成：更新 {n} 本"),
                Err(e) => tracing::warn!("书架更新检查失败: {e:#}"),
            }
        }
    });
}

/// 启动自动备份定时任务
///
/// 每天 READER_AUTO_BACKUP_HOUR（0-23，默认 3 = 03:00）触发一次（同日不重复）；
/// 环境变量为空 → 默认 3；非法值 → 告警并回退默认。
pub fn spawn_auto_backup_job(storage: Storage) {
    let raw = std::env::var("READER_AUTO_BACKUP_HOUR").unwrap_or_default();
    let hour: i64 = if raw.trim().is_empty() {
        3
    } else {
        match raw.trim().parse::<i64>() {
            Ok(h) if (0..=23).contains(&h) => h,
            _ => {
                tracing::warn!("READER_AUTO_BACKUP_HOUR 非法（{raw}），使用默认 3");
                3
            }
        }
    };
    tracing::info!("自动备份定时任务：每天 {hour:02}:00 执行");
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_date: Option<chrono::NaiveDate> = None;
        loop {
            interval.tick().await;
            let now = chrono::Local::now();
            if now.hour() as i64 != hour {
                continue;
            }
            if last_date == Some(now.date_naive()) {
                continue; // 当天已备份
            }
            match run_auto_backup(&storage).await {
                Ok(n) => {
                    tracing::info!("自动备份完成：{} 个命名空间", n);
                    last_date = Some(now.date_naive());
                }
                Err(e) => tracing::warn!("自动备份失败: {e:#}"),
            }
        }
    });
}

/// 自动备份：各命名空间 → webdav/legado/auto-YYYYMMDD.zip（同日跳过）→
/// 保留最近 7 份（删除更旧的 auto-*.zip）。返回执行备份的命名空间数。
pub async fn run_auto_backup(storage: &Storage) -> Result<usize> {
    let nss = storage.schedule_namespaces().await;
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    let mut done = 0usize;
    for ns in nss {
        let legado = storage
            .config
            .storage_dir()
            .join("data")
            .join(&ns)
            .join("webdav")
            .join("legado");
        // 同日已备份 → 跳过（重启/多轮 tick 幂等）
        if legado.join(format!("auto-{today}.zip")).exists() {
            continue;
        }
        match storage
            .write_backup_zip(&ns, &format!("auto-{today}"))
            .await
        {
            Ok(path) => {
                tracing::info!("自动备份 [{ns}]: {}", path);
                done += 1;
            }
            Err(e) => tracing::warn!("自动备份 [{ns}] 失败: {e:#}"),
        }
        // 保留最近 7 份（单命名空间失败不影响清理）
        storage.prune_auto_backups(&ns, AUTO_BACKUP_KEEP);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use crate::storage::Storage;

    async fn test_storage(tag: &str) -> (Storage, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("reader-schedule-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = crate::storage::init(&config).await.unwrap();
        (storage, dir)
    }

    async fn cleanup(storage: Storage, dir: std::path::PathBuf) {
        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 命名空间集合（非 secure → default；secure → 用户列表）
    #[tokio::test]
    async fn test_schedule_namespaces_non_secure() {
        let (mut storage, dir) = test_storage("ns").await;
        assert_eq!(storage.schedule_namespaces().await, vec!["default"]);
        storage
            .insert_user(&crate::model::User {
                username: "alice".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        storage.config.secure = true;
        let nss = storage.schedule_namespaces().await;
        assert!(nss.contains(&"alice".to_string()));
        cleanup(storage, dir).await;
    }
}
