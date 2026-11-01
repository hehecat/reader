# UI/UX 评审清单（双模型独立评审 + 交叉审核 + 代码复核）

- 评审对象：`reader/frontend`（砚台 · 在线小说阅读，React 19 + Tailwind v4 + radix + 自研 shadcn 风格组件）
- 方法：`deepseek-v4.1-flash` 与 `hy4-preview` 各自独立通读 A/B/C/D 四个区（壳与通用件 / 阅读器 / 发现与书 / 管理面），各出 15–18 条带 `file:line` 的发现 → 互相对方清单回代码逐条核证（成立 / 驳回 / 存疑）→ 本清单对每条再做一次代码与数值复核。
- 本轮**未改动任何产品代码**。
- 说明：hy4 对 deepseek 的反向审核未产出（该 agent 返回空），这一方向由 captain 亲自逐条核证（数值项全部用 oklch→sRGB 复算、UI 项全部回到源码核对行号），结论与"驳回"记录见下文「交叉审核结论」。

---

## 一、整体判断

风格定位清楚且成立：暖白纸面 + 朱砂红 accent + 中文衬线标题，四套主题全走 CSS 变量，`motion.css` 有独立的时长词表与 `prefers-reduced-motion` 降级块，列表页四态（加载/错误/空/筛选空）与 `EmptyState`（22 处）覆盖完整，危险删除在书架 / RSS / 净化 / 缓存四处都已走统一的 Dialog 模式。**这套 UI 已经过了"能看"的阶段。**

缺的不是好看，是三件事：

1. **页面的身份**——最大的字被营销口号占走，真实页名是全站最小的标题（h1 = 18px，h2 = 36px，层级倒置）。
2. **动作的分层与护栏**——同一类动作有 2–3 个入口、2–3 套取值；最危险的两个动作（书源批量删除、阅读器恢复默认）反而没有护栏。
3. **移动端的触控预算**——图标按钮 32px、阅读器顶栏 10 个图标靠 gap 归零硬撑、没有底部导航，桌面很从容，375px 靠挤像素。

---

## 二、交叉审核结论

| 条目 | 提出方 | 对方判定 | 我的复核 |
|---|---|---|---|
| 书源行 6 个同形图标 + 删除紧贴开关 | deepseek D1 / hy4 C1 | 互相 confirm | 成立；但 hy4 建议"加确认"多余——删除本来就走 `SourcesPage.tsx:801-829` 的 Dialog |
| 阅读器顶栏 10 个 32px 图标平权 | deepseek B1 / hy4 B2 | 互相 confirm | 成立；补正：`ReaderToolbar.tsx:131-141` 注释显示"目录"已被刻意挪到返回旁，其余 8 个仍平权 |
| 主题三入口三套口径 | deepseek A2 / hy4 A1 | 互相 confirm | 成立，且是清单里唯一"状态破坏"级：`setTheme(isDark ? "light" : "dark")` 会把 sepia/system 永久改写 |
| 羊皮纸 muted 对比度不足 | deepseek A6 / hy4 A4 | 互相 confirm | 成立：复算 `#8a7558` on `#f6eeda` = **3.81:1**（需 4.5） |
| RSS 表单暴露 ruleXXX 字段名 | deepseek D2 / hy4 C3 | 互相 confirm | 成立（`EditRssSourceDialog.tsx:324-390` 共 9 处），标签语义本身有 `htmlFor`，纯属术语外泄 |
| window.confirm 两处 | deepseek D3 / hy4 D2 | 互相 confirm | 成立；比双方描述的更割裂——同页 `UsersPage.tsx:383/523` 自己就在用规范 Dialog |
| 阅读器动画动 height/width | deepseek B3 / hy4 B4 | 互相 confirm | 成立；第五处同类：`SideNav.tsx:124` 的 `transition-all` + `w-16↔w-56` |
| danger 按钮对比度 4.2:1 | hy4 A3 | **deepseek 驳回** | **驳回成立**：复算 danger `oklch(0.577 0.245 27.325)` = `#e7000b`，on background `#fcfaf6` = **4.58:1 通过**；真实缺陷只剩 `hover:bg-danger/90` ≈ 3.37:1 |
| 焦点环 27 处 `ring-accent/60` | deepseek A5 | — | 成立：60% 合成后 light 3.17 / dark **2.46** / green **2.77** / sepia **2.42**，非 light 主题全部低于 3:1 |
| 设置面板"无 Esc" | hy4 B3 | — | **部分驳回**：Esc 由 `ReaderPage.tsx:555-573` 全局 `closeAll()` 兜住；但"非 Radix 浮层、无焦点捕获/aria-modal/焦点归还、关闭键 16px 无 ring"成立 |
| 尺寸估算（三条降级条 90px / Slider 12px） | hy4 B5/B6 | deepseek 修正 | 采纳修正：三条约 **72px**（每条 `py-1`+`text-xs`），Slider 可点区约 **16px** |
| 幽灵 token（4 文件 7 处） | deepseek「补漏」 | — | 成立且已用生产 CSS 复核：`@theme inline`（`globals.css:71-83`）未定义 `destructive/input/muted/primary`，构建产物里 `.text-destructive` 计数 **0** |

