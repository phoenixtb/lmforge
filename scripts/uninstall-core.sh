#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — Uninstall Script
#  Stops the daemon, removes the service, removes the binary and PATH entries.
#  Models and config in ~/.lmforge are kept unless --purge is passed.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash
#    curl -fsSL ... | bash -s -- --purge     # also removes models
#
#  Flags:
#    --purge    Remove ~/.lmforge/* including downloaded models
#    --yes      Skip the confirmation prompt (for scripted use)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PURGE=false
YES=false
for arg in "$@"; do
    [[ "$arg" == "--purge" ]] && PURGE=true
    [[ "$arg" == "--yes"   ]] && YES=true
done

# ── Config ────────────────────────────────────────────────────────────────────
BINARY_NAME="lmforge"
# All locations install-core.sh might have placed the binary
INSTALL_DIRS=(
    "${HOME}/.local/bin"
    "${HOME}/.cargo/bin"
    "/usr/local/bin"
    "/opt/homebrew/bin"
)
DATA_DIR="${HOME}/.lmforge"
LAUNCHD_LABEL="com.lmforge.daemon"
LAUNCHD_PLIST="${HOME}/Library/LaunchAgents/${LAUNCHD_LABEL}.plist"
SYSTEMD_UNIT="${HOME}/.config/systemd/user/lmforge.service"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# ── Banner ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}  LMForge Core — Uninstaller${NC}"
echo    "  ─────────────────────────────────────────"
if $PURGE; then
    echo -e "  ${RED}--purge: ALL data including downloaded models will be deleted.${NC}"
else
    echo    "  Models and config in $DATA_DIR will be kept."
    echo    "  Pass --purge to remove everything."
fi
echo ""

# Prompt — works both interactively AND when piped via curl | bash
if ! $YES; then
    if [ -t 0 ]; then
        read -rp "  Continue? [y/N] " confirm
    else
        # stdin is a pipe — read from the terminal directly
        read -rp "  Continue? [y/N] " confirm </dev/tty
    fi
    [[ "$confirm" =~ ^[Yy]$ ]] || { echo "  Aborted."; exit 0; }
fi

# ── 1. Use lmforge CLI to stop + unregister service (handles all platforms) ──
section "Stopping daemon and removing service..."
if command -v "$BINARY_NAME" &>/dev/null; then
    "$BINARY_NAME" service stop    2>/dev/null || true
    "$BINARY_NAME" service uninstall 2>/dev/null || true
    info "Service unregistered via lmforge CLI"
fi

# ── 2. macOS launchd — belt-and-suspenders cleanup ───────────────────────────
if [[ -f "$LAUNCHD_PLIST" ]]; then
    launchctl bootout "gui/$(id -u)" "$LAUNCHD_PLIST" 2>/dev/null || \
    launchctl unload  "$LAUNCHD_PLIST" 2>/dev/null || true
    rm -f "$LAUNCHD_PLIST"
    info "Removed launchd plist: $LAUNCHD_PLIST"
fi

# ── 3. Linux systemd — belt-and-suspenders cleanup ───────────────────────────
if [[ -f "$SYSTEMD_UNIT" ]]; then
    systemctl --user disable --now lmforge.service 2>/dev/null || true
    rm -f "$SYSTEMD_UNIT"
    systemctl --user daemon-reload 2>/dev/null || true
    info "Removed systemd unit"
fi

# ── 4. Stop daemon via API (graceful), then force-kill ───────────────────────
section "Stopping any running daemon process..."
if curl -sf --max-time 3 http://127.0.0.1:11430/health >/dev/null 2>&1; then
    curl -sf -X POST http://127.0.0.1:11430/lf/shutdown >/dev/null 2>&1 || true
    sleep 1
    info "Daemon shutdown via API"
fi
pkill -x "$BINARY_NAME" 2>/dev/null || true
sleep 1
pkill -9 -x "$BINARY_NAME" 2>/dev/null || true
info "No lmforge processes running"

# ── 5. Remove binary from every known install location ───────────────────────
section "Removing binary..."
FOUND=false
for dir in "${INSTALL_DIRS[@]}"; do
    bin="$dir/$BINARY_NAME"
    if [[ -f "$bin" ]]; then
        rm -f "$bin"
        info "Removed $bin"
        FOUND=true
    fi
done
$FOUND || warn "lmforge binary not found in standard locations (may already be removed)"

# ── 6. Remove PATH injection lines from shell profiles ───────────────────────
section "Cleaning up PATH entries..."
for profile in "${HOME}/.zshrc" "${HOME}/.bashrc" "${HOME}/.profile"; do
    if [[ -f "$profile" ]] && grep -q "\.local/bin" "$profile"; then
        # Remove the "# LMForge" comment + the export PATH line we added
        sed -i.bak '/^# LMForge$/d; /\.local\/bin.*PATH/d' "$profile" 2>/dev/null || true
        rm -f "${profile}.bak"
        info "Cleaned PATH entry from $profile"
    fi
done

# ── 7. Remove PID file and engine socket ─────────────────────────────────────
rm -f "$DATA_DIR/lmforge.pid"   2>/dev/null || true
rm -f "$DATA_DIR/lmforge.sock"  2>/dev/null || true

# ── 8. Remove engine installs (venvs, downloaded binaries in ~/.lmforge) ──────
section "Removing installed engines..."
if [[ -d "$DATA_DIR/engines" ]]; then
    rm -rf "$DATA_DIR/engines"
    info "Removed $DATA_DIR/engines"
fi

# ── 9. Data directory ─────────────────────────────────────────────────────────
section "Data directory..."
if $PURGE; then
    if [[ -d "$DATA_DIR" ]]; then
        MODEL_SIZE=$(du -sh "$DATA_DIR/models" 2>/dev/null | cut -f1 || echo "unknown")
        echo "  Removing $DATA_DIR (models: $MODEL_SIZE)"
        rm -rf "$DATA_DIR"
        info "Data directory removed"
    fi
    # macOS app support / cache
    rm -rf "${HOME}/Library/Application Support/com.lmforge.ui" 2>/dev/null || true
    rm -rf "${HOME}/Library/Caches/com.lmforge.ui"              2>/dev/null || true
    rm -rf "${HOME}/Library/Preferences/com.lmforge.ui.plist"   2>/dev/null || true
    rm -rf "${HOME}/Library/WebKit/com.lmforge.ui"              2>/dev/null || true
else
    info "Keeping $DATA_DIR (pass --purge to remove)"
    echo    "  Your downloaded models are safe."
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge Core uninstalled.${NC}"
if ! $PURGE && [[ -d "$DATA_DIR" ]]; then
    echo ""
    echo    "  Models still at: $DATA_DIR/models"
    echo    "  To remove everything:"
    echo    "    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash -s -- --purge"
fi
echo ""
