//! legado 规则引擎 v2：规则字符串解析 + CSS/JSONPath/Regex/XPath/JS 执行
//!
//! 规则语法（对齐 legado analyzeRule / AnalyzeByJSoup / AnalyzeByJSonPath / RuleAnalyzer）：
//! - 三段式：`规则体##替换正则##替换串`（## 分隔；第三段 `##` 后跟 `#` → 仅替换首个匹配）
//! - 纯替换规则：`##pat##rep###`（规则体为空 → 直接对输入应用替换）
//! - 类型检测：`{...}` JSONPath / `//` XPath / `@js:`|`js:` JS / `:` 正则（列表规则）/ 其余 CSS 或 Regex
//! - JS 链：`<js>...</js>` / `@js:`（贪婪到末尾）——规则结果进 JS（result 变量），再进后续规则
//! - `{{...}}` 内嵌表达式：`{{$.x}}`/`{{$[n]}}` JSONPath 提取；`{{@rule}}`/`{{//xpath}}`
//!   规则引用（legado isRule）；其余按 JS 执行（注入 result/key/page），结果替换回规则
//! - 组合分隔：`&&` 合并 / `||` 首个命中 / `%%` 按位交错（CSS/JSONPath/XPath 均支持）
//! - JSONPath v2：`$..` 递归下降 / `[?()]` 过滤（@ 属性、比较、&&/||）/ `[-1]` / 切片 / 通配
//! - 结果：字符串列表（legado 返回字符串列表语义）

// GAP 153：正则经 util::regex 兼容层执行（lookbehind 自动升级 fancy-regex）