---

## 三、清单（按优先级）

### P0 —— 会造成状态/数据损失或明显误导，建议先修

**1. 主题被拆成三个互不知情的入口，其中一个会破坏另外两个的状态**
`TopBar.tsx:19-23`（3 档：light/green/dark）、`SettingsPanel.tsx:49-55`（5 档：+ sepia/system）、`ReaderToolbar.tsx:390`（`setTheme(isDark ? "light" : "dark")`）。
在羊皮纸/跟随系统下点阅读器底栏的日夜按钮 → 主题被改写成 dark，再点变 light，羊皮纸与跟随系统**永久丢失**（只能回设置面板重选）。另外顶栏三档在 sepia 下**没有任何一档高亮**（生产实测：`data-theme="sepia"` 时三个胶囊全灰），当前状态不可见。
改法：抽一个统一 `ThemePicker`（5 档）复用于顶栏与阅读器面板；底栏按钮改成"在 5 档里循环"或直接打开 Picker，禁止二值翻转。成本 S。

**2. 书源批量删除没有确认，单项删除有**
`SourcesPage.tsx:284-287`（`bulkDelete` 直接 `mutateAsync`，`onSuccess` 只清选择，**连 toast 都没有**）、`:626-632`（danger 按钮 `onClick={() => bulkDelete.mutate()}`）。
对照：单项删除走 `:801-829` 的 Dialog；书架页批量删除已有现成模式（`ShelfPage.tsx:479-496`「确定删除选中的 N 本书吗」）。选 1 个要确认、选 50 个一键清空。
改法：复用现有 Dialog，文案「确定删除选中的 N 个书源吗」+ 列出前几个源名 + 成功 toast。成本 S。

**3. 阅读器「恢复默认」一键清空全部偏好，无确认、无撤销、无提示**
`SettingsPanel.tsx:404-406`（`handleReset` → `useSettingsStore.setState(defaultSettings)`）、`:552-558`（全宽 `text-xs text-muted-foreground` 文本按钮，渲染在面板**最末项「自动滚动速度」下方**，滚动区末尾）。
`settings-store.ts` 的 `defaultSettings` 含字号/行距/段距/页宽/字体/主题/自动滚动速度/**TTS 网关地址与音源**/搜索超时。点一下全没，且连一条 toast 都没有。
改法：移到面板顶部独立危险区，点击弹 Dialog 确认，成功后给带「撤销」的 toast。成本 S。

### P1 —— 明显影响体验或直觉，建议本批做完

