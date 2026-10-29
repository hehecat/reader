//! 选章 + 拼提示词。移植自 `scripts/graph-demo/02_make_prompt.py`（权威口径）。
//!
//! 目标：把「全书已缓存章节」切成若干批，每批正文控制在 `BUDGET` 字以内，
//! 让模型一次只看到一个窗口的内容，多批合起来覆盖尽量多的章。
//!
//! 与脚本的对应关系：
//! * 窗口：全书切 `BATCHES` 个重叠窗口（`WIN_LEN` 章、步长约半个窗口），
//!   窗口内按「人名密度」排序取 `PER_BATCH` 章，已采过的章在后续窗口降权
//!   （[`CHOSEN_PENALTY`]），每窗首尾各强制留 1 章，正文按 `EXCERPT` 截断。
//! * 提示词：`INSTRUCTION`/`SYSTEM` 逐条照抄，只把写死的 `1..395` 换成实际章序上限。
//!
//! 简化（都不影响「批次/预算/章序」三条硬保证）：
//! * 脚本的种子人名来自现成图谱 `graph-demo-data.json` 的 name+aliases；后端 B 阶段
//!   还没有图谱，只能从正文里猜（见 [`seed_names`]，明知粗糙）。
//! * 章数自适应：脚本是给《绍宋》395 章调死的，这里按章数决定批数与窗口长度。
//! * 只做第一轮「重叠窗口批」（`kind = "window"`）。脚本的补充批（亲缘/冷门势力）
//!   与第二~五轮补抽（no-proto / camp / court / ally）没移植：它们拿现成图谱做排重，
//!   后端得等 normalize 产出人名表，留作后续。
//! * 预算超限时直接丢弃该批剩余章（脚本没这道闸，因为 40×2800 天然低于 BUDGET）。

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

// `BatchPlan` 定义在模块根 `graph.rs`；这里 re-export 一下，模块根的
// `pub use plan::{.., BatchPlan}` 才有东西可借（别名也方便本文件直接用）。
pub use super::BatchPlan;
use super::{Chapter, BATCHES, BUDGET, EXCERPT, PER_BATCH, WIN_LEN};

/// 已采过的章在后续窗口里的降权幅度下限（脚本写死 `dens[n] - 6.0`，单位是
/// 名字命中数 / 千字）。脚本有 100+ 个种子人名、密度尺度平缓，6.0 压得住；
/// 这里种子只有 5 个、密度起伏更陡，所以实际降权取
/// `max(CHOSEN_PENALTY, 全书最大密度)` —— 等价于「没采过的章一律优先」。
/// 用写死的 6.0 时高密度章会在每个重叠窗口里被反复抽中（实测重叠率 ~29%，
/// 贴线），按全书最大密度取值后降到 ~5%，覆盖章数更多。
const CHOSEN_PENALTY: f64 = 6.0;
/// 猜种子时最多取几个词
const MAX_SEEDS: usize = 5;
/// 猜种子时最多抽多少章、每章看多少字（全量统计会吃掉几十 MB 内存）
const SEED_SAMPLE_CHAPTERS: usize = 120;
const SEED_SAMPLE_CHARS: usize = 1500;
/// 候选词至少出现这么多次才算「人名式专名」
const SEED_MIN_HITS: u32 = 3;

/// 候选词里一旦出现这些字就整词丢弃——宁可错杀，别让「说道」「自己」「一个」当种子。
///
/// 局限：含常见字的人名（如「吕好问」的「问」）也会被误杀，只是少几个种子；
/// 密度排序本身对种子精度不敏感（少一个种子 ≈ 该名字在密度里不加分）。
const NOISE_CHARS: &str = "的了着过是在有和与就都也而及之为以于对从被把很更最又还但却只可才\
能会要个们一不上下中大小来去说道问听想知见觉笑看走站坐起得地他她它我你您这那哪什谁怎如\
此吗呢吧啊呀哦呗且若虽则并或没么";
/// 章号里可能出现的数字（含大写）
const NUMERALS: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '零', '〇', '一', '二', '三', '四', '五',
    '六', '七', '八', '九', '十', '百', '千', '两',
];

