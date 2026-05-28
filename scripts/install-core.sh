#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — Install Script
#
#  Downloads the pre-built bundle for the current platform, extracts it to
#  ~/.local/share/lmforge/bin/, and symlinks `lmforge` onto PATH. The bundle
#  ships both `lmforge` and a matching `llama-server` (the default chat tier)
#  so the user gets a working chat without any second download.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
#
#  Environment variables:
#    LMFORGE_VERSION     Pin a specific version, e.g. "v0.3.0" (default: latest)
#    LMFORGE_INSTALL_DIR Where to place the `lmforge` symlink (default: ~/.local/bin)
#    LMFORGE_BUNDLE_DIR  Where to extract the bundle  (default: ~/.local/share/lmforge/bin)
#    LMFORGE_VARIANT     Force a variant: "gpu" | "cpu" | "auto" (default: auto)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="phoenixtb/lmforge"
BINARY_NAME="lmforge"
INSTALL_DIR="${LMFORGE_INSTALL_DIR:-$HOME/.local/bin}"
BUNDLE_DIR="${LMFORGE_BUNDLE_DIR:-$HOME/.local/share/lmforge/bin}"
VERSION="${LMFORGE_VERSION:-latest}"
VARIANT="${LMFORGE_VARIANT:-auto}"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit 1; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# ── Detect GPU presence (Linux only — macOS Apple Silicon always uses MLX) ───
# Returns "gpu" if any discrete or integrated GPU is detected via lspci, else "cpu".
detect_linux_variant() {
    if [[ "$VARIANT" != "auto" ]]; then
        echo "$VARIANT"
        return
    fi
    # NVIDIA, AMD, and Intel iGPUs are all Vulkan-capable with proper drivers.
    if command -v nvidia-smi &>/dev/null && nvidia-smi -L 2>/dev/null | grep -q 'GPU'; then
        echo "gpu"
        return
    fi
    if command -v lspci &>/dev/null && lspci 2>/dev/null | grep -qiE 'vga|3d|display'; then
        # Filter out classic "Cirrus Logic" / virtualised display adapters that
        # have no real acceleration path.
        if lspci 2>/dev/null | grep -iE 'vga|3d|display' | grep -qvE 'cirrus|qemu|vmware svga|virtualbox'; then
            echo "gpu"
            return
        fi
    fi
    echo "cpu"
}

# ── Detect platform asset ────────────────────────────────────────────────────
# Returns "<basename> <ext>" so caller can build the archive name + know how
# to extract it.
detect_asset() {
    local os arch
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Darwin)
            case "$arch" in
                arm64)   echo "lmforge-macos-arm64 tar.gz" ;;
                x86_64)  error "macOS Intel (x86_64) is not supported. Apple Silicon (M1/M2/M3/M4) only." ;;
                *)       error "Unsupported macOS arch: $arch" ;;
            esac ;;
        Linux)
            case "$arch" in
                x86_64)
                    local v
                    v=$(detect_linux_variant)
                    if [[ "$v" == "gpu" ]]; then
                        echo "lmforge-linux-x64 tar.gz"
                    else
                        echo "lmforge-linux-x64-cpu tar.gz"
                    fi
                    ;;
                aarch64|arm64)
                    error "Linux ARM64 bundles aren't shipped yet. Tracking issue: https://github.com/$REPO/issues" ;;
                *)
                    error "Unsupported Linux arch: $arch" ;;
            esac ;;
        *)
            error "Unsupported OS: $os. For Windows, download lmforge-windows-x64.zip from https://github.com/$REPO/releases" ;;
    esac
}

# ── Resolve download URL ──────────────────────────────────────────────────────
resolve_url() {
    local asset="$1"
    if [[ "$VERSION" == "latest" ]]; then
        echo "https://github.com/${REPO}/releases/latest/download/${asset}"
    else
        echo "https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
    fi
}

# ── Banner ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}  LMForge Core — Installer${NC}"
echo    "  ─────────────────────────────────────────"
echo    "  Repo   : https://github.com/$REPO"
echo    "  Version: $VERSION"
echo    "  Symlink: $INSTALL_DIR/$BINARY_NAME"
echo    "  Bundle : $BUNDLE_DIR"
echo ""

