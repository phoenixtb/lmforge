#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Dev UI launcher for LMForge on Linux (apt-based distros).
#
# - Auto-detects webkit version (4.1 for Ubuntu 22/24/Debian 12; 6.0 for 26+)
# - Skips package installs that are already satisfied (no needless sudo)
# - Pings the daemon and warns if it's not running before launching tauri dev
# - Runs `npm ci` only when node_modules is missing or package-lock changed
#
# Usage:
#   ./dev_ui_ubuntu24.sh [--skip-deps] [--skip-daemon-check] [--build]
#
# Flags:
#   --skip-deps          Don't probe/install webkit/appindicator (faster restart)
#   --skip-daemon-check  Launch UI even if no daemon is responding on :11430
#   --build              Run `npm run tauri build` instead of `tauri dev`
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
UI_DIR="$REPO_ROOT/ui"

SKIP_DEPS=0
SKIP_DAEMON_CHECK=0
DO_BUILD=0
while (($#)); do
    case "$1" in
        --skip-deps)         SKIP_DEPS=1 ;;
        --skip-daemon-check) SKIP_DAEMON_CHECK=1 ;;
        --build)             DO_BUILD=1 ;;
        -h|--help)           sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)                   echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info() { echo -e "${GREEN}  ✓${NC} $*"; }
warn() { echo -e "${YELLOW}  ⚠${NC} $*"; }

# ── 1. Dependencies (apt-only; auto-detect webkit major) ─────────────────────
if (( SKIP_DEPS )); then
    info "skipping dep check (--skip-deps)"
else
    command -v apt-get >/dev/null || { warn "non-apt distro — see scripts/install-ui.sh for branching logic"; exit 1; }

    # Pick webkit dev package by what apt actually has (works on 22/24/26 + Debian).
    WEBKIT_DEV=""
    if apt-cache show libwebkitgtk-6.0-dev   &>/dev/null; then WEBKIT_DEV="libwebkitgtk-6.0-dev"
    elif apt-cache show libwebkit2gtk-4.1-dev &>/dev/null; then WEBKIT_DEV="libwebkit2gtk-4.1-dev"
    elif apt-cache show libwebkit2gtk-4.0-dev &>/dev/null; then WEBKIT_DEV="libwebkit2gtk-4.0-dev"
    fi
    [[ -z "$WEBKIT_DEV" ]] && { warn "no webkit*-dev package found in apt — Tauri build will fail"; exit 1; }

    DEPS=(
        "$WEBKIT_DEV"
        libayatana-appindicator3-dev
        librsvg2-dev
        libxdo-dev
        patchelf
    )
    MISSING=()
    for d in "${DEPS[@]}"; do dpkg -s "$d" &>/dev/null || MISSING+=("$d"); done

    if (( ${#MISSING[@]} == 0 )); then
        info "all UI build deps present ($WEBKIT_DEV)"
    else
        warn "installing missing deps: ${MISSING[*]}"
        sudo apt-get update -qq
        sudo apt-get install -y "${MISSING[@]}"
        info "installed: ${MISSING[*]}"
    fi
fi

# ── 2. Daemon precheck (UI without a daemon is just an error screen) ─────────
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

# ── 3. npm deps (only run npm ci when needed — slow otherwise) ───────────────
cd "$UI_DIR"
NEED_NPM_CI=0
[[ ! -d node_modules ]] && NEED_NPM_CI=1
if [[ -f package-lock.json && -d node_modules ]]; then
    # Re-run ci if lockfile is newer than the installed marker
    if [[ "package-lock.json" -nt "node_modules/.package-lock.json" ]]; then
        NEED_NPM_CI=1
    fi
fi
if (( NEED_NPM_CI )); then
    warn "installing npm deps (one-time / lockfile changed)..."
    npm ci
else
    info "node_modules up-to-date with package-lock.json"
fi

# ── 4. Run ───────────────────────────────────────────────────────────────────
if (( DO_BUILD )); then
    info "building AppImage (npm run tauri build) — output in src-tauri/target/release/bundle/"
    exec npm run tauri build
else
    info "launching tauri dev (hot-reload on save; Ctrl-C to stop)"
    exec npm run tauri dev
fi
