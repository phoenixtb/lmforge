#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# LMForge throughput + latency bench.
#
# Fires N requests at /v1/chat/completions with C concurrency and reports:
#   - throughput (req/s)
#   - latency p50 / p90 / p95 / p99 / max (seconds)
#   - tokens/sec (estimated from response token counts in `usage`)
#   - error rate
#
# Assumptions:
#   - Daemon is running at http://127.0.0.1:11430
#   - The model is already pulled (run `lmforge pull <model>` first, or
#     use `dev_test.sh --with-e2e --e2e-model <model>` once).
#
# Usage:
#   ./dev_bench.sh                          n=20 c=4 model=qwen3:1.7b:4bit
#   ./dev_bench.sh -n 100 -c 8 -m qwen3:8b:4bit
#   ./dev_bench.sh --stream                 use streaming, measure TTFT instead of total
#   ./dev_bench.sh --prompt "long..."       override the default prompt
#   ./dev_bench.sh --max-tokens 256
#   ./dev_bench.sh --csv out.csv            also dump per-request stats to CSV
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

N=20
C=4
MODEL="qwen3:1.7b:4bit"
PROMPT="Write a one-paragraph summary of the difference between a CPU and a GPU."
MAX_TOK=128
STREAM=0
CSV=""

while (($#)); do
    case "$1" in
        -n|--requests)    N="$2"; shift ;;
        -c|--concurrency) C="$2"; shift ;;
        -m|--model)       MODEL="$2"; shift ;;
        --prompt)         PROMPT="$2"; shift ;;
        --max-tokens)     MAX_TOK="$2"; shift ;;
        --stream)         STREAM=1 ;;
        --csv)            CSV="$2"; shift ;;
        -h|--help)        sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)                echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'

BASE="http://127.0.0.1:11430"
curl -sf --max-time 2 "$BASE/health" >/dev/null || {
    echo -e "${RED}  ✗${NC} no daemon on :11430 — start it first"
    exit 1
}

# Verify model is loadable (warmup) — first request always pays load cost,
# so we issue one synchronous warmup request and exclude it from stats.
echo -e "${BOLD}Warmup${NC} (loads $MODEL — first request, excluded from stats)..."
WARMUP_BODY=$(printf '{"model":%s,"messages":[{"role":"user","content":"hi"}],"max_tokens":4,"temperature":0}' "$(jq -Rs . <<<"$MODEL")")
WARMUP_T0=$(date +%s.%N)
if ! curl -sf --max-time 300 "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' -d "$WARMUP_BODY" >/dev/null; then
    echo -e "${RED}  ✗${NC} warmup request failed — is the model pulled? Try: lmforge pull $MODEL"
    exit 1
fi
WARMUP_DT=$(awk -v t0="$WARMUP_T0" 'BEGIN{printf "%.2f", systime()+0 - t0}')
echo -e "  ${GREEN}✓${NC} warmup done in ${WARMUP_DT}s"

# Build request body
BODY=$(jq -nc \
    --arg m "$MODEL" --arg p "$PROMPT" \
    --argjson mt "$MAX_TOK" --argjson s "$STREAM" \
    '{model: $m, messages: [{role:"user", content:$p}], max_tokens: $mt, temperature: 0, stream: ($s == 1)}')

# Each request writes one line to RESULTS_FILE:
#   <duration_s> <status_code> <prompt_toks> <completion_toks>
# For streaming, completion_toks is parsed from the last data: chunk's usage.
RESULTS_FILE=$(mktemp /tmp/lmforge-bench-XXXX.tsv)
trap 'rm -f "$RESULTS_FILE"' EXIT

echo -e "\n${BOLD}Bench${NC} n=$N c=$C model=$MODEL stream=$STREAM max_tokens=$MAX_TOK"
echo "  Pushing ${N} requests with ${C}-way concurrency..."

run_one() {
    local i="$1"
    local out
    if (( STREAM )); then
        # Stream mode: capture full SSE body to extract usage from final chunk
        out=$(curl -sN --max-time 600 -w "\n%{http_code} %{time_total}\n" \
              "$BASE/v1/chat/completions" -H 'Content-Type: application/json' \
              -d "$BODY")
        local code=$(echo "$out" | tail -1 | awk '{print $1}')
        local dur=$( echo "$out" | tail -1 | awk '{print $2}')
        # Find the usage object across SSE chunks (usually in [DONE] frame or near it)
        local usage=$(echo "$out" | grep -oE '"usage":\s*\{[^}]*\}' | tail -1)
        local pt=$(echo "$usage" | grep -oE '"prompt_tokens":\s*[0-9]+'      | grep -oE '[0-9]+' | tail -1)
        local ct=$(echo "$usage" | grep -oE '"completion_tokens":\s*[0-9]+'  | grep -oE '[0-9]+' | tail -1)
        printf "%s\t%s\t%s\t%s\n" "$dur" "$code" "${pt:-0}" "${ct:-0}"
    else
        out=$(curl -s --max-time 600 -o /tmp/lmforge-bench-resp-$$ -w "%{http_code} %{time_total}" \
              "$BASE/v1/chat/completions" -H 'Content-Type: application/json' -d "$BODY")
        local code=$(echo "$out" | awk '{print $1}')
        local dur=$( echo "$out" | awk '{print $2}')
        local pt=$(jq -r '.usage.prompt_tokens // 0'     </tmp/lmforge-bench-resp-$$ 2>/dev/null)
        local ct=$(jq -r '.usage.completion_tokens // 0' </tmp/lmforge-bench-resp-$$ 2>/dev/null)
        rm -f /tmp/lmforge-bench-resp-$$
        printf "%s\t%s\t%s\t%s\n" "$dur" "$code" "${pt:-0}" "${ct:-0}"
    fi
}
export -f run_one
export BASE BODY STREAM

