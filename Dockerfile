# ============================================================
# reader 统一镜像多阶段构建 (context = 仓库根)
#   frontend/ React (vite + pnpm) → dist
#   backend/  Rust (warp) → reader-dev 二进制, rust-embed 内嵌同一份 dist
#   camoufox  验证码/登录浏览器后端 (pip 包 + Firefox 二进制, 构建期下载)
# 构建: docker build -t reader .
# ============================================================

# ---------- 阶段 1：前端构建 (React) ----------
FROM node:22-slim AS web
WORKDIR /web
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN npm i -g pnpm@11 && pnpm fetch --frozen-lockfile
COPY frontend ./
RUN pnpm install --frozen-lockfile --offline && pnpm build

# ---------- 阶段 2：后端编译（cargo-chef 三段式: 依赖层缓存, src 改动只编本 crate） ----------
FROM rust:1.97-slim AS chef
RUN cargo install cargo-chef --locked

FROM chef AS planner
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/src ./src
COPY backend/.cargo ./.cargo
# rust-embed/include_bytes 路径占位（prepare 只扫源码不编译, 真产物在 cook/build 阶段注入）
RUN mkdir -p web-ui/dist web-ui/public/fonts \
    && echo '<!doctype html><title>placeholder</title>' > web-ui/dist/index.html
RUN cargo chef prepare --recipe-path /app/recipe.json

FROM chef AS cook
WORKDIR /app
ENV RUSTFLAGS="--cfg reqwest_unstable"
COPY --from=planner /app/recipe.json /app/recipe.json
RUN cargo chef cook --release --recipe-path /app/recipe.json

FROM cook AS builder
WORKDIR /app
COPY backend/src ./src
COPY backend/.cargo ./.cargo
# GAP 176：epub 导出内嵌中文字体（include_bytes 编译期内嵌, 路径相对 cargo 根）
COPY backend/web-ui/public/fonts ./web-ui/public/fonts
# rust-embed 编译期嵌入前端（web-ui/dist 由 web 阶段产出——本仓库不存旧前端产物）
COPY --from=web /web/dist ./web-ui/dist
RUN cargo build --release

# ---------- 阶段 3：camoufox 求解后端（pip 包 + 浏览器二进制，构建期下载） ----------
FROM python:3.13-slim AS camo
RUN pip install --no-cache-dir camoufox==0.5.4 \
    && python -m camoufox fetch

# ---------- 阶段 4：运行镜像（python:3.13-slim 基础——与 camo 同解释器版本,
#           camo 的 site-packages 直拷即可被 import, 避免系统 python 版本错配） ----------
FROM python:3.13-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        tzdata \
        fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/*

# camoufox 运行时系统库（Firefox 内核依赖集）+ tini
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        tini \
        libnss3 libnspr4 libdbus-1-3 libatk1.0-0 libatk-bridge2.0-0 \
        libcups2 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 \
        libxfixes3 libxrandr2 libgbm1 libasound2 libpango-1.0-0 libcairo2 \
    && ln -sf /usr/bin/tini /sbin/tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=camo /usr/local/lib/python3.13/site-packages /usr/local/lib/python3.13/site-packages
# 浏览器二进制(~600MB)不内嵌运行镜像——容器内 env 不兼容(Playwright 启动失败);
# 需要浏览器登录的部署方在宿主机跑 scripts/camoufox_solver.py, 设 READER_CAMOUFOX_URL 指向它
# COPY --from=camo /root/.cache/camoufox /root/.cache/camoufox  # 注释掉: 不内置
COPY backend/scripts/camoufox_solver.py /usr/local/bin/camoufox_solver.py

ENV TZ=Asia/Shanghai
ENV READER_APP_WEB_ROOT=/app/web-ui/dist
# 未配置 READER_CAMOUFOX_URL → 首次用到浏览器时自动 spawn python3 camoufox_solver.py --port 8196
ENV READER_CAMOUFOX_SCRIPT=/usr/local/bin/camoufox_solver.py

COPY --from=builder /app/target/release/reader-dev /usr/local/bin/reader-dev
# 唯一高频变化层（二进制 + dist）放末尾, 稳定层前置
COPY --from=builder /app/web-ui/dist /app/web-ui/dist

EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["reader-dev"]
