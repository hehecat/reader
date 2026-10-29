//! 后台生成任务：逐批调用模型、落库、报进度、可取消。仿 `service/cache_job.rs`。
//!
//! 链路（增量）：已缓存章节 → 台账对账 → [`super::plan_incremental`] 只挑「这次该跑的章」→
//! 逐批 `ai::chat_once` → 每批成功后**立即**落 `book_graph_units` + 标
//! `book_graph_chapters.done_at` → 全部批完成后 [`super::normalize`] 合并该书**全部** unit
//! （历史 + 本次）→ `book_graphs` 成品。
//! 进度双写：内存任务表（SSE 每 300ms 读）+ `book_graph_jobs` 行（进程重启后仍可回显）。

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::service::ai::{self, AiConfig};
use crate::storage::Storage;

use super::normalize::BookHints;
use super::plan::{number_chapters_keyed, parse_toc};
use super::{
    build_batch_messages, content_hash, normalize, plan_incremental, BatchPlan, Chapter,
    ChapterEntry, Covered, MAX_BATCH_TOKENS,
};

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

/// 归并结果为空时是否保留原成品。
///
/// 模型偶尔会输出一堆「quote 在正文里找不到」的关系，全被校验丢掉后归并结果就是
/// 0 人 0 关系；这种情况下把已有的好成品覆盖掉是纯损失。
fn keep_old_on_empty(characters: usize, relations: usize, old_characters: usize) -> bool {
    characters == 0 && relations == 0 && old_characters > 0
}

fn array_len(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

/// 目录缓存的有效期：目录是阅读序的唯一权威来源，就算过期也比重排错强（10 年）
const TOC_MAX_AGE_MS: i64 = 10 * 365 * 24 * 3600 * 1000;

/// 取章节 → 按目录排阅读序 → 重编号 1..N；顺手查出权威书名与全书总章数。
///
/// Bug 1：`list_cached_chapters` 的 `ORDER BY chapter_index` 对书源书是**哈希序**
/// （`chapter_index = md5(chapterUrl)`，见 `Storage::cache_chapter_content`），数字还是
/// 1e18 量级。原样喂模型，页面就显示「1151345960693571501 章」、prompt 里也成了
/// 【第1151345960693571501章】。权威阅读序在 `toc_cache.chapters_json`。
/// Bug 2：书名/总章数别问模型，`books` 表里现成就有。
///
/// 增量：排序时把**原 `chapter_index`**一起带出来当 `chapter_key`（跨生成稳定，TOC 重排也不变），
/// 并给每章算正文哈希 —— 台账靠这两个键判断「新章 / 变章 / 已覆盖」。
fn prepare_chapters(
    storage: &Storage,
    ns: &str,
    book_url: &str,
    requested_name: &str,
) -> Result<PreparedBook> {
    let cached = block_on(storage.list_cached_chapters(ns, book_url))
        .map_err(|e| anyhow!("读取已缓存章节失败: {e}"))?;
    if cached.is_empty() {
        return Ok(PreparedBook::default());
    }

    let book = block_on(storage.find_book(ns, book_url)).ok().flatten();
    let toc_url = book
        .as_ref()
        .map(|b| b.toc_url.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| book_url.to_string());
    // 取不到目录（没缓存过 / JSON 坏了）时 number_chapters 只按原序重编号，绝不 panic
    let toc = block_on(storage.get_toc_cache(ns, &toc_url, TOC_MAX_AGE_MS))
        .ok()
        .flatten()
        .map(|json| parse_toc(&json))
        .unwrap_or_default();
    let entries: Vec<ChapterEntry> = number_chapters_keyed(&cached, &toc)
        .into_iter()
        .map(|(chapter, key)| ChapterEntry {
            content_hash: content_hash(&chapter.2),
            chapter,
            key,
        })
        .collect();
    // 书名优先级：books.name > 调用方给的名字 > book_info_cache > 空（normalize 退模型输出）
    let mut book_name = book
        .as_ref()
        .map(|b| b.name.trim().to_string())
        .unwrap_or_default();
    if book_name.is_empty() {
        book_name = requested_name.trim().to_string();
    }
    if book_name.is_empty() {
        book_name = block_on(storage.get_book_info_cache(ns, book_url, TOC_MAX_AGE_MS))
            .ok()
            .flatten()
            .and_then(|info| {
                info.get("name")
                    .and_then(Value::as_str)
                    .map(|n| n.trim().to_string())
            })
            .unwrap_or_default();
    }
    // 全书总章数：books.total_chapter_num 优先，退目录长度（本书 1745，与实际一致）
    let total = book
        .as_ref()
        .map(|b| b.total_chapter_num)
        .unwrap_or(0)
        .max(toc.len() as i64);
    Ok(PreparedBook {
        entries,
        hints: BookHints {
            book_name,
            total_chapters: total,
        },
    })
}

/// 一次生成要用的全部章节数据（章身 + 增量台账要用的两个稳定键）
#[derive(Debug, Clone, Default)]
struct PreparedBook {
    /// 已按阅读序重编号 1..N
    entries: Vec<ChapterEntry>,
    hints: BookHints,
}

impl PreparedBook {
    /// normalize 要的「(序数, 标题, 正文)」视图
    fn chapters(&self) -> Vec<Chapter> {
        self.entries.iter().map(|e| e.chapter.clone()).collect()
    }
}

// ---------------- 对外接口 ----------------

/// 启动的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// 启动了任务（或在跑的任务被复用）
    Started {
        job_id: String,
        /// 本次要跑的批数（不含历史批）
        total_batches: usize,
        /// 本次入选章数
        chapters_selected: usize,
        /// 是否复用了一个正在跑的任务
        reused: bool,
    },
    /// 已是最新：不建任务、不花 token（用户重复点「生成」不该烧钱）
    UpToDate {
        /// 台账里已覆盖的章数
        covered_chapters: usize,
    },
}

