#!/usr/bin/env bash
# E1-03 旧客户端兼容验收：1.2.1 / 1.3.0 正式客户端 × 当前 1.4 服务端。
# 验证 v1 握手、proxy 注册与 TCP 数据转发；不修改仓库，不提交二进制。
#
# 用法：
#   cargo build
#   ./scripts/compat-old-clients.sh
#
# 可用环境变量覆盖 fixture 路径：
#   MEATUS_BINARY      当前服务端二进制，默认 target/debug/meatus
#   MEATUS_OLD_121     1.2.1 客户端二进制
#   MEATUS_OLD_130     1.3.0 客户端二进制
#   MEATUS_SERVER_BIN  被测服务端二进制，默认同 MEATUS_BINARY
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${MEATUS_BINARY:-target/debug/meatus}"
SERVER="${MEATUS_SERVER_BIN:-$BIN}"
[[ -x "$BIN" ]] || { echo "missing $BIN（先 cargo build）" >&2; exit 1; }
command -v python3 >/dev/null || { echo "缺少 python3" >&2; exit 1; }
command -v curl >/dev/null || { echo "缺少 curl" >&2; exit 1; }

TMP="$(mktemp -d)"
SERVER_PID=""; CLIENT121_PID=""; CLIENT130_PID=""; HTTP_PID=""
cleanup() {
    for pid in "$CLIENT130_PID" "$CLIENT121_PID" "$SERVER_PID" "$HTTP_PID"; do
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

# 从已解包目录或 release tar.gz 找 fixture；优先环境变量。
find_fixture() {
    local override="$1" dir="$2" tar="$3" out="$4"
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
    echo "missing fixture; set $5 or place it at $dir/meatus" >&2
    return 1
}

OLD121="$(find_fixture "${MEATUS_OLD_121:-}" "dist/meatus-1.2.1-$TARGET" "dist/meatus-1.2.1-$TARGET.tar.gz" "f121" "MEATUS_OLD_121")"
OLD130="$(find_fixture "${MEATUS_OLD_130:-}" "dist/v1.3.0/meatus-1.3.0-$TARGET" "dist/v1.3.0/meatus-1.3.0-$TARGET.tar.gz" "f130" "MEATUS_OLD_130")"

check_version() {
    local bin="$1" want="$2" got
    got="$("$bin" --version | awk '{print $2}')"
    [[ "$got" == "$want" ]] || { echo "fixture $bin is $got, expected $want" >&2; exit 1; }
}
check_version "$OLD121" "1.2.1"
check_version "$OLD130" "1.3.0"
echo "fixture 1.2.1: $OLD121"
echo "fixture 1.3.0: $OLD130"

# 一次占用 5 个空闲端口：control / admin / echo / remote121 / remote130。
read -r CONTROL ADMIN ECHO REMOTE121 REMOTE130 <<EOF
$(python3 - <<'PY'
import socket
socks = []
for _ in range(5):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    socks.append(s)
print(" ".join(str(s.getsockname()[1]) for s in socks))
PY
)
EOF

if [[ "$REMOTE121" -lt "$REMOTE130" ]]; then
    PORTS_MIN="$REMOTE121"; PORTS_MAX="$REMOTE130"
else
    PORTS_MIN="$REMOTE130"; PORTS_MAX="$REMOTE121"
fi

mkdir -p "$TMP/certs" "$TMP/www"
printf 'old-client-ok\n' > "$TMP/www/old.txt"
"$BIN" gen-cert --domain localhost --domain 127.0.0.1 --out "$TMP/certs" >/dev/null

gen_token() {
    "$BIN" gen-token
}
TOK121_OUT="$(gen_token)"
TOK121="$(printf '%s\n' "$TOK121_OUT" | awk '$1 == "token" { print $2 }')"
HASH121="$(printf '%s\n' "$TOK121_OUT" | awk '$1 == "sha256" { print $2 }')"
TOK130_OUT="$(gen_token)"
TOK130="$(printf '%s\n' "$TOK130_OUT" | awk '$1 == "token" { print $2 }')"
HASH130="$(printf '%s\n' "$TOK130_OUT" | awk '$1 == "sha256" { print $2 }')"
[[ -n "$TOK121" && -n "$HASH121" && -n "$TOK130" && -n "$HASH130" ]] || { echo "gen-token failed" >&2; exit 1; }

cat > "$TMP/server.toml" <<CFG
[server]
bind_addr = "127.0.0.1"
control_port = $CONTROL
tls_cert = "$TMP/certs/server.crt"
tls_key = "$TMP/certs/server.key"

[auth]
mode = "hmac"
clients = [
  { id = "old-121", tenant = "default", token_sha256 = "$HASH121" },
  { id = "old-130", tenant = "default", token_sha256 = "$HASH130" },
]

[ports]
range = [$PORTS_MIN, $PORTS_MAX]
release_policy = "immediate"

[admin]
bind = "127.0.0.1:$ADMIN"
panel = false

[log]
level = "info"
format = "text"
CFG

write_client() {
    local path="$1" id="$2" token="$3" remote="$4" echo_port="$5"
    cat > "$path" <<CFG
[client]
server_addr = "127.0.0.1:$CONTROL"
server_name = "localhost"
server_ca_cert = "$TMP/certs/server.crt"
client_id = "$id"
tenant = "default"
token = "$token"

[[proxy]]
name = "$id-http"
type = "tcp"
local_addr = "127.0.0.1:$echo_port"
remote_port = $remote

[log]
level = "info"
format = "text"
CFG
}
write_client "$TMP/client121.toml" "old-121" "$TOK121" "$REMOTE121" "$ECHO"
write_client "$TMP/client130.toml" "old-130" "$TOK130" "$REMOTE130" "$ECHO"

python3 -m http.server "$ECHO" --bind 127.0.0.1 --directory "$TMP/www" >"$TMP/http.log" 2>&1 & HTTP_PID=$!
"$SERVER" server --config "$TMP/server.toml" >"$TMP/server.log" 2>&1 & SERVER_PID=$!

for _ in $(seq 1 40); do
    if curl -fsS --max-time 2 "http://127.0.0.1:$ADMIN/healthz" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "server exited during startup" >&2; cat "$TMP/server.log"; exit 1
    fi
    sleep 0.5
done
curl -fsS --max-time 2 "http://127.0.0.1:$ADMIN/healthz" >/dev/null 2>&1 || {
    echo "server healthz did not become ready" >&2; cat "$TMP/server.log"; exit 1
}

"$OLD121" client --config "$TMP/client121.toml" >"$TMP/client121.log" 2>&1 & CLIENT121_PID=$!
"$OLD130" client --config "$TMP/client130.toml" >"$TMP/client130.log" 2>&1 & CLIENT130_PID=$!

wait_tunnel() {
    local label="$1" port="$2" pid="$3" log="$4"
    for _ in $(seq 1 40); do
        if curl -fsS --max-time 3 "http://127.0.0.1:$port/old.txt" 2>/dev/null | grep -qx 'old-client-ok'; then
            echo "PASS  $label  v1 handshake + proxy register + TCP forward"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "FAIL  $label client exited before tunnel became ready" >&2
            cat "$log" >&2
            return 1
        fi
        sleep 0.5
    done
    echo "FAIL  $label tunnel did not become ready within 20s" >&2
    echo "--- server log ---" >&2; cat "$TMP/server.log" >&2
    echo "--- client log ---" >&2; cat "$log" >&2
    return 1
}

wait_tunnel "1.2.1" "$REMOTE121" "$CLIENT121_PID" "$TMP/client121.log"
wait_tunnel "1.3.0" "$REMOTE130" "$CLIENT130_PID" "$TMP/client130.log"

# 服务端文本日志即使重定向仍带 ANSI 颜色码，先剥离再断言字段。
SERVER_PLAIN="$TMP/server.plain.log"
python3 - "$TMP/server.log" > "$SERVER_PLAIN" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
sys.stdout.write(re.sub(r"\x1b\[[0-9;]*m", "", text))
PY

# v1 协商必须显式留痕，且两个 client 都走 protocol=1；日志写入允许短暂延迟。
SERVER_VERSION="$("$SERVER" --version | awk '{print $2}')"
PROTOCOL_ONE_COUNT=0
for _ in $(seq 1 20); do
    PROTOCOL_ONE_COUNT="$(grep -c 'protocol=1' "$SERVER_PLAIN" || true)"
    [[ "${PROTOCOL_ONE_COUNT:-0}" -ge 2 ]] && break
    sleep 0.1
done
if [[ "$SERVER_VERSION" != "1.3.0" && "${PROTOCOL_ONE_COUNT:-0}" -lt 2 ]]; then
    echo "FAIL  server log missing two protocol=1 negotiations" >&2
    cat "$SERVER_PLAIN" >&2
    exit 1
fi
if [[ "$SERVER_VERSION" == "1.3.0" ]]; then
    CLIENT_FIELD='client='
else
    CLIENT_FIELD='client_id='
fi
if ! grep -q "${CLIENT_FIELD}old-121" "$SERVER_PLAIN" || ! grep -q "${CLIENT_FIELD}old-130" "$SERVER_PLAIN"; then
    echo "FAIL  server log missing old client ids" >&2
    cat "$SERVER_PLAIN" >&2
    exit 1
fi

echo "RESULT: PASS (1.2.1 / 1.3.0 -> current server, protocol=1)"
