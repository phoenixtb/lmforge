#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Interactive LMForge *UI* cleanup — Tauri dev, AppImage, npm/rust artefacts.
#
#   scripts/util/dev-clean-ui.sh
#   scripts/util/dev-clean-ui.sh --yes --full
#
# Flags:
#   -y, --yes              Non-interactive
#   --processes            Stop LMForge / tauri / vite
#   --appimage             Remove ~/.local/bin/LMForge
#   --desktop              Remove launcher + hicolor icons
#   --node                 ui/node_modules, dist, .svelte-kit, build
#   --rust-target          ui/src-tauri/target
#   --macos-app            ~/Applications/LMForge.app (Darwin only)
#   --app-support          macOS LMForge app support caches
#   --full                 processes+appimage+desktop+node+rust-target
#   -h, --help
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=dev-lib.sh
source "$SCRIPT_DIR/dev-lib.sh"

UI_DIR="$DEV_REPO_ROOT/ui"
DEV_NONINTERACTIVE=0
DO_STOP=0
DO_APPIMAGE=0
DO_DESKTOP=0
DO_NODE=0
DO_RUST=0
DO_MACOS_APP=0
DO_APP_SUPPORT=0

while (($#)); do
    case "$1" in
        -y|--yes)           DEV_NONINTERACTIVE=1 ;;
        --processes)        DO_STOP=1; DEV_NONINTERACTIVE=1 ;;
        --appimage)         DO_APPIMAGE=1; DEV_NONINTERACTIVE=1 ;;
        --desktop)          DO_DESKTOP=1; DEV_NONINTERACTIVE=1 ;;
        --node)             DO_NODE=1; DEV_NONINTERACTIVE=1 ;;
        --rust-target)      DO_RUST=1; DEV_NONINTERACTIVE=1 ;;
        --macos-app)        DO_MACOS_APP=1; DEV_NONINTERACTIVE=1 ;;
        --app-support)      DO_APP_SUPPORT=1; DEV_NONINTERACTIVE=1 ;;
        --full)             DO_STOP=1; DO_APPIMAGE=1; DO_DESKTOP=1; DO_NODE=1; DO_RUST=1
                            DEV_NONINTERACTIVE=1 ;;
        -h|--help)          sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)                  echo "Unknown: $1" >&2; exit 1 ;;
    esac
    shift
done

echo ""
echo -e "${BOLD}  LMForge — dev clean (UI)${NC}"
echo "  ui: $UI_DIR"
echo ""

dev_section "Audit"
dev_audit_line "ui/node_modules" "$UI_DIR/node_modules"
dev_audit_line "ui/dist" "$UI_DIR/dist"
dev_audit_line "ui/.svelte-kit" "$UI_DIR/.svelte-kit"
dev_audit_line "ui/build" "$UI_DIR/build"
dev_audit_line "ui/src-tauri/target" "$UI_DIR/src-tauri/target"
dev_audit_line "AppImage" "$HOME/.local/bin/LMForge"
dev_audit_line "desktop entry" "$HOME/.local/share/applications/lmforge.desktop"
if [[ "$(uname -s)" == "Darwin" ]]; then
    dev_audit_line "LMForge.app" "$HOME/Applications/LMForge.app"
fi
echo ""

if ! (( DEV_NONINTERACTIVE )) && dev_is_tty; then
    dev_section "Configure cleanup"
    yn=""
    dev_prompt_yn yn y "Stop UI processes (LMForge, tauri dev, vite)?"
    [[ "$yn" == "y" ]] && DO_STOP=1

    dev_prompt_yn yn y "Remove installed AppImage (~/.local/bin/LMForge)?"
    [[ "$yn" == "y" ]] && DO_APPIMAGE=1

    dev_prompt_yn yn y "Remove desktop launcher + icons?"
    [[ "$yn" == "y" ]] && DO_DESKTOP=1

    dev_prompt_yn yn y "Remove ui/node_modules + dist + .svelte-kit + build?"
    [[ "$yn" == "y" ]] && DO_NODE=1

    dev_prompt_yn yn y "Remove ui/src-tauri/target?"
    [[ "$yn" == "y" ]] && DO_RUST=1

    if [[ "$(uname -s)" == "Darwin" ]]; then
        dev_prompt_yn yn n "Remove ~/Applications/LMForge.app?"
        [[ "$yn" == "y" ]] && DO_MACOS_APP=1
        dev_prompt_yn yn n "Remove macOS app support caches?"
        [[ "$yn" == "y" ]] && DO_APP_SUPPORT=1
    fi

    echo ""
    dev_prompt_yn yn y "Proceed?"
    [[ "$yn" == "y" ]] || { echo "Aborted."; exit 0; }
fi

if ! (( DO_STOP + DO_APPIMAGE + DO_DESKTOP + DO_NODE + DO_RUST + DO_MACOS_APP + DO_APP_SUPPORT )); then
    dev_warn "nothing selected"
    exit 0
fi

dev_section "Execute"

if (( DO_STOP )); then
    dev_stop_ui
    dev_info "UI processes stopped"
fi

if (( DO_APPIMAGE )); then
    dev_remove "$HOME/.local/bin/LMForge" "AppImage"
fi

if (( DO_DESKTOP )); then
    dev_remove "$HOME/.local/share/applications/lmforge.desktop" "desktop entry"
    if [[ -d "$HOME/.local/share/icons/hicolor" ]]; then
        find "$HOME/.local/share/icons/hicolor" -path '*/apps/lmforge-ui.png' -delete 2>/dev/null || true
        command -v gtk-update-icon-cache &>/dev/null && \
            gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
    fi
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    dev_info "desktop integration removed"
fi

if (( DO_NODE )); then
    dev_remove "$UI_DIR/node_modules" "node_modules"
    dev_remove "$UI_DIR/dist" "dist"
    dev_remove "$UI_DIR/.svelte-kit" ".svelte-kit"
    dev_remove "$UI_DIR/build" "build"
fi

if (( DO_RUST )); then
    dev_remove "$UI_DIR/src-tauri/target" "tauri target"
fi

if (( DO_MACOS_APP )); then
    dev_remove "$HOME/Applications/LMForge.app" "LMForge.app"
    dev_remove "/Applications/LMForge.app" "system LMForge.app"
fi

if (( DO_APP_SUPPORT )); then
    dev_remove "$HOME/Library/Application Support/com.lmforge.ui" "app support"
    dev_remove "$HOME/Library/Application Support/com.lmforge.app" "app support (alt)"
    dev_remove "$HOME/Library/Caches/com.lmforge.ui" "ui cache"
    dev_remove "$HOME/Library/WebKit/com.lmforge.ui" "webkit cache"
fi

echo ""
dev_info "UI cleanup complete"
echo "  Reinstall UI: scripts/util/dev-clean-reinstall-ui.sh"
