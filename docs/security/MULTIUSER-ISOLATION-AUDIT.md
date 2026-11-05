# 多用户隔离与个人配置审计（双模型交叉 + 线上实测）

- 日期：2026-11-04
- 对象：reader（生产实例 `127.0.0.1:4396`，`secure=true` 多用户模式）
- 方法：两路只读静态审计（后端 A / 前端 B）+ 本项目在**生产库上**的双账号实测（A=`testuser01` 管理员，B=临时账号 `audituser01`，审计结束后已删除）
- 结论级别：✅ 我亲自实测或逐行复核过；⚠️ 单侧报告指出、我仅做了代码抽查

---

## 0. 一句话结论

**服务端数据面（书架 / 书源 / RSS / 替换规则 / 书签 / 字体 / AI 会话 / 管理接口）按命名空间隔离是对的**：身份只从 `accessToken` 解析，请求里带的 `ns`/`username` 不参与身份判定；我在生产上用两个账号逐项试过，A 的书架 6 本、书源 615、书源 Cookie、RSS 对 B 全部不可见，B 调管理接口全部被拒。

但有 **3 个真实缺陷**：1 个服务端跨用户文件读取（已复现）、2 个在前端（本地存储不分用户、登出不清缓存）。

另外「个人配置」在语义上**当前并不存在**：后端有 `user_config` 表且按用户隔离，但**前端从未调用**（全仓 grep `getUserConfig|saveUserConfig` = 0 命中）。偏好、批注、阅读进度全在 localStorage —— 不跨设备、不分账号。所以「多设备同步冲突」今天反而不成立；真实问题是**换号继承**和**换设备丢失**。

---

## 1. P0

### P0-1 本地书文件通道可跨命名空间读他人私有文件 ✅ 已实测

**证据链**
- 入口 `backend/src/api/router.rs:4558`：`chapter_url.contains("#") && is_loc_book_file_chapter(...)` → 分派到 `get_book_content_file`
- `is_loc_book_file_chapter`（`router.rs:13319`）：`book_part.starts_with("storage/")` **一律返回 true**（不看扩展名、不看归属）
- `get_book_content_file`（`router.rs:13535`）：
  ```rust
  let shelf = state.storage.find_book(ns, book_part).await.ok().flatten(); // 仅用于 toc_mode / split_long
  let path = resolve_loc_book_file(&state.storage.config.storage_dir(), book_part)?; // 路径直接来自请求
  ```
  `resolve_loc_book_file` 只保证 canonicalize 后仍在 `storage_dir` 内 —— 即「不逃出 storage 根」，但**storage 根下所有用户目录都在里面**。

**复现（B 是普通用户）**
```bash
# 在 A 的 webdav 目录放一个私有 txt（模拟用户自己上传的文本）
/home/reader/storage/data/testuser01/webdav/audit-secret.txt

curl "$BASE/reader3/getBookToc?accessToken=$B&url=storage/data/testuser01/webdav/audit-secret.txt"
# → {"data":[{"title":"正文","url":"storage/data/testuser01/webdav/audit-secret.txt#0","chapterWordCount":58}]}

curl "$BASE/reader3/getBookContent?accessToken=$B&chapterUrl=storage/data/testuser01/webdav/audit-secret.txt%230"
# → {"data":{"content":"第一行 private-A 笔记标题\n这是 A 的私密内容 AUDIT_MARKER_9911\n第三行 A 的第二段"}}
```

**影响面**（实测边界）
- 可读：`.txt` / `.epub` / `.pdf` 等 `SUPPORTED_EXTENSIONS` 白名单内的文件 —— 覆盖 webdav 目录里用户放的书/笔记、`data/<user>/` 下的导入源文件
- 不可读：`json` / `sqlite` / `zip`（返回「本地书章节不存在」），所以 `reader.db`、`bookGroup.json`、`webdav/legado/*.zip` 备份包拿不到
- 目录名可枚举：用户名在 `data/<username>/` 下明文，且 `getUsers` 之外还有 OPDS / 用户列表等渠道

**同侧第二个洞**：`get_book_toc_local`（`router.rs:13593`）签名是 `_namespace: &str` —— **参数收到了但完全没用**，`list_chapters_with_word_count(book_url)` 不带 ns。

