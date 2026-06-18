#!/usr/bin/env bash
# =============================================================================
#  LMForge — unified E2E runner (macOS / Linux)
#
#  One runner for every install source. Composes the shared lifecycle
#  (scripts/lib/e2e-lifecycle.sh) with optional UI install, asset verification,
#  and multi-model inference.
#
#  Full pre-release cycle (default for --source local):
#    full clean → build from current code → install locally (core + UI) →
#    install lifecycle → multi-model inference → full purge (incl. models),
#    unless --keep-install.
#
#  Usage:
#    scripts/util/e2e.sh --source local                 # full pre-release cycle
#    scripts/util/e2e.sh --source release:v0.1.5 --keep-install
#    scripts/util/e2e.sh --source release:v0.1.5 --verify-assets --no-inference   # release smoke
#
#  Flags:
#    --source local|release[:TAG]   Install source (default: infer from env, else release:latest)
#    --inference | --no-inference   Run tests/multi_model_e2e.sh (default: on)
#    --with-ui | --no-ui            Install + verify the desktop UI (default: on)
#    --verify-assets                Release only: check published assets + scripts match this checkout
#    --no-build                     Local only: reuse target/release binary (skip cargo build)
#    --keep-install                 Skip teardown — leave core (+UI) + models installed
#    -h | --help
#
#  Teardown is a FULL purge (binary, service, UI, ~/.lmforge incl. models) unless
#  --keep-install. Exit code 0 = all steps passed.
# =============================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

SOURCE=""
INFERENCE=1
WITH_UI=""          # empty = auto (on)
VERIFY_ASSETS=0
KEEP_INSTALL=0
DO_BUILD=1

while (($#)); do
    case "$1" in
        --source)        SOURCE="${2:?--source requires local|release[:TAG]}"; shift ;;
        --source=*)      SOURCE="${1#*=}" ;;
        --inference)     INFERENCE=1 ;;
        --no-inference)  INFERENCE=0 ;;
        --with-ui)       WITH_UI=1 ;;
        --no-ui)         WITH_UI=0 ;;
        --verify-assets) VERIFY_ASSETS=1 ;;
        --no-build)      DO_BUILD=0 ;;
        --keep-install)  KEEP_INSTALL=1 ;;
        -h|--help)       sed -n '2,/^# ===/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown flag: $1 (try --help)" >&2; exit 1 ;;
    esac
    shift
done

# ── Resolve install source → export LMFORGE_LOCAL_BIN or LMFORGE_VERSION ─────
KIND=""   # local | release
if [[ -z "$SOURCE" ]]; then
    if [[ -n "${LMFORGE_LOCAL_BIN:-}" ]]; then SOURCE="local"
    elif [[ -n "${LMFORGE_VERSION:-}" ]]; then SOURCE="release:${LMFORGE_VERSION}"
    else SOURCE="release:latest"; fi
fi

case "$SOURCE" in
    local)
        KIND="local"
        if (( ! DO_BUILD )); then
            BIN_PATH="${LMFORGE_LOCAL_BIN:-}"
            if [[ -z "$BIN_PATH" ]]; then
                if   [[ -x "$REPO_ROOT/target/release/lmforge" ]]; then BIN_PATH="$REPO_ROOT/target/release/lmforge"
                elif [[ -x "$REPO_ROOT/target/debug/lmforge"   ]]; then BIN_PATH="$REPO_ROOT/target/debug/lmforge"
                else echo "--no-build set but no binary found. Build: cargo build --release --bin lmforge" >&2; exit 1; fi
            fi
            BIN_PATH="$(cd "$(dirname "$BIN_PATH")" && pwd)/$(basename "$BIN_PATH")"
            export LMFORGE_LOCAL_BIN="$BIN_PATH"
        fi
        unset LMFORGE_VERSION 2>/dev/null || true
        ;;
    release|release:*|latest)
        KIND="release"
        local_tag="${SOURCE#release}"; local_tag="${local_tag#:}"
        [[ "$SOURCE" == "latest" ]] && local_tag="latest"
        [[ -z "$local_tag" ]] && local_tag="latest"
        if [[ "$local_tag" != "latest" ]]; then export LMFORGE_VERSION="$local_tag"; fi
        unset LMFORGE_LOCAL_BIN 2>/dev/null || true
        ;;
    *) echo "Bad --source: $SOURCE (want local|release[:TAG])" >&2; exit 1 ;;
esac

# UI default: on. --source local builds the UI from this checkout (tauri build)
# and installs it; --source release installs the published UI artifact.
[[ -z "$WITH_UI" ]] && WITH_UI=1

# shellcheck source=../lib/e2e-lifecycle.sh
source "$REPO_ROOT/scripts/lib/e2e-lifecycle.sh"

echo "LMForge E2E — source=$SOURCE ui=$WITH_UI inference=$INFERENCE verify=$VERIFY_ASSETS keep=$KEEP_INSTALL on $E2E_OS/$E2E_ARCH"

# ── Asset verification (release only) ────────────────────────────────────────
if (( VERIFY_ASSETS )); then
    if [[ "$KIND" == "release" && "${LMFORGE_VERSION:-latest}" != "latest" ]]; then
        e2e_step "release scripts match repo" e2e_release_scripts_match
        e2e_step "release core binary"        e2e_release_core_binary
        e2e_step "release UI asset"           e2e_release_ui_asset
        e2e_step "ui runtime deps"            e2e_ui_runtime_deps
    else
        echo "  --verify-assets needs --source release:<tag> (skipped)"
    fi
fi

# ── Full clean slate (any prior install: git script, dev symlink, …) ─────────
e2e_step "full clean"           e2e_full_clean

# ── Build from current source (local only) ───────────────────────────────────
if [[ "$KIND" == "local" && $DO_BUILD -eq 1 ]]; then
    e2e_step "build (cargo release)" e2e_build_local
fi

# ── Install + lifecycle ──────────────────────────────────────────────────────
e2e_step "install-core"         e2e_install_core
e2e_step "binary installed"     e2e_binary_installed
[[ "$KIND" == "release" ]] && e2e_step "core version matches tag" e2e_core_version_matches
e2e_step "health"               e2e_health_ok
e2e_step "sysinfo"              e2e_sysinfo_ok
e2e_step "service status"       e2e_service_status_ok
e2e_step "autostart registered" e2e_autostart_registered

if (( WITH_UI )); then
    if [[ "$KIND" == "local" ]]; then
        e2e_step "build+install UI (local)" e2e_install_ui_local
    else
        e2e_step "install-ui" e2e_install_ui
    fi
    e2e_step "ui installed" e2e_ui_installed
    e2e_step "ui launches"  e2e_ui_launches
    e2e_step "health after ui" e2e_health_ok
fi

# ── Inference ────────────────────────────────────────────────────────────────
if (( INFERENCE )); then
    e2e_step "multi-model inference" e2e_inference
fi

# ── Teardown (full purge incl. models, unless --keep-install) ────────────────
if (( ! KEEP_INSTALL )); then
    (( WITH_UI )) && e2e_step "uninstall-ui" e2e_uninstall_ui
    export E2E_PURGE=1
    e2e_step "uninstall-core (purge)" e2e_uninstall_core
    e2e_step "binary removed"     e2e_binary_removed
    e2e_step "daemon down"        e2e_daemon_down
    e2e_step "autostart removed"  e2e_autostart_removed
    e2e_step "data/models removed" e2e_data_removed
else
    echo ""
    echo "  --keep-install: leaving core (+UI) + models in place."
fi

e2e_summary || exit 1
exit 0
