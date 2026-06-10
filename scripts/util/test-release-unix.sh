#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge — release smoke test (macOS / Linux)
#  Unix counterpart of test-release-windows.ps1.
#
#  Verifies a *published* GitHub release end-to-end on a real machine:
#    release assets → core install → health/sysinfo/service/autostart →
#    UI install (DMG / AppImage) → UI launch (if display available) →
#    uninstall UI → uninstall core.
#
#  Uses the scripts from this checkout (must match the tag) and the release
#  binaries from GitHub via LMFORGE_VERSION.
#
#  Usage:
#    ./scripts/util/test-release-unix.sh v0.1.5
#    ./scripts/util/test-release-unix.sh v0.1.5 --skip-uninstall
#
#  Supported: macOS (arm64), Linux x86_64 — debian/ubuntu, rhel/fedora,
#  arch, opensuse (lib checks are distro-agnostic via ldconfig).
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

VERSION="${1:-${LMFORGE_VERSION:-}}"
SKIP_UNINSTALL=false
[[ "${2:-}" == "--skip-uninstall" ]] && SKIP_UNINSTALL=true

if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <vX.Y.Z> [--skip-uninstall]" >&2
    exit 2
fi
export LMFORGE_VERSION="$VERSION"

REPO="phoenixtb/lmforge"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
API="http://127.0.0.1:11430"
BIN="$HOME/.local/bin/lmforge"
OS="$(uname -s)"
ARCH="$(uname -m)"
RESULTS=()
FAILED=0

case "$OS" in
    Darwin)
        UI_ARTIFACT="$HOME/Applications/LMForge.app"
        case "$ARCH" in
            arm64)  CORE_ASSET="lmforge-macos-arm64"; UI_ASSET="LMForge-UI-macos-arm64.dmg" ;;
            *)      echo "Unsupported macOS arch: $ARCH" >&2; exit 2 ;;
        esac ;;
    Linux)
        UI_ARTIFACT="$HOME/.local/bin/LMForge"
        case "$ARCH" in
            x86_64)  CORE_ASSET="lmforge-linux-x86_64"; UI_ASSET="LMForge-UI-linux-x86_64.AppImage" ;;
            aarch64) CORE_ASSET="lmforge-linux-arm64";  UI_ASSET="" ;;  # no UI build for arm64 linux
            *)       echo "Unsupported Linux arch: $ARCH" >&2; exit 2 ;;
        esac ;;
    *)
        echo "Unsupported OS: $OS (use test-release-windows.ps1 on Windows)" >&2; exit 2 ;;
esac

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

head_ok() {  # head_ok <url> <min-bytes>
    local url="$1" min="$2" len
    len=$(curl -sfIL "$url" | tr -d '\r' | awk 'tolower($1)=="content-length:"{print $2}' | tail -1)
    [[ -n "$len" ]] || { echo "HEAD failed: $url"; return 1; }
    [[ "$len" -ge "$min" ]] || { echo "asset too small ($len bytes): $url"; return 1; }
    echo "ok ($len bytes) $url"
}

# ── Step bodies ───────────────────────────────────────────────────────────────

release_scripts_match() {
    local n url tmp
    for n in install-core.sh install-ui.sh uninstall-core.sh uninstall-ui.sh; do
        url="https://github.com/$REPO/releases/download/$VERSION/$n"
        tmp=$(mktemp)
        curl -sfL "$url" -o "$tmp" || { echo "download failed: $url"; rm -f "$tmp"; return 1; }
        if ! diff -q "$tmp" "$REPO_ROOT/scripts/$n" >/dev/null; then
            echo "$n content mismatch (release vs repo at $VERSION)"
            rm -f "$tmp"; return 1
        fi
        rm -f "$tmp"
        echo "$n matches repo"
    done
}

release_core_binary() {
    head_ok "https://github.com/$REPO/releases/download/$VERSION/$CORE_ASSET" $((1024 * 1024))
}

release_ui_asset() {
    if [[ -z "$UI_ASSET" ]]; then echo "no UI asset for $OS/$ARCH — skipped"; return 0; fi
    head_ok "https://github.com/$REPO/releases/download/$VERSION/$UI_ASSET" $((5 * 1024 * 1024))
}

ui_runtime_deps() {
    [[ "$OS" == "Linux" ]] || { echo "n/a on macOS"; return 0; }
    local ok=0
    # Distro-agnostic: probe the loader cache instead of the package manager.
    if ldconfig -p 2>/dev/null | grep -qE "libwebkit2gtk-4\.1|libwebkitgtk-6\.0"; then
        echo "webkit2gtk 4.1+ / webkitgtk 6.0 present"
    else
        echo "webkit2gtk-4.1 / webkitgtk-6.0 NOT found — install-ui.sh will offer to install it"
        echo "  debian/ubuntu: sudo apt-get install libwebkit2gtk-4.1-0   (or libwebkitgtk-6.0-0)"
        echo "  fedora/rhel:   sudo dnf install webkit2gtk4.1"
        echo "  arch:          sudo pacman -S webkit2gtk-4.1"
        echo "  opensuse:      sudo zypper install libwebkit2gtk-4_1-0"
        ok=1
    fi
    # AppImage runtime needs FUSE 2 (or a new-enough type-2 runtime).
    if ldconfig -p 2>/dev/null | grep -q "libfuse.so.2"; then
        echo "libfuse2 present (AppImage can mount)"
    else
        echo "libfuse2 NOT found — AppImage falls back to --appimage-extract"
        echo "  debian/ubuntu: sudo apt-get install libfuse2 | fedora/rhel: fuse-libs | arch: fuse2"
    fi
    return $ok
}

