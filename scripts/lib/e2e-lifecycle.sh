#!/usr/bin/env bash
# =============================================================================
#  LMForge — shared install/lifecycle primitives for E2E harnesses (source only)
#
#  Single source of truth for the install → health → service → uninstall
#  lifecycle steps that e2e-core.sh and the unified e2e.sh runner share. The
#  install SOURCE is whatever the caller exported before sourcing/using these:
#     LMFORGE_LOCAL_BIN=<path>  → install a locally built binary
#     LMFORGE_VERSION=<tag>     → install a published GitHub release
#     (neither)                 → install latest release
#
#  Callers compose the step bodies with e2e_step() and finish with e2e_summary.
# =============================================================================
[[ -n "${_LMFORGE_E2E_LIFECYCLE_LOADED:-}" ]] && return 0
_LMFORGE_E2E_LIFECYCLE_LOADED=1

E2E_REPO="${E2E_REPO:-phoenixtb/lmforge}"
E2E_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
E2E_API="${E2E_API:-http://127.0.0.1:11430}"
E2E_BIN="${E2E_BIN:-$HOME/.local/bin/lmforge}"
E2E_OS="$(uname -s)"
E2E_ARCH="$(uname -m)"

# Which Linux UI format this host installs (must match install-ui.sh's logic):
# native rpm/deb where the package manager resolves webkit deps, AppImage
# fallback otherwise. Drives the UI asset name + installed-artifact path below.
e2e_linux_pkg_kind() {
    local id="" like=""
    if [[ -f /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        id="${ID:-}"; like="${ID_LIKE:-}"
    fi
    case "$id" in
        fedora|rhel|centos|rocky|almalinux|ol|amzn|opensuse*|suse|sles) echo rpm; return ;;
        ubuntu|debian|pop|linuxmint|elementary|zorin|kali|raspbian|neon) echo deb; return ;;
    esac
    case " $like " in
        *rhel*|*fedora*|*suse*) echo rpm; return ;;
        *debian*|*ubuntu*)      echo deb; return ;;
    esac
    echo appimage
}

E2E_LINUX_PKG_KIND=""

# Per-platform UI artifact path + release asset names (empty asset = none ships).
case "$E2E_OS" in
    Darwin)
        E2E_UI_ARTIFACT="$HOME/Applications/LMForge.app"
        case "$E2E_ARCH" in
            arm64) E2E_CORE_ASSET="lmforge-macos-arm64"; E2E_UI_ASSET="LMForge-UI-macos-arm64.dmg" ;;
            *)     E2E_CORE_ASSET=""; E2E_UI_ASSET="" ;;
        esac ;;
    Linux)
        E2E_LINUX_PKG_KIND="$(e2e_linux_pkg_kind)"
        case "$E2E_ARCH" in
            x86_64)
                E2E_CORE_ASSET="lmforge-linux-x86_64"
                case "$E2E_LINUX_PKG_KIND" in
                    rpm) E2E_UI_ASSET="LMForge-UI-linux-x86_64.rpm"; E2E_UI_ARTIFACT="/usr/bin/lmforge-ui" ;;
                    deb) E2E_UI_ASSET="LMForge-UI-linux-x86_64.deb"; E2E_UI_ARTIFACT="/usr/bin/lmforge-ui" ;;
                    *)   E2E_UI_ASSET="LMForge-UI-linux-x86_64.AppImage"; E2E_UI_ARTIFACT="$HOME/.local/bin/LMForge" ;;
                esac ;;
            aarch64) E2E_CORE_ASSET="lmforge-linux-arm64"; E2E_UI_ASSET=""; E2E_UI_ARTIFACT="" ;;
            *)       E2E_CORE_ASSET=""; E2E_UI_ASSET=""; E2E_UI_ARTIFACT="" ;;
        esac ;;
esac

export LMFORGE_YES=1

# ── Result accumulation ──────────────────────────────────────────────────────
E2E_RESULTS=()
E2E_FAILED=0

e2e_step() {
    local name="$1"; shift
    echo ""
    echo "=== $name ==="
    if "$@"; then
        E2E_RESULTS+=("PASS  $name")
        echo "PASS  $name"
    else
        E2E_RESULTS+=("FAIL  $name")
        echo "FAIL  $name"
        E2E_FAILED=$((E2E_FAILED + 1))
    fi
}