**修法（约 3 处，各 1 行）**
```rust
// get_book_content_file / get_book_toc_file / get_book_toc_local 开头统一：
let Some(shelf) = state.storage.find_book(ns, book_part).await.ok().flatten() else { return None; };
```
即「不在本人书架上的文件路径一律不认」。**无回归风险**：本机 `books` 表 6 本全是网络书，`select count(*) from books where book_url like 'storage/%'` = 0，legacy 直读路径当前没有任何合法读者（`collect_export_chapters` at `router.rs:5222` 已是这个范式，照抄即可）。

### P0-2 前端本地存储不分用户 ✅ 已实测

所有键都不带用户维度（来源行号见 §3 表）：

`reader.annotations` / `reader.progress.<bookUrl>` / `reader.searchHistory` / `reader.settings` / `reader.shelfLayout` / `reader.sources.pageSize` / `reader.aiScope` / `reader.bookInfoCache` / `reader.navCollapsed` / `reader.explore.health.v1`；IndexedDB `reader.chapterCache` 同样只有 bookUrl。

实测：A 写入一条批注 + 阅读进度 + 搜索词 → 仅把 `localStorage["reader.accessToken"]` 换成 B → 三条原样可读：

```
换 B 后 reader.annotations: {"byBook":{"https://www.heiyan.org/book/123":[{"text":"A 的私密划线","note":"A 的私人笔记：别告诉别人",...}]}}
换 B 后 阅读进度: {"index":7,"pos":1234}
换 B 后 搜索历史: ["A搜过的关键词xp"]
```

更严重的是**批注只有本地副本**（`frontend/src/stores/annotations-store.ts`，全仓无批注同步接口）→ B 打开同一本书能看到 A 的私密笔记并删除，**删完 A 的数据永久消失**（服务端没有副本可恢复）。

### P0-3 登出不清缓存 → 非管理员看到全量账号列表 ✅ 已实测

- `frontend/src/hooks/useAuth.ts:65-72` `logout()` 只清 token；**全仓无 `queryClient.clear()`**，无 store reset
- `frontend/src/main.tsx` react-query 全局 `staleTime: 30_000`，且所有 queryKey 无用户维度
- `frontend/src/hooks/useAppMode.ts:33-37` `["systemInfo"]` 另设 `staleTime: 5 * 60_000`，`isAdmin` 从这里取
- `frontend/src/pages/UsersPage.tsx:115/130/223` 只用缓存里的 `isAdmin` 做守卫

实测：A(admin) 打开 `/users` → 把 token 换成 B(非管理员) → SPA 内跳 `/users` → **仍完整渲染「用户管理」页**，含两个账号的配额、上次登录时间。

后端 `/getSystemInfo` 对 B 返回 `isAdmin:false`（已验），所以整页刷新即恢复 —— 泄露窗口 = 同一 tab 内登出→换号后的 **5 分钟**。写操作仍被后端 403 拦住（`getUsers/addUser/updateUser/deleteUser` 对 B 全返回「仅管理员可执行该操作」），所以是**信息泄露**，不是提权。

---

## 2. P1 — 服务端

