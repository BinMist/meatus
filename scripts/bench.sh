#!/usr/bin/env bash
# E5-01/E5-02 基准入口：当前二进制 TCP + QUIC（可选 1.3.0 TCP 基线）。
set -euo pipefail
cd "$(cd "$(dirname "$0")/.." && pwd)"
BIN="${MEATUS_BIN:-target/release/meatus}"
OUT="${BENCH_OUT:-bench-results}"
DURATION="${BENCH_DURATION:-5}"
CONCURRENCY="${BENCH_CONCURRENCY:-8}"
PAYLOAD="${BENCH_PAYLOAD:-4096}"
mkdir -p "$OUT"
python3 scripts/bench.py --binary "$BIN" --transport tcp --duration "$DURATION" --concurrency "$CONCURRENCY" --payload "$PAYLOAD" --out "$OUT/tcp.json"
python3 scripts/bench.py --binary "$BIN" --transport quic --duration "$DURATION" --concurrency "$CONCURRENCY" --payload "$PAYLOAD" --out "$OUT/quic.json"
if [[ -n "${MEATUS_130_BIN:-}" ]]; then
  python3 scripts/bench.py --binary "$MEATUS_130_BIN" --transport tcp --duration "$DURATION" --concurrency "$CONCURRENCY" --payload "$PAYLOAD" --out "$OUT/tcp-1.3.0.json"
fi
python3 scripts/bench-gate.py --tcp "$OUT/tcp.json" --quic "$OUT/quic.json"
