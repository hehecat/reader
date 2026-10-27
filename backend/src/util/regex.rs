//! 正则兼容层（GAP 153）：regex 快路径优先，fancy-regex 回退
//!
//! 背景：书源规则正则 / 替换规则 / TXT 目录规则可能使用 lookbehind（`(?<=...)`）等
//! regex crate 不支持的语法。本包装统一两引擎：
//! - 优先 regex 编译（快路径，绝大多数规则走此）；
//! - regex 编译失败（如含 lookbehind）→ 自动升级 fancy-regex 编译执行；
//! - 两引擎均失败 → 返回带双方原因的明确错误（调用方记日志/报错，不再静默吞掉）。
//!
//! 对外 API 与 regex crate 常用子集一致（new/is_match/captures_iter/replace_all +
//! RegexBuilder::multi_line/case_insensitive），便于逐点替换。
//!
//! 正则超时防护（对齐 legacy RegexTimeoutException 的防卡死目标）：
//! - std 引擎为线性时间 DFA/NFA 实现，无灾难性回溯；
//! - fancy-regex 引擎（lookbehind 等扩展语法）使用回溯，默认回溯上限
//!   [`DEFAULT_BACKTRACK_LIMIT`]（100 万次）；超限时执行返回错误，
//!   本包装按“不匹配/跳过”处理，不会卡死请求线程。
//! 注意：Rust 无法像 Android `RegexTimeoutException` 一样在同步执行中按墙钟
//! 中断正则，回溯上限是等价且可移植的防卡死手段。

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{LazyLock, Mutex};

/// P1-5：编译结果缓存（防同一模式反复编译——替换规则/TXT 目录规则/书源规则高频路径）。
/// 上限 500 条：P3-A 起为 LRU 淘汰（原满则整体清空——持续涌入 >500 唯一模式时
/// 会反复重建，退化为无缓存；LRU 只淘汰最久未命中项，热点模式保持命中）。
const REGEX_CACHE_MAX: usize = 500;

/// fancy-regex 回溯上限（默认即 fancy-regex 内置默认；显式化以声明防卡死语义）
pub const DEFAULT_BACKTRACK_LIMIT: usize = 1_000_000;

/// 缓存键：(pattern, multi_line, case_insensitive, backtrack_limit)
type RegexCacheKey = (String, bool, bool, usize);

/// 缓存条目：(最近访问序号, 编译结果)。序号单调递增，淘汰时移除最小者。
/// 命中/插入均刷新序号（访问序 = 真实 LRU 序）。
struct RegexCache {
    entries: HashMap<RegexCacheKey, (u64, Regex)>,
    clock: u64,
    #[cfg(test)]
    hits: HashMap<String, usize>,
}

impl RegexCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            #[cfg(test)]
            hits: HashMap::new(),
        }
    }

    fn get(&mut self, key: &RegexCacheKey) -> Option<Regex> {
        let hit = if let Some((rec, re)) = self.entries.get_mut(key) {
            self.clock += 1; // 命中刷新访问序（LRU）
            *rec = self.clock;
            Some(re.clone())
        } else {
            None
        };
        #[cfg(test)]
        if hit.is_some() {
            *self.hits.entry(key.0.clone()).or_insert(0) += 1;
        }
        hit
    }

    fn put(&mut self, key: RegexCacheKey, re: &Regex) {
        if self.entries.len() >= REGEX_CACHE_MAX && !self.entries.contains_key(&key) {
            // LRU 淘汰：移除访问序号最小（最久未命中）的条目
            if let Some(evict) = self
                .entries
                .iter()
                .min_by_key(|(_, (rec, _))| *rec)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&evict);
            }
        }
        self.clock += 1;
        self.entries.insert(key, (self.clock, re.clone()));
    }

    #[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn hits(&self, pattern: &str) -> usize {
        self.hits.get(pattern).copied().unwrap_or(0)
    }
}

static REGEX_CACHE: LazyLock<Mutex<RegexCache>> = LazyLock::new(|| Mutex::new(RegexCache::new()));

fn cache_get(key: &RegexCacheKey) -> Option<Regex> {
    let mut cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.get(key)
}

fn cache_put(key: RegexCacheKey, re: &Regex) {
    let mut cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.put(key, re);
}

/// 编译后的正则（std 或 fancy 引擎之一）
#[derive(Debug, Clone)]
pub struct Regex {
    inner: Inner,
}

