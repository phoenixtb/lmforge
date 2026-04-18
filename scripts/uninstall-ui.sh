#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge UI — Uninstall Script
#  Removes the desktop app (macOS .app / Linux AppImage).
#  Does NOT affect the daemon, service, or models.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.sh | bash
#    ... | bash -s -- --yes     # skip confirmation
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

YES=false
for arg in "$@"; do
    [[ "$arg" == "--yes" ]] && YES=true
done

OS=$(uname -s)
APP_NAME="LMForge"

# All locations install-ui.sh might have placed the app (ordered: user-local first)
MACOS_APP_LOCATIONS=(
    "${HOME}/Applications/${APP_NAME}.app"   # install-ui.sh default (no sudo)
    "/Applications/${APP_NAME}.app"          # legacy / manual installs
)
LINUX_APPIMAGE="${HOME}/.local/bin/LMForge"
LINUX_DESKTOP="${HOME}/.local/share/applications/lmforge.desktop"

# macOS app support / cache dirs left by Tauri
APP_SUPPORT="${HOME}/Library/Application Support/com.lmforge.ui"
APP_PREFS="${HOME}/Library/Preferences/com.lmforge.ui.plist"
APP_CACHE="${HOME}/Library/Caches/com.lmforge.ui"
WEBKIT_CACHE="${HOME}/Library/WebKit/com.lmforge.ui"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# ── Banner ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}  LMForge UI — Uninstaller${NC}"
echo    "  ─────────────────────────────────────────"
echo    "  Removes the desktop app only."
echo    "  Daemon service and models are NOT affected."
echo ""

if ! $YES; then
    if [ -t 0 ]; then
        read -rp "  Continue? [y/N] " confirm
    else
        read -rp "  Continue? [y/N] " confirm </dev/tty
    fi
    [[ "$confirm" =~ ^[Yy]$ ]] || { echo "  Aborted."; exit 0; }
fi

# ── macOS ─────────────────────────────────────────────────────────────────────
if [[ "$OS" == "Darwin" ]]; then
    # 1. Quit the running app
    section "Quitting LMForge..."
    osascript -e 'tell application "LMForge" to quit' 2>/dev/null || true
    pkill -x "LMForge"        2>/dev/null || true
    pkill -x "lmforge-ui"     2>/dev/null || true
    sleep 1
    info "App process stopped"

    # 2. Remove .app bundle from all known locations
    section "Removing app bundle..."
    FOUND=false
    for bundle in "${MACOS_APP_LOCATIONS[@]}"; do
        if [[ -d "$bundle" ]]; then
            rm -rf "$bundle"
            info "Removed $bundle"
            FOUND=true
        fi
    done
    $FOUND || warn "LMForge.app not found — may already be uninstalled"

    # 3. Remove Tauri/macOS app support files
    section "Removing app support files..."
    for path in "$APP_SUPPORT" "$APP_PREFS" "$APP_CACHE" "$WEBKIT_CACHE"; do
        if [[ -e "$path" ]]; then
            rm -rf "$path"
            info "Removed $path"
        fi
    done
fi

# ── Linux ─────────────────────────────────────────────────────────────────────
if [[ "$OS" == "Linux" ]]; then
    # 1. Kill the app if running
    section "Stopping LMForge..."
    pkill -x "LMForge"    2>/dev/null || true
    pkill -x "lmforge-ui" 2>/dev/null || true
    sleep 1
    info "App process stopped"

    # 2. Remove AppImage
    section "Removing AppImage..."
    if [[ -f "$LINUX_APPIMAGE" ]]; then
        rm -f "$LINUX_APPIMAGE"
        info "Removed $LINUX_APPIMAGE"
    else
        warn "AppImage not found at $LINUX_APPIMAGE"
    fi

    # 3. Remove .desktop launcher
    if [[ -f "$LINUX_DESKTOP" ]]; then
        rm -f "$LINUX_DESKTOP"
        info "Removed $LINUX_DESKTOP"
        # Refresh app launcher cache
        update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
    fi
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge UI uninstalled.${NC}"
echo ""
echo    "  The daemon is still running. To also remove Core:"
echo    "    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash"
echo ""
echo    "  To reinstall the UI:"
echo    "    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash"
echo ""
