<div align="center">

# Reader Dev

**自托管 Web 阅读服务 —— 书源搜索 · 本地书仓 · OPDS · WebDAV · 多用户 · 双向缓存**

Rust + Vue 3 实现，legado 语义书源规则引擎。当前主线发布 **v5.2.4**；`legacy` 分支保留 Kotlin v4.0.7 仅维护。

</div>

---

> ⚠️ **历史笔记（v5 时代）**：前端已从 Vue 3 换成 React 19，版本号以 `backend/Cargo.toml` 为准，
> 内置浏览器后端已从 obscura 换成 camoufox。本文其余内容作为设计背景参考，与现状冲突时以代码和 README 为准。

## 功能特性

### 书源与抓取

- **legado 语义规则引擎**：CSS 链式（`@CSS:`/`@@`/`class.x@tag.a@text`/`||`/`&&`/`%%`）、JSONPath（`$.`/`$[`/`{{}}`）、XPath、正则（多段 `##` 替换）、JS（boa 沙箱 + `java.*`/`source.*`/`cookie.*` shim）
- **Android 环境兼容**：补充 `application`（`getSharedPreferences`/文件目录/包名/版本）、`URLEncoder/URLDecoder`、`UUID`、`System`、`java.util.Base64`/`android.util.Base64`、`Log`、`context/activity/app` 等旧书源 Header 脚本常用全局，避免迁移书源因 `ReferenceError` 打不开
- **书级 `@put/@get` 变量**：搜索条目写入的变量贯通详情、目录、正文（`bookUrl`/`tocUrl` 双 key 保存，URL 内嵌 `@get` 拼接）
- **fetch URL 后缀**：`{...}` 附加 js/headers/method/body/bodyJs/charset，搜索/目录/正文/详情/媒体统一支持
- **init / preUpdateJs**：详情、目录、正文解析前的 JS 预处理
- **JS 能力**：`java.get/put/post/ajax/getCookie/timeFormat/timeFormatUTC`、`cookie.getCookie/getKey/setCookie/replaceCookie/removeCookie/clearCookie`、全局 `gzip`（GZip→base64）、AES、`getWbiEnc`、`Reload` 等
- **书源管理**：增删改、启停、分组、失效检测、本地/远程导入导出（导入前预览勾选、排序——全选/反选/仅新增/重复标记）、订阅源（订阅即自动刷新记录，删除订阅即停止刷新）、登录流（`loginUrl` + 验证码）、手动 Cookie；普通用户删除/停用系统书源只对本人生成私有覆盖；管理员默认使用本人账号，可手动进入 `default` 系统配置层编辑对所有用户生效的公用数据
- **书源调试**：搜索/目录/正文逐规则逐步日志（SSE 流式）
- **换源**：阅读中直接换源，弹层展示书源作者、最新章节、当前章各源末尾预览（宽度自适应截断）；并发多源搜索 + 书名过滤去重 + 书源名过滤 + 手动刷新，切换保留当前章进度

### 反检测（进程内——obscura）

- obscura stealth 浏览器内嵌（BoringSSL TLS 指纹模拟/反检测/追踪器拦截），无系统浏览器依赖
- Cloudflare 质询检测 → 浏览器求解 → cookie 合并 → 原请求重试；Turnstile iframe 点击；登录滑块 JS 拖拽
- camoufox 强质询兜底 + 可选代理（`READER_OBSCURA_PROXY`）
- 按用户独立实例、闲置回收、求解前清 cookie 防跨用户泄漏
- **浏览器优先默认开启**：所有 GET 抓取默认先经内置 obscura 导航，最大限度减少验证码/WAF 拦截；直连仅在浏览器不可用或求解失败时自动降级，普通抓取不受缺浏览器影响（`READER_BROWSER_FIRST=0` 关闭恢复直连优先；`READER_BROWSER_FALLBACK_DISABLE=1` 关闭反爬兜底重试）

### 阅读体验

