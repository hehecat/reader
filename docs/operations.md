# 运维与排错

日常维护、常见故障定位与升级回滚指南。

## 常见故障排查

| 现象 | 原因与处理 |
| --- | --- |
| 搜索一直显示「0 部作品」 | 检查反代是否缓冲/压缩了 `/reader3/*SSE`；或「书源管理」中尚无启用的可用书源 |
| 搜书首结果偏慢 | 当前批次命中慢源，可在「设置 → 阅读与排版 → 搜书网络偏好」调低单源超时（如 8–15s）快速跳过 |
| 书海打开无内容或空频道 | 点书海顶部「检测」后台探活，系统会自动置顶有书频道并过滤空源/死链 |
| 某书源搜不到或目录为空 | 目标站改版或失效；可在「书源管理」筛选失效源，或进入「规则调试台」查看逐步骤 SSE 解析日志 |
| 端口冲突 | 在 `deploy/.env` 中修改 `READER_PORT=<新端口>`（直跑二进制时改 `READER_SERVER_PORT`） |
| 权限报错 / 无法建库 | 确认挂载的 `storage/` 目录对运行用户可写 |
| 多端进度不同步 | 多用户模式下数据按用户名隔离，请确认两端登录同一账号 |
| 更新后页面样式或资源异常 | 检查反代是否误缓存了 `index.html`，确保 HTML 走 `no-cache` |

## 升级与回滚

```bash
# 拉取最新镜像并重启
cd deploy
docker compose pull && docker compose up -d

# 如需回滚到指定版本
docker compose up -d reader@sha-<旧sha>
```

数据全部持久化在挂载目录 `storage/` 中，升级容器不会丢失藏书与进度；每次启动前自动保留最近 5 份 `reader.db.bak-*` 快照，必要时可直接停服替换 `reader.db` 恢复。

### 本地源码自建镜像

```bash
docker build -t reader .
```

`Dockerfile` 采用多阶段构建（`frontend-v2` via `pnpm` → `backend` via `cargo-chef` → 精简运行层），无需本地安装额外工具链。

## 日志排查

- Docker 部署：`docker compose logs -f --tail=100`
- 本地二进制运行：设置 `READER_LOG_DIR=/path/to/logs` 后，日志自动按日轮转写入该目录，避免脱离终端时 stdout 阻塞。
