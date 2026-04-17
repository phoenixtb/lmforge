#!/usr/bin/env bash
# =============================================================================
#  LMForge — Multi-Model E2E Integration Test
#  Layer 2: live daemon, real engine, real GPU models
#
#  Validates that an embed model and a chat model can be co-loaded by the same
#  LMForge daemon and handle burst traffic without interfering with each other.
#
#  USAGE
#  -----
#    bash tests/multi_model_e2e.sh
#
#  CONFIGURABLE DEFAULTS (override via env vars)
#  -----------------------------------------------
#    EMBED_MODEL   Embed model shortcut (default: qwen3-embed:0.6b:4bit)
#    CHAT_MODEL    Chat model shortcut  (default: qwen3.5:4b:4bit)
#    LF_HOST       LMForge API host     (default: http://127.0.0.1:11430)
#    LF_BIN        Path to lmforge bin  (default: ./target/debug/lmforge)
#    N_REQUESTS    Requests per burst   (default: 10)
#    SKIP_PULL     Set to 1 to skip pull step (models must already be present)
#    SKIP_START    Set to 1 to skip daemon start (daemon must already be running)
#
#  EXAMPLE
#  -------
#    EMBED_MODEL=nomic-embed-text:v1.5 \
#    CHAT_MODEL=qwen3.5:2b:4bit \
#    N_REQUESTS=5 \
#    bash tests/multi_model_e2e.sh
# =============================================================================

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────────────
EMBED_MODEL="${EMBED_MODEL:-qwen3-embed:0.6b:4bit}"
CHAT_MODEL="${CHAT_MODEL:-qwen3.5:4b:4bit}"
LF_HOST="${LF_HOST:-http://127.0.0.1:11430}"
LF_BIN="${LF_BIN:-./target/debug/lmforge}"
N="${N_REQUESTS:-10}"
SKIP_PULL="${SKIP_PULL:-0}"
SKIP_START="${SKIP_START:-0}"

# ─── Colour palette ───────────────────────────────────────────────────────────
GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BLUE='\033[0;34m'; BOLD='\033[1m'; DIM='\033[2m'; NC='\033[0m'

ok()   { echo -e "${GREEN}✓${NC} $*"; }
fail() { echo -e "${RED}✗${NC} $*"; exit 1; }
info() { echo -e "${CYAN}ℹ${NC} $*"; }
warn() { echo -e "${YELLOW}⚠${NC} $*"; }
sep()  { echo -e "${DIM}────────────────────────────────────────────────────────────────────${NC}"; }
hdr()  { echo -e "${CYAN}────────────────────────────────────────────────────────────────────${NC}"; }

# ─── Result ledger ────────────────────────────────────────────────────────────
# TC_RESULTS stores lines: "ID|STATUS|DESC|DETAIL"
# STATUS = PASS | FAIL | SKIP
TC_RESULTS=()

record_pass() { TC_RESULTS+=("$1|PASS|$2|$3"); }
record_fail() { TC_RESULTS+=("$1|FAIL|$2|$3"); }

# ─── Timing helper (bash 3.2 compatible — no associative arrays) ─────────────
# Keys are sanitised to valid identifier chars and stored as TIMER_<key> vars.
timer_start() {
    local key="${1//[^a-zA-Z0-9]/_}"
    printf -v "TIMER_${key}" '%s' "$(date +%s%N)"
}
timer_end() {
    local key="${1//[^a-zA-Z0-9]/_}"
    local end; end=$(date +%s%N)
    local varname="TIMER_${key}"
    local start="${!varname}"
    echo $(( (end - start) / 1000000 ))
}

