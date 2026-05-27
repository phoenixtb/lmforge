#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Dev UI launcher for LMForge on Arch / Manjaro / EndeavourOS (pacman).
# Mirror of dev_ui_ubuntu24.sh adapted for pacman.
#
# Webkit picks (Tauri 2): webkit2gtk-4.1 (current) → webkitgtk-6.0 (newer).
# Arch official repos carry both as -devel-free monolithic packages (no -dev).
#
# Usage:
#   ./dev_ui_arch.sh [--skip-deps] [--skip-daemon-check] [--build]
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
UI_DIR="$REPO_ROOT/ui"

SKIP_DEPS=0; SKIP_DAEMON_CHECK=0; DO_BUILD=0
while (($#)); do
    case "$1" in
        --skip-deps)         SKIP_DEPS=1 ;;
        --skip-daemon-check) SKIP_DAEMON_CHECK=1 ;;
        --build)             DO_BUILD=1 ;;
        -h|--help)           sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)                   echo "Unknown flag: $1" >&2; exit 1 ;;
    esac; shift
done

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info() { echo -e "${GREEN}  ✓${NC} $*"; }
warn() { echo -e "${YELLOW}  ⚠${NC} $*"; }

# ── 1. Deps via pacman ───────────────────────────────────────────────────────
if (( SKIP_DEPS )); then
    info "skipping dep check (--skip-deps)"
else
    command -v pacman >/dev/null || { warn "non-pacman distro"; exit 1; }

    # Prefer the newer 6.0 if available, else 4.1.
    WEBKIT=""
    if pacman -Si webkitgtk-6.0 &>/dev/null; then WEBKIT="webkitgtk-6.0"
    elif pacman -Si webkit2gtk-4.1 &>/dev/null; then WEBKIT="webkit2gtk-4.1"
    fi
    [[ -z "$WEBKIT" ]] && { warn "no webkitgtk-6.0 or webkit2gtk-4.1 in pacman — check repo enablement"; exit 1; }

    DEPS=(
        "$WEBKIT"
        libayatana-appindicator
        librsvg
        xdotool
        patchelf
        base-devel openssl pkgconf
    )
    MISSING=()
    for d in "${DEPS[@]}"; do pacman -Q "$d" &>/dev/null || MISSING+=("$d"); done

    if (( ${#MISSING[@]} == 0 )); then
        info "all UI build deps present ($WEBKIT)"
    else
        warn "installing missing deps: ${MISSING[*]}"
        sudo pacman -S --needed --noconfirm "${MISSING[@]}"
        info "installed: ${MISSING[*]}"
    fi
fi

# ── 2. Daemon precheck ───────────────────────────────────────────────────────
if (( SKIP_DAEMON_CHECK )); then
    info "skipping daemon check (--skip-daemon-check)"
elif curl -sf --max-time 2 http://127.0.0.1:11430/health >/dev/null; then
    info "daemon up at http://127.0.0.1:11430"
else
    warn "daemon NOT running on :11430 — start it in another terminal:"
    echo "      RUST_LOG=lmforge=debug lmforge start"
    echo ""
    read -r -p "  Continue anyway? [y/N] " REPLY
    [[ "$REPLY" =~ ^[Yy]$ ]] || exit 1
fi

# ── 3. npm deps ──────────────────────────────────────────────────────────────
cd "$UI_DIR"
NEED_NPM_CI=0
[[ ! -d node_modules ]] && NEED_NPM_CI=1
if [[ -f package-lock.json && -d node_modules ]]; then
    [[ "package-lock.json" -nt "node_modules/.package-lock.json" ]] && NEED_NPM_CI=1
fi
if (( NEED_NPM_CI )); then
    warn "installing npm deps..."
    npm ci
else
    info "node_modules up-to-date"
fi

# ── 4. Run ───────────────────────────────────────────────────────────────────
if (( DO_BUILD )); then
    info "building (npm run tauri build)"
    exec npm run tauri build
else
    info "launching tauri dev (Ctrl-C to stop)"
    exec npm run tauri dev
fi
