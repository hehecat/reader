//! 目录抓取与解析（ruleToc / 多页 nextTocUrl 有界并发保序）

use anyhow::Result;
use serde::Deserialize;

use crate::model::book_chapter::BookChapter;
use crate::model::BookSource;
use crate::parser::css_chain::css_chain;
use crate::parser::rule::{apply, apply_with_vars, parse_rule, RuleKind};
use crate::service::crawler;

use super::{apply_login_check_js, expand_next_url, fetch_url, to_abs};

/// ruleToc 结构（legacy TocRule）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TocRule {
    pub chapter_list: Option<String>,
    pub chapter_name: Option<String>,
    pub chapter_url: Option<String>,
    /// legacy isVolume 规则（命中 true → 卷标题）
    pub is_volume: Option<String>,
    pub chapter_vip: Option<String>,
    pub update_time: Option<String>,
    pub next_toc_url: Option<String>,
    pub chapter_type: Option<String>,
    pub init: Option<String>,
    pub pre_update_js: Option<String>,
}

/// 目录回退判定：首轮 0 章、且书页地址非空又确实不同于目录页地址时，才值得补抓一次
pub(super) fn should_retry_toc_with_book_page(
    toc_url: &str,
    book_url: &str,
    got: &[BookChapter],
) -> bool {
    got.is_empty() && !book_url.trim().is_empty() && book_url != toc_url
}

