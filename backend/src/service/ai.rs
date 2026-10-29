//! AI 阅读助手（P1：只读已缓存正文）
//!
//! 设计要点：
//! - **不做全书预索引**：一本书已缓存正文通常几 MB，提问时按关键词在已缓存章节里召回
//!   Top-K 片段（毫秒级），把片段拼进提示词；模型只允许引用这些片段。
//! - **查询侧分词**：问题用 jieba 切词（`萧炎 / 什么时候 / 得到 / 药老`），再按"书内稀缺度"
//!   丢掉泛词（df > 总章数/3）、按 idf×词长 排序。旧版把整句切 2-gram，会产出
//!   `是什 / 么时 / 候得` 这类噪声词——实测召回 24~29%，换 jieba 后 55~68%。
//! - **知识边界**：没读过的章节不可能回答——片段缺失时明确让用户先读/缓存更多章节。
//! - **引用可信**：回答要求按 `[第N章]` 标注，前端用 `title` 反查章节并跳转。
//! - 配置（缺一即视为未启用）：`READER_AI_BASE_URL` / `READER_AI_API_KEY` /
//!   `READER_AI_MODEL`（默认 deepseek-v4.1-flash）/ `READER_AI_MAX_TOKENS`
//!   （0 = 不限制，省略该字段交给模型/网关；达到上限时自动续写最多 2 轮）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Bytes;
use futures::StreamExt;
use jieba_rs::Jieba;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::storage::Storage;

/// 单次问答注入的章节片段总预算（字符）
const CONTEXT_BUDGET: usize = 6000;
/// 每次最多引用几章
const MAX_SNIPPETS: usize = 6;
/// 单个窗口的字符上限（比原来的 900 小：多个小窗口比少数大片段定位更准）
const SNIPPET_WINDOW: usize = 600;
/// 打分低于最高分这个比例的章节就不再取（省 token，也避免凑数塞噪声）
const SCORE_CUTOFF: f64 = 0.4;
/// 全书检索的放宽档：段数更多、门槛更低（「全书」范围用）
const BOOK_SNIPPETS: usize = 10;
const BOOK_SCORE_CUTOFF: f64 = 0.2;
/// 每章最多取几个窗口
const MAX_WINDOWS_PER_CHAPTER: usize = 2;
/// 问题关键词上限
const MAX_KEYWORDS: usize = 8;
/// 词性里的专名标签：nr 人名 / nz 其它专名 / ns 地名 / nt 机构名
const ENTITY_TAGS: [&str; 4] = ["nr", "nz", "ns", "nt"];
/// 专名关键词的权重下限（专名不受 df 过滤，也不能被"看中/身边"这类泛词挤掉）
const ENTITY_FLOOR: f64 = 3.0;

/// 章节评分：(分数, 章节下标)。
type ScoredChapter = (f64, usize);

/// 中文分词器：首次检索时加载内置词典（百毫秒级，之后常驻进程）
fn segmenter() -> &'static Jieba {
    static SEG: OnceLock<Jieba> = OnceLock::new();
    SEG.get_or_init(Jieba::new)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u64,
    /// 思考档位: ""/"off"(默认) / "low" / "medium" / "high"
    ///
    /// 默认关闭: 思考 token 也算在 `max_tokens` 里, 给不够时 content 会被挤成空串
    /// (`finish=length`)。114 题 gold 实测开启只值 +3 个点的准确率、总成本 +75%,
    /// 所以默认 off, 想要更"肯想"的答案才由用户在设置页打开。
    #[serde(default)]
    pub thinking: String,
}

/// 开思考时 `max_tokens` 至少要给这么多: 否则预算被思考吃光, content 为空
pub const THINKING_MIN_TOKENS: u64 = 3000;

impl AiConfig {
    /// 归一化档位: 未知值与空串都按 off 处理
    pub fn thinking_mode(&self) -> &'static str {
        match self.thinking.trim().to_ascii_lowercase().as_str() {
            "low" => "low",
            "medium" => "medium",
            "high" => "high",
            _ => "off",
        }
    }

    /// 请求里要带的 `reasoning_effort`; off 时返回 None(改用 thinking=disabled)
    fn thinking_effort(&self) -> Option<&'static str> {
        match self.thinking_mode() {
            "off" => None,
            mode => Some(mode),
        }
    }

    /// 实际输出预算: 开思考时给思考留够空间(0 = 不限制, 保持 0)
    pub fn output_budget(&self, max_tokens: u64) -> u64 {
        if self.thinking_effort().is_some() && max_tokens > 0 {
            max_tokens.max(THINKING_MIN_TOKENS)
        } else {
            max_tokens
        }
    }

    /// 给请求体加思考相关字段: off 显式关闭, 否则用 `reasoning_effort` 开档
    fn apply_thinking(&self, body: &mut Value) {
        match self.thinking_effort() {
            Some(effort) => {
                body["reasoning_effort"] = json!(effort);
            }
            None => {
                body["thinking"] = json!({ "type": "disabled" });
            }
        }
    }
}

/// 设置页保存的配置（存 system_settings 键 `ai_settings`），进程内缓存，保存即生效
static RUNTIME: std::sync::OnceLock<std::sync::RwLock<Option<AiConfig>>> =
    std::sync::OnceLock::new();
static LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn runtime() -> &'static std::sync::RwLock<Option<AiConfig>> {
    RUNTIME.get_or_init(|| std::sync::RwLock::new(None))
}

fn write_runtime(cfg: Option<AiConfig>) {
    if let Ok(mut guard) = runtime().write() {
        *guard = cfg;
    }
}

/// 配置来源：saved（设置页保存）/ env（环境变量）/ none
pub fn source() -> &'static str {
    if runtime()
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|_| ()))
        .is_some()
    {
        "saved"
    } else if env_config().is_some() {
        "env"
    } else {
        "none"
    }
}

/// 当前生效配置：设置页保存的优先，其次环境变量
pub fn config() -> Option<AiConfig> {
    if let Ok(guard) = runtime().read() {
        if let Some(cfg) = guard.as_ref() {
            return Some(cfg.clone());
        }
    }
    env_config()
}

