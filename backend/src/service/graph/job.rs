//! 后台生成任务：逐批调用模型、落库、报进度、可取消。仿 `service/cache_job.rs`。
//!
//! 链路：已缓存章节 → [`super::plan_batches`] 分批 → 逐批 `ai::chat_once` → 原始 JSON 落
//! `book_graph_batches` → 全部批完成后 [`super::normalize`] 合并 → `book_graphs` 成品。
//! 进度双写：内存任务表（SSE 每 300ms 读）+ `book_graph_jobs` 行（进程重启后仍可回显）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::service::ai::{self, AiConfig};
use crate::storage::Storage;

use super::{build_batch_messages, normalize, plan_batches, Chapter, MAX_BATCH_TOKENS};

/// 任务状态机：queued → running → done | failed | cancelled
/// （cancelled/failed 都保留已完成批次的落库结果，便于排查与续跑）
const ST_QUEUED: &str = "queued";
const ST_RUNNING: &str = "running";
const ST_DONE: &str = "done";
const ST_FAILED: &str = "failed";
const ST_CANCELLED: &str = "cancelled";

/// 错误文本入库前的截断长度（DB 里没必要存整段模型输出）
const ERROR_MAX_CHARS: usize = 500;

/// 任务进度（内存态，DB 里另有持久记录）
#[derive(Debug, Clone, Default)]
pub struct GraphProgress {
    /// 任务号（已有在跑任务被复用时要能把它回给调用方，故随进度一起存）
    pub job_id: String,
    /// queued | running | done | failed | cancelled
    pub status: String,
    pub total_batches: usize,
    pub done_batches: usize,
    pub chapters_scanned: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub characters: usize,
    pub relations: usize,
    pub error: Option<String>,
}

/// 启动参数
#[derive(Debug, Clone, Default)]
pub struct JobOptions {
    pub book_name: String,
    /// 只跑前 N 批（省钱自测用）
    pub max_batches: Option<usize>,
}

/// 内存任务表（book_url → 进度句柄）
static GRAPH_TASKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<GraphProgress>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 进程级 storage 句柄：`progress_of` 是同步签名，DB 回落要靠它。
/// 由 graph 相关 handler 在入口处注册（同 `crawler::register_cookie_storage` 的做法）。
static GRAPH_STORAGE: LazyLock<std::sync::RwLock<Option<Storage>>> =
    LazyLock::new(|| std::sync::RwLock::new(None));

/// 注册 storage（供 `progress_of` 的 DB 回落用；幂等，后注册的覆盖先注册的）
pub fn register_storage(storage: Storage) {
    *GRAPH_STORAGE.write().unwrap_or_else(|e| e.into_inner()) = Some(storage);
}

// ---------------- 锁 / 小工具 ----------------

fn lock_tasks() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Mutex<GraphProgress>>>> {
    GRAPH_TASKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_progress(p: &Arc<Mutex<GraphProgress>>) -> std::sync::MutexGuard<'_, GraphProgress> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_terminal(status: &str) -> bool {
    is_terminal_status(status)
}

/// 终态判定（SSE 据此决定发完最后一帧后关流）：done / failed / cancelled
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, ST_DONE | ST_FAILED | ST_CANCELLED)
}

/// 取消标记就用 status 本身（cancelled 是终态，任务循环每批之间看一眼）
fn is_cancelled(p: &Arc<Mutex<GraphProgress>>) -> bool {
    lock_progress(p).status == ST_CANCELLED
}

fn snippet(raw: &str) -> String {
    raw.chars().take(120).collect()
}

fn truncate(text: &str, max: usize) -> String {
    let truncated: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// 同步入口里跑一小段 async（建行/读章节/DB 回落）。
///
/// 生产 runtime 是多线程（`src/main.rs` 的 `new_multi_thread`）→ `block_in_place` 让出
/// worker 执行权再 block；非多线程 runtime 上下文（单测）→ 交给 futures 的阻塞执行器。
fn block_on<F: Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }
        _ => futures::executor::block_on(fut),
    }
}

