#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Holistic dev loop: clean state → build lmforge from this repo → init →
# llama.cpp variant (cuda12 default) → optional daemon start + smoke pull.
#
# Run from the repo root:
#   scripts/util/dev-reinstall-core.sh
#   scripts/util/dev-reinstall-core.sh --yes
#   scripts/util/dev-reinstall-core.sh --cuda13 --pull qwen3:1.7b:4bit
#
# Interactive by default (sensible defaults). Pass --yes to accept defaults,
# or use flags to skip individual prompts.
#
# Flags:
#   -y, --yes              Non-interactive; accept defaults / explicit flags
#   --variant ID           llamacpp variant: cuda12 | cuda13 | vulkan | cpu
#   --cuda12 | --cuda13    Shorthand for --variant
#   --keep-engines         Do not wipe ~/.lmforge/engines (faster Rust-only loop)
#   --wipe-models          Remove ~/.lmforge/models + models.json
#   --no-cargo-clean       Incremental cargo build
#   --no-init              Skip lmforge init
#   --no-engine            Skip variant install (init normally handles this)
#   --no-start             Do not start the daemon
#   --background           Start daemon in background (implies --no-start exec)
#   --release              cargo build --release
#   --pull MODEL           Pull catalog model after daemon is up
#   --smoke                Alias for --pull qwen3:1.7b:4bit
#   --data-dir PATH        LMFORGE_DATA_DIR (default: ~/.lmforge)
#   -h, --help             Show help
#
# Exit codes: 0 ok | 1 preflight | 2 build | 3 init/engine | 4 start/health
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── defaults (cuda12-first MVP) ───────────────────────────────────────────────
NONINTERACTIVE=0
VARIANT="cuda12"
KEEP_ENGINES=0
WIPE_MODELS=0
CARGO_CLEAN=1
DO_INIT=1
DO_ENGINE=1
DO_START=1
START_BG=0
PROFILE="debug"
PULL_MODEL=""
DATA_DIR="${LMFORGE_DATA_DIR:-$HOME/.lmforge}"
RUST_LOG="${RUST_LOG:-lmforge=debug,tower_http=debug}"

# ── flag parsing ─────────────────────────────────────────────────────────────
while (($#)); do
    case "$1" in
        -y|--yes)              NONINTERACTIVE=1 ;;
        --variant)             VARIANT="${2:?--variant requires cuda12|cuda13|vulkan|cpu}"; NONINTERACTIVE=1; shift ;;
        --cuda12)              VARIANT="cuda12"; NONINTERACTIVE=1 ;;
        --cuda13)              VARIANT="cuda13"; NONINTERACTIVE=1 ;;
        --keep-engines)        KEEP_ENGINES=1; NONINTERACTIVE=1 ;;
        --wipe-models)         WIPE_MODELS=1; NONINTERACTIVE=1 ;;
        --no-cargo-clean)      CARGO_CLEAN=0; NONINTERACTIVE=1 ;;
        --no-init)             DO_INIT=0; NONINTERACTIVE=1 ;;
        --no-engine)           DO_ENGINE=0; NONINTERACTIVE=1 ;;
        --no-start)            DO_START=0; NONINTERACTIVE=1 ;;
        --background)          START_BG=1; NONINTERACTIVE=1 ;;
        --release)             PROFILE="release"; NONINTERACTIVE=1 ;;
        --pull)                PULL_MODEL="${2:?--pull requires a catalog shortcut}"; NONINTERACTIVE=1; shift ;;
        --smoke)               PULL_MODEL="qwen3:1.7b:4bit"; NONINTERACTIVE=1 ;;
        --data-dir)            DATA_DIR="${2:?--data-dir requires a path}"; NONINTERACTIVE=1; shift ;;
        -h|--help)
            sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown flag: $1 (try --help)" >&2; exit 1 ;;
    esac
    shift
done

# ── UI helpers ───────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit "${2:-1}"; }
section() { echo -e "\n${BOLD}$*${NC}"; }
banner()  {
    echo ""
    echo -e "${BOLD}  LMForge — dev core reinstall${NC}"
    echo "  repo: $REPO_ROOT"
    echo ""
}

is_tty() { [[ -t 0 ]]; }