# Returns non-zero when any step failed — callers `exit` on the result.
e2e_summary() {
    echo ""
    echo "========== SUMMARY =========="
    local line
    for line in "${E2E_RESULTS[@]}"; do echo "$line"; done
    echo ""
    [[ $E2E_FAILED -eq 0 ]]
}

# ── Release-asset verification (release source only) ─────────────────────────
e2e_head_ok() {  # <url> <min-bytes>
    local url="$1" min="$2" len
    len=$(curl -sfIL "$url" | tr -d '\r' | awk 'tolower($1)=="content-length:"{print $2}' | tail -1)
    [[ -n "$len" ]] || { echo "HEAD failed: $url"; return 1; }
    [[ "$len" -ge "$min" ]] || { echo "asset too small ($len bytes): $url"; return 1; }
    echo "ok ($len bytes) $url"
}

e2e_release_scripts_match() {
    local n url tmp
    for n in install-core.sh install-ui.sh uninstall-core.sh uninstall-ui.sh; do
        url="https://github.com/$E2E_REPO/releases/download/$LMFORGE_VERSION/$n"
        tmp=$(mktemp)
        curl -sfL "$url" -o "$tmp" || { echo "download failed: $url"; rm -f "$tmp"; return 1; }
        if ! diff -q "$tmp" "$E2E_REPO_ROOT/scripts/$n" >/dev/null; then
            echo "$n content mismatch (release vs repo at $LMFORGE_VERSION)"
            rm -f "$tmp"; return 1
        fi
        rm -f "$tmp"
        echo "$n matches repo"
    done
}

e2e_release_core_binary() {
    [[ -n "$E2E_CORE_ASSET" ]] || { echo "no core asset for $E2E_OS/$E2E_ARCH"; return 1; }
    e2e_head_ok "https://github.com/$E2E_REPO/releases/download/$LMFORGE_VERSION/$E2E_CORE_ASSET" $((1024 * 1024))
}

e2e_release_ui_asset() {
    [[ -n "$E2E_UI_ASSET" ]] || { echo "no UI asset for $E2E_OS/$E2E_ARCH — skipped"; return 0; }
    e2e_head_ok "https://github.com/$E2E_REPO/releases/download/$LMFORGE_VERSION/$E2E_UI_ASSET" $((5 * 1024 * 1024))
}

e2e_ui_runtime_deps() {
    [[ "$E2E_OS" == "Linux" ]] || { echo "n/a on macOS"; return 0; }
    local ok=0
    echo "linux UI package kind: ${E2E_LINUX_PKG_KIND:-appimage}"
    if ldconfig -p 2>/dev/null | grep -qE "libwebkit2gtk-4\.1|libwebkitgtk-6\.0"; then
        echo "webkit2gtk 4.1+ / webkitgtk 6.0 present"
    elif [[ "${E2E_LINUX_PKG_KIND:-appimage}" == "appimage" ]]; then
        # AppImage can't pull deps — webkit must already be present.
        echo "webkit2gtk-4.1 / webkitgtk-6.0 NOT found — install-ui.sh will offer to install it"
        ok=1
    else
        # Native rpm/deb declares webkit as a dependency; the package manager
        # installs it during install-ui, so absence here is not a failure.
        echo "webkit2gtk not present yet — native $E2E_LINUX_PKG_KIND package will pull it in at install"
    fi
    if [[ "${E2E_LINUX_PKG_KIND:-appimage}" == "appimage" ]]; then
        if ldconfig -p 2>/dev/null | grep -q "libfuse.so.2"; then
            echo "libfuse2 present (AppImage can mount)"
        else
            echo "libfuse2 NOT found — AppImage launches in extract-and-run mode"
        fi
    fi
    return $ok
}

