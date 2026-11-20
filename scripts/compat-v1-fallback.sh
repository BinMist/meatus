#!/usr/bin/env bash
# E1-04 回落验收：当前 1.4 客户端 × 1.3.0 服务端。
# 验证客户端先发 v2 Hello，收到 1002 后以 protocol=1 重连并完成代理转发。
#
# 用法：
#   cargo build
#   ./scripts/compat-v1-fallback.sh
#
# 可用环境变量覆盖：
#   MEATUS_BINARY       当前客户端二进制，默认 target/debug/meatus
#   MEATUS_OLD_SERVER   1.3.0 服务端二进制
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${MEATUS_BINARY:-target/debug/meatus}"
[[ -x "$BIN" ]] || { echo "missing $BIN（先 cargo build）" >&2; exit 1; }
command -v python3 >/dev/null || { echo "缺少 python3" >&2; exit 1; }
command -v curl >/dev/null || { echo "缺少 curl" >&2; exit 1; }

TMP="$(mktemp -d)"
SERVER_PID=""; CLIENT_PID=""; HTTP_PID=""
cleanup() {
    for pid in "$CLIENT_PID" "$SERVER_PID" "$HTTP_PID"; do
        [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
    done
    rm -rf "$TMP"
}
trap cleanup EXIT

target_triple() {
    case "$(uname -s):$(uname -m)" in
        Darwin:arm64) echo "aarch64-apple-darwin" ;;
        Darwin:x86_64) echo "x86_64-apple-darwin" ;;
        Linux:aarch64|Linux:arm64) echo "aarch64-unknown-linux-musl" ;;
        Linux:x86_64) echo "x86_64-unknown-linux-musl" ;;
        *) echo "unsupported host: $(uname -s)/$(uname -m)" >&2; return 1 ;;
    esac
}
TARGET="$(target_triple)"

find_fixture() {
    local override="$1" dir="$2" tar="$3" out="$4" var="$5"
    if [[ -n "$override" ]]; then
        printf '%s\n' "$override"
        return
    fi
    if [[ -x "$dir/meatus" ]]; then
        printf '%s\n' "$dir/meatus"
        return
    fi
    if [[ -f "$tar" ]]; then
        mkdir -p "$TMP/$out"
        tar xzf "$tar" -C "$TMP/$out"
        local found
        found="$(find "$TMP/$out" -type f -name meatus -perm -111 | head -n 1)"
        if [[ -n "$found" ]]; then
            printf '%s\n' "$found"
            return
        fi
    fi
    echo "missing fixture; set $var or place it at $dir/meatus" >&2
    return 1
}

OLD_SERVER="$(find_fixture "${MEATUS_OLD_SERVER:-}" "dist/v1.3.0/meatus-1.3.0-$TARGET" "dist/v1.3.0/meatus-1.3.0-$TARGET.tar.gz" "f130server" "MEATUS_OLD_SERVER")"
check_version() {
    local bin="$1" want="$2" got
    got="$("$bin" --version | awk '{print $2}')"
    [[ "$got" == "$want" ]] || { echo "fixture $bin is $got, expected $want" >&2; exit 1; }
}
check_version "$OLD_SERVER" "1.3.0"
echo "fixture 1.3.0 server: $OLD_SERVER"

# control / admin / echo / remote。
read -r CONTROL ADMIN ECHO REMOTE <<EOF
$(python3 - <<'PY'
import socket
socks = []
for _ in range(4):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    socks.append(s)
print(" ".join(str(s.getsockname()[1]) for s in socks))
PY
)
EOF

mkdir -p "$TMP/certs" "$TMP/www"
printf 'fallback-ok\n' > "$TMP/www/fallback.txt"
"$BIN" gen-cert --domain localhost --domain 127.0.0.1 --out "$TMP/certs" >/dev/null
TOKEN_OUT="$("$BIN" gen-token)"
TOKEN="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "token" { print $2 }')"
HASH="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "sha256" { print $2 }')"
[[ -n "$TOKEN" && -n "$HASH" ]] || { echo "gen-token failed" >&2; exit 1; }

