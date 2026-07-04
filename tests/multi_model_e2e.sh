#!/usr/bin/env bash
# =============================================================================
#  LMForge — Multi-Model E2E Integration Test
#  Layer 2: live daemon, real engine, real GPU models
#
#  Validates that an embed model and a chat model can be co-loaded by the same
#  LMForge daemon and handle burst traffic without interfering with each other.
#  TC-E01..E12 cover load/burst/concurrency/gate/VLM/rerank/MTP; TC-E13..E15
#  cover the thinking pipeline (ADR-007): think=on reasoning+answer, think=off
#  non-blank (Fix #3c), and the thinking_budget orchestrator (Fix #5b). The
#  thinking cases auto-skip when the chat model isn't thinking-capable.
#
#  USAGE
#  -----
#    bash tests/multi_model_e2e.sh
#
#  CONFIGURABLE DEFAULTS (override via env vars)
#  -----------------------------------------------
#    EMBED_MODEL   Embed model shortcut (default: qwen3-embed:0.6b:8bit)
#    CHAT_MODEL    Chat model shortcut  (default: qwen3.5:2b:4bit)
#    VLM_MODEL     Vision model shortcut (default: qwen3-vl:2b:4bit) — skip via --skip-vlm / DO_VLM=0
#    RERANK_MODEL  Rerank model shortcut (default: qwen3-reranker:0.6b:8bit) — skip via --skip-rerank / DO_RERANK=0
#    MTP_MODEL     MTP model shortcut (default: qwen3.5:4b:mtp:4bit) — skip via --skip-mtp / DO_MTP=0
#    LF_HOST       LMForge API host     (default: http://127.0.0.1:11430)
#    LF_BIN        Path to lmforge bin  (default: ./target/debug/lmforge, else PATH)
#    N_REQUESTS    Requests per burst   (default: 10)
#    SKIP_PULL     Set to 1 to skip pull step (models must already be present)
#    SKIP_START    Set to 1 to skip daemon start (daemon must already be running)
#    SKIP_BUILD    Set to 1 to skip `cargo build` (use installed LF_BIN / PATH)
#    DO_VLM/DO_RERANK/DO_MTP  Default 1 (all suites on). Set 0 to disable.
#    NO_BURST       Set to 1 for low-memory hosts: skip parallel/co-resident
#                   probes (TC-E04/E05) and don't require co-residency in
#                   TC-E01/E07. Every capability is still exercised sequentially.
#
#  FLAGS
#  -----
#    --skip-vlm      Skip VLM probes (TC-E08–E10)
#    --skip-rerank   Skip rerank probe (TC-E11)
#    --skip-mtp      Skip MTP probe (TC-E12)
#    --full          Alias: all suites on (default)
#    --with-vlm / --with-rerank / --with-mtp  Force-enable a suite
#    --no-burst      Low-memory mode — no parallel execution; capabilities are
#                    checked sequentially (models may evict between loads)
#    --burst         Force parallel/co-resident probes on (default)
#
#  EXAMPLE
#  -------
#    EMBED_MODEL=nomic-embed-text:v1.5 \
#    CHAT_MODEL=qwen3.5:2b:4bit \
#    N_REQUESTS=5 \
#    bash tests/multi_model_e2e.sh --full
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
E2E_REPO_ROOT="$REPO_ROOT"
# shellcheck source=../scripts/lib/e2e-api.sh
source "$REPO_ROOT/scripts/lib/e2e-api.sh"

# ─── Configuration ────────────────────────────────────────────────────────────
N="${N_REQUESTS:-10}"
SKIP_PULL="${SKIP_PULL:-0}"
SKIP_START="${SKIP_START:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
DO_VLM="${DO_VLM:-1}"
DO_RERANK="${DO_RERANK:-1}"
DO_MTP="${DO_MTP:-1}"
NO_BURST="${NO_BURST:-0}"
LF_BIN="${LF_BIN:-./target/debug/lmforge}"

while (($#)); do
    case "$1" in
        --full)         DO_VLM=1; DO_RERANK=1; DO_MTP=1 ;;
        --skip-vlm)     DO_VLM=0 ;;
        --skip-rerank)  DO_RERANK=0 ;;
        --skip-mtp)     DO_MTP=0 ;;
        --with-vlm)     DO_VLM=1 ;;
        --with-rerank)  DO_RERANK=1 ;;
        --with-mtp)     DO_MTP=1 ;;
        --no-burst)     NO_BURST=1 ;;
        --burst)        NO_BURST=0 ;;
        -h|--help)      sed -n '2,46p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)              echo "Unknown flag: $1 (try --help)" >&2; exit 1 ;;
    esac
    shift