# ── Idempotency check ─────────────────────────────────────────────────────────
if command -v "$BINARY_NAME" &>/dev/null; then
    INSTALLED_VER=$("$BINARY_NAME" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")
    warn "lmforge $INSTALLED_VER is already installed at $(command -v $BINARY_NAME)"
    warn "Use 'lmforge service status' to check the daemon."
    warn "To reinstall, first run: curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-core.sh | bash"
    exit 0
fi

# ── Prerequisites ─────────────────────────────────────────────────────────────
section "Checking prerequisites..."

for cmd in curl tar; do
    command -v "$cmd" &>/dev/null || error "'$cmd' is required but not installed."
done
info "curl, tar available"

mkdir -p "$INSTALL_DIR" "$BUNDLE_DIR" || error "Cannot create install/bundle dirs"
[[ -w "$INSTALL_DIR" ]] || error "$INSTALL_DIR is not writable"
[[ -w "$BUNDLE_DIR"  ]] || error "$BUNDLE_DIR is not writable"
info "Install dirs OK"

# ── Resolve + download ────────────────────────────────────────────────────────
section "Downloading bundle..."

read -r ASSET_BASE EXT < <(detect_asset)
ARCHIVE_NAME="${ASSET_BASE}.${EXT}"
URL=$(resolve_url "$ARCHIVE_NAME")

echo    "  Asset  : $ARCHIVE_NAME"
echo    "  URL    : $URL"
if [[ "$VARIANT" == "auto" ]] && [[ "$(uname -s)" == "Linux" ]]; then
    if [[ "$ASSET_BASE" == *-cpu ]]; then
        echo    "  Variant: CPU  (no GPU detected via lspci/nvidia-smi)"
    else
        echo    "  Variant: GPU  (Vulkan — covers NVIDIA + AMD + Intel)"
    fi
    echo    "  Override: LMFORGE_VARIANT=gpu|cpu"
fi
echo ""

TMP_ARCHIVE=$(mktemp --suffix=".${EXT}")
trap 'rm -f "$TMP_ARCHIVE"' EXIT

if ! curl -fSL --progress-bar "$URL" -o "$TMP_ARCHIVE"; then
    error "Download failed from $URL\n  Check https://github.com/$REPO/releases for available versions."
fi
info "Downloaded $ARCHIVE_NAME ($(du -h "$TMP_ARCHIVE" | cut -f1))"

# ── Extract into the bundle directory ─────────────────────────────────────────
section "Extracting bundle..."

# Clean any prior contents (idempotent reinstall after `uninstall-core.sh`).
rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR"

# The archive contains a top-level directory matching ASSET_BASE; --strip-components=1
# unpacks its contents directly into BUNDLE_DIR.
case "$EXT" in
    tar.gz) tar -xzf "$TMP_ARCHIVE" -C "$BUNDLE_DIR" --strip-components=1 ;;
    zip)
        command -v unzip &>/dev/null || error "'unzip' is required to extract this bundle."
        unzip -q "$TMP_ARCHIVE" -d "$BUNDLE_DIR"
        # Flatten the top-level directory if present.
        inner=$(find "$BUNDLE_DIR" -maxdepth 1 -mindepth 1 -type d | head -1)
        if [[ -n "$inner" ]]; then
            mv "$inner"/* "$BUNDLE_DIR/"
            rmdir "$inner"
        fi
        ;;
    *) error "Unknown archive extension: $EXT" ;;
esac

if [[ ! -x "$BUNDLE_DIR/$BINARY_NAME" ]]; then
    error "Extracted bundle is missing $BINARY_NAME — archive layout unexpected."
fi
chmod +x "$BUNDLE_DIR/$BINARY_NAME"
[[ -f "$BUNDLE_DIR/llama-server" ]] && chmod +x "$BUNDLE_DIR/llama-server"
info "Extracted to $BUNDLE_DIR"

# ── Symlink into PATH ─────────────────────────────────────────────────────────
section "Linking onto PATH..."

ln -sfn "$BUNDLE_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
info "Linked $INSTALL_DIR/$BINARY_NAME → $BUNDLE_DIR/$BINARY_NAME"

# Add INSTALL_DIR to PATH in every shell config if not already present.
add_to_path() {
    local profile_file="$1"
    local export_line="export PATH=\"$INSTALL_DIR:\$PATH\""
    if [[ -f "$profile_file" ]] && grep -qF "$INSTALL_DIR" "$profile_file"; then
        return  # already present
    fi
    if [[ -f "$profile_file" ]] || [[ "$profile_file" == "$HOME/.zshrc" ]] || [[ "$profile_file" == "$HOME/.bashrc" ]]; then
        echo "" >> "$profile_file"
        echo "# LMForge" >> "$profile_file"
        echo "$export_line" >> "$profile_file"
        info "Added $INSTALL_DIR to PATH in $profile_file"
    fi
}

add_to_path "$HOME/.zshrc"
add_to_path "$HOME/.bashrc"
add_to_path "$HOME/.profile"

# Make available in this session immediately
export PATH="$INSTALL_DIR:$PATH"

if ! command -v "$BINARY_NAME" &>/dev/null; then
    warn "Run: source ~/.zshrc  (or open a new terminal) to use lmforge from PATH"
fi

# Verify
INSTALLED_VER=$("$BUNDLE_DIR/$BINARY_NAME" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "?")
info "lmforge $INSTALLED_VER installed and working"
if [[ -x "$BUNDLE_DIR/llama-server" ]]; then
    info "Bundled llama-server present ($(du -sh "$BUNDLE_DIR" | cut -f1) total)"
fi

# ── NVIDIA driver info (Linux + NVIDIA only — informational) ──────────────────
# Default tier (bundled llama.cpp via Vulkan) needs only the NVIDIA driver —
# no nvcc, no Python, no pip. Driver provides libvulkan.so.1.
#
# nvcc is only required for the *opt-in* engines (vLLM, TabbyAPI/EXL3) which
# build CUDA kernels. Surface this so users can sanity-check what they have.
if [[ "$(uname -s)" == "Linux" ]] && command -v nvidia-smi &>/dev/null; then
    section "NVIDIA GPU detected..."
    DRIVER_VER=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 || echo "?")
    CUDA_RUNTIME=$(nvidia-smi 2>/dev/null | grep -oE 'CUDA Version: [0-9]+\.[0-9]+' | awk '{print $3}' | head -1)
    COMPUTE_CAP=$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -1 || echo "?")
    info "Driver $DRIVER_VER | CUDA runtime ${CUDA_RUNTIME:-?} | compute cap ${COMPUTE_CAP}"

    if [[ -n "${CUDA_RUNTIME:-}" ]]; then
        if ! command -v nvcc &>/dev/null && [[ -x /usr/local/cuda/bin/nvcc ]]; then
            warn "nvcc lives at /usr/local/cuda/bin/nvcc but is not on PATH."
            echo "  Add to your shell rc (only needed if you opt into vLLM or TabbyAPI later):"
            echo "    export PATH=/usr/local/cuda/bin:\$PATH"
            echo ""
        elif ! command -v nvcc &>/dev/null; then
            echo "  nvcc not found — that's fine for the default llama.cpp tier."
            echo "  Install it later only if you want vLLM or TabbyAPI (EXL3):"
            echo "    sudo apt-get install -y nvidia-cuda-toolkit   # (or your distro's equivalent)"
            echo ""
        fi
    fi
fi

# ── Init (first-time setup) ───────────────────────────────────────────────────
section "Initializing LMForge..."
"$BUNDLE_DIR/$BINARY_NAME" init

# ── Service install ───────────────────────────────────────────────────────────
section "Installing system service..."
"$BUNDLE_DIR/$BINARY_NAME" service install

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge Core $INSTALLED_VER installed successfully!${NC}"
echo ""
echo    "  The daemon is running and starts automatically on login."
echo    "  API:   http://127.0.0.1:11430"
echo    "  Logs:  ${HOME}/.lmforge/logs/daemon.out.log"
echo ""
echo    "  Quick start:"
echo    "    lmforge status              — show engine + model status"
echo    "    lmforge pull <model>        — download a model"
echo    "    lmforge service status      — show service health"
echo ""
echo    "  Opt-in engines (NVIDIA only):"
echo    "    lmforge engine list                  — see what's available"
echo    "    lmforge engine install vllm          — vLLM (Safetensors, ~5 GB)"
echo    "    lmforge engine install tabbyapi      — EXL3 via TabbyAPI (~3 GB)"
echo ""
echo    "  Install the desktop UI:"
echo    "    curl -fsSL https://github.com/$REPO/releases/latest/download/install-ui.sh | bash"
echo ""
