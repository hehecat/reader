//! 归一化合并。移植自 `scripts/graph-demo/03_normalize.py` 与 `04_verify.py`（权威口径）。
//!
//! 顺序即正确性：端点解析 → 同人合并 → quote 回正文校验 → 类型纠偏 → 去重 →
//! generation → 规模收敛 → 势力投票 → 输出 `graph-demo-v1`。
//!
//! 与离线脚本有意的差异（为了「换一本书也能用」）：丢掉《绍宋》专用的人名表
//! （`KIN_FLIP`/`KIN_RETYPE`/`KIN_LABEL`/`FORCE_MERGE`/`KEEP_CHARS`/`EXTRA_ALIASES`/
//! `SEED_FACTIONS`/`FACTION_MAP`/`ALIAS_DROP`），换成就地可判的通用规则：
//! - [`repair_kin_edge`]：quote 里的亲缘字眼（其父/生母/长子/女婿…）决定方向与类型，
//!   取代逐条手写的方向表；公开入口 [`repair_parent_direction`] 与它共用同一套规则，
//!   所以流水线里不必先落成 id 再回头修一遍。
//! - [`faction_kind`]：按势力名的词根猜「门派/家族/势力/组织」，取代硬编码的势力表。
//! - 势力成员/领袖靠「人物的 faction 字段 + role 里点名的势力 + 批次声明的 leader」投票，
//!   不再钉死某个小势力的领袖。

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};

use super::{
    Chapter, MAX_CHARS, MAX_DEGREE_RATIO, MAX_FACTIONS, MAX_RELATIONS, RELATION_TYPES, SCHEMA,
    TARGET_CHARS,
};

/// 「>= GAP 章未出场」视为角色线结束（`03_normalize.py` 的 `GAP`）
const GAP: i64 = 20;
/// 砍边时到此为止的关系数下限（`TARGET_RELS_MIN`）
const TARGET_RELS_MIN: usize = 240;
/// 度数上限的参照关系数（防止「砍边→上限跟着降」的死循环，`CAP_REF`）
const CAP_REF: usize = 265;
/// quote 落盘上限（`04_verify.py` 断言 quote ≤ 20 字）
const QUOTE_CAP: usize = 20;
/// 书名兜底
const BOOK_FALLBACK: &str = "未知作品";
/// 性别兜底
const GENDER_FALLBACK: &str = "未知";

/// 归一化用的书籍元信息：来自 `books` 表 / 目录缓存，**优先于**模型输出。
///
/// 模型的批次 JSON 里经常压根没有 `book` 字段（实测 6 批全无），只靠它取名就会一路
/// 回落到「未知作品」（Bug 2）。这类权威信息在库里现成可取，别问模型要。
#[derive(Debug, Clone, Default)]
pub struct BookHints {
    /// 权威书名（`books.name`；退 `book_info_cache`）；空 = 没取到
    pub book_name: String,
    /// 真实总章数（`books.total_chapter_num`，退目录长度）；0 = 没取到
    pub total_chapters: i64,
}

/// 泛称：不当作人物（模型常把「陛下/将军」当名字抽出来）
const GENERIC: &[&str] = &[
    "陛下",
    "官家",
    "大家",
    "相公",
    "太尉",
    "娘子",
    "殿下",
    "大王",
    "将军",
    "圣人",
    "足下",
    "老臣",
    "小底",
    "官人",
    "国主",
    "元帅",
    "皇嗣",
    "官家爷",
    "太皇",
    "太后",
    "皇后",
    "太子",
    "圣人爷",
    "押班",
    "大押班",
    "大臣",
    "群臣",
    "诸将",
    "主公",
    "诸位",
    "众人",
    "百姓",
];

/// 没有信息量的 role（宁可回落成「某势力属员」）
const BAD_ROLE: &[&str] = &[
    "",
    "主角",
    "配角",
    "反派",
    "路人",
    "龙套",
    "次要人物",
    "人物",
    "武将",
    "文臣",
    "未知",
    "未知人物",
    "配角（仅被提及）",
    "群像",
    "众人",
    "群演",
    "兵士",
    "士兵",
];

// ---------------------------------------------------------------- 线索正则（通用）

/// 「其父X」/「X的爹」：紧跟其后的名字是父/母端
///
/// 「的爹/之父」这族是白话叙述里最常见的写法（「刘光世的爹就是著名的刘延庆」），
/// 少了它们会把模型写反的亲子边原样留下，再连带把同代的兄弟边判成代差矛盾丢掉。
static SENIOR_CUE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"其父|乃父|生父|其母|生母|父亲|家父|爹爹|阿爹|老爹|之父|的爹|的父|的阿爹")
        .expect("regex")
});

/// 「X长子」：紧邻其前的名字是父/母端，后面紧跟的是子女端
static JUNIOR_CUE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"长子|次子|幼子|其子|之子|亲女|女儿|其女|独子").expect("regex"));

/// 出现即说明这条不是亲子边（姻亲/旁系）
static STEP_KIN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"女婿|岳父|岳丈|嫡母|继母|庶母|正宫|侄|外甥|族兄|堂兄|表兄|乳母").expect("regex")
});

/// parent 边必须有亲子线索（否则多半是模型把姻亲/兄弟标成了父子）
static PARENT_CUE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"之子|长子|次子|幼子|其子|儿子|皇子|公主|爹爹|老爹|爹|其父|生父|父亲|家父|亲女|女儿|岳父|女婿|所出|正宫|生母|乳名|亲弟|亲子",
    )
    .expect("regex")
});
static SPOUSE_ANY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"妻|夫人|娘子|娶|嫁|成亲|为后|贵妃|正宫|妃子|大婚|夫君").expect("regex")
});
static SIBLING_ANY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"兄长|弟弟|哥哥|兄弟|胞弟|亲弟|族兄|堂兄|义弟|结义|二弟|三弟|四弟|小弟|阿弟")
        .expect("regex")
});
static DISCIPLE_ANY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"弟子|徒弟|门生|拜师|受业|学于|从学|恩师|师父").expect("regex"));
static MASTER_CUE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"恩师|师父|师从|从学于|门下|授业|弟子|徒弟|门生|拜师|受业|学于|师事|传其衣钵")
        .expect("regex")
});
static ALLY_CUE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"心腹|亲信|同盟|结盟|联手|联合|相约|一党|同党|合作|共谋|倚重|亲厚|相善|交好|盟|归附|归顺|投效|效力|效死|听命|唯命|追随|拥戴|辅佐|同心|倚为|亲兵|部将|部下|麾下|生死之交|结义|义兄弟",
    )
    .expect("regex")
});
static OTHER_CUE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"主公|部将|部下|麾下|属下|心腹|亲信|幕僚|军师|举荐|推荐|引荐|收留|随侍|投奔|来降|投降|召见|召来|遣使|委派|拜见|拜为|拜入|授|封为|门生|学生|同学|同僚|同党|一党|翁婿|女婿|姻亲|结盟|相约|联手|联合|弹劾|参劾|请和|降|仇|怨|庇护|护送|教导|训练|督战|节制|统辖|隶属|归附|归顺|投效|从军|麾|请降|议和|使节|使者|受业|父子|之弟|之兄|之妻|之子|侄|甥|妻|婿|族|宗|师",
    )
    .expect("regex")
});

/// 领袖气质词（role 里出现说明这人是头）
static LEADER_WORD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"首领|总将|主帅|都统制|都统|皇帝|国主|国王|王爷|宰相|丞相|枢密使|留守|节度|大元帅|统帅|统领|族长|家主|掌门|教主|帮主|盟主|宗主|头领|当家|总管",
    )
    .expect("regex")
});
/// 一把手（比普通领袖词更高一档）
static LEADER_SOVEREIGN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"皇帝|国主|国王|太祖|圣上|大汗|可汗|君主").expect("regex"));
/// 副职：降半档，避免把副将推成领袖
static LEADER_DEPUTY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"前军|左军|右军|中军|后军|副都统|副统领|副帅|副使|副将|副").expect("regex")
});

// ---------------------------------------------------------------- 小工具

fn cjk_only(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
}

fn is_punct(c: char) -> bool {
    c.is_whitespace()
        || c.is_ascii_punctuation()
        || c.is_ascii_digit()
        || "，。、；：？！…—－·「」『』“”‘’（）《》〈〉【】０１２３４５６７８９".contains(c)
}

/// 去空白/标点，返回 (normalized, idx) —— idx[i] 是 normalized[i] 在原文里的下标
fn norm_text(text: &str) -> (Vec<char>, Vec<usize>) {
    let mut buf = Vec::with_capacity(text.len());
    let mut idx = Vec::with_capacity(text.len());
    for (i, ch) in text.chars().enumerate() {
        if is_punct(ch) {
            continue;
        }
        buf.push(ch);
        idx.push(i);
    }
    (buf, idx)
}

fn find_sub(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn find_all_sub(hay: &[char], needle: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || needle.len() > hay.len() {
        return out;
    }
    for i in 0..=hay.len() - needle.len() {
        if hay[i..i + needle.len()] == *needle {
            out.push(i);
        }
    }
    out
}

/// 正则命中位置都在字符边界上：取其后最多 max_chars 个字符
fn char_window(s: &str, from: usize, max_chars: usize) -> String {
    s.get(from..)
        .unwrap_or("")
        .chars()
        .take(max_chars)
        .collect()
}

/// 取 byte 偏移 to 之前最多 max_chars 个字符
fn char_window_before(s: &str, to: usize, max_chars: usize) -> String {
    let head = s.get(..to).unwrap_or("");
    let n = head.chars().count();
    head.chars().skip(n.saturating_sub(max_chars)).collect()
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn label_of(t: &str) -> &'static str {
    match t {
        "parent" => "亲缘",
        "spouse" => "夫妻",
        "sibling" => "手足",
        "master" | "disciple" => "师徒",
        "ally" => "同盟",
        "enemy" => "敌对",
        _ => "其他",
    }
}

fn is_symmetric(t: &str) -> bool {
    matches!(t, "spouse" | "sibling" | "ally" | "enemy")
}

/// 越小越该保留（`EDGE_RANK`）
fn edge_rank(t: &str) -> u8 {
    match t {
        "parent" | "spouse" | "sibling" | "master" | "disciple" => 0,
        "enemy" => 1,
        "ally" => 2,
        _ => 3,
    }
}

fn parse_type(raw: &str) -> &'static str {
    RELATION_TYPES
        .iter()
        .copied()
        .find(|t| *t == raw)
        .unwrap_or("other")
}

fn strip_quotes(s: &str) -> &str {
    s.trim()
        .trim_matches(|c| "\"'“”‘’「」『』".contains(c))
        .trim()
}