/// 首次使用时从数据库载入设置页保存的配置（幂等）
pub async fn ensure_loaded(storage: &Storage) {
    if LOADED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    match storage.get_system_setting("ai_settings").await {
        Ok(Some(raw)) => match serde_json::from_str::<AiConfig>(&raw) {
            Ok(cfg) => {
                tracing::info!("AI 阅读助手: 已载入设置页保存的端点配置");
                write_runtime(Some(cfg));
            }
            Err(e) => tracing::warn!("AI 设置解析失败: {e}"),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("读取 AI 设置失败: {e}"),
    }
}

/// 保存配置到数据库并立即生效（无需重启）
pub async fn save(storage: &Storage, cfg: AiConfig) -> anyhow::Result<()> {
    let raw = serde_json::to_string(&cfg)?;
    storage.set_system_setting("ai_settings", &raw).await?;
    write_runtime(Some(cfg));
    LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 清空设置页保存的配置（回退到环境变量；都没有则 AI 面板不可用）
pub async fn clear(storage: &Storage) -> anyhow::Result<()> {
    storage.delete_system_setting("ai_settings").await?;
    write_runtime(None);
    LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 读取环境变量配置；缺少 base/key 时返回 None（AI 面板整体不可用）
fn env_config() -> Option<AiConfig> {
    let base_url = std::env::var("READER_AI_BASE_URL")
        .ok()?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let api_key = std::env::var("READER_AI_API_KEY").ok()?.trim().to_string();
    if base_url.is_empty() || api_key.is_empty() {
        return None;
    }
    let model = std::env::var("READER_AI_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "deepseek-v4.1-flash".to_string());
    let max_tokens = std::env::var("READER_AI_MAX_TOKENS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(3500); // 0 = 不限制(请求里省略 max_tokens)
    let thinking = std::env::var("READER_AI_THINKING")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default(); // 空 = 关闭思考
    Some(AiConfig {
        base_url,
        api_key,
        model,
        max_tokens,
        thinking,
    })
}

pub fn enabled() -> bool {
    config().is_some()
}

/// 检索结果：片段 + 已缓存章节数（用于区分"没有缓存"与"没检索到"）
pub struct Retrieval {
    pub snippets: Vec<Snippet>,
    pub cached_chapters: usize,
}

/// 一条检索片段
pub struct Snippet {
    pub index: i64,
    pub title: String,
    pub text: String,
    /// 该章在全书里的位置（1 起；0 = 未知）。模型靠它判断先后。
    pub order: usize,
}

/// 常见疑问词/虚词：不参与检索，避免"什么时候/为什么"把整本书都命中
const STOPWORDS: &[&str] = &[
    "什么",
    "怎么",
    "为什么",
    "如何",
    "哪里",
    "哪个",
    "哪些",
    "多少",
    "是不是",
    "有没有",
    "之后",
    "之前",
    "后来",
    "然后",
    "现在",
    "这个",
    "那个",
    "这些",
    "那些",
    "时候",
    "请问",
    "帮我",
    "一下",
    "他们",
    "我们",
    "你们",
    "这里",
    "那里",
    "因为",
    "所以",
    "但是",
    "可以",
    "还是",
    "就是",
    "已经",
    "一个",
    "没有",
    "知道",
    "告诉我",
    "讲讲",
    "说说",
    "关于",
    "相关",
    "内容",
    "情节",
    "哪一章",
    "哪几章",
    "第几章",
    "几章",
    "第一次",
    "首次",
    "最初",
    "最早",
    "那一段",
    "哪一段",
    "这一段",
    "这是",
    "那是",
    "出场",
    "登场",
];

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 提问里表示"先后/首次"的词：命中时把分数接近的章节按章节序排列，
/// 让"X 是什么时候出场的"优先看到最早出现那章，而不是全书出现次数最多的那章。
const TIME_HINTS: &[&str] = &[
    "什么时候",
    "何时",
    "哪一章",
    "哪几章",
    "第几章",
    "几章",
    "第一次",
    "首次",
    "最初",
    "最早",
    "先后",
    "先得到",
];

fn is_time_question(question: &str) -> bool {
    TIME_HINTS.iter().any(|h| question.contains(h))
}

/// 从问题里取关键词：jieba 分词+词性 → 去虚词/单字/纯数字 → 按"书内稀缺度"筛泛词 →
/// 按 idf×词长 降序取前 MAX_KEYWORDS 个，返回 (词, 权重)。
///
/// 为什么要 df 过滤：`得到`、`一章` 这类词在几百章里到处出现，命中数完全由它们主导，
/// 真名（`萧炎`）反而被稀释；只留下"书里出现但不泛滥"的词，检索才指向具体情节。
///
/// 但 df 过滤有条硬伤：**主角名本身就是最高频的词**。《绍宋》里`赵玖`出现在 300/395 章、
/// `岳飞`208、`杨沂中`207，全被当成泛词丢掉，结果"问主角的事反而查不到"——32 题 gold
/// 评测里召回失败的那几题几乎全是被丢名字的。jieba-rs 的 `tag()` 能给出词性，所以
/// nr/nz/ns/nt（人名/专名/地名/机构）不但不丢，还给一个权重下限，让它们排得进前 8。
fn question_keywords(question: &str, chapters: &[(i64, String, String)]) -> Vec<(String, f64)> {
    let seg = segmenter();
    let mut cand: Vec<(String, bool)> = Vec::new(); // (词, 是否专名)
    for token in seg.tag(question, true) {
        let w = token.word.trim();
        let n = w.chars().count();
        if !(2..=8).contains(&n) {
            continue;
        }
        if !w.chars().any(is_cjk) || w.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if STOPWORDS.contains(&w) {
            continue;
        }
        let is_entity = ENTITY_TAGS.contains(&token.tag);
        if !cand.iter().any(|(x, _)| x == w) {
            cand.push((w.to_string(), is_entity));
        }
    }
    let total = chapters.len();
    if total == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(f64, String)> = Vec::new();
    for (w, is_entity) in cand {
        let df = chapters.iter().filter(|ch| ch.2.contains(&w)).count();
        if df == 0 {
            continue; // 书里没有这个词：不是这本书的实体
        }
        if df * 3 > total && !is_entity {
            continue; // 太常见（泛词）
        }
        let idf = (1.0 + total as f64 / df as f64).ln();
        let len_w = w.chars().count().min(4) as f64;
        let weight = if is_entity {
            (idf * len_w).max(ENTITY_FLOOR)
        } else {
            idf * len_w
        };
        scored.push((weight, w));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.truncate(MAX_KEYWORDS);
    scored.into_iter().map(|(w, kw)| (kw, w)).collect()
}

/// 关键词在正文里的全部出现位置（字节下标，最多取 cap 个）
fn find_positions(content: &str, kw: &str, cap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if kw.is_empty() {
        return out;
    }
    let mut from = 0usize;
    while out.len() < cap {
        match content[from..].find(kw) {
            Some(i) => {
                let pos = from + i;
                out.push(pos);
                from = pos + kw.len();
            }
            None => break,
        }
    }
    out
}

/// 以命中点为锚，取一段窗口的字节区间（锚点前留 120 字上下文）
fn window_span(content: &str, pos: usize, limit: usize) -> (usize, usize) {
    let start = content[..pos]
        .char_indices()
        .rev()
        .nth(120)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = content[start..]
        .char_indices()
        .nth(limit)
        .map(|(i, _)| start + i)
        .unwrap_or(content.len());
    (start, end)
}

/// 把窗口收尾在句末（避免片段以半句话结束，模型容易读串）
fn trim_to_sentence(text: &str) -> String {
    const ENDS: [char; 8] = ['。', '！', '？', '…', '”', '」', '’', '\n'];
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    // 掐掉开头的半句
    let cut_head = chars
        .iter()
        .take((chars.len() / 4).max(1))
        .rposition(|c| ENDS.contains(c));
    let head_skip = cut_head.map(|i| i + 1).unwrap_or(0);
    // 尾部对齐句末
    let tail = &chars[head_skip..];
    if tail.last().is_some_and(|c| ENDS.contains(c)) {
        return tail.iter().collect::<String>().trim().to_string();
    }
    let cut = tail
        .iter()
        .enumerate()
        .filter(|(_, c)| ENDS.contains(c))
        .map(|(i, _)| i)
        .next_back();
    match cut {
        Some(i) if i * 2 >= tail.len() => tail[..=i].iter().collect::<String>(),
        _ => format!("{}…", tail.iter().collect::<String>().trim()),
    }
    .trim()
    .to_string()
}

/// 在正文里挑窗口：优先"覆盖不同关键词最多"的位置，最多取 max_n 段
fn best_windows(content: &str, kws: &[(String, f64)], limit: usize, max_n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut remaining: Vec<usize> = (0..kws.len()).collect();
    for _ in 0..max_n {
        if remaining.is_empty() {
            break;
        }
        let mut best: Option<(usize, usize, usize)> = None; // (不同关键词数, 命中位置, 关键词下标)
        for &ki in &remaining {
            for pos in find_positions(content, &kws[ki].0, 40) {
                let (start, end) = window_span(content, pos, limit);
                let slice = &content[start..end];
                let distinct = remaining
                    .iter()
                    .filter(|&&j| slice.contains(kws[j].0.as_str()))
                    .count();
                let better = match best {
                    None => true,
                    Some((bd, bp, _)) => distinct > bd || (distinct == bd && pos < bp),
                };
                if better {
                    best = Some((distinct, pos, ki));
                }
            }
        }
        let Some((_, pos, _)) = best else { break };
        let (start, end) = window_span(content, pos, limit);
        let text = trim_to_sentence(&content[start..end]);
        if text.is_empty() {
            break;
        }
        if !out.iter().any(|t| t == &text) {
            out.push(text);
        }
        let covered = out.last().cloned().unwrap_or_default();
        remaining.retain(|&j| !covered.contains(kws[j].0.as_str()));
    }
    out
}

/// 章节标题里的章号（"第12章 xxx" → 12），用于"无命中时取最近章节"
fn title_order(title: &str) -> Option<i64> {
    let digits: String = title
        .chars()
        .skip_while(|c| *c != '第')
        .skip(1)
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i64>().ok()
}

/// 章节在全书里的位置（1 起；0 = 未知）：优先认前端传上来的 TOC 标题序，其次认标题里的章号。
fn order_of(titles: &[String], title: &str) -> usize {
    if let Some(pos) = titles.iter().position(|t| t == title) {
        return pos + 1;
    }
    title_order(title)
        .map(|v| if v > 0 { v as usize } else { 0 })
        .unwrap_or(0)
}

/// 按 TOC（前端传上来的目录标题，阅读顺序）给已缓存章节排阅读序。
///
/// 书源书的 `chapter_index` 是 `md5(chapterUrl)` 哈希（见 `Storage::cache_chapter_content`），
/// 所以 `list_cached_chapters` 的 `ORDER BY chapter_index` 拿到的是**哈希序**，不是阅读顺序。
/// 下游凡是「当前章的前后章」「按章节先后排序」都必须走这里，不能靠行序 ±1。
/// 排序键：命中 TOC 的按下标；否则退回标题里的章号；两者都没有的保持原序垫底。
fn order_chapters_by_toc(
    mut chapters: Vec<(i64, String, String)>,
    titles: &[String],
) -> Vec<(i64, String, String)> {
    let pos: HashMap<&str, usize> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    chapters.sort_by_key(|c| match pos.get(c.1.as_str()) {
        Some(&p) => (0usize, p),
        None => match title_order(&c.1) {
            Some(o) => (1, o.max(0) as usize),
            None => (2, 0),
        },
    });
    chapters
}

/// 「当前章及之前」在已排好阅读序的 `chapters` 里的下标（阅读顺序升序，最后一个就是当前章）。
///
/// 读者问「到现在为止发生了什么」要的是累积上文，所以窗口是 TOC 的**前缀** `titles[0..=i]`
/// （当前章 + 之前所有已缓存章），**绝不取 `i+1` 及之后**——后一章读者还没读到，塞进去就是剧透。
/// 前缀可能很长，具体取到第几章由 `history_snippets` 的预算决定，这里只负责给出候选。
///
/// 邻章只认 TOC：书源书的行序是哈希序，`anchor ± 1` 取到的是随机章。TOC 里定位不到当前章时
/// （老客户端没传 `titles`、章节标题对不上、当前章正文还没缓存）退回标题/章号匹配，
/// 再不行就按已排好的阅读序以最后一章为当前章——三种退路都只取「当前章及其之前」。
fn history_slots(
    chapters: &[(i64, String, String)],
    titles: &[String],
    current_title: &str,
) -> Option<Vec<usize>> {
    if chapters.is_empty() {
        return None;
    }
    let cur = current_title.trim();
    let by_title: HashMap<&str, usize> = chapters
        .iter()
        .enumerate()
        .map(|(i, c)| (c.1.as_str(), i))
        .collect();
    // 正规路径：当前章在 TOC 里 → 取 titles[0..=i] 中已缓存的那几章
    if !cur.is_empty() {
        if let Some(ti) = titles.iter().position(|t| t == cur) {
            if by_title.contains_key(cur) {
                let slots: Vec<usize> = titles[..=ti]
                    .iter()
                    .filter_map(|t| by_title.get(t.as_str()).copied())
                    .collect();
                if !slots.is_empty() {
                    return Some(slots);
                }
            }
            tracing::debug!(
                current_title = cur,
                "AI 检索(current)：当前章不在已缓存正文里，退回标题/章号匹配"
            );
        } else {
            tracing::debug!(
                current_title = cur,
                toc_len = titles.len(),
                "AI 检索(current)：TOC 里定位不到当前章，退回标题/章号匹配"
            );
        }
    }
    // 退回路径：标题精确 → 章号 → 最后一章（读者正在读的通常就是它）
    let anchor = chapters
        .iter()
        .position(|c| c.1 == cur)
        .or_else(|| {
            let want = title_order(cur)?;
            chapters
                .iter()
                .position(|c| title_order(&c.1) == Some(want))
        })
        .unwrap_or_else(|| {
            tracing::debug!(
                current_title = cur,
                cached = chapters.len(),
                "AI 检索(current)：定位不到当前章，退化成最后一章"
            );
            chapters.len() - 1
        });
    Some((0..=anchor).collect())
}

/// 没有可用关键词时的兜底取样："主角是谁""刚才发生了什么"这类泛问。
/// 取开头两章（人物/世界首次出场）+ 当前章及其前一章（最近情节）+ 最后一章。
fn fallback_snippets(
    chapters: &[(i64, String, String)],
    current_title: &str,
    order_of: &dyn Fn(&str) -> usize,
) -> Vec<Snippet> {
    let mut ordered: Vec<&(i64, String, String)> = chapters.iter().collect();
    ordered.sort_by_key(|ch| order_of(&ch.1));
    let mut picked: Vec<&(i64, String, String)> = ordered.iter().take(2).copied().collect();
    if !current_title.trim().is_empty() {
        if let Some(pos) = ordered.iter().position(|ch| ch.1 == current_title) {
            if pos > 0 {
                picked.push(ordered[pos - 1]);
            }
            picked.push(ordered[pos]);
        }
    }
    picked.extend(ordered.iter().rev().take(1).copied());
    picked.sort_by_key(|ch| ch.0);
    picked.dedup_by_key(|ch| ch.0);

    let mut out: Vec<Snippet> = Vec::new();
    let mut budget = CONTEXT_BUDGET;
    for ch in picked {
        if budget == 0 {
            break;
        }
        let text: String = ch.2.chars().take(budget.min(1400)).collect();
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        budget = budget.saturating_sub(text.chars().count());
        out.push(Snippet {
            index: ch.0,
            order: order_of(&ch.1),
            title: ch.1.clone(),
            text: text.to_string(),
        });
    }
    out
}

/// 检索范围归一化。
///
/// 界面上只有两档：`current`（默认，只看当前章及之前已缓存的章）与 `book`（全书放宽）。
/// `auto`（全书关键词检索）保留但不暴露——少了它，用户不用猜"自动"到底偏哪边。
pub fn scope_of(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "book" | "all" | "whole" => "book",
        "auto" => "auto",
        _ => "current",
    }
}

/// 按范围取片段。
///
/// - `current`（默认）：只读当前章及之前已缓存的章（按阅读顺序的前缀），问"到现在为止发生了什么"时不会
///   被别处的高分章挤掉，也不会提前剧透下一章；
/// - `book`：全书关键词检索，放宽到 10 段、打分门槛降到 0.2，跨章线索/全书类问题用；
/// - `auto`：全书关键词检索的默认参数（6 段、门槛 0.4），界面上已不暴露。
pub async fn retrieve_scoped(
    storage: &Storage,
    ns: &str,
    book_url: &str,
    question: &str,
    titles: &[String],
    current_title: &str,
    scope: &str,
) -> Retrieval {
    match scope_of(scope) {
        "current" => retrieve_history(storage, ns, book_url, question, titles, current_title).await,
        "book" => {
            retrieve_inner(
                storage,
                ns,
                book_url,
                question,
                titles,
                current_title,
                Tuning {
                    max_snippets: BOOK_SNIPPETS,
                    score_cutoff: BOOK_SCORE_CUTOFF,
                },
            )
            .await
        }
        _ => retrieve(storage, ns, book_url, question, titles, current_title).await,
    }
}

/// 检索：已缓存章节里按关键词稀缺度加权打分，取前几章；每章截 1-2 个命中窗口。
///
/// 排序：常规问题按分数降序（同分按章节先后）；"什么时候/第一次/第几章"这类问题会把
/// 与最高分接近的章节改按章节序升序，保证最早出现那章能进片段。
pub async fn retrieve(
    storage: &Storage,
    ns: &str,
    book_url: &str,
    question: &str,
    titles: &[String],
    current_title: &str,
) -> Retrieval {
    retrieve_inner(
        storage,
        ns,
        book_url,
        question,
        titles,
        current_title,
        Tuning {
            max_snippets: MAX_SNIPPETS,
            score_cutoff: SCORE_CUTOFF,
        },
    )
    .await
}

/// 检索的调参。单独收成结构体而不是两个散参数，否则 retrieve_inner 会到 8 个参数。
#[derive(Clone, Copy)]
struct Tuning {
    max_snippets: usize,
    score_cutoff: f64,
}

async fn retrieve_inner(
    storage: &Storage,
    ns: &str,
    book_url: &str,
    question: &str,
    titles: &[String],
    current_title: &str,
    tuning: Tuning,
) -> Retrieval {
    let chapters = match storage.list_cached_chapters(ns, book_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("AI 检索读取章节失败: {e}");
            return Retrieval {
                snippets: Vec::new(),
                cached_chapters: 0,
            };
        }
    };
    if chapters.is_empty() {
        return Retrieval {
            snippets: Vec::new(),
            cached_chapters: 0,
        };
    }
    let cached_chapters = chapters.len();
    // 书源书的行序是 chapter_index 哈希序：关键词打分本身与顺序无关，但同分时要按
    // 阅读顺序挑，所以先统一按 TOC 重排（与 current 档共用同一份顺序）。
    let chapters = order_chapters_by_toc(chapters, titles);
    let order_of = |title: &str| order_of(titles, title);

    let kws = question_keywords(question, &chapters);
    if kws.is_empty() {
        return Retrieval {
            snippets: fallback_snippets(&chapters, current_title, &order_of),
            cached_chapters,
        };
    }

    let mut scored: Vec<(f64, usize)> = Vec::new();
    for (ci, ch) in chapters.iter().enumerate() {
        let mut score = 0.0f64;
        for (kw, weight) in &kws {
            let hits = ch.2.matches(kw.as_str()).count();
            if hits > 0 {
                score += (1.0 + hits.min(4) as f64) * weight;
            }
        }
        if score > 0.0 {
            scored.push((score, ci));
        }
    }
    if scored.is_empty() {
        return Retrieval {
            snippets: fallback_snippets(&chapters, current_title, &order_of),
            cached_chapters,
        };
    }

    let max_score = scored.iter().map(|x| x.0).fold(0.0f64, f64::max);
    if is_time_question(question) {
        let (mut strong, mut rest): (Vec<ScoredChapter>, Vec<ScoredChapter>) =
            scored.into_iter().partition(|x| x.0 >= max_score * 0.6);
        strong.sort_by_key(|x| order_of(&chapters[x.1].1));
        rest.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored = strong.into_iter().chain(rest).collect();
    } else {
        scored.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| order_of(&chapters[a.1].1).cmp(&order_of(&chapters[b.1].1)))
        });
    }

    let mut out: Vec<Snippet> = Vec::new();
    let mut budget = CONTEXT_BUDGET;
    for (score, ci) in scored {
        if out.len() >= tuning.max_snippets || budget < 200 {
            break;
        }
        if score < max_score * tuning.score_cutoff {
            break;
        }
        let ch = &chapters[ci];
        let windows = best_windows(
            &ch.2,
            &kws,
            SNIPPET_WINDOW.min(budget),
            MAX_WINDOWS_PER_CHAPTER,
        );
        if windows.is_empty() {
            continue;
        }
        let text = windows.join("\n…\n");
        let used = text.chars().count();
        if used > budget {
            continue;
        }
        budget -= used;
        out.push(Snippet {
            index: ch.0,
            order: order_of(&ch.1),
            title: ch.1.clone(),
            text,
        });
    }
    Retrieval {
        snippets: out,
        cached_chapters,
    }
}

/// `current` 范围的片段选择（纯逻辑，便于单测）。
///
/// 上下文 = TOC 里的**当前章及之前**（见 `history_slots`），按阅读顺序升序输出：最早的在前、
/// 当前章最后。预算：总量 ≤ `CONTEXT_BUDGET`，当前章先拿 50%，其余 50% 由先前章节**由近及远**
/// 共享（最近的一章先拿，没内容/用不完的份额顺延给更远的那章），每章另受
/// `SNIPPET_WINDOW × MAX_WINDOWS_PER_CHAPTER` 上限约束——所以通常只装得下最近 4~5 章。
fn history_snippets(
    chapters: Vec<(i64, String, String)>,
    titles: &[String],
    current_title: &str,
    question: &str,
) -> Vec<Snippet> {
    let chapters = order_chapters_by_toc(chapters, titles);
    let Some(slots) = history_slots(&chapters, titles, current_title) else {
        return Vec::new();
    };
    let kws = question_keywords(question, &chapters);
    let focus_share = CONTEXT_BUDGET / 2;
    let chapter_cap = SNIPPET_WINDOW * MAX_WINDOWS_PER_CHAPTER;
    let mut out: Vec<Snippet> = Vec::with_capacity(slots.len().min(8));
    let mut left = CONTEXT_BUDGET;
    let mut carry = 0usize; // 前面几章没用完的额度
                            // 由近及远分配：先当前章（slots 的最后一个），再从最近的前一章一路往回
    for k in (0..slots.len()).rev() {
        let own = if k + 1 == slots.len() {
            focus_share
        } else {
            chapter_cap
        };
        let grant = (own + carry).min(left);
        if grant < 200 {
            break; // 预算见底：更远的章只会更少
        }
        let ch = &chapters[slots[k]];
        let limit = SNIPPET_WINDOW.min(grant);
        let mut chops = best_windows(&ch.2, &kws, limit, MAX_WINDOWS_PER_CHAPTER);
        if chops.is_empty() {
            let head: String = ch.2.chars().take(limit).collect();
            if head.trim().is_empty() {
                carry = grant; // 空章：份额留给更远的章
                continue;
            }
            chops = vec![head];
        }
        let text = chops.join("\n…\n");
        let used = text.chars().count();
        if used > grant {
            carry = grant;
            continue;
        }
        left -= used;
        carry = grant - used;
        out.push(Snippet {
            index: ch.0,
            order: order_of(titles, &ch.1),
            title: ch.1.clone(),
            text,
        });
    }
    out.reverse(); // 输出按阅读顺序升序：最早的在前，当前章最后
    out
}

/// `current` 范围：取当前章及之前已缓存的章节，不做全书关键词排序。
///
/// 用户问"到现在为止发生了什么"时，要的是累积上文，不是全书最相关的几章（会把刚读过的
/// 前一章挤掉）；同时"全书检索"也不合适——按范围直接取前缀，关键词只用来挑窗口。
async fn retrieve_history(
    storage: &Storage,
    ns: &str,
    book_url: &str,
    question: &str,
    titles: &[String],
    current_title: &str,
) -> Retrieval {
    let chapters = match storage.list_cached_chapters(ns, book_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("AI 检索读取章节失败: {e}");
            return Retrieval {
                snippets: Vec::new(),
                cached_chapters: 0,
            };
        }
    };
    if chapters.is_empty() {
        return Retrieval {
            snippets: Vec::new(),
            cached_chapters: 0,
        };
    }
    let cached_chapters = chapters.len();
    let snippets = history_snippets(chapters, titles, current_title, question);
    Retrieval {
        snippets,
        cached_chapters,
    }
}

