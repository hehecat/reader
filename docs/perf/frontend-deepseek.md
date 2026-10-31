# 前端性能画像 — reader/frontend

> 产出者: deepseek-v4.1-flash（frontend 视角）
> 时间: 2026-11-01
> 仓库: `/home/hehecat/code/reader`，前端 `frontend/`（React 19 + Vite 7 + Tailwind 4，pnpm）
> 铁律遵守: 未改 `src/` 下任何文件；所有实验在 `.tmp/perf/exp`（src 副本 + node_modules 软链）里做；所有 API 探测走生产 4396 只读、SQLite 只读打开。

本轮的**已上线基线**（不重复提案）: 搜索墙钟 136s→95.3s、首壳 FCP 7236→1308ms、家族树布局重写（`frontend/src/components/graph-demo/graphTreeLayout.ts`）、目录/正文按源缓存。

---

## 0. 测量环境与可复现

| 项 | 值 |
|---|---|
| 构建 | `pnpm build` = `tsc -b && vite build`，vite 7.3.6，2240 modules，**18.54 s** |
| 归因构建 | `npx vite build --sourcemap`（体积与正式构建逐 chunk 一致，±0.05 kB） |
| Node | v22.23.2（`--experimental-strip-types` 直接跑 `.ts` 源码做纯函数基准） |
| 生产后端 | `http://127.0.0.1:4396`，`accessToken=testuser01:…`（**该 token 在测量中途失效**，见 §6） |
| 生产库 | `sqlite3 "file:/home/reader/storage/reader.db?mode=ro"`（只读） |

脚本（全部在 `.tmp/perf/`，可直接重跑）:

| 脚本 | 作用 |
|---|---|
| `attr.mjs <dist/static> <chunk.js>` | 解 sourcemap 的 VLQ mappings，把 chunk 的生成字节数归因到**单个源文件** |
| `agg.mjs` / `appsrc.mjs` | 同上，分别按 **npm 包** / 只看 **src/ 应用代码** 聚合 |
| `slicegz.mjs` | 按包切出 chunk 的字节区间并**单独 gzip**，得到每包对 gzip 体积的贡献（上界） |
| `bench-graph.mjs` | `parseGraphDemoData`（zod）与 `computeKinTreeLayout` 的耗时 |
| `bench-parse.mjs` | `stripNullsDeep` + 各 zod schema 对**真实接口负载**的解析耗时 |
| `bench-rank.mjs` | `rankResults` 随结果数的伸缩 |
| `alias-loader.mjs` + `hooks.mjs` | Node ESM resolve hook：`@/x` → `frontend/src/x{.ts,.tsx,/index.ts}`，并解析无扩展名的相对导入 |
| `exp/` | 实验构建副本（`src` 拷贝 + `node_modules` 软链），用来实测「删掉某个依赖能省多少」 |

> 踩坑记录（给后来者）: 自写 loader 里若给 `.ts` 显式声明 `format:"module"`，Node 22 会**跳过类型擦除**并报 `SyntaxError: Unexpected identifier 'as'`；不声明 format 才对。另外 loader 必须一并解析无扩展名的相对导入，否则 `services/explore.ts` 会 `ERR_MODULE_NOT_FOUND: ./exploreMenus`。

---

## 1. 构建体积画像

### 1.1 全量 chunk（正式构建，`dist/`）

| chunk | raw | gzip |
|---|---|---|
| **vendor-graph-D7iQuDfZ.js** | **1,041.59 kB** | **329.47 kB** |
| **index-q22jiYrq.js**（入口） | **281.87 kB** | **91.44 kB** |
| vendor-core-B5iSmSrq.js | 271.71 kB | 86.57 kB |
| ReaderPage-YflwuXdE.js | 112.41 kB | 36.51 kB |
| vendor-reader-DunTtkAf.js | 84.61 kB | 30.18 kB |
| index-Djndpho4.css | 78.40 kB | 13.52 kB |
| GraphDemoPage-BOj5DNYZ.js | 64.48 kB | 22.75 kB |
| SourcesPage / SettingsPage | 40.55 / 39.79 kB | 13.07 / 14.67 kB |
| SourceWorkbenchPage / ShelfPage | 31.60 / 29.86 kB | 9.77 / 10.51 kB |
| RssPage / SettingsPanel | 26.77 / 24.55 kB | 9.26 / 9.04 kB |
| Select / SearchPage / BookDetailDialog | 21.22 / 18.83 / 12.80 kB | 7.46 / 7.20 / 4.79 kB |
| UsersPage / Slider / GraphGenerateDialog / LibraryPage | 12.23 / 11.01 / 10.75 / 10.48 kB | 4.16 / 4.40 / 4.46 / 4.56 kB |
| PurifyPage / useChapterContent / SwitchSourceDialog | 9.66 / 6.55 / 6.39 kB | 3.92 / 2.81 / 3.18 kB |
| index.html | 6.30 kB | 2.01 kB |
| 其余 ~45 个 ≤6 kB 的小 chunk（含 ~40 个单图标 chunk） | — | — |

