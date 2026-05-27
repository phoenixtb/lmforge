#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Dev clean-reinstall for LMForge Core on Linux + NVIDIA + CUDA 13.x + SGLang.
#
# Idempotent. Resumable. Flag-driven so you don't pay the 8 GB engine-rebuild
# cost on every Rust-only iteration.
#
# Usage:
#   ./dev-clean-reinstall-core_linux-sglang-cuda13.sh [flags]
#
# Flags:
#   --keep-engines     Skip the ~/.lmforge/engines + bin wipe (saves ~5 min)
#   --keep-models      Default behaviour, kept for explicit-intent docs
#   --wipe-models      Also remove ~/.lmforge/models (forces re-pull)
#   --no-cargo-clean   Incremental Rust build instead of from-scratch
#   --no-init          Skip `lmforge init` (just rebuild + symlink)
#   --no-start         Skip launching the daemon at the end
#   --release          Build with --release instead of debug (slower, smaller)
#   --pull MODEL       Smoke-pull this catalog key after start (default: skip)
#   -h | --help        Show this help and exit
#
# Exit codes:
#   0  success
#   1  preflight failure (nvcc missing, etc.)
#   2  build failure
#   3  init failure
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# Resolve repo root from this script's location (works regardless of cwd).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── defaults ─────────────────────────────────────────────────────────────────
KEEP_ENGINES=0
WIPE_MODELS=0
CARGO_CLEAN=1
DO_INIT=1
DO_START=1
PROFILE="debug"          # 'debug' | 'release'
PULL_MODEL=""

# ── arg parsing ──────────────────────────────────────────────────────────────
while (($#)); do
    case "$1" in
        --keep-engines)   KEEP_ENGINES=1 ;;
        --keep-models)    WIPE_MODELS=0 ;;
        --wipe-models)    WIPE_MODELS=1 ;;
        --no-cargo-clean) CARGO_CLEAN=0 ;;
        --no-init)        DO_INIT=0 ;;
        --no-start)       DO_START=0 ;;
        --release)        PROFILE="release" ;;
        --pull)           PULL_MODEL="${2:?--pull requires a model key}"; shift ;;
        -h|--help)        sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)                echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

# ── colours / helpers ────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit "${2:-1}"; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# ── 0. Preflight (fail fast) ─────────────────────────────────────────────────
section "[0/6] Preflight"
[[ "$(uname -s)" == "Linux" ]] || error "Linux-only script; this OS: $(uname -s)" 1
command -v cargo    >/dev/null || error "cargo not on PATH — install Rust toolchain first" 1
command -v nvcc     >/dev/null || error "nvcc not on PATH — install nvidia-cuda-toolkit and add /usr/local/cuda/bin to PATH" 1
command -v nvidia-smi >/dev/null || error "nvidia-smi not found — NVIDIA driver missing" 1
CUDA_DRIVER=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1)
CUDA_VER=$(nvidia-smi 2>/dev/null | awk '/CUDA Version/ {print $9}')
NVCC_VER=$(nvcc --version | awk '/release/ {print $5}' | tr -d ',')
info "driver=$CUDA_DRIVER  cuda(driver)=$CUDA_VER  cuda(nvcc)=$NVCC_VER"
info "repo=$REPO_ROOT  profile=$PROFILE"

# ── 1. Stop any running daemon ───────────────────────────────────────────────
section "[1/6] Stopping daemons (if any)"
pkill -f "target/(debug|release)/lmforge" 2>/dev/null && info "killed stray lmforge process(es)" || info "no daemon running"
rm -f "$HOME/.lmforge/lmforge.pid"

# ── 2. Wipe state ────────────────────────────────────────────────────────────
section "[2/6] Wiping stale state"
if (( KEEP_ENGINES )); then
    warn "skipping engines/bin wipe (--keep-engines) — Python venv not refreshed"
else
    rm -rf "$HOME/.lmforge/engines" "$HOME/.lmforge/bin"
    info "removed ~/.lmforge/{engines,bin} — uv + sglang venv will be rebuilt"
fi
if (( WIPE_MODELS )); then
    rm -rf "$HOME/.lmforge/models" "$HOME/.lmforge/models.json"
    info "removed ~/.lmforge/models* — every pull will redownload"