done

# ─── Colour palette ───────────────────────────────────────────────────────────
GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BLUE='\033[0;34m'; BOLD='\033[1m'; DIM='\033[2m'; NC='\033[0m'

ok()   { echo -e "${GREEN}✓${NC} $*"; }
fail() { echo -e "${RED}✗${NC} $*"; ABORT_REASON="$*"; exit 1; }
info() { echo -e "${CYAN}ℹ${NC} $*"; }
warn() { echo -e "${YELLOW}⚠${NC} $*"; }
sep()  { echo -e "${DIM}────────────────────────────────────────────────────────────────────${NC}"; }
hdr()  { echo -e "${CYAN}────────────────────────────────────────────────────────────────────${NC}"; }

# ─── Result capture (file artifact) ───────────────────────────────────────────
# Mirrors think_bench: results land in tests/bench/results/<ts>__<slug>-mm-e2e/
# (report.md + results.tsv + daemon log tails) so runs are analyzable after the
# terminal is gone. Written incrementally — a mid-suite abort still leaves the
# ledger and provenance on disk.
RESULTS_TS="$(date +%Y%m%d_%H%M%S)"
RESULTS_SLUG="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m | tr '[:upper:]' '[:lower:]')"
RESULTS_DIR="${E2E_RESULTS_DIR:-$REPO_ROOT/tests/bench/results}/${RESULTS_TS}__${RESULTS_SLUG}-mm-e2e"
mkdir -p "$RESULTS_DIR"
ABORT_REASON=""

# ─── Result ledger ────────────────────────────────────────────────────────────
# TC_RESULTS stores lines: "ID|STATUS|DESC|DETAIL"
# STATUS = PASS | FAIL | SKIP
TC_RESULTS=()

