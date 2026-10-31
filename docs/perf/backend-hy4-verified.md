# backend-hy4.md 交叉验证（复现 / 证伪 / 补强）

对象：`docs/perf/backend-hy4.md`（509 行，§0–§15）
验证时间：2026-11-01 01:1x–01:4x（Asia/Shanghai）
验证方式：同一生产后端 `http://127.0.0.1:4396`、同一生产库 `file:/home/reader/storage/reader.db?mode=ro` 只读；HTTP 用 `http.client` 进程内复用连接（**不复用 curl 的进程启动开销**，因此与本文件里「提案数」的差值本身也是一条结论）；SQL 用 python sqlite3 单连接 n=10~50 取中位/p95；Rust 解析用 `.tmp/perf/pbench`（release + lto）直接跑真实 `chapters_json`。

全部数字均可在 `.tmp/perf/` 的脚本里原样复跑（脚本清单见 §5）。

---

## 0. 测量口径差异（先说明，后面不再重复）

| 项 | 提案（curl） | 本验证（http.client 复用连接） | 说明 |
|---|---|---|---|
| `/health` | 12.1 ms | **1.19 ms**（p95 2.22） | 差 10.9 ms = curl 进程启动 + 连接建立 |
| `getBookshelf` | 13.9 ms | **5.18 ms**（p95 69.75） | 同上 |
| `getChapterList`(kushuge) | 18.8 ms | **21.71 ms**（p95 68.38） | 去掉进程开销后反而更高：提案那次是热态单发 |

→ 提案 §0 表格「p50/p95 被 curl 起进程（≈10 ms）主导」这句**成立**，但**不能把它当理由去低估 SQL 侧数字**：§2/§5a/§11 三条大偏差（详见下）不是测量口径问题，是**把 `user_namespace` 写成了 `'default'`**。

**核心证伪点（一句话）**：生产库里 `testuser01` 才是数据所在命名空间——`toc_cache` 1346 行、`book_vars_cache` 990 226 行、`source_stats` 1698 行全在 `testuser01` 下；`default` 在这三张表上分别是 **0 / 0 / 0 行**。提案里所有「0.011 ms 级」结论几乎都来自 `ns='default'` 的空结果集，于是把一个 **493 ms** 的查询写进了「已核查不成立」表（§11）。

---

## 1. 逐条裁决表