**4. 三页把营销口号当页标题，页名降级成 18px 小字**
`ShelfPage.tsx:280`「让每一次打开, 都从上次停下的地方开始.」/ `SearchPage.tsx:313`「找下一本要读的书.」/ `ExplorePage.tsx:203`「去书海里, 遇见下一本想读的书.」；样式见 `PageIntro.tsx:22-31`（`mb-8 pb-7`、`text-3xl sm:text-4xl`）。
移动端 375px 下 intro 区吃掉约 280–300px，书海页还要再加书源 Select 与分类行——首屏看不到一本书。用户返回列表后"我在哪"只能看左上角 18px 小字。
改法：`PageIntro.title` 恢复页名，slogan 降级为 desc 上方一行小字；书源 Select 移进内容区工具栏。成本 S。

**5. 标题层级倒置：h1 是全站最小的标题，h2 反而最大**
`TopBar.tsx:54` h1 = 18px；`PageIntro.tsx:30` h2 = 30→36px；`SettingCard.tsx:19` h3 = 18px。语义 h1>h2>h3 与视觉 h1<h2>h3 完全错位，且同页出现两次同名标题（TopBar 一次、PageIntro 一次）。
改法：PageIntro 成为页面唯一 h1 并降到 24–28px；TopBar 标题降级为面包屑小标；页内从 h2 起。成本 M（与 4 合并做）。

**6. 幽灵 token：4 个文件引用了 `@theme inline` 里根本不存在的颜色**
`globals.css:71-83` 只映射 background/foreground/surface/surface-muted/border/accent/accent-foreground/muted-foreground/danger/secondary，没有 `destructive`/`input`/`muted`/`primary`。生产 CSS 里 `.text-destructive` 计数 **0**。
实际后果：
- `SourceLoginDialog.tsx:238`、`SourceProbeDialog.tsx:129` 的 `text-destructive` → **书源登录/试搜失败的报错不变红**，与普通说明文字同色（这两个是本应用最主要的失败场景）；
- `SourceLoginDialog.tsx:227` 的 `bg-white` → 夜间/羊皮纸主题下验证码落在纯白硬底框；
- `ImportSourcesDialog.tsx:202` 的 `bg-muted` → 「已存在」徽标无底色；
- `PurifyPage.tsx:422` 的 `outline-none focus:border-primary` → 净化页导入框**键盘聚焦零反馈**。
改法：`text-destructive→text-danger`、`border-input→border-border`、`bg-muted→bg-surface-muted`、`bg-white→bg-background`、`focus:border-primary→focus-visible:ring-2 ring-accent/40`。成本 S。

**7. 羊皮纸主题的次要文字 3.81:1（唯一一套裸 hex token）**
`globals.css:35-46`：`--muted-foreground: #8a7558` on `#f6eeda` = 3.81:1（同主题 foreground 10.36:1、accent 5.08:1 都正常，说明是漏测而非体系问题）。而阅读器里章节副信息、章末「本章完」、注解都吃这个 token，羊皮纸恰恰是长时间阅读的首选主题；`ReaderStatus.tsx:45/88` 还再叠 `/80` 不透明度 → 约 3.1:1。
改法：muted 压到 `#6f5c40` 一带（≥4.6:1），并明确"禁止在 muted-foreground 上再叠不透明度"。成本 S。

**8. 焦点环在非 light 主题下不可见**
`Button.tsx:22`、`Switch.tsx:22` 等 **27 处** `focus-visible:ring-accent/60` + `ring-offset-background`。复算：light 3.17、dark **2.46**、green **2.77**、sepia **2.42**（需 ≥3:1）。
改法：新增不透明的 `--ring` token，四套主题各给一个值，替换全部 `ring-accent/60`。成本 S。

**9. 阅读器顶栏 10 个 32px 图标平权，移动端靠 gap 归零硬撑**
`ReaderToolbar.tsx:124-126`（注释自述 390px 下 10×36px 溢出 4px，于是 `gap-0 px-1`）、`:127-233`（返回/目录/书签/书架/批注/朗读/自动滚动/沉浸/AI/设置）。导航、面板、书级写入、播放、模式五类动作同尺寸同权重；"加入书架"（改数据）与"沉浸阅读"（纯视图）长得一样；32px < 44px 触控线。
改法：顶栏只留 返回/目录/书签/书架/设置，其余收进「更多」；命中区撑到 44（视觉可仍 20px）。成本 M。

