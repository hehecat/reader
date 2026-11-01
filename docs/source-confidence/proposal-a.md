# 书源置信度重做（方案 A）

依据 `docs/source-confidence/FACTS.md`（下称 F）。生产库实测：`source_stats` 1698 行（现役 615 源中 611 行有数据），`user_namespace='testuser01'`。结论先说：现公式在 1698 行里有 **1052 行落在 0.45–0.55**（旧分位 p20–p80 全等于 0.5）——它测不出好坏，只是把「有证据但很差」和「完全没证据」写成同一个数。

## 1. 新公式

三个数字，语义不同、分母不同、用途不同：`point` = 该源下一次被调用时**能给出可用内容的概率**（Beta 后验均值，通道间几何池化）→ 排序/UI 主数值；`lo`/`hi` = 同量的 95% Wilson 区间 → **只有 `lo` 够高才敢说「优」，只有 `hi` 够低才敢说「确信差」**，小样本因此不再伪装；`Verdict` = 把「判死」从分数阈值里摘出来（旧代码把两件事都塞进 `confidence()<=0.05`，`router.rs:9911`）。

`backend/src/model/source_stat.rs`，替换现 32–70 行：

```rust
pub const DECAY_HALF_LIFE_MS: f64 = 30.0 * 24.0 * 3600.0 * 1000.0; // 30 天半衰期
pub const MIN_EFFECTIVE_N: f64 = 0.5;   // 有效样本低于此 → 该通道无证据
pub const POOL_FLOOR: f64 = 0.02;       // 几何池化下限，避免 ln(0)
pub const Z95: f64 = 1.959_963_984_540_054;
pub const W_SEARCH: f64 = 0.35;         // 通道权重，缺失通道按比例重归一
pub const W_TOC: f64 = 0.30; pub const W_CONTENT: f64 = 0.20; pub const W_SANE: f64 = 0.15;
pub const STRONG_LO: f64 = 0.7;         // 与前端 0.7 对齐，但作用于 lo
pub const WEAK_HI: f64 = 0.4;           // 与前端 0.4 对齐
pub const DEAD_EVIDENCE_TTL_MS: i64 = 30 * 24 * 3_600_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Verdict { Unknown, Dead, Weak, Ok, Strong }
#[derive(Debug, Clone, Copy)] pub struct Conf { pub point: f64, pub lo: f64, pub hi: f64 }
/// 95% Wilson 区间；n=0 → (0,1) 全不确定
fn wilson(ok: f64, n: f64) -> (f64, f64) {
    if n <= 0.0 { return (0.0, 1.0); }
    let (p, z2) = ((ok / n).clamp(0.0, 1.0), Z95 * Z95);
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    ((center - half).max(0.0), (center + half).min(1.0))
}
impl SourceStat {
    /// 无任何有效通道 → None（「没有证据」是 None，不是 0.5，也不是 0）
    pub fn evaluate(&self, now_ms: i64) -> Option<Conf> {
        // 单通道：(权重, 衰减后成功数, 衰减后样本数)。样本不足 → 整条通道消失
        let ch = |w: f64, ok: i64, n: i64, anchor: i64| -> Option<(f64, f64, f64)> {
            if n <= 0 { return None; }
            let f = 0.5_f64.powf((now_ms - anchor).max(0) as f64 / DECAY_HALF_LIFE_MS);
            let n_eff = n as f64 * f;
            if n_eff < MIN_EFFECTIVE_N { return None; }
            Some((w, (ok as f64 * f).min(n_eff), n_eff))
        };
        // 锚点缺失（老库未回填）时退化用 updated_at，偏乐观，见 §2 末段
        let toc_at = if self.toc_updated_at > 0 { self.toc_updated_at } else { self.updated_at };
        let cont_at = if self.content_updated_at > 0 { self.content_updated_at } else { self.updated_at };
        let chs: Vec<_> = [ // 搜索通道：n = attempts（超时算未送达），ok = successes
            ch(W_SEARCH, self.successes, self.attempts, self.updated_at),
            ch(W_TOC, self.toc_successes, self.toc_attempts, toc_at),
            ch(W_CONTENT, self.content_successes, self.content_attempts, cont_at),
            ch(W_SANE, self.sane_hits, self.sane_attempts, cont_at),
        ].into_iter().flatten().collect();
        if chs.is_empty() { return None; }
        let wsum: f64 = chs.iter().map(|c| c.0).sum();
        let (mut lp, mut ll, mut lh) = (0.0, 0.0, 0.0);
        for (w, ok, n) in chs {
            let (mean, (lo, hi)) = ((ok + 0.5) / (n + 1.0), wilson(ok, n)); // Beta(½,½) 后验均值
            lp += w * mean.max(POOL_FLOOR).ln();   // 几何池化：串联链路，任一环失效即不可用
            ll += w * lo.max(POOL_FLOOR).ln();
            lh += w * hi.max(POOL_FLOOR).ln();
        }
        Some(Conf { point: (lp / wsum).exp(), lo: (ll / wsum).exp(), hi: (lh / wsum).exp() })
    }
    /// 判死与打分分开：死亡证据用**剔除超时**的分母（超时是慢不是死），
    /// 且证据会陈旧——30 天没再失败就不再判死（替代 router.rs:9911 的 6h 窗口）。
    pub fn is_dead(&self, now_ms: i64) -> bool {
        if self.last_failure_at <= 0 || now_ms - self.last_failure_at > DEAD_EVIDENCE_TTL_MS { return false; }
        let decisive = (self.attempts - self.timeouts).max(0) as f64;
        self.consec_failures >= 3
            || (self.attempts >= 10 && decisive >= 2.0 && self.successes == 0)
            || wilson(self.successes as f64, decisive).1 < 0.25
    }
    pub fn verdict(&self, now_ms: i64) -> Verdict {
        if self.is_dead(now_ms) { return Verdict::Dead; }
        match self.evaluate(now_ms) {
            None => Verdict::Unknown,
            Some(c) if c.lo >= STRONG_LO => Verdict::Strong,                 // 优必须「被证明」
            Some(c) if c.hi < WEAK_HI || c.point < WEAK_HI => Verdict::Weak, // 差可以是「最佳猜测」
            Some(_) => Verdict::Ok,
        }
    }
}
```

