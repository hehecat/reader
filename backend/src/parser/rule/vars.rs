//! 书源规则变量（`@put` / `@get`）、JS 上下文保留键绑定与书级变量 LRU + SQLite 读写穿透

use super::chain::find_ci;

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

/// 优先从当前任务级依赖注入作用域（`current_task_storage`）取 Storage，
/// 仅未处于请求作用域（独立同步单测等）时回退全局注册句柄，避免多实例/并发测试竞争。
fn book_vars_registered() -> Option<crate::storage::Storage> {
    crate::storage::current_task_storage().or_else(|| {
        BOOK_VARS_STORAGE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    })
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

/// 超时/失败仅降级纯内存；WARN 去抖 60s——并发搜索时锁竞争会集中爆发，逐条刷屏无益
fn warn_book_vars_write_err(e: impl std::fmt::Display) {
    static LAST_WARN_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
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

/// SQLite 写穿透执行边界：内存缓存已同步更新；多线程 Tokio runtime 下异步后台 spawn 落库，
/// 零阻塞解析线程；CurrentThread 单测 runtime 或非 Tokio 线程保持 `block_on_task` 同步落库。
fn spawn_or_block_book_vars_write<T: Send + 'static>(
    fut: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    timeout: std::time::Duration,
    what: &'static str,
) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            handle.spawn(async move {
                if let Err(e) = fut.await {
                    warn_book_vars_write_err(e);
                }
            });
        }
        _ => {
            if let Err(e) = crate::parser::js::block_on_task(fut, timeout, what) {
                warn_book_vars_write_err(e);
            }
        }
    }
}

/// SQLite 写穿透落库（单键）
fn book_vars_db_write(ns: &str, source: &str, url: &str, json: &str) {
    let Some(storage) = book_vars_registered() else {
        return;
    };
    let (ns, source, url) = (ns.to_string(), source.to_string(), url.to_string());
    let json = json.to_string();
    let fut = async move { storage.put_book_vars_cache(&ns, &source, &url, &json).await };
    spawn_or_block_book_vars_write(fut, std::time::Duration::from_secs(10), "bookVars.put");
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
            spawn_or_block_book_vars_write(
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
pub(super) fn split_put(rule: &str) -> (String, Vec<(String, String)>) {
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