# $1=variable name  $2=default y|n  $3=prompt text
prompt_yn() {
    local __var="$1" __def="$2" __q="$3" __hint=""
    if [[ "$__def" == "y" ]]; then __hint="[Y/n]"; else __hint="[y/N]"; fi
    if (( NONINTERACTIVE )) || ! is_tty; then
        printf -v "$__var" '%s' "$__def"
        return
    fi
    local __reply
    read -r -p "  $__q $__hint " __reply </dev/tty || true
    __reply="${__reply:-$__def}"
    case "${__reply,,}" in
        y|yes)  printf -v "$__var" '%s' "y" ;;
        n|no)   printf -v "$__var" '%s' "n" ;;
        *)      printf -v "$__var" '%s' "$__def" ;;
    esac
}

# $1=variable name  $2=default value  $3=prompt  $4+=choices
prompt_choice() {
    local __var="$1" __def="$2" __q="$3"
    shift 3
    local -a __choices=("$@")
    if (( NONINTERACTIVE )) || ! is_tty; then
        printf -v "$__var" '%s' "$__def"
        return
    fi
    echo "  $__q"
    local i=1 c
    for c in "${__choices[@]}"; do
        local mark=""
        [[ "$c" == "$__def" ]] && mark=" (default)"
        echo "    $i) $c$mark"
        ((i++)) || true
    done
    local __pick __reply
    read -r -p "  Choice [${__def}]: " __reply </dev/tty || true
    if [[ -z "$__reply" ]]; then
        printf -v "$__var" '%s' "$__def"
        return
    fi
    if [[ "$__reply" =~ ^[0-9]+$ ]] && (( __reply >= 1 && __reply <= ${#__choices[@]} )); then
        printf -v "$__var" '%s' "${__choices[$(( __reply - 1 ))]}"
        return
    fi
    for c in "${__choices[@]}"; do
        if [[ "${__reply,,}" == "${c,,}" ]]; then
            printf -v "$__var" '%s' "$c"
            return
        fi
    done
    warn "unrecognized input — using default ($__def)"
    printf -v "$__var" '%s' "$__def"
}

prompt_string() {
    local __var="$1" __def="$2" __q="$3"
    if (( NONINTERACTIVE )) || ! is_tty; then
        printf -v "$__var" '%s' "$__def"
        return
    fi
    local __reply
    read -r -p "  $__q [${__def}]: " __reply </dev/tty || true
    printf -v "$__var" '%s' "${__reply:-$__def}"
}

detect_cuda_hint() {
    if ! command -v nvidia-smi &>/dev/null; then
        echo "no NVIDIA GPU detected — consider variant cpu or vulkan"
        return
    fi
    local drv
    drv=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 || echo "?")
    echo "driver $drv — cuda12 is the default; cuda13 needs r590+ and Blackwell-class GPUs"
}

# ── interactive configuration ────────────────────────────────────────────────
banner

if ! (( NONINTERACTIVE )) && is_tty; then
    section "Configure (Enter = default)"
    echo -e "  ${CYAN}$(detect_cuda_hint)${NC}"

    prompt_choice VARIANT "$VARIANT" "llama.cpp CUDA variant" \
        cuda12 cuda13 vulkan cpu

    yn=""
    prompt_yn yn y "Wipe ~/.lmforge/engines and bin? (re-download llama.cpp variant)"
    [[ "$yn" == "y" ]] && KEEP_ENGINES=0 || KEEP_ENGINES=1

    prompt_yn yn n "Wipe downloaded models (~/.lmforge/models)?"
    [[ "$yn" == "y" ]] && WIPE_MODELS=1 || WIPE_MODELS=0

    prompt_yn yn y "Run cargo clean before build?"
    [[ "$yn" == "y" ]] && CARGO_CLEAN=1 || CARGO_CLEAN=0

    prompt_choice PROFILE "$PROFILE" "Cargo profile" debug release

    prompt_yn yn y "Run lmforge init (hardware probe + engine install)?"
    [[ "$yn" == "y" ]] && DO_INIT=1 || DO_INIT=0

    if (( DO_INIT )); then
        DO_ENGINE=1
        info "engine install runs inside init (LMFORGE_LLAMACPP_VARIANT=$VARIANT)"
    else
        prompt_yn yn y "Install llamacpp variant $VARIANT anyway?"
        [[ "$yn" == "y" ]] && DO_ENGINE=1 || DO_ENGINE=0
    fi

    prompt_yn yn y "Start daemon when done?"
    [[ "$yn" == "y" ]] && DO_START=1 || DO_START=0

    if (( DO_START )); then
        prompt_choice _start_mode "$([[ $START_BG -eq 1 ]] && echo background || echo foreground)" \
            "Daemon start mode" foreground background
        [[ "$_start_mode" == "background" ]] && START_BG=1 || START_BG=0
    fi

    if [[ -z "$PULL_MODEL" ]]; then
        prompt_choice _pull "none" "Smoke-pull a model after start?" \
            none qwen3:1.7b:4bit qwen3-embed:0.6b:8bit custom
        case "$_pull" in
            none)        PULL_MODEL="" ;;
            custom)
                prompt_string PULL_MODEL "" "Catalog shortcut to pull"
                ;;
            *)           PULL_MODEL="$_pull" ;;
        esac
    fi

    prompt_string DATA_DIR "$DATA_DIR" "LMForge data directory"
    prompt_string RUST_LOG "$RUST_LOG" "RUST_LOG"

    echo ""
    info "plan: variant=$VARIANT profile=$PROFILE data=$DATA_DIR"
    info "      wipe_engines=$(( ! KEEP_ENGINES )) wipe_models=$WIPE_MODELS cargo_clean=$CARGO_CLEAN"
    info "      init=$DO_INIT start=$DO_START bg=$START_BG pull=${PULL_MODEL:-<none>}"
    echo ""
    prompt_yn yn y "Proceed?"
    [[ "$yn" == "y" ]] || { echo "Aborted."; exit 0; }
