#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge UI — Uninstall Script
#  Removes the LMForge.app desktop client.
#  Does NOT affect the daemon or models.
#
#  Usage:
#    bash scripts/uninstall-ui.sh
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

APP_NAME="LMForge"
APP_BUNDLE="/Applications/$APP_NAME.app"
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
echo    "  This removes the desktop app only."
echo    "  The daemon and your models are NOT affected."
echo ""
read -rp "  Continue? [y/N] " confirm
[[ "$confirm" =~ ^[Yy]$ ]] || { echo "  Aborted."; exit 0; }

# ── 1. Quit the running app ───────────────────────────────────────────────────
section "Quitting LMForge app..."
if pgrep -x "LMForge" >/dev/null 2>&1 || pgrep -x "lmforge-ui" >/dev/null 2>&1; then
    osascript -e 'tell application "LMForge" to quit' 2>/dev/null || true
    pkill -x "LMForge"    2>/dev/null || true
    pkill -x "lmforge-ui" 2>/dev/null || true
    sleep 1
    info "App quit"
else
    info "App not running"
fi

# ── 2. Remove .app bundle ─────────────────────────────────────────────────────
section "Removing app bundle..."
if [[ -d "$APP_BUNDLE" ]]; then
    rm -rf "$APP_BUNDLE"
    info "Removed $APP_BUNDLE"
else
    warn "App not found at $APP_BUNDLE"
fi

# ── 3. Remove app data / cache ────────────────────────────────────────────────
section "Removing app support files..."
for path in "$APP_SUPPORT" "$APP_PREFS" "$APP_CACHE" "$WEBKIT_CACHE"; do
    if [[ -e "$path" ]]; then
        rm -rf "$path"
        info "Removed $path"
    fi
done

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge UI uninstalled.${NC}"
echo ""
echo    "  The LMForge daemon is still running."
echo    "  Your models are still at: ${HOME}/.lmforge/models"
echo ""
echo    "  To stop the daemon:         lmforge service stop"
echo    "  To uninstall everything:    bash scripts/uninstall-core.sh --purge"
echo    "  To reinstall the UI:        bash scripts/install-ui.sh"
echo ""
