# 移动端适配评审与优化方案（双模型独立审计 + 实测复核）

- 评审对象：`reader/frontend`（React 19 + Tailwind v4 + radix，生产 `http://127.0.0.1:4396`）
- 方法：两路**只读**审计（A = 布局/溢出的工程视角，B = 信息密度/触控的设计视角），各自独立读源码并在 headless Chromium 上以真实构建 CSS 复现；本文对两路结论**逐条回代码与生产环境复核**，冲突处给出裁决。所有数值为本机实测（视口 320 / 390，`deviceScaleFactor=1`，测试账号 `testuser01`）。
- 复现脚本：`.tmp/verify/mobile_audit.mjs`（越界元素 + 横向滚动容器 + 逐页截图）、`.tmp/verify/me_probe.mjs`、`.tmp/verify/search_probe.mjs`、`.tmp/verify/fix_check.mjs`（注入候选修法再量一遍）、`.tmp/verify/touch_probe.mjs`（<32px 触控目标）。

---

## 一、整体判断

不是「几个页面没适配」，而是 **4 个机制性缺陷经共用组件扩散到 11 处**，其中 3 处已经让页面不可用（整页横滚 / 按钮出屏 / 卡片被裁）：

| 规则 | 缺陷 | 已爆点 | 扩散面 |
|---|---|---|---|
| R1 | grid 只有 `lg:grid-cols-2`、没有 base 列定义 → 落隐式 `auto` 轨道，下限是子项 min-content | `/me` 卡片右侧被裁 | 4 处同型 |
| R2 | `ui/Input.tsx` 外层 div 缺 `min-w-0` → 作为 flex 子项停在 min-content（实测 206px） | `/search` 320px 按钮出屏、书架页顶栏溢出 | 4 处使用者 |
| R3 | 徽章行 `flex` 不换行且徽章 `shrink-0` → 名称被压到 56–106px（极端 0） | 书源行/净化/RSS 名称几乎不可见 | 3 处 |
| R4 | `shrink-0` 加在自带 `flex-wrap` 的容器上 → 容器定死在 max-content，内部换行永不生效 | `/sources` 胶囊行整页横滚 | 4 处同型 |

修复顺序 **R2 → R4 → R1 → R3**：全部是类名级改动，合计约 8 个 class，能一次消掉 3 条 P0。

---

## 二、P0（4 条，修法均已在生产环境注入实测通过）

### P0-1 `/sources` 筛选胶囊行撑破页面（整页横向滚动）
- 实测：`div.flex.shrink-0.flex-wrap`（`pages/SourcesPage.tsx:448`）宽 **401**、right **417**；`main` scrollWidth 417 / clientWidth 390。容器上 `shrink-0` 让它停在 max-content，内部 `flex-wrap` 只能让「组」换行、不能让 6 个胶囊换行。
- 修法：`:448` `flex shrink-0 flex-wrap` → **`flex min-w-0 flex-wrap`**（顺带去掉 `:471`、`:484` 的 `ml-1`）。
- 实测结果：容器 401 → **358**（2 行）、right 417 → 374、`main` scrollWidth 417 → **390**。
- 对应 B 的方案（胶囊进 Popover）不作为 P0：那是信息架构改动，见 P2-6。

### P0-2 搜索行在 320px 把提交按钮推出屏幕
- 实测 320：`form.flex.gap-2` 宽 288，Input 外层（`components/ui/Input.tsx:64-71`）**min-content 206**，+ gap 8 + 按钮 92 = 306 > 288 → 按钮 right **322**（出屏 2px，页面可轻微横晃）。
- 根因不是 `SearchBar` 的 `flex-1`，而是 `Input` 外层同时带 `w-full` 且没有 `min-w-0`：flex 子项的自动最小尺寸取「指定尺寸（100%=288）与内容 min-content 的较小者」= 206，于是它拒绝收缩。
- 修法：`ui/Input.tsx:65` 基类加 **`min-w-0`**（一行，四个使用者同时受益）；`SearchBar.tsx:75` 按钮 `px-5` → `px-4`。
- 实测结果：按钮 right 322 → **304**，`form` scrollWidth 288 = 320 视口内。
- 另一处已爆点：书架页 `pages/ShelfPage.tsx:285`（`<Input className="w-56 sm:w-64">`）+ `PageIntro.tsx:37` 的 `shrink-0` action 区 → 320px 下页面 `body.scrollWidth` 363；R2 落地后回 320。