`dist/static/*.js` 合计 **2,237,097 B（2.13 MB raw）**，`dist/static/*.css` 78,403 B。整个 `dist/` **34,716,508 B（33.1 MB，不含 sourcemap 的干净构建）**，其中 `dist/fonts/` **32.24 MB**（见 §2.4）。

### 1.2 入口 chunk 的字节归因（`index-q22jiYrq.js`，mapped 269.6 kB / 177 个源文件）

按 npm 包（`agg.mjs`）:

| 包 | raw | 占入口 |
|---|---|---|
| **zod** | **54.4 kB** | 20.2% |
| **axios** | **50.4 kB** | 18.7% |
| 应用代码（`src/`，33 个文件） | 38.9 kB | 14.4% |
| **tailwind-merge** | **26.6 kB** | 9.9% |
| @radix-ui/react-menu | 8.5 kB | 3.2% |
| @floating-ui/core | 7.9 kB | 2.9% |
| @radix-ui/react-tooltip | 7.6 kB | 2.8% |
| @floating-ui/dom | 7.1 kB | 2.7% |
| @radix-ui/react-collection | 7.1 kB | 2.6% |
| react-remove-scroll | 5.7 kB | 2.1% |
| @radix-ui/react-dismissable-layer | 5.2 kB | 1.9% |
| lucide-react | 5.0 kB | 1.9% |
| @radix-ui/react-dialog | 4.1 kB | 1.5% |
| floating-ui/utils + react-popper + focus-scope + roving-focus + react-dom + presence + slot + remove-scroll-bar 等 | ~21 kB | ~8% |

单文件最大的几个: `zod/v3/types.js` **46.5 kB（17.3%）**、`tailwind-merge/dist/bundle-mjs.mjs` 26.6 kB、`@radix-ui/react-menu` 8.5 kB、`@floating-ui/core` 7.9 kB、`@radix-ui/react-tooltip` 7.6 kB、`axios/lib/utils.js` 7.5 kB、`axios/lib/adapters/fetch.js` 6.3 kB、`axios/lib/core/AxiosHeaders.js` 4.2 kB、`axios/lib/core/Axios.js` 3.0 kB。

**入口 chunk 里 `src/` 只有 38.9 kB（33 个文件，最大的 `lib/ttsRoles.ts` 3.0 kB、`ui/Toast.tsx` 2.8 kB、`App.tsx` 2.7 kB）—— 入口 86% 是 vendor 代码。** 这就是「为什么一个 281 kB 的入口 chunk 里几乎没有业务逻辑」。

gzip 归因（`slicegz.mjs`，每片单独 gzip，属**上界**）: zod 13.4 kB / axios 20.6 kB / tailwind-merge 8.8 kB / 应用代码 15.1 kB，三者合计 **131.5 kB raw ≈ 42.7 kB gzip，占入口 89.3 kB gzip 的 48%**。

### 1.3 vendor-graph（1,041.59 kB / 329.47 kB gzip）只有 6 个源文件

| 文件 | raw | 占 chunk |
|---|---|---|
| `klayjs@0.4.1/klay.js` | **448.2 kB** | **44.1%** |
| `cytoscape@3.34.4/dist/cytoscape.esm.mjs` | 438.0 kB | 43.1% |
| `layout-base@2.0.1` | 57.8 kB | 5.7% |
| `cose-base@2.2.0` | 45.6 kB | 4.5% |
| `cytoscape-fcose@2.2.0` | 20.9 kB | 2.1% |
| `cytoscape-klay@3.1.4` | 6.3 kB | 0.6% |

**klay 已经是死代码**（§2.2 F1）：`layoutFamilyTree` 上一轮改成纯 TS 的 `computeKinTreeLayout`（`graphLayouts.ts:167`（`layoutFamilyTree`）/ `:310`（`layoutFactionMap`）/ `:441`（`layoutPanorama`）/ `:477`（`runLayout`）），三种视图 `tree/faction/panorama` 分别走「自研 tidy-tree / 自研货架打包 / fcose」，**没有任何一处请求 `name:"klay"` 的布局**。但 `frontend/src/pages/GraphDemoPage.tsx:20` 仍 `import klay from "cytoscape-klay"`、`:61` 仍 `cytoscape.use(klay)`，于是 448 kB 被无条件打进 chunk、被每个打开图谱页的用户下载。

`cytoscape-dagre` 与 `cytoscape-node-html-label` 同样只存在于 `package.json` 和 `vite.config.ts` 的 manualChunks 正则里，`grep -rn "dagre|node-html-label|nodeHtmlLabel" frontend/src` **0 命中**（不进包，但是两笔纯负担的依赖声明）。

### 1.4 动态 import 与依赖盘点（含"查过没有"的负面结论）

