# 书源置信度：当前实现 + 实测数据（两个 agent 的公共事实底稿）

> 这是任务底稿，不是结论。任何一条都可以在生产库里重新验证（只读）。

## 1. 当前公式（`backend/src/model/source_stat.rs:32-70`）

```rust
pub fn confidence(&self) -> f64 {
    if self.attempts == 0 { return 0.5; }              // ← 早退
    let decisive = (self.attempts - self.timeouts).max(0) as f64;
    let rel  = if decisive > 0.0 { self.successes as f64 / decisive } else { 0.5 };
    let toc_rel  = if self.toc_attempts > 0 { self.toc_successes as f64 / self.toc_attempts as f64 } else { rel };
    let cont_rel = if self.content_attempts > 0 { self.content_successes as f64 / self.content_attempts as f64 } else { toc_rel };
    let avg_lat = self.lat_sum_ms as f64 / self.attempts as f64;
    let speed = (1.0 - (avg_lat / 30000.0).min(1.0)).max(0.0);
    let rich = if self.intro_count > 0 { self.intro_hits as f64 / self.intro_count as f64 } else { 0.5 };
    let quality = 0.55 * rel + 0.15 * toc_rel + 0.15 * cont_rel + 0.15 * rich;
    let mut c = quality * (0.3 + 0.7 * speed);
    if self.consec_failures >= 3 || (self.attempts >= 5 && rel < 0.2) { c = c.min(0.05); }
    c.clamp(0.0, 1.0)
}
```

字段来源（`source_stats` 表，`backend/src/storage/mod.rs:1714-1736`）：
- `attempts/successes/timeouts/lat_sum_ms/intro_hits/intro_count` 由**搜索**记账（`record_search_outcome_state`）
- `toc_attempts/toc_successes` 由**取目录**记账（`record_path_outcome(kind="toc")`，调用点 `backend/src/service/book.rs:589,602`）
- `content_attempts/content_successes` 由**取正文**记账（`record_path_outcome(kind="content")`，调用点 `backend/src/api/router.rs:4812,4850`）
- **三条记账路径各自独立，谁先发生就只写自己那几列**

消费点（都只读 `confidence()`）：
- 搜索前排序 `backend/src/api/router.rs:2936-2952`、`:9420-9440`（降序，同分比平均延迟，缺行给 `(0.5, 3000)`）
- 死源判定 `backend/src/api/router.rs:9911`：`confidence() <= 0.05 && attempts >= 5 && now - last_failure_at <= 6h`
- 前端展示/清洗 `frontend/src/pages/SourcesPage.tsx:184-217`（`confidence < 0.4 && attempts >= 3` 算差源；`>= 0.7 && attempts >= 5` 算优质）

## 2. 生产实测（库 `/home/reader/storage/reader.db`，命名空间 `testuser01`）

**统计必须带 `user_namespace='testuser01'`**：其它 ns（`default`/`probe*`）是压测与探针残留，混进来会得出完全错误的结论。

| 事实 | 数字 |
|---|---|
| `source_stats` 行数 | 1698（现役源只有 615 → 1083 行是已删源的孤儿行，从不清理） |
| `attempts = 0` 的行 | 947 |
| 置信度分布 | `0-0.05`: 238 / `0.05-0.2`: 21 / `0.2-0.4`: 154 / **`0.4-0.6`: 1155** / `0.6-0.8`: 96 / `0.8-1`: 34 |
| 有目录证据（`toc_attempts>=3`） | 362 行 |
| 有正文证据（`content_attempts>=3`） | 54 行 |
| 目录、正文**都**没证据 | **1333 行** |
| 统计窗口 | 2026-09-06 → 2026-10-31（约 8 周，无衰减） |

### 2.1 早退把 947 行钉死在中性 0.5
`attempts == 0` 但有目录/正文证据的行 **947** 条，全部返回 0.5。其中目录成功率 < 50% 的有 **498** 条（例：`http://www.xiaxs.la#🎃` toc 3/8、`https://www.janpn.net/` toc 2/8）。它们现在是"中性"，排在有证据的差源前面。

### 2.2 搜索笑嘻嘻、目录哭哈哈（同一行内自相矛盾）
`attempts>=10 且 toc_attempts>=3` 的 151 行里，**26 行**目录成功率 < 50%：

```
toc=0/87  search=91/92   http://m.zhizihuan.com     ← 87 次目录全失败，搜索 99% 成功
toc=0/52  search=121/133 https://api.kkmh.com
toc=0/29  search=83/88   https://h5.reader.qq.com
toc=0/3   search=49/55   http://m.shubl.com
```

按现公式这些源拿 0.55~0.6（`toc_rel=0` 只扣掉 0.15 权重，而 `rel` 仍按 0.55 满分计入）。

### 2.3 "成功"不等于"内容对"
同一本书《一世之尊》第 1 章「机心」各源实测（干净正文 ≈ **3215 字**）：

```
书客吧 3216 / 书城免费小说 3215 / 肉书屋 #一程 3215 / 酷书阁 3215 / 快眼看书 3215   ← 正确
🌟去看小说网 5718（偏大）
顶点小说 21635 / 滋滋读书 25771 / 🌊溜达小说 226494 / 精华书阁 304567              ← 多章串一页
QQ浏览器 `17|0|OK` / 松鹤庭沐 `174|0|OK` / 爱读小说 `569|0|OK`（标题是垃圾串，正文 0 字）
```

这些"串章/垃圾标题"的源在统计里都是**成功**：HTTP 200、解析出内容、非空 → `successes++`。现公式没有任何一列能表达"内容对不对"。

## 3. 已确认的其它结构性缺陷（都已核对代码）

1. **无样本量校正**：`attempts=1, successes=1` → `rel=1.0` → 直接高分（Beta/Wilson 下界能解决）。
2. **无时间衰减**：8 周前的成功与今天的成功等权；源上周坏了仍背着 150 次旧成功。
3. **速度因子用全期平均**：一次 60s 超时把 30 次 1s 的平均拉到 3s；无 p95/分位。
4. **`rich`（简介抓取率）在 90% 行上无数据**，`intro_count=0` 时固定给 0.5，等于给所有源加同一个常数，不区分。
5. **`toc_rel`/`cont_rel` 无数据时直接抄上一级**（`else { rel }`），把"没有证据"伪装成"有证据且等于搜索成功率"。
6. **死源门槛是硬阈值** `0.05` + 6h 窗口，靠 `consec_failures` 计数；一次抖动就连败 3 次。
7. **孤儿统计行不清理**（1083 行），删掉的源如果同名重建会继承旧账。
8. **记账口径不一致**：`attempts` 只统计搜索，却当成"总尝试"用于 `attempts >= 5` 之类的门限 —— 一个只被取过目录、从没搜过的源永远进不了这些门限。
