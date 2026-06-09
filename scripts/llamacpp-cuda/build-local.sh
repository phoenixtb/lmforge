#!/usr/bin/env bash
# Build llama.cpp CUDA variant tarballs locally via Docker (Rocky8 devel images).
#
# Prerequisites:
#   docker engine, images pulled:
#     nvidia/cuda:12.8.1-devel-rockylinux8
#     nvidia/cuda:13.1.0-devel-rockylinux8
#
# Usage:
#   scripts/llamacpp-cuda/build-local.sh --variant cuda12 --tag b9351
#   scripts/llamacpp-cuda/build-local.sh --variant all --tag b9351
#   LMFORGE_BUILD_JOBS=4 scripts/llamacpp-cuda/build-local.sh --variant cuda12
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VARIANT="cuda12"
TAG="b9351"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --variant) VARIANT="$2"; shift 2 ;;
    --tag)     TAG="$2"; shift 2 ;;
    -h|--help)
      sed -n '1,20p' "$0"
      exit 0
      ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v docker >/dev/null || { echo "docker required" >&2; exit 1; }

# shellcheck source=scripts/llamacpp-cuda/variants.conf
source "$ROOT/scripts/llamacpp-cuda/variants.conf"

run_one() {
  local v="$1"
  local image
  case "$v" in
    cuda12) image="$variant_cuda12_image" ;;
    cuda13) image="$variant_cuda13_image" ;;
    *) echo "bad variant: $v" >&2; return 2 ;;
  esac

  echo ""
  echo "── Docker build: $v (image=$image) ──"
  docker run --rm \
    -v "${ROOT}:/work:rw" \
    -v "lmforge-ccache-${v}:/work/.ccache" \
    -e CCACHE_DIR=/work/.ccache \
    -e LMFORGE_BUILD_JOBS="${LMFORGE_BUILD_JOBS:-4}" \
    -w /work \
    "$image" \
    bash scripts/llamacpp-cuda/build-variant.sh "$v" "$TAG"
}

mkdir -p "$ROOT/dist/llamacpp"

case "$VARIANT" in
  all)
    run_one cuda12
    run_one cuda13
    ;;
  cuda12|cuda13)
    run_one "$VARIANT"
    ;;
  *)
    echo "variant must be cuda12, cuda13, or all" >&2
    exit 2
    ;;
esac

echo ""
echo "Artifacts in $ROOT/dist/llamacpp/"
ls -lh "$ROOT/dist/llamacpp/"*.tar.gz 2>/dev/null || true
