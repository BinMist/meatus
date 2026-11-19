#!/usr/bin/env bash
# 构建并推送双仓库镜像：GHCR（canonical）+ 阿里云 ACR（国内镜像）。
#
# 前置：已登录两个 registry，或由 CI 的 login-action 完成登录。
#   docker login ghcr.io
#   docker login registry.cn-hangzhou.aliyuncs.com
#
# 用法：
#   ./scripts/docker-release.sh --version 1.2.0
#   ./scripts/docker-release.sh --version 1.2.0 --skip-acr
#   ./scripts/docker-release.sh --version 1.2.0 --platforms linux/amd64,linux/arm64
#
# Gitee Go 可直接把本脚本作为「镜像构建并推送」步骤；GHCR 与 ACR 的凭据放流水线 secret。
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSION=""
GHCR="ghcr.io/binsail/meatus"
ACR="registry.cn-hangzhou.aliyuncs.com/mist/meatus"
PLATFORMS="linux/amd64,linux/arm64"
PUSH=1
PUSH_GHCR=1
PUSH_ACR=1
SBOM=1
SYFT_BIN="${SYFT:-syft}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="${2:?}"; shift 2 ;;
        --ghcr) GHCR="${2:?}"; shift 2 ;;
        --acr) ACR="${2:?}"; shift 2 ;;
        --platforms) PLATFORMS="${2:?}"; shift 2 ;;
        --no-push) PUSH=0; shift ;;
        --skip-ghcr) PUSH_GHCR=0; shift ;;
        --skip-acr) PUSH_ACR=0; shift ;;
        --no-sbom) SBOM=0; shift ;;
        -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    VERSION="$(awk '/^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)"
fi
[[ -n "$VERSION" ]] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
MINOR="${VERSION%.*}"

TAGS=()
if [[ "$PUSH_GHCR" -eq 1 ]]; then
    TAGS+=(-t "$GHCR:$VERSION" -t "$GHCR:$MINOR" -t "$GHCR:latest")
fi
if [[ "$PUSH_ACR" -eq 1 ]]; then
    TAGS+=(-t "$ACR:$VERSION" -t "$ACR:$MINOR" -t "$ACR:latest")
fi
if [[ ${#TAGS[@]} -eq 0 ]]; then
    echo "both registries are disabled" >&2
    exit 2
fi

# ACR 不接受 buildx 的 OCI 1.1 attestation manifest（unknown manifest class
# application/vnd.oci.empty.v1+json）。因此只要推 ACR，就关闭 provenance / SBOM
# attestation，改为构建后用 syft 单独生成镜像 SBOM；仅推 GHCR 时保留 SBOM attestation。
ATTEST_ARGS=(--provenance=false --sbom=false)
if [[ "$PUSH_GHCR" -eq 1 && "$PUSH_ACR" -eq 0 ]]; then
    ATTEST_ARGS=(--provenance=false --attest "type=sbom")
fi

echo "== buildx image: version=$VERSION platforms=$PLATFORMS =="
if [[ "$PUSH" -eq 1 ]]; then
    docker buildx build \
        --platform "$PLATFORMS" \
        --build-arg VERSION="$VERSION" \
        "${ATTEST_ARGS[@]}" \
        "${TAGS[@]}" \
        --push .
elif [[ "$PLATFORMS" == *,* ]]; then
    # 多平台产物无法 --load 到本地 docker，改为导出 OCI 归档。
    mkdir -p dist
    OCI="dist/meatus-$VERSION-multi.oci.tar"
    rm -f "$OCI"
    docker buildx build \
        --platform "$PLATFORMS" \
        --build-arg VERSION="$VERSION" \
        "${TAGS[@]}" \
        --output "type=oci,dest=$OCI" .
    echo "OCI archive: $OCI"
else
    docker buildx build \
        --platform "$PLATFORMS" \
        --build-arg VERSION="$VERSION" \
        "${TAGS[@]}" \
        --load .
fi

digest_of() {
    docker buildx imagetools inspect "$1" 2>/dev/null \
        | awk '/^Digest:/ { print $2; exit }'
}

if [[ "$PUSH" -eq 1 && "$PUSH_GHCR" -eq 1 && "$PUSH_ACR" -eq 1 ]]; then
    GHCR_DIGEST="$(digest_of "$GHCR:$VERSION")"
    ACR_DIGEST="$(digest_of "$ACR:$VERSION")"
    echo "GHCR digest: ${GHCR_DIGEST:-unknown}"
    echo "ACR  digest: ${ACR_DIGEST:-unknown}"
    if [[ -z "$GHCR_DIGEST" || -z "$ACR_DIGEST" ]]; then
        echo "WARN: cannot read one of the manifest digests; check both registries manually" >&2
    elif [[ "$GHCR_DIGEST" != "$ACR_DIGEST" ]]; then
        echo "FAIL: GHCR and ACR manifest digests differ" >&2
        exit 1
    fi
fi

if [[ "$SBOM" -eq 1 ]]; then
    mkdir -p dist
    SBOM_IMAGE=""
    if [[ "$PUSH_GHCR" -eq 1 ]]; then
        SBOM_IMAGE="$GHCR:$VERSION"
    elif [[ "$PUSH_ACR" -eq 1 ]]; then
        SBOM_IMAGE="$ACR:$VERSION"
    fi
    if command -v "$SYFT_BIN" >/dev/null 2>&1 && [[ "$PUSH" -eq 1 && -n "$SBOM_IMAGE" ]]; then
        echo "== SBOM: $SBOM_IMAGE =="
        "$SYFT_BIN" "$SBOM_IMAGE" -o "spdx-json=dist/sbom-meatus-$VERSION-image.spdx.json"
    else
        echo "WARN: syft not installed or image not pushed; SBOM skipped" >&2
    fi
fi

echo
echo "image tags:"
for tag in "${TAGS[@]}"; do
    [[ "$tag" == "-t" ]] || echo "  $tag"
done
