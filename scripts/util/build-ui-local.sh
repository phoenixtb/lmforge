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

echo "==> npm run tauri build"
npm run tauri build

# Cargo workspace places bundles under the workspace-root target/; older/standalone
# layouts use ui/src-tauri/target/. Check both.
BUNDLE_DIRS=("$REPO_ROOT/target/release/bundle" "$UI_DIR/src-tauri/target/release/bundle")
ART=""
for BUNDLE in "${BUNDLE_DIRS[@]}"; do
    case "$(uname -s)" in
        Darwin) ART=$(ls -t "$BUNDLE"/dmg/*.dmg 2>/dev/null | head -1) ;;
        Linux)  ART=$(ls -t "$BUNDLE"/appimage/*.AppImage 2>/dev/null | head -1) ;;
        *)      echo "Unsupported OS for local UI build" >&2; exit 2 ;;
    esac
    [[ -n "$ART" && -f "$ART" ]] && break
done

[[ -n "${ART:-}" && -f "$ART" ]] || { echo "no UI artifact found under: ${BUNDLE_DIRS[*]}" >&2; exit 1; }
ART="$(cd "$(dirname "$ART")" && pwd)/$(basename "$ART")"
echo "==> built artifact: $ART"

echo "==> install-ui.sh (local artifact)"
LMFORGE_UI_LOCAL="$ART" bash "$REPO_ROOT/scripts/install-ui.sh"
