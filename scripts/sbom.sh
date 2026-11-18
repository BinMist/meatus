#!/usr/bin/env bash
# 生成 Meatus 源码依赖 SPDX SBOM（Cargo.lock + web/package-lock.json）。
#
# 前置：安装 syft（https://github.com/anchore/syft）
#   brew install syft
#   # 或使用发布二进制，并把 syft 放到 PATH
#
# 用法：
#   ./scripts/sbom.sh
#   ./scripts/sbom.sh --out dist/sbom-meatus-1.1.0.spdx.json
#   SYFT=/path/to/syft ./scripts/sbom.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSION="$(awk '/^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)"
[[ -n "$VERSION" ]] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
OUT="dist/sbom-meatus-$VERSION.spdx.json"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) OUT="${2:?}"; shift 2 ;;
        -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

SYFT_BIN="${SYFT:-}"
if [[ -z "$SYFT_BIN" ]]; then
    SYFT_BIN="$(command -v syft || true)"
fi
if [[ -z "$SYFT_BIN" ]]; then
    echo "syft is required to generate the release SBOM." >&2
    echo "Install: https://github.com/anchore/syft#installation" >&2
    echo "Or set SYFT=/path/to/syft." >&2
    exit 1
fi

mkdir -p "$(dirname "$OUT")"
"$SYFT_BIN" scan dir:"$ROOT" \
    --source-name meatus \
    --source-version "$VERSION" \
    --exclude './target/**' \
    --exclude './web/node_modules/**' \
    --exclude './web/dist/**' \
    --exclude './dist/**' \
    --exclude './.git/**' \
    -o "spdx-json=$OUT"

echo "SPDX SBOM: $OUT"
if command -v jq >/dev/null 2>&1; then
    echo "packages:  $(jq '.packages | length' "$OUT")"
else
    echo "packages:  $(grep -c '"SPDXID": "SPDXRef-Package' "$OUT" || true)"
fi
