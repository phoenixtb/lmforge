#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — install-lifecycle E2E (macOS / Linux)
#  Full lifecycle: install → health → sysinfo → service → autostart → uninstall.
#  No inference, no UI — this is the CI release gate (e2e.yml / release.yml).
#
#  Modes (one required):
#    LMFORGE_LOCAL_BIN=target/release/lmforge  ./scripts/util/e2e-core.sh
#        Test a locally built binary (CI release gate).
#    LMFORGE_VERSION=v0.1.6  ./scripts/util/e2e-core.sh
#        Test a published GitHub release.
#
#  For inference / UI / asset-verification, use scripts/util/e2e.sh.
#  Exit code 0 = all steps passed.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ -z "${LMFORGE_LOCAL_BIN:-}" && -z "${LMFORGE_VERSION:-}" ]]; then
    echo "Set LMFORGE_LOCAL_BIN=<path> (local build) or LMFORGE_VERSION=<tag> (release)." >&2
    exit 2
fi
if [[ -n "${LMFORGE_LOCAL_BIN:-}" ]]; then
    LMFORGE_LOCAL_BIN="$(cd "$(dirname "$LMFORGE_LOCAL_BIN")" && pwd)/$(basename "$LMFORGE_LOCAL_BIN")"
    export LMFORGE_LOCAL_BIN
fi

# shellcheck source=../lib/e2e-lifecycle.sh
source "$REPO_ROOT/scripts/lib/e2e-lifecycle.sh"

e2e_step "preclean"             e2e_preclean
e2e_step "install-core"         e2e_install_core
e2e_step "binary installed"     e2e_binary_installed
e2e_step "health"               e2e_health_ok
e2e_step "sysinfo"              e2e_sysinfo_ok
e2e_step "service status"       e2e_service_status_ok
e2e_step "autostart registered" e2e_autostart_registered
e2e_step "uninstall-core"       e2e_uninstall_core
e2e_step "binary removed"       e2e_binary_removed
e2e_step "daemon down"          e2e_daemon_down
e2e_step "autostart removed"    e2e_autostart_removed

e2e_summary || exit 1
exit 0