/// 组装对话消息。
///
/// 顺序刻意保持"稳定内容在前、易变内容在后"：规则 + 书籍 + 读者进度 → 历史 → 问题 + 片段。
/// 上游（DeepSeek 系网关）按前缀命中缓存，实测同前缀换问题只收 1/14 的钱；把片段塞进
/// system 最前面会让前缀每轮都变，命中率归零。
pub fn build_messages(
    book_name: &str,
    question: &str,
    snippets: &[Snippet],
    cached_chapters: usize,
    history: &[(String, String)],
    reading: Option<&str>,
    total_chapters: usize,
) -> Vec<Value> {
    let progress = reading.map(|r| format!("{r}\n")).unwrap_or_default();
    let system = if snippets.is_empty() && cached_chapters == 0 {
        format!(
            "你是小说阅读助手。当前书籍《{book_name}》还没有可用的已缓存章节正文。\n\
             {progress}请直接告诉用户：需要先阅读一些章节（或开启整书缓存）后我才能回答，不要编造任何情节。"
        )
    } else if snippets.is_empty() {
        format!(
            "你是小说阅读助手。当前书籍《{book_name}》已缓存 {cached_chapters} 章正文。\n\
             {progress}这次没有检索到与问题相关的片段。请告诉用户：换个更具体的说法（提到人名/物品名）或先读更多章节；不要编造情节。"
        )
    } else {
        format!(
            "你是小说阅读助手，帮读者回忆情节、人物何时出场、物品何时获得、人物关系。\n\
             书籍：《{book_name}》\n\
             {progress}\
             规则：\n\
             1) 只能依据下面给出的章节片段回答，不得编造片段之外的情节；\n\
             2) 每个结论后用 [章节标题] 标注依据，例如 [第三章 夜访]；\n\
             3) 片段里没有的信息，直接说“已缓存的章节里没提到”，不要推测；\n\
             4) 回答简洁：≤3 条要点、总长 ≤200 字；人物关系用 “A → B：关系” 的紧凑写法。"
        )
    };

    let mut msgs = vec![json!({ "role": "system", "content": system })];
    let start = history.len().saturating_sub(4);
    for (role, content) in &history[start..] {
        msgs.push(json!({ "role": role, "content": content }));
    }
    if snippets.is_empty() {
        msgs.push(json!({ "role": "user", "content": question }));
        return msgs;
    }
    // 片段跟着本轮问题走（易变内容放尾部，别破坏 system 的缓存前缀）
    let mut ctx = String::new();
    for s in snippets {
        let pos = if s.order > 0 && total_chapters > 0 {
            format!(" · {}/{}", s.order, total_chapters)
        } else {
            String::new()
        };
        ctx.push_str(&format!("\n【{}{pos}】\n{}\n", s.title, s.text));
    }
    msgs.push(json!({
        "role": "user",
        "content": format!("{question}\n\n【已缓存章节片段（按章节先后排列）】{ctx}"),
    }));
    msgs
}

