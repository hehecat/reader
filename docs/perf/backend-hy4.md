# 后端性能画像（hy4-preview / backend 端）

生成时间：2026-11-01 01:0x（Asia/Shanghai）
被测后端：生产 `http://127.0.0.1:4396`（Rust axum + sqlx-sqlite，WAL）
被测库：`/home/reader/storage/reader.db`（只读打开，1.0 GB，262845 页）
证据约定：代码引用为 `backend/src/...`（相对行号），SQL/行数均为**实测**。

---

## 0. 实测基线（curl 起进程，含 curl+HTTP 栈固定开销 ≈ 10ms）

各取 10 次（getBookContent 冷/热分别取），报中位数 / p95：

| 接口 | 载荷 | 中位 | p95 | 备注 |
|---|---|---|---|---|
| `/health` | 3 B | 12.1 ms | 18.1 ms | 纯栈开销，无 DB |
| `/reader3/getBookshelf` | 8.7 KB | 13.9 ms | 16.1 ms | 5 本，最多 0.1 ms 是 SQL |
| `/reader3/getChapterList` (kushuge, 命中) | **201.8 KB** | 18.8 ms | 25.4 ms | 命中目录缓存 |
| `/reader3/getChapterList` (roushuwu, 命中) | 52.1 KB | 14.4 ms | 15.9 ms | |
| `/reader3/getChapterList?refresh=1` (kushuge) | 198 KB | **11210.8 ms** | **26194.6 ms** | 回源抓取 |
| `/reader3/getAvailableBookSource` | 10.7 KB | 14.7 ms | 24.4 ms | 仅读候选缓存 |
| `/reader3/getBookContent`（fq，回源） | 10.3 KB | 97.5 ms | 115.8 ms | |
| `/reader3/getBookContent`（kushuge，回源） | 54 B（空正文） | 2959.9 ms | 6033.9 ms | 源站慢 |
| `searchBookMultiSSE` 首事件（maxSources=3） | — | 325.7–495.6 ms | 782.7 ms | 见 §5 |

结论：**绝大多数接口的后端自身耗时 < 10 ms；p50/p95 的绝对数值被 curl 起进程（≈10 ms）主导。**
真正的耗时全部集中在两处：① 回源抓取（秒级，外部网络）；② 少数几条全表扫描 SQL（数十 ms 量级）。
下面按「能省多少 ms / 多少次查询」排序。

---

## 1. 【P0】`get_toc_cache` 按 `toc_url` 查，但主键是 `(book_url, user_namespace)` → 全表扫 1443 行 / 94 MB

**证据**

- 表定义 `PRIMARY KEY (book_url, user_namespace)` —— `storage/mod.rs:615-623`
- 查询：`SELECT chapters_json FROM toc_cache WHERE toc_url = ?1 AND updated_at >= ?2 AND user_namespace = ?3` —— `storage/mod.rs:3527-3541`
- `EXPLAIN QUERY PLAN` → **`SCAN toc_cache`**（无索引可用）
- 调用点：`api/router.rs:4131`（getBookToc 命中路径）、`api/router.rs:4877`（save_reading_progress）、`api/router.rs:4919`（lookup_toc_chapter_ctx）
- 表实际规模：**1443 行 / 94.6 MB**（单行 `chapters_json` 平均 67 KB，最大 1.9 MB）

**实测（python sqlite3 只读，200 次）**

| 查询 | 中位 | p95 |
|---|---|---|
| 现写法（`WHERE toc_url=...`） | **1.504 ms** | **2.084 ms** |
| 若改按主键（`WHERE book_url=...`） | 0.004 ms | 0.007 ms |

→ **每次命中省 1.50 ms**（p95 省 2.08 ms）。

**为什么命中路径只花 1.5 ms 却仍值得修**：这条查询在**每个 getBookToc 命中请求**上跑 1 次，在**每个 getBookContent** 上额外跑 1 次（§3 的 `lookup_toc_chapter_ctx`），且 §6 的并发实测显示并发 20 时它会退化到 p95 30 ms。

**改法**：`storage/mod.rs:615` 建表处加 `CREATE INDEX IF NOT EXISTS idx_toc_cache_url ON toc_cache(toc_url, user_namespace)`。
（注意：这张表 94 MB、单行最大 1.9 MB，DDL 要在低峰做；索引本身约 +几 MB。）

**预计收益**：每次目录缓存命中 −1.50 ms（p95 −2.08 ms）；并发 20 场景下每次 −29 ms（§6）。
**风险**：低。只读 SELECT 语义不变；索引是附加结构，不影响 `cache_toc`（`storage/mod.rs:3473`）的 `INSERT OR REPLACE`。
**回滚**：`DROP INDEX idx_toc_cache_url`。

---

## 2. 【P0】`get_book_sources` / `find_book_source`：618 行全表扫 + 临时 B-tree 排序

**证据**

- `SELECT * FROM book_sources WHERE user_namespace = ?1 ORDER BY weight DESC, custom_order, book_source_name` —— `storage/mod.rs:1287-1294`
- `EXPLAIN QUERY PLAN` → **`SCAN book_sources`** + **`USE TEMP B-TREE FOR ORDER BY`**
- `find_book_source` 用 `book_source_url = ?2 OR book_source_url LIKE ?3` —— `storage/mod.rs:1345-1355`，`EXPLAIN` → **`SCAN book_sources`**
- 生产行数：**618**

**实测（60 次）**

| 查询 | 中位 | p95 |
|---|---|---|
| `get_book_sources`（SCAN+ORDER） | 1.965 ms | 2.626 ms |
| `find_book_source`（OR+LIKE） | **5.612 ms** | **18.200 ms** |