fi

export LMFORGE_DATA_DIR="$DATA_DIR"
export LMFORGE_LLAMACPP_VARIANT="$VARIANT"
export RUST_LOG RUST_BACKTRACE="${RUST_BACKTRACE:-full}"

mkdir -p "$DATA_DIR/logs"

# ── [0] Preflight ────────────────────────────────────────────────────────────
section "[0/7] Preflight"
[[ "$(uname -s)" == "Linux" ]] || error "Linux-only script (use repo build on macOS/Windows separately)" 1
command -v cargo >/dev/null || error "cargo not on PATH" 1

HAS_NVIDIA=0
if command -v nvidia-smi >/dev/null; then
    HAS_NVIDIA=1
    DRV=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1)
    CUDA_RT=$(nvidia-smi 2>/dev/null | awk '/CUDA Version/ {print $9; exit}')
    info "NVIDIA driver=$DRV  cuda-runtime=${CUDA_RT:-?}"
else
    warn "nvidia-smi not found"
fi

case "$VARIANT" in
    cuda12|cuda13)
        (( HAS_NVIDIA )) || error "variant $VARIANT requires an NVIDIA GPU" 1
        ;;
    vulkan)
        warn "vulkan variant — ensure Mesa/Vulkan drivers are installed"
        ;;
    cpu)
        info "cpu variant — no GPU required"
        ;;
    *)
        error "unknown variant: $VARIANT (cuda12|cuda13|vulkan|cpu)" 1
        ;;
esac

if [[ "$VARIANT" == "cuda13" ]]; then
    warn "cuda13 targets newer drivers (r590+) — use cuda12 if install fails"
fi

info "data_dir=$DATA_DIR  variant=$VARIANT  profile=$PROFILE"

# ── [1] Stop daemons ─────────────────────────────────────────────────────────
section "[1/7] Stopping daemons"
if command -v lmforge &>/dev/null; then
    lmforge service stop 2>/dev/null || true
    lmforge stop 2>/dev/null || true
fi
pkill -x lmforge 2>/dev/null || true
rm -f "$DATA_DIR/lmforge.pid"
info "stopped"

# ── [2] Wipe / seed state ────────────────────────────────────────────────────
section "[2/7] Preparing $DATA_DIR"
if (( KEEP_ENGINES )); then
    warn "keeping engines/ — variant may be stale; use wipe for a clean engine install"
else
    rm -rf "$DATA_DIR/engines" "$DATA_DIR/bin"
    info "removed engines/ and bin/"
fi

if (( WIPE_MODELS )); then
    rm -rf "$DATA_DIR/models" "$DATA_DIR/models.json"
    info "removed models/ and models.json"
fi

mkdir -p "$DATA_DIR/catalogs" "$DATA_DIR/logs"
for f in mlx.json safetensors.json gguf.json exl3.json; do
    [[ -f "$REPO_ROOT/data/catalogs/$f" ]] && cp "$REPO_ROOT/data/catalogs/$f" "$DATA_DIR/catalogs/$f"
done
info "catalogs re-seeded from repo"

