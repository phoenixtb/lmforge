#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge UI — Install Script
#  Downloads the LMForge desktop app from GitHub Releases and installs it.
#  Requires LMForge Core to be installed first.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash
#
#  Environment variables:
#    LMFORGE_VERSION   Pin a specific version, e.g. "v0.3.1" (default: latest)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="phoenixtb/lmforge"
VERSION="${LMFORGE_VERSION:-latest}"
MIN_CORE_VERSION="0.1.0"

# macOS — install to user Applications (no sudo required)
APP_NAME="LMForge"
APP_BUNDLE="${HOME}/Applications/${APP_NAME}.app"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit 1; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# ── Detect platform ───────────────────────────────────────────────────────────
OS=$(uname -s)
ARCH=$(uname -m)

detect_ui_asset() {
    case "$OS" in
        Darwin)
            case "$ARCH" in
                arm64)   echo "LMForge-UI-macos-arm64.dmg" ;;
                x86_64)  echo "LMForge-UI-macos-x86_64.dmg" ;;
                *)        error "Unsupported macOS arch: $ARCH" ;;
            esac ;;
        Linux)
            case "$ARCH" in
                x86_64)  echo "LMForge-UI-linux-x86_64.AppImage" ;;
                *)        error "Unsupported Linux arch: $ARCH. Only x86_64 is supported currently." ;;
            esac ;;
        *)
            error "Unsupported OS: $OS. For Windows, download the NSIS installer (.exe) from GitHub Releases." ;;
    esac
}

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
echo -e "${BOLD}  LMForge UI — Installer${NC}"
echo    "  ─────────────────────────────────────────"
echo    "  Repo   : https://github.com/$REPO"
echo    "  Version: $VERSION"
echo    "  OS/Arch: $OS/$ARCH"
echo ""

# ── Idempotency check ─────────────────────────────────────────────────────────
if [[ "$OS" == "Darwin" && -d "$APP_BUNDLE" ]]; then
    warn "LMForge.app already installed at $APP_BUNDLE"
    warn "To update, uninstall first: curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-ui.sh | bash"
    warn "Opening existing app..."
    open "$APP_BUNDLE" 2>/dev/null || true
    exit 0
fi

# ── Prerequisite: Core must be installed ─────────────────────────────────────
section "Checking LMForge Core..."

if ! command -v lmforge &>/dev/null; then
    error "LMForge Core not found. Install it first:\n  curl -fsSL https://github.com/$REPO/releases/latest/download/install-core.sh | bash"
fi

CORE_VER=$(lmforge --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "0.0.0")
info "Core version: $CORE_VER"

# Semver check (major.minor)
CORE_MAJOR=$(echo "$CORE_VER"     | cut -d. -f1)
CORE_MINOR=$(echo "$CORE_VER"     | cut -d. -f2)
MIN_MAJOR=$(echo "$MIN_CORE_VERSION"  | cut -d. -f1)
MIN_MINOR=$(echo "$MIN_CORE_VERSION"  | cut -d. -f2)

if (( CORE_MAJOR < MIN_MAJOR )) || (( CORE_MAJOR == MIN_MAJOR && CORE_MINOR < MIN_MINOR )); then
    error "Core $CORE_VER is too old. UI requires >= $MIN_CORE_VERSION\nUpdate: curl -fsSL https://github.com/$REPO/releases/latest/download/install-core.sh | bash"
fi
info "Core $CORE_VER >= $MIN_CORE_VERSION (compatible)"

# Check daemon is running (it should be — installed by install-core.sh)
if curl -sf --max-time 3 http://127.0.0.1:11430/health >/dev/null 2>&1; then
    info "Daemon is running"
else
    warn "Daemon not currently running. Starting it now..."
    lmforge start || true
    sleep 2
fi

# ── Download ──────────────────────────────────────────────────────────────────
section "Downloading LMForge UI..."

ASSET=$(detect_ui_asset)
URL=$(resolve_url "$ASSET")
echo    "  Asset:  $ASSET"
echo    "  URL:    $URL"
echo ""

TMP_FILE=$(mktemp "/tmp/lmforge-ui-XXXXXX")
trap 'rm -f "$TMP_FILE"' EXIT

if ! curl -fSL --progress-bar "$URL" -o "$TMP_FILE"; then
    error "Download failed from $URL\n  Check https://github.com/$REPO/releases for available versions."
fi
info "Downloaded $ASSET"

# ── Install macOS DMG ─────────────────────────────────────────────────────────
if [[ "$OS" == "Darwin" ]]; then
    section "Installing LMForge.app..."

    # Ensure ~/Applications exists (no sudo required)
    mkdir -p "${HOME}/Applications"

    # Mount DMG
    MOUNT_POINT=$(mktemp -d "/tmp/lmforge-dmg-XXXXXX")
    trap 'hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true; rm -f "$TMP_FILE"' EXIT
    hdiutil attach "$TMP_FILE" -mountpoint "$MOUNT_POINT" -nobrowse -quiet

    # Copy .app to ~/Applications
    APP_SRC=$(find "$MOUNT_POINT" -name "*.app" -maxdepth 1 | head -1)
    [[ -z "$APP_SRC" ]] && error "No .app found in DMG"

    if [[ -d "$APP_BUNDLE" ]]; then
        rm -rf "$APP_BUNDLE"
    fi
    cp -R "$APP_SRC" "$APP_BUNDLE"
    info "Installed: $APP_BUNDLE"

    # Detach DMG
    hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true

    # Remove quarantine (so macOS doesn't block first open)
    xattr -dr com.apple.quarantine "$APP_BUNDLE" 2>/dev/null || true
    info "Quarantine cleared"

    # Launch
    section "Launching LMForge..."
    open "$APP_BUNDLE"
    info "LMForge opened"
fi

# ── Install Linux AppImage ────────────────────────────────────────────────────
if [[ "$OS" == "Linux" ]]; then
    section "Installing LMForge AppImage..."

    APPIMAGE_DIR="${HOME}/.local/bin"
    mkdir -p "$APPIMAGE_DIR"
    APPIMAGE_PATH="$APPIMAGE_DIR/LMForge"

    chmod +x "$TMP_FILE"
    cp "$TMP_FILE" "$APPIMAGE_PATH"
    info "Installed: $APPIMAGE_PATH"

    # Create .desktop entry
    DESKTOP_DIR="${HOME}/.local/share/applications"
    mkdir -p "$DESKTOP_DIR"
    cat > "$DESKTOP_DIR/lmforge.desktop" <<EOF
[Desktop Entry]
Name=LMForge
Comment=Local LLM Orchestrator
Exec=$APPIMAGE_PATH %u
Icon=lmforge
Terminal=false
Type=Application
Categories=Development;AI;
EOF
    info "Desktop entry created"

    echo ""
    echo    "  Launch: $APPIMAGE_PATH"
    echo    "  Or find 'LMForge' in your app launcher"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge UI installed successfully!${NC}"
echo ""
if [[ "$OS" == "Darwin" ]]; then
    echo    "  App:     $APP_BUNDLE"
    echo    "  Open:    open -a LMForge  (also available in Spotlight / Launchpad)"
fi
echo    ""
echo    "  The UI connects to the daemon at http://127.0.0.1:11430"
echo    "  Closing the UI window does NOT stop the daemon or your models."
echo ""