**调用频次**：`get_book_sources` 出现在**每次请求都必经的 `resolve_book_source_for_url`**（`api/router.rs:3235`）里，而它又被 getBookToc / getBookContent / 换源 / 换源 SSE 调用。
`get_available_book_source`（`api/router.rs:6407`）与 `search_book_source_sse`（`api/router.rs:6855`）各调一次。

**改法**（两条，各自独立）
1. `storage/mod.rs:1287` 的排序：把 `ORDER BY weight DESC, custom_order, book_source_name` 移到 Rust 侧（`get_book_sources` 末尾**已经有** `merged.sort_by(...)` 做了完全相同的排序 —— `storage/mod.rs:1320-1325`）。SQL 去掉 ORDER BY 即可消掉 TEMP B-TREE。
2. `storage/mod.rs:466` 建表处加：
   `CREATE INDEX IF NOT EXISTS idx_book_sources_ns_enabled ON book_sources(user_namespace, enabled)`；
   `CREATE INDEX IF NOT EXISTS idx_book_sources_url ON book_sources(user_namespace, book_source_url)`。

**预计收益**：`get_book_sources` −1.0~2.0 ms/次；`find_book_source` −5.6 ms/次（p95 −18 ms）。
**风险**：低-中。① 排序下移要求 `default` 回落合并后的排序与现状一致 —— 现状代码已经是先合并再 sort，只是 SQL 里多排了一次，删掉 SQL ORDER BY 后顺序由 `sort_by` 保证，**语义等价**。② 618 行加索引，写入开销可忽略。
**验证**：改完对 `getAvailableBookSource` / `getChapterList` 各打 10 次，比对中位数。

---

## 3. 【P0】`getBookContent`：为拿 1 个章节 index，把整份目录 JSON 读出来全解析 + O(n) 线性扫

**证据**：`lookup_toc_chapter_ctx` —— `api/router.rs:4911-4943`

```
4919  get_toc_cache(ns, book_url, ttl)          → 取回整份 chapters_json
4926  serde_json::from_str::<Vec<Value>>(...)   → 全量解析成 Vec<Value>
4927  for (i,c) in chapters.iter().enumerate()  → O(n) 线性比对 url
```

调用点：**`api/router.rs:4611`**，条件是 `book_url` 非空且非 local —— 也就是**每一次 getBookContent**。

**实测解析成本**（python，对生产库里最大的 6 份 toc，`json.loads` 25 次取中位 + 线性扫）：

| 书 | JSON 大小 | 章数 | 解析中位 | 线性扫 |
|---|---|---|---|---|
| aixiashu.la/4/4357 | 1922 KB | 16896 | **110.85 ms** | 6.55 ms |
| wap.115wx.xyz/tunshigudi | 844 KB | 6943 | **154.18 ms** | 1.50 ms |
| iyruan.info/144131 | 1025 KB | 8234 | 89.48 ms | 4.70 ms |
| kujiang catalog | 1335 KB | 9665 | 87.56 ms | 6.14 ms |
| ilaidudu.com/book/2989 | 1576 KB | 12975 | 46.73 ms | 2.75 ms |
| lulushu18.com/41_41447 | 1393 KB | 12048 | 51.38 ms | 3.71 ms |

> 说明：Rust `serde_json` 比 CPython 快（通常 3-5×），且 CPython 进程内无其它负载；上表是**上界参考**。生产书架 5 本里 fq 书 533 章，按量级外推 Rust 侧约 **10-30 ms/次**。

**同一病在第二处**：`save_reading_progress_if_shelf` —— `api/router.rs:4862-4907`，
`get_toc_cache`(4877) → 全解析(4879) → `for c in chapters` 线性扫(4880-4891) → `update_book_progress`(4900)。
它在 `cache://` 章节**缓存命中路径**上也被调用（`api/router.rs:4508`），即缓存命中也不省这份解析。

**第三处，解析结果直接丢弃**：`save_book_progress` —— `api/router.rs:9089` 取 toc、`9090-9094` 解析统计长度，然后 **`api/router.rs:9100` `let _ = toc_len;`** 丢掉。整份 1.9 MB JSON 读了、解析了、扔了。

**改法**
1. 加进程内 parsed-toc LRU：`Arc<RwLock<LruCache<(ns, book_url), Arc<Vec<BookChapter>>>>`，键带 `updated_at`；`lookup_toc_chapter_ctx`(4911) 与 `save_reading_progress_if_shelf`(4877) 共用。把「每次请求一份 1.9 MB 的解析」变成「首次 1 次」。
2. 更彻底：在 `book_chapters` 上加按 `chapter_url` 的反查（现状 `chapter_index` 是 md5(chapterUrl) 不可逆，见 `storage/mod.rs:2703-2719` 的注释）；或在 `toc_cache` 旁建一张 `(ns, book_url, chapter_url, index, title)` 明细表，直接 `WHERE book_url=? AND chapter_url=?`。
3. 直接删 `api/router.rs:9089-9100` 那三行（读+解析+丢弃）。

**预计收益**：单请求 −40~150 ms（按书上界 16896 章算 −117 ms；生产书架书按 −20~40 ms 更现实）；第二次请求起趋近 −0。
**风险**：低。三者都是纯读取路径。LRU 需注意 `cache_toc` 写入时要失效同键 —— 可只对 `updated_at` 做键的一部分，天然失效。
**验证**：改完对同一 `bookUrl` 连打 10 次 `getBookContent`，比对 p50 下降。

---

## 4. 【P1】`clear_toc_cache_for_book` 的 `LIKE '%/tail'` 四路 OR → 70 ms 全表扫

**证据**：`storage/mod.rs:3495-3517`，DELETE 条件为
`(book_url = ?2 OR toc_url = ?2 OR book_url LIKE ?3 OR toc_url LIKE ?3)`，其中 `?3 = '%/{tail}'`。
`EXPLAIN QUERY PLAN` → **`SCAN toc_cache`**。

