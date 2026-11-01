# 方案 B：让书源置信度可证伪（Beta 后验 + 时间衰减 + 长度离群 + 探索项）

> 数字来自 `FACTS.md`（§1/§2/§3），生产库复算见 §6，一律带 `user_namespace='testuser01'`。

## 1. 新公式（可直接落地）

对着 FACTS §3 逐条开方：小样本 → Beta 后验下界；无衰减 → 14 天半衰期；均值延迟 → 指数速度因子；`rich` 是死常数 → 删权重；`toc_rel/cont_rel` 缺证据抄上一级 → **不再补全**，缺证据的通道不进加权；`attempts==0 → 0.5` → **不再特殊化**，无证据就返回先验（均值 0.5、下界 0.211），由 UCB 决定是否探索。

其中"抄上一级"最坏：把"没证据"伪装成"有证据且等于搜索成功率"，这正是 1333 行双无证据（FACTS §2）稳坐 `0.4-0.6` 桶（1155 行，全表 68%）的原因。
```rust
const W_SEARCH: f64 = 0.50;   // 搜索可假阳性 → 权重不是最高
const W_TOC: f64 = 0.30;
const W_CONTENT: f64 = 0.20;
const HALF_LIFE_MS: f64 = 14.0 * 86_400_000.0;  // 8 周前的成功权重 = 0.5^4 = 1/16
const LAT_TAU_MS: f64 = 4_000.0;
const SPEED_FLOOR: f64 = 0.55;   // 纯乘法惩罚会误杀慢而正确的源，见 §5
const PRIOR_A: f64 = 0.5;        // Jeffreys 先验：无证据 → Beta(0.5,0.5)，均值 0.5、sd = 1/√12
const PRIOR_B: f64 = 0.5;

pub struct Confidence { pub lcb: f64, pub mean: f64, pub ucb: f64, pub n_eff: f64 }

/// `now_ms` = `chrono::Utc::now().timestamp_millis()`，由调用方传入（可测）。
pub fn score(&self, now_ms: i64) -> Confidence {
    let decay = 0.5_f64.powf((now_ms - self.updated_at).max(0) as f64 / HALF_LIFE_MS);
    // 三条流水各自衰减；衰减后 n < 0.05 视为"这条通道等于没有证据"
    let ch = [
        ((self.attempts - self.timeouts).max(0) as f64 * decay,
         self.successes.min((self.attempts - self.timeouts).max(0)) as f64 * decay, W_SEARCH),
        (self.toc_attempts as f64 * decay, self.toc_successes.min(self.toc_attempts) as f64 * decay, W_TOC),
        (self.content_attempts as f64 * decay,
         self.content_successes.min(self.content_attempts) as f64 * decay, W_CONTENT),
    ];
    let live: Vec<_> = ch.iter().filter(|(n, _, _)| *n > 0.05).collect();
    let w: f64 = live.iter().map(|(_, _, wt)| wt).sum();
    // 没有证据时不伪装：后验 = 先验，均值 0.5、var = 1/12 → lcb = 0.5 - 0.289 = 0.211。
    // 无证据的源排不进队首，也绝不会被当死源。
    let (mean, var) = if w > 0.0 {
        let mean = live.iter()
            .map(|(n, s, wt)| (wt / w) * (s + PRIOR_A) / (n + PRIOR_A + PRIOR_B)).sum::<f64>();
        let var = live.iter().map(|(n, s, wt)| {
            let (a, b, k) = (s + PRIOR_A, n - s + PRIOR_B, wt / w);
            k * k * a * b / ((a + b) * (a + b) * (a + b + 1.0))
        }).sum::<f64>();
        (mean, var)
    } else {
        (0.5, 1.0 / 12.0)
    };
    let sd = var.sqrt().min(0.5);
    // 速度因子：1.0 → SPEED_FLOOR 指数衰减，不是 (0.3 + 0.7×(1-lat/30s)) 的一次式
    let avg_ms = if self.attempts > 0 { (self.lat_sum_ms / self.attempts).max(0) as f64 } else { 0.0 };
    let speed = SPEED_FLOOR + (1.0 - SPEED_FLOOR) * (0.4 + 0.6 * (-avg_ms / LAT_TAU_MS).exp());
    Confidence {
        lcb: ((mean - sd) * speed).clamp(0.0, 1.0),   // 排序 / 死源用
        mean: (mean * speed).clamp(0.0, 1.0),          // 展示用
        ucb: ((mean + sd) * speed).clamp(0.0, 1.0),    // 探索用
        n_eff: ch.iter().map(|(n, _, _)| n).sum(),     // 衰减后有效样本量
    }
}
```