| # | 原提案（要点 + 原数字） | 结论 | 证据（实测数字 + file:line） | 改法（file:line + 动作） |
|---|---|---|---|---|
| §0 | 基线表：/health 12.1、getBookshelf 13.9/8.7KB、getChapterList 18.8/201.8KB、getBookContent 回源 2959.9/6033 | **修改** | `/health` 1.19ms、`getBookshelf` 5.18ms/8453B、`getChapterList`(kushuge) 21.71ms/**201 849 B**；getBookContent 无 `bookUrl` 时 **6561 ms**（正文缓存键含 book_url，见 `api/router.rs:4746-4768`）。绝对值吻合，固定开销 ~11ms 应显式扣掉 | 文档级：基线表标注「含 curl 进程开销 ~11ms」，或改用长连接客户端 |
| §1 | `get_toc_cache` 按 `toc_url` 查但主键是 `(book_url,user_namespace)` → 全表扫；现写法 1.504ms、改后 0.004ms，每次省 1.50ms | **采纳（数字修正）** | `storage/mod.rs:3527-3542` EQP=`SCAN toc_cache`。实测：命中 1.9MB 行 **8.735**、kushuge 166KB **2.435**、roushuwu 47KB **1.993**、未命中 **2.339**；建 `idx_toc_cache_url(toc_url,user_namespace)` 后 → **5.467 / 0.680 / 0.114 / 0.011**。净省 **1.7–3.3 ms**；**「改后 0.004 ms」不可达**——索引只免掉扫表，仍要读 overflow 页上的 blob | `storage/mod.rs:609-624` 建表处加 `CREATE INDEX IF NOT EXISTS idx_toc_cache_url ON toc_cache(toc_url, user_namespace)`；验证：`python3 .tmp/perf/v5_toc.py`（改前后各跑 50 次比对中位） |
| §2a | `get_book_sources` 全表扫+临时 B-tree；实测 1.965 ms；「每次请求必经」；把 ORDER BY 下移 Rust，省 1.0–2.0 ms | **修改（数据不符）** | `storage/mod.rs:1287-1327` EQP=`SCAN book_sources`+`TEMP B-TREE`。实测**同一 SQL 在 testuser01（615 行）：带 ORDER **44.74 ms** / 不带 **30.76 ms**；取单列 `book_source_url` 仅 4.5 ms、`count(*)` 0.11 ms → 成本在 materialize 615 行 ×44 列（raw_json 共 2.64 MB），不在扫描。HTTP 侧 `getBookSources?simple=1` **117.08 ms**（615 源），`ns=default`（3 源）仅 **7.90 ms**。**1.965 ms 差 ~55×，与 `ns='default'` 的 3 行一致**。「每次请求必经」也不成立：`getChapterList` 命中缓存在 `api/router.rs:4129-4148` 直接 return，根本没走到 `resolve_book_source_for_url` | 删 `ORDER BY`（`storage/mod.rs:1294`）——`storage/mod.rs:1314-1320` 已有同义 `merged.sort_by(weight desc, custom_order, name)`，语义等价，**实测省 14 ms**（p95 更明显）；验证：`python3 .tmp/perf/v8_sources.py` + `getBookSources?simple=1` 打 10 次比对 |
| §2b | 加 `idx_book_sources_ns_enabled(user_namespace,enabled)` 与 `idx_book_sources_url(user_namespace,book_source_url)`；称 `find_book_source` 5.612→省 5.6 ms | **驳回** | `find_book_source`（`storage/mod.rs:1341-1360`）条件是 `book_source_url = ?2 OR book_source_url LIKE ?3`，`?3` 为**前导通配** `'%/{tail}'` → 任何以 `book_source_url` 为前导列的索引都用不上；实测该 SQL 未命中 3.27 ms / 命中 3.50 ms（只 materialize 1 行，本来就便宜）。HTTP 侧 `getBookSource?url=` 命中 **9.48** / 未命中 **10.52 ms**（其中 ~1.2 ms 是连接与序列化）。「省 5.6 ms」基数不存在。`(user_namespace,enabled)` 对 `SELECT *`+按 weight 排序无帮助（enabled 不在排序列上） | 不改。若将来要动，唯一有效形态是 `(user_namespace, weight DESC, custom_order, book_source_name)` 让排序走索引（副本实测 65.77→**32.23 ms**，p95 139.94→38.67），但与 §2a 删 ORDER BY 等效且更贵，二选一即可 |
| §3 | `getBookContent` 为取 1 个 index 把整份 toc JSON 解析 + O(n) 扫；Rust 侧「10–30 ms/次」；改：LRU / 明细表 / 删 `9089-9100` | **修改** | `lookup_toc_chapter_ctx`=`api/router.rs:4911-4939`。用真实 JSON 跑 Rust（release+lto，`.tmp/perf/pbench`）：kushuge 1428 章 **Vec\<Value\> 2.557 ms** / typed **1.185 ms**；xbotaodz 1749 章 3.145/1.404；roushuwu 400 章 0.728/0.440；**aixiashu 16 896 章 46.027/15.557**。→ **书架书只有 0.4–3.1 ms，提案高估 4–12×**；只有万章级目录才到几十 ms（与提案 python 110 ms 的比值符合 2.4×）。第三处 `api/router.rs:9083-9100`：`toc_len = count_book_chapters()`（`storage/mod.rs` 内已有行）非 None 时**根本不进** `9089` 解析分支，所以「每次读 1.9 MB 解析后丢弃」只发生在**无 book_chapters 行**的书上 | ① 最小改：`api/router.rs:4918` 把 `Vec<Value>` 换成 typed `Vec<BookChapter>`（实测 **快 2.2–3.0×**，且顺手省掉 `as_str()` 取值）；② 再加缓存就复用现成骨架 `BOOK_INFO_CACHE`（`api/router.rs:3698-3762`，HashMap+VecDeque+TTL，**不引入 lru/dashmap 新依赖**），键带 `updated_at` 天然失效；③ 删 `api/router.rs:9089-9100` 无副作用（`let _ = toc_len;` 已是丢弃），但收益只覆盖无章节缓存的书 |
| §4 | `clear_toc_cache_for_book` 的 `LIKE '%/tail'` 四路 OR → 70.407/157.626 ms；改「LIKE 兜底改走 toc_url 索引前缀范围」 | **修改（改法不可行）** | `storage/mod.rs:3496-3521` 实测等价 SELECT 未命中 **67.12**/p95 92.31、命中 56.76/120.59；真实 DELETE（删 3 行）**338 ms + commit 65.67 ms**（WAL 下 DELETE 还要写页）。提案的「走 idx_toc_cache_url 前缀范围」**技术上不成立**：`?3 = format!("%/{tail}")` 是前导通配，索引无从 seek | 有效改法（副本实测 54.87→**0.96 ms**，p95 117.79→1.17）：建 `CREATE INDEX idx_toc_ns_url_book ON toc_cache(user_namespace, toc_url, book_url)`，让 `user_namespace=?1` 走 **COVERING INDEX** 后再在 Rust 里做 `ends_with("/tail")` 过滤，或精确匹配 `(book_url=?2 OR toc_url=?2)` 先走索引、只有 0 行命中时才跑 LIKE 兜底。验证：`python3 .tmp/perf/v3_sql.py`（tail 命中/未命中两组） |
| §5a | `search_book_multi` 把 `get_source_stats` 查两遍；实测 0.011 ms；改：复用一次 | **采纳（数字修正）** | `api/router.rs:2906` 与 `:2937` 两次 `get_source_stats`，后者守卫 `!include_dead && single_source_url.is_empty()`（默认路径必走）。实测 `storage/mod.rs:1862-1870`：testuser01 **1698 行 / 12.385 ms**（p95 37.2），不是 0.011 ms（0.011 是 `ns='default'`、0 行的结果）。×2 = **~25 ms/搜索请求**；但搜索墙钟 95 s，占比 0.03% | 照抄 SSE 侧写法（`api/router.rs:9382`）把 stats 提到外层复用，删 `:2937` 的重查。零风险，优先级按「代码卫生」而非性能 |
| §5b | `get_book_toc` 的 `_shelf_for_write` 是死代码，白付 1–2 次 `find_book` | **采纳** | 确认：`api/router.rs:4161-4175` 只写不读；真正用的是 `:4249-4253` 的 `shelf_for_write`（内部 `:4251` 再查一次 `find_book(ns,url_param)`）。`find_book` 走主键（`storage/mod.rs:3289-3298`，实测 0.010–0.2 ms）。且只在 cache-miss 路径（命中在 `:4129-4148` 早退） | 删 `api/router.rs:4161-4175` 整块。验证：`cargo test --lib` + 打 10 次 `getChapterList?refresh=1` 比中位 |
| §5c | `get_book_info` 用全表 `list_books` 找 1 行 | **采纳（低优）** | `api/router.rs:3785` 对所有 getBookInfo 请求执行 `list_books`（`storage/mod.rs:5017-5028`，`SCAN books`+`ORDER BY order_num, MAX(...)`），缓存判定在其后 `:3834`。实测 getBookInfo 命中缓存 **10.0 ms**（kushuge）/ **4.0 ms**（xbotaodz），5 本书时 `list_books` 只占 <1 ms | `api/router.rs:3785` 改懒求值：先 `find_book(ns,&url)`（主键），只在命中且 `is_local_book` 时才走书架分支。书架增长后线性收益 |
| §6 | 无跨请求全局并发上限；conc1 58.9 ms → conc20 449.4 ms（7.6×）「线性退化=排队」；改 ①`tower::limit::ConcurrencyLimitLayer` ②`clamp(1,128)`→`clamp(1,32)` | **驳回（诊断不成立）** | 同一并发脚本实测（进程内线程，非 curl）`getChapterList`：conc1 **27.2** → conc4 35.4 → conc8 46.2 → conc16 81.0 → conc20 **138.7** ms（5.1×）；**对照组 `/health`（不碰 DB、不碰 sqlx）：conc1 1.8 → conc20 9.8 ms（5.4×）**。两者退化比例一致 → 退化来自 **4 核上的 axum/hyper/Tokio 栈争用**，不是 DB 排队；DB 池 32 路（`storage/mod.rs:255-266`）在 20 并发下根本没触及。提案的 449 ms 还叠加了「20 个 curl 进程同起」的 fork/exec 开销。另：① 现状**编译不过**——`backend/Cargo.toml` 的 `tower = "0.5"` 未启 `limit` feature | 全局限流**不做**（只会把排队换成 503，不降延迟）。② `concurrentCount` 是**出站 HTTP fanout**（每任务主要耗时在网络），`api/router.rs:9282-9285` 的 `clamp(1,128)` 收成 `clamp(1,32)` 属可用性护栏（防单客户端 128 路外连），保留但归为「低优·非性能项」 |
| §7a | `COOKIE_STORAGE` 是 `std::sync::Mutex`，持锁期间 `.await` 阻塞整个 worker；`LOAD_LOCK` 也是 std Mutex，全进程一把卡热路径 | **驳回（两处事实错误）** | ① `service/crawler.rs:1189/1212/1226/1242/1256/1272` 全是 `COOKIE_STORAGE.lock().unwrap_or_else(..).clone()?` —— **guard 是语句级临时量，`let` 语句结束即 drop，`.await` 在下一行**，不存在持锁跨 await（clippy `await_holding_lock` 也不会触发）；锁内动作 = 克隆一个 `Storage` 指针（纳秒级）。② `storage/mod.rs:1928-1933` 的 `LOAD_LOCK` 是 **`tokio::sync::Mutex`**（提案写的 std::sync::Mutex 是错的），本来就是 async 挂起而非阻塞 worker；快路径 peek 在 `storage/mod.rs:1922` 已先于取锁。锁内那条 SQL 在 testuser01 实测 **4.75 ms**（1620 行 ×4 列文本，`SEARCH … USING autoindex (user_namespace=?)`），**不是提案的 0.009 ms**（又一个空 ns）——但 10 s TTL + single-flight（`storage/mod.rs:1934-1941` 双检）把它的执行频率压到每 ns 每 10 s 一次，且锁是 async 的，因此结论仍是「不改」 | 不改。唯一可选：`crawler.rs:1647/1649/1677` 三次调用合成一次 `cookie_snapshot`（快照本身已含 cookie/ua/login_header 三字段），省 2–3 次 `Arc` clone；属洁癖级，收益 < 0.1 ms |
| §7b | `BOOK_INFO_CACHE` 是 `std::sync::Mutex`，判定安全非瓶颈 | **采纳（判定正确）** | `api/router.rs:3740-3762`：`book_info_cache_get/put` 都是 **sync fn**，锁内无 await，容量 200 / TTL 10 min，`unwrap_or_else(|e| e.into_inner())` 处理中毒。确认安全 | 不改；但建议把它**提取成通用骨架**给 §3 ②与 §12-0 复用（见 §4 补强） |
| §8 | PRAGMA 缺失：`synchronous=NORMAL` / `cache_size=-65536` / `temp_store=MEMORY` / `mmap_size=256MB` / `PRAGMA optimize` | **采纳（收益需分项，且有一项无效）** | 当前值与提案表**完全一致**（`storage/mod.rs:258-268` 只设 WAL+busy_timeout）：journal=wal、synchronous=**2(FULL)**、cache_size=**-2000**、temp_store=0、mmap_size=0。实测（同盘 /dev/sda2、WAL、200 次 autocommit，两轮）：**FULL 2.65–3.09 ms/commit → NORMAL 0.06–0.09 ms/commit（34–44×）**；`cache_size=-65536` 对 `book_sources` 全表扫 64.62→36.60 ms（有效）；`mmap_size=256MB` 对 `book_sources` 全扫 **无收益**（47.42 vs 44.74），对 1.9 MB 单行 blob 读 **8.06→6.24 ms（−23%）**（有效，但需配合 §1 索引才有意义）。`temp_store=MEMORY` 只影响排序临时表 → 与 §2a 删 ORDER BY 重复 | `storage/mod.rs:258-268` options 链加 `.synchronous(SqliteSynchronous::Normal)`（**最值钱的单条**：每个内容请求的 `save_book_vars` 落库、每章缓存写入都吃这 3 ms）；`pragma("cache_size","-65536")`；`pragma("mmap_size","268435456")` 只在读多写少路径有收益，建议单独验证后再开；`temp_store`/`PRAGMA optimize` 略过。回归：`cargo test --lib storage::` + 一次 `getChapterList?refresh=1` 全流程（验证 NORMAL 下 WAL checkpoint 正常） |
| §9 | 表增长：book_vars_cache 1 018 747 行/680 MB、toc_cache 1443/94.6 MB、book_chapters 3627/83 MB、search_cache 1770/25 MB；建议加 TTL 清理 | **修改（数字基本对，风险判断对，收益被夸大）** | dbstat：`book_vars_cache` **714 407 936 B**（提案 680 MB，≈同）、`toc_cache` 118 329 344 B（payload `sum(length(chapters_json))`=99 202 786 B）、`book_chapters` 87 465 984 B（**3834 行**，提案 3627 有漂移）、`search_cache` 27 258 880 B。提案对 `book_vars_cache` 的风险判断（`@get:{key}` 依赖）**正确**，但「库 1.0 GB → 300 MB → §1/§4 全表扫描常数变小」不成立：§1 索引后已不扫表，§4 的 LIKE 与表大小无关（覆盖索引） | 只做 `toc_cache` 字节封顶（保留 N 天/200 MB），`book_vars_cache` 不动。**注意**：`book_vars_cache` 一旦加 `(user_namespace,url)` 索引（见 §4 补强 N1），写入放大 ≥1 个索引，需与 §9 一起评估 |
| §10 | 大 JSON 无 ETag/304；`skip_serializing_if` 省 40%；`book_json_with_group_ids` 二次 serde | **修改（一项驳回）** | ① `getChapterList`(kushuge)=**201 849 B** ✓、`getBookshelf`=**8453 B** ✓。② **`skip_serializing_if` 数据不符**：5 本书 240 个字段里显式 `null` 只有 **1353 B（16%）**，其余空值是 `""`/`[]`（`Option::is_none` 一个都省不掉）；且 gzip 已把 201 849→**29 789 B（14.8%）**，8 KB 的响应在 gzip 后 ~2 KB，收益接近 0。③ 仓库里 **`tower-http` 的 gzip 挂了两层**（`api/router.rs:99-106` 内层 + `src/lib.rs:172` 外层兜底），做 ETag 必须让 304 在压缩层之前短路、且 ETag 基于未压缩体，否则同一 URL 会因编码不同出现两个 ETag | ① 保留 ETag 但**必须配前端改**（`frontend/` 需发 `If-None-Match` 并按 304 复用缓存），单改后端收益为 0；实现要点：`get_toc_cache` 需额外返回 `updated_at`（`storage/mod.rs:3527` 现在只 select `chapters_json`）作为弱 ETag 的来源。② `skip_serializing_if` **驳回**（16% + 兼容风险）。③ 二次 serde（`api/router.rs:6986-7021`）在 5 本书上 <1 ms，不做 |
| §11 | 「已核查不成立」13 项（含 `list_urls_under_prefix` 0.011 ms、`get_source_stats` 0.011 ms、`list_cached_chapter_headers` 0.110 ms、`parser/js.rs block_on_task` 未验证） | **驳回（3 项误判 + 1 项漏判）** | 实测（testuser01）：**`list_urls_under_prefix` = 493.45 ms（p95 898.88）**，990 226 行，EQP `SEARCH … USING COVERING INDEX (user_namespace=?)` —— 只用到索引第一列，等于全扫；`get_source_stats` **12.385 ms**（不是 0.011）；`list_cached_chapter_headers` **2.26–4.06 ms**（不是 0.110，20–37×，行数从 380 涨到 793）。另：`block_on_task` 不是「未验证」而是**确定性慢**（见 N2）。误判根源同 §0：`ns='default'` 三张表都是 0 行 | 见 N1（改名 range seek）与 N2（去掉同步落库） |
| §12-#0 | `getCacheInfo` 加 10s TTL 进程内缓存，−555 ms/次 | **采纳** | HTTP 侧实测 **583.52 ms**（p95 762.25）；SQL 四条连跑 523.69 ms。理由与提案一致：统计只给 UI 看 | `api/router.rs:1939-1958` 包一层 TTL 缓存（复用 `BOOK_INFO_CACHE` 的 `LazyLock<Mutex<HashMap+Instant>>` 骨架，键=`ns`）；`clearCache` 路径主动失效。回归：连打 10 次，第 2 次起应 <5 ms |
| §12-4b | `cache_job` 逐章写入包进事务（照抄 `save_chapters`） | **采纳（收益被严重低估，从 P2 上调）** | `service/cache_job.rs:255-260` 确实在 `for h in handles { h.await → cache_chapter_content }` 里**逐章 autocommit**（`storage/mod.rs:2780-2795` 单条 `INSERT OR REPLACE` 在 `&self.pool` 上）；范例在 `storage/mod.rs:2652-2669`（`let mut tx = self.pool.begin()`）。实测同盘 500 行：**逐章 commit 2349.8 ms（4.70 ms/行）→ 单事务 15.3 ms（0.03 ms/行）**；200 行对照组两轮 901 ms（4.51/行）vs 21 ms（0.11/行）、697 ms（3.49/行）vs 5 ms（0.03/行），**43–139×** | `service/cache_job.rs:255-260` 改成「先收集 `(idx,title,content)`，再在 `storage` 加一个 `cache_chapter_contents_batch(&[(i64,String,String)])`（单事务，照抄 `save_chapters` 的形状，但保留 `user_namespace` 绑定）」。**500 章省 ~2.2 s**，且与 §8 的 `synchronous=NORMAL` 叠乘（NORMAL 下 4.5 ms→0.09 ms/commit）。回归：`cargo test --lib service::cache_job` + 对一个 400 章书跑 `?refresh=1` 计时 |
| §12-2b | getCacheInfo 4 条 SQL 合成 2 条（省 0.9 ms，仅在不做 #0 时用） | **采纳（作为 #0 的补充）** | 四条实测 0.24 / 313.24 / 0.70 / 249.16 ms —— 合并只能省两次往返（~0.9 ms），主体 562 ms 一分不少。提案自己的判断正确 | 与 #0 一起做：合并成 2 条（`SELECT COUNT(*), COALESCE(SUM(length(chapters_json)),0) …`），减少一次全扫的机会成本 |
| §12-5 | `concurrentCount` 上限 128 → 32 | **修改（降级为护栏）** | 见 §6：不影响延迟，只防单客户端 128 路外连；DB 池排队由 sqlx 自己处理（`storage/mod.rs:255-266` clamp 1..128），不会「打爆」 | 做，但归「可用性护栏」，不进性能收益表 |
| §12-12 | PRAGMA 三条 | **采纳（拆开）** | 见 §8：`synchronous` 34×（值钱）、`cache_size` 对全扫有效、`mmap_size` 只对大 blob 有效、`temp_store`/`optimize` 无效 | 先只上 `synchronous=NORMAL`（一条 line diff，收益最大、风险最低）；其余各自独立验证 |
| §13 | `get_cache_info` 4 条查询读 ~126 MB，实测 522 ms（p95 860） | **采纳（完全吻合）** | 生产实测：`#1 COUNT(*) toc_cache` 0.24、`#2 SUM(length(chapters_json))` **313.24**、`#3 COUNT(*) book_chapters` 0.70、`#4 SUM(length(content))` **249.16**；四条连跑 **523.69**（p95 715.5）；HTTP 侧 **583.52**。提案写 306.215/248.911/0.233/0.618、合计 522.18 → **误差 <2%**。EQP 亦一致（#2/#4 `SCAN`，必须读 blob 才能算 length） | 同 §12-#0。注意提案里「`default` 约 97 行 109.59 ms」这一句无法复现（`default` 在 `toc_cache` 是 **0 行**），不影响主结论 |
| §14 | 生产 token 会话中途失效（NEED_LOGIN），getCacheInfo 等未测到 HTTP 侧 | **驳回（环境差异，非后端问题）** | 本验证全程同一 token 有效（01:1x–01:4x 共 4 轮、数百请求），`getCacheInfo`/`getBookGroups`/`getBookshelf`/`getBookContent` 全部 200 且 `isSuccess:true`。推测是提案侧并发建连或多次重启后端导致 token_map 失效 | 无需改代码；建议后续基准统一用长连接客户端 + 单 token，避免把鉴权抖动写成结论 |
| §15 | 子代理 18 条复核（`get_cache_info` 升 P0、`save_chapters` 不绑 ns 驳回为误报、`cache_job` fsync 保留 P2 等） | **部分采纳** | 抽查成立：`book_chapters` 中 `user_namespace='default'` = **0 行**（「save_chapters 不绑 ns」确为误报判定正确）；`list_books` 5 本 0.133 ms（降级正确）；`cache_job` 机制描述**正确且被低估**（见 §12-4b）；`list_urls_under_prefix` 的「驳回」**错误**（493 ms） | 沿用 §15 的降级结论，但把 `cache_job` 从 P2 上调（§12-4b），把 `list_urls_under_prefix` 从「不成立」改为 N1 |

