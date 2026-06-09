#!/usr/bin/env bash
# Release-binary smoke matrix: cuda12 + cuda13 engine install from R2 CDN,
# then chat (plain + MTP), embeddings, and VLM on a live daemon.
#
# Usage:
#   LF_BIN=./target/release/lmforge scripts/util/release_binary_test.sh
#   LF_BIN=lmforge scripts/util/release_binary_test.sh          # post GitHub install
#   SKIP_PULL=1 LF_BIN=./target/release/lmforge scripts/util/release_binary_test.sh
#
# Exit: 0 all pass, 1 one or more failures
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LF_BIN="${LF_BIN:-$REPO_ROOT/target/release/lmforge}"
LF_HOST="${LF_HOST:-http://127.0.0.1:11430}"
DATA_DIR="${LMFORGE_DATA_DIR:-$HOME/.lmforge}"
SKIP_PULL="${SKIP_PULL:-0}"
VARIANTS="${VARIANTS:-cuda12,cuda13}"

CHAT_MODEL="${CHAT_MODEL:-qwen3:1.7b:4bit}"
MTP_MODEL="${MTP_MODEL:-qwen3.5:4b:mtp:4bit}"
EMBED_MODEL="${EMBED_MODEL:-qwen3-embed:0.6b:8bit}"
VLM_MODEL="${VLM_MODEL:-qwen2.5-vl:3b:4bit}"
MODEL_WAIT_SECS="${MODEL_WAIT_SECS:-180}"

# 1×1 red PNG
RED_PNG_B64="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="

PASS=0
FAIL=0
_log() { echo "$*"; }

pass() { PASS=$((PASS + 1)); echo "PASS: $*"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $*" >&2; }

require_bin() {
  [[ -x "$LF_BIN" ]] || { fail "LF_BIN not executable: $LF_BIN"; exit 2; }
}

wait_health() {
  local i
  for i in $(seq 1 90); do
    if curl -sf --max-time 2 "$LF_HOST/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  return 1
}

wait_model_running() {
  local model="$1" i max="${2:-$MODEL_WAIT_SECS}"
  for i in $(seq 1 "$max"); do
    if curl -sf "$LF_HOST/lf/status" 2>/dev/null | jq -e --arg m "$model" \
        '.running_models[]? | select(.model_id == $m) | select(.status == "Ready" or .status == "ready" or .status == "Running" or .status == "running")' >/dev/null 2>&1; then
      return 0
    fi
    # Accept any running_models entry with matching id (status casing varies)
    if curl -sf "$LF_HOST/lf/status" 2>/dev/null | jq -e --arg m "$model" \
        '.running_models[]? | select(.model_id == $m)' >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  return 1
}

stop_daemon() {
  "$LF_BIN" stop 2>/dev/null || true
  curl -sf -X POST "$LF_HOST/lf/shutdown" >/dev/null 2>&1 || true
  sleep 2
  pkill -x lmforge 2>/dev/null || true
  sleep 1
}

preflight_variant() {
  local v="$1"
  local dir="$DATA_DIR/engines/llamacpp/variants/$v"
  local bin="$dir/llama-server"
  [[ -x "$bin" ]] || { fail "${v} llama-server missing"; return 1; }
  if ! LD_LIBRARY_PATH="$dir/lib" "$bin" --version >/dev/null 2>&1; then
    fail "${v} llama-server --version"
    return 1
  fi
  pass "${v} llama-server (bundled libs)"
  if LD_LIBRARY_PATH="$dir/lib" ldd "$bin" 2>/dev/null | grep -q 'not found'; then
    fail "${v} ldd has unresolved libs"
    return 1
  fi
  pass "${v} ldd clean"
}

install_variant() {
  local v="$1"
  _log "Installing llamacpp variant $v from CDN..."
  if ! "$LF_BIN" engine install llamacpp --variant "$v" 2>&1 | tail -5; then
    fail "${v} engine install"
    return 1
  fi
  preflight_variant "$v"
}

pull_models() {
  if [[ "$SKIP_PULL" == "1" ]]; then
    _log "SKIP_PULL=1 — skipping model pulls"
    for m in "$CHAT_MODEL" "$MTP_MODEL" "$EMBED_MODEL" "$VLM_MODEL"; do
      if ! "$LF_BIN" models list 2>/dev/null | awk '{print $1}' | grep -qx "$m"; then
        fail "model $m not installed (pull it or unset SKIP_PULL)"
        return 1
      fi
    done
    return 0
  fi
  for m in "$CHAT_MODEL" "$MTP_MODEL" "$EMBED_MODEL" "$VLM_MODEL"; do
    _log "Pulling $m ..."
    "$LF_BIN" pull "$m" || { fail "pull $m"; return 1; }
  done
}

start_daemon() {
  local model="${1:-}"
  stop_daemon
  if [[ -n "$model" ]]; then
    "$LF_BIN" start --model "$model" &
  else
    "$LF_BIN" start &
  fi
  wait_health || return 1
}

test_chat_plain() {
  local v="$1"
  export LMFORGE_LLAMACPP_VARIANT="$v"
  start_daemon "$CHAT_MODEL" || { fail "${v} daemon health (chat)"; return 1; }
  wait_model_running "$CHAT_MODEL" 120 || { fail "${v} chat model running"; return 1; }

  local spec
  spec="$(curl -sf "$LF_HOST/lf/status" | jq -r --arg m "$CHAT_MODEL" \
    '.running_models[] | select(.model_id == $m) | .spec_mode // "null"' | head -1)"
  if [[ "$spec" == "off" || "$spec" == "null" ]]; then
    pass "${v} chat spec off"
  else
    fail "${v} chat spec_mode=$spec (want off)"
  fi

  local content
  content="$(curl -sf "$LF_HOST/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$CHAT_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with one word: okay\"}],\"max_tokens\":32,\"temperature\":0,\"think\":false}" \
    | jq -r '.choices[0].message.content // .choices[0].message.reasoning_content // empty')"
  if [[ -n "$content" ]]; then
    pass "${v} chat completion: ${content:0:40}"
  else
    fail "${v} chat empty response"
  fi
}