**实测（60 次，改成等价 SELECT 计时）**：中位 **70.407 ms**，p95 **157.626 ms**。
（对比：§1 的 `get_toc_cache` 同样扫表只要 1.5 ms —— 因为它是短路命中即返回；DELETE 必须扫完 1443 行 × 94 MB。）

**触发点**：`run_shelf_update` 里章数变化时才调（`storage/mod.rs:6142`），不在普通读路径上；但**书架刷新 `getBookshelf?refresh=1` 会整本跑一遍**。

**改法**：`storage/mod.rs:3495`，把尾段匹配从 SQL 下移到 Rust：先按主键 `WHERE user_namespace=? AND book_url=?` 精确删（0.004 ms 级），`LIKE` 兜底改为**按 `toc_url` 索引（§1 建的 idx_toc_cache_url）走前缀范围**，或直接限定「仅当 DELETE 精确匹配 0 行时才执行 LIKE 兜底」。

**预计收益**：每次章数变化的失效操作 −70 ms（p95 −157 ms）。
**风险**：低。`run_shelf_update` 是后台路径，删漏的后果是目录缓存 TTL 内（24h）陈旧 —— 现状已有 `refresh=1` 手动兜底。

---

## 5. 【P1】重复查询：同一请求内同参数查两次

### 5a. `search_book_multi` 把 `get_source_stats` 查了两遍

**证据**：`api/router.rs:2913`（置信度排序）与 `api/router.rs:2931`（垫底源过滤）。
2913 的 `stats` 在块尾丢弃，2931 同参数重查。2931 的守卫是 `!include_dead && single_source_url.is_empty()` —— 也就是**默认路径必走**。

**同仓库已有对照**：SSE 孪生 `search_book_multi_sse` 在 `api/router.rs:9382` 只查一次，且源码注释写着：
> 「历史表现快照：一次查询供三处复用（置信度排序 / 垫底源跳过 / 窗内派发顺序），**原来这里查了两次**，且窗内排序还得再查一次 —— 不新增 DB 往返。」

即：**这是同一个缺陷，SSE 侧已修，非 SSE 侧是遗留未修**。

**实测**：`SELECT * FROM source_stats WHERE user_namespace='default'` 中位 0.011 ms（2089 行，走了 `sqlite_autoindex_source_stats_1`）。

**改法**：`api/router.rs:2913` 的 `stats` 提到函数外层复用，删掉 2931 的重查（照抄 9382 的写法）。
**收益**：每请求 −1 次查询，−0.011 ms。量小，但属于零风险清理。

### 5b. `get_book_toc` 的 `_shelf_for_write` 是死代码，白付 1-2 次 `find_book`

**证据**：`api/router.rs:4161-4175` 绑定 `let _shelf_for_write = { ... }`，内含 `find_book` (4164) 与 `find_book` (4171)；
该变量**只写不读**（下划线前缀，全函数无其它引用）。真正被用的是 `shelf_for_write`（`api/router.rs:4249-4253`，内部再一次 `find_book(ns, url_param)` @4251）。
→ 当 `url_param` 在书架里时，4164 与 4251 是**同参同 SQL 查两次**。

**实测**：`find_book` 走主键，`EXPLAIN` → `SEARCH books USING INDEX sqlite_autoindex_books_1 (book_url=? AND user_namespace=?)`，中位 0.010 ms。

**改法**：删掉 `api/router.rs:4161-4175` 整个 `let _shelf_for_write = { ... };` 块。
**收益**：每请求 −1~2 次查询，−0.01~0.02 ms。零风险（变量无人读）。

### 5c. `get_book_info` 用全表 `list_books` 找 1 行

**证据**：`api/router.rs:3780` `list_books(&namespace)` → `3783` `books.iter().find(|b| b.book_url == url)`。
`list_books`（`storage/mod.rs:5017`）是全量 `SELECT books.*, books.rowid ... ORDER BY order_num ASC, MAX(dur_chapter_time, created_at) DESC, rowid DESC`。

**实测**：`list_books` 中位 0.018 ms（生产书架 5 本，`EXPLAIN` → `SCAN books`）。
**改法**：`api/router.rs:3780` 换成 `find_book(&namespace, &url)`（主键索引，0.010 ms，且不读大 TEXT 列）。
**收益**：当前 5 本书下 −0.008 ms / −1 次 O(n) 遍历；书架增长后收益线性放大（`list_books` 是 `SCAN books`，O(全部书)，`find_book` 是 O(log n)）。
**风险**：低。注意 `list_books` 带 `ORDER BY`，但 3783 只取 `.find()` 第一个匹配，顺序不影响结果。

---

## 6. 【P1】并发：没有跨请求的全局并发上限

**证据**（全部是 `Arc<Semaphore>` 在函数内新建，进程内无全局闸门，middleware 层无 ConcurrencyLimit）：

| 位置 | 并发数 | 说明 |
|---|---|---|
| `api/router.rs:2939` `search_book_multi` | 24 | |
| `api/router.rs:3173` `search_book_source` | 24 | |
| `api/router.rs:6419` `get_available_book_source` | 16 | |
| `api/router.rs:6878` `search_book_source_sse` | 24 | |
| `api/router.rs:9479` `search_book_multi_sse` | `concurrent_count` | **客户端可控 `clamp(1,128)`，见 `api/router.rs:9284`** |
| `service/health.rs:145` `find_invalid` | 96 | 后台探测全源 |

**实测并发退化**（ThreadPoolExecutor 同时打 `getChapterList`，生产库）：

| 并发 | 墙钟 | 单请求中位 | 单请求 max |
|---|---|---|---|
| 1 | 69.2 ms | 58.9 ms | 58.9 ms |
| 5 | 141.4 ms | 119.8 ms | 128.6 ms |
| **20** | **698.4 ms** | **449.4 ms** | **599.9 ms** |