**10. 书源行 6 个同形图标按钮 + 删除紧贴启用开关**
`SourceListItem.tsx:166-205`：试搜/登录/工作台/调试/编辑 JSON/删除，全 `size="sm"`（32px）、`gap-0.5`（2px）、删除只靠 `hover:text-danger` 区分（触屏无 hover）、末尾 `Switch` 只隔 `ml-1`。操作区约 238px，375px 屏上留给「名称 + 5 个徽标 + 地址 + 分组」不足 120px。
改法：行内只留「开关 + ⋯ 菜单」，删除沉到菜单底部并用分隔线隔开。成本 M。

**11. 目录抽屉页脚常驻三个运维动作 + 换源**
`TocDrawer.tsx:516-567`：「重新拉取本章」「刷新目录」「换源」+「倒序目录」开关。99% 打开目录只是翻章，却要先绕过一排"这本书可能坏了"的按钮；「换源」会改变全书正文来源且无确认。
改法：页脚只留「倒序目录」，其余收进抽屉标题栏「更多」，或只在目录加载失败时以提示条出现。成本 S。

**12. 阅读设置面板是自制浮层（全站唯一）**
`SettingsPanel.tsx:412-434`：手写遮罩 + `role="dialog"`，无 Portal、无 `aria-modal`、无焦点捕获与归还；关闭键是裸 `button` + `size-4` 图标（命中区≈16px）无 focus ring。（Esc 有，由 `ReaderPage.tsx:555-573` 的 `closeAll()` 兜住。）
改法：改用 `ui/Dialog`/`ui/Drawer`，或至少补 `aria-modal` + 焦点捕获/归还 + IconButton。成本 M。

**13. 表单标签语义缺失（三处）**
- `SettingCard.tsx:44` `SettingRow` 的 label 渲染成 `<p>`，右侧控件无 `htmlFor`/`aria-labelledby`；`SettingsPage.tsx:87` 还有一个裸 `<input type="number">` 绕开 `ui/Input`；
- `LoginPage.tsx:98-125` 三个输入框**只有 placeholder**，无可见 label 无 aria-label（placeholder 在输入首字符后消失，切登录/注册 tab 时密码框 placeholder 还会变）；
- `SettingsControls.tsx:17` 的 `Field` 同样是 `<p>`；`Segmented`（`:124-139`）/`StepperButton`（`:32-35`）/`SelectionToolbar`（`:448` 色点）**全无 focus-visible 环**（而 Button/IconButton/Switch 都有）。
改法：`Field`/`SettingRow` 改真 `<label htmlFor>`（项目里 `EditRssSourceDialog.tsx:253` 已有现成写法）；登录页补可见小标签；三组控件补 ring。成本 S。

**14. 错误提示活不过 3 秒，且与成功提示同一个通道**
`Toast.tsx:31` `DEFAULT_DURATION = 3000`、`:40` 的 `duration` 不按 type 分支；`:182-184` 视口 `aria-live="polite"`，没有 assertive 通道。登录输错密码 3 秒后提示消失，页面上不留任何痕迹（校验失败一律走 toast，`LoginPage.tsx:34/39/43/58`）。
改法：`toast.error` 默认 6000 或常驻，错误单独 `aria-live="assertive"`；表单类错误补字段旁 `role="alert"` 行内提示。成本 S。

**15. 换源有三个入口、两种形态**
`BookDetailDialog.tsx:418-427` 点「换源」在**原地展开内联面板**（另一套视觉与文案，含「未入架: 换源只作用于本次预览」）；`SwitchSourceDialog.tsx:207` 是正式对话框（`ReaderPage.tsx:974`、`BookInfoDialog.tsx:212` 用它）；`TocDrawer.tsx:555` 目录抽屉里还有一个「换源」按钮。
改法：三处统一打开 `SwitchSourceDialog`，删掉详情页内联面板，并在对话框里说明进度/书签保留策略。成本 M。