const SYSTEM_PROMPT: &str =
    "你是严谨的中文小说人物关系抽取器。你只输出 JSON，不输出任何解释、注释或 markdown 代码块。";

/// 体系提示词（= 02_make_prompt.py 的 `INSTRUCTION` 全文）。
/// `{book}` / `{max}` 由 [`user_instruction`] 替换，不走 `format!`（正文里有大量 `{`）。
const USER_INSTRUCTION: &str = r#"下面是长篇小说《{book}》的部分章节正文，每章以【第N章 标题】开头，N 是该章在全书的章序（1..{max}）。

请从这些正文中抽取人物关系图谱。严格遵守：

1. 只依据给定正文，不得使用你对该书或历史的先验知识补全；正文没写的关系不要写。
2. 同一人物的不同称呼（本名、字、号、官称、绰号、尊称）必须合并为一个人物，放进 aliases；
   每个主要人物的 aliases 尽量收全（本名/字/号/官称/绰号）。官称若被多人共用（如「太尉」「相公」）不要当别名。
3. 人物 45–70 个，只收有实际戏份或有明确关系的人物；纯路人不要。
   role 必须写**身份/官职/阵营地位**（如「宋廷皇帝」「金国主帅」「宋廷宰相」「岳飞部将」
   「伪齐皇帝」「东京留守」「内侍省大押班」），**禁止**写「主角」「配角」「路人」「武将」这类空话。
4. 关系 100–160 条。type 只能是：
   - parent（父母 → 子女）、spouse（夫妻）、sibling（兄弟姐妹，含堂/表/族兄弟）、
     master（师父 → 徒弟）、disciple（徒弟 → 师父，一般不用，写 master 反向即可）、
     ally（同盟/亲近/同党）、enemy（敌对/交战/政敌）、other（其他明确关系）。
   - parent 方向必须是「父/母 → 子」，master 方向是「师父 → 徒弟」。
   - spouse/sibling/ally/enemy 无方向，写一端即可。
   - **亲缘关系必须逐条穷举**：正文里出现的父子、母子、兄弟姐妹、叔侄、翁婿、姻亲、
     族亲都要抽成边，不要因为戏份少而省略；同一家族里凡是能确定的都写。
   - **敌对关系要写全**：战场对手、政敌、弹劾与被弹劾、背叛与猜忌都算 enemy。
5. 每条关系都要有：
   - chapter：该关系成立/被明确写出的那一章的 N（必须是上面正文里出现过的章号）；
   - quote：<=20 字的**原文片段**（逐字摘录，不要改写、不要加引号），作为出处。
   **没有原文出处的关系不要输出。**
6. 门派/家族/势力/组织/朝廷机构都进 factions，必须 6–10 个，kind 只能从
   「门派|家族|势力|组织」里选；leader 填该势力领袖的人物 name（能填就填，不认识填 null）。
   同一势力只写一条，不要重复（如「宋廷」「大宋」「宋朝」只保留「宋廷」）。
7. 每个人物的 faction 填其所属势力的 name（须出现在 factions 里），无所属填 null。
8. 每个人物至少出现在一条关系里，不要输出孤立人物。
9. 只输出一个 JSON 对象，不要 markdown、不要解释。字段用 name（不要用 id），下游会自己编号。
   不要输出 generation / firstSeen / lastSeen 这类派生字段，也不要按年龄辈分去猜——合并阶段会自己算。

输出结构（示例仅示意格式，内容以正文为准）：
{"characters":[{"name":"张三","aliases":["三哥","张三哥"],"gender":"男","role":"云岚宗掌门","faction":"云岚宗"}],
"factions":[{"name":"云岚宗","kind":"门派","leader":"张三"}],
"relations":[{"from":"张三","to":"李四","type":"master","chapter":3,"quote":"张三纳头便拜"}]}

正文如下：
"#;