# xargs runs N workers, C in flight. Progress dots every 5 requests.
T0=$(date +%s.%N)
seq 1 "$N" | xargs -P "$C" -I {} bash -c 'run_one {}' \
    | tee "$RESULTS_FILE" \
    | awk 'BEGIN{c=0}{c++; if(c%5==0) printf "."; fflush()} END{print ""}'
TTOTAL=$(awk -v t0="$T0" 'BEGIN{printf "%.3f", systime()+0 - t0}')

# ── Aggregate ────────────────────────────────────────────────────────────────
# duration_s  status_code  prompt_toks  completion_toks
TOTAL=$(wc -l < "$RESULTS_FILE")
OK=$(awk '$2 == 200 {c++} END{print c+0}'      "$RESULTS_FILE")
ERR=$(awk '$2 != 200 {c++} END{print c+0}'     "$RESULTS_FILE")
SUM_CT=$(awk '$2 == 200 {s+=$4} END{print s+0}' "$RESULTS_FILE")
SUM_PT=$(awk '$2 == 200 {s+=$3} END{print s+0}' "$RESULTS_FILE")

# Percentiles via sort + awk
percentile() {
    local p="$1"
    awk -v p="$p" '$2 == 200 {print $1}' "$RESULTS_FILE" | sort -n | awk -v p="$p" '
        BEGIN { c = 0 }
        { v[c++] = $1 }
        END {
            if (c == 0) { print "n/a"; exit }
            i = int((c - 1) * p)
            printf "%.3f", v[i]
        }'
}

P50=$(percentile 0.50)
P90=$(percentile 0.90)
P95=$(percentile 0.95)
P99=$(percentile 0.99)
PMAX=$(awk '$2 == 200 {print $1}' "$RESULTS_FILE" | sort -n | tail -1)
PMIN=$(awk '$2 == 200 {print $1}' "$RESULTS_FILE" | sort -n | head -1)

# Throughput + tokens/sec
RPS=$(awk -v n="$OK" -v t="$TTOTAL" 'BEGIN{ if(t>0) printf "%.2f", n/t; else print "n/a" }')
TPS=$(awk -v ct="$SUM_CT" -v t="$TTOTAL" 'BEGIN{ if(t>0) printf "%.1f", ct/t; else print "n/a" }')

# Per-request avg tokens (sanity check that model is actually generating)
AVG_CT=$(awk -v ct="$SUM_CT" -v ok="$OK" 'BEGIN{ if(ok>0) printf "%.1f", ct/ok; else print "n/a" }')

echo ""
echo -e "${BOLD}────────────────────────────────────────${NC}"
echo -e "${BOLD}Results${NC}  (warmup excluded; ${TTOTAL}s wall)"
printf "  %-22s %s\n" "model"        "$MODEL"
printf "  %-22s %s / %s succeeded\n" "requests"     "$OK" "$TOTAL"
printf "  %-22s %s\n" "errors"       "${ERR}"
printf "  %-22s ${GREEN}%s${NC} req/s\n" "throughput" "$RPS"
printf "  %-22s ${GREEN}%s${NC} tok/s  (avg %s tokens/resp, %s prompt tokens total)\n" "completion rate" "$TPS" "$AVG_CT" "$SUM_PT"
echo "  ──────────────────────────"
printf "  %-22s %ss\n"  "latency  min"   "$PMIN"
printf "  %-22s %ss\n"  "latency  p50"   "$P50"
printf "  %-22s %ss\n"  "latency  p90"   "$P90"
printf "  %-22s %ss\n"  "latency  p95"   "$P95"
printf "  %-22s ${YELLOW}%s${NC}s\n" "latency  p99" "$P99"
printf "  %-22s ${RED}%s${NC}s\n"   "latency  max"   "$PMAX"

if [[ -n "$CSV" ]]; then
    {
        echo "request,duration_s,status_code,prompt_tokens,completion_tokens"
        awk 'BEGIN{OFS=","; n=0} {n++; print n,$1,$2,$3,$4}' "$RESULTS_FILE"
    } > "$CSV"
    echo -e "\n  ${GREEN}✓${NC} per-request CSV → $CSV"
fi

# Pull live Prometheus metrics too (more authoritative than client-side timers)
echo -e "\n${BOLD}Live /metrics snapshot:${NC}"
curl -s "$BASE/metrics" | grep -E "^lmforge_(requests|ttft|tokens|active_models)" | head -10 | sed 's/^/  /'

(( ERR == 0 )) || exit 1