# ── Build from current source (local install only) ──────────────────────────
# rustup installs cargo to ~/.cargo/bin and relies on ~/.cargo/env being sourced
# by the login shell. A non-login shell (or the menu launched from Finder/an IDE)
# often lacks it, so the build step dies with "cargo: command not found". Pull it
# onto PATH ourselves so the harness runs out of the box.
e2e_ensure_cargo() {
    command -v cargo >/dev/null 2>&1 && { echo "cargo resolved at $(command -v cargo)"; return 0; }
    [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
    [[ -d "$HOME/.cargo/bin" ]] && export PATH="$HOME/.cargo/bin:$PATH"
    if command -v cargo >/dev/null 2>&1; then
        echo "cargo resolved at $(command -v cargo)"
        return 0
    fi
    cat >&2 <<'EOF'
cargo not found — the Rust toolchain is not installed. Install rustup (macOS/Linux):
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
Then re-run this command. (Use rustup, not `brew install rust` — rustup wires
PATH into your shell profiles and ships clippy/rustfmt that the release checks need.)
EOF
    return 1
}

e2e_build_local() {
    e2e_ensure_cargo || return 1
    ( cd "$E2E_REPO_ROOT" && cargo build --release --bin lmforge ) || return 1
    local b="$E2E_REPO_ROOT/target/release/lmforge"
    [[ -x "$b" ]] || { echo "build produced no binary at $b"; return 1; }
    export LMFORGE_LOCAL_BIN="$b"
    echo "built $("$b" --version 2>/dev/null | head -1)"
}

# ── Install lifecycle ────────────────────────────────────────────────────────
# Light preclean: remove any prior install but KEEP data/models (CI gate uses
# this). Uninstallers run unconditionally; without --purge they leave models
# intact while still clearing the binary/autostart/PATH/engine leftovers.
e2e_preclean() {
    e2e_kill_engines
    bash "$E2E_REPO_ROOT/scripts/uninstall-ui.sh" --yes >/dev/null 2>&1 || true
    bash "$E2E_REPO_ROOT/scripts/uninstall-core.sh" --yes >/dev/null 2>&1 || true
    e2e_kill_engines
    return 0
}

# Stop the daemon + ALL engine subprocesses (llama-server, …). Orphaned engine
# children survive a crashed/aborted run and keep VRAM held; if left running they
# exhaust the GPU and a dying daemon can hold the API port, breaking the next
# install's daemon start.
e2e_kill_engines() {
    pkill -x lmforge 2>/dev/null || true
    pkill -x llama-server 2>/dev/null || true
    pkill -f "$HOME/.lmforge/" 2>/dev/null || true
    sleep 1
    return 0
}

# Full clean slate: stop everything, remove any install (git script, dev symlink,
# …) AND all data. The uninstallers run unconditionally (NOT gated on the
# binary/UI existing) — they are safe when nothing is installed and are exactly
# what clears binary-absent leftovers (autostart entry, PATH, engines dir).
e2e_full_clean() {
    e2e_kill_engines
    bash "$E2E_REPO_ROOT/scripts/uninstall-ui.sh" --yes >/dev/null 2>&1 || true
    bash "$E2E_REPO_ROOT/scripts/uninstall-core.sh" --yes --purge >/dev/null 2>&1 || true
    e2e_kill_engines
    rm -rf "$HOME/.lmforge" 2>/dev/null || true
    echo "clean slate — install + data removed"
    return 0
}

e2e_install_core() { bash "$E2E_REPO_ROOT/scripts/install-core.sh"; }

e2e_binary_installed() {
    [[ -x "$E2E_BIN" ]] || { echo "missing $E2E_BIN"; return 1; }
    "$E2E_BIN" --version
}

# When LMFORGE_VERSION is set, assert the installed binary reports that tag.
e2e_core_version_matches() {
    [[ -x "$E2E_BIN" ]] || { echo "missing $E2E_BIN"; return 1; }
    local v; v=$("$E2E_BIN" --version 2>/dev/null)
    echo "$v"
    [[ -z "${LMFORGE_VERSION:-}" ]] && return 0
    [[ "$v" == *"${LMFORGE_VERSION#v}"* ]] || { echo "expected ${LMFORGE_VERSION#v}"; return 1; }
}

e2e_health_ok() {
    local body
    body=$(curl -sf --max-time 20 "$E2E_API/health") || { echo "health unreachable"; return 1; }
    echo "$body"
    [[ "$body" =~ \"status\"[[:space:]]*:[[:space:]]*\"ok\" ]] || { echo "unexpected health body"; return 1; }
}

e2e_sysinfo_ok() {
    local body
    body=$(curl -sf --max-time 15 "$E2E_API/lf/sysinfo") || { echo "sysinfo unreachable"; return 1; }
    [[ "$body" == *'"cpu_pct"'* ]] || { echo "no cpu_pct in: $body"; return 1; }
    echo "sysinfo ok (cpu_pct present)"
}

e2e_service_status_ok() {
    local out; out=$("$E2E_BIN" service status 2>&1)
    echo "$out"
    [[ "$out" == *"reachable"* ]]
}

e2e_autostart_registered() {
    case "$E2E_OS" in
        Darwin)
            local plist="$HOME/Library/LaunchAgents/com.lmforge.daemon.plist"
            [[ -f "$plist" ]] || { echo "missing $plist"; return 1; }
            launchctl list com.lmforge.daemon >/dev/null 2>&1 \
                && echo "launchd plist present + loaded" \
                || { echo "launchd job NOT loaded"; return 1; }
            ;;
        Linux)
            local unit="$HOME/.config/systemd/user/lmforge.service"
            [[ -f "$unit" ]] || { echo "missing $unit"; return 1; }
            [[ "$(systemctl --user is-enabled lmforge.service 2>&1)" == "enabled" ]] \
                || { echo "unit not enabled"; return 1; }
            echo "systemd unit present + enabled"
            ;;
    esac
}

# ── UI install lifecycle ─────────────────────────────────────────────────────
e2e_install_ui() {
    [[ -n "$E2E_UI_ASSET" ]] || { echo "no UI asset for $E2E_OS/$E2E_ARCH — skipped"; return 0; }
    bash "$E2E_REPO_ROOT/scripts/install-ui.sh"
}

# Build the UI from current source and install it (local pre-release path).
e2e_install_ui_local() {
    [[ -n "$E2E_UI_ASSET" ]] || { echo "no UI build target for $E2E_OS/$E2E_ARCH — skipped"; return 0; }
    bash "$E2E_REPO_ROOT/scripts/util/build-ui-local.sh"
}

e2e_ui_installed() {
    [[ -n "$E2E_UI_ASSET" ]] || { echo "skipped"; return 0; }
    case "$E2E_OS" in
        Darwin)
            [[ -d "$E2E_UI_ARTIFACT" ]] || { echo "missing $E2E_UI_ARTIFACT"; return 1; }
            echo "app bundle present" ;;
        Linux)
            [[ -x "$E2E_UI_ARTIFACT" ]] || { echo "missing/not executable: $E2E_UI_ARTIFACT"; return 1; }
            case "$E2E_LINUX_PKG_KIND" in
                rpm|deb)
                    [[ -f /usr/share/applications/LMForge.desktop ]] \
                        || { echo "missing /usr/share/applications/LMForge.desktop"; return 1; }
                    echo "native $E2E_LINUX_PKG_KIND package ($E2E_UI_ARTIFACT) + .desktop present" ;;
                *)
                    [[ -f "$HOME/.local/share/applications/lmforge.desktop" ]] \
                        || { echo "missing .desktop entry"; return 1; }
                    echo "AppImage + .desktop entry present" ;;
            esac ;;
    esac
}

