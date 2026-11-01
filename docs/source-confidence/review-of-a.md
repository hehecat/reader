# 方案 A 证伪评审（review-of-a）

> 依据：生产库只读复算（`user_namespace='testuser01'`，`NOW = max(updated_at) = 1793491525491`）+ 现有代码逐处核对。
> 复算脚本 `/tmp/sc/sim.py`（A §1 的 `evaluate`/`is_dead` 逐行译 python，本文数字可重跑）。
> A 的 `sane_*` 通道在现有库恒为 0，按 A 自己的「缺失通道重归一」处理 → 本文数字 = A 上线**当天**的真实形态。

## 1. 算错的：公式在边界上的行为

**1.1 `is_dead()` 自锁——最严重的一条。** `verdict()` 首行 `if is_dead() { Dead }`，`is_dead()` 第一条析取式是裸 `self.consec_failures >= 3`。而 `consec_failures` 只在 `Fail` +1、**只在 `Ok` 归零**（`storage/mod.rs:1788-1795` 的 `CASE WHEN ?11=1 THEN 0 WHEN ?8=1 THEN consec_failures+1`），死源又被 `sources.retain()` 从搜索列表剔除（`router.rs:2955-2962`、`:9445`）→ **永远收不到 `Ok` → 永不归零 → 永不复活**。数据已在发生：34 条 `is_dead=true` 中 **34/34 靠这条命中**；失败年龄**全在 12.2–16.1h**（一次近期故障打进来的）。A §4 把它当成绩（「34 行全现役」），但旧 6h 窗口对这 34 行**全部放行**——旧规则命中 0 是因为窗口在放行，不是判据失灵；A 却把 TTL 拉到 30 天钉死它们。修法：`consec_failures>=3` 必须配新鲜度门限，或 `is_dead` 只留剔除超时分母的两条（现命中 7 条）。

**1.2 `attempts=0 → None` 只在 35 行上成立，不是通用规则。** A §1 注释与 §8 都这么写。实测：947 行 `attempts=0` 里**只有 35 行 `evaluate()==None`，912 行照常出分**（944 行 `toc_attempts>0` 走目录通道）。分布：`weak 482 / ok 430 / unknown 35`——**430 行（45%）仍判 `ok`**，仍排在「有证据的差源」之前，正是 A §2.1 批评的现象，换公式后原样保留。A §8 的测试改写（`stat(0,0,0,0,0)`）测的是这 35 行的退化特例，掩盖了 912 行的真实分支。

**1.3 全超时 → 判 `Weak`，与 `Unknown` 的定义冲突。** `n=12, ok=0, timeouts=12`（A §8 的 `all_timeouts_is_undecided_not_dead`）：`mean=(0+0.5)/13=0.038`，`wilson(0,12).hi=0.243` → `point=0.038 < 0.4` → `Weak`。A 只说改判 `!is_dead`，没提 `point`。同是 0 成败证据，`attempts=0` 给 `Unknown`、`attempts=12 全超时` 给 `Weak` 并在 `SourceListItem.tsx:131` 染 danger 色。

**1.4 超时被当成确定性失败，且比 A §5a 承认的更狠。** 因 `ok=successes` 不变而 `n=attempts` 含超时。搜索单通道、送达即成功（n=60）：

| timeouts | 0 | 20 | 35 | **42** | 50 | 55 |
|---|---|---|---|---|---|---|
| `point` | 0.992 | 0.664 | 0.418 | **0.303** | 0.172 | 0.090 |

**42/60 超时（送达成功率 1.0）就越过 `point<0.4` 变 `weak`。** A §5a 声称「超时永不进死亡证据」，但把它整份塞进 `point` 分母：不判死改判差，一样染 danger、一样排队尾。其补救②（`ok=successes+0.5×timeouts`）只把临界点推到约 50，没改掉建模错误。

**1.5 30 天 TTL 把抖动成本放大 120 倍。** 现有 207 行 `consec_failures>=3` 中仅 12 行真实成功率 ≥0.8（多数确实是坏源），但窗口从 6h → 720h：现在一次 3 连败只影响 6 小时，A 之下影响 30 天。A 的「30 天没再失败就不再判死」读起来像保护，实为「30 天内一次 3 连败即判死」。

**1.6 样本=1 方向对但数字错。** A §5e 说 1/1 → `point=0.75`/`lo=0.207`。库里唯一一条（`http://172.17.0.1:9911`, 1/1）：搜索通道确实是 0.75/0.2065，但目录通道池化后 **`point=0.135`、`lo=0.057`**。A 报的是单通道值，`verdict` 用的是池化值——说明 A 的数字没做端到端校验。