**每一列的来源**（全部现有列，无一条需要联网）：

- 搜索通道：`successes` / `attempts`（现有），衰减锚点 `updated_at`。
- 目录通道：`toc_successes` / `toc_attempts`（现有，写点 `service/book.rs:589/:602`），锚点 `toc_updated_at`（**新增**）。
- 正文通道：`content_successes` / `content_attempts`（现有，写点 `router.rs:4812/:4850`），锚点 `content_updated_at`（**新增**）。
- 内容可信通道：`sane_hits` / `sane_attempts`（**新增**，写点 `router.rs:4812`，同一次调用、零网络），复用 `content_updated_at`。

判死不用概率通道，直接用现有 `consec_failures` / `last_failure_at` / `timeouts`，配 30 天 TTL。

移除的三处伪装：① `attempts==0 → 0.5`（F §2.1：947 行靠它变中性，其中 498 行目录成功率<50%）；② `toc_rel←rel`、`cont_rel←toc_rel` 的「抄上级」；③ `rich`（`intro_hits/intro_count`，仅 195/1698 行有数据，测的是搜索富信息而非可读性）。移除速度因子 `×(0.3+0.7×speed)`：它用全期均值把慢源整体压到 0.3 倍（F §3.3），并让 0.4/0.7 失去可解释性；延迟退回它该在的位置——排序第二键（`router.rs:2942` 已在用 `avg_latency_ms()`），UI 仍展示 `avgLatencyMs`。

## 2. 数据可得性：4 个新增列

写在 `storage/mod.rs` 的 `ensure_source_stats_table()`（现 1710–1755 行）末尾，复用 `ensure_column_typed`（6317 行，幂等 `ALTER TABLE ADD COLUMN`，旧库自动升级）：

