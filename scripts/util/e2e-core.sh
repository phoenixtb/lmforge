#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge Core — E2E test (macOS / Linux)
#  Full lifecycle: install → health → sysinfo → service → uninstall.
#  Uses the real installer/uninstaller scripts from this checkout.
#
#  Modes (one required):
#    LMFORGE_LOCAL_BIN=target/release/lmforge  ./scripts/util/e2e-core.sh
#        Test a locally built binary (CI release gate).
#    LMFORGE_VERSION=v0.1.6  ./scripts/util/e2e-core.sh
#        Test a published GitHub release (manual post-release check).
#
#  Exit code 0 = all steps passed.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="$HOME/.local/bin/lmforge"
API="http://127.0.0.1:11430"
RESULTS=()
FAILED=0

if [[ -z "${LMFORGE_LOCAL_BIN:-}" && -z "${LMFORGE_VERSION:-}" ]]; then
    echo "Set LMFORGE_LOCAL_BIN=<path> (local build) or LMFORGE_VERSION=<tag> (release)." >&2
    exit 2
fi
if [[ -n "${LMFORGE_LOCAL_BIN:-}" ]]; then
    LMFORGE_LOCAL_BIN="$(cd "$(dirname "$LMFORGE_LOCAL_BIN")" && pwd)/$(basename "$LMFORGE_LOCAL_BIN")"
    export LMFORGE_LOCAL_BIN
fi

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

# ── Step bodies ───────────────────────────────────────────────────────────────

preclean() {
    if [[ -x "$BIN" ]] || curl -sf --max-time 2 "$API/health" >/dev/null 2>&1; then
        bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes || true
    fi
    pkill -x lmforge 2>/dev/null || true
    return 0
}

install_core() {
    bash "$REPO_ROOT/scripts/install-core.sh"
}

binary_installed() {
    [[ -x "$BIN" ]] || { echo "missing $BIN"; return 1; }
    "$BIN" --version
}

health_ok() {
    local body
    body=$(curl -sf --max-time 15 "$API/health") || { echo "health unreachable"; return 1; }
    echo "$body"
    [[ "$body" == *'"ok"'* ]] || { echo "unexpected health body"; return 1; }
}

sysinfo_ok() {
    local body
    body=$(curl -sf --max-time 15 "$API/lf/sysinfo") || { echo "sysinfo unreachable"; return 1; }
    [[ "$body" == *'"cpu_pct"'* ]] || { echo "no cpu_pct in: $body"; return 1; }
    echo "sysinfo ok (cpu_pct present)"
}

service_status_ok() {
    local out
    out=$("$BIN" service status 2>&1)
    echo "$out"
    [[ "$out" == *"reachable"* ]] || return 1
}

autostart_registered() {
    case "$(uname -s)" in
        Darwin)
            local plist="$HOME/Library/LaunchAgents/com.lmforge.daemon.plist"
            [[ -f "$plist" ]] || { echo "missing $plist"; return 1; }
            echo "launchd plist present"
            launchctl list com.lmforge.daemon >/dev/null 2>&1 \
                && echo "launchd job loaded" \
                || { echo "launchd job NOT loaded"; return 1; }
            ;;
        Linux)
            local unit="$HOME/.config/systemd/user/lmforge.service"
            [[ -f "$unit" ]] || { echo "missing $unit"; return 1; }
            echo "systemd unit present"
            local state
            state=$(systemctl --user is-enabled lmforge.service 2>&1)
            [[ "$state" == "enabled" ]] || { echo "unit not enabled: $state"; return 1; }
            echo "systemd unit enabled"
            ;;
    esac
}

uninstall_core() {
    bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes
}

binary_removed() {
    [[ ! -e "$BIN" ]] || { echo "$BIN still exists"; return 1; }
    echo "binary removed"
}

daemon_down() {
    sleep 2
    if curl -sf --max-time 2 "$API/health" >/dev/null 2>&1; then
        echo "daemon still reachable after uninstall"
        return 1
    fi
    echo "daemon down"
}

autostart_removed() {
    case "$(uname -s)" in
        Darwin)
            [[ ! -f "$HOME/Library/LaunchAgents/com.lmforge.daemon.plist" ]] \
                || { echo "plist still exists"; return 1; }
            ;;
        Linux)
            [[ ! -f "$HOME/.config/systemd/user/lmforge.service" ]] \
                || { echo "unit still exists"; return 1; }
            ;;
    esac
    echo "autostart artifacts removed"
}

# ── Run ───────────────────────────────────────────────────────────────────────

step "preclean"             preclean
step "install-core"         install_core
step "binary installed"     binary_installed
step "health"               health_ok
step "sysinfo"              sysinfo_ok
step "service status"       service_status_ok
step "autostart registered" autostart_registered
step "uninstall-core"       uninstall_core
step "binary removed"       binary_removed
step "daemon down"          daemon_down
step "autostart removed"    autostart_removed

echo ""
echo "========== SUMMARY =========="
for line in "${RESULTS[@]}"; do echo "$line"; done
echo ""
[[ $FAILED -eq 0 ]] || exit 1
exit 0
