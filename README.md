<div align="center">

# reader

**自托管在线阅读服务 —— 多源搜索 · 书架 · 阅读器 · 换源 · 多级缓存 · TTS 朗读**

Rust(warp) 后端 + React 前端, 单一仓库 · GitHub Actions 构建 · GHCR 分发
legado 语义书源规则引擎

</div>

---

## 目录

- [五分钟上手](#五分钟上手)
- [功能特性](#功能特性)
- [配置详解](#配置详解)
- [可选组件](#可选组件)
- [故障排查](#故障排查)
- [升级与回滚](#升级与回滚)
- [性能实测](#性能实测)
- [界面](#界面)
- [开发须知](#开发须知)
- [致谢](#致谢)

---

## 五分钟上手

### 0. 准备

- 一台能跑 Docker 的机器(Linux//macOS/Windows+WSL 均可), 磁盘预留 3GB(镜像约 2GB + 数据)
- 无需公网 IP、无需域名、无需反代 — 容器单端口直服前端与全部 API
- 无需数据库服务 — 内置 SQLite 单文件

### 1. 启动(推荐 compose)

```bash
git clone https://github.com/hehecat/reader.git
cd reader/deploy
docker compose up -d          # 默认单用户模式, 无需 .env
```

打开 `http://<机器IP>:8080` 直接注册/登录。
> 多用户: `cp .env.example .env` 设 `READER_APP_SECURE=true` 与邀请码/管理密码后 `docker compose up -d`。
> 端口占用: `.env` 设 `READER_PORT=<其他>` 或在环境变量里传。

> 不用 compose 也行:
> ```bash
> docker run -d --name reader -p 8080:8080 \
>   -v "$PWD/data:/storage/storage" -e READER_APP_WORKDIR=/storage \
>   ghcr.io/hehecat/reader:latest
> ```

### 2. 账号

- **默认单用户模式**: 登录页直接注册/登录即建本地账号, 数据存 `default` 命名空间, 无邀请码
- 多人共用一台: `.env` 设 `READER_APP_SECURE=true` + `READER_APP_INVITECODE=<邀请码>` + `READER_APP_SECUREKEY=<管理密码>`, 首个注册用户为管理员

### 3. 导入书源(**必做, 否则搜索无结果**)

服务不自带任何书源(版权考虑)。书源 = legado/阅读 兼容的 JSON 规则文件:

1. 侧边栏「书源」→「导入」→ 粘贴 JSON 或填远程 URL
2. 社区书源合集可自行搜索「legado 书源」获取; 导入后在书源页启用/分组/调试

### 4. 搜索与阅读

- 「搜索」输入书名 → 多源 SSE 流式出结果(进度可见, 随时停止/续搜)
- 点结果卡 → 详情/目录 → 点章节**直接阅读, 不自动加入书架**(想收藏手动点「加入书架」)
- 书架: 分组/进度条/未读; 阅读器: 主题/翻页/批注/书签/TTS/目录/进度同步

完成。其余都是可选。

---

## 功能特性

### 搜索与书源治理

- **多源 SSE 流式搜索**: 逐源完成逐批推送, 进度可见(已找到 N 本 · 书源 x/y), 随时停止/续搜
- **置信度排序与死源跳过**: 每源记录搜索/目录/正文成功率、延迟、简介丰富度; 置信度 = 质量分 × 速度因子; 连败源 6h 跳过到期放行探针(`all=1` 强制全搜)
- **相关性收口**: 聚合前过滤「书名/作者/简介都不含关键词全部 token」的结果, 不混站内推荐垃圾
- **隐藏无章节结果**(设置页开关): 后台并发探针校验目录, 0 章结果不展示; 缓存 7 天 + 静默重验
- **书源管理**: 增删改/启停/分组/导入导出; 源工作台逐规则调试(搜索/目录/正文 SSE 流式日志); 失效源标记与清理
- **换源**: 书架书走后端换源(保留当前章进度); 未入架书纯前端切换(详情/目录/阅读全跟新源, 不动书架)
- **legado 语义规则引擎**: CSS/JSONPath/XPath/正则/JS 沙箱, `@put/@get` 书级变量贯通搜索→详情→目录→正文

### 阅读

- 主题(亮/暗/护眼/跟随系统)、字号/行距/段距/宽度、沉浸模式、自动滚动、键盘翻页
- **TTS 朗读**(可选 TTS 网关, 见下); 书签 + 划选批注; 章末块导航
- 阅读进度多端同步; 书架卡片显示阅读进度与未读
- 正文/目录/书籍信息多级缓存, 未命中自动抓取并回写

### 书架与缓存

- 书架分组、网格/列表布局、自定义封面长缓存
- **缓存层级**: 目录(24h, 前置命中毫秒级) · 正文(永久, md5(chapterUrl) 键, 多端共用) · 书籍信息(24h) · 搜索批次
- 搜索列表会话快照: 预览/阅读返回, 原列表与书源游标即时恢复, 可续搜

### 本地书与导出

- 本地书监听目录自动导入(epub/txt/mobi/azw3/pdf/fb2/docx/cbz/umd), 手动上传导入
- 导出 TXT/EPUB(内嵌中文字体 + 完整目录导航)
- WebDAV 备份/恢复(幂等, 兼容 legacy 备份)

### 多用户与安全

- 命名空间隔离(书架/书源/进度/缓存按用户)
- secure 模式: 邀请码注册 + 管理密码; token 鉴权; SSRF 防护开关
- 验证码/登录墙: 容器内 camoufox(Firefox 内核)按需 spawn 求解(可外置)

### 前端

- React + Vite + Tailwind, 响应式(移动端竖屏阅读适配), 深色主题
- **字体零外依赖**: Noto Serif/Sans SC 606 个 unicode-range 子集本地自托管, 按需加载
- 路由级代码切分; SSE 流式 UI; 搜索历史/快照恢复

---

## 配置详解

### 数据目录

compose 默认挂 `deploy/storage/`(容器内 `/storage/storage`), 内含:

- `reader.db` — SQLite 主库(书架/书源/进度/缓存/用户)
- `reader.db.bak-*` — 启动前自动快照(保留 5 份, `READER_DB_BACKUP=0` 关)
- 封面/本地书/WebDAV 数据

### 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `READER_APP_WORKDIR` | 当前目录 | 数据根(其 `storage/` 子目录存库与资产) |
| `READER_APP_SECURE` | false | 多用户 secure 模式开关 |
| `READER_APP_INVITECODE` | - | secure 模式注册邀请码 |
| `READER_APP_SECUREKEY` | - | secure 模式管理密码 |
| `READER_SERVER_PORT` | 8080 | 容器监听端口(compose 用 `READER_PORT` 映射宿主端口) |
| `READER_TOC_CACHE_TTL_MS` | 86400000 | 目录缓存 TTL(24h) |
| `READER_DB_BACKUP` | 1 | 启动前 db 快照开关(0 关) |
| `READER_AUTO_BACKUP_HOUR` | 3 | 每日 WebDAV zip 备份时刻(0-23) |
| `READER_HTTP_RETRIES` | 2 | 搜索/抓取单源重试次数(compose 默认 1, 死源排空更快) |
| `SSRF_ALLOW_PRIVATE` | false | 允许抓内网地址(本地书同步/内网 TTS 需要) |
| `READER_CAMOUFOX_URL` | - | 外置 camoufox 服务地址; 缺省容器内自 spawn |
| `READER_BROWSER_FIRST` | 1 | 抓取优先经浏览器反检测; 0 恢复直连优先 |

**运行时可调(无需重启/重建)**: 设置页「搜索 → 单源搜索超时」(3-60s, 即时保存, 随每次搜索请求下发)。

### 反代(通常不需要)

默认**不需要任何反代**: 容器单端口直服前端与全部 API, 局域网/内网穿透直接用。
仅当你需要 HTTPS(公网访问建议)时才自备反代, 届时注意三点:

1. SSE 路径(`/reader3/*SSE`)不压缩、关代理缓冲, 否则搜索流式失效
2. **HTML 永不缓存**: `/` 与 `/reader3/*` 必须 no-cache/不缓存; 只允许缓存 `/static/*`(前端哈希产物, 容器已发 immutable); `/assets/*` 是用户封面存储路由, 勿加 immutable。给 HTML 加缓存会导致旧 index.html 引用旧 chunk 哈希 → 整页 404(常见事故)
3. 参考 `deploy/Caddyfile.example`

容器自身已发正确缓存头(HTML no-cache + Surrogate-Control: no-store; /static/* immutable), 无代理部署天然自洽。

---

## 可选组件

### TTS 网关(朗读)

阅读器 TTS 走独立网关(单文件 Python, `deploy/tts-gateway.py`):

```bash
pip install edge-tts
python3 tts-gateway.py        # 默认 :9912, 配置 ~/.local/share/tts-gateway-config.json
```

- 引擎: edge-tts(免费云神经音, 中文音色内置筛选); 可配腾讯/阿里/火山等密钥云
- 前端「设置 → 阅读偏好 → TTS」网关地址填 `http://<运行网关的机器IP>:9912`(无反代时直连); 若配了反代也可留空走同源 `/tts-gateway`
- 不装网关: 朗读自动降级为浏览器系统语音

### camoufox(反爬求解)

镜像已内置(pip 包 + Firefox 二进制), 遇验证码/Cloudflare 质询自动 spawn 求解, 无需配置。源站普遍直连可达时可设 `READER_BROWSER_FIRST=0` 提速。

---

## 故障排查

| 症状 | 原因与处理 |
|---|---|
| 搜索永远「已找到 0 本」 | 反代压缩/缓冲了 SSE(见反代两条硬要求); 或无启用书源 |
| 搜索首结果慢(>30s) | 持有该书的源站慢; 设置页调低「单源搜索超时」快速跳过, 或等续搜扫到快源 |
| 首屏字体不对 | 字体本地自托管, 无外依赖; 若缺失回退系统字体栈, 检查 `/fonts/noto.css` 可达 |
| 端口占用 | compose `.env` 设 `READER_PORT=<其他>` |
| 权限报错 | 确保挂载目录可写(compose 自动创建 `deploy/storage/`) |
| 某源搜索/目录空 | 源规则老化(站点改版); 书源页「失效」标记/清理, 或工作台调试修规则 |
| 多设备进度不同步 | 进度按用户命名空间同步, 确认同账号登录 |

---

## 升级与回滚

```bash
cd deploy && docker compose pull && docker compose up -d     # 升级
docker compose up -d reader@sha-<旧sha>                      # 回滚(镜像标签含 sha)
```

镜像标签: `latest`(main) · `main` · `sha-<full>` · semver(tag)。数据在挂载目录, 升级不丢; 启动前自动快照可再退一层。

自建镜像(不依赖 GHCR): 仓库根 `docker build -t reader .`(多阶段: pnpm → cargo → camoufox; 后端编译需 `RUSTFLAGS='--cfg reqwest_unstable'`, Dockerfile 已内置)。

---

## 性能实测

作者机器(4 核)直连后端, 缓存命中路径, median of 3:

| 功能点 | 时延 |
|---|---|
| 书架/分组/书签 | 2-5ms |
| 详情/目录/正文(缓存命中) | 4-13ms |
| 书源全量/轻量/统计 | 100/50/18ms |
| 搜索首结果(缓存词/新词) | ~1.7s |
| 页面加载(书架/搜索/书源/书海) | 1.0-1.7s |
| 外部请求 | 0(字体/资源全本地) |

---

## 界面

一张海报看全貌(上排 PC 1440×900 · 下排移动 390×844):

![reader 界面海报](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/poster.png)

### PC 端

| 书架(网格/分组/未读角标) | 多源搜索(SSE 流式·关键词高亮) |
|---|---|
| ![pc-shelf](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/pc-shelf.png) | ![pc-search](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/pc-search.png) |
| **详情与章节目录(多源徽标·换源)** | **阅读器(主题·批注·TTS·沉浸)** |
| ![pc-detail](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/pc-detail.png) | ![pc-reader](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/pc-reader.png) |

书源治理页(置信度/成功率/延迟):

![pc-sources](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/pc-sources.png)

### 移动端

| 书架 | 搜索 | 详情 | 阅读器 |
|---|---|---|---|
| ![m-shelf](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/mobile-shelf.png) | ![m-search](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/mobile-search.png) | ![m-detail](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/mobile-detail.png) | ![m-reader](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/mobile-reader.png) |

---

## 开发须知

- 后端编译**必须**带 `RUSTFLAGS='--cfg reqwest_unstable'`(reqwest http3 实验特性; `.cargo/config.toml` 已写但部分环境不拾取)
- 前端包管理 pnpm(lockfile v9); 注意 `bookSource`(单源搜索/正文/探索) 与 `bookSourceUrl`(SSE 单源) 参数名差异
- SSE 相关改动务必真机验证流式(压缩/缓冲是历史事故高发区)
- 书源规则调试用前端「书源工作台」, 逐规则 SSE 日志
- push `main`/tag → Actions 构建镜像推 GHCR(GHA 层缓存, 增量约 2 分钟)

---

## 致谢

- [legado](https://github.com/gedoor/legado): 书源规则语义与生态
- [reader(warpdotsys)](https://github.com/warpdotsys/reader-dev): Rust 重写路线参考
- [hectorqin/reader](https://github.com/hectorqin/reader): legacy 服务器版起点