fi
# Always re-seed catalogs so we pick up edits in data/catalogs/
mkdir -p "$HOME/.lmforge/catalogs"
cp "$REPO_ROOT/data/catalogs/safetensors.json" "$HOME/.lmforge/catalogs/safetensors.json"
cp "$REPO_ROOT/data/catalogs/mlx.json"          "$HOME/.lmforge/catalogs/mlx.json"
info "catalogs re-seeded from $REPO_ROOT/data/catalogs/"
# Truncate (don't delete) logs so `tail -f` keeps working across sessions
: > "$HOME/.lmforge/logs/daemon.out.log" 2>/dev/null || true
: > "$HOME/.lmforge/logs/daemon.err.log" 2>/dev/null || true

# ── 3. Build ─────────────────────────────────────────────────────────────────
section "[3/6] Build ($PROFILE)"
cd "$REPO_ROOT"
if (( CARGO_CLEAN )); then
    cargo clean 2>&1 | tail -1
    info "cargo clean done"
fi
BUILD_ARGS=("--bin" "lmforge")
[[ "$PROFILE" == "release" ]] && BUILD_ARGS+=("--release")
cargo build "${BUILD_ARGS[@]}" 2>&1 | tail -3 || error "cargo build failed" 2

# ── 4. Symlink onto PATH ─────────────────────────────────────────────────────
section "[4/6] Symlinking ~/.cargo/bin/lmforge → target/$PROFILE/lmforge"
mkdir -p "$HOME/.cargo/bin"
rm -f "$HOME/.cargo/bin/lmforge" "$HOME/.local/bin/lmforge"
ln -sf "$REPO_ROOT/target/$PROFILE/lmforge" "$HOME/.cargo/bin/lmforge"
lmforge --version || error "binary not executable — check PATH" 2
info "lmforge $(lmforge --version | awk '{print $2}') on PATH"

# ── 5. Init ──────────────────────────────────────────────────────────────────
if (( DO_INIT )); then
    section "[5/6] lmforge init (UV_TORCH_BACKEND=auto picks cu13x from driver)"
    mkdir -p "$HOME/.lmforge/logs"
    if RUST_LOG="lmforge=debug,info" RUST_BACKTRACE=full \
       lmforge init 2>&1 | tee "$HOME/.lmforge/logs/init.log"; then
        info "init completed — verifying torch wheel..."
        TORCH_INFO=$("$HOME/.lmforge/engines/sglang/venv/bin/python" -c \
            "import torch; print(torch.__version__, torch.version.cuda, torch.cuda.is_available())" 2>/dev/null \
            || echo "FAIL")
        if [[ "$TORCH_INFO" == "FAIL" ]]; then
            warn "could not import torch from venv — check init.log"
        else
            info "torch: $TORCH_INFO  (expect: 2.x cu13x True)"
        fi
    else
        error "lmforge init failed — see ~/.lmforge/logs/init.log" 3
    fi
else
    info "[5/6] skipped (--no-init)"
fi

# ── 6. Start daemon (foreground) ─────────────────────────────────────────────
if (( DO_START )); then
    section "[6/6] Starting daemon (foreground; Ctrl-C to stop)"
    if [[ -n "$PULL_MODEL" ]]; then
        # Background daemon, wait for ready, pull model, then surface logs.
        RUST_LOG="lmforge=debug,tower_http=debug,sglang=info" RUST_BACKTRACE=full \
            lmforge start > "$HOME/.lmforge/logs/dev.log" 2>&1 &
        DAEMON_PID=$!
        info "daemon PID=$DAEMON_PID — waiting for /health..."
        for i in {1..30}; do
            curl -sf --max-time 1 http://127.0.0.1:11430/health >/dev/null && break
            sleep 1
        done
        curl -sf http://127.0.0.1:11430/health >/dev/null \
            || { warn "/health never came up"; tail -20 "$HOME/.lmforge/logs/dev.log"; exit 3; }
        info "daemon ready — pulling $PULL_MODEL"
        lmforge pull "$PULL_MODEL"
        info "tail -f ~/.lmforge/logs/dev.log  to follow output"
        info "kill $DAEMON_PID  or  lmforge stop  to terminate"
    else
        echo "  → tail ~/.lmforge/logs/dev.log in another terminal to follow output"
        echo "  → in another terminal: curl -s http://127.0.0.1:11430/lf/status | jq"
        echo ""
        exec env RUST_LOG="lmforge=debug,tower_http=debug,sglang=info" RUST_BACKTRACE=full \
            lmforge start 2>&1 | tee "$HOME/.lmforge/logs/dev.log"
    fi
else
    info "[6/6] skipped (--no-start). Run yourself:"
    echo "    RUST_LOG=lmforge=debug RUST_BACKTRACE=full lmforge start"
fi