/// 启动一次生成任务（增量）
///
/// 选章在**建任务行之前**就算完：这样才能在「没有新章、没有变章、成品已存在」时
/// 直接返回 [`StartOutcome::UpToDate`]，一个 token 都不花、也不留任务行。
pub fn start_job(
    storage: Storage,
    ns: String,
    book_url: String,
    opts: JobOptions,
    cfg: AiConfig,
) -> anyhow::Result<StartOutcome> {
    if book_url.trim().is_empty() {
        return Err(anyhow!("缺少书籍地址"));
    }
    register_storage(storage.clone());

    // 同一本书已有在跑的任务 → 复用，绝不重复烧 token
    if let Some(existing) = mem_progress(&book_url) {
        let (running_job, total_batches, done) = {
            let p = lock_progress(&existing);
            if is_terminal(&p.status) {
                (String::new(), 0, 0)
            } else {
                (p.job_id.clone(), p.total_batches, p.chapters_scanned)
            }
        };
        if !running_job.is_empty() {
            return Ok(StartOutcome::Started {
                job_id: running_job,
                total_batches,
                chapters_selected: done,
                reused: true,
            });
        }
    }

    // 已缓存章节是整条链路的前置条件：没有缓存就不必建行。
    // 章节在这里就按目录排成阅读序、重编号 1..N（书源书的 chapter_index 是 md5 哈希，
    // 既是哈希序又是天文数字，见 `plan::number_chapters`），书名/总章数顺手从库里查实。
    let prepared = prepare_chapters(&storage, &ns, &book_url, &opts.book_name)?;
    if prepared.entries.is_empty() {
        return Err(anyhow!(
            "本书还没有已缓存章节, 先读几章(或先缓存章节)再生成关系图"
        ));
    }
    // 提示词与成品都用库里的权威书名（模型批次 JSON 里通常没有 book 字段）
    let mut opts = opts;
    if !prepared.hints.book_name.is_empty() {
        opts.book_name = prepared.hints.book_name.clone();
    }

    // ---- 增量选章：先跟台账对账，只挑「这次真的该跑」的章 ----
    // 放在建任务行之前：这样「没新章 / 没变章 / 成品已在」时能直接返回已是最新，
    // 既不留任务行、也不花一个 token。
    let covered = load_covered(&storage, &ns, &book_url)?;
    let product_exists = block_on(storage.graph_get(&ns, &book_url))
        .map_err(|e| anyhow!("读取已有关系图失败: {e}"))?
        .is_some();
    let product_scanned = if product_exists {
        block_on(storage.graph_chapters_scanned(&ns, &book_url))
            .map_err(|e| anyhow!("读取已有关系图失败: {e}"))?
    } else {
        0
    };
    let inc = plan_incremental(&prepared.entries, &covered);
    let mut plans = inc.batches;
    // 没有新章、没有变章、成品已在 → 已是最新：不建任务、不烧 token。
    // （选章函数在这种情况下会用「最久没覆盖的老章」把预算填满，所以这里必须先拦住，
    //  否则用户每点一次「生成」都要重抽 240 章。）
    if inc.fresh == 0 && inc.changed == 0 && product_exists {
        if product_scanned.max(0) as usize >= covered.len() {
            return Ok(StartOutcome::UpToDate {
                covered_chapters: covered.len(),
            });
        }
        // 台账比成品多：上次跑到一半就失败/取消了。历史 unit 都在，
        // 0 批跑一次纯代码重归并（不花 token）就能把成品补齐。
        plans.clear();
    }
    if let Some(cap) = opts.max_batches {
        plans.truncate(cap);
    }
    if plans.is_empty() && !product_exists {
        let units = block_on(storage.graph_unit_max_seq(&ns, &book_url))
            .map_err(|e| anyhow!("读取批次沉淀失败: {e}"))?;
        // 没成品也没历史批次 → 真的没东西可做
        if units == 0 {
            return Ok(StartOutcome::UpToDate {
                covered_chapters: covered.len(),
            });
        }
    }
    let total_batches = plans.len();
    let chapters_selected: usize = plans.iter().map(|p| p.chapters.len()).sum();

    let job_id = uuid::Uuid::new_v4().simple().to_string();
    let progress = Arc::new(Mutex::new(GraphProgress {
        job_id: job_id.clone(),
        status: ST_QUEUED.to_string(),
        // 选章已在建行前算完，这里是本次**真实**批数（历史批不计入）
        total_batches,
        chapters_scanned: 0,
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
            prepared,
            plans,
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

    Ok(StartOutcome::Started {
        job_id,
        total_batches,
        chapters_selected,
        reused: false,
    })
}

fn progress_batches(p: &Arc<Mutex<GraphProgress>>) -> usize {
    lock_progress(p).total_batches
}

/// 读台账 → `chapter_key → (content_hash, done_at)`（增量选章的唯一输入）
///
/// `content_hash` 库里存 i64（SQLite 只有有符号整数），这里按位回 u64。
fn load_covered(
    storage: &Storage,
    ns: &str,
    book_url: &str,
) -> anyhow::Result<BTreeMap<i64, Covered>> {
    let rows = block_on(storage.graph_ledger(ns, book_url))
        .map_err(|e| anyhow!("读取章节覆盖台账失败: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(key, _ordinal, _title, hash, done_at)| {
            (
                key,
                Covered {
                    content_hash: hash as u64,
                    done_at,
                },
            )
        })
        .collect())
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
/// 只作为没有台账数据时的兜底（真选章走 [`plan_incremental`]）。
pub fn estimate_batches(cached_chapters: usize) -> usize {
    if cached_chapters == 0 {
        return 0;
    }
    cached_chapters
        .div_ceil(super::PER_BATCH)
        .min(super::BATCHES)
}

/// 生成前预估的结果（只读本地库，不调用模型、不花钱）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Preview {
    /// 该书已缓存章数
    pub cached: usize,
    /// 台账里已抽取过（且正文没变）的章数：本次不会再送模型
    pub covered: usize,
    /// 本次要覆盖的章数（增量口径：已覆盖且没变的不算）
    pub chapters: usize,
    /// 本次要跑的批数
    pub batches: usize,
    /// 本次进 prompt 的 token 估计（按真实批次消息的字符数算，不是「章数 × 系数」）
    pub prompt_tokens: u64,
    /// 本次输出 token 估计
    pub completion_tokens: u64,
}

/// 生成前预估：走真选章 + 真拼提示词，再按**实际字符数**估 token。
///
/// 之前按「已缓存章数 × 4300 token」拍，把全书正文都算了进去，虚高约 7 倍——
/// 实际进 prompt 的只有本次选的 ≤ `BATCHES*PER_BATCH` 章、且每批 ≤ `BUDGET` 字。
pub fn preview(storage: &Storage, ns: &str, book_url: &str) -> Result<Preview> {
    let prepared = prepare_chapters(storage, ns, book_url, "")?;
    let cached = prepared.entries.len();
    if cached == 0 {
        return Ok(Preview::default());
    }
    let covered = load_covered(storage, ns, book_url)?;
    // 与 `start_job` 同一套「已是最新」判定：没新章、没变章、成品已在且扫够
    // → 预估也必须报 0 批，否则前端显示「6 批 240 章」而真点下去返回 upToDate，
    //   两个接口自相矛盾（实测过：全覆盖后预估仍报 240 章/6 批）。
    let product_exists = block_on(storage.graph_get(ns, book_url))
        .map_err(|e| anyhow!("读取已有关系图失败: {e}"))?
        .is_some();
    let product_scanned = if product_exists {
        block_on(storage.graph_chapters_scanned(ns, book_url))
            .map_err(|e| anyhow!("读取已有关系图失败: {e}"))?
    } else {
        0
    };
    if product_exists && product_scanned.max(0) as usize >= covered.len() {
        let inc_probe = plan_incremental(&prepared.entries, &covered);
        if inc_probe.fresh == 0 && inc_probe.changed == 0 {
            return Ok(Preview {
                cached,
                covered: covered.len(),
                ..Default::default()
            });
        }
    }
    let inc = plan_incremental(&prepared.entries, &covered);
    // 中文≈1 token/字（略保守），英文与 JSON 骨架占得更少，不再另做换算
    let prompt_chars: usize = inc
        .batches
        .iter()
        .flat_map(|plan| {
            build_batch_messages(
                &prepared.hints.book_name,
                plan,
                prepared.hints.total_chapters,
            )
        })
        .filter_map(|m| {
            m.get("content")
                .and_then(Value::as_str)
                .map(str::chars)
                .map(Iterator::count)
        })
        .sum();
    Ok(Preview {
        cached,
        covered: covered.len(),
        chapters: inc.chapter_count(),
        batches: inc.batches.len(),
        prompt_tokens: prompt_chars as u64,
        completion_tokens: (inc.batches.len() as u64).saturating_mul(3_500),
    })
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
    prepared: PreparedBook,
    plans: Vec<BatchPlan>,
    progress: Arc<Mutex<GraphProgress>>,
) -> Result<()> {
    // 选章已在 start_job 里做完（要据此决定「要不要建任务」），这里只负责跑。
    let total = plans.len();
    set_total_batches(&storage, &job_id, total).await?;
    {
        let mut p = lock_progress(&progress);
        p.status = ST_RUNNING.to_string();
        // 进度按**本次**批数报，历史批不计入
        p.total_batches = total;
    }
    // 序数 → 章条目：批次里只有 (序数, 标题, 正文)，落台账还要章键与正文哈希
    let by_ordinal: HashMap<i64, &ChapterEntry> =
        prepared.entries.iter().map(|e| (e.chapter.0, e)).collect();
    // unit 序号接在历史之后，历史 unit 永不被覆盖
    let mut seq = block_on(storage.graph_unit_max_seq(&ns, &book_url))?;

    let mut done = 0usize;
    let mut fed = 0usize;
    let mut usage = (0u64, 0u64);
    for plan in &plans {
        // 每批之间检查取消：已完成的批已沉淀成 unit + 台账，不丢
        if is_cancelled(&progress) {
            return finish_cancelled(&storage, &job_id, &progress, done, fed).await;
        }
        let messages = build_batch_messages(&opts.book_name, plan, prepared.hints.total_chapters);
        let value = batch_with_retry(&cfg, messages)
            .await
            .with_context(|| format!("第 {} 批失败", plan.batch_no))?;
        let raw = value.to_string();
        // 历史遗留表照写（排查用），新逻辑不依赖它
        storage
            .graph_batch_put(&job_id, plan.batch_no as i64, &raw)
            .await?;
        // 立即沉淀：unit + 台账同一事务写入。此后即便任务失败/取消，这一批也不白跑
        seq += 1;
        let metas: Vec<(i64, i64, String, i64)> = plan
            .chapters
            .iter()
            .filter_map(|c| by_ordinal.get(&c.0).copied())
            .map(|e| {
                (
                    e.key,
                    e.chapter.0,
                    e.chapter.1.clone(),
                    e.content_hash as i64,
                )
            })
            .collect();
        storage
            .graph_unit_commit(&ns, &book_url, &job_id, seq, &raw, &metas)
            .await?;
        done += 1;
        fed += plan.chapters.len();
        usage.0 += value
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage.1 += value
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // 拿到了真实 usage 就报真值；拿不到（网关不回 usage）才退回经验估算
        let (prompt_tokens, completion_tokens) = if usage.0 > 0 || usage.1 > 0 {
            usage
        } else {
            estimate(prepared.entries.len(), done)
        };
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

    // 全部批完成：汇总该书**所有** unit（历史 + 本次）→ 归一化 → 落成品。
    // 归并是纯代码不花 token，历史数据留着只会让家族归并更准。
    let chapters = prepared.chapters();
    let raws: Vec<Value> = storage
        .graph_units(&ns, &book_url)
        .await?
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .collect();
    if raws.is_empty() {
        return Err(anyhow!("没有可归一化的批次结果"));
    }
    let graph = normalize(&raws, &chapters, &prepared.hints);
    let characters = array_len(&graph, "characters");
    let relations = array_len(&graph, "relations");
    // 台账覆盖数（不是本次 fed）：成品里的 chapters_scanned 要反映全书已覆盖量
    let covered_total = storage.graph_ledger_count(&ns, &book_url).await?;
    // 兜底：模型这次一个字都没抽出（全被 quote 校验/字段校验丢掉）时，别把已有的好成品
    // 覆盖成空图 —— 实测过假输出会把 1745 章的成品清成 0 人 0 关系。
    {
        let old_chars = match storage.graph_get(&ns, &book_url).await? {
            Some(raw) => serde_json::from_str::<Value>(&raw)
                .map(|v| array_len(&v, "characters"))
                .unwrap_or(0),
            None => 0,
        };
        if keep_old_on_empty(characters, relations, old_chars) {
            tracing::warn!(
                "图谱归并结果为空（本次 {fed} 章），保留原成品 {old_chars} 人，不覆盖 [{ns}/{book_url}]"
            );
            {
                let mut p = lock_progress(&progress);
                p.status = ST_DONE.to_string();
                p.done_batches = done;
                p.chapters_scanned = fed;
            }
            let _ = storage.graph_job_set_status(&job_id, ST_DONE, None).await;
            return Ok(());
        }
    }
    storage
        .graph_save(
            &ns,
            &book_url,
            &serde_json::to_string(&graph)?,
            characters as i64,
            relations as i64,
            covered_total,
            &job_id,
        )
        .await?;
    {
        let mut p = lock_progress(&progress);
        p.status = ST_DONE.to_string();
        p.done_batches = done;
        // 进度里报本次扫的章数（用户看的是「这次干了多少」）
        p.chapters_scanned = fed;
        p.characters = characters;
        p.relations = relations;
    }
    let _ = storage.graph_job_set_status(&job_id, ST_DONE, None).await;
    Ok(())
}

/// 跑一批：失败重试 1 次（网络/模型抖一下很常见），两次都失败才放弃。
///
/// 拿到 usage 就塞进批次 JSON 的 `usage` 字段：`normalize` 的 `token_cost` 从这里汇总
/// `meta.cost`（网关不回 usage 时不塞，别硬编 0）。
async fn batch_with_retry(cfg: &AiConfig, messages: Vec<Value>) -> Result<Value> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..2 {
        match ai::chat_once_with_usage(cfg, messages.clone(), MAX_BATCH_TOKENS).await {
            Ok((raw, (prompt_tokens, completion_tokens))) => match ai::extract_json_object(&raw) {
                Some(mut value) => {
                    if prompt_tokens > 0 || completion_tokens > 0 {
                        if let Some(obj) = value.as_object_mut() {
                            obj.insert(
                                "usage".to_string(),
                                json!({
                                    "prompt_tokens": prompt_tokens,
                                    "completion_tokens": completion_tokens,
                                }),
                            );
                        }
                    }
                    return Ok(value);
                }
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
    use crate::service::graph::plan::INCREMENTAL_CHAPTER_BUDGET;
    use crate::service::graph::plan_batches;

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

    /// 造 N 章已缓存正文（章序 i，正文带 i 以便哈希不同）
    async fn seed_chapters(storage: &Storage, ns: &str, url: &str, n: i64) {
        for i in 1..=n {
            storage
                .cache_chapter_content(
                    ns,
                    url,
                    i * 100,
                    &format!("第{i}章 测试"),
                    &format!(
                        "第{i}章正文：{}",
                        "他进了城，街上人来人往。".repeat(i as usize)
                    ),
                )
                .await
                .expect("写章节缓存");
        }
    }

    /// 台账幂等：同一批重复提交不会多出行、不会重复计数
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unit_commit_is_idempotent() {
        let (storage, dir) = temp_storage("ledger-idempotent");
        let ns = "default";
        let url = "https://example.com/book/ledger-idempotent";
        let metas = vec![
            (101i64, 1i64, "第1章 测试".to_string(), 111i64),
            (201, 2, "第2章 测试".to_string(), 222),
        ];
        for _ in 0..3 {
            storage
                .graph_unit_commit(ns, url, "job-x", 1, "{\"characters\":[]}", &metas)
                .await
                .expect("提交批");
        }
        assert_eq!(
            storage.graph_ledger_count(ns, url).await.expect("台账数"),
            2,
            "重复标同一章不该重复计数"
        );
        assert_eq!(
            storage
                .graph_unit_max_seq(ns, url)
                .await
                .expect("unit 序号"),
            1,
            "同一 seq 重复提交应覆盖，不新增"
        );
        assert_eq!(
            storage.graph_units(ns, url).await.expect("unit 列表").len(),
            1,
            "unit 行数不该变"
        );
        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 全覆盖 + 成品已在 → 直接返回「已是最新」，不建任务、不烧 token
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_job_up_to_date_when_fully_covered() {
        let (storage, dir) = temp_storage("up-to-date");
        let ns = "default";
        let url = "https://example.com/book/up-to-date";
        seed_chapters(&storage, ns, url, 3).await;

        // 台账：3 章都覆盖过，哈希与当前正文一致
        let prepared = prepare_chapters(&storage, ns, url, "").expect("章节");
        let metas: Vec<(i64, i64, String, i64)> = prepared
            .entries
            .iter()
            .map(|e| {
                (
                    e.key,
                    e.chapter.0,
                    e.chapter.1.clone(),
                    e.content_hash as i64,
                )
            })
            .collect();
        storage
            .graph_unit_commit(ns, url, "job-old", 1, "{\"characters\":[]}", &metas)
            .await
            .expect("提交批");
        storage
            .graph_save(ns, url, "{\"characters\":[]}", 0, 0, 3, "job-old")
            .await
            .expect("落成品");

        let outcome = start_job(
            storage.clone(),
            ns.to_string(),
            url.to_string(),
            JobOptions::default(),
            ai_cfg(),
        )
        .expect("启动应成功");
        assert_eq!(
            outcome,
            StartOutcome::UpToDate {
                covered_chapters: 3
            },
            "全覆盖且成品已在，必须不启动任务"
        );
        assert!(
            mem_progress(url).is_none(),
            "已是最新时不该留内存任务（否则 SSE 会挂住）"
        );

        // 老章正文被改了 → 下次生成必须重抽这一章
        storage
            .cache_chapter_content(ns, url, 200, "第2章 测试", "第2章正文：作者改过的一段。")
            .await
            .expect("改正文");
        let prepared = prepare_chapters(&storage, ns, url, "").expect("章节");
        let covered = load_covered(&storage, ns, url).expect("台账");
        let inc = plan_incremental(&prepared.entries, &covered);
        let picked: Vec<i64> = inc
            .batches
            .iter()
            .flat_map(|b| b.chapters.iter().map(|c| c.0))
            .collect();
        assert_eq!(inc.changed, 1, "只有被改的那章是变章");
        assert!(
            picked.contains(&2),
            "正文变化的章应被重抽（其余是预算内的老章补抽）"
        );

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

    // ---- 真实生产库验证（`#[ignore]`：只在手工验收时跑，别进 CI） ----

    /// 生产库副本的 work_dir（`reader.db` 在 `<work_dir>/storage/` 下，只读消费）
    const PROD_WORKDIR: &str = "/home/hehecat/code/.tmp/agent-graph";
    /// 《玄鉴仙族》的 book_url（chapter_index 是 md5 哈希的那本）
    const PROD_BOOK: &str = "http://www.xbotaodz.info/455131.html";
    /// 生产库里这个 namespace 的行（不是 default）
    const PROD_NS: &str = "testuser01";

    /// 打开生产库副本；副本不存在就返回 None（让人看到 SKIP 而不是 panic）
    fn prod_storage() -> Option<Storage> {
        let dir = std::path::Path::new(PROD_WORKDIR);
        if !dir.join("storage/reader.db").exists() {
            println!("SKIP: 没有生产库副本 {PROD_WORKDIR}/storage/reader.db");
            return None;
        }
        let mut config = crate::AppConfig::from_env();
        config.work_dir = PROD_WORKDIR.to_string();
        Some(block_on(crate::storage::init(&config)).expect("打开生产库副本失败"))
    }

    /// 增量验证（**不调模型**，只跑选章 + 台账）：
    /// ① 台账空时首次选多少章 → ② 标 240 章后第二次选哪些章 → ③ 全覆盖后是否直接不启动
    /// → ④ 只剩 100 新章时跑几批（回答「几天更 100 章要花几批钱」）
    ///
    /// 在生产库**副本**上跑（拷到 temp，不动原副本）：
    /// ```bash
    /// cargo test --lib prod_db_incremental_selection -- --ignored --nocapture
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要 .tmp/agent-graph 里的生产库副本"]
    async fn prod_db_incremental_selection() {
        let src = std::path::Path::new(PROD_WORKDIR)
            .join("storage")
            .join("reader.db");
        if !src.exists() {
            println!("SKIP: 没有生产库副本 {}", src.display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("reader-graph-incr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 注意：init 找的是 {work_dir}/storage/reader.db，副本必须放这个位置
        std::fs::create_dir_all(dir.join("storage")).expect("建临时目录");
        std::fs::copy(&src, dir.join("storage").join("reader.db")).expect("复制生产库副本");
        let mut config = crate::AppConfig::from_env();
        config.work_dir = dir.to_string_lossy().into_owned();
        let storage = block_on(crate::storage::init(&config)).expect("打开库副本");

        // 模拟老用户：台账为空（老版本的 book_graph_chapters 是新建的表）
        storage
            .graph_ledger_reset(PROD_NS, PROD_BOOK)
            .await
            .expect("清台账");
        let prepared = prepare_chapters(&storage, PROD_NS, PROD_BOOK, "").expect("章节");
        let cached = prepared.entries.len();
        let has_product = block_on(storage.graph_get(PROD_NS, PROD_BOOK))
            .expect("读成品")
            .is_some();
        let old_scanned =
            block_on(storage.graph_chapters_scanned(PROD_NS, PROD_BOOK)).expect("读成品扫描数");
        println!(
            "== 生产库副本：已缓存 {cached} 章，旧成品={has_product}，成品记 scanned={old_scanned}"
        );

        let by_ordinal: HashMap<i64, &ChapterEntry> =
            prepared.entries.iter().map(|e| (e.chapter.0, e)).collect();
        let show = |tag: &str, inc: &crate::service::graph::IncrementalPlan| -> Vec<i64> {
            let ords: Vec<i64> = inc
                .batches
                .iter()
                .flat_map(|b| b.chapters.iter().map(|c| c.0))
                .collect();
            println!(
                "{tag}: {} 批 / {} 章，章序 {:?}..{:?}，新章={} 变章={} 补老章={}",
                inc.batches.len(),
                ords.len(),
                ords.first().copied().unwrap_or(0),
                ords.last().copied().unwrap_or(0),
                inc.fresh,
                inc.changed,
                inc.backfill
            );
            ords
        };

        // ① 台账空 → 首次
        let covered = load_covered(&storage, PROD_NS, PROD_BOOK).expect("台账");
        let inc = plan_incremental(&prepared.entries, &covered);
        let first = show("第 1 次（台账空）", &inc);
        assert_eq!(first.len(), INCREMENTAL_CHAPTER_BUDGET, "首次应跑满预算");
        assert_eq!(first[0], 1, "首次从第 1 章开始");

        // 把这批标成已覆盖（模拟它们成功跑完）
        let mut seq = 0i64;
        for batch in &inc.batches {
            seq += 1;
            let metas: Vec<(i64, i64, String, i64)> = batch
                .chapters
                .iter()
                .filter_map(|c| by_ordinal.get(&c.0).copied())
                .map(|e| {
                    (
                        e.key,
                        e.chapter.0,
                        e.chapter.1.clone(),
                        e.content_hash as i64,
                    )
                })
                .collect();
            storage
                .graph_unit_commit(PROD_NS, PROD_BOOK, "job-prod-1", seq, "{}", &metas)
                .await
                .expect("提交批");
        }
        let ledger = storage
            .graph_ledger_count(PROD_NS, PROD_BOOK)
            .await
            .expect("台账数");
        println!("已标 {ledger} 章为已抽取");

        // ② 再跑一次 → 应该挑**新的**章，不再重抽前 240 章
        let covered = load_covered(&storage, PROD_NS, PROD_BOOK).expect("台账");
        let inc = plan_incremental(&prepared.entries, &covered);
        let second = show("第 2 次（已标 240 章）", &inc);
        assert!(
            second.iter().all(|o| *o > first[first.len() - 1]),
            "第二次不该重抽已覆盖的章"
        );

        // ③ 全部覆盖 → 没有新章也没有变章
        let mut seq = 100i64;
        let mut rest: Vec<(i64, i64, String, i64)> = Vec::new();
        for e in &prepared.entries {
            rest.push((
                e.key,
                e.chapter.0,
                e.chapter.1.clone(),
                e.content_hash as i64,
            ));
        }
        seq += 1;
        storage
            .graph_unit_commit(PROD_NS, PROD_BOOK, "job-prod-2", seq, "{}", &rest)
            .await
            .expect("提交全部");
        let covered = load_covered(&storage, PROD_NS, PROD_BOOK).expect("台账");
        let inc = plan_incremental(&prepared.entries, &covered);
        println!(
            "第 3 次（全覆盖）：新章={} 变章={}（成品记 scanned={old_scanned}）",
            inc.fresh, inc.changed
        );
        assert_eq!(inc.fresh, 0, "全覆盖后不该有新章");
        assert_eq!(inc.changed, 0, "正文没变就不该有变章");

        // 成品也已经反映全部覆盖 → start_job 必须直接返回「已是最新」，不建任务
        storage
            .graph_save(
                PROD_NS,
                PROD_BOOK,
                "{\"characters\":[]}",
                0,
                0,
                covered.len() as i64,
                "job-prod-2",
            )
            .await
            .expect("落成品");
        let outcome = start_job(
            storage.clone(),
            PROD_NS.to_string(),
            PROD_BOOK.to_string(),
            JobOptions::default(),
            ai_cfg(),
        )
        .expect("启动应成功");
        println!("全覆盖后再点生成 → {outcome:?}");
        assert_eq!(
            outcome,
            StartOutcome::UpToDate {
                covered_chapters: cached
            },
            "全覆盖 + 成品已在 → 不该启动任务"
        );

        // ④ 几天后新增 100 章（= 台账里没有的 100 章）→ 只跑这 100 章
        let covered = load_covered(&storage, PROD_NS, PROD_BOOK).expect("台账");
        let cut = prepared.entries.len() - 100;
        let partial: BTreeMap<i64, Covered> = prepared.entries[..cut]
            .iter()
            .filter_map(|e| covered.get(&e.key).map(|c| (e.key, *c)))
            .collect();
        let inc = plan_incremental(&prepared.entries, &partial);
        let fresh = show("新增 100 章", &inc);
        assert_eq!(fresh.len(), 100, "只该跑新增的 100 章");

        storage.pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 链路验证 ①：真实库里「取章节 → 按目录排阅读序 → 重编号 1..N → plan_batches」
    ///
    /// ```bash
    /// cargo test --lib prod_db_chapter_numbering -- --ignored --nocapture
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要 .tmp/agent-graph 里的生产库副本"]
    async fn prod_db_chapter_numbering() {
        let Some(storage) = prod_storage() else {
            return;
        };
        let prepared =
            prepare_chapters(&storage, PROD_NS, PROD_BOOK, "").expect("prepare_chapters");
        let hints = &prepared.hints;
        let chapters = prepared.chapters();
        println!(
            "书名={:?} total_chapters={} 已缓存章数={}",
            hints.book_name,
            hints.total_chapters,
            chapters.len()
        );
        assert_eq!(hints.book_name, "玄鉴仙族", "书名应取自 books.name");
        assert_eq!(
            hints.total_chapters, 1745,
            "总章数应取 books.total_chapter_num"
        );
        let nums: Vec<i64> = chapters.iter().map(|c| c.0).collect();
        assert_eq!(nums[0], 1, "第一章的序数必须是 1");
        assert!(
            nums.windows(2).all(|w| w[0] + 1 == w[1]),
            "序数应是连续的 1..N"
        );

        println!("前 5 章（序数 + 目录标题）：");
        for (idx, title, _) in chapters.iter().take(5) {
            println!("  {idx:>2}. {title}");
        }

        let plans = plan_batches(&chapters);
        println!(
            "批数={} 各批章数={:?}",
            plans.len(),
            plans.iter().map(|p| p.chapters.len()).collect::<Vec<_>>()
        );
        let msgs = build_batch_messages(&hints.book_name, &plans[0], hints.total_chapters);
        let prompt = msgs
            .iter()
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        println!("第一批 prompt 里的前 5 个章节头：");
        for line in prompt.lines().filter(|l| l.starts_with("【第")).take(5) {
            println!("  {line}");
        }
        assert!(
            prompt.contains("【第1章"),
            "prompt 里的章号应是真实序数，不是 md5"
        );
        assert!(
            !prompt.contains("1151345960693571501"),
            "prompt 里不该再出现 md5 章号"
        );
        let total_line = format!("1..{}", hints.total_chapters);
        assert!(prompt.contains(&total_line), "prompt 应写明全书总章数");
        storage.pool.close().await;
    }

    /// 链路验证 ②：真实 `book_graph_batches`（6 批）喂给 normalize，看势力归并前后
    ///
    /// ```bash
    /// cargo test --lib prod_db_faction_merge -- --ignored --nocapture
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要 .tmp/agent-graph 里的生产库副本"]
    async fn prod_db_faction_merge() {
        let Some(storage) = prod_storage() else {
            return;
        };
        let job_id: Option<String> = block_on(
            sqlx::query_scalar::<_, String>(
                "SELECT job_id FROM book_graph_jobs WHERE book_url = ?1 \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(PROD_BOOK)
            .fetch_optional(&storage.pool),
        )
        .expect("查 job_id");
        let Some(job_id) = job_id else {
            println!("SKIP: 库里没有这本书的 graph 任务");
            return;
        };
        let batches = block_on(storage.graph_batches(&job_id)).expect("读批次");
        println!("job={job_id} 批数={}", batches.len());
        let raws: Vec<Value> = batches
            .iter()
            .filter_map(|(no, raw)| {
                let v: Value = serde_json::from_str(raw).ok()?;
                println!(
                    "  批{no}: characters={} factions={} relations={}",
                    array_len(&v, "characters"),
                    array_len(&v, "factions"),
                    array_len(&v, "relations")
                );
                Some(v)
            })
            .collect();

        // 归并前：各批 declared/提到的势力名去重（约等于归并前的势力条数）
        let mut before: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for v in &raws {
            for f in v
                .get("factions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = f.get("name").and_then(Value::as_str) {
                    before.insert(name.trim().to_string());
                }
            }
            for c in v
                .get("characters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = c.get("faction").and_then(Value::as_str) {
                    before.insert(name.trim().to_string());
                }
            }
        }

        let prepared = prepare_chapters(&storage, PROD_NS, PROD_BOOK, "").expect("章节");
        let hints = &prepared.hints;
        let chapters = prepared.chapters();
        let data = normalize(&raws, &chapters, hints);
        let factions = data["factions"].as_array().expect("factions");
        println!(
            "势力：归并前候选 {} 个 → 归并后 {} 个（meta.totalChapters={} book={}）",
            before.len(),
            factions.len(),
            data["meta"]["totalChapters"],
            data["meta"]["book"]
        );
        for f in factions {
            println!(
                "  {:<4} {:<10} kind={:<4} 成员={:<3} aliases={:?}",
                f["id"].as_str().unwrap_or(""),
                f["name"].as_str().unwrap_or(""),
                f["kind"].as_str().unwrap_or(""),
                f["members"].as_array().map_or(0, Vec::len),
                f["aliases"]
            );
        }
        assert!(factions.len() < before.len(), "归并后势力数应减少");
        assert_eq!(data["meta"]["book"], "玄鉴仙族", "meta.book 不该是未知作品");
        assert_eq!(data["meta"]["totalChapters"], 1745);
        // memberships / character.faction 不许指向已不存在的势力
        let ids: std::collections::BTreeSet<&str> =
            factions.iter().filter_map(|f| f["id"].as_str()).collect();
        for m in data["memberships"].as_array().into_iter().flatten() {
            let fid = m["faction"].as_str().unwrap_or_default();
            assert!(ids.contains(fid), "membership 指向已删势力 {fid}");
        }
        for c in data["characters"].as_array().into_iter().flatten() {
            if let Some(fid) = c["faction"].as_str() {
                assert!(ids.contains(fid), "character.faction 指向已删势力 {fid}");
            }
        }
        storage.pool.close().await;
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

#[cfg(test)]
mod keep_old_tests {
    use super::keep_old_on_empty;

    #[test]
    fn keep_old_on_empty_cases() {
        // 新结果空、旧成品有内容 → 保留旧的（不覆盖）
        assert!(keep_old_on_empty(0, 0, 131));
        // 新结果空、旧成品也空 → 照常落盘（没有可损失的东西）
        assert!(!keep_old_on_empty(0, 0, 0));
        // 新结果非空 → 正常覆盖
        assert!(!keep_old_on_empty(3, 2, 131));
        // 只有人物没关系的半空结果也算「非空」，允许覆盖
        assert!(!keep_old_on_empty(5, 0, 131));
    }
}