```rust
ensure_column_typed(pool, "source_stats", "toc_updated_at", "INTEGER NOT NULL DEFAULT 0").await?;
ensure_column_typed(pool, "source_stats", "content_updated_at", "INTEGER NOT NULL DEFAULT 0").await?;
ensure_column_typed(pool, "source_stats", "sane_attempts", "INTEGER NOT NULL DEFAULT 0").await?;
ensure_column_typed(pool, "source_stats", "sane_hits", "INTEGER NOT NULL DEFAULT 0").await?;
// 回填：老行的通道锚点 = 行最后写入时间（一次性、幂等）
sqlx::query("UPDATE source_stats SET toc_updated_at = updated_at WHERE toc_updated_at = 0 AND toc_attempts > 0").execute(pool).await?;
sqlx::query("UPDATE source_stats SET content_updated_at = updated_at WHERE content_updated_at = 0 AND content_attempts > 0").execute(pool).await?;
```

写入点两处，各 +1 列，无读改写、无事务：`storage/mod.rs:1833` `record_path_outcome` 的 upsert 里把 `toc_updated_at`/`content_updated_at` 写成本次 `now`；新增 `record_content_sanity(ns, source_url, sane: bool)`（`sane_attempts+1`、`sane_hits + sane as i64`、`content_updated_at = now`）。
**不新增「衰减后计数」列**：`d_*` 快照要写路径读改写+并发重试，而「通道锚点 + 读时衰减」等价且零额外写入。老库无新列或回填未跑时锚点为 0 → 退化用 `updated_at`，不会把一切判成 None。

## 3. 内容正确性信号（F §2.3：串章 / 垃圾标题，现模型全算成功）

不需要任何网络：判断所需两个量在写账那一行已在作用域内——`content: String` 与 `chapter_title_ctx`。`service/content_sanity.rs`（新文件，纯函数，可单测）：

```rust
/// 0 网络。阈值来自生产实测（745 本本地缓存书 / 3525 章）：干净首章 ≈ 3215 字（F §2.3）；
/// 117/745 本整本章节中位 > 40000 字（串章）；319/3525 章 < 200 字（空/占位正文）；
/// 18 章标题形态垃圾（含 '|'、无 CJK、>40 字）。
pub fn chapter_is_sane(title: &str, content: &str) -> bool {
    let (len, t) = (content.chars().count(), title.trim());
    (200..=40_000).contains(&len)                            // 空正文 / 串章（串章源 21635…304567 字）
        && !t.is_empty() && t.chars().count() <= 40
        && !t.contains('|') && !t.contains('\\')             // "17|0|OK" / "174|0|OK"
        && t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}
```

覆盖与代价：**0 次网络请求，0 次额外 SQL**（只对已在内存的字符串做长度/字符判断），每次正文抓取 +1 次 upsert（可与现有 `record_path_outcome` 合并）。局限说清楚：测不出「张冠李戴但长度正常」的正文（抓到别的小说的正常章节）——那要拿同一章比对第二个源，是网络代价，明确不做。当前只有 54 行 `content_attempts>=3`，这通道初期几乎全空：它是**新数据才长出来**的通道，缺了就按比例重归一，不误伤老源。

## 4. 与消费点的契约

