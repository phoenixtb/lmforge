#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  bundle-llamacpp.sh — Stage llama.cpp prebuilt binaries for the LMForge
#                      release tarballs.
#
#  Pulls platform-specific archives from the upstream ggml-org/llama.cpp
#  release page, SHA256-verifies each against `data/engines/llamacpp/SHA256SUMS`,
#  extracts the `llama-server` binary, and stages it under
#  `dist/bundled/llamacpp/<platform>/`.
#
#  Release CI calls this once per build-core matrix entry. The staged tree is
#  folded into the final release archive (alongside the `lmforge` binary) so
#  end users get a working default-tier engine without a network fetch at
#  install time.
#
#  PLATFORMS (4)
#    linux-x64-gpu      Vulkan build — works on NVIDIA + AMD + Intel iGPU
#    linux-x64-cpu      CPU-only, smaller
#    windows-x64-gpu    Vulkan build — works on NVIDIA + AMD + Intel iGPU
#    windows-x64-cpu    CPU-only
#
#  NOT BUNDLED (intentional)
#    macos-arm64        Apple Silicon uses MLX (omlx) as the default engine,
#                       not llama.cpp. No bundle needed.
#    linux-arm64        Upstream ships this; could be added later if demand
#                       arises. Out of scope for v0.3.
#    windows-cuda       Vulkan covers NVIDIA on Windows without the ~400 MB
#                       cudart DLL payload. NVIDIA-specific CUDA path can
#                       become an opt-in tier if peak perf is ever needed.
#
#  USAGE
#    scripts/bundle-llamacpp.sh                            # all 4 platforms
#    scripts/bundle-llamacpp.sh --version b9351            # override version
#    scripts/bundle-llamacpp.sh --platform linux-x64-gpu   # one platform only
#    scripts/bundle-llamacpp.sh --refresh-sums             # recompute SHA256SUMS
#
#  EXIT CODES
#    0    everything staged + verified
#    2    SHA256 mismatch (refusing to bundle)
#    3    download or extract error
#    4    bad arguments
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENGINES_TOML="${ROOT}/data/engines.toml"
SUMS_FILE="${ROOT}/data/engines/llamacpp/SHA256SUMS"
DIST_DIR="${ROOT}/dist/bundled/llamacpp"

VERSION=""             # resolved from engines.toml if not given
PLATFORM_FILTER=""     # empty = all
REFRESH_SUMS=0

ALL_PLATFORMS=(
    "linux-x64-gpu"
    "linux-x64-cpu"
    "windows-x64-gpu"
    "windows-x64-cpu"
)

# Map our platform tag → upstream asset filename.
asset_for_platform() {
    case "$1" in
        linux-x64-gpu)     echo "llama-${VERSION}-bin-ubuntu-vulkan-x64.tar.gz" ;;
        linux-x64-cpu)     echo "llama-${VERSION}-bin-ubuntu-x64.tar.gz" ;;
        windows-x64-gpu)   echo "llama-${VERSION}-bin-win-vulkan-x64.zip" ;;
        windows-x64-cpu)   echo "llama-${VERSION}-bin-win-cpu-x64.zip" ;;
        *) return 1 ;;
    esac
}

# Colors
G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; B='\033[1m'; N='\033[0m'
info()    { echo -e "${G}  ✓${N} $*"; }
warn()    { echo -e "${Y}  ⚠${N} $*"; }
error()   { echo -e "${R}  ✗${N} $*" >&2; exit "${2:-3}"; }
section() { echo -e "\n${B}$*${N}"; }

# ── Arg parsing ──────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)        VERSION="${2:?missing arg for --version}"; shift 2 ;;
        --platform)       PLATFORM_FILTER="${2:?missing arg for --platform}"; shift 2 ;;
        --refresh-sums)   REFRESH_SUMS=1; shift ;;
        -h|--help)
            grep -E '^#( |$)' "$0" | sed -E 's/^# ?//'
            exit 0
            ;;
        *) error "Unknown arg: $1" 4 ;;
    esac
done

# ── Resolve version from engines.toml if not given ───────────────────────────
if [[ -z "$VERSION" ]]; then
    # Reads the `version = "..."` line inside the llamacpp [[engine]] block.
    # The awk state machine: every [[engine]] header resets the in-block flag;
    # an `id = "llamacpp"` line latches it on; the next matching `version = "..."`
    # gets captured. `match` + `substr` avoid the greedy gsub trap of stripping
    # both quoted segments from the line.
    VERSION=$(awk '
        /^\[\[engine\]\]/                         { in_block = 0 }
        /^id[[:space:]]*=[[:space:]]*"llamacpp"/  { in_block = 1 }
        in_block && /^version[[:space:]]*=[[:space:]]*"/ {
            if (match($0, /"[^"]+"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                exit
            }
        }
    ' "$ENGINES_TOML")
    [[ -n "$VERSION" ]] || error "Could not infer version from engines.toml; pass --version explicitly" 4
fi

echo ""
echo -e "${B}  bundle-llamacpp${N}"
echo    "  ────────────────────────────"
echo    "  Version : $VERSION"
echo    "  Dist    : $DIST_DIR"
echo    "  Sums    : $SUMS_FILE"
if [[ -n "$PLATFORM_FILTER" ]]; then
    echo "  Filter  : $PLATFORM_FILTER"
fi
if [[ "$REFRESH_SUMS" == "1" ]]; then
    echo "  Mode    : REFRESH (rewriting SHA256SUMS)"
fi
echo ""

mkdir -p "$DIST_DIR" "$(dirname "$SUMS_FILE")"

