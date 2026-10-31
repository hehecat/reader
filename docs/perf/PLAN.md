# 前后端性能执行清单（2026-11-01）

来源（都在本目录）：
- `backend-hy4.md` — hy4-preview 的后端画像（509 行）
- `backend-hy4-verified.md` — deepseek-v4.1-flash 的逐条交叉验证（复现/证伪/补强）
- `frontend-deepseek.md` — deepseek-v4.1-flash 的前端画像
- 前端验证：hy4-preview 与 claude-opus-4-6 两轮都卡在该步未产出，改由 captain 抽查了代码级证据（见文末「前端抽查记录」），标注 `[已抽查]` 的条目为 captain 亲自复核过

> 全局教训（两个模型都踩过）：**用生产 SQLite 做统计必须带 `user_namespace='testuser01'`**。`ns='default'` 在 `toc_cache` / `book_vars_cache` / `source_stats` 三张表上是 0 行，据此得出的「0.011 ms」级结论全部作废（hy4 画像里有 3 条因此误判，已在 verified 文档里纠正）。

---

## 先做这三条

1. **`backend/src/storage/mod.rs:258-268` 加 `.synchronous(SqliteSynchronous::Normal)`**
   实测 commit 2.65–3.09 ms → 0.06–0.09 ms（**34–44×**）。每个正文请求的 `save_book_vars`、每章缓存写入都吃这 3 ms。风险：掉电可能丢最后几秒事务（WAL + NORMAL 的标准语义），阅读站可接受。
2. **`backend/src/service/cache_job.rs:255-260` 逐章 autocommit 改单事务**
   现有写法在 `for h in handles` 里逐章 `cache_chapter_content`，每章一次 commit（4.5–4.7 ms/行）。实测 500 行 **2349.8 ms → 15.3 ms**（43–139×）。与第 1 条叠乘。
3. **`backend/src/storage/mod.rs:609-624` 建 `idx_toc_cache_url(toc_url, user_namespace)`**
   现在 `get_toc_cache` 按 `toc_url` 查但主键是 `(book_url,user_namespace)` → 每次全表扫。实测 1.9 MB 行 8.735 → 5.467 ms，普通行 2.4 → 0.68 ms。顺手做 `storage/mod.rs:1294` 删 `ORDER BY`（Rust 侧 `:1314-1320` 已有同义排序）**省 14 ms**。

---

## 完整清单（按 ROI）

| # | 目标 | 改哪里 | 预期收益 | 验证 | 风险 |
|---|---|---|---|---|---|
| B1 | WAL `synchronous=NORMAL` | `backend/src/storage/mod.rs:258-268` | commit 34–44× | `cargo test --lib storage::` + 一次 `getChapterList?refresh=1` | 掉电丢数秒 |
| B2 | 缓存章节目录写批事务 | `backend/src/service/cache_job.rs:255-260`，storage 加 `cache_chapter_contents_batch` | 500 章 2.35 s → 15 ms | `cargo test --lib service::cache_job` | 单事务过大时内存峰值 |
| B3 | `toc_cache` 按 URL 建索引 | `backend/src/storage/mod.rs:609-624` | 1.7–3.3 ms/次 | `.tmp/perf/v5_toc.py` 50 次中位 | 写入略放大 |
| B4 | `get_book_sources` 删 SQL `ORDER BY` | `backend/src/storage/mod.rs:1294` | 44.74 → 30.76 ms（615 源），p95 更明显 | `.tmp/perf/v8_sources.py` + `getBookSources?simple=1` ×10 | 排序语义等价（Rust 侧已排） |
| B5 | `getCacheInfo` 加 10 s 进程内 TTL 缓存 | `backend/src/api/router.rs:1939-1958`（复用 `BOOK_INFO_CACHE` 骨架） | 583 ms → <5 ms | 连打 10 次，第 2 次起 <5 ms | 统计数字最多旧 10 s |
| B6 | `list_urls_under_prefix` 改范围 seek | `backend/src/storage/mod.rs`（新增），调用点在缓存清理 | **493 ms → 预期个位数**（990 226 行，EQP 只用到索引首列） | 同库 EXPLAIN + 计时 | 前缀边界要处理 |
| B7 | `clear_toc_cache_for_book` 走覆盖索引 | 建 `idx_toc_ns_url_book(user_namespace, toc_url, book_url)`，Rust 侧做 `ends_with` | 54.87 → 0.96 ms（p95 117.79 → 1.17） | `.tmp/perf/v3_sql.py` | 与 B3 索引并存，写放大 |
| B8 | `getBookContent` 章节上下文用 typed `Vec<BookChapter>` | `backend/src/api/router.rs:4918` | 解析快 2.2–3×（书架书 0.4–3.1 ms，万章目录 15.5 vs 46 ms） | `cargo test --lib` | 类型改动的编译面 |
| B9 | `get_source_stats` 搜索路径查两次 → 复用 | `backend/src/api/router.rs:2906` / `:2937` | ~25 ms/搜索请求（占 95 s 墙钟 0.03%，属代码卫生） | 打 10 次搜索比对 | 无 |
| B10 | 删 `get_book_toc` 里 `_shelf_for_write` 死代码 | `backend/src/api/router.rs:4161-4175` | 白省的 1–2 次 `find_book` | `cargo test --lib` | 无 |
| B11 | `get_book_info` 的 `list_books` 改懒求值 | `backend/src/api/router.rs:3785` | 现在 <1 ms（5 本书），书架增长后线性 | getBookInfo ×10 | 无 |
| B12 | `cache_size=-65536` | `backend/src/storage/mod.rs:258-268` | `book_sources` 全扫 64.62 → 36.60 ms | 同 B1 | 内存 64 MB |
| F1 | 删 cytoscape-klay `[已抽查]` | `frontend/src/pages/GraphDemoPage.tsx:20,61` + `package.json:29` + `types/cytoscape-plugins.d.ts:5` | **−473.56 kB raw / −153.09 kB gzip**，二进制同步瘦 473 kB | `pnpm build` 对比 chunk 表；家族树布局已改 `layoutFamilyTree`，klay 只剩注册 | 极低；确认没有按名 `name:'klay'` 的调用（已 grep，无） |
| F6 | 删 `frontend/public/fonts/` `[已抽查]` | 目录删除 | `dist/` 33.1 MB → 2.4 MB（**−32.24 MB**），镜像同步瘦 | `du -sh dist` | 低；607 个 woff2，`src/` 与 `index.html` 无引用（已 grep） |
| H1 | TOC 跳过全量 zod `[已抽查]` | `frontend/src/services/book.ts:73` 一带的 `parseWith` | −7.6 ms/次（10.05 → 2.47 ms） | 手测目录加载 + 纯函数基准 | 低（校验退化） |
| H3 | 图谱数据 fastParse `[已抽查]` | `frontend/src/types/graphDemo.ts:117` `parseGraphDemoData` 现在 `safeParse` | −14.3 ms/次（15.32 → 0.99 ms） | `.tmp/perf/bench-graph.mjs` | 低 |
| F2 | axios → fetch | `frontend/src/lib/api-client.ts` | −17~20 kB gzip（axios 单片 20.6 kB 为上界） | 对照构建 + 401/超时/取消真机 | 中（错误分支） |
| H2 | 换源面板行级 memo | `frontend/src/components/book/SwitchSourceDialog.tsx:246` | 40 次全列表重渲染 → 20 次单行 | 面板实测 | 低 |
| F3 | zod 移出首屏链 | `frontend/src/lib/api-client.ts:2`、`services/auth.ts:1` | 关键链 −13.4 kB gzip | 首屏字节对比 | 中 |
| F4 | 落地路由 chunk `modulepreload` | vite 插件 | 省 1 次串行 RTT | 首屏瀑布图 | 低 |

