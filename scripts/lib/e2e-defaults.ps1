# Default model shortcuts for E2E / release tests (Windows) — dot-source only.
$E2E_CHAT_MODEL   = if ($env:E2E_CHAT_MODEL)   { $env:E2E_CHAT_MODEL }   else { "qwen3.5:2b:4bit" }
$E2E_EMBED_MODEL  = if ($env:E2E_EMBED_MODEL)  { $env:E2E_EMBED_MODEL }  else { "qwen3-embed:0.6b:8bit" }
$E2E_VLM_MODEL    = if ($env:E2E_VLM_MODEL)    { $env:E2E_VLM_MODEL }    else { "qwen3-vl:2b:4bit" }
$E2E_RERANK_MODEL = if ($env:E2E_RERANK_MODEL) { $env:E2E_RERANK_MODEL } else { "qwen3-reranker:0.6b:8bit" }
$E2E_MTP_MODEL    = if ($env:E2E_MTP_MODEL)    { $env:E2E_MTP_MODEL }    else { "qwen3.5:4b:mtp:4bit" }
$E2E_VLM_IMAGE_URL = if ($env:E2E_VLM_IMAGE_URL) { $env:E2E_VLM_IMAGE_URL } else { "https://picsum.photos/seed/picsum/200/300" }
$E2E_RED_PNG_B64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