| # | 位置 | 问题 | 复核 |
|---|---|---|---|
| S1 | `storage/mod.rs:2665` `save_chapters`、`4707` `save_local_book`、`4755` `migrate_loc_book`、`4885` `replace_chapters` | 章节写入是 4 列 INSERT，**丢 `user_namespace`**，落表默认 `'default'`；而读路径 `get_chapter_content`(`:2756`) 严格按 ns → 自己写的读不到，管理员带 `?ns=default` 反而读到全站 | ⚠️ 代码成立；线上 `book_chapters` 3575 行全在 `testuser01`（主路径 `cache_chapter_content` 带 ns），属条件性 |
| S2 | `storage/mod.rs:2683` `list_chapters`、`2742` `list_chapters_with_word_count`、`4899` `list_chapters_full` | 章节读路径**无 ns 过滤**；`get_book_toc_local` 的 `_namespace` 未使用（`router.rs:13593`） | ✅ |
| S3 | `storage/mod.rs:4749` | `migrate_loc_book` 裸 `DELETE FROM book_chapters WHERE book_url = ?1` —— 删掉**所有**命名空间该 url 的章节；`delete_book`(`:3177`) 同类 | ⚠️ |
| S4 | `storage/mod.rs:658` | `bookmarks` 主键 `(book_url, title)` **不含 ns**，`save_bookmark`(`:3585`) 直接 `INSERT OR REPLACE` → B 用同名 title 会顶掉 A 的行并把 `user_namespace` 改成 B | ✅ |
| S5 | `service/graph/job.rs:259`、`router.rs:10480/10544` | 图任务注册表以 `book_url` 为键（无 ns），`graph_sse`/`graph_cancel` 里 `let _namespace = ...` 未使用 → B 可取消 / 旁观 A 的生成任务，`start_job` 还会把 A 的任务 `reused:true` 给 B | ✅ |
| S6 | `api/router.rs:8312`（OPDS Basic）、`api/webdav.rs:101`（WebDAV Basic） | Basic 认证只看 `enable_webdav`/密码，**不查 `disabled`**（对比 `resolve_current_user` 有 disabled + TTL）→ 停用账号仍可全量读写 | ✅ |
| S7 | `storage/mod.rs:5349` | `reset_user_password` 只清 `token`，**不清 `token_map`** → 改密后旧 token 仍命中 `token_map_valid` 继续有效；`logout_user`(`:5116`) 同病 | ✅ |
| S8 | `model/user.rs:77` | `token_map_valid` 里非 Number 值 `=> true`（永不过期），旧数组形态完全不看到期；token 用 `==` 比较（非常量时间） | ⚠️ |
| S9 | `storage/mod.rs:4208` (+`router.rs:5778`) | `user_config` 整包 `INSERT OR REPLACE`，无版本号/`updated_at` 比较 → 多设备互相覆盖（`@updateTime` 只写不读）。当前前端不接线，属「一接就炸」 | ✅ |
| S10 | `storage/mod.rs:3500` | `update_book_progress` 无 `dur_chapter_time` 比较 → 离线设备回网把旧进度写回，覆盖刚读到的章节 | ⚠️ |
| S11 | `router.rs:13008 / 13173 / 13628` | 本地书三条导入路径不校验 `book_limit`（`saveBook` 有，`save_local_book` 没有）→ 配额绕过 | ⚠️ |
| S12 | `api/files.rs:82` | `__LOCAL_STORE__` 是**全站共享目录** `storage/localStore`，只靠 `enable_local_store` 开关；两人都开则互相可见（读无需 manager，写/删需要） | ✅ 代码；实测 B 被「未开启本地书仓功能」挡住（当前均未开） |
| S13 | `frontend/src/services/api-client.ts:98` / `lib/sse.ts:39` | `accessToken` 拼在 URL query 上 → 进 access log / Referer / 浏览器历史 / 代理日志 | ✅ |

---

## 3. P1 — 前端（本地存储与缓存）

| 键 | 位置 | 后果 |
|---|---|---|
| `reader.settings` | `stores/settings-store.ts:254` | 主题/字号/字体/TTS 换号即继承；同机两账号无法各自偏好 |
| `reader.annotations` | `stores/annotations-store.ts:31` | **私密笔记跨账号可见可删**，且无服务端副本 |
| `reader.progress.<bookUrl>` | `hooks/useReaderProgress.ts:26` | 阅读位置串号；本地记录优先于 `book.durChapterPos`（`:153-156`） |
| `reader.searchHistory` | `hooks/useSearchHistory.ts:5` | 搜索词串号 |
| `reader.shelfLayout` / `reader.sources.pageSize` / `reader.navCollapsed` / `reader.aiScope` / `reader.bookInfoCache` / `reader.explore.health.v1` | 见 §0 清单 | 视图/缓存串号 |
| IndexedDB `reader.chapterCache` | `services/chapterCache.ts:26` | 正文缓存跨账号共享（内容本身是公开网页，风险低） |
| `["systemInfo"]` 5min / 其他 queryKey 30s，无用户维度 | `hooks/useAppMode.ts:33`、`main.tsx` | 见 P0-3 |

---

## 4. P2