---

## 已否掉（别再提案）

| 提案 | 否掉的理由 |
|---|---|
| 全局限流 `ConcurrencyLimitLayer` | `tower = "0.5"` 未启 `limit` feature，现在**编译不过**；且退化来自 4 核 Tokio/hyper 争用（对照组 `/health` 同样退化 5.4×），限流只是把排队换成 503 |
| `idx_book_sources_url` / `(user_namespace, enabled)` 索引 | 查询条件是 `book_source_url = ?2 OR LIKE '%/tail'`，前导通配让任何以该列为前导的索引失效；实测 3.27/3.50 ms，本来就不贵 |
| `skip_serializing_if` | 5 本书 240 个字段里显式 `null` 只占 1353 B（16%），且 gzip 后 201 849 → 29 789 B，收益≈0 |
| `COOKIE_STORAGE` 持锁跨 await / `LOAD_LOCK` 是 std Mutex | 两处事实错误：cookie 的 guard 是语句级临时量；`LOAD_LOCK` 本就是 `tokio::sync::Mutex`，且 10 s TTL + single-flight |
| `concurrentCount` 128→32 当性能项 | 是**出站** fanout 护栏，不影响延迟，只防单客户端 128 路外连 |
| `toc_cache`/`book_vars_cache` 大清理降扫描常数 | B3/B7 索引后已不扫表；`LIKE` 与表大小无关。真正该封顶的只有 `toc_cache` 字节数，`book_vars_cache` 有 `@get:{key}` 依赖，不动 |

---

## 前端抽查记录（captain 亲自复核）

- chunk 体积：本人 `pnpm build` 输出与文档 §1.1 一致（`index-q22jiYrq.js` 281.87 kB/gzip 91.44、`vendor-graph-D7iQuDfZ.js` 1,041.59 kB/gzip 329.47）。
- F1：`grep -rn "klay" frontend/src` 只有 `GraphDemoPage.tsx:20,61`（注册）、`package.json:29`、`types/cytoscape-plugins.d.ts:5`；`graphLayouts.ts:484` 的 `mode === "tree"` 走的是 `layoutFamilyTree`，全仓无 `name:'klay'` 的 layout 调用 → 确认是死重量。
- F6：`ls frontend/public/fonts | wc -l` = 607、`du -sh` = 32M，`src/`、`index.html`、`vite.config.ts` 里搜不到 `/fonts/` 引用 → 确认死资产。
- H1：`frontend/src/services/book.ts:73` 一带确实对目录做 `parseWith` 全量 zod。
- H3：`frontend/src/types/graphDemo.ts:117` `parseGraphDemoData` 用的是 `graphDemoSchema.safeParse` 全量校验。
- 前端验证文档未产出（两轮 agent 卡住），所以 H1/H3/F2/F3/F4 的**数字**仍来自 deepseek 画像，只有代码位置是复核过的。

## 复跑脚本

`.tmp/perf/` 下：`v3_sql.py`（B7）、`v5_toc.py`（B3）、`v8_sources.py`（B4）、`pbench/`（B8 的 Rust 解析基准）、`bench-graph.mjs`/`bench-rank.mjs`（H3/H5）、`attr.mjs`/`agg.mjs`/`slicegz.mjs`（体积归因）、`exp/`（F1/F2 对照构建）。