# ─── Sparkline renderer ───────────────────────────────────────────────────────
# Given an array of millisecond values (name-ref), print one ASCII bar per value
# scaled relative to the max in the set. Each bar is labelled with its ms value.
# Usage: sparkline label_prefix "${arr[@]}"
sparkline() {
    local prefix="$1"; shift
    local values=("$@")
    local max_val=0

    for v in "${values[@]}"; do
        (( v > max_val )) && max_val=$v
    done

    local bar_max=30   # columns
    local i=0
    for v in "${values[@]}"; do
        local filled=0
        if (( max_val > 0 )); then
            filled=$(( v * bar_max / max_val ))
        fi
        # colour: green <50%, yellow <80%, red >=80% of max
        local colour="${GREEN}"
        (( filled > bar_max * 8 / 10 )) && colour="${RED}"
        (( filled > bar_max / 2 && filled <= bar_max * 8 / 10 )) && colour="${YELLOW}"

        local bar
        bar=$(printf '%0.s█' $(seq 1 "$filled") 2>/dev/null || printf '%*s' "$filled" '' | tr ' ' '█')
        local empty
        empty=$(printf '%*s' $(( bar_max - filled )) '')
        printf "   ${DIM}%s[%02d]${NC}  ${colour}%s${NC}${DIM}%s${NC}  ${DIM}%sms${NC}\n" \
            "$prefix" $((i+1)) "$bar" "$empty" "$v"
        (( i++ ))
    done
}