/// 把章节切成若干批（纯函数，无 IO，方便单测）
///
/// 保证：`batch_no` 从 1 连续编号；每批 `chapters` 非空、按章序升序；所有
/// `chapter_index` 都来自入参；每章正文 ≤ `EXCERPT`，每批正文合计 ≤ `BUDGET`。
pub fn plan_batches(chapters: &[Chapter]) -> Vec<BatchPlan> {
    let mut sorted: Vec<&Chapter> = chapters.iter().collect();
    sorted.sort_by_key(|c| c.0);
    sorted.dedup_by_key(|c| c.0); // 同一章重复入参只算一次
    let total = sorted.len();
    if total == 0 {
        return Vec::new();
    }

    let idx: Vec<i64> = sorted.iter().map(|c| c.0).collect();
    let dens = name_density(&sorted, &seed_names(&sorted));

    let n = batch_count(total);
    // 短书别让 3 批都抓同一批章：每批上限收到「平均每批该覆盖的章数」
    let take = PER_BATCH.min(total.div_ceil(n).max(1));
    let win = window_span(total, n, take);
    // 降权幅度：见 CHOSEN_PENALTY 的注释（密度尺度不同，写死 6.0 压不住）
    let penalty = dens.iter().copied().fold(CHOSEN_PENALTY, f64::max);

    let mut chosen: HashSet<i64> = HashSet::new();
    let mut plans: Vec<BatchPlan> = Vec::with_capacity(n);
    for (a, b) in window_ranges(total, n, win) {
        let mut batch: Vec<Chapter> = Vec::new();
        let mut used = 0usize;
        for p in pick_window(&idx, &dens, a, b, take, penalty, &chosen) {
            let (chapter_index, title, body) = &sorted[p];
            let text = excerpt(body, EXCERPT.min(BUDGET));
            let len = text.chars().count();
            // 预算闸：装不下就到此为止（宁可少覆盖几章，也不让 prompt 超长）
            if !batch.is_empty() && used + len > BUDGET {
                break;
            }
            used += len;
            batch.push((*chapter_index, (*title).clone(), text));
        }
        if batch.is_empty() {
            continue;
        }
        chosen.extend(batch.iter().map(|c| c.0));
        plans.push(BatchPlan {
            batch_no: plans.len() + 1,
            kind: "window",
            chapters: batch,
        });
    }
    plans
}

/// 拼一批的 chat messages（system + user）
///
/// `total_chapters` 是全书真实总章数（`books.total_chapter_num` / 目录长度），提示词里的
/// `1..{max}` 说的就是它；传 0 时退回「本批最大章号」。
pub fn build_batch_messages(book_name: &str, plan: &BatchPlan, total_chapters: i64) -> Vec<Value> {
    let last = plan.chapters.iter().map(|c| c.0).max().unwrap_or(1);
    let max_chapter = total_chapters.max(last).max(1);
    let mut user = user_instruction(book_name, max_chapter);
    for (chapter_index, title, text) in &plan.chapters {
        let title = strip_chapter_prefix(title);
        if title.is_empty() {
            user.push_str(&format!("\n【第{chapter_index}章】\n{}", text.trim()));
        } else {
            user.push_str(&format!(
                "\n【第{chapter_index}章 {title}】\n{}",
                text.trim()
            ));
        }
    }
    vec![
        json!({ "role": "system", "content": SYSTEM_PROMPT }),
        json!({ "role": "user", "content": user }),
    ]
}

fn user_instruction(book_name: &str, max_chapter: i64) -> String {
    USER_INSTRUCTION
        .replace("{book}", book_name)
        .replace("{max}", &max_chapter.to_string())
}

// ---------------------------------------------------------------- 目录排序（Bug 1）

/// 目录的一条（`toc_cache.chapters_json` 的元素，只留排章序要用的两个字段）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntry {
    /// 目录标题（阅读序 = 它在目录数组里的位置）
    pub title: String,
    /// 书源书的章 URL 尾巴 `#<md5>`，正好等于 `book_chapters.chapter_index`
    pub hash: Option<i64>,
}

