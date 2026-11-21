#!/usr/bin/env bash
# E5-05 TCP 高并发回归门禁：连续多轮 32 并发 × 16KB，任一 worker 失败即阻断。
# 数据面与控制面必须分离；默认未启用连接池时也应有一条专用数据连接。
set -euo pipefail
cd "$(cd "$(dirname "$0")/.." && pwd)"
BIN="${MEATUS_BIN:-target/release/meatus}"
OUT="${BENCH_OUT:-bench-results}"
ROUNDS="${BENCH_ROUNDS:-3}"
CONCURRENCY="${BENCH_CONCURRENCY:-32}"
PAYLOAD="${BENCH_PAYLOAD:-16384}"
DURATION="${BENCH_DURATION:-10}"
mkdir -p "$OUT"

for round in $(seq 1 "$ROUNDS"); do
  echo "tcp concurrency round ${round}/${ROUNDS}: ${CONCURRENCY}x${PAYLOAD}B ${DURATION}s"
  python3 scripts/bench.py \
    --binary "$BIN" \
    --transport tcp \
    --duration "$DURATION" \
    --concurrency "$CONCURRENCY" \
    --payload "$PAYLOAD" \
    --out "$OUT/tcp-c${CONCURRENCY}-r${round}.json"
done

echo "tcp concurrency regression passed: ${ROUNDS} rounds"