preclean() {
    if [[ -e "$UI_ARTIFACT" ]]; then
        bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes || true
    fi
    if [[ -x "$BIN" ]] || curl -sf --max-time 2 "$API/health" >/dev/null 2>&1; then
        bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes || true
    fi
    pkill -x lmforge 2>/dev/null || true
    return 0
}

install_core() {
    bash "$REPO_ROOT/scripts/install-core.sh"
}

core_version_matches() {
    [[ -x "$BIN" ]] || { echo "missing $BIN"; return 1; }
    local v
    v=$("$BIN" --version 2>/dev/null)
    echo "$v"
    [[ "$v" == *"${VERSION#v}"* ]] || { echo "expected ${VERSION#v}"; return 1; }
}

health_ok() {
    local body
    body=$(curl -sf --max-time 15 "$API/health") || { echo "health unreachable"; return 1; }
    echo "$body"
    [[ "$body" == *'"ok"'* ]]
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
    [[ "$out" == *"reachable"* ]]
}

autostart_registered() {
    case "$OS" in
        Darwin)
            [[ -f "$HOME/Library/LaunchAgents/com.lmforge.daemon.plist" ]] \
                || { echo "missing launchd plist"; return 1; }
            launchctl list com.lmforge.daemon >/dev/null 2>&1 \
                || { echo "launchd job not loaded"; return 1; }
            echo "launchd plist present + loaded"
            ;;
        Linux)
            [[ -f "$HOME/.config/systemd/user/lmforge.service" ]] \
                || { echo "missing systemd unit"; return 1; }
            [[ "$(systemctl --user is-enabled lmforge.service 2>&1)" == "enabled" ]] \
                || { echo "unit not enabled"; return 1; }
            echo "systemd unit present + enabled"
            ;;
    esac
}

install_ui() {
    if [[ -z "$UI_ASSET" ]]; then echo "no UI asset for $OS/$ARCH — skipped"; return 0; fi
    bash "$REPO_ROOT/scripts/install-ui.sh"
}

ui_installed() {
    if [[ -z "$UI_ASSET" ]]; then echo "skipped"; return 0; fi
    case "$OS" in
        Darwin)
            [[ -d "$UI_ARTIFACT" ]] || { echo "missing $UI_ARTIFACT"; return 1; }
            echo "app bundle present"
            ;;
        Linux)
            [[ -x "$UI_ARTIFACT" ]] || { echo "missing/not executable: $UI_ARTIFACT"; return 1; }
            [[ -f "$HOME/.local/share/applications/lmforge.desktop" ]] \
                || { echo "missing .desktop entry"; return 1; }
            echo "AppImage + .desktop entry present"
            ;;
    esac
}

ui_launches() {
    if [[ -z "$UI_ASSET" ]]; then echo "skipped"; return 0; fi
    case "$OS" in
        Darwin)
            # install-ui.sh already ran `open`; give it a moment then verify.
            local i
            for i in $(seq 1 10); do
                pgrep -f "LMForge.app|lmforge-ui" >/dev/null 2>&1 && break
                sleep 1
            done
            pgrep -f "LMForge.app|lmforge-ui" >/dev/null 2>&1 \
                || { echo "UI process not running"; return 1; }
            echo "UI process running"
            osascript -e 'tell application "LMForge" to quit' 2>/dev/null || true
            sleep 1
            pkill -x lmforge-ui 2>/dev/null || true
            ;;
        Linux)
            if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
                echo "no display — launch check skipped (headless)"
                return 0
            fi
            "$UI_ARTIFACT" >/dev/null 2>&1 &
            local pid=$!
            sleep 5
            kill -0 "$pid" 2>/dev/null || { echo "UI exited within 5s"; return 1; }
            echo "UI process running"
            pkill -x lmforge-ui 2>/dev/null || true
            kill "$pid" 2>/dev/null || true
            ;;
    esac
}

health_after_ui() {
    health_ok
}

uninstall_ui() {
    if [[ -z "$UI_ASSET" ]]; then echo "skipped"; return 0; fi
    bash "$REPO_ROOT/scripts/uninstall-ui.sh" --yes
    [[ ! -e "$UI_ARTIFACT" ]] || { echo "$UI_ARTIFACT still exists"; return 1; }
    echo "UI removed"
}

uninstall_core() {
    bash "$REPO_ROOT/scripts/uninstall-core.sh" --yes
    [[ ! -e "$BIN" ]] || { echo "$BIN still exists"; return 1; }
    sleep 2
    if curl -sf --max-time 2 "$API/health" >/dev/null 2>&1; then
        echo "daemon still reachable after uninstall"; return 1
    fi
    echo "core removed, daemon down"
}

# ── Run ───────────────────────────────────────────────────────────────────────

echo "LMForge release smoke test — $VERSION on $OS/$ARCH"
[[ -f /etc/os-release ]] && . /etc/os-release && echo "distro: ${ID:-?} ${VERSION_ID:-}"

step "release scripts on GitHub" release_scripts_match
step "release core binary"       release_core_binary
step "release UI asset"          release_ui_asset
step "ui runtime deps"           ui_runtime_deps
step "preclean"                  preclean
step "install-core"              install_core
step "core version matches tag"  core_version_matches
step "health"                    health_ok
step "sysinfo"                   sysinfo_ok
step "service status"            service_status_ok
step "autostart registered"      autostart_registered
step "install-ui"                install_ui
step "ui installed"              ui_installed
step "ui launches"               ui_launches
step "health after ui"           health_after_ui
if ! $SKIP_UNINSTALL; then
    step "uninstall-ui"          uninstall_ui
    step "uninstall-core"        uninstall_core
fi

echo ""
echo "========== SUMMARY =========="
for line in "${RESULTS[@]}"; do echo "$line"; done
echo ""
[[ $FAILED -eq 0 ]] || exit 1
exit 0
