# Default models, workloads, and probes for E2E / release tests — source only.
[[ -n "${_LMFORGE_E2E_DEFAULTS_LOADED:-}" ]] && return 0
_LMFORGE_E2E_DEFAULTS_LOADED=1

E2E_CHAT_MODEL="${E2E_CHAT_MODEL:-qwen3.5:2b:4bit}"
E2E_EMBED_MODEL="${E2E_EMBED_MODEL:-qwen3-embed:0.6b:8bit}"
E2E_VLM_MODEL="${E2E_VLM_MODEL:-qwen3-vl:2b:4bit}"
E2E_RERANK_MODEL="${E2E_RERANK_MODEL:-qwen3-reranker:0.6b:8bit}"
E2E_MTP_MODEL="${E2E_MTP_MODEL:-qwen3.5:4b:mtp:4bit}"

E2E_VLM_IMAGE_URL="${E2E_VLM_IMAGE_URL:-https://picsum.photos/seed/picsum/200/300}"
E2E_RED_PNG_B64="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="

# Token budgets (realistic generation, not one-word probes)
E2E_CHAT_MAX_TOKENS="${E2E_CHAT_MAX_TOKENS:-128}"
E2E_MTP_MAX_TOKENS="${E2E_MTP_MAX_TOKENS:-256}"
E2E_VLM_TEXT_MAX_TOKENS="${E2E_VLM_TEXT_MAX_TOKENS:-128}"
E2E_VLM_IMAGE_MAX_TOKENS="${E2E_VLM_IMAGE_MAX_TOKENS:-192}"

# Paragraph-scale workloads
E2E_EMBED_COLD="${E2E_EMBED_COLD:-Natural language processing combines computational linguistics with statistical and neural models to interpret human language. Modern embedding models map text into dense vector spaces so semantic similarity can be measured for search, clustering, and retrieval-augmented generation pipelines.}"

E2E_CHAT_COLD="${E2E_CHAT_COLD:-Explain how transformer self-attention helps language models capture long-range dependencies. Cover queries, keys, values, and why this matters for code and document understanding. Write at least four sentences.}"

E2E_EMBED_MIXED="${E2E_EMBED_MIXED:-Concurrent embedding workload: indexing multi-paragraph knowledge-base chunks for a local RAG stack running beside an interactive chat model on the same GPU daemon. Each chunk should retain enough context for citation back to source documents.}"

E2E_CHAT_MIXED="${E2E_CHAT_MIXED:-While embeddings run in parallel, respond with a concise paragraph about orchestrating chat and embed models on one machine without VRAM thrashing. Mention co-load, burst traffic, and when to serialize vs parallelize requests.}"

E2E_EMBED_BURST_PREFIX="${E2E_EMBED_BURST_PREFIX:-RAG index batch -}"

E2E_EMBED_BURST_BODY="${E2E_EMBED_BURST_BODY:-Retrieval systems shard long PDFs into overlapping segments. Each segment must embed faithfully so cosine similarity ranks relevant policy clauses above unrelated boilerplate. Include metadata such as section titles and page numbers when available.}"

E2E_CHAT_BURST_TEMPLATE="${E2E_CHAT_BURST_TEMPLATE:-Request %d of %d: Compare running LLM inference locally versus a cloud API for a 20-person engineering team. Address latency, privacy, compliance, offline use, and hardware capex. Give a balanced summary in a full paragraph.}"

E2E_VLM_TEXT="${E2E_VLM_TEXT:-You are a vision-language assistant. Describe what visual features you would examine in a street photograph and how lighting, color balance, and depth of field affect mood. Write 5-7 sentences suitable for a creative brief.}"

E2E_VLM_REMOTE_PROMPT="${E2E_VLM_REMOTE_PROMPT:-Study this photograph carefully. Describe the main subject, composition, lighting, dominant colors, background elements, and overall mood. Write at least 80 words as if briefing a designer.}"

E2E_VLM_BASE64_PROMPT="${E2E_VLM_BASE64_PROMPT:-This image is a tiny test pattern. Name the dominant color and explain in 2-3 sentences how vision models fuse patch embeddings with text tokens during inference.}"

E2E_RERANK_QUERY="${E2E_RERANK_QUERY:-Which passage best explains deploying a private OpenAI-compatible LLM server for a team that needs offline document Q&A?}"

E2E_MTP_WARM="${E2E_MTP_WARM:-Write a technical paragraph of at least 100 words on how speculative decoding with draft MTP heads raises tokens-per-second on CUDA inference while preserving output quality when acceptance thresholds are tuned correctly.}"

e2e_burst_embed_text() {
    local i="$1" n="$2"
    printf '%s chunk %d/%d: %s' "$E2E_EMBED_BURST_PREFIX" "$i" "$n" "$E2E_EMBED_BURST_BODY"
}

e2e_burst_chat_text() {
    local i="$1" n="$2"
    # shellcheck disable=SC2059
    printf "$E2E_CHAT_BURST_TEMPLATE" "$i" "$n"
}

e2e_rerank_documents_json() {
    jq -nc '[
        "LMForge ships a local daemon exposing /v1/chat/completions and /v1/embeddings with hardware-aware engine selection.",
        "Kubernetes pod scheduling uses taints, tolerations, and resource requests to place GPU workloads.",
        "A sourdough starter requires regular feeding with flour and water to maintain yeast activity.",
        "Teams can run lmforge init once, pull GGUF or MLX models, and serve tools without sending data to the cloud.",
        "The weather in London is often overcast with light rain during autumn months."
    ]'
}

# Apply standard model env defaults (call after sourcing).
e2e_apply_model_defaults() {
    CHAT_MODEL="${CHAT_MODEL:-$E2E_CHAT_MODEL}"
    EMBED_MODEL="${EMBED_MODEL:-$E2E_EMBED_MODEL}"
    VLM_MODEL="${VLM_MODEL:-$E2E_VLM_MODEL}"
    RERANK_MODEL="${RERANK_MODEL:-$E2E_RERANK_MODEL}"
    MTP_MODEL="${MTP_MODEL:-$E2E_MTP_MODEL}"
    LF_HOST="${LF_HOST:-http://127.0.0.1:11430}"
}

# All capability suites on by default; set DO_VLM=0 / --skip-vlm to disable.
e2e_suite_enabled() {
    local flag="$1"
    [[ "${!flag:-1}" -eq 1 ]]
}
