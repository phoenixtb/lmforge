#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — Uninstall Script
#  Stops and removes the lmforge daemon, service, and binary.
#  Models and data are kept unless --purge is passed.
#
#  Usage:
#    bash scripts/uninstall-core.sh            # keeps ~/.lmforge data
#    bash scripts/uninstall-core.sh --purge    # removes everything including models
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PURGE=false
for arg in "$@"; do
    [[ "$arg" == "--purge" ]] && PURGE=true
done

# ── Config ────────────────────────────────────────────────────────────────────
BINARY_NAME="lmforge"
INSTALL_DIRS=("/usr/local/bin" "/opt/homebrew/bin" "${HOME}/.local/bin" "${HOME}/.cargo/bin")
DATA_DIR="${HOME}/.lmforge"
LAUNCHD_LABEL="com.lmforge.daemon"
LAUNCHD_PLIST="${HOME}/Library/LaunchAgents/${LAUNCHD_LABEL}.plist"
APP_SUPPORT="${HOME}/Library/Application Support/com.lmforge.ui"
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
    echo -e "  ${RED}--purge mode: ALL data including models will be deleted.${NC}"
else
    echo    "  Models and config in $DATA_DIR will be kept."
    echo    "  Pass --purge to remove everything."
fi
echo ""
read -rp "  Continue? [y/N] " confirm
[[ "$confirm" =~ ^[Yy]$ ]] || { echo "  Aborted."; exit 0; }

# ── 1. Stop daemon via API (graceful) ─────────────────────────────────────────
section "Stopping daemon..."
if curl -sf --max-time 3 http://127.0.0.1:11430/health >/dev/null 2>&1; then
    curl -sf -X POST http://127.0.0.1:11430/lf/shutdown >/dev/null 2>&1 || true
    sleep 1
    info "Daemon stopped via API"
else
    info "Daemon not running"
fi

# ── 2. Remove macOS LaunchAgent ───────────────────────────────────────────────
section "Removing system service..."
if [[ -f "$LAUNCHD_PLIST" ]]; then
    launchctl unload "$LAUNCHD_PLIST" 2>/dev/null || true
    rm -f "$LAUNCHD_PLIST"
    info "LaunchAgent removed: $LAUNCHD_PLIST"
else
    info "No LaunchAgent found"
fi

# ── 3. Remove Linux systemd unit ─────────────────────────────────────────────
if [[ -f "$SYSTEMD_UNIT" ]]; then
    systemctl --user disable --now lmforge.service 2>/dev/null || true
    rm -f "$SYSTEMD_UNIT"
    systemctl --user daemon-reload 2>/dev/null || true
    info "systemd unit removed"
fi

# ── 4. Kill any still-running lmforge processes ───────────────────────────────
pkill -x lmforge 2>/dev/null || true
# Wait briefly and force-kill if needed
sleep 1
pkill -9 -x lmforge 2>/dev/null || true
info "Ensured no lmforge processes running"

# ── 5. Remove binary from all known install locations ─────────────────────────
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
$FOUND || warn "lmforge binary not found in standard locations"

# ── 6. Remove PID file ────────────────────────────────────────────────────────
rm -f "$DATA_DIR/lmforge.pid" 2>/dev/null || true

# ── 7. Data directory ─────────────────────────────────────────────────────────
section "Data directory..."
if $PURGE; then
    if [[ -d "$DATA_DIR" ]]; then
        MODEL_SIZE=$(du -sh "$DATA_DIR/models" 2>/dev/null | cut -f1 || echo "unknown")
        echo    "  Removing $DATA_DIR (models: $MODEL_SIZE)"
        rm -rf "$DATA_DIR"
        info "Data directory removed"
    fi
    # Remove macOS app support/cache
    rm -rf "$APP_SUPPORT" 2>/dev/null || true
else
    info "Keeping $DATA_DIR (use --purge to remove)"
    echo    "  Your downloaded models are safe."
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge Core uninstalled.${NC}"
if ! $PURGE && [[ -d "$DATA_DIR" ]]; then
    echo ""
    echo    "  Models still at: $DATA_DIR/models"
    echo    "  To remove everything: bash scripts/uninstall-core.sh --purge"
fi
echo ""