**1.7 搜索通道的衰减锚点挂错列。** A §1 用 `updated_at`，但 `record_path_outcome`（`storage/mod.rs:1846-1860`）的 upsert **无条件** `updated_at = now`——取一次目录就刷新搜索锚点。947 行 `attempts=0` 里 944 行的 `updated_at` 实为最后一次**取目录**时间，其搜索数据可能来自 8 周前却按「昨天」计衰减（偏乐观）。

## 2. 做不到的：新增列在现有写入点上真的写得到吗

**2.1 `sane_*` 拿不到标题。** A §3 称「`content` 与 `chapter_title_ctx` 已在作用域内」只对一半：`chapter_title_ctx` 来自客户端的 `title` 参数（`router.rs:4600-4606`），**不是解析产物**。主阅读路径 `useChapterContent.ts:194,239,286` 确实传 `title: chapter?.title`；但 **`SwitchSourceDialog.tsx:150` 的换源探测不传、`workbenchApi.ts:78` 的正文测试也不传** → 这两条路径 `chapter_title_ctx=None` → `t.is_empty()` 恒真 → **恒记 `sane=false`**。换源探测恰恰最该抓串章，却 100% 记负。更根本的是源返回的正确标题根本没进后端：要拿到它必须改 `analyze_content`（`service/book.rs:1504-1512`，现返回 `Result<String>`）让它把标题带出来——A 未列此改动。

**2.2 回填会污染 944 行。** A §2 的 `SET toc_updated_at = updated_at WHERE toc_updated_at=0 AND toc_attempts>0`：因 `updated_at` 三通道共用（见 1.7），947 行里 **944 行 `toc_attempts>0`、351 行 `content_attempts>0` 全部被回填成「目录锚点 = 最后一次任意通道写入时间」**，而这些行 `updated_at` 跨度 55 天。等于给 8 周前攒的目录成功数按「昨天」锚点计衰减。新列天然为 0，正确做法是**不回填**，让它从头累积。

**2.3 漏了 `SourceStat` 的 decode 契约。** `ensure_column_typed`（`mod.rs:6317-6334`）加的 4 列必须同步进 `SourceStat`（`model/source_stat.rs:3-18`），因为它靠 `SELECT *` 直连 decode（`storage/mod.rs:1867-1872`）。A §2 说「无读改写、无事务」，§8 末尾才一句「构造器补字段」——实际是 struct 定义 + 5 处测试构造全要改，漏改则 sqlx 在旧库 decode 失败。

**2.4 `content_updated_at` 被两通道共用。** A §1 让正文(0.20)与 sane(0.15) 共用 `cont_at`，而 `record_content_sanity` 也写它 → 0.15 权重的记账会刷新 0.20 权重通道的锚点。

## 3. 自相矛盾的：与 FACTS / 与 A 自己的数字冲突

**3.1 「947 行会被处理」不成立。** A §6 的 M3 只报 498 行子集（中位 0.500→0.252、`point<0.4` 占 473/498，复算 ✓），但 947 行全貌是 `weak 482 / ok 430 / unknown 35`——**430 行仍 `ok`**，A 全文未提这两个数。