e2e_ui_launches() {
    [[ -n "$E2E_UI_ASSET" ]] || { echo "skipped"; return 0; }
    case "$E2E_OS" in
        Darwin)
            local i
            for i in $(seq 1 10); do
                pgrep -f "LMForge.app|lmforge-ui" >/dev/null 2>&1 && break
                sleep 1
            done
            pgrep -f "LMForge.app|lmforge-ui" >/dev/null 2>&1 \
                || { echo "UI process not running"; return 1; }
            echo "UI process running"
            osascript -e 'tell application "LMForge" to quit' 2>/dev/null || true
            sleep 1; pkill -x lmforge-ui 2>/dev/null || true ;;
        Linux)
            if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
                echo "no display — launch check skipped (headless)"; return 0
            fi
            # APPIMAGE_EXTRACT_AND_RUN lets the AppImage fallback launch without
            # libfuse2; it's ignored by the native /usr/bin/lmforge-ui binary.
            APPIMAGE_EXTRACT_AND_RUN=1 "$E2E_UI_ARTIFACT" >/dev/null 2>&1 &
            local pid=$!; sleep 5
            kill -0 "$pid" 2>/dev/null || { echo "UI exited within 5s"; return 1; }
            echo "UI process running"
            pkill -x lmforge-ui 2>/dev/null || true; kill "$pid" 2>/dev/null || true ;;
    esac
}

