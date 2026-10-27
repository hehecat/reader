//! 后台缓存任务（cacheBookOnServer / cacheBookRangeOnServer / cacheBookSSE / cancelCacheBook）
//!
//! 内存任务表（taskId 键）：目录 → 按范围选章 → 逐章 getBookContent 语义抓取 →
//! 写 book_chapters 缓存表，并发 3；SSE 轮询进度 {cached,total,title}；cancel 置取消标记。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};

/// 缓存任务进度
#[derive(Debug, Clone, Default)]
pub struct CacheProgress {
    /// 已缓存章节数
    pub cached: usize,
    /// 总章节数
    pub total: usize,
    /// 书名
    pub title: String,
    /// 是否结束（成功或失败）
    pub finished: bool,
    /// 是否被取消
    pub cancelled: bool,
    /// 错误信息（失败时）
    pub error: Option<String>,
}

/// 内存任务表（url → 进度句柄）
static CACHE_TASKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<CacheProgress>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 查询任务进度（不存在返回 None）
pub fn progress_of(url: &str) -> Option<Arc<Mutex<CacheProgress>>> {
    progress_of_key(url)
}

/// 查询任务进度（taskId 精确键）
pub fn progress_of_key(task_id: &str) -> Option<Arc<Mutex<CacheProgress>>> {
    CACHE_TASKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)
        .cloned()
}

/// 取消任务（从任务表移除并置取消标记）；返回是否命中
pub fn cancel(url: &str) -> bool {
    cancel_key(url)
}

/// 取消任务（taskId 精确键）
pub fn cancel_key(task_id: &str) -> bool {
    let mut map = CACHE_TASKS.lock().unwrap_or_else(|e| e.into_inner());
    match map.remove(task_id) {
        Some(p) => {
            if let Ok(mut p) = p.lock() {
                p.cancelled = true;
            }
            true
        }
        None => false,
    }
}

/// 范围切分辅助：越界返回错误
fn slice_range<T: Clone>(items: Vec<T>, range: Option<(usize, usize)>) -> Result<Vec<T>> {
    match range {
        Some((from, to)) => {
            if from >= items.len() || to >= items.len() || from > to {
                return Err(anyhow!("缓存范围无效（共 {} 章）", items.len()));
            }
            Ok(items[from..=to].to_vec())
        }
        None => Ok(items),
    }
}

/// 启动后台缓存任务（同一 url 已运行则复用；已完成的任务会被新任务覆盖）
pub fn start(ns: &str, url: &str, storage: crate::storage::Storage) -> Arc<Mutex<CacheProgress>> {
    start_range(ns, url, None, storage).1
}

/// 启动后台缓存任务并返回 (taskId, 进度句柄)。
/// `range = None` 表示整书；`Some((from, to))` 表示目录实章 0 基闭区间。
pub fn start_range(
    ns: &str,
    url: &str,
    range: Option<(usize, usize)>,
    storage: crate::storage::Storage,
) -> (String, Arc<Mutex<CacheProgress>>) {
    let task_id = match range {
        Some((from, to)) => format!("{url}#{from}-{to}"),
        None => url.to_string(),
    };
    {
        let map = CACHE_TASKS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(&task_id) {
            let p = existing.lock().unwrap_or_else(|e| e.into_inner());
            if !p.finished {
                return (task_id, existing.clone());
            }
        }
    }
    let progress = Arc::new(Mutex::new(CacheProgress {
        title: url.to_string(),
        ..Default::default()
    }));
    {
        let mut map = CACHE_TASKS.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(task_id.clone(), progress.clone());
    }
    let ns = ns.to_string();
    let url = url.to_string();
    let progress_for_task = progress.clone();
    tokio::spawn(async move {
        let result = run_job(&ns, &url, range, storage, &progress_for_task).await;
        let mut p = progress_for_task.lock().unwrap_or_else(|e| e.into_inner());
        p.finished = true;
        match result {
            Ok((title, total, cached)) => {
                p.title = title;
                p.total = total;
                p.cached = cached;
            }
            Err(e) => {
                p.error = Some(e.to_string());
            }
        }
    });
    (task_id, progress)
}