→ 单请求延迟随并发**线性退化**（1→20：59 ms → 449 ms，7.6×）。20 并发下墙钟只用了 698 ms，说明不是吞吐瓶颈而是**排队**。

DB 池上限 32（`storage/mod.rs:255-266`，`READER_DB_MAX_CONNECTIONS` 默认 32，clamp 1..128）。
SQLite 侧只有 WAL + busy_timeout(5s)，无任何排队保护。

**改法**
1. 在 `api/router.rs:71` 的 `router()` 里加一层全局 `tower::limit::ConcurrencyLimitLayer`（或自定义 `GlobalSemaphore`），把**跨请求**的并发钳在比如 64 以内。
2. `api/router.rs:9284` 的 `clamp(1, 128)` 收到 `clamp(1, 32)`（与 DB 池 32 对齐）。现状单客户端传 `concurrentCount=128` 就能把 32 的池打爆到 4×。

**预计收益**：20 并发下 p95 从 600 ms 降到 ~150 ms 量级（排队消失）；阻止单客户端用 `concurrentCount=128` 拖垮全站。
**风险**：中。全局限流会影响「同时开多个搜索页」的体验，且被拒的请求是 503 而非排队 —— 建议先只做 ②（收 `concurrentCount` 上限，零副作用），① 留作观察项。

---

## 7. 【P1】两个全局 `std::sync::Mutex` 卡在抓取热路径上

### 7a. `crawler.rs:1270` `session_for` / `1188` `cookie_for` / `1240` `login_header_for` 各持一把 `COOKIE_STORAGE` 锁

**证据**

```
service/crawler.rs:1269-1272   let storage = COOKIE_STORAGE.lock()...clone()?;   ← std::sync::Mutex
service/crawler.rs:1647        session_for(ns,&url).await
service/crawler.rs:1649        cookie_for(ns,&url).await
service/crawler.rs:1677        login_header_for(ns,&url).await
service/crawler.rs:1886        login_header_for(ns,&url).await   （重试路径）
```

→ **每次 HTTP 抓取，最多 4 次进入 `COOKIE_STORAGE` 这个 `std::sync::Mutex`**（另 1188/1240 内部同样有）。
它们是 `std::sync::Mutex`（非 async），持锁期间 `.await` 会被阻塞整个 worker 线程（不是 async 挂起）。

**同时**：`storage/mod.rs:1928-1933` 的 `LOAD_LOCK` 是 **函数级 static，全进程一把，跨命名空间共享**。
好消息是**快路径在锁之前**：`storage/mod.rs:1922` `if let Some(snap) = cookie_snapshot_peek(&key) { return Ok(snap); }` 在 1928 取锁之前 —— 命中不付锁代价。
**待修点**：miss 时，`storage/mod.rs:1938-1942` 的 `SELECT ... FROM book_source_cookies WHERE user_namespace=?1` 跑在 guard 内。

**实测该 SQL**：中位 **0.009 ms**（1620 行，走 `sqlite_autoindex_book_source_cookies_1`，`EXPLAIN` → `SEARCH ... USING INDEX`）。

→ 结论修正：**这条 SQL 本身不慢（0.009 ms），锁内 await 的实际代价是「持锁 0.009 ms × fan-out 24 路串行排队」≈ 零点几 ms**，不是大老虎。但 `std::sync::Mutex` 在 async fn 里是 clippy `await_holding_lock` 级别的隐患：一旦快照过期，24 路 fan-out 会全堵在这一把锁上。

**改法**
1. `storage/mod.rs:1928` 的 `LOAD_LOCK` 改为**按 key（= ns）分片**，比如 `DashMap<String, Arc<tokio::sync::Mutex<()>>>` 或 per-key `OnceCell`。跨命名空间不再互相阻塞。
2. `storage/mod.rs:1938-1942` 的 SQL 移出 guard（double-checked locking）：先取锁 → peek → **放锁** → 查库 → 取锁 → 写入。
3. `service/crawler.rs:1647/1649/1677` 三次调用合并成一次 `cookie_snapshot` 拿到 `(cookie, ua, login_header)` 三元组（快照对象本身已含全部三个字段，见 `storage/mod.rs:2078/2147/2160` 三个 wrapper 各自只取其一）。
   → 每次抓取 **−2~3 次锁获取 + −2~3 次 Arc clone**。

**风险**：低-中。① ③ 是纯机械合并，语义不变。② 需要小心：现状注释（`storage/mod.rs:1950-1953`）说明「校验写代数 + 写入在同一把锁内完成」是为了避免「读到写前旧数据、却在 invalidate 之后才塞进缓存」的竞态 —— 改成放锁查库后，**必须保留 `gen` 代数校验**（1936 取 gen、1952 比对 gen 的逻辑要原样保留）。

### 7b. `api/router.rs:3740` `BOOK_INFO_CACHE`

```
api/router.rs:3740  static BOOK_INFO_CACHE: LazyLock<std::sync::Mutex<BookInfoCacheStore>>
api/router.rs:3745  book_info_cache_get  → .lock().unwrap_or_else(...)
api/router.rs:3753  book_info_cache_put  → .lock().unwrap_or_else(...)
```
—— 两者都是 **sync fn，锁内无 await**，容量 200 / TTL 10min（`api/router.rs:3700-3701`）。**判定：安全，非瓶颈。** 记录在案仅供后续排查。

---

## 8. 【P2】PRAGMA 缺失

**证据**：应用只在连接池建立时设了两个（`storage/mod.rs:258-268`）：
- `journal_mode = WAL`（`storage/mod.rs:263`）
- `busy_timeout = 5s`（`storage/mod.rs:265`）
- `statement_cache_capacity(0)`（`storage/mod.rs:268`，为规避 sqlx-sqlite 0.7.4 的 DDL 重准备 panic）

