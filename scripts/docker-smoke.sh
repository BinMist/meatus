#!/usr/bin/env bash
# Docker 部署冒烟：用镜像起一个 server，验证首启配置、面板、Bearer API 与 SIGTERM 退出。
# 用法：IMAGE=meatus:1.0.0 ./scripts/docker-smoke.sh
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="${IMAGE:-meatus:1.0.0}"
CONTROL_PORT="${CONTROL_PORT:-7800}"
ADMIN_PORT="${ADMIN_PORT:-7801}"
NAME="${NAME:-meatus-docker-smoke}"
TMP="$(mktemp -d)"
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true; rm -rf "$TMP"' EXIT

command -v docker >/dev/null || { echo "docker 未安装" >&2; exit 2; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || { echo "镜像不存在：$IMAGE" >&2; exit 2; }

docker rm -f "$NAME" >/dev/null 2>&1 || true
mkdir -p "$TMP"
docker run -d --name "$NAME" \
    -p "$CONTROL_PORT:7000" \
    -p "127.0.0.1:$ADMIN_PORT:7400" \
    -v "$TMP:/data" \
    -e MEATUS_DOMAIN=localhost \
    "$IMAGE" server --config /data/server.toml >/dev/null

for _ in $(seq 1 30); do
    if curl -fsS "http://127.0.0.1:$ADMIN_PORT/healthz" >/dev/null 2>&1; then break; fi
    sleep 1
done
curl -fsS "http://127.0.0.1:$ADMIN_PORT/healthz" >/dev/null
curl -fsS "http://127.0.0.1:$ADMIN_PORT/" >/dev/null

TOKEN="$(docker exec "$NAME" cat /data/admin.token | tr -d '\r\n')"
test -n "$TOKEN"
curl -fsS -H "Authorization: Bearer $TOKEN" \
    "http://127.0.0.1:$ADMIN_PORT/api/v1/status" >/dev/null

docker stop "$NAME" >/dev/null
docker logs "$NAME" 2>&1 | grep -q 'shutdown requested'

echo "docker smoke PASS: $IMAGE (control=$CONTROL_PORT admin=127.0.0.1:$ADMIN_PORT)"