# ── Teardown ─────────────────────────────────────────────────────────────────
e2e_uninstall_ui() {
    [[ -n "$E2E_UI_ASSET" ]] || { echo "skipped"; return 0; }
    bash "$E2E_REPO_ROOT/scripts/uninstall-ui.sh" --yes
    [[ ! -e "$E2E_UI_ARTIFACT" ]] || { echo "$E2E_UI_ARTIFACT still exists"; return 1; }
    echo "UI removed"
}

# Honours E2E_PURGE=1 to also delete ~/.lmforge (models + config).
e2e_uninstall_core() {
    if [[ "${E2E_PURGE:-0}" == "1" ]]; then
        bash "$E2E_REPO_ROOT/scripts/uninstall-core.sh" --yes --purge
    else
        bash "$E2E_REPO_ROOT/scripts/uninstall-core.sh" --yes
    fi
}

e2e_binary_removed() {
    [[ ! -e "$E2E_BIN" ]] || { echo "$E2E_BIN still exists"; return 1; }
    echo "binary removed"
}

e2e_data_removed() {
    [[ -d "$HOME/.lmforge/models" ]] && { echo "models still present at ~/.lmforge/models"; return 1; }
    echo "data/models removed"
}

e2e_daemon_down() {
    sleep 2
    if curl -sf --max-time 2 "$E2E_API/health" >/dev/null 2>&1; then
        echo "daemon still reachable after uninstall"; return 1
    fi
    echo "daemon down"
}

e2e_autostart_removed() {
    case "$E2E_OS" in
        Darwin)
            [[ ! -f "$HOME/Library/LaunchAgents/com.lmforge.daemon.plist" ]] \
                || { echo "plist still exists"; return 1; } ;;
        Linux)
            [[ ! -f "$HOME/.config/systemd/user/lmforge.service" ]] \
                || { echo "unit still exists"; return 1; } ;;
    esac
    echo "autostart artifacts removed"
}