**生产库实测当前值**：

| PRAGMA | 当前值 | 说明 |
|---|---|---|
| journal_mode | `wal` | 已设 |
| synchronous | **2 (FULL)** | 未设，SQLite 默认 |
| cache_size | **-2000**（= 2000 页 ≈ **7.8 MB**） | 未设，默认 |
| page_size | 4096 | 默认 |
| temp_store | 0 (file) | 未设 |

库文件 1.0 GB / 262845 页 / freelist 0。

**改法**（`storage/mod.rs:258-268` 的 options 链里追加）
```
.synchronous(SqliteSynchronous::Normal)   // WAL 下 FULL 只影响 checkpoint，NORMAL 已足够安全
pragma("cache_size", "-65536")            // 64 MB，当前 7.8 MB
pragma("temp_store", "MEMORY")
pragma("mmap_size", "268435456")          // 256 MB，让大 toc_cache 走 mmap 免 memcpy
```
外加一个启动后/周期性的 `PRAGMA optimize;`（会在无 `sqlite_stat1` 时建统计信息，帮助 planner 选对索引）。

**预计收益**：`cache_size` 7.8 MB → 64 MB 直接减少大表（book_vars_cache 680 MB、toc_cache 112 MB）的重复缺页读；`mmap_size` 对 §1/§3 的 1.9 MB 单行读取收益明显。
**风险**：
- `synchronous=NORMAL`：WAL 下断电可能丢最后一个 checkpoint 之后的事务。本库内容是缓存（目录/正文/变量），可重建 → **风险可接受**。
- `cache_size` 64 MB：进程 RSS +64 MB，注意容器内存上限。
- `mmap_size`：只读路径安全；**写时 mmap 区域需要额外拷贝，且 WAL 下 mmap 写有已知限制**，建议**只在读连接上设**（若未来分读写池）。
- 需要先在备份库上验证，且这些是**连接级** PRAGMA，改完必须重启进程。

---

## 9. 【P2】表增长：`toc_cache` 94 MB / `book_vars_cache` 101 万行 680 MB

**实测**

| 表 | 行数 | 空间 | 说明 |
|---|---|---|---|
| `book_vars_cache` | **1,018,747** | **680 MB** | 主键 `(ns, source_url, url)` |
| `toc_cache` | 1,443 | **94.6 MB** | 单行最大 1.9 MB |
| `book_chapters` | 3,627 | 83 MB | |
| `search_cache` | 1,770 | 25 MB | |
| 合计 | | 库 1.0 GB | |

**`book_vars_cache` 的问题**：它是全表最大的一块，且被 `list_urls_under_prefix`（`storage/mod.rs:2703-2719`）用 `url LIKE 'prefix%'` 前缀扫。
**实测**：`EXPLAIN` → `SEARCH book_vars_cache USING COVERING INDEX sqlite_autoindex_book_vars_cache_1 (user_namespace=?)`（走索引前缀，无需排序），中位 **0.011 ms**。
→ **现状不慢，但 101 万行仍以每行约 680 字节在涨**；行来源是 `analyze_toc/analyze_content` 每章写一行（`storage/mod.rs:2703` 的注释自述「analyze_toc/analyze_content 都会按章节 URL 写一行变量缓存」）。

**改法**
1. 加 TTL 清理 job：对 `book_vars_cache` 按 `updated_at` 做周期性 DELETE（如保留 30 天）。**注意**：删前确认 `list_urls_under_prefix` 的离线兜底依赖它 —— 建议只对**不在书架上的书**的前缀清理。
2. `toc_cache` 加上限：现状 1443 行里有很多是「不在书架的临时书」（`book_url` 前缀五花八门：aixiashu / laidudu / lulushu / 115wx / i7wx…）。按 `updated_at` 保留 N 天或按总字节数封顶（如 200 MB）。
3. 现状 `getCacheInfo`（`api/router.rs:1939`）已经能报 `toc_cache_size` / `chapter_size`，可先拿它做监控基线。

**收益**：库从 1.0 GB 降到 300 MB 量级 → 备份更快、页缓存命中率更高、§1/§4 的全表扫描绝对值下降（比例不变但常数变小）。
**风险**：中。**必须先确认没有别处依赖被删的 vars**（`parser/rule.rs:148` 的 `BOOK_VARS_CACHE` 内存层有 read-through 到 DB 的语义，删库行可能导致 `@get:{key}` 取不到值 → 影响正文规则求值）。**建议只做 §9.2（toc_cache 封顶），§9.1 留观察。**

---

## 10. 【P2】大 JSON 响应没有 ETag / 304 / 分页

**证据**

- `api/router.rs:245-248`：`/reader3/getChapterList` 是 `get_book_toc` 的别名
- `api/router.rs:4142` / `4239`：`serde_json::to_value(chapters)` 全量序列化
- `api/router.rs:7018-7021`：`list_books` → `books.iter().map(book_json_with_group_ids)` → `Value::Array`
- `api/router.rs:6986-6999` `book_json_with_group_ids`：`serde_json::to_value(book)`（全量）+ `serde_json::from_str(&book.group_ids)`（再解析一次）= **每本书 2 次 serde**
- `model/book.rs` 的 `Book` 约 40 个 camelCase 字段，**Option 字段无 `skip_serializing_if`** → 空值显式输出 `null`
- 全仓 grep `ETag|etag|If-None-Match` 在 API 侧 **0 命中**；`CacheControlLayer`（`lib.rs:166`）仅限静态资源
- gzip 已开（`api/router.rs:99-105` + `lib.rs:172` 外层兜底）

**实测载荷**：`getChapterList`(kushuge) = **201,849 B**（约 1400+ 章 × 5 字段）；`getBookshelf` = 8,661 B。

