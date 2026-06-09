#!/usr/bin/env bash
# Build one lmforge llama.cpp CUDA variant tarball inside the Rocky8 devel
# container. Called by build-local.sh (Docker on host) or CI workflow.
#
# Usage (inside container, repo mounted at /work):
#   bash scripts/llamacpp-cuda/build-variant.sh cuda12 b9351
#   bash scripts/llamacpp-cuda/build-variant.sh cuda13 b9351
#
# Outputs:
#   dist/llamacpp/lmforge-llamacpp-<tag>-<variant>-linux-x64.tar.gz
#   dist/llamacpp/lmforge-llamacpp-<tag>-<variant>-linux-x64.tar.gz.sha256
set -euo pipefail

VARIANT="${1:?variant required: cuda12|cuda13}"
TAG="${2:?llama.cpp tag required, e.g. b9351}"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# CI sets GITHUB_WORKSPACE; local Docker mount uses /work or repo root.
WORK="${GITHUB_WORKSPACE:-$ROOT}"
cd "$WORK"

# shellcheck source=scripts/llamacpp-cuda/variants.conf
source "$WORK/scripts/llamacpp-cuda/variants.conf"

case "$VARIANT" in
  cuda12)
    CUDA="$variant_cuda12_cuda"
    ARCHS="$variant_cuda12_archs"
    CUDART_SO="$variant_cuda12_cudart_so"
    CUBLAS_SO="$variant_cuda12_cublas_so"
    CUBLASLT_SO="$variant_cuda12_cublaslt_so"
    DRIVER_MIN="$variant_cuda12_driver_min"
    ;;
  cuda13)
    CUDA="$variant_cuda13_cuda"
    ARCHS="$variant_cuda13_archs"
    CUDART_SO="$variant_cuda13_cudart_so"
    CUBLAS_SO="$variant_cuda13_cublas_so"
    CUBLASLT_SO="$variant_cuda13_cublaslt_so"
    DRIVER_MIN="$variant_cuda13_driver_min"
    ;;
  *)
    echo "Unknown variant: $VARIANT" >&2
    exit 2
    ;;
esac

case "$CUDA" in
  13.2*|13.3*)
    echo "CUDA $CUDA is forbidden (Unsloth GGUF corruption)." >&2
    exit 1
    ;;
esac

echo "== build-variant: $VARIANT tag=$TAG cuda=$CUDA archs=$ARCHS =="

# ── Toolchain (Rocky 8 + EPEL) ───────────────────────────────────────────────
set -euxo pipefail
dnf -y install dnf-plugins-core epel-release git
/usr/bin/crb enable 2>/dev/null \
  || dnf config-manager --set-enabled crb 2>/dev/null \
  || dnf config-manager --set-enabled powertools 2>/dev/null \
  || true
dnf clean expire-cache
dnf -y install \
  cmake ninja-build gcc-toolset-12 \
  libcurl-devel patchelf tar gzip findutils which ccache

test -f /usr/local/cuda/lib64/stubs/libcuda.so \
  || { echo "CUDA stubs/libcuda.so missing" >&2; exit 1; }
nvcc --version

ln -sfv /usr/local/cuda/lib64/stubs/libcuda.so /usr/lib64/libcuda.so
ln -sfv /usr/local/cuda/lib64/stubs/libcuda.so /usr/lib64/libcuda.so.1
ldconfig -n /usr/lib64 || true

# ── llama.cpp source ─────────────────────────────────────────────────────────
LLAMA_DIR="$WORK/.build/llama.cpp-$TAG"
rm -rf "$LLAMA_DIR"
git clone --depth 1 --branch "$TAG" https://github.com/ggml-org/llama.cpp.git "$LLAMA_DIR"

export CCACHE_DIR="${CCACHE_DIR:-$WORK/.ccache}"
export CCACHE_MAXSIZE="${CCACHE_MAXSIZE:-5G}"
mkdir -p "$CCACHE_DIR"

# ── Compile ──────────────────────────────────────────────────────────────────
source /opt/rh/gcc-toolset-12/enable
ccache -z || true

cmake -S "$LLAMA_DIR" -B "$LLAMA_DIR/build" -G Ninja \
  -DGGML_CUDA=ON \
  -DGGML_NATIVE=OFF \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -DLLAMA_CURL=ON \
  -DGGML_CUDA_FA_ALL_QUANTS=ON \
  -DCMAKE_CUDA_ARCHITECTURES="$ARCHS" \
  -DCMAKE_EXE_LINKER_FLAGS="-Wl,-rpath,'\$ORIGIN/lib'" \
  -DCMAKE_C_COMPILER_LAUNCHER=ccache \
  -DCMAKE_CXX_COMPILER_LAUNCHER=ccache \
  -DCMAKE_CUDA_COMPILER_LAUNCHER=ccache