/// 打印一次调用的 token 账本。
/// 网关（octopus）除了标准 `prompt_tokens/completion_tokens` 还会给
/// `prompt_cache_hit_tokens`（或 anthropic 口径的 `cache_read_input_tokens`）与 `credit`，
/// 这两个才是判断"前缀缓存有没有生效、这次到底多贵"的依据。
fn log_usage(cfg: &AiConfig, usage: &Value) {
    let get = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let prompt = get("prompt_tokens");
    let cached = get("prompt_cache_hit_tokens").max(get("cache_read_input_tokens"));
    let completion = get("completion_tokens");
    let thinking = get("completion_thinking_tokens");
    let credit = usage
        .get("credit")
        .and_then(|v| v.as_f64())
        .map(|c| format!("{c:.4}"))
        .unwrap_or_else(|| "-".into());
    let hit = if prompt > 0 {
        format!("{:.0}%", cached as f64 * 100.0 / prompt as f64)
    } else {
        "-".into()
    };
    tracing::info!(
        target: "ai_cost",
        model = %cfg.model,
        prompt_tokens = prompt,
        cached_tokens = cached,
        cache_hit_rate = %hit,
        completion_tokens = completion,
        thinking_tokens = thinking,
        credit = %credit,
        "AI 调用账本"
    );
}

