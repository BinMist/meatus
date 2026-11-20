#!/usr/bin/env bash
# E7-02 下载/校验正式 Release fixture；离线模式只使用本地缓存。
set -euo pipefail
cd "$(dirname "$0")/.."
CACHE="${MEATUS_FIXTURE_CACHE:-$HOME/.cache/meatus-fixtures}"
MANIFEST="fixtures/release-assets.tsv"
mkdir -p "$CACHE"

target_triple() {
    case "$(uname -s):$(uname -m)" in
        Darwin:arm64) echo "aarch64-apple-darwin" ;;
        Darwin:x86_64) echo "x86_64-apple-darwin" ;;
        Linux:x86_64) echo "x86_64-unknown-linux-musl" ;;
        Linux:aarch64|Linux:arm64) echo "aarch64-unknown-linux-musl" ;;
        *) echo "unsupported host: $(uname -s)/$(uname -m)" >&2; return 1 ;;
    esac
}
TARGET="${MEATUS_FIXTURE_TRIPLE:-$(target_triple)}"
OFFLINE="${1:-}"
while IFS=$'\t' read -r version triple sha url; do
    [[ "$version" =~ ^# || -z "$version" ]] && continue
    [[ "$triple" == "$TARGET" ]] || continue
    name="$(basename "$url")"
    dir="$CACHE/${version}-${triple}"
    bin="$dir/meatus"
    if [[ ! -x "$bin" ]]; then
        if [[ "$OFFLINE" == "--offline" ]]; then
            echo "offline: missing cached fixture $bin" >&2
            exit 1
        fi
        tarball="$CACHE/$name"
        if [[ ! -f "$tarball" ]]; then
            echo "download $url"
            curl -fL --retry 3 -o "$tarball.part" "$url"
            mv "$tarball.part" "$tarball"
        fi
        if command -v sha256sum >/dev/null; then
            echo "$sha  $tarball" | sha256sum -c - >/dev/null
        else
            echo "$sha  $tarball" | shasum -a 256 -c - >/dev/null
        fi
        rm -rf "$dir" && mkdir -p "$dir"
        tar xzf "$tarball" -C "$dir"
        found="$(find "$dir" -type f -name meatus -perm -111 | head -n 1)"
        [[ -n "$found" ]] || { echo "meatus not found in $tarball" >&2; exit 1; }
        [[ "$found" == "$bin" ]] || cp "$found" "$bin"
        chmod +x "$bin"
    fi
    echo "$version $triple $bin"
done < "$MANIFEST"
