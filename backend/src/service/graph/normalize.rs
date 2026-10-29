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

/// 一个势力：(名字, 成员下标（按度数降序）, 领袖下标)
type Faction = (String, Vec<usize>, Option<usize>);

/// 人物 → 所属势力下标（可能同时属于多个）
type MemberOf = Vec<Vec<usize>>;

/// 势力投票：人物的 faction 字段 + role 里点名的势力名 + 批次声明的 leader
///
/// 返回投出来的势力，以及「人物 → 势力下标」的反向表。
fn vote_factions(
    batches: &[Value],
    clusters: &[Cluster],
    resolver: &Resolver<'_>,
    degree: &[usize],
) -> (Vec<Faction>, MemberOf) {
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

    let mut chosen: Vec<(String, Vec<usize>)> = members
        .into_iter()
        .filter(|(_, m)| !m.is_empty())
        .map(|(f, m)| (f, m.into_iter().collect()))
        .collect();
    chosen.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    chosen.truncate(MAX_FACTIONS);

    let mut mem_of: Vec<Vec<usize>> = vec![Vec::new(); clusters.len()];
    let mut out = Vec::with_capacity(chosen.len());
    for (fi, (name, mut mem)) in chosen.into_iter().enumerate() {
        mem.sort_by_key(|&i| (Reverse(degree[i]), i));
        for &i in &mem {
            mem_of[i].push(fi);
        }
        let leader = pick_leader(&name, &mem, clusters, declared.get(&name), degree);
        out.push((name, mem, leader));
    }
    (out, mem_of)
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
pub fn normalize(raws: &[Value], chapters: &[Chapter]) -> Value {
    let batches = batch_objects(raws);
    let book = Book::new(chapters);
    let total = total_chapters(chapters);
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

    // 势力投票（裁剪后重建名字表）
    let post_resolver = Resolver::new(&kept_clusters);
    let (factions, mut mem_of) = vote_factions(&batches, &kept_clusters, &post_resolver, &degree);
    let fid: BTreeMap<String, String> = factions
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| (name.clone(), format!("f{}", i + 1)))
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
            match &c.faction {
                Some(f) => format!("{f}属员"),
                None => "正文提及人物".to_string(),
            }
        } else {
            c.role.clone()
        };
        let faction = mem_of
            .get(i)
            .and_then(|fs| fs.first())
            .and_then(|fi| factions.get(*fi))
            .and_then(|(name, _, _)| fid.get(name).cloned());
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
            let Some((name, _, _)) = factions.get(*fi) else {
                continue;
            };
            let Some(faction_id) = fid.get(name) else {
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
    for (fi, (name, members, leader)) in factions.iter().enumerate() {
        out_factions.push(json!({
            "id": format!("f{}", fi + 1),
            "name": name,
            "kind": faction_kind(name),
            "leader": leader.map(|i| cid[i].clone()),
            "members": members.iter().map(|&i| cid[i].clone()).collect::<Vec<_>>(),
        }));
    }

    let (prompt_tokens, completion_tokens) = token_cost(raws);
    tracing::debug!(
        drops = ?drops.0,
        ambiguous = ambiguous.len(),
        "graph normalize 丢弃统计"
    );

    json!({
        "meta": {
            "book": book_name(&batches),
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
    })
}

fn total_chapters(chapters: &[Chapter]) -> i64 {
    let last = chapters.iter().map(|c| c.0).max().unwrap_or(0);
    last.max(chapters.len() as i64)
}

fn book_name(batches: &[Value]) -> String {
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
        let data = normalize(&[raw], &chapters(&["正文里只有无关紧要的几句闲话。"]));
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
        let data = normalize(&[raw], &chapters(&["孙七与周八结盟。"]));
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
        let data = normalize(&[raw], &chapters(&[text]));
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
        let data = normalize(&[raw], &chapters(&[&text]));

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
        let data = normalize(&[], &[]);
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
        let data = normalize(&[raw], &chapters(&["张三麾下李四，皆听号令。"]));
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
}