**16. RSS 表单把后端字段名当标签**
`EditRssSourceDialog.tsx:324-390` 共 9 处：「列表规则 (ruleArticles)」「标题 (ruleTitle)」「链接 (ruleLink)」「发布时间 (rulePubDate)」「描述 (ruleDescription)」「图片 (ruleImage)」「下一页 (ruleNextPage)」「正文 (ruleContent)」「分类 (sortUrl)」，placeholder 是裸 DSL（`tag.item@css:.feed-entry`）。同表单「名称/分组/备注」却是人话。
改法：「解析规则」收进默认折叠的「高级（可选）」，标签改纯中文语义，字段名只留 hint。成本 M。

**17. 移动端没有底部导航，9 个一级入口全在汉堡抽屉里**
`AppShell.tsx:14` 注释「移动端(<md): 顶栏 + 内容区, 汉堡按钮打开 slide-over 侧栏(无底部导航)」；`nav.ts:28-42` 共 9 项（7 主项 + 设置 + 用户）。
改法：移动端底部固定 5 项（书架/搜索/书海/订阅/我的），书源/净化/设置/用户收进「我的」。成本 L。

### P2 —— 打磨项，可穿插做

**18. 书海页首屏被筛选器吃满，分页范式与其它页不一致** — `ExplorePage.tsx:203-224`（intro action 里塞 `w-52` 的 Select + 2 个 IconButton）+ 分类 chips；底部「上一页/第 N 页·M 本/下一页」与搜索页的流式加载、书架的无限滚动三套范式并存。成本 M。

**19. 「我的」页进度显示 0%、统计卡语义重复** — 实测「第2章 · 2/533 章」旁标 0%（四舍五入），进度条 1px 浅色几乎不可见；「藏书 5 / 在读书籍 5 / 读完 0」三个数字同义；「关于」卡里写技术栈（React / 阅读3 server）面向用户暴露实现细节；右侧「数据与备份」只是跳设置页的入口。成本 S。

**20. Segmented 手写 `role="radiogroup"` 却没做 roving tabindex** — `SettingsControls.tsx:116-139`：6 组共 22 个独立 Tab 停靠点，方向键不响应（ARIA radiogroup 的契约是"只有选中项 tabbable，方向键组内移动"）。项目里 radix 的 Switch/Slider/Tabs 都是正确实现，只有这个自制的是例外。成本 M。

**21. 正文上方最多可同时堆三条降级条** — `ReaderPage.tsx:734-779` 三段同结构（`py-1 text-xs`，各约 24px，同时命中约 72px），文案重复（"用的是本地缓存"/"当前显示本地缓存"）。改法：合并成一条可展开的「当前显示本地缓存 · 3 项待恢复」。成本 M。

**22. 章节跳转 Slider 的 thumb 只有 16px，夹在两个翻章按钮之间** — `Slider.tsx:77` `size-4`、`ReaderToolbar.tsx:340` 一行 6 个控件 `gap-1 py-1.5`。改法：thumb 视觉 24px、热区 44px，轨道 `h-2`，与翻章按钮拉开间距。成本 S。

**23. chrome 收放动画动布局属性** — `ReaderToolbar.tsx:119`（`transition-[height,...]` + `h-14↔h-0`）、`:336`（`max-height`）、`:252`（进度条 `transition-[width]`）、`SideNav.tsx:124`（`transition-all` + `w-16↔w-56`）。改法：translate + opacity，进度条用 `scaleX`。注意 `:119/:336` 目前挂着 `motion-reduce:transition-none`，改方案时别丢。成本 M。