fn array_len(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

// ---------------- 对外接口 ----------------

/// 启动一次生成任务，返回 job_id
pub fn start_job(
    storage: Storage,
    ns: String,
    book_url: String,
    opts: JobOptions,
    cfg: AiConfig,
) -> anyhow::Result<String> {
    if book_url.trim().is_empty() {
        return Err(anyhow!("缺少书籍地址"));
    }
    register_storage(storage.clone());

    // 同一本书已有在跑的任务 → 复用，绝不重复烧 token
    if let Some(existing) = mem_progress(&book_url) {
        let running_job = {
            let p = lock_progress(&existing);
            if is_terminal(&p.status) {
                String::new()
            } else {
                p.job_id.clone()
            }
        };
        if !running_job.is_empty() {
            return Ok(running_job);
        }
    }

    // 已缓存章节是整条链路的前置条件：没有缓存就不必建行
    let chapters = block_on(storage.list_cached_chapters(&ns, &book_url))
        .map_err(|e| anyhow!("读取已缓存章节失败: {e}"))?;
    if chapters.is_empty() {
        return Err(anyhow!(
            "本书还没有已缓存章节, 先读几章(或先缓存章节)再生成关系图"
        ));
    }

    let job_id = uuid::Uuid::new_v4().simple().to_string();
    let progress = Arc::new(Mutex::new(GraphProgress {
        job_id: job_id.clone(),
        status: ST_QUEUED.to_string(),
        // 批数要等 plan_batches 算完才知道；先用上界估计填着，run_job 里回写真实值
        total_batches: estimate_batches(chapters.len()),
        ..Default::default()
    }));
    let total_hint = progress_batches(&progress);
    {
        let mut map = lock_tasks();
        map.insert(book_url.clone(), progress.clone());
    }
    // 先落行（前端立刻能看到任务存在），失败就地清理内存态
    if let Err(e) = block_on(storage.graph_job_create(&job_id, &ns, &book_url, total_hint as i64)) {
        lock_tasks().remove(&book_url);
        return Err(anyhow!("创建任务失败: {e}"));
    }

    let storage_for_run = storage.clone();
    let storage_for_finish = storage;
    let ns_for_run = ns;
    let url_for_run = book_url.clone();
    let job_for_run = job_id.clone();
    let progress_for_run = progress.clone();
    tokio::spawn(async move {
        // 内层再 spawn 一层：plan/normalize 还在并行开发（todo!() 会 panic），
        // 靠 JoinError 把 panic 也收成 failed，否则任务永远停在 running、SSE 永不关流。
        let inner = tokio::spawn(run_job(
            storage_for_run,
            ns_for_run,
            url_for_run,
            job_for_run.clone(),
            opts,
            cfg,
            chapters,
            progress_for_run.clone(),
        ));
        let err = match inner.await {
            Ok(Ok(())) => return,
            Ok(Err(e)) => format!("{e:#}"),
            Err(join) if join.is_panic() => "任务内部错误（生成管线的某个环节 panic）".to_string(),
            Err(join) => format!("任务被中止: {join}"),
        };
        mark_failed(&storage_for_finish, &job_for_run, &progress_for_run, &err).await;
    });

    Ok(job_id)
}

fn progress_batches(p: &Arc<Mutex<GraphProgress>>) -> usize {
    lock_progress(p).total_batches
}

/// 查询进度：优先内存态；进程重启后回落到 `book_graph_jobs` 最近一行
pub fn progress_of(book_url: &str) -> Option<Arc<Mutex<GraphProgress>>> {
    if let Some(p) = mem_progress(book_url) {
        return Some(p);
    }
    let storage = GRAPH_STORAGE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    let row = block_on(load_latest_progress(&storage, book_url))?;
    Some(Arc::new(Mutex::new(row)))
}

fn mem_progress(book_url: &str) -> Option<Arc<Mutex<GraphProgress>>> {
    lock_tasks().get(book_url).cloned()
}

/// 取消任务（置 cancelled 标记）；返回是否命中。不删表项——SSE 还要读到 cancelled 收尾。
pub fn cancel(book_url: &str) -> bool {
    let Some(p) = mem_progress(book_url) else {
        return false;
    };
    let mut p = lock_progress(&p);
    if is_terminal(&p.status) {
        return false;
    }
    p.status = ST_CANCELLED.to_string();
    true
}

/// 粗估消耗：(prompt_tokens, completion_tokens)
///
/// 经验值：按《绍宋》两次整本实测回推——约 395 章已缓存时 prompt ≈ 170 万 token
/// （≈4300 token/章，含提示词模板与跨批重复窗口），单批输出按 3500 token 估。
/// 只用于花钱前的量级提示，实际会上下浮动 30% 左右。
pub fn estimate(cached_chapters: usize, batches: usize) -> (u64, u64) {
    let prompt = (cached_chapters as u64).saturating_mul(1_700_000) / 395;
    let completion = (batches as u64).saturating_mul(3_500);
    (prompt, completion)
}

/// 批数上界估计：`plan.rs` 的口径是「`BATCHES` 批、每批最多 `PER_BATCH` 章」。
///
/// `plan_batches` 现在是 `todo!()`（并行开发中），估算接口与任务行先用它兜底；
/// 真正开跑时以 `plan_batches` 的实际批数为准（`run_job` 会回写）。
pub fn estimate_batches(cached_chapters: usize) -> usize {
    if cached_chapters == 0 {
        return 0;
    }
    cached_chapters
        .div_ceil(super::PER_BATCH)
        .min(super::BATCHES)
}

// ---------------- 任务执行 ----------------

/// 执行生成任务。成功返回 Ok（状态已写 done），失败返回 Err（由外层统一标 failed）；
/// 被取消时自行收尾成 cancelled 并返回 Ok。
#[allow(clippy::too_many_arguments)]
async fn run_job(
    storage: Storage,
    ns: String,
    book_url: String,
    job_id: String,
    opts: JobOptions,
    cfg: AiConfig,
    chapters: Vec<Chapter>,
    progress: Arc<Mutex<GraphProgress>>,
) -> Result<()> {
    let mut plans = plan_batches(&chapters);
    if let Some(max) = opts.max_batches.filter(|n| *n > 0) {
        plans.truncate(max);
    }
    if plans.is_empty() {
        return Err(anyhow!("选章结果为空（已缓存 {} 章）", chapters.len()));
    }
    let total = plans.len();
    set_total_batches(&storage, &job_id, total).await?;
    {
        let mut p = lock_progress(&progress);
        p.status = ST_RUNNING.to_string();
        p.total_batches = total;
    }

    let mut done = 0usize;
    let mut fed = 0usize;
    for plan in &plans {
        // 每批之间检查取消：已完成的批留在 book_graph_batches，不丢
        if is_cancelled(&progress) {
            return finish_cancelled(&storage, &job_id, &progress, done, fed).await;
        }
        let messages = build_batch_messages(&opts.book_name, plan);
        let value = batch_with_retry(&cfg, messages)
            .await
            .with_context(|| format!("第 {} 批失败", plan.batch_no))?;
        storage
            .graph_batch_put(&job_id, plan.batch_no as i64, &value.to_string())
            .await?;
        done += 1;
        fed += plan.chapters.len();
        let (prompt_tokens, completion_tokens) = estimate(chapters.len(), done);
        {
            let mut p = lock_progress(&progress);
            p.done_batches = done;
            p.chapters_scanned = fed;
            p.prompt_tokens = prompt_tokens;
            p.completion_tokens = completion_tokens;
        }
        storage
            .graph_job_set_progress(
                &job_id,
                done as i64,
                fed as i64,
                prompt_tokens as i64,
                completion_tokens as i64,
            )
            .await?;
    }
    if is_cancelled(&progress) {
        return finish_cancelled(&storage, &job_id, &progress, done, fed).await;
    }

    // 全部批完成：读回原始 JSON → 归一化 → 落成品
    let raws: Vec<Value> = storage
        .graph_batches(&job_id)
        .await?
        .into_iter()
        .filter_map(|(_, raw)| serde_json::from_str::<Value>(&raw).ok())
        .collect();
    if raws.is_empty() {
        return Err(anyhow!("没有可归一化的批次结果"));
    }
    let graph = normalize(&raws, &chapters);
    let characters = array_len(&graph, "characters");
    let relations = array_len(&graph, "relations");
    storage
        .graph_save(
            &ns,
            &book_url,
            &serde_json::to_string(&graph)?,
            characters as i64,
            relations as i64,
            fed as i64,
            &job_id,
        )
        .await?;
    {
        let mut p = lock_progress(&progress);
        p.status = ST_DONE.to_string();
        p.done_batches = done;
        p.chapters_scanned = fed;
        p.characters = characters;
        p.relations = relations;
    }
    let _ = storage.graph_job_set_status(&job_id, ST_DONE, None).await;
    Ok(())
}

/// 跑一批：失败重试 1 次（网络/模型抖一下很常见），两次都失败才放弃
async fn batch_with_retry(cfg: &AiConfig, messages: Vec<Value>) -> Result<Value> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..2 {
        match ai::chat_once(cfg, messages.clone(), MAX_BATCH_TOKENS).await {
            Ok(raw) => match ai::extract_json_object(&raw) {
                Some(value) => return Ok(value),
                None => last = Some(anyhow!("模型未返回合法 JSON: {}", snippet(&raw))),
            },
            Err(e) => last = Some(e),
        }
        if attempt == 0 {
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("模型调用失败")))
}

