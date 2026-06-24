# Shared LMForge live-API helpers for E2E scripts — source only.
[[ -n "${_LMFORGE_E2E_API_LOADED:-}" ]] && return 0
_LMFORGE_E2E_API_LOADED=1

_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=e2e-defaults.sh
source "$_LIB_DIR/e2e-defaults.sh"

e2e_apply_model_defaults

# ── health / binary ───────────────────────────────────────────────────────────

e2e_health_ok() {
    curl -sf --max-time "${E2E_HEALTH_TIMEOUT:-3}" "${LF_HOST}/health" >/dev/null 2>&1
}

e2e_wait_health() {
    local max="${1:-90}" i
    for (( i = 1; i <= max; i++ )); do
        e2e_health_ok && return 0
        sleep 1
    done
    return 1
}

e2e_resolve_bin() {
    local repo_root="${E2E_REPO_ROOT:-.}"
    if [[ -n "${LF_BIN:-}" && -x "$LF_BIN" ]]; then
        return 0
    fi
    local alt
    for alt in \
        "$repo_root/target/debug/lmforge" \
        "$repo_root/target/release/lmforge" \
        "$HOME/.local/bin/lmforge" \
        "$HOME/.cargo/bin/lmforge"; do
        if [[ -x "$alt" ]]; then
            LF_BIN="$alt"
            return 0
        fi
    done
    if command -v lmforge >/dev/null 2>&1; then
        LF_BIN="$(command -v lmforge)"
        return 0
    fi
    return 1
}

# ── models ────────────────────────────────────────────────────────────────────

e2e_model_installed() {
    local id="$1"
    curl -sf --max-time 5 "${LF_HOST}/lf/model/list" \
        | jq -e --arg id "$id" '.models[] | select(.id == $id)' >/dev/null 2>&1
}

# e2e_pull_if_needed MODEL REF_VAR_NAME  → sets ref to 1 if newly downloaded.
#
# `lmforge pull` prints a native indicatif progress bar to STDERR and the status
# lines ("already installed", …) to STDOUT. We capture stdout (for the
# "already installed" probe + caller's message) but route stderr to the
# controlling terminal so the real bar renders live — instead of being swallowed
# by command substitution. No tty (CI) → fold stderr into the captured temp.
e2e_pull_if_needed() {
    local model="$1" ref_name="$2" tmp rc
    tmp="$(mktemp)"
    if { true >/dev/tty; } 2>/dev/null; then
        if "${LF_BIN:?}" pull "$model" >"$tmp" 2>/dev/tty; then rc=0; else rc=$?; fi
    else
        if "${LF_BIN:?}" pull "$model" >"$tmp" 2>&1; then rc=0; else rc=$?; fi
    fi
    if [[ $rc -ne 0 ]]; then
        cat "$tmp"; rm -f "$tmp"; return 1
    fi
    if grep -q "already installed" "$tmp"; then
        echo "$model already present"
    else
        printf -v "$ref_name" '%s' "1"
        echo "$model downloaded"
    fi
    rm -f "$tmp"
}

e2e_wait_model_ready() {
    local id="$1" max="${2:-180}" i
    for (( i = 1; i <= max; i++ )); do
        if curl -sf "${LF_HOST}/lf/status" 2>/dev/null | jq -e --arg m "$id" \
            '.running_models[]? | select(.model_id == $m)' >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    return 1
}

# ── API calls (stdout = JSON body) ───────────────────────────────────────────

e2e_lf_status() { curl -sf "${LF_HOST}/lf/status"; }

e2e_engine_supports_rerank() {
    curl -sf "${LF_HOST}/lf/engines" 2>/dev/null | jq -r \
        '.engines[] | select(.active == true) | .supports_reranking' 2>/dev/null | head -1
}

e2e_api_embed() {
    local model="${1:-$EMBED_MODEL}" text="$2"
    curl -sf --max-time "${3:-180}" -X POST "${LF_HOST}/v1/embeddings" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc --arg m "$model" --arg t "$text" '{model:$m,input:$t}')"
}

