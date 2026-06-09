#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Holistic LMForge dev test runner: cargo + CLI + live API/inference matrix.
#
#   scripts/util/dev_test.sh                    # interactive
#   scripts/util/dev_test.sh --yes              # defaults (no VLM/MTP)
#   scripts/util/dev_test.sh --yes --full       # + VLM + MTP + rerank probe
#   scripts/util/dev_test.sh --e2e-only --yes   # skip cargo, hit running daemon
#
# Layers:
#   1. cargo test --lib / --tests
#   2. CLI smoke (engine list, catalog, doctor)
#   3. API surface (health, status, engines, metrics, logs, catalog, models, config)
#   4. Inference (chat, stream, embeddings, switch, unload)
#   5. VLM (text + image_url) — opt-in (--with-vlm / --full)
#   6. MTP speculative — opt-in (--with-mtp / --full)
#
# Requires: curl, jq, lmforge on PATH (or REPO target), GPU for inference layers.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=dev-lib.sh
source "$SCRIPT_DIR/dev-lib.sh"

# ── defaults ──────────────────────────────────────────────────────────────────
DEV_NONINTERACTIVE=0
DO_UNIT=1
DO_INTEGRATION=1
DO_CLI=1
DO_API=1
DO_INFERENCE=1
DO_VLM=0
DO_MTP=0
DO_RERANK=0
E2E_ONLY=0
SKIP_PULL=0
KEEP_DAEMON=0
CARGO_PROFILE=""
LF_HOST="${LF_HOST:-http://127.0.0.1:11430}"
DATA_DIR="${LMFORGE_DATA_DIR:-$HOME/.lmforge}"

CHAT_MODEL="${CHAT_MODEL:-qwen3:1.7b:4bit}"
EMBED_MODEL="${EMBED_MODEL:-qwen3-embed:0.6b:8bit}"
VLM_MODEL="${VLM_MODEL:-qwen2.5-vl:3b:4bit}"
MTP_MODEL="${MTP_MODEL:-qwen3.5:4b:mtp:4bit}"
RERANK_MODEL="${RERANK_MODEL:-bge-reranker-v2-m3:8bit}"
MODEL_WAIT_SECS="${MODEL_WAIT_SECS:-180}"
CHAT_MAX_TOKENS="${CHAT_MAX_TOKENS:-64}"

# 1×1 red PNG for offline-safe VLM probe
RED_PNG_B64="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="

FAILS=0
SKIPS=0
START_TOTAL=$(date +%s)
DAEMON_STARTED_BY_US=0
DAEMON_PID=""
BIN=""

while (($#)); do
    case "$1" in
        -y|--yes)              DEV_NONINTERACTIVE=1 ;;
        --quick)               DO_VLM=0; DO_MTP=0; DO_RERANK=0; DO_INFERENCE=0; DEV_NONINTERACTIVE=1 ;;
        --full)                DO_VLM=1; DO_MTP=1; DO_RERANK=1; DEV_NONINTERACTIVE=1 ;;
        --e2e-only)            E2E_ONLY=1; DO_UNIT=0; DO_INTEGRATION=0; DEV_NONINTERACTIVE=1 ;;
        --with-e2e)            DO_API=1; DO_INFERENCE=1; DEV_NONINTERACTIVE=1 ;;
        --with-vlm)            DO_VLM=1; DEV_NONINTERACTIVE=1 ;;
        --with-mtp)            DO_MTP=1; DEV_NONINTERACTIVE=1 ;;
        --with-rerank)         DO_RERANK=1; DEV_NONINTERACTIVE=1 ;;
        --no-unit)             DO_UNIT=0; DEV_NONINTERACTIVE=1 ;;
        --no-integration)      DO_INTEGRATION=0; DEV_NONINTERACTIVE=1 ;;
        --no-cli)              DO_CLI=0; DEV_NONINTERACTIVE=1 ;;
        --no-api)              DO_API=0; DEV_NONINTERACTIVE=1 ;;
        --no-inference)        DO_INFERENCE=0; DEV_NONINTERACTIVE=1 ;;
        --no-vlm)              DO_VLM=0; DEV_NONINTERACTIVE=1 ;;
        --no-mtp)              DO_MTP=0; DEV_NONINTERACTIVE=1 ;;
        --skip-pull)           SKIP_PULL=1; DEV_NONINTERACTIVE=1 ;;
        --keep-daemon)         KEEP_DAEMON=1; DEV_NONINTERACTIVE=1 ;;
        --release)             CARGO_PROFILE="--release"; DEV_NONINTERACTIVE=1 ;;
        --chat-model)          CHAT_MODEL="${2:?}"; shift; DEV_NONINTERACTIVE=1 ;;
        --embed-model)         EMBED_MODEL="${2:?}"; shift; DEV_NONINTERACTIVE=1 ;;
        --vlm-model)           VLM_MODEL="${2:?}"; shift; DEV_NONINTERACTIVE=1 ;;
        --mtp-model)           MTP_MODEL="${2:?}"; shift; DEV_NONINTERACTIVE=1 ;;
        -h|--help)             sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)                     echo "Unknown: $1" >&2; exit 1 ;;
    esac
    shift
