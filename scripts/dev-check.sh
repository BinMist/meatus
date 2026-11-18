#!/usr/bin/env bash
# 本地开发环境自检：工具链 + .env + 本地 Postgres/Redis。
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
FAIL=0

pass() { printf 'PASS  %s\n' "$1"; }
warn() { printf 'WARN  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; FAIL=1; }

echo "== meatus dev check =="
echo "root: $ROOT"

echo
echo "-- .env"
if [[ -f .env ]]; then
    while IFS='=' read -r key value; do
        [[ -z "$key" || "$key" == \#* ]] && continue
        export "$key=$value"
    done < .env
    pass ".env loaded"
else
    fail ".env missing (cp .env.example .env and fill real values)"
fi

echo
echo "-- rust toolchain"
if command -v rustc >/dev/null 2>&1; then
    pass "rustc $(rustc --version)"
else
    fail "rustc not found"
fi
if command -v cargo >/dev/null 2>&1; then
    pass "cargo $(cargo --version)"
else
    fail "cargo not found"
fi
if command -v protoc >/dev/null 2>&1; then
    pass "protoc $(protoc --version)"
else
    pass "protoc not installed (build uses protoc-bin-vendored, no system change needed)"
fi

echo
echo "-- postgres (docker: local-postgres)"
PG_USER="${MEATUS_PG_USER:-postgres}"
PG_DB="${MEATUS_PG_DB:-meatus}"
if docker inspect local-postgres >/dev/null 2>&1; then
    if docker exec local-postgres psql -U "$PG_USER" -d "$PG_DB" -tAc "select current_database()" >/dev/null 2>&1; then
        pass "postgres reachable (db=$PG_DB user=$PG_USER)"
    else
        fail "postgres not reachable (container exists, query failed)"
    fi
    BIND="$(docker inspect -f '{{range $p, $c := .NetworkSettings.Ports}}{{range $c}}{{.HostIp}} {{end}}{{end}}' local-postgres 2>/dev/null || true)"
    if [[ "$BIND" == *"0.0.0.0"* ]]; then
        warn "local-postgres publishes on 0.0.0.0; prefer 127.0.0.1 binding"
    fi
else
    fail "docker container local-postgres not found"
fi

echo
echo "-- redis (docker: local-redis)"
if docker inspect local-redis >/dev/null 2>&1; then
    if [[ "$(docker exec local-redis redis-cli ping 2>/dev/null)" == "PONG" ]]; then
        pass "redis reachable (PONG)"
    else
        fail "redis not responding to PING"
    fi
    BIND="$(docker inspect -f '{{range $p, $c := .NetworkSettings.Ports}}{{range $c}}{{.HostIp}} {{end}}{{end}}' local-redis 2>/dev/null || true)"
    if [[ "$BIND" == *"0.0.0.0"* ]]; then
        warn "local-redis publishes on 0.0.0.0; prefer 127.0.0.1 binding"
    fi
else
    fail "docker container local-redis not found"
fi

echo
echo "-- host ports"
for port in 504 505; do
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        exec 3>&-
        pass "127.0.0.1:$port accepting connections"
    else
        fail "127.0.0.1:$port not accepting connections"
    fi
done

echo
if [[ "$FAIL" -eq 0 ]]; then
    echo "RESULT: PASS"
    exit 0
else
    echo "RESULT: FAIL"
    exit 1
fi