e2e_api_embed_batch() {
    local model="${1:-$EMBED_MODEL}"; shift
    curl -sf --max-time "${E2E_EMBED_TIMEOUT:-90}" -X POST "${LF_HOST}/v1/embeddings" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc --arg m "$model" --argjson a "$(printf '%s\n' "$@" | jq -R . | jq -s .)" '{model:$m,input:$a}')"
}

e2e_api_chat() {
    local model="${1:-$CHAT_MODEL}" text="$2" max_tokens="${3:-${E2E_CHAT_MAX_TOKENS:-128}}"
    local payload
    if [[ "$model" == qwen3* ]]; then
        payload=$(jq -nc --arg m "$model" --arg t "$text" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:$t}],stream:false,max_tokens:$n,temperature:0,chat_template_kwargs:{enable_thinking:false}}')
    else
        payload=$(jq -nc --arg m "$model" --arg t "$text" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:$t}],stream:false,max_tokens:$n,temperature:0}')
    fi
    curl -sf --max-time "${E2E_CHAT_TIMEOUT:-180}" -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" -d "$payload"
}

e2e_api_chat_thinking_off() {
    local model="${1:-$CHAT_MODEL}" text="$2" max_tokens="${3:-64}"
    curl -sf --max-time "${E2E_CHAT_TIMEOUT:-120}" -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc \
            --arg m "$model" --arg t "$text" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:$t}],stream:false,max_tokens:$n,temperature:0,chat_template_kwargs:{enable_thinking:false}}')"
}

e2e_api_chat_stream() {
    local model="${1:-$CHAT_MODEL}" text="$2" max_tokens="${3:-32}"
    curl -sN --max-time "${E2E_CHAT_TIMEOUT:-120}" -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc \
            --arg m "$model" --arg t "$text" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:$t}],stream:true,max_tokens:$n,temperature:0}')"
}

e2e_api_vlm_text() {
    local model="${1:-$VLM_MODEL}" text="${2:-$E2E_VLM_TEXT}" max_tokens="${3:-${E2E_VLM_TEXT_MAX_TOKENS:-128}}"
    e2e_api_chat "$model" "$text" "$max_tokens"
}

e2e_api_vlm_image_remote() {
    local model="${1:-$VLM_MODEL}" url="${2:-$E2E_VLM_IMAGE_URL}" max_tokens="${3:-${E2E_VLM_IMAGE_MAX_TOKENS:-192}}"
    curl -sf --max-time "${E2E_VLM_TIMEOUT:-240}" -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc \
            --arg m "$model" --arg u "$url" --arg t "$E2E_VLM_REMOTE_PROMPT" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:[
                {type:"text",text:$t},
                {type:"image_url",image_url:{url:$u}}
            ]}],max_tokens:$n,temperature:0}')"
}

e2e_api_vlm_image_base64() {
    local model="${1:-$VLM_MODEL}" b64="${2:-$E2E_RED_PNG_B64}" max_tokens="${3:-${E2E_VLM_IMAGE_MAX_TOKENS:-192}}"
    curl -sf --max-time "${E2E_VLM_TIMEOUT:-180}" -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc \
            --arg m "$model" --arg b "$b64" --arg t "$E2E_VLM_BASE64_PROMPT" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:[
                {type:"text",text:$t},
                {type:"image_url",image_url:{url:("data:image/png;base64," + $b)}}
            ]}],max_tokens:$n,temperature:0}')"
}

e2e_api_rerank() {
    local model="${1:-$RERANK_MODEL}"
    curl -sf --max-time "${E2E_RERANK_TIMEOUT:-90}" -X POST "${LF_HOST}/v1/rerank" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc \
            --arg m "$model" --arg q "$E2E_RERANK_QUERY" \
            --argjson docs "$(e2e_rerank_documents_json)" \
            '{model:$m,query:$q,documents:$docs,top_n:3}')"
}

e2e_api_mtp_warm() {
    local model="${1:-$MTP_MODEL}" max_tokens="${2:-${E2E_MTP_MAX_TOKENS:-256}}"
    LMFORGE_SPECULATIVE_MODE=auto \
    curl -sf --max-time "${E2E_CHAT_TIMEOUT:-180}" -X POST "${LF_HOST}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "$(jq -nc \
            --arg m "$model" --arg t "$E2E_MTP_WARM" --argjson n "$max_tokens" \
            '{model:$m,messages:[{role:"user",content:$t}],max_tokens:$n,temperature:0,think:false,chat_template_kwargs:{enable_thinking:false}}')" \
        2>&1
}