record_pass() { TC_RESULTS+=("$1|PASS|$2|$3"); printf '%s\tPASS\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS_DIR/results.tsv"; }
record_fail() { TC_RESULTS+=("$1|FAIL|$2|$3"); printf '%s\tFAIL\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS_DIR/results.tsv"; }
record_skip() { TC_RESULTS+=("$1|SKIP|$2|$3"); printf '%s\tSKIP\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS_DIR/results.tsv"; }

# Render report.md from the ledger + provenance. Called from the EXIT trap so it
# runs on every exit path — pass, hard fail(), or crash.
write_report_file() {
    local ver="" sha=""
    [[ -n "${LF_BIN:-}" ]] && ver="$("$LF_BIN" --version 2>/dev/null | head -1 || true)"
    sha="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    {
        echo "# multi_model_e2e report"
        echo ""
        echo "- when: $RESULTS_TS"
        echo "- machine: **${RESULTS_SLUG}**"
        echo "- build: ${ver:-unknown} | checkout: ${sha}"
        echo "- models: chat=$CHAT_MODEL embed=$EMBED_MODEL vlm=$VLM_MODEL rerank=$RERANK_MODEL mtp=$MTP_MODEL"
        echo "- config: N=$N no_burst=$NO_BURST vlm=$DO_VLM rerank=$DO_RERANK mtp=$DO_MTP chat_max_tokens=${E2E_CHAT_MAX_TOKENS:-128}"
        if [[ -n "$ABORT_REASON" ]]; then
            echo ""
            echo "## ABORTED"
            echo ""
            echo '```'
            echo "$ABORT_REASON"
            echo '```'
        fi
        echo ""
        echo "| Test | Status | Description | Detail |"
        echo "|---|---|---|---|"
        local line id status desc detail
        for line in ${TC_RESULTS[@]+"${TC_RESULTS[@]}"}; do
            IFS='|' read -r id status desc detail <<< "$line"
            echo "| $id | $status | $desc | $detail |"
        done
    } > "$RESULTS_DIR/report.md"
    # Daemon + engine log tails for post-mortem (best-effort). MUST live under
    # logs/ — .gitignore blanket-ignores *.log but whitelists results/**/logs/.
    mkdir -p "$RESULTS_DIR/logs"
    tail -n 400 "$HOME/.lmforge/logs/daemon.err.log" > "$RESULTS_DIR/logs/daemon.err.log" 2>/dev/null || true
    tail -n 100 "$HOME/.lmforge/logs/daemon.out.log" > "$RESULTS_DIR/logs/daemon.out.log" 2>/dev/null || true
    # Engine stderr: head carries build/version + template init lines, tail the
    # recent traffic — capture both.
    local eng
    for eng in "$HOME"/.lmforge/logs/engine-*.stderr.log; do
        [[ -f "$eng" ]] || continue
        {
            head -n 60 "$eng"
            echo "----8<---- (head above / tail below) ----8<----"
            tail -n 200 "$eng"
        } > "$RESULTS_DIR/logs/$(basename "$eng")" 2>/dev/null || true
    done
    echo -e "${DIM}  results captured: $RESULTS_DIR${NC}"
}

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
        # Use pre-increment / explicit form — under `set -e` a bare
        # `(( i++ ))` aborts the script when i was 0, because the post-
        # increment expression evaluates to the OLD value (0) and bash
        # treats arithmetic-zero as exit 1. This previously truncated the
        # sparkline after one row and made the suite "succeed silently"
        # on the trap with no final report.
        i=$(( i + 1 ))
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
        [[ "$status" == "FAIL" ]] && all_pass=false
    done

    echo ""
    hdr
    if $all_pass; then
        echo -e "${BOLD}${GREEN}  ✦  LMForge Multi-Model E2E — Passed (SKIP = unavailable capability)  ✦${NC}"
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
        elif [[ "$status" == "SKIP" ]]; then
            icon="  SKIP  "; colour="${YELLOW}"
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
printf "  ${DIM}%-16s${NC}  vlm=%s rerank=%s mtp=%s\n" "Suites" \
    "$([[ $DO_VLM -eq 1 ]] && echo on || echo off)" \
    "$([[ $DO_RERANK -eq 1 ]] && echo on || echo off)" \
    "$([[ $DO_MTP -eq 1 ]] && echo on || echo off)"
printf "  ${DIM}%-16s${NC}  %s\n" "Execution" \
    "$([[ $NO_BURST -eq 1 ]] && echo 'sequential (--no-burst: no parallel/co-resident probes)' || echo 'parallel (co-resident + concurrent bursts)')"
printf "  ${DIM}%-16s${NC}  chat_max_tokens=%s\n" "Load profile" "${E2E_CHAT_MAX_TOKENS:-128}"
hdr

# Parallel-only measurements; stay "n/a" when --no-burst skips those probes.
concurrent_ms="n/a"
mixed_ms="n/a"

# ─── Trap / cleanup ───────────────────────────────────────────────────────────
# Track which models this test run downloaded (vs found pre-existing).
# Only those will be removed on exit — pre-existing models are never touched.
daemon_pid=""
EMBED_PULLED_BY_TEST=0
CHAT_PULLED_BY_TEST=0
VLM_PULLED_BY_TEST=0
RERANK_PULLED_BY_TEST=0
MTP_PULLED_BY_TEST=0

resolve_lf_bin() { e2e_resolve_bin; }

pull_if_needed() {
    local msg
    msg=$(e2e_pull_if_needed "$1" "$2") || fail "$msg"
    ok "$msg"
}

pull_optional() {
    local model="$1" ref_name="$2" suite_var="$3"
    [[ "${!suite_var}" -eq 1 ]] || return 0
    echo "  Pulling optional: ${model}"
    local msg
    if msg=$(e2e_pull_if_needed "$model" "$ref_name" 2>&1); then
        ok "$msg"
    else
        warn "Optional pull failed for ${model} — skipping ${suite_var} tests"
        printf -v "$suite_var" '%s' "0"
    fi
}

cleanup() {
    echo ""
    write_report_file || true
    info "Cleaning up..."
    if [[ -n "$daemon_pid" ]] && [[ "$SKIP_START" -eq 0 ]]; then
        "$LF_BIN" stop 2>/dev/null || true
        kill "$daemon_pid" 2>/dev/null || true
    fi
    e2e_cleanup_pulled_models \
        "EMBED_PULLED_BY_TEST:$EMBED_MODEL" \
        "CHAT_PULLED_BY_TEST:$CHAT_MODEL" \
        "VLM_PULLED_BY_TEST:$VLM_MODEL" \
        "RERANK_PULLED_BY_TEST:$RERANK_MODEL" \
        "MTP_PULLED_BY_TEST:$MTP_MODEL"
}
trap cleanup EXIT

# ─── Pre-flight: build ────────────────────────────────────────────────────────
if [[ "$SKIP_BUILD" -eq 1 ]]; then
    resolve_lf_bin || fail "SKIP_BUILD=1 but no lmforge binary found (set LF_BIN or install core)"
    ok "Using binary: $LF_BIN"
else
    info "Building lmforge..."
    (cd "$REPO_ROOT" && cargo build) 2>&1 | tail -3
    resolve_lf_bin || fail "build finished but binary not found at $LF_BIN"
    ok "Build complete → $LF_BIN"
fi
sep

# ─── Step 1: Pull models ──────────────────────────────────────────────────────
if [[ "$SKIP_PULL" -ne 1 ]]; then
    info "Step 1 — Pulling models (this may take a while the first time)"

    echo "  Pulling embed model: ${EMBED_MODEL}"
    pull_if_needed "$EMBED_MODEL" EMBED_PULLED_BY_TEST

    echo "  Pulling chat model: ${CHAT_MODEL}"
    pull_if_needed "$CHAT_MODEL" CHAT_PULLED_BY_TEST

    pull_optional "$VLM_MODEL" VLM_PULLED_BY_TEST DO_VLM
    pull_optional "$RERANK_MODEL" RERANK_PULLED_BY_TEST DO_RERANK
    pull_optional "$MTP_MODEL" MTP_PULLED_BY_TEST DO_MTP
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
    if e2e_wait_health 90; then
        echo ""
        ok "Daemon healthy"
        healthy=1
    fi
    [[ "$healthy" -eq 1 ]] || fail "Daemon did not become healthy within 90s"
else
    info "Step 2 — Skipping daemon start (SKIP_START=1), assuming ${LF_HOST} is live"
    e2e_health_ok || fail "Daemon at ${LF_HOST} is not healthy (HTTP non-200)"
    ok "Daemon healthy"
fi

# Build-provenance gate: the daemon answering on the port MUST be the binary we
# just built. A stale installed/service daemon holding the port silently
# invalidates every result (bit us twice already).
bin_sha=$("$LF_BIN" --version 2>/dev/null | sed -n 's/.*(\([^ ]*\) .*/\1/p')
daemon_sha=$(curl -sf "${LF_HOST}/lf/status" 2>/dev/null | jq -r '.daemon_build.sha // "missing"' 2>/dev/null)
if [[ -n "$bin_sha" && "$daemon_sha" != "$bin_sha" ]]; then
    fail "STALE DAEMON: port is served by build '${daemon_sha}' but test binary is '${bin_sha}'. \
Stop the installed/service daemon (lmforge stop; systemctl --user stop lmforge 2>/dev/null; pkill -f 'lmforge start') and re-run."
fi
ok "Daemon build verified: ${daemon_sha}"
sep

# ─── Helpers: thin wrappers over scripts/lib/e2e-api.sh ───────────────────────
lf_embed() { e2e_api_embed "$EMBED_MODEL" "$1"; }
lf_chat()  { e2e_api_chat "$CHAT_MODEL" "$1"; }
lf_status() { e2e_lf_status; }

assert_embed_response() {
    e2e_assert_embed_response "$@" || fail "${E2E_ASSERT_MSG}"
}

assert_chat_response() {
    local min_len="${3:-1}"
    e2e_assert_chat_response "$1" "$2" "$min_len" || fail "${E2E_ASSERT_MSG}"
}

# ─── TC-E01: Cold-start co-load ───────────────────────────────────────────────
echo -e "\n${BOLD}TC-E01${NC}  Cold-start co-load"

timer_start "embed_cold"
resp=$(lf_embed "$E2E_EMBED_COLD" 2>&1) \
    || fail "TC-E01: embed cold-load failed — $(e2e_embed_diag "$EMBED_MODEL" "$E2E_EMBED_COLD")"
embed_cold_ms=$(timer_end "embed_cold")
assert_embed_response "$resp" "TC-E01 embed"
printf "  ${GREEN}✓${NC} Embed model loaded  ${DIM}%sms${NC}\n" "$embed_cold_ms"

timer_start "chat_cold"
resp=$(lf_chat "$E2E_CHAT_COLD" 2>&1) \
    || fail "TC-E01: chat cold-load failed — $(e2e_chat_diag "$CHAT_MODEL" "$E2E_CHAT_COLD")"
chat_cold_ms=$(timer_end "chat_cold")
assert_chat_response "$resp" "TC-E01 chat" 20
printf "  ${GREEN}✓${NC} Chat model loaded   ${DIM}%sms${NC}\n" "$chat_cold_ms"

if [[ "$NO_BURST" -eq 1 ]]; then
    # Low-memory mode: the orchestrator may evict the embed model to make room
    # for chat (sequential residency). Both already loaded and returned valid
    # responses above, which is what we assert here — co-residency is not required.
    printf "  ${DIM}↳ sequential mode: each model loaded & responded (co-residency not required)${NC}\n"
    record_pass "TC-E01" "Sequential load embed+chat" \
        "embed=${embed_cold_ms}ms  chat=${chat_cold_ms}ms"
else
    status_resp=$(lf_status)
    # running_models is a JSON array; find each model by .model_id
    for _model in "$EMBED_MODEL" "$CHAT_MODEL"; do
        found=$(echo "$status_resp" | jq -r --arg m "$_model" \
            '[.running_models[] | select(.model_id == $m)] | length' 2>/dev/null)
        [[ "$found" -gt 0 ]] \
            || fail "TC-E01: model '${_model}' not co-resident in /lf/status running_models (low memory? retry with --no-burst)"
    done

    record_pass "TC-E01" "Cold-start co-load" \
        "embed=${embed_cold_ms}ms  chat=${chat_cold_ms}ms"
fi

# ─── TC-E02: Sequential embed burst ───────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E02${NC}  Sequential embed burst  ${DIM}(${N} requests)${NC}"
embed_latencies=()

for i in $(seq 1 "$N"); do
    timer_start "embed_seq_$i"
    resp=$(lf_embed "$(e2e_burst_embed_text "$i" "$N")" 2>&1) \
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
    resp=$(lf_chat "$(e2e_burst_chat_text "$i" "$N")" 2>&1) \
        || fail "TC-E03: chat request $i failed"
    ms=$(timer_end "chat_seq_$i")
    chat_latencies+=("$ms")
    assert_chat_response "$resp" "TC-E03 req $i" 15
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
if [[ "$NO_BURST" -eq 1 ]]; then
    echo -e "\n${BOLD}TC-E04${NC}  Concurrent embed burst  ${DIM}(skipped — --no-burst)${NC}"
    record_skip "TC-E04" "Concurrent embed (${N}x)" "skipped (--no-burst: low memory)"
else
echo -e "\n${BOLD}TC-E04${NC}  Concurrent embed burst  ${DIM}(${N} parallel requests)${NC}"

tmpdir=$(mktemp -d)
timer_start "embed_concurrent"

pids=()
for i in $(seq 1 "$N"); do
    (
        resp=$(lf_embed "$(e2e_burst_embed_text "$i" "$N")" 2>&1)
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
fi

# ─── TC-E05: Simultaneous embed + chat ────────────────────────────────────────
sep
if [[ "$NO_BURST" -eq 1 ]]; then
    echo -e "\n${BOLD}TC-E05${NC}  Simultaneous embed + chat  ${DIM}(skipped — --no-burst)${NC}"
    record_skip "TC-E05" "Simultaneous embed+chat" "skipped (--no-burst: low memory)"
else
echo -e "\n${BOLD}TC-E05${NC}  Simultaneous embed + chat  ${DIM}(1 of each in parallel)${NC}"

tmpdir=$(mktemp -d)
timer_start "mixed_concurrent"

lf_embed "$E2E_EMBED_MIXED" > "${tmpdir}/embed.json" 2>&1 &
pid_embed=$!
lf_chat  "$E2E_CHAT_MIXED" > "${tmpdir}/chat.json" 2>&1 &
pid_chat=$!

wait "$pid_embed" || fail "TC-E05: simultaneous embed request failed"
wait "$pid_chat"  || fail "TC-E05: simultaneous chat request failed"
mixed_ms=$(timer_end "mixed_concurrent")

assert_embed_response "$(cat "${tmpdir}/embed.json")" "TC-E05 embed"
assert_chat_response  "$(cat "${tmpdir}/chat.json")"  "TC-E05 chat" 20
rm -rf "$tmpdir"

printf "  ${GREEN}✓${NC} Embed + chat completed simultaneously  ${DIM}(wall: %sms)${NC}\n" "$mixed_ms"
record_pass "TC-E05" "Simultaneous embed+chat" "wall=${mixed_ms}ms"
fi

# ─── TC-E06: Cross-endpoint rejection ─────────────────────────────────────────
sep
echo -e "\n${BOLD}TC-E06${NC}  Cross-endpoint capability gate rejection"

code_e_at_chat=$(e2e_http_post_code "/v1/chat/completions" \
    "$(jq -nc --arg m "$EMBED_MODEL" '{model:$m,messages:[{role:"user",content:"hi"}],stream:false}')")
code_c_at_embed=$(e2e_http_post_code "/v1/embeddings" \
    "$(jq -nc --arg m "$CHAT_MODEL" '{model:$m,input:"test"}')")

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

if [[ "$NO_BURST" -eq 1 ]]; then
    # Sequential mode evicts to fit memory, so embed+chat are not both resident.
    # Assert instead that every currently-resident slot reports a healthy status.
    bad=$(lf_status | jq -r '[.running_models[] | select(.status != "ready")] | length' 2>/dev/null || echo "?")
    n_running=$(lf_status | jq -r '.running_models | length' 2>/dev/null || echo 0)
    if [[ "$bad" == "0" ]]; then
        printf "  ${GREEN}✓${NC} ${DIM}%s resident slot(s), all status=ready${NC}\n" "$n_running"
        record_pass "TC-E07" "State consistency (sequential)" "${n_running} ready"
    else
        record_fail "TC-E07" "State consistency (sequential)" "${bad} slot(s) not ready"
        fail "TC-E07: ${bad} resident slot(s) not ready"
    fi
else
state_detail=""
state_ok=true
for _label_model in "embed:${EMBED_MODEL}" "chat:${CHAT_MODEL}"; do
    _label="${_label_model%%:*}"
    _model="${_label_model#*:}"
    if ! e2e_wait_slot_ready "$_model" 90; then
        warn "TC-E07: ${_label} model '${_model}' not status=ready within 90s"
        state_ok=false
    fi
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
fi

# ─── TC-E08..E12: capability suites (graceful skip on unavailable) ───────────
if [[ "$DO_VLM" -eq 1 ]]; then
    sep
    echo -e "\n${BOLD}TC-E08${NC}  VLM text-only (${VLM_MODEL})"
    timer_start "vlm_text"
    if resp=$(e2e_api_vlm_text "$VLM_MODEL" 2>&1); then
        vlm_text_ms=$(timer_end "vlm_text")
        if e2e_assert_chat_response "$resp" "TC-E08" 20; then
            printf "  ${GREEN}✓${NC} VLM text-only  ${DIM}%sms${NC}\n" "$vlm_text_ms"
            record_pass "TC-E08" "VLM text-only" "${vlm_text_ms}ms"
        else
            record_fail "TC-E08" "VLM text-only" "${E2E_ASSERT_MSG}"
            warn "TC-E08: assertion failed"
        fi
    else
        timer_end "vlm_text" >/dev/null
        # Re-issue without curl -f to see the real status/body. A 5xx means the
        # engine took the request and crashed (a defect — FAIL, don't hide it as
        # a missing capability); 4xx/timeout = genuine capability gap (SKIP).
        diag=$(e2e_chat_diag "$VLM_MODEL" "$E2E_VLM_TEXT")
        if [[ "$(e2e_diag_class "$diag")" == fail ]]; then
            warn "TC-E08: engine error — $diag"
            record_fail "TC-E08" "VLM text-only" "$diag"
        else
            warn "TC-E08 skipped: $diag"
            record_skip "TC-E08" "VLM text-only" "$diag"
        fi
    fi

    sep
    echo -e "\n${BOLD}TC-E09${NC}  VLM image_url remote (${E2E_VLM_IMAGE_URL})"
    timer_start "vlm_remote"
    if resp=$(e2e_api_vlm_image_remote "$VLM_MODEL" 2>&1); then
        vlm_remote_ms=$(timer_end "vlm_remote")
        if e2e_assert_chat_response "$resp" "TC-E09" 30; then
            printf "  ${GREEN}✓${NC} VLM image_url remote  ${DIM}%sms${NC}\n" "$vlm_remote_ms"
            record_pass "TC-E09" "VLM image_url (remote)" "${vlm_remote_ms}ms"
        else
            record_fail "TC-E09" "VLM image_url (remote)" "${E2E_ASSERT_MSG}"
            warn "TC-E09: assertion failed"
        fi
    else
        timer_end "vlm_remote" >/dev/null
        diag=$(e2e_vlm_remote_diag "$VLM_MODEL")
        if [[ "$(e2e_diag_class "$diag")" == fail ]]; then
            warn "TC-E09: engine error — $diag"
            record_fail "TC-E09" "VLM image_url (remote)" "$diag"
        else
            warn "TC-E09 skipped: $diag"
            record_skip "TC-E09" "VLM image_url (remote)" "$diag"
        fi
    fi

    sep
    echo -e "\n${BOLD}TC-E10${NC}  VLM image_url base64 (${VLM_MODEL})"
    timer_start "vlm_image"
    if resp=$(e2e_api_vlm_image_base64 "$VLM_MODEL" 2>&1); then
        vlm_image_ms=$(timer_end "vlm_image")
        if e2e_assert_chat_response "$resp" "TC-E10" 15; then
            printf "  ${GREEN}✓${NC} VLM image_url base64  ${DIM}%sms${NC}\n" "$vlm_image_ms"
            record_pass "TC-E10" "VLM image_url (base64)" "${vlm_image_ms}ms"
        else
            record_fail "TC-E10" "VLM image_url (base64)" "${E2E_ASSERT_MSG}"
            warn "TC-E10: assertion failed"
        fi
    else
        timer_end "vlm_image" >/dev/null
        diag=$(e2e_vlm_base64_diag "$VLM_MODEL")
        if [[ "$(e2e_diag_class "$diag")" == fail ]]; then
            warn "TC-E10: engine error — $diag"
            record_fail "TC-E10" "VLM image_url (base64)" "$diag"
        else
            warn "TC-E10 skipped: $diag"
            record_skip "TC-E10" "VLM image_url (base64)" "$diag"
        fi
    fi
fi

if [[ "$DO_RERANK" -eq 1 ]]; then
    sep
    echo -e "\n${BOLD}TC-E11${NC}  Rerank endpoint (${RERANK_MODEL})"
    supports=$(e2e_engine_supports_rerank)
    if [[ "$supports" != "true" ]]; then
        warn "TC-E11: active engine lacks reranking — skipping"
        record_skip "TC-E11" "Rerank endpoint" "engine lacks reranking"
    else
        timer_start "rerank"
        if resp=$(e2e_api_rerank "$RERANK_MODEL" 2>&1); then
            rerank_ms=$(timer_end "rerank")
            if e2e_assert_rerank_response "$resp" "TC-E11"; then
                count=$(echo "$resp" | jq -r '.results | length' 2>/dev/null || echo 0)
                printf "  ${GREEN}✓${NC} Rerank returned ${count} result(s)  ${DIM}%sms${NC}\n" "$rerank_ms"
                record_pass "TC-E11" "Rerank endpoint" "${rerank_ms}ms count=${count}"
            else
                record_fail "TC-E11" "Rerank endpoint" "${E2E_ASSERT_MSG}"
                warn "TC-E11: assertion failed"
            fi
        else
            timer_end "rerank" >/dev/null
            diag=$(e2e_rerank_diag "$RERANK_MODEL")
            if [[ "$(e2e_diag_class "$diag")" == fail ]]; then
                warn "TC-E11: engine error — $diag"
                record_fail "TC-E11" "Rerank endpoint" "$diag"
            else
                warn "TC-E11 skipped: $diag"
                record_skip "TC-E11" "Rerank endpoint" "$diag"
            fi
        fi
    fi
fi

if [[ "$DO_MTP" -eq 1 ]]; then
    sep
    echo -e "\n${BOLD}TC-E12${NC}  MTP speculative (${MTP_MODEL})"
    timer_start "mtp_warm"
    if e2e_api_mtp_warm "$MTP_MODEL" >/dev/null 2>&1; then
        mtp_warm_ms=$(timer_end "mtp_warm")
        sleep 2
        read -r spec samples <<< "$(e2e_mtp_status "$MTP_MODEL")"
        if [[ "$spec" == "mtp" ]] || [[ "${samples:-0}" -ge 1 ]]; then
            printf "  ${GREEN}✓${NC} MTP active  ${DIM}mode=%s samples=%s warm=%sms${NC}\n" "$spec" "$samples" "$mtp_warm_ms"
            record_pass "TC-E12" "MTP speculative" "mode=${spec} samples=${samples}"
        else
            warn "TC-E12: MTP not active (mode=${spec} samples=${samples}) — skipping"
            record_skip "TC-E12" "MTP speculative" "mode=${spec} samples=${samples}"
        fi
    else
        mtp_warm_ms=$(timer_end "mtp_warm")
        warn "TC-E12 skipped: warm chat failed"
        record_skip "TC-E12" "MTP speculative" "warm failed after ${mtp_warm_ms}ms"
    fi
fi

# ─── TC-E13..E15: thinking pipeline (ADR-007) ────────────────────────────────
# Native, Python-free smoke of the reasoning fixes against the chat model.
# Gracefully skipped when the chat model is not thinking-capable.
sep
think_capable=$(e2e_model_thinking_capable "$CHAT_MODEL")

# TC-E14 always applies: think=off MUST yield a non-blank answer (Fix #3c —
# plain-client default must not silently suppress the answer on thinking models).
echo -e "\n${BOLD}TC-E14${NC}  Thinking off → non-blank answer (${CHAT_MODEL})"
timer_start "think_off"
if resp=$(e2e_api_chat_thinking_off "$CHAT_MODEL" "What is the capital of France? Answer in one word." 64 2>&1); then
    think_off_ms=$(timer_end "think_off")
    ans=$(echo "$resp" | jq -r '.choices[0].message.content // ""' 2>/dev/null)
    if [[ -n "${ans// }" ]]; then
        printf "  ${GREEN}✓${NC} answered (%s chars)  ${DIM}%sms${NC}\n" "${#ans}" "$think_off_ms"
        record_pass "TC-E14" "Thinking off non-blank" "${think_off_ms}ms"
    else
        record_fail "TC-E14" "Thinking off non-blank" "blank answer (Fix #3c regression)"
        warn "TC-E14: think=off produced a blank answer"
    fi
else
    timer_end "think_off" >/dev/null
    record_fail "TC-E14" "Thinking off non-blank" "request failed"
    warn "TC-E14: request failed — $(e2e_chat_diag "$CHAT_MODEL" "ping")"
fi

if [[ "$think_capable" != "true" ]]; then
    warn "TC-E13/E15: ${CHAT_MODEL} is not thinking-capable — skipping"
    record_skip "TC-E13" "Thinking on → reasoning_content" "model not thinking-capable"
    record_skip "TC-E15" "Thinking budget → answer" "model not thinking-capable"
else
    # TC-E13: think=true → reasoning_content present AND a non-blank answer.
    echo -e "\n${BOLD}TC-E13${NC}  Thinking on → reasoning_content + answer (${CHAT_MODEL})"
    timer_start "think_on"
    if resp=$(e2e_api_chat_think_on "$CHAT_MODEL" "A bat and ball cost \$1.10. The bat costs \$1 more than the ball. How much is the ball? Think step by step." 512 2>&1); then
        think_on_ms=$(timer_end "think_on")
        reasoning=$(echo "$resp" | jq -r '.choices[0].message.reasoning_content // ""' 2>/dev/null)
        ans=$(echo "$resp" | jq -r '.choices[0].message.content // ""' 2>/dev/null)
        if [[ -n "${reasoning// }" && -n "${ans// }" ]]; then
            printf "  ${GREEN}✓${NC} reasoning=%s chars, answer=%s chars  ${DIM}%sms${NC}\n" "${#reasoning}" "${#ans}" "$think_on_ms"
            record_pass "TC-E13" "Thinking on reasoning+answer" "${think_on_ms}ms r=${#reasoning} a=${#ans}"
        else
            record_fail "TC-E13" "Thinking on reasoning+answer" "reasoning='${#reasoning}' answer='${#ans}' (expected both non-empty)"
            warn "TC-E13: missing reasoning_content or answer"
            # Preserve the raw response for post-mortem analysis.
            echo "$resp" > "$RESULTS_DIR/tc-e13.response.json" 2>/dev/null || true
        fi
    else
        timer_end "think_on" >/dev/null
        record_fail "TC-E13" "Thinking on reasoning+answer" "request failed"
        warn "TC-E13: request failed"
    fi

    # TC-E15: think=true + thinking_budget → orchestrator must still yield an answer
    # (empty-guard / plain-answer fallback must never produce a silent blank — Fix #5b).
    echo -e "\n${BOLD}TC-E15${NC}  Thinking budget → non-blank answer (${CHAT_MODEL})"
    timer_start "think_budget"
    if resp=$(e2e_api_chat_think_budget "$CHAT_MODEL" "Explain why the sky appears blue, briefly." 512 256 2>&1); then
        think_budget_ms=$(timer_end "think_budget")
        ans=$(echo "$resp" | jq -r '.choices[0].message.content // ""' 2>/dev/null)
        err=$(echo "$resp" | jq -r '.error.message // ""' 2>/dev/null)
        if [[ -n "${ans// }" ]]; then
            printf "  ${GREEN}✓${NC} answered (%s chars)  ${DIM}%sms${NC}\n" "${#ans}" "$think_budget_ms"
            record_pass "TC-E15" "Thinking budget answer" "${think_budget_ms}ms a=${#ans}"
        elif [[ -n "${err// }" ]]; then
            # A structured error is the acceptable Fix #5b outcome when the model
            # genuinely produces nothing — far better than a silent blank.
            warn "TC-E15: orchestrator returned structured error (acceptable): $err"
            record_skip "TC-E15" "Thinking budget answer" "structured error (no model output)"
        else
            record_fail "TC-E15" "Thinking budget answer" "blank answer with no error (Fix #5b regression)"
            warn "TC-E15: budget path produced a silent blank"
        fi
    else
        timer_end "think_budget" >/dev/null
        record_fail "TC-E15" "Thinking budget answer" "request failed"
        warn "TC-E15: request failed"
    fi
fi

# ─── Final report ─────────────────────────────────────────────────────────────
print_report