/// 执行缓存任务：目录 → 按范围选章 → 逐章正文 → 写缓存表（并发 3）
async fn run_job(
    ns: &str,
    url: &str,
    range: Option<(usize, usize)>,
    storage: crate::storage::Storage,
    progress: &Arc<Mutex<CacheProgress>>,
) -> Result<(String, usize, usize)> {
    let book = storage
        .find_book(ns, url)
        .await?
        .ok_or_else(|| anyhow!("书籍不存在（请先加入书架）"))?;

    // 本地书：章节已在 book_chapters（导入时全量入库）——直接计数
    if url.starts_with("local://") {
        let rows = storage.list_chapters(url).await?;
        let rows = slice_range(rows, range)?;
        return Ok((book.name, rows.len(), rows.len()));
    }

    // 文件型本地书（storage/ 路径或白名单扩展名）：重解析原文件 → 章节落库
    let is_file_book = url.starts_with("storage/")
        || crate::service::local_book::SUPPORTED_EXTENSIONS
            .iter()
            .any(|e| url.to_lowercase().ends_with(&format!(".{e}")));
    if is_file_book {
        let path = crate::api::router::resolve_export_file_path(&storage.config.storage_dir(), url)
            .ok_or_else(|| anyhow!("本地书文件不存在"))?;
        let imported = crate::service::local_book::parse_loc_book_path(
            &path,
            &[],
            &book.toc_url,
            book.split_long_chapter,
        )?;
        let pairs: Vec<(String, String)> = imported
            .chapters
            .iter()
            .map(|c| (c.title.clone(), c.content.clone()))
            .collect();
        let selected = slice_range(pairs, range)?;
        let offset = range.map(|(from, _)| from).unwrap_or(0);
        for (i, (title, content)) in selected.iter().enumerate() {
            storage
                .cache_chapter_content(ns, url, (offset + i) as i64, title, content)
                .await?;
        }
        return Ok((book.name, selected.len(), selected.len()));
    }

    // 书源书：目录（复用规则引擎）→ 并发 3 逐章正文 → 缓存表
    let source = if !book.origin.is_empty() {
        storage.find_book_source(ns, &book.origin).await?
    } else {
        None
    };
    let Some(source) = source else {
        return Err(anyhow!("书源不存在（origin={}）", book.origin));
    };
    let toc_url = if book.toc_url.is_empty() {
        url.to_string()
    } else {
        book.toc_url.clone()
    };
    let toc =
        crate::service::book::analyze_toc(ns, &toc_url, &source, 20, Some(&book.name), url).await?;
    let chapters: Vec<(String, String)> = toc
        .into_iter()
        .filter(|c| !c.is_volume)
        .map(|c| (c.title, c.url))
        .collect();
    let chapters = slice_range(chapters, range)?;
    let total = chapters.len();
    if total == 0 {
        return Ok((book.name, 0, 0));
    }

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(3));
    let mut handles = Vec::with_capacity(total);
    let ns = ns.to_string();
    let book_name = book.name.clone();
    let book_url = url.to_string();
    for (title, ch_url) in chapters {
        let sem = semaphore.clone();
        let ns = ns.clone();
        let source = source.clone();
        let book_name = book_name.clone();
        let book_url = book_url.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            match crate::service::book::analyze_content(
                &ns,
                &ch_url,
                &source,
                5,
                Some(&title),
                Some(&book_name),
                &book_url,
            )
            .await
            {
                Ok(content) => {
                    let idx = crate::util::md5::chapter_url_hash(&ch_url);
                    Ok((title, idx, content))
                }
                Err(e) => Err(e),
            }
        }));
    }

    let mut cached = 0usize;
    for h in handles {
        // 取消检查（逐任务粒度）
        {
            let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
            if p.cancelled {
                break;
            }
            p.cached = cached;
            p.total = total;
            p.title = book.name.clone();
        }
        match h.await {
            Ok(Ok((title, idx, content))) => {
                let _ = storage
                    .cache_chapter_content(&ns, url, idx, &title, &content)
                    .await;
                cached += 1;
            }
            _ => {
                // 单章失败不中断整书（进度仍推进）
            }
        }
    }
    Ok((book.name, total, cached))
}

/// 等待任务结束（测试辅助：轮询直到 finished 或超时）
pub async fn wait_finished(url: &str, timeout: Duration) -> bool {
    let started = std::time::Instant::now();
    loop {
        if let Some(p) = progress_of(url) {
            let p = p.lock().unwrap_or_else(|e| e.into_inner());
            if p.finished {
                return true;
            }
        }
        if started.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_range_selects_closed_interval() {
        let items = vec![1, 2, 3, 4, 5];
        assert_eq!(slice_range(items.clone(), None).unwrap(), items);
        assert_eq!(
            slice_range(items.clone(), Some((1, 3))).unwrap(),
            vec![2, 3, 4]
        );
        assert_eq!(slice_range(items.clone(), Some((0, 0))).unwrap(), vec![1]);
        assert_eq!(slice_range(items.clone(), Some((4, 4))).unwrap(), vec![5]);
    }

    #[test]
    fn slice_range_rejects_invalid_bounds() {
        let items = vec![1, 2, 3];
        assert!(slice_range(items.clone(), Some((3, 3))).is_err());
        assert!(slice_range(items.clone(), Some((2, 3))).is_err());
        assert!(slice_range(items.clone(), Some((2, 1))).is_err());
    }
}