- 全 `frontend/src` 只有 **13 处真实动态 import**，全部是 `App.tsx:11-23` 的路由级 `lazyPage(() => import("@/pages/…"))`。其余 8 处 `import(` 命中都是 `*.test.ts` 里的 `await import("./x.ts")` 和 1 处注释。**除 13 个页面外，198 个文件里所有模块都静态连进入口图**，包括 `AppShell/TopBar/SideNav/ui` 组件、`lib/api-client`、`services/auth`、`stores/*`。
- **没有 lodash、没有 dayjs、没有重复的 react 副本**（`agg.mjs` 对 vendor-core 的归因里只有一份 `react.production.js` 7.5 kB + 一份 `react-dom`）。
- **图标不是问题**：`lucide-react` 被 75 个文件引用，但 tree-shaking 生效——入口只带 5.0 kB，其余按图标拆成 ~40 个 0.12–0.48 kB 的独立 chunk（`chevron-down-abe6QWmv.js` 0.13 kB 等）。**不要提「换图标库」这类方案。**
- `tailwind-merge` 只被 `frontend/src/components/ui/cn.ts:2` 引用，`axios` 只被 `frontend/src/lib/api-client.ts:1` 引用——两个都是**单点依赖**，所以它们出现在入口 chunk 完全是因为入口图里必然要 import `cn()` 和 `apiClient`。
- **`@/components/ui` 桶文件没有把没用到的组件拖进入口**（这条要写清楚，避免有人去做无收益的改造）: `Select`/`Slider`/`Switch`/`Tabs` 都有自己独立的小 chunk（`Select-BrnciAIq.js` 21.22 kB、`Slider-CrP4Lfan.js` 11.01 kB、`Switch-CcOfCyIa.js` 4.28 kB、`Tabs-DNRpPBOt.js` 3.28 kB），说明 Rollup 对桶文件的 re-export 做了正确的 tree-shake。入口里留下的 radix 部分（menu/collection/tooltip/dialog/dismissable-layer/focus-scope/roving-focus/popper/floating-ui ≈ 79 kB raw）是 `App.tsx:4`（TooltipProvider、ToastViewport）+ `AppShell.tsx:6`（Drawer）+ `TopBar.tsx`（DropdownMenu）**首屏真的会渲染**的东西。

---

## 2. 首屏关键路径

### 2.1 实测链路（生产 4396，`Accept-Encoding: gzip`，loopback）

`dist/index.html` 里 Vite 只注入了 **1 条预载提示**:

```html
<link rel="modulepreload" crossorigin href="/static/vendor-core-B5iSmSrq.js">
<script type="module" crossorigin src="/static/index-q22jiYrq.js">
```

| 请求 | TTFB | 总时长 | 线上字节(gzip) | 磁盘 raw |
|---|---|---|---|---|
| `GET /`（index.html，含内联启动骨架） | 13.8 ms | 13.9 ms | 2,004 B | 6,303 B |
| `/static/index-Djndpho4.css` | 11.5 ms | 12.0 ms | 13,419 B | 78,403 B |
| `/static/vendor-core-B5iSmSrq.js` | 21.0 ms | 44.5 ms | 86,483 B | 271,755 B |
| `/static/index-q22jiYrq.js` | 35.0 ms | 66.2 ms | 91,292 B | 281,908 B |
| `/static/ShelfPage-B4nKyiQv.js`（落地路由） | 10.4 ms | 13.2 ms | 10,373 B | 29,905 B |
| **首屏合计** | | | **203,571 B ≈ 199 kB gzip** | **668,274 B ≈ 653 kB** |

生产侧头部（实测）: `content-encoding: gzip`、`cache-control: public, max-age=31536000, immutable`、`vary: accept-encoding` —— **压缩和长缓存都已就绪，不是问题点。**

链路形状: `index.html`（2 kB gzip，**零 JS 的内联骨架先画出壳**）→ `vendor-core`（并行预载）→ 入口 chunk 执行 → React 挂载替换骨架 → 才发起落地路由 chunk（`ShelfPage` 10.4 kB gzip）。

`index.html` 已经做对的部分（不要再动）: 内联骨架零请求零 JS、字体 CSS 用 `rel=preload as=style onload` 非阻塞切换、`preconnect` 两个字体域、`<noscript>` 兜底。

### 2.2 可延后 / 可预取（每条都带改法与成本）

#### F1 — 删掉 klayjs（**实测 −153.09 kB gzip，全篇最大单笔收益**）

- 证据: `frontend/src/pages/GraphDemoPage.tsx:20` `import klay from "cytoscape-klay";`、`:61` `cytoscape.use(klay);`；三种布局的实现在 `frontend/src/components/graph-demo/graphLayouts.ts`（`layoutFamilyTree` 用纯 TS `computeKinTreeLayout`、`layoutFactionMap` 自研货架打包、`layoutPanorama` 用 `name:"fcose"`），**没有任何 `name:"klay"`**；`graphElements.ts:25` 的提示文案「klay 分层」是上一轮重写后遗留的过期注释。
- 实测（`.tmp/perf/exp`，只删那 2 行 + 重新 `npx vite build`）:
  - `vendor-graph`: 1,041.59 kB → **568.03 kB raw**；329.47 kB → **176.38 kB gzip** ⇒ **−473.56 kB raw / −153.09 kB gzip（该 chunk 的 46.5%）**
  - `GraphDemoPage` chunk 不变（64.46 / 22.74 kB），`vendor-core` 与 `index` 逐字节不变。
  - 旁证: `gzip -c klayjs/klay.js` = 154,240 B。
