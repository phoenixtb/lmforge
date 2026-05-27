#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  bundle-llamacpp.sh — Stage llama.cpp prebuilt binaries for the LMForge
#                      release tarball.
#
#  Pulls platform-specific archives from the upstream ggml-org/llama.cpp
#  release page, SHA256-verifies each against `data/engines/llamacpp/SHA256SUMS`,
#  extracts the `llama-server` binary (+ on Windows CUDA, the `cudart` DLLs),
#  and stages them under `dist/bundled/llamacpp/<platform>/`.
#
#  Release CI runs this once per LMForge release. The staged tree is folded
#  into the GitHub release tarball alongside the `lmforge` binary, so end
#  users get a working chat-tier engine without a network fetch at install
#  time.
#
#  USAGE
#    scripts/bundle-llamacpp.sh                     # all platforms, version pinned in engines.toml
#    scripts/bundle-llamacpp.sh --version b9351     # override version
#    scripts/bundle-llamacpp.sh --platform linux-x64-cuda   # one platform only
#    scripts/bundle-llamacpp.sh --refresh-sums      # recompute SHA256SUMS (use sparingly)
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

VERSION=""             # resolved below
PLATFORM_FILTER=""     # empty = all
REFRESH_SUMS=0

ALL_PLATFORMS=(
    "linux-x64-cuda"
    "linux-x64-cpu"
    "windows-x64-cuda"
    "windows-x64-cpu"
    "macos-arm64-metal"
)

# Map our platform tag → upstream asset name (without extension).
asset_for_platform() {
    local p="$1"
    case "$p" in
        # Linux upstream releases no longer ship CUDA prebuilts (dropped around
        # b8370). Vulkan is the GPU path on Linux; covers NVIDIA + AMD + Intel.
        linux-x64-cuda)     echo "llama-${VERSION}-bin-ubuntu-vulkan-x64.tar.gz" ;;
        linux-x64-cpu)      echo "llama-${VERSION}-bin-ubuntu-x64.tar.gz" ;;
        # Windows CUDA: upstream ships a 12.4 and a 13.1 variant.
        # We bundle BOTH so the installer can pick at runtime (per driver).
        windows-x64-cuda)   echo "llama-${VERSION}-bin-win-cuda-12.4-x64.zip" ;;  # paired w/ 13.1 below
        windows-x64-cpu)    echo "llama-${VERSION}-bin-win-cpu-x64.zip" ;;
        macos-arm64-metal)  echo "llama-${VERSION}-bin-macos-arm64.tar.gz" ;;
        *) return 1 ;;
    esac
}

cudart_for_platform() {
    case "$1" in
        windows-x64-cuda) echo "cudart-llama-bin-win-cuda-12.4-x64.zip" ;;
        *) echo "" ;;
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
    # Reads the first `version = "..."` line inside the llamacpp [[engine]] block.
    VERSION=$(awk '
        /^\[\[engine\]\]/        { in_block = 0 }
        /id[[:space:]]*=[[:space:]]*"llamacpp"/ { in_block = 1 }
        in_block && /^version[[:space:]]*=/ {
            gsub(/[^"]*"|"[[:space:]]*$/, "", $0)
            print $0
            exit
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

# Build a temp manifest in refresh mode and overwrite SUMS at the end.
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

    # Windows CUDA: also pull the cudart DLL companion archive.
    cudart=$(cudart_for_platform "$platform")
    if [[ -n "$cudart" ]]; then
        cudart_url="https://github.com/ggml-org/llama.cpp/releases/download/${VERSION}/${cudart}"
        cudart_path="${work}/${cudart}"
        echo "  cudart: ${cudart}"
        if ! curl -fSL --progress-bar "$cudart_url" -o "$cudart_path"; then
            error "Download failed: $cudart_url" 3
        fi
        cudart_expected=""
        if [[ -f "$SUMS_FILE" && "$REFRESH_SUMS" != "1" ]]; then
            cudart_expected=$(awk -v want="$cudart" '$2 == want {print $1; exit}' "$SUMS_FILE")
        fi
        new_cudart_sum=$(verify_sha "$cudart_path" "$cudart_expected")
        if [[ "$REFRESH_SUMS" == "1" || -z "$cudart_expected" ]]; then
            echo "$new_cudart_sum" >> "$TMP_NEW_SUMS"
        fi
        info "cudart hash OK"
    fi

    # Extract and stage. Only the `llama-server` binary (plus DLLs on Win-CUDA).
    extract_dir="${work}/extract"
    mkdir -p "$extract_dir"
    case "$archive" in
        *.tar.gz) tar -xzf "$archive" -C "$extract_dir" ;;
        *.zip)    unzip -q "$archive" -d "$extract_dir" ;;
        *) error "Unknown archive type: $archive" 3 ;;
    esac
    if [[ -n "$cudart" ]]; then
        unzip -qo "${work}/${cudart}" -d "$extract_dir"
    fi

    binary_name="llama-server"
    case "$platform" in *windows*) binary_name="llama-server.exe" ;; esac

    found=$(find "$extract_dir" -type f -name "$binary_name" | head -n 1 || true)
    [[ -n "$found" ]] || error "$binary_name not found inside $asset" 3

    stage_dir="${DIST_DIR}/${platform}"
    rm -rf "$stage_dir"
    mkdir -p "$stage_dir"
    cp "$found" "$stage_dir/"
    chmod +x "$stage_dir/$binary_name" || true

    # On Windows CUDA, ship all DLLs alongside the binary so the runtime works
    # without requiring a separately-installed CUDA toolkit.
    if [[ -n "$cudart" ]]; then
        find "$extract_dir" -type f -iname '*.dll' -exec cp -n {} "$stage_dir/" \;
    fi

    info "Staged → ${stage_dir}/${binary_name}"
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
