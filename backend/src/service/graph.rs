//! 关系图谱（B 阶段）：把整本书的已缓存章节批量丢给模型，抽出人物/关系并落库。
//!
//! 口径来自离线管线 `scripts/graph-demo/`（唯一权威），这里是它的 Rust 移植：
//! - `plan.rs`      ← `02_make_prompt.py`（选章 + 提示词）
//! - `normalize.rs` ← `03_normalize.py` + `04_verify.py`（合并/校验/方向修复/收敛）
//! - `job.rs`       ← 后台任务（进度、逐批落库、取消），仿 `service/cache_job.rs`
//!
//! 目标产物是一份 `graph-demo-v1` JSON，前端 `GraphDemoPage` 直接渲染。

pub mod job;
pub mod normalize;
pub mod plan;

pub use job::{cancel, estimate, progress_of, start_job, GraphProgress, JobOptions};
pub use normalize::{normalize, repair_parent_direction};
pub use plan::{build_batch_messages, plan_batches};

/// 章节行：(chapter_index, 标题, 正文)
pub type Chapter = (i64, String, String);

/// 选章/拼提示词的参数（与 `02_make_prompt.py` 默认值一致）
pub const BATCHES: usize = 6;
pub const WIN_LEN: usize = 105;
pub const PER_BATCH: usize = 40;
pub const EXCERPT: usize = 2800;
/// 每批正文总字数上限（防超长 prompt）
pub const BUDGET: usize = 115_000;
/// 单批输出上限：思考关掉后 16k 足够装 40 章的关系
pub const MAX_BATCH_TOKENS: u64 = 16_000;

/// 规模收敛（与 `03_normalize.py` 一致）
pub const TARGET_CHARS: usize = 133;
pub const MAX_CHARS: usize = 139;
pub const MAX_RELATIONS: usize = 300;
pub const MAX_FACTIONS: usize = 10;
/// 单点度数上限占关系总数的比例
pub const MAX_DEGREE_RATIO: f64 = 0.12;

/// 前端契约里的关系类型（与 `frontend/src/types/graphDemo.ts` 对齐）
pub const RELATION_TYPES: &[&str] = &[
    "parent", "spouse", "sibling", "master", "disciple", "ally", "enemy", "other",
];

/// schema 版本号
pub const SCHEMA: &str = "graph-demo-v1";

/// 一批的输入：章已按 `EXCERPT` 裁剪好正文
#[derive(Debug, Clone)]
pub struct BatchPlan {
    /// 1 起
    pub batch_no: usize,
    /// window | kin | fill | bias（只用于日志/进度文案）
    pub kind: &'static str,
    /// (chapter_index, 标题, 已裁剪正文)
    pub chapters: Vec<Chapter>,
}