: > "$DATA_DIR/logs/daemon.out.log" 2>/dev/null || true
: > "$DATA_DIR/logs/daemon.err.log" 2>/dev/null || true

# ── [3] Build from repo ──────────────────────────────────────────────────────
section "[3/7] cargo build ($PROFILE)"
cd "$REPO_ROOT"
if (( CARGO_CLEAN )); then
    cargo clean 2>&1 | tail -1 || true
    info "cargo clean done"
fi
BUILD_ARGS=(build --bin lmforge)
[[ "$PROFILE" == "release" ]] && BUILD_ARGS+=(--release)
cargo "${BUILD_ARGS[@]}" 2>&1 | tail -5 || error "cargo build failed" 2

# ── [4] Symlink onto PATH ────────────────────────────────────────────────────
section "[4/7] Symlink lmforge → target/$PROFILE/lmforge"
mkdir -p "$HOME/.cargo/bin" "$HOME/.local/bin"
BIN="$REPO_ROOT/target/$PROFILE/lmforge"
rm -f "$HOME/.cargo/bin/lmforge" "$HOME/.local/bin/lmforge"
ln -sf "$BIN" "$HOME/.cargo/bin/lmforge"
ln -sf "$BIN" "$HOME/.local/bin/lmforge"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
"$BIN" --version || error "built binary not runnable" 2
info "lmforge on PATH ($(lmforge --version 2>/dev/null | head -1))"

# ── [5] Init + engine variant ────────────────────────────────────────────────
if (( DO_INIT )); then
    section "[5/7] lmforge init (LMFORGE_LLAMACPP_VARIANT=$VARIANT)"
    if lmforge init 2>&1 | tee "$DATA_DIR/logs/init.log"; then
        info "init ok — see $DATA_DIR/logs/init.log"
    else
        error "lmforge init failed" 3
    fi
elif (( DO_ENGINE )); then
    section "[5/7] lmforge engine install llamacpp --variant $VARIANT"
    lmforge engine install llamacpp --variant "$VARIANT" 2>&1 | tee "$DATA_DIR/logs/engine-install.log" \
        || error "engine install failed" 3
else
    info "[5/7] skipped init and engine install"
fi

# Verify variant binary exists when we expect GPU path
if [[ "$VARIANT" =~ ^cuda ]]; then
    VBIN="$DATA_DIR/engines/llamacpp/variants/$VARIANT/llama-server"
    if [[ -x "$VBIN" ]]; then
        info "variant binary: $VBIN"
    else
        warn "expected $VBIN — init may have used a different variant; check init.log"
    fi
fi

# ── [6] Start daemon ─────────────────────────────────────────────────────────
if ! (( DO_START )); then
    section "[6/7] skipped start"
    echo ""
    echo "  Next:"
    echo "    export LMFORGE_LLAMACPP_VARIANT=$VARIANT"
    echo "    export LMFORGE_DATA_DIR=$DATA_DIR"
    echo "    RUST_LOG=$RUST_LOG lmforge start"
    echo "    scripts/util/release_binary_test.sh   # full matrix (optional)"
    exit 0
fi

section "[6/7] Starting daemon"
LOG="$DATA_DIR/logs/dev.log"

if (( START_BG )) || [[ -n "$PULL_MODEL" ]]; then
    lmforge start >"$LOG" 2>&1 &
    DPID=$!
    info "daemon PID=$DPID (logs: $LOG)"
    for _ in $(seq 1 60); do
        curl -sf --max-time 1 http://127.0.0.1:11430/health >/dev/null && break
        sleep 1
    done
    curl -sf http://127.0.0.1:11430/health >/dev/null \
        || { warn "health check failed"; tail -30 "$LOG"; exit 4; }
    info "daemon ready — curl -s http://127.0.0.1:11430/lf/status | jq"

    if [[ -n "$PULL_MODEL" ]]; then
        section "[7/7] Pull $PULL_MODEL"
        lmforge pull "$PULL_MODEL" || warn "pull failed (model may need manual retry)"
    else
        section "[7/7] done"
    fi
    echo ""
    echo "  tail -f $LOG"
    echo "  lmforge stop  # or kill $DPID"
    exit 0
fi

section "[7/7] Foreground daemon (Ctrl-C to stop)"
echo "  → another terminal: tail -f $LOG"
echo "  → status: curl -s http://127.0.0.1:11430/lf/status | jq"
echo ""
exec lmforge start 2>&1 | tee "$LOG"