#[derive(Debug, Clone)]
enum Inner {
    Std(regex::Regex),
    Fancy(fancy_regex::Regex),
}

impl Regex {
    /// 编译：regex 优先；失败回退 fancy-regex（lookbehind 等）；均失败 → Err。
    /// P1-5：编译结果缓存（上限 500 条）——命中直接返回克隆（regex::Regex 克隆为
    /// 内部 Arc 引用计数，O(1)），避免重复编译同一模式。
    pub fn new(pattern: &str) -> Result<Self, String> {
        let key = (pattern.to_string(), false, false, DEFAULT_BACKTRACK_LIMIT);
        if let Some(cached) = cache_get(&key) {
            return Ok(cached);
        }
        let compiled = compile(pattern)?;
        cache_put(key, &compiled);
        Ok(compiled)
    }

    /// 是否匹配（fancy 引擎求值出错视为不匹配）
    pub fn is_match(&self, text: &str) -> bool {
        match &self.inner {
            Inner::Std(re) => re.is_match(text),
            Inner::Fancy(re) => re.is_match(text).unwrap_or(false),
        }
    }

    /// 捕获迭代器（fancy 引擎单次求值出错跳过该项）
    pub fn captures_iter<'t>(&'t self, text: &'t str) -> CaptureMatches<'t> {
        match &self.inner {
            Inner::Std(re) => CaptureMatches {
                inner: CaptureMatchesInner::Std(re.captures_iter(text)),
            },
            Inner::Fancy(re) => CaptureMatches {
                inner: CaptureMatchesInner::Fancy(re.captures_iter(text)),
            },
        }
    }

    /// 全部替换（fancy 引擎替换出错 → 原样返回）
    pub fn replace_all<'t>(&'t self, text: &'t str, rep: &str) -> Cow<'t, str> {
        match &self.inner {
            Inner::Std(re) => re.replace_all(text, rep),
            Inner::Fancy(re) => re.try_replacen(text, 0, rep).unwrap_or(Cow::Borrowed(text)),
        }
    }

    /// 仅替换第一个匹配（legado `###` replaceFirst 语义；无匹配 → 原样返回）
    pub fn replace_first<'t>(&'t self, text: &'t str, rep: &str) -> Cow<'t, str> {
        match &self.inner {
            Inner::Std(re) => re.replacen(text, 1, rep),
            Inner::Fancy(re) => re.try_replacen(text, 1, rep).unwrap_or(Cow::Borrowed(text)),
        }
    }
}

/// 单次捕获（统一两引擎的 get(i) 语义）
#[derive(Debug, Clone, Copy)]
pub struct Match<'t> {
    start: usize,
    end: usize,
    text: &'t str,
}

impl<'t> Match<'t> {
    pub fn start(&self) -> usize {
        self.start
    }
    pub fn end(&self) -> usize {
        self.end
    }
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
    pub fn as_str(&self) -> &'t str {
        self.text
    }
}

/// 捕获组集合
#[derive(Debug)]
pub struct Captures<'t> {
    inner: CapturesInner<'t>,
}

#[derive(Debug)]
enum CapturesInner<'t> {
    Std(regex::Captures<'t>),
    Fancy(fancy_regex::Captures<'t, str>),
}

impl<'t> Captures<'t> {
    /// 第 i 组（0 = 全匹配；缺失组 → None）
    pub fn get(&self, i: usize) -> Option<Match<'t>> {
        match &self.inner {
            CapturesInner::Std(c) => c.get(i).map(|m| Match {
                start: m.start(),
                end: m.end(),
                text: m.as_str(),
            }),
            CapturesInner::Fancy(c) => c.get(i).map(|m| Match {
                start: m.start(),
                end: m.end(),
                text: m.as_str(),
            }),
        }
    }
}

/// 捕获迭代器
pub struct CaptureMatches<'t> {
    inner: CaptureMatchesInner<'t>,
}

enum CaptureMatchesInner<'t> {
    Std(regex::CaptureMatches<'t, 't>),
    Fancy(fancy_regex::CaptureMatches<'t, 't, str>),
}

impl<'t> Iterator for CaptureMatches<'t> {
    type Item = Captures<'t>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            CaptureMatchesInner::Std(it) => it.next().map(|c| Captures {
                inner: CapturesInner::Std(c),
            }),
            CaptureMatchesInner::Fancy(it) => it
                .next()
                .and_then(|r| r.ok()) // 单次求值出错跳过（如回溯超限）
                .map(|c| Captures {
                    inner: CapturesInner::Fancy(c),
                }),
        }
    }
}