- `router.rs:5790-5805`：`userConfig` 的 `ns`/`key` 直取 body 且无长度上限（可无限建配置行）
- `router.rs:8097`：管理员 `?ns=default` 是显式设计，但与 S1/S2 叠加等于「管理员可读全站本地书正文」——修完 S1/S2 后复核其必要性
- `router.rs:13593 / 10480 / 10544`：函数签名里带 `_namespace` / `let _namespace` 未使用 —— 建议列为 review 规则，这已是第二个由此发现的洞
- `router.rs:8310`：OPDS 独立账号的 ns 在 `users` 表无对应行（孤儿命名空间，删号清理不覆盖）
- `storage/mod.rs:4885`：`replace_chapters` 用 `INSERT INTO`（非 OR REPLACE），重复迁移撞 PK，目前靠前置 DELETE 掩盖
- `data/` 下残留 `dictprobe01/02`、`perfprobe01`、`probeai01/02/03/05`、`pagesqa0904`、`shelfqa0904`、`ttstest01` 等历史测试用户目录（非本次审计产生，建议统一清理）

---

## 5. 已确认安全（勿重复审）

- **身份解析无回退**：`resolve_namespace`（`router.rs:8085`）在 secure 下拿不到合法 token 直接 `login_required()`，不存在落 `default`；请求里的 `ns`/`username` 只当配置 key 或管理员特例
- **用户名不可穿越**：`router.rs:790-802` 长度≥5、`^[a-zA-Z0-9]+$`、禁 `default`；`update_user` 不允许改名
- **user_config 参数化**：`storage/mod.rs:4197/4208` 全 `?` 绑定，按 `(token 解析出的 ns, key)` 定位 —— 实测 B 带 `ns=default`/`username=testuser01` 读不到任何 A 的东西
- **整表按 ns 隔离**：`books`(PK `(book_url,user_namespace)`)、`book_sources`、`book_source_cookies`、`book_groups`、`rss_*`、`replace_rules`/`txt_toc_rules`(`ensure_rule_id_owned` 防抢 id)、`reading_stats`、`search_cache`、`book_graph_*`、`http_tts_list`；`storage/mod.rs:6888 test_ns_isolation_same_url` 已覆盖五表
- **管理接口服务端强制**：`check_manager_auth`(`router.rs:7508`)、`require_ai_admin`、`require_*_permission`、`book_limit`/`book_source_limit`、最后一名管理员保护
- **文件管理接口**：`files.rs:61 resolve_secure_path`（组件级 normalize + containment）、`file_home` 对 `__HOME__/__WEBDAV__/__LOCAL_STORE__/__STORAGE__` 分别校验 —— 实测 B 访问 `home=__STORAGE__`→「仅管理员」、`home=/etc`→「非法访问」、`path=../../../`→「路径不存在」
- **OPDS / 导出主链**：`pro_export.rs` 四端点全 `resolve_namespace`+`find_book(ns)`+`list_cached_chapters(ns)`；`opds.rs:202/1689` 带 ns
- **AI**：`ai_chat` 只 `retrieve_scoped(ns,..)`；`ai_settings` 仅管理员可写；key 只回 `keySet`

### 我这一轮的双账号实测清单（A vs B）

| 项目 | A(testuser01) | B(audituser01) | 判定 |
|---|---|---|---|
| 书架 | 6 本 | 0 本 | ✅ 隔离 |
| 书源 | 615（私有 612 + 公共 3） | 3（仅公共） | ✅ 隔离 |
| 书源登录态 Cookie | 有 | `[]` | ✅ 隔离 |
| RSS 源 | 有 | `[]` | ✅ 隔离 |
| 改公共源 | — | 只在自己 ns 生成副本，A 视角不变 | ✅ 覆盖语义正确 |
| 删公共源 | 仍 615 | 只删自己那份 | ✅ |
| WebDAV 备份 | 已开启 | 「未开启webdav功能」 | ✅ 服务端强制 |
| 管理接口（getUsers/addUser/updateUser/deleteUser/setAsDefaultBookSources/backupToMongodb） | 可用 | 全部「仅管理员可执行该操作」 | ✅ |
| `getSystemInfo.isAdmin` | true | false | ✅ |
| 本地文件读（P0-1） | — | **读到 A 的私密 txt** | ❌ |
| `/users` 缓存（P0-3） | 正常 | **换号后仍渲染全量账号** | ❌ |
| localStorage（P0-2） | — | **继承 A 的批注/进度/搜索词** | ❌ |

---

## 6. 修复批次建议