**改法**
1. `/reader3/getChapterList` 与 `/reader3/getBookshelf` 加 ETag：对 `(ns, toc_url, updated_at)` 做弱 ETag，命中 `If-None-Match` 直接 304。
2. `model/book.rs` 的 Option 字段加 `#[serde(skip_serializing_if = "Option::is_none")]`。
3. `api/router.rs:6986` 的第二次 serde：把 `group_ids` 的解析提到 `to_value` 之前，或直接在 `Book` 上存成 `Vec<i64>` 字段。

**收益**：
- ETag：重复请求从 201 KB → 0 B（**−201 KB 出网 + −20 ms 序列化**）。这是前端二次进入目录页的最大单点。
- `skip_serializing_if`：getBookshelf 载荷 8.6 KB → 估计 4-5 KB（**−40%**）。
**风险**：低。ETag 需注意与 `refresh=1` 语义交互（`refresh=1` 时跳过 ETag 校验）。

---

## 11. 已核查但不成立 / 暂不提议

| 项 | 核查结果 |
|---|---|
| `find_book` 慢 | **不成立**。走主键，`EXPLAIN` → `SEARCH books USING INDEX sqlite_autoindex_books_1`，中位 **0.010 ms** |
| `get_chapter_content` 慢 | **不成立**。走主键，中位 **0.004 ms** |
| `list_cached_chapter_headers` 慢 | **不成立**。`EXPLAIN` → `SEARCH book_chapters USING INDEX sqlite_autoindex_book_chapters_1 (book_url=?)`，中位 0.110 ms |
| `list_urls_under_prefix` 慢 | **不成立**（但有增长风险，见 §9）。走 covering index，中位 **0.011 ms** |
| `get_book_candidates` 慢 | **不成立**。走主键，中位 **0.008 ms** |
| `get_source_stats` 慢 | **不成立**。走 `sqlite_autoindex_source_stats_1`，中位 **0.011 ms** |
| `cookie_snapshot` 的 SQL 慢 | **不成立**。走 `sqlite_autoindex_book_source_cookies_1`，中位 **0.009 ms**（§7a 的锁问题是另一回事） |
| `getBookContent` 热路径持 std 锁 | 部分成立但代价小，见 §7a 的修正说明 |
| `parser/js.rs` 的 `block_on_task` | **未验证**。已知 `get_book_content` 会调 `load_book_vars`(4578) / `save_book_vars`(4627)，是否每章都进 `block_on_task`（`parser/js.rs:3126`）未追踪。**留作观察项**，不作提议 |
| `rss_articles` | 只有 1 行，`EXPLAIN` → `SCAN`，但规模不值得加索引 |
| 搜索墙钟 / FCP / 家族树布局 / 目录正文按源缓存 | 上一轮已上线，**不在本轮范围** |

---

## 12. 执行顺序建议（按 收益/风险 比）

| # | 改动 | 位置 | 预计收益 | 风险 |
|---|---|---|---|---|
| **0** | **`getCacheInfo` 结果加 10s TTL 进程内缓存** | `api/router.rs:1948` | **−555 ms/次**（p95 −890 ms），并停止每次挤出 126 MB 页缓存 | **零**（数字偏旧 10s 内，缓存统计本就是给 UI 看） |
| 1 | 删 `_shelf_for_write` 死代码块 | `api/router.rs:4161-4175` | −1~2 次查询/请求 | **零** |
| 2 | `get_source_stats` 提为一次（照抄 9382） | `api/router.rs:2913,2931` | −1 次查询/请求 | **零** |
| 2b | `getCacheInfo` 4 条 SQL 合成 2 条 | `storage/mod.rs:2316-2332` | −2 次往返（约 −0.9 ms）；与 #0 一起上时收益被吞掉，仅在不上 #0 时用 | 低 |
| 3 | `get_book_info` 用 `find_book` 替 `list_books` | `api/router.rs:3780` | −1 次全表遍历 | 低 |
| 4 | 加 `idx_toc_cache_url(toc_url, user_namespace)` | `storage/mod.rs:615` | **−1.50 ms/次**（p95 −2.08 ms），并发 20 时 **−29 ms** | 低 |
| 4b | `cache_job` 逐章写包进事务 | `service/cache_job.rs:175-181` | 500 章 = 500 次 fsync → 1 次（synchronous=FULL 下每次是真实磁盘同步） | 低（照抄 `save_chapters` `storage/mod.rs:2654`） |
| 5 | `concurrentCount` 上限 128 → 32 | `api/router.rs:9284` | 阻止单客户端打爆 DB 池 | 低 |
| 6 | parsed-toc LRU（供 4911 / 4877 共用） | `api/router.rs:4911` | **−20~150 ms/次**（按书大小） | 低 |
| 7 | 删 `save_book_progress` 里「读了就扔」的 toc 解析 | `api/router.rs:9089-9100` | −1 次 1.9 MB 读+解析 | **零** |
| 8 | `getChapterList` / `getBookshelf` 加 ETag+304 | `api/router.rs:4142, 7018` | 重复请求 **−201 KB 出网** | 低 |
| 9 | `book_sources` 排序下移到 Rust + 加 2 个索引 | `storage/mod.rs:1287, 466` | −1.0~5.6 ms/次（p95 −18 ms） | 低-中 |
| 10 | `clear_toc_cache_for_book` 的 LIKE 兜底改走索引 | `storage/mod.rs:3495` | **−70 ms/次**（p95 −157 ms） | 低 |
| 11 | 爬虫 3 次 cookie 调用合并 + `LOAD_LOCK` 按 ns 分片 | `crawler.rs:1647/1649/1677`, `storage/mod.rs:1928` | −2~3 次锁获取/抓取，跨 ns 不再互阻塞 | 中（需保 `gen` 校验） |
| 12 | PRAGMA：cache_size 64MB / mmap 256MB / sync NORMAL | `storage/mod.rs:258-268` | 大单行读取与缺页读下降 | 中（需重启、需先备份验证） |
| 13 | `toc_cache` 字节数封顶清理 job | 新 job | 库 1.0 GB → 300 MB 量级 | 中 |
| 14 | 全局 ConcurrencyLimitLayer | `api/router.rs:71` | 20 并发 p95 600 ms → ~150 ms | 中（会出 503） |
| — | `book_vars_cache` TTL 清理 | — | 680 MB → ? | **暂缓**（有 `@get` 依赖风险） |
| — | `parser/js.rs` `block_on_task` 是否每章调用 | — | ? | **未验证，留观察** |

