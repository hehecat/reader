//! 本地书目录监听同步（仅支持 `.epub` / `.txt`）
//!
//! - 正常读写全走 DB：书架/目录/正文均读 `books` + `book_chapters` 表；
//! - 文件变更自动同步进 DB：书仓目录（`storage/data/{ns}/books/` + 可选环境变量
//!   `READER_LOCAL_BOOK_DIR`）由 notify 后台监听（300ms 去抖批量），事件后对账；
//!
//! 对账任务（启动时 + 文件事件后，幂等，每命名空间互斥锁）：
//! 1. 新文件（无 `local_file` 记录）→ 自动导入（`parse_file_bytes` → `book_chapters` + 元数据）
//! 2. 文件修改（mtime/大小变化）→ 重扫（重新解析，`replace_chapters` 替换章节 + 元数据 patch）
//! 3. 文件删除 → 书籍保留（进度/书签/章节不丢），置 `local_file_deleted = 1`（文件重现时自动重链重扫）

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::service::local_book::{self, ImportedBook};
use crate::storage::Storage;

/// 事件去抖窗口（300ms 批量——连续写入合并为一次对账）
const DEBOUNCE_MS: Duration = Duration::from_millis(300);
/// 书仓目录名（storage/data/{ns}/books/）
const BOOKS_DIR: &str = "books";
/// 书仓支持的文件格式（仅核心 EPUB + TXT）
const SYNC_EXTENSIONS: &[&str] = &["epub", "txt"];

// ---------------------------------------------------------------------------
// 启动入口
// ---------------------------------------------------------------------------

/// 启动本地书目录同步（lib.rs serve() 时调用一次）：
/// 1. notify 文件监听（storage/data 递归 + env READER_LOCAL_BOOK_DIR 可选）
/// 2. 初始对账（启动时）
/// 3. 事件去抖循环（300ms 批量 → 受影响命名空间对账）
pub fn spawn_local_sync(storage: Storage) {
    tokio::spawn(async move {
        let data_dir = storage.config.storage_dir().join("data");
        if let Err(e) = tokio::fs::create_dir_all(&data_dir).await {
            tracing::warn!("本地书同步：data 目录创建失败: {e}");
        }
        // ① 文件监听（先于初始对账——避免初始对账期间漏事件）
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        spawn_watcher(&storage, event_tx);
        // ② 初始对账（启动时；幂等）
        if let Err(e) = run_reconcile_all(&storage).await {
            tracing::error!("本地书初始对账失败: {e:#}");
        }
        // ③ 事件去抖循环：每个命名空间独立计时，300ms 无新事件 → 对账一次
        let mut pending: HashMap<String, Instant> = HashMap::new();
        loop {
            let wait = pending
                .values()
                .map(|t| (*t + DEBOUNCE_MS).saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(DEBOUNCE_MS);
            tokio::select! {
                _ = tokio::time::sleep(wait.max(Duration::ZERO)) => {
                    let nss: Vec<String> = pending.keys().cloned().collect();
                    pending.clear();
                    for ns in nss {
                        let storage = storage.clone();
                        tokio::spawn(async move {
                            let lock = namespace_sync_lock(&ns);
                            let _guard = lock.lock().await;
                            if let Err(e) = reconcile_namespace(&storage, &ns).await {
                                tracing::warn!("本地书对账失败 [{ns}]: {e:#}");
                            }
                        });
                    }
                }
                Some(ns) = event_rx.recv() => {
                    pending.insert(ns, Instant::now());
                }
                else => break,
            }
        }
    });
}

/// notify watcher 全局保活（Drop 即停止投递事件）
static KEEPALIVE_WATCHERS: parking_lot::Mutex<Vec<notify::RecommendedWatcher>> =
    parking_lot::Mutex::new(Vec::new());

fn spawn_watcher(storage: &Storage, event_tx: tokio::sync::mpsc::UnboundedSender<String>) {
    use notify::Watcher as _;
    let data_dir = storage.config.storage_dir().join("data");
    let env_dir = std::env::var("READER_LOCAL_BOOK_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut roots: Vec<PathBuf> = vec![data_dir.clone()];
    if let Some(d) = &env_dir {
        if !roots.iter().any(|r| r == d) {
            roots.push(d.clone());
        }
    }
    for root in roots {
        let tx = event_tx.clone();
        let (std_tx, std_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let _ = std_tx.send(res);
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("本地书监听创建失败（{}）: {e}", root.display());
                    continue;
                }
            };
        if let Err(e) = watcher.watch(&root, notify::RecursiveMode::Recursive) {
            tracing::warn!("本地书监听目录失败（{}）: {e}", root.display());
            continue;
        }
        KEEPALIVE_WATCHERS.lock().push(watcher);
        let root2 = root.clone();
        let data2 = data_dir.clone();
        std::thread::spawn(move || {
            for res in std_rx {
                let Ok(ev) = res else { continue };
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    continue;
                }
                let mut nss = HashSet::new();
                for p in ev.paths {
                    if let Some(ns) = namespace_of_path(&p, &data2, &root2) {
                        nss.insert(ns);
                    }
                }
                for ns in nss {
                    let _ = tx.send(ns);
                }
            }
        });
        tracing::info!("本地书监听已启动: {}", root.display());
    }
}

