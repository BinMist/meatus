#!/usr/bin/env bash
# Linux 弱网验收：20% 丢包 + 50ms 延迟下，TCP 隧道可用且 client 重启后恢复。
# 需要 root、iproute2(tc)、python3、curl；会临时修改 lo 的 qdisc，trap 恢复。
# 用法：sudo ./scripts/weaknet-test.sh [--yes]
set -euo pipefail
cd "$(dirname "$0")/.."

[[ "$(uname -s)" == "Linux" ]] || { echo "weaknet-test.sh 只支持 Linux" >&2; exit 2; }
[[ "$(id -u)" -eq 0 ]] || { echo "请用 root 运行（需要 tc）" >&2; exit 2; }
command -v tc >/dev/null || { echo "缺少 tc（iproute2）" >&2; exit 2; }
[[ "${1:-}" == "--yes" ]] || { echo "会修改 lo qdisc；确认请加 --yes" >&2; exit 2; }

BIN="${MEATUS_BINARY:-target/release/meatus}"
[[ -x "$BIN" ]] || { echo "missing $BIN（先 cargo build --release）" >&2; exit 1; }

TMP="$(mktemp -d)"
SERVER_PID=""; CLIENT_PID=""; HTTP_PID=""
cleanup() {
    [[ -n "$CLIENT_PID" ]] && kill "$CLIENT_PID" 2>/dev/null || true
    [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
    [[ -n "$HTTP_PID" ]] && kill "$HTTP_PID" 2>/dev/null || true
    tc qdisc del dev lo root 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

mkdir -p "$TMP/www" "$TMP/certs"
printf 'weaknet-ok\n' > "$TMP/www/index.html"
TOKEN_OUT="$("$BIN" gen-token)"
TOKEN="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "token" { print $2 }')"
HASH="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "sha256" { print $2 }')"
"$BIN" gen-cert --domain localhost --domain 127.0.0.1 --out "$TMP/certs" >/dev/null

cat > "$TMP/server.toml" <<CFG
[server]
bind_addr = "127.0.0.1"
control_port = 7060
tls_cert = "$TMP/certs/server.crt"
tls_key = "$TMP/certs/server.key"
[auth]
clients = [{ id = "weak", token_sha256 = "$HASH" }]
[admin]
bind = "127.0.0.1:7460"
[log]
level = "info"
format = "text"
CFG
cat > "$TMP/client.toml" <<CFG
[client]
server_addr = "127.0.0.1:7060"
server_name = "localhost"
server_ca_cert = "$TMP/certs/server.crt"
client_id = "weak"
tenant = "default"
token = "$TOKEN"
[[proxy]]
name = "weak-http"
type = "tcp"
local_addr = "127.0.0.1:19090"
remote_port = 21060
[log]
level = "info"
format = "text"
CFG

python3 -m http.server 19090 --bind 127.0.0.1 --directory "$TMP/www" >"$TMP/http.log" 2>&1 & HTTP_PID=$!
"$BIN" server --config "$TMP/server.toml" >"$TMP/server.log" 2>&1 & SERVER_PID=$!
sleep 1
"$BIN" client --config "$TMP/client.toml" >"$TMP/client.log" 2>&1 & CLIENT_PID=$!
sleep 2
curl -fsS --max-time 10 http://127.0.0.1:21060/index.html >/dev/null || { echo "baseline failed" >&2; cat "$TMP/client.log"; exit 1; }

tc qdisc add dev lo root netem loss 20% delay 50ms
OK=0; TOTAL=20
for _ in $(seq 1 "$TOTAL"); do
    if curl -fsS --retry 2 --retry-delay 1 --max-time 10 http://127.0.0.1:21060/index.html >/dev/null 2>&1; then
        OK=$((OK+1))
    fi
done
echo "lossy requests: $OK/$TOTAL"

# client 重启后必须重新注册并恢复
kill -9 "$CLIENT_PID"; wait "$CLIENT_PID" 2>/dev/null || true
"$BIN" client --config "$TMP/client.toml" >"$TMP/client2.log" 2>&1 & CLIENT_PID=$!
RECONNECT=0
for _ in $(seq 1 30); do
    if curl -fsS --max-time 5 http://127.0.0.1:21060/index.html >/dev/null 2>&1; then
        RECONNECT=1
        break
    fi
    sleep 1
done
if [[ "$RECONNECT" -eq 1 ]]; then
    echo "reconnect: PASS"
else
    echo "reconnect: FAIL" >&2; cat "$TMP/client2.log"; exit 1
fi

if [[ "$OK" -lt $((TOTAL * 95 / 100)) ]]; then
    echo "FAIL: lossy success $OK/$TOTAL < 95%" >&2
    exit 1
fi
echo "RESULT: PASS (20% loss)"