/// 单轮流式请求：返回 (累积正文, finish_reason)；正文增量已通过 `send` 转发
async fn stream_round<F>(
    client: &reqwest::Client,
    cfg: &AiConfig,
    messages: &[Value],
    send: &F,
) -> anyhow::Result<(String, String)>
where
    F: Fn(Value) + Sync,
{
    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        // 让网关在流末尾带 usage，否则永远不知道这次花了多少 token、缓存命中多少
        "stream_options": { "include_usage": true },
    });
    // 思考默认关闭: 思考 token 既贵又会挤占 max_tokens 预算
    // (实测 max_tokens=1 时 content 为空、reasoning_content 有值, 直接触发 finish=length 续写)
    cfg.apply_thinking(&mut body);
    let budget = cfg.output_budget(cfg.max_tokens);
    if budget > 0 {
        body["max_tokens"] = json!(budget);
    }
    let resp = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "模型返回 {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut text = String::new();
    let mut thinking_chars = 0usize;
    let mut thinking_sent = 0usize;
    let mut finish_reason = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                return Ok((
                    text,
                    if finish_reason.is_empty() {
                        "stop".into()
                    } else {
                        finish_reason
                    },
                ));
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            // usage 出现在流的最后一个 chunk（choices 为空数组），务必在取 choices[0] 之前读
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                log_usage(cfg, u);
            }
            let delta = &v["choices"][0]["delta"];
            if let Some(r) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
                thinking_chars += r.chars().count();
                if thinking_chars - thinking_sent >= 200 {
                    thinking_sent = thinking_chars;
                    send(json!({ "type": "thinking", "chars": thinking_chars }));
                }
            }
            if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
                finish_reason = fr.to_string();
            }
            if let Some(c) = delta.get("content").and_then(|x| x.as_str()) {
                if !c.is_empty() {
                    text.push_str(c);
                    send(json!({ "type": "delta", "text": c }));
                }
            }
        }
    }
    Ok((
        text,
        if finish_reason.is_empty() {
            "stop".into()
        } else {
            finish_reason
        },
    ))
}