- 字体、行距/段距/字重/宽度/字距/缩进/对齐、主题（亮/暗/暖/自定义/跟随系统）、翻页模式（滚动/滑动/仿真）、自动阅读、亮度、键盘翻页、Wake Lock
- 全局简繁转换、12 项阅读偏好云端同步、每本书独立配置
- **双向章节缓存**：缓存到服务器（多端共用）或拉取到本机（IndexedDB 离线回读）；支持当前章、至末尾、全本、指定范围，目录页可单章缓存，阅读页可缓存指定章节或全本；读取时本机优先，未命中自动走服务器缓存/书源
- **正文 HTML 清洗**：`@html` 书源的 `<br>/<p>/<li>` 转为换行、`&nbsp;/&amp;/数字实体` 解码、其余标签剥离，迁移缓存不再显示标签或实体原文
- **编码探测**：chardetng 统计式识别 GBK/Big5/Shift_JIS/EUC-JP 等非 UTF-8 网页与本地书，乱码页自动按正确编码解码
- 整书缓存（SSE 进度）、正文缓存、全书搜索、阅读统计、章节字数
- 非文本书籍：音频 / 视频 / 漫画（图片逐页）/ 文件书
- 搜索到的书不入架直接阅读；退出时项目风格挽留弹窗可一键入架（补齐封面、作者、章节目录），点「暂不加入」直接返回
- 搜索可按书源分组过滤（胶囊切换，切换后立即重搜）；结果按书源折叠分组展示
- 书架卡片显示上次阅读章节名与未读更新数（`最新章总数 - 已读进度`），未读角标提示

### 本地书（9 格式）

EPUB · TXT · MOBI · AZW3 · PDF · FB2 · DOCX · CBZ（漫画）· UMD —— 上传导入（含预览）、目录、正文、重扫、全书搜索

- 双轨同步仓：文件变更自动导入/重扫；DB 书自动生成 epub 镜像
- EPUB 导入按 OPF manifest/nav/NCX 解析，章节顺序与媒体类型完整保留
- TXT 分章对齐 legacy 内置 18 条目录规则（含启用状态，`importDefaultTxtTocRules` 可全量导入）；文件名自动解析书名/作者（`《书名》`、`书名 作者：xx`、`书名 by xx`）
- CBZ 读取 `ComicInfo.xml` 的 Title/Writer 作为书名/作者，zip 首图自动作封面；漫画页按自然序分章（`page2 < page10`）
- **本地目录自动加入书架**：把书籍文件放进监听目录即可自动导入，无需上传——`{READER_APP_WORKDIR}/storage/data/{用户名}/books/`（非 secure 模式为 `default`），或 `READER_LOCAL_BOOK_DIR` 指向的任意挂载目录（归属 `default` 命名空间）；文件修改自动重扫、删除保留书籍；自动监听支持 epub/txt/mobi/azw3/pdf/fb2/docx/cbz/umd；目录扫描默认每秒最多 20 次文件系统操作（`READER_DIR_SCAN_RPS` 可调，0 不限速）——网盘目录挂载成本地目录时避免瞬时 readdir/stat 触发网盘风控
- **文件页直接导入**：书仓 / 用户数据 / WebDAV 中已有的书籍文件可单选「导入书架」、多选批量导入，或对当前目录使用「导入目录」递归扫描，服务端直接解析入库（同一路径重复导入幂等覆盖）

### 导出与备份

- 导出：TXT（编码可选）/ EPUB（内嵌中文字体 + 完整目录导航）/ HTML
- 备份：WebDAV / zip；恢复（zip/WebDAV——9 类目幂等，兼容 legacy 备份）
- 数据迁移：legacy（Kotlin）JSON → SQLite 全量自动迁移（书/书源/书签/规则/RSS/分组/用户配置——原文件保留可回退）
- **迁移与保存修复**：书架迁移 SQL 与 `upsert_book` 均补写 `toc_url`，避免目录地址被默认空值清空；启动时从 `raw_json` 批量回填历史迁移缺失的 `toc_url`，旧迁移书无需逐本换源

### OPDS & WebDAV

- OPDS 1.2 + 2.0 + PSE（进度保存）；独立 OPDS 账号或系统账号/token 三路认证
- WebDAV 服务器（全方法 + 路径穿越防护）

### 多用户与安全

