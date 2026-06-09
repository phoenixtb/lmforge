#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Post-dev disk cleanup for LMForge.
#
# Audits + frees disk in 4 tiers (always shows size before deleting). Default
# is dry-run: nothing is removed unless --yes is passed.
#
# Tier breakdown (typical on a Linux+SGLang dev box):
#   target         3–6 GB    cargo build artefacts (recoverable, ~5 s rebuild)
#   ui-target      ~3 GB     Tauri Rust build artefacts
#   node_modules   500 MB+   UI JS deps (recoverable: npm ci)
#   engines        ~8.5 GB   sglang venv + uv (recoverable: lmforge init, ~5 min)
#   models         varies    HF weights (recoverable: lmforge pull, slow)
#   logs           few MB    daemon stdout/stderr
#   hf-cache       varies    ~/.cache/huggingface/hub (HF download cache)
#
# Usage:
#   ./dev_clean.sh                  # audit only (dry-run, no deletion)
#   ./dev_clean.sh --target         # rust target dirs (project + UI)
#   ./dev_clean.sh --node           # ui/node_modules + ui/dist
#   ./dev_clean.sh --engines        # ~/.lmforge/{engines,bin}
#   ./dev_clean.sh --logs           # ~/.lmforge/logs/*
#   ./dev_clean.sh --models         # ~/.lmforge/models* (re-pull needed)
#   ./dev_clean.sh --hf-cache       # ~/.cache/huggingface/hub
#   ./dev_clean.sh --all            # everything above EXCEPT --models, --hf-cache
#   ./dev_clean.sh --nuke           # full wipe: ~/.lmforge entirely + target dirs
#   --yes / -y                      # don't prompt, just do it
#
# Safe defaults: --models, --hf-cache, --nuke ALWAYS prompt regardless of --yes.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

DO_TARGET=0
DO_NODE=0
DO_ENGINES=0
DO_LOGS=0
DO_MODELS=0
DO_HFCACHE=0
DO_NUKE=0
YES=0

while (($#)); do
    case "$1" in
        --target)   DO_TARGET=1 ;;
        --node)     DO_NODE=1 ;;
        --engines)  DO_ENGINES=1 ;;
        --logs)     DO_LOGS=1 ;;
        --models)   DO_MODELS=1 ;;
        --hf-cache) DO_HFCACHE=1 ;;
        --all)      DO_TARGET=1; DO_NODE=1; DO_ENGINES=1; DO_LOGS=1 ;;
        --nuke)     DO_NUKE=1 ;;
        -y|--yes)   YES=1 ;;
        -h|--help)  sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)          echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'

# du -sh that doesn't error on missing paths and prints "0" for them.
size_of() { [[ -e "$1" ]] && du -sh "$1" 2>/dev/null | cut -f1 || echo "—"; }

# Sum bytes of an existing path, 0 if missing.
bytes_of() { [[ -e "$1" ]] && du -sb "$1" 2>/dev/null | cut -f1 || echo 0; }

confirm() {
    local prompt="$1"
    (( YES )) && return 0
    read -r -p "  $prompt [y/N] " REPLY
    [[ "$REPLY" =~ ^[Yy]$ ]]
}

remove() {
    local path="$1" label="$2"
    if [[ -e "$path" ]]; then
        local s=$(size_of "$path")
        rm -rf "$path"
        echo -e "  ${GREEN}✓${NC} removed $label ($s) — $path"
    fi
}

stop_daemon() {
    if pgrep -f "target/(debug|release)/lmforge" >/dev/null 2>&1; then
        echo -e "  ${YELLOW}⚠${NC} stopping running daemon first..."
        pkill -f "target/(debug|release)/lmforge" || true
        sleep 1
    fi
    rm -f "$HOME/.lmforge/lmforge.pid"
}

# ── Audit phase (always runs; shows what's there) ────────────────────────────
echo -e "${BOLD}LMForge disk audit${NC}"
echo "  ─────────────────────────────────────────────"
printf "  %-30s %8s   %s\n" "target (cargo)"       "$(size_of "$REPO_ROOT/target")"             "$REPO_ROOT/target"
printf "  %-30s %8s   %s\n" "ui/src-tauri/target"  "$(size_of "$REPO_ROOT/ui/src-tauri/target")" "$REPO_ROOT/ui/src-tauri/target"
printf "  %-30s %8s   %s\n" "ui/node_modules"      "$(size_of "$REPO_ROOT/ui/node_modules")"     "$REPO_ROOT/ui/node_modules"
printf "  %-30s %8s   %s\n" "ui/dist"              "$(size_of "$REPO_ROOT/ui/dist")"             "$REPO_ROOT/ui/dist"
printf "  %-30s %8s   %s\n" "~/.lmforge/engines"   "$(size_of "$HOME/.lmforge/engines")"         "$HOME/.lmforge/engines"
printf "  %-30s %8s   %s\n" "~/.lmforge/bin"       "$(size_of "$HOME/.lmforge/bin")"             "$HOME/.lmforge/bin"
printf "  %-30s %8s   %s\n" "~/.lmforge/models"    "$(size_of "$HOME/.lmforge/models")"          "$HOME/.lmforge/models"
printf "  %-30s %8s   %s\n" "~/.lmforge/logs"      "$(size_of "$HOME/.lmforge/logs")"            "$HOME/.lmforge/logs"
printf "  %-30s %8s   %s\n" "HF download cache"    "$(size_of "$HOME/.cache/huggingface/hub")"   "$HOME/.cache/huggingface/hub"
echo "  ─────────────────────────────────────────────"

