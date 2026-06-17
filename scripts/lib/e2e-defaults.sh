# Default model shortcuts and probes for E2E / release tests — source only.
[[ -n "${_LMFORGE_E2E_DEFAULTS_LOADED:-}" ]] && return 0
_LMFORGE_E2E_DEFAULTS_LOADED=1

E2E_CHAT_MODEL="${E2E_CHAT_MODEL:-qwen3.5:2b:4bit}"
E2E_EMBED_MODEL="${E2E_EMBED_MODEL:-qwen3-embed:0.6b:8bit}"
E2E_VLM_MODEL="${E2E_VLM_MODEL:-qwen3-vl:2b:4bit}"
E2E_RERANK_MODEL="${E2E_RERANK_MODEL:-qwen3-reranker:0.6b:8bit}"
E2E_MTP_MODEL="${E2E_MTP_MODEL:-qwen3.5:4b:mtp:4bit}"

E2E_VLM_IMAGE_URL="${E2E_VLM_IMAGE_URL:-https://picsum.photos/seed/picsum/200/300}"
E2E_RED_PNG_B64="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="

# Apply standard model env defaults (call after sourcing).
e2e_apply_model_defaults() {
    CHAT_MODEL="${CHAT_MODEL:-$E2E_CHAT_MODEL}"
    EMBED_MODEL="${EMBED_MODEL:-$E2E_EMBED_MODEL}"
    VLM_MODEL="${VLM_MODEL:-$E2E_VLM_MODEL}"
    RERANK_MODEL="${RERANK_MODEL:-$E2E_RERANK_MODEL}"
    MTP_MODEL="${MTP_MODEL:-$E2E_MTP_MODEL}"
    LF_HOST="${LF_HOST:-http://127.0.0.1:11430}"
}