/// 解析 `toc_cache.chapters_json`。坏 JSON / 坏元素都跳过，绝不 panic（缓存是好几年攒的）。
pub fn parse_toc(json: &str) -> Vec<TocEntry> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim().to_string();
            if title.is_empty() {
                return None;
            }
            // `cache://http://…/455131.html#483218903185877268` → 483218903185877268
            let hash = item
                .get("url")
                .and_then(Value::as_str)
                .and_then(|url| url.rsplit_once('#'))
                .and_then(|(_, tail)| tail.trim().parse::<i64>().ok());
            Some(TocEntry { title, hash })
        })
        .collect()
}

/// 章标题里自带的章号（「第12章 xxx」→ 12）。抄 `service::ai::title_order`。
fn title_order(title: &str) -> Option<i64> {
    let digits: String = title
        .trim_start()
        .chars()
        .skip_while(|c| *c != '第')
        .skip(1)
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i64>().ok()
}

/// 把已缓存章节排成阅读序，并把「真实章序 1..N」写回 `Chapter.0`。
///
/// 书源书的 `chapter_index` 是 `md5(chapterUrl)`（见 `Storage::cache_chapter_content`），
/// 于是 `list_cached_chapters` 的 `ORDER BY chapter_index` 给的是**哈希序**（不是阅读序），
/// 数字还是 1e18 量级 —— 原样喂模型/记账就有「1151345960693571501 章」这种页面文案
/// 和【第1151345960693571501章】这种块头。这里按目录重排，再统一重编号 1..N。
///
/// 排序键（优先级从高到低）：
/// 1. 章 URL 哈希命中目录（`cache://…#<md5>`）→ 用目录下标（书源书最准）；
/// 2. 标题命中目录 → 用目录下标；
/// 3. 标题自带章号（「第12章」）→ 按章号；
/// 4. 都对不上 → 保持入参原序垫底（本地文件书的 `chapter_index` 本身就是阅读序）。
///
/// 无论目录有没有命中，输出都是 1..N 的连续整数：模型看到的章号、`meta.totalChapters`、
/// 关系的 `from_chapter` 从此共用一个「人话章序」。
pub fn number_chapters(chapters: &[Chapter], toc: &[TocEntry]) -> Vec<Chapter> {
    let by_hash: HashMap<i64, usize> = toc
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.hash.map(|h| (h, i)))
        .collect();
    let by_title: HashMap<&str, usize> = toc
        .iter()
        .enumerate()
        .map(|(i, e)| (e.title.as_str(), i))
        .collect();
    let mut keyed: Vec<((usize, usize), &Chapter)> = chapters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let key = if let Some(&p) = by_hash.get(&c.0) {
                (0, p)
            } else if let Some(&p) = by_title.get(c.1.trim()) {
                (1, p)
            } else if let Some(o) = title_order(&c.1) {
                (2, o.max(0) as usize)
            } else {
                (3, i)
            };
            (key, c)
        })
        .collect();
    // 稳定排序：键相同的章（同名/同章号）保持入参相对顺序
    keyed.sort_by_key(|item| item.0);
    keyed
        .into_iter()
        .enumerate()
        .map(|(k, (_, c))| ((k + 1) as i64, c.1.clone(), c.2.clone()))
        .collect()
}

/// 批数：脚本按《绍宋》395 章调死 6 批，这里按章数给（算出几个就是几个，不凑满）
fn batch_count(total: usize) -> usize {
    match total {
        0 => 0,
        1..=59 => 1,
        60..=200 => 3,
        _ => BATCHES,
    }
}

/// 窗口长度：步长 ≈ 窗口一半 → `win ≈ 1.75 * total / n`，上限 `WIN_LEN`（脚本值）。
/// 395 章 6 批时算出来正好 105，与脚本完全一致。
fn window_span(total: usize, n: usize, take: usize) -> usize {
    if n <= 1 {
        return total;
    }
    let ideal = (total as f64 * 1.75 / n as f64).round() as usize;
    let lo = (take + 1).min(total);
    let hi = WIN_LEN.min(total).max(lo);
    ideal.clamp(lo, hi)
}

