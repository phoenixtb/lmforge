# Default models, workloads, and probes for E2E / release tests (Windows) - dot-source only.
$E2E_CHAT_MODEL   = if ($env:E2E_CHAT_MODEL)   { $env:E2E_CHAT_MODEL }   else { "qwen3.5:2b:4bit" }
$E2E_EMBED_MODEL  = if ($env:E2E_EMBED_MODEL)  { $env:E2E_EMBED_MODEL }  else { "qwen3-embed:0.6b:8bit" }
$E2E_VLM_MODEL    = if ($env:E2E_VLM_MODEL)    { $env:E2E_VLM_MODEL }    else { "qwen3-vl:2b:4bit" }
$E2E_RERANK_MODEL = if ($env:E2E_RERANK_MODEL) { $env:E2E_RERANK_MODEL } else { "qwen3-reranker:0.6b:8bit" }
$E2E_MTP_MODEL    = if ($env:E2E_MTP_MODEL)    { $env:E2E_MTP_MODEL }    else { "qwen3.5:4b:mtp:4bit" }
$E2E_VLM_IMAGE_URL = if ($env:E2E_VLM_IMAGE_URL) { $env:E2E_VLM_IMAGE_URL } else { "https://picsum.photos/seed/picsum/200/300" }
$E2E_RED_PNG_B64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="

$E2E_CHAT_MAX_TOKENS      = if ($env:E2E_CHAT_MAX_TOKENS)      { [int]$env:E2E_CHAT_MAX_TOKENS }      else { 128 }
$E2E_MTP_MAX_TOKENS       = if ($env:E2E_MTP_MAX_TOKENS)       { [int]$env:E2E_MTP_MAX_TOKENS }       else { 256 }
$E2E_VLM_TEXT_MAX_TOKENS  = if ($env:E2E_VLM_TEXT_MAX_TOKENS)  { [int]$env:E2E_VLM_TEXT_MAX_TOKENS }  else { 128 }
$E2E_VLM_IMAGE_MAX_TOKENS = if ($env:E2E_VLM_IMAGE_MAX_TOKENS) { [int]$env:E2E_VLM_IMAGE_MAX_TOKENS } else { 192 }

$E2E_EMBED_COLD = if ($env:E2E_EMBED_COLD) { $env:E2E_EMBED_COLD } else {
    "Natural language processing combines computational linguistics with statistical and neural models to interpret human language. Modern embedding models map text into dense vector spaces so semantic similarity can be measured for search, clustering, and retrieval-augmented generation pipelines."
}
$E2E_CHAT_COLD = if ($env:E2E_CHAT_COLD) { $env:E2E_CHAT_COLD } else {
    "Explain how transformer self-attention helps language models capture long-range dependencies. Cover queries, keys, values, and why this matters for code and document understanding. Write at least four sentences."
}
$E2E_EMBED_MIXED = if ($env:E2E_EMBED_MIXED) { $env:E2E_EMBED_MIXED } else {
    "Concurrent embedding workload: indexing multi-paragraph knowledge-base chunks for a local RAG stack running beside an interactive chat model on the same GPU daemon."
}
$E2E_CHAT_MIXED = if ($env:E2E_CHAT_MIXED) { $env:E2E_CHAT_MIXED } else {
    "While embeddings run in parallel, respond with a concise paragraph about orchestrating chat and embed models on one machine without VRAM thrashing. Mention co-load and burst traffic."
}
$E2E_EMBED_BURST_BODY = if ($env:E2E_EMBED_BURST_BODY) { $env:E2E_EMBED_BURST_BODY } else {
    "Retrieval systems shard long PDFs into overlapping segments. Each segment must embed faithfully so cosine similarity ranks relevant policy clauses above unrelated boilerplate."
}
$E2E_CHAT_BURST_TEMPLATE = if ($env:E2E_CHAT_BURST_TEMPLATE) { $env:E2E_CHAT_BURST_TEMPLATE } else {
    "Request {0} of {1}: Compare running LLM inference locally versus a cloud API for a 20-person engineering team. Address latency, privacy, compliance, offline use, and hardware capex. Give a balanced summary in a full paragraph."
}
$E2E_VLM_TEXT = if ($env:E2E_VLM_TEXT) { $env:E2E_VLM_TEXT } else {
    "You are a vision-language assistant. Describe what visual features you would examine in a street photograph and how lighting affects mood. Write 5-7 sentences."
}
$E2E_VLM_REMOTE_PROMPT = if ($env:E2E_VLM_REMOTE_PROMPT) { $env:E2E_VLM_REMOTE_PROMPT } else {
    "Study this photograph carefully. Describe the main subject, composition, lighting, dominant colors, background elements, and overall mood. Write at least 80 words as if briefing a designer."
}
$E2E_VLM_BASE64_PROMPT = if ($env:E2E_VLM_BASE64_PROMPT) { $env:E2E_VLM_BASE64_PROMPT } else {
    "This image is a tiny test pattern. Name the dominant color and explain how vision models fuse patch embeddings with text tokens."
}
$E2E_RERANK_QUERY = if ($env:E2E_RERANK_QUERY) { $env:E2E_RERANK_QUERY } else {
    "Which passage best explains deploying a private OpenAI-compatible LLM server for a team that needs offline document Q&A?"
}
$E2E_MTP_WARM = if ($env:E2E_MTP_WARM) { $env:E2E_MTP_WARM } else {
    "Write a technical paragraph of at least 100 words on how speculative decoding with draft MTP heads raises tokens-per-second on CUDA inference while preserving output quality when acceptance thresholds are tuned correctly."
}

function Get-E2eBurstEmbedText([int]$Index, [int]$Total) {
    return "RAG index batch - chunk ${Index}/${Total}: $E2E_EMBED_BURST_BODY"
}

function Get-E2eBurstChatText([int]$Index, [int]$Total) {
    return [string]::Format($E2E_CHAT_BURST_TEMPLATE, $Index, $Total)
}

function Get-E2eRerankDocuments() {
    return @(
        "LMForge ships a local daemon exposing /v1/chat/completions and /v1/embeddings with hardware-aware engine selection.",
        "Kubernetes pod scheduling uses taints, tolerations, and resource requests to place GPU workloads.",
        "A sourdough starter requires regular feeding with flour and water to maintain yeast activity.",
        "Teams can run lmforge init once, pull GGUF or MLX models, and serve tools without sending data to the cloud.",
        "The weather in London is often overcast with light rain during autumn months."
    )
}

function Test-E2eSuiteEnabled([string]$EnvName) {
    $v = [Environment]::GetEnvironmentVariable($EnvName)
    if (-not $v) { return $true }
    return ($v -notmatch '^(0|false|no)$')
}