**24. 骨架屏与批注闪烁绕过了 reduced-motion** — `Skeleton.tsx:28` 用 Tailwind 内置 `animate-pulse`（不在 `motion.css:276-311` 的降级块里），`ContentView.tsx:381` 的 `element.animate(..., {duration: 2000})` 是裸 WAAPI 无 matchMedia 判断。成本 S。

**25. 最小触控目标 20–28px** — `Toast.tsx:161` 关闭键 `size-5`（20px）、`SelectionToolbar.tsx:448` 高亮色点 `size-7`（28px，5 个连排 `gap` 极小）、`Switch.tsx:12` `sm: h-5 w-9`（轨道 20px，书源/净化每行末尾都是它）。改法：伪元素或负 margin 把热区撑到 44，视觉尺寸不变。成本 S。

**26. 用户管理两处 `window.confirm`** — `UsersPage.tsx:337/368`（删单个用户、清理不活跃用户，都写着"不可恢复"），而同页 `:383/:523` 与书架/RSS/净化全是规范 Dialog；批量清理那句在确认前拿不到"要删多少个"。成本 S。

**27. danger 按钮 hover 态对比度掉到 3.37:1** — `Button.tsx:28` `hover:bg-danger/90`（默认态 4.58:1 是**通过**的，hy4 报的 4.2:1 不成立）。改法：`hover:bg-danger/95` 或加深 token。成本 S。

**28. 没有 skip link，`<main>` 无 id** — `AppShell.tsx:35`。桌面收益有限（侧栏本就最前），移动端抽屉未打开时不在 Tab 序列，故列 P2；修起来 3 行。成本 S。

**29. 书架网格与书源列表未虚拟化** — `ShelfPage.tsx:423` `visibleBooks.map`、`SourcesPage.tsx:651` `pagedSources.map`（每页可选 20/50/100/200）。项目里 `SearchPage`/`TocDrawer`/`ContentView` 都已有 Virtuoso 正确用法。建议先做书源列表（一行一 DOM，收益确定），书架是多列自适应网格，改造成本高。成本 M。

**30. 一级导航里放了高级功能** — `nav.ts:35`「净化」（正则替换规则，96 条规则直接列正则）与书源/用户管理并列。改法：净化收进设置或「我的」，或至少标注"高级"。成本 S。

---

## 四、明确不建议改（避免过度设计）

- 四态（加载/错误/空/筛选空）与 `EmptyState`（22 处）：覆盖已经完整，别再抽统一容器；
- Toast 自建队列（同屏 3 条、退场 180ms）：够用，不需要换 radix toast；
- `motion.css` 的时长词表 + safe-area 工具类：这是这套 UI 最扎实的部分；
- 阅读器正文虚拟化（段落数与字数双阈值）、目录抽屉 `fixedItemHeight`：已经做对了；
- 危险删除的 Dialog 模式（书架/RSS/净化/缓存）：模式现成，问题只在"漏接"的两处；
- 顶栏主题胶囊的**位置**（放顶栏右侧）：位置本身合理，问题在取值集合与状态表达，别顺手搬走。

---

## 五、建议实施顺序

1. **第一批（护栏与状态正确性，约 1 天）**：#1 主题统一、#2 批量删除确认、#3 恢复默认确认、#6 幽灵 token、#7 sepia muted、#8 焦点环 token、#14 错误 toast 时长。
2. **第二批（层级与直觉，约 2 天）**：#4+#5 页标题与 h 层级、#9 阅读器顶栏溢出菜单、#10 书源行收菜单、#11 目录页脚、#15 换源统一入口、#13 表单标签与焦点环、#16 RSS 高级折叠。
3. **第三批（打磨）**：#12 设置面板换 Dialog、#17 移动端底部导航、#18–#29 其余。

每批落地后建议回归：`pnpm lint` / `pnpm test` / `pnpm build` + 375/768/1024/1440 四档截图 + 四套主题各截一次顶栏与设置页（当前 sepia 的对比度问题就是"只截了 light"才漏掉的）。
