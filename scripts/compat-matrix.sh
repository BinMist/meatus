#!/usr/bin/env bash
# E7-01 兼容矩阵：1.2.1 / 1.3.0 / 1.4.0 客户端 × 1.3.0 / 1.4.0 服务端。
set -uo pipefail
cd "$(dirname "$0")/.."
CURRENT="${MEATUS_CURRENT:-target/release/meatus}"
CACHE="${MEATUS_FIXTURE_CACHE:-$HOME/.cache/meatus-fixtures}"
TRIPLE="$(uname -s):$(uname -m)"
case "$TRIPLE" in
    Darwin:arm64) TARGET=aarch64-apple-darwin ;;
    Darwin:x86_64) TARGET=x86_64-apple-darwin ;;
    Linux:x86_64) TARGET=x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-musl ;;
    *) echo "unsupported host $TRIPLE" >&2; exit 1 ;;
esac
OLD121="${MEATUS_OLD_121:-$CACHE/1.2.1-$TARGET/meatus}"
OLD130="${MEATUS_OLD_130:-$CACHE/1.3.0-$TARGET/meatus}"
for bin in "$CURRENT" "$OLD121" "$OLD130"; do
    [[ -x "$bin" ]] || { echo "missing $bin; run scripts/fetch-fixtures.sh" >&2; exit 1; }
done
LOG="${TMPDIR:-/tmp}/meatus-compat-matrix.$$.log"
RESULTS=()

run_case() {
    local client="$1" server="$2"; shift 2
    local label="$client client × $server server"
    echo "== $label =="
    if "$@" >"$LOG" 2>&1; then
        RESULTS+=("$client|$server|PASS")
        echo "PASS $label"
    else
        RESULTS+=("$client|$server|FAIL")
        echo "FAIL $label"
        tail -30 "$LOG" >&2
        return 1
    fi
}

fail=0
run_old_pair() {
    local server_label="$1" server_bin="$2"
    if env MEATUS_BINARY="$CURRENT" MEATUS_SERVER_BIN="$server_bin" MEATUS_OLD_121="$OLD121" MEATUS_OLD_130="$OLD130" bash scripts/compat-old-clients.sh >"$LOG" 2>&1; then
        RESULTS+=("1.2.1|$server_label|PASS")
        RESULTS+=("1.3.0|$server_label|PASS")
        echo "PASS 1.2.1 / 1.3.0 × $server_label"
    else
        RESULTS+=("1.2.1|$server_label|FAIL")
        RESULTS+=("1.3.0|$server_label|FAIL")
        echo "FAIL 1.2.1 / 1.3.0 × $server_label"
        tail -30 "$LOG" >&2
        fail=1
    fi
}
run_old_pair "1.4.0" "$CURRENT"
run_old_pair "1.3.0" "$OLD130"
run_case "1.4.0" "1.3.0" env MEATUS_BINARY="$CURRENT" MEATUS_OLD_SERVER="$OLD130" bash scripts/compat-v1-fallback.sh || fail=1
run_case "1.4.0" "1.4.0" cargo test --release -p meatus-server --test tunnel --offline tcp_tunnel_roundtrip -- --exact || fail=1

rm -f "$LOG"
echo
echo "| client | server | result |"
echo "| --- | --- | --- |"
for row in "${RESULTS[@]}"; do
    IFS='|' read -r client server result <<< "$row"
    printf '| %s | %s | %s |\n' "$client" "$server" "$result"
done
exit "$fail"
