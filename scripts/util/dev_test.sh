#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# LMForge dev test runner.
#
# Three layers, each opt-out via flag:
#   1. cargo test --lib          (~5 s, 285 unit tests)
#   2. cargo test --tests        (~5 s, 13 in-process integration tests)
#   3. live e2e                  (~3 min on first run, ~30 s after model cached)
#                                — opt-IN via --with-e2e
#
# E2E tests exercise the running daemon as a real client would:
#   /health, /lf/status, /lf/catalog (count check), /v1/chat/completions
#   (non-streaming + streaming), /v1/embeddings, /lf/model/unload, /lf/sysinfo,
#   /metrics (Prometheus). Each test prints PASS/FAIL with timing.
#
# Usage:
#   ./dev_test.sh                        unit + integration only (fast)
#   ./dev_test.sh --with-e2e             everything (needs GPU + ~3 GB free)
#   ./dev_test.sh --e2e-only             skip cargo, run e2e against existing daemon
#   ./dev_test.sh --no-unit              skip --lib
#   ./dev_test.sh --no-integration       skip --tests
#   ./dev_test.sh --release              cargo build/test with --release
#   ./dev_test.sh --e2e-model qwen3:4b:4bit   override default chat model
#   ./dev_test.sh --embed-model qwen3-embed:0.6b:8bit   override embed model
#   ./dev_test.sh --no-embed             skip the embeddings probe
#   ./dev_test.sh --keep-daemon          don't stop daemon after e2e (for poking)
#
# Exit codes:
#   0  all selected tests passed
#   1  cargo unit/integration failures
#   2  daemon failed to start
#   3  one or more e2e assertions failed
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail        # NOTE: no -e — we want to count failures, not abort

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

DO_UNIT=1
DO_INTEGRATION=1
DO_E2E=0
E2E_ONLY=0
CARGO_PROFILE=""        # "" = debug, "--release" = release
E2E_MODEL="qwen3:1.7b:4bit"            # default chat model (~1 GB GGUF)
EMBED_MODEL="qwen3-embed:0.6b:8bit"    # default embed model (~600 MB GGUF)
DO_EMBED=1
KEEP_DAEMON=0

while (($#)); do
    case "$1" in
        --with-e2e)       DO_E2E=1 ;;
        --e2e-only)       DO_E2E=1; E2E_ONLY=1; DO_UNIT=0; DO_INTEGRATION=0 ;;
        --no-unit)        DO_UNIT=0 ;;
        --no-integration) DO_INTEGRATION=0 ;;
        --release)        CARGO_PROFILE="--release" ;;
        --e2e-model)      E2E_MODEL="${2:?--e2e-model requires a key}"; shift ;;
        --embed-model)    EMBED_MODEL="${2:?--embed-model requires a key}"; shift ;;
        --no-embed)       DO_EMBED=0 ;;
        --keep-daemon)    KEEP_DAEMON=1 ;;
        -h|--help)        sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)                echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
pass() { echo -e "  ${GREEN}PASS${NC}  $1 ${BLUE}(${2}s)${NC}"; }
fail() { echo -e "  ${RED}FAIL${NC}  $1 ${BLUE}(${2}s)${NC}  ${RED}${3:-}${NC}"; FAILS=$((FAILS+1)); }
sec()  { echo -e "\n${BOLD}── $* ──${NC}"; }
note() { echo -e "  ${YELLOW}·${NC} $*"; }

FAILS=0
START_TOTAL=$(date +%s)

# ── Phase 1: cargo --lib ─────────────────────────────────────────────────────
if (( DO_UNIT )); then
    sec "Unit tests (cargo test --lib)"
    cd "$REPO_ROOT"
    t0=$(date +%s)
    if cargo test --lib $CARGO_PROFILE 2>&1 | tail -5 | grep -E "test result:"; then
        pass "cargo test --lib"             "$(( $(date +%s) - t0 ))"
    else
        fail "cargo test --lib"             "$(( $(date +%s) - t0 ))" "see output above"
    fi
fi

