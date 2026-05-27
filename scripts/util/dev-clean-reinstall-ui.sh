#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Dev clean-reinstall for the LMForge UI (Tauri + SvelteKit).
#
# Equivalent of dev-clean-reinstall-core_linux-sglang-cuda13.sh, but for the
# `ui/` workspace. Stops any running tauri dev, wipes JS+Rust build artefacts,
# reinstalls npm deps, and either launches `tauri dev` (default) or builds the
# AppImage (`--build`). System deps (webkitgtk, libayatana-appindicator, …) are
# handled by the distro-specific dev_ui_*.sh script that this calls into.
#
# Distro autodetect: apt → dev_ui_ubuntu24.sh, dnf → dev_ui_fedora.sh,
#                    pacman → dev_ui_arch.sh. Override with --distro <name>.
#
# Usage:
#   ./dev-clean-reinstall-ui.sh [flags]
#
# Flags:
#   --keep-node        Skip `rm -rf ui/node_modules ui/dist` (faster restart)
#   --keep-rust        Skip `rm -rf ui/src-tauri/target` (saves ~2-5 min)
#   --no-launch        Just clean + rebuild, don't `tauri dev` / `tauri build`
#   --build            Build AppImage instead of running `tauri dev`
#   --skip-deps        Don't probe/install system deps (webkit etc.)
#   --skip-daemon-check Don't ping :11430 before launching
#   --distro NAME      Force distro variant: ubuntu | fedora | arch
#   -h | --help        Show this help and exit
#
# Exit codes:
#   0  success
#   1  preflight failure (ui/ missing, no distro variant)
#   2  npm install failure
#   3  build / launch failure
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
UI_DIR="$REPO_ROOT/ui"

# ── defaults ─────────────────────────────────────────────────────────────────
KEEP_NODE=0
KEEP_RUST=0
DO_LAUNCH=1
DO_BUILD=0
SKIP_DEPS=0
SKIP_DAEMON_CHECK=0
DISTRO=""

# ── arg parsing ──────────────────────────────────────────────────────────────
while (($#)); do
    case "$1" in
        --keep-node)         KEEP_NODE=1 ;;
        --keep-rust)         KEEP_RUST=1 ;;
        --no-launch)         DO_LAUNCH=0 ;;
        --build)             DO_BUILD=1 ;;
        --skip-deps)         SKIP_DEPS=1 ;;
        --skip-daemon-check) SKIP_DAEMON_CHECK=1 ;;
        --distro)            DISTRO="${2:?--distro requires ubuntu|fedora|arch}"; shift ;;
        -h|--help)           sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)                   echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

# ── colours / helpers ────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info() { echo -e "${GREEN}  ✓${NC} $*"; }
warn() { echo -e "${YELLOW}  ⚠${NC} $*"; }
err()  { echo -e "${RED}  ✗${NC} $*"; }
sec()  { echo -e "\n${BOLD}$*${NC}"; }

# ── 1. Preflight ─────────────────────────────────────────────────────────────
sec "1. Preflight"
[[ -d "$UI_DIR" ]] || { err "no ui/ dir at $UI_DIR"; exit 1; }
[[ -f "$UI_DIR/package.json" ]] || { err "no ui/package.json"; exit 1; }
command -v npm >/dev/null || { err "npm not in PATH — install Node 20+"; exit 1; }

# Resolve which distro variant to call for system deps + launch.
if [[ -z "$DISTRO" ]]; then
    if   command -v apt-get >/dev/null; then DISTRO="ubuntu"
    elif command -v dnf     >/dev/null; then DISTRO="fedora"
    elif command -v pacman  >/dev/null; then DISTRO="arch"
    else err "no apt/dnf/pacman found — pass --distro explicitly"; exit 1; fi
fi
DISTRO_SCRIPT="$SCRIPT_DIR/dev_ui_${DISTRO}.sh"
[[ "$DISTRO" = "ubuntu" ]] && DISTRO_SCRIPT="$SCRIPT_DIR/dev_ui_ubuntu24.sh"
[[ -x "$DISTRO_SCRIPT" ]] || { err "missing distro script: $DISTRO_SCRIPT"; exit 1; }
info "ui/ ok | npm $(npm --version) | node $(node --version 2>/dev/null || echo n/a) | distro=$DISTRO"

# ── 2. Stop any running tauri dev / vite ─────────────────────────────────────
sec "2. Stop running UI processes"
KILLED=0
for proc in "tauri dev" "vite" "tauri-dev-build"; do
    if pgrep -f "$proc" >/dev/null 2>&1; then
        pkill -f "$proc" && KILLED=1
        info "stopped: $proc"
    fi
done
(( KILLED )) || info "no running UI processes"
sleep 1

# ── 3. Wipe artefacts (selective) ────────────────────────────────────────────
sec "3. Wipe artefacts"
remove() {
    local p="$1" label="$2"
    if [[ -e "$p" ]]; then
        local s=$(du -sh "$p" 2>/dev/null | cut -f1)
        rm -rf "$p"
        info "removed $label ($s)"
    else
        info "$label already absent"
    fi
}
if (( KEEP_NODE )); then
    info "keeping node_modules + dist (--keep-node)"
else
    remove "$UI_DIR/node_modules" "ui/node_modules"
    remove "$UI_DIR/dist"         "ui/dist"
    remove "$UI_DIR/.svelte-kit"  "ui/.svelte-kit (svelte build cache)"
fi
if (( KEEP_RUST )); then
    info "keeping src-tauri/target (--keep-rust)"
else
    remove "$UI_DIR/src-tauri/target" "ui/src-tauri/target (tauri rust artefacts)"
fi

# ── 4. npm ci (clean install — respects package-lock.json) ───────────────────
sec "4. Install JS deps (npm ci)"
cd "$UI_DIR"
if ! npm ci; then
    err "npm ci failed"
    exit 2
fi
info "npm deps installed"

# ── 5. Hand off to distro launcher (it handles system-dep probe + run/build) ─
if (( ! DO_LAUNCH )); then
    info "stopping here (--no-launch). Run later with: $DISTRO_SCRIPT"
    exit 0
fi

sec "5. Launch via $DISTRO_SCRIPT"
LAUNCH_ARGS=( "--skip-deps" )            # we just reinstalled npm; deps already probed if user wants
(( SKIP_DEPS == 0 )) && LAUNCH_ARGS=()    # re-probe system deps on full run
(( SKIP_DAEMON_CHECK )) && LAUNCH_ARGS+=( "--skip-daemon-check" )
(( DO_BUILD ))           && LAUNCH_ARGS+=( "--build" )

exec "$DISTRO_SCRIPT" "${LAUNCH_ARGS[@]}"