- 顺带收益: rust-embed 把 `web-ui/dist` 编进二进制（§2.3），所以**发布二进制同时瘦 473 kB**。
- 成本: 删 2 行 + 顺手把 `graphElements.ts:25` 的 hint 改成「自研 tidy-tree」；`package.json` 里 `cytoscape-klay`（以及同样没人 import 的 `cytoscape-dagre`、`cytoscape-node-html-label`）可一并移除，`vite.config.ts` 的 manualChunks 正则同步收窄。风险≈0（死代码），需要人工点一遍 `/graph-demo` 三个视图做视觉回归。

#### F2 — 把 axios 换成 fetch（估算 −17~20 kB gzip 关键链，砍掉一个依赖）

- 证据: `frontend/src/lib/api-client.ts:1` 是**全仓唯一** axios 引用（`grep -rn "from \"axios\"" src/` 只有这一行）；`grep -rn apiClient src/ | grep -v "src/lib/api-client.ts"` **无输出** ⇒ `apiClient: AxiosInstance` 没有任何外部使用者，重写只需改这一个文件。对外暴露面（已枚举）: `API_BASE_URL, ApiError, ApiParams, ApiRequestConfig, UNAUTHORIZED_EVENT, UnauthorizedError, dispatchUnauthorized, envelopeMessage, get, isApiEnvelope, parseWith, post, stripNullsDeep, toApiError`。
- 实测贡献: axios 在入口 chunk 里 50.4 kB raw / **20.6 kB gzip（单片上界）**；它做的事只是 `axios.create({baseURL, timeout, withCredentials})` + 两个拦截器（`api-client.ts:98` 请求拦截 / `:108` 响应拦截）+ `get`（`:143`）/`post`（`:158`）薄封装，语义上完全可以用 `fetch` + `AbortSignal.any([AbortSignal.timeout(t), signal])` + `credentials:"include"` 复刻。
- 成本: 中等（~180 行文件重写一遍，要复刻 4 个错误分支：`ERR_CANCELED`→"请求已取消"、`ECONNABORTED/ETIMEDOUT`→"请求超时"、无 response→"网络连接失败"、`NEED_LOGIN/401`→`dispatchUnauthorized`+`UnauthorizedError`）。**注意 `services/fonts.ts:24` 的注释已经写明「大文件上传走 fetch 直传，axios 实例的 30s 超时会把慢网大文件掐断」**——说明团队已经在绕 axios 的超时语义，换 fetch 反而统一了这条链路。
- 验证要求: 401 全局登出、超时、取消、`NEED_LOGIN` 四条分支都要真机点一遍。

#### F3 — 把 zod 挪出首屏关键链（估算 −13.4 kB gzip 关键链，**不减少总字节**）

- 证据: zod 在入口 chunk 里 54.4 kB raw（`zod/v3/types.js` 单文件 46.5 kB），入口链上的 zod 消费者只有三处——`frontend/src/lib/api-client.ts:2`（`parseWith`）、`frontend/src/services/auth.ts:1`（`loginResultSchema`/`userInfoSchema`/`systemInfoSchema`）、`frontend/src/types/api.ts:1`（被 `auth-store.ts` 与 `api-client.ts` 静态引入）。而 `App.tsx:7` 的 `useAuth()` 在**首帧同步**就走到这条链。
- 改法（最小改动版）: ① 把 `parseWith` + `stripNullsDeep` 从 `api-client.ts` 移到新文件 `src/lib/parse.ts`（只有它 import zod），`api-client.ts` 不再 import zod；② `useAuth.ts:4` 的 `import { logout as logoutRequest } from "@/services/auth"` 改成在 `logout()` 里 `const { logout } = await import("@/services/auth")`（登出是用户点击行为，不在首帧）；③ `stores/auth-store.ts:2` 的 `import type { LoginResult }` 已是类型导入，无需动。
- 收益边界（必须说清）: zod 仍会被**第一个页面 chunk** 拉下来（21 个 service 都在用），所以**总下载字节不变**；省的是「首壳（AppShell + 骨架）绘制前必须等的那 13.4 kB gzip + 一段解析执行」。首屏路由 chunk 与 zod 并行下载，墙钟收益取决于带宽，保守估计 **10–40 ms**。
- 成本: 中等偏低，但要重新跑一次 `agg.mjs` 确认 zod 落到哪个 chunk（预期落在被多页面共享的公共 chunk 里），并验证 `/login` 与 `/graph-demo`（免登录页）不因此多拉一次请求。

#### F4 — 给落地路由 chunk 加 `modulepreload`（估算省一次串行 RTT）

