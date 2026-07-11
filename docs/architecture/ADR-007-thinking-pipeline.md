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

> **Numbering note.** The original `docs/archive/THINKING_REFACTOR.md` playbook and
> this ADR ended up with different Fix #1/#2 definitions. This ADR is the source
> of truth; Phase 5 below folds in the remaining playbook fixes and reconciles
> the two.

**Fix #3a — oMLX double-emit:** `stream_call1_accumulate` forwarded the raw SSE line
once per matched field. A chunk with both `reasoning_content` and `content` was sent
twice. Fixed: accumulate both fields, forward the raw line exactly once.

**Fix #3b — orchestrator empty-guard:** When Call-1 exhausts the budget but
`reasoning_buf` is empty (engine produced no structured reasoning), Call-2 proceeded
with a degenerate empty-reasoning directive. Initial fix: skip Call-2, return
`content_buf` with `finish_reason=length`. **Upgraded in Phase 5** to a true
plain-answer fallback (see below).

**Fix #3c — enable_thinking:false default (thinking opt-in):** On template-flag engines
(llama.cpp; SGLang when explicitly selected) with a thinking-capable model and **no**
think intent, the Qwen3 HF Jinja template defaults `enable_thinking=true`, burns
`max_tokens` on reasoning, and returns no answer (or echoes reasoning as `content`).
Fixed: `apply_think_for_engine` injects `enable_thinking:false` for this case.