---

## 2. 提案未覆盖的新发现（补强）

### N1【P1】`list_urls_under_prefix` 实测 493 ms，被提案误判为 0.011 ms

- 位置：`storage/mod.rs:2709`（`SELECT url FROM book_vars_cache WHERE user_namespace=?1 AND url LIKE ?2 ESCAPE '\'`），唯一调用点 `api/router.rs:4049`（`toc_from_cached_chapters`，目录抓取失败时的本地兜底；同函数 `:4040` 还有一条 `list_cached_chapter_headers` = 2.26–4.06 ms）。
- 实测：testuser01 **990 226 行**，**493.45 ms / p95 898.88**（返回 2715 行；次日复跑命中涨到 2936 行，483.61 ms）。
- 为什么索引没救：`LIKE ?||'%'` 走的是**参数绑定**模式，SQLite 不做 LIKE 优化 → 计划退化为 `SEARCH … USING COVERING INDEX (user_namespace=?)`，等于扫 99 万条索引项。建 `idx_bvc_ns_url(user_namespace,url)` 后**仍然是** 604 ms（更慢，索引更大）。
- 有效改法（副本 1 021 242 行实测）：

```sql
-- 1) 建索引（1M 行实测 3.3–3.5 s；索引体积按 ~1M 条 × ~50 B 估算 50–60 MB）
CREATE INDEX IF NOT EXISTS idx_book_vars_ns_url ON book_vars_cache(user_namespace, url);
```

