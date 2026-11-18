#!/usr/bin/env bash
# 运行 cargo-fuzz。需要 nightly + cargo-fuzz：
#   rustup toolchain install nightly
#   cargo install cargo-fuzz
# 用法：./scripts/fuzz.sh [秒数] [target...]
set -euo pipefail
cd "$(dirname "$0")/.."

SECS="${1:-60}"
shift || true
TARGETS=("$@")
if [[ ${#TARGETS[@]} -eq 0 ]]; then
    TARGETS=(decode_control_frame parse_config)
fi

command -v cargo-fuzz >/dev/null || { echo "cargo-fuzz 未安装：cargo install cargo-fuzz" >&2; exit 2; }
rustup run nightly rustc --version >/dev/null 2>&1 || { echo "nightly 未安装：rustup toolchain install nightly" >&2; exit 2; }

for target in "${TARGETS[@]}"; do
    echo "== fuzz $target for ${SECS}s =="
    (cd fuzz && cargo +nightly fuzz run "$target" -- -max_total_time="$SECS")
done
echo "fuzz done: no crash"
