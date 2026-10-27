# AGENTS.md

给在这个仓库里写代码的 AI 助手（以及新加入的人）：这里只放「怎么改、怎么验、别踩什么坑」。
面向使用者/部署者的内容在 [README](README.md) 与 [docs/](docs/)。

## 仓库结构

| 路径 | 内容 |
|---|---|
| `backend/` | Rust(axum) 后端：`src/api/router.rs` 路由（很大，改前先 grep 函数名）、`src/service/*` 抓取/搜索/阅读、`src/storage/*` SQLite 与迁移、`src/parser/*` 书源规则引擎 |
| `frontend/` | React + Vite + Tailwind，`pnpm` 管理 |
| `deploy/` | compose 与部署资产；`docker-compose.override.yml` 本机配置，已 gitignore |
| `docs/` | 人类文档：配置、运维、截图 |
| `scripts/` | `dev-backend.sh` 本地起后端、`camoufox_solver.py`、curated sources |

## 构建与测试

```bash
# 后端（RUSTFLAGS 必需，否则 reqwest http3 编译失败）
cd backend
export PATH="$HOME/.cargo/bin:$PATH"
export RUSTFLAGS="--cfg reqwest_unstable"

cargo test --lib                    # 全部单测（约 730 个，4 分钟）
cargo test --lib service::crawler   # 只跑相关模块（执行 <1s，重链约 22s）
cargo test --lib --tests            # 单测 + 集成测试
cargo fmt --check
cargo clippy --all-targets -- -D warnings

# 前端
cd frontend
pnpm install --frozen-lockfile
pnpm lint
pnpm build                          # tsc -b && vite build
```

构建耗时已调优（`backend/Cargo.toml` 的 dev/test profile）：改一行到 test 二进制就绪约 22 秒；
冷构建约 11 分钟（一次性）。迭代时别在 `cargo check` 和 `cargo test` 之间来回切——两套产物会互相
触发重编，切换一次约 5 分钟。前端改样式用 `pnpm dev`（HMR），`pnpm build` 只在发版前跑。

本地起后端不等 GH 镜像：`scripts/dev-backend.sh`（增量 debug 编译 + 临时实例 :4599 + 独立 storage），
`scripts/dev-backend.sh smoke` 冒烟（index/注册登录/书架），`scripts/dev-backend.sh stop` 停止。

## 开发纪律

- **个人网络配置不入库**：内网 IP、自建域名、网关网络名只写 `deploy/docker-compose.override.yml`
  （已 gitignore）。提交前钩子 `git config core.hooksPath .githooks` 与 CI `hygiene` job 都会拦。
- **运行时文件不入库**：`deploy/storage/` 是运行时 sqlite（含 WAL/SHM），首次启动自动建库迁移。
- **秘密不入库**：AI key 通过环境变量或设置页写入本地 DB；任何密钥都不许出现在代码、文档、测试、日志里。
- **SSE 改动必须真机验证流式**：压缩/缓冲是历史事故高发区（反代相关要求见 `docs/configuration.md`）。
- **书源规则调试**用前端「书源工作台」，有逐规则 SSE 日志，不要靠猜。
- **参数名别混**：`bookSource`（单源搜索/正文/探索的 JSON body）与 `bookSourceUrl`（SSE 单源 query）。
- **CI 会跑**：后端 `cargo test --lib --tests` + `cargo fmt --check` + `cargo clippy -- -D warnings`，
  前端 lint + build，外加 hygiene。clippy 已开 `-D warnings`，新代码不能带 warning。
- **工具链钉在 1.97**（与 `Dockerfile` 的 `rust:1.97-slim` 一致）。升级要同时改 Dockerfile 与
  `.github/workflows/ci.yml`，并接受新 lint 带来的修复成本。

## 代码约定

- 测试与源码同文件的 `#[cfg(test)]` 模块；跨模块集成测试在 `backend/tests/`。
- 存储 schema 迁移在 `backend/src/storage/migrate.rs`，改表要兼容旧库。
- 提交信息说清「为什么」，不要只写「update」。
- 大改动前先看 `backend/BACKEND-NOTES.md`（v5 时代设计背景，与现状冲突时以代码为准）。

## 常见坑

- 开了 sccache 但 `~/.cache` 不可写时：`export RUSTC_WRAPPER="" SCCACHE_DISABLE=1`，否则 C 依赖
  （如 ring）的构建脚本会以 sccache 包装 `cc` 失败。
- `rust-embed` 编译期要求 `backend/web-ui/dist/` 存在；CI 放占位 `index.html`，本地先 `pnpm build`。
- Windows GNU 工具链的 `getrandom` 后端覆盖写在 `backend/.cargo/config.toml`，别删。