e2e_mtp_status() {
    local model="${1:-$MTP_MODEL}"
    e2e_lf_status | jq -r --arg m "$model" \
        '.running_models[]? | select(.model_id == $m) | "\(.spec_mode // "off") \(.spec_stats.samples // 0)"' \
        2>/dev/null | head -1
}

e2e_http_post_code() {
    local path="$1" body="$2"
    curl -s -o /dev/null -w "%{http_code}" -X POST "${LF_HOST}${path}" \
        -H "Content-Type: application/json" -d "$body"
}

# ── Failure diagnostics ──────────────────────────────────────────────────────
# Re-issue a request WITHOUT curl -f so the error body is visible. The normal
# API helpers use -f, which discards the body on HTTP 4xx/5xx — that is why a
# failed embed/chat surfaced as an empty string. Use only on the failure path.
e2e_api_post_diag() {
    local path="$1" payload="$2" body code
    body="$(mktemp)"
    code=$(curl -s -o "$body" -w "%{http_code}" --max-time "${E2E_DIAG_TIMEOUT:-30}" \
        -X POST "${LF_HOST}${path}" -H "Content-Type: application/json" \
        -d "$payload" 2>/dev/null)
    printf 'HTTP %s — %s' "$code" "$(tr -d '\r' < "$body" | tr '\n' ' ' | cut -c1-300)"
    rm -f "$body"
}

e2e_embed_diag() {
    e2e_api_post_diag "/v1/embeddings" \
        "$(jq -nc --arg m "${1:-$EMBED_MODEL}" --arg t "${2:-probe}" '{model:$m,input:$t}')"
}

e2e_chat_diag() {
    e2e_api_post_diag "/v1/chat/completions" \
        "$(jq -nc --arg m "${1:-$CHAT_MODEL}" --arg t "${2:-probe}" \
            '{model:$m,messages:[{role:"user",content:$t}],stream:false,max_tokens:16}')"
}

# ── assertions (return 0 ok / 1 fail; set E2E_ASSERT_MSG) ───────────────────

e2e_assert_embed_response() {
    local resp="$1" label="${2:-embed}"
    local dims
    dims=$(echo "$resp" | jq -r '.data[0].embedding | length' 2>/dev/null) || {
        E2E_ASSERT_MSG="${label}: invalid JSON — ${resp:0:200}"
        return 1
    }
    if [[ "$dims" =~ ^[0-9]+$ ]] && [[ "$dims" -gt 0 ]]; then
        return 0
    fi
    E2E_ASSERT_MSG="${label}: empty embedding (dims=${dims})"
    return 1
}

e2e_assert_chat_response() {
    local resp="$1" label="${2:-chat}" min_len="${3:-1}"
    local content
    content=$(echo "$resp" | jq -r '(.choices[0].message.content // "") + (.choices[0].message.reasoning_content // "")' 2>/dev/null) \
        || { E2E_ASSERT_MSG="${label}: invalid JSON — ${resp:0:200}"; return 1; }
    if [[ -n "$content" && ${#content} -ge $min_len ]]; then
        return 0
    fi
    E2E_ASSERT_MSG="${label}: empty or short content"
    return 1
}

e2e_assert_rerank_response() {
    local resp="$1" label="${2:-rerank}"
    local count
    count=$(echo "$resp" | jq -r '.results | length' 2>/dev/null || echo 0)
    if [[ "$count" -ge 1 ]]; then
        return 0
    fi
    E2E_ASSERT_MSG="${label}: no results — ${resp:0:200}"
    return 1
}

# Remove models flagged as pulled-by-test (associative via namerefs).
e2e_cleanup_pulled_models() {
    local bin="${LF_BIN:?}"
    local _model _flag
    for _pair in "$@"; do
        _flag="${_pair%%:*}"
        _model="${_pair#*:}"
        if [[ "${!_flag:-0}" -eq 1 ]]; then
            echo "  removing $_model (downloaded this run)"
            "$bin" models remove "$_model" 2>/dev/null || true
        fi
    done
}