```rust
// storage/mod.rs:2709 把 LIKE 换成半开区间（不需要 LIKE 转义了）
// prefix_upper = prefix 末字节 +1，或直接拼接 '\u{FFFF}'（UTF-8 下大于任何 ASCII 续字节）
"SELECT url FROM book_vars_cache WHERE user_namespace = ?1 AND url >= ?2 AND url < ?3",
```
实测：**493.45 ms → 5.58 ms（88×）**，返回行数一致（2715）；EQP 变成 `SEARCH … USING COVERING INDEX (user_namespace=? AND url>? AND url<?)`。
二轮复跑（行数已涨到 2 936 命中）：**483.61 → 4.67 ms（105×）**，断言「返回行集合完全一致」通过。
回归：`python3 .tmp/perf/v9_new.py`（自带断言，`--skip-copy` 可跳过副本实验）。

> 注意：该表每章写一行（`book_vars_cache` 1M 行且持续增长），新索引会带来写放大；但写入本身已被 fsync 主导（4.5 ms/commit），+1 个索引可忽略。**若不做 N1，就不要给这张表加任何索引**（实测纯负收益）。

### N2【P1】每个 `getBookContent` 都做一次「新建 OS 线程 + 新建 Tokio runtime + 同步 fsync」

链路（全部实测确认）：