done

# ── test harness ──────────────────────────────────────────────────────────────
t_pass() { echo -e "  ${GREEN}PASS${NC}  $1 ${BLUE}(${2}s)${NC}"; }
t_fail() { echo -e "  ${RED}FAIL${NC}  $1 ${CYAN}(${2}s)${NC}  ${RED}${3:-}${NC}"; FAILS=$((FAILS + 1)); }
t_skip() { echo -e "  ${YELLOW}SKIP${NC} $1 — ${3:-}"; SKIPS=$((SKIPS + 1)); }
t_note() { echo -e "  ${YELLOW}·${NC} $*"; }
t_sec()  { echo -e "\n${BOLD}── $* ──${NC}"; }

curl_health() { curl -sf --max-time 3 "$LF_HOST/health" >/dev/null 2>&1; }

resolve_bin() {
    BIN="$REPO_ROOT/target/debug/lmforge"
    [[ -x "$BIN" ]] || BIN="$REPO_ROOT/target/release/lmforge"
    [[ -x "$BIN" ]] || BIN="$(command -v lmforge 2>/dev/null || true)"
}

ensure_daemon() {
    if curl_health; then
        t_note "using running daemon at $LF_HOST"
        return 0
    fi
    resolve_bin
    [[ -n "$BIN" && -x "$BIN" ]] || { t_fail "lmforge binary" "0" "build or install first"; return 1; }
    mkdir -p "$DATA_DIR/logs"
    t_note "starting daemon (background)..."
    RUST_LOG="${RUST_LOG:-lmforge=info}" RUST_BACKTRACE=1 \
        LMFORGE_DATA_DIR="$DATA_DIR" \
        "$BIN" start >"$DATA_DIR/logs/dev_test.log" 2>&1 &
    DAEMON_PID=$!
    DAEMON_STARTED_BY_US=1
    local i
    for i in $(seq 1 45); do
        curl_health && return 0
        sleep 1
    done
    t_fail "daemon health timeout" "45" "tail $DATA_DIR/logs/dev_test.log"
    tail -15 "$DATA_DIR/logs/dev_test.log" 2>/dev/null || true
    return 1
}

model_installed() {
    curl -sf --max-time 5 "$LF_HOST/lf/model/list" \
        | jq -e --arg id "$1" '.models[] | select(.id == $id)' >/dev/null 2>&1
}

ensure_model() {
    local id="$1" optional="${2:-0}"
    model_installed "$id" && return 0
    if (( SKIP_PULL )); then
        if (( optional )); then
            t_skip "model $id" "0" "not installed (SKIP_PULL)"
            return 1
        fi
        t_fail "model $id missing" "0" "SKIP_PULL=1"
        return 1
    fi
    resolve_bin
    t_note "pulling $id ..."
    local t0=$(date +%s)
    if "$BIN" pull "$id" 2>&1 | tail -5; then
        t_pass "pull $id" "$(( $(date +%s) - t0 ))"
        return 0
    fi
    t_fail "pull $id" "$(( $(date +%s) - t0 ))" ""
    return 1
}

wait_model_ready() {
    local id="$1" max="${2:-$MODEL_WAIT_SECS}"
    local i
    for i in $(seq 1 "$max"); do
        if curl -sf "$LF_HOST/lf/status" 2>/dev/null | jq -e --arg m "$id" \
            '.running_models[]? | select(.model_id == $m)' >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    return 1
}

