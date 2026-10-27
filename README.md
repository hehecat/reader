<div align="center">

# reader

**自托管在线阅读服务 —— 多源搜索 · 书架 · 阅读器 · 换源 · 多级缓存 · TTS 朗读**

Rust(axum) + React · 单容器 · SQLite 单文件 · 数据全部留在自己机器上

[![ci](https://github.com/hehecat/reader/actions/workflows/ci.yml/badge.svg)](https://github.com/hehecat/reader/actions/workflows/ci.yml)

[快速开始](#快速开始) · [功能](#功能) · [配置](docs/configuration.md) · [运维](docs/operations.md) · [界面](docs/screenshots.md)

</div>

![reader 界面海报](https://cdn.jsdelivr.net/gh/hehecat/reader@main/docs/images/poster.png)

书源在你手里，书架在你手里，进度也在你手里。

reader 把 legado 的书源规则搬进一个 Rust 单容器：多源并发搜索、换源、整书缓存、离线词典、TTS 朗读，全部跑在自己的机器上，不依赖第三方账号或云服务。对外只有一个端口，反代、域名、公网 IP 都不是必需的。

## 快速开始

```bash
git clone https://github.com/hehecat/reader.git
cd reader/deploy
docker compose up -d
```

打开 `http://<机器IP>:8080`，注册即可用（默认单用户模式，无需 `.env`）。

1. **导入书源（必做）**：侧边栏「书源」→「导入」，粘贴 legado 书源 JSON 或填远程地址。仓库不自带任何书源（版权考虑）。
2. **搜索**：输入书名，多源 SSE 流式出结果，随时可以停。
3. **阅读**：点结果直接读，不自动进书架；想收藏再点「加入书架」。

不用 compose 也行：

```bash
docker run -d --name reader -p 8080:8080 \
  -v "$PWD/data:/storage/storage" -e READER_APP_WORKDIR=/storage \
  ghcr.io/hehecat/reader:latest
```

多用户：`cp .env.example .env`，设 `READER_APP_SECURE=true` 与 `READER_APP_INVITECODE=<邀请码>`，再 `docker compose up -d`。

## 功能

- **多源 SSE 流式搜索**：逐源推送、进度可见；置信度排序、6h 死源跳过、相关性收口，可隐藏 0 章结果
- **legado 规则引擎**：CSS / JSONPath / XPath / 正则 / JS 沙箱，`@put/@get` 书级变量贯通搜索→详情→目录→正文
- **书源治理**：增删改查、启停分组、导入导出、逐规则 SSE 调试工作台、失效源标记
- **阅读器**：主题/字号/行距/沉浸模式、键盘翻页、书签与批注、进度多端同步
- **书架**：分组、网格/列表、未读角标；搜索列表会话快照，返回即恢复、可续搜
- **换源**：书架书走后端换源并保留进度；未入架书纯前端切换，不动书架
- **多级缓存**：目录 24h、正文永久、书籍信息 24h；缓存命中路径 4–13ms；单章可重新拉取，缓存可按目录/正文分类清理
- **本地书**：epub/txt/mobi/azw3/pdf/fb2/docx/cbz/umd 自动导入，导出 TXT/EPUB
- **离线词典**：ECDICT 英汉 77 万词条 + 新华字典，划词即查，零网络零密钥
- **TTS 朗读**：接自建网关（edge-tts 或云厂商），不装则降级浏览器语音
- **多用户与安全**：命名空间隔离、邀请码注册、token 鉴权、SSRF 防护
- **前端零外依赖**：Noto Serif/Sans SC 按 unicode-range 切 606 个子集自托管，字体不走 CDN
- **AI 阅读助手**（可选）：只检索本机已缓存章节，答案标注章节出处，不编造

## 配置

最常用的几个（完整列表、反代要求、可选组件见 [docs/configuration.md](docs/configuration.md)）：

| 变量 | 默认 | 说明 |
|---|---|---|
| `READER_SERVER_PORT` | 8080 | 容器监听端口（compose 用 `READER_PORT` 映射宿主端口） |
| `READER_APP_SECURE` | false | 多用户模式开关 |
| `READER_APP_INVITECODE` | - | 多用户注册邀请码 |
| `READER_AI_BASE_URL` / `READER_AI_API_KEY` | 空 | OpenAI 兼容端点，两者都非空才启用 AI 面板 |
| `READER_BROWSER_FIRST` | 1 | 抓取优先走浏览器反检测（compose 默认 0） |

数据都在挂载目录的 `storage/` 下：`reader.db` 加封面、本地书、WebDAV 备份；启动前自动快照保留 5 份。

## 文档

- [配置与部署](docs/configuration.md) — 数据目录、全部环境变量、反代、TTS / 词典 / camoufox
- [运维](docs/operations.md) — 故障排查、升级回滚、性能基线
- [界面截图](docs/screenshots.md)
- [开发指南](AGENTS.md) — 仓库结构、构建与测试、开发纪律
- [后端笔记](backend/BACKEND-NOTES.md) — v5 时代的设计背景（部分内容已过时）

## 致谢

- [legado](https://github.com/gedoor/legado)：书源规则语义与生态
- [reader(warpdotsys)](https://github.com/warpdotsys/reader-dev)：Rust 重写路线参考
- [hectorqin/reader](https://github.com/hectorqin/reader)：legacy 服务器版起点