/// 解析后的规则
#[derive(Debug, Clone)]
pub struct Rule {
    /// 规则类型
    pub kind: RuleKind,
    /// 规则主体（类型检测前的原始文本，## 尾段已剥离）
    pub body: String,
    /// `##@前缀`（legacy 旧格式：结果前缀拼接，可选）
    pub prefix: Option<String>,
    /// `##` 第二段（替换正则，可选）
    pub replace_regex: Option<String>,
    /// `##` 第三段（替换串，可选；无第三段 = 替换为空串）
    pub replacement: Option<String>,
    /// `###` 标志（仅替换首个匹配；无匹配 → 空串，legado replaceFirst）
    pub replace_first: bool,
    /// 无显式类型前缀的裸键规则（legacy Mode.Default）：内容为 JSON 时强制走
    /// JsonPath 提取（legacy AnalyzeRule.SourceRule init 的 `isJSON ||` 分支，
    /// ar.kt:469——真实书源常省略 `$.` 前缀写 `data.list.name`）
    pub default_mode: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleKind {
    Css,
    JsonPath,
    Regex,
    XPath, // v2 支持（sxd-xpath）
    Js,    // v2 支持（boa）
           // P3-A：Url 变体已删除——无检测路径（detect_kind 无分支产生它），
           // 匹配臂与 url_replace 为不可达死代码；legacy @url 规则现落入 Css 分支。
}

/// 书源规则变量（legado `@put`/`@get` 的条目级/书级存储）。
/// F12/AR4：`chapter_title`/`book_name` 为 legacy AnalyzeRule 实体字段回退上下文——
/// `@get:{title}`/`@get:{bookName}` 在变量表未命中时回退（legacy ar.kt:632-645）。
/// 这两个字段不参与 [`save_book_vars`] 持久化，每次分析前由调用方按当前书/章注入。
/// E10/AR5：`chapter_url`/`next_chapter_url` 同为实体字段回退上下文——JS 求值绑定的
/// `chapter.url`/`nextChapterUrl` 来源（legacy AnalyzeRule.setBook/setChapter），
/// 不参与持久化。
#[derive(Debug, Clone, Default)]
pub struct RuleVars {
    map: std::collections::HashMap<String, String>,
    pub chapter_title: Option<String>,
    pub book_name: Option<String>,
    pub chapter_url: Option<String>,
    pub next_chapter_url: Option<String>,
}

/// E10/AR5 保留键：章节/书上下文经 vars 表传给 JS 求值（[`push_js_context`] 写入、
/// `parser::js::install_context_bindings` 检测后展开为 title/chapter/book/nextChapterUrl
/// 类型化全局，并从普通字符串注入中剔除——不以 `__x__` 裸名暴露）
pub(crate) const RK_CHAPTER_TITLE: &str = "__chapter_title__";
pub(crate) const RK_CHAPTER_URL: &str = "__chapter_url__";
pub(crate) const RK_CHAPTER_INDEX: &str = "__chapter_index__";
pub(crate) const RK_NEXT_CHAPTER_URL: &str = "__next_chapter_url__";
pub(crate) const RK_BOOK_NAME: &str = "__book_name__";
pub(crate) const RK_BOOK_AUTHOR: &str = "__book_author__";
pub(crate) const RK_BOOK_URL: &str = "__book_url__";

/// E10/AR5：把 RuleVars 携带的章节/书上下文以保留键写入 JS 求值变量表
/// （legacy AnalyzeRule.kt:650-664 evalJS 注入集的 master 对齐：
/// chapter/title/book/nextChapterUrl/baseUrl/src）。
/// 结构体字段为基础来源；map 中同名保留键优先（调用方可 seed 更完整上下文，
/// 如路由层从目录缓存反查的 `__chapter_index__`/`__next_chapter_url__`）；
/// map 中非空 `baseUrl`/`src` 一并透传（纯规则路径的 JS 不再恒空串）。
pub(crate) fn push_js_context(
    map: &mut std::collections::HashMap<String, String>,
    vars: Option<&RuleVars>,
) {
    let Some(v) = vars else { return };
    let mut put = |k: &str, val: &Option<String>| {
        if let Some(s) = val {
            if !s.is_empty() {
                map.insert(k.to_string(), s.clone());
            }
        }
    };
    put(RK_CHAPTER_TITLE, &v.chapter_title);
    put(RK_CHAPTER_URL, &v.chapter_url);
    put(RK_NEXT_CHAPTER_URL, &v.next_chapter_url);
    put(RK_BOOK_NAME, &v.book_name);
    if let Some(b) = v.get("baseUrl") {
        if !b.is_empty() {
            map.insert("baseUrl".to_string(), b.clone());
        }
    }
    if let Some(s) = v.get("src") {
        if !s.is_empty() {
            map.insert("src".to_string(), s.clone());
        }
    }
    for (k, val) in v.iter() {
        if k.starts_with("__") && k.ends_with("__") && k.len() > 4 && !val.is_empty() {
            map.insert(k.clone(), val.clone());
        }
    }
}

impl RuleVars {
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::ops::Deref for RuleVars {
    type Target = std::collections::HashMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for RuleVars {
    #[allow(clippy::type_complexity)] // 返回 (body, cookies, ua) 这类元组是内部约定，起别名反而要多跳一层
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

/// 书级变量缓存：跨 getBookInfo → getChapterList → getBookContent 请求共享
/// （legado 语义：变量存于 Book 实体，随同一本书的解析流程存活）。
/// 键 = (用户命名空间, 书源 key, 书 URL/目录 URL/章节 URL)——P0 跨用户隔离：
/// 变量可能携带书源登录态/用户专属字段，禁止跨命名空间共享。
///
/// P1 持久化：内存缓存 miss 时读穿透 SQLite（[`BOOK_VARS_STORAGE`]），写入双写落库——
/// 登录态型书源 `@put:{token:...}` 重启后不再批量失效（对齐 EG4 js_cache 模式）。
const BOOK_VARS_CACHE_MAX: usize = 512;
const BOOK_VARS_ENTRIES_MAX: usize = 64;
const BOOK_VARS_BYTES_MAX: usize = 1024 * 1024;
/// 逐章批量缓存的单值上限: 超过则丢弃 (toc 页 src 等整页文档不进逐章缓存)
const BOOK_VARS_BATCH_VALUE_MAX: usize = 4096;

#[allow(clippy::type_complexity)] // 键是 (ns, source, url) 三元组，起别名反而多跳一层
static BOOK_VARS_CACHE: std::sync::RwLock<Vec<((String, String, String), RuleVars)>> =
    std::sync::RwLock::new(Vec::new());

/// @put/@get 变量 SQLite 持久化句柄（serve() 启动时注册；None = 未注册（测试/降级）——仅内存）
static BOOK_VARS_STORAGE: std::sync::LazyLock<std::sync::Mutex<Option<crate::storage::Storage>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[allow(clippy::type_complexity)] // 内部约定的复合类型，起别名反而多跳一层，暂不拆
/// 注册书级变量持久化存储（启动时调用一次）
pub fn register_book_vars_storage(storage: crate::storage::Storage) {
    *BOOK_VARS_STORAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(storage);
}

/// 注销书级变量持久化存储（回到纯内存模式；重启模拟/测试收尾用）
pub fn clear_book_vars_storage() {
    *BOOK_VARS_STORAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn registered_storage() -> Option<crate::storage::Storage> {
    book_vars_registered()
}

fn book_vars_registered() -> Option<crate::storage::Storage> {
    BOOK_VARS_STORAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 清空内存缓存（保留 SQLite 持久层）——重启模拟/测试用
pub fn clear_book_vars_memory_cache() {
    match BOOK_VARS_CACHE.write() {
        Ok(mut g) => g.clear(),
        Err(e) => e.into_inner().clear(),
    }
}

/// 清空指定命名空间的内存缓存（重启模拟；不动其他命名空间——并发测试隔离）
pub fn clear_book_vars_memory_cache_ns(ns: &str) {
    match BOOK_VARS_CACHE.write() {
        Ok(mut g) => g.retain(|(k, _)| k.0 != ns),
        Err(e) => e.into_inner().retain(|(k, _)| k.0 != ns),
    }
}

/// RuleVars map → JSON（仅持久化变量表；章节/书上下文字段不参与——与内存缓存一致）
fn book_vars_to_json(vars: &RuleVars) -> Option<String> {
    serde_json::to_string(&vars.map).ok()
}

/// JSON → 仅 map 的 RuleVars（上下文字段为 None）
fn book_vars_from_json(json: &str) -> Option<RuleVars> {
    let map: std::collections::HashMap<String, String> = serde_json::from_str(json).ok()?;
    let mut vars = RuleVars::new();
    vars.map = map;
    Some(vars)
}

/// SQLite 读穿透（同步阻塞等待；失败按未命中处理）
fn book_vars_db_read(ns: &str, source: &str, url: &str) -> Option<RuleVars> {
    let storage = book_vars_registered()?;
    let (ns, source, url) = (ns.to_string(), source.to_string(), url.to_string());
    let fut = async move { storage.get_book_vars_cache(&ns, &source, &url).await };
    match crate::parser::js::block_on_task(fut, std::time::Duration::from_secs(10), "bookVars.get")
    {
        Ok(Some(json)) => book_vars_from_json(&json),
        Ok(None) => None,
        Err(e) => {
            tracing::debug!("book_vars 读库失败（按未命中处理）: {e}");
            None
        }
    }
}

/// SQLite 落库（同步阻塞等待；失败仅告警不中断——降级为纯内存）
fn book_vars_db_write(ns: &str, source: &str, url: &str, json: &str) {
    if let Some(storage) = book_vars_registered() {
        let (ns, source, url) = (ns.to_string(), source.to_string(), url.to_string());
        let json = json.to_string();
        let fut = async move { storage.put_book_vars_cache(&ns, &source, &url, &json).await };
        // 超时/失败仅降级纯内存；WARN 去抖 60s——并发搜索时锁竞争会集中爆发，逐条刷屏无益
        static LAST_WARN_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        if let Err(e) = crate::parser::js::block_on_task(
            fut,
            std::time::Duration::from_secs(10),
            "bookVars.put",
        ) {
            use std::sync::atomic::Ordering;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default();
            let prev = LAST_WARN_MS.load(Ordering::Relaxed);
            if now - prev >= 60_000
                && LAST_WARN_MS
                    .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                tracing::warn!("book_vars 落库失败（本次仅内存，60s 内同类告警去抖）: {e}");
            }
        }
    }
}

/// 读取书级变量：① 内存命中 → 返回；② 读穿透 SQLite 回填（重启后恢复）；均未命中返回空 map
/// ——P0 按命名空间隔离
pub fn load_book_vars(ns: &str, source: &str, book_url: &str) -> RuleVars {
    let key = (ns.to_string(), source.to_string(), book_url.to_string());
    let mem_hit = match BOOK_VARS_CACHE.read() {
        Ok(g) => g.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()),
        Err(e) => e
            .into_inner()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone()),
    };
    if let Some(v) = mem_hit {
        return v;
    }
    // 读穿透：命中后回填内存热缓存
    if let Some(v) = book_vars_db_read(ns, source, book_url) {
        let mut g = match BOOK_VARS_CACHE.write() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if let Some(slot) = g.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = v.clone();
        } else {
            if g.len() >= BOOK_VARS_CACHE_MAX {
                g.remove(0);
            }
            g.push((key, v.clone()));
        }
        return v;
    }
    RuleVars::default()
}

/// 跨阶段合并读取：`root_key` 级作底、`leaf_key` 级覆盖
/// （与 legacy book→chapter 单 varMap 回退链一致；正文阶段 root=bookUrl、leaf=章节 URL，
/// 目录阶段 root=bookUrl、leaf=tocUrl。空 root_key / 同键时退化为单键读取）
pub fn load_book_vars_merged(ns: &str, source: &str, root_key: &str, leaf_key: &str) -> RuleVars {
    let mut merged = if root_key.is_empty() || root_key == leaf_key {
        RuleVars::default()
    } else {
        load_book_vars(ns, source, root_key)
    };
    let leaf = load_book_vars(ns, source, leaf_key);
    for (k, v) in leaf.iter() {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

/// [`save_book_vars`] 双键版：leaf 键 + root 键各存一份（值相同）——
/// 详情/目录/正文任一阶段写入后，直接取正文/目录时按两级合并都能命中。
pub fn save_book_vars_two_level(
    ns: &str,
    source: &str,
    root_key: &str,
    leaf_key: &str,
    vars: &RuleVars,
) {
    save_book_vars(ns, source, leaf_key, vars);
    if !root_key.is_empty() && root_key != leaf_key {
        save_book_vars(ns, source, root_key, vars);
    }
}

/// 保存书级变量（LRU 上限 + 单书条目/字节上限，超限静默丢弃——与 source.put 上限语义一致；
/// 内存 + SQLite 双写）。P0 按命名空间隔离
pub fn save_book_vars(ns: &str, source: &str, book_url: &str, vars: &RuleVars) {
    let key = (ns.to_string(), source.to_string(), book_url.to_string());
    let mut capped = RuleVars::new();
    let mut bytes = 0usize;
    for (k, v) in vars.iter() {
        if capped.len() >= BOOK_VARS_ENTRIES_MAX {
            break;
        }
        bytes += k.len() + v.len();
        if bytes > BOOK_VARS_BYTES_MAX {
            break;
        }
        capped.insert(k.clone(), v.clone());
    }
    let mut g = match BOOK_VARS_CACHE.write() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some(slot) = g.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = capped.clone();
    } else {
        if g.len() >= BOOK_VARS_CACHE_MAX {
            g.remove(0);
        }
        g.push((key, capped.clone()));
    }
    drop(g);
    // 双写落库（capped 已剔除超限条目；上下文字段不序列化）
    if let Some(json) = book_vars_to_json(&capped) {
        book_vars_db_write(ns, source, book_url, &json);
    }
}

/// 批量保存书级变量 (目录循环按章写入): LRU 逐条 + SQLite 单事务,
/// 避免逐章 block_on_task + 单条往返 (千章级目录首抓从数十秒降到秒级)
pub fn save_book_vars_batch(ns: &str, source: &str, urls: &[String], vars: &RuleVars) {
    if urls.is_empty() {
        return;
    }
    // cap/序列化只做一次: vars 含整页 src (数百 KB), 逐章 clone+序列化+整量写库
    // 是千章级目录首抓的主要开销 (1658 章 ≈ 350MB 无用拷贝)
    let mut capped = RuleVars::new();
    let mut bytes = 0usize;
    for (k, v) in vars.iter() {
        if capped.len() >= BOOK_VARS_ENTRIES_MAX {
            break;
        }
        // 逐章缓存丢弃超大值 (toc 页 src 等): 正文流程会自行重绑 src,
        // 逐章嵌入整页 HTML 曾导致千章目录写库 431MB / 18s
        if v.len() > BOOK_VARS_BATCH_VALUE_MAX {
            continue;
        }
        bytes += k.len() + v.len();
        if bytes > BOOK_VARS_BYTES_MAX {
            break;
        }
        capped.insert(k.clone(), v.clone());
    }
    {
        let mut g = match BOOK_VARS_CACHE.write() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        for url in urls {
            let key = (ns.to_string(), source.to_string(), url.clone());
            if let Some(slot) = g.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = capped.clone();
            } else {
                if g.len() >= BOOK_VARS_CACHE_MAX {
                    g.remove(0);
                }
                g.push((key, capped.clone()));
            }
        }
    }
    if let Some(json) = book_vars_to_json(&capped) {
        if let Some(storage) = book_vars_registered() {
            let (ns, source) = (ns.to_string(), source.to_string());
            let urls = urls.to_vec();
            let fut = async move {
                storage
                    .put_book_vars_cache_batch(&ns, &source, &urls, &json)
                    .await
            };
            let _ = crate::parser::js::block_on_task(
                fut,
                std::time::Duration::from_secs(30),
                "bookVars.putBatch",
            );
        }
    }
}

/// 从规则串中提取并移除 `@put:{...}` 段（legado splitPutRule）：
/// 大小写不敏感；花括号按引号/嵌套平衡匹配（比 legado 的 `[^}]+?` 更容错）。
/// 返回 (清理后的规则, 提取的键值对)。值保留原样，由调用方按当前上下文求值。
fn split_put(rule: &str) -> (String, Vec<(String, String)>) {
    let mut out = String::new();
    let mut puts = Vec::new();
    let mut i = 0;
    while i < rule.len() {
        if rule[i..]
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("@put:"))
        {
            let rest = &rule[i + 5..];
            if let Some(open_rel) = rest.find('{') {
                let open = i + 5 + open_rel;
                if let Some(end) = matching_brace(rule, open) {
                    let body = &rule[open + 1..end];
                    if let Some(map) = parse_put_json(body) {
                        puts.extend(map);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        let ch = rule[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&rule[i..i + ch]);
        i += ch;
    }
    (out, puts)
}

/// 查找 `{`（open 下标）的配对 `}`（跳过引号内字符与嵌套花括号）
fn matching_brace(rule: &str, open: usize) -> Option<usize> {
    let b = rule.as_bytes();
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\'' if !in_d => in_s = !in_s,
            b'"' if !in_s => in_d = !in_d,
            b'{' if !in_s && !in_d => depth += 1,
            b'}' if !in_s && !in_d => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 解析 `@put` 的对象段（键值均为字符串；容忍真实书源的宽松写法：
/// 未加引号的键、单引号字符串、裸规则值，如 `@put:{bid:$.comic_id}`）。
fn parse_put_json(body: &str) -> Option<Vec<(String, String)>> {
    let body = body.trim();
    // 兼容带外层花括号的调用（split_put 传内层，直接测试可能传整段）
    let body = body
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(body);
    let entries = split_put_entries(body)?;
    let mut out = Vec::new();
    for entry in entries {
        let colon = find_put_colon(entry)?;
        let key = unquote_put(entry[..colon].trim());
        let value = unquote_put(entry[colon + 1..].trim());
        if key.is_empty() {
            return None;
        }
        out.push((key, value));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 按顶层逗号切分 `@put` 键值对（跳过引号与花括号嵌套——JSONPath 过滤可含逗号/花括号）
fn split_put_entries(body: &str) -> Option<Vec<&str>> {
    let b = body.as_bytes();
    let mut entries = Vec::new();
    let mut start = 0usize;
    let mut i = 0;
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    while i < b.len() {
        let c = b[i] as char;
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        return None; // 未闭合的括号
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => {
                    let part = body[start..i].trim();
                    if part.is_empty() {
                        return None;
                    }
                    entries.push(part);
                    start = i + 1;
                }
                _ => {}
            }
        }
        if i == b.len() - 1 {
            let part = body[start..].trim();
            if part.is_empty() {
                return None;
            }
            entries.push(part);
        }
        i += 1;
    }
    if in_s || in_d || depth != 0 {
        return None;
    }
    Some(entries)
}

/// 找顶层冒号（键值分隔；跳过引号与花括号）
fn find_put_colon(entry: &str) -> Option<usize> {
    let b = entry.as_bytes();
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ':' if depth == 0 => return Some(i),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 去掉值两侧的单/双引号（保留内部转义原样——规则值按字符串传给求值层）
fn unquote_put(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (open, close) = (bytes[0], bytes[bytes.len() - 1]);
        if (open == b'\'' || open == b'"') && open == close {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// 替换规则中的 `@get:{key}`（legado makeUpRule getRuleType）：从变量表取值，缺失 → 空串；
/// F12/AR4：`title`/`bookName` 变量表未命中时回退实体字段（legacy ar.kt:632-645）
#[allow(clippy::while_let_loop)] // 显式 loop + let-else 便于多处 break，改成 while let 反而难读
pub fn resolve_get(rule: &str, vars: &RuleVars) -> String {
    let mut out = String::new();
    let mut base = 0usize;
    loop {
        let Some(rel) = find_ci(&rule[base..], "@get:") else {
            break;
        };
        let start = base + rel;
        let after = start + 5;
        let Some(end_rel) = rule[after..].find('}') else {
            break;
        };
        let key = rule[after + 1..after + end_rel].trim();
        out.push_str(&rule[base..start]);
        out.push_str(match vars.get(key) {
            Some(v) => v.as_str(),
            // F12/AR4：内建回退——变量表未命中时读当前章标题/书名
            None => match key {
                "title" => vars.chapter_title.as_deref().unwrap_or(""),
                "bookName" => vars.book_name.as_deref().unwrap_or(""),
                _ => "",
            },
        });
        base = after + end_rel + 1;
    }
    out.push_str(&rule[base..]);
    out
}

/// 解析规则字符串（对齐 legado SourceRule + makeUpRule）
/// - `@@` 前缀去掉（默认规则）
/// - `@CSS:`/`@XPath:`/`@Json:`/`@js:`/`js:` 前缀（大小写不敏感，对齐 legado startsWith(ignoreCase)）
/// - `:` 前缀 → 正则规则（legado allInOne：书籍列表/目录列表专用）
/// - 孤立 `@` 前缀剥除（legado RuleAnalyzer.trim——链式规则中 @ 为冗余符号）
/// - `##` 多段：第二段为替换正则（@ 开头 → legacy 前缀）；第三段为替换串；
///   存在第四段（`###`）→ 仅替换首个匹配
pub fn parse_rule(rule: &str) -> Rule {
    // ## 切分需避开 {{...}} 内嵌规则（legado：evalMatcher 先于 makeUpRule 的 ## 切分）
    let parts = split_hashes(rule);
    let raw_main = parts[0].trim();
    // (规则体, 类型, 是否裸键 Default 模式——仅无显式前缀的兜底分支为 true)
    let (main, kind, default_mode) = if let Some(rest) = raw_main.strip_prefix("@@") {
        (rest.trim().to_string(), RuleKind::Css, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@CSS:") {
        (rest.trim().to_string(), RuleKind::Css, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@XPath:") {
        (rest.trim().to_string(), RuleKind::XPath, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@Json:") {
        (rest.trim().to_string(), RuleKind::JsonPath, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "@js:") {
        (rest.trim().to_string(), RuleKind::Js, false)
    } else if let Some(rest) = strip_prefix_ci(raw_main, "js:") {
        (rest.trim().to_string(), RuleKind::Js, false)
    } else if let Some(rest) = raw_main.strip_prefix(':') {
        // legado allInOne：: 开头整条规则为正则
        (rest.trim().to_string(), RuleKind::Regex, false)
    } else {
        // 孤立 @ 前缀剥除（legado RuleAnalyzer.trim：@ 或空白符）
        let cleaned = raw_main.trim_start_matches('@').trim();
        let kind = detect_kind(cleaned);
        // 仅裸键兜底分支视为 legacy Mode.Default（@CSS:/@@ 等显式前缀已在上方拦截）
        let default_mode = kind == RuleKind::Css;
        (cleaned.to_string(), kind, default_mode)
    };

    let mut prefix = None;
    let mut replace_regex = None;
    let mut replacement = None;
    let mut replace_first = false;
    if parts.len() > 1 {
        let tail = parts[1].trim();
        if tail.starts_with('@') {
            // legacy 前缀格式：规则##@前缀（拼接在结果前）
            prefix = Some(parts[1..].join("##"));
        } else {
            replace_regex = Some(tail.to_string());
            if parts.len() > 2 {
                replacement = Some(parts[2].to_string());
            }
            if parts.len() > 3 {
                replace_first = true;
            }
        }
    }

    Rule {
        kind,
        body: main,
        prefix,
        replace_regex,
        replacement,
        replace_first,
        default_mode,
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// 在 `start..` 内寻找花括号（`{{...}}`）之外的 `<js>` / `@js:` 标记
/// （大小写不敏感，同 find_ci 语义；P3-A：{{}} 内嵌模板中的标记是规则引用而非 JS 段）
fn find_js_markers(rule: &str, start: usize) -> (Option<usize>, Option<usize>) {
    let b = rule.as_bytes();
    let mut i = start;
    let mut depth = 0i32;
    let mut js_tag = None;
    let mut js_at = None;
    while i < rule.len() {
        // 只处理字符边界（{{/}}/@js:/<js> 均为 ASCII；多字节字符逐字跳过）
        if !rule.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if b[i] == b'{' && i + 1 < rule.len() && b[i + 1] == b'{' {
            depth += 1;
            i += 2;
            continue;
        }
        if b[i] == b'}' && i + 1 < rule.len() && b[i + 1] == b'}' && depth > 0 {
            depth -= 1;
            i += 2;
            continue;
        }
        if depth == 0 {
            if js_at.is_none()
                && rule[i..]
                    .get(..4)
                    .is_some_and(|s| s.eq_ignore_ascii_case("@js:"))
            {
                js_at = Some(i);
            }
            if js_tag.is_none()
                && rule[i..]
                    .get(..4)
                    .is_some_and(|s| s.eq_ignore_ascii_case("<js>"))
            {
                js_tag = Some(i);
            }
            if js_at.is_some() && js_tag.is_some() {
                break;
            }
        }
        i += rule[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    (js_tag, js_at)
}

/// 按 `##` 切分（避开 `{{...}}` 内嵌规则——其内容可含 `##` 替换链）
fn split_hashes(rule: &str) -> Vec<&str> {
    let b = rule.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            depth += 1;
            i += 2;
            continue;
        }
        if b[i] == b'}' && b[i + 1] == b'}' && depth > 0 {
            depth -= 1;
            i += 2;
            continue;
        }
        if depth == 0 && b[i] == b'#' && b[i + 1] == b'#' {
            parts.push(&rule[start..i]);
            i += 2;
            start = i;
            continue;
        }
        i += 1;
    }
    parts.push(&rule[start..]);
    parts
}

#[allow(clippy::if_same_then_else)] // 不同前缀走向同一 RuleKind 是有意的 legado 语义映射，分开写便于对照
fn detect_kind(body: &str) -> RuleKind {
    let b = body.trim();
    // 对齐 legado SourceRule 类型检测（AnalyzeRule.kt）
    if b.starts_with("@CSS:") {
        RuleKind::Css // @CSS: 显式 CSS
    } else if b.starts_with("@@") {
        RuleKind::Css // @@ 默认规则（去前缀由 parse 处理）
    } else if b.starts_with("@XPath:") {
        RuleKind::XPath
    } else if b.starts_with("@Json:") {
        RuleKind::JsonPath
    } else if b.starts_with("$.") || b.starts_with("$[") || b.starts_with('{') {
        RuleKind::JsonPath // $. / $[ 或 JSON 片段
    } else if b.starts_with(':') {
        RuleKind::Regex // legado allInOne：: 前缀正则规则
    } else if b.starts_with('/') {
        RuleKind::XPath // XPath 特征明显，无需标识头
    } else if b.starts_with("@js:") || b.starts_with("js:") {
        RuleKind::Js
    } else if b.contains("$1") || b.contains("$2") {
        RuleKind::Regex // $N 引用 → 正则
    } else {
        RuleKind::Css
    }
}

/// legado init 规则：先提取上下文（CSS/JSONPath/正则/JS），后续字段规则在其上相对应用。
/// 提取为空 → 返回原上下文（不阻断解析链）。JS 规则注入 result=原文。
pub fn apply_init(context: &str, init: Option<&str>) -> String {
    apply_init_impl(context, init, None)
}

/// [`apply_init`] 带变量版本（@put/@get 贯通同一本书的详情/目录/正文流程）
pub fn apply_init_with_vars(context: &str, init: Option<&str>, vars: &mut RuleVars) -> String {
    apply_init_impl(context, init, Some(vars))
}

fn apply_init_impl(context: &str, init: Option<&str>, vars: Option<&mut RuleVars>) -> String {
    let Some(r) = init else {
        return context.to_string();
    };
    let r = r.trim();
    if r.is_empty() {
        return context.to_string();
    }
    let parsed = parse_rule(r);
    let out = match parsed.kind {
        RuleKind::Js => {
            let mut js_vars = std::collections::HashMap::new();
            js_vars.insert("result".to_string(), context.to_string());
            // E10/AR5：init 段 JS 同样注入章节/书上下文（legacy evalJS 全量绑定）
            push_js_context(&mut js_vars, vars.as_deref());
            crate::parser::js::eval_js(&parsed.body, &js_vars).unwrap_or_default()
        }
        _ => apply_depth(r, context, 0, vars)
            .into_iter()
            .next()
            .unwrap_or_default(),
    };
    if out.is_empty() {
        context.to_string()
    } else {
        out
    }
}

/// 对文档执行规则，返回结果列表（含 <js>/@js: 链）
pub fn apply(rule: &str, html: &str) -> Vec<String> {
    apply_depth(rule, html, 0, None)
}

/// [`apply`] 带变量版本：同一流程内多次 apply 共享 `@put`/`@get` 变量
pub fn apply_with_vars(rule: &str, html: &str, vars: &mut RuleVars) -> Vec<String> {
    apply_depth(rule, html, 0, Some(vars))
}

/// 链式执行（legado splitSourceRule：先按 JS 标记切段，逐段顺序执行、结果管道传递）
fn apply_depth(
    rule: &str,
    html: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> Vec<String> {
    let segs = split_js_chain(rule);
    if segs.len() == 1 && !segs[0].is_js {
        return apply_single(segs[0].text, html, depth, vars);
    }
    let mut result: Option<Vec<String>> = None;
    for seg in segs {
        // AR3：某段结果为空 → 提前终止整链返回空（legado AnalyzeRule.getString：
        // 段空结果为 null，循环内 result?.let 使后续所有段跳过，最终得空串；
        // 原实现以空串续喂下一段，如 class.missing@text@js:result.length 误得 "0"）
        if result.as_ref().is_some_and(|r| r.is_empty()) {
            return vec![];
        }
        let input = result
            .as_ref()
            .map(|r| r.join("\n"))
            .unwrap_or_else(|| html.to_string());
        if seg.is_js {
            // JS 段：{{...}} 先展开（legado makeUpRule 对 JS 规则同样处理），再以 result 执行
            let code = expand_inline_depth_checked(seg.text, &input, depth, vars.as_deref_mut()).0;
            let mut js_vars = std::collections::HashMap::new();
            js_vars.insert("result".to_string(), input);
            js_vars.insert("key".to_string(), String::new());
            js_vars.insert("page".to_string(), "1".to_string());
            js_vars.insert("baseUrl".to_string(), String::new());
            js_vars.insert("urlSearchSeries".to_string(), String::new());
            js_vars.insert("urlSearch".to_string(), String::new());
            js_vars.insert("url".to_string(), String::new());
            // E10/AR5：章节/书上下文绑定（legacy AnalyzeRule evalJS）
            push_js_context(&mut js_vars, vars.as_deref());
            match crate::parser::js::eval_js(&code, &js_vars) {
                Ok(s) => {
                    // 空串结果 → 空列表；下一轮循环检测到空结果即终止整链（AR3）
                    result = Some(if s.is_empty() { vec![] } else { vec![s] });
                }
                Err(_) => return vec![], // legado：JS 失败 result=null → 整链终止为空
            }
        } else {
            result = Some(apply_single(seg.text, &input, depth, vars.as_deref_mut()));
        }
    }
    result.unwrap_or_default()
}

/// 单条规则执行（parse + {{}} 展开 + 类型分发 + 前缀/替换）
fn apply_single(
    rule_str: &str,
    html: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> Vec<String> {
    // legado SourceRule：先分离 @put（移除并求值存入变量），再 makeUpRule 替换 @get
    let (cleaned, puts) = split_put(rule_str);
    if let Some(v) = vars.as_deref_mut() {
        if !puts.is_empty() {
            apply_put_vars(&puts, html, v, depth);
        }
        let cleaned = resolve_get(&cleaned, v);
        let rule = parse_rule(&cleaned);
        return apply_rule_inner(&rule, html, depth, vars);
    }
    let rule = parse_rule(&cleaned);
    apply_rule_inner(&rule, html, depth, None)
}

/// @put 值求值（legado putRule → getString(value)）：对当前上下文按规则取首个结果；
/// JSON 上下文允许裸字段名（如 `@put:{bid:bookId}`——legado isJSON 下按 JSONPath 处理）
fn apply_put_vars(puts: &[(String, String)], html: &str, vars: &mut RuleVars, depth: usize) {
    for (k, v) in puts {
        let val = if v.trim().is_empty() {
            String::new()
        } else {
            let resolved = resolve_get(v, vars);
            let first = if parse_json_value(html).is_ok() {
                json_path_single(&resolved, html).into_iter().next()
            } else {
                apply_depth(&resolved, html, depth + 1, Some(vars))
                    .into_iter()
                    .next()
            };
            first.unwrap_or_default()
        };
        vars.insert(k.clone(), val);
    }
}

fn apply_rule_inner(
    rule: &Rule,
    html: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> Vec<String> {
    // {{...}} 内嵌表达式：先展开，再重新解析执行（类型可能变化，如 {{$.x}} 拼接出 CSS）
    let rule = if depth < 4 && rule.body.contains("{{") {
        let (expanded, unsafe_value) =
            expand_inline_depth_checked(&rule.body, html, depth, vars.as_deref_mut());
        if expanded != rule.body {
            if unsafe_value {
                // P2：模板替换值本身含规则控制标记（## 段切分 / {{ 二次模板 / @js:<js>
                // JS 标记 / @、// 规则前缀）——视为纯文本结果，不再重新解析执行
                // （防数据驱动二次执行：书内容可借 {{$.x}} 注入 @js: 代码被再次 eval）。
                // 原规则的前缀/替换段仍应用。
                return apply_post(vec![expanded], rule);
            }
            // 重建完整规则串（保留 ##前缀/##替换段）后重新解析
            let mut full = expanded.clone();
            if let Some(p) = &rule.prefix {
                full.push_str("##");
                full.push_str(p);
            } else if let Some(re) = &rule.replace_regex {
                full.push_str("##");
                full.push_str(re);
                if let Some(rep) = &rule.replacement {
                    full.push_str("##");
                    full.push_str(rep);
                    if rule.replace_first {
                        full.push_str("##");
                    }
                }
            }
            let r = apply_single(&full, html, depth + 1, vars.as_deref_mut());
            if r.is_empty() && !expanded.trim().is_empty() {
                // legado：含 {{}} 的规则展开后即结果文本（{{}} 使规则进入 Regex 模式 →
                // 规则串本身即结果）。执行无果时返回展开文本（前缀/替换仍应用）
                return apply_post(vec![expanded], rule);
            }
            return r;
        }
        rule
    } else {
        rule
    };
    // 空规则：纯替换规则（##pat##rep###）→ 直接对输入应用替换；否则空结果
    if rule.body.trim().is_empty() {
        if rule.replace_regex.is_some() || rule.prefix.is_some() {
            return apply_post(vec![html.to_string()], rule);
        }
        return vec![];
    }
    // legacy ar.kt:468-471（setContent isJSON）：内容为 JSON 时，无显式前缀的裸键规则段
    // 强制走 JsonPath 提取（Mode.Default → Mode.Json）；@CSS:/@@/@XPath:/@Json:/js:/
    // $. 等有类型特征的规则不受影响
    let results = if rule.default_mode && rule.kind == RuleKind::Css && is_json_text(html) {
        json_path(rule, html)
    } else {
        match rule.kind {
            RuleKind::Css => css_select(rule, html),
            RuleKind::JsonPath => json_path(rule, html),
            RuleKind::Regex => regex_match(&rule.body, html),
            RuleKind::XPath => xpath_select_rules(rule, html),
            RuleKind::Js => {
                // JS 规则：注入 result/key/page/baseUrl 环境 + 章节/书上下文（E10/AR5）
                let mut js_vars = std::collections::HashMap::new();
                js_vars.insert("result".to_string(), html.to_string());
                js_vars.insert("key".to_string(), String::new());
                js_vars.insert("page".to_string(), "1".to_string());
                js_vars.insert("baseUrl".to_string(), String::new());
                js_vars.insert("urlSearchSeries".to_string(), String::new());
                js_vars.insert("urlSearch".to_string(), String::new());
                js_vars.insert("url".to_string(), String::new());
                push_js_context(&mut js_vars, vars.as_deref());
                match crate::parser::js::eval_js(&rule.body, &js_vars) {
                    Ok(s) if !s.is_empty() => vec![s],
                    _ => vec![],
                }
            }
        }
    };
    // 前缀/替换处理（legado：@@/替换在结果上应用）
    apply_post(results, rule)
}

/// 展开规则中的 `{{...}}` 内嵌表达式（legado 模板替换语义）：
/// - `{{$.xxx}}` / `{{$[n]}}`：JSONPath 从当前上下文文本提取（复用 json_path 逻辑）
/// - `{{@rule}}` / `{{//xpath}}`：规则引用（legado isRule：@ 开头或 // 开头）
/// - 其他内容：作为 JS 执行（注入 result=上下文文本 / key / page），结果替换回规则
/// - 提取失败 / JS 报错 / 结果为空 → 替换为空串；未闭合的 `{{` → 原样返回
///
/// 注意：JS 字符串内若含 `}}` 会提前截断（v1 限制，规则 JS 避免字面 `}}`）
#[allow(dead_code)] // TODO(审阅): 当前无调用方，待确认是接线还是删除
fn expand_inline_depth(body: &str, text: &str, depth: usize) -> String {
    expand_inline_depth_checked(body, text, depth, None).0
}

/// 展开 `{{...}}`（返回展开串 + 是否含规则控制值）——见 [`expand_inline_depth`] 语义；
/// 第二个返回值供调用方决定是否安全地重新解析（P2：含控制标记的值不再二次解析）
fn expand_inline_depth_checked(
    body: &str,
    text: &str,
    depth: usize,
    mut vars: Option<&mut RuleVars>,
) -> (String, bool) {
    let mut out = String::new();
    let mut rest = body;
    let mut unsafe_value = false;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = match after.find("}}") {
            Some(e) => e,
            None => return (body.to_string(), false), // 未闭合：不处理
        };
        let expr = after[..end].trim();
        let replaced = if expr.starts_with("$.") || expr.starts_with("$[") {
            inline_json_path(expr, text)
        } else if expr.starts_with('@') || expr.starts_with("//") {
            // legado isRule：@ 开头（@@/@CSS:/@XPath:/@Json:/@js:）或 // → 作为规则递归求值
            apply_depth(expr, text, depth + 1, vars.as_deref_mut()).join("\n")
        } else {
            inline_js(expr, text, vars.as_deref())
        };
        if is_rule_control_value(&replaced) {
            unsafe_value = true;
        }
        out.push_str(&replaced);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    (out, unsafe_value)
}

/// 模板替换值是否含规则控制标记（重新解析会改变语义 / 触发二次执行）：
/// - `##`：段切分（值可拆出新的规则链）；`{{`：二次模板展开
/// - `@js:`/`<js>`（任意位置，大小写不敏感）：JS 代码执行标记
/// - 以 `@` / `//` 开头：规则引用/类型前缀（值被当作规则而非文本）
fn is_rule_control_value(v: &str) -> bool {
    v.contains("##")
        || v.contains("{{")
        || contains_js_marker(v)
        || v.starts_with('@')
        || v.starts_with("//")
}

/// 展开 `{{...}}`（测试便捷入口，无递归深度）
#[cfg(test)]
fn expand_inline(body: &str, text: &str) -> String {
    expand_inline_depth(body, text, 0)
}

/// 内嵌 JSONPath：`{{$.a.b}}` / `{{$[0].c}}` → 从上下文文本提取
/// （多结果以换行拼接；无结果 → 空串）
fn inline_json_path(expr: &str, text: &str) -> String {
    let path = if let Some(p) = expr.strip_prefix('$') {
        p // "$.a" → ".a"；"$..a" → "..a"（保留递归标记）；"$[0]" → "[0]"
    } else if let Some(p) = expr.strip_prefix('.') {
        p
    } else {
        expr
    };
    let mut results = vec![];
    match parse_json_value(text) {
        Ok(v) => walk_json(&v, path, &mut results),
        Err(_) => results = json_from_html(path, text),
    }
    if results.is_empty() {
        String::new()
    } else {
        results.join("\n")
    }
}

/// 内嵌 JS：`{{expr}}` → 执行（注入 result=上下文文本 / key / page），失败 → 空串
/// （pub(crate)：search.rs 字段路径的 `{{js}}` 展开复用同一语义）
pub(crate) fn inline_js(expr: &str, text: &str, vars: Option<&RuleVars>) -> String {
    let mut js_vars = std::collections::HashMap::new();
    js_vars.insert("result".to_string(), text.to_string());
    js_vars.insert("key".to_string(), String::new());
    js_vars.insert("page".to_string(), "1".to_string());
    // E10/AR5：{{}} 内嵌 JS 同样绑定章节/书上下文
    push_js_context(&mut js_vars, vars);
    crate::parser::js::eval_js(expr, &js_vars).unwrap_or_default()
}

/// CSS 选择器执行（legado 链式：<js> 链 + &&/||/%% 组合 + @ 链 + 末段属性）
fn css_select(rule: &Rule, html: &str) -> Vec<String> {
    crate::parser::css_chain::css_chain(&rule.body, html)
}

/// XPath 执行（legado AnalyzeByXPath：&&/||/%% 组合）
fn xpath_select_rules(rule: &Rule, html: &str) -> Vec<String> {
    let (sep, subs) = split_combined(&rule.body);
    let mut groups: Vec<Vec<String>> = Vec::new();
    for sub in subs {
        let r = crate::parser::xpath::xpath_select(sub.trim(), html);
        if !r.is_empty() {
            groups.push(r);
            if sep == Some("||") {
                break;
            }
        }
    }
    merge_groups(sep, groups)
}

/// Regex 执行（legado：规则整体当正则，提取 group 1 或全匹配）
/// GAP 153：经 fancy-regex 兼容层编译（支持 lookbehind）；编译失败记日志并返回空
/// E16/EG1：正则匹配（legacy AnalyzeByRegex 对齐）
/// - `&&` 多链：逐条顺序过滤（每条 find_iter 全匹配拼接作为下一条输入）
/// - 末级：captures() 取所有非空捕获组 join("\n") 返回（不只 group(1)||group(0)）
fn regex_match(pattern: &str, text: &str) -> Vec<String> {
    // EG1：&& 多链——legacy AnalyzeByRegex.kt:31-116 按链切分顺序过滤
    if pattern.contains("&&") {
        let mut current = text.to_string();
        for reg in pattern.split("&&") {
            let reg = reg.trim();
            if reg.is_empty() {
                continue;
            }
            match crate::util::regex::Regex::new(reg) {
                Ok(re) => {
                    let mut buf = String::new();
                    for caps in re.captures_iter(&current) {
                        if let Some(m) = caps.get(0) {
                            buf.push_str(m.as_str());
                        }
                    }
                    current = buf;
                }
                Err(e) => {
                    tracing::warn!("正则多链段编译失败（跳过该条）: {e}");
                }
            }
        }
        return if current.is_empty() {
            vec![]
        } else {
            vec![current]
        };
    }

    let re = match crate::util::regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("正则规则编译失败（规则引擎返回空）: {e}");
            return vec![];
        }
    };
    re.captures_iter(text)
        .map(|caps| {
            // EG1：全捕获组提取（group(0)..group(n)），非只 group(1)||group(0)
            let mut parts: Vec<String> = Vec::new();
            let mut gi = 0usize;
            while caps.get(gi).is_some() {
                if let Some(m) = caps.get(gi) {
                    let v = m.as_str().trim().to_string();
                    if !v.is_empty() {
                        parts.push(v);
                    }
                }
                gi += 1;
            }
            if parts.is_empty() {
                String::new()
            } else {
                parts.join("\n")
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// JSONPath 执行（对齐 legado AnalyzeByJSonPath：&&/||/%% 组合 + 完整路径语法）
fn json_path(rule: &Rule, text: &str) -> Vec<String> {
    let (sep, subs) = split_combined(&rule.body);
    let mut groups: Vec<Vec<String>> = Vec::new();
    for sub in subs {
        let r = json_path_single(sub, text);
        if !r.is_empty() {
            groups.push(r);
            if sep == Some("||") {
                break;
            }
        }
    }
    merge_groups(sep, groups)
}

fn merge_groups(sep: Option<&'static str>, groups: Vec<Vec<String>>) -> Vec<String> {
    match sep {
        // legado %%：按 results[0] 的行号交错取各组同位置元素
        Some("%%") => {
            let mut out = Vec::new();
            if let Some(first) = groups.first() {
                for i in 0..first.len() {
                    for g in &groups {
                        if i < g.len() {
                            out.push(g[i].clone());
                        }
                    }
                }
            }
            out
        }
        _ => groups.into_iter().flatten().collect(),
    }
}

/// 单条 JSONPath（输入可能是 JSON 文本或 HTML 中的 JSON 片段）。
/// E14（legacy AnalyzeByJSonPath.kt:38,84 innerRule）：规则中部可内嵌多个 `{$.x}`
/// 子引用——先全部求值替换再拼串（`{$.a.t}-{$.b.t}` → "V1-V2"）；整体恰为一个
/// 内嵌段时走原单路径语义。
fn json_path_single(body: &str, text: &str) -> Vec<String> {
    let trimmed = body.trim();
    // 收集顶层内嵌段（{ 开头、配对 } 结尾、内容形如 $.x / .x / $[i]）
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let bytes = trimmed.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel) = trimmed[i + 1..].find('}') {
                let end = i + 1 + rel + 1;
                let inner = &trimmed[i + 1..end - 1];
                if inner.starts_with("$.") || inner.starts_with("$[") || inner.starts_with('.') {
                    spans.push((i, end));
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    let whole_span =
        spans.len() == 1 && spans[0].0 == 0 && spans[0].1 == trimmed.len() && !spans.is_empty();
    if spans.is_empty() || whole_span {
        return json_path_single_inner(trimmed, text);
    }
    // 模板模式：字面文本与子路径求值结果交替拼接（多值以 \n 连接）
    let mut out = String::new();
    let mut cursor = 0usize;
    for (s, e) in spans {
        out.push_str(&trimmed[cursor..s]);
        let sub_body = format!("{{{}}}", &trimmed[s + 1..e - 1]);
        out.push_str(&json_path_single(&sub_body, text).join("\n"));
        cursor = e;
    }
    out.push_str(&trimmed[cursor..]);
    vec![out]
}

/// 原 json_path_single 主体（单一路径读取）
fn json_path_single_inner(body: &str, text: &str) -> Vec<String> {
    // 提取 body 内路径：{$.list.xxx} 或 {.list.xxx}
    let inner = body.trim().trim_start_matches('{').trim_end_matches('}');
    let json: serde_json::Value = match parse_json_value(text) {
        Ok(v) => v,
        Err(_) => {
            // HTML 中可能内嵌 JSON（如 <script>），尝试按行提取
            return json_from_html(inner, text);
        }
    };
    let path = if let Some(p) = inner.strip_prefix('$') {
        p // "$.a" → ".a"；"$..a" → "..a"（保留递归标记）；"$[0]" → "[0]"
    } else if let Some(p) = inner.strip_prefix('.') {
        p
    } else {
        inner
    };
    let mut results = vec![];
    walk_json(&json, path, &mut results);
    results
}

/// 在 HTML 中查找形如 `{"...` 的 JSON 片段尝试解析
fn json_from_html(path: &str, html: &str) -> Vec<String> {
    let mut results = vec![];
    // 简单策略：按行找包含 { 的片段
    for line in html.lines() {
        let line = line.trim();
        if line.starts_with('{') && line.ends_with('}') {
            if let Ok(v) = parse_json_value(line) {
                let mut r = vec![];
                walk_json(&v, path, &mut r);
                results.extend(r);
            }
        }
    }
    results
}

// ---------- JSONPath 路径引擎 ----------

/// JSONPath 求值递归深度上限（段数 + 值嵌套 + 过滤括号嵌套共用）：
/// 恶意/病态规则（超深路径、超深嵌套括号、超深数组）超限时按“规则错误”处理
/// （记日志、返回空结果），绝不递归到栈溢出 abort。
const JSONPATH_MAX_DEPTH: usize = 64;

/// 深度超限错误（内部传播；顶层 walk_json 转日志 + 空结果）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JsonPathDepthExceeded;

/// serde_json 解析：经 serde_stacker 把 serde 递归移到堆上（解析机制本身不栈溢出）；
/// serde_json 内置 128 层递归上限保留——超限返回解析错误（上层回退 json_from_html/空结果），
/// 不会 abort；求值侧另有 JSONPATH_MAX_DEPTH=64 兜底（Value 深度超限的 drop 递归风险也因此排除）。
fn parse_json_value(text: &str) -> serde_json::Result<serde_json::Value> {
    let mut inner = serde_json::Deserializer::from_str(text);
    let de = serde_stacker::Deserializer::new(&mut inner);
    <serde_json::Value as serde::Deserialize>::deserialize(de)
}

/// 内容是否为 JSON（legacy AnalyzeRule.setContent 的 isJSON 判定，
/// StringExtensions.kt:16 首字符 `{`/`[` + 尾字符配对；此处再经整体解析确认，
/// 比纯首尾括号检查更严——非 JSON 花括号文本不误触发裸键强制 JsonPath）。
fn is_json_text(text: &str) -> bool {
    let t = text.trim();
    let bracketed =
        (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'));
    bracketed && parse_json_value(t).is_ok()
}

#[derive(Debug, Clone, PartialEq)]
enum JSeg {
    Key(String),
    RecKey(String),
    Wildcard,
    Index(i64),
    Slice(Option<i64>, Option<i64>, i64),
    Multi(Vec<JItem>),
    Filter(String),
    QuotedKey(String),
}

#[derive(Debug, Clone, PartialEq)]
enum JItem {
    I(i64),
    S(Option<i64>, Option<i64>, i64),
}

/// 简化 JSONPath 遍历（支持 .a.b / [0] / [-1] / [*] / $..递归 / [?()] 过滤 / 切片）
fn walk_json(value: &serde_json::Value, path: &str, out: &mut Vec<String>) {
    let segs = tokenize_json_path(path);
    let mut found: Vec<&serde_json::Value> = Vec::new();
    if let Err(JsonPathDepthExceeded) = eval_segments(value, &segs, 0, &mut found) {
        tracing::warn!(
            "JSONPath 求值深度超限（>{JSONPATH_MAX_DEPTH}），按空结果处理（路径: {path}）"
        );
        return;
    }
    for v in found {
        push_json_value(v, out);
    }
}

fn push_json_value(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                push_json_value(item, out);
            }
        }
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Null => {}
        other => out.push(other.to_string()), // 数字/布尔/对象（JSON 序列化）
    }
}

fn tokenize_json_path(path: &str) -> Vec<JSeg> {
    let b = path.as_bytes();
    let mut segs = Vec::new();
    let mut i = 0;
    if b.first() == Some(&b'$') {
        i = 1;
    }
    while i < b.len() {
        match b[i] {
            b'.' => {
                if i + 1 < b.len() && b[i + 1] == b'.' {
                    // 递归下降 ..name
                    i += 2;
                    let (name, ni) = read_name(path, i);
                    segs.push(JSeg::RecKey(name));
                    i = ni;
                } else if i + 1 < b.len() && b[i + 1] == b'*' {
                    segs.push(JSeg::Wildcard);
                    i += 2;
                } else {
                    i += 1;
                    let (name, ni) = read_name(path, i);
                    if !name.is_empty() {
                        segs.push(JSeg::Key(name));
                    }
                    i = ni;
                }
            }
            b'[' => {
                let (content, ni) = read_bracket(path, i);
                segs.push(parse_bracket(&content));
                i = ni;
            }
            b'*' => {
                segs.push(JSeg::Wildcard);
                i += 1;
            }
            _ => {
                let (name, ni) = read_name(path, i);
                segs.push(JSeg::Key(name));
                i = ni;
            }
        }
    }
    segs
}

fn read_name(path: &str, mut i: usize) -> (String, usize) {
    let b = path.as_bytes();
    let start = i;
    while i < b.len() && b[i] != b'.' && b[i] != b'[' {
        i += 1;
    }
    (path[start..i].to_string(), i)
}

/// 读取 `[...]` 内容（引号与括号平衡）
fn read_bracket(path: &str, start: usize) -> (String, usize) {
    let b = path.as_bytes();
    let mut i = start + 1;
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                b'[' | b'(' => depth += 1,
                b']' => {
                    if depth == 0 {
                        return (path[start + 1..i].to_string(), i + 1);
                    }
                    depth -= 1;
                }
                b')' => depth -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    (path[start + 1..].to_string(), b.len())
}

fn parse_bracket(content: &str) -> JSeg {
    let c = content.trim();
    if let Some(rest) = c.strip_prefix("?(") {
        if let Some(expr) = rest.strip_suffix(')') {
            return JSeg::Filter(expr.to_string());
        }
    }
    if c.starts_with('\'') || c.starts_with('"') {
        let q = c.chars().next().unwrap();
        if c.len() >= 2 && c.ends_with(q) {
            return JSeg::QuotedKey(c[1..c.len() - 1].to_string());
        }
    }
    if c == "*" {
        return JSeg::Wildcard;
    }
    let items: Vec<&str> = split_top_level(c, ",");
    if items.is_empty() {
        return JSeg::Key(String::new());
    }
    let parsed: Vec<JItem> = items
        .iter()
        .map(|it| {
            let it = it.trim();
            if it.contains(':') {
                let parts: Vec<&str> = it.split(':').collect();
                JItem::S(
                    parts
                        .first()
                        .copied()
                        .filter(|s| !s.is_empty())
                        .and_then(|x| x.trim().parse::<i64>().ok()),
                    parts
                        .get(1)
                        .copied()
                        .filter(|s| !s.is_empty())
                        .and_then(|x| x.trim().parse::<i64>().ok()),
                    parts
                        .get(2)
                        .copied()
                        .filter(|s| !s.is_empty())
                        .and_then(|x| x.trim().parse::<i64>().ok())
                        .unwrap_or(1),
                )
            } else {
                JItem::I(it.parse::<i64>().unwrap_or(0))
            }
        })
        .collect();
    if parsed.len() == 1 {
        match parsed.into_iter().next().unwrap() {
            JItem::I(n) => JSeg::Index(n),
            JItem::S(s, e, st) => JSeg::Slice(s, e, st),
        }
    } else {
        JSeg::Multi(parsed)
    }
}

fn eval_segments<'v>(
    value: &'v serde_json::Value,
    segs: &[JSeg],
    depth: usize,
    out: &mut Vec<&'v serde_json::Value>,
) -> Result<(), JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    if segs.is_empty() {
        out.push(value);
        return Ok(());
    }
    match &segs[0] {
        JSeg::Key(name) | JSeg::QuotedKey(name) => match value {
            serde_json::Value::Object(map) => {
                if let Some(v) = map.get(name) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            serde_json::Value::Array(arr) => {
                // 数组自动展开（对齐 v1 行为）
                for item in arr {
                    eval_segments(item, segs, depth + 1, out)?;
                }
            }
            _ => {}
        },
        JSeg::RecKey(name) => {
            let mut found: Vec<&'v serde_json::Value> = Vec::new();
            collect_rec(value, name, depth + 1, &mut found)?;
            for v in found {
                eval_segments(v, &segs[1..], depth + 1, out)?;
            }
        }
        JSeg::Wildcard => match value {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    eval_segments(item, &segs[1..], depth + 1, out)?;
                }
            }
            serde_json::Value::Object(map) => {
                for v in map.values() {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            _ => {}
        },
        JSeg::Index(n) => match value {
            serde_json::Value::Array(arr) => {
                if let Some(v) = norm_index(*n, arr.len()).and_then(|idx| arr.get(idx)) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(v) = map.get(&n.to_string()) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
            _ => {}
        },
        JSeg::Slice(s, e, st) => {
            if let serde_json::Value::Array(arr) = value {
                for v in slice_items(arr, *s, *e, *st) {
                    eval_segments(v, &segs[1..], depth + 1, out)?;
                }
            }
        }
        JSeg::Multi(items) => {
            if let serde_json::Value::Array(arr) = value {
                for it in items {
                    match it {
                        JItem::I(n) => {
                            if let Some(v) = norm_index(*n, arr.len()).and_then(|idx| arr.get(idx))
                            {
                                eval_segments(v, &segs[1..], depth + 1, out)?;
                            }
                        }
                        JItem::S(s, e, st) => {
                            for v in slice_items(arr, *s, *e, *st) {
                                eval_segments(v, &segs[1..], depth + 1, out)?;
                            }
                        }
                    }
                }
            }
        }
        JSeg::Filter(expr) => match value {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    if eval_filter(expr, item, depth + 1)? {
                        eval_segments(item, &segs[1..], depth + 1, out)?;
                    }
                }
            }
            other => {
                if eval_filter(expr, other, depth + 1)? {
                    eval_segments(other, &segs[1..], depth + 1, out)?;
                }
            }
        },
    }
    Ok(())
}

fn norm_index(n: i64, len: usize) -> Option<usize> {
    let len_i = len as i64;
    let idx = if n < 0 { n + len_i } else { n };
    if idx >= 0 && idx < len_i {
        Some(idx as usize)
    } else {
        None
    }
}

/// $..name：任意深度收集键 name 的值（深度受限，防栈溢出 abort）
fn collect_rec<'v>(
    value: &'v serde_json::Value,
    name: &str,
    depth: usize,
    out: &mut Vec<&'v serde_json::Value>,
) -> Result<(), JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    match value {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get(name) {
                out.push(v);
            }
            for v in map.values() {
                collect_rec(v, name, depth + 1, out)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_rec(v, name, depth + 1, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Python 风格切片（end 排除；负数回绕；step 可为负反向）
fn slice_items(
    arr: &[serde_json::Value],
    s: Option<i64>,
    e: Option<i64>,
    st: i64,
) -> Vec<&serde_json::Value> {
    let len = arr.len() as i64;
    let step = if st == 0 { 1 } else { st };
    let mut start = s.unwrap_or(if step > 0 { 0 } else { len - 1 });
    if start < 0 {
        start += len;
    }
    let mut end = e.unwrap_or(if step > 0 { len } else { -(len + 1) });
    if end < 0 {
        end += len;
    }
    let mut out = Vec::new();
    if step > 0 {
        let mut i = start.max(0);
        while i < end.min(len) {
            out.push(&arr[i as usize]);
            i += step;
        }
    } else {
        let mut i = start.min(len - 1);
        while i > end && i >= 0 {
            out.push(&arr[i as usize]);
            i += step;
        }
    }
    out
}

// ---------- [?()] 过滤表达式 ----------

fn eval_filter(
    expr: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    for part in split_top_level(expr, "||") {
        if eval_and(part, item, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn eval_and(
    expr: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    for part in split_top_level(expr, "&&") {
        if !eval_primary(part, item, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn eval_primary(
    expr: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    if depth > JSONPATH_MAX_DEPTH {
        return Err(JsonPathDepthExceeded);
    }
    let s = expr.trim();
    if s.is_empty() {
        return Ok(false);
    }
    if let Some(rest) = s.strip_prefix('(') {
        if let Some(inner) = rest.strip_suffix(')') {
            return eval_filter(inner, item, depth + 1);
        }
    }
    if let Some(rest) = s.strip_prefix('!') {
        return Ok(!eval_primary(rest, item, depth + 1)?);
    }
    for op in ["==", "!=", "<=", ">=", "<", ">"] {
        if let Some(pos) = find_op_top(s, op) {
            let lhs = s[..pos].trim();
            let rhs = s[pos + op.len()..].trim();
            let lv = eval_filter_path(lhs, item, depth + 1)?;
            let rv = parse_filter_literal(rhs);
            return Ok(compare_filter(lv, rv.as_ref(), op));
        }
    }
    // 高频操作符（=~ / in / nin / size）：词边界匹配，避免误伤路径键名
    if let Some((op, pos)) = find_word_op_top(s) {
        let lhs = s[..pos].trim();
        let rhs = s[pos + op.len()..].trim();
        return eval_word_op(op, lhs, rhs, item, depth + 1);
    }
    // 无比较：裸存在性（legacy：键存在即匹配——值为 null/false/"" 也返回 true）
    Ok(eval_filter_path(s, item, depth + 1)?.is_some())
}

/// 过滤内路径求值：@ / @.a.b / @['a']（取首个结果）
fn eval_filter_path<'v>(
    path: &str,
    item: &'v serde_json::Value,
    depth: usize,
) -> Result<Option<&'v serde_json::Value>, JsonPathDepthExceeded> {
    let p = path.trim();
    if p == "@" {
        return Ok(Some(item));
    }
    let p = p.strip_prefix('@').unwrap_or(p);
    let segs = tokenize_json_path(p);
    let mut found: Vec<&'v serde_json::Value> = Vec::new();
    eval_segments(item, &segs, depth, &mut found)?;
    Ok(found.into_iter().next())
}

fn parse_filter_literal(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        || (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
    {
        return Some(serde_json::Value::String(s[1..s.len() - 1].to_string()));
    }
    match s {
        "true" => return Some(serde_json::Value::Bool(true)),
        "false" => return Some(serde_json::Value::Bool(false)),
        "null" => return Some(serde_json::Value::Null),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(serde_json::json!(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(serde_json::json!(f));
    }
    Some(serde_json::Value::String(s.to_string()))
}

fn compare_filter(
    lv: Option<&serde_json::Value>,
    rv: Option<&serde_json::Value>,
    op: &str,
) -> bool {
    match op {
        // legacy NotEqualsEvaluator 即 !EQ：路径缺失（取不到值）→ true
        "!=" => match (lv, rv) {
            (Some(l), Some(r)) => !json_eq(l, r),
            _ => true,
        },
        _ => {
            let (Some(l), Some(r)) = (lv, rv) else {
                return false; // 其余操作符缺失值比较恒 false
            };
            match op {
                "==" => json_eq(l, r),
                "<" | "<=" | ">" | ">=" => match (l.as_f64(), r.as_f64()) {
                    (Some(a), Some(b)) => cmp_num(a, b, op),
                    _ => match (l.as_str(), r.as_str()) {
                        (Some(a), Some(b)) => cmp_str(a, b, op),
                        _ => false,
                    },
                },
                _ => false,
            }
        }
    }
}

fn json_eq(l: &serde_json::Value, r: &serde_json::Value) -> bool {
    match (l, r) {
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            a.as_f64().unwrap_or(0.0) == b.as_f64().unwrap_or(0.0)
        }
        // legacy NumberNode.equals 接受 StringNode → 数值与数字字符串松散相等
        (serde_json::Value::Number(a), serde_json::Value::String(b))
        | (serde_json::Value::String(b), serde_json::Value::Number(a)) => {
            match (a.as_f64(), b.parse::<f64>()) {
                (Some(x), Ok(y)) => x == y,
                _ => false,
            }
        }
        (serde_json::Value::String(a), serde_json::Value::String(b)) => a == b,
        (serde_json::Value::Bool(a), serde_json::Value::Bool(b)) => a == b,
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        _ => false,
    }
}

fn cmp_num(a: f64, b: f64, op: &str) -> bool {
    match op {
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => false,
    }
}

fn cmp_str(a: &str, b: &str, op: &str) -> bool {
    match op {
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => false,
    }
}

/// 顶层切分（引号/括号内不切）
fn split_top_level<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                _ if depth == 0 && s.is_char_boundary(i) && s[i..].starts_with(sep) => {
                    parts.push(&s[start..i]);
                    i += sep.len();
                    start = i;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

fn find_op_top(s: &str, op: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut in_s = false;
    let mut in_d = false;
    while i + op.len() <= b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d && s.is_char_boundary(i) && s[i..].starts_with(op) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 词操作符查找（=~ / in / nin / size）：顶层、引号外，且两侧须为空白/端点——
/// 防止误伤含这些词的路径键名（如 @.min、@.size）
fn find_word_op_top(s: &str) -> Option<(&'static str, usize)> {
    let b = s.as_bytes();
    let mut best: Option<(&'static str, usize)> = None;
    let mut in_s = false;
    let mut in_d = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
        } else if c == b'"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d && s.is_char_boundary(i) {
            for op in ["=~", "nin", "size", "in"] {
                if s[i..].starts_with(op)
                    && (i == 0 || (b[i - 1] as char).is_whitespace())
                    && (i + op.len() == b.len() || (b[i + op.len()] as char).is_whitespace())
                {
                    // 首字符互异无前缀冲突；取最早出现者
                    if best.is_none_or(|(_, p)| i < p) {
                        best = Some((op, i));
                    }
                    break;
                }
            }
        }
        i += 1;
    }
    best
}

/// 高频操作符求值：
/// - `=~`：正则匹配（右值 /pattern/ 形式）
/// - `in`/`nin`：左值（不）在右值数组中
/// - `size`：数组/字符串长度等于右值
fn eval_word_op(
    op: &str,
    lhs: &str,
    rhs: &str,
    item: &serde_json::Value,
    depth: usize,
) -> Result<bool, JsonPathDepthExceeded> {
    let lv = eval_filter_path(lhs, item, depth)?;
    match op {
        "=~" => {
            let Some(v) = lv else { return Ok(false) };
            let text = match v {
                serde_json::Value::String(t) => t.clone(),
                other => other.to_string(),
            };
            let pat = rhs
                .strip_prefix('/')
                .and_then(|p| p.strip_suffix('/'))
                .unwrap_or(rhs);
            match crate::util::regex::Regex::new(pat) {
                Ok(re) => Ok(re.is_match(&text)),
                Err(e) => {
                    tracing::warn!("过滤表达式 =~ 正则编译失败（{pat}）：{e}");
                    Ok(false)
                }
            }
        }
        "in" => Ok(lv.is_some_and(|v| parse_literal_array(rhs).iter().any(|e| json_eq(v, e)))),
        "nin" => Ok(!lv.is_some_and(|v| parse_literal_array(rhs).iter().any(|e| json_eq(v, e)))),
        "size" => {
            let Some(v) = lv else { return Ok(false) };
            let n = match v {
                serde_json::Value::Array(a) => a.len(),
                serde_json::Value::String(t) => t.chars().count(),
                _ => return Ok(false),
            };
            let rv = parse_filter_literal(rhs);
            Ok(rv
                .as_ref()
                .map(|r| json_eq(&serde_json::json!(n), r))
                .unwrap_or(false))
        }
        _ => Ok(false),
    }
}

/// 解析右值数组字面量（[1,2,3] / ['a','b'] / ('a','b')），非数组 → 空
fn parse_literal_array(s: &str) -> Vec<serde_json::Value> {
    let t = s.trim();
    let t = match (t.starts_with('('), t.ends_with(')')) {
        (true, true) => t[1..t.len() - 1].trim(),
        _ => t,
    };
    if !(t.starts_with('[') && t.ends_with(']')) {
        return Vec::new();
    }
    split_top_level(&t[1..t.len() - 1], ",")
        .iter()
        .filter_map(|p| parse_filter_literal(p))
        .collect()
}

// ---------- 组合分隔与 JS 链切分 ----------

/// JS 链段
pub struct JsSeg<'a> {
    pub is_js: bool,
    pub text: &'a str,
}

/// 是否含 JS 标记（<js> 或 @js:，大小写不敏感——对齐 legado JS_PATTERN）
pub fn contains_js_marker(rule: &str) -> bool {
    find_ci(rule, "<js>").is_some() || find_ci(rule, "@js:").is_some()
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let nb = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(nb.len())
        .position(|w| w.eq_ignore_ascii_case(nb))
}

/// 按 JS 标记切段（legado JS_PATTERN：`<js>...</js>` 或 `@js:` 贪婪到末尾，均大小写不敏感）
pub fn split_js_chain(rule: &str) -> Vec<JsSeg<'_>> {
    let mut segs: Vec<JsSeg<'_>> = Vec::new();
    let mut start = 0;
    while start < rule.len() {
        // P3-A 修复：{{...}} 内嵌模板内的 @js:/<js> 不视为 JS 段——{{@js:...}} 是
        // 规则引用语法（legado isRule），须留给 expand_inline 处理；原先会被切成
        // 文本段 "{{" + JS 段 "'@js:1+1'}}"（含未闭合 }}）导致求值失败
        let (js_tag, js_at) = find_js_markers(rule, start);
        // <js> 需有闭合 </js> 才成为 JS 段（legado 非贪婪匹配失败则跳过）
        let tag_ok = js_tag.and_then(|p| find_ci(&rule[p + 4..], "</js>").map(|q| (p, p + 4 + q)));
        enum Kind {
            Tag,
            At,
        }
        let cand = match (tag_ok, js_at) {
            (Some((p, _)), Some(q)) if q < p => Some((Kind::At, q, rule.len())),
            (Some((p, end)), _) => Some((Kind::Tag, p, end)),
            (None, Some(q)) => Some((Kind::At, q, rule.len())),
            (None, None) => None,
        };
        match cand {
            None => {
                let t = rule[start..].trim();
                if !t.is_empty() {
                    segs.push(JsSeg {
                        is_js: false,
                        text: t,
                    });
                }
                break;
            }
            Some((kind, seg_start, seg_end)) => {
                let t = rule[start..seg_start].trim();
                if !t.is_empty() {
                    segs.push(JsSeg {
                        is_js: false,
                        text: t,
                    });
                }
                let code = match kind {
                    Kind::Tag => &rule[seg_start + 4..seg_end],
                    Kind::At => &rule[seg_start + 4..],
                };
                let code = code.trim();
                if !code.is_empty() {
                    segs.push(JsSeg {
                        is_js: true,
                        text: code,
                    });
                }
                start = match kind {
                    Kind::Tag => seg_end + 5, // 跳过 </js>（5 字符）
                    Kind::At => rule.len(),
                };
            }
        }
    }
    segs
}

/// 组合分隔切分（对齐 legado RuleAnalyzer.splitRule）：
/// 取最早出现在平衡组（[] / () / {} / 引号）外的 `&&` / `||` / `%%`，
/// 确定分隔符后剩余部分按该分隔符朴素切分（legado 二段切分语义）。
/// 返回 (分隔符, 子规则列表)；无分隔符 → (None, [整条])
pub fn split_combined(rule: &str) -> (Option<&'static str>, Vec<&str>) {
    let b = rule.as_bytes();
    let mut depth_sq = 0i32;
    let mut depth_par = 0i32;
    let mut depth_cur = 0i32;
    let mut in_s = false;
    let mut in_d = false;
    let mut esc = false;
    let mut sep_pos: Option<usize> = None;
    let mut sep_kind: &'static str = "&&";
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if esc {
            esc = false;
            i += 1;
            continue;
        }
        if c == b'\\' {
            esc = true;
            i += 1;
            continue;
        }
        if c == b'\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == b'"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if in_s || in_d {
            i += 1;
            continue;
        }
        match c {
            b'[' => depth_sq += 1,
            b']' => depth_sq -= 1,
            b'(' => depth_par += 1,
            b')' => depth_par -= 1,
            b'{' => depth_cur += 1,
            b'}' => depth_cur -= 1,
            _ if depth_sq == 0 && depth_par == 0 && depth_cur == 0 => {
                let rest = &b[i..];
                if rest.starts_with(b"&&") {
                    sep_pos = Some(i);
                    sep_kind = "&&";
                    break;
                }
                if rest.starts_with(b"||") {
                    sep_pos = Some(i);
                    sep_kind = "||";
                    break;
                }
                if rest.starts_with(b"%%") {
                    sep_pos = Some(i);
                    sep_kind = "%%";
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    match sep_pos {
        None => (None, vec![rule]),
        Some(pos) => {
            let mut subs = vec![&rule[..pos]];
            let rest = &rule[pos + sep_kind.len()..];
            let mut start = 0;
            while let Some(p) = rest[start..].find(sep_kind) {
                subs.push(&rest[start..start + p]);
                start += p + sep_kind.len();
            }
            subs.push(&rest[start..]);
            (Some(sep_kind), subs)
        }
    }
}

/// 应用前缀与替换（legado 语义：前缀拼接 + 正则替换/替换首个）
fn apply_post(results: Vec<String>, rule: &Rule) -> Vec<String> {
    results
        .into_iter()
        .map(|mut s| {
            if let Some(prefix) = &rule.prefix {
                if !s.starts_with(prefix.as_str()) {
                    s = format!("{prefix}{s}");
                }
            }
            if let Some(re) = &rule.replace_regex {
                if !re.is_empty() {
                    let rep = rule.replacement.as_deref().unwrap_or("");
                    s = replace_regex_str(&s, re, rep, rule.replace_first);
                }
            }
            s
        })
        .collect()
}

/// 替换执行（对齐 legado replaceRegex）：
/// - replaceFirst（###）：仅替换首个匹配；无匹配 → 空串；正则编译失败 → 替换串本身
/// - 普通：全部替换；正则编译失败 → 字面串替换
fn replace_regex_str(result: &str, re_str: &str, replacement: &str, first: bool) -> String {
    if first {
        match crate::util::regex::Regex::new(re_str) {
            Ok(re) => {
                if re.is_match(result) {
                    re.replace_first(result, replacement).into_owned()
                } else {
                    String::new()
                }
            }
            Err(_) => replacement.to_string(),
        }
    } else {
        match crate::util::regex::Regex::new(re_str) {
            Ok(re) => re.replace_all(result, replacement).into_owned(),
            Err(_) => result.replace(re_str, replacement),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_prefix() {
        // ## 第二段 @ 开头 → 前缀（兼容 legacy 旧格式）
        let r = parse_rule("div.book##@https://a.com");
        assert_eq!(r.kind, RuleKind::Css);
        assert_eq!(r.prefix.as_deref(), Some("@https://a.com"));
        assert!(r.replace_regex.is_none());
    }

    #[test]
    fn test_parse_legado_flags() {
        assert_eq!(parse_rule("@Json:$.list.name").kind, RuleKind::JsonPath);
        assert_eq!(parse_rule("$.list.name").kind, RuleKind::JsonPath);
        assert_eq!(parse_rule("@XPath://div/a").kind, RuleKind::XPath);
        assert_eq!(parse_rule("//div/a").kind, RuleKind::XPath);
        assert_eq!(parse_rule("@@div.book").kind, RuleKind::Css);
        assert_eq!(parse_rule("a@href").kind, RuleKind::Css);
        // 大小写不敏感标志（legado startsWith(ignoreCase)）
        assert_eq!(parse_rule("@json:$.a").kind, RuleKind::JsonPath);
        assert_eq!(parse_rule("@xpath://a").kind, RuleKind::XPath);
        assert_eq!(parse_rule("@JS:result").kind, RuleKind::Js);
        // 孤立 @ 前缀剥除（链式冗余符号）
        assert_eq!(parse_rule("@class.b@text").kind, RuleKind::Css);
        assert_eq!(parse_rule("@class.b@text").body, "class.b@text");
        // : 前缀 → 正则规则（legado allInOne）
        assert_eq!(parse_rule(":第(.+?)章").kind, RuleKind::Regex);
    }

    #[test]
    fn test_parse_replace_segments() {
        // 三段：替换正则 + 替换串
        let r = parse_rule("a@href##(\\d+)##[$1]");
        assert_eq!(r.replace_regex.as_deref(), Some("(\\d+)"));
        assert_eq!(r.replacement.as_deref(), Some("[$1]"));
        assert!(!r.replace_first);
        // 两段：替换正则，替换串为空（删除匹配）
        let r2 = parse_rule("a@href##\\s+");
        assert_eq!(r2.replace_regex.as_deref(), Some("\\s+"));
        assert!(r2.replacement.is_none());
        // 四段（###）：replaceFirst
        let r3 = parse_rule("##(第.章)##[$1]###");
        assert!(r3.replace_first);
        assert_eq!(r3.body, "");
        assert_eq!(r3.replacement.as_deref(), Some("[$1]"));
    }

    #[test]
    fn test_css_select() {
        let html = r#"<html><body><div class="book"><a href="/1">书名A</a></div><div class="book"><a href="/2">书名B</a></div></body></html>"#;
        let r = apply("div.book a", html);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn test_regex_fallback() {
        let html = "书名：测试书 作者：张三";
        let r = apply("书名：(.+?)\\s", html);
        assert_eq!(r.first().map(String::as_str), Some("测试书"));
    }

    /// GAP 153：规则正则支持 lookbehind（fancy-regex 升级路径）
    #[test]
    fn test_regex_lookbehind() {
        let html = "书名：测试书 作者：张三";
        // 规则主体 lookbehind
        let r = apply("(?<=书名：)\\S+", html);
        assert_eq!(r, vec!["测试书".to_string()]);
        // 替换规则（## 第三段）lookbehind：旧值 (?<=：)(.+?)$ 命中 "测试书"
        let r = apply("(.+?)$##(?<=：)(.+?)$##[$1]", "书名：测试书");
        assert_eq!(r, vec!["书名：[测试书]".to_string()]);
    }

    #[test]
    fn test_regex_case_flag() {
        // 大小写标志：内联 (?i)
        let r = apply("(?i)ABC", "xx abc yy");
        assert_eq!(r, vec!["abc".to_string()]);
        // : 前缀正则规则
        // E16/EG1：全捕获组——每匹配输出 group(0)..group(n) 以 \n 连接
        let r2 = apply(":第(.+?)章", "第一章 第二章");
        assert_eq!(
            r2,
            vec!["第一章\n一".to_string(), "第二章\n二".to_string()],
            "全捕获组：每匹配 group(0)+group(1) 以 \n 连接"
        );
    }

    #[test]
    fn test_regex_chain_replace() {
        // ##pat##rep## 多段替换
        let r = apply("##(第.章)##[$1]", "第一章 第二章");
        assert_eq!(r, vec!["[第一章] [第二章]".to_string()]);
        // 两段：删除匹配
        let r2 = apply("##\\s+", "a b  c");
        assert_eq!(r2, vec!["abc".to_string()]);
        // ### replaceFirst：仅替换首个；无匹配 → 空串
        let r3 = apply("##(第.章)##[$1]###", "第一章 第二章");
        assert_eq!(r3, vec!["[第一章] 第二章".to_string()]);
        let r4 = apply("##(第.章)##[$1]###", "无匹配文本");
        assert_eq!(r4, vec![String::new()]);
        // 主规则 + 替换链（书名去书名号）
        let html = r#"<h2 class="t">《测试书》</h2>"#;
        let r5 = apply("class.t@text##《(.*)》##$1", html);
        assert_eq!(r5, vec!["测试书".to_string()]);
    }

    #[test]
    fn test_json_path() {
        let json = r#"{"data":{"list":[{"name":"书1"},{"name":"书2"}]}}"#;
        let r = apply("{$.data.list.name}", json);
        assert_eq!(r, vec!["书1".to_string(), "书2".to_string()]);
    }

    #[test]
    fn test_json_content_bare_key_forced_jsonpath() {
        // legacy ar.kt:469（setContent isJSON）：JSON 内容上裸键规则强制走 JsonPath
        let json = r#"{"data":{"list":[{"name":"书1"},{"name":"书2"}]}}"#;
        assert_eq!(apply("data.list.name", json), vec!["书1", "书2"]);
        // 数组根自动展开
        let arr = r#"[{"name":"甲"},{"name":"乙"}]"#;
        assert_eq!(apply("name", arr), vec!["甲", "乙"]);
        // 强制 JsonPath 后 ## 替换链仍生效
        let r = apply("data.name##书##本", r#"{"data":{"name":"书名"}}"#);
        assert_eq!(r, vec!["本名"]);
        // && 组合在裸键下同样可用
        let r2 = apply("a.b&&a.c", r#"{"a":{"b":"1","c":"2"}}"#);
        assert_eq!(r2, vec!["1", "2"]);
    }

    #[test]
    fn test_json_explicit_prefix_unchanged_on_json() {
        let json = r#"{"data":{"name":"书名"}}"#;
        // 显式 $. / @Json: 前缀不受影响
        assert_eq!(apply("$.data.name", json), vec!["书名"]);
        assert_eq!(apply("@Json:$.data.name", json), vec!["书名"]);
        // 显式 @CSS: 固定走 JSoup（legacy @CSS: 分支），JSON 文本上无命中 → 空
        assert!(apply("@CSS:data.name", json).is_empty());
        // 非 JSON 括号文本（解析失败）不触发强制
        assert!(apply("data.name", "{not valid json").is_empty());
    }

    #[test]
    fn test_html_css_unaffected_by_json_detect() {
        let html = r#"<html><body><div class="t">标题</div></body></html>"#;
        assert_eq!(apply("class.t@text", html), vec!["标题"]);
        // 含花括号内容的 HTML 不会被误判为 JSON
        let html2 = r#"<html><body><div class="t">{标题}</div></body></html>"#;
        assert_eq!(apply("class.t@text", html2), vec!["{标题}"]);
    }

    #[test]
    fn test_json_path_recursive() {
        let json = r#"{"a":{"content":"深1","b":{"content":"深2"}},"content":"浅"}"#;
        // $..content 任意深度（DFS 前序：父键先出）
        let r = apply("$..content", json);
        assert_eq!(
            r,
            vec!["浅".to_string(), "深1".to_string(), "深2".to_string()]
        );
    }

    #[test]
    fn test_json_path_indexes() {
        let json = r#"{"data":["a","b","c","d"]}"#;
        assert_eq!(apply("$.data[0]", json), vec!["a".to_string()]);
        assert_eq!(apply("$.data[-1]", json), vec!["d".to_string()]);
        assert_eq!(
            apply("$.data[1:3]", json),
            vec!["b".to_string(), "c".to_string()]
        );
        assert_eq!(
            apply("$.data[0,2]", json),
            vec!["a".to_string(), "c".to_string()]
        );
        assert_eq!(
            apply("$.data[*]", json),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
        // 对象通配
        let obj = r#"{"x":1,"y":2}"#;
        assert_eq!(apply("$.*", obj), vec!["1".to_string(), "2".to_string()]);
        // 引号键
        let obj2 = r#"{"a b":{"c":7}}"#;
        assert_eq!(apply("$['a b'].c", obj2), vec!["7".to_string()]);
    }

    /// E14：JsonPath 中部内嵌 `{$.x}` 子引用（模板拼接形态）
    #[test]
    fn test_json_path_embedded_template() {
        let json = r#"{"a":{"t":"T1"},"b":{"t":"T2"},"n":3}"#;
        // 双内嵌 + 字面连接符
        assert_eq!(apply("{$.a.t}-{$.b.t}", json), vec!["T1-T2".to_string()]);
        // 三内嵌含数字字段
        assert_eq!(
            apply("{$.a.t}({$.n}){$.b.t}", json),
            vec!["T1(3)T2".to_string()]
        );
        // 整体单内嵌保持原语义
        assert_eq!(apply("{$.a.t}", json), vec!["T1".to_string()]);
        // 内嵌 + 后缀字面量
        assert_eq!(apply("$.a.t", json), vec!["T1".to_string()]);
    }

    #[test]
    fn test_json_path_filters() {
        let json = r#"{"list":[{"name":"书1","grade":5,"volume":false},{"name":"书2","grade":1},{"name":"书3","grade":8,"volume":true}]}"#;
        // 存在性过滤（真实书源：[?(@.bookName)]）
        let r = apply("$.list[?(@.name)]", json);
        assert_eq!(r.len(), 3);
        // 数值比较（真实书源：[?(@.grade > 1)]）
        let r2 = apply("$.list[?(@.grade > 1)].name", json);
        assert_eq!(r2, vec!["书1".to_string(), "书3".to_string()]);
        // 布尔比较（真实书源：[?(@.volume == false)]）
        let r3 = apply("$.list[?(@.volume == false)].name", json);
        assert_eq!(r3, vec!["书1".to_string()]);
        // 字符串比较 + && 组合
        let r4 = apply("$.list[?(@.grade > 1 && @.name == '书3')].name", json);
        assert_eq!(r4, vec!["书3".to_string()]);
        // 过滤后对象 JSON 化（bookList 场景）
        let r5 = apply("$.list[?(@.grade > 5)]", json);
        assert_eq!(r5.len(), 1);
        assert!(r5[0].contains("\"name\":\"书3\""));
        // || 组合规则：首个命中
        let r6 = apply("$.list[?(@.grade > 5)].name||$.list[0].name", json);
        assert_eq!(r6, vec!["书3".to_string()]);
        let r7 = apply("$.missing||$.list[0].name", json);
        assert_eq!(r7, vec!["书1".to_string()]);
    }

    /// JP1：裸存在性真值——键存在即匹配（值为 null/false/"" 也返回 true），
    /// 含 `"vip":false` 的列表项不被误丢弃；键不存在仍不匹配
    #[test]
    fn test_jsonpath_filter_bare_existence() {
        // 正例：false/null/空串值均视为存在
        let json =
            r#"[{"vip":false,"tag":"a"},{"vip":null,"tag":"b"},{"vip":"","tag":"c"},{"tag":"d"}]"#;
        let r = apply("$[?(@.vip)].tag", json);
        assert_eq!(r, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        // 反例：键缺失不匹配
        let r2 = apply("$[?(@.nope)].tag", json);
        assert!(r2.is_empty());
    }

    /// JP2：缺失路径 `!=` 恒 true（legacy NotEqualsEvaluator 即 !EQ）
    #[test]
    fn test_jsonpath_filter_missing_not_equals() {
        let json = r#"[{"name":"a","extra":"x"},{"name":"b"}]"#;
        // 正例：a 有 extra='x'（!= 'y' 成立）；b 无 extra 键（缺失 → true）
        let r = apply("$[?(@.extra != 'y')].name", json);
        assert_eq!(r, vec!["a".to_string(), "b".to_string()]);
        // 反例：存在且相等 → false
        let r2 = apply("$[?(@.extra != 'x')].name", json);
        assert_eq!(r2, vec!["b".to_string()]);
    }

    /// JP3：数值与数字字符串松散相等（legacy NumberNode.equals → BigDecimal 比较）
    #[test]
    fn test_jsonpath_filter_loose_number_string_eq() {
        let json = r#"[{"n":123,"name":"a"},{"n":456,"name":"b"}]"#;
        // 正例：数值 123 == 字符串 '123'
        let r = apply("$[?(@.n == '123')].name", json);
        assert_eq!(r, vec!["a".to_string()]);
        // 正例（反向）：字符串 "123" == 数值 123
        let j2 = r#"[{"s":"123.5","name":"c"}]"#;
        let r2 = apply("$[?(@.s == 123.5)].name", j2);
        assert_eq!(r2, vec!["c".to_string()]);
        // 反例：非数字字符串仍不相等
        let r3 = apply("$[?(@.n == 'abc')].name", json);
        assert!(r3.is_empty());
    }

    /// JP4：高频操作符补齐——`=~` 正则 / `in` / `nin` / `size`
    #[test]
    fn test_jsonpath_filter_extra_operators() {
        // =~：正则匹配（右值 /pattern/ 形式）
        let json = r#"[{"name":"斗破苍穹"},{"name":"凡人修仙传"}]"#;
        let r = apply("$[?(@.name =~ /斗破/)].name", json);
        assert_eq!(r, vec!["斗破苍穹".to_string()]);
        let r2 = apply("$[?(@.name =~ /金庸/)].name", json);
        assert!(r2.is_empty(), "正则不匹配应为空: {r2:?}");
        // in / nin：左值（不）在右值数组中
        let j2 = r#"[{"g":1,"name":"a"},{"g":2,"name":"b"},{"g":3,"name":"c"}]"#;
        assert_eq!(
            apply("$[?(@.g in [2,3])].name", j2),
            vec!["b".to_string(), "c".to_string()]
        );
        assert_eq!(apply("$[?(@.g nin [2,3])].name", j2), vec!["a".to_string()]);
        assert_eq!(
            apply("$[?(@.name in ['a','c'])].name", j2),
            vec!["a".to_string(), "c".to_string()]
        );
        // size：数组/字符串长度等于右值
        let j3 =
            r#"[{"tags":[1,2,3],"name":"x"},{"tags":[1],"name":"y"},{"title":"abcd","name":"z"}]"#;
        assert_eq!(apply("$[?(@.tags size 3)].name", j3), vec!["x".to_string()]);
        assert_eq!(apply("$[?(@.tags size 1)].name", j3), vec!["y".to_string()]);
        assert_eq!(
            apply("$[?(@.title size 4)].name", j3),
            vec!["z".to_string()]
        );
        let r3 = apply("$[?(@.tags size 9)].name", j3);
        assert!(r3.is_empty(), "长度不等应为空: {r3:?}");
        // 词操作符不误伤路径键名（@.min / @.size 含操作符字样）
        let j4 = r#"[{"min":5},{"size":[1,2]}]"#;
        assert_eq!(apply("$[?(@.min > 3)]", j4).len(), 1);
        assert_eq!(apply("$[?(@.size size 2)]", j4).len(), 1);
    }

    /// P0-2：JSONPath 段数超限（超深路径）→ 返回错误语义（空结果 + 日志），不栈溢出 abort
    #[test]
    fn test_jsonpath_depth_limit_segments() {
        let json = r#"{"a":{"a":{"a":1}}}"#;
        // 100 段路径（远超 JSONPATH_MAX_DEPTH=64）
        let path = format!("$.{}", "a.".repeat(100).trim_end_matches('.'));
        let r = apply(&path, json);
        assert!(r.is_empty(), "超深路径应返回空结果而非崩溃: {r:?}");
        // 64 段以内仍正常（不误伤合法规则）
        let ok_path = format!("$.{}", "a.".repeat(3).trim_end_matches('.'));
        assert_eq!(apply(&ok_path, json), vec!["1".to_string()]);
    }

    /// P0-2：过滤表达式括号嵌套超限 → 空结果，不栈溢出 abort
    #[test]
    fn test_jsonpath_depth_limit_filter_parens() {
        let json = r#"{"list":[{"name":"书1"}]}"#;
        // 200 层括号嵌套的过滤表达式
        let expr = format!(
            "$.list[?({}@.name{})].name",
            "(".repeat(200),
            ")".repeat(200)
        );
        let r = apply(&expr, json);
        assert!(r.is_empty(), "超深括号过滤应返回空结果而非崩溃: {r:?}");
        // 正常括号仍工作
        let ok = apply("$.list[?((@.name))].name", json);
        assert_eq!(ok, vec!["书1".to_string()]);
    }

    /// P0-2：深层嵌套 JSON 解析（serde_json 128 层上限之外的输入）→ 解析错误回退空结果，不 abort；
    /// 128 层内可解析的深层值 + 递归求值 → 求值深度上限（64）返回空结果，不 abort
    #[test]
    fn test_jsonpath_deep_nested_value() {
        // 500 层嵌套数组（超过 serde_json 内置 128 层递归上限 → 解析报错 → 回退空结果）
        let deep = format!("{}0{}", "[".repeat(500), "]".repeat(500));
        let r = apply("$[*]", &deep);
        assert!(r.is_empty(), "超深解析应优雅失败而非崩溃: {r:?}");
        let r2 = apply("$..x", &deep);
        assert!(r2.is_empty());
        // 100 层（可解析）嵌套数组：$[*].x 沿数组展开下钻 → 求值深度超限 → 空结果，不 abort
        let deep100 = format!("{}0{}", "[".repeat(100), "]".repeat(100));
        let r3 = apply("$[*].x", &deep100);
        assert!(r3.is_empty(), "深层值求值应受深度上限约束而非崩溃: {r3:?}");
        // 普通深度 JSON 正常
        let normal = r#"{"a":[{"b":{"c":"深"}}]}"#;
        assert_eq!(apply("$.a[0].b.c", normal), vec!["深".to_string()]);
    }

    #[test]
    fn test_json_path_html_embedded() {
        // HTML 内嵌 JSON 行（json_from_html 回退）
        let html = "前文\n{\"data\":{\"name\":\"内嵌\"}}\n后文";
        let r = apply("$.data.name", html);
        assert_eq!(r, vec!["内嵌".to_string()]);
    }

    #[test]
    fn test_js_rule() {
        let html = "abc123";
        // js: / @js: 前缀剥离 + result 变量注入
        assert_eq!(apply("js:result.length", html), vec!["6".to_string()]);
        assert_eq!(
            apply("@js:result.toUpperCase()", html),
            vec!["ABC123".to_string()]
        );
        // JS 失败 → 空结果
        assert!(apply("@js:throw new Error('x')", html).is_empty());
        // JS 返回空串 → 空结果
        assert!(apply("@js:''", html).is_empty());
    }

    #[test]
    fn test_js_chain() {
        let html = r#"<div class="b">abc123</div>"#;
        // <js> 链：规则结果进 JS（result 变量），再进后续规则
        let r = apply("<js>result.toUpperCase()</js>", html);
        assert_eq!(r, vec![html.to_uppercase()]);
        // CSS → JS → 结果
        let r2 = apply("class.b@text@js:result.replace('abc','xyz')", html);
        assert_eq!(r2, vec!["xyz123".to_string()]);
        // <js> 在前 → JS → CSS 链（真实书源：<js>...</js>$.data[*] 形态）
        let r3 = apply("<js>result.replace('abc','xyz')</js>@class.b@text", html);
        assert_eq!(r3, vec!["xyz123".to_string()]);
        // JS 失败 → 整链空
        assert!(apply("class.b@text@js:throw new Error('x')", html).is_empty());
    }

    /// AR3：链式规则某段结果为空 → 提前终止整链返回空（legado AnalyzeRule.getString：
    /// 段空结果为 null，后续所有段跳过；此前以空串续喂下一段，如
    /// class.missing@text@js:result.length 误得 "0"）
    #[test]
    fn test_js_chain_empty_intermediate_terminates() {
        let html = r#"<div class="b">abc123</div>"#;
        // CSS 段无命中 → JS 不执行 → 空（非 "0"）
        assert!(apply("class.missing@text@js:result.length", html).is_empty());
        // 三段链：中段 JS 得空 → 后续段跳过 → 整链空
        assert!(apply("class.b@text<js>''</js>class.b@text", html).is_empty());
        // 对照：各段非空时多段链照常贯通
        let r = apply(
            "class.b@text<js>result.replace('abc','x')</js>@js:result + 'y'",
            html,
        );
        assert_eq!(r, vec!["x123y".to_string()]);
    }

    #[test]
    fn test_inline_js_substitution() {
        let html = r#"<html><body><div class="book">书名A</div><div class="book">书名B</div></body></html>"#;
        // {{...}} JS 构建 CSS 选择器，替换回规则后执行
        let r = apply("{{'div.' + 'book'}}", html);
        assert_eq!(r.len(), 2);
        // JS 可读取注入的 result（当前上下文文本），条件返回正则规则
        let html2 = "书名：测试书 作者：张三";
        let rule = r#"{{result.startsWith('书名') ? '书名：(.+?)\\s' : 'div'}}"#;
        let r2 = apply(rule, html2);
        assert_eq!(r2.first().map(String::as_str), Some("测试书"));
        // JS 失败 → 展开为空 → 空结果
        assert!(apply("{{nonexistent.fn()}}", html).is_empty());
        // 未闭合 {{ 原样处理（按 JsonPath 分支解析失败 → 空结果），不 panic
        assert!(apply("{{div.book", html).is_empty());
    }

    #[test]
    fn test_inline_jsonpath_substitution() {
        let json = r#"{"data":{"n":42}}"#;
        // {{$.x}} → JSONPath 提取（非 JS 执行），替换回规则后执行
        let r = apply("@js:{{$.data.n}}", json);
        assert_eq!(r, vec!["42".to_string()]);
        // 提取失败 → 替换为空 → 空结果
        let r2 = apply("@js:{{$.missing}}", json);
        assert!(r2.is_empty());
    }

    #[test]
    fn test_inline_rule_ref_substitution() {
        // {{@@rule}} 规则引用（真实书源：{{@@[name$=update_time]@content##T##🔸}}）
        let html =
            r#"<meta name="update_time" content="2024-01-01"><div class="card"><p>正文</p></div>"#;
        let r = apply("更新时间：{{@@[name$=update_time]@content##-##/}}", html);
        assert_eq!(r, vec!["更新时间：2024/01/01".to_string()]);
        let r2 = apply("{{@@.card@p@text}}", html);
        assert_eq!(r2, vec!["正文".to_string()]);
        // {{//xpath}} 规则引用（真实书源：{{//data[@name='Title']/text()}}）
        let xml = r#"<data><item name="Title">测试书</item></data>"#;
        let r3 = apply("书名：{{//data/item[@name='Title']/text()}}", xml);
        assert_eq!(r3, vec!["书名：测试书".to_string()]);
        // {{$.x}} 多结果换行拼接
        assert_eq!(
            expand_inline(
                "{{$.list.name}}",
                r#"{"list":[{"name":"书1"},{"name":"书2"}]}"#
            ),
            "书1\n书2"
        );
    }

    /// P2：{{}} 模板替换值含规则控制标记（@js:/##/{{/@// 前缀）——替换后不再递归
    /// 重新解析执行（防数据驱动二次执行），按纯文本返回；安全值拼接照常重新解析
    #[test]
    fn dbg_tmp_rule() {
        let html = "<html><body></body></html>";
        let r = apply_depth("@js:'@js:1+1'", html, 1, None);
        eprintln!("DBG apply_depth = {:?}", r);
        let mut vars = std::collections::HashMap::new();
        vars.insert("result".to_string(), html.to_string());
        eprintln!(
            "DBG eval_js = {:?}",
            crate::parser::js::eval_js("'@js:1+1'", &vars)
        );
        eprintln!("DBG segs len = {}", split_js_chain("@js:'@js:1+1'").len());
    }

    #[test]
    fn test_inline_template_no_double_parse_of_control_values() {
        let html = "<html><body></body></html>";
        // 值以 @js: 开头：旧实现重新解析会再次执行（返回执行结果）；现按纯文本
        let r = apply("{{$.x}}", r#"{"x":"@js:result + '!'"}"#);
        assert_eq!(
            r,
            vec!["@js:result + '!'".to_string()],
            "@js: 前缀值不得二次执行"
        );
        // 值中间含 @js:（后缀链标记）：同样不再执行
        let r = apply("{{$.x}}", r#"{"x":"abc@js:1+1"}"#);
        assert_eq!(r, vec!["abc@js:1+1".to_string()]);
        // 值含 ##：旧实现重新解析会切出新规则链；现按纯文本
        let r = apply("x{{$.a}}y", r###"{"a":"##"}"###);
        assert_eq!(r, vec!["x##y".to_string()]);
        // 值含 {{：不再二次模板展开
        let r = apply("{{$.x}}", r#"{"x":"{{'div'}}"}"#);
        assert_eq!(r, vec!["{{'div'}}".to_string()]);
        // 值以 // 开头：不再按 XPath 规则解析
        let r = apply("{{$.x}}", r#"{"x":"//div"}"#);
        assert_eq!(r, vec!["//div".to_string()]);
        // {{@js:...}} 规则引用：内层求值结果含控制标记 → 外层按纯文本（不二次执行）
        let r = apply("{{@js:'@js:1+1'}}", html);
        assert_eq!(r, vec!["@js:1+1".to_string()]);
        // 前缀/替换段在控制值路径仍应用（##pat##rep）
        let r = apply("{{$.x}}##x##y", r#"{"x":"@js:xx"}"#);
        assert_eq!(r, vec!["@js:yy".to_string()]);
        // 安全值：拼接 CSS/正则仍照常重新解析执行（既有语义不受影响）
        let html2 = r#"<div class="book">书名A</div>"#;
        let r = apply("{{'div.' + 'book'}}", html2);
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("书名A"), "CSS 拼接应重新解析执行: {r:?}");
        // 安全 CSS 值经 JSON 上下文提取（html 为 JSON 文本，无 HTML 可匹配）→
        // 按 legado 语义返回展开文本（{{}} 规则执行无果 → 规则串本身即结果）
        let r = apply("{{$.x}}", r#"{"x":"div.book"}"#);
        assert_eq!(
            r,
            vec!["div.book".to_string()],
            "JSON 上下文安全值按展开文本返回: {r:?}"
        );
    }

    #[test]
    fn test_expand_inline() {
        // 数组下标形式 {{$.a[0]}}
        assert_eq!(
            expand_inline("{{$.list[0]}}", r#"{"list":["书1","书2"]}"#),
            "书1"
        );
        // 上下文非完整 JSON → 逐行提取 JSON 片段（json_from_html 回退）
        assert_eq!(
            expand_inline(
                "{{$.data.name}}",
                "前文\n{\"data\":{\"name\":\"内嵌\"}}\n后文"
            ),
            "内嵌"
        );
        // 未闭合 {{ 原样返回
        assert_eq!(expand_inline("{{div.book", "<html></html>"), "{{div.book");
    }

    #[test]
    fn test_split_combined_basic() {
        let (sep, subs) = split_combined("a&&b&&c");
        assert_eq!(sep, Some("&&"));
        assert_eq!(subs, vec!["a", "b", "c"]);
        let (sep, subs) = split_combined("a||b");
        assert_eq!(sep, Some("||"));
        assert_eq!(subs, vec!["a", "b"]);
        let (sep, _subs) = split_combined("a%%b");
        assert_eq!(sep, Some("%%"));
        // 无分隔符
        let (sep, subs) = split_combined("a.b@c");
        assert_eq!(sep, None);
        assert_eq!(subs, vec!["a.b@c"]);
        // 平衡组内分隔符不参与（属性选择器 / 引号 / {{}} / 过滤表达式）
        let (sep, subs) = split_combined("a[href='x&&y']@b||c");
        assert_eq!(sep, Some("||"));
        assert_eq!(subs, vec!["a[href='x&&y']@b", "c"]);
        let (sep, _subs) = split_combined("[?(@.a == 'x' && @.b)]");
        assert_eq!(sep, None);
        let (sep, subs) = split_combined("{{a&&b}}&&c");
        assert_eq!(sep, Some("&&"));
        assert_eq!(subs, vec!["{{a&&b}}", "c"]);
    }

    #[test]
    fn test_split_js_chain() {
        let segs = split_js_chain("a@href<js>code1</js>b@text@js:code2");
        let kinds: Vec<bool> = segs.iter().map(|s| s.is_js).collect();
        let texts: Vec<&str> = segs.iter().map(|s| s.text).collect();
        assert_eq!(kinds, vec![false, true, false, true]);
        assert_eq!(texts, vec!["a@href", "code1", "b@text", "code2"]);
        // 大小写不敏感
        let segs2 = split_js_chain("<JS>x</JS>");
        assert_eq!(segs2.len(), 1);
        assert!(segs2[0].is_js);
        assert_eq!(segs2[0].text, "x");
        // 未闭合 <js> → 普通段
        let segs3 = split_js_chain("<js>unclosed");
        assert_eq!(segs3.len(), 1);
        assert!(!segs3[0].is_js);
        // @js: 贪婪到末尾（后续 @js: 并入首个代码）
        let segs4 = split_js_chain("x@js:a@js:b");
        assert_eq!(segs4.len(), 2);
        assert_eq!(segs4[1].text, "a@js:b");
        // 无标记 → 单普通段
        let segs5 = split_js_chain("class.a@text");
        assert_eq!(segs5.len(), 1);
        assert!(!segs5[0].is_js);
    }

    #[test]
    fn test_xpath_rules() {
        let xml = r#"<?xml version="1.0"?>
<library>
  <book id="1"><title>三体</title></book>
  <book id="2"><title>流浪地球</title></book>
</library>"#;
        assert_eq!(
            apply("//book/title", xml),
            vec!["三体".to_string(), "流浪地球".to_string()]
        );
        // && 组合
        let r = apply("//book[1]/title&&//book[2]/title", xml);
        assert_eq!(r, vec!["三体".to_string(), "流浪地球".to_string()]);
        // || 首个命中（legado：同一条规则只按最早出现的分隔符切分——|| 单独使用）
        let r2 = apply("//nonexistent||//book[1]/title", xml);
        assert_eq!(r2, vec!["三体".to_string()]);
    }

    #[test]
    fn test_url_kind_kept() {
        // Url 变体保留（无检测路径——孤立 @ 现按 CSS/正则处理）
        let r = parse_rule("@class.a");
        assert_eq!(r.kind, RuleKind::Css);
        assert_eq!(r.body, "class.a");
    }

    /// F12/AR4：@get:{title}/@get:{bookName} 内建回退（legacy ar.kt:632-645）——
    /// 变量表未命中时回退当前章标题/书名；无上下文 → 空串
    #[test]
    fn test_resolve_get_builtin_fallback() {
        // 无上下文：回退为空串（legacy chapter/book 为 null）
        let vars = RuleVars::new();
        assert_eq!(resolve_get("@get:{title}", &vars), "");
        assert_eq!(resolve_get("@get:{bookName}", &vars), "");

        // 注入实体上下文后命中
        let mut vars = RuleVars::new();
        vars.chapter_title = Some("第一章 测试".to_string());
        vars.book_name = Some("测试书名".to_string());
        assert_eq!(resolve_get("@get:{title}", &vars), "第一章 测试");
        assert_eq!(resolve_get("@get:{bookName}", &vars), "测试书名");

        // URL 模板内混合拼接 + 未命中键仍为空
        assert_eq!(
            resolve_get("https://x.test/@get:{bookName}/c/@get:{title}.json", &vars),
            "https://x.test/测试书名/c/第一章 测试.json"
        );
        assert_eq!(
            resolve_get("@get:{missing}@get:{title}", &vars),
            "第一章 测试"
        );
    }

    /// F12/AR4：变量表优先于内建回退（legacy getString 先查 varMap）
    #[test]
    fn test_resolve_get_vars_table_overrides_builtin() {
        let mut vars = RuleVars::new();
        vars.chapter_title = Some("章节标题".to_string());
        vars.book_name = Some("书名".to_string());
        vars.insert("title".to_string(), "@put 手动覆盖".to_string());
        vars.insert("bookName".to_string(), "@put 书名覆盖".to_string());
        assert_eq!(resolve_get("@get:{title}", &vars), "@put 手动覆盖");
        assert_eq!(resolve_get("@get:{bookName}", &vars), "@put 书名覆盖");
        // 其他键不受影响
        assert_eq!(resolve_get("@get:{other}", &vars), "");
    }

    /// F12/AR4：规则链中 @get:{title}/@get:{bookName} 经 apply_with_vars 求值同样吃到内建回退
    /// （apply_single 先 makeUpRule 替换 @get 再执行规则）
    #[test]
    fn test_apply_with_vars_builtin_fallback() {
        let mut vars = RuleVars::new();
        vars.chapter_title = Some("第2章".to_string());
        vars.book_name = Some("书A".to_string());
        // 正文规则内嵌 @get 替换后成为可用正则
        let out = apply_with_vars("^@get:{bookName}，(.+)", "书A，张三", &mut vars);
        assert_eq!(out, vec!["张三".to_string()]);
        // 无回退上下文时 @get 解析为空串，规则失配
        let mut empty = RuleVars::new();
        let out2 = apply_with_vars("^@get:{bookName}，(.+)", "书A，张三", &mut empty);
        assert!(out2.is_empty() || out2.iter().all(|s| s.is_empty()));
    }

    /// F12/AR4：内建上下文字段不随 save_book_vars 持久化——跨请求无脏值
    #[test]
    fn test_book_vars_persistence_strips_context() {
        let mut vars = RuleVars::new();
        vars.insert("bid".to_string(), "42".to_string());
        vars.chapter_title = Some("脏标题".to_string());
        vars.book_name = Some("脏书名".to_string());
        save_book_vars("ns-f12", "src-f12", "https://b.test/1", &vars);
        let loaded = load_book_vars("ns-f12", "src-f12", "https://b.test/1");
        assert_eq!(loaded.get("bid").map(String::as_str), Some("42"));
        assert!(loaded.chapter_title.is_none(), "章标题上下文不应持久化");
        assert!(loaded.book_name.is_none(), "书名上下文不应持久化");
    }

    /// E10/AR5：push_js_context 把章节/书上下文以保留键写入 JS 变量表——
    /// 结构体字段（title/url/next/bookName）+ map 透传（baseUrl/src/额外 `__x__` 键）
    #[test]
    fn test_push_js_context_reserved_keys() {
        let mut vars = RuleVars::new();
        vars.chapter_title = Some("第1章".to_string());
        vars.chapter_url = Some("https://b.test/c/1".to_string());
        vars.next_chapter_url = Some("https://b.test/c/2".to_string());
        vars.book_name = Some("书名甲".to_string());
        vars.insert("baseUrl".to_string(), "https://b.test/c/1".to_string());
        vars.insert("src".to_string(), "<p>正文</p>".to_string());
        vars.insert("__chapter_index__".to_string(), "3".to_string());
        vars.insert("__book_author__".to_string(), "作者丙".to_string());

        let mut js_vars = std::collections::HashMap::new();
        push_js_context(&mut js_vars, Some(&vars));
        assert_eq!(
            js_vars
                .get(crate::parser::rule::RK_CHAPTER_TITLE)
                .map(String::as_str),
            Some("第1章")
        );
        assert_eq!(
            js_vars
                .get(crate::parser::rule::RK_CHAPTER_URL)
                .map(String::as_str),
            Some("https://b.test/c/1")
        );
        assert_eq!(
            js_vars
                .get(crate::parser::rule::RK_NEXT_CHAPTER_URL)
                .map(String::as_str),
            Some("https://b.test/c/2")
        );
        assert_eq!(
            js_vars
                .get(crate::parser::rule::RK_BOOK_NAME)
                .map(String::as_str),
            Some("书名甲")
        );
        // map 透传：baseUrl/src/路由层反查的 index 与 author
        assert_eq!(
            js_vars.get("baseUrl").map(String::as_str),
            Some("https://b.test/c/1")
        );
        assert_eq!(js_vars.get("src").map(String::as_str), Some("<p>正文</p>"));
        assert_eq!(
            js_vars
                .get(crate::parser::rule::RK_CHAPTER_INDEX)
                .map(String::as_str),
            Some("3")
        );
        assert_eq!(
            js_vars
                .get(crate::parser::rule::RK_BOOK_AUTHOR)
                .map(String::as_str),
            Some("作者丙")
        );

        // None → 不写入任何键
        let mut empty = std::collections::HashMap::new();
        push_js_context(&mut empty, None);
        assert!(empty.is_empty());
    }

    /// E10/AR5：apply_with_vars 的 JS 规则可访问 chapter/title/book/nextChapterUrl 绑定
    /// （push_js_context 贯通 → parser::js 展开为类型化全局；legacy AnalyzeRule evalJS）
    #[test]
    fn test_apply_with_vars_js_context_bindings() {
        let mut vars = RuleVars::new();
        vars.chapter_title = Some("第三章".to_string());
        vars.chapter_url = Some("https://b.test/c/3".to_string());
        vars.next_chapter_url = Some("https://b.test/c/4".to_string());
        vars.book_name = Some("测试书".to_string());
        // JS 规则：拼接上下文绑定
        let out = apply_with_vars(
            "@js:chapter.title + '|' + chapter.url + '|' + nextChapterUrl + '|' + book.name",
            "ignored",
            &mut vars,
        );
        assert_eq!(
            out,
            vec!["第三章|https://b.test/c/3|https://b.test/c/4|测试书".to_string()]
        );
        // {{}} 内嵌 JS 同样吃到绑定
        let out2 = apply_with_vars("^{{chapter.title}}，(.+)", "第三章，张三", &mut vars);
        assert_eq!(out2, vec!["张三".to_string()]);
    }

    /// P1 跨阶段合并：book 级作底、章节级覆盖（legacy book→chapter 单 varMap 回退链）
    #[test]
    fn test_load_book_vars_merged_two_level() {
        let ns = format!("p1m-{}", uuid::Uuid::new_v4());
        let src = "https://src.test/p1m";
        let mut book_level = RuleVars::new();
        book_level.insert("bid".to_string(), "B1".to_string());
        book_level.insert("shared".to_string(), "from-book".to_string());
        save_book_vars(&ns, src, "https://b.test/book", &book_level);
        let mut ch_level = RuleVars::new();
        ch_level.insert("cid".to_string(), "C7".to_string());
        ch_level.insert("shared".to_string(), "from-chapter".to_string());
        save_book_vars(&ns, src, "https://b.test/c/1", &ch_level);

        let merged = load_book_vars_merged(&ns, src, "https://b.test/book", "https://b.test/c/1");
        assert_eq!(
            merged.get("bid").map(String::as_str),
            Some("B1"),
            "book 级作底"
        );
        assert_eq!(
            merged.get("cid").map(String::as_str),
            Some("C7"),
            "章节级并入"
        );
        assert_eq!(
            merged.get("shared").map(String::as_str),
            Some("from-chapter"),
            "同名键章节级覆盖"
        );

        // 空 root / 同键 → 退化为单键读取
        let only_leaf = load_book_vars_merged(&ns, src, "", "https://b.test/c/1");
        assert_eq!(only_leaf.get("cid").map(String::as_str), Some("C7"));
        assert_eq!(only_leaf.get("bid"), None);
    }

    /// P1 双键保存：save_book_vars_two_level 同时写 root/leaf 两键
    #[test]
    fn test_save_book_vars_two_level() {
        let ns = format!("p1s-{}", uuid::Uuid::new_v4());
        let src = "https://src.test/p1s";
        let mut vars = RuleVars::new();
        vars.insert("k".to_string(), "v".to_string());
        save_book_vars_two_level(&ns, src, "https://b.test/book", "https://b.test/c/2", &vars);
        assert_eq!(
            load_book_vars(&ns, src, "https://b.test/c/2")
                .get("k")
                .map(String::as_str),
            Some("v")
        );
        assert_eq!(
            load_book_vars(&ns, src, "https://b.test/book")
                .get("k")
                .map(String::as_str),
            Some("v")
        );
        // root == leaf → 只存一份不 panic
        save_book_vars_two_level(
            &ns,
            src,
            "https://b.test/same",
            "https://b.test/same",
            &vars,
        );
        assert_eq!(
            load_book_vars(&ns, src, "https://b.test/same")
                .get("k")
                .map(String::as_str),
            Some("v")
        );
    }
}