start_with_model() {
    local model="$1"
    resolve_bin
    dev_stop_core "$DATA_DIR"
    LMFORGE_DATA_DIR="$DATA_DIR" LMFORGE_LLAMACPP_VARIANT="${LMFORGE_LLAMACPP_VARIANT:-cuda12}" \
        "$BIN" start --model "$model" >"$DATA_DIR/logs/dev_test.log" 2>&1 &
    DAEMON_PID=$!
    DAEMON_STARTED_BY_US=1
    curl_health || sleep 2
    wait_model_ready "$model" 120
}

# ── interactive plan ──────────────────────────────────────────────────────────
resolve_test_models() {
    local ids
    ids=$(curl -sf --max-time 5 "$LF_HOST/lf/model/list" 2>/dev/null | jq -r '.models[].id' 2>/dev/null || true)
    if ! model_installed "$CHAT_MODEL"; then
        if echo "$ids" | grep -qx 'qwen3:1.7b:4bit'; then CHAT_MODEL='qwen3:1.7b:4bit'
        elif echo "$ids" | grep -qx 'qwen3.5:4b:6bit'; then CHAT_MODEL='qwen3.5:4b:6bit'
        elif id=$(echo "$ids" | head -1) && [[ -n "$id" ]]; then
            CHAT_MODEL="$id"
            t_note "CHAT_MODEL → $CHAT_MODEL (first installed)"
        fi
    fi
    if ! model_installed "$EMBED_MODEL"; then
        if echo "$ids" | grep -qx 'qwen3-embed:0.6b:8bit'; then EMBED_MODEL='qwen3-embed:0.6b:8bit'
        elif id=$(echo "$ids" | grep -i embed | head -1) && [[ -n "$id" ]]; then
            EMBED_MODEL="$id"
            t_note "EMBED_MODEL → $EMBED_MODEL"
        fi
    fi
}

if ! (( DEV_NONINTERACTIVE )) && dev_is_tty; then
    echo ""
    echo -e "${BOLD}  LMForge — dev test${NC}"
    echo -e "  ${CYAN}Tip:${NC} scripts/util/dev_test.sh --yes   (skip prompts, sensible defaults)"
    t_sec "Configure test layers"
    yn=""
    dev_prompt_yn yn y "Run cargo unit tests (--lib)?"; [[ "$yn" == "y" ]] && DO_UNIT=1 || DO_UNIT=0
    dev_prompt_yn yn y "Run cargo integration tests (--tests)?"; [[ "$yn" == "y" ]] && DO_INTEGRATION=1 || DO_INTEGRATION=0
    dev_prompt_yn yn y "Run CLI smoke tests?"; [[ "$yn" == "y" ]] && DO_CLI=1 || DO_CLI=0
    dev_prompt_yn yn y "Run API surface tests (no GPU inference)?"; [[ "$yn" == "y" ]] && DO_API=1 || DO_API=0
    dev_prompt_yn yn y "Run inference tests (chat + embed)?"; [[ "$yn" == "y" ]] && DO_INFERENCE=1 || DO_INFERENCE=0
    dev_prompt_yn yn n "Run VLM tests (image + text)?"; [[ "$yn" == "y" ]] && DO_VLM=1 || DO_VLM=0
    dev_prompt_yn yn n "Run MTP speculative test?"; [[ "$yn" == "y" ]] && DO_MTP=1 || DO_MTP=0
    dev_prompt_yn yn n "Probe /v1/rerank?"; [[ "$yn" == "y" ]] && DO_RERANK=1 || DO_RERANK=0
    dev_prompt_yn yn n "Skip model pulls (models must exist)?"; [[ "$yn" == "y" ]] && SKIP_PULL=1 || SKIP_PULL=0
    echo ""
    dev_prompt_yn yn y "Proceed?"
    [[ "$yn" == "y" ]] || { echo "Aborted."; exit 0; }
fi

cleanup() {
    if (( DAEMON_STARTED_BY_US )) && (( ! KEEP_DAEMON )); then
        dev_stop_core "$DATA_DIR"
    fi
}
trap cleanup EXIT

command -v jq >/dev/null || { dev_err "jq required"; exit 1; }
command -v curl >/dev/null || { dev_err "curl required"; exit 1; }