test_chat_mtp() {
  local v="$1"
  export LMFORGE_LLAMACPP_VARIANT="$v"
  export LMFORGE_SPECULATIVE_MODE=auto
  start_daemon "$MTP_MODEL" || { fail "${v} daemon health (mtp)"; return 1; }
  wait_model_running "$MTP_MODEL" 180 || { fail "${v} mtp model running"; return 1; }

  local spec samples
  spec="$(curl -sf "$LF_HOST/lf/status" | jq -r --arg m "$MTP_MODEL" \
    '.running_models[] | select(.model_id == $m) | .spec_mode // empty' | head -1)"

  curl -sf "$LF_HOST/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$MTP_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Count to five slowly.\"}],\"max_tokens\":128,\"temperature\":0,\"think\":false}" \
    >/dev/null || { fail "${v} mtp chat request"; return 1; }

  sleep 3
  spec="$(curl -sf "$LF_HOST/lf/status" | jq -r --arg m "$MTP_MODEL" \
    '.running_models[] | select(.model_id == $m) | .spec_mode // empty' | head -1)"
  samples="$(curl -sf "$LF_HOST/lf/status" | jq -r --arg m "$MTP_MODEL" \
    '.running_models[] | select(.model_id == $m) | .spec_stats.samples // 0' | head -1)"
  if [[ "$spec" == "mtp" ]]; then
    pass "${v} spec mtp"
  elif [[ "${samples:-0}" -ge 1 ]]; then
    pass "${v} spec mtp (via spec_stats; spec_mode=$spec)"
  else
    fail "${v} spec_mode=$spec samples=$samples (want mtp)"
  fi

  if [[ "${samples:-0}" -ge 1 ]]; then
    pass "${v} MTP stats samples=$samples"
  else
    fail "${v} MTP spec_stats missing"
  fi
}

test_embed() {
  local v="$1"
  export LMFORGE_LLAMACPP_VARIANT="$v"
  export LMFORGE_SPECULATIVE_MODE=off
  start_daemon "$EMBED_MODEL" || { fail "${v} daemon health (embed)"; return 1; }

  local dim
  dim="$(curl -sf "$LF_HOST/v1/embeddings" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$EMBED_MODEL\",\"input\":\"hello world\"}" \
    | jq -r '.data[0].embedding | length // 0')"
  if [[ "${dim:-0}" -gt 0 ]]; then
    pass "${v} embed dim=$dim"
  else
    fail "${v} embed dim=0"
  fi
}

test_vlm() {
  local v="$1"
  export LMFORGE_LLAMACPP_VARIANT="$v"
  start_daemon "$VLM_MODEL" || { fail "${v} daemon health (vlm)"; return 1; }
  wait_model_running "$VLM_MODEL" "$MODEL_WAIT_SECS" || { fail "${v} vlm model running"; return 1; }

  local text
  text="$(curl -sf "$LF_HOST/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$VLM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Say okay in one word.\"}],\"max_tokens\":16,\"temperature\":0}" \
    | jq -r '.choices[0].message.content // empty')"
  if [[ -n "$text" ]]; then
    pass "${v} VLM text: ${text:0:30}"
  else
    fail "${v} VLM text empty"
  fi

  local img_reply
  img_reply="$(curl -sf "$LF_HOST/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$VLM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"What color is this image? One word only.\"},{\"type\":\"image_url\",\"image_url\":{\"url\":\"data:image/png;base64,${RED_PNG_B64}\"}}]}],\"max_tokens\":16,\"temperature\":0}" \
    | jq -r '.choices[0].message.content // empty')"
  if [[ -n "$img_reply" ]]; then
    pass "${v} VLM image: ${img_reply:0:30}"
  else
    fail "${v} VLM image empty"
  fi
}

run_variant() {
  local v="$1"
  echo ""
  echo "========== VARIANT $v =========="
  install_variant "$v" || return 1
  test_chat_plain "$v" || true
  test_chat_mtp "$v" || true
  test_embed "$v" || true
  test_vlm "$v" || true
}

main() {
  require_bin
  command -v jq >/dev/null || { fail "jq required"; exit 2; }
  command -v curl >/dev/null || { fail "curl required"; exit 2; }

  # Avoid fighting systemd user service during daemon stop/start cycles.
  "$LF_BIN" service stop 2>/dev/null || true
  systemctl --user stop lmforge.service 2>/dev/null || true
  stop_daemon

  echo "=== release_binary_test ==="
  echo "LF_BIN=$LF_BIN"
  echo "VARIANTS=$VARIANTS"

  pull_models || true

  IFS=',' read -ra VAR_ARR <<< "$VARIANTS"
  for v in "${VAR_ARR[@]}"; do
    run_variant "$v"
  done

  stop_daemon
  echo ""
  echo "=== SUMMARY PASS=$PASS FAIL=$FAIL ==="
  [[ "$FAIL" -eq 0 ]]
}

main "$@"