/// 调上游并把增量转发成 SSE 事件（meta 由调用方先发）。
/// 达到 max_tokens 被截断时自动续写（最多 2 轮，拼接到同一条回答）。
pub async fn stream_chat(
    cfg: AiConfig,
    messages: Vec<Value>,
    tx: UnboundedSender<Result<Bytes, Infallible>>,
) {
    let send = |payload: Value| {
        let _ = tx.send(Ok(Bytes::from(format!("data: {payload}\n\n"))));
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            send(json!({ "type": "error", "message": format!("初始化模型客户端失败: {e}") }));
            return;
        }
    };

    let mut convo: Vec<Value> = messages;
    let mut continued = 0usize;
    #[allow(unused_assignments)] // 初值仅为循环未执行时的兜底
    let mut finish = "stop".to_string();
    let mut saw_any = false;
    loop {
        match stream_round(&client, &cfg, &convo, &send).await {
            Ok((text, round_finish)) => {
                if !text.is_empty() {
                    saw_any = true;
                }
                finish = round_finish;
                if finish == "length" && cfg.max_tokens > 0 && continued < 2 {
                    continued += 1;
                    send(json!({
                        "type": "notice",
                        "message": format!("输出达到上限, 自动续写中({continued}/2)…"),
                    }));
                    convo.push(json!({ "role": "assistant", "content": text }));
                    convo.push(json!({
                        "role": "user",
                        "content": "继续从断点接着写, 不要重复已经写过的内容, 不要重新开头。",
                    }));
                    continue;
                }
                break;
            }
            Err(e) => {
                send(json!({ "type": "error", "message": format!("{e}") }));
                return;
            }
        }
    }

    if !saw_any {
        let message = if finish == "length" {
            format!(
                "模型在思考阶段用尽了输出预算(当前 max_tokens={})。\n请到「设置 → AI 阅读助手」调大最大输出 tokens(建议 4000+), 或把思考设为关闭, 或设为 0 表示不限制。",
                cfg.output_budget(cfg.max_tokens)
            )
        } else {
            "模型没有返回内容, 请重试, 或在设置里更换模型。".to_string()
        };
        send(json!({ "type": "error", "message": message }));
    } else {
        send(json!({ "type": "done", "finish": finish }));
    }
}

/// 拉取端点支持的模型 id 列表（`GET {base_url}/models`）。
///
/// 设置页用它做模型下拉，省掉"手敲模型名敲错一次就报 404"的来回。
/// 端点不支持该接口时返回空列表而不是报错——模型名仍然可以手填。
pub async fn list_models(cfg: &AiConfig) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let resp = client
        .get(format!("{}/models", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "端点返回 {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let value: Value = resp.json().await?;
    let mut out: Vec<String> = value
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    Ok(out)
}

/// 非流式对话（用于结构化输出：人物关系图 JSON）
pub async fn chat_once(
    cfg: &AiConfig,
    messages: Vec<Value>,
    max_tokens: u64,
) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": false,
        "max_tokens": cfg.output_budget(max_tokens),
        // 结构化输出（关系图 JSON）要的是结果本身：思考过程既烧 token，
        // 又容易把 max_tokens 预算吃光导致 content 为空（所以开了思考也要给足预算）
    });
    cfg.apply_thinking(&mut body);
    let resp = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "模型返回 {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let v: Value = resp.json().await?;
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        log_usage(cfg, u);
    }
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if content.trim().is_empty() {
        anyhow::bail!("模型没有返回内容（推理模型可能在思考阶段用尽了 token 预算）");
    }
    Ok(content)
}

