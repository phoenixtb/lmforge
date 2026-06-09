#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — Install Script
#  Downloads the pre-built binary from GitHub Releases, installs it to the
#  current user's local bin directory, adds it to PATH, runs init, and
#  registers the system service.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
#
#  Environment variables:
#    LMFORGE_VERSION     Pin a specific version, e.g. "v0.1.0" (default: latest)
#    LMFORGE_INSTALL_DIR Where to place the binary (default: ~/.local/bin)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="phoenixtb/lmforge"
BINARY_NAME="lmforge"
INSTALL_DIR="${LMFORGE_INSTALL_DIR:-$HOME/.local/bin}"
LMFORGE_RELEASE="${LMFORGE_VERSION:-latest}"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit 1; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# shellcheck source=banner.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/banner.sh" 2>/dev/null || true
if ! declare -F print_lmforge_banner &>/dev/null; then
    print_lmforge_banner() {
        echo ""
        echo "  ██╗     ███╗   ███╗███████╗ ██████╗ ██████╗  ██████╗ ███████╗"
        echo "  ██║     ████╗ ████║██╔════╝██╔═══██╗██╔══██╗██╔════╝ ██╔════╝"
        echo "  ██║     ██╔████╔██║█████╗  ██║   ██║██████╔╝██║  ███╗█████╗  "
        echo "  ██║     ██║╚██╔╝██║██╔══╝  ██║   ██║██╔══██╗██║   ██║██╔══╝  "
        echo "  ███████╗██║ ╚═╝ ██║██║     ╚██████╔╝██║  ██║╚██████╔╝███████╗"
        echo "  ╚══════╝╚═╝     ╚═╝╚═╝      ╚═════╝ ╚═╝  ╚═╝ ╚═════╝ ╚══════╝"
        echo ""
        echo "  ${1:-Hardware-aware LLM inference orchestrator}"
        echo ""
    }
fi

# Stop daemon/service so we can overwrite the binary (ETXTBSY / "Text file busy").
stop_running_lmforge_for_install() {
    export PATH="$INSTALL_DIR:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
    if command -v "$BINARY_NAME" &>/dev/null; then
        "$BINARY_NAME" service stop 2>/dev/null || true
        "$BINARY_NAME" stop 2>/dev/null || true
    fi
    systemctl --user stop lmforge.service 2>/dev/null || true
    curl -sf -X POST --max-time 3 http://127.0.0.1:11430/lf/shutdown 2>/dev/null || true
    sleep 1
    pkill -x "$BINARY_NAME" 2>/dev/null || true
    sleep 1
}

TARGET_BIN="$INSTALL_DIR/$BINARY_NAME"
detect_asset() {
    local os arch
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Darwin)
            case "$arch" in
                arm64)   echo "lmforge-macos-arm64" ;;
                x86_64)  echo "lmforge-macos-x86_64" ;;
                *)        error "Unsupported macOS arch: $arch" ;;
            esac ;;
        Linux)
            case "$arch" in
                x86_64)  echo "lmforge-linux-x86_64" ;;
                aarch64) echo "lmforge-linux-arm64" ;;
                *)        error "Unsupported Linux arch: $arch" ;;
            esac ;;
        *)
            error "Unsupported OS: $os. For Windows, run:\n  irm https://github.com/$REPO/releases/latest/download/install-core.ps1 | iex" ;;
    esac
}

# ── Resolve download URL ──────────────────────────────────────────────────────
resolve_url() {
    local asset="$1"
    if [[ "$LMFORGE_RELEASE" == "latest" ]]; then
        echo "https://github.com/${REPO}/releases/latest/download/${asset}"
    else
        echo "https://github.com/${REPO}/releases/download/${LMFORGE_RELEASE}/${asset}"
    fi
}

# ── Banner ────────────────────────────────────────────────────────────────────
print_lmforge_banner "LMForge Core — Installer"
echo    "  Repo   : https://github.com/$REPO"
echo    "  Version: $LMFORGE_RELEASE"
echo    "  Install: $INSTALL_DIR/$BINARY_NAME"
echo ""

