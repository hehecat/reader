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
# 默认自启(镜像内求解器就是要常驻); 指向外部求解器时设 CAMOUFOX_SOLVER_AUTOSTART=0
AUTOSTART="${CAMOUFOX_SOLVER_AUTOSTART:-1}"

if [ "$AUTOSTART" = "1" ] && [ -f "$SOLVER" ]; then
    (
        while true; do
            python3 "$SOLVER" --port "$SOLVER_PORT" --host 127.0.0.1 >>"$SOLVER_LOG" 2>&1 || true
            echo "[entrypoint] camoufox solver 退出, 5s 后重启" >>"$SOLVER_LOG"
            sleep 5
        done
    ) &
    # 等监听就绪(≤30s), 再后台预热浏览器(常驻单例): 首次真实质询省掉冷启动等待。
    # 预热失败不影响任何流程(仅记录), 主进程照常启动。
    (
        for _ in $(seq 1 30); do
            python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:$SOLVER_PORT/health', timeout=2)" 2>/dev/null && break
            sleep 1
        done
        python3 - "$SOLVER_PORT" >>"$SOLVER_LOG" 2>&1 <<'PY'
import json, sys, urllib.request
port = sys.argv[1]
req = urllib.request.Request(
    f"http://127.0.0.1:{port}/solve",
    data=json.dumps({"url": "https://www.gstatic.com/generate_204", "maxWaitMs": 15000}).encode(),
    headers={"Content-Type": "application/json"},
)
try:
    urllib.request.urlopen(req, timeout=180)
    print("[entrypoint] 浏览器预热完成")
except Exception as e:
    print(f"[entrypoint] 浏览器预热失败(不影响): {e}")
PY
    ) &
fi

exec "$@"