- 证据: `dist/index.html` 只有 `vendor-core` 一条 `modulepreload`；`ShelfPage-B4nKyiQv.js`（10,373 B gzip）要等入口 chunk 执行完、React 走到 `/` 路由才开始下载。实测这段是**串行**的：入口 chunk 总时长 66.2 ms，落地 chunk 13.2 ms。
- 收益: loopback 上就是 13 ms；按 4G RTT 100 ms 估，**省 ~100 ms**。因为 gzip 后只有 10.4 kB，预取几乎不抢带宽。
- 改法: 在构建期把落地 chunk 的 hash 注入 `index.html`。Vite 原生没有「给非入口 chunk 加 preload」的配置，需要一个小插件（`transformIndexHtml` 钩子，按 `manifest` 找到 `ShelfPage-*.js` 并插入 `<link rel="modulepreload">`），或退一步：把 `App.tsx` 里的 `ShelfPage` 改成静态 import（代价是把 10.51 kB gzip 从「晚一点并行拉」变成「入口里必带」，净字节不变、少一个 RTT，但丢掉该页的独立缓存粒度）。
- 成本: 小插件约 20 行；**建议连同 F1 一起做一次，别单独排期**。

#### F5 — 明确不建议做的（已实测证伪，写下来防止有人重做）

| 想法 | 实测 | 结论 |
|---|---|---|
| 优化 `rankResults`（每次 flush 全量重排） | n=2000 → **1.45 ms**，n=4000 → **2.82 ms**（`bench-rank.mjs`） | 不值得 |
| 优化家族树布局计算 | `computeKinTreeLayout` 139 人/62 亲缘边: 1400×800 → **3.72 ms**，390×780 → **2.48 ms** | 不值得 |
| 优化搜索长列表渲染 | `SearchPage.tsx:421` 已用 Virtuoso 虚拟化（`increaseViewportBy={600}`、`customScrollParent=main`），代码注释记录了基线「200 条一次性挂 DOM 单次长任务 2.2 s」 | 已解决，别再提 |
| 改 `@/components/ui` 桶文件 | `Select/Slider/Switch/Tabs` 已各自独立成 chunk（§1.4） | 无收益 |
| 换图标库 / 按需引入 lucide | 入口只 5.0 kB，其余按图标分 chunk | 无收益 |
| 拆 vendor-core | react-dom 单文件 172.6 kB 占该 chunk 65.2%，拆了也还是首屏必需 | 无收益（只是缓存粒度） |

### 2.3 rust-embed 对代码分割的限制（`backend/src/web_assets.rs`）

```rust
// backend/src/web_assets.rs:1-13
//! 前端静态资源内嵌（rust-embed——编译时打包 web-ui/dist 进二进制）。
//! 动机：裸二进制分发（Windows exe / Linux musl）不再依赖外部 web-ui/dist 目录
//! 注意：cargo build 前 web-ui/dist 必须已构建（CI 编排：npm build 先于 cargo build）。
#[derive(RustEmbed)]
#[folder = "web-ui/dist/"]
```

配套: `backend/Cargo.toml:12` `rust-embed = { version = "8", features = ["debug-embed"] }`；`backend/src/api/router.rs:13769`「内嵌资产优先（rust-embed——发布单文件免外部 dist）；磁盘目录回退」；`backend/src/lib.rs:96` 回退目录名 `"web-ui/dist"`。

它对前端性能的真实约束有四条:

1. **资产集合 = 二进制的一部分，chunk 划分只能影响「下载顺序」，不能影响「分发方式」**。想让某个 chunk 走 CDN、或做灰度/按需上传，rust-embed 这条路走不通（要么换磁盘回退模式，要么在反代上把 `/static/*` 前置到对象存储）。
2. **所有 chunk 的字节都会进发布产物**。当前 `dist/` 33.1 MB，其中 JS 2.13 MB、CSS 78 kB、**`fonts/` 32.24 MB**。也就是说 F1 删掉 klay 不止省用户 153 kB 下载，还让**二进制/镜像瘦 473 kB**。
3. **发布是原子的，chunk 404 风险只存在于多实例滚动发布期间**。`lib/lazy.tsx` 的 `ChunkErrorBoundary` 兜底（"版本已更新" + reload）覆盖的正是这个窗口；`cache-control: immutable` 一年，所以新版本必须换 hash（Vite 已保证）。
4. **构建顺序耦合**: 前端构建必须先于 `cargo build`（CI 编排如此）。这意味着**任何前端 chunk 实验都要重编后端**才能在生产二进制里生效——评估「改法成本」时要把这条算进去，也是为什么「先量、再改」比「边改边试」划算。

### 2.4 附带发现: `frontend/public/fonts/` 是 32 MB 死资产

- 实测: `frontend/public/fonts/` **607 个 `.woff2`、32,241,363 B（32.24 MB）**，被 Vite 原样复制到 `dist/fonts/`，再被 rust-embed 全量编进二进制。
- **没有任何东西引用它们**: 构建产物 `dist/static/index-Djndpho4.css` 里 `@font-face` 数量 = **0**、`unicode-range` = **0**；`grep -rn "\.woff2" frontend/src frontend/index.html` 只有一处 `accept=".ttf,.otf,.woff,.woff2"`（上传控件的文件类型过滤，见 `components/settings/ReadingPreferencesCard.tsx:243`）；`public/sw.js:129` 只在 cache-first 分支里提到 `/fonts/` 前缀，而 `PRECACHE`（`sw.js:15`）**不含任何字体**。页面实际用的中文字体走外链 `fonts.loli.net`（`index.html` 的 preload 那两行）。
- 成本/收益: 删掉该目录 → `dist/` 33.1 MB → **2.4 MB**（−32.24 MB），二进制/镜像同步瘦 32 MB。风险: 需确认它不是老版本 UI（`backend/web-ui/` 旧产物）的遗留依赖——`git ls-files frontend/public/fonts | wc -l` = 607（**607 个文件都还在版本库里**），建议先 `git log --diff-filter=A -- frontend/public/fonts | tail -1` 确认来源再删。
- 这条不属于「首屏 ms」，但属于「发布产物体积」，且金额比 F1 大 68 倍，值得单独一个 PR。