```
api/router.rs:4624  save_book_vars(ns, source, chapter_url, &ctx)      // 每个内容请求都执行
 └ parser/rule.rs:319-346  save_book_vars  → book_vars_db_write
    └ parser/rule.rs:223-248  block_on_task(fut, 10s, "bookVars.put")
       └ parser/js.rs:3123-3160  std::thread::Builder::spawn(...)      // 每次新建线程
                                  + tokio::runtime::Builder::new_current_thread()
                                  + rx.recv_timeout(timeout)           // 调用方线程被阻塞
          └ storage.put_book_vars_cache 单条 INSERT OR REPLACE
             └ synchronous=FULL → 每次 2.65–3.09 ms fsync（实测，见 §8）
```

- 同一请求里还有 `api/router.rs:4580 load_book_vars`（miss 时）走同一条 `block_on_task`。
- 后果有三层：① 每次内容请求固定多一次磁盘同步；② 每次调用新起一个 OS 线程 + 一个 runtime（~50–200 µs 建/销）；③ 调用方是 axum worker（4 核 = 4 个 worker），`recv_timeout` **真阻塞**，并发内容请求超过 worker 数即排队 —— 这才是「worker 被同步阻塞」的真实来源（提案 §7a 猜的是 `COOKIE_STORAGE`，指错了）。
- 改法（按性价比排序）：
  1. **`synchronous=NORMAL`**（§8）：3.09 → 0.09 ms/commit，一行配置消掉 96% 的代价。
  2. `api/router.rs:4624` 的 `save_book_vars` 只在 **ctx 相对缓存有变化** 时才写（比较内存中的 `BOOK_VARS_CACHE` 条目，`parser/rule.rs:341-346` 已经有 slot 比较的位置）；纯读章节（99% 的场景）应零写入。
  3. 中期：给 `book_vars_db_write` 换成 `tokio::spawn` + 有界队列（写入本就可丢弃，`rule.rs:222` 注释自称「失败仅降级为内存」），彻底离开 worker 线程。