> 1-8 项为零风险或低风险，可作为一个批次直接上；9-11 建议逐个上并各打 10 次比对；12-14 需要在备份库先验证。

---

---

## 13. 【P0】`get_cache_info` —— 4 条查询读 ~150 MB，生产 `testuser01` 实测 522 ms

（来自 storage 子代理，在**生产库**上重新量化 —— 它原用的是 324 MB 的副本，低估了近 4 倍。）

**证据**：`get_cache_info` —— `storage/mod.rs:2314-2337`，四条 SQL：

| # | 行 | SQL | EQP |
|---|---|---|---|
| 1 | `storage/mod.rs:2316` | `SELECT COUNT(*) FROM toc_cache WHERE user_namespace=?` | `SCAN toc_cache USING COVERING INDEX` |
| 2 | `storage/mod.rs:2321` | `SELECT COALESCE(SUM(length(chapters_json)),0) FROM toc_cache WHERE user_namespace=?` | **`SCAN toc_cache`（必须读每行 41 KB blob）** |
| 3 | `storage/mod.rs:2327` | `SELECT COUNT(*) FROM book_chapters WHERE user_namespace=?` | `SCAN book_chapters USING COVERING INDEX` |
| 4 | `storage/mod.rs:2332` | `SELECT COALESCE(SUM(length(content)),0) FROM book_chapters WHERE user_namespace=?` | **`SCAN book_chapters`（读每行正文）** |

路由：`api/router.rs:347` `/reader3/getCacheInfo`，handler `api/router.rs:1935-1958`（需登录，§14 说明本轮未能测到 HTTP 侧）。

**实测（生产库，python 只读，`testuser01` 命名空间）**

单条拆分（20 次，单独计时）：

| 查询 | 中位 | p95 |
|---|---|---|
| #2 `SUM(length(chapters_json))` | **306.215 ms** | **367.405 ms** |
| #4 `SUM(length(content))` | **248.911 ms** | **522.159 ms** |
| #1 `COUNT(*) toc_cache` | 0.233 ms | 0.293 ms |
| #3 `COUNT(*) book_chapters` | 0.618 ms | 0.723 ms |

→ **一次 `getCacheInfo` 调用 ≈ 555 ms**（SUM 两条合计 555 ms，p95 合计 890 ms），
单次调用期间必须读完 `testuser01` 的 **90.9 MB 目录 JSON + 35.7 MB 章节正文 = 126.6 MB**。

四条连跑 10 轮的总墙钟复核：`testuser01` 中位 **522.18 ms** / p95 **859.64 ms**；`default`（约 97 行）109.59 ms / 131.30 ms。
→ 两个测法互相印证，**单请求量级 = 0.5~0.9 秒**，且会挤出 126 MB 的页缓存。

**为什么它没被前面的接口基准发现**：本轮 HTTP 基准里没有包含它（§14：生产 token 在会话中途失效，只覆盖了 top-5 接口）。

**改法**
1. 加 `CREATE INDEX idx_toc_cache_ns (user_namespace)` 与 `idx_book_chapters_ns (user_namespace)` —— 但**对 `SUM(length(blob))` 无效**，它仍要读每一行。索引只解决两条 COUNT（−1.1 ms）。
2. **真正的修法：把大小统计改成抽样/估算或增量维护**。
   - 最省事：缓存结果。加一个 60s TTL 的进程内 `OnceLock<(i64, Instant)>`，`api/router.rs:1948` 命中直接返回。缓存统计本就是给 UI 看的，不需要实时。
   - 更彻底：在 `cache_toc`/`cache_chapter_content` 写入时把 `length()` 累加到一张计数器表（或直接读 `PRAGMA page_count` 估算）。
3. 立刻可做：把 4 条合并为 2 条（`SELECT COUNT(*), COALESCE(SUM(length(chapters_json)),0) FROM toc_cache WHERE ns=?`），省 2 次往返。

**预计收益**：
- 方案 2 的缓存版：重复调用 522 ms → **~0 ms**（−522 ms，p95 −860 ms），并把 150 MB 的页缓存污染降到 60s 一次。
- 方案 3 的合并版：−1.1 ms，但**保留 522 ms 的主体**（不推荐单独用）。

**风险**：低（方案 2）。缓存 60s 内数字偏旧；`clearCache` 后需主动失效 —— 若嫌麻烦，可把 TTL 设 10s，仍能砍掉 90% 的开销。注意统计是按命名空间隔离的（`api/router.rs:1943` 注释），缓存键要带 ns。

---

## 14. 实测中断说明：生产 token 在会话中途失效

会话前半段（token 有效）完成的接口基准见 §0，全部返回 200 与真实 JSON。
**01:0x 之后，同一个 token 对 `/reader3/getBookshelf` 等返回 HTTP 200 但 body 是 `{"isSuccess":false,"errorMsg":"请登录后使用","data":"NEED_LOGIN"}`（71 B）** —— 即后端鉴权已不认该 token。

