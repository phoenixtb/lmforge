#!/usr/bin/env bash
# ==============================================================================
# LMForge CLI — dev / test / release menu (macOS / Linux)
#
# Module model (install SOURCE is a parameter, not a separate script):
#   clean    [--dev] [--purge]            uninstall core + UI (+ dev artefacts)
#   install  --source local|release[:TAG] build+install local, or install release
#   e2e      --source local|release[:TAG] [--inference|--no-inference] [--with-ui]
#                                          [--verify-assets] [--keep-install]
#   dev-up   [flags…]                      build+run from repo in debug (dev loop)
#   dev-down [flags…]                      tear down / wipe dev artefacts
#
# Usage:
#   ./scripts/lmforge.sh                              # interactive menu
#   ./scripts/lmforge.sh status
#   ./scripts/lmforge.sh e2e --source local --inference
#   ./scripts/lmforge.sh e2e --source release:v0.1.5 --keep-install
#   ./scripts/lmforge.sh install --source release:v0.1.5
#   ./scripts/lmforge.sh clean --dev --purge
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
    "install"
    "e2e"
    "clean"
    "dev-up"
    "dev-up-ui"
    "dev-down"
    "dev-logs"
    "test-unit"
    "test-dev"
    "test-multi"
    "quit"
)

LABELS=(
    "Status             Snapshot binaries, daemon, disk"
    "Install            Install core (--source local|release[:TAG])"
    "E2E                Install + lifecycle + inference (--source …)"
    "Clean              Uninstall core + UI (--dev --purge)"
    "Dev up             Build + run from repo, debug (dev loop)"
    "Dev up UI          Clean build + launch UI from repo"
    "Dev down           Audit / wipe dev build artefacts"
    "Logs               Tail daemon / dev logs"
    "Test: cargo        Unit + integration (cargo test)"
    "Test: dev matrix   API + inference (dev_test.sh)"
    "Test: multi-model  Inference suite against running daemon"
    "Quit"
)

# install --source local|release[:TAG]
run_install() {
    local src="release"
    while (($#)); do
        case "$1" in
            --source)   src="${2:?--source requires local|release[:TAG]}"; shift ;;
            --source=*) src="${1#*=}" ;;
            *) echo "install: unknown flag $1" >&2; return 1 ;;
        esac
        shift
    done
    case "$src" in
        local)
            cargo build --release --bin lmforge || return 1
            LMFORGE_LOCAL_BIN="$REPO_ROOT/target/release/lmforge" \
                bash "$REPO_ROOT/scripts/install-core.sh"
            ;;
        release|release:*|latest)
            local tag="${src#release}"; tag="${tag#:}"
            [[ -n "$tag" && "$tag" != "latest" ]] && export LMFORGE_VERSION="$tag"
            bash "$REPO_ROOT/scripts/install-core.sh"
            bash "$REPO_ROOT/scripts/install-ui.sh" || true
            ;;
        *) echo "install: bad --source $src (want local|release[:TAG])" >&2; return 1 ;;
    esac
}

# clean [--dev] [--purge]
run_clean() {
    local dev=0 purge=0
    while (($#)); do
        case "$1" in
            --dev)   dev=1 ;;
            --purge) purge=1 ;;
            *) echo "clean: unknown flag $1" >&2; return 1 ;;
        esac
        shift
    done
    bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes || true
    if (( purge )); then
        bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes --purge || true
    else
        bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes || true
    fi
    (( dev )) && bash "$UTIL/dev_clean.sh" --all --yes
    return 0
}

dispatch() {
    local action="$1"; shift || true
    case "$action" in
        status)        bash "$UTIL/dev_status.sh" ;;
        install)       run_install "$@" ;;
        e2e)           bash "$UTIL/e2e.sh" "$@" ;;
        clean)         run_clean "$@" ;;
        dev-up)        bash "$UTIL/dev-reinstall-core.sh" "$@" ;;
        dev-up-ui)     bash "$UTIL/dev-clean-reinstall-ui.sh" "$@" ;;
        dev-down)      bash "$UTIL/dev_clean.sh" "$@" ;;
        dev-logs)      bash "$UTIL/dev_logs.sh" "$@" ;;
        test-unit)     cargo test --lib && cargo test --tests -- --test-threads=1 ;;
        test-dev)      bash "$UTIL/dev_test.sh" "$@" ;;
        test-multi)    bash "$REPO_ROOT/tests/multi_model_e2e.sh" "$@" ;;
        quit|__quit__) exit 0 ;;
        *) echo "Unknown action: $action" >&2; exit 1 ;;
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