# ── Engine preflight ─────────────────────────────────────────────────────────
# Run the ACTIVE engine's binary directly (not via the daemon) so a broken
# install fails fast with remediation guidance instead of an opaque 503 deep in
# TC-E01. Catches e.g. a Homebrew oMLX venv whose pinned python was upgraded out
# from under it ("bad interpreter"), or a half-extracted llama-server.
# Return 0 if dotted version $1 < $2 (numeric, up to 3 components). Non-numeric
# trailing text is ignored (10# strips leading zeros / forces base-10).
_e2e_ver_lt() {
    local IFS=. a b i x y
    read -r -a a <<< "$1"; read -r -a b <<< "$2"
    for i in 0 1 2; do
        x=${a[i]:-0}; x=${x//[!0-9]/}; x=${x:-0}
        y=${b[i]:-0}; y=${y//[!0-9]/}; y=${y:-0}
        (( 10#$x < 10#$y )) && return 0
        (( 10#$x > 10#$y )) && return 1
    done
    return 1
}

e2e_engine_preflight() {
    local engine
    engine=$(curl -sf --max-time 5 "$E2E_API/lf/engines" 2>/dev/null \
        | jq -r '.engines[] | select(.active==true) | .id' 2>/dev/null | head -1)
    [[ -n "$engine" ]] || { echo "could not read active engine from $E2E_API/lf/engines (daemon up?)"; return 1; }
    echo "active engine: $engine"

    local bin=""
    case "$engine" in
        omlx)
            bin="$(command -v omlx 2>/dev/null)"
            [[ -n "$bin" ]] || { echo "omlx not on PATH — reinstall: brew install jundot/omlx/omlx"; return 1; } ;;
        llamacpp)
            # Mirror the daemon's resolve_executable() order: PATH, then the
            # variant-aware layout (engines/llamacpp/variants/<id>/llama-server),
            # then the legacy flat layout (engines/llama-server).
            bin="$(command -v llama-server 2>/dev/null)"
            [[ -n "$bin" ]] || bin="$(ls -t "$HOME"/.lmforge/engines/llamacpp/variants/*/llama-server 2>/dev/null | head -1)"
            [[ -n "$bin" ]] || bin="$(ls -t "$HOME"/.lmforge/engines/llama-server 2>/dev/null | head -1)"
            [[ -n "$bin" ]] || bin="$(ls -t "$HOME"/.lmforge/engines/llamacpp/*/llama-server 2>/dev/null | head -1)"
            [[ -n "$bin" ]] || { echo "llama-server not found — reinstall: lmforge engine install llamacpp"; return 1; } ;;
        *)
            echo "no preflight defined for engine '$engine' — skipped"; return 0 ;;
    esac

    local out
    if out=$("$bin" --version 2>&1); then
        echo "engine binary OK: $bin"
        echo "$out" | head -1
        # Version-gate the engine. oMLX 0.4.0–0.4.3 regressed the Qwen3-VL prefill
        # path (jundot/omlx#1685); 0.4.4 fixes it. Warn below the floor so VLM
        # SKIP/FAILs aren't mistaken for a missing capability. Keep the floor in
        # sync with engines.toml min_version.
        if [[ "$engine" == "omlx" ]]; then
            local ver; ver=$(printf '%s\n' "$out" | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
            if [[ -n "$ver" ]] && _e2e_ver_lt "$ver" "0.4.4"; then
                echo "  ⚠ oMLX $ver is below the validated floor (0.4.4)."
                echo "    VLM (Qwen3-VL) chat completions crash with 'There is no Stream(gpu, 1)'"
                echo "    on 0.4.0–0.4.3 (jundot/omlx#1685). Upgrade:"
                echo "      brew upgrade omlx   # or: brew install jundot/omlx/omlx@0.4.4"
            fi
        fi
        return 0
    fi
    echo "engine binary BROKEN: $bin"
    echo "$out" | head -3
    case "$engine" in
        omlx)     echo "  fix: brew reinstall omlx  (or restore its interpreter, e.g. brew install python@3.11)" ;;
        llamacpp) echo "  fix: lmforge engine install llamacpp  (re-extract the llama.cpp build)" ;;
    esac
    return 1
}

# ── Inference (delegates to the shared multi-model suite) ────────────────────
# Per-platform capability auto-skip so the suite runs out of the box:
#   • Apple Silicon (oMLX) has no speculative/MTP path — the adapter hardcodes
#     spec_mode=Off — so skip MTP and avoid pulling a 4B MTP GGUF that cannot
#     accelerate here. Override with E2E_WITH_MTP=1.
# Other suites (VLM, rerank) already skip themselves at runtime when the active
# engine/model lacks the capability, so they need no OS gate.
e2e_inference() {
    local extra=()
    if [[ "$E2E_OS" == "Darwin" && "${E2E_WITH_MTP:-0}" != "1" ]]; then
        extra+=(--skip-mtp)
    fi
    SKIP_START=1 SKIP_BUILD=1 LF_BIN="$E2E_BIN" \
        bash "$E2E_REPO_ROOT/tests/multi_model_e2e.sh" ${extra[@]+"${extra[@]}"} "$@"
}

# ── Thinking regression gate (delegates to think_bench.py --assert) ──────────
# Validates the reasoning pipeline (ADR-007) against the running daemon:
#   • think=off must produce a non-blank answer (Fix #3c plain-client default)
#   • no reasoning==content duplication (Fix #5a)
#   • no engine errors
# Bounded by default to the e2e chat model so it stays fast; override the model
# set with E2E_THINK_MODELS="a b c". Results go to a temp dir (not committed).
# Requires python3; skips gracefully if absent.
e2e_thinking() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo "  python3 not found — skipping thinking gate"
        return 0
    fi
    local models="${E2E_THINK_MODELS:-${CHAT_MODEL:-${E2E_CHAT_MODEL:-qwen3.5:2b:4bit}}}"
    local outdir
    outdir="$(mktemp -d 2>/dev/null || echo "${TMPDIR:-/tmp}/lmforge-think-$$")"
    local strict=()
    [[ "${E2E_THINK_STRICT:-0}" == "1" ]] && strict+=(--assert-strict)
    # shellcheck disable=SC2086
    python3 "$E2E_REPO_ROOT/tests/bench/think_bench.py" \
        --base "${E2E_API:-http://127.0.0.1:11430}" \
        --models $models \
        --quick --assert "${strict[@]}" \
        --outdir "$outdir" --no-capture-logs
    local rc=$?
    rm -rf "$outdir" 2>/dev/null || true
    return $rc
}
