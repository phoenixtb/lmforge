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
LMFORGE_RELEASE="${LMFORGE_VERSION:-latest}"
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
    if [[ "$LMFORGE_RELEASE" == "latest" ]]; then
        echo "https://github.com/${REPO}/releases/latest/download/${asset}"
    else
        echo "https://github.com/${REPO}/releases/download/${LMFORGE_RELEASE}/${asset}"
    fi
}

# ── Banner ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}  LMForge UI — Installer${NC}"
echo    "  ─────────────────────────────────────────"
echo    "  Repo   : https://github.com/$REPO"
echo    "  Version: $LMFORGE_RELEASE"
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

# Augment PATH with every location install-core.sh might have used,
# so this check works even in a fresh bash subprocess (curl | bash)
# where ~/.zshrc / ~/.bashrc has not been sourced.
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"

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

# ── Linux UI runtime deps (Tauri 2 needs webkit2gtk on the host) ──────────────
# The AppImage bundles most libraries but webkit2gtk/webkitgtk MUST come from
# the host system — it's the HTML rendering engine and cannot be relocated.
# Package names differ by distro AND distro version (Tauri 2 needs webkit 4.1+,
# the 4.0 series was deprecated). We probe `/etc/os-release` and install
# conservatively, with explicit confirmation. Unknown distros: print
# instructions and continue (the AppImage may still launch if libs are
# pre-installed, or fail with a clear message).
if [[ "$OS" == "Linux" ]]; then
    section "Checking UI runtime dependencies..."

    # Default — overridden per distro below.
    LINUX_DEPS=()
    PKG_INSTALL_CMD=""

    DISTRO_ID=""
    DISTRO_VER=""
    if [[ -f /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        DISTRO_ID="${ID:-}"
        DISTRO_VER="${VERSION_ID:-}"
    fi

    # Choose webkit + tray package names per distro/version.
    case "$DISTRO_ID" in
        ubuntu|debian|pop|linuxmint)
            # Map to webkit major version.
            # Ubuntu 22.04 = jammy (4.0, EOL for Tauri 2),
            # Ubuntu 24.04 = noble (4.1), Ubuntu 26.04 = ? (6.0),
            # Debian 12 = bookworm (4.1), Debian 13 = trixie (6.0/4.1).
            WEBKIT_PKG=""
            # First, query apt-cache to see what's actually available — this
            # is the most reliable signal across distro forks (Pop!_OS, Mint).
            if apt-cache show libwebkitgtk-6.0-0 &>/dev/null; then
                WEBKIT_PKG="libwebkitgtk-6.0-0"
            elif apt-cache show libwebkit2gtk-4.1-0 &>/dev/null; then
                WEBKIT_PKG="libwebkit2gtk-4.1-0"
            elif apt-cache show libwebkit2gtk-4.0-37 &>/dev/null; then
                # Ubuntu 22.04 fallback — Tauri 2 nominally needs 4.1, but
                # the AppImage *may* run if upstream still links 4.0.
                WEBKIT_PKG="libwebkit2gtk-4.0-37"
                warn "Detected older Ubuntu/Debian (${DISTRO_VER}). Tauri 2 prefers webkit 4.1+; upgrading distro recommended."
            fi
            LINUX_DEPS=("libayatana-appindicator3-1")
            [[ -n "$WEBKIT_PKG" ]] && LINUX_DEPS+=("$WEBKIT_PKG")
            PKG_INSTALL_CMD="sudo apt-get install -y"
            ;;
        fedora|rhel|centos|rocky|almalinux)
            # Fedora 39+ ships webkit2gtk4.1; older RHEL/Rocky needs EPEL.
            if command -v dnf &>/dev/null; then
                if dnf list available webkitgtk6.0 &>/dev/null 2>&1; then
                    LINUX_DEPS=("webkitgtk6.0" "libayatana-appindicator-gtk3")
                elif dnf list available webkit2gtk4.1 &>/dev/null 2>&1; then
                    LINUX_DEPS=("webkit2gtk4.1" "libayatana-appindicator-gtk3")
                else
                    warn "Could not find webkit2gtk4.1+ in dnf repos. You may need EPEL or COPR."
                fi
                PKG_INSTALL_CMD="sudo dnf install -y"
            fi
            ;;
        arch|manjaro|endeavouros)
            # Arch keeps both 4.1 and 6.0 in the official repos.
            if pacman -Si webkitgtk-6.0 &>/dev/null; then
                LINUX_DEPS=("webkitgtk-6.0" "libayatana-appindicator")
            else
                LINUX_DEPS=("webkit2gtk-4.1" "libayatana-appindicator")
            fi
            PKG_INSTALL_CMD="sudo pacman -S --noconfirm"
            ;;
        opensuse*|suse|sles)
            # openSUSE uses underscores in versioned suffixes.
            if zypper se -x libwebkitgtk-6_0-0 &>/dev/null; then
                LINUX_DEPS=("libwebkitgtk-6_0-0" "libayatana-appindicator3-1")
            else
                LINUX_DEPS=("libwebkit2gtk-4_1-0" "libayatana-appindicator3-1")
            fi
            PKG_INSTALL_CMD="sudo zypper install -y"
            ;;
        *)
            warn "Unknown distro '$DISTRO_ID' — cannot auto-install UI runtime deps."
            echo ""
            echo "  Manually install (whichever your package manager exposes):"
            echo "    • webkit2gtk-4.1 OR webkitgtk-6.0  (HTML renderer)"
            echo "    • libayatana-appindicator3        (system tray)"
            echo ""
            echo "  The AppImage may still launch if these are already present."
            ;;
    esac

    # Filter to only the missing packages and prompt before installing.
    if [[ ${#LINUX_DEPS[@]} -gt 0 && -n "$PKG_INSTALL_CMD" ]]; then
        MISSING_DEPS=()
        for dep in "${LINUX_DEPS[@]}"; do
            case "$DISTRO_ID" in
                ubuntu|debian|pop|linuxmint)
                    dpkg -s "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
                fedora|rhel|centos|rocky|almalinux)
                    rpm -q "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
                arch|manjaro|endeavouros)
                    pacman -Q "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
                opensuse*|suse|sles)
                    rpm -q "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
            esac
        done

        if [[ ${#MISSING_DEPS[@]} -eq 0 ]]; then
            info "All UI runtime deps already present."
        else
            warn "Missing UI runtime deps (${DISTRO_ID} ${DISTRO_VER}):"
            for d in "${MISSING_DEPS[@]}"; do echo "    • $d"; done
            echo ""
            echo "  Install command:"
            echo "    $PKG_INSTALL_CMD ${MISSING_DEPS[*]}"
            echo ""

            if [ -t 0 ]; then
                printf "  Run this now (requires sudo)? [Y/n] "
                read -r REPLY </dev/tty
                REPLY="${REPLY:-Y}"
                if [[ "$REPLY" == "y" || "$REPLY" == "Y" ]]; then
                    if eval "$PKG_INSTALL_CMD ${MISSING_DEPS[*]}"; then
                        info "UI runtime deps installed."
                    else
                        warn "Install failed. The AppImage will still download but may fail to launch."
                    fi
                else
                    warn "Skipped. Install the deps manually before launching the UI."
                fi
            else
                warn "Non-interactive shell — skipping auto-install. Run manually:"
                echo "    $PKG_INSTALL_CMD ${MISSING_DEPS[*]}"
            fi
        fi
    fi
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