- 回归：`getBookContent` 连打 10 次（正文已缓存）比中位；`cargo test --lib parser::rule`。

### N3【P0，最大单点】`getBookContent` 每请求白付 109 ms：正文缓存读在书源解析之后

- 顺序：`api/router.rs:4555-4560` 先 `resolve_book_source_for_url`（`api/router.rs:3254-3279`；无 `bookSource` 参数时 → `get_book_sources` 615 行 materialize ≈ 110 ms），正文缓存读在 **`api/router.rs:4746-4768`**（键 = `ns + book_url + md5(chapter_url)`，条件 `refresh<=0 && !book_url.is_empty()`）。
- A/B 实测（同一章、正文已在缓存）：不传 `bookSource` **151.1 ms**（p95 192.9）vs 传 `bookSource=https://www.kushuge.com`（走单行 `find_book_source`）**42.3 ms**（p95 65.9）→ **109 ms/请求**，全部是 `get_book_sources` 的 materialize。
- 为什么是安全的改动：正文缓存键只依赖 `ns`、客户端给的 `bookUrl`、`chapterUrl` 三样，**都不需要先解析书源**；命中后现有的早退分支（`:4746-4768` 命中即返回）本来就不需要 `source`（`save_reading_progress_if_shelf` 也只用 `find_book`+`toc_cache`）。
- 改法：把 `:4746-4768` 的缓存读（含 `cache://` 分支语义）**上移到 `:4555` 之前**，命中就 return；miss 再走 `resolve_book_source_for_url`。等价 diff 形状：