- argon2id 密码哈希（PHC——登录自动升级）、token 随机化（uuid v4、多设备上限 5）、登录限流（直连 IP）
- 命名空间隔离、路径穿越防护、SSRF 防护、图片缓存按用户隔离、SQL 全参数化
- secure 多用户：首个注册用户自动成为管理员；管理员默认使用本人账号（个人书架/书源/进度等），从顶栏「用户」入口管理账号，并可手动进入 default 系统配置层（编辑公用书源等）；普通用户覆盖系统配置只对自己生效，最后一名管理员不可撤销/删除
- 注册默认权限全开（WebDAV/本地书仓/书源/RSS），书源上限 80000、书籍上限 5000；旧库启动时一次性纠正仍等于旧错误默认值的用户，人工改过的不动
- 服务监控页（内存/CPU/请求/在线/书源成功率）、日志

### 前端

Vue 3 + Vite + Element Plus，极简风格、响应式（含移动端竖屏阅读适配）、深色主题、虚拟滚动、SSE 流式、PWA、i18n（中/英）、命令面板（Ctrl+K）；书源管理页订阅配置与多选工具条置于顶部，工具栏自动换行不溢出

---

## 快速开始

### Docker（推荐）

```bash
docker pull ghcr.io/warpdotsys/reader-dev:latest
docker run -d --name reader-dev -p 8080:8080 \
  -v "$PWD/data:/storage" \
  -e READER_APP_WORKDIR=/storage \
  -e READER_APP_SECURE=true \
  ghcr.io/warpdotsys/reader-dev:latest
```

镜像内置 obscura（stealth）+ camoufox + python，反检测能力开箱即用。

### 直接运行（Linux/Windows/macOS）

```bash
# 后端
cargo build --release
# 前端
cd web-ui && npm install && npm run build && cd ..
# 运行
export READER_APP_WORKDIR="$PWD/data"
export READER_APP_SECURE=true
./target/release/reader-dev
```

浏览器打开 `http://localhost:8080`。