**列来源**（对照 `storage/mod.rs:1714-1736`）：`attempts/successes/timeouts/lat_sum_ms` ← `record_search_outcome_state`（`mod.rs:1760`，仅搜索记账，分母仍剔除 `timeouts`）；`toc_*` ← `record_path_outcome("toc")`（`service/book.rs:589,602`）；`content_*` ← `record_path_outcome("content")`（`api/router.rs:4812,4850`）；`updated_at` ← 三个写入点都写（`mod.rs:1805`/`:1856`），是**唯一衰减基准**，只取过目录的源同样会衰减；`consec_failures` 只留给死源语义、**不进公式**；`intro_hits/intro_count` **不再使用**（§2.1，列保留）。

**为什么不用 Wilson**：Wilson 是单通道频率下界，这里有三个异构通道——FACTS §2.2 里 `toc=0/87` 与 `search=91/92` 在同一行打架，必须加权合并。Beta 在「均值 − 1 后验 sd」处与 Wilson 下界在 n≥10 时相差 < 0.03，但天然支持跨通道方差合成与先验注入。**半衰期为何 14 天**：全表 `updated_at` 距今是双峰（0-1 天 1382 行、52-55 天 314 行），14 天让 55 天前证据权重降到 6.5%——陈旧源沉底但不一次性清零（FACTS §3.2 反向也成立：源上周修好了不该背着 150 次旧失败）。

## 2. 数据可得性：现有列 + 3 个新增列

**删 `rich`**：FACTS §3.4 说 `intro_count=0` 时固定 0.5，等于给所有源加同一常数（全表 90% 行如此），**不携带信息**。0.15 权重改给 `W_CONTENT`（0.15 → 0.20）——正文比简介更能代表"这个源还能用"。

新增 3 列（`backend/src/storage/migrate.rs`，与 `ensure_column_typed` 同风格、幂等）：

```sql
ALTER TABLE source_stats ADD COLUMN content_bytes_sum INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_stats ADD COLUMN content_bytes_n   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_stats ADD COLUMN last_probe_at     INTEGER NOT NULL DEFAULT 0;
```

- `content_bytes_sum/content_bytes_n`：写入点就是 `api/router.rs:4812` 那次 `record_path_outcome(...,"content",...)` 之后，同请求同作用域内加 `record_content_bytes(ns, url, content.len())`——`content` 已在作用域里（`router.rs:4806-4812`），**零额外网络请求**。这是 §3 的全部数据基础。
- `last_probe_at`：写于死源放行时（§4.2），取代现在靠 `updated_at` 隐式实现的探针逻辑。

## 3. 内容正确性信号（串章/垃圾标题）

FACTS §2.3：干净正文 3215 字，串章源 21635/25771/226494/304567，垃圾标题源 `17|0|OK` 之类 0-569 字。现模型一列都测不出。零网络近似（比的是源自己的历史平均，不是单章）：

```rust
const SOURCE_BYTES_MEDIAN: f64 = 3714.0;   // 生产实测中位数，见 §6

pub fn content_len_flag(&self) -> f64 {
    if self.content_bytes_n < 3 { return 1.0; }   // 样本不够，不判
    let avg = self.content_bytes_sum as f64 / self.content_bytes_n as f64;
    if avg > 5.0 * SOURCE_BYTES_MEDIAN { return 0.0; }  // 串章：21635 是 3215 的 6.7 倍
    if avg < 500.0 { return 0.0; }                      // 垃圾标题/空壳：17 / 174 / 569
    1.0
}
// 乘进质量分（乘不加权 —— 一票否决性质）：lcb *= flag;  mean *= flag;
// ucb 不乘，保留探索可能。
```

**代价与局限**：① 只能抓整源系统性偏差——单章天然差 10 倍正常（生产分位 p25=2134/p50=3714/p75=4376/p95=18814），所以判的是 `content_bytes_n>=3` 的平均。② 对"偏大 1.8 倍"（5718 字）**无效**；真能判的是**章节数一致性**——`book_chapters` 行数 vs `toc_cache.chapters_json` 条数可直接 join（生产 888 行能 join 上），代价仅一次本地 SQLite 查询（~0.1ms）、无网络，但 `book_chapters` **无 `source_url` 列**，需新增（写入点 `cache_chapter_content`，`router.rs:4829`），作第二阶段。③ 测不出"标题是 `17|0|OK`"本身；近似是 `title.len()<4` 或数字/竖线/等号占比 > 30%，数据在 `book_chapters.title`，同样缺 `source_url`，共用一次迁移。**第一阶段覆盖度**：FACTS §2.3 的 4/4 串章源（全部 > 5×3714）与 3/3 垃圾标题源（全部 < 500）。