```rust
// api/router.rs:~4548（resolve_book_source_for_url 之前）
if refresh <= 0 && !book_url.is_empty() && !book_url.starts_with("local://") {
    let idx = crate::util::md5::chapter_url_hash(&chapter_url);
    if let Ok(Some(content)) = state.storage.get_chapter_content(&namespace, &book_url, idx).await {
        // 保持不变：进度回写 + 返回同一 JSON 形状
        save_reading_progress_if_shelf(&state, &namespace, &book_url, &chapter_url, &chapter_title_param).await;
        return Json(ReturnData::new(content));
    }
}
```
- 收益：**命中路径 151 → ~42 ms**（−72%），是本轮单条最大收益，且与 §8/N2 叠加后还能再降 ~3 ms。
- 风险：中低。要保证三条语义不变：① `refresh>0` 仍强制回源；② `bookUrl` 为空（老客户端只传 `chapterUrl`）时行为不变（现在就是 miss 后回源，实测 6561 ms）；③ 命中分支的 `chapterType`/`book_type` 处理与现状一致（当前早退分支已如此，照抄即可）。
- 回归：`getBookContent?chapterUrl=…&bookUrl=…` 连打 20 次比对中位（改前 ~151 ms → 改后应 ≤45 ms）；再测 `refresh=1` 与 `bookUrl` 空两条，确认仍回源。