| 位置 | 现状 | 改成 | 阈值动不动 |
|---|---|---|---|
| `router.rs:2936-2952`、`:9420-9440` 搜索排序 | `st.confidence()` + 缺行 `unwrap_or((0.5,3000))` | `st.evaluate(now).map(\|c\| c.point).unwrap_or(0.5)`；第二键仍 `avg_latency_ms()` | **不动**。0.5 保留为「未知源占位」维持探索，但现在是显式 fallback，不再是「算出来的 0.5」 |
| `router.rs:9911` `is_dead_source` | `confidence()<=0.05 && attempts>=5 && now-last_failure_at<=6h` | 函数体改 `st.is_dead(now_ms)`，签名不变，`dead_source_urls()`/调度器零改动 | **改判定逻辑，不改数值**。实测现规则命中 **0/1698**（137 行满足 `conf<=0.05 && attempts>=5`，但 `last_failure_at` 最新也距今 6.6h，全被 6h 窗口挡掉）→ 新规则命中 **34 行，全现役** |
| `router.rs:4877` `getSourceStats` + `services/sources.ts:323` zod schema | `confidence: round2` / 7 字段 | 加 `confidenceLo/Hi`（可 null）、`verdict`、`saneAttempts/Hits`；`confidence` 改 `Option<f64>`，zod 改 `.optional()`（`stripNullsDeep` 剥 null） | — |
| `SourcesPage.tsx:185/:194` `poorSet`/`premiumSet` | `conf<0.4 && attempts>=3` / `>=0.7 && attempts>=5` | `verdict==='weak'\|\|'dead'` / `verdict==='strong'`；同文件 `:96` 的第三套死源口径 `attempts>=5 && successRate<0.2` 删除，统一用 `verdict==='dead'` | **0.4/0.7 保留、改判据**：0.7 作用于 `lo`，0.4 作用于 `hi`/`point`，不再作用于裸 `confidence`；三套死源口径合一 |
| `SourceListItem.tsx:131` 徽标 | `attempts>=3` 才显示，0.7/0.4 上色 | 按 `verdict` 上色：strong=accent、ok=muted、weak/dead=danger、unknown=灰「未评估」 | 门槛 `attempts>=3` → `verdict!=='unknown'` |

阈值为什么是「保留数字、改判据」而不是整体重标定：新 `point` 是概率，0.4/0.7 有了直白含义（「四次成一次」「十次成七次」）。但**优必须被证明、差可以是猜测**——两类错误代价不对称：把好源标成差只是排后面，把差源标成优会让用户白等。实测落地数量：优 `lo>=0.7` **48 行**（现役 26，旧 67）；中 779（392）；差 `hi<0.4||point<0.4` **836**（现役 193，旧口径 413）；未评估 35（现役 0，另有 4 个现役源无 stats 行，旧公式把它们都伪装成 0.5）；死 **34，全现役，旧 0**。

## 5. 回归风险与防法

| 会误伤谁 | 为什么 | 防法 |
|---|---|---|
| a. 超时密集但可用的源（`timeouts` 占 6155/22831 = 27%；F §3 说 deadline 上线后还会涨） | 搜索通道 `n=attempts`，超时算未送达 → 8 成功/50 超时 从旧 0.29 掉到 0.139 | ① 判死走剔除超时的分母，超时**永不**计入死亡证据；② 若 deadline 上线后 `strong` 现役数从 26 掉到 <10，把搜索通道改成 `ok = successes + 0.5×timeouts`（一行）；③ M2 每次上线都跑 |
| b. 只搜不读的源（`toc_attempts=0`，如 qidian 41/44 无目录证据） | 搜索 0.90 会排在「有目录证据但成功率 0.6」的源前面 | `Strong` 要求 `lo>=0.7`，带 `toc_attempts>=3` 的实测只剩 5 行；需要更强约束时可给 `Strong` 追加 `toc_attempts>=3`（现役 26→5，偏严，先不加） |
| c. 慢而可靠的源 | 去掉速度因子后 0.26 → 0.90（qidian） | 这是修复不是误伤：`point` 只回答「能不能给内容」，慢由排序第二键与 `avgLatencyMs` 表达；调度器 `router.rs:9878` 的窗内派发用自己的 `success_rate+latency`，未受影响 |
| d. 孤儿行（1083/1698 不在 `book_sources`，F §3.6） | 会被算进 weak/strong 计数污染 UI | UI 只渲染 enabled 源（已如此）；另建议一次性 dry-run `SELECT count(*) FROM source_stats s WHERE s.user_namespace='testuser01' AND s.source_url NOT IN (SELECT book_source_url FROM book_sources WHERE user_namespace='testuser01')` → 确认 1083 后再删，**不自动执行** |
| e. 极小样本（1/1） | 点估计 0.75 看着「不错」 | `lo=0.207` 进不了 `Strong`，徽标「中」；旧公式给 0.98 且无区间 |
| f. 正常但单章很长的书（实体书单章 1.5 万字） | `len>40000` 硬阈值 | 阈值取干净章的 12 倍（3215×12≈40k），本地实测只 117/745 本越线、263 本落在 2000–6000 正常带；且 `sane` 权重 0.15，单次 `false` 不足以打到 weak |