## 4. 与消费点的契约
**排序用 `lcb`（保守、抗抖），探索用 `ucb`（乐观、给机会），展示用 `mean`。** 现公式只有一个数，所以"没证据"和"证据差"在排序里是同一件事——这是 1155 行挤在 0.4-0.6 桶的根源。

**4.1 搜索排序（`api/router.rs:2936-2952`、`:9420-9440`）**：

```rust
let now = chrono::Utc::now().timestamp_millis();
for st in &stats_rows {
    let c = st.score(now);
    // 队首按 lcb 排（证据不足的源拿不到队首）；
    // 但 n_eff < 3 时按 ucb 排 —— 否则「证明自己」永远无法启动（冷启动死锁）。
    map.insert(st.source_url.clone(), (
        if c.n_eff < 3.0 { c.ucb } else { c.lcb },
        st.avg_latency_ms(),
    ));
}
// 缺行仍给 (0.5, 3000)，但语义变了：它现在走 ucb（乐观）分支，
// 不再伪装成"有证据的中性分"。与 lcb 接近 0 的差源同台时自然沉底。
```

**4.2 死源判定（`api/router.rs:9911` `is_dead_source`）**：

```rust
fn is_dead_source(st: &SourceStat, now_ms: i64) -> bool {
    let c = st.score(now_ms);
    c.lcb <= 0.05
        && c.n_eff >= 5.0                                  // 不再用 attempts，见下
        && now_ms - st.last_failure_at <= 6 * 3_600_000
        && now_ms - st.last_probe_at >= 6 * 3_600_000      // 新增：6h 到期必放行一次探针
}
```

`0.05` **数值不变**，语义从"点估计被打压"变成"后验下界被打穿"，更抗抖；`attempts>=5 → n_eff>=5.0` 修 FACTS §3.8 的口径 bug（`attempts` 只统计搜索，只取过目录的源永远进不了门槛）；`last_failure_at` 6h 窗口保留（`mod.rs:1796` 写）；新增 `last_probe_at` 放行是对着 FACTS §3.6（硬阈值靠 `consec_failures`，一次抖动就连败 3 次），放行后源每 6h 必被探一次，写入点在 `dead_source_urls`（`router.rs:9935` 附近）。生产复算：现役 611 行里 `lcb<=0.05 && n_eff>=5` 的 14 行、7 天内有失败的 10 行——**10 个源被跳过，占现役 1.6%**（现公式同条件命中 0 个，因为 6h 窗口内没人失败）。

**4.3 前端 SourcesPage（`frontend/src/pages/SourcesPage.tsx:184-217`）**：阈值必须跟着改，因为分布整个变了。现役 611 行复算 `lcb` 分位 p10=0.363/p50=0.536/p90=0.709；`ucb>=0.7` 有 306 行、`lcb>=0.7` 只有 64 行。

| 位置 | 现状 | 改为 |
|---|---|---|
| 差源 `:190` | `confidence < 0.4 && attempts >= 3` | `lcb < 0.40 && n_eff >= 3.0` |
| 优质 `:198` | `confidence >= 0.7 && attempts >= 5` | `lcb >= 0.70 && n_eff >= 8.0` |
| 排序 `:207` | `stat.confidence` | `stat.lcb`（默认）/ 切「潜力」时用 `ucb` |
| 新增 | — | 一列「证据量」显示 `n_eff`，`n_eff<3` 标「待验证」 |

差源 `0.4` **不变**：现役 `<0.20` 有 0 个、`<0.45` 有 51 个，0.4 落在合理分界。优质 `0.7` **不变**但换门限：`lcb>=0.7` 有 64 行（旧公式全表才 34 行），语义正是"证据充分且下界很高"；样本量抬到 8，因 `lcb` 已含样本惩罚，`n_eff` 只需防"3 次全中标优质"。

## 5. 回归风险与防护

