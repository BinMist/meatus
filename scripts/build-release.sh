#!/usr/bin/env bash
# 构建 V1 可分发产物：meatus-<version>-<target>.tar.gz + .sha256 + SPDX SBOM
#
# 本机（macOS）默认打当前主机包；Linux 静态包需指定 musl target：
#   rustup target add x86_64-unknown-linux-musl
#   ./scripts/build-release.sh --target x86_64-unknown-linux-musl
# 依赖 syft 生成 SBOM；本地快速打包可用 --no-sbom。
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
TARGET="$(rustc -vV | awk '/host:/ { print $2 }')"
OUT_DIR="$ROOT/dist"
BUILD_WEB=1
BUILD_SBOM=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET="${2:?}"; shift 2 ;;
        --out) OUT_DIR="${2:?}"; shift 2 ;;
        --no-web) BUILD_WEB=0; shift ;;
        --no-sbom) BUILD_SBOM=0; shift ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

VERSION="$(awk '/^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)"
[[ -n "$VERSION" ]] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }

if [[ "$BUILD_WEB" -eq 1 ]]; then
    if [[ ! -f web/dist/index.html ]]; then
        echo "== build web panel =="
        (cd web && npm ci --no-audit --no-fund && npm run build)
    else
        echo "== reuse web/dist =="
    fi
fi

echo "== build release: target=$TARGET =="
cargo build --release --locked --bin meatus --target "$TARGET"
BIN="$ROOT/target/$TARGET/release/meatus"
[[ -x "$BIN" ]] || { echo "missing $BIN" >&2; exit 1; }

# 能 strip 就 strip；失败只告警，不影响产物。
if file "$BIN" | grep -q 'ELF'; then
    if command -v llvm-strip >/dev/null 2>&1; then
        llvm-strip "$BIN" || echo "WARN: llvm-strip failed, keep unstripped" >&2
    elif command -v strip >/dev/null 2>&1; then
        strip "$BIN" || echo "WARN: strip failed, keep unstripped" >&2
    fi
else
    strip "$BIN" 2>/dev/null || echo "WARN: strip failed, keep unstripped" >&2
fi

SIZE="$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN")"
if [[ "$TARGET" == *musl* && "$SIZE" -gt 20971520 ]]; then
    echo "FAIL: musl binary is $SIZE bytes (> 20 MiB)" >&2
    exit 1
fi

SBOM="$OUT_DIR/sbom-meatus-$VERSION.spdx.json"
if [[ "$BUILD_SBOM" -eq 1 ]]; then
    echo "== generate SPDX SBOM =="
    ./scripts/sbom.sh --out "$SBOM"
elif [[ ! -f "$SBOM" ]]; then
    echo "FAIL: --no-sbom requires an existing $SBOM" >&2
    exit 1
fi

PKG="$OUT_DIR/meatus-$VERSION-$TARGET"
rm -rf "$PKG"
mkdir -p "$PKG/deploy/systemd" "$PKG/config" "$PKG/scripts"
install -m 0755 "$BIN" "$PKG/meatus"
install -m 0644 config/server.example.toml config/client.example.toml "$PKG/config/"
install -m 0644 config/server.example.yaml config/client.example.yaml "$PKG/config/"
install -m 0644 deploy/systemd/*.service "$PKG/deploy/systemd/"
install -m 0755 scripts/install.sh "$PKG/scripts/install.sh"
install -m 0644 README.md docs/INSTALL.md "$PKG/"
install -m 0644 LICENSE THIRD_PARTY_NOTICES.md "$PKG/"
install -m 0644 docs/THIRD_PARTY_LICENSES.md docs/THIRD_PARTY_LICENSE_TEXTS.txt "$PKG/"
install -m 0644 "$SBOM" "$PKG/SBOM.spdx.json"

TAR="$PKG.tar.gz"
tar -C "$OUT_DIR" -czf "$TAR" "$(basename "$PKG")"
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$OUT_DIR" && sha256sum "$(basename "$TAR")" > "$(basename "$TAR").sha256")
else
    (cd "$OUT_DIR" && shasum -a 256 "$(basename "$TAR")" > "$(basename "$TAR").sha256")
fi

echo
echo "package: $TAR"
echo "size:    $(( SIZE / 1024 )) KiB (binary)"
echo "sha256:  $(cat "$TAR.sha256")"