# 16 GB hosts: cap parallelism to reduce OOM during nvcc.
JOBS="${LMFORGE_BUILD_JOBS:-$(nproc)}"
cmake --build "$LLAMA_DIR/build" -j"$JOBS" \
  --target llama-server llama-cli llama-bench llama-quantize
ccache -s || true

# ── Stage tarball ────────────────────────────────────────────────────────────
name="lmforge-llamacpp-${TAG}-${VARIANT}-linux-x64"
out="$WORK/dist/llamacpp/${name}"
mkdir -p "$out/lib" "$WORK/dist/llamacpp"

for bin in llama-server llama-cli llama-bench llama-quantize; do
  cp "$LLAMA_DIR/build/bin/${bin}" "$out/${bin}"
done

for so in "$LLAMA_DIR/build/bin"/*.so "$LLAMA_DIR/build/bin"/*.so.*; do
  [ -f "$so" ] || continue
  cp -L "$so" "$out/lib/$(basename "$so")"
done

strip "$out"/llama-* "$out"/lib/*.so* 2>/dev/null || true

for so in "$CUDART_SO" "$CUBLAS_SO" "$CUBLASLT_SO"; do
  src="$(find -L \
    /usr/local/cuda /usr/local/cuda-*/ /usr/lib64 /usr/lib \
    -name "${so}*" -type f 2>/dev/null | head -n 1 || true)"
  if [ -z "$src" ]; then
    echo "missing ${so} in CUDA toolkit paths" >&2
    exit 1
  fi
  echo "Bundling $(basename "$src") (from $src)"
  cp -L "$src" "$out/lib/$(basename "$src")"
  base="$(basename "$src")"
  if [ "$base" != "$so" ] && [ ! -e "$out/lib/$so" ]; then
    ln -s "$base" "$out/lib/$so"
  fi
done

nccl_src="$(find -L \
  /usr/local/cuda /usr/local/cuda-*/ /usr/lib64 /usr/lib \
  -name 'libnccl.so.2' -type f 2>/dev/null | head -n 1 || true)"
if [ -n "$nccl_src" ]; then
  echo "Bundling $(basename "$nccl_src") (from $nccl_src)"
  cp -L "$nccl_src" "$out/lib/$(basename "$nccl_src")"
elif ldd "$LLAMA_DIR/build/bin/llama-server" 2>/dev/null | grep -q 'libnccl.so.2'; then
  echo "llama-server links libnccl.so.2 but no libnccl found" >&2
  exit 1
fi

for bin in llama-server llama-cli llama-bench llama-quantize; do
  patchelf --set-rpath '$ORIGIN/lib' "$out/$bin"
done
for so in "$out"/lib/*.so "$out"/lib/*.so.*; do
  [ -f "$so" ] || continue
  patchelf --set-rpath '$ORIGIN' "$so" 2>/dev/null || true
done

cat > "$out/VERSION" <<EOF
llamacpp_tag=${TAG}
cuda=${CUDA}
archs=${ARCHS}
driver_min=${DRIVER_MIN}
variant=${VARIANT}
EOF

missing="$(LD_LIBRARY_PATH="$out/lib" ldd "$out/llama-server" | grep 'not found' || true)"
if [ -n "$missing" ]; then
  echo "llama-server has unresolved NEEDED entries:" >&2
  echo "$missing" >&2
  LD_LIBRARY_PATH="$out/lib" ldd "$out/llama-server" >&2
  exit 1
fi

if LD_LIBRARY_PATH="$out/lib" ldd "$out/llama-server" \
    | grep -E '/(usr|opt)/.*lib(cublas|cudart)' >/dev/null; then
  echo "llama-server resolves cudart/cublas via system paths" >&2
  exit 1
fi

tar -czf "${out}.tar.gz" -C "$WORK/dist/llamacpp" "${name}"
sha256sum "${out}.tar.gz" | tee "${out}.tar.gz.sha256"

size_mb="$(du -m "${out}.tar.gz" | cut -f1)"
echo "tarball size = ${size_mb} MB"
if [ "$size_mb" -ge 1500 ]; then
  echo "tarball exceeds 1500 MB budget" >&2
  exit 1
fi

echo "== done: ${out}.tar.gz =="