/// 事件路径 → 命名空间：
/// - `storage/data/{ns}/books/...`（路径含 books 组件）→ ns
/// - env READER_LOCAL_BOOK_DIR 根下（不在 data 子树内）→ "default"
/// - 其他目录 → None（不触发对账）
fn namespace_of_path(p: &Path, data_dir: &Path, root: &Path) -> Option<String> {
    if p.starts_with(root) && !p.starts_with(data_dir) {
        return Some("default".to_string());
    }
    let rel = p.strip_prefix(data_dir).ok()?;
    let mut comps = rel.components();
    let ns = comps.next()?.as_os_str().to_string_lossy().into_owned();
    let rest: Vec<String> = comps
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if rest.iter().any(|c| c == BOOKS_DIR) {
        Some(ns)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 对账任务
// ---------------------------------------------------------------------------

/// 每命名空间对账互斥锁（避免并发重扫/重链交错）
pub fn namespace_sync_lock(ns: &str) -> Arc<Mutex<()>> {
    static LOCKS: std::sync::LazyLock<parking_lot::Mutex<HashMap<String, Arc<Mutex<()>>>>> =
        std::sync::LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));
    LOCKS
        .lock()
        .entry(ns.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// 对账全部命名空间（启动时）。返回有变更的命名空间数。
pub async fn run_reconcile_all(storage: &Storage) -> Result<usize> {
    let nss = storage.schedule_namespaces().await;
    let mut changed = 0usize;
    for ns in nss {
        let lock = namespace_sync_lock(&ns);
        let _guard = lock.lock().await;
        if reconcile_namespace(storage, &ns).await? {
            changed += 1;
        }
    }
    Ok(changed)
}

/// 单命名空间对账（幂等；文件事件后 / 启动时调用）。返回是否有任何变更。
pub async fn reconcile_namespace(storage: &Storage, ns: &str) -> Result<bool> {
    let env_dirs: Vec<PathBuf> = std::env::var("READER_LOCAL_BOOK_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .into_iter()
        .collect();
    reconcile_namespace_dirs(storage, ns, &env_dirs).await
}

/// 对账核心（env_dirs 为额外书仓目录；测试直接传入临时目录）
async fn reconcile_namespace_dirs(
    storage: &Storage,
    ns: &str,
    env_dirs: &[PathBuf],
) -> Result<bool> {
    let books_dir = storage
        .config
        .storage_dir()
        .join("data")
        .join(ns)
        .join(BOOKS_DIR);
    tokio::fs::create_dir_all(&books_dir).await?;
    let env_books: Vec<PathBuf> = if ns == "default" {
        env_dirs.to_vec()
    } else {
        Vec::new()
    };

    let mut changed = false;

    // ① 磁盘文件清单（books_dir 递归 + env 目录递归）
    let mut files: Vec<PathBuf> = Vec::new();
    collect_book_files(&books_dir, &mut files).await;
    for d in &env_books {
        collect_book_files(d, &mut files).await;
    }

    // ② DB 关联清单（含已删除标记的——文件重现时重链）
    let linked = storage.list_linked_local_books(ns).await?;
    let mut linked_by_path: HashMap<String, crate::model::Book> = HashMap::new();
    for b in &linked {
        if let Some(p) = &b.local_file {
            linked_by_path.insert(normalize_path(p), b.clone());
        }
    }

    // ③ 逐文件对账（新文件导入 / 修改重扫 / 重现重链）
    let user_rules: Vec<String> = storage
        .get_txt_toc_rules(ns)
        .await
        .ok()
        .map(|rules| {
            rules
                .into_iter()
                .filter(|r| r.enable)
                .map(|r| r.rule)
                .collect()
        })
        .unwrap_or_default();
    for path in &files {
        let key = normalize_path(&path.to_string_lossy());
        crate::service::fs_rate::tick().await;
        let meta = match tokio::fs::metadata(path).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = file_mtime_ms(&meta);
        let size = meta.len() as i64;
        match linked_by_path.get(&key).cloned() {
            Some(book) => {
                if book.local_file_deleted
                    || book.local_file_mtime != mtime
                    || book.local_file_size != size
                {
                    match reparse_and_update(storage, ns, &book, path, mtime, size).await {
                        Ok(()) => changed = true,
                        Err(e) => {
                            tracing::warn!("本地书重扫失败 [{}] {}: {e:#}", ns, path.display())
                        }
                    }
                }
            }
            None => match import_file(storage, ns, path, &user_rules, mtime, size).await {
                Ok(()) => changed = true,
                Err(e) => {
                    tracing::warn!("本地书导入失败 [{}] {}: {e:#}", ns, path.display())
                }
            },
        }
    }

    // ④ 已关联但文件消失 → 标记删除（保留书籍/进度/章节）
    let disk_keys: HashSet<String> = files
        .iter()
        .map(|p| normalize_path(&p.to_string_lossy()))
        .collect();
    for book in &linked {
        let Some(p) = &book.local_file else { continue };
        let key = normalize_path(p);
        if !disk_keys.contains(&key) && !book.local_file_deleted {
            storage
                .link_local_file(
                    ns,
                    &book.book_url,
                    Some(p),
                    book.local_file_mtime,
                    book.local_file_size,
                    true,
                )
                .await?;
            tracing::info!(
                "本地书文件已删除，书籍保留 [{}] {}（local_file_deleted=1）",
                ns,
                book.name
            );
            changed = true;
        }
    }

    Ok(changed)
}

// ---------------------------------------------------------------------------
// ① 新文件导入 / ② 修改重扫
// ---------------------------------------------------------------------------

async fn import_file(
    storage: &Storage,
    ns: &str,
    path: &Path,
    user_rules: &[String],
    mtime: i64,
    size: i64,
) -> Result<()> {
    let ext = local_book::file_ext(&path.to_string_lossy());
    if !SYNC_EXTENSIONS.contains(&ext.as_str()) {
        return Ok(());
    }
    crate::service::fs_rate::tick().await;
    let bytes = tokio::fs::read(path).await?;
    let ext_owned = ext.clone();
    let rules_owned = user_rules.to_vec();
    let imported = tokio::task::spawn_blocking(move || {
        local_book::parse_file_bytes(
            &bytes,
            &ext_owned,
            &rules_owned,
            local_book::DEFAULT_EPUB_TOC_MODE,
            false,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("解析任务异常: {e}"))?
    .map_err(|e| anyhow::anyhow!("解析失败: {e:#}"))?;
    if imported.chapters.is_empty() {
        anyhow::bail!("未解析到章节内容");
    }
    let book_url = format!("local://{}", uuid::Uuid::new_v4());
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (fn_name, fn_author) = local_book::analyze_name_author(&file_name);
    let name = if ext == "txt" && !fn_name.is_empty() {
        fn_name
    } else if !imported.meta.title.is_empty() {
        imported.meta.title.clone()
    } else if !fn_name.is_empty() {
        fn_name
    } else {
        "本地书籍".to_string()
    };
    let author = if !imported.meta.author.is_empty() {
        imported.meta.author.clone()
    } else {
        fn_author
    };
    let info = crate::model::book_chapter::BookInfo {
        name,
        author,
        kind: imported.meta.subjects.first().cloned(),
        intro: imported.meta.description.clone(),
        language: imported.meta.language.clone(),
        publisher: imported.meta.publisher.clone(),
        published_at: imported.meta.published_at.clone(),
        toc_url: Some(format!("{book_url}/toc")),
        book_url: book_url.clone(),
        origin: "local".to_string(),
        origin_name: "本地书".to_string(),
        ..Default::default()
    };
    storage.save_local_book(ns, &info, &imported).await?;
    if let Some(cover) = &imported.cover {
        let cover_dir = storage
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
            let _ = storage
                .update_book_cover(ns, &book_url, &format!("/assets/{ns}/covers/{file_id}"))
                .await;
        }
    }
    storage
        .link_local_file(
            ns,
            &book_url,
            Some(&normalize_path(&path.to_string_lossy())),
            mtime,
            size,
            false,
        )
        .await?;
    tracing::info!(
        "本地书自动导入 [{}] {}（{} 章）← {}",
        ns,
        info.name,
        imported.chapters.len(),
        path.display()
    );
    Ok(())
}

async fn reparse_and_update(
    storage: &Storage,
    ns: &str,
    book: &crate::model::Book,
    path: &Path,
    mtime: i64,
    size: i64,
) -> Result<()> {
    let ext = local_book::file_ext(&path.to_string_lossy());
    crate::service::fs_rate::tick().await;
    let bytes = tokio::fs::read(path).await?;
    let toc_url_owned = book.toc_url.clone();
    let split_long = book.split_long_chapter;
    let imported: ImportedBook = tokio::task::spawn_blocking(move || {
        local_book::parse_file_bytes(&bytes, &ext, &[], &toc_url_owned, split_long)
    })
    .await
    .map_err(|e| anyhow::anyhow!("解析任务异常: {e}"))?
    .map_err(|e| anyhow::anyhow!("解析失败: {e:#}"))?;
    if imported.chapters.is_empty() {
        anyhow::bail!("未解析到章节内容");
    }
    let pairs: Vec<(String, String)> = imported
        .chapters
        .iter()
        .map(|c| (c.title.clone(), c.content.clone()))
        .collect();
    storage.replace_chapters(ns, &book.book_url, &pairs).await?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        "totalChapterNum".to_string(),
        serde_json::json!(pairs.len() as i64),
    );
    if book.name.is_empty() && !imported.meta.title.is_empty() {
        patch.insert("name".to_string(), serde_json::json!(imported.meta.title));
    }
    if !imported.meta.author.is_empty() {
        patch.insert(
            "author".to_string(),
            serde_json::json!(imported.meta.author),
        );
    }
    if let Some(d) = &imported.meta.description {
        if !d.is_empty() {
            patch.insert("intro".to_string(), serde_json::json!(d));
        }
    }
    if let Some(l) = &imported.meta.language {
        patch.insert("language".to_string(), serde_json::json!(l));
    }
    if let Some(p) = &imported.meta.publisher {
        patch.insert("publisher".to_string(), serde_json::json!(p));
    }
    if let Some(d) = &imported.meta.published_at {
        patch.insert("publishedAt".to_string(), serde_json::json!(d));
    }
    if let Some(k) = imported.meta.subjects.first() {
        patch.insert("kind".to_string(), serde_json::json!(k));
    }
    let _ = storage.patch_book(ns, &book.book_url, &patch).await;
    if let Some(cover) = &imported.cover {
        let cover_dir = storage
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
            let _ = storage
                .update_book_cover(
                    ns,
                    &book.book_url,
                    &format!("/assets/{ns}/covers/{file_id}"),
                )
                .await;
        }
    }
    storage
        .link_local_file(
            ns,
            &book.book_url,
            Some(&normalize_path(&path.to_string_lossy())),
            mtime,
            size,
            false,
        )
        .await?;
    tracing::info!(
        "本地书重扫 [{}] {}（{} 章）← {}",
        ns,
        book.name,
        pairs.len(),
        path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// 递归收集书仓格式文件（仅 epub / txt）
async fn collect_book_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        crate::service::fs_rate::tick().await;
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            paths.push(entry.path());
        }
        paths.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
        for p in paths.into_iter().rev() {
            crate::service::fs_rate::tick().await;
            let Ok(meta) = tokio::fs::metadata(&p).await else {
                continue;
            };
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file() {
                let ext = local_book::file_ext(&p.to_string_lossy());
                if SYNC_EXTENSIONS.contains(&ext.as_str()) {
                    out.push(p);
                }
            }
        }
    }
}

fn normalize_path(p: &str) -> String {
    p.replace('\\', "/")
}

fn file_mtime_ms(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_storage(tag: &str) -> Storage {
        let dir = std::env::temp_dir().join(format!(
            "reader-localsync-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        crate::storage::init(&config).await.unwrap()
    }

    async fn cleanup(storage: Storage, tag: &str) {
        storage.pool.close().await;
        let dir = std::env::temp_dir().join(format!(
            "reader-localsync-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn no_env_dirs() -> Vec<PathBuf> {
        Vec::new()
    }

    fn books_dir(storage: &Storage, ns: &str) -> PathBuf {
        storage
            .config
            .storage_dir()
            .join("data")
            .join(ns)
            .join(BOOKS_DIR)
    }

    fn write_txt(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const SAMPLE1: &str = "第一章 起点\n内容一。\n第二章 成长\n内容二。";
    const SAMPLE2: &str =
        "第一章 起点\n内容一（修订）。\n第二章 成长\n内容二（修订）。\n第三章 终章\n内容三。";

    #[tokio::test]
    async fn reconcile_imports_new_file() {
        let storage = test_storage("import").await;
        write_txt(&books_dir(&storage, "default").join("测试书.txt"), SAMPLE1);
        let changed = reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        assert!(changed);
        let books = storage.list_books("default").await.unwrap();
        assert_eq!(books.len(), 1);
        let b = &books[0];
        assert!(b.book_url.starts_with("local://"));
        assert_eq!(b.name, "测试书");
        assert_eq!(b.origin, "local");
        assert!(b
            .local_file
            .as_deref()
            .unwrap()
            .ends_with("books/测试书.txt"));
        assert!(!b.local_file_deleted);
        assert!(b.local_file_mtime > 0);
        assert_eq!(
            storage
                .count_chapters("default", &b.book_url)
                .await
                .unwrap(),
            2
        );
        let changed2 = reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        assert!(!changed2);
        cleanup(storage, "import").await;
    }

    #[tokio::test]
    async fn reconcile_rescans_modified_file() {
        let storage = test_storage("rescan").await;
        let path = books_dir(&storage, "default").join("测试书.txt");
        write_txt(&path, SAMPLE1);
        reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        let book_url = storage.list_books("default").await.unwrap()[0]
            .book_url
            .clone();
        std::thread::sleep(Duration::from_millis(20));
        write_txt(&path, SAMPLE2);
        let changed = reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        assert!(changed);
        let books = storage.list_books("default").await.unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].book_url, book_url);
        assert_eq!(
            storage.count_chapters("default", &book_url).await.unwrap(),
            3
        );
        let content = storage
            .get_chapter_content("default", &book_url, 1)
            .await
            .unwrap()
            .unwrap();
        assert!(content.contains("内容二（修订）"));
        cleanup(storage, "rescan").await;
    }

    #[tokio::test]
    async fn reconcile_marks_deleted_and_relinks() {
        let storage = test_storage("delete").await;
        let path = books_dir(&storage, "default").join("测试书.txt");
        write_txt(&path, SAMPLE1);
        reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        let book_url = storage.list_books("default").await.unwrap()[0]
            .book_url
            .clone();
        std::fs::remove_file(&path).unwrap();
        let changed = reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        assert!(changed);
        let b = storage
            .find_book("default", &book_url)
            .await
            .unwrap()
            .unwrap();
        assert!(b.local_file_deleted);
        assert!(b.is_in_shelf);
        assert_eq!(
            storage.count_chapters("default", &book_url).await.unwrap(),
            2
        );
        std::thread::sleep(Duration::from_millis(20));
        write_txt(&path, SAMPLE2);
        let changed = reconcile_namespace_dirs(&storage, "default", &no_env_dirs())
            .await
            .unwrap();
        assert!(changed);
        let books = storage.list_books("default").await.unwrap();
        assert_eq!(books.len(), 1);
        assert!(!books[0].local_file_deleted);
        assert_eq!(
            storage.count_chapters("default", &book_url).await.unwrap(),
            3
        );
        cleanup(storage, "delete").await;
    }

    #[tokio::test]
    async fn reconcile_env_book_dir_imports() {
        let env_dir = std::env::temp_dir().join(format!(
            "reader-localsync-env-import-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&env_dir);
        std::fs::create_dir_all(&env_dir).unwrap();
        std::env::set_var("READER_LOCAL_BOOK_DIR", &env_dir);
        let storage = test_storage("envimport").await;
        write_txt(&env_dir.join("环境书.txt"), SAMPLE1);
        let changed = reconcile_namespace(&storage, "default").await.unwrap();
        assert!(changed);
        let books = storage.list_books("default").await.unwrap();
        assert_eq!(books.len(), 1);
        assert!(books[0]
            .local_file
            .as_deref()
            .unwrap()
            .contains("环境书.txt"));
        let changed2 = reconcile_namespace(&storage, "other").await.unwrap();
        assert!(!changed2);
        cleanup(storage, "envimport").await;
    }

    #[test]
    fn test_namespace_of_path_mapping() {
        let data = PathBuf::from("/srv/storage/data");
        let env = PathBuf::from("/srv/books");
        assert_eq!(
            namespace_of_path(
                Path::new("/srv/storage/data/alice/books/a.txt"),
                &data,
                &data
            )
            .as_deref(),
            Some("alice")
        );
        assert_eq!(
            namespace_of_path(
                Path::new("/srv/storage/data/alice/books/sub/b.epub"),
                &data,
                &data
            )
            .as_deref(),
            Some("alice")
        );
        assert_eq!(
            namespace_of_path(
                Path::new("/srv/storage/data/alice/assets/x.txt"),
                &data,
                &data
            ),
            None
        );
        assert_eq!(
            namespace_of_path(Path::new("/srv/books/a.txt"), &data, &env).as_deref(),
            Some("default")
        );
        assert_eq!(
            namespace_of_path(Path::new("/tmp/x.txt"), &data, &data),
            None
        );
    }
}