/// 取消收尾：状态 cancelled，已完成的批保持原样
async fn finish_cancelled(
    storage: &Storage,
    job_id: &str,
    progress: &Arc<Mutex<GraphProgress>>,
    done: usize,
    fed: usize,
) -> Result<()> {
    {
        let mut p = lock_progress(progress);
        p.status = ST_CANCELLED.to_string();
        p.done_batches = done;
        p.chapters_scanned = fed;
    }
    storage
        .graph_job_set_status(job_id, ST_CANCELLED, None)
        .await?;
    Ok(())
}

/// 失败收尾：(进度 + DB 行) 一起标 failed，错误文本截断到 500 字
async fn mark_failed(
    storage: &Storage,
    job_id: &str,
    progress: &Arc<Mutex<GraphProgress>>,
    err: &str,
) {
    let text = truncate(err, ERROR_MAX_CHARS);
    {
        let mut p = lock_progress(progress);
        // 用户先点的取消优先：不要把它改写成 failed
        if p.status == ST_CANCELLED {
            return;
        }
        p.status = ST_FAILED.to_string();
        p.error = Some(text.clone());
    }
    let _ = storage
        .graph_job_set_status(job_id, ST_FAILED, Some(&text))
        .await;
}

/// 回写真实批数 + 转 running。
///
/// storage 层只有 `graph_job_create`（建行时定 total_batches）没有回写接口，
/// 而批数要等 `plan_batches` 算完才知道——这里直连 pool 补一条 UPDATE（不改 storage）。
async fn set_total_batches(storage: &Storage, job_id: &str, total: usize) -> Result<()> {
    sqlx::query(
        "UPDATE book_graph_jobs SET total_batches = ?2, status = ?3, updated_at = ?4 \
         WHERE job_id = ?1",
    )
    .bind(job_id)
    .bind(total as i64)
    .bind(ST_RUNNING)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&storage.pool)
    .await?;
    Ok(())
}

