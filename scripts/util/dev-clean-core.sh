#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Interactive LMForge *core* cleanup — processes, symlinks, data dir, cargo
# artefacts. Covers dev builds, release installs, systemd, and Cursor sandbox.
#
#   scripts/util/dev-clean-core.sh
#   scripts/util/dev-clean-core.sh --yes
#   scripts/util/dev-clean-core.sh --yes --wipe-models --cargo-target
#
# Flags:
#   -y, --yes           Non-interactive (defaults below)
#   --data-dir PATH     LMForge data root (default ~/.lmforge)
#   --stop-only         Only stop processes; no file removal
#   --processes         Stop daemons / engines / pkill
#   --symlinks          Remove ~/.cargo/bin and ~/.local/bin lmforge symlinks
#   --engines           Remove $DATA_DIR/engines and bin/
#   --logs              Truncate or remove logs
#   --cargo-target      Remove repo target/ (debug + release)
#   --cursor-sandbox    Remove /tmp/cursor-sandbox-cache/*/target/*/lmforge
#   --models            Remove models/ + models.json (always confirms)
#   --hf-cache          Remove ~/.cache/huggingface/hub (always confirms)
#   --config            Remove config.toml + hardware.json (keeps models)
#   --nuke-data         Remove entire data dir (always confirms)
#   --all-safe          processes+symlinks+engines+logs (no models)
#   --full              all-safe + cargo-target + cursor-sandbox
#   -h, --help
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=dev-lib.sh
source "$SCRIPT_DIR/dev-lib.sh"

DEV_NONINTERACTIVE=0
DATA_DIR="${LMFORGE_DATA_DIR:-$HOME/.lmforge}"
DO_STOP=0
DO_SYMLINKS=0
DO_ENGINES=0
DO_LOGS=0
DO_CARGO=0
DO_SANDBOX=0
DO_MODELS=0
DO_HFCACHE=0
DO_CONFIG=0
DO_NUKE=0
STOP_ONLY=0

while (($#)); do
    case "$1" in
        -y|--yes)           DEV_NONINTERACTIVE=1 ;;
        --data-dir)         DATA_DIR="${2:?}"; DEV_NONINTERACTIVE=1; shift ;;
        --stop-only)        STOP_ONLY=1; DO_STOP=1; DEV_NONINTERACTIVE=1 ;;
        --processes)        DO_STOP=1; DEV_NONINTERACTIVE=1 ;;
        --symlinks)         DO_SYMLINKS=1; DEV_NONINTERACTIVE=1 ;;
        --engines)          DO_ENGINES=1; DEV_NONINTERACTIVE=1 ;;
        --logs)             DO_LOGS=1; DEV_NONINTERACTIVE=1 ;;
        --cargo-target)     DO_CARGO=1; DEV_NONINTERACTIVE=1 ;;
        --cursor-sandbox)   DO_SANDBOX=1; DEV_NONINTERACTIVE=1 ;;
        --models)           DO_MODELS=1; DEV_NONINTERACTIVE=1 ;;
        --hf-cache)         DO_HFCACHE=1; DEV_NONINTERACTIVE=1 ;;
        --config)           DO_CONFIG=1; DEV_NONINTERACTIVE=1 ;;
        --nuke-data)        DO_NUKE=1; DEV_NONINTERACTIVE=1 ;;
        --all-safe)         DO_STOP=1; DO_SYMLINKS=1; DO_ENGINES=1; DO_LOGS=1; DEV_NONINTERACTIVE=1 ;;
        --full)             DO_STOP=1; DO_SYMLINKS=1; DO_ENGINES=1; DO_LOGS=1
                            DO_CARGO=1; DO_SANDBOX=1; DEV_NONINTERACTIVE=1 ;;
        -h|--help)          sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)                  echo "Unknown: $1" >&2; exit 1 ;;
    esac
    shift
done

echo ""
echo -e "${BOLD}  LMForge — dev clean (core)${NC}"
echo "  data: $DATA_DIR"
echo ""

dev_section "Audit"
dev_audit_line "repo target/" "$DEV_REPO_ROOT/target"
dev_audit_line "~/.cargo/bin/lmforge" "$HOME/.cargo/bin/lmforge"
dev_audit_line "~/.local/bin/lmforge" "$HOME/.local/bin/lmforge"
dev_audit_line "data dir" "$DATA_DIR"
dev_audit_line "  └ engines" "$DATA_DIR/engines"
dev_audit_line "  └ models" "$DATA_DIR/models"
dev_audit_line "  └ logs" "$DATA_DIR/logs"
dev_audit_line "HF hub cache" "$HOME/.cache/huggingface/hub"

SANDBOX_COUNT=0
if compgen -G "/tmp/cursor-sandbox-cache/*/target/*/lmforge" >/dev/null 2>&1; then
    SANDBOX_COUNT=$(find /tmp/cursor-sandbox-cache -path '*/target/*/lmforge' -type f 2>/dev/null | wc -l)
fi
echo "  cursor-sandbox lmforge binaries: $SANDBOX_COUNT"
echo ""

if (( STOP_ONLY )); then
    dev_section "Stop processes only"
    dev_stop_core "$DATA_DIR"
    dev_info "done"
    exit 0
fi