/// 位置窗口（闭区间，0 起）：与脚本 `windows(1, hi, n, win)` 同构，只是把章号换成下标
fn window_ranges(total: usize, n: usize, win: usize) -> Vec<(usize, usize)> {
    if n <= 1 || win >= total {
        return vec![(0, total.saturating_sub(1))];
    }
    let step = (total - win) as f64 / (n - 1) as f64;
    (0..n)
        .map(|i| {
            let a = (step * i as f64).round() as usize;
            (a, (a + win - 1).min(total - 1))
        })
        .collect()
}

/// 窗口内选章：人名密度降序 + 已采过降权 + 同分靠窗口中心，然后每窗首尾各强制留 1 章
fn pick_window(
    idx: &[i64],
    dens: &[f64],
    a: usize,
    b: usize,
    take: usize,
    penalty: f64,
    chosen: &HashSet<i64>,
) -> Vec<usize> {
    let center = (a + b) as f64 / 2.0;
    let score = |p: usize| {
        let hit = if chosen.contains(&idx[p]) {
            penalty
        } else {
            0.0
        };
        dens[p] - hit
    };
    let dist = |p: usize| (p as f64 - center).abs();

    let mut pool: Vec<usize> = (a..=b).collect();
    pool.sort_by(|&x, &y| {
        score(y)
            .partial_cmp(&score(x))
            .unwrap_or(Ordering::Equal)
            .then_with(|| dist(x).partial_cmp(&dist(y)).unwrap_or(Ordering::Equal))
    });
    pool.truncate(take);

    // 每窗首尾各留一章，保证阶段连续（脚本 `picked[-1] = edge` 挤掉排最后那章）。
    // 与脚本不同的是两个边界各自独立挤人，不会互相覆盖。
    for edge in [a, b] {
        if pool.contains(&edge) {
            continue;
        }
        if pool.len() >= take {
            pool.pop();
        }
        pool.push(edge);
    }
    pool.sort_unstable();
    pool.dedup();
    pool
}

/// 每章正文截断到 `max` 字（按字符，不是字节），首尾空白去掉
fn excerpt(text: &str, max: usize) -> String {
    let cut: String = text.chars().take(max).collect();
    cut.trim().to_string()
}