### P0-3 `/me` 两张卡右侧被裁
- 实测 390：`div.grid.items-start.gap-5.lg:grid-cols-2`（`components/me/PersonalCenter.tsx:180`）的两个子项宽 **390**，而内容区只有 **358**（`px-4`）→ 右侧溢出 32px，被 `AppShell.tsx:44` 的 `main` 静默裁掉。因为下层是 `overflow-y-auto`，浏览器把 `overflow-x` 也算成 auto，所以不报错、只表现为「卡片像被刀切」。
- 修法：`:180` 加 **`grid-cols-1`**（= `minmax(0,1fr)`）；等价备选是给 `:181` 的 `section` 与 `:198` 的 `div.space-y-5` 各加 `min-w-0`。
- 实测结果：子项 390 → **358**，right 406 → **374**；注入后 `main` 内只剩 `truncate` 元素的正常 ellipsis。
- 与数据相关：A 用长书名复刻时子项宽到 442 —— 书名/章节名越长越严重，回归要拿长书名测。

### P0-4 书源行名称被徽章挤到不可读（长名 + 多徽章时归零）
- 实测 390（生产真数据）：`SourceListItem.tsx:106` 徽章行内名称宽 **56 / 70 / 94 / 106**；A/B 用满 6 徽章的样本复刻到 **0**、320px 下净化规则名剩 6px、RSS 名剩 30px。
- 机制：名称 `truncate`（`:107-112`）带 `overflow:hidden` → 自动最小尺寸为 0，徽章全 `shrink-0`（`ui/Badge.tsx` 基类）→ 剩余宽度全给徽章，名称退化。
- 修法：名称 span 加 **`min-w-0 flex-1`**（把剩余宽度优先给名称），徽章行保持 `flex items-center gap-1.5`。
- 实测结果：名称 56/70/94/106 → **94/108/240/106**，行 scrollWidth 无变化（不引入新溢出）。

---

## 三、P1（8 条：能用但难用）

| # | 问题 | 位置 | 实测 / 依据 | 修法 |
|---|---|---|---|---|
| P1-1 | 首屏被页头吃掉：书源页第一行书源在 **413px** 以下 | `ui/PageIntro.tsx:22/:27/:34`、`pages/SourcesPage.tsx:398-428`（移动端「导入 + ⋯」独占一行）、`:432-448` | 667px 屏减底栏 56 只剩 134px ≈ 1 行 | PageIntro 的 eyebrow / desc 各加 `hidden sm:block`，`mb-6 pb-5` → `mb-4 pb-3 sm:mb-6 sm:pb-5`；「导入 + ⋯」并进筛选行右端 |
| P1-2 | `/me` 4 张统计卡单列吃掉 **348px** | `PersonalCenter.tsx:164` | 单列 348px（667 屏的 52%），`grid-cols-2` 后 168px | `grid grid-cols-2 gap-3 lg:grid-cols-4`，卡内 `p-4` → `p-3` |
| P1-3 | 触控目标系统性偏小（实测 6 档：**20 / 26 / 30 / 32 / 36 / 38**px） | 筛选胶囊 `SourcesPage.tsx:471-499`(26)；历史 chip `SearchBar.tsx:126-137`(30)；书架分组 `GroupTabs.tsx:57`(38)；翻页 `ExplorePager.tsx:36-43`(32)；书源行 ⋯ `SourceListItem.tsx:190`(32)；输入框清除 `Input.tsx:90`(**20**)；顶栏主题 `TopBar.tsx:61`(36×28) | 拇指可靠点按 44×44，来自 `Button.tsx` 的 `sm`（h-8）与 `IconButton` 的 `sm`（size-8）被用在移动首要操作上 | 胶囊 `min-h-10`；清除钮 `size-6` + `after:-inset-1.5`；IconButton 触屏路径用 `md` + `after:-inset-1`；翻页 `size="md"` |
| P1-4 | 顶栏主题钮与账户钮相邻 8px，均 <44 | `TopBar.tsx:56-77`、`:78-95` | `gap-2`=8px | 移动端主题收进账户菜单（容器 `hidden sm:flex`），账户钮 `size-11` |
| P1-5 | RSS 行「编辑 / 删除 / 开关」间距 2px，删除紧贴开关 | `RssSourceListItem.tsx:74-95` | `gap-0.5` + Switch `ml-1` | 与 `SourceListItem.tsx:187` 对齐：编辑/删除进 ⋯，行内只留 Switch，`gap-2` |
| P1-6 | 书源行一行 6 个信息点（名称 + 6 徽章 + URL + 分组 + ⋯ + 开关） | `SourceListItem.tsx:106`、`:173-182`、`:187-238` | 移动列表行上限 3 信息点 + 1 主操作 | 徽章最多 2 个 + `+N` 开 Popover；URL 与分组二选一 `hidden sm:block` |
| P1-7 | 分组条 48px 里只有 32px 可点，且横滚零可发现性 | `SourceGroupTabs.tsx:27`（`h-12`）、`ui/Tabs.tsx:32`（Trigger ≈32）、`globals.css:137-152`（`.scroll-x-implicit` 只藏滚动条） | 上下各 8px 死区；`globals.css:146-147` 注释写明「不再用渐隐遮罩」故不加 mask | Trigger 补 `h-full`；可发现性用右缘 `sm:hidden` 的 `ChevronRight` 提示，而不是推翻既有遮罩取舍 |
| P1-8 | Toast 压在底部标签栏上 | `ui/Toast.tsx:193-194`（`z-100 bottom-0`）+ `AppShell.tsx:58`（底栏 `h-14 z-30`） | `z-100` > `z-30`，toast 盖住导航 | `bottom-0` → `bottom-14 md:bottom-0`（保留 `ui-safe-b`） |

