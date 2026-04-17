#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — Install Script
#  Downloads the pre-built binary from GitHub Releases, installs it,
#  runs init, and registers the system service.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
#
#  Environment variables:
#    LMFORGE_VERSION     Pin a specific version, e.g. "v0.3.1" (default: latest)
#    LMFORGE_INSTALL_DIR Where to place the binary (default: /usr/local/bin)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="phoenixtb/lmforge"
BINARY_NAME="lmforge"
INSTALL_DIR="${LMFORGE_INSTALL_DIR:-/usr/local/bin}"
VERSION="${LMFORGE_VERSION:-latest}"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit 1; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# ── Detect platform ───────────────────────────────────────────────────────────
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
            error "Unsupported OS: $os. For Windows, download lmforge-windows-x86_64.exe from GitHub Releases." ;;
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
echo    "  Install: $INSTALL_DIR/$BINARY_NAME"
echo ""

# ── Idempotency check ─────────────────────────────────────────────────────────
if command -v "$BINARY_NAME" &>/dev/null; then
    INSTALLED_VER=$("$BINARY_NAME" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")
    warn "lmforge $INSTALLED_VER is already installed at $(command -v $BINARY_NAME)"
    warn "Use 'lmforge service status' to check the daemon."
    warn "To reinstall, run: bash <(curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-core.sh)"
    exit 0
fi

# ── Prerequisites ─────────────────────────────────────────────────────────────
section "Checking prerequisites..."

for cmd in curl; do
    command -v "$cmd" &>/dev/null || error "'$cmd' is required but not installed."
done
info "curl available"

# Ensure install dir exists and is writable
if [[ ! -d "$INSTALL_DIR" ]]; then
    mkdir -p "$INSTALL_DIR" 2>/dev/null || \
        error "Cannot create $INSTALL_DIR. Try: sudo mkdir -p $INSTALL_DIR && sudo chown \$(whoami) $INSTALL_DIR"
fi
if [[ ! -w "$INSTALL_DIR" ]]; then
    error "$INSTALL_DIR is not writable.\nTry: sudo chown \$(whoami) $INSTALL_DIR\nOr:  LMFORGE_INSTALL_DIR=~/.local/bin bash <(curl ...)"
fi
info "Install dir writable: $INSTALL_DIR"

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
cp "$TMP_BIN" "$INSTALL_DIR/$BINARY_NAME"
info "Installed $INSTALL_DIR/$BINARY_NAME"

# Check PATH
if ! command -v "$BINARY_NAME" &>/dev/null; then
    warn "lmforge is not yet on PATH. Add this to your shell profile:"
    warn "  export PATH=\"$INSTALL_DIR:\$PATH\""
    warn "Then reload: source ~/.zshrc  (or ~/.bashrc)"
    export PATH="$INSTALL_DIR:$PATH"
fi

# Verify
INSTALLED_VER=$("$INSTALL_DIR/$BINARY_NAME" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "?")
info "lmforge $INSTALLED_VER installed and working"

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
echo    "    curl -fsSL https://github.com/$REPO/releases/latest/download/install-ui.sh | bash"
echo ""
