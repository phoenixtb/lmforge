# ADR-007 — Thinking Pipeline Architecture

**Status:** Accepted  
**Date:** 2026-06-30  
**Author:** lmforge core team

---

## Context

LMForge provides an OpenAI-compatible chat-completions API that surfaces structured
reasoning ("thinking") across three engine backends. Each engine has a different wire
protocol for reasoning, and the failure modes differ:

| Engine | Reasoning wire format | Risk without orchestration |
|---|---|---|
| oMLX (Apple Silicon) | `delta.reasoning_content` field | `enable_thinking:true` — safe on 0.4.4 under a token cap; unbounded → loop |
| llama.cpp / SGLang | `<think>…</think>` tags in `delta.content` | Model reasons until `max_tokens`, returns blank answer (`finish=length`) |
| vLLM / others | not supported | plain passthrough |

Additionally, the Qwen3 HF Jinja template on llama.cpp/SGLang **defaults to
`enable_thinking=true`** when no flag is supplied, meaning plain requests to a
thinking-capable model silently produce reasoning-only output with no answer
(the "blank reply bug").

The v0.1.5 implementation had the orchestration logic embedded inline in
`src/server/openai.rs` (~150 lines), mixed with request parsing and vision checks,
making it difficult to reason about and extend.

---

## Decision

Refactor the thinking pipeline into a self-contained `src/server/thinking/` module
in four phases (all behaviour-preserving within a phase unless noted):

### Phase 1 — Module extraction + ThinkSplitter consolidation

`thinking.rs` → `thinking/mod.rs` + `thinking/splitter.rs`.  
`ThinkTagRewriter` in `proxy.rs` (duplicate of `ThinkSplitter`) removed; all
call sites updated to `ThinkSplitter`.

### Phase 2 — ThinkingAdapter trait

`thinking/adapter.rs` introduces a `ThinkingAdapter` trait with two boolean
properties:

- **`supports_orchestrator`** — engine is wired into the two-call budget path.
- **`inline_think`** — reasoning arrives as `<think>` tags in `content` (vs native
  `reasoning_content`).

Three concrete adapters: `OmlxAdapter`, `TemplateAdapter` (llama.cpp/SGLang),
`PassthroughAdapter` (vLLM/unknown). A `adapter_for_engine(id)` factory returns a
`'static dyn ThinkingAdapter` with zero allocation.

`openai.rs` and `ollama.rs` replace inline `engine_id == "omlx"` / `|| "sglang"`
comparisons with adapter calls.

### Phase 3 — Three bug fixes

**Fix #1 — oMLX double-emit:** `stream_call1_accumulate` forwarded the raw SSE line
once per matched field. A chunk with both `reasoning_content` and `content` was sent
twice. Fixed: accumulate both fields, forward the raw line exactly once.

**Fix #2 — orchestrator empty-guard:** When Call-1 exhausts the budget but
`reasoning_buf` is empty (engine produced no structured reasoning), Call-2 proceeded
with a degenerate empty-reasoning directive. Fixed: skip Call-2, return `content_buf`
with `finish_reason=length`. Applied to both streaming and non-streaming paths.

**Fix #3 — enable_thinking:false default:** On llama.cpp/SGLang with a thinking-capable
model and no think intent, the Qwen3 HF Jinja template defaults `enable_thinking=true`,
burns `max_tokens` on reasoning, and returns no answer. Fixed: `apply_think_for_engine`
now explicitly injects `enable_thinking:false` for this case, making thinking opt-in.
oMLX is unaffected (no template, reasoning governed by model weights).

### Phase 4 — ThinkingContext + thin callers

`thinking::prepare_request(body, engine_id, model_caps) → ThinkingContext` encapsulates
the entire pre-routing preamble:

1. Extract `has_think`, `thinking_budget`, `stream_reasoning_deltas`, `original_max_tokens`
2. Apply sampling defaults + engine transforms
3. Strip LMForge-private fields from body
4. Resolve adapter flags + model capabilities into `can_use_budget` / `inline_think`
5. Default `thinking_budget` to `DEFAULT_THINKING_BUDGET` (2048) when applicable

`openai.rs::chat_completions` calls `prepare_request` once and reads from `ThinkingContext`
for routing. The ~150 inline lines collapse to ~15.

---

## Two-call Orchestrator Protocol

```
Client → LMForge → Call-1 (engine): max_tokens = thinking_budget, enable_thinking:true
                ←  stream: delta.reasoning_content / <think>…</think> in delta.content
                   Accumulate into reasoning_buf; forward live if stream_reasoning_deltas=true
                   finish_reason = "length" when budget exhausted

       → Call-2 (engine): reasoning prefill + enable_thinking:false
         oMLX path:   prepend a USER turn with the reasoning text (oMLX regenerates
                      assistant prefills, causing duplication if prefilled as assistant)
         llama.cpp/SGLang: append closed <think>…</think> ASSISTANT turn (engine
                      continues the assistant turn into the answer)

                ←  stream: delta.content (answer only)
                   Forward directly to client
```

Natural finish (Call-1 `finish_reason != "length"`): skip Call-2; emit buffered content.

Empty-guard: `reasoning_buf.is_empty()` after budget exhaustion → skip Call-2; return
content as `finish_reason=length`. Prevents degenerate directive to engine.

---

## Engine Matrix (post-Phase 4)

| Engine | `supports_orchestrator` | `inline_think` | Fix #3 applies |
|---|---|---|---|
| oMLX | ✓ | ✗ | ✗ (natural reasoning) |
| llama.cpp | ✓ | ✓ | ✓ |
| SGLang | ✓ | ✓ | ✓ |
| vLLM / others | ✗ | ✗ | ✗ |

---

## Consequences

- **+** Engine-specific thinking logic is fully contained in `src/server/thinking/`.
  Adding a new engine's thinking semantics requires only a new `ThinkingAdapter` impl
  and a match arm in `adapter_for_engine`.
- **+** `openai.rs::chat_completions` is freed from thinking orchestration details.
- **+** Three production bugs fixed (double-emit, empty-guard, blank reply).
- **−** `prepare_request` takes `model_caps: Option<&ModelCapabilities>` — callers
  that don't load the model index still get a correct (if conservative) context, but
  budget-defaulting and Fix #3 require the index to identify thinking models.
- **−** oMLX's stop-token injection remains in `openai.rs` (engine-specific but not
  thinking-specific; moving it to the adapter is a separate concern).

---

## References

- `src/server/thinking/` — full module
- `src/server/proxy.rs` — `proxy_stream_with_thinking_budget`, `proxy_nonstream_with_thinking_budget`
- `src/server/openai.rs` — `chat_completions` handler
- `src/server/ollama.rs` — `chat` handler (Ollama compatibility layer)
- `docs/dev/OMLX_SHARED_SERVER_FINDINGS.md` — oMLX engine behaviour reference