---

## 四、P2（10 条：打磨 / 需产品决策）

1. **书架 320px 三列过挤**：`ShelfPage.tsx:66` `grid-cols-3 gap-x-6 gap-y-8` → `gap-x-3 sm:gap-x-6`。每格内容宽实测 69.3px，「读至第 1024 章 · 共 2048 章」只剩 4–5 字。（3 列本身是第三批反馈定的，不动列数。）
2. **书海分类 chip 全量换行占 3–4 行**：`ExploreMenuTabs.tsx:35-49` 现为 `flex-wrap`（有意为之）。改成横滚要配套渐隐，与 `globals.css:146-147` 的既有取舍冲突 → 倾向「超过 8 个折叠成『更多分类』」，待定。
3. **底栏信息架构**：`nav.ts:42-44` 五个入口含管理型 `/sources`，阅读型 `/rss` 在汉堡里。是否换位属产品决策，待定。
4. **多选批量条 8 个动作**：`SourcesPage.tsx:555-651` 在 390 下换 2–3 行 → 只留 启用/禁用/删除，导出/发布/下架进 ⋯。
5. **多选勾选框 16×16 无扩展命中**：`SourceListItem.tsx:87-95` → 加 `after:-inset-3`。
6. **筛选胶囊进一步收进 Popover**（B 的 P0-1 建议）：只留「全部 / 失效」+「筛选」按钮。是 P0-1 之后的 IA 升级，不是修复。
7. **设置项移动端堆两行**：`SettingCard.tsx:41-55`（`flex-col … sm:flex-row`），20+ 项 × 2 行 → 紧凑变体 `py-2.5` 或 value 区并入一行；卡片 `p-5` → `p-4 sm:p-5`（`:18`）。
8. **整窗高度用 `height:100%`**：`globals.css:98-102` → `h-dvh`，消除 iOS 地址栏收放时的底栏跳动。
9. **软键盘未处理**：`index.html:5-8` 的 viewport 无 `interactive-widget=resizes-content`，全仓无 `visualViewport` 监听 → 搜索/RSS 过滤框聚焦时底栏与输入框互顶。加一个 meta 属性是最小改动。
10. **下拉刷新只有书架页**：`components/shelf/PullToRefresh.tsx` 已存在且只在 `ShelfPage.tsx:314` 用 → 搜索/书源/RSS/我的 可直接复用。

补充（顺手项，非缺陷）：
- `SearchBar.tsx:126-137` 历史胶囊加 `max-w-full`，防超长不可断关键词顶出。
- `main` 加 `overflow-x-clip`（不要 `hidden`，会影响 sticky）作为越界兜底；真正修法是上面四条规则。
- `ui/Tabs.tsx:19` 的裸 `overflow-x-auto` 统一改用既有 `.scroll-x-implicit`。

---

## 五、两路审计的冲突与裁决