## 6. 验收指标（现成数据，一条命令出前后对比）

**主指标 M1 — 假中性率**：`point ∈ [0.45,0.55]` 的行占比。旧公式 **1052/1698 = 62%**（分位 p20–p80 全 = 0.5），新公式 **108/1663 = 6.5%**。这就是「更可信」的定义：一个分数只有不把 62% 的行塞进同一个桶，它才携带信息。

| 分位 | p10 | p20 | p30 | p40 | p50 | p60 | p70 | p80 | p90 |
|---|---|---|---|---|---|---|---|---|---|
| 旧 `confidence()` | 0.05 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.586 |
| 新 `point` | 0.169 | 0.251 | 0.274 | 0.301 | 0.394 | 0.460 | 0.707 | 0.749 | 0.815 |

复算脚本：把 §1 的 `evaluate` 逐行译成 python（读 `select * from source_stats where user_namespace='testuser01'`，`NOW = max(updated_at)`），对每行输出 `(point, lo, hi, verdict)` 再统计上表；本方案全部数字由此产出，原始实现见 `/tmp/sc/sim*.py`，落地时放进 `scripts/` 作回归对比基线。三个支撑指标（同一脚本一起出）：

- **M2 死源闸门真阳性**：旧 `is_dead_source` 命中 **0/1698**；新 `is_dead` 命中 **34 行，全现役**（207 行 `consec_failures>=3`，其中 173 行失败证据已超 30 天，按设计不再判死）。证明「死源判定从没生效过」被修好。
- **M3 F §2.1 的 498 行**（`attempts=0` 且目录成功率<50%）：中位 **0.500 → 0.252**，`point<0.4` 占比 **0% → 473/498 = 95%**，其余 22 行因目录样本衰减到 <0.5 转为 `unknown`（也是诚实结果）。
- **M4 判别力冒烟**：负例 = F §2.2 的 26 行（`attempts>=10 && toc>=3 && toc 成功率<50%`），正例 = 82 行（同条件但目录成功率 ≥90%），AUC **0.638 → 0.728**（按 `lo` 排 0.680）。这条部分循环（正负例本身按目录成功率切），只作冒烟，不作主指标。

## 7. 不做什么

不抓源站页面验证（无网络）；不做跨源正文比对（需抓第二个源，另立项）；不做衰减后计数列（读改写复杂度换不来精度）；不自动清理 1083 行孤儿（只给 dry-run 计数）；不动 `router.rs:9878` 的窗内派发排序（该处注释明确不复用 `confidence()`）。

## 8. 测试迁移（`source_stat.rs` 现有 5 个测试）

`timeout_heavy_good_source_not_judged_dead`（60/8/50）**原样通过**：新 `point=0.139 > 0.1` 且 `is_dead=false`。`all_timeouts_is_undecided_not_dead`（12/0/12）新 `point=0.038`（12 次全超时确实从没送达），`>0.05` 那条改 `assert!(!st.is_dead(now))`——测试名说的是「不判死」，判死现在由 `is_dead` 回答。`real_bad_source_still_judged_dead` 的 2a/2c（20/0/0、20/0/18）**原样通过**（`point=0.024`，`is_dead=true`），2b（20/1/0）`point=0.071 > 0.05` 故改判 `assert!(st.is_dead(now))`（`wilson(1,20).hi=0.154 < 0.25`）。`rows_without_timeouts_unchanged` **删除**——它把「与旧公式逐位一致」写成契约，正是本次要废弃的。`cold_start_unchanged`（`attempts=0`）改为 `assert!(stat(0,0,0,0,0).evaluate(now).is_none())`。

另需在 `stat()` 构造器补 `toc_updated_at`/`content_updated_at`/`sane_*` 字段，并给 `last_failure_at` 传 `now-1h`（`is_dead` 要求失败时间戳新鲜，否则连败源不再判死）。