# ── Per-platform download → verify → stage ───────────────────────────────────
PLATFORMS=()
if [[ -n "$PLATFORM_FILTER" ]]; then
    PLATFORMS=("$PLATFORM_FILTER")
else
    PLATFORMS=("${ALL_PLATFORMS[@]}")
fi

verify_sha() {
    # $1 = file, $2 = expected hex (or empty when in refresh mode)
    local file="$1" expected="$2"
    local actual
    if command -v sha256sum &>/dev/null; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    else
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    fi
    if [[ "$REFRESH_SUMS" == "1" || -z "$expected" ]]; then
        echo "$actual  $(basename "$file")"
        return 0
    fi
    if [[ "$actual" != "$expected" ]]; then
        error "SHA256 mismatch for $(basename "$file"): got $actual, expected $expected" 2
    fi
}

# Accumulate sums in refresh mode; commit atomically at the end.
TMP_NEW_SUMS="$(mktemp)"
trap 'rm -f "$TMP_NEW_SUMS"' EXIT

for platform in "${PLATFORMS[@]}"; do
    section "[$platform]"
    asset=$(asset_for_platform "$platform") || error "Unknown platform tag: $platform" 4
    url="https://github.com/ggml-org/llama.cpp/releases/download/${VERSION}/${asset}"

    work="$(mktemp -d)"
    trap 'rm -rf "$work"; rm -f "$TMP_NEW_SUMS"' EXIT
    archive="${work}/${asset}"

    echo    "  Asset: ${asset}"
    echo    "  URL  : ${url}"
    if ! curl -fSL --progress-bar "$url" -o "$archive"; then
        error "Download failed: $url" 3
    fi

    expected=""
    if [[ -f "$SUMS_FILE" && "$REFRESH_SUMS" != "1" ]]; then
        expected=$(awk -v want="$asset" '$2 == want {print $1; exit}' "$SUMS_FILE")
        if [[ -z "$expected" ]]; then
            warn "$asset not in SHA256SUMS — re-run with --refresh-sums after audit"
        fi
    fi
    new_sum=$(verify_sha "$archive" "$expected")
    if [[ "$REFRESH_SUMS" == "1" || -z "$expected" ]]; then
        echo "$new_sum" >> "$TMP_NEW_SUMS"
    fi
    info "Hash OK"

    # Extract and stage. Only the `llama-server` binary.
    extract_dir="${work}/extract"
    mkdir -p "$extract_dir"
    case "$archive" in
        *.tar.gz) tar -xzf "$archive" -C "$extract_dir" ;;
        *.zip)    unzip -q "$archive" -d "$extract_dir" ;;
        *) error "Unknown archive type: $archive" 3 ;;
    esac

    binary_name="llama-server"
    case "$platform" in *windows*) binary_name="llama-server.exe" ;; esac

    found=$(find "$extract_dir" -type f -name "$binary_name" | head -n 1 || true)
    [[ -n "$found" ]] || error "$binary_name not found inside $asset" 3
    upstream_root="$(dirname "$found")"

    stage_dir="${DIST_DIR}/${platform}"
    rm -rf "$stage_dir"
    mkdir -p "$stage_dir"

    # Starting around b8800+, upstream split `llama-server` into a ~20 KB
    # wrapper that dlopens `libllama-server-impl.{so,dll}` plus ~40 sibling
    # shared libs (libggml, libggml-cpu-*, libggml-vulkan, libllama, libmtmd,
    # libllama-common). All of them MUST be staged next to `llama-server` or
    # it dies on startup with "cannot open shared object file".
    #
    # We copy the whole upstream layout, then prune CLI tooling we don't ship
    # (llama-bench, llama-cli, llama-imatrix, etc.) to keep the bundle lean.
    # The system Vulkan loader (libvulkan.so.1 / vulkan-1.dll) still has to
    # come from the user's GPU driver install — that's by design.
    cp -P "$upstream_root"/* "$stage_dir/" 2>/dev/null || true

    # Prune CLI binaries we never invoke from lmforge. Leaves llama-server
    # and the libs it dlopens.
    case "$platform" in
        *windows*)
            find "$stage_dir" -maxdepth 1 -type f \
                \( -iname 'llama-*.exe' ! -iname 'llama-server.exe' \) -delete
            ;;
        *)
            find "$stage_dir" -maxdepth 1 -type f -executable \
                \( -name 'llama-*' ! -name 'llama-server' ! -name 'lib*' \) -delete
            # Also remove standalone helper binaries that aren't llama-*-named.
            for extra in rpc-server llama; do
                [[ -f "$stage_dir/$extra" ]] && rm -f "$stage_dir/$extra"
            done
            ;;
    esac

    chmod +x "$stage_dir/$binary_name" 2>/dev/null || true

    size=$(du -sh "$stage_dir" | awk '{print $1}')
    file_count=$(find "$stage_dir" -maxdepth 1 -type f | wc -l)
    info "Staged → ${stage_dir} (${size}, ${file_count} files)"
done

# Promote refreshed sums atomically.
if [[ "$REFRESH_SUMS" == "1" && -s "$TMP_NEW_SUMS" ]]; then
    LC_ALL=C sort -u "$TMP_NEW_SUMS" -o "$SUMS_FILE"
    info "Wrote $SUMS_FILE"
elif [[ ! -f "$SUMS_FILE" && -s "$TMP_NEW_SUMS" ]]; then
    LC_ALL=C sort -u "$TMP_NEW_SUMS" -o "$SUMS_FILE"
    warn "$SUMS_FILE did not exist — created from this run. Audit it before committing!"
fi

echo ""
echo -e "${B}${G}  ✓ Done.${N}"
echo    "  Staged under: $DIST_DIR"
echo    "  Manifest    : $SUMS_FILE"
echo ""