/// 丢弃计数（只进日志，不进 JSON）
#[derive(Default)]
struct Drops(BTreeMap<&'static str, usize>);

impl Drops {
    fn bump(&mut self, key: &'static str) {
        *self.0.entry(key).or_insert(0) += 1;
    }
    fn add(&mut self, key: &'static str, n: usize) {
        if n > 0 {
            *self.0.entry(key).or_insert(0) += n;
        }
    }
}

// ---------------------------------------------------------------- 正文索引

/// 全书索引：normalized 文本 + 归一化下标 → 原文下标
struct Book {
    ns: Vec<i64>,
    norm: HashMap<i64, Vec<char>>,
    idx: HashMap<i64, Vec<usize>>,
    raw: HashMap<i64, String>,
    raw_chars: HashMap<i64, Vec<char>>,
}

impl Book {
    fn new(chapters: &[Chapter]) -> Self {
        let mut book = Self {
            ns: Vec::new(),
            norm: HashMap::new(),
            idx: HashMap::new(),
            raw: HashMap::new(),
            raw_chars: HashMap::new(),
        };
        for (n, _title, content) in chapters {
            let (nrm, idx) = norm_text(content);
            book.norm.insert(*n, nrm);
            book.idx.insert(*n, idx);
            book.raw.insert(*n, content.clone());
            book.raw_chars.insert(*n, content.chars().collect());
        }
        book.ns = book.norm.keys().copied().collect();
        book.ns.sort_unstable();
        book
    }

    fn is_empty(&self) -> bool {
        self.ns.is_empty()
    }

    fn find(&self, needle: &[char]) -> Vec<i64> {
        self.ns
            .iter()
            .copied()
            .filter(|n| find_sub(&self.norm[n], needle).is_some())
            .collect()
    }

    /// 把 (章, normalized 起点, 长度) 还原成逐字原文，最多 QUOTE_CAP 字
    fn slice(&self, n: i64, pos: usize, len: usize) -> Option<String> {
        let idx = self.idx.get(&n)?;
        let len = len.min(idx.len().saturating_sub(pos));
        if len == 0 {
            return None;
        }
        let start = idx[pos];
        let end = idx[pos + len - 1] + 1;
        let raw = self.raw_chars.get(&n)?;
        let mut s: String = raw
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .take(QUOTE_CAP)
            .collect();
        if char_len(&s) >= QUOTE_CAP {
            s = s.trim_end().to_string();
        }
        if char_len(&s) >= 4 {
            Some(s)
        } else {
            None
        }
    }

    fn contains_anywhere(&self, text: &str) -> bool {
        self.raw.values().any(|raw| raw.contains(text))
    }

    /// 出现过任一词条的章
    fn chapters_with_any(&self, toks: &[String]) -> Vec<i64> {
        self.ns
            .iter()
            .copied()
            .filter(|n| {
                let raw = &self.raw[n];
                toks.iter().any(|t| raw.contains(t.as_str()))
            })
            .collect()
    }
}

// ---------------------------------------------------------------- 亲缘纠偏（通用规则）

/// 一条边需要怎么改
#[derive(Debug, Clone, PartialEq, Eq)]
struct KinRepair {
    /// 端点写反了：父/母 与 子/女 对调
    flip: bool,
    /// 不是亲子边：换成这个类型
    retype: Option<&'static str>,
    /// 降级时用 quote 里的那个称谓词当 label
    label: Option<String>,
}

/// 用 quote 自己的亲缘字眼判一次方向/类型（不看具体人名，所以换一本书也成立）。
///
/// - 「其父/乃父/生父/其母/生母/父亲/家父/爹爹/之父/的爹」后面紧跟的名字必须是父/母端；
/// - 「长子/次子/幼子/其子/之子/亲女/女儿/其女」前面紧邻的必须是父/母端、后面紧跟的必须是子女端；
/// - quote 出现「女婿/岳父/岳丈/嫡母/继母/庶母/正宫/侄/外甥/族兄/堂兄/表兄/乳母」→ 不是亲子边。
///
/// `pn`/`cn` 是这条边当前的（父/母端，子/女端）名字；只知道 id 时传空串，
/// 此时只做纯 quote 规则（第 3 条）。
fn repair_kin_edge(quote: &str, pn: &str, cn: &str, t: &str) -> Option<KinRepair> {
    if quote.is_empty() {
        return None;
    }
    // 1) 姻亲/旁系：quote 明说了这不是亲子边（先于方向判断——方向对不对都不该留 parent）
    if matches!(t, "parent" | "sibling" | "spouse") {
        if let Some(m) = STEP_KIN.find(quote) {
            return Some(KinRepair {
                flip: false,
                retype: Some("other"),
                label: Some(m.as_str().to_string()),
            });
        }
    }
    if t != "parent" || pn.is_empty() || cn.is_empty() {
        return None;
    }
    // 2) 「其父X」：X 必须是长辈，不能是子女位
    for m in SENIOR_CUE.find_iter(quote) {
        let after = char_window(quote, m.end(), 8);
        if after.contains(cn) && !after.contains(pn) {
            return Some(KinRepair {
                flip: true,
                retype: None,
                label: None,
            });
        }
    }
    // 3) 「X长子」：前面是长辈、后面紧跟的是子女
    for m in JUNIOR_CUE.find_iter(quote) {
        let before = char_window_before(quote, m.start(), 12);
        let after = char_window(quote, m.end(), 8);
        if before.contains(cn) && !before.contains(pn) && !after.contains(pn) {
            return Some(KinRepair {
                flip: true,
                retype: None,
                label: None,
            });
        }
        if after.contains(pn) && !after.contains(cn) {
            return Some(KinRepair {
                flip: true,
                retype: None,
                label: None,
            });
        }
    }
    None
}

/// 亲子方向修复：quote 里的「其父/生母/长子…」说了算；姻亲降级 other
///
/// `relations` 是落盘形状（`from`/`to`/`type`/`label`/`quote`）。端点带名字才判得了方向：
/// 给了 `from_name`/`to_name` 就用它，否则 `from`/`to` 本身不是 `c12` 这种 id 时按名字处理；
/// 只有 id 的关系仍会应用纯 quote 规则（姻亲降级）。
///
/// 传了 `chapters` 时先确认 quote 确实在正文里——出处都找不到的 quote 不足为凭，跳过。
pub fn repair_parent_direction(relations: &mut Vec<Value>, chapters: &[Chapter]) {
    let book = Book::new(chapters);
    for rel in relations.iter_mut() {
        let Some(obj) = rel.as_object_mut() else {
            continue;
        };
        let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("other");
        if !matches!(t, "parent" | "sibling" | "spouse") {
            continue;
        }
        let quote = obj
            .get("quote")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if quote.is_empty() {
            continue;
        }
        if !book.is_empty() && !book.contains_anywhere(&quote) {
            continue;
        }
        let pn = endpoint_name(obj, "from_name", "from");
        let cn = endpoint_name(obj, "to_name", "to");
        let Some(rep) = repair_kin_edge(&quote, &pn, &cn, parse_type(t)) else {
            continue;
        };
        if rep.flip {
            swap_fields(obj, "from", "to");
            swap_fields(obj, "from_name", "to_name");
        }
        if let Some(nt) = rep.retype {
            obj.insert("type".to_string(), Value::String(nt.to_string()));
        }
        if let Some(lb) = rep.label {
            obj.insert("label".to_string(), Value::String(lb));
        }
    }
    // 修完可能撞出重复（方向翻正后与已有的正确边重合），压一次
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    relations.retain(|rel| {
        let Some(obj) = rel.as_object() else {
            return true;
        };
        let t = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("other")
            .to_string();
        let a = obj
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let b = obj
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let (x, y) = if is_symmetric(&t) && a > b {
            (b, a)
        } else {
            (a, b)
        };
        seen.insert((x, y, t))
    });
}

fn endpoint_name(obj: &Map<String, Value>, named: &str, plain: &str) -> String {
    let pick = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let name = pick(named);
    if !name.is_empty() {
        return name;
    }
    let value = pick(plain);
    if is_char_id(&value) {
        String::new()
    } else {
        value
    }
}

fn is_char_id(s: &str) -> bool {
    match s.strip_prefix('c') {
        Some(rest) => !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

fn swap_fields(obj: &mut Map<String, Value>, ka: &str, kb: &str) {
    let a = obj.get(ka).cloned();
    let b = obj.get(kb).cloned();
    if let (Some(a), Some(b)) = (a, b) {
        obj.insert(ka.to_string(), b);
        obj.insert(kb.to_string(), a);
    }
}

// ---------------------------------------------------------------- 同人合并

#[derive(Clone, Debug)]
struct Rec {
    name: String,
    tokens: BTreeSet<String>,
    gender: String,
    role: String,
    faction: Option<String>,
}

#[derive(Clone, Debug)]
struct Cluster {
    name: String,
    aliases: Vec<String>,
    role: String,
    faction: Option<String>,
    gender: String,
    first_seen: i64,
    last_seen: i64,
}

impl Cluster {
    fn tokens(&self) -> Vec<String> {
        let mut toks = Vec::with_capacity(self.aliases.len() + 1);
        toks.push(self.name.clone());
        toks.extend(self.aliases.iter().cloned());
        toks
    }
}

/// 关系（内部形状：端点是 cluster 下标）
#[derive(Clone, Debug)]
struct Rel {
    a: usize,
    b: usize,
    t: &'static str,
    ch: i64,
    q: String,
    label: Option<String>,
}

fn clean_name(s: &str) -> String {
    let s = strip_quotes(s);
    if char_len(s) < 2 || char_len(s) > 6 || GENERIC.contains(&s) || !cjk_only(s) {
        return String::new();
    }
    s.to_string()
}

/// 「张荣部尤学究」这种拼接名 → (尤学究, 原名留作别名)
fn split_name(s: &str) -> (String, Option<String>) {
    static PART: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(.{2,4})部(.{2,4})$").expect("regex"));
    if let Some(c) = PART.captures(s) {
        let tail = c.get(2).map(|m| m.as_str()).unwrap_or("");
        if !tail.is_empty() && !GENERIC.contains(&tail) {
            return (tail.to_string(), Some(s.to_string()));
        }
    }
    if s.ends_with('部') && char_len(s) >= 3 {
        let head: String = s.chars().take(char_len(s) - 1).collect();
        return (head, Some(s.to_string()));
    }
    (s.to_string(), None)
}

/// 势力名归一：只做通用清洗（去引号/空白），具体书目的别名表留给人去配
fn normalize_faction_name(raw: &str) -> String {
    let s = strip_quotes(raw);
    if char_len(s) < 2 || char_len(s) > 12 {
        return String::new();
    }
    s.to_string()
}

fn collect_recs(batches: &[Value]) -> Vec<Rec> {
    let mut recs = Vec::new();
    for batch in batches {
        let Some(chars) = batch.get("characters").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in chars {
            let nm = clean_name(c.get("name").and_then(|v| v.as_str()).unwrap_or(""));
            if nm.is_empty() {
                continue;
            }
            let (nm, extra) = split_name(&nm);
            let mut tokens = BTreeSet::new();
            tokens.insert(nm.clone());
            if let Some(extra) = extra {
                tokens.insert(extra);
            }
            if let Some(aliases) = c.get("aliases").and_then(|v| v.as_array()) {
                for a in aliases {
                    let a = clean_name(a.as_str().unwrap_or(""));
                    if !a.is_empty() && a != nm {
                        tokens.insert(a);
                    }
                }
            }
            let faction = c
                .get("faction")
                .and_then(|v| v.as_str())
                .map(normalize_faction_name)
                .filter(|s| !s.is_empty());
            recs.push(Rec {
                name: nm,
                tokens,
                gender: c
                    .get("gender")
                    .and_then(|v| v.as_str())
                    .unwrap_or(GENDER_FALLBACK)
                    .trim()
                    .to_string(),
                role: c
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                faction,
            });
        }
    }
    recs
}

fn uf_root(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_root(parent, a);
    let rb = uf_root(parent, b);
    if ra != rb {
        parent[ra.max(rb)] = ra.min(rb);
    }
}

/// 跨批并查集合并：名字相等 / 一方名字是另一方的别名
fn merge_characters(batches: &[Value]) -> (Vec<Cluster>, BTreeSet<String>) {
    let recs = collect_recs(batches);
    let n = recs.len();
    let mut parent: Vec<usize> = (0..n).collect();

    let mut tok2idx: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, r) in recs.iter().enumerate() {
        for t in &r.tokens {
            tok2idx.entry(t.as_str()).or_default().push(i);
        }
    }
    for i in 0..n {
        // 只按「名字」合并，不按「别名互相重合」合并（别名表是模型给的，噪音大）
        for name in recs[i].tokens.iter() {
            let Some(js) = tok2idx.get(name.as_str()) else {
                continue;
            };
            for &j in js {
                if j <= i {
                    continue;
                }
                let other = &recs[j];
                if recs[i].name == other.name
                    || other.tokens.contains(&recs[i].name)
                    || recs[i].tokens.contains(&other.name)
                {
                    uf_union(&mut parent, i, j);
                }
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = uf_root(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    let mut clusters = Vec::with_capacity(groups.len());
    for idxs in groups.values() {
        // 正名：出现次数最多、其次最长、最后按字典序（保证确定性）
        let mut name_cnt: BTreeMap<&str, usize> = BTreeMap::new();
        for &i in idxs {
            *name_cnt.entry(recs[i].name.as_str()).or_insert(0) += 1;
        }
        let canonical = name_cnt
            .iter()
            .max_by(|a, b| {
                a.1.cmp(b.1)
                    .then(char_len(a.0).cmp(&char_len(b.0)))
                    .then(b.0.cmp(a.0))
            })
            .map(|(k, _)| (*k).to_string())
            .unwrap_or_default();

        let mut tokens: BTreeSet<String> = BTreeSet::new();
        for &i in idxs {
            tokens.extend(recs[i].tokens.iter().cloned());
        }
        tokens.remove(&canonical);

        let mut role_cnt: BTreeMap<&str, usize> = BTreeMap::new();
        for &i in idxs {
            let role = recs[i].role.as_str();
            if role.is_empty() || BAD_ROLE.contains(&role) {
                continue;
            }
            *role_cnt.entry(role).or_insert(0) += 1;
        }
        let role = role_cnt
            .iter()
            .max_by(|a, b| {
                a.1.cmp(b.1)
                    .then(char_len(a.0).cmp(&char_len(b.0)))
                    .then(b.0.cmp(a.0))
            })
            .map(|(k, _)| (*k).to_string())
            .unwrap_or_default();

        let mut fac_cnt: BTreeMap<&str, usize> = BTreeMap::new();
        for &i in idxs {
            if let Some(f) = recs[i].faction.as_deref() {
                *fac_cnt.entry(f).or_insert(0) += 1;
            }
        }
        let faction = fac_cnt
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(k, _)| (*k).to_string());

        let mut gender_cnt: BTreeMap<&str, usize> = BTreeMap::new();
        for &i in idxs {
            *gender_cnt.entry(recs[i].gender.as_str()).or_insert(0) += 1;
        }
        let gender = gender_cnt
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(k, _)| (*k).to_string())
            .unwrap_or_else(|| GENDER_FALLBACK.to_string());

        clusters.push(Cluster {
            name: canonical,
            aliases: tokens.into_iter().collect(),
            role,
            faction,
            gender,
            first_seen: 0,
            last_seen: 0,
        });
    }

    // 别名卫生：别名正是别人的正名 → 模型串了人，剔除
    let all_names: BTreeSet<String> = clusters.iter().map(|c| c.name.clone()).collect();
    for c in clusters.iter_mut() {
        c.aliases.retain(|a| !all_names.contains(a));
    }
    // 同一个别名被两个 cluster 同时认领 → 两边都不认（只留名字）
    let mut owner: HashMap<String, BTreeSet<usize>> = HashMap::new();
    for (k, c) in clusters.iter().enumerate() {
        owner.entry(c.name.clone()).or_default().insert(k);
        for a in &c.aliases {
            owner.entry(a.clone()).or_default().insert(k);
        }
    }
    let ambiguous: BTreeSet<String> = owner
        .iter()
        .filter(|(_, s)| s.len() > 1)
        .map(|(t, _)| t.clone())
        .collect();
    for c in clusters.iter_mut() {
        let name = c.name.clone();
        c.aliases
            .retain(|a| !ambiguous.contains(a) && *a != name && !all_names.contains(a));
    }
    (clusters, ambiguous)
}

/// 名字 → cluster 下标（多主的名字不进表）
struct Resolver<'a> {
    tok2c: HashMap<&'a str, usize>,
    long_toks: Vec<&'a str>,
}

impl<'a> Resolver<'a> {
    fn new(clusters: &'a [Cluster]) -> Self {
        let mut owner: HashMap<&str, BTreeSet<usize>> = HashMap::new();
        for (k, c) in clusters.iter().enumerate() {
            owner.entry(c.name.as_str()).or_default().insert(k);
            for a in &c.aliases {
                owner.entry(a.as_str()).or_default().insert(k);
            }
        }
        let tok2c: HashMap<&str, usize> = owner
            .into_iter()
            .filter(|(_, s)| s.len() == 1)
            .filter_map(|(t, s)| s.iter().next().map(|&k| (t, k)))
            .collect();
        let mut long_toks: Vec<&str> = tok2c.keys().copied().filter(|t| char_len(t) >= 3).collect();
        long_toks.sort_by(|a, b| char_len(b).cmp(&char_len(a)).then(a.cmp(b)));
        Self { tok2c, long_toks }
    }

    fn resolve(&self, name: &str) -> Option<usize> {
        let nm = strip_quotes(name);
        if let Some(&k) = self.tok2c.get(nm) {
            return Some(k);
        }
        // 「岳飞长子岳云」这类长名退化到已知人名
        for t in &self.long_toks {
            if nm.contains(t) {
                return self.tok2c.get(t).copied();
            }
        }
        let cleaned = clean_name(nm);
        if cleaned.is_empty() {
            return None;
        }
        self.tok2c.get(cleaned.as_str()).copied()
    }
}

// ---------------------------------------------------------------- 出处校验

/// 在正文里核验 quote；返回 (真实章号, 逐字原文) 或 None
///
/// 顺序：先用模型给的章号 → 全书找整句 → 按标点切段找 >=5 字的段。
/// 命中多个章时优先「两个端点名字都出现」的章，其次最早。
fn verify_quote(
    book: &Book,
    quote: &str,
    claimed: Option<i64>,
    endpoints: &[String],
) -> Option<(i64, String)> {
    let (nq, _) = norm_text(quote);
    if nq.len() < 4 {
        return None;
    }

    let pick = |hits: Vec<i64>, needle: &[char]| -> Option<(i64, String)> {
        let mut best: Option<(i64, usize)> = None; // (章, 命中端点个数)
        for n in hits {
            let raw = &book.raw[&n];
            let hit = endpoints
                .iter()
                .filter(|e| !e.is_empty() && raw.contains(e.as_str()))
                .count();
            let better = match best {
                None => true,
                Some((bn, bhit)) => hit > bhit || (hit == bhit && n < bn),
            };
            if better {
                best = Some((n, hit));
            }
        }
        let (n, _) = best?;
        let pos = find_sub(&book.norm[&n], needle)?;
        let q = book.slice(n, pos, needle.len())?;
        Some((n, q))
    };

    if let Some(claimed) = claimed {
        if let Some(norm) = book.norm.get(&claimed) {
            if let Some(pos) = find_sub(norm, &nq) {
                if let Some(q) = book.slice(claimed, pos, nq.len()) {
                    return Some((claimed, q));
                }
            }
        }
    }
    if let Some(got) = pick(book.find(&nq), &nq) {
        return Some(got);
    }
    // 整句找不到 → 按标点切段，从最长的段试起
    let mut segs: Vec<Vec<char>> = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    for ch in quote.chars().chain(std::iter::once('，')) {
        if ('\u{4E00}'..='\u{9FFF}').contains(&ch) {
            cur.push(ch);
        } else if !cur.is_empty() {
            segs.push(std::mem::take(&mut cur));
        }
    }
    segs.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    for seg in segs {
        if seg.len() < 5 {
            continue;
        }
        if let Some(got) = pick(book.find(&seg), &seg) {
            return Some(got);
        }
    }
    None
}

// ---------------------------------------------------------------- 关系：端点解析 + 出处校验

fn build_relations(
    batches: &[Value],
    clusters: &[Cluster],
    book: &Book,
    drops: &mut Drops,
) -> Vec<Rel> {
    let resolver = Resolver::new(clusters);
    let mut out = Vec::new();
    for batch in batches {
        let Some(rels) = batch.get("relations").and_then(|v| v.as_array()) else {
            continue;
        };
        for r in rels {
            let (Some(mut a), Some(mut b)) = (
                resolver.resolve(r.get("from").and_then(|v| v.as_str()).unwrap_or("")),
                resolver.resolve(r.get("to").and_then(|v| v.as_str()).unwrap_or("")),
            ) else {
                drops.bump("端点未知");
                continue;
            };
            let mut t = parse_type(r.get("type").and_then(|v| v.as_str()).unwrap_or("").trim());
            let quote =
                strip_quotes(r.get("quote").and_then(|v| v.as_str()).unwrap_or("")).to_string();
            let mut label: Option<String> = None;
            if let Some(rep) = repair_kin_edge(&quote, &clusters[a].name, &clusters[b].name, t) {
                if rep.flip {
                    std::mem::swap(&mut a, &mut b);
                }
                if let Some(nt) = rep.retype {
                    t = nt;
                    drops.bump("亲缘类型纠偏");
                }
                label = rep.label;
            }
            if a == b {
                drops.bump("自环");
                continue;
            }
            let mut endpoints = clusters[a].tokens();
            endpoints.extend(clusters[b].tokens());
            let claimed = r.get("chapter").and_then(|v| v.as_i64());
            let Some((ch, qv)) = verify_quote(book, &quote, claimed, &endpoints) else {
                drops.bump("出处核验失败");
                continue;
            };
            out.push(Rel {
                a,
                b,
                t,
                ch,
                q: qv,
                label,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- 亲缘出处修复

fn kin_cue(t: &str) -> Option<&'static Lazy<Regex>> {
    match t {
        "parent" => Some(&PARENT_CUE),
        "master" => Some(&MASTER_CUE),
        "spouse" => Some(&SPOUSE_ANY),
        "sibling" => Some(&SIBLING_ANY),
        "disciple" => Some(&DISCIPLE_ANY),
        _ => None,
    }
}

/// quote 里没有亲缘线索时，回正文找一句同时套住两端的话（找不到就丢掉这条边）
fn repair_kin_quotes(
    rels: Vec<Rel>,
    clusters: &[Cluster],
    book: &Book,
    drops: &mut Drops,
) -> Vec<Rel> {
    if book.is_empty() {
        return rels;
    }
    let where_of: Vec<Vec<i64>> = clusters
        .iter()
        .map(|c| {
            let toks: Vec<String> = c
                .tokens()
                .into_iter()
                .filter(|t| char_len(t) >= 2)
                .collect();
            book.chapters_with_any(&toks)
        })
        .collect();
    let mut fixed = 0usize;
    let mut kept = Vec::with_capacity(rels.len());
    for mut r in rels {
        let Some(cue) = kin_cue(r.t) else {
            kept.push(r);
            continue;
        };
        if cue.is_match(&r.q) {
            kept.push(r);
            continue;
        }
        match find_kin_quote(clusters, book, &where_of, r.a, r.b, cue) {
            Some((ch, q)) => {
                r.ch = ch;
                r.q = q;
                fixed += 1;
                kept.push(r);
            }
            None => drops.bump("亲缘-无出处"),
        }
    }
    drops.add("亲缘出处修复", fixed);
    kept
}

fn find_kin_quote(
    clusters: &[Cluster],
    book: &Book,
    where_of: &[Vec<i64>],
    a: usize,
    b: usize,
    cue: &Regex,
) -> Option<(i64, String)> {
    let set: BTreeSet<i64> = where_of.get(a)?.iter().copied().collect();
    let both: Vec<i64> = where_of
        .get(b)?
        .iter()
        .copied()
        .filter(|n| set.contains(n))
        .collect();
    let toks_a: Vec<Vec<char>> = clusters[a]
        .tokens()
        .into_iter()
        .filter(|t| char_len(t) >= 2)
        .map(|t| t.chars().collect())
        .collect();
    let toks_b: Vec<Vec<char>> = clusters[b]
        .tokens()
        .into_iter()
        .filter(|t| char_len(t) >= 2)
        .map(|t| t.chars().collect())
        .collect();
    for n in both {
        let Some(raw) = book.raw_chars.get(&n) else {
            continue;
        };
        let raw_str = &book.raw[&n];
        let mut pa: Vec<usize> = Vec::new();
        for t in &toks_a {
            pa.extend(find_all_sub(raw, t));
        }
        let mut pb: Vec<usize> = Vec::new();
        for t in &toks_b {
            pb.extend(find_all_sub(raw, t));
        }
        pa.sort_unstable();
        pb.sort_unstable();
        for &x in &pa {
            for &y in &pb {
                let lo = x.min(y);
                let hi = (x.max(y) + 2).min(raw.len());
                if hi <= lo || hi - lo > 20 {
                    continue;
                }
                let snippet: String = raw[lo..hi].iter().collect();
                if snippet.contains('\n') || snippet.contains('\r') {
                    continue; // 跨段落拼出来的不是原句
                }
                let wide_lo = byte_of(raw_str, lo.saturating_sub(12));
                let wide_hi = byte_of(raw_str, (hi + 12).min(raw.len()));
                let wide = raw_str.get(wide_lo..wide_hi).unwrap_or("");
                if cue.is_match(&snippet) || cue.is_match(wide) {
                    let trimmed = snippet
                        .trim_matches(|c: char| "\"'“”‘’「」，。、；： ".contains(c))
                        .to_string();
                    return Some((n, trimmed));
                }
            }
        }
    }
    None
}

/// char 下标 → byte 偏移
fn byte_of(raw: &str, char_pos: usize) -> usize {
    raw.char_indices()
        .nth(char_pos)
        .map(|(b, _)| b)
        .unwrap_or(raw.len())
}

// ---------------------------------------------------------------- 去重 / 精度过滤

fn rel_key(r: &Rel) -> (usize, usize, &'static str) {
    if is_symmetric(r.t) {
        (r.a.min(r.b), r.a.max(r.b), r.t)
    } else {
        (r.a, r.b, r.t)
    }
}

/// 同键里哪条更值得留：quote 更长，其次章号更早
fn quote_better(old: &Rel, new: &Rel) -> bool {
    (char_len(&new.q), Reverse(new.ch)) > (char_len(&old.q), Reverse(old.ch))
}

fn dedupe(rels: Vec<Rel>, drops: &mut Drops) -> Vec<Rel> {
    let before = rels.len();
    let mut best: BTreeMap<(usize, usize, &'static str), Rel> = BTreeMap::new();
    for r in rels {
        let key = rel_key(&r);
        match best.get(&key) {
            Some(old) if !quote_better(old, &r) => {}
            _ => {
                best.insert(key, r);
            }
        }
    }
    // 有向边双向冲突（parent/master 同时存在 A->B 与 B->A）：留 quote 更长的一条
    let mut seen: HashSet<(usize, usize, &'static str)> = HashSet::new();
    let mut out = Vec::with_capacity(best.len());
    for (key, r) in &best {
        if seen.contains(key) {
            continue;
        }
        let mut keep = r.clone();
        if matches!(keep.t, "parent" | "master") {
            let rev = (keep.b, keep.a, keep.t);
            if let Some(other) = best.get(&rev) {
                if char_len(&other.q) > char_len(&keep.q) {
                    keep = other.clone();
                }
                seen.insert(rev);
            }
        }
        out.push(keep);
        seen.insert(*key);
    }
    drops.add("去重", before - out.len());
    out
}

/// master 常被模型用来标「隶属/心腹/上下级」：没有师承字样就降级成 other
fn fix_master(rels: Vec<Rel>, drops: &mut Drops) -> Vec<Rel> {
    let mut out = Vec::with_capacity(rels.len());
    for mut r in rels {
        if r.t == "master" && !MASTER_CUE.is_match(&r.q) {
            r.t = "other";
            drops.bump("master-非师承降级");
        }
        out.push(r);
    }
    // 降级后可能与已有 other 边撞车
    dedupe(out, drops)
}

/// parent 边必须有亲子线索：看不出亲子关系的（多半是女婿/兄弟被标成父子）丢掉
fn keep_parent_cue(rels: Vec<Rel>, drops: &mut Drops) -> Vec<Rel> {
    rels.into_iter()
        .filter(|r| {
            let ok = r.t != "parent" || PARENT_CUE.is_match(&r.q);
            if !ok {
                drops.bump("parent-无亲子线索");
            }
            ok
        })
        .collect()
}

/// ally 边精度：quote 里必须能看出「同盟」线索（或者至少在 quote 里点名了两端）
fn keep_ally_cue(rels: Vec<Rel>, clusters: &[Cluster], drops: &mut Drops) -> Vec<Rel> {
    rels.into_iter()
        .filter(|r| {
            if r.t != "ally" || ALLY_CUE.is_match(&r.q) {
                return true;
            }
            if both_named(r, clusters) {
                return true;
            }
            drops.bump("ally-无同盟线索");
            false
        })
        .collect()
}

fn both_named(r: &Rel, clusters: &[Cluster]) -> bool {
    [r.a, r.b].iter().all(|&i| {
        clusters[i]
            .tokens()
            .iter()
            .any(|t| !t.is_empty() && r.q.contains(t.as_str()))
    })
}

/// 同一个人不能既是盟友又是敌人：这种对子两边都不可信，整对丢掉
fn drop_ally_enemy_conflict(rels: Vec<Rel>, drops: &mut Drops) -> Vec<Rel> {
    let mut types: HashMap<(usize, usize), BTreeSet<&'static str>> = HashMap::new();
    for r in &rels {
        types
            .entry((r.a.min(r.b), r.a.max(r.b)))
            .or_default()
            .insert(r.t);
    }
    let bad: HashSet<(usize, usize)> = types
        .into_iter()
        .filter(|(_, ts)| ts.contains("ally") && ts.contains("enemy"))
        .map(|(k, _)| k)
        .collect();
    rels.into_iter()
        .filter(|r| {
            let ok = !bad.contains(&(r.a.min(r.b), r.a.max(r.b)));
            if !ok {
                drops.bump("盟友/敌对冲突");
            }
            ok
        })
        .collect()
}

/// other 边精度：quote 里没有任何社会关系线索的纯共现边丢掉
fn keep_other_cue(rels: Vec<Rel>, drops: &mut Drops) -> Vec<Rel> {
    rels.into_iter()
        .filter(|r| {
            let ok = r.t != "other" || OTHER_CUE.is_match(&r.q);
            if !ok {
                drops.bump("other-无关系线索");
            }
            ok
        })
        .collect()
}

// ---------------------------------------------------------------- generation

fn gen_find(gp: &mut [usize], pot: &mut [i64], x: usize) -> usize {
    if gp[x] == x {
        return x;
    }
    let p = gp[x];
    let root = gen_find(gp, pot, p);
    pot[x] += pot[p];
    gp[x] = root;
    root
}

/// level(b) - level(a) = d
fn gen_union(gp: &mut [usize], pot: &mut [i64], a: usize, b: usize, d: i64) -> bool {
    let ra = gen_find(gp, pot, a);
    let rb = gen_find(gp, pot, b);
    if ra == rb {
        return pot[b] - pot[a] == d;
    }
    if ra < rb {
        gp[rb] = ra;
        pot[rb] = pot[a] + d - pot[b];
    } else {
        gp[ra] = rb;
        pot[ra] = pot[b] - d - pot[a];
    }
    true
}

/// 加权并查集：parent 差 1 最高优先，spouse/sibling 同代，冲突边丢弃，
/// 每个亲缘连通块从 0 起跳。返回 (应丢弃的边, 每人的 generation, 是否卷入矛盾)
fn assign_generation(
    rels: &[Rel],
    n: usize,
    drops: &mut Drops,
) -> (Vec<bool>, Vec<i64>, Vec<bool>) {
    let mut gp: Vec<usize> = (0..n).collect();
    let mut pot: Vec<i64> = vec![0; n];
    let mut worn = vec![false; rels.len()];
    let mut conflict = vec![false; n];
    for (i, r) in rels.iter().enumerate() {
        if r.t != "parent" {
            continue;
        }
        if !gen_union(&mut gp, &mut pot, r.a, r.b, 1) {
            worn[i] = true;
            conflict[r.a] = true;
            conflict[r.b] = true;
            drops.bump("亲缘同代矛盾");
        }
    }
    for (i, r) in rels.iter().enumerate() {
        if !matches!(r.t, "spouse" | "sibling") {
            continue;
        }
        if !gen_union(&mut gp, &mut pot, r.a, r.b, 0) {
            worn[i] = true;
            conflict[r.a] = true;
            conflict[r.b] = true;
            drops.bump("亲缘同代矛盾");
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = gen_find(&mut gp, &mut pot, i);
        groups.entry(root).or_default().push(i);
    }
    let mut level = vec![0i64; n];
    for members in groups.values() {
        let base = members.iter().map(|&i| pot[i]).min().unwrap_or(0);
        for &i in members {
            level[i] = pot[i] - base;
        }
    }
    (worn, level, conflict)
}

// ---------------------------------------------------------------- 规模收敛

fn degrees(rels: &[Rel], n: usize) -> Vec<usize> {
    let mut d = vec![0usize; n];
    for r in rels {
        d[r.a] += 1;
        d[r.b] += 1;
    }
    d
}

/// 砍边时的优先级，越大越先丢：非亲缘 > 两端都热门 > quote 短 > 章号晚
///
/// 「两端都热门」放这条规则是为了保住叶子：只剩一条边的人物，那条边永远排到最后才砍，
/// 否则砍边会把一批配角直接砍成孤点，连带把关系图砍空。
fn edge_weight(r: &Rel, deg: &[usize]) -> (u8, usize, isize, i64) {
    (
        edge_rank(r.t),
        deg[r.a].min(deg[r.b]),
        -(char_len(&r.q) as isize),
        r.ch,
    )
}

/// 找最该丢的一条（`only` 限定只在该节点的邻边里挑）；亲缘/师承边一律跳过
fn weakest(rels: &[Rel], deg: &[usize], only: Option<usize>) -> Option<usize> {
    rels.iter()
        .enumerate()
        .filter(|(_, r)| edge_rank(r.t) != 0)
        .filter(|(_, r)| only.is_none_or(|i| r.a == i || r.b == i))
        .map(|(k, r)| (edge_weight(r, deg), k))
        .max()
        .map(|(_, k)| k)
}

/// 把图裁到 demo 可读的规模：留关键人物、压单点度数、限关系总数
///
/// 返回 (保住的旧下标, 重排后的关系)；关系端点已重映射到新的下标空间。
fn prune_for_demo(
    clusters: &[Cluster],
    rels: &[Rel],
    must_keep: &HashSet<usize>,
    drops: &mut Drops,
) -> (Vec<usize>, Vec<Rel>) {
    let n = clusters.len();
    let mut deg = vec![0usize; n];
    let mut kin = vec![0usize; n];
    let mut enemy = vec![0usize; n];
    for r in rels {
        deg[r.a] += 1;
        deg[r.b] += 1;
        match r.t {
            "enemy" => {
                enemy[r.a] += 1;
                enemy[r.b] += 1;
            }
            _ if edge_rank(r.t) == 0 => {
                kin[r.a] += 1;
                kin[r.b] += 1;
            }
            _ => {}
        }
    }
    let span: Vec<f64> = clusters
        .iter()
        .map(|c| ((c.last_seen - c.first_seen).max(1)) as f64)
        .collect();
    // 打分：有亲缘 >> 有敌对 >> 关系多 >> 出场跨度长
    let score: Vec<f64> = (0..n)
        .map(|i| 3.0 * kin[i] as f64 + 1.5 * enemy[i] as f64 + deg[i] as f64 + 0.03 * span[i])
        .collect();

    let mut cand: Vec<usize> = (0..n).filter(|&i| deg[i] > 0).collect();
    cand.sort_by(|&x, &y| {
        score[y]
            .partial_cmp(&score[x])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(clusters[x].first_seen.cmp(&clusters[y].first_seen))
            .then(x.cmp(&y))
    });
    let mut keep: Vec<usize> = cand.iter().copied().take(TARGET_CHARS).collect();

    // 亲缘/师承边上的人一律保住（「家族树」视图要有内容），再从低分非亲缘人物里挤名额
    let mut kin_chars: HashSet<usize> = (0..n).filter(|&i| kin[i] > 0).collect();
    kin_chars.extend(must_keep.iter().copied());
    let mut extra: Vec<usize> = kin_chars
        .iter()
        .copied()
        .filter(|i| !keep.contains(i) && deg[*i] > 0)
        .collect();
    extra.sort_by(|&x, &y| {
        score[y]
            .partial_cmp(&score[x])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x.cmp(&y))
    });
    keep.extend(extra);
    if keep.len() > MAX_CHARS {
        let mut spare: Vec<usize> = keep
            .iter()
            .copied()
            .filter(|i| !kin_chars.contains(i) && !must_keep.contains(i))
            .collect();
        spare.sort_by(|&x, &y| {
            score[x]
                .partial_cmp(&score[y])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(Reverse(clusters[x].first_seen).cmp(&Reverse(clusters[y].first_seen)))
        });
        for victim in spare {
            if keep.len() <= MAX_CHARS {
                break;
            }
            keep.retain(|&i| i != victim);
        }
    }
    keep.sort_unstable();
    drops.add("超规模截断人物", cand.len().saturating_sub(keep.len()));

    let kept: HashSet<usize> = keep.iter().copied().collect();
    let mut pruned: Vec<Rel> = rels
        .iter()
        .filter(|r| kept.contains(&r.a) && kept.contains(&r.b))
        .cloned()
        .collect();
    drops.add("截断人物带掉的关系", rels.len() - pruned.len());

    // 单点度数上限：超限节点各砍掉自己最弱的一条
    let cap = (MAX_DEGREE_RATIO * CAP_REF as f64) as usize;
    while pruned.len() > TARGET_RELS_MIN {
        let d = degrees(&pruned, n);
        let over: Vec<usize> = (0..n).filter(|&i| d[i] > cap).collect();
        if over.is_empty() {
            break;
        }
        let mut drop_at: BTreeSet<usize> = BTreeSet::new();
        for i in over {
            if let Some(k) = weakest(&pruned, &d, Some(i)) {
                drop_at.insert(k);
            }
        }
        if drop_at.is_empty() {
            break;
        }
        drops.add("度数上限削减", drop_at.len());
        let mut idx = 0usize;
        pruned.retain(|_| {
            let alive = !drop_at.contains(&idx);
            idx += 1;
            alive
        });
    }

    // 关系总数封顶：每次砍当前最弱的一条，亲缘边不参与
    while pruned.len() > MAX_RELATIONS {
        let d = degrees(&pruned, n);
        let Some(k) = weakest(&pruned, &d, None) else {
            break;
        };
        pruned.remove(k);
        drops.bump("关系总数削减");
    }

    // 孤点剔除 + 重映射
    let d = degrees(&pruned, n);
    let kept2: Vec<usize> = keep.iter().copied().filter(|&i| d[i] > 0).collect();
    drops.add("孤立点", keep.len() - kept2.len());
    let mut remap = vec![usize::MAX; n];
    for (new, &old) in kept2.iter().enumerate() {
        remap[old] = new;
    }
    let rels2: Vec<Rel> = pruned
        .into_iter()
        .filter_map(|mut r| {
            let a = *remap.get(r.a)?;
            let b = *remap.get(r.b)?;
            if a == usize::MAX || b == usize::MAX {
                return None;
            }
            r.a = a;
            r.b = b;
            Some(r)
        })
        .collect();
    (kept2, rels2)
}

// ---------------------------------------------------------------- 势力

/// 按名字里的词根猜势力类型（`04_verify.py` 只认 门派|家族|势力|组织）
fn faction_kind(name: &str) -> &'static str {
    const SECT: &[&str] = &[
        "派", "门", "宗", "寺", "观", "教", "帮", "阁", "山庄", "书院",
    ];
    const POWER: &[&str] = &["国", "朝", "廷", "军", "队", "营", "司", "帅", "署", "衙"];
    const CLAN: &[&str] = &["家", "氏", "族", "府", "宗祠"];
    if SECT.iter().any(|k| name.contains(k)) {
        return "门派";
    }
    if POWER.iter().any(|k| name.contains(k)) {
        return "势力";
    }
    if CLAN.iter().any(|k| name.contains(k)) {
        return "家族";
    }
    "组织"
}

/// 一个势力
#[derive(Clone, Debug)]
struct Faction {
    /// 保留下来（信息量最大）的名字
    name: String,
    /// 被归并掉的其他写法（「黎泾李家」「李家」…）：排障用，也进输出便于人工核对
    aliases: Vec<String>,
    /// 成员下标（裁剪后的 cluster 下标，按度数降序）
    members: Vec<usize>,
    /// 领袖下标
    leader: Option<usize>,
    /// 门派 | 家族 | 势力 | 组织（归并时取出现最多的那个）
    kind: &'static str,
}

/// 人物 → 所属势力下标（可能同时属于多个）
type MemberOf = Vec<Vec<usize>>;

/// 家族势力归并的成员重合度阈值（Jaccard）
const FACTION_MERGE_JACCARD: f64 = 0.3;
/// 家族后缀字：`X家` / `X氏` / `X族` / `X府`
const CLAN_SUFFIX: &[char] = &['家', '氏', '族', '府'];

/// 势力投票：人物的 faction 字段 + role 里点名的势力名 + 批次声明的 leader
///
/// 返回**未归并、未截断**的势力表（成员按度数降序）与「势力名 → 领袖票」；
/// 归并 / 收敛 / 选领袖见 [`settle_factions`]。
fn vote_factions(
    batches: &[Value],
    clusters: &[Cluster],
    resolver: &Resolver<'_>,
    degree: &[usize],
) -> (Vec<Faction>, BTreeMap<String, BTreeMap<usize, usize>>) {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for c in clusters {
        if let Some(f) = &c.faction {
            names.insert(f.clone());
        }
    }
    for batch in batches {
        let Some(factions) = batch.get("factions").and_then(|v| v.as_array()) else {
            continue;
        };
        for f in factions {
            let name = normalize_faction_name(f.get("name").and_then(|v| v.as_str()).unwrap_or(""));
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }

    let mut members: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for name in &names {
        members.entry(name.clone()).or_default();
    }
    for (i, c) in clusters.iter().enumerate() {
        if let Some(f) = &c.faction {
            members.entry(f.clone()).or_default().insert(i);
        }
        // role 里点名的役属势力也算成员（「赤心队首领」→ 赤心队）
        if c.role.is_empty() {
            continue;
        }
        let hit = names
            .iter()
            .filter(|f| char_len(f) >= 2 && c.role.contains(f.as_str()))
            .max_by(|a, b| char_len(a).cmp(&char_len(b)).then(b.cmp(a)));
        if let Some(f) = hit {
            members.entry(f.clone()).or_default().insert(i);
        }
    }

    let mut declared: BTreeMap<String, BTreeMap<usize, usize>> = BTreeMap::new();
    for batch in batches {
        let Some(factions) = batch.get("factions").and_then(|v| v.as_array()) else {
            continue;
        };
        for f in factions {
            let name = normalize_faction_name(f.get("name").and_then(|v| v.as_str()).unwrap_or(""));
            let leader = strip_quotes(f.get("leader").and_then(|v| v.as_str()).unwrap_or(""));
            if name.is_empty() || leader.is_empty() {
                continue;
            }
            if let Some(k) = resolver.resolve(leader) {
                *declared
                    .entry(name.clone())
                    .or_default()
                    .entry(k)
                    .or_insert(0) += 1;
                members.entry(name).or_default().insert(k); // 声明的领袖本身就是成员
            }
        }
    }

    let mut out: Vec<Faction> = members
        .into_iter()
        .filter(|(_, m)| !m.is_empty())
        .map(|(name, m)| {
            let mut mem: Vec<usize> = m.into_iter().collect();
            mem.sort_by_key(|&i| (Reverse(degree[i]), i));
            let leader = pick_leader(&name, &mem, clusters, declared.get(&name), degree);
            Faction {
                kind: faction_kind(&name),
                name,
                aliases: Vec::new(),
                members: mem,
                leader,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(a.name.cmp(&b.name))
    });
    (out, declared)
}

/// 势力归并 + 规模收敛 + 重算领袖 + 「人物 → 势力下标」反查表
///
/// 顺序有讲究：**先归并再截断**。反过来的话，先被 `MAX_FACTIONS` 砍掉的正好可能是
/// 「黎泾李家」这种小变体，归并就无从谈起了（Bug 3 的现场就是这样）。
fn settle_factions(
    factions: Vec<Faction>,
    clusters: &[Cluster],
    declared: &BTreeMap<String, BTreeMap<usize, usize>>,
    degree: &[usize],
) -> (Vec<Faction>, MemberOf, BTreeMap<String, String>) {
    let (mut factions, rename) = merge_factions(factions, clusters, degree);
    factions.truncate(MAX_FACTIONS);
    let mut mem_of: Vec<Vec<usize>> = vec![Vec::new(); clusters.len()];
    for (fi, f) in factions.iter_mut().enumerate() {
        // 归并后的领袖票 = 本名 + 全部别名的票之和
        let mut votes: BTreeMap<usize, usize> = BTreeMap::new();
        for name in std::iter::once(&f.name).chain(f.aliases.iter()) {
            if let Some(v) = declared.get(name) {
                for (k, c) in v {
                    *votes.entry(*k).or_insert(0) += *c;
                }
            }
        }
        for &i in &f.members {
            mem_of[i].push(fi);
        }
        let (name, members) = (f.name.clone(), f.members.clone());
        let votes = (!votes.is_empty()).then_some(&votes);
        f.leader = pick_leader(&name, &members, clusters, votes, degree);
    }
    (factions, mem_of, rename)
}

/// 家族势力的「姓根」候选字。
///
/// `李氏` → {李}；`黎泾李家` → {黎, 李}（地名前缀 + 姓）；`李通崖家` → {崖, 李}
/// （按人名派生的碎片写法）。取**候选集合**而不是单个字，是为了「黎泾李家」能和「李氏」
/// 对上，而「陈家」不会因为首字相同就和「陈氏」以外的东西混一起。
fn clan_roots(name: &str) -> Vec<char> {
    let cs: Vec<char> = name.chars().collect();
    let mut out: Vec<char> = Vec::new();
    // 去掉尾部的家族后缀（`家族` 两字叠用要都剥）后，末字通常就是姓：`黎泾李` → 李
    let mut end = cs.len();
    while end > 0 && CLAN_SUFFIX.contains(&cs[end - 1]) {
        end -= 1;
    }
    if end > 0 && end < cs.len() {
        out.push(cs[end - 1]);
    }
    // 两个字的名字没有前缀可剥（`李氏` / `李家` / `郁家`）：首字就是姓。
    // 反过来，三字以上的名字首字多是地名（`黎泾李家` 的黎、`黎夏萧家` 的黎），
    // 把它当姓根会把「黎泾李家」和「黎夏萧家」串成一串，最后把萧家并进李家。
    if out.is_empty() && cs.len() == 2 {
        out.push(cs[0]);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// 成员名的共同「姓」：首字里覆盖 ≥60%（且至少 2 人）的那个字。
///
/// 家族势力的成员几乎同姓，所以哪怕势力名写成了「李通崖家」（按人名派生），也能靠成员
/// 对上「李氏」。覆盖率的闸门是为了别把「袁家」和「费家」这类小势力误并。
fn member_surname(f: &Faction, clusters: &[Cluster]) -> Option<char> {
    if f.members.len() < 2 {
        return None;
    }
    let mut counts: BTreeMap<char, usize> = BTreeMap::new();
    for &i in &f.members {
        if let Some(c) = clusters[i].name.chars().next() {
            *counts.entry(c).or_insert(0) += 1;
        }
    }
    let best = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;
    (best.1 >= 2 && best.1 * 5 >= f.members.len() * 3).then_some(best.0)
}

/// 名字是不是「纯姓根」写法（`X家` / `X氏` / `X族`）——归并时优先保留这种名字
fn clan_form(name: &str) -> usize {
    usize::from(name.ends_with(['家', '氏', '族']))
}

fn jaccard(a: &[usize], b: &[usize]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let sa: BTreeSet<usize> = a.iter().copied().collect();
    let sb: BTreeSet<usize> = b.iter().copied().collect();
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 家族势力归并（Bug 3）：同一家族在批次之间会以「李氏 / 黎泾李家 / 李家 / 李通崖家」
/// 等变体各成一条势力，成员集合被切碎，图上看就是几块互不相连的家族。
///
/// 合并依据（命中任一即并；只并 `kind == "家族"` 的——门派/势力/组织的同名变体少得多，
/// 乱并的代价更大）：
/// a. 姓根相交（`X氏` / `X家` / `地名+X家` 都归到同一个姓，见 [`clan_roots`]）；
/// b. `leader` 是同一个人（这个证据最硬：两批都指认同一个族长）；
/// c. 成员重合度（Jaccard）≥ [`FACTION_MERGE_JACCARD`]。
///
/// 保留最像正式家族名的那个（纯姓根名 > 成员多 > 更短），其余进 `aliases`；成员取并集，
/// `kind` 取归并前出现最多的那个。返回 (归并后的势力, 旧名 → 新名)。
fn merge_factions(
    factions: Vec<Faction>,
    clusters: &[Cluster],
    degree: &[usize],
) -> (Vec<Faction>, BTreeMap<String, String>) {
    let n = factions.len();
    if n <= 1 {
        let rename = factions
            .iter()
            .map(|f| (f.name.clone(), f.name.clone()))
            .collect();
        return (factions, rename);
    }
    let roots: Vec<Vec<char>> = factions
        .iter()
        .map(|f| {
            let mut r = clan_roots(&f.name);
            if let Some(s) = member_surname(f, clusters) {
                r.push(s);
            }
            r.sort_unstable();
            r.dedup();
            r
        })
        .collect();

    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            let same_root = roots[i].iter().any(|c| roots[j].contains(c));
            let no_root = roots[i].is_empty() || roots[j].is_empty();
            let same_leader =
                factions[i].leader.is_some() && factions[i].leader == factions[j].leader;
            let clan_pair = factions[i].kind == "家族" && factions[j].kind == "家族";
            let overlaps =
                jaccard(&factions[i].members, &factions[j].members) >= FACTION_MERGE_JACCARD;
            // 姓根不同的一律不并：生产库里「李氏」和「萧氏」会被同一个高连通人物同时投成
            // 领袖，光看 leader 会把萧家整块吞进李家（实测 f1 里混进了萧氏/萧家/黎夏萧家）。
            let merge = (same_root && clan_pair)
                // 姓根一致 + 有一方看不出姓根（「陇西堂」这种堂号）时，leader 相同就能并
                || (same_root && same_leader)
                || (same_leader && no_root)
                // 实在看不出姓根，但成员高度重合：仍是同一家（门派/势力不参与这一条）
                || (overlaps && no_root && clan_pair);
            if merge {
                uf_union(&mut parent, i, j);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = uf_root(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    let mut out: Vec<Faction> = Vec::with_capacity(groups.len());
    let mut rename: BTreeMap<String, String> = BTreeMap::new();
    for (_, mut idxs) in groups {
        // 保留哪个名字：① 「李/萧/郁」这种纯姓根名（X家/X氏/X族）压过带地名前缀或堂号的
        // 变体（「黎泾李家」「陇西堂」），② 成员多的更权威，③ 同分取更短的，④ 字典序兜底
        idxs.sort_by(|&a, &b| {
            let (fa, fb) = (&factions[a], &factions[b]);
            clan_form(&fb.name)
                .cmp(&clan_form(&fa.name))
                .then(fb.members.len().cmp(&fa.members.len()))
                .then(char_len(&fa.name).cmp(&char_len(&fb.name)))
                .then(fa.name.cmp(&fb.name))
        });
        let keep = idxs[0];
        let mut members: BTreeSet<usize> = BTreeSet::new();
        let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut aliases: Vec<String> = Vec::new();
        for &k in &idxs {
            members.extend(factions[k].members.iter().copied());
            *kinds.entry(factions[k].kind).or_insert(0) += 1;
            if k != keep {
                aliases.push(factions[k].name.clone());
            }
            rename.insert(factions[k].name.clone(), factions[keep].name.clone());
        }
        let kind = kinds
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
            .map_or("组织", |(k, _)| k);
        let mut mem: Vec<usize> = members.into_iter().collect();
        mem.sort_by_key(|&i| (Reverse(degree[i]), i));
        aliases.sort();
        out.push(Faction {
            name: factions[keep].name.clone(),
            aliases,
            members: mem,
            leader: factions[keep].leader,
            kind,
        });
    }
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(a.name.cmp(&b.name))
    });
    (out, rename)
}

/// 领袖：role 里的「势力名 + 领袖词」> 多批一致声明 > 单票声明 > 度数最高
fn pick_leader(
    faction: &str,
    members: &[usize],
    clusters: &[Cluster],
    votes: Option<&BTreeMap<usize, usize>>,
    degree: &[usize],
) -> Option<usize> {
    let mut ranked: Vec<(usize, f64, usize)> = Vec::new();
    for &i in members {
        let role = &clusters[i].role;
        if role.is_empty() || !role.contains(faction) {
            continue;
        }
        let mut pr = if LEADER_SOVEREIGN.is_match(role) {
            3.0
        } else if LEADER_WORD.is_match(role) {
            2.0
        } else {
            1.0
        };
        if pr >= 2.0 && LEADER_DEPUTY.is_match(role) {
            pr -= 0.5;
        }
        ranked.push((i, pr, degree[i]));
    }
    let top = ranked.iter().max_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.2.cmp(&b.2))
    });
    if let Some((i, pr, _)) = top {
        if *pr >= 2.0 {
            return Some(*i);
        }
    }
    if let Some(votes) = votes {
        let mut sorted: Vec<(usize, usize)> = votes
            .iter()
            .filter(|(k, _)| members.contains(k))
            .map(|(k, v)| (*k, *v))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        if let Some(&(k, _)) = sorted.first() {
            return Some(k);
        }
    }
    members.first().copied()
}

// ---------------------------------------------------------------- 主体

/// 把多批模型原始 JSON 合并成一份 `graph-demo-v1` 数据
///
/// `hints` 是库里的权威书籍信息（`books` 表 / 目录），模型输出只作次选：
/// 实测 6 批 JSON 里一个 `book` 字段都没有，光靠它取名只会得到「未知作品」。
pub fn normalize(raws: &[Value], chapters: &[Chapter], hints: &BookHints) -> Value {
    let batches = batch_objects(raws);
    let book = Book::new(chapters);
    let total = total_chapters(chapters, hints.total_chapters);
    let mut drops = Drops::default();

    let (mut clusters, ambiguous) = merge_characters(&batches);

    // 端点解析 + 同人合并后解析端点 + quote 回正文校验（含章号归一到真实章号）
    let mut rels = build_relations(&batches, &clusters, &book, &mut drops);
    rels = repair_kin_quotes(rels, &clusters, &book, &mut drops);
    rels = dedupe(rels, &mut drops);
    rels = fix_master(rels, &mut drops);
    rels = keep_parent_cue(rels, &mut drops);
    rels = keep_ally_cue(rels, &clusters, &mut drops);
    rels = drop_ally_enemy_conflict(rels, &mut drops);
    rels = keep_other_cue(rels, &mut drops);

    // 出场区间
    for c in clusters.iter_mut() {
        let toks: Vec<String> = c
            .tokens()
            .into_iter()
            .filter(|t| char_len(t) >= 2)
            .collect();
        let appear = book.chapters_with_any(&toks);
        c.first_seen = appear.first().copied().unwrap_or(total);
        c.last_seen = appear.last().copied().unwrap_or(total);
    }

    // role 里点名的势力补 faction（「赤心队首领」→ 赤心队）
    let mut faction_names: BTreeSet<String> =
        clusters.iter().filter_map(|c| c.faction.clone()).collect();
    for batch in &batches {
        let Some(factions) = batch.get("factions").and_then(|v| v.as_array()) else {
            continue;
        };
        for f in factions {
            let name = normalize_faction_name(f.get("name").and_then(|v| v.as_str()).unwrap_or(""));
            if !name.is_empty() {
                faction_names.insert(name);
            }
        }
    }
    let mut by_len: Vec<&String> = faction_names.iter().collect();
    by_len.sort_by(|a, b| char_len(b).cmp(&char_len(a)).then(a.cmp(b)));
    for c in clusters.iter_mut() {
        if c.faction.is_some() || c.role.is_empty() {
            continue;
        }
        if let Some(f) = by_len.iter().find(|f| c.role.contains(f.as_str())) {
            c.faction = Some((*f).clone());
        }
    }

    // generation（裁剪前算，裁剪后按下标映射回来）
    let (worn, level, conflict) = assign_generation(&rels, clusters.len(), &mut drops);
    let rels: Vec<Rel> = rels
        .into_iter()
        .zip(worn)
        .filter(|(_, worn)| !worn)
        .map(|(r, _)| r)
        .collect();

    // 批次声明的势力领袖保住，别被规模收敛砍掉
    let pre_resolver = Resolver::new(&clusters);
    let mut must_keep: HashSet<usize> = HashSet::new();
    for batch in &batches {
        let Some(factions) = batch.get("factions").and_then(|v| v.as_array()) else {
            continue;
        };
        for f in factions {
            let leader = strip_quotes(f.get("leader").and_then(|v| v.as_str()).unwrap_or(""));
            if leader.is_empty() {
                continue;
            }
            if let Some(k) = pre_resolver.resolve(leader) {
                must_keep.insert(k);
            }
        }
    }

    let (kept, rels) = prune_for_demo(&clusters, &rels, &must_keep, &mut drops);
    let mut gen_of: Vec<i64> = kept.iter().map(|&old| level[old]).collect();
    let conflict_of: Vec<bool> = kept.iter().map(|&old| conflict[old]).collect();
    let kept_clusters: Vec<Cluster> = kept.iter().map(|&old| clusters[old].clone()).collect();

    // id 按出场先后分配
    let mut order: Vec<usize> = (0..kept_clusters.len()).collect();
    order.sort_by_key(|&i| (kept_clusters[i].first_seen, i));
    let mut cid = vec![String::new(); kept_clusters.len()];
    for (k, &i) in order.iter().enumerate() {
        cid[i] = format!("c{}", k + 1);
    }
    let mut degree = vec![0usize; kept_clusters.len()];
    for r in &rels {
        degree[r.a] += 1;
        degree[r.b] += 1;
    }

    // 势力投票（裁剪后重建名字表）→ 家族归并 → 规模收敛 → 选领袖
    let post_resolver = Resolver::new(&kept_clusters);
    let (voted, declared) = vote_factions(&batches, &kept_clusters, &post_resolver, &degree);
    let (factions, mut mem_of, rename) = settle_factions(voted, &kept_clusters, &declared, &degree);
    let fid: BTreeMap<String, String> = factions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), format!("f{}", i + 1)))
        .collect();

    // 岳父/女婿（正文核实后改成 other + label）：辈分上晚辈低一代
    for r in &rels {
        if r.t == "other" && matches!(r.label.as_deref(), Some("岳父") | Some("女婿")) {
            let child = gen_of[r.a] + 1;
            gen_of[r.b] = child;
        }
    }

    // characters
    let mut out_chars = Vec::with_capacity(order.len());
    for &i in &order {
        let c = &kept_clusters[i];
        let role = if BAD_ROLE.contains(&c.role.as_str()) {
            match c.faction.as_deref() {
                // 归并后的势力名（「黎泾李家属员」→「李家属员」）
                Some(f) => format!("{}属员", rename.get(f).map_or(f, String::as_str)),
                None => "正文提及人物".to_string(),
            }
        } else {
            c.role.clone()
        };
        let faction = mem_of
            .get(i)
            .and_then(|fs| fs.first())
            .and_then(|fi| factions.get(*fi))
            .and_then(|f| fid.get(&f.name).cloned());
        out_chars.push(json!({
            "id": cid[i],
            "name": c.name,
            "aliases": c.aliases,
            "gender": if c.gender.is_empty() { GENDER_FALLBACK } else { c.gender.as_str() },
            "role": role,
            "faction": faction,
            "generation": gen_of[i],
            "firstSeen": c.first_seen,
            "lastSeen": c.last_seen,
            "conflict": conflict_of[i],
        }));
    }

    // relations
    let mut sorted: Vec<&Rel> = rels.iter().collect();
    sorted.sort_by_key(|r| (r.ch, r.a, r.b));
    let mut out_rels = Vec::with_capacity(sorted.len());
    for (k, r) in sorted.into_iter().enumerate() {
        let last = kept_clusters[r.a]
            .last_seen
            .max(kept_clusters[r.b].last_seen);
        let to_chapter = if total - last >= GAP {
            json!(last)
        } else {
            Value::Null
        };
        out_rels.push(json!({
            // 前端 zod 契约里 relation 没有 id（按 from/to 引用）；这里仍给稳定编号，方便排障
            "id": format!("r{}", k + 1),
            "from": cid[r.a],
            "to": cid[r.b],
            "type": r.t,
            "label": r.label.clone().unwrap_or_else(|| label_of(r.t).to_string()),
            "from_chapter": r.ch,
            "to_chapter": to_chapter,
            "quote": r.q,
        }));
    }

    // memberships
    let mut out_mem = Vec::new();
    for &i in &order {
        let c = &kept_clusters[i];
        let Some(fs) = mem_of.get_mut(i) else {
            continue;
        };
        if fs.is_empty() {
            continue;
        }
        fs.sort_unstable();
        let to = if total - c.last_seen >= GAP {
            json!(c.last_seen)
        } else {
            Value::Null
        };
        for fi in fs.iter() {
            let Some(f) = factions.get(*fi) else {
                continue;
            };
            let Some(faction_id) = fid.get(&f.name) else {
                continue;
            };
            out_mem.push(json!({
                "character": cid[i],
                "faction": faction_id,
                "from": c.first_seen,
                "to": to.clone(),
            }));
        }
    }

    // factions
    let mut out_factions = Vec::with_capacity(factions.len());
    for (fi, f) in factions.iter().enumerate() {
        out_factions.push(json!({
            "id": format!("f{}", fi + 1),
            "name": f.name,
            // 归并掉的写法（「黎泾李家」「萧家」…）：前端 zod 宽容解析会忽略，留给排障/人工核对
            "aliases": f.aliases,
            "kind": f.kind,
            "leader": f.leader.map(|i| cid[i].clone()),
            "members": f.members.iter().map(|&i| cid[i].clone()).collect::<Vec<_>>(),
        }));
    }

    let (prompt_tokens, completion_tokens) = token_cost(raws);
    tracing::debug!(
        drops = ?drops.0,
        ambiguous = ambiguous.len(),
        "graph normalize 丢弃统计"
    );

    let mut graph = json!({
        "meta": {
            "book": book_name(&batches, &hints.book_name),
            "totalChapters": total,
            "sampledChapters": chapters.len(),
            "generatedAt": generated_at(),
            "schema": SCHEMA,
            "source": "batch",
            "cost": { "promptTokens": prompt_tokens, "completionTokens": completion_tokens },
        },
        "characters": out_chars,
        "factions": out_factions,
        "memberships": out_mem,
        "relations": out_rels,
        "stats": {
            "characters": out_chars.len(),
            "relations": out_rels.len(),
            "factions": out_factions.len(),
            "chaptersScanned": chapters.len(),
            "sampledChapters": chapters.len(),
        },
    });
    // 血缘边不该有结束章：落盘前先归一，保证写进去的和读出来的是一个口径（幂等）
    normalize_kin_permanence(&mut graph);
    graph
}

// ---------------------------------------------------------------- 血缘「至今仍在」归一 + 体检

/// 「一旦成立就至今仍在」的关系类型：血缘（父子 / 兄弟姐妹）。
///
/// 抽取模型把「最后一次提到 / 人物死亡」写进了 `to_chapter`，于是血缘也会过期：
/// 生产实例《玄鉴仙族》里 `李木田 → 李长湖 [parent] 4..1711`，前端按
/// `to_chapter >= 当前章` 判「还成不成立」，读到 1749 章就把这条边藏了——同一个
/// 37 人的亲缘连通分量在画面上裂成两块。血缘是身份不是状态，收盘章一律 `null`
/// （= 至今仍在）；`spouse`/`enemy`/`ally`/`other`/`master`/`disciple` 保留原区间语义。
pub const PERMANENT_RELATION_TYPES: &[&str] = &["parent", "sibling"];

/// 日志里每类告警最多带几条样例（别逐条刷屏）
const HEALTH_SAMPLES: usize = 5;

/// 一次归一的体检结果：数量给日志，样例给排障。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GraphHealth {
    /// 关系总数
    pub relations: usize,
    /// 端点缺失（`from`/`to` 的人物 id 不在 characters 里）的关系数
    pub dangling: usize,
    /// 端点缺失样例（至多 [`HEALTH_SAMPLES`] 条）
    pub dangling_samples: Vec<String>,
    /// 本次被抹掉结束章的血缘边数（`to_chapter` 原本非 null）
    pub kin_ended: usize,
    /// 血缘边样例（至多 [`HEALTH_SAMPLES`] 条）
    pub kin_samples: Vec<String>,
    /// 同一势力内却与主家不连通的血缘小块数（疑似抽取漏边）
    pub stranded: usize,
    /// 孤离小块样例（至多 [`HEALTH_SAMPLES`] 条）
    pub stranded_samples: Vec<String>,
}

/// 写路径的归一 + 体检：`normalize()` 落盘前调用（一本书归一一次，日志照发）。
///
/// 归一口径见 [`kin_permanence_pass`]；这层只多一步「落日志」。
pub fn normalize_kin_permanence(graph: &mut Value) -> GraphHealth {
    let health = kin_permanence_pass(graph);
    log_graph_health(&health);
    health
}

/// 归一本体（纯函数、幂等、不打日志）：
/// 1. `parent`/`sibling` 的 `to_chapter` → `null`（血缘不随最后一次提及过期）；
/// 2. 顺带体检：端点缺失、血缘边带结束章，数量与前几条样例回给调用方落日志。
///
/// 已存成品（`book_graphs.data_json`）不用重新生成，走一遍这个函数就修好了；
/// 幂等：跑两遍的结果与跑一遍完全一致（第二遍 `kin_ended` 为 0，JSON 不变）。
/// 不增删关系、不改顺序、不动其他字段——前端 zod 契约里 `to_chapter` 本就允许 null。
fn kin_permanence_pass(graph: &mut Value) -> GraphHealth {
    let mut health = GraphHealth::default();

    // 第三种体检：同势力内的孤离血缘小块。只读 characters/relations/factions，
    // 与「抹结束章」互不干扰，所以放在借用 relations 之前先算。
    let (stranded, stranded_samples) = stranded_kin_blocks(graph);
    health.stranded = stranded;
    health.stranded_samples = stranded_samples;

    // id → 名字：告警样例要能被人读懂（「李木田(c5) → 李长湖(c7)」），不是纯 c 编号
    let names: HashMap<String, String> = graph
        .get("characters")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let id = c.get("id").and_then(|v| v.as_str())?;
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    Some((id.to_string(), name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    let Some(rels) = graph.get_mut("relations").and_then(|v| v.as_array_mut()) else {
        // 没有 relations（未生成/坏数据）：无事可做，也没得体检
        return health;
    };
    health.relations = rels.len();

    for rel in rels.iter_mut() {
        let Some(obj) = rel.as_object_mut() else {
            continue;
        };
        let t = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("other")
            .to_string();
        let a = obj
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let b = obj
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !names.contains_key(&a) || !names.contains_key(&b) {
            health.dangling += 1;
            if health.dangling_samples.len() < HEALTH_SAMPLES {
                health.dangling_samples.push(format!(
                    "{} → {} [{t}]",
                    person(&names, &a),
                    person(&names, &b)
                ));
            }
        }

        if !PERMANENT_RELATION_TYPES.contains(&t.as_str()) {
            continue;
        }
        let ended = obj.get("to_chapter").cloned().unwrap_or(Value::Null);
        if ended.is_null() {
            continue;
        }
        health.kin_ended += 1;
        if health.kin_samples.len() < HEALTH_SAMPLES {
            let from = obj
                .get("from_chapter")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            health.kin_samples.push(format!(
                "{} → {} [{t}] {from}..{}",
                person(&names, &a),
                person(&names, &b),
                ended
            ));
        }
        obj.insert("to_chapter".to_string(), Value::Null);
    }

    health
}

/// 告警样例里的人物写法：`李木田(c5)`；id 缺失/查不到时退回原始 id
fn person(names: &HashMap<String, String>, id: &str) -> String {
    match names.get(id) {
        Some(n) => format!("{n}({id})"),
        None => format!("{id}(?)"),
    }
}

/// 判亲缘连通时算进去的关系类型：父子、兄弟姐妹、夫妻
const KIN_GRAPH_TYPES: &[&str] = &["parent", "sibling", "spouse"];

/// 孤离小块的下限：主块至少这么大（人），且至少是小块的这么多倍
const STRANDED_MAIN_MIN: usize = 10;
const STRANDED_RATIO: usize = 3;

/// 同势力内「孤离血缘小块」检测（疑似抽取漏边，只报不改）。
///
/// 生产实例：《玄鉴仙族》「黎泾李家」35 人里 27 人连成一片，只剩
/// `李承福 → 李秋阳 [parent]` 两人自成一小块——两人都在 `characters` 里、
/// 端点没缺，所以端点检查看不见它；缺的是把这一小撮并回主家的那条边，
/// 画面上就是一座飘着的孤岛。补边要重跑模型，本次不修，先让日志看见。
fn stranded_kin_blocks(graph: &Value) -> (usize, Vec<String>) {
    let Some(chars) = graph.get("characters").and_then(|v| v.as_array()) else {
        return (0, Vec::new());
    };
    let mut names: Vec<&str> = vec![""; chars.len()];
    let mut idx: HashMap<&str, usize> = HashMap::with_capacity(chars.len());
    for (i, c) in chars.iter().enumerate() {
        let Some(id) = c.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        idx.insert(id, i);
        names[i] = c.get("name").and_then(|v| v.as_str()).unwrap_or(id);
    }

    // 并查集：亲缘边把人物连成块（端点缺失的边天然被跳过）
    let mut uf: Vec<usize> = (0..names.len()).collect();
    if let Some(rels) = graph.get("relations").and_then(|v| v.as_array()) {
        for r in rels {
            let t = r.get("type").and_then(|v| v.as_str()).unwrap_or("other");
            if !KIN_GRAPH_TYPES.contains(&t) {
                continue;
            }
            let end = |k: &str| {
                r.get(k)
                    .and_then(|v| v.as_str())
                    .and_then(|s| idx.get(s).copied())
            };
            let (Some(a), Some(b)) = (end("from"), end("to")) else {
                continue;
            };
            uf_union(&mut uf, a, b);
        }
    }

    let Some(factions) = graph.get("factions").and_then(|v| v.as_array()) else {
        return (0, Vec::new());
    };
    let mut total = 0;
    let mut samples: Vec<String> = Vec::new();
    for f in factions {
        let fname = f.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let Some(members) = f.get("members").and_then(|v| v.as_array()) else {
            continue;
        };
        // 本势力成员按亲缘连通块归组
        let mut blocks: HashMap<usize, Vec<usize>> = HashMap::new();
        for m in members {
            let Some(i) = m.as_str().and_then(|s| idx.get(s).copied()) else {
                continue;
            };
            let root = uf_root(&mut uf, i);
            blocks.entry(root).or_default().push(i);
        }
        let mut sizes: Vec<(usize, Vec<usize>)> =
            blocks.into_values().map(|v| (v.len(), v)).collect();
        sizes.sort_by_key(|b| std::cmp::Reverse(b.0));
        let Some(main) = sizes.first().map(|(n, _)| *n) else {
            continue;
        };
        if main < STRANDED_MAIN_MIN || sizes.len() < 2 {
            continue;
        }
        for (len, who) in sizes.iter().skip(1) {
            // 单人块多半是「属员/管事」这类本来就没血缘的人，噪音大，只报 ≥2 人的
            if *len < 2 || *len * STRANDED_RATIO > main {
                continue;
            }
            total += 1;
            if samples.len() < HEALTH_SAMPLES {
                let who: Vec<&str> = who.iter().map(|i| names[*i]).collect();
                samples.push(format!(
                    "{fname}: {}（{len} 人）与主块 {main} 人之间没有任何血缘/姻亲边相连",
                    who.join("、")
                ));
            }
        }
    }
    (total, samples)
}

/// 两类数据缺口各汇总一条 `warn!`（数量 + 前几条样例），不逐条刷屏
fn log_graph_health(h: &GraphHealth) {
    if h.dangling > 0 {
        tracing::warn!(
            dangling = h.dangling,
            total = h.relations,
            samples = ?h.dangling_samples,
            "图谱关系端点缺失：from/to 的人物 id 不在 characters 里（抽取漏边/合并丢人，本次只报不改）"
        );
    }
    if h.kin_ended > 0 {
        tracing::warn!(
            kin_ended = h.kin_ended,
            total = h.relations,
            samples = ?h.kin_samples,
            "血缘边被写成有结束章，已归一为 to_chapter = null（父子/兄弟姐妹不随最后一次提及过期）"
        );
    }
    if h.stranded > 0 {
        tracing::warn!(
            stranded_blocks = h.stranded,
            samples = ?h.stranded_samples,
            "同势力内出现孤离的血缘小块：块内人物与主家无任何血缘/姻亲边相连，疑似抽取漏边（本次只报不改）"
        );
    }
}

/// 读取路径入口：把「已存成品」过一遍幂等归一后交给路由。
///
/// 存在的理由是**老数据**：`book_graphs` 里已经躺着的成品是旧口径生成的（血缘边带
/// 结束章），重跑模型又贵又没必要——每次读出来归一一次即可，且幂等所以不会漂移。
/// 独立成一个函数是为了让路由侧只多一行调用，改动面最小。
pub fn serve_stored(mut graph: Value) -> Value {
    let health = kin_permanence_pass(&mut graph);
    // 成品不会被写回，所以 `kin_ended` 每次读都 > 0：同一本书每进程只告警一次，
    // 否则刷新页面就会反复刷 warn。
    let key = format!(
        "{}#{}",
        graph
            .pointer("/meta/book")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        health.relations
    );
    let first_time = {
        let mut warned = warned_books().lock().unwrap_or_else(|e| e.into_inner());
        warned.insert(key)
    };
    if first_time {
        log_graph_health(&health);
    }
    graph
}

/// 读路径已告警过的书（进程内去重）
fn warned_books() -> &'static Mutex<BTreeSet<String>> {
    static WARNED: Lazy<Mutex<BTreeSet<String>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));
    &WARNED
}

/// 全书真实总章数。
///
/// 优先用库里查实的值（`books.total_chapter_num`，退目录长度），再退「最大章号 / 章数」。
/// 旧实现只取 `max(chapter_index)`：书源书的 `chapter_index` 是 md5 哈希，于是页面显示
/// 「1151345960693571501 章」（Bug 1）。
fn total_chapters(chapters: &[Chapter], hint: i64) -> i64 {
    let last = chapters.iter().map(|c| c.0).max().unwrap_or(0);
    hint.max(last).max(chapters.len() as i64)
}

/// 书名：库里的权威名 > 模型批次 JSON 的 `book` > 兜底「未知作品」。
///
/// 模型这边指望不上：实测整本书 6 批 JSON 里全部没有 `book` 字段（Bug 2）。
fn book_name(batches: &[Value], hint: &str) -> String {
    let hint = hint.trim();
    if !hint.is_empty() && hint != BOOK_FALLBACK {
        return hint.to_string();
    }
    for batch in batches {
        if let Some(name) = batch.get("book").and_then(|v| v.as_str()) {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
        if let Some(name) = batch
            .get("meta")
            .and_then(|m| m.get("book"))
            .and_then(|v| v.as_str())
        {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    BOOK_FALLBACK.to_string()
}

/// RFC3339 本地时间（固定 +08:00），与离线脚本的 `%Y-%m-%dT%H:%M:%S%z` 一致
fn generated_at() -> String {
    let offset = chrono::FixedOffset::east_opt(8 * 3600)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC 偏移恒有效"));
    chrono::Utc::now()
        .with_timezone(&offset)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn token_cost(raws: &[Value]) -> (u64, u64) {
    let mut prompt = 0u64;
    let mut completion = 0u64;
    for raw in raws {
        let Some(usage) = raw.get("usage") else {
            continue;
        };
        prompt += usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        completion += usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    }
    (prompt, completion)
}

/// 每批的抽取结果（容错：既吃已解析的批对象，也吃 chat completion 信封）
fn batch_objects(raws: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for raw in raws {
        if raw.get("characters").is_some() || raw.get("relations").is_some() {
            out.push(raw.clone());
            continue;
        }
        let Some(text) = raw
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let cleaned = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        match serde_json::from_str::<Value>(cleaned) {
            Ok(v) => out.push(v),
            Err(err) => tracing::warn!("跳过无法解析的一批: {err}"),
        }
    }
    out
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn chapters(texts: &[&str]) -> Vec<Chapter> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| ((i + 1) as i64, format!("第{}章", i + 1), (*t).to_string()))
            .collect()
    }

    /// 不带书籍元信息的归一化（多数用例只关心关系/势力逻辑）
    fn norm(raws: &[Value], chapters: &[Chapter]) -> Value {
        normalize(raws, chapters, &BookHints::default())
    }

    fn rels_of(data: &Value) -> &Vec<Value> {
        data["relations"].as_array().expect("relations 是数组")
    }

    fn chars_of(data: &Value) -> &Vec<Value> {
        data["characters"].as_array().expect("characters 是数组")
    }

    fn find_char<'a>(data: &'a Value, name: &str) -> Option<&'a Value> {
        chars_of(data)
            .iter()
            .find(|c| c["name"].as_str() == Some(name))
    }

    fn generation_of(data: &Value, name: &str) -> i64 {
        find_char(data, name).unwrap_or_else(|| panic!("{name} 应当在人物表里"))["generation"]
            .as_i64()
            .expect("generation 是整数")
    }

    /// ① quote 在正文里找不到 → 该关系被丢
    #[test]
    fn drops_relation_when_quote_missing() {
        let raw = json!({
            "characters": [{"name": "钱九"}, {"name": "吴十"}],
            "relations": [{
                "from": "钱九", "to": "吴十", "type": "parent",
                "quote": "钱九之子吴十", "chapter": 1,
            }],
        });
        // 正文里根本没有这句
        let data = norm(&[raw], &chapters(&["正文里只有无关紧要的几句闲话。"]));
        assert!(rels_of(&data).is_empty(), "找不到出处的边必须丢掉");
        assert!(chars_of(&data).is_empty(), "孤点也要被剔除");
    }

    /// ② 无向类型 A-B 与 B-A 视为同一条
    #[test]
    fn dedupes_undirected_pair() {
        let raw = json!({
            "characters": [{"name": "孙七"}, {"name": "周八"}],
            "relations": [
                {"from": "孙七", "to": "周八", "type": "ally", "quote": "孙七与周八结盟", "chapter": 1},
                {"from": "周八", "to": "孙七", "type": "ally", "quote": "孙七与周八结盟", "chapter": 1},
            ],
        });
        let data = norm(&[raw], &chapters(&["孙七与周八结盟。"]));
        assert_eq!(rels_of(&data).len(), 1, "A-B / B-A 只留一条");
        assert_eq!(rels_of(&data)[0]["type"], "ally");
    }

    /// ③ parent 链条 父比子小 1；sibling 同代
    #[test]
    fn generation_from_parent_and_sibling() {
        let text = "张三之子李四。李四之子王五。李四亲弟赵六。";
        let raw = json!({
            "characters": [{"name": "张三"}, {"name": "李四"}, {"name": "王五"}, {"name": "赵六"}],
            "relations": [
                {"from": "张三", "to": "李四", "type": "parent", "quote": "张三之子李四", "chapter": 1},
                {"from": "李四", "to": "王五", "type": "parent", "quote": "李四之子王五", "chapter": 1},
                {"from": "李四", "to": "赵六", "type": "sibling", "quote": "李四亲弟赵六", "chapter": 1},
            ],
        });
        let data = norm(&[raw], &chapters(&[text]));
        assert_eq!(
            generation_of(&data, "李四"),
            generation_of(&data, "张三") + 1,
            "父比子小 1"
        );
        assert_eq!(
            generation_of(&data, "王五"),
            generation_of(&data, "李四") + 1,
            "父比子小 1（多代）"
        );
        assert_eq!(
            generation_of(&data, "赵六"),
            generation_of(&data, "李四"),
            "兄弟同代"
        );
    }

    /// ④ 规模收敛后亲缘边的端点仍在
    #[test]
    fn prune_keeps_kin_endpoints() {
        let n = 150usize;
        // 名字必须 2-6 个汉字，所以用「人」+ 一个 CJK 码点拼出 150 个不重名的人
        let names: Vec<String> = (0..n)
            .map(|k| {
                let ch = char::from_u32(0x4E00 + k as u32).expect("CJK 码点");
                format!("人{ch}")
            })
            .collect();

        let chars: Vec<Value> = names
            .iter()
            .map(|name| json!({"name": name, "role": "兵卒"}))
            .collect();
        let mut text = String::new();
        let mut rels = Vec::new();
        // 前 148 人各 6 度（高分，占满 TARGET_CHARS）
        for k in 0..148usize {
            for step in [1usize, 7, 13] {
                let other = (k + step) % 148;
                let quote = format!("{}与{}结盟", names[k], names[other]);
                text.push_str(&quote);
                text.push('。');
                rels.push(json!({
                    "from": names[k], "to": names[other], "type": "ally",
                    "quote": quote, "chapter": 1,
                }));
            }
        }
        // 最后两人只有一条亲缘边：分数垫底，全靠「亲缘端点保住」才能活下来
        let kin_quote = format!("{}之子{}", names[148], names[149]);
        text.push_str(&kin_quote);
        text.push('。');
        rels.push(json!({
            "from": names[148], "to": names[149], "type": "parent",
            "quote": kin_quote, "chapter": 1,
        }));

        let raw = json!({"characters": chars, "relations": rels});
        let data = norm(&[raw], &chapters(&[&text]));

        let chars = chars_of(&data);
        assert!(
            chars.len() <= MAX_CHARS,
            "人物数要收敛到上限内: {}",
            chars.len()
        );
        assert!(chars.len() >= TARGET_CHARS, "不该砍过头: {}", chars.len());
        assert!(chars.len() < n, "确实发生了截断: {} vs {n}", chars.len());

        let kin_names = [names[148].as_str(), names[149].as_str()];
        for name in kin_names {
            assert!(find_char(&data, name).is_some(), "亲缘端点 {name} 必须保住");
        }
        let parents: Vec<&Value> = rels_of(&data)
            .iter()
            .filter(|r| r["type"] == "parent")
            .collect();
        assert_eq!(parents.len(), 1, "亲缘边必须保留");
        let ids: HashSet<&str> = chars.iter().filter_map(|c| c["id"].as_str()).collect();
        assert!(
            ids.contains(parents[0]["from"].as_str().unwrap()),
            "父端仍在人物表里"
        );
        assert!(
            ids.contains(parents[0]["to"].as_str().unwrap()),
            "子端仍在人物表里"
        );
    }

    /// ⑤ repair_parent_direction 的四种句式
    #[test]
    fn repair_direction_by_quote() {
        let mut rels = vec![
            // 「其父X」写反：X 才是父端
            json!({"from": "刘子羽", "to": "刘韐", "type": "parent", "quote": "其父刘韐"}),
            // 「生母X」写反：X 才是母端
            json!({"from": "赵玖", "to": "韦太后", "type": "parent", "quote": "官家的生母韦太后"}),
            // 姻亲不是亲子：降 other 并用那个词做 label
            json!({"from": "薛元礼", "to": "高守义", "type": "parent", "quote": "他岳父薛元礼"}),
            // 方向本来就对：不许动
            json!({"from": "王氏", "to": "李小", "type": "parent", "quote": "李小生母王氏"}),
            // 「X长子」写反：前面那个才是父端
            json!({"from": "岳甫", "to": "岳云", "type": "parent", "quote": "岳云长子岳甫"}),
            // 白话叙述「X的爹就是Y」：Y 才是父端（模型把方向写反了）
            json!({"from": "刘光世", "to": "刘延庆", "type": "parent",
                   "quote": "刘光世的爹就是著名的刘延庆"}),
        ];
        let book = chapters(&[
            "其父刘韐。官家的生母韦太后。他岳父薛元礼。李小生母王氏。岳云长子岳甫。\
             刘光世的爹就是著名的刘延庆。",
        ]);
        repair_parent_direction(&mut rels, &book);

        assert_eq!(rels[0]["from"], "刘韐");
        assert_eq!(rels[0]["to"], "刘子羽");
        assert_eq!(rels[0]["type"], "parent");

        assert_eq!(rels[1]["from"], "韦太后");
        assert_eq!(rels[1]["to"], "赵玖");
        assert_eq!(rels[1]["type"], "parent");

        assert_eq!(rels[2]["type"], "other");
        assert_eq!(rels[2]["label"], "岳父");
        assert_eq!(rels[2]["from"], "薛元礼", "姻亲不做翻转");

        assert_eq!(rels[3]["from"], "王氏", "方向本来就对，不动");
        assert_eq!(rels[3]["to"], "李小");
        assert_eq!(rels[3]["type"], "parent");

        assert_eq!(rels[4]["from"], "岳云", "「X长子」的 X 才是父端");
        assert_eq!(rels[4]["to"], "岳甫");
        assert_eq!(rels[4]["type"], "parent");

        assert_eq!(rels[5]["from"], "刘延庆", "「X的爹就是Y」的 Y 才是父端");
        assert_eq!(rels[5]["to"], "刘光世");
        assert_eq!(rels[5]["type"], "parent");
        assert!(rels[3].get("label").is_none());
    }

    /// 出处都找不到的 quote 不足为凭：不修
    #[test]
    fn repair_direction_skips_unverifiable_quote() {
        let mut rels = vec![json!({
            "from": "刘子羽", "to": "刘韐", "type": "parent", "quote": "其父刘韐",
        })];
        repair_parent_direction(&mut rels, &chapters(&["另一段完全不相干的正文。"]));
        assert_eq!(rels[0]["from"], "刘子羽");
    }

    /// 空输入不能炸，且仍然是合法的 graph-demo-v1
    #[test]
    fn empty_input_stays_valid() {
        let data = norm(&[], &[]);
        assert!(chars_of(&data).is_empty());
        assert!(rels_of(&data).is_empty());
        assert!(data["factions"].as_array().expect("factions").is_empty());
        assert!(data["memberships"]
            .as_array()
            .expect("memberships")
            .is_empty());
        assert_eq!(data["meta"]["schema"], SCHEMA);
        assert_eq!(data["meta"]["totalChapters"], 0);
    }

    /// 输出的字段名必须和 `frontend/src/types/graphDemo.ts` 一致
    #[test]
    fn output_matches_frontend_contract() {
        let raw = json!({
            "book": "测试书",
            "characters": [
                {"name": "张三", "role": "家主", "faction": "张氏"},
                {"name": "李四", "role": "张氏部将"},
            ],
            "relations": [
                {"from": "张三", "to": "李四", "type": "other", "quote": "张三麾下李四", "chapter": 1},
            ],
            "factions": [{"name": "张氏", "leader": "张三"}],
        });
        let data = norm(&[raw], &chapters(&["张三麾下李四，皆听号令。"]));
        assert_eq!(data["meta"]["schema"], SCHEMA);
        assert_eq!(data["meta"]["source"], "batch");
        assert!(data["meta"]["generatedAt"]
            .as_str()
            .unwrap()
            .ends_with("+08:00"));
        for key in [
            "book",
            "totalChapters",
            "sampledChapters",
            "generatedAt",
            "schema",
        ] {
            assert!(!data["meta"][key].is_null(), "meta.{key} 缺失");
        }
        let c = &chars_of(&data)[0];
        for key in [
            "id",
            "name",
            "aliases",
            "gender",
            "role",
            "faction",
            "generation",
            "firstSeen",
            "lastSeen",
            "conflict",
        ] {
            assert!(
                !c[key].is_null() && c.get(key).is_some(),
                "character.{key} 缺失"
            );
        }
        let r = &rels_of(&data)[0];
        for key in [
            "from",
            "to",
            "type",
            "label",
            "from_chapter",
            "to_chapter",
            "quote",
        ] {
            assert!(r.get(key).is_some(), "relation.{key} 缺失");
        }
        assert!(RELATION_TYPES.contains(&r["type"].as_str().unwrap()));
        assert_eq!(r["id"], "r1", "关系按输出顺序编稳定号");
        assert_eq!(c["id"], "c1", "人物按出场顺序编稳定号");
        let f = &data["factions"][0];
        for key in ["id", "name", "kind", "leader", "members"] {
            assert!(f.get(key).is_some(), "faction.{key} 缺失");
        }
        let m = &data["memberships"][0];
        for key in ["character", "faction", "from", "to"] {
            assert!(m.get(key).is_some(), "membership.{key} 缺失");
        }
        // 关系端点必须在人物表里，且不许自环
        let ids: HashSet<&str> = chars_of(&data)
            .iter()
            .filter_map(|c| c["id"].as_str())
            .collect();
        for r in rels_of(&data) {
            assert!(ids.contains(r["from"].as_str().unwrap()));
            assert!(ids.contains(r["to"].as_str().unwrap()));
            assert_ne!(r["from"], r["to"]);
            assert!(char_len(r["quote"].as_str().unwrap()) <= QUOTE_CAP);
        }
    }

    // ---- 书名 / 总章数：库里查实的比模型输出权威（Bug 2） ----

    /// 模型批次里没有 `book` 字段（生产现场 6 批全空）→ 取 `books.name`
    #[test]
    fn book_name_prefers_library_hint() {
        let raw = json!({"characters": [], "relations": []});
        let hints = BookHints {
            book_name: "玄鉴仙族".into(),
            total_chapters: 1745,
        };
        let data = normalize(std::slice::from_ref(&raw), &chapters(&["正文。"]), &hints);
        assert_eq!(data["meta"]["book"], "玄鉴仙族");
        // 没查到书（hints 空）→ 才退回模型输出，再不行才算兜底
        let data = normalize(
            std::slice::from_ref(&raw),
            &chapters(&["正文。"]),
            &BookHints::default(),
        );
        assert_eq!(data["meta"]["book"], BOOK_FALLBACK);
        let with_book = json!({"book": "模型给的书名", "characters": [], "relations": []});
        let data = normalize(&[with_book], &chapters(&["正文。"]), &BookHints::default());
        assert_eq!(data["meta"]["book"], "模型给的书名");
    }

    /// `meta.totalChapters` 取 `books.total_chapter_num`，不再被 md5 章号带跑
    #[test]
    fn total_chapters_uses_library_hint() {
        let raws = vec![json!({"characters": [], "relations": []})];
        // number_chapters 之后章号是 1..N（这里只缓存了 3 章）
        let chs = chapters(&["甲。", "乙。", "丙。"]);
        let hints = BookHints {
            book_name: "玄鉴仙族".into(),
            total_chapters: 1745,
        };
        let data = normalize(&raws, &chs, &hints);
        assert_eq!(data["meta"]["totalChapters"], 1745, "应有 1745 章");
        // 没有库信息时退回「章数」，也绝不出现 1e18
        let data = normalize(&raws, &chs, &BookHints::default());
        assert_eq!(data["meta"]["totalChapters"], 3);
    }

    // ---- 家族势力归并（Bug 3） ----

    fn cluster_named(name: &str, faction: Option<&str>) -> Cluster {
        Cluster {
            name: name.into(),
            aliases: Vec::new(),
            role: String::new(),
            faction: faction.map(str::to_string),
            gender: GENDER_FALLBACK.into(),
            first_seen: 1,
            last_seen: 1,
        }
    }

    fn faction_of(name: &str, members: &[usize], leader: Option<usize>) -> Faction {
        Faction {
            name: name.into(),
            aliases: Vec::new(),
            members: members.to_vec(),
            leader,
            kind: faction_kind(name),
        }
    }

    /// 「李氏 / 黎泾李家」「萧氏 / 萧家」是同一家：按姓根归并，保留成员最多的名字
    #[test]
    fn merges_clan_factions_sharing_surname_root() {
        let clusters = vec![
            cluster_named("李明", Some("李氏")),
            cluster_named("李强", Some("李氏")),
            cluster_named("李伟", Some("李氏")),
            cluster_named("李项平", Some("黎泾李家")),
            cluster_named("田芸", Some("黎泾李家")),
            cluster_named("萧初庭", Some("萧氏")),
            cluster_named("萧归鸾", Some("萧氏")),
            cluster_named("萧元思", Some("萧家")),
            cluster_named("郁让", Some("郁家")),
            cluster_named("郁三", Some("郁家")),
        ];
        let degree = vec![3usize, 2, 2, 2, 1, 3, 2, 2, 2, 1];
        let factions = vec![
            faction_of("李氏", &[0, 1, 2], Some(0)),
            faction_of("黎泾李家", &[3, 4], Some(3)),
            faction_of("萧氏", &[5, 6], Some(5)),
            faction_of("萧家", &[7], Some(5)),
            faction_of("郁家", &[8, 9], Some(8)),
        ];
        let (merged, rename) = merge_factions(factions, &clusters, &degree);

        let names: Vec<&str> = merged.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["李氏", "萧氏", "郁家"], "5 个势力应并成 3 个");

        let li = &merged[0];
        assert_eq!(li.aliases, vec!["黎泾李家"], "被并掉的名字进 aliases");
        assert_eq!(li.members, vec![0, 1, 2, 3, 4], "成员取并集（按度数降序）");
        assert_eq!(li.kind, "家族");
        assert_eq!(li.leader, Some(0));

        let xiao = &merged[1];
        assert_eq!(xiao.aliases, vec!["萧家"]);
        assert_eq!(xiao.members, vec![5, 6, 7]);

        assert_eq!(rename.get("黎泾李家").map(String::as_str), Some("李氏"));
        assert_eq!(rename.get("萧家").map(String::as_str), Some("萧氏"));
    }

    /// 名字对不上、但两批都指认同一个领袖 → 同一家（证据比名字硬）
    #[test]
    fn merges_clan_factions_sharing_leader() {
        let clusters = vec![
            cluster_named("李家", Some("李家")),
            cluster_named("李二", Some("李家")),
            cluster_named("李三", Some("陇西堂")),
            cluster_named("李四", Some("陇西堂")),
        ];
        let degree = vec![2usize, 1, 1, 1];
        let factions = vec![
            faction_of("李家", &[0, 1], Some(0)),
            faction_of("陇西堂", &[2, 3], Some(0)),
        ];
        let (merged, _) = merge_factions(factions, &clusters, &degree);
        assert_eq!(merged.len(), 1, "leader 相同应并成一个");
        assert_eq!(merged[0].name, "李家");
        assert_eq!(merged[0].members, vec![0, 1, 2, 3]);
    }

    /// 地名首字不许当姓根：`黎泾李家` 与 `黎夏萧家` 共用一个「黎」字，
    /// 早先把它们串成一串，最后「萧家」整块并进了「李氏」（生产库实测到的现场）
    #[test]
    fn does_not_chain_clans_through_place_prefix() {
        let clusters = vec![
            cluster_named("李明", Some("李氏")),
            cluster_named("李强", Some("李氏")),
            cluster_named("李项平", Some("黎泾李家")),
            cluster_named("李二", Some("黎泾李家")),
            cluster_named("萧初庭", Some("萧氏")),
            cluster_named("萧归鸾", Some("萧氏")),
            cluster_named("萧元思", Some("黎夏萧家")),
            cluster_named("萧暮云", Some("黎夏萧家")),
        ];
        let degree = vec![3usize, 2, 2, 1, 3, 2, 2, 1];
        let factions = vec![
            faction_of("李氏", &[0, 1], Some(0)),
            faction_of("黎泾李家", &[2, 3], Some(2)),
            faction_of("萧氏", &[4, 5], Some(4)),
            faction_of("黎夏萧家", &[6, 7], Some(4)),
        ];
        let (merged, rename) = merge_factions(factions, &clusters, &degree);
        let names: Vec<&str> = merged.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["李氏", "萧氏"],
            "李家和萧家各并成一块：{names:?}"
        );
        let li = &merged[0];
        assert_eq!(li.members, vec![0, 1, 2, 3]);
        assert_eq!(li.aliases, vec!["黎泾李家"]);
        let xiao = &merged[1];
        assert_eq!(xiao.members, vec![4, 5, 6, 7], "萧家不许被李家吞掉");
        assert_eq!(xiao.aliases, vec!["黎夏萧家"]);
        assert_eq!(rename.get("黎夏萧家").map(String::as_str), Some("萧氏"));
    }

    /// 不同姓的小家族不许乱并（袁家 / 费家）
    #[test]
    fn keeps_unrelated_clans_separate() {
        let clusters = vec![
            cluster_named("袁大", Some("袁家")),
            cluster_named("袁二", Some("袁家")),
            cluster_named("费三", Some("费家")),
            cluster_named("费四", Some("费家")),
        ];
        let degree = vec![1usize, 1, 1, 1];
        let factions = vec![
            faction_of("袁家", &[0, 1], Some(0)),
            faction_of("费家", &[2, 3], Some(2)),
        ];
        let (merged, _) = merge_factions(factions, &clusters, &degree);
        assert_eq!(merged.len(), 2, "袁家 / 费家 是两家，不许并");
    }

    /// 端到端：两批里「李氏」和「黎泾李家」合成一个势力，
    /// 且 memberships / characters.faction 不再指向被删掉的势力（Bug 3 的一致性要求）
    #[test]
    fn merged_factions_keep_memberships_consistent() {
        let text = "李通崖与李项平是兄弟。萧初庭是萧元思的兄长。";
        let batch1 = json!({
            "characters": [
                {"name": "李通崖", "role": "黎泾李家练气修士", "faction": "李氏"},
                {"name": "李项平", "role": "李家修士", "faction": "黎泾李家"},
            ],
            "factions": [{"name": "李氏", "leader": "李通崖"}],
            "relations": [
                {"from": "李通崖", "to": "李项平", "type": "sibling",
                 "quote": "李通崖与李项平是兄弟", "chapter": 1},
            ],
        });
        let batch2 = json!({
            "characters": [
                {"name": "萧初庭", "role": "萧家族长", "faction": "萧氏"},
                {"name": "萧元思", "role": "萧家修士", "faction": "萧家"},
            ],
            "factions": [{"name": "萧氏", "leader": "萧初庭"}, {"name": "萧家", "leader": "萧初庭"}],
            "relations": [
                {"from": "萧初庭", "to": "萧元思", "type": "sibling",
                 "quote": "萧初庭是萧元思的兄长", "chapter": 1},
            ],
        });
        let data = norm(&[batch1, batch2], &chapters(&[text]));
        let factions = data["factions"].as_array().expect("factions");
        let names: Vec<&str> = factions
            .iter()
            .map(|f| f["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(names.len(), 2, "两家不该碎成多块：{names:?}");

        let ids: HashSet<String> = factions
            .iter()
            .map(|f| f["id"].as_str().unwrap_or_default().to_string())
            .collect();
        for m in data["memberships"].as_array().expect("memberships") {
            let fid = m["faction"].as_str().unwrap_or_default();
            assert!(ids.contains(fid), "membership 指向了已删势力 {fid}");
        }
        for c in chars_of(&data) {
            if let Some(fid) = c["faction"].as_str() {
                assert!(ids.contains(fid), "character.faction 指向了已删势力 {fid}");
            }
        }
        // 李家这块要把两批的人并到一起（保留哪个写法看成员数/名字干净程度，
        // 被并掉的那个进 aliases：这里是「黎泾李家练气修士」这种 role 又给「黎泾李家」
        // 投了一票，所以两种写法都可能胜出，关键是它们合成了一块）
        let li = factions
            .iter()
            .find(|f| matches!(f["name"].as_str(), Some("李氏" | "黎泾李家")))
            .unwrap_or_else(|| panic!("应有李家势力：{names:?}"));
        assert_eq!(
            li["members"].as_array().map(Vec::len),
            Some(2),
            "李家应含两批的人：{li}"
        );
        let alias = li["aliases"][0].as_str().unwrap_or_default();
        assert!(
            matches!(alias, "李氏" | "黎泾李家") && Some(alias) != li["name"].as_str(),
            "被并掉的写法应进 aliases：{li}"
        );
    }

    // ---------------------------------------------- 血缘「至今仍在」归一（落盘形状：端点是 c 编号）

    /// 一份「已经躺在 book_graphs 里」的成品：端点用 c 编号，血缘边带结束章，
    /// 另有 spouse 一条（区间要保住）和一条端点缺失的 parent（抽取漏边）。
    fn stored_graph() -> Value {
        json!({
            "meta": { "book": "玄鉴仙族", "totalChapters": 1749 },
            "characters": [
                {"id": "c1", "name": "李木田"},
                {"id": "c2", "name": "李长湖"},
                {"id": "c3", "name": "李项平"},
                {"id": "c4", "name": "田芸"},
                {"id": "c5", "name": "李承福"},
            ],
            "relations": [
                {"id": "r1", "from": "c1", "to": "c2", "type": "parent", "label": "父亲",
                 "from_chapter": 4, "to_chapter": 1711, "quote": "李木田之子李长湖"},
                {"id": "r2", "from": "c1", "to": "c3", "type": "sibling", "label": "兄弟",
                 "from_chapter": 2, "to_chapter": 1711, "quote": "李木田亲弟李项平"},
                {"id": "r3", "from": "c3", "to": "c4", "type": "spouse", "label": "夫妻",
                 "from_chapter": 192, "to_chapter": 1648, "quote": "李项平之妻田芸"},
                {"id": "r4", "from": "c5", "to": "c99", "type": "parent", "label": "父亲",
                 "from_chapter": 300, "to_chapter": null, "quote": "李承福之子李秋阳"},
            ],
        })
    }

    fn rel_by_id<'a>(data: &'a Value, id: &str) -> &'a Value {
        rels_of(data)
            .iter()
            .find(|r| r["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("应有关系 {id}"))
    }

    /// ① parent / sibling 的 to_chapter 归一为 null（血缘不随最后一次提及过期）
    #[test]
    fn kin_edges_become_open_ended() {
        let mut graph = stored_graph();
        let health = normalize_kin_permanence(&mut graph);
        assert!(
            rel_by_id(&graph, "r1")["to_chapter"].is_null(),
            "parent 的收盘章必须抹成 null：{}",
            rel_by_id(&graph, "r1")
        );
        assert!(
            rel_by_id(&graph, "r2")["to_chapter"].is_null(),
            "sibling 的收盘章必须抹成 null：{}",
            rel_by_id(&graph, "r2")
        );
        // 起始章照留（前端靠它判「这条边还没开始」）
        assert_eq!(rel_by_id(&graph, "r1")["from_chapter"], json!(4));
        assert_eq!(health.kin_ended, 2, "两条血缘边被归一");
        assert_eq!(health.kin_samples.len(), 2, "样例按条给，但总数封顶");
    }

    /// ② spouse 保留原区间（夫妻会离/丧偶，是有状态的关系）
    #[test]
    fn spouse_keeps_its_range() {
        let mut graph = stored_graph();
        normalize_kin_permanence(&mut graph);
        let spouse = rel_by_id(&graph, "r3");
        assert_eq!(spouse["from_chapter"], json!(192));
        assert_eq!(
            spouse["to_chapter"],
            json!(1648),
            "spouse 不该被归一：{spouse}"
        );
    }

    /// ③ 归一幂等：跑两遍输出完全一致（第二遍已经没有可改的边）
    #[test]
    fn kin_normalization_is_idempotent() {
        let mut once = stored_graph();
        let mut twice = stored_graph();
        let h1 = normalize_kin_permanence(&mut once);
        normalize_kin_permanence(&mut twice);
        let h2 = normalize_kin_permanence(&mut twice);
        assert_eq!(once, twice, "两遍归一后的 JSON 必须一模一样");
        assert_eq!(h1.kin_ended, 2);
        assert_eq!(h2.kin_ended, 0, "第二遍没东西可归一");
        assert_eq!(h2.dangling, h1.dangling, "体检结论不随次数变化");
    }

    /// ④ 端点缺失要有告警数据（数量 + 前几条样例），且不因归一挂掉
    #[test]
    fn dangling_endpoints_are_reported() {
        let mut graph = stored_graph();
        let health = normalize_kin_permanence(&mut graph);
        assert_eq!(health.relations, 4);
        assert_eq!(health.dangling, 1, "c99 不在 characters 里：{health:?}");
        let sample = health.dangling_samples[0].as_str();
        assert!(
            sample.contains("李承福") && sample.contains("c99") && sample.contains("parent"),
            "样例要能直接看出是谁：{sample:?}"
        );
        // 只报不改：关系还在（本函数不负责补漏边）
        assert_eq!(rels_of(&graph).len(), 4);
    }

    /// ⑤ 已存成品走读取路径后血缘区间被修正（不重跑模型、不写回库）
    #[test]
    fn stored_graph_is_fixed_on_read_path() {
        let stored = serde_json::to_string(&stored_graph()).expect("序列化旧成品");
        let served = serve_stored(serde_json::from_str::<Value>(&stored).expect("解析旧成品"));
        assert!(
            rel_by_id(&served, "r1")["to_chapter"].is_null()
                && rel_by_id(&served, "r2")["to_chapter"].is_null(),
            "读取路径必须把旧成品的血缘边修好"
        );
        // 别的字段一个都不许动
        assert_eq!(rel_by_id(&served, "r3")["to_chapter"], json!(1648));
        assert_eq!(served["meta"]["totalChapters"], json!(1749));
        assert_eq!(chars_of(&served).len(), 5);
        // 同一份旧成品连读两次，输出一致（幂等）
        let again = serve_stored(serde_json::from_str::<Value>(&stored).expect("再解析旧成品"));
        assert_eq!(served, again);
    }

    /// ⑥ 同势力内的孤离血缘小块（生产实例里的李承福 / 李秋阳）要进告警数据
    #[test]
    fn stranded_kin_block_is_reported() {
        let mut chars: Vec<Value> = (0..10)
            .map(|i| json!({"id": format!("m{i}"), "name": format!("主{i}")}))
            .collect();
        let mut rels: Vec<Value> = (1..10)
            .map(|i| {
                json!({"id": format!("e{i}"), "from": format!("m{}", i - 1),
                       "to": format!("m{i}"), "type": "parent", "from_chapter": i})
            })
            .collect();
        // 一对父子自成一小块：两人都在 characters 里（端点不缺），缺的是并回主家的边
        chars.push(json!({"id": "s1", "name": "李承福"}));
        chars.push(json!({"id": "s2", "name": "李秋阳"}));
        rels.push(
            json!({"id": "e99", "from": "s1", "to": "s2", "type": "parent", "from_chapter": 54}),
        );
        let members: Vec<String> = (0..12)
            .map(|i| {
                if i < 10 {
                    format!("m{i}")
                } else {
                    format!("s{}", i - 9)
                }
            })
            .collect();
        let mut graph = json!({
            "meta": {"book": "玄鉴仙族"},
            "characters": chars,
            "relations": rels,
            "factions": [{"id": "f1", "name": "黎泾李家", "members": members}],
        });
        let health = normalize_kin_permanence(&mut graph);
        assert_eq!(health.dangling, 0, "两人都在 characters 里，端点并不缺");
        assert_eq!(health.stranded, 1, "孤离小块没被看见：{health:?}");
        let sample = health.stranded_samples[0].as_str();
        assert!(
            sample.contains("黎泾李家") && sample.contains("李承福") && sample.contains("李秋阳"),
            "样例要能直接指认是谁：{sample:?}"
        );
    }
}