# ── Idempotency check ─────────────────────────────────────────────────────────
export PATH="$INSTALL_DIR:$HOME/.local/bin:$PATH"
if [[ -x "$TARGET_BIN" ]] && [[ "${LMFORGE_UPGRADE:-0}" != "1" ]]; then
    INSTALLED_VER=$("$TARGET_BIN" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")
    warn "lmforge $INSTALLED_VER is already installed at $TARGET_BIN"
    warn "Use 'lmforge service status' to check the daemon."
    warn "To upgrade in place: LMFORGE_UPGRADE=1 curl -fsSL .../install-core.sh | bash"
    warn "To reinstall clean: bash <(curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-core.sh)"
    exit 0
fi
if [[ -x "$TARGET_BIN" ]] && [[ "${LMFORGE_UPGRADE:-0}" == "1" ]]; then
    section "Upgrading lmforge..."
    stop_running_lmforge_for_install
fi

# ── Prerequisites ─────────────────────────────────────────────────────────────
section "Checking prerequisites..."

for cmd in curl; do
    command -v "$cmd" &>/dev/null || error "'$cmd' is required but not installed."
done
info "curl available"

# Ensure install dir exists and is writable (no sudo — user-local only)
if [[ ! -d "$INSTALL_DIR" ]]; then
    mkdir -p "$INSTALL_DIR" || error "Cannot create $INSTALL_DIR"
fi
if [[ ! -w "$INSTALL_DIR" ]]; then
    error "$INSTALL_DIR is not writable. Try: LMFORGE_INSTALL_DIR=~/bin bash <(curl ...)"
fi
info "Install dir: $INSTALL_DIR"

# ── Download ──────────────────────────────────────────────────────────────────
section "Downloading lmforge..."

ASSET=$(detect_asset)
URL=$(resolve_url "$ASSET")

echo    "  Asset:  $ASSET"
echo    "  URL:    $URL"
echo ""

TMP_BIN=$(mktemp)
trap 'rm -f "$TMP_BIN"' EXIT

if ! curl -fSL --progress-bar "$URL" -o "$TMP_BIN"; then
    error "Download failed from $URL\n  Check https://github.com/$REPO/releases for available versions."
fi
info "Downloaded $ASSET"

# ── Install ───────────────────────────────────────────────────────────────────
section "Installing..."

chmod +x "$TMP_BIN"
if [[ -e "$TARGET_BIN" ]] || pgrep -x "$BINARY_NAME" &>/dev/null; then
    stop_running_lmforge_for_install
fi
cp "$TMP_BIN" "$TARGET_BIN"
info "Installed $TARGET_BIN"

# ── PATH injection ────────────────────────────────────────────────────────────
# Add INSTALL_DIR to PATH in every shell config if not already present.
add_to_path() {
    local profile_file="$1"
    local begin="# >>> LMForge >>>"
    local end="# <<< LMForge <<<"
    local export_line="export PATH=\"$INSTALL_DIR:\$PATH\""
    # Idempotent: skip if our managed block (sentinel or legacy comment) is
    # already present. We intentionally do NOT skip merely because INSTALL_DIR
    # appears elsewhere (e.g. the stock ~/.profile "$HOME/.local/bin" line) —
    # that line is the user's, not ours, and must be left untouched.
    if [[ -f "$profile_file" ]] && grep -qE "^# >>> LMForge >>>$|^# LMForge$" "$profile_file"; then
        return
    fi
    # Only write to files that exist OR the primary shell rc
    if [[ -f "$profile_file" ]] || [[ "$profile_file" == "$HOME/.zshrc" ]] || [[ "$profile_file" == "$HOME/.bashrc" ]]; then
        {
            echo ""
            echo "$begin"
            echo "$export_line"
            echo "$end"
        } >> "$profile_file"
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
INSTALLED_VER=$("$INSTALL_DIR/$BINARY_NAME" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "?")
info "lmforge $INSTALLED_VER installed and working"

# ── NVIDIA driver info (Linux + NVIDIA only — informational) ──────────────────
# The default engine (llama.cpp, pulled at init) needs nothing more than the
# NVIDIA driver — no `nvcc`, no Python, no pip. We surface the driver version
# and CUDA-runtime info here purely so users can sanity-check what they have.
#
# The CUDA toolkit (`nvcc`) is only required for the *opt-in* engines that
# build CUDA kernels (vLLM, TabbyAPI/EXL3). Default llama.cpp users don't need it.
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
"$INSTALL_DIR/$BINARY_NAME" init

# ── Service install ───────────────────────────────────────────────────────────
section "Installing system service..."
"$INSTALL_DIR/$BINARY_NAME" service install

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
echo    "  Install the desktop UI:"
echo    "    macOS/Linux: curl -fsSL https://github.com/$REPO/releases/latest/download/install-ui.sh | bash"
echo    "    Windows:     irm https://github.com/$REPO/releases/latest/download/install-ui.ps1 | iex"
echo ""
echo    "  Uninstall:"
echo    "    UI only:  curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-ui.sh | bash"
echo    "    Core:     curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-core.sh | bash"
echo    "    Purge:    curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-core.sh | bash -s -- --purge"
echo ""