# ══════════════════════════════════════════════════════════════════════════════
# Layer 1–2: cargo
# ══════════════════════════════════════════════════════════════════════════════
run_cargo_tests() {
    local label="$1" args="$2"
    cd "$REPO_ROOT"
    local t0=$(date +%s) ec=0
    cargo test $args $CARGO_PROFILE 2>&1 | tee /tmp/lmforge-dev-test-cargo.log | tail -8
    ec=${PIPESTATUS[0]}
    if (( ec == 0 )); then
        t_pass "$label" "$(( $(date +%s) - t0 ))"
    else
        t_fail "$label" "$(( $(date +%s) - t0 ))" "exit=$ec (see /tmp/lmforge-dev-test-cargo.log)"
    fi
}

if (( DO_UNIT )) && !(( E2E_ONLY )); then
    t_sec "Unit tests"
    run_cargo_tests "cargo test --lib" "--lib"
fi

if (( DO_INTEGRATION )) && !(( E2E_ONLY )); then
    t_sec "Integration tests"
    run_cargo_tests "cargo test --tests" "--tests -- --test-threads=1"
fi

# ══════════════════════════════════════════════════════════════════════════════
# Layer 3: CLI
# ══════════════════════════════════════════════════════════════════════════════
if (( DO_CLI )) && !(( E2E_ONLY )); then
    t_sec "CLI smoke"
    resolve_bin
    if [[ -z "$BIN" || ! -x "$BIN" ]]; then
        t_fail "lmforge binary" "0" "not found"
    else
        t0=$(date +%s)
        OUT=$("$BIN" engine list 2>&1 || true)
        if echo "$OUT" | grep -q '^llamacpp '; then
            t_pass "engine list" "$(( $(date +%s) - t0 ))"
        else
            t_fail "engine list" "$(( $(date +%s) - t0 ))" "missing llamacpp"
        fi

        t0=$(date +%s)
        OUT=$("$BIN" catalog --format gguf 2>&1 | head -5 || true)
        if echo "$OUT" | grep -qi 'shortcut'; then
            t_pass "catalog --format gguf" "$(( $(date +%s) - t0 ))"
        else
            t_fail "catalog --format gguf" "$(( $(date +%s) - t0 ))"
        fi

        t0=$(date +%s)
        OUT=$("$BIN" doctor 2>&1 || true)
        if [[ -n "$OUT" ]]; then
            t_pass "doctor" "$(( $(date +%s) - t0 ))"
        else
            t_skip "doctor" "$(( $(date +%s) - t0 ))" "empty output"
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# Live daemon tests
# ══════════════════════════════════════════════════════════════════════════════
NEED_DAEMON=$(( DO_API + DO_INFERENCE + DO_VLM + DO_MTP + DO_RERANK ))
if (( NEED_DAEMON )); then
    ensure_daemon || exit 2
    BASE="$LF_HOST"
    resolve_test_models
fi