| 条目 | A（工程） | B（设计） | 裁决 |
|---|---|---|---|
| `/me` 溢出量 | 长书名下 442 / right 458 | 320px 下 40px | 生产真数据 390 视口是 **390 宽 / right 406**；长度依赖，两者不矛盾，文档统一记「可用 358 → 实际 390，长书名更差」 |
| `/sources` 胶囊行 | 去掉 `shrink-0` 即可 | 应收进 Popover | **先按 A 修**（1 个类、实测生效）；Popover 归 P2-6 |
| 书源名 0 宽 | P1 | P0 | 生产数据下是 56–106px（可见但被压），满徽章长名才归零 → 定 **P0-4**（潜在不可读，修法一行） |
| 首屏 413px / 统计卡 348px | 未提 | P0 | 页面可用、只是要滑一下 → 定 **P1-1/P1-2** |
| PageIntro 与 TopBar「标题重复」 | — | 判为违反「一屏一个标题」 | 复核：`TopBar.tsx:50` 注释写明「顶栏只做面包屑，页面唯一 h1 在 PageIntro」，是**有意分层**，不算重复；成本问题是移动端总高度 → 只做 P1-1 瘦身 |
| 横滚加渐隐遮罩 | — | 建议加 mask | `globals.css:146-147` 明确记录「两侧 tab 显透明，不再用渐隐遮罩」→ 不推翻，改用箭头提示（P1-7） |
| 320 搜索按钮出屏量 | 310.9（复刻） | 6px | 生产实测 right **322**（出屏 2px）、`form` 溢出 18px；以生产为准 |
| 书海分类改横滚 | — | P2 建议 | 现为有意 `flex-wrap`，改横滚与遮罩取舍冲突 → 降级为「折叠更多」，待产品定 |

---

## 六、明确不改（设计如此，别当 bug 修）

- `SourceGroupTabs.tsx:27` / `GroupTabs.tsx:50` 的分类/分组胶囊横滚容器（你量到的 `scrollWidth=12101` 就是它，是滚出来的宽度，不是撑破页面）。
- `Drawer.tsx` 的 `w-72` 侧滑、`Dialog.tsx:84` 移动端底部 sheet、`BookmarksDrawer`/`AnnotationsDrawer` 的 `w-[85vw] max-w-xs`。
- `CoverImage` 的 aspect-3/4 书脊、`GraphLegend.tsx:27` 的 `max-w-[calc(100vw-1.5rem)]`。
- `SideNav` 的 `w-56`（`md` 以上才渲染，移动端走抽屉）。
- Virtuoso 虚拟列表（#29 的实测结论见 `UI-REVIEW.md`）。
- 各项 `truncate` 元素：它的 `scrollWidth > clientWidth` 是预期行为。

---

## 七、实施批次

**批次 1（P0，约 8 个类名，风险最低，建议立刻做）**
1. `ui/Input.tsx:65` 加 `min-w-0`（R2，连带修 `/search` 与书架页顶栏）+ `SearchBar.tsx:75` 按钮 `px-4`
2. `SourcesPage.tsx:448` 去 `shrink-0`（R4）
3. `PersonalCenter.tsx:180` 加 `grid-cols-1`（R1）
4. `SourceListItem.tsx:107` 名称加 `min-w-0 flex-1`（R3）

验收：390 与 320 两档跑 `mobile_audit.mjs`，`main.scrollWidth == 视口宽`；`/sources` 胶囊两行、`/me` 卡片 right ≤ 374、`/search` 按钮 right ≤ 视口宽-16；桌面 1440 截图无回归。

**批次 2（P1 可用性）**：P1-3 触控目标、P1-4 顶栏合并、P1-5 RSS 行、P1-8 Toast 位置 —— 都是移动端专属类名，不影响桌面。

**批次 3（P1 首屏 + P2 待定项）**：P1-1 PageIntro 瘦身、P1-2 统计卡两列、P1-6/P1-7 书源行与分组条；P2 里需要产品决策的（底栏换位、分类折叠、批量条收菜单）先出效果图再定。

---

## 八、批次 1–3 落地记录（已完成）

三个批次同批落地，文件白名单互不重叠、并行修改，最终一并过门禁与双档验收。

**机制修复（4 条扩散源）**

