#!/usr/bin/env bash
# =============================================================================
#  LMForge — build the desktop UI from this checkout and install it locally
#  (macOS / Linux)
#
#  Runs `npm run tauri build` in ui/, then installs the produced artifact
#  (.dmg / .AppImage) via install-ui.sh using LMFORGE_UI_LOCAL — i.e. the same
#  install path a real user gets, but from current source instead of a release.
#
#  Usage:
#    scripts/util/build-ui-local.sh            # npm ci if needed, build, install
#    scripts/util/build-ui-local.sh --no-deps  # skip npm ci (reuse node_modules)
#
#  Requires: node/npm, the Rust toolchain, and Tauri 2 system deps (webkit2gtk,
#  libayatana-appindicator on Linux). Core must already be installed + running.
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UI_DIR="$REPO_ROOT/ui"
DO_DEPS=1
[[ "${1:-}" == "--no-deps" ]] && DO_DEPS=0

# cargo (rustup) lives in ~/.cargo/bin and needs ~/.cargo/env sourced — a
# non-login shell often lacks it. Pull it onto PATH so the build runs out of box.
if ! command -v cargo >/dev/null; then
    [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
    [[ -d "$HOME/.cargo/bin" ]] && export PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v npm >/dev/null; then
    cat >&2 <<'EOF'
npm not on PATH — install Node.js LTS (ships npm):
    macOS:  brew install node       (or download from https://nodejs.org)
    Linux:  https://github.com/nvm-sh/nvm  then: nvm install --lts
EOF
    exit 1
fi
if ! command -v cargo >/dev/null; then
    cat >&2 <<'EOF'
cargo not on PATH — install the Rust toolchain (rustup, macOS/Linux):
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
EOF
    exit 1
fi

cd "$UI_DIR"
if (( DO_DEPS )) || [[ ! -d node_modules ]]; then
    echo "==> npm ci"
    npm ci
fi

# Pick the Linux distribution format for this host so the local install path
# matches what a real user on this distro gets: native .rpm (Fedora/RHEL/SUSE)
# or .deb (Debian/Ubuntu) where the package manager resolves webkit deps;
# AppImage is the portable fallback for everything else.
detect_linux_pkg_kind() {
    local id="" like=""
    if [[ -f /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        id="${ID:-}"; like="${ID_LIKE:-}"
    fi
    case "$id" in
        fedora|rhel|centos|rocky|almalinux|ol|amzn|opensuse*|suse|sles)
            echo rpm; return ;;
        ubuntu|debian|pop|linuxmint|elementary|zorin|kali|raspbian|neon)
            echo deb; return ;;
    esac
    case " $like " in
        *rhel*|*fedora*|*suse*) echo rpm; return ;;
        *debian*|*ubuntu*)      echo deb; return ;;
    esac
    echo appimage
}

OS="$(uname -s)"
LINUX_PKG_KIND=""
BUNDLE_FLAG=()
if [[ "$OS" == "Linux" ]]; then
    LINUX_PKG_KIND=$(detect_linux_pkg_kind)
    BUNDLE_FLAG=(--bundles "$LINUX_PKG_KIND")
fi

# linuxdeploy/appimagetool are themselves AppImages that need libfuse2 to mount.
# extract-and-run makes them self-extract instead, so the build never depends on
# host FUSE (Fedora 40+ ships only fuse3). Harmless on macOS/Windows.
export APPIMAGE_EXTRACT_AND_RUN=1

echo "==> npm run tauri build ${BUNDLE_FLAG[*]+"${BUNDLE_FLAG[*]}"}"
npm run tauri build -- ${BUNDLE_FLAG[@]+"${BUNDLE_FLAG[@]}"}

# Cargo workspace places bundles under the workspace-root target/; older/standalone
# layouts use ui/src-tauri/target/. Check both.
BUNDLE_DIRS=("$REPO_ROOT/target/release/bundle" "$UI_DIR/src-tauri/target/release/bundle")
ART=""
for BUNDLE in "${BUNDLE_DIRS[@]}"; do
    case "$OS" in
        Darwin) ART=$(ls -t "$BUNDLE"/dmg/*.dmg 2>/dev/null | head -1) ;;
        Linux)
            case "$LINUX_PKG_KIND" in
                rpm) ART=$(ls -t "$BUNDLE"/rpm/*.rpm 2>/dev/null | head -1) ;;
                deb) ART=$(ls -t "$BUNDLE"/deb/*.deb 2>/dev/null | head -1) ;;
                *)   ART=$(ls -t "$BUNDLE"/appimage/*.AppImage 2>/dev/null | head -1) ;;
            esac ;;
        *)      echo "Unsupported OS for local UI build" >&2; exit 2 ;;
    esac
    [[ -n "$ART" && -f "$ART" ]] && break
done

[[ -n "${ART:-}" && -f "$ART" ]] || { echo "no UI artifact found under: ${BUNDLE_DIRS[*]}" >&2; exit 1; }
ART="$(cd "$(dirname "$ART")" && pwd)/$(basename "$ART")"
echo "==> built artifact: $ART"

echo "==> install-ui.sh (local artifact)"
LMFORGE_UI_LOCAL="$ART" bash "$REPO_ROOT/scripts/install-ui.sh"