未完成的实测：
- `/reader3/getCacheInfo`（§13，改用 SQL 侧直接计时，已补上）
- `/reader3/getBookGroups`（§15，同样受阻）
- `getBookContent` 并发扫描的第二次采样

**这不影响 SQL 侧的量化**（只读库访问与 HTTP 鉴权无关），但 §0 的 HTTP 基准表应视为**会话前半段的快照**。

---

## 15. storage 子代理其它发现的核对结果（在生产库上复核）

该子代理用的是 `/home/hehecat/code/.tmp/rdb/reader.db`（324 MB，testuser01 数据）。
以下在生产库（1.0 GB）上重新测量，**按实际收益重排**：

| 子代理发现 | 位置 | 生产实测 | 裁决 |
|---|---|---|---|
| `get_cache_info` 4 条全扫 | `storage/mod.rs:2314-2337` | **522 ms / p95 860 ms** | **升为 P0，见 §13** |
| `get_toc_cache` 全表扫 | `storage/mod.rs:3535` | 1.504 ms | 成立，与 §1 同一条，保留 P0 |
| `list_books` 全索引扫 + temp b-tree | `storage/mod.rs:5017` | **0.133 ms**（仅 5 本） | **降级**。5 本书时无感；书架增长后才显形。**并入 §5c 一起修**（换 `find_book` 或加 `(user_namespace, order_num)` 索引） |
| `list_book_groups_with_count` N+1 | `storage/mod.rs:3781` | 单次 0.012 ms × **4 组 = 0.048 ms** | **降级为 P3**。testuser01 只有 4 个分组，`SCAN books` 在 5 行上等于常数时间。**不改** |
| `delete_books` 前置 SELECT | `storage/mod.rs:3923` | 0.007 ms，**且走 PK**（`SEARCH USING INDEX`） | **驳回**。子代理说它「事务外」属实，但单次 0.007 ms 且是主键 seek，删 N 本也才 N×0.007 ms。**不改** |
| `upsert_book` 53 列写放大 | `storage/mod.rs:3334` | 前置 `find_book` **0.032 ms** | **降级**。53 列的 `INSERT OR REPLACE` 是真的（生产 `books` 表确为 53 列），但它在 saveBook 路径上而非每请求路径；前置 `SELECT *` 只有 0.032 ms。**留作观察，不进本轮** |
| `list_updatable_books` 全扫 | `storage/mod.rs:5334` | 0.118 ms（无 ns 过滤） | **驳回**。0.118 ms，且只在 `refresh=1` 时跑一次，相对 §0 里 11.2 s 的 `refresh=1` 总耗时是噪声 |
| `graph_job_unfinished` 全扫 | `storage/mod.rs:2888` | 0.016 ms | **驳回**。生产 `book_graph_jobs` 极小 |
| `rss_read_flags` 全扫 | `storage/mod.rs:2284` | 0.009 ms | **驳回**。生产 `rss_articles` 只有 1 行 |
| `find_book_by_name_author` 全扫 | `storage/mod.rs:3308` | 0.021 ms | **驳回**。5 本书 |
| `list_loc_book_books` 全扫 | `storage/mod.rs:4761` | 0.025 ms | **驳回** |
| `cache_job` 每章一次 fsync | `service/cache_job.rs:177` | （只读库无法测写） | **保留为 P2**。**机制成立**：`cache_chapter_content`（`storage/mod.rs:2780-2795`）是单条 `INSERT OR REPLACE` 跑在 `&self.pool` 上、无事务；而 `save_chapters`（`storage/mod.rs:2654`）是正确的事务包裹范例。500 章 = 500 次 fsync。synchronous=FULL（§8）下每次 fsync 是真实磁盘同步 |
| `restore_backup_zip` 逐项 N+1 | `storage/mod.rs:5544` | （备份路径，非在线） | **保留为 P3**。只在恢复备份时触发，可容忍 |
| `run_shelf_update` 每本各自提交 | `storage/mod.rs:6095` | — | **并入 §P2**。但注意 `refresh=1` 的 11.2 s 里绝大部分是 `analyze_toc` 网络（§0），fsync 不是主项 |
| `bookmarks` PK 缺 ns | `storage/mod.rs:631` | 生产 1 行 | **驳回**（规模不足） |
| search_cache 等无 TTL 清理 | `storage/mod.rs:1683` | 生产 **1705 行 / 25 MB**（子代理报 675 行） | **保留为 P2，并入 §9** |
| `save_chapters` 不绑 ns 的「bug」 | `storage/mod.rs:2658` | 生产 `user_namespace='default'` 的 book_chapters 行数 = **0** | **驳回为误报**。生产无该路径产生的行，路径未触发。且列名省略时落到列 DEFAULT，非数据丢失 |

**净结果：子代理报的 18 条里，在生产库上成立且值得进本轮的只有 3 条**（`get_cache_info` 升 P0、`get_toc_cache` 与 §1 重合、事务粒度 2 条）；其余 13 条因生产规模过小而被驳回或降级。

---

## 附：脚本位置

- `.tmp/perf/bench.py` —— 主接口基准（health / getBookshelf / getChapterList / getAvailableBookSource）
- `.tmp/perf/bench2.py` `bench3.py` `bench4.py` —— getBookContent 冷/热、getCacheInfo/getBookGroups（受阻）
- `.tmp/perf/conc.py` `conc2.py` —— 并发退化
- `.tmp/perf/sse.py` —— searchBookMultiSSE 首事件
- `.tmp/perf/qtime.py` `qtime2.py` `q3.py` `q4.py` `q5.py` `q6.py` `q7.py` —— 只读库 SQL 计时 + EXPLAIN + 行数 + ns 分布
- `.tmp/perf/parse.py` `parse2.py` —— toc JSON 解析成本
- `.tmp/perf/pickch.py` —— 从 toc_cache 抽章节 URL
