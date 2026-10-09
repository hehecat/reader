//! reader-dev 入口
//!
//! 单二进制双形态（feature gate）：
//! - 默认构建 = CLI 服务端（musl 静态可用）
//! - `--features gui` = 桌面变体：自动选端口起服务 + 窗口 + 托盘常驻
//!   （关窗收托盘；`--headless` 强制纯服务模式）

use anyhow::Result;

#[cfg(feature = "gui")]
mod gui;

fn main() -> Result<()> {
    // .env 先加载——RUST_LOG / READER_LOG_DIR 等日志 env 才能生效
    dotenvy::dotenv().ok();

    // 自建部署 opt-in: 书源回源放行私网/回环 (LAN/本机书源服务常见, 如 docker 桥接宿主机);
    // 默认关闭保持 SSRF 防护. 仅影响书源抓取回源, 不影响鉴权.
    if std::env::var("SSRF_ALLOW_PRIVATE").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")) {
        reader_dev::service::crawler::SSRF_ALLOW_PRIVATE.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!("SSRF_ALLOW_PRIVATE=on: 书源回源放行私网/回环目标 (自建部署专用, 公网部署勿开)");
    }

    init_tracing();

    let config = reader_dev::AppConfig::from_env();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // GAP 148：数据库迁移/升级前自动备份（storage 初始化之前）
    rt.block_on(async {
        match reader_dev::util::db_backup::backup_reader_db(&config.storage_dir()).await {
            Ok(Some(path)) => tracing::info!("启动备份完成: {}", path.display()),
            Ok(None) => {}
            Err(e) => tracing::warn!("启动数据库备份失败（继续启动）: {e}"),
        }
    });

    // 离线词典: 数据存在则后台导入 dict.sqlite(不阻塞启动)
    reader_dev::service::dict::spawn_build_if_needed(&config.storage_dir());

    // GUI 分派：feature=gui 且未显式 --headless 时进窗口模式
    // 事件循环必须在主线程（Windows/macOS）
    #[cfg(feature = "gui")]
    {
        if !std::env::args().any(|a| a == "--headless") {
            return gui::run(config);
        }
    }

    rt.block_on(config.serve())
}

/// 日志初始化（GAP 114：长期运行日志增长）
///
/// 现状：默认仅控制台输出（tracing_subscriber fmt，级别由 `RUST_LOG` 控制，默认 info）——
/// 进程本身不写日志文件，不存在单文件无限增长；若部署时重定向 stdout 到文件，
/// 文件增长由外部工具（如 Linux logrotate / Windows 计划任务）处理。
///
/// 文件轮转（可选）：设置环境变量后启用「控制台 + 文件」双写，文件按大小轮转：
/// - `READER_LOG_DIR`：日志目录（设置后启用文件日志；默认空 = 仅控制台）
/// - `READER_LOG_MAX_SIZE_MB`：单文件大小上限（默认 10 MB）
/// - `READER_LOG_MAX_FILES`：保留的历史文件数（默认 7，超出删最旧）
///
/// 说明：tracing-appender 只支持按时间轮转（无按大小），故自实现
/// `RotatingFileWriter`（按字节数轮转，无额外依赖）。
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let log_dir = std::env::var("READER_LOG_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let Some(log_dir) = log_dir else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
        return;
    };

    let max_size_mb = std::env::var("READER_LOG_MAX_SIZE_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10)
        .clamp(1, 1024);
    let max_files = std::env::var("READER_LOG_MAX_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(7)
        .clamp(1, 100);

    let file_writer = std::sync::Mutex::new(RotatingFileWriter::new(
        &log_dir,
        "reader-dev",
        "log",
        max_size_mb * 1024 * 1024,
        max_files,
    ));

    use tracing_subscriber::layer::{Layer, SubscriberExt};
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer)
                .with_filter(filter.clone()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_filter(filter),
        )
        .init();
    tracing::info!("文件日志已启用: {log_dir}（{max_size_mb}MB × {max_files} 个轮转）");
}

/// 按大小轮转的日志 writer（tracing-appender 无按大小轮转，自实现）
///
/// 文件命名：`{dir}/{prefix}.{suffix}`（当前）、`{dir}/{prefix}.{suffix}.1` …（历史，编号越大越旧）。
/// 当前文件超过 `max_bytes` 时轮转：`.{i} → .{i+1}` 平移，超出 `max_files` 的历史删除。
/// 直接写 File（不加 BufWriter）——日志事件即时落盘，便于 tail 观察。
struct RotatingFileWriter {
    dir: std::path::PathBuf,
    prefix: String,
    suffix: String,
    max_bytes: u64,
    max_files: usize,
    current: Option<std::fs::File>,
    current_bytes: u64,
}

impl RotatingFileWriter {
    fn new(
        dir: impl Into<std::path::PathBuf>,
        prefix: &str,
        suffix: &str,
        max_bytes: u64,
        max_files: usize,
    ) -> Self {
        Self {
            dir: dir.into(),
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
            max_bytes,
            max_files: max_files.max(1),
            current: None,
            current_bytes: 0,
        }
    }

    fn log_path(&self, n: usize) -> std::path::PathBuf {
        let name = if n == 0 {
            format!("{}.{}", self.prefix, self.suffix)
        } else {
            format!("{}.{}.{}", self.prefix, self.suffix, n)
        };
        self.dir.join(name)
    }

    fn ensure_open(&mut self) -> std::io::Result<&mut std::fs::File> {
        if self.current.is_none() {
            std::fs::create_dir_all(&self.dir)?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.log_path(0))?;
            self.current_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
            self.current = Some(file);
        }
        Ok(self.current.as_mut().expect("just opened"))
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        // 关掉当前文件
        self.current.take();
        // 历史平移：.i → .i+1（从最旧开始），超出 max_files 的直接删除
        for i in (1..self.max_files).rev() {
            let from = self.log_path(i);
            let to = self.log_path(i + 1);
            if from.exists() {
                let _ = std::fs::rename(&from, &to); // 单文件失败不阻塞整体轮转
            }
        }
        let base = self.log_path(0);
        if base.exists() {
            std::fs::rename(&base, self.log_path(1))?;
        }
        self.current_bytes = 0;
        Ok(())
    }
}

impl std::io::Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.ensure_open()?.write(buf)?;
        self.current_bytes += n as u64;
        if self.current_bytes >= self.max_bytes {
            self.rotate()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(w) = self.current.as_mut() {
            w.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn rotating_writer_rotates_by_size_and_bounds_file_count() {
        let dir = std::env::temp_dir().join(format!("reader-dev-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = RotatingFileWriter::new(&dir, "test", "log", 100, 3);
        for i in 0..10 {
            let line = format!("line {i}: {}\n", "x".repeat(40));
            w.write_all(line.as_bytes()).unwrap();
        }
        w.flush().unwrap();

        // 确实发生过轮转（有历史文件）
        assert!(dir.join("test.log.1").exists(), "should have rotated");
        // 当前文件大小不超过上限
        let cur = std::fs::metadata(dir.join("test.log")).unwrap().len();
        assert!(cur < 100, "current file {cur} bytes >= max");
        // 文件总数 = 当前 + 历史 ≤ max_files + 1
        let count = std::fs::read_dir(&dir).unwrap().count();
        assert!(count <= 4, "file count {count} > max_files + 1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