/// 拼人物关系图的系统提示
pub fn build_graph_messages(book_name: &str, snippets: &[Snippet], question: &str) -> Vec<Value> {
    let mut ctx = String::new();
    for s in snippets {
        ctx.push_str(&format!("\n【{}】\n{}\n", s.title, s.text));
    }
    let system = format!(
        "你是小说人物关系抽取器。只依据给定章节片段抽取人物与关系，不得编造。\n\
         只输出 JSON（不要解释、不要 markdown 代码块），结构：\n\
         {{\"characters\":[{{\"name\":\"人物名\",\"alias\":\"别名/称号(可空)\",\"firstChapter\":\"片段中出现的章节标题\",\"role\":\"身份或一句话定位\"}}],\n\
         \"relations\":[{{\"from\":\"人物A\",\"to\":\"人物B\",\"label\":\"关系(≤8字)\",\"chapter\":\"依据章节标题\"}}]}}\n\
         约束：characters 取 5-15 个最重要的人物；relations 5-15 条，from/to 必须是 characters 里的 name；\n\
         firstChapter/chapter 必须逐字取自片段里的【标题】；片段没有的信息留空串，不要猜。\n\n\
         书籍：《{book_name}》\n\n【章节片段】{ctx}"
    );
    let user = if question.trim().is_empty() {
        "请抽取主要人物及其关系。".to_string()
    } else {
        format!("重点关注：{question}")
    };
    vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": user }),
    ]
}