/// 进程重启后的回落：读该书最近一行任务 + 成品里的规模数字。
///
/// 注意：DB 里 queued/running 但内存里没有对应任务 = 上次进程异常中断（含 panic），
/// 这类行若原样回显，前端 SSE 会永远等一个不会结束的任务 → 这里按 failed 回显。
async fn load_latest_progress(storage: &Storage, book_url: &str) -> Option<GraphProgress> {
    let row = sqlx::query_as::<_, (String, String, i64, i64, i64, i64, i64, Option<String>)>(
        "SELECT job_id, status, total_batches, done_batches, chapters_scanned, prompt_tokens, \
         completion_tokens, error FROM book_graph_jobs WHERE book_url = ?1 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(book_url)
    .fetch_optional(&storage.pool)
    .await
    .ok()
    .flatten()?;
    let (
        job_id,
        mut status,
        total_batches,
        done_batches,
        chapters_scanned,
        prompt_tokens,
        completion_tokens,
        mut error,
    ) = row;
    if status == ST_QUEUED || status == ST_RUNNING {
        status = ST_FAILED.to_string();
        error = Some("任务中断（服务重启或上次异常退出）".to_string());
    }
    let (characters, relations) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT characters, relations FROM book_graphs WHERE book_url = ?1 \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(book_url)
    .fetch_optional(&storage.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or((0, 0));
    Some(GraphProgress {
        job_id,
        status,
        total_batches: total_batches.max(0) as usize,
        done_batches: done_batches.max(0) as usize,
        chapters_scanned: chapters_scanned.max(0) as usize,
        prompt_tokens: prompt_tokens.max(0) as u64,
        completion_tokens: completion_tokens.max(0) as u64,
        characters: characters.max(0) as usize,
        relations: relations.max(0) as usize,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立临时目录的存储（不污染真实 storage/reader.db）
    fn temp_storage(tag: &str) -> (Storage, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-graph-job-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = block_on(crate::storage::init(&config)).expect("测试存储初始化失败");
        (storage, dir)
    }

    fn ai_cfg() -> AiConfig {
        AiConfig {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            max_tokens: 1000,
            thinking: String::new(),
        }
    }

    /// 估算随章数/批数单调不减（花钱前的量级提示不能反着来）
    #[test]
    fn estimate_is_monotonic() {
        let mut last_prompt = 0u64;
        for chapters in [0usize, 1, 40, 395, 1000] {
            let prompt = estimate(chapters, 1).0;
            assert!(
                prompt >= last_prompt,
                "prompt 应随章数单调不减: {prompt} < {last_prompt}"
            );
            last_prompt = prompt;
        }
        let mut last_completion = 0u64;
        for batches in [0usize, 1, 3, 6] {
            let completion = estimate(120, batches).1;
            assert!(
                completion >= last_completion,
                "completion 应随批数单调不减: {completion} < {last_completion}"
            );
            last_completion = completion;
        }
        assert_eq!(estimate(0, 0), (0, 0));
        // 《绍宋》口径：395 章 ≈ 170 万 prompt token；单批输出 3500
        assert_eq!(estimate(395, 6), (1_700_000, 6 * 3_500));
    }

    /// 批数上界：每批 `PER_BATCH` 章、最多 `BATCHES` 批
    #[test]
    fn estimate_batches_caps_at_plan_budget() {
        assert_eq!(estimate_batches(0), 0);
        assert_eq!(estimate_batches(1), 1);
        assert_eq!(estimate_batches(super::super::PER_BATCH), 1);
        assert_eq!(estimate_batches(super::super::PER_BATCH + 1), 2);
        assert_eq!(estimate_batches(100_000), super::super::BATCHES);
    }

    /// 没有已缓存章节 → 直接报错（不起任务、不建行、不留内存态）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_job_rejects_book_without_cached_chapters() {
        let (storage, dir) = temp_storage("no-cache");
        let url = "https://example.com/book/no-cache";
        let err = start_job(
            storage.clone(),
            "default".to_string(),
            url.to_string(),
            JobOptions::default(),
            ai_cfg(),
        )
        .expect_err("没有已缓存章节时必须报错");
        assert!(
            err.to_string().contains("已缓存章节"),
            "错误文案应说明缺已缓存章节: {err}"
        );
        assert!(mem_progress(url).is_none(), "失败时不应留下内存任务");
        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 任务表命中/未命中 + 取消语义
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn progress_of_hits_and_misses() {
        let url = "https://example.com/book/progress-hit";
        assert!(progress_of(url).is_none(), "未登记的书应查不到");
        let progress = Arc::new(Mutex::new(GraphProgress {
            job_id: "job-1".to_string(),
            status: ST_RUNNING.to_string(),
            total_batches: 6,
            done_batches: 2,
            ..Default::default()
        }));
        lock_tasks().insert(url.to_string(), progress);

        let hit = progress_of(url).expect("应命中内存任务表");
        {
            let p = lock_progress(&hit);
            assert_eq!(p.job_id, "job-1");
            assert_eq!(p.status, ST_RUNNING);
            assert_eq!(p.total_batches, 6);
            assert_eq!(p.done_batches, 2);
        }

        // 在跑的任务可取消；取消后是终态，不能再取消
        assert!(cancel(url), "running 任务应可取消");
        assert!(
            progress_of(url).is_some_and(|p| lock_progress(&p).status == ST_CANCELLED),
            "取消后应能读到 cancelled 终态"
        );
        assert!(!cancel(url), "已终态的任务不算命中");
        lock_tasks().remove(url);
    }

    /// 终态判定
    #[test]
    fn terminal_states() {
        for s in [ST_DONE, ST_FAILED, ST_CANCELLED] {
            assert!(is_terminal(s));
        }
        for s in [ST_QUEUED, ST_RUNNING, "", "weird"] {
            assert!(!is_terminal(s));
        }
    }

    /// 错误文本截断到 500 字（不切断 UTF-8）
    #[test]
    fn truncate_keeps_chars_bounded() {
        let long = "错".repeat(900);
        let out = truncate(&long, ERROR_MAX_CHARS);
        assert_eq!(out.chars().count(), ERROR_MAX_CHARS + 1); // +1 是省略号
        assert!(out.ends_with('…'));
        assert_eq!(truncate("短错误", ERROR_MAX_CHARS), "短错误");
    }
}