/// 目录解析（ruleToc：chapterList 定位 + 字段规则；多页 nextTocUrl 循环）
/// F12/AR4：`book_name` 为当前书名——@get:{bookName} 内建回退源（legacy setBook）
/// P1 跨阶段：`book_url` 非空时按 book 级作底、tocUrl 级覆盖合并读取（详情 @put 直达目录）；
/// 结束后双键回写（目录阶段变量对正文可见）
///
/// legacy 对齐：抓取报错标记运行期失效快照，成功则清除该源标记。
#[allow(clippy::too_many_arguments)]
pub async fn analyze_toc(
    ns: &str,
    toc_url: &str,
    source: &BookSource,
    max_pages: usize,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<BookChapter>> {
    let mut result = analyze_toc_impl(ns, toc_url, source, max_pages, book_name, book_url).await;
    // 目录页规则落空 → 回退拿书页当目录页再抓一次。
    // 大量老源的 ruleBookInfo.tocUrl 指向早已失效的镜像域（页面 404/错误页），而书页
    // 本身仍带完整目录；只在「首轮 0 章且 tocUrl 与 bookUrl 不同」时补抓一次，代价可控。
    if should_retry_toc_with_book_page(toc_url, book_url, result.as_deref().unwrap_or(&[])) {
        if let Ok(v) = analyze_toc_impl(ns, book_url, source, max_pages, book_name, book_url).await
        {
            if !v.is_empty() {
                tracing::debug!(
                    "目录页为空, 回退书页重抓成功 [{toc_url} → {book_url}]: {} 章",
                    v.len()
                );
                result = Ok(v);
            }
        }
    }
    match result {
        Ok(v) => {
            crate::service::health::clear_source_invalid(ns, &source.book_source_url);
            if let Some(st) = crate::parser::rule::registered_storage() {
                let _ = st
                    .record_path_outcome(ns, &source.book_source_url, "toc", !v.is_empty())
                    .await;
            }
            Ok(v)
        }
        Err(e) => {
            crate::service::health::mark_source_invalid(
                ns,
                &source.book_source_url,
                &e.to_string(),
            );
            if let Some(st) = crate::parser::rule::registered_storage() {
                let _ = st
                    .record_path_outcome(ns, &source.book_source_url, "toc", false)
                    .await;
            }
            Err(e)
        }
    }
}

/// 目录抓取总预算(秒, env READER_TOC_BUDGET_SECS 覆盖, 默认 30): 整次目录抓取的墙钟上限。
/// 到点后不再发起新页, 用**已抓到的页**产出部分目录(日志 warn 标注「目录不完整」),
/// 只有一页都没抓成才报错——慢源不再把整次进书拖成失败。
fn toc_budget_secs() -> u64 {
    static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let base = *B.get_or_init(|| {
        std::env::var("READER_TOC_BUDGET_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v: &u64| *v > 0)
            .unwrap_or(30)
    });
    match TOC_BUDGET_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => base,
        v => v,
    }
}

/// 测试覆盖总预算(None = 用 env/默认)
static TOC_BUDGET_OVERRIDE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
pub(super) fn set_toc_budget_secs(v: u64) {
    TOC_BUDGET_OVERRIDE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 目录单页并发上限(env READER_TOC_CONCURRENCY 覆盖, 默认 4, clamp 1..=8)
///
/// 语义: 单次目录抓取**同时在飞**的页请求数上限；= 1 时退化为改造前的串行语义
/// (逐页抓、逐页处理，用于对照/回滚)。
/// 说明: 链式翻页(nextTocUrl 由上一页 HTML 解析得出)时任一时刻只有下一页地址已知,
/// 在飞数结构上就是 1；该上限真正约束的是「下一页预取」与「当前页解析/变量落盘」
/// 重叠阶段的在飞请求数, 以及一次解析出多个候选地址时的扇出。
/// 跨用户不会放大: 每个目录请求自带一个小池, 源站级限速仍由
/// [`crate::service::search::concurrent_rate_acquire`] 按书源统一把关。
fn toc_concurrency() -> usize {
    static C: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let base = *C.get_or_init(|| {
        clamp_toc_concurrency(
            std::env::var("READER_TOC_CONCURRENCY")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(4),
        )
    });
    match TOC_CONCURRENCY_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => base,
        v => clamp_toc_concurrency(v),
    }
}

/// 并发上限收敛到 1..=8(0/超大值都夹回合法区间, 防止误配把源站打爆)
pub(super) fn clamp_toc_concurrency(v: usize) -> usize {
    v.clamp(1, 8)
}

/// 测试覆盖并发上限(0 = 用 env/默认)
static TOC_CONCURRENCY_OVERRIDE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) fn set_toc_concurrency(v: usize) {
    TOC_CONCURRENCY_OVERRIDE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 单页目录抓取: 偶发失败(连接抖动/5xx/CF 质询临失败)重试 1 次, 仍失败则上抛
async fn fetch_toc_page_retry(
    ns: &str,
    url: &str,
    source: &BookSource,
) -> Result<crawler::FetchResponse> {
    match fetch_url(ns, url, source).await {
        Ok(r) => Ok(r),
        Err(e) => {
            tracing::warn!("目录页抓取失败, 重试 1 次 [{url}]: {e:#}");
            fetch_url(ns, url, source).await
        }
    }
}

/// nextTocUrl → **多个**页地址（legado 多 URL 语义）
///
/// legado 的 nextTocUrl 一次可以给出多页地址：
/// - CSS/XPath 命中多个元素（`option@value` / `.page-item a@href`——一个 `<select>`
///   或一组页码 `<a>` 里就是全套页码）；
/// - `@js:` / `<js>` 返回 URL 数组（legado：JS 可返回字符串或数组）。
///
/// 单 URL 时行为与改造前一致：首元素走原 [`crate::service::search::field_url_with_vars`]
/// 语义（URL 字段 isUrl 直判 + 相对转绝对），多 URL 只是把同一套管线对每个地址各跑一次。
pub(crate) fn next_toc_urls(
    context: &str,
    rule: &str,
    base: &str,
    source: &BookSource,
    ns: &str,
    vars: &mut crate::parser::rule::RuleVars,
) -> Vec<String> {
    let mut out = Vec::new();
    for u in next_toc_url_candidates(context, rule, base, vars) {
        // E7：翻页 URL 过模板/JS 管线后再绝对化（与单 URL 路径同序）
        let expanded = expand_next_url(&u, base, source, ns);
        let abs = to_abs(expanded.trim(), base);
        if !abs.trim().is_empty() {
            out.push(abs);
        }
    }
    out
}

/// [`next_toc_urls`] 的规则求值阶段：一次解析出全部候选页地址
///
/// 形态分派：
/// 1. `@get:` / `{{}}` 先展开（与 field_url_with_vars 同一套；JS 规则不做 `{{}}`
///    展开，避免误伤 JS 源码）；
/// 2. 直接 URL / 根相对路径 → 单值（legado isUrl 直判）；
/// 3. CSS 选择器不含 `@`（legado 元素 text() 回退形态）→ 走原单值路径；
/// 4. 其余取**全量**结果（AR2 全量语义：getStringList 而非 getString0），
///    每条结果再按「JSON 数组 / 换行」拆成多个 URL。
fn next_toc_url_candidates(
    context: &str,
    rule: &str,
    base: &str,
    vars: &mut crate::parser::rule::RuleVars,
) -> Vec<String> {
    // legado makeUpRule：@get 先替换（引用已存变量）
    let resolved = crate::parser::rule::resolve_get(rule, vars);
    let trimmed = resolved.trim();
    if trimmed.is_empty() {
        return vec![];
    }
    let is_js = crate::parser::rule::contains_js_marker(trimmed);
    let expanded = if is_js {
        trimmed.to_string()
    } else {
        crate::service::search::expand_embedded_with_vars(trimmed, context, vars)
    };
    let t = expanded.trim();
    if t.is_empty() {
        return vec![];
    }
    // URL 型直判：完整 URL / 根相对路径（`//` 开头是 XPath，不在此列）
    if t.starts_with("http://") || t.starts_with("https://") {
        return vec![t.to_string()];
    }
    if t.starts_with('/') && !t.starts_with("//") {
        return vec![to_abs(t, base)];
    }
    // 无 `@` 的裸 CSS 选择器：legado 走元素 text()（本仓由 field_url_impl 的
    // visible_text 回退实现）——保持原单值路径，不在此重复实现文本提取
    if !is_js {
        let parsed = parse_rule(t);
        if parsed.kind == RuleKind::Css && !parsed.body.contains('@') {
            let v =
                crate::service::search::field_url_with_vars(context, Some(rule), "", base, vars);
            return if v.trim().is_empty() { vec![] } else { vec![v] };
        }
    }
    // 全量求值：CSS 多命中 / JSONPath 多命中 / JS 返回数组
    // （eval 出口已按 JSON.stringify 序列化，如 `["https://x/p2","https://x/p3"]`）
    let mut out = Vec::new();
    for v in apply_with_vars(t, context, vars) {
        out.extend(split_url_list(&v));
    }
    out.into_iter()
        .map(|u| to_abs(u.trim(), base))
        .filter(|u| !u.trim().is_empty())
        .collect()
}

/// 规则命中值 → URL 列表
///
/// - JS 返回数组：eval 出口按 JSON.stringify 序列化为 `["u1","u2"]` → 按 JSON 数组展开
/// - 数组元素/普通值可能是换行拼接的多条（legado getStringList 的 join("\n") 语义）
///   → 继续按换行拆；不含换行的普通值即整条（单 URL 向后兼容）
pub(super) fn split_url_list(v: &str) -> Vec<String> {
    fn push_lines(s: &str, out: &mut Vec<String>) {
        for line in s.split('\n') {
            let line = line.trim();
            if !line.is_empty() {
                out.push(line.to_string());
            }
        }
    }
    let v = v.trim();
    let mut out = Vec::new();
    if v.starts_with('[') {
        // 解析成功即按数组语义返回（空数组 `[]` → 0 个 URL，不能退化成字面量 "[]"）
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(v) {
            for item in items {
                if let serde_json::Value::String(s) = item {
                    push_lines(&s, &mut out);
                }
            }
            return out;
        }
    }
    push_lines(v, &mut out);
    out
}

/// 目录抓取主体: 有界并发抓页 + **严格保序**处理（按页序 → 页内序合并）
///
/// 并发只影响「谁先抓回来」，不影响合并顺序：每页结果按页序索引落到 ready 表，
/// 主循环永远只处理当前队首页，所以最终目录与串行抓取逐条一致。
async fn analyze_toc_impl(
    ns: &str,
    toc_url: &str,
    source: &BookSource,
    max_pages: usize,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<BookChapter>> {
    let mut all: Vec<BookChapter> = Vec::new();
    let toc_t0 = std::time::Instant::now();
    let toc_budget = std::time::Duration::from_secs(toc_budget_secs());
    let concurrency = toc_concurrency();
    let mut reverse = false;
    // legado Book.putVariable：详情（getBookInfo）写入的变量在目录/正文流程共享
    // P1 双键合并：book_url 级作底、toc_url 级覆盖
    let mut vars =
        crate::parser::rule::load_book_vars_merged(ns, &source.book_source_url, book_url, toc_url);
    vars.book_name = book_name.map(str::to_string);

    // ---- 抓取泵 ----
    // pending: 已发现但未发起的页地址; inflight: 在飞请求(≤ concurrency);
    // ready: 已抓完但还没轮到处理的页(按页序索引 → 完成顺序不影响合并顺序)
    let mut pending: std::collections::VecDeque<(usize, String)> =
        std::collections::VecDeque::new();
    let mut ready: std::collections::BTreeMap<usize, Result<crawler::FetchResponse>> =
        std::collections::BTreeMap::new();
    let mut inflight: tokio::task::JoinSet<(
        usize,
        std::time::Instant,
        usize,
        Result<crawler::FetchResponse>,
    )> = tokio::task::JoinSet::new();
    if max_pages > 0 {
        pending.push_back((0, toc_url.to_string()));
    }
    // 两个独立游标（多 URL 扇出必须分开，否则页序错位丢页）：
    // - head: 下一个待**处理**的页序（成功/跳过一页就 +1，严格递增）
    // - next_idx: 下一个待**分配**的页序（每次入池 +1）
    // 单 URL 链式翻页时两者恒相等；多 URL 时一次可入池 N 页，head 只 +1。
    let mut head = 0usize;
    let mut next_idx = 1usize;
    let mut pages_done = 0usize; // 成功合并的页数
    let mut failures_in_row = 0usize; // 连续失败页数（重试后仍失败）
    let mut stop_reason: Option<String> = None;
    // 已入池/已抓取的页地址（绝对 URL 精确匹配）：同一 URL 只抓一次。
    // 多 URL 规则常把「当前页」也列进页码集合（option@value 全套页码），靠它避开自抓。
    let mut seen_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen_urls.insert(toc_url.to_string());
    // 诊断: 在飞峰值 / 抓取耗时累计 / 页内解析落盘耗时累计（RUST_LOG=debug 可见）
    let mut max_inflight = 0usize;
    let mut fetch_ms_sum = 0u128;
    let mut parse_ms_sum = 0u128;

    loop {
        // ① 预算：到点后不再发起新页(已拿到的页照常产出)
        let remaining = toc_budget.saturating_sub(toc_t0.elapsed());
        if remaining.is_zero() && (!pending.is_empty() || !inflight.is_empty()) {
            stop_reason = Some(format!("预算 {}s 用完", toc_budget.as_secs()));
            break;
        }
        // ② 入池：按并发上限发起页请求（同时最多 concurrency 个在飞）
        while inflight.len() < concurrency {
            let Some((idx, url)) = pending.pop_front() else {
                break;
            };
            let ns_owned = ns.to_string();
            let src_owned = source.clone();
            inflight.spawn(async move {
                let t_fetch = std::time::Instant::now();
                let r = fetch_toc_page_retry(&ns_owned, &url, &src_owned).await;
                (idx, t_fetch, url.len(), r)
            });
            max_inflight = max_inflight.max(inflight.len());
        }
        if inflight.is_empty() {
            break; // 没有待抓地址也没有在飞请求 → 目录抓完
        }
        // ③ 等队首页(其余页先回来就先进 ready 等着，顺序不变)
        let mut wait_abort = false;
        while !ready.contains_key(&head) && !wait_abort {
            tokio::select! {
                joined = inflight.join_next() => match joined {
                    Some(Ok((idx, t_fetch, _, r))) => {
                        tracing::debug!(
                            "目录页抓取完成 #{idx} 耗时 {}ms status={} 在飞={} 队列={} [{toc_url}]",
                            t_fetch.elapsed().as_millis(),
                            r.as_ref()
                                .map(|resp| resp.status.to_string())
                                .unwrap_or_else(|_| "ERR".to_string()),
                            inflight.len(),
                            pending.len()
                        );
                        fetch_ms_sum += t_fetch.elapsed().as_millis();
                        ready.insert(idx, r);
                    }
                    Some(Err(e)) => {
                        stop_reason = Some(format!("目录页抓取任务异常: {e}"));
                        wait_abort = true;
                    }
                    None => wait_abort = true,
                },
                _ = tokio::time::sleep(remaining) => {
                    stop_reason = Some(format!("预算 {}s 用完", toc_budget.as_secs()));
                    wait_abort = true;
                }
            }
        }
        let Some(page) = ready.remove(&head) else {
            break;
        };
        let resp = match page {
            Ok(r) => r,
            Err(e) => {
                failures_in_row += 1;
                // 链式翻页的下一页地址只能从本页解析出来——本页(重试 1 次后仍)失败即链断，
                // 没有别的待抓页就只能收尾；若 pending 里还有已知页，则连续失败 2 页才收尾，
                // 避免对已经不稳的源站无谓放大压力。
                if failures_in_row >= 2 || pending.is_empty() {
                    stop_reason = Some(if failures_in_row >= 2 {
                        format!("连续 {failures_in_row} 页抓取失败: {e:#}")
                    } else {
                        format!("第 {} 页抓取失败: {e:#}", head + 1)
                    });
                    break;
                }
                tracing::warn!(
                    "目录第 {} 页抓取失败(重试后仍失败, 连续 {failures_in_row} 页), 跳过继续: {e:#}",
                    head + 1
                );
                head += 1; // 只跳处理游标；next_idx 不动（该页序已作废）
                if head >= max_pages {
                    break;
                }
                continue;
            }
        };
        failures_in_row = 0;
        let t_parse = std::time::Instant::now();
        // legado WebBook.getChapterList：目录页抓取后执行 loginCheckJs
        let page_body = apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;
        let base = resp.url.clone();
        // 本页已抓 → 记入去重集（规则把当前页列进页码集合时不再自抓；重定向后以真实 URL 记）
        seen_urls.insert(base.clone());
        // E10/AR5：真实页 URL → JS 求值绑定 baseUrl（push_js_context 透传）
        vars.insert("baseUrl".to_string(), base.clone());
        let rule: TocRule = source
            .rule_toc
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let Some(list_rule) = rule.chapter_list.clone() else {
            break;
        };
        // legado 目录列表前缀：`-` = 全部目录倒序；`+` = 仅去前缀
        let (list_rule, page_reverse) = crate::service::search::strip_list_rule_prefix(&list_rule);
        reverse |= page_reverse;

        // legado init：目录上下文提取（每页应用）
        let mut page_html =
            crate::parser::rule::apply_init_with_vars(&page_body, rule.init.as_deref(), &mut vars);
        // legado preUpdateJs：目录解析前 JS 预处理（result=抓取内容）
        if let Some(js) = &rule.pre_update_js {
            if !js.trim().is_empty() {
                vars.insert("result".to_string(), page_html.clone());
                page_html = crate::parser::js::eval_js(js.trim(), &vars).unwrap_or(page_html);
            }
        }
        // E10（目录路径补全）：src 绑定 = 当前页预处理后文档
        vars.insert("src".to_string(), page_html.clone());
        let items = toc_items(&list_rule, &page_html);
        let start_index = all.len() as i64;
        let chapters = chapters_from_items(&items, &rule, &base, start_index, &mut vars);
        // 正文流程只带章节 URL——按章节 URL 再存一份，保证 getBookContent 命中 (批量单事务)
        let urls: Vec<String> = chapters.iter().map(|ch| ch.url.clone()).collect();
        crate::parser::rule::save_book_vars_batch(ns, &source.book_source_url, &urls, &vars);
        all.extend(chapters);
        pages_done += 1;
        parse_ms_sum += t_parse.elapsed().as_millis();

        // 多页目录：nextTocUrl 一次可解析出**多个**页地址（legado 多 URL 语义——
        // `option@value` / `a@href` 一次就是全套页码，`@js:` 可直接返回 URL 数组）。
        // 提取顺序即页序：逐个分配严格递增的页序，保证合并顺序与串行逐条一致。
        let next_urls = rule
            .next_toc_url
            .as_deref()
            .map(|r| next_toc_urls(&page_body, r, &base, source, ns, &mut vars))
            .unwrap_or_default();
        if next_urls.is_empty() {
            // 链尾：本页没有下一页地址。多 URL 扇出时可能还有已入池的兄弟页在等，
            // 只有「无待抓且无在飞」才算目录抓完。
            head += 1;
            if pending.is_empty() && inflight.is_empty() {
                break;
            }
            continue;
        }
        let mut queued = 0usize;
        tracing::debug!(
            "目录页 #{} 解析出 {} 个页地址（逐个去重后入池）[{toc_url}]",
            head + 1,
            next_urls.len()
        );
        for next_abs in next_urls {
            // 同一 URL 只抓一次（规则常把「当前页」也列在页码集合里）
            if !seen_urls.insert(next_abs.clone()) {
                continue;
            }
            if next_idx >= max_pages {
                break;
            }
            crate::parser::rule::save_book_vars(ns, &source.book_source_url, &next_abs, &vars);
            // ④ 地址已知 → 立刻入池（与当前页后续处理/下一轮解析重叠）
            pending.push_back((next_idx, next_abs));
            next_idx += 1;
            queued += 1;
        }
        // 本页处理完毕 → 处理游标前进（与入池游标分离，多 URL 才不会错位）
        head += 1;
        if queued == 0 && pending.is_empty() && inflight.is_empty() {
            break; // 解析出的地址都抓过了（或超出 max_pages）→ 目录已完整
        }
    }

    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        toc_url,
        &vars,
    );
    // legado：多页目录汇总后去重（LinkedHashSet 保序）；`-` 前缀时最终列表倒序
    let mut all = dedupe_chapters(all);
    if reverse {
        all.reverse();
    }
    for (i, ch) in all.iter_mut().enumerate() {
        ch.index = i as i64;
    }

    // 诊断汇总（RUST_LOG=debug 可见）：页数 / 在飞峰值 / 抓取与解析耗时占比
    tracing::debug!(
        "目录抓取汇总 [{toc_url}]: 页数={pages_done} 并发上限={concurrency} 在飞峰值={max_inflight} \
         抓取累计={fetch_ms_sum}ms 解析累计={parse_ms_sum}ms 总耗时={}ms 停止原因={stop_reason:?}",
        toc_t0.elapsed().as_millis()
    );
    if let Some(reason) = stop_reason {
        if all.is_empty() {
            return Err(anyhow::anyhow!(
                "目录抓取失败(预算 {}s, 已抓 {} 页): {reason}; 源响应过慢, 可点刷新目录重试",
                toc_budget.as_secs(),
                pages_done
            ));
        }
        // 不假装成功：日志明确标注不完整（API 返回值形状不变，仍是章节数组；
        // 路由侧质量门(plausible_chapter_total)会拦住残缺目录写缓存/回写章数）
        tracing::warn!(
            "目录不完整（已抓 {}/{} 页, {reason}）[{toc_url}]：返回已抓到的部分目录, 共 {} 章",
            pages_done,
            next_idx,
            all.len()
        );
    }
    Ok(all)
}

/// 单页目录解析（ruleToc 应用一次——getChapterListByRule 调试接口复用）
pub async fn parse_toc_page(
    ns: &str,
    url: &str,
    source: &BookSource,
    book_name: Option<&str>,
    book_url: &str,
) -> Result<Vec<BookChapter>> {
    let resp = fetch_url(ns, url, source).await?;
    let page_body = apply_login_check_js(ns, source, &resp.body, &resp.url, None).await;
    let base = resp.url.clone();
    // P1 双键合并：book_url 级作底、当前页级覆盖
    let mut vars =
        crate::parser::rule::load_book_vars_merged(ns, &source.book_source_url, book_url, url);
    vars.book_name = book_name.map(str::to_string);
    // E10/AR5：真实页 URL → JS 求值绑定 baseUrl
    vars.insert("baseUrl".to_string(), base.clone());
    let rule: TocRule = source
        .rule_toc
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let Some(list_rule) = rule.chapter_list.clone() else {
        return Ok(vec![]);
    };
    let (list_rule, reverse) = crate::service::search::strip_list_rule_prefix(&list_rule);
    let mut page_html =
        crate::parser::rule::apply_init_with_vars(&page_body, rule.init.as_deref(), &mut vars);
    if let Some(js) = &rule.pre_update_js {
        if !js.trim().is_empty() {
            vars.insert("result".to_string(), page_html.clone());
            page_html = crate::parser::js::eval_js(js.trim(), &vars).unwrap_or(page_html);
        }
    }
    let items = toc_items(&list_rule, &page_html);
    let chapters = chapters_from_items(&items, &rule, &base, 0, &mut vars);
    let urls: Vec<String> = chapters.iter().map(|ch| ch.url.clone()).collect();
    crate::parser::rule::save_book_vars_batch(ns, &source.book_source_url, &urls, &vars);
    crate::parser::rule::save_book_vars_two_level(
        ns,
        &source.book_source_url,
        book_url,
        url,
        &vars,
    );
    let mut chapters = dedupe_chapters(chapters);
    if reverse {
        chapters.reverse();
    }
    for (i, ch) in chapters.iter_mut().enumerate() {
        ch.index = i as i64;
    }
    Ok(chapters)
}

/// chapterList 规则 → 章节上下文列表（CSS/JSONPath/Regex/JS 全类型）
pub(crate) fn toc_items(list_rule: &str, body: &str) -> Vec<String> {
    let parsed = parse_rule(list_rule);
    let mut items: Vec<String> = match parsed.kind {
        RuleKind::Css => css_chain(list_rule, body),
        RuleKind::JsonPath | RuleKind::Regex => apply(list_rule, body),
        RuleKind::Js => js_chapter_items(list_rule, body),
        _ => vec![],
    };
    // <js> 包裹形式（parse_rule 不识别为 Js）——兜底
    if items.is_empty()
        && (list_rule.contains("<js>") || list_rule.trim_start().starts_with("@js:"))
    {
        items = js_chapter_items(list_rule, body);
    }
    items
}

/// JS chapterList（<js> 或 @js:——eval 返回章节对象数组）→ 每项 JSON 文本
/// （数组经递归 JSON 转换——避免 ToString 的 "[object Object]" 使解析为空）
fn js_chapter_items(rule: &str, body: &str) -> Vec<String> {
    let code = if rule.trim_start().starts_with("@js:") {
        rule.trim_start()[4..].to_string()
    } else if let Some(start) = rule.find("<js>") {
        let rest = &rule[start + 4..];
        let end = rest.find("</js>").unwrap_or(rest.len());
        rest[..end].to_string()
    } else {
        return vec![];
    };
    let mut vars = std::collections::HashMap::new();
    vars.insert("result".to_string(), body.to_string());
    vars.insert("key".to_string(), String::new());
    vars.insert("page".to_string(), "1".to_string());
    let Ok(result) = crate::parser::js::eval_js_json(&code, &vars) else {
        return vec![];
    };
    match result {
        serde_json::Value::Array(list) => list
            .iter()
            .map(|item| match item {
                serde_json::Value::Object(_) => item.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect(),
        serde_json::Value::Object(_) => vec![result.to_string()],
        _ => vec![],
    }
}

/// 章节上下文列表 → 章节（字段规则应用 + 相对 URL 转绝对）
pub(super) fn chapters_from_items(
    items: &[String],
    rule: &TocRule,
    base: &str,
    start_index: i64,
    vars: &mut crate::parser::rule::RuleVars,
) -> Vec<BookChapter> {
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let title = crate::service::search::field_with_vars(
                item,
                rule.chapter_name.as_deref(),
                "",
                vars,
            );
            let url = match &rule.chapter_url {
                Some(r) => {
                    crate::service::search::field_url_with_vars(item, Some(r), "", base, vars)
                }
                None => String::new(),
            };
            if title.is_empty() && url.is_empty() {
                return None;
            }
            // legacy isVolume 规则优先；无规则时按标题特征判断
            let is_volume = match &rule.is_volume {
                Some(r) => {
                    let v = crate::service::search::field_with_vars(item, Some(r), "", vars);
                    is_true(&v)
                }
                None => title.starts_with("卷") || title.contains("【卷"),
            };
            // legacy：卷章节无 URL 用 标题+序号 占位；普通章节无 URL 用当前页 URL
            let url = if url.is_empty() {
                if is_volume {
                    format!("{title}{i}")
                } else {
                    base.to_string()
                }
            } else {
                to_abs(&url, base)
            };
            let mut title = title;
            // legacy isVip 命中 → 标题前加锁图标
            if let Some(vip_rule) = &rule.chapter_vip {
                let v = crate::service::search::field_with_vars(item, Some(vip_rule), "", vars);
                if is_true(&v) {
                    title = format!("\u{1F512}{title}");
                }
            }
            // legacy BookChapter.tag：目录规则 updateTime 的解析结果
            let tag = crate::service::search::opt_field_with_vars(
                item,
                rule.update_time.as_deref(),
                vars,
            )
            .filter(|s| !s.trim().is_empty());
            Some(BookChapter {
                title,
                url,
                tag,
                is_volume,
                index: start_index + i as i64,
            })
        })
        .collect()
}

/// 章节去重（legacy LinkedHashSet 语义）：按 (title, url, is_volume) 保序去重
pub(super) fn dedupe_chapters(chapters: Vec<BookChapter>) -> Vec<BookChapter> {
    let mut seen = std::collections::HashSet::new();
    chapters
        .into_iter()
        .filter(|c| seen.insert((c.title.clone(), c.url.clone(), c.is_volume)))
        .collect()
}

/// legacy `String?.isTrue()`：空白/`null`/`false`/`no`/`not`/`0` → false，其余 true
fn is_true(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("null") {
        return false;
    }
    !v.eq_ignore_ascii_case("false")
        && !v.eq_ignore_ascii_case("no")
        && !v.eq_ignore_ascii_case("not")
        && v != "0"
}