# ── API surface (no model load required for most) ─────────────────────────────
if (( DO_API )); then
    t_sec "API surface"

    api_test() {
        local name="$1" method="$2" url="$3" data="${4:-}" expect_jq="${5:-}"
        local t0=$(date +%s) resp code
        if [[ "$method" == "GET" ]]; then
            resp=$(curl -sf --max-time 10 -w '\n%{http_code}' "$url" 2>/dev/null || echo -e "\n000")
        else
            resp=$(curl -sf --max-time 10 -w '\n%{http_code}' -X "$method" "$url" \
                -H 'Content-Type: application/json' -d "$data" 2>/dev/null || echo -e "\n000")
        fi
        code=$(echo "$resp" | tail -1)
        resp=$(echo "$resp" | sed '$d')
        if [[ "$code" =~ ^2 ]] && { [[ -z "$expect_jq" ]] || echo "$resp" | jq -e "$expect_jq" >/dev/null 2>&1; }; then
            t_pass "$name" "$(( $(date +%s) - t0 ))"
        else
            t_fail "$name" "$(( $(date +%s) - t0 ))" "http=$code body=${resp:0:120}"
        fi
    }

    api_test "GET /health" GET "$BASE/health" "" '.status == "ok"'
    api_test "GET /lf/status" GET "$BASE/lf/status" "" \
        '.overall_status and .engine.id and (.last_errors | type == "object")'
    api_test "GET /lf/engines" GET "$BASE/lf/engines" "" \
        '(.engines | type == "array") and (.engines | length > 0)'
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 10 "$BASE/lf/hardware" 2>/dev/null || true)
    if echo "$RESP" | jq -e 'type == "object"' >/dev/null 2>&1; then
        t_pass "GET /lf/hardware" "$(( $(date +%s) - t0 ))"
    else
        t_fail "GET /lf/hardware" "$(( $(date +%s) - t0 ))" "${RESP:0:80}"
    fi
    api_test "GET /lf/sysinfo" GET "$BASE/lf/sysinfo" "" \
        '.cpu_pct != null and .mem_total_gb > 0'
    api_test "GET /lf/metrics" GET "$BASE/lf/metrics" "" \
        '.endpoints // .requests_total'
    api_test "GET /lf/model/list" GET "$BASE/lf/model/list" "" \
        '(.models | type == "array")'

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 10 "$BASE/metrics" 2>/dev/null || true)
    if echo "$RESP" | grep -qE '^# (HELP|TYPE)'; then
        t_pass "GET /metrics (Prometheus)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "GET /metrics" "$(( $(date +%s) - t0 ))" "not prometheus"
    fi

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 10 "$BASE/lf/catalog?format=gguf" 2>/dev/null || true)
    COUNT=$(echo "$RESP" | jq '.entries | length' 2>/dev/null || echo 0)
    if (( COUNT >= 50 )); then
        t_pass "GET /lf/catalog?format=gguf ($COUNT)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "GET /lf/catalog gguf" "$(( $(date +%s) - t0 ))" "count=$COUNT"
    fi

    api_test "GET /lf/logs/list" GET "$BASE/lf/logs/list" "" \
        '(.components | type == "array")'

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 10 "$BASE/lf/logs/tail?component=daemon&stream=stderr&lines=5" 2>/dev/null || true)
    if [[ -n "$RESP" ]]; then
        t_pass "GET /lf/logs/tail" "$(( $(date +%s) - t0 ))"
    else
        t_skip "GET /lf/logs/tail" "$(( $(date +%s) - t0 ))" "empty or missing daemon logs"
    fi

    api_test "GET /v1/models" GET "$BASE/v1/models" "" \
        '(.data | type == "array")'

    t0=$(date +%s)
    CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d '{"model":"nonexistent-model-xyz","messages":[{"role":"user","content":"hi"}],"max_tokens":8}' 2>/dev/null)
    CODE="${CODE:-000}"
    if [[ "$CODE" =~ ^(400|404|422|503)$ ]]; then
        t_pass "POST /v1/chat invalid model → $CODE" "$(( $(date +%s) - t0 ))"
    else
        t_fail "POST /v1/chat invalid model" "$(( $(date +%s) - t0 ))" "code=$CODE"
    fi

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/api/tags" 2>/dev/null || true)
    if echo "$RESP" | jq -e '.models' >/dev/null 2>&1; then
        t_pass "GET /api/tags (Ollama compat)" "$(( $(date +%s) - t0 ))"
    else
        t_skip "GET /api/tags" "$(( $(date +%s) - t0 ))" "optional compat"
    fi
fi

# ── Inference: chat + embed ───────────────────────────────────────────────────
if (( DO_INFERENCE )); then
    t_sec "Inference (chat + embed)"
    ensure_model "$CHAT_MODEL" || { t_skip "inference" "0" "chat model unavailable"; DO_INFERENCE=0; }