**批次 1（安全，约 30 行）**：P0-1 三处 `find_book` 前置校验 + S6 两处 `disabled` 检查 + S7 `token_map` 清空
**批次 2（前端隔离，约 60 行）**：本地存储键加用户维度（`persist` name 拼用户名 + 登出清 store）+ `logout()` 里 `queryClient.clear()` + `systemInfo`/其余 queryKey 带用户（或登出时 reset）
**批次 3（数据一致性）**：S2/S3/S4/S5 的 ns 过滤、S10 时间比较、S11 配额、S12 目录分 ns
**批次 4（加固）**：S8 恒定时间比较 + `token_map` 形态收敛、S9 版本号 409、S13 token 移出 URL

验收口径建议：批次 1 用本文件 §1 的 curl 复现命令回归（期望「本地书章节不存在」）；批次 2 用 `switch_user_probe.mjs` / `ls_inherit_probe.mjs`（`/home/hehecat/code/.tmp/verify/`）复跑，期望换号后三个数据源均为空。

---

## 7. 待产品决策

1. **批注要不要上云**：现在纯本地，换设备即丢、换号即串、误删不可恢复
2. **个人配置要不要真做同步**：`user_config` 表 + 接口都就绪且已按用户隔离，缺的是前端接线和冲突策略（先落 S9 版本号再做）
3. **`enable_local_store` 是否限制为单用户/共享模式二选一**：现在目录不分 ns

---

## 8. 修复落地记录

§1–§4 的全部条目已修（S12 `__LOCAL_STORE__` 共享目录按 §7-3 留待产品决策，未动）。门禁与验证见下表。

### 8.1 后端

| 编号 | 修法 | 位置 |
|---|---|---|
| P0-1 | 新增 `loc_file_owned_by(storage_dir, ns, book_part)`：路径 canonicalize 后必须 `starts_with(storage_dir/data/<ns>)`，ns 空串按 `default`，canonicalize 失败一律 false。守卫放在任何文件读取之前 | `backend/src/api/router.rs:13424`、`get_book_content_file`/`get_book_toc_file`/`get_book_toc_local` 三处 |
| S1/S3 | 章节读写全程带 ns：`save_chapters_ns`/`list_chapters_ns`/`list_chapters_with_word_count_ns`/`list_chapters_full_ns`；`delete_book`/`delete_local_book`/`migrate_loc_book` 的 DELETE 加 `user_namespace` | `backend/src/storage/mod.rs`、`backend/src/api/router.rs:5952` |
| S1 历史数据 | 新增 `backfill_chapters_ns`：早期 4 列 INSERT 把非 default 用户的章节写进 `'default'`，读路径收紧后会整本读不到；只在「该 url 在整个 books 表里只属一个非 default ns」时归位 | `backend/src/storage/migrate.rs:395` |
| S4 | `bookmarks` 主键 `(book_url, title)` → `(book_url, title, user_namespace)`；SQLite 不支持改主键 → 事务内建表/拷贝/换名，空 ns 归 `default`，`DROP TABLE IF EXISTS bookmarks_new` 兜中断重试 | `backend/src/storage/mod.rs:658`、`backend/src/storage/migrate.rs:327` |
| S5 | 任务注册表键改 `"{ns}\u{1}{book_url}"`；`progress_of`/`cancel` 加 ns；`progress_of_key`/`cancel_key` 也加 ns（不把 ns 拼进客户端可见 taskId）；`load_latest_progress` 两条 SQL 加 ns 过滤 | `backend/src/service/graph/job.rs`、`backend/src/service/cache_job.rs` |
| S6 | OPDS Basic 与 WebDAV Basic 校验 `disabled`（401 统一，不泄露状态） | `backend/src/api/router.rs:8321`、`backend/src/api/webdav.rs:105` |
| S7 | 登出 / 改密同时 `token_map = '[]'` | `backend/src/storage/mod.rs:5186/5418` |
| S8 | `token_map_valid` 只认数字 exp（字符串/对象/数组/null 一律无效）；token 比较复用 `util::constant_time::ct_eq` | `backend/src/model/user.rs:73`、`backend/src/api/router.rs:8130` |
| S9 | 个人配置 `ON CONFLICT ... DO UPDATE ... WHERE excluded.updated_at >= user_config.updated_at` | `backend/src/storage/mod.rs:4257` |
| S10 | 进度写入加 `COALESCE(dur_chapter_time,0) <= ?`；`saveBookProgress` 的 `Ok(0)` 从「未加入书架」报错改成幂等 `ok(null)`（否则多端乱序回写会假报错） | `backend/src/storage/mod.rs:3546`、`backend/src/api/router.rs:9176` |
| S11 | 新增 `ensure_book_quota`，接在 `scan_local_book_dir` / `upload_local_book` 两个导入入口 | `backend/src/api/router.rs:13730` |
| 附带 | `insert_user` 的 INSERT 补 `disabled` 与 `token_map` 两列——`INSERT OR REPLACE` 整行覆盖，缺列会让「创建即停用」失效、让恢复备份清空多端会话 | `backend/src/storage/mod.rs:5026` |