**Fix #3c (oMLX parity, 2026-07):** The same no-intent path on oMLX previously omitted
the flag (“natural reasoning”), which let hybrid Qwen3 models reason unprompted;
oMLX then truncated at `max_tokens` and echoed the reasoning blob as the answer
(`reasoning==content` dup — 21 hits in think_bench pre-fix). Empirical oMLX testing
already confirmed `enable_thinking:false` suppresses reasoning cleanly (0 reasoning
tokens); only `enable_thinking:true` is forbidden (infinite-loop risk). oMLX now
matches llamacpp: `Some(false)` and `None` (no intent) on hybrid thinking models →
`enable_thinking:false`; `Some(true)` → omit flag (orchestrator path). Native-reasoning
models ignore the flag (bounded by Fix #5c floor + Fix #5a dedup instead).

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

### Phase 5 — Playbook reconciliation (two remaining fixes)

Phase 3 closed three bugs but left two items from the `THINKING_REFACTOR.md`
playbook open. Phase 5 lands them with unit tests:

**Fix #5a — native-reasoning truncation echo (the playbook's "Fix #1").**
Native-reasoning models (`phi4:reasoning`, `qwen3:4b:thinking`) bypass the budget
orchestrator and stream straight through. On oMLX, when generation hits
`finish=length`, the engine emits the reasoning as `reasoning_content` deltas and
then re-emits the **entire reasoning once more as a single `content` delta**
(`r==c`). That echo is not an answer. Fix:
- Streaming: `proxy::proxy_stream_dedup_native_reasoning` accumulates reasoning and
  suppresses any `content` delta that is a verbatim, length-guarded copy of the
  accumulated reasoning. Routed only for oMLX native-reasoning (`is_native_reasoning
  && !inline_think`); every other engine/model keeps raw passthrough.
- Non-streaming: `proxy_request_assembling_stream` drops `content` when
  `finish=length && is_reasoning_echo(content, reasoning)`.
- Shared predicate `is_reasoning_echo` (min 40 trimmed chars + exact match) guards
  against false positives; natural `stop` is never touched. Unit-tested.

**Fix #5b — orchestrator plain-answer fallback (upgrades Fix #3b).**
The empty-guard now distinguishes three sub-cases after budget exhaustion:
1. reasoning present → Call-2 (reasoning prefill + answer).
2. reasoning empty, content present → return that content as the answer.
3. reasoning empty **and** content empty (engine evicted/errored/empty) →
   `build_plain_answer_body` re-issues the **original** messages with
   `enable_thinking:false` and the full token budget (a single normal call). If
   that *also* yields nothing, a structured error frame is emitted instead of a
   silent blank stream. Applied to both streaming and non-streaming paths.

**Fix #5c — native-reasoning budget floor.**
Native-reasoning models ignore `enable_thinking`, so the orchestrator can't bound
their reasoning. With a small client `max_tokens` they spend the whole budget
thinking and emit no answer (`finish=length`, empty `content`) — a true blank, not
an echo (#5a). This is *not* recoverable by a second call: every major provider
(OpenAI, Anthropic, vLLM, SGLang, llama.cpp) treats it as a budget problem and the
fix is always headroom, never a continuation call (researched 2026-06-30). Fix:
`thinking::prepare_request` raises `max_tokens` to `NATIVE_REASONING_MIN_TOKENS`
(4096) for native-reasoning models — floor only (a larger client value is never
lowered), applied regardless of think on/off since these models reason in both
modes. Industry parallels: OpenAI reserves ≥25k tokens; Anthropic enforces
`max_tokens > budget_tokens`; llama.cpp/Qwen publish per-task minimums. Unit-tested.

### Phase 6 — orchestrator robustness hardening

Closes the "Medium-confidence" gaps from the workstream review (first-class engines
oMLX + llama.cpp; SGLang/vLLM are opt-in, not first-class):

- **Mid-stream Call-1 abort → no silent blank.** `stream_call1_accumulate` could
  return `finish_reason=None` (stream error / engine omitted the marker), which the
  orchestrator treated as a natural finish and emitted an empty stream. Routing is
  now via `call1_finished_with_answer(finish_reason, has_content)`: clean finish →
  answer; `length` → Call-2/guards; `None` → complete only if content was produced,
  otherwise drop to the empty-guard fallback. A missing marker is normalised to
  `"stop"` on the terminal chunk. Unit-tested.
- **Call-1 safety cap + idle timeout.** `stream_call1_accumulate` now bounds
  accumulation (`CALL1_MAX_DATA_LINES=16384`, `CALL1_MAX_TOTAL_BYTES=1 MB`) and adds
  a 120 s per-chunk idle timeout — mirroring the assemble/rewriter paths so a runaway
  or stalled Call-1 can neither hang the stream nor balloon memory. Tripping the cap
  is treated as `length` so Call-2 still recovers an answer. `[DONE]` now stops the
  whole accumulator (was inner-loop only).
- **Ollama native-reasoning dedup parity (Fix #5a).** `/api/chat` streaming now
  routes native-reasoning oMLX models through `proxy_stream_dedup_native_reasoning`,
  matching the OpenAI path; previously the truncation echo could reach
  `message.thinking`/`content` via Ollama.

### Deviation from the playbook design

`THINKING_REFACTOR.md` proposed a heavier `ThinkingAdapter` (per-engine
`call1_body`/`parse_delta`/`call2_body`/`finalize`), a dedicated `orchestrator.rs`,
`adapters/{omlx,template}.rs`, and a `classify() → Plan` dispatcher with
`thinking::handle()`. The shipped design is intentionally lighter: a 2-bool adapter
(`supports_orchestrator` / `inline_think`) + `prepare_request() → ThinkingContext`,
with the two-call loop remaining in `proxy.rs`. Rationale: the boolean adapter
captures every routing decision the engines actually need today with far less code,
and the orchestrator's `proxy.rs` home keeps the SSE plumbing in one place. The
richer trait can be revisited if a future engine needs per-call body shaping that
the booleans can't express.

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

Empty-guard (Fix #5b): after budget exhaustion, if `reasoning_buf` is empty →
return `content_buf` if present, else issue a plain-answer fallback call
(`enable_thinking:false`, full budget, original messages); if the fallback also
produces nothing, emit a structured error frame. Never a silent blank stream.

---

## Engine Matrix (post-Phase 6)

First-class engines: **oMLX** (macOS) and **llama.cpp** (Linux/Windows). SGLang shares
the llamacpp thinking adapter but is **experimental opt-in only** (see ADR-001).

| Engine | Tier | `supports_orchestrator` | `inline_think` | Fix #3c (no think intent) | Fix #5a (echo dedup) | Fix #5c (budget floor) |
|---|---|---|---|---|---|---|
| oMLX | default (macOS) | ✓ | ✗ | ✓ (hybrid models; native-reasoning ignores flag) | ✓ (native-reasoning) | ✓ (native-reasoning) |
| llama.cpp | default (Linux/Win) | ✓ | ✓ | ✓ | ✗ (inline tags, not field-echo) | ✓ (native-reasoning) |
| SGLang | experimental | ✓ | ✓ | ✓ (same as llamacpp) | ✗ | ✓ (native-reasoning) |
| vLLM / others | opt-in | ✗ | ✗ | ✗ | ✗ | ✓ (native-reasoning) |

---

## Consequences

- **+** Engine-specific thinking logic is fully contained in `src/server/thinking/`.
  Adding a new engine's thinking semantics requires only a new `ThinkingAdapter` impl
  and a match arm in `adapter_for_engine`.
- **+** `openai.rs::chat_completions` is freed from thinking orchestration details.
- **+** Six production bugs fixed: double-emit (#3a), empty-guard→fallback (#3b/#5b),
  plain-client blank reply (#3c), native-reasoning truncation echo (#5a),
  native-reasoning truncated-to-blank (#5c, budget floor).
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
- `docs/archive/OMLX_SHARED_SERVER_FINDINGS.md` — oMLX engine behaviour reference (archived spike)
- [ADR-008](./ADR-008-pool-residency.md) — residency terminology + platform validation matrix