> 本机直跑时反检测浏览器需下载 [obscura](https://github.com/h4ckf0r0day/obscura) stealth 构建，放同目录或配置 `READER_OBSCURA_BIN`。无 obscura 时浏览器优先自动回退直连（普通抓取不受影响）；只有遇到人机验证/质询的站点才需要浏览器。

---

## 从 legacy（Kotlin）Docker 迁移

### 1panel 升级（面板更新镜像）

> v4.x（Kotlin）→ v5.x（Rust）镜像结构完全不同。旧镜像启动命令为 `java -jar /app/bin/reader.jar` + Entrypoint `/sbin/tini`；v5 为 `reader-dev` + `/usr/bin/tini`。
> **1panel 升级会用旧容器的启动配置创建新容器，直接启动会报 `exec: "/sbin/tini": no such file or directory` 或找不到 java——必须改一次配置：**

1. **1panel → 容器 → reader → 编辑**
2. **启动命令（Command）：填 `reader-dev`**。旧值 `java -jar /app/bin/reader.jar` 在 v5 不存在；**不能清空**——tini 没有命令参数会直接退出导致反复重启
3. **入口点（Entrypoint）：填 `/usr/bin/tini --`**（`/sbin/tini` 已做符号链接兼容，但用新路径最稳）
4. 保存 → 重启容器
5. 首次启动自动迁移（JSON → SQLite 全量——控制台/日志见迁移横幅与「JSON→SQLite 迁移完成」；大书架备份阶段无日志属正常，请勿中断）

> 数据卷挂载路径无需改（保持 `/storage` 或原路径）；迁移完成后旧 JSON 保留在 `storage/backup-before-migrate-*/` 可回退。

### 只换镜像（docker run，数据零改动）

```bash
# 1. 备份（保险）
docker exec <旧容器> tar czf /tmp/backup.tar.gz /storage
docker cp <旧容器>:/tmp/backup.tar.gz .

# 2. 停旧容器（数据卷不动）
docker stop <旧容器>

# 3. 起新容器（同一数据卷——挂载路径保持）
docker run -d --name reader-dev-rust \
  -v <同一数据卷>:/storage \
  -p 8080:8080 \
  -e READER_APP_WORKDIR=/storage \
  -e READER_APP_SECURE=true \
  ghcr.io/warpdotsys/reader-dev:latest

# 4. 启动时自动迁移（JSON → SQLite 全量——日志见「JSON→SQLite 迁移完成」）
#    原 JSON 文件保留（可回退）
```

### 直接跑二进制

```bash
# release 资产 reader-dev-linux-x64-musl（静态——任何发行版）
READER_APP_WORKDIR=/storage READER_APP_SECURE=true ./reader-dev-linux-x64-musl
```

### 迁移覆盖

用户 / 书架（含进度）/ 书源 / RSS / 书签 / 替换规则 / TXT 目录规则 / HttpTTS / 分组 / 用户配置——全量，raw_json 保底。

---

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `READER_SERVER_PORT` | `8080` | 端口 |
| `READER_APP_WORKDIR` | 当前目录 | 数据目录（storage/ 下） |
| `READER_APP_WEB_ROOT` | `web-ui/dist` | 前端静态根 |
| `READER_APP_SECURE` | 关 | 多用户安全模式 |
| `READER_APP_SECUREKEY` | 空 | 匿名默认用户密钥（secure 模式测试用） |
| `READER_APP_MINUSERPASSWORDLENGTH` | `8` | 密码最小长度 |
| `READER_APP_INVITECODE` | 空 | 注册邀请码 |
| `READER_APP_DEFAULTUSERENABLEWEBDAV` | `true` | 新用户默认 WebDAV 权限 |
| `READER_APP_DEFAULTUSERENABLELOCALSTORE` | `true` | 新用户默认本地书仓权限 |
| `READER_APP_DEFAULTUSERENABLEBOOKSOURCE` | `true` | 新用户默认书源权限 |
| `READER_APP_DEFAULTUSERENABLERSSSOURCE` | `true` | 新用户默认 RSS 权限 |
| `READER_APP_DEFAULTUSERBOOKSOURCELIMIT` | `80000` | 新用户默认书源上限 |
| `READER_APP_DEFAULTUSERBOOKLIMIT` | `5000` | 新用户默认书籍上限 |
| `READER_OBSCURA_BIN` | 自动探测 | obscura 可执行文件路径 |
| `READER_OBSCURA_URL` | 空 | 连接既有 obscura CDP 服务 |
| `READER_OBSCURA_PROXY` | 空 | obscura 代理（如 socks5://127.0.0.1:1080） |
| `READER_CAMOUFOX_URL` | 空 | camoufox 求解后端地址 |
| `READER_BROWSER_FIRST` | `1` | 浏览器优先（所有 GET 先经 obscura；`0`/`false`/`off` 关闭） |
| `READER_BROWSER_FALLBACK_DISABLE` | `0` | 关闭直连失败/反爬特征后的浏览器兜底 |
| `READER_UPLOAD_MAX_MB` | `100` | 上传上限 |
| `READER_IMAGE_CACHE_MB` | `512` | 图片代理磁盘缓存上限 |
| `READER_TOKEN_TTL_DAYS` | `30` | token 过期天数 |
| `READER_DB_BACKUP` | `1` | 启动时 DB 快照备份 |
| `READER_AUTO_BACKUP_HOUR` | `3` | 每日自动备份小时 |
| `READER_LOCAL_BOOK_DIR` | 空 | 本地书监听目录 |
| `READER_DIR_SCAN_RPS` | `20` | 目录扫描每秒文件系统操作上限（0 不限速，最大 500） |
| `READER_LOG_DIR` | 空 | 日志目录（按大小轮转） |

---

## 开发

```bash
cargo test          # 612 个 lib 单测 + 集成测试（规则引擎/格式解析/obscura/CF 质询/WebDAV/迁移回填）
cd web-ui && npm run build   # 前端（vue-tsc 类型检查 + vite）
```

### 结构

```
src/
├── api/          # axum 路由
├── model/        # 数据模型
├── parser/       # 规则引擎（css_chain/js/rule/xpath/jsonpath）
├── service/      # 业务（browser(obscura CDP)/crawler/search/explore/local_book/opds/cache_job/...）
├── storage/      # SQLite（迁移/CRUD/缓存/统计）
└── util/         # password(argon2)/regex/md5/...
web-ui/src/       # Vue3 视图/组件/api/utils
scripts/          # 审计/测试工具（api-scan/mock 站点等）
docs/             # SECURITY/ARCHITECTURE/ROADMAP/FRONTEND
```

---

## 文档

- `docs/SECURITY.md` —— 安全设计（argon2/SSRF/隔离/限流）
- `docs/ARCHITECTURE.md` —— 架构（obscura 内嵌/分层）
- `docs/ROADMAP.md` —— 路线图与待办
- `docs/legado-ref/ruleHelp.md` —— 规则参考

## 版本与分支

| 分支 | 说明 |
|---|---|
| `master` | **Rust 版（当前）——v5.2.4** |
| `legacy` | Kotlin 稳定版（ghcr v4.x） |

- 发布：GitHub Releases（`reader-dev-linux-x64-musl` 静态二进制 + `reader-dev-linux-x64.zip` + `reader-dev-windows-x64.exe`）与 Docker 镜像（`ghcr.io/warpdotsys/reader-dev:latest` / `:v5.2.4`，Docker Hub 同步）
- Linux 与 Windows 构建并行；Linux 产物为 musl 静态链接（无 glibc 依赖，zip 内含可执行文件与前端资源，非空白压缩包）
- v5.0.0/v5.0.1 为 Rust 重构早期发布；v5.0.2 未单独发布（功能并入 v5.0.3）；v5.0.4 起 Linux/Windows 构建分离并行；v5.0.5 补齐用户管理/权限隔离/书源管理 UI；v5.0.6 增加双向章节缓存、迁移 `toc_url` 回填、正文 HTML 清洗、Android `application` 兼容；v5.0.7 修复范围缓存 JSON 数值参数；v5.0.8 管理员命名空间与 default 系统配置层分离：管理员默认本人账号，显式进入 default 编辑公用数据，default 历史个人数据自动回迁本人；v5.0.9 完成 legacy 全量逐文件审计（640 文件，审计文档 `docs/legacy-parity/`），补齐默认 TXT 目录规则、本地文件名书名/作者解析、CBZ ComicInfo 元数据与封面；v5.1.0 完成 legacy Web UI 批次：simple-web 搜索详情弹窗/阅读页换源/RSS 分类分页、14 张内置阅读背景图库、替换规则批量删除与 JSON 导入导出、RSS 源编辑与 JSON 导入、书源订阅批量删除（移除无意义禁用语义）、阅读页详情入口与追更开关；v5.2.0 增加阅读中换源（作者/最新章/当前章末尾预览）与规则引擎修复（JS 搜索 URL、相对 URL、URL/URLSearchParams、书源 jsLib/variable 全局注入、data URI），chardetng 统计式编码探测，内置反检测浏览器默认兜底，Docker 构建分层复用与移动端宽度自适应，另补齐正文 HTML 换行清洗、本地书 `type`/`toc_url` 持久化、HTTP 重试/CA/代理、正则编译缓存确定化、书源 Cookie 管理、多分组批量接口、离线书架缓存、图片代理、SW 强制更新、quickKey/点击区域/切章动画/章节超时、自定义字体、文本/JSON 文件编辑、书源规则符号快捷栏、搜索链接直开详情、精确搜书等 legacy 对齐项；v5.2.1 修复 MOBI/AZW3 未知编码中文乱码（PalmDoc/Huffman 原始字节解压 + chardetng 编码探测）；v5.2.2 清理 KindleMOBI 记录尾部 trailing/multibyte 附加数据并修正 PalmDoc 重叠回引展开，修复 4KB 边界后中文乱码与残留 HTML，《盗墓笔记重启·极海听雷》样本正文验证通过；v5.2.3 书源导入预览选择/排序、按书源分组搜索、书仓目录直接扫描导入书架、书架已读章节与未读更新数、正文 script 泄漏清洗、`java.createSymmetricCrypto` 对称解密、暂不加入可返回、移动端竖屏适配；v5.2.4 搜索并发提升（多源 24 / SSE 48）、内置反检测浏览器增强（stealth 指纹补齐 + 反爬域名自动优先）、失效书源检测超时修复（96 并发 + 900s 前端超时）、书架密度按钮/悬浮简介层叠修复、书源管理/文件页/设置页移动端布局修复、正文无换行智能分句

## 赞助

| 网络 | 币种 | 地址 |
|---|---|---|
| Arbitrum | USDC | `0x0B704AcC2EdD28DdaE80e03f1a98e2cD00B0B5ae` |
| Ethereum | USDT | `0x0B704AcC2EdD28DdaE80e03f1a98e2cD00B0B5ae` |
| Arbitrum | USDC.e | `0x0B704AcC2EdD28DdaE80e03f1a98e2cD00B0B5ae` |

> 地址已通过 EIP-55 校验。转账前请核对网络与币种。

## License

[GNU General Public License v3.0](LICENSE) (GPL-3.0)