**3.2 M1 的两个分母不同。** A §6 表头写「行占比」，却是旧 1052/**1698**=62.0%（✓ 复算）对比新 108/**1663**=6.5%（按 1698 算 6.4%）。不是同口径前后对比。

**3.3 §5a 的「防法①」对唯一生效的析取式无效。** A 称「超时永不进死亡证据」，第二、三条确实用了剔除超时分母，但**第一条裸 `consec_failures>=3` 没有任何剔除，而它命中了全部 34 行**。

**3.4 「旧规则命中 0/1698」数字对、归因反了。** 满足 `conf<=0.05 && attempts>=5` 的 143 行（A 说 137，同量级），**最小失败年龄 1338h（55.8 天）** → 被 6h 挡掉。旧规则命中 0 是因为失败证据全是 8 周前老账，不是窗口太短。A 把 TTL 6h→30 天正好反向：需要的是「新鲜失败才判死」，30 天反而让老账能判死。

**3.5 落地数字有出入。** A 称 strong 48 / weak 836；复算 strong **46**（现役 26 ✓，与 A 一致）、weak **818**、ok 765、unknown 35 ✓、dead 34 ✓。A §5b 说现役 strong 带 `toc_attempts>=3` 的「只剩 5 行」，复算是 **4 行**（`http://api.anwaben.com` toc=30；其余如 `http://api.17k.com` att=158 toc=0、`http://api.cdyt.com` att=161 toc=0、`http://m.kxs9.com/` att=76 toc=0 全无目录证据）→ **26 个「优」里 22 个靠纯搜索证据上桌**，即 `Strong` 无法证伪 F §2.2 那类「搜索行、目录死」的源；A §5b 承认却列为「偏严先不加」。

## 4. 会误伤的：好源被打进死源 / 差源档

**4.1 死源档（染 danger + 从搜索列表剔除）：34 行里 8 行真实成功率 ≥0.83，全现役，旧 6h 窗口全部放行：**

| 源 URL | att | suc | to | consec | 剔除超时后成功率 | 失败年龄 |
|---|---|---|---|---|---|---|
| `https://m.rourouwu16.com#🎃` | 59 | 49 | 7 | 3 | **0.942** | 14.5h |
| `https://www.mangacopy.com` | 73 | 59 | 10 | 3 | **0.937** | 14.5h |
| `https://i.12bz.net/` | 68 | 56 | 7 | 3 | **0.918** | 14.5h |
| `https://www.mangacopy.com/` | 56 | 45 | 7 | 3 | **0.918** | 14.5h |
| `http://www.dmshuzhai.com` | 93 | 77 | 8 | 3 | **0.906** | 14.4h |
| `https://m.lewenxiaoshuo.com` | 63 | 47 | 10 | 3 | **0.887** | 12.7h |
| `https://zhaoze.pro` | 59 | 45 | 8 | 3 | **0.882** | 14.5h |
| `https://www.wanglisong.com#🎃` | 51 | 35 | 9 | 3 | **0.833** | 14.4h |

按 §1.1，剔除后它们再也收不到 `Ok` → `consec_failures` 永不归零；即便 30 天 TTL 过期放行一次，下一次 `Fail` 立刻回到 ≥3 → 再次判死。**这是永久性损失，不是 30 天的。**

**4.2 差源档：几何池化是一票否决，A 的风险表整条漏了。** 现役、搜索成功率 ≥0.85、样本 ≥15 但判 `weak` 的有 **11 个**：

```
https://api.kkmh.com      att=133 suc=121 to= 8 rel=0.968 -> point=0.159 lo=0.094 hi=0.346 weak
https://h5.reader.qq.com  att= 88 suc= 83 to= 5 rel=1.000 -> point=0.159 lo=0.153 hi=0.368 weak
https://so.html5.qq.com/  att= 88 suc= 79 to= 9 rel=1.000 -> point=0.260 lo=0.092 hi=0.644 weak
https://www.lanlanbo.com/ att= 73 suc= 63 to= 9 rel=0.984 -> point=0.301 lo=0.142 hi=0.536 weak
https://m.92dxs.org#      att= 65 suc= 54 to=10 rel=0.982 -> point=0.225 lo=0.088 hi=0.580 weak
```

`api.kkmh.com` 正是 F §2.2 点名的 `toc=0/52 search=121/133`。几何池化 `lp += w*ln(mean)`：目录通道 `mean=(0+0.5)/53=0.0094`，`0.30×ln(0.0094) = -1.394`，把搜索的 `0.35×ln(0.91) = -0.033` 彻底淹没。0.30 权重的通道一挂，整个源被压到**该通道水平以下**而非「扣 30%」。A §1 用「串联链路」论证几何池化，但它不是串联：搜索与目录是并行能力，不是同一请求的上下游。A §5 列了 6 类风险，**唯独漏了核心设计选择的否决效应**。

**4.3 超时密集档：** 现役、超时 ≥30% 且送达成功率 ≥0.7 的 25 源被扣分（`https://www.85novel.com` att=102 suc=68 to=31 → `point=0.665`，勉强留 `ok`）。临界点见 §1.4；FACTS §3 说 deadline 上线后超时会涨，这批首当其冲。

## 5. 保留的真知（应采纳）

1. **「没有证据」必须是 `Unknown` 而非 `0.5`**——取消 `attempts==0 → 0.5`（`source_stat.rs:33-35`）与 `toc_rel←rel`/`cont_rel←toc_rel` 的「抄上级」（`:44-51`）。全案最有价值的一条。
2. **判死与打分分离**——把 `router.rs:9911` 的 `confidence()<=0.05 && attempts>=5 && ...` 拆成独立谓词，方向正确，但谓词内容须重写（§1.1）。
3. **给分数配置信区间，小样本不伪装**——`1/1` 不该直接拿高分，用 Wilson/Beta 下界把「优」卡成「被证明」；可用，但须同时改「超时不计入分母」（§1.4）与「几何池化改算术加权或加下限」（§4.2）才不误伤。

---

附：未跑 `cargo test`（仅复算+代码核对）。A §8 的断言值（0.139/0.038/0.024/0.071）在 `stat()` 的 `updated_at=0` 下**全部不可达**：`(NOW-0)/30d = 691.9` 个半衰期 → `f = 5.1e-209` → `n_eff < 0.5` → `evaluate()` 返回 `None`；A 要么给测试传 `updated_at = now`，要么 5 个测试全得重写。