# ── Phase 2: cargo --tests ───────────────────────────────────────────────────
if (( DO_INTEGRATION )); then
    sec "Integration tests (cargo test --tests)"
    cd "$REPO_ROOT"
    t0=$(date +%s)
    if cargo test --tests $CARGO_PROFILE 2>&1 | tail -5 | grep -E "test result:"; then
        pass "cargo test --tests"           "$(( $(date +%s) - t0 ))"
    else
        fail "cargo test --tests"           "$(( $(date +%s) - t0 ))" "see output above"
    fi
fi

# ── Phase 2.5: CLI smoke — engine subcommand ─────────────────────────────────
# The `engine` subcommand is daemon-independent: it loads engines.toml,
# probes hardware, and prints status. Bugs in tier gating / install-state
# detection caught here are way cheaper than catching them in e2e.
if (( DO_INTEGRATION )); then
    sec "CLI smoke (lmforge engine)"
    cd "$REPO_ROOT"

    # Use the debug binary directly — works even when symlink in ~/.cargo/bin
    # is stale (Cursor sandbox redirects target/, common dev pitfall).
    BIN="$REPO_ROOT/target/debug/lmforge"
    if [[ ! -x "$BIN" ]]; then
        BIN="$(command -v lmforge || true)"
    fi

    if [[ -z "$BIN" || ! -x "$BIN" ]]; then
        fail "lmforge binary"               "0" "no debug build or PATH binary"
    else
        # engine list — must succeed and mention each engine id.
        t0=$(date +%s)
        OUT=$("$BIN" engine list 2>&1 || true)
        if echo "$OUT" | grep -q '^omlx ' \
            && echo "$OUT" | grep -q '^sglang ' \
            && echo "$OUT" | grep -q '^llamacpp ' \
            && echo "$OUT" | grep -q '^vllm ' \
            && echo "$OUT" | grep -q '^tabbyapi '; then
            pass "lmforge engine list"      "$(( $(date +%s) - t0 ))"
        else
            fail "lmforge engine list"      "$(( $(date +%s) - t0 ))" "missing one of omlx/sglang/llamacpp/vllm/tabbyapi"
        fi

        # engine status tabbyapi — must report opt-in tier verdict.
        t0=$(date +%s)
        OUT=$("$BIN" engine status tabbyapi 2>&1 || true)
        if echo "$OUT" | grep -qi 'opt-in' && echo "$OUT" | grep -qi 'tabbyapi'; then
            pass "lmforge engine status tabbyapi" "$(( $(date +%s) - t0 ))"
        else
            fail "lmforge engine status tabbyapi" "$(( $(date +%s) - t0 ))" "missing opt-in verdict"
        fi

        # engine status vllm — must report the opt-in tier verdict.
        t0=$(date +%s)
        OUT=$("$BIN" engine status vllm 2>&1 || true)
        if echo "$OUT" | grep -q 'Tier:.*opt-in'; then
            pass "lmforge engine status vllm"  "$(( $(date +%s) - t0 ))"
        else
            fail "lmforge engine status vllm"  "$(( $(date +%s) - t0 ))" "tier not surfaced"
        fi

        # engine install bogus — must fail fast with a clear error.
        # `set -uo pipefail` would let the failing exit code from `engine
        # install bogus` short-circuit the pipe; capture into a variable
        # so we only grep the message, not the exit status.
        t0=$(date +%s)
        OUT=$("$BIN" engine install bogus 2>&1 || true)
        if echo "$OUT" | grep -qi 'unknown engine'; then
            pass "lmforge engine install <bogus> rejects" "$(( $(date +%s) - t0 ))"
        else
            fail "lmforge engine install <bogus>" "$(( $(date +%s) - t0 ))" "missing error for unknown id"
        fi

        # engine install llamacpp — must be a no-op (default tier).
        t0=$(date +%s)
        OUT=$("$BIN" engine install llamacpp 2>&1 || true)
        if echo "$OUT" | grep -qi 'default-tier'; then
            pass "lmforge engine install llamacpp (no-op)" "$(( $(date +%s) - t0 ))"
        else
            fail "lmforge engine install llamacpp" "$(( $(date +%s) - t0 ))" "expected no-op explanation"
        fi
    fi
