#!/usr/bin/env bash
# ==============================================================================
# LMForge CLI — interactive dev / test / release menu (macOS / Linux)
#
# Usage:
#   ./scripts/lmforge.sh                  # interactive menu
#   ./scripts/lmforge.sh status           # non-interactive dispatch
#   ./scripts/lmforge.sh test-multi       # run multi-model E2E
#   ./scripts/lmforge.sh release-e2e      # install from release + inference E2E
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UTIL="$REPO_ROOT/scripts/util"
cd "$REPO_ROOT"

# shellcheck source=lib/menu-common.sh
source "$SCRIPT_DIR/lib/menu-common.sh"

ACTIONS=(
    "status"
    "dev-reinstall-core"
    "dev-reinstall-ui"
    "dev-clean"
    "dev-logs"
    "test-unit"
    "test-dev"
    "test-multi"
    "test-e2e-core"
    "test-release"
    "release-e2e"
    "cleanup-core"
    "cleanup-ui"
    "quit"
)

LABELS=(
    "Status             Snapshot binaries, daemon, disk"
    "Reinstall core     Clean build + install core from repo"
    "Reinstall UI       Clean build + launch UI from repo"
    "Dev clean          Audit / wipe build artefacts"
    "Logs               Tail daemon / dev logs"
    "Test: cargo        Unit + integration (cargo test)"
    "Test: dev matrix   API + inference (dev_test.sh)"
    "Test: multi-model  Chat+embed co-load E2E"
    "Test: install E2E  Core install lifecycle (local bin)"
    "Test: release      Release smoke (no model pull)"
    "Release E2E        Install release + models + inference + cleanup"
    "Uninstall core     uninstall-core.sh"
    "Uninstall UI       uninstall-ui.sh"
    "Quit"
)

dispatch() {
    local action="$1"
    case "$action" in
        status)
            bash "$UTIL/dev_status.sh"
            ;;
        dev-reinstall-core)
            bash "$UTIL/dev-reinstall-core.sh"
            ;;
        dev-reinstall-ui)
            bash "$UTIL/dev-clean-reinstall-ui.sh"
            ;;
        dev-clean)
            bash "$UTIL/dev_clean.sh"
            ;;
        dev-logs)
            bash "$UTIL/dev_logs.sh" "$@"
            ;;
        test-unit)
            cargo test --lib && cargo test --tests -- --test-threads=1
            ;;
        test-dev)
            bash "$UTIL/dev_test.sh"
            ;;
        test-multi)
            bash "$REPO_ROOT/tests/multi_model_e2e.sh" "$@"
            ;;
        test-e2e-core)
            if [[ -x "$REPO_ROOT/target/release/lmforge" ]]; then
                LMFORGE_LOCAL_BIN="$REPO_ROOT/target/release/lmforge" bash "$UTIL/e2e-core.sh"
            else
                echo "Build release binary first: cargo build --release --bin lmforge"
                exit 1
            fi
            ;;
        test-release)
            local ver="${LMFORGE_VERSION:-latest}"
            if [[ "$ver" == "latest" ]]; then
                read -r -p "  Release tag [latest]: " ver
                ver="${ver:-latest}"
            fi
            bash "$UTIL/test-release-unix.sh" "$ver"
            ;;
        release-e2e)
            local ver="${LMFORGE_VERSION:-latest}"
            if [[ "$ver" == "latest" ]]; then
                read -r -p "  Release tag [latest]: " ver
                ver="${ver:-latest}"
            fi
            bash "$UTIL/e2e-release.sh" "$ver" "$@"
            ;;
        cleanup-core)
            bash "$REPO_ROOT/scripts/uninstall-core.sh"
            ;;
        cleanup-ui)
            bash "$REPO_ROOT/scripts/uninstall-ui.sh"
            ;;
        quit|__quit__)
            exit 0
            ;;
        *)
            echo "Unknown action: $action" >&2
            exit 1
            ;;
    esac
}

# Non-interactive: first arg is action name
if [[ $# -gt 0 && "$1" != "--" ]]; then
    dispatch "$@"
    exit $?
fi

clear
echo ""
echo -e "  ${MENU_BOLD}LMForge CLI${MENU_NC}"
echo -e "  ${MENU_DIM}Dev, test, and release tools — repo: $REPO_ROOT${MENU_NC}"
echo ""

menu_pick "Select action" ACTIONS LABELS 0
action="$PICK_RESULT"
[[ "$action" == "__quit__" ]] && exit 0

echo ""
echo -e "${MENU_BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${MENU_NC}"
echo ""

dispatch "$action"