| 会被误伤的源 | 机理 | 防护 |
|---|---|---|
| **慢但正确的大站**（`lat_sum/att` 最差 6 个：58xs.la 84s、kan76 81s、dmxs.me 80s、zhuiyo 80s、hmba.vip 79s、trxs.org 79s；共 187 行均值 > 30s） | 现公式 `speed = 1 - lat/30s` 让它们**直接归零** | 指数 + `SPEED_FLOOR=0.55`：80s 源从 0.30 抬到 ≈0.55，仍沉队尾但不等于死源——**把"慢"和"坏"分开** |
| **只取目录、从没搜过的源**（FACTS §2：1333 行双无证据、362 行只有目录证据） | `n_eff` 只有 2-3，`lcb` 被后验方差压到 0.2-0.4 | §4.1 的 `n_eff<3 → ucb` 分支；复算现役 `n_eff<3` 有 200 行，全部走 ucb，不会永久沉底 |
| **本机/自建源**（如 `http://172.17.0.1:9911`，旧分 0.7） | 延迟极低、样本大，旧公式被 `speed=1.0` 抬高 | 复算新 `mean=0.395`、`lcb=0.199`，**确实降了**。上线前跑全量双算 diff，凡 `|new_lcb - old| > 0.25` 且 `enabled=1` 的打印清单人工过一遍 |
| **删掉又重建的同名源**（FACTS §3.7，1083 行孤儿行） | 新账继承旧账 | **衰减帮了忙**：孤儿行 `updated_at` 是 52-55 天前（实测 314 行），衰减后 `n_eff<1` 等同无证据 → 走 ucb 重新证明自己 |
| **`consec_failures>=3` 抖动源**（FACTS §3.6） | 一次抖动连败 3 次 | 新公式不用 `consec_failures` 打分，再加 §4.2 的 6h 探针放行 |

## 6. 验收指标（一个可度量的数）

**主指标：矛盾源与双高源是否仍然排序倒挂。** 取现役源里 `attempts>=10 && toc_attempts>=3` 的集合（FACTS §2.2 口径，现役 19 行）与 `attempts>=20 && 搜索成功率>=0.9 && toc>=3 && 目录成功率>=0.9` 的双高源（现役 5 个），新旧公式各算一次：

```python
# 只读生产库，一次查询：现役源里 attempts>=10 && toc_attempts>=3 的行（FACTS §2.2 口径）
rows = c.execute("""select s.* from source_stats s join book_sources b
    on b.book_source_url = s.source_url and b.user_namespace = s.user_namespace
    where s.user_namespace='testuser01' and s.attempts>=10 and s.toc_attempts>=3""").fetchall()
# 每行算 old(source_stat.rs:32-70) 与 new(§1.2)：矛盾源中位分 vs 双高源最低分
```

已跑出的结果（现役 611 行，2026-10-31 快照）：

| 指标 | 旧公式 | 新公式 |
|---|---|---|
| 矛盾源（现役 19 个）中位分 | 0.460 | `mean` 0.494 / **`lcb` 0.445** |
| 矛盾源中 `lcb<=0.10`（沉底） | 0 | **1**（`toc=0/87 search=91/92` 的 `m.zhizihuan.com`，`lcb=0.142`） |
| 双高源（现役 5 个）最低分 | **0.340** ← 比矛盾源中位分还低，倒挂 | **`lcb` 0.602** |
| 现役源 `lcb` p10/p50/p90 | — | 0.363 / 0.536 / 0.709 |

**通过线**：矛盾源中位分 − 双高源最低分 > 0。旧公式 **−0.120**（倒挂），新公式 **+0.157**。

**副指标**（同一份数据顺手算）：① 中性桶塌缩——旧 `0.4-0.6` 桶 1155 行（全表 68%），新公式现役 356 行里 `<0.2` 0 个 / `0.2-0.45` 51 个 / `>=0.45` 305 个，不再是"大家都 0.5"。② 抖动不再误杀——旧 `<=0.05` 的现役 68 行里 12 行新 `lcb>=0.45`。③ 陈旧源沉底——`updated_at>28 天` 的源 0.022 → 0.063，仍留回升路径。

**上线前必跑的双算 diff**（`source_stat.rs` 的 `#[cfg(test)]`，该文件已有 `legacy_confidence()` 可对照）：断言"矛盾源中位分 − 双高源最低分 > 0"，且 `|new_lcb - old| > 0.25` 的行数 < 现役源的 5%（防 §5 的慢源/本地源回归）。