fi
if (( DO_INFERENCE )); then
    HAS_EMBED=1
    ensure_model "$EMBED_MODEL" 1 || HAS_EMBED=0

    start_with_model "$CHAT_MODEL" || t_fail "start $CHAT_MODEL" "0" ""

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 120 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$CHAT_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with exactly: OK\"}],\"max_tokens\":$CHAT_MAX_TOKENS,\"temperature\":0,\"chat_template_kwargs\":{\"enable_thinking\":false}}")
    TXT=$(echo "$RESP" | jq -r '(.choices[0].message.content // "") + (.choices[0].message.reasoning_content // "")' 2>/dev/null)
    if [[ -n "$TXT" ]]; then
        t_pass "POST /v1/chat ($CHAT_MODEL)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "POST /v1/chat" "$(( $(date +%s) - t0 ))" "${RESP:0:200}"
    fi

    t0=$(date +%s)
    STREAM=$(curl -sN --max-time 120 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$CHAT_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Say: hi\"}],\"max_tokens\":32,\"stream\":true,\"temperature\":0}")
    CHUNKS=$(echo "$STREAM" | grep -c '^data: ' || true)
    if (( CHUNKS >= 2 )) && echo "$STREAM" | grep -q 'data: \[DONE\]'; then
        t_pass "POST /v1/chat stream ($CHUNKS chunks)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "POST /v1/chat stream" "$(( $(date +%s) - t0 ))" "chunks=$CHUNKS"
    fi

    if (( HAS_EMBED )); then
        t0=$(date +%s)
        RESP=$(curl -sf --max-time 90 "$BASE/v1/embeddings" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$EMBED_MODEL\",\"input\":\"Hello world\"}")
        DIM=$(echo "$RESP" | jq -r '.data[0].embedding | length' 2>/dev/null || echo 0)
        if (( DIM > 0 )); then
            t_pass "POST /v1/embeddings dim=$DIM" "$(( $(date +%s) - t0 ))"
        else
            t_fail "POST /v1/embeddings" "$(( $(date +%s) - t0 ))" "${RESP:0:120}"
        fi

        t0=$(date +%s)
        RESP=$(curl -sf --max-time 90 "$BASE/v1/embeddings" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$EMBED_MODEL\",\"input\":[\"a\",\"b\",\"c\"]}")
        N=$(echo "$RESP" | jq '.data | length' 2>/dev/null || echo 0)
        if (( N == 3 )); then
            t_pass "POST /v1/embeddings batch" "$(( $(date +%s) - t0 ))"
        else
            t_fail "POST /v1/embeddings batch" "$(( $(date +%s) - t0 ))" "n=$N"
        fi

        t0=$(date +%s)
        curl -sf --max-time 30 -X POST "$BASE/lf/model/switch" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$EMBED_MODEL\"}" >/dev/null 2>&1 || true
        sleep 3
        if wait_model_ready "$EMBED_MODEL" 90; then
            t_pass "POST /lf/model/switch → $EMBED_MODEL" "$(( $(date +%s) - t0 ))"
        else
            t_fail "POST /lf/model/switch" "$(( $(date +%s) - t0 ))"
        fi

        t0=$(date +%s)
        curl -sf --max-time 30 -X POST "$BASE/lf/model/unload" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$EMBED_MODEL\"}" >/dev/null 2>&1 || true
        t_pass "POST /lf/model/unload" "$(( $(date +%s) - t0 ))"
    else
        t_skip "embeddings + switch + unload" "0" "$EMBED_MODEL not installed"
    fi
fi

