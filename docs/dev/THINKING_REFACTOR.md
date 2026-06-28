# Thinking Layer Refactor — Execution Playbook

> Status: **planned, not started**. This is a pick-up-and-execute plan so the
> work survives context loss. Each phase is self-contained: goal, steps, tests,
> exit criteria. Do them in order; the benchmark is the regression net.

## Why

Today the "thinking" behaviour is smeared across four files with engine-specific
branches inlined into request handlers:

- `src/server/openai.rs` — decides `can_use_budget`, picks stream vs non-stream,
  injects oMLX stop tokens, defaults the budget.
- `src/server/ollama.rs` — its own option mapping + a second think path.
- `src/server/thinking.rs` — engine-aware `apply_think_for_engine`,
  sampling defaults, `ThinkSplitter`, extractors.
- `src/server/proxy.rs` — the two-call orchestrator + a *second* inline-tag
  rewriter (`ThinkTagRewriter`) that duplicates `ThinkSplitter`.

Two consequences we proved with the cross-platform benchmark (commit `d85e0bd`):

1. **Decisions are engine-keyed, not capability-keyed.** Whether to orchestrate
   is really a function of *model capability* (`thinking`, `native_reasoning`),
   while *how* to reason is engine-specific. The two are tangled.
2. **Engine quirks leak into handlers and get missed.** e.g. the oMLX
   truncation-dup (below) lives in a path nobody owns.

## Guiding principle

> **What** path a request takes is decided by **model capability**.
> **How** each engine reasons is an **adapter**.
> `openai.rs` / `ollama.rs` become thin callers of one `thinking::handle(...)`.

## Target layout

```
src/server/thinking/
  mod.rs          # public API + capability gating
                  #   classify(model_caps, engine, req) -> Plan
                  #   Plan = Passthrough | Orchestrate | SingleCallNative
  orchestrator.rs # engine-agnostic two-call budget loop.
                  #   INVARIANT: never yields a fully-empty stream.
  adapter.rs      # trait ThinkingAdapter (the only engine-specific surface)
  adapters/
    omlx.rs       # native reasoning_content; Call-2 = user directive;
                  #   dedup trailing reasoning-as-content on truncation (fix #1)
    template.rs   # inline <think> via splitter (llama.cpp / sglang);
                  #   Call-2 = assistant <think> prefill
  splitter.rs     # ThinkSplitter — consolidates proxy.rs::ThinkTagRewriter
```

`ThinkingAdapter` trait (shape, finalize during Phase 2):

```rust
trait ThinkingAdapter {
    /// Body for Call-1 (reasoning phase): enable thinking, cap to budget.
    fn call1_body(&self, base: &Value, budget: u32) -> Value;
    /// Split a streamed delta into (reasoning, answer) fragments.
    fn parse_delta(&self, delta: &Value, st: &mut ParseState) -> Parsed;
    /// Body for Call-2 (answer phase) given accumulated reasoning.
    fn call2_body(&self, base: &Value, reasoning: &str, remaining: u32) -> Value;
    /// Final dedup/cleanup hook (e.g. oMLX echoes reasoning as content).
    fn finalize(&self, out: &mut Assembled);
}
```

Ollama surface stays **deferred** — it keeps its current path until after the
OpenAI path is proven on the adapter layer. (Explicit decision.)

## Two behaviour fixes to fold in (the only intended behaviour changes)

Both are currently un-owned; they belong in the new layer with tests.

- **Fix #1 — oMLX truncation dup.** Native-reasoning models (`qwen3:4b:thinking`,
  `phi4:reasoning`) bypass the orchestrator (plain passthrough). On `finish=length`
  oMLX streams reasoning as `reasoning_content` deltas **and then one
  `content` delta = the full reasoning** (`r==c, cd=1`, confirmed from raw
  payloads). The OmlxAdapter `finalize` must drop a trailing content run that
  merely repeats the accumulated reasoning. Only triggers on truncation; natural
  `stop` already separates correctly — guard against false positives.
- **Fix #2 — orchestrator silent-empty.** When Call-1 yields nothing (engine
  evicted/erroring — see Fedora), the stream emits only `[DONE]` → blank reply.
  `orchestrator.rs` INVARIANT: if Call-1 produced neither reasoning nor content,
  fall back to a single plain answer call; if that also fails, emit a structured
  error frame. Never a silent empty stream.

## Feature invariants (must hold every phase)

- Streaming **and** non-streaming chat completions.
- `stream_reasoning_deltas` live-forwarding.
- `thinking_budget` default (`DEFAULT_THINKING_BUDGET`) when omitted + eligible.
- Server-seeded anti-loop sampling defaults for `think:true` w/o sampling.
- oMLX stop-token injection (`model_caps.stop_tokens`).
- VLM image parts pass through untouched.
- OpenAI + Ollama contracts (`reasoning_content` / `message.thinking`).
- `native_reasoning` single-call models stay single-call.
- Embeddings / rerank paths untouched.

