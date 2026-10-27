# 配置与部署

部署者需要知道的：数据落在哪、有哪些环境变量、反代要注意什么、可选组件怎么接。

## 数据目录

compose 默认挂 `deploy/storage/`(容器内 `/storage/storage`), 内含:

- `reader.db` — SQLite 主库(书架/书源/进度/缓存/用户)
- `reader.db.bak-*` — 启动前自动快照(保留 5 份, `READER_DB_BACKUP=0` 关)
- 封面/本地书/WebDAV 数据

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `READER_APP_WORKDIR` | 当前目录 | 数据根(其 `storage/` 子目录存库与资产) |
| `READER_APP_SECURE` | false | 多用户 secure 模式开关 |
| `READER_APP_INVITECODE` | - | secure 模式注册邀请码 |
| `READER_SERVER_PORT` | 8080 | 容器监听端口(compose 用 `READER_PORT` 映射宿主端口) |
| `READER_TOC_CACHE_TTL_MS` | 86400000 | 目录缓存 TTL(24h) |
| `READER_DB_BACKUP` | 1 | 启动前 db 快照开关(0 关) |
| `READER_AUTO_BACKUP_HOUR` | 3 | 每日 WebDAV zip 备份时刻(0-23) |
| `READER_HTTP_RETRIES` | 2 | 搜索/抓取单源重试次数(compose 默认 1, 死源排空更快) |
| `SSRF_ALLOW_PRIVATE` | false | 图片代理/TTS/登录检测允许私网地址(内网 TTS 等自建场景) |
| `READER_ALLOW_PRIVATE_SOURCE` | false | 书源抓取允许私网/回环地址(局域网书源、本地测试源才开; 默认拦截防 SSRF) |
| `READER_AI_BASE_URL` | 空 | OpenAI 兼容端点(如 `http://octopus:8080/v1`); 与 KEY 同时非空才启用 AI 面板 |
| `READER_AI_API_KEY` | 空 | 上述端点密钥 |
| `READER_AI_MODEL` | deepseek-v4.1-flash | 模型名(支持推理模型, 思考过程不显示) |
| `READER_AI_MAX_TOKENS` | 3500 | 单次回答上限(0 = 不限制, 推理模型思考也吃预算) |
| `READER_DICT_ONLINE` | 1 | 词典在线兜底开关(本地未命中时有道查询, 缓存 30 天) |
| `READER_DICT_DIR` | /app/dict | 离线词典原始数据目录(ecdict.csv / word.json / ci.json / idiom.json) |
| `READER_CAMOUFOX_URL` | - | 外置 camoufox 服务地址; 缺省容器内自 spawn |
| `READER_BROWSER_FIRST` | 1 | 抓取优先经浏览器反检测; 0 恢复直连优先(compose 默认 0) |

**运行时可调(无需重启/重建)**: 设置页「搜索 → 单源搜索超时」(3-60s, 即时保存, 随每次搜索请求下发)。

镜像构建为 cargo-chef 三段式, 依赖层命中缓存时 src 改动只编本 crate; 本地开发见 [AGENTS.md](../AGENTS.md)。

**CF 质询/浏览器自动登录**: 镜像内置 camoufox Python 求解器脚本, 但**不内置浏览器二进制**(~600MB, 容器内环境兼容性不可靠)。需要浏览器自动登录时, 在宿主机跑:
```bash
pip install camoufox && python -m camoufox fetch
python scripts/camoufox_solver.py
```
再给容器设 `READER_CAMOUFOX_URL=http://<宿主机IP>:<端口>`。未配置时施报「camoufox 求解服务启动失败」并缓存, 不影响直连抓取。

## 反代(通常不需要)

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

### 词典(选中查词)

镜像默认内置离线词典(构建期下载 ECDICT 英汉 + 新华字典字词/成语, MIT),
本地未命中时自动走**在线兜底**(有道 jsonapi 免密钥: 释义/百科/网络释义), 结果缓存 30 天;
`READER_DICT_ONLINE=0` 可关成纯离线:

- 数据目录: 镜像内 `/app/dict`(可用 `READER_DICT_DIR` 覆盖挂载自己的数据)
- 首次启动后台导入为 `{数据目录}/dict.sqlite`(索引库, 之后查询毫秒级); 导入期间查词返回「查询中」自动轮询
- 不需要词典: 构建时 `--build-arg WITH_DICT=0` 跳过(镜像小 ~130MB), 查词入口提示未安装

### camoufox(反爬求解)

镜像已内置(pip 包 + Firefox 二进制), 遇验证码/Cloudflare 质询自动 spawn 求解, 无需配置。源站普遍直连可达时可设 `READER_BROWSER_FIRST=0` 提速。

---

