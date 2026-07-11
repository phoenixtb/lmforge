# Phase 0 Spike Findings — oMLX 0.4.4 Shared-Server Behavior

> **Archived.** Empirical spike notes used to write
> [`../architecture/ADR-006-engine-residency.md`](../architecture/ADR-006-engine-residency.md).
> Not maintained; may lag current oMLX.

Empirical validation against the actual running `omlx serve` process.
All tests run against the existing LMForge-managed instance on port 11431
(which already passes `--model-dir <parent>` due to the current adapter code).

## Results

### One process, all models visible

A single `omlx serve --model-dir ~/.lmforge/models --port <any>` discovers all
14 model subdirectories at startup and exposes them as distinct model IDs via
`/v1/models`. `model_count: 14`, `loaded_count: 0` (none hot until first
request).

### On-demand load via `model` field

Requesting any discovered model via the `model` field causes oMLX to load it
into Metal memory on the first call. Subsequent calls are hot. This is the
native "lazy load on request" pattern — no LMForge-side spawn required.

```
curl .../v1/chat/completions -d '{"model":"qwen3.5-2b-4bit",...}'
→ cold load ~2-6s, answer returned; subsequent calls are fast.
```

### Multi-model routing works as designed

Two different chat models served from the same process on the same port, just
by changing the `model` field:
- `qwen3.5-2b-4bit`: `{"content": "The answer to 2+2 is: **4**", "finish": "length"}`
- `qwen3-1.7b-4bit`: `{"content": "Okay, let's see. The user is asking...", "finish": "length"}`

### Embeddings, rerank, VLM — all in one process

| model field | endpoint | result |
|---|---|---|
| `qwen3-embedding-0.6b-8bit` | `/v1/embeddings` | 1024-dim vectors |
| `qwen3-reranker-0.6b-mxfp8` | `/v1/rerank` | relevance scores (0.28, 0.006) |
| `qwen3-vl-2b-instruct-4bit` | `/v1/chat/completions` | text answer |

All via the same `omlx serve` process. No per-role spawn needed.

### Streaming `reasoning_content` works

`enable_thinking: true` in `chat_template_kwargs` emits `delta.reasoning_content`
SSE frames from the shared server — same as the current per-process path.

### Pull → discovery: REQUIRES RESTART

oMLX discovers models only at startup (scans `--model-dir` once). A newly-pulled
subdir added after startup is **not auto-discovered**:
- `/v1/models` count stays at 14 after adding a new subdir.
- The admin `/admin/api/models/{id}/load` endpoint exists but requires a
  session-cookie-based login (always returns `"Admin authentication required"`
  unless a browser-established session cookie is provided — not automatable by
  LMForge without implementing an auth flow).
- A model request for an unknown ID returns a 404 with the known-model list.

**Discovery strategy: restart the oMLX server after `lmforge pull`.**

This is clean, reliable, and costs only a few seconds (oMLX startup is fast —
health-ready in ~1s in testing). The resident models' KV cache is lost on
restart, but that is acceptable and matches user expectations for a "newly
added model, reload required" flow. LMForge already controls the process
lifecycle; restarting after pull is one more managed transition.

### Admin API

oMLX 0.4.4 admin endpoints are session-cookie-gated (browser login only).
No machine-callable rescan hook is available. The `--api-key` CLI flag gates
inference endpoints only, not admin.

## Design decisions (confirmed)

| Question | Decision |
|---|---|
| Shared server? | YES — one `omlx serve --model-dir <parent>` per LMForge daemon |
| Per-model spawn? | NO — `model` field routes; oMLX handles loading/eviction |
| LRU/TTL management | Delegated entirely to oMLX's `EnginePool` + `ProcessMemoryEnforcer` |
| `inflight` counter | LMForge keeps it (uniform contract for `InflightGuard`); SharedServer never uses it for eviction |
| Pull → discovery | Restart oMLX server after `lmforge pull` (new subdir not auto-discovered) |
| Lazy start | YES — spawn on first model request; stay resident |
| Admin load API | Not usable (auth-gated); restart is the only reliable hook |
| Model ID format | oMLX uses the subdir name as-is → must match LMForge model ID naming |
