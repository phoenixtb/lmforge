#!/usr/bin/env bash
# =============================================================================
# LMForge — Release E2E (macOS / Linux)
# Install core (+ UI when available) from a published GitHub release, pull
# predefined models, run multi-model E2E inference tests, optionally uninstall.
#
# Usage:
#   ./scripts/util/e2e-release.sh
#   ./scripts/util/e2e-release.sh v0.1.5
#   ./scripts/util/e2e-release.sh latest --keep-install
#   ./scripts/util/e2e-release.sh v0.1.5 --purge
#   --full is a legacy no-op (all suites on by default in multi_model_e2e.sh)
# =============================================================================
set -uo pipefail

VERSION="${1:-${LMFORGE_VERSION:-latest}}"
shift || true
FULL=0
KEEP_INSTALL=0
SKIP_CLEANUP=0
PURGE=0

while (($#)); do
    case "$1" in
        --full)          FULL=1 ;;
        --keep-install)  KEEP_INSTALL=1 ;;
        --skip-cleanup)  SKIP_CLEANUP=1 ;;
        --purge)         PURGE=1 ;;
        -h|--help)
            sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown: $1" >&2; exit 1 ;;
    esac
    shift
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
API="http://127.0.0.1:11430"
BIN="$HOME/.local/bin/lmforge"
OS="$(uname -s)"
RESULTS=()
FAILED=0

export LMFORGE_YES=1
[[ "$VERSION" != "latest" ]] && export LMFORGE_VERSION="$VERSION"

export N_REQUESTS="${N_REQUESTS:-5}"

step() {
    local name="$1"; shift
    echo ""
    echo "=== $name ==="
    if "$@"; then
        RESULTS+=("PASS  $name")
        echo "PASS  $name"
    else
        RESULTS+=("FAIL  $name")
        echo "FAIL  $name"
        FAILED=$((FAILED + 1))
    fi
}

preclean() {
    case "$OS" in
        Darwin)
            if [[ -d "$HOME/Applications/LMForge.app" ]] || [[ -d "/Applications/LMForge.app" ]]; then
                pkill -x "LMForge" 2>/dev/null || true
                pkill -x "lmforge-ui" 2>/dev/null || true
                sleep 2
                bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes || true
            fi
            ;;
        Linux)
            if [[ -x "$HOME/.local/bin/LMForge" ]]; then
                pkill -x "LMForge" 2>/dev/null || true
                pkill -x "lmforge-ui" 2>/dev/null || true
                sleep 2
                bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes || true
            fi
            ;;
    esac
    if [[ -x "$BIN" ]] || curl -sf --max-time 2 "$API/health" >/dev/null 2>&1; then
        bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes || true
    fi
    pkill -x lmforge 2>/dev/null || true
    return 0
}

install_core() { bash "$REPO_ROOT/scripts/install-core.sh"; }

health_ok() {
    local body
    body=$(curl -sf --max-time 20 "$API/health") || { echo "unreachable"; return 1; }
    echo "$body"
    [[ "$body" =~ \"status\"[[:space:]]*:[[:space:]]*\"ok\" ]]
}

install_ui() {
    case "$OS" in
        Linux)
            if [[ "$(uname -m)" == "aarch64" ]]; then
                echo "no Linux arm64 UI asset — skipped"
                return 0
            fi
            ;;
    esac
    bash "$REPO_ROOT/scripts/install-ui.sh"
}

run_multi_model() {
    export SKIP_START=1
    export SKIP_BUILD=1
    export LF_BIN="$BIN"
    bash "$REPO_ROOT/tests/multi_model_e2e.sh" "$@"
}

uninstall_all() {
    case "$OS" in
        Darwin) [[ ! -d "$HOME/Applications/LMForge.app" ]] || bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes ;;
        Linux)  [[ ! -x "$HOME/.local/bin/LMForge" ]] || bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes ;;
    esac
    if (( PURGE )); then export LMFORGE_PURGE=1; fi
    bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes
    sleep 2
    curl -sf --max-time 2 "$API/health" >/dev/null 2>&1 && { echo "daemon still up"; return 1; }
    echo "daemon down"
}

echo "LMForge Release E2E — $VERSION on $OS"
echo "burst=$N_REQUESTS keep=$KEEP_INSTALL (models: scripts/lib/e2e-defaults.sh)"

(( SKIP_CLEANUP )) || step "preclean" preclean
step "install-core" install_core
sleep 3
step "health" health_ok
step "install-ui" install_ui
step "multi-model e2e" run_multi_model

if (( ! KEEP_INSTALL )); then
    step "uninstall" uninstall_all
fi

echo ""
echo "========== SUMMARY =========="
for line in "${RESULTS[@]}"; do echo "$line"; done
echo ""
[[ $FAILED -eq 0 ]] || exit 1
exit 0