---

## 3. 运行时热点

### H1 — 目录（TOC）每次加载在主线程上多烧 ~7.6 ms（实测）

- 证据: `frontend/src/services/book.ts:73` `getChapterList` → `parseWith(bookChapterListSchema, data)` → `frontend/src/lib/api-client.ts:198` 的 `parseWith`（`schema.safeParse(stripNullsDeep(data))`）。
- 实测（`bench-parse.mjs`，真实负载 `payloads/chapters-1419.json` = 197,784 B / 1,419 章，取自生产 `POST /getChapterList`）:

| 步骤 | 耗时 |
|---|---|
| `JSON.parse`（仅解析） | 2.47 ms |
| `stripNullsDeep`（递归 null→undefined） | 1.55 ms |
| `bookChapterListSchema.parse`（1419 章逐字段校验） | **8.50 ms** |
| **实际路径合计** | **≈ 10.05 ms**（主线程阻塞） |

- 改法: 目录是**纯展示数据、字段全部可缺省**，对 1419 章逐条跑 zod 的收益极低。把 `getChapterList` 的 `parseWith` 换成「类型断言 + 只校验前 N 条（如 5 条）」或干脆 `as BookChapter[]`：`services/book.ts:73-82`。收益 **≈ 7.6 ms/次目录加载**（10.05 → 2.47 ms），对 1747 章的大部头（`玄鉴仙族`）线性更大。
- 成本: 低（改 3 行），风险是「后端字段漂移时前端不再报『响应格式错误』而是运行时 `undefined`」——建议保留首条校验。

### H2 — 换源面板「比对本章」: 20 个候选 × 2 请求，墙钟 ≈ 7.5 s（实测分项 + 波次估算）

- 证据: `frontend/src/components/book/SwitchSourceDialog.tsx:116` `probeChapters()`（函数体到 `:170`）：并发度 `Math.min(3, list.length)`（`:168`），每个候选先 `await getChapterList(candidate.bookUrl)`（`:142`）再 `await getBookContent(target.url, target.index, …)`（`:150`）。
- 实测单请求（生产 4396，缓存命中/未命中分开）:

| 请求 | 耗时 | 负载 |
|---|---|---|
| `POST /getAvailableBookSource`（打开面板，1 次） | 3.2–4.5 ms | 8,873–10,725 B，**20 个候选** |
| `POST /getChapterList`（比对第 1 步） | 44.1 / 59.3 / 94.0 ms | 13.5 / 40.7 / **197.8 kB**（100 / 253 / **1,419 章**） |
| `POST /getBookContent`（比对第 2 步，**未命中缓存**) | **973.1 ms** | 9,803 B |

- 推算: 20 候选 ÷ 并发 3 = 7 波 × (≈94 ms 目录 + ≈973 ms 正文) ≈ **7.5 s**，期间每完成一行就 `setProbes({...results})` + `setProbeProgress(...)`（`:164-165`）——**2 次 setState × 20 行 = 40 次重渲染**，每次都会重建整个候选列表的 React 元素（列表用 `list.map` 直接渲染，`:258`，没有 memo）。
- 改法（按性价比排序）:
  1. **`probes` 用 `Map` + 每行 `React.memo`**（`SwitchSourceDialog.tsx:258` 的 `<button>` 抽成 `CandidateRow` 组件并 `memo`，props 只传该行的 `probe`）→ 把 40 次全列表重渲染压到 20 次单行重渲染。收益: 每次重渲染从「20 行 DOM diff」降到「1 行」，估算省 **~5–15 ms/行 × 20 ≈ 100–300 ms** 总墙钟（不减少网络）。
  2. **正文请求并发度从 3 提到 6**（`:168`）: 波次从 7 降到 4，估算 **7.5 s → ~4.3 s**。成本: 对源站压力翻倍，需与后端确认（代码注释明确写「并发 3，控制对源站压力」，属于**产品取舍**，不能当纯性能改动做）。
  3. **别把 `getChapterList` 的 197.8 kB 全量 zod 校验放在这条链上**: 同 H1，每个候选都要吃一次 `bookChapterListSchema.parse`（1419 章时 8.50 ms × 3 并发 = 主线程连续阻塞）——H1 的改法在这里**再赚一次**。
- 补充（数据质量，不是性能）: 20 个候选里 `latestChapterTitle` 有 **10 个是 null**（实测），所以面板有一半行的副标题是空白（`rowDetail` 走 `chapterTitleText(candidate.latestChapterTitle)`，`:186`）。