# ─── Stats helper (bash 3.2 compatible) ──────────────────────────────────────
# Usage: compute_stats val1 val2 val3 ...
# Sets globals: STAT_MIN STAT_MAX STAT_AVG STAT_P50
compute_stats() {
    local total=0 count=$#
    STAT_MIN=99999999; STAT_MAX=0
    local sorted=()

    for v in "$@"; do
        total=$(( total + v ))
        (( v < STAT_MIN )) && STAT_MIN=$v
        (( v > STAT_MAX )) && STAT_MAX=$v
        sorted+=("$v")
    done

    STAT_AVG=$(( total / count ))

    # p50 — sort numerically and pick middle
    IFS=$'\n' sorted=($(sort -n <<<"${sorted[*]}")); unset IFS
    local mid=$(( ${#sorted[@]} / 2 ))
    STAT_P50="${sorted[$mid]}"
}

# ─── Final report renderer ────────────────────────────────────────────────────
print_report() {
    local all_pass=true
    for line in "${TC_RESULTS[@]}"; do
        local status; status=$(echo "$line" | cut -d'|' -f2)
        [[ "$status" != "PASS" ]] && all_pass=false
    done

    echo ""
    hdr
    if $all_pass; then
        echo -e "${BOLD}${GREEN}  ✦  LMForge Multi-Model E2E — All Tests Passed  ✦${NC}"
    else
        echo -e "${BOLD}${RED}  ✦  LMForge Multi-Model E2E — Some Tests Failed  ✦${NC}"
    fi
    hdr
    echo ""

    # ── Config block ──────────────────────────────────────────────────────────
    printf "  ${DIM}%-18s${NC}  %s\n" "Embed model"  "$EMBED_MODEL"
    printf "  ${DIM}%-18s${NC}  %s\n" "Chat model"   "$CHAT_MODEL"
    printf "  ${DIM}%-18s${NC}  %s\n" "Host"         "$LF_HOST"
    printf "  ${DIM}%-18s${NC}  %s\n" "Burst size"   "${N} requests"
    echo ""

    # ── Test case result table ─────────────────────────────────────────────────
    printf "  ${BOLD}%-8s  %-8s  %-40s  %s${NC}\n" "Test" "Status" "Description" "Detail"
    printf "  %s\n" "$(printf '─%.0s' {1..72})"

    for line in "${TC_RESULTS[@]}"; do
        local id desc detail status
        id=$(echo "$line"    | cut -d'|' -f1)
        status=$(echo "$line" | cut -d'|' -f2)
        desc=$(echo "$line"  | cut -d'|' -f3)
        detail=$(echo "$line" | cut -d'|' -f4)

        local icon colour
        if [[ "$status" == "PASS" ]]; then
            icon="  PASS  "; colour="${GREEN}"
        else
            icon="  FAIL  "; colour="${RED}"
        fi

        printf "  ${BOLD}%-8s${NC}  ${colour}%s${NC}  %-40s  ${DIM}%s${NC}\n" \
            "$id" "$icon" "$desc" "$detail"
    done

    echo ""

    # ── Latency summary panel ─────────────────────────────────────────────────
    printf "  ${BOLD}%-36s  %7s  %7s  %7s  %7s${NC}\n" \
        "Measurement" "min" "avg" "p50" "max"
    printf "  %s\n" "$(printf '─%.0s' {1..72})"

    printf "  %-36s  %6sms  %6sms  %6sms  %6sms\n" \
        "Cold load — embed"            "${embed_cold_ms}"  "${embed_cold_ms}"  "${embed_cold_ms}"  "${embed_cold_ms}"
    printf "  %-36s  %6sms  %6sms  %6sms  %6sms\n" \
        "Cold load — chat"             "${chat_cold_ms}"   "${chat_cold_ms}"   "${chat_cold_ms}"   "${chat_cold_ms}"
    printf "  %-36s  %6sms  %6sms  %6sms  %6sms\n" \
        "Sequential embed (${N}x)"     "${min_embed}"      "${avg_embed}"      "${p50_embed}"      "${max_embed}"
    printf "  %-36s  %6sms  %6sms  %6sms  %6sms\n" \
        "Sequential chat  (${N}x)"     "${min_chat}"       "${avg_chat}"       "${p50_chat}"       "${max_chat}"
    printf "  %-36s  %6s    %6s    %6s    %6sms\n" \
        "Concurrent embed (${N}x wall)" "—"                 "—"                 "—"                "${concurrent_ms}"
    printf "  %-36s  %6s    %6s    %6s    %6sms\n" \
        "Simultaneous embed+chat (wall)" "—"                "—"                 "—"                "${mixed_ms}"

    echo ""
    hdr

    if $all_pass; then
        exit 0
    else
        exit 1
    fi
}

# ─── Header ───────────────────────────────────────────────────────────────────
hdr
echo -e "  ${BOLD}LMForge Multi-Model E2E Test${NC}"
echo ""
printf "  ${DIM}%-16s${NC}  %s\n" "Embed model"  "$EMBED_MODEL"
printf "  ${DIM}%-16s${NC}  %s\n" "Chat model"   "$CHAT_MODEL"
printf "  ${DIM}%-16s${NC}  %s\n" "API host"     "$LF_HOST"
printf "  ${DIM}%-16s${NC}  %s\n" "Burst size"   "$N"
hdr

# ─── Trap / cleanup ───────────────────────────────────────────────────────────
# Track which models this test run downloaded (vs found pre-existing).
# Only those will be removed on exit — pre-existing models are never touched.
daemon_pid=""
EMBED_PULLED_BY_TEST=0
CHAT_PULLED_BY_TEST=0

cleanup() {
    echo ""
    info "Cleaning up..."

    # Stop daemon (only if we started it)
    if [[ -n "$daemon_pid" ]] && [[ "$SKIP_START" -eq 0 ]]; then
        "$LF_BIN" stop 2>/dev/null || true
        kill "$daemon_pid" 2>/dev/null || true
    fi

    # Remove models only if this test run downloaded them
    if [[ "$EMBED_PULLED_BY_TEST" -eq 1 ]]; then
        info "Removing embed model downloaded by this test run: ${EMBED_MODEL}"
        "$LF_BIN" models remove "$EMBED_MODEL" 2>/dev/null || true
    fi
    if [[ "$CHAT_PULLED_BY_TEST" -eq 1 ]]; then
        info "Removing chat model downloaded by this test run: ${CHAT_MODEL}"
        "$LF_BIN" models remove "$CHAT_MODEL" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ─── Pre-flight: build ────────────────────────────────────────────────────────
info "Building lmforge..."
cargo build 2>&1 | tail -3
ok "Build complete"
sep

# ─── Step 1: Pull models ──────────────────────────────────────────────────────
if [[ "$SKIP_PULL" -ne 1 ]]; then
    info "Step 1 — Pulling models (this may take a while the first time)"

    echo "  Pulling embed model: ${EMBED_MODEL}"
    pull_out=$("$LF_BIN" pull "$EMBED_MODEL" 2>&1) || fail "Failed to pull embed model: $pull_out"
    if echo "$pull_out" | grep -q "already installed"; then
        ok "Embed model already present — skipping download"
    else
        ok "Embed model downloaded"
        EMBED_PULLED_BY_TEST=1
    fi

    echo "  Pulling chat model: ${CHAT_MODEL}"
    pull_out=$("$LF_BIN" pull "$CHAT_MODEL" 2>&1) || fail "Failed to pull chat model: $pull_out"
    if echo "$pull_out" | grep -q "already installed"; then
        ok "Chat model already present — skipping download"
    else
        ok "Chat model downloaded"
        CHAT_PULLED_BY_TEST=1
    fi
else
    info "Step 1 — Skipping pull (SKIP_PULL=1)"
fi
sep

# ─── Step 2: Start daemon ─────────────────────────────────────────────────────
if [[ "$SKIP_START" -ne 1 ]]; then
    info "Step 2 — Starting daemon"
    "$LF_BIN" start &
    daemon_pid=$!

    info "Waiting for daemon to become healthy..."
    # Health response is {"status":"ok"} (HTTP 200). Use curl -sf -o /dev/null:
    # -f exits non-zero on 4xx/5xx, -s is silent, -o /dev/null discards body.
    # No grep needed — exit code is the only signal we care about.
    healthy=0
    for i in $(seq 1 90); do
        if curl -sf -o /dev/null "${LF_HOST}/health" 2>/dev/null; then
            echo ""
            ok "Daemon healthy (after ${i}s)"
            healthy=1
            break
        fi
        printf "  ${DIM}waiting... %ds${NC}\r" "$i"
        sleep 1
    done
    [[ "$healthy" -eq 1 ]] || fail "Daemon did not become healthy within 90s"
else
    info "Step 2 — Skipping daemon start (SKIP_START=1), assuming ${LF_HOST} is live"
    curl -sf -o /dev/null "${LF_HOST}/health" 2>/dev/null || fail "Daemon at ${LF_HOST} is not healthy (HTTP non-200)"
    ok "Daemon healthy"
fi
sep

# ─── Helpers: typed request functions ─────────────────────────────────────────
lf_embed() {
    curl -sf -X POST "${LF_HOST}/v1/embeddings" \
        -H "Content-Type: application/json" \
        -d "{\"model\": \"${EMBED_MODEL}\", \"input\": \"$1\"}"
}

lf_chat() {
    curl -sf -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "{\"model\": \"${CHAT_MODEL}\", \"messages\": [{\"role\": \"user\", \"content\": \"$1\"}], \"stream\": false}"
}

lf_status() { curl -sf "${LF_HOST}/lf/status"; }

# Assert that a JSON response is a valid /v1/embeddings reply.
# Uses jq — no Python required.
assert_embed_response() {
    local resp="$1" label="$2"
    # Check .data[0].embedding is a non-empty array
    local dims
    dims=$(echo "$resp" | jq -r '.data[0].embedding | length' 2>/dev/null) \
        || fail "${label}: response is not valid JSON — ${resp:0:200}"
    [[ "$dims" =~ ^[0-9]+$ ]] && [[ "$dims" -gt 0 ]] \
        || fail "${label}: embedding vector is empty or missing (dims=${dims}) — ${resp:0:200}"
}

# Assert that a JSON response is a valid /v1/chat/completions reply.
assert_chat_response() {
    local resp="$1" label="$2"
    local content
    content=$(echo "$resp" | jq -r '.choices[0].message.content // empty' 2>/dev/null) \
        || fail "${label}: response is not valid JSON — ${resp:0:200}"
    [[ -n "$content" ]] \
        || fail "${label}: assistant content is empty — ${resp:0:200}"
}

# ─── TC-E01: Cold-start co-load ───────────────────────────────────────────────
echo -e "\n${BOLD}TC-E01${NC}  Cold-start co-load"

timer_start "embed_cold"
resp=$(lf_embed "what is natural language processing?" 2>&1) || fail "TC-E01: embed cold-load failed: $resp"
embed_cold_ms=$(timer_end "embed_cold")
assert_embed_response "$resp" "TC-E01 embed"
printf "  ${GREEN}✓${NC} Embed model loaded  ${DIM}%sms${NC}\n" "$embed_cold_ms"

timer_start "chat_cold"
resp=$(lf_chat "Say hello in one word." 2>&1) || fail "TC-E01: chat cold-load failed: $resp"
chat_cold_ms=$(timer_end "chat_cold")
assert_chat_response "$resp" "TC-E01 chat"
printf "  ${GREEN}✓${NC} Chat model loaded   ${DIM}%sms${NC}\n" "$chat_cold_ms"

status_resp=$(lf_status)
# running_models is a JSON array; find each model by .model_id
for _model in "$EMBED_MODEL" "$CHAT_MODEL"; do
    found=$(echo "$status_resp" | jq -r --arg m "$_model" \
        '[.running_models[] | select(.model_id == $m)] | length' 2>/dev/null)
    [[ "$found" -gt 0 ]] \
        || fail "TC-E01: model '${_model}' not found in /lf/status running_models"
done

record_pass "TC-E01" "Cold-start co-load" \
    "embed=${embed_cold_ms}ms  chat=${chat_cold_ms}ms"

# ─── TC-E02: Sequential embed burst ───────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E02${NC}  Sequential embed burst  ${DIM}(${N} requests)${NC}"
embed_latencies=()

for i in $(seq 1 "$N"); do
    timer_start "embed_seq_$i"
    resp=$(lf_embed "sequential embed sentence number $i, testing latency and correctness" 2>&1) \
        || fail "TC-E02: embed request $i failed"
    ms=$(timer_end "embed_seq_$i")
    embed_latencies+=("$ms")
    assert_embed_response "$resp" "TC-E02 req $i"
done

compute_stats "${embed_latencies[@]}"
min_embed=$STAT_MIN; avg_embed=$STAT_AVG; max_embed=$STAT_MAX; p50_embed=$STAT_P50

echo ""
sparkline "embed" "${embed_latencies[@]}"
echo ""
printf "  ${DIM}min ${GREEN}%sms${NC}  ${DIM}avg${NC} ${YELLOW}%sms${NC}  ${DIM}p50${NC} %sms  ${DIM}max ${RED}%sms${NC}\n" \
    "$min_embed" "$avg_embed" "$p50_embed" "$max_embed"

record_pass "TC-E02" "Sequential embed (${N}x)" \
    "min=${min_embed}ms  avg=${avg_embed}ms  p50=${p50_embed}ms  max=${max_embed}ms"

# ─── TC-E03: Sequential chat burst ────────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E03${NC}  Sequential chat burst  ${DIM}(${N} requests)${NC}"
chat_latencies=()

for i in $(seq 1 "$N"); do
    timer_start "chat_seq_$i"
    resp=$(lf_chat "What is 1 + $i? Answer with only the number." 2>&1) \
        || fail "TC-E03: chat request $i failed"
    ms=$(timer_end "chat_seq_$i")
    chat_latencies+=("$ms")
    assert_chat_response "$resp" "TC-E03 req $i"
done

compute_stats "${chat_latencies[@]}"
min_chat=$STAT_MIN; avg_chat=$STAT_AVG; max_chat=$STAT_MAX; p50_chat=$STAT_P50

echo ""
sparkline "chat " "${chat_latencies[@]}"
echo ""
printf "  ${DIM}min ${GREEN}%sms${NC}  ${DIM}avg${NC} ${YELLOW}%sms${NC}  ${DIM}p50${NC} %sms  ${DIM}max ${RED}%sms${NC}\n" \
    "$min_chat" "$avg_chat" "$p50_chat" "$max_chat"

record_pass "TC-E03" "Sequential chat (${N}x)" \
    "min=${min_chat}ms  avg=${avg_chat}ms  p50=${p50_chat}ms  max=${max_chat}ms"

# ─── TC-E04: Concurrent embed burst ───────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E04${NC}  Concurrent embed burst  ${DIM}(${N} parallel requests)${NC}"

tmpdir=$(mktemp -d)
timer_start "embed_concurrent"

pids=()
for i in $(seq 1 "$N"); do
    (
        resp=$(lf_embed "concurrent embed batch item $i" 2>&1)
        echo "$resp" > "${tmpdir}/embed_${i}.json"
    ) &
    pids+=($!)
done

fail_count=0
for pid in "${pids[@]}"; do
    wait "$pid" 2>/dev/null || (( fail_count++ )) || true
done
concurrent_ms=$(timer_end "embed_concurrent")

[[ $fail_count -gt 0 ]] && fail "TC-E04: $fail_count/${N} concurrent embed requests failed"

for i in $(seq 1 "$N"); do
    [[ -f "${tmpdir}/embed_${i}.json" ]] || fail "TC-E04: response missing for request $i"
    assert_embed_response "$(cat "${tmpdir}/embed_${i}.json")" "TC-E04 req $i"
done
rm -rf "$tmpdir"

printf "  ${GREEN}✓${NC} All ${N} concurrent embed requests succeeded  ${DIM}(wall: %sms)${NC}\n" "$concurrent_ms"
record_pass "TC-E04" "Concurrent embed (${N}x)" \
    "wall=${concurrent_ms}ms  throughput=$(( N * 1000 / concurrent_ms )) req/s"

# ─── TC-E05: Simultaneous embed + chat ────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E05${NC}  Simultaneous embed + chat  ${DIM}(1 of each in parallel)${NC}"

tmpdir=$(mktemp -d)
timer_start "mixed_concurrent"

lf_embed "simultaneous embedding test" > "${tmpdir}/embed.json" 2>&1 &
pid_embed=$!
lf_chat  "Say 'concurrent' in your response." > "${tmpdir}/chat.json" 2>&1 &
pid_chat=$!

wait "$pid_embed" || fail "TC-E05: simultaneous embed request failed"
wait "$pid_chat"  || fail "TC-E05: simultaneous chat request failed"
mixed_ms=$(timer_end "mixed_concurrent")

assert_embed_response "$(cat "${tmpdir}/embed.json")" "TC-E05 embed"
assert_chat_response  "$(cat "${tmpdir}/chat.json")"  "TC-E05 chat"
rm -rf "$tmpdir"

printf "  ${GREEN}✓${NC} Embed + chat completed simultaneously  ${DIM}(wall: %sms)${NC}\n" "$mixed_ms"
record_pass "TC-E05" "Simultaneous embed+chat" "wall=${mixed_ms}ms"

# ─── TC-E06: Cross-endpoint rejection ─────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E06${NC}  Cross-endpoint capability gate rejection"

code_e_at_chat=$(curl -s -o /dev/null -w "%{http_code}" -X POST "${LF_HOST}/v1/chat/completions" \
    -H "Content-Type: application/json" \
    -d "{\"model\": \"${EMBED_MODEL}\", \"messages\": [{\"role\": \"user\", \"content\": \"hi\"}], \"stream\": false}")

code_c_at_embed=$(curl -s -o /dev/null -w "%{http_code}" -X POST "${LF_HOST}/v1/embeddings" \
    -H "Content-Type: application/json" \
    -d "{\"model\": \"${CHAT_MODEL}\", \"input\": \"test\"}")

gate_ok=true
[[ "$code_e_at_chat" != "400" ]] && { warn "embed@chat returned ${code_e_at_chat} (expected 400)"; gate_ok=false; }
[[ "$code_c_at_embed" != "400" ]] && { warn "chat@embed returned ${code_c_at_embed} (expected 400)"; gate_ok=false; }

if $gate_ok; then
    printf "  ${GREEN}✓${NC} embed→chat rejected ${DIM}(HTTP %s)${NC}   ${GREEN}✓${NC} chat→embed rejected ${DIM}(HTTP %s)${NC}\n" \
        "$code_e_at_chat" "$code_c_at_embed"
    record_pass "TC-E06" "Cross-endpoint rejection" \
        "embed@chat=${code_e_at_chat}  chat@embed=${code_c_at_embed}"
else
    record_fail "TC-E06" "Cross-endpoint rejection" \
        "embed@chat=${code_e_at_chat}  chat@embed=${code_c_at_embed}"
    fail "TC-E06: capability gate did not reject as expected"
fi

# ─── TC-E07: State consistency after burst ────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E07${NC}  State consistency after full burst"

state_detail=""
state_ok=true
for _label_model in "embed:${EMBED_MODEL}" "chat:${CHAT_MODEL}"; do
    _label="${_label_model%%:*}"
    _model="${_label_model#*:}"
    _slot=$(lf_status | jq -c --arg m "$_model" '.running_models[] | select(.model_id == $m)' 2>/dev/null)
    if [[ -z "$_slot" ]]; then
        warn "TC-E07: ${_label} model '${_model}' missing from running_models"
        state_ok=false
    else
        _status=$(echo "$_slot" | jq -r '.status // "?"')
        _idle=$(echo "$_slot" | jq -r '.idle_secs // "?"')
        state_detail+="${_label}: status=${_status} idle=${_idle}s  "
        if [[ "$_status" != "ready" ]]; then
            warn "TC-E07: ${_label} model status='${_status}' (expected 'ready')"
            state_ok=false
        fi
    fi
done
if ! $state_ok; then
    record_fail "TC-E07" "State consistency" "${state_detail}"
    fail "TC-E07 failed"
fi

printf "  ${GREEN}✓${NC} ${DIM}%s${NC}\n" "$state_detail"
record_pass "TC-E07" "State consistency" "$state_detail"

# ─── Final report ─────────────────────────────────────────────────────────────
print_report
