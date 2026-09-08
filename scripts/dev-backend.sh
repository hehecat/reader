#!/usr/bin/env bash
# 后端本地快速验证: 增量编译(debug) + 起临时实例 + 冒烟, 不等 GH 镜像.
#
# 用法:
#   scripts/dev-backend.sh            # 增量 build + 后台起临时实例(默认 :4599, 独立 storage)
#   scripts/dev-backend.sh smoke      # 对临时实例跑冒烟: index 200 / 注册登录 / 书架 200
#   scripts/dev-backend.sh log        # tail 运行日志
#   scripts/dev-backend.sh stop       # 停临时实例
#
# 环境变量:
#   DEV_BACKEND_PORT     监听端口(默认 4599)
#   DEV_BACKEND_WORKDIR  实例数据目录(默认 /tmp/reader-devbackend-<port>, 含 storage/)
#   DEV_BACKEND_WEBROOT  前端目录(默认仓库 deploy/web-dist; 不存在则用 frontend/dist)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${DEV_BACKEND_PORT:-4599}"
WORKDIR="${DEV_BACKEND_WORKDIR:-/tmp/reader-devbackend-$PORT}"
WEBROOT="${DEV_BACKEND_WEBROOT:-}"
BIN="$ROOT/backend/target/debug/reader-dev"
cmd="${1:-run}"

stop() {
  if [ -f "$WORKDIR/run.pid" ]; then
    kill "$(cat "$WORKDIR/run.pid")" 2>/dev/null || true
    rm -f "$WORKDIR/run.pid"
  fi
}

case "$cmd" in
  stop)
    stop; echo "stopped"; exit 0 ;;
  log)
    tail -n 40 -f "$WORKDIR/run.log"; exit 0 ;;
esac

cd "$ROOT/backend"
echo "== cargo build (debug 增量)"
cargo build 2>&1 | tail -2

if [ -z "$WEBROOT" ]; then
  if [ -d "$ROOT/deploy/web-dist" ]; then WEBROOT="$ROOT/deploy/web-dist";
  elif [ -d "$ROOT/frontend/dist" ]; then WEBROOT="$ROOT/frontend/dist";
  else WEBROOT="$ROOT/backend/web-ui/dist"; fi
fi

mkdir -p "$WORKDIR/storage"
stop
echo "== 起临时实例 :$PORT (workdir=$WORKDIR webroot=$WEBROOT)"
READER_APP_WORKDIR="$WORKDIR/storage" \
READER_APP_WEB_ROOT="$WEBROOT" \
READER_SERVER_PORT="$PORT" \
READER_APP_SECURE=true \
READER_APP_SECUREKEY=devkey \
READER_DB_BACKUP=0 \
  nohup "$BIN" > "$WORKDIR/run.log" 2>&1 &
echo $! > "$WORKDIR/run.pid"

for _ in $(seq 1 30); do
  if curl -sf -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/"; then break; fi
  sleep 1
done

if [ "$cmd" = "smoke" ]; then
  echo "== 冒烟"
  curl -s -o /dev/null -w 'index %{http_code}\n' --max-time 5 "http://127.0.0.1:$PORT/"
  TOK=$(curl -s -X POST -H 'Content-Type: application/json' \
    -d '{"username":"smoke01","password":"smoke123456","isLogin":false}' --max-time 10 \
    "http://127.0.0.1:$PORT/reader3/login" | python3 -c 'import json,sys;print(json.load(sys.stdin)["data"]["accessToken"])')
  echo "login token=${TOK:0:12}..."
  curl -s -X POST -H 'Content-Type: application/json' -H "accessToken: $TOK" -d '{}' \
    --max-time 10 "http://127.0.0.1:$PORT/reader3/getBookshelf" | head -c 60; echo
  echo "== 实例保持运行: scripts/dev-backend.sh stop 停止"
else
  echo "== ready: http://127.0.0.1:$PORT (logs: $WORKDIR/run.log)"
fi
