# 配置与部署

本文说明数据目录结构、环境变量、反向代理注意事项以及可选扩展能力（TTS 听书、词典、Camoufox 反爬求解）。

## 数据目录

Docker Compose 默认挂载 `deploy/storage/`（容器内 `/storage/storage`），包含：

- `reader.db`（含 WAL/SHM）：SQLite 主库，保存用户、书架、分组、书源、进度、书签与章节缓存
- `reader.db.bak-*`：服务启动前自动创建的数据库快照（默认保留最近 5 份，可设 `READER_DB_BACKUP=0` 关闭）
- `data/`：本地上传的 `.txt` / `.epub` 电子书与自动监听书仓目录
- `covers/`、`fonts/`：书籍封面缓存与用户上传的自定义字体
- `dict.sqlite`：离线词典索引库（启用词典时自动生成）

## 环境变量

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `READER_SERVER_PORT` | `8080` | 服务端监听端口（Compose 用 `READER_PORT` 映射宿主机端口） |
| `READER_APP_WORKDIR` | 当前目录 | 数据根目录，SQLite 与资产保存在其 `storage/` 下 |
| `READER_APP_MODE` | `single` | 运行模式：`single`（单用户）或 `multi`（多用户命名空间隔离） |
| `READER_APP_SECURE` | `false` | 是否开启登录鉴权与会话保护（多用户模式建议同时开启） |
| `READER_APP_INVITECODE` | — | 多用户模式下新用户注册所需邀请码 |
| `READER_DB_MAX_CONNECTIONS` | `24` | SQLite WAL 连接池上限（范围 `1..64`） |
| `READER_DB_BACKUP` | `1` | 启动前自动快照开关（`0` 关闭） |
| `READER_LOG_DIR` | — | 可选，指定后开启按日轮转的后端日志文件（如 `/storage/logs`） |
| `READER_APP_WEB_ROOT` | — | 可选，指定外部前端静态目录，覆盖二进制内嵌的 `frontend-v2` 产物 |
| `READER_TOC_CACHE_TTL_MS` | `86400000` | 目录缓存 TTL（默认 24 小时） |
| `READER_AUTO_BACKUP_HOUR` | `3` | 每日自动 WebDAV 备份触发小时（`0-23`） |
| `READER_HTTP_RETRIES` | `2` | 单源抓取/搜索重试次数（Compose 默认 `1`，加快死源排空） |
| `SSRF_ALLOW_PRIVATE` | `false` | 允许图片代理 / HttpTTS / 登录检测访问私网地址（自建内网服务时开启） |
| `READER_ALLOW_PRIVATE_SOURCE` | `false` | 允许书源抓取访问私网/回环地址（仅局域网或本地调试源开启，默认拦截防 SSRF） |
| `READER_DICT_ONLINE` | `1` | 本地词典未命中时是否启用在线兜底查询（结果缓存 30 天，`0` 为纯离线） |
| `READER_DICT_DIR` | `/app/dict` | 离线词典原始数据目录 |
| `READER_CAMOUFOX_URL` | — | 外部 Camoufox 过盾求解服务地址 |
| `READER_BROWSER_FIRST` | `1` | 是否优先经浏览器反检测抓取；`0` 为直连优先（Compose 默认 `0`） |

> **运行时即时生效项（无需重启）**：在前端「设置 → 阅读与排版 → 搜书网络偏好」中可实时调整单源搜索超时（3–60 秒），每次搜书随请求下发。

## 反向代理（可选）

容器单端口直接提供前端页面与全部 `/reader3/*` API，局域网或内网穿透无需反代。如需公网 HTTPS 反代，请确认以下三点：

1. **SSE 路径禁止压缩与缓冲**：对 `/reader3/*SSE`（如 `searchBookMultiSSE`、`cacheBookSSE`、`debugBookSourceSSE`）关闭代理缓冲和 gzip 压缩，否则流式进度无法实时推送。
2. **HTML 不缓存，静态哈希长缓存**：
   - `/` 及 SPA 路由、`/reader3/*` 保持 `no-cache`（容器已下发正确响应头）；
   - `/static/*` 为带哈希的前端产物，可长期缓存（`immutable`）；
   - `/assets/*` 为用户封面等动态资源，不要强加 `immutable`。
3. 可直接参考 `deploy/Caddyfile.example`。

## 可选能力

### 听书语音（TTS）
在「设置 → 听书语音」中支持三种模式：
- **Edge TTS**：直接通过后端 WebSocket 合成微软神经语音，开箱可用；
- **自定义 HttpTTS**：兼容 legado HttpTTS 音源格式，可添加与管理多套云端/自建音源；
- **浏览器语音**：零依赖兜底方案。

### WebDAV 云备份
在「设置 → 缓存与备份」中填写 WebDAV 地址、账号密码与远程路径即可手动或定时备份/恢复书架、书源、分组、书签、净化规则与 HttpTTS 配置。

### 词典（划词查词）
默认内置离线词典索引（首次启动在后台构建 `dict.sqlite`），未命中时可自动走在线兜底。若构建自定义镜像不需要词典，可在 `docker build` 时传 `--build-arg WITH_DICT=0` 进一步减小镜像体积。

### Camoufox（Cloudflare / 交互过盾）
如需处理 Cloudflare 质询或复杂反爬站点，可在宿主机运行外部求解服务：

```bash
pip install camoufox && python -m camoufox fetch
python scripts/camoufox_solver.py
```

随后设置 `READER_CAMOUFOX_URL=http://<宿主机地址>:<端口>` 即可；未配置时不影响常规直连书源抓取。