## Regression-net protocol

- The benchmark `tests/bench/think_bench.py` is the behavioural oracle.
- Baseline = current committed reports for mac/win (Fedora is env-limited, see
  separate efficiency workstream).
- After Phases 1–2 (behaviour-preserving): bench deltas must be **noise only**.
- After Phase 3 (fixes): `blank` and `dup (r==c)` counts must **drop**, nothing
  else regresses.
- Provenance: every report carries the build SHA; no stale-daemon banners.

---

## Phase 0 — Safety net (no code moves)

**Goal:** lock current behaviour in fast, deterministic tests before touching it.

**Steps**
1. Record SSE fixtures (captured engine output) for: oMLX native reasoning,
   oMLX truncation-dup, llama.cpp inline `<think>`, natural `stop`, budget
   `length`, and non-stream.
2. Add `thinking` characterization tests that feed fixtures through the current
   code paths and assert the exact framing (reasoning vs content, finish_reason).
3. Capture a one-shot local bench (`--quick`) as a smoke baseline.

**Exit:** characterization tests green against today's code; fixtures committed.
**Risk:** low. No behaviour change.

## Phase 1 — Extract module (pure move)

**Goal:** create `thinking/` and move existing logic in, zero behaviour change.

**Steps**
1. Create `thinking/mod.rs`, `splitter.rs`; move `ThinkSplitter` there.
2. Consolidate `proxy.rs::ThinkTagRewriter` into `splitter.rs` (single impl;
   keep both call sites working via the unified type).
3. Move `apply_think_for_engine`, sampling defaults, extractors into the module.
4. Re-export to keep `openai.rs` / `ollama.rs` / `proxy.rs` compiling unchanged.

**Exit:** all unit tests + Phase 0 characterization tests green; `cargo clippy`
clean; bench `--quick` == baseline.
**Risk:** low/medium (mechanical). No logic edits.

## Phase 2 — Adapter trait (behaviour-preserving)

**Goal:** route the orchestrator through `ThinkingAdapter`; no behaviour change.

**Steps**
1. Define `ThinkingAdapter` + `ParseState`/`Parsed`/`Assembled` types.
2. Implement `TemplateAdapter` (llama.cpp/sglang) — moves the splitter +
   assistant-prefill Call-2 logic behind the trait.
3. Implement `OmlxAdapter` — native `reasoning_content` + user-directive Call-2
   (the existing `build_call2_body(inline_think=false)` logic).
4. `orchestrator.rs` calls the trait; `classify()` in `mod.rs` selects adapter
   from `(engine, model_caps)`.
5. `openai.rs` streaming/non-stream arms call `thinking::handle(...)`.

**Exit:** unit tests per adapter; full bench on mac + win == baseline (noise
only); clippy clean. `too_many_arguments` on the old orchestrator fn resolved by
the struct-based adapter call.
**Risk:** medium. Mitigated by Phase 0 net + behaviour-preserving constraint.

## Phase 3 — Fold in the two fixes (intended behaviour change)

**Goal:** ship fix #1 (oMLX dedup) and fix #2 (empty-guard) as adapter/
orchestrator responsibilities.

**Steps**
1. `OmlxAdapter::finalize`: detect + drop trailing content that repeats the
   accumulated reasoning on `finish=length`. Unit tests incl. false-positive
   guards (legit answers that paraphrase reasoning must survive).
2. `orchestrator.rs`: empty-guard → single plain answer fallback → structured
   error. Unit tests for: Call-1 empty, Call-1 error, Call-2 empty.
3. Re-run full bench mac/win (+ Fedora once efficiency workstream lands).

**Exit:** `dup (r==c)` → 0 on mac; `blank` from Call-1 failure → fallback/err,
not silent; nothing else regresses.
**Risk:** medium. These are the real changes — keep them isolated in this phase.

## Phase 4 — Thin the callers + docs

**Goal:** finish the cleanup and document.

**Steps**
1. Reduce `openai.rs` / `ollama.rs` think branches to `thinking::handle(...)`.
2. Delete dead code (old `build_call2_body` free fn, duplicate rewriter).
3. Update `README.md`, `DEV_GUIDE.md`, and add an ADR
   (`docs/architecture/ADR-006-thinking-layer.md`) describing the adapter model.
4. Final 3-box baseline; archive under `tests/bench/results/`.

**Exit:** handlers are thin; ADR merged; clean 3-box baseline committed.
**Risk:** low.

---

## Open follow-ups (not in this refactor)

- **Efficiency workstream (separate):** Fedora ran CPU-only (`ngl=0`) despite a
  detectable Vulkan device → RAM thrash. GPU/variant auto-use, fit-planner,
  model lease across multi-call ops, smarter eviction. The **model-lease** idea
  overlaps with Fix #2 (don't evict mid-logical-request) and should coordinate.
- **Ollama on the shared orchestrator** — deferred; revisit after Phase 4.
