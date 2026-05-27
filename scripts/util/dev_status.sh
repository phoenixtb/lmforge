#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Read-only snapshot of LMForge dev state.
#   - Which binary is on PATH (symlink target + mtime)?
#   - Is the daemon running? On which port? Which engine?
#   - What's the live torch backend in the SGLang venv?
#   - Disk footprint per area.
# Use before/after a test session to confirm nothing is dangling.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
ok()   { echo -e "  ${GREEN}✓${NC} $*"; }
miss() { echo -e "  ${YELLOW}—${NC} $*"; }
bad()  { echo -e "  ${RED}✗${NC} $*"; }
sec()  { echo -e "\n${BOLD}$*${NC}"; }

sec "BINARY"
if BIN=$(command -v lmforge 2>/dev/null); then
    if [[ -L "$BIN" ]]; then
        TARGET=$(readlink -f "$BIN" 2>/dev/null || readlink "$BIN")
        [[ -f "$TARGET" ]] && ok "lmforge → $TARGET ($(date -r "$TARGET" '+%Y-%m-%d %H:%M:%S' 2>/dev/null))" \
                           || bad "lmforge → $TARGET (BROKEN SYMLINK)"
    else
        ok "lmforge at $BIN ($(date -r "$BIN" '+%Y-%m-%d %H:%M:%S' 2>/dev/null))"
    fi
    VER=$(lmforge --version 2>/dev/null || echo "?")
    ok "$VER"
else
    miss "no lmforge on PATH"
fi

sec "DAEMON"
if PID_FILE_PID=$(cat "$HOME/.lmforge/lmforge.pid" 2>/dev/null) && kill -0 "$PID_FILE_PID" 2>/dev/null; then
    ok "PID file says $PID_FILE_PID — process exists"
else
    miss "no live PID file"
fi
if curl -sf --max-time 2 http://127.0.0.1:11430/health >/dev/null 2>&1; then
    HEALTH=$(curl -s http://127.0.0.1:11430/health)
    STATUS=$(curl -s http://127.0.0.1:11430/lf/status 2>/dev/null)
    ok "daemon on :11430 — $HEALTH"
    echo "$STATUS" | jq -r '"      engine: \(.engine.id) v\(.engine.version) (\(.overall_status))  models: \(.running_models | length)"' 2>/dev/null || true
else
    miss "no daemon on :11430"
fi
PROCS=$(pgrep -af "target/(debug|release)/lmforge" || true)
[[ -n "$PROCS" ]] && echo "    running lmforge procs:" && echo "$PROCS" | sed 's/^/      /'

sec "TORCH / CUDA (in sglang venv)"
VENV_PY="$HOME/.lmforge/engines/sglang/venv/bin/python"
if [[ -x "$VENV_PY" ]]; then
    "$VENV_PY" -c "
import torch
print(f'  ✓ torch {torch.__version__}  CUDA {torch.version.cuda}  available={torch.cuda.is_available()}  devices={torch.cuda.device_count()}')
" 2>/dev/null || bad "could not import torch from venv"
else
    miss "no sglang venv (~/.lmforge/engines/sglang/venv) — run lmforge init"
fi

sec "CUDA TOOLCHAIN (host)"
command -v nvidia-smi >/dev/null && ok "driver: $(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1)" || bad "nvidia-smi missing"
command -v nvcc >/dev/null && ok "nvcc: $(nvcc --version | awk '/release/ {print $5}' | tr -d ',')" || bad "nvcc not on PATH"

sec "DISK"
size() { [[ -e "$1" ]] && du -sh "$1" 2>/dev/null | cut -f1 || echo "—"; }
printf "  %-28s %s\n" "target/"                "$(size "$REPO_ROOT/target")"
printf "  %-28s %s\n" "ui/src-tauri/target/"   "$(size "$REPO_ROOT/ui/src-tauri/target")"
printf "  %-28s %s\n" "ui/node_modules/"       "$(size "$REPO_ROOT/ui/node_modules")"
printf "  %-28s %s\n" "~/.lmforge/engines/"    "$(size "$HOME/.lmforge/engines")"
printf "  %-28s %s\n" "~/.lmforge/models/"     "$(size "$HOME/.lmforge/models")"
printf "  %-28s %s\n" "~/.lmforge/bin/"        "$(size "$HOME/.lmforge/bin")"
printf "  %-28s %s\n" "~/.lmforge/logs/"       "$(size "$HOME/.lmforge/logs")"
printf "  %-28s %s\n" "~/.cache/huggingface/"  "$(size "$HOME/.cache/huggingface")"

sec "CATALOG"
CAT="$HOME/.lmforge/catalogs/safetensors.json"
SRC="$REPO_ROOT/data/catalogs/safetensors.json"
if [[ -f "$CAT" && -f "$SRC" ]]; then
    if cmp -s "$CAT" "$SRC"; then
        ok "safetensors.json matches repo source"
    else
        bad "safetensors.json differs from repo — re-seed with: cp $SRC $CAT"
    fi
    N=$(jq '[to_entries[] | select(.key | startswith("_comment") | not)] | length' "$CAT" 2>/dev/null || echo "?")
    ok "$N shortcuts indexed"
fi

echo ""