### H3 — 图谱页加载时 zod 校验吃掉 14.3 ms 主线程（实测）

- 证据: `frontend/src/pages/GraphDemoPage.tsx` 加载 `/graph-demo-data.json`（**103,830 B**，139 人 / 277 关系 / 161 成员 / 8 势力）后过 `parseGraphDemoData`（`frontend/src/types/graphDemo.ts:117` → `graphDemoSchema.safeParse`）。
- 实测: `JSON.parse` **0.99 ms** vs `parseGraphDemoData` **15.32 ms** ⇒ **校验税 14.3 ms**，且发生在首帧前的同步路径上。
- 改法: 这份数据是**仓库内离线脚本生成的静态文件**（`graph-demo-data.json` 的 `meta.schema = "graph-demo-v1"`，与前端同仓同版本），校验的边际价值很低。可改成「解析失败才走 zod 兜底」：先用 `JSON.parse` + 一次轻量形状检查（`Array.isArray(data.characters)`），异常时才 `parseGraphDemoData`。收益 **≈ 14.3 ms/次打开**。成本: 低（`types/graphDemo.ts` 增加一个 `fastParse` 导出，`GraphDemoPage` 改调用点）。
- 注意: 页内已有内联兜底数据（`GraphDemoPage.tsx` 头部注释「文件缺失或校验不过时用文件内自带的兜底数据」），所以降级路径是现成的。

### H4 — 滑块拖动 / 重排: 上一轮已经做对，本轮只剩两个小尾巴

- 已正确: `handleChapterPreview`（`GraphDemoPage.tsx:1055`）拖动中只 `setChapter`（切显隐，不重排）；`handleChapterCommit`（`:1062`）松手才 `layoutAndLabel`；`syncEdgeRoutes`（`:762`）只在 zoom 跨过 `DETOUR_MIN_ZOOM` 时重算绕行；`applyFontScale`（`:747`）只在 `labelFontScale` 变化 >0.05 或窄屏标记翻转时重建样式表。**不要动这些。**
- 尾巴 1: `GraphDemoPage.tsx:1038-1042` 的 effect 依赖 `[visibility, mode, minDegree, showIntraFaction, treeFamilyIds]`，而 `visibility` 是 `useMemo(() => computeVisibility(data, chapter), [data, chapter])`（`:562`）——**拖动过程中每变一章都会触发一次 `applyVisibility + applyModeFilter`**。这是设计意图（拖动就要看显隐变化），但 `applyModeFilter` 每次都遍历全部元素，值得实测一次拖动全程的帧时间再决定是否加 100 ms 节流（本轮没有 DevTools Profiler 环境，**未实测，不作为结论**）。
- 尾巴 2: `routeEdges`（`graphEdgeRoutes.ts:49`）在家族树 + `zoom ≥ 1` 时对每条边做中点探针，是 O(边 × 节点)。当前 139 人 / 277 关系下未观察到问题（本轮无法在生产上采样帧耗时，**未实测**）；如果未来数据到 500+ 人，这是第一个该看的地方。

### H5 — SSE 搜索长列表: 现状已优化到位，只剩一个可选的「省一次字符串拼接」

- 证据链（`frontend/src/hooks/useSearchSSE.ts`）: 100 ms 合并窗口（`:71` `FLUSH_INTERVAL_MS`）→ `flushNow`（`:320`）一次性 `setResults(rankResults(...))`；累加器用 `Set` 去重 + `Map` 聚合（`:118-160`）；静默态用 `reduceQuiet` 且「状态没变就不 setState」（`:253-261`）；`useEffect` 卸载时关掉在飞的 EventSource + 清定时器（`:353-364`）；`onEnd` 立即 flush（`:509`）。
- 实测: `rankResults` 在 4000 条时 **2.82 ms**（§2.2 F5），`searchSSEBatchSchema.safeParse`（`:490`）单批开销可忽略（同 schema 量级的 `availableBookSourceListSchema` 20 条只要 0.60 ms）。
- 唯一可挑的: `SearchPage.tsx:426` 的 `computeItemKey` 每次渲染都 `row.map(...).join(",")` 新建字符串；`ResultRow`（`SearchPage.tsx:109`）没有 `memo`，`itemContent` 是内联闭包。收益量级是**每行几十纳秒**，在虚拟化只挂可视区的前提下没有意义——**记录在案，不建议改**。
- `frontend/src/lib/sse.ts:76` 用的是原生 `EventSource`（`withCredentials: true`），`finish()` 幂等，`end` 事件后主动 close 避免自动重连——这条链路没有重复请求问题。

### H6 — 重复请求: 逐个查过，没有发现

- 打开换源面板只发 **1 个** `getAvailableBookSource`（`SwitchSourceDialog.tsx:72`，`useQuery` 缓存键含 `bookUrl` + `forceSearch`）。
- 候选行显示「最新章」**不发请求**，用的是同一个响应里的 `latestChapterTitle`（`:182`）。
- 目录/正文/书详情的缓存键分别是 `["chapters"|"content"|"bookInfo", bookUrl]`，换源成功后统一 invalidate（`:101-103`）。
- 搜索页 `shelfKeys`、`resultRows` 都有 `useMemo`；`useBookshelf`/`useSearchHistory` 走 react-query 缓存（`main.tsx` `staleTime: 30_000`、`refetchOnWindowFocus: false`）。