| 文件 | 改动 |
| --- | --- |
| `frontend/src/components/ui/Input.tsx:65` | base 加 `min-w-0`（附根因注释）；清除钮 `size-5`→`size-6` + `before:-inset-1` 热区 |
| `frontend/src/pages/SourcesPage.tsx:448` | 胶囊容器 `shrink-0`→`min-w-0`；6 个胶囊 `px-2.5 py-1`→`px-3 min-h-9`；去掉「按置信度排序」「多选」的 `ml-1` |
| `frontend/src/components/me/PersonalCenter.tsx:180` | 双列 grid 补 `grid-cols-1`；统计卡 `grid-cols-2 lg:grid-cols-4` + `p-3 sm:p-4`；骨架屏同步两列；`RecentBook` `px-2`→`px-3`，百分比 `w-10 text-right` |
| `frontend/src/components/sources/SourceListItem.tsx:107` | 名称 `min-w-0 flex-1`（附根因注释） |

**触控与移动端小件**

| 文件 | 改动 |
| --- | --- |
| `frontend/src/components/layout/TopBar.tsx` | 主题菜单整体 `hidden sm:block`（移动端不显示）；账户钮 `after:-inset-1` 把命中区 36→44px（视觉不变，未升 `lg` 档以免改桌面圆角与图标）；右区 `gap-2`→`gap-3` |
| `frontend/src/components/rss/RssSourceListItem.tsx` | 操作区 `gap-0.5`→`gap-2`，Switch 去掉 `ml-1` |
| `frontend/src/components/ui/Toast.tsx` | bottom 分支 `bottom-0`→`bottom-14 md:bottom-0`（不压底部标签栏） |
| `frontend/src/components/explore/ExplorePager.tsx` | 两个翻页钮 `size="sm"` + `h-9 px-3`（32→36px；不升 `md`，否则字号 12→14 桌面走样） |
| `frontend/src/components/shelf/GroupTabs.tsx` | 分组 chip `min-h-10`（38→40px） |
| `frontend/src/components/search/SearchBar.tsx` | 两个按钮 `px-5`→`px-4 shrink-0`；历史 chip `max-w-full truncate` |

**首屏与结构**

| 文件 | 改动 |
| --- | --- |
| `frontend/src/components/ui/PageIntro.tsx` | 移动端只留 h1 + action：eyebrow/desc `hidden sm:block`，外层 `mb-4 pb-3 sm:mb-6 sm:pb-5` |
| `frontend/src/pages/RssPage.tsx` | 「刷新订阅」从独立一行移进 PageIntro action，移动端只留图标（功能不丢）；计数行 `hidden sm:block` |
| `frontend/src/components/sources/SourceGroupTabs.tsx` | `TabsList` 加 `pr-7 sm:pr-1` + 外层 `relative`，右缘 `sm:hidden pointer-events-none` 的 `ChevronRight` 作可滚动提示（不用渐隐遮罩） |
| `frontend/src/components/ui/Tabs.tsx:32` | `TabsTrigger` 加 `h-full`（只有 `h-12` 的分组条被撑到 40px，其余 `h-10`/`h-auto` 使用处视觉不变） |
| `frontend/src/pages/ShelfPage.tsx:66` | `gap-x-6`→`gap-x-3 sm:gap-x-6`（列数不动） |

**验收数据**（`vite preview` 起在 4422、代理到生产 4396，`mobile_audit.mjs` 实测）

| 路由 | 390 档 | 320 档 |
| --- | --- | --- |
| `/search` | `docScroll=390` ✓（修前按钮 right=322@320 出屏） | `docScroll=320` ✓ |
| `/sources` | `docScroll=390` ✓，胶囊换两行；书源名从最窄 56px 恢复到 106px | `docScroll=320` ✓ |
| `/me` | `docScroll=390` ✓（修前 right=406），统计卡 2×2，进度 `<1%` 有可见细条 | `docScroll=320` ✓ |
| `/library` | `docScroll=390` ✓ | `docScroll=320` ✓ |

剩余 `offenders` 只有两处、均属预期：skip link（`sr-only` 在视口左缘 -1px）、`/sources` 书源分组横滚容器内的 chip（容器自身 `overflow-x-auto`，设计如此）。门禁：`tsc -b` ✓、`pnpm lint` 0 error / 48 warning（与基线一致）、`pnpm test` 55/55、`pnpm build` ✓；桌面 1440 六页截图无回归。

**未做（留待产品决策）**：底栏导航换位（`nav.ts:42-44`）、书海分类折叠、批量操作条 8 个动作收进菜单、`PullToRefresh` 推广到 `/library` 之外、`index.html` 的 `interactive-widget` 与 `h-dvh` 迁移（这两条涉及软键盘与 iOS 地址栏收缩，需要真机验证）。