/// 构建器（multi_line/case_insensitive，与 regex::RegexBuilder 用法一致）
pub struct RegexBuilder<'a> {
    pattern: &'a str,
    multi_line: bool,
    case_insensitive: bool,
    backtrack_limit: usize,
}

impl<'a> RegexBuilder<'a> {
    pub fn new(pattern: &'a str) -> Self {
        Self {
            pattern,
            multi_line: false,
            case_insensitive: false,
            backtrack_limit: DEFAULT_BACKTRACK_LIMIT,
        }
    }

    pub fn multi_line(&mut self, yes: bool) -> &mut Self {
        self.multi_line = yes;
        self
    }

    pub fn case_insensitive(&mut self, yes: bool) -> &mut Self {
        self.case_insensitive = yes;
        self
    }

    /// 设置 fancy-regex 回溯上限（默认 [`DEFAULT_BACKTRACK_LIMIT`]）
    pub fn backtrack_limit(&mut self, limit: usize) -> &mut Self {
        self.backtrack_limit = limit;
        self
    }

    pub fn build(&self) -> Result<Regex, String> {
        let key = (
            self.pattern.to_string(),
            self.multi_line,
            self.case_insensitive,
            self.backtrack_limit,
        );
        if let Some(cached) = cache_get(&key) {
            return Ok(cached);
        }
        let compiled = build_uncached(
            self.pattern,
            self.multi_line,
            self.case_insensitive,
            self.backtrack_limit,
        )?;
        cache_put(key, &compiled);
        Ok(compiled)
    }
}

/// 实际编译（无缓存路径）——regex 优先，fancy-regex 回退
fn compile(pattern: &str) -> Result<Regex, String> {
    match regex::Regex::new(pattern) {
        Ok(re) => Ok(Regex {
            inner: Inner::Std(re),
        }),
        Err(std_err) => match fancy_regex::RegexBuilder::new(pattern)
            .backtrack_limit(DEFAULT_BACKTRACK_LIMIT)
            .build()
        {
            Ok(re) => Ok(Regex {
                inner: Inner::Fancy(re),
            }),
            Err(fancy_err) => Err(format!(
                "正则编译失败: {pattern:?}（regex: {std_err}；fancy-regex: {fancy_err}）"
            )),
        },
    }
}