---

## 4. 优先级汇总

| # | 改动 | 类型 | 实测/估算收益 | 成本 | 风险 |
|---|---|---|---|---|---|
| **F1** | 删 klay（`GraphDemoPage.tsx:20,61`） | 构建+运行时 | **−473.56 kB raw / −153.09 kB gzip**（实测）+ 二进制瘦 473 kB | 删 2 行 | 极低 |
| **F6** | 删 `frontend/public/fonts/`（607 个 woff2，无引用） | 产物体积 | `dist/` 33.1 MB → **2.4 MB**（−32.24 MB），二进制同步瘦 | 删目录 | 低（先确认来源） |
| **H1** | TOC 跳过全量 zod（`services/book.ts:73`） | 运行时 | **≈ −7.6 ms/次**（10.05 → 2.47 ms，实测分项） | 3 行 | 低 |
| **H3** | 图谱数据 fastParse（`types/graphDemo.ts:117`） | 运行时 | **≈ −14.3 ms/次**（15.32 → 0.99 ms，实测） | ~15 行 | 低 |
| **F2** | axios → fetch（`lib/api-client.ts`） | 构建 | **≈ −17~20 kB gzip** 关键链（axios 单片 gzip 20.6 kB 为上界），砍 1 个依赖 | 重写 1 个文件 + 4 条错误分支真机验证 | 中 |
| **H2** | 换源面板行级 memo（`SwitchSourceDialog.tsx:246`） | 运行时 | 40 次全列表重渲染 → 20 次单行，估 **−100~300 ms** 总墙钟 | ~30 行 | 低 |
| **F3** | zod 移出首屏链（`api-client.ts:2`、`services/auth.ts:1`） | 构建 | 关键链 **−13.4 kB gzip**，估 **−10~40 ms** FCP；总字节不变 | 中等 | 中 |
| **F4** | 落地路由 chunk `modulepreload` | 首屏 | 省 1 次串行 RTT（loopback 13 ms，4G 估 ~100 ms） | ~20 行插件 | 低 |
| — | H2-2 比对并发 3→6 | 运行时 | 7.5 s → ~4.3 s | 1 个常量 | **产品取舍**（源站压力） |

**建议的落地顺序**: F1 + F6（一个 PR，纯删除，收益最大且风险最低）→ H1 + H3（两个小补丁，纯收益）→ F2（独立 PR，需真机回归 401/超时/取消）→ H2-1 → F3/F4（可选）。

---

## 5. 数据出处速查

- 体积/gzip: `pnpm build` 输出（§1.1）；逐包归因 `agg.mjs`/`attr.mjs`/`appsrc.mjs`/`slicegz.mjs`（§1.2–1.3）。
- klay 删除收益: `.tmp/perf/exp` 对照构建（§2.2 F1）。
- 线上字节: `curl -H "Accept-Encoding: gzip" -w "%{size_download}"` 对 4396 的 5 个首屏资源（§2.1）。
- zod/布局/排序耗时: `bench-parse.mjs` / `bench-graph.mjs` / `bench-rank.mjs`（§2.2 F5、§3 H1/H3/H5）。
- 接口耗时与负载: `curl -X POST` 对 4396 的 `getAvailableBookSource` / `getChapterList` / `getBookContent`，负载样本留在 `.tmp/perf/payloads/`（§3 H2）。
- 字体死资产: `du -sb dist dist/fonts`、`grep -c "@font-face" dist/static/*.css` = 0、`git ls-files frontend/public/fonts | wc -l` = 607（§2.4）。

---

## 6. 本轮受限/未完成

1. **生产 token 在测量中途失效**（后续请求返回 `{"isSuccess":false,"errorMsg":"请登录后使用","data":"NEED_LOGIN"}`），所以 §3 H2 的「20 候选全量比对墙钟」是**用实测分项 + 波次公式推算的 7.5 s**，不是端到端秒表读数。拿到新 token 后重跑 `.tmp/perf` 里的 curl 序列即可校准。
2. **没有浏览器 DevTools Profiler 环境**，所以 H4 的「拖动全程帧耗时」、H5 的「Virtuoso 实际渲染行数」只有代码级证据，没有帧级实测——已在正文标注「未实测，不作为结论」。
3. **F2（axios→fetch）的实验构建没跑完**：向 `.tmp/perf/exp/src/lib/api-client.ts` 写 fetch 版实现时被文件工具拒绝（"file has not been read"），因此 F2 的「−17~20 kB gzip」是**按 axios 单片 gzip 20.6 kB 推的上界**，不是对照构建的实测值。要拿实测值：`read` 该文件后写入 fetch 版 → 在 `exp/` 里 `npx vite build` → 与基线 `index-q22jiYrq.js` 的 281.87 kB / 91.44 kB 对比。