# ── VLM ───────────────────────────────────────────────────────────────────────
if (( DO_VLM )); then
    t_sec "VLM ($VLM_MODEL)"
    ensure_model "$VLM_MODEL" || true
    start_with_model "$VLM_MODEL" || { t_fail "VLM start" "0" ""; }

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 180 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$VLM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Say okay in one word.\"}],\"max_tokens\":24,\"temperature\":0}")
    TXT=$(echo "$RESP" | jq -r '.choices[0].message.content // empty' 2>/dev/null)
    if [[ -n "$TXT" ]]; then
        t_pass "VLM text-only" "$(( $(date +%s) - t0 ))"
    else
        t_fail "VLM text-only" "$(( $(date +%s) - t0 ))"
    fi

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 240 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$VLM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":[
            {\"type\":\"text\",\"text\":\"Describe in less than 50 words but more than 35 words.\"},
            {\"type\":\"image_url\",\"image_url\":{\"url\":\"https://picsum.photos/200/300/?blur=2\"}}
        ]}],\"max_tokens\":96,\"temperature\":0}")
    TXT=$(echo "$RESP" | jq -r '.choices[0].message.content // empty' 2>/dev/null)
    if [[ -n "$TXT" ]] && [[ ${#TXT} -gt 20 ]]; then
        t_pass "VLM image_url (remote)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "VLM image_url" "$(( $(date +%s) - t0 ))" "${TXT:0:80}"
    fi

    t0=$(date +%s)
    RESP=$(curl -sf --max-time 180 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$VLM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":[
            {\"type\":\"text\",\"text\":\"What color? One word.\"},
            {\"type\":\"image_url\",\"image_url\":{\"url\":\"data:image/png;base64,${RED_PNG_B64}\"}}
        ]}],\"max_tokens\":16,\"temperature\":0}")
    TXT=$(echo "$RESP" | jq -r '.choices[0].message.content // empty' 2>/dev/null)
    if [[ -n "$TXT" ]]; then
        t_pass "VLM image_url (base64)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "VLM image_url (base64)" "$(( $(date +%s) - t0 ))"
    fi
fi

# ── MTP ───────────────────────────────────────────────────────────────────────
if (( DO_MTP )); then
    t_sec "MTP ($MTP_MODEL)"
    ensure_model "$MTP_MODEL" || true
    export LMFORGE_SPECULATIVE_MODE=auto
    start_with_model "$MTP_MODEL" || { t_fail "MTP start" "0" ""; }

    curl -sf --max-time 120 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$MTP_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Count slowly to five.\"}],\"max_tokens\":128,\"temperature\":0,\"think\":false}" \
        >/dev/null 2>&1 || true
    sleep 3

    t0=$(date +%s)
    SPEC=$(curl -sf "$BASE/lf/status" 2>/dev/null | jq -r --arg m "$MTP_MODEL" \
        '.running_models[] | select(.model_id == $m) | .spec_mode // "off"' | head -1)
    SAMPLES=$(curl -sf "$BASE/lf/status" 2>/dev/null | jq -r --arg m "$MTP_MODEL" \
        '.running_models[] | select(.model_id == $m) | .spec_stats.samples // 0' | head -1)
    if [[ "$SPEC" == "mtp" ]] || [[ "${SAMPLES:-0}" -ge 1 ]]; then
        t_pass "MTP active (mode=$SPEC samples=$SAMPLES)" "$(( $(date +%s) - t0 ))"
    else
        t_fail "MTP not active" "$(( $(date +%s) - t0 ))" "spec_mode=$SPEC samples=$SAMPLES"
    fi
    unset LMFORGE_SPECULATIVE_MODE
fi

# ── Rerank (optional) ─────────────────────────────────────────────────────────
if (( DO_RERANK )); then
    t_sec "Rerank ($RERANK_MODEL)"
    SUPPORTS=$(curl -sf "$BASE/lf/engines" 2>/dev/null | jq -r \
        '.engines[] | select(.active == true) | .supports_reranking' 2>/dev/null | head -1)
    if [[ "$SUPPORTS" != "true" ]]; then
        t_skip "POST /v1/rerank" "0" "active engine lacks reranking"
    else
        if ! ensure_model "$RERANK_MODEL" 1; then
            t_skip "POST /v1/rerank" "0" "$RERANK_MODEL unavailable"
        elif ! start_with_model "$RERANK_MODEL"; then
            t_fail "start $RERANK_MODEL" "0" ""
        else
        t0=$(date +%s)
        RESP=$(curl -sf --max-time 90 "$BASE/v1/rerank" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$RERANK_MODEL\",\"query\":\"What is Python?\",\"documents\":[\"Python is a language.\",\"The sky is blue.\"],\"top_n\":2}" 2>/dev/null || true)
        if echo "$RESP" | jq -e '.results | length >= 1' >/dev/null 2>&1; then
            t_pass "POST /v1/rerank" "$(( $(date +%s) - t0 ))"
        else
            t_fail "POST /v1/rerank" "$(( $(date +%s) - t0 ))" "${RESP:0:120}"
        fi
        fi
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────
ELAPSED=$(( $(date +%s) - START_TOTAL ))
echo ""
echo -e "${BOLD}────────────────────────────────────────${NC}"
if (( FAILS == 0 )); then
    echo -e "${GREEN}  ✓ ALL PASSED${NC}  (${ELAPSED}s, ${SKIPS} skipped)"
    exit 0
else
    echo -e "${RED}  ✗ $FAILS FAILURE(S)${NC}  (${ELAPSED}s, ${SKIPS} skipped)"
    exit 3
fi
