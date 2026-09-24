#!/bin/sh
# 容器启动: 常驻拉起 camoufox 求解服务(后台守护), 再 exec 主进程。
#
# 为什么不用后端的惰性 spawn: spawn 的 stdout/stderr 被丢弃, 失败时没有任何可查线索;
# 且首次命中 Cloudflare 质询要等 spawn + 健康轮询(最长 20s)才开始求解, 期间抓取被拖慢。
# 这里常驻启动 + 崩溃自动重启(5s 退避), 日志留在 /tmp/camoufox-solver.log 便于排查。
# 主进程仍以 READER_CAMOUFOX_URL 指向它, 后端不再重复 spawn。
set -eu

SOLVER="${READER_CAMOUFOX_SCRIPT:-/usr/local/bin/camoufox_solver.py}"
SOLVER_PORT="${CAMOUFOX_SOLVER_PORT:-8196}"
SOLVER_LOG="/tmp/camoufox-solver.log"

if [ -f "$SOLVER" ] && [ -z "${READER_CAMOUFOX_URL:-}" ]; then
    (
        while true; do
            python3 "$SOLVER" --port "$SOLVER_PORT" --host 127.0.0.1 >>"$SOLVER_LOG" 2>&1 || true
            echo "[entrypoint] camoufox solver 退出, 5s 后重启" >>"$SOLVER_LOG"
            sleep 5
        done
    ) &
fi

exec "$@"