# Interactive plan
if ! (( DEV_NONINTERACTIVE )) && dev_is_tty; then
    dev_section "Configure cleanup"
    yn=""
    dev_prompt_yn yn y "Stop all LMForge/llama-server processes and systemd service?"
    [[ "$yn" == "y" ]] && DO_STOP=1

    dev_prompt_yn yn y "Remove lmforge symlinks (~/.cargo/bin, ~/.local/bin)?"
    [[ "$yn" == "y" ]] && DO_SYMLINKS=1

    dev_prompt_yn yn y "Remove engines + uv bin ($DATA_DIR/engines, bin/)?"
    [[ "$yn" == "y" ]] && DO_ENGINES=1

    dev_prompt_yn yn y "Clear daemon logs?"
    [[ "$yn" == "y" ]] && DO_LOGS=1

    dev_prompt_yn yn n "Remove repo cargo target/ (debug+release builds)?"
    [[ "$yn" == "y" ]] && DO_CARGO=1

    dev_prompt_yn yn n "Remove Cursor sandbox stale lmforge binaries?"
    [[ "$yn" == "y" ]] && DO_SANDBOX=1

    dev_prompt_yn yn n "Remove downloaded models? (large — re-pull required)"
    [[ "$yn" == "y" ]] && DO_MODELS=1

    dev_prompt_yn yn n "Remove HF download cache (~/.cache/huggingface)?"
    [[ "$yn" == "y" ]] && DO_HFCACHE=1

    dev_prompt_yn yn n "Remove config.toml + hardware.json (keeps models)?"
    [[ "$yn" == "y" ]] && DO_CONFIG=1

    dev_prompt_yn yn n "NUKE entire data directory ($DATA_DIR)?"
    [[ "$yn" == "y" ]] && DO_NUKE=1

    echo ""
    dev_prompt_yn yn y "Proceed?"
    [[ "$yn" == "y" ]] || { echo "Aborted."; exit 0; }
fi

if ! (( DO_STOP + DO_SYMLINKS + DO_ENGINES + DO_LOGS + DO_CARGO + DO_SANDBOX + DO_MODELS + DO_HFCACHE + DO_CONFIG + DO_NUKE )); then
    dev_warn "nothing selected — audit only"
    exit 0
fi

# Destructive tiers: always confirm unless --yes and explicit flag
confirm_destructive() {
    local msg="$1"
    if (( DEV_NONINTERACTIVE )); then return 0; fi
    local yn=""
    dev_prompt_yn yn n "$msg"
    [[ "$yn" == "y" ]]
}

dev_section "Execute"

if (( DO_STOP || DO_ENGINES || DO_MODELS || DO_NUKE )); then
    dev_stop_core "$DATA_DIR"
    dev_info "processes stopped"
fi

if (( DO_NUKE )); then
    confirm_destructive "DELETE entire $DATA_DIR?" || exit 0
    dev_remove "$DATA_DIR" "data directory"
    DO_ENGINES=0; DO_LOGS=0; DO_MODELS=0; DO_CONFIG=0
fi

if (( DO_SYMLINKS )); then
    dev_remove "$HOME/.cargo/bin/lmforge" "cargo bin symlink"
    dev_remove "$HOME/.local/bin/lmforge" "local bin symlink"
fi

if (( DO_ENGINES )); then
    dev_remove "$DATA_DIR/engines" "engines"
    dev_remove "$DATA_DIR/bin" "lmforge bin (uv)"
fi

if (( DO_LOGS )); then
    if [[ -d "$DATA_DIR/logs" ]]; then
        find "$DATA_DIR/logs" -type f -exec truncate -s 0 {} + 2>/dev/null || rm -rf "$DATA_DIR/logs"/*
        dev_info "logs cleared"
    fi
fi

if (( DO_CONFIG )); then
    if confirm_destructive "Remove config.toml and hardware.json?"; then
        dev_remove "$DATA_DIR/config.toml" "config.toml"
        dev_remove "$DATA_DIR/hardware.json" "hardware.json"
        dev_remove "$DATA_DIR/engines.toml" "engines.toml"
    fi
fi

if (( DO_MODELS )); then
    if ! confirm_destructive "Remove models and models.json?"; then
        DO_MODELS=0
    fi
    if (( DO_MODELS )); then
        dev_stop_core "$DATA_DIR"
        dev_remove "$DATA_DIR/models" "models"
        rm -f "$DATA_DIR/models.json"
        dev_info "models index removed"
    fi
fi

if (( DO_CARGO )); then
    dev_stop_core "$DATA_DIR"
    dev_remove "$DEV_REPO_ROOT/target" "cargo target"
fi

if (( DO_SANDBOX )); then
    n=0
    while IFS= read -r -d '' f; do
        rm -f "$f"
        ((n++)) || true
    done < <(find /tmp/cursor-sandbox-cache -path '*/target/*/lmforge' -type f -print0 2>/dev/null || true)
    dev_info "removed $n cursor-sandbox lmforge binary(ies)"
fi

if (( DO_HFCACHE )); then
    confirm_destructive "Remove shared HuggingFace hub cache?" || DO_HFCACHE=0
    if (( DO_HFCACHE )); then
        dev_remove "$HOME/.cache/huggingface/hub" "HF hub cache"
    fi
fi

echo ""
dev_info "core cleanup complete"
echo "  Rebuild: scripts/util/dev-reinstall-core.sh"