### 8.2 前端

- **本地存储按用户分桶**（P0-2）：新增 `frontend/src/lib/user-scope.ts`——live 键（`reader.settings` 等）始终代表当前用户，登录/登出时整体搬进 `reader.u.<user>.<key>` 桶再搬回目标桶；`EXCLUDED` 排除 `reader.accessToken`/`reader.secureKey`；`reader.u.active` marker 解决 401 路径里 token 已先被清掉、认不出上一个用户的问题。接线在 `frontend/src/stores/auth-store.ts` 的 `login`/`logout`，`hooks/useAuth.ts` 全部走 store，无双重搬家。
- **运行时缓存**（P0-3）：新增 `frontend/src/lib/query-client.ts`，`resetUserRuntime()` = `queryClient.clear()` + 判 live 键存在才 setState 复位（zustand v5 persist 在 storage 无值时会保留内存态）+ rehydrate + `closeChapterCache()`；`lib/chapterCache.ts` 的 IndexedDB 库名带用户名（`reader.chapterCache.<user>`），不再跨号串。
- **token 移出 URL**（S13）：`lib/api-client.ts` 拦截器把 `accessToken` 从 query 改到请求头（后端 `access_token_of` 支持）；`services/ai.ts`、`services/import.ts`、`services/webdav.ts` 三处手写请求同步；`lib/sse.ts` 保持 query（EventSource 无法设头）并加注释。

### 8.3 验证

| 项 | 结果 |
|---|---|
| `cargo fmt --check` / `cargo clippy --all-targets -D warnings` | 无 diff / 零 warning |
| `cargo test --lib --tests` | 854 passed / 0 failed / 3 ignored（新增 7 个隔离测试） |
| `npx tsc -b` / `pnpm lint` / `pnpm test` / `pnpm build` | 0 error / 0 error 48 warning（基线同）/ 55 passed / 成功 |
| P0-1 回归（新二进制 :4599，`/home/hehecat/code/.tmp/verify/isolation_regress.sh`） | **PASS**：B 读 A 的目录「本地书籍源文件不存在」、正文「本地书章节不存在」；A 读自己正文正常；B 调 `getUsers` 被拒；B 带 `ns=default` 读系统层无数据 |
| 生产库迁移演练（1.1 GB `reader.db` 副本起临时实例） | bookmarks 2 行、book_chapters 3575 行、books 6、book_sources 618 全部保留，PK 升为 3 列且 ns 回填为 `testuser01`，`pragma integrity_check` = ok；二次启动不再迁移（幂等） |
| 前端换号 E2E（`/home/hehecat/code/.tmp/verify/user_isolation_e2e.mjs`，CDP 真鼠标） | **PASS** 18 项：A 的搜索词/进度进桶、登出后 live 无残留、B 看不到 A 的数据、B 打开 `/users` 看不到账号列表、B 登出后 A 的桶完好 |

### 8.4 已知残留

1. `token 直换`（不经 login/logout 改 localStorage）无法观测——本次会话内 live 仍是旧用户数据；IndexedDB 因分库不串。
2. 老 `token_map` 非数字 exp 的设备需重新登录一次。
3. S10 使 `durChapterTime=0` 或时间回退的进度不落库（设计意图），发版说明需提一句。
4. `cache_job` 的 `progress_of_key`/`cancel_key` 签名加了 ns，其它分支调用需同步。
5. `enable_local_store` 共享目录（S12）仍不分 ns。