/// 去掉标题自带的「第N章 」前缀（对齐脚本的
/// `re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\s*", "", title)`）
fn strip_chapter_prefix(title: &str) -> &str {
    let t = title.trim_start();
    let Some(pos) = t.find('章') else {
        return t;
    };
    if !t.starts_with('第') || pos > 18 {
        return t;
    }
    let mid = &t['第'.len_utf8()..pos];
    if mid.is_empty() || !mid.chars().all(|c| NUMERALS.contains(&c)) {
        return t;
    }
    t[pos + '章'.len_utf8()..].trim_start()
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

fn is_noise(c: char) -> bool {
    NOISE_CHARS.contains(c)
}

/// 连续汉字片段（拿 n-gram 用）
fn cjk_runs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if is_cjk(c) {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push(&text[s..i]);
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// 2~3 字候选词压成 u64（CJK 都在 BMP 低 16 位内，2 字高位补 0）
fn pack_key(seg: &[char]) -> u64 {
    let mut key = 0u64;
    for (i, &c) in seg.iter().enumerate() {
        key |= ((c as u64) & 0xFFFF) << (32 - 16 * i);
    }
    key
}

fn unpack_key(key: u64) -> String {
    let mut s = String::with_capacity(3);
    for shift in [32, 16, 0] {
        let v = ((key >> shift) & 0xFFFF) as u32;
        if v != 0 {
            s.push(char::from_u32(v).unwrap_or(' '));
        }
    }
    s
}

/// 猜种子人名：全书（抽样）里出现次数最多的 5 个「连续 2–3 个汉字且不含常见虚词」的词。
///
/// 这是脚本 `seeds()` 的土办法替代品（脚本直接读现成图谱的 name+aliases）。
/// 局限很多，明知粗糙：不切词、不懂词性，只靠「高频 + 不含虚词」硬筛，会把
/// 「金国」「宋廷」这类势力名、甚至「街上」这类高频名词当成种子。反过来它对密度
/// 排序够用——种子只是拿来给章节打「有多少人在说话」的分。
fn seed_names(chapters: &[&Chapter]) -> Vec<String> {
    if chapters.is_empty() {
        return Vec::new();
    }
    let step = (chapters.len() / SEED_SAMPLE_CHAPTERS).max(1);
    let mut freq: HashMap<u64, u32> = HashMap::new();
    for ch in chapters.iter().step_by(step) {
        let sample: String = ch.2.chars().take(SEED_SAMPLE_CHARS).collect();
        for run in cjk_runs(&sample) {
            let cs: Vec<char> = run.chars().collect();
            for (i, seg_start) in cs.iter().enumerate() {
                let mut seg: Vec<char> = vec![*seg_start];
                for &next in cs.iter().skip(i + 1).take(2) {
                    seg.push(next);
                    if seg.iter().any(|&c| is_noise(c)) {
                        continue;
                    }
                    *freq.entry(pack_key(&seg)).or_insert(0) += 1;
                }
            }
        }
    }

    let mut cands: Vec<(u64, u32)> = freq
        .into_iter()
        .filter(|(_, hits)| *hits >= SEED_MIN_HITS)
        .collect();
    // 次数降序；同次数用 key 兜底，保证同输入同输出（单测与线上一致）
    cands.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

    let mut seeds: Vec<String> = Vec::with_capacity(MAX_SEEDS);
    for (key, _) in cands {
        if seeds.len() >= MAX_SEEDS {
            break;
        }
        let word = unpack_key(key);
        // 同一个名字的各种切片只留一个（否则密度被同一个名字重复计分）
        if seeds
            .iter()
            .any(|s| s.contains(&word) || word.contains(s.as_str()))
        {
            continue;
        }
        seeds.push(word);
    }
    seeds
}

/// 人名密度：每千字命中种子人名的次数（脚本同名口径）
fn name_density(chapters: &[&Chapter], seeds: &[String]) -> Vec<f64> {
    chapters
        .iter()
        .map(|ch| {
            let text = &ch.2;
            let len = text.chars().count().max(1) as f64;
            let hits: usize = seeds.iter().map(|s| text.matches(s.as_str()).count()).sum();
            hits as f64 * 1000.0 / len
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一批假章节：章序 1..=n，正文 900~3000 字，人名密度按章序起伏
    fn book(n: usize) -> Vec<Chapter> {
        let names = ["赵玖", "岳飞", "韩世忠", "秦桧", "兀术"];
        (1..=n as i64)
            .map(|i| {
                let chars = 900 + (i as usize * 37) % 2100;
                let mentions = (i % 7) as usize * 2;
                let mut body = String::new();
                let mut k = 0usize;
                while body.chars().count() < chars {
                    body.push_str("他进了城，街上人来人往，市井喧闹，天色渐暗，远处传来鼓声。");
                    if mentions > 0 && k.is_multiple_of(8 - mentions.min(7)) {
                        let a = names[(i as usize + k) % names.len()];
                        let b = names[(i as usize + k + 1) % names.len()];
                        body.push_str(&format!("{a}对{b}道：「此事当如何？」"));
                    }
                    k += 1;
                }
                (i, format!("第{i}章 渭水之盟第{i}回"), body)
            })
            .collect()
    }

    fn all_indices(plans: &[BatchPlan]) -> Vec<i64> {
        plans
            .iter()
            .flat_map(|p| p.chapters.iter().map(|c| c.0))
            .collect()
    }

    /// ① 395 章 → 6 批，且各批章集基本不重复（重叠率 < 30%）
    #[test]
    fn plan_batches_covers_395_with_low_overlap() {
        let chapters = book(395);
        let plans = plan_batches(&chapters);
        let picked = all_indices(&plans);
        let distinct: HashSet<i64> = picked.iter().copied().collect();
        let overlap = 1.0 - distinct.len() as f64 / picked.len() as f64;
        eprintln!(
            "395 章 → {} 批，选章 {} 章（去重 {} 章），重叠率 {:.1}%",
            plans.len(),
            picked.len(),
            distinct.len(),
            overlap * 100.0
        );
        for plan in &plans {
            eprintln!(
                "  batch {}：{} 章 {:?}",
                plan.batch_no,
                plan.chapters.len(),
                plan.chapters.iter().map(|c| c.0).collect::<Vec<_>>()
            );
        }

        assert_eq!(plans.len(), 6, "395 章应出 6 批");
        assert!(overlap < 0.30, "重叠率 {:.1}% 超标", overlap * 100.0);
    }

    /// ② 50 章 → 只出 1 批
    #[test]
    fn plan_batches_single_batch_for_short_book() {
        let chapters = book(50);
        let plans = plan_batches(&chapters);
        eprintln!(
            "50 章 → {} 批，选章 {} 章",
            plans.len(),
            all_indices(&plans).len()
        );
        assert_eq!(plans.len(), 1, "50 章应只出 1 批");
    }

    /// ③ 每批正文合计 ≤ BUDGET、每章 ≤ EXCERPT；批号连续、章序升序、章号来自入参
    #[test]
    fn plan_batches_respects_budget_and_order() {
        for n in [1usize, 50, 61, 120, 201, 395] {
            let chapters = book(n);
            let input: HashSet<i64> = chapters.iter().map(|c| c.0).collect();
            let plans = plan_batches(&chapters);
            assert!(!plans.is_empty(), "{n} 章不该出 0 批");
            for (i, plan) in plans.iter().enumerate() {
                assert_eq!(plan.batch_no, i + 1, "批号必须从 1 连续");
                assert_eq!(plan.kind, "window");
                assert!(!plan.chapters.is_empty(), "第 {i} 批是空批");
                let total: usize = plan.chapters.iter().map(|c| c.2.chars().count()).sum();
                assert!(total <= BUDGET, "{n} 章第 {i} 批正文 {total} 字超预算");
                for ch in &plan.chapters {
                    assert!(
                        ch.2.chars().count() <= EXCERPT,
                        "{n} 章第 {i} 批第 {} 章摘录过长",
                        ch.0
                    );
                    assert!(input.contains(&ch.0), "章号 {} 不来自入参", ch.0);
                }
                let idx: Vec<i64> = plan.chapters.iter().map(|c| c.0).collect();
                let mut sorted = idx.clone();
                sorted.sort_unstable();
                assert_eq!(idx, sorted, "{n} 章第 {i} 批章序必须升序");
            }
        }
    }

    /// ④ user 内容包含全部章标题、且不含 markdown 代码块围栏
    #[test]
    fn build_batch_messages_contains_all_titles() {
        let chapters = book(120);
        let plans = plan_batches(&chapters);
        let plan = &plans[0];
        let msgs = build_batch_messages("绍宋", plan, 120);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");

        let user = msgs[1]["content"].as_str().unwrap_or_default();
        assert!(user.contains("《绍宋》"), "缺书名");
        assert!(user.contains("factions"), "缺 schema");
        assert!(!user.contains("```"), "不该出现 markdown 代码块围栏");
        let system = msgs[0]["content"].as_str().unwrap_or_default();
        assert!(system.contains("只输出 JSON"));

        for (chapter_index, title, _) in &plan.chapters {
            assert!(
                user.contains(&format!("【第{chapter_index}章")),
                "缺第 {chapter_index} 章的块头"
            );
            // 标题自带的「第N章 」前缀要被剥掉，但标题文字本身必须在
            assert!(
                user.contains(strip_chapter_prefix(title)),
                "缺标题：{title}"
            );
        }
    }

    // ---- 目录排序 + 真实章序（Bug 1） ----

    /// 生产库《玄鉴仙族》的真实形状：toc_cache 里是 `cache://…#<md5>`，
    /// `book_chapters.chapter_index` 就是那个 md5，而且行序是哈希序。
    #[test]
    fn number_chapters_orders_by_toc_hash() {
        // 哈希序（乱）= list_cached_chapters 给的顺序；真实阅读序是 1 2 3
        let chapters: Vec<Chapter> = vec![
            (831657723095650985, "第2章 李家".into(), "乙".into()),
            (1151345960693571501, "第3章 鉴子".into(), "丙".into()),
            (483218903185877268, "第1章 初入".into(), "甲".into()),
        ];
        let toc = parse_toc(
            r#"[{"title":"第1章 初入","url":"cache://http://x/455131.html#483218903185877268"},
                {"title":"第2章 李家","url":"cache://http://x/455131.html#831657723095650985"},
                {"title":"第3章 鉴子","url":"cache://http://x/455131.html#1151345960693571501"}]"#,
        );
        assert_eq!(toc.len(), 3);
        assert_eq!(toc[0].hash, Some(483218903185877268));

        let ordered = number_chapters(&chapters, &toc);
        let got: Vec<(i64, &str)> = ordered.iter().map(|c| (c.0, c.1.as_str())).collect();
        assert_eq!(
            got,
            vec![(1, "第1章 初入"), (2, "第2章 李家"), (3, "第3章 鉴子")],
            "必须按目录顺序，且章号重编号成 1..N"
        );
    }

    /// 没有 `#hash` 的目录（普通书源/本地书）靠标题匹配；目录里没有的章垫到最后。
    #[test]
    fn number_chapters_falls_back_to_toc_titles() {
        let chapters: Vec<Chapter> = vec![
            (77, "第三章 收网".into(), "c".into()),
            (11, "第一章 起风".into(), "a".into()),
            (99, "番外 年表".into(), "x".into()),
            (55, "第二章 落子".into(), "b".into()),
        ];
        let toc = parse_toc(
            r#"[{"title":"第一章 起风"},{"title":"第二章 落子"},{"title":"第三章 收网"}]"#,
        );
        assert_eq!(toc.len(), 3);
        assert!(toc.iter().all(|e| e.hash.is_none()));

        let ordered = number_chapters(&chapters, &toc);
        let titles: Vec<&str> = ordered.iter().map(|c| c.1.as_str()).collect();
        assert_eq!(
            titles,
            vec!["第一章 起风", "第二章 落子", "第三章 收网", "番外 年表"],
            "目录命中的按目录序，未命中的垫在最后"
        );
    }

    /// 目录整个缺失（没缓存过 toc）：也要退回「按入参原序重编号 1..N」，
    /// 绝不把入参里的哈希/0 起行号原样传给模型。
    #[test]
    fn number_chapters_without_toc_still_renumbers() {
        let chapters: Vec<Chapter> = (0..4)
            .map(|i| {
                (
                    i * 1_000_000_007,
                    format!("第{}章 标题", i + 1),
                    "正文".to_string(),
                )
            })
            .collect();
        let ordered = number_chapters(&chapters, &[]);
        let nums: Vec<i64> = ordered.iter().map(|c| c.0).collect();
        assert_eq!(nums, vec![1, 2, 3, 4]);
        assert_eq!(ordered[0].1, "第1章 标题");
    }

    /// 坏 JSON、缺 title、title 为空：解析出空目录，不 panic。
    #[test]
    fn parse_toc_tolerates_junk() {
        assert!(parse_toc("not json").is_empty());
        assert!(parse_toc("{}").is_empty());
        assert!(
            parse_toc(r#"[{"title":"  "},{"url":"cache://x#1"},{"title":"第一章"}]"#).len() == 1
        );
    }

    /// 提示词里的「1..{max}」用全书真实总章数，而不是本批最大哈希章号。
    #[test]
    fn build_batch_messages_uses_real_total_chapters() {
        let chapters = book(60);
        let plans = plan_batches(&chapters);
        let user = build_batch_messages("绍宋", &plans[0], 1745)[1]["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', " ");
        assert!(user.contains("1..1745"), "缺真实总章数：{user:.120}");
        for (chapter_index, _, _) in &plans[0].chapters {
            assert!(
                user.contains(&format!("【第{chapter_index}章")),
                "缺第 {chapter_index} 章的块头"
            );
        }
    }
}