---

## 3. 按实测收益重排的执行顺序（取代提案 §12 的排序）

| 新序 | 项 | 实测收益 | 风险 | 备注 |
|---|---|---|---|---|
| **A** | **N3 正文缓存前置** | **−109 ms/内容请求（−72%）** | 中低 | 单条最大；纯代码移动 |
| **B** | §8 `synchronous=NORMAL` | **每个写事务 −3.0 ms（34×）** | 低（缓存类数据可重建） | 一行配置 |
| **C** | §12-#0 `getCacheInfo` TTL | **−523 ms/次**（p95 −860） | 零 | 顺带把 §12-2b 合并 SQL 一起做 |
| **D** | §12-4b `cache_job` 批事务 | **−2.2 s/500 章** | 低 | 与 B 叠乘 |
| E | §2a 删 `ORDER BY` | **−14 ms/次** `getBookSources`（p95 更明显） | 零 | Rust 侧已有同义排序 |
| F | §1 `idx_toc_cache_url` | −1.7~3.3 ms/次 | 低 | 94 MB 表 DDL 放低峰 |
| G | §4 `idx_toc_ns_url_book` + Rust 端 `ends_with` | **−54 ms/次**（p95 −116） | 低 | 仅在章数变化时触发 |
| H | N1 `book_vars` 半开区间 + 索引 | **−488 ms**（失败兜底路径） | 中（写放大、1M 行索引） | 只影响源站失败的兜底 |
| I | §3-① 解析改 typed | 每次 −1.4 ms（万章书 −30 ms） | 低 | 顺带简化取值 |
| J | §5b / §5c / §5a / §12-5 | 合计 <1 ms + 2 次查询 | 零 | 代码卫生 |
| — | §10 ETag | 命中 304 时 −19~51 ms | 中（必须配前端 + 双层压缩） | 无前端改动则收益 0 |
| — | §6 全局限流 / §7a 锁重构 / §2b 索引 / §10 `skip_serializing_if` | 0 或负 | — | **驳回，不做** |

---

## 4. 一句话总结

提案的方向基本对（缓存命中路径的重复解析、PRAGMA、getCacheInfo、批量事务都是真问题），但**三条 SQL 数字（§2 / §5a / §11）是在 `ns='default'`（0 行）上测的**，导致 1 个 493 ms 的真问题被写进「不成立」，而 §2 的真收益（−14 ms，不是 −1~2 ms）被埋没；**§6 的并发诊断和 §7a 的锁诊断都是误判**（4 核 HTTP 栈争用 / guard 不跨 await）；同时**漏掉了两个更大单点**：正文缓存顺序（−109 ms/请求）与每请求同步落库（−3 ms/请求）。

---

## 5. 复现脚本（`.tmp/perf/`）

| 脚本 | 覆盖 |
|---|---|
| `v1_pragma.py` | PRAGMA 当前值 + dbstat 各表体积/页数 |
| `v2_sql.py` `/` `v3_sql.py` | 行数、ns 分布、`clear_toc_cache_for_book` 的 SELECT/DELETE 计时 |
| `v4_http.py` | 6 条主要接口的 HTTP 计时（http.client 复用连接） |
| `v5_toc.py` | `get_toc_cache` 命中/未命中计时 + 索引前后对比（用 `toccopy.db`） |
| `v6_content.py` `/` `v7_content2.py` | `getBookContent` 冷/热 + `bookUrl`/`bookSource` A/B（N3 的 109 ms） |
| `v8_sources.py` | `get_book_sources` 带/不带 ORDER BY 计时（§2a） |
| `v9_new.py` | **新增结论的一键复现**：N1（LIKE vs 半开区间，含行集合一致性断言）、§5a/§11 两条小查询、§8 `synchronous` FULL/NORMAL、§12-4b 逐章 commit vs 单事务。自带断言，跑完自删临时库 |
| `pbench/` | Rust 侧真实 toc JSON 解析（typed vs `Vec<Value>` vs 线性扫，§3） |
| 写基准 | 已并入 `v9_new.py`（`WAL`+`synchronous` 对照、逐章 commit vs 单事务，跑完自删临时库） |
| 并发基准 | `conc.py` / `conc2.py`（务必用进程内线程，不要用 curl，见 §0/§6） |