cat > "$TMP/server.toml" <<CFG
[server]
bind_addr = "127.0.0.1"
control_port = $CONTROL
tls_cert = "$TMP/certs/server.crt"
tls_key = "$TMP/certs/server.key"

[auth]
mode = "hmac"
clients = [
  { id = "new-client", tenant = "default", token_sha256 = "$HASH" },
]

[ports]
range = [$REMOTE, $REMOTE]
release_policy = "immediate"

[admin]
bind = "127.0.0.1:$ADMIN"
panel = false

[log]
level = "info"
format = "text"
CFG

cat > "$TMP/client.toml" <<CFG
[client]
server_addr = "127.0.0.1:$CONTROL"
server_name = "localhost"
server_ca_cert = "$TMP/certs/server.crt"
client_id = "new-client"
tenant = "default"
token = "$TOKEN"

[[proxy]]
name = "new-client-http"
type = "tcp"
local_addr = "127.0.0.1:$ECHO"
remote_port = $REMOTE

[log]
level = "info"
format = "text"
CFG

python3 -m http.server "$ECHO" --bind 127.0.0.1 --directory "$TMP/www" >"$TMP/http.log" 2>&1 & HTTP_PID=$!
"$OLD_SERVER" server --config "$TMP/server.toml" >"$TMP/server.log" 2>&1 & SERVER_PID=$!

for _ in $(seq 1 40); do
    if curl -fsS --max-time 2 "http://127.0.0.1:$ADMIN/healthz" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "1.3.0 server exited during startup" >&2; cat "$TMP/server.log"; exit 1
    fi
    sleep 0.5
done
curl -fsS --max-time 2 "http://127.0.0.1:$ADMIN/healthz" >/dev/null 2>&1 || {
    echo "1.3.0 server healthz did not become ready" >&2; cat "$TMP/server.log"; exit 1
}

"$BIN" client --config "$TMP/client.toml" >"$TMP/client.log" 2>&1 & CLIENT_PID=$!

for _ in $(seq 1 60); do
    if curl -fsS --max-time 3 "http://127.0.0.1:$REMOTE/fallback.txt" 2>/dev/null | grep -qx 'fallback-ok'; then
        break
    fi
    if ! kill -0 "$CLIENT_PID" 2>/dev/null; then
        echo "FAIL  client exited before fallback tunnel became ready" >&2
        cat "$TMP/client.log" >&2
        exit 1
    fi
    sleep 0.5
done
if ! curl -fsS --max-time 3 "http://127.0.0.1:$REMOTE/fallback.txt" 2>/dev/null | grep -qx 'fallback-ok'; then
    echo "FAIL  fallback tunnel did not become ready" >&2
    echo "--- server log ---" >&2; cat "$TMP/server.log" >&2
    echo "--- client log ---" >&2; cat "$TMP/client.log" >&2
    exit 1
fi
echo "PASS  v2 rejected -> v1 fallback -> proxy register -> TCP forward"

strip_ansi() {
    local src="$1" dst="$2"
    python3 - "$src" > "$dst" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
sys.stdout.write(re.sub(r"\x1b\[[0-9;]*m", "", text))
PY
}
strip_ansi "$TMP/server.log" "$TMP/server.plain.log"
strip_ansi "$TMP/client.log" "$TMP/client.plain.log"

grep -q 'server rejected protocol version; retrying with protocol 1' "$TMP/client.plain.log" || {
    echo "FAIL  client did not log fallback" >&2; cat "$TMP/client.plain.log" >&2; exit 1
}
grep -q 'client_protocol=1' "$TMP/client.plain.log" || {
    echo "FAIL  client did not settle on protocol=1" >&2; cat "$TMP/client.plain.log" >&2; exit 1
}
grep -q 'proxy registered proxy=new-client-http' "$TMP/server.plain.log" || {
    echo "FAIL  1.3.0 server did not register the proxy" >&2; cat "$TMP/server.plain.log" >&2; exit 1
}
echo "RESULT: PASS (1.4 client -> 1.3.0 server, protocol=1 fallback)"