/// 从模型输出中提取 JSON 对象（容忍 ```json 包裹 / 前后解释文字）
pub fn extract_json_object(raw: &str) -> Option<Value> {
    let text = raw.trim();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapters(n: usize) -> Vec<(i64, String, String)> {
        (0..n)
            .map(|i| {
                (
                    i as i64,
                    format!("第{}章", i + 1),
                    format!("第{}章的正文", i + 1),
                )
            })
            .collect()
    }

    /// 主角名是全书最高频的词，旧版 df 过滤会把它当泛词丢掉，问主角的事就查不到。
    /// 专名（词性 nr）必须豁免过滤，并且权重不低于 ENTITY_FLOOR。
    #[test]
    fn question_keywords_keeps_high_frequency_entity() {
        let mut chs = chapters(120);
        for (_, _, content) in chs.iter_mut() {
            content.push_str("赵玖看了看岳飞，没说话。");
        }
        // 两个名字都在 120 章里出现 → df*3 > total，旧逻辑直接丢弃
        let kws = question_keywords("赵玖和岳飞私下说了什么？", &chs);
        assert!(
            kws.iter().any(|(kw, _)| kw == "赵玖"),
            "主角名不该被 df 过滤丢掉: {kws:?}"
        );
        assert!(
            kws.iter().any(|(kw, _)| kw == "岳飞"),
            "主角名不该被 df 过滤丢掉: {kws:?}"
        );
        for (kw, w) in &kws {
            assert!(*w >= ENTITY_FLOOR, "{kw} 权重 {w} 低于专名下限");
        }
    }

    /// 非专名的高频词仍然要被过滤（否则检索被"得到/一章"这类泛词主导）。
    #[test]
    fn question_keywords_still_drops_common_non_entity() {
        let mut chs = chapters(120);
        for (_, _, content) in chs.iter_mut() {
            content.push_str("他得到了一章的书稿，又得到了一章的批注。");
        }
        let kws = question_keywords("他得到了一章的什么？", &chs);
        assert!(
            !kws.iter().any(|(kw, _)| kw == "一章"),
            "泛词不该进关键词: {kws:?}"
        );
    }

    fn cfg_with(thinking: &str, max_tokens: u64) -> AiConfig {
        AiConfig {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "test-model".to_string(),
            max_tokens,
            thinking: thinking.to_string(),
        }
    }

    #[test]
    fn thinking_defaults_to_off_and_sends_disabled() {
        let cfg = cfg_with("", 1200);
        assert_eq!(cfg.thinking_mode(), "off");
        assert_eq!(cfg.output_budget(1200), 1200, "关思考不额外加预算");
        let mut body = serde_json::json!({});
        cfg.apply_thinking(&mut body);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn thinking_levels_set_effort_and_raise_budget() {
        for level in ["low", "medium", "high"] {
            let cfg = cfg_with(level, 1200);
            assert_eq!(cfg.thinking_mode(), level);
            // 预算被思考吃光时 content 会是空的, 所以开思考必须抬到下限以上
            assert_eq!(cfg.output_budget(1200), THINKING_MIN_TOKENS);
            assert_eq!(cfg.output_budget(8000), 8000, "已经够大就不动");
            assert_eq!(cfg.output_budget(0), 0, "0 = 不限制, 保持不限制");
            let mut body = serde_json::json!({});
            cfg.apply_thinking(&mut body);
            assert_eq!(body["reasoning_effort"], level);
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn unknown_thinking_value_falls_back_to_off() {
        assert_eq!(cfg_with("LOW", 0).thinking_mode(), "low", "大小写不敏感");
        assert_eq!(cfg_with("turbo", 0).thinking_mode(), "off");
    }

    #[test]
    fn old_settings_json_without_thinking_still_loads() {
        // 老版本保存的 ai_settings 没有 thinking 字段, 升级后必须还能读
        let raw = r#"{"base_url":"http://x/v1","api_key":"k","model":"m","max_tokens":1200}"#;
        let cfg: AiConfig = serde_json::from_str(raw).expect("旧配置应能反序列化");
        assert_eq!(cfg.thinking_mode(), "off");
        assert_eq!(cfg.max_tokens, 1200);
    }

    #[test]
    fn scope_of_normalizes_and_defaults_to_current() {
        // 缺省/乱填都落到 current（界面上只有「当前章 / 全书」两档）
        assert_eq!(scope_of(""), "current");
        assert_eq!(scope_of("whatever"), "current");
        assert_eq!(scope_of(" Current "), "current");
        assert_eq!(scope_of("chapter"), "current");
        assert_eq!(scope_of("BOOK"), "book");
        assert_eq!(scope_of("all"), "book");
        // auto 保留给老客户端，界面不再暴露
        assert_eq!(scope_of("auto"), "auto");
    }

    /// 模拟书源书：`chapter_index` 是 `md5(chapterUrl)` 的大整数哈希，与阅读顺序完全无关，
    /// `list_cached_chapters` 的 `ORDER BY chapter_index` 回来就是下面这个哈希序。
    /// 正文里每句都带「xx标记」，用来断言片段来自哪一章。
    fn hash_ordered_book() -> (Vec<(i64, String, String)>, Vec<String>) {
        let titles: Vec<String> = [
            "第一章 起点",
            "第二章 风波",
            "第三章 夜访",
            "第四章 转折",
            "第五章 归途",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // 哈希序 = 第四章 / 第二章 / 第三章 / 第五章 / 第一章，与阅读顺序错开
        let hashes = [
            880_000_000_000_000_000i64,
            12_000_000_000_000_000,
            470_000_000_000_000_000,
            5_000_000_000_000_000,
            630_000_000_000_000_000,
        ];
        let marks = ["起点", "风波", "夜访", "转折", "归途"];
        let mut chapters: Vec<(i64, String, String)> = (0..titles.len())
            .map(|i| {
                let mut body = String::new();
                while body.chars().count() < 1400 {
                    body.push_str(&format!(
                        "{}标记。{}里赵玖看了看窗外，没有说话。",
                        marks[i], titles[i]
                    ));
                }
                (hashes[i], titles[i].clone(), body)
            })
            .collect();
        chapters.sort_by_key(|c| c.0); // 模拟 SQL 的 ORDER BY chapter_index
        (chapters, titles)
    }

    /// 假书：n 章，`chapter_index` 为哈希（与阅读顺序错开），正文每句带「第x章标记」。
    fn long_hash_book(n: usize) -> (Vec<(i64, String, String)>, Vec<String>) {
        let titles: Vec<String> = (1..=n).map(|i| format!("第{i}章 测试")).collect();
        let mut chapters: Vec<(i64, String, String)> = (0..n)
            .map(|i| {
                let mark = format!("第{}章标记", i + 1);
                let mut body = String::new();
                while body.chars().count() < 1400 {
                    body.push_str(&format!(
                        "{mark}。{}里赵玖看了看窗外，没有说话。",
                        titles[i]
                    ));
                }
                // 哈希序刻意与阅读顺序错开
                let hash = ((i * 7919 + 13) % 1000) as i64 * 1_000_000_000_000;
                (hash, titles[i].clone(), body)
            })
            .collect();
        chapters.sort_by_key(|c| c.0); // 模拟 SQL 的 ORDER BY chapter_index
        (chapters, titles)
    }

    fn titles_of(out: &[Snippet]) -> Vec<&str> {
        out.iter().map(|s| s.title.as_str()).collect()
    }

    /// T1 回归：书源书行序是哈希序，`current` 档取的是「当前章及之前」（TOC 前缀），
    /// 当前章排在最后，下一章不许进来（剧透）。
    #[test]
    fn current_scope_takes_history_up_to_now_by_toc_order() {
        let (chapters, titles) = hash_ordered_book();
        let out = history_snippets(
            chapters,
            &titles,
            &titles[2],
            "第二章里的风波到底是怎么回事",
        );
        assert_eq!(
            titles_of(&out),
            ["第一章 起点", "第二章 风波", "第三章 夜访"],
            "当前章 = 第三章 → 只取第一~三章，按阅读顺序升序"
        );
        assert_eq!(
            out.last().map(|s| s.title.as_str()),
            Some("第三章 夜访"),
            "当前章必须排在最后（最早的在前）"
        );
        assert_eq!(
            out.iter().map(|s| s.order).collect::<Vec<_>>(),
            [1, 2, 3],
            "order 走 TOC 下标（1 起）"
        );
        for (s, mark) in out.iter().zip(["起点标记", "风波标记", "夜访标记"]) {
            assert!(s.text.contains(mark), "{} 片段里应有 {mark}", s.title);
        }
        assert!(
            !out.iter()
                .any(|s| s.text.contains("转折标记") || s.text.contains("归途标记")),
            "第四章及之后（读者还没读到）绝不许进来: {:?}",
            titles_of(&out)
        );
    }

    /// T2 边界：当前章是第一章时只有它自己（没有更早的章可取）。
    #[test]
    fn current_scope_first_chapter_takes_only_itself() {
        let (chapters, titles) = hash_ordered_book();
        let out = history_snippets(chapters, &titles, &titles[0], "第一章 起点");
        assert_eq!(titles_of(&out), ["第一章 起点"]);
        assert!(!out[0].text.contains("风波标记"), "不许取后面的章");
    }

    /// T3 预算：装不下全部上文时，必须**由近及远**地取——绝不允许跳过最近的前一章却取了更早的章。
    #[test]
    fn current_scope_budget_prefers_nearer_history() {
        let (chapters, titles) = long_hash_book(14);
        // 无关键词 → 每章都退化成"开头 600 字"，最能暴露预算饥饿
        let out = history_snippets(chapters, &titles, &titles[13], "");
        let got = titles_of(&out);
        let total: usize = out.iter().map(|s| s.text.chars().count()).sum();
        assert!(total <= CONTEXT_BUDGET, "总预算不能超: {total}");
        assert!(out.len() >= 2, "至少要有当前章 + 前一章: {got:?}");
        assert_eq!(
            got.last().copied(),
            Some(titles[13].as_str()),
            "当前章在最后"
        );
        assert!(
            got.contains(&titles[12].as_str()),
            "最近的上一章(第十三章)必须在内，否则就是跳章: {got:?}"
        );
        // 取到的必须是紧贴当前章的一段连续上文：不许中间缺章
        let want: Vec<&str> = titles[14 - out.len()..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(got, want, "必须是「紧邻当前章的连续上文」，不能跳着取");
        assert!(
            !got.iter().any(|t| *t == titles[0].as_str()),
            "太早的章装不下就该被挤掉: {got:?}"
        );
    }

    /// T4 降级：TOC 里查不到当前章、甚至 `titles` 为空，都不能 panic，也不能返回空。
    #[test]
    fn current_scope_degrades_without_toc() {
        let (chapters, titles) = hash_ordered_book();
        // TOC 里没有这一章（老客户端没传 titles / 标题对不上）：退回章号/最后一章，取「它及之前」
        let fallback = history_snippets(chapters.clone(), &titles, "第九章 不存在", "随便问问");
        assert!(!fallback.is_empty(), "退回路径也得有内容");
        assert_eq!(
            fallback.last().map(|s| s.title.as_str()),
            Some("第五章 归途"),
            "定位不到当前章时以已排好的阅读序最后一章为准"
        );
        assert!(
            fallback.iter().all(|s| s.order <= 5),
            "退化路径同样只取前缀: {:?}",
            titles_of(&fallback)
        );
        // 完全没有 TOC、连当前章都空：没有阅读序可言（书源书的行序是哈希序），
        // 只能保证不 panic、不返回空
        let bare = history_snippets(chapters, &[], "", "");
        assert!(!bare.is_empty(), "titles 为空时也得有内容");
        assert!(
            bare.iter().all(|s| titles.contains(&s.title)),
            "片段必须来自这本书: {:?}",
            titles_of(&bare)
        );
    }
}