/// RegexBuilder 实际编译（无缓存路径）
fn build_uncached(
    pattern: &str,
    multi_line: bool,
    case_insensitive: bool,
    backtrack_limit: usize,
) -> Result<Regex, String> {
    let mut sb = regex::RegexBuilder::new(pattern);
    sb.multi_line(multi_line).case_insensitive(case_insensitive);
    match sb.build() {
        Ok(re) => Ok(Regex {
            inner: Inner::Std(re),
        }),
        Err(std_err) => {
            let mut fb = fancy_regex::RegexBuilder::new(pattern);
            fb.multi_line(multi_line).case_insensitive(case_insensitive);
            fb.backtrack_limit(backtrack_limit);
            match fb.build() {
                Ok(re) => Ok(Regex {
                    inner: Inner::Fancy(re),
                }),
                Err(fancy_err) => Err(format!(
                    "正则编译失败: {:?}（regex: {}；fancy-regex: {}）",
                    pattern, std_err, fancy_err
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookbehind_compiles_and_matches() {
        // regex crate 不支持 (?<=...)——wrapper 应自动升级 fancy-regex
        let re = Regex::new(r"(?<=书名[:：])\S+").expect("lookbehind 应可编译");
        assert!(re.is_match("书名：测试书"));
        assert!(!re.is_match("作者：张三"));
        let caps: Vec<String> = re
            .captures_iter("书名：测试书 书名：第二本")
            .filter_map(|c| c.get(0).map(|m| m.as_str().to_string()))
            .collect();
        assert_eq!(caps, vec!["测试书".to_string(), "第二本".to_string()]);
    }

    #[test]
    fn test_std_fast_path_unchanged() {
        let re = Regex::new(r"第(.+?)章").unwrap();
        let caps: Vec<String> = re
            .captures_iter("第一章 第二章")
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        assert_eq!(caps, vec!["一".to_string(), "二".to_string()]);
    }

    #[test]
    fn test_replace_all() {
        let re = Regex::new(r"\s+").unwrap();
        assert_eq!(re.replace_all("a  b   c", " "), "a b c");
        // lookbehind 替换路径
        let re = Regex::new(r"(?<=第)\d+").unwrap();
        assert_eq!(re.replace_all("第1章 第2章", "X"), "第X章 第X章");
    }

    #[test]
    fn test_builder_multi_line() {
        let re = RegexBuilder::new(r"^第.+$")
            .multi_line(true)
            .build()
            .unwrap();
        let caps: Vec<String> = re
            .captures_iter("第一章 内容\n中间\n第二章 内容")
            .filter_map(|c| c.get(0).map(|m| m.as_str().to_string()))
            .collect();
        assert_eq!(
            caps,
            vec!["第一章 内容".to_string(), "第二章 内容".to_string()]
        );
        // lookbehind + multiline 组合
        let re = RegexBuilder::new(r"(?<=^第)\d+")
            .multi_line(true)
            .build()
            .unwrap();
        assert!(re.is_match("第1章\n第2章"));
    }

    #[test]
    fn test_invalid_pattern_returns_clear_error() {
        let err = Regex::new(r"(?<=unclosed").unwrap_err();
        assert!(err.contains("正则编译失败"), "错误信息应明确: {err}");
        assert!(
            err.contains("fancy-regex"),
            "应包含 fancy-regex 原因: {err}"
        );
    }

    /// 正则超时防护：fancy 回溯超限时不 panic、按不匹配/跳过处理（等价防卡死）
    #[test]
    fn test_backtrack_limit_prevents_catastrophic_backtracking() {
        // atomic group `(?>` 强制走 fancy 引擎；回溯上限极小 → 超限而非卡死
        let re = RegexBuilder::new(r"(?i)(a|b|ab)*(?>c)")
            .backtrack_limit(10_000)
            .build()
            .expect("fancy 引擎应可编译");
        let text = "ab".repeat(40);
        // 不 panic、快速返回（is_match 超限时按不匹配处理）
        let _ = re.is_match(&text);
        let caps: Vec<String> = re
            .captures_iter(&text)
            .filter_map(|c| c.get(0).map(|m| m.as_str().to_string()))
            .collect();
        assert!(caps.is_empty(), "回溯超限应跳过捕获而非 panic");
        // 默认上限下同一模式不 panic（1M 次回溯足够）
        let re2 = RegexBuilder::new(r"(?i)(a|b|ab)*(?>c)")
            .build()
            .expect("默认上限可编译");
        let _ = re2.is_match(&text);
    }

    #[test]
    fn test_match_range() {
        let re = Regex::new(r"书").unwrap();
        let caps: Vec<(usize, usize)> = re
            .captures_iter("两本书")
            .filter_map(|c| c.get(0).map(|m| (m.start(), m.end())))
            .collect();
        // UTF-8 字节偏移（两=3B 本=3B 书=3B）
        assert_eq!(caps, vec![(6, 9)]);
    }

    /// P1-5：编译缓存命中——同一模式二次编译走缓存（命中计数 +1，唯一模式无竞争）
    #[test]
    fn test_compile_cache_hit() {
        let mut cache = RegexCache::new();
        let pattern = "p1-5-hit-唯一模式";
        let key = (pattern.to_string(), false, false, DEFAULT_BACKTRACK_LIMIT);
        let re1 = compile(pattern).unwrap();
        assert_eq!(cache.hits(pattern), 0, "首次编译不应命中缓存");
        cache.put(key.clone(), &re1);
        let re2 = cache.get(&key).expect("第二次应命中缓存（不重复编译）");
        assert_eq!(cache.hits(pattern), 1);
        // 缓存克隆功能等价
        assert!(re1.is_match("前缀 p1-5-hit-唯一模式 后缀"));
        assert!(re2.is_match("p1-5-hit-唯一模式"));
        assert!(!re1.is_match("第一章"));
    }

    /// P1-5：builder 缓存键含 flags——同 flags 命中缓存，不同 flags 单独编译
    #[test]
    fn test_compile_cache_builder_flags() {
        let mut cache = RegexCache::new();
        let pattern = r"^p1-5-builder-\d+";
        let r1 = RegexBuilder::new(r"^p1-5-builder-\d+")
            .multi_line(true)
            .build()
            .unwrap();
        let key_ml = (pattern.to_string(), true, false, DEFAULT_BACKTRACK_LIMIT);
        cache.put(key_ml.clone(), &r1);
        let r2 = cache.get(&key_ml).expect("同 flags 第二次应命中");
        assert_eq!(cache.hits(pattern), 1);
        // 不同 flags → 新键，不命中
        let key_plain = (pattern.to_string(), false, false, DEFAULT_BACKTRACK_LIMIT);
        assert!(cache.get(&key_plain).is_none(), "不同 flags 不应命中");
        assert!(r1.is_match("\np1-5-builder-7"));
        assert!(r2.is_match("\np1-5-builder-7"), "同 flags 克隆功能等价");
        let r3 = RegexBuilder::new(r"^p1-5-builder-\d+").build().unwrap();
        assert!(
            !r3.is_match("\np1-5-builder-7"),
            "非 multiline 不匹配行首于换行后"
        );
    }

    /// P1-5：缓存上限 500——超限按 LRU 淘汰最久未命中项，仍可正常编译使用（不 panic、不泄漏）
    #[test]
    fn test_compile_cache_cap() {
        let mut cache = RegexCache::new();
        for i in 0..(REGEX_CACHE_MAX + 50) {
            let pattern = format!(r"pat-{i:04}-\d+");
            let re = compile(&pattern).unwrap();
            cache.put((pattern, false, false, DEFAULT_BACKTRACK_LIMIT), &re);
            assert!(re.is_match(&format!("pat-{i:04}-123")));
        }
        // 淘汰后重新编译仍正常
        let re = compile(r"pat-0000-\d+").unwrap();
        assert!(re.is_match("pat-0000-42"));
        assert!(cache.len() <= REGEX_CACHE_MAX, "LRU 淘汰保证稳态不超上限");
    }

    /// P3-A：LRU 淘汰语义——满后插入新条目淘汰最久未命中项；
    /// 刚命中的热点模式保持缓存命中，被淘汰模式重新编译
    #[test]
    fn test_compile_cache_lru_evicts_least_recent() {
        let mut cache = RegexCache::new();
        // 填满缓存（恰好 MAX 条唯一模式）
        for i in 0..REGEX_CACHE_MAX {
            let pattern = format!(r"p3a-lru-{i:04}-\d+");
            let re = compile(&pattern).unwrap();
            cache.put((pattern, false, false, DEFAULT_BACKTRACK_LIMIT), &re);
        }
        // 命中第一条（刷新为最久未命中项的反面：最新访问）
        let keep_key = (
            "p3a-lru-0000-\\d+".to_string(),
            false,
            false,
            DEFAULT_BACKTRACK_LIMIT,
        );
        let keep = cache.get(&keep_key).expect("0000 应仍在缓存");
        assert!(keep.is_match("p3a-lru-0000-1"));
        assert_eq!(cache.hits("p3a-lru-0000-\\d+"), 1, "touch 命中一次");
        // 再插一条 → 满 → 淘汰最久未命中项（p3a-lru-0001，0000 刚被 touch）
        let extra = compile(r"p3a-lru-extra-\d+").unwrap();
        cache.put(
            (
                "p3a-lru-extra-\\d+".to_string(),
                false,
                false,
                DEFAULT_BACKTRACK_LIMIT,
            ),
            &extra,
        );
        // 被淘汰模式重新编译：不命中缓存（命中计数不涨）
        let evicted_key = (
            "p3a-lru-0001-\\d+".to_string(),
            false,
            false,
            DEFAULT_BACKTRACK_LIMIT,
        );
        assert!(cache.get(&evicted_key).is_none(), "被淘汰项应重新编译");
        let evicted = compile(r"p3a-lru-0001-\d+").unwrap();
        assert!(evicted.is_match("p3a-lru-0001-2"));
        assert_eq!(cache.hits("p3a-lru-0001-\\d+"), 0);
        // 热点模式仍在缓存：再次编译命中
        assert!(cache.get(&keep_key).is_some(), "热点项应继续命中");
        assert_eq!(cache.hits("p3a-lru-0000-\\d+"), 2);
        // 稳态：条数不超上限
        assert!(cache.len() <= REGEX_CACHE_MAX);
    }

    /// P1-5：编译失败不缓存（错误路径不受缓存影响）
    #[test]
    fn test_compile_cache_invalid_not_cached() {
        assert!(Regex::new(r"(?<=unclosed").is_err());
        assert!(Regex::new(r"(?<=unclosed").is_err());
    }
}