TOTAL_B=0
for p in "$REPO_ROOT/target" "$REPO_ROOT/ui/src-tauri/target" "$REPO_ROOT/ui/node_modules" \
         "$REPO_ROOT/ui/dist" "$HOME/.lmforge/engines" "$HOME/.lmforge/bin" \
         "$HOME/.lmforge/models" "$HOME/.lmforge/logs" "$HOME/.cache/huggingface/hub"; do
    TOTAL_B=$(( TOTAL_B + $(bytes_of "$p") ))
done
TOTAL_H=$(numfmt --to=iec --suffix=B "$TOTAL_B" 2>/dev/null || echo "$TOTAL_B bytes")
printf "  %-30s %8s\n" "Total reclaimable (max)" "$TOTAL_H"
echo

# If nothing selected → exit after audit
if (( DO_TARGET + DO_NODE + DO_ENGINES + DO_LOGS + DO_MODELS + DO_HFCACHE + DO_NUKE == 0 )); then
    echo "  No --flag given — audit only. Pass one of:"
    echo "    --target, --node, --engines, --logs, --models, --hf-cache, --all, --nuke"
    exit 0
fi

# ── Nuke handling (highest priority, always prompts) ─────────────────────────
if (( DO_NUKE )); then
    echo -e "${RED}${BOLD}  --nuke${NC} will remove EVERYTHING listed above (including models)."
    confirm "Are you absolutely sure?" || { echo "  aborted"; exit 1; }
    stop_daemon
    remove "$REPO_ROOT/target"             "cargo target"
    remove "$REPO_ROOT/ui/src-tauri/target" "ui rust target"
    remove "$REPO_ROOT/ui/node_modules"    "ui node_modules"
    remove "$REPO_ROOT/ui/dist"            "ui dist"
    remove "$HOME/.lmforge"                "~/.lmforge (engines + models + logs + uv)"
    rm -f "$HOME/.cargo/bin/lmforge" "$HOME/.local/bin/lmforge"
    echo -e "${GREEN}  ✓ full wipe complete${NC}"
    exit 0
fi

# ── Selective tiers ──────────────────────────────────────────────────────────
ACTION_TAKEN=0

if (( DO_TARGET )); then
    if confirm "Remove cargo target dirs ($(size_of "$REPO_ROOT/target") + $(size_of "$REPO_ROOT/ui/src-tauri/target"))?"; then
        remove "$REPO_ROOT/target"              "cargo target"
        remove "$REPO_ROOT/ui/src-tauri/target" "ui rust target"
        ACTION_TAKEN=1
    fi
fi

if (( DO_NODE )); then
    if confirm "Remove ui/node_modules + ui/dist ($(size_of "$REPO_ROOT/ui/node_modules") + $(size_of "$REPO_ROOT/ui/dist"))?"; then
        remove "$REPO_ROOT/ui/node_modules" "ui node_modules"
        remove "$REPO_ROOT/ui/dist"         "ui dist"
        ACTION_TAKEN=1
    fi
fi

if (( DO_ENGINES )); then
    if confirm "Remove ~/.lmforge/{engines,bin} ($(size_of "$HOME/.lmforge/engines"))?"; then
        stop_daemon
        remove "$HOME/.lmforge/engines" "engines (sglang venv)"
        remove "$HOME/.lmforge/bin"     "uv binary"
        echo -e "  ${YELLOW}↻${NC} run \`lmforge init\` to recreate (~5 min)"
        ACTION_TAKEN=1
    fi
fi

if (( DO_LOGS )); then
    if confirm "Truncate ~/.lmforge/logs/* ($(size_of "$HOME/.lmforge/logs"))?"; then
        for f in "$HOME/.lmforge/logs"/*; do [[ -f "$f" ]] && : > "$f"; done
        echo -e "  ${GREEN}✓${NC} logs truncated"
        ACTION_TAKEN=1
    fi
fi

if (( DO_MODELS )); then
    echo -e "${YELLOW}  --models${NC} removes downloaded HF weights — they'll need to be re-pulled."
    # Models always prompts, ignoring --yes (too destructive).
    YES=0 confirm "Remove ~/.lmforge/models ($(size_of "$HOME/.lmforge/models"))?" || \
        { echo "  skipped models"; }
    if [[ "$REPLY" =~ ^[Yy]$ ]]; then
        stop_daemon
        remove "$HOME/.lmforge/models"      "models"
        rm -f "$HOME/.lmforge/models.json"
        ACTION_TAKEN=1
    fi
fi

if (( DO_HFCACHE )); then
    echo -e "${YELLOW}  --hf-cache${NC} removes shared HF cache used by ALL HF-CLI tools, not just LMForge."
    YES=0 confirm "Remove ~/.cache/huggingface/hub ($(size_of "$HOME/.cache/huggingface/hub"))?" || \
        { echo "  skipped HF cache"; }
    if [[ "$REPLY" =~ ^[Yy]$ ]]; then
        remove "$HOME/.cache/huggingface/hub" "HF download cache"
        ACTION_TAKEN=1
    fi
fi

(( ACTION_TAKEN )) || echo "  Nothing was removed."