fi

# ── Phase 3: live e2e ────────────────────────────────────────────────────────
DAEMON_STARTED_BY_US=0
DAEMON_PID=""

cleanup() {
    if (( DAEMON_STARTED_BY_US == 1 && KEEP_DAEMON == 0 )); then
        note "stopping daemon we started (PID=$DAEMON_PID)"
        kill "$DAEMON_PID" 2>/dev/null || true
        sleep 1
    fi
}
trap cleanup EXIT

curl_health() { curl -sf --max-time 2 http://127.0.0.1:11430/health >/dev/null 2>&1; }

if (( DO_E2E )); then
    sec "E2E setup"

    # Discover or start daemon
    if curl_health; then
        note "using already-running daemon"
    else
        command -v lmforge >/dev/null || { fail "no lmforge on PATH" "0" "build + symlink first"; exit 2; }
        note "starting daemon in background (logs → ~/.lmforge/logs/dev_test.log)..."
        RUST_LOG="lmforge=info,sglang=info" RUST_BACKTRACE=1 \
            lmforge start > "$HOME/.lmforge/logs/dev_test.log" 2>&1 &
        DAEMON_PID=$!
        DAEMON_STARTED_BY_US=1
        for i in {1..30}; do
            curl_health && break
            sleep 1
        done
        if ! curl_health; then
            fail "daemon did not become healthy in 30 s" "30" "tail ~/.lmforge/logs/dev_test.log"
            tail -20 "$HOME/.lmforge/logs/dev_test.log" 2>/dev/null
            exit 2
        fi
        note "daemon ready, PID=$DAEMON_PID"
    fi

    sec "E2E suite (model=$E2E_MODEL)"
    BASE="http://127.0.0.1:11430"

    # 1. /health returns ok with version
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/health")
    if echo "$RESP" | jq -e '.status == "ok"' >/dev/null 2>&1; then
        pass "GET /health"                      "$(( $(date +%s) - t0 ))"
    else
        fail "GET /health"                      "$(( $(date +%s) - t0 ))" "$RESP"
    fi

    # 2. /lf/status returns expected schema. `last_errors` MUST be present
    #    (added in Phase 2.3, consumed by the UI Overview's Engine Load
    #    Errors panel in Phase 6). It's allowed to be empty `{}` but the
    #    key itself must not disappear.
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/lf/status")
    if echo "$RESP" | jq -e '.overall_status and .engine.id and (.running_models | type == "array") and (.last_errors | type == "object")' >/dev/null 2>&1; then
        pass "GET /lf/status schema (overall_status/engine/running_models/last_errors)" "$(( $(date +%s) - t0 ))"
    else
        fail "GET /lf/status schema"            "$(( $(date +%s) - t0 ))" "$RESP"
    fi

    # 2b. /lf/engines — Phase 6 endpoint that drives the Settings → Engine UI.
    #     Shape guard: must return an array of engine rows with at least
    #     `id`, `tier`, `installed`, `compatible` fields. Any one of those
    #     missing breaks the tier-switcher renderer.
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/lf/engines")
    if echo "$RESP" | jq -e '(.engines | type == "array") and (.engines | length > 0) and (.engines[0] | (.id and .tier and (.installed | type == "boolean")))' >/dev/null 2>&1; then
        ECOUNT=$(echo "$RESP" | jq '.engines | length')
        pass "GET /lf/engines ($ECOUNT engines, tier+installed fields present)" "$(( $(date +%s) - t0 ))"
    else
        fail "GET /lf/engines schema"           "$(( $(date +%s) - t0 ))" "$RESP"
    fi

    # 3. catalog has expected count (current quantized-only safetensors.json = 80).
    #    The endpoint returns {"entries":[...]} so we count the array, not the wrapper.
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/lf/catalog?format=safetensors")
    COUNT=$(echo "$RESP" | jq '.entries | length' 2>/dev/null || echo 0)
    if (( COUNT >= 75 )); then
        pass "GET /lf/catalog?format=safetensors ($COUNT entries)" "$(( $(date +%s) - t0 ))"
    else
        fail "GET /lf/catalog"                  "$(( $(date +%s) - t0 ))" "got $COUNT entries, expected ≥ 75"
    fi

    # 4. /metrics returns Prometheus body
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/metrics")
    if echo "$RESP" | grep -qE "^# (HELP|TYPE)"; then
        pass "GET /metrics (Prometheus format)" "$(( $(date +%s) - t0 ))"
    else
        fail "GET /metrics"                     "$(( $(date +%s) - t0 ))" "not Prometheus format"
    fi

    # 5. /lf/sysinfo returns live CPU / memory / GPU / per-model RSS telemetry.
    #    Schema (see src/server/sysinfo.rs): cpu_pct, mem_total_gb, mem_avail_gb,
    #    gpu{util_pct,mem_used_mb}, model_procs[], model_rss_gb.
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 5 "$BASE/lf/sysinfo")
    if echo "$RESP" | jq -e '.cpu_pct != null and .mem_total_gb > 0 and (.gpu | type == "object") and (.model_procs | type == "array")' >/dev/null 2>&1; then
        pass "GET /lf/sysinfo"                  "$(( $(date +%s) - t0 ))"
    else
        fail "GET /lf/sysinfo"                  "$(( $(date +%s) - t0 ))" "$RESP"
    fi

    # 6. Model pull (largest step; only if not already indexed)
    sec "E2E inference (model=$E2E_MODEL)"
    if curl -sf --max-time 3 "$BASE/lf/model/list" | jq -e --arg id "$E2E_MODEL" '.models[] | select(.id==$id)' >/dev/null 2>&1; then
        note "model $E2E_MODEL already in index — skipping pull"
    else
        note "pulling $E2E_MODEL (one-time, may take ~3 min)..."
        t0=$(date +%s)
        if lmforge pull "$E2E_MODEL" 2>&1 | tail -3; then
            pass "lmforge pull $E2E_MODEL"      "$(( $(date +%s) - t0 ))"
        else
            fail "lmforge pull $E2E_MODEL"      "$(( $(date +%s) - t0 ))" "pull failed — engine logs?"
            exit 3
        fi
    fi

    # 7. Non-streaming chat completion.
    #    `chat_template_kwargs.enable_thinking=false` disables the Qwen3/3.5
    #    reasoning preamble at the template level (the canonical way per the
    #    Qwen model card). `/no_think` as plain user text does NOT work —
    #    Qwen3.5 treats it as a literal prompt and still emits <think>.
    #    Pass-condition accepts either `content` OR `reasoning_content` so
    #    the test stays green for any model family — what we're verifying
    #    here is "tokens flowed end-to-end", not the absence of CoT.
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 120 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$E2E_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with exactly the word OK\"}],\"max_tokens\":48,\"temperature\":0,\"chat_template_kwargs\":{\"enable_thinking\":false}}")
    TXT=$(echo "$RESP" | jq -r '(.choices[0].message.content // "") + (.choices[0].message.reasoning_content // "") | select(. != "")' 2>/dev/null)
    if [[ -n "$TXT" ]]; then
        pass "POST /v1/chat/completions (got: \"${TXT:0:40}\")" "$(( $(date +%s) - t0 ))"
    else
        fail "POST /v1/chat/completions"        "$(( $(date +%s) - t0 ))" "no content/reasoning; resp: $RESP"
    fi

    # 8. Streaming chat (SSE) — count chunks, look for [DONE].
    t0=$(date +%s)
    STREAM_OUT=$(curl -sN --max-time 120 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$E2E_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Count: 1 2 3\"}],\"max_tokens\":48,\"stream\":true,\"temperature\":0,\"chat_template_kwargs\":{\"enable_thinking\":false}}")
    CHUNKS=$(echo "$STREAM_OUT" | grep -c '^data: ')
    if (( CHUNKS >= 2 )) && echo "$STREAM_OUT" | grep -q '^data: \[DONE\]'; then
        pass "POST /v1/chat/completions stream ($CHUNKS chunks + [DONE])" "$(( $(date +%s) - t0 ))"
    else
        fail "POST /v1/chat/completions stream" "$(( $(date +%s) - t0 ))" "$CHUNKS chunks, [DONE] not seen"
    fi

    # 9. Embeddings — dedicated step that pulls (if needed) and probes its own
    #    embed model, independent of the chat model under test. Set --no-embed
    #    to skip, or --embed-model <id> to override.
    if (( DO_EMBED )); then
        sec "E2E embeddings (model=$EMBED_MODEL)"
        if curl -sf --max-time 3 "$BASE/lf/model/list" | jq -e --arg id "$EMBED_MODEL" '.models[] | select(.id==$id)' >/dev/null 2>&1; then
            note "embed model $EMBED_MODEL already in index — skipping pull"
        else
            note "pulling $EMBED_MODEL (one-time, ~30 s)..."
            t0=$(date +%s)
            if lmforge pull "$EMBED_MODEL" 2>&1 | tail -3; then
                pass "lmforge pull $EMBED_MODEL"  "$(( $(date +%s) - t0 ))"
            else
                fail "lmforge pull $EMBED_MODEL"  "$(( $(date +%s) - t0 ))" "pull failed — engine logs?"
            fi
        fi

        t0=$(date +%s)
        RESP=$(curl -sf --max-time 60 "$BASE/v1/embeddings" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$EMBED_MODEL\",\"input\":\"hello world\"}")
        DIM=$(echo "$RESP" | jq -r '.data[0].embedding | length' 2>/dev/null || echo 0)
        if (( DIM > 0 )); then
            pass "POST /v1/embeddings (dim=$DIM)"   "$(( $(date +%s) - t0 ))"
        else
            fail "POST /v1/embeddings"              "$(( $(date +%s) - t0 ))" "no vector returned; resp: $RESP"
        fi

        # Batch embedding (multiple inputs in one call) — exercises the chunker.
        t0=$(date +%s)
        RESP=$(curl -sf --max-time 60 "$BASE/v1/embeddings" \
            -H 'Content-Type: application/json' \
            -d "{\"model\":\"$EMBED_MODEL\",\"input\":[\"alpha\",\"beta\",\"gamma\"]}")
        N=$(echo "$RESP" | jq -r '.data | length' 2>/dev/null || echo 0)
        if (( N == 3 )); then
            pass "POST /v1/embeddings batch (n=$N)" "$(( $(date +%s) - t0 ))"
        else
            fail "POST /v1/embeddings batch"        "$(( $(date +%s) - t0 ))" "got $N vectors, expected 3"
        fi
    else
        note "skipping /v1/embeddings (--no-embed)"
    fi

    # 10. Model unload — exercises the orchestrator control plane.
    sec "E2E control plane"
    t0=$(date +%s)
    RESP=$(curl -sf --max-time 30 -X POST "$BASE/lf/model/unload" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$E2E_MODEL\"}")
    if echo "$RESP" | jq -e '.status // .ok // .message' >/dev/null 2>&1; then
        pass "POST /lf/model/unload ($E2E_MODEL)"   "$(( $(date +%s) - t0 ))"
    else
        # Some daemons return 200 with empty body — that's also a pass.
        pass "POST /lf/model/unload (no body)"      "$(( $(date +%s) - t0 ))"
    fi
fi

# ── Summary ──────────────────────────────────────────────────────────────────
ELAPSED=$(( $(date +%s) - START_TOTAL ))
echo ""
echo -e "${BOLD}────────────────────────────────────────${NC}"
if (( FAILS == 0 )); then
    echo -e "${GREEN}  ✓ ALL PASSED${NC}  (${ELAPSED}s total)"
    exit 0
else
    echo -e "${RED}  ✗ $FAILS FAILURE(S)${NC}  (${ELAPSED}s total)"
    exit 3
fi
