# Quality & Competitiveness Plan — Sep 2026

> **Working doc** — tracks the fixes/features identified from the 2026-09-07
> Ubuntu-box validation (think_bench 192 runs + multi_model_e2e 15/15 on
> RTX 5060 Ti, lmforge 0.1.9, llama.cpp b9861 cuda12) and the industry
> comparison against Ollama / LM Studio / llama-server / vLLM.
> Delete or archive once all batches ship.
>
> Status legend: `[ ]` todo · `[~]` in progress · `[x]` done (commit sha)

Baseline for all file:line references: commit `466d8a0`.

---

## Batch 1 — internal quality bugs (target: v0.1.10)

### 1.1 Call-2 failure → silent blank answer, no terminal `finish_reason`  — HIGH

- [x] Status (pending commit)
- **Problem:** In the two-call thinking workflow, if Call-2's HTTP request to
  llama-server fails at send time, the client gets
  `data: {"error":...}\n\ndata: [DONE]` — no terminal chunk, no
  `finish_reason`. The user sees a blank answer after a full reasoning phase.
  Observed live: think_bench 2026-09-07, `qwen3.5:4b:6bit`/`seq_next`/think=on
  r2 — Call-1 exhausted budget correctly, Call-2 `reqwest` send failed once
  (next request on the same slot 2 ms later succeeded), run recorded
  `finish_reason=None`, `blank=true`.
- **Evidence:** `src/server/proxy.rs` ~1169 (Call-2 `Err(e)` arm). Violates the
  codebase's own invariant — the Call-1 natural-finish path (~1077) normalises
  missing markers to `"stop"` with a comment "clients never see a null
  finish_reason on the terminal chunk".
- **Design:**
  1. Retry Call-2 once on transport error (short backoff, ~200 ms — the
     observed failure was a transient same-port hop).
  2. On final failure, still emit a terminal chunk with
     `finish_reason: "length"` (reasoning was produced, budget consumed)
     *after* the SSE error object, before `[DONE]`.
  3. Call-2 success stream end: synthesise terminal `finish_reason`
     (`"stop"`/`"length"`) when the engine omitted it — mirror the Call-1 path.
- **Acceptance:** unit/integration test covering the Call-2 error arm asserting
  a terminal chunk with non-null `finish_reason` is always emitted.

### 1.2 `/lf/status.metrics` is a never-populated stub — MED

- [x] Status (pending commit)
- **Problem:** `EngineMetrics` (`requests_total`, `ttft_avg_ms`,
  `uptime_secs`, `restart_count`) is only ever `Default::default()` — never
  written. Live daemon after 13 h / 276 requests reports all zeros while
  `/lf/metrics` has the real numbers. UI `StatusBar.svelte` reads the dead
  fields (shows `—` forever); the Overview page already migrated to
  `/lf/metrics`.
- **Evidence:** `src/engine/manager.rs:39,434`, `src/engine/process_pool.rs:103`,
  `src/server/native.rs:112`; live curl 2026-09-07.
- **Design:** populate the block at read time in `native::status` from the real
  sources (`metrics::uptime_secs()`, request counter, TTFT aggregate if
  available; `restart_count` from the pool's respawn ledger if cheap, else keep
  0 and document). Do NOT remove the field (breaking API). Update
  `StatusBar.svelte` only if field semantics change.
- **Acceptance:** `curl /lf/status` on a daemon that served ≥1 request shows
  non-zero `requests_total` and `uptime_secs`.

### 1.3 `lmforge_active_models` gauge stale after TTL sweep / crash reap — MED

- [x] Status (pending commit)
- **Problem:** gauge updated only on load and manual unload
  (`process_pool.rs:899,934,945`); the keep-alive TTL sweep (~992) and
  crash reap (~963) evict slots without updating it. Observed live: gauge=2
  with `running_models: []` and no llama-server process.
- **Design:** call `metrics::set_active_models(self.active_slots.len())` at the
  end of both eviction paths (single helper if cleaner).
- **Acceptance:** unit test or targeted assertion; gauge matches slot count
  after TTL eviction.

### 1.4 `/lf/metrics` latency percentiles always null — MED

- [x] Status (pending commit)
- **Problem:** the Prometheus exporter emits **summaries** (`quantile=` lines);
  the `/lf/metrics` digest parser expects **histogram buckets** — p50/p95/p99
  render as null/0 despite real traffic (chat sum 82.5 s / 212 reqs observed).
- **Evidence:** `src/server/metrics_api.rs:142-157` vs exporter config in
  `src/server/metrics.rs`.
- **Design:** either configure `PrometheusBuilder` to emit histogram buckets
  for the latency metrics, or teach the digest parser to read `quantile=`
  summary lines. Prefer whichever keeps `/metrics` output stable for external
  scrapers (summaries → parse summaries).
- **Acceptance:** `/lf/metrics` shows non-null p50/p95/p99 after a few chat
  requests (covered by e2e TC or integration test).

### 1.5 `/lf/engines` reports `installed:false` for the active variant-layout llamacpp — MED

- [x] Status (pending commit)
- **Problem:** `install_state` looks for `data_dir/engines/llama-server`, but
  CUDA variants install to
  `~/.lmforge/engines/llamacpp/variants/<variant>/llama-server`. Live box:
  `active: true, version: b9861, installed: false`. UI would offer "Install"
  on a working engine.
- **Evidence:** `src/cli/engine.rs:610-617`; live curl 2026-09-07.
- **Design:** make the check variant-aware — consider installed when the
  active variant dir contains the binary (reuse
  `installer::active_installed_llamacpp_tag()` / variant resolution).
- **Acceptance:** `/lf/engines` shows `installed:true` on a variant-layout
  install (test with a fake dir layout).

### 1.6 e2e scripts capture `status.json` before traffic — LOW (test artifact)

- [x] Status (pending commit)
- **Problem:** `tests/multi_model_e2e.sh:489` (and `.ps1`) snapshot
  `/lf/status` before any test request — captured metrics/last_errors describe
  the *previous* workload (caused two false alarms during the Sep-07 review).
- **Design:** re-capture `status.json` (overwrite or `status.final.json`) at
  teardown in both scripts.

### 1.7 `floored_max_tokens` logged at WARN — COSMETIC

- [x] Status (pending commit)
- **Problem:** native-reasoning floor (`thinking/mod.rs:166-185`) fires WARN on
  every think-on request with small `max_tokens` (24 hits in one bench run).
  It's by-design behaviour → noise.
- **Design:** demote `warn!` → `info!`.

---

## Batch 2 — agent-API correctness (target: v0.1.11)

### 2.1 Ollama translator drops `tools` and `format`

- [x] Status (pending commit)
- **Problem:** `translate_ollama_to_openai` (`src/server/ollama.rs:381`) copies
  model/messages/stream/think/options only. Ollama clients (Open WebUI,
  Continue) sending `tools` / `format` (incl. JSON-schema) silently lose
  function calling and structured output. Native Ollama supports both.
- **Design:** pass through `tools` → `tools`, `format` →
  `response_format` (`"json"` → `{"type":"json_object"}`, schema object →
  `{"type":"json_schema", ...}`); translate `tool_calls` back in the response
  path (check what the Ollama response translator does today).

### 2.2 OpenAI-path tool calling / `response_format`: zero test coverage

- [x] Status (pending commit)
- **Problem:** proxy forwards the fields and accumulates `delta.tool_calls`
  (`proxy.rs:352-480`) but no test exercises them; llama.cpp b9861 supports
  both natively. Unproven ≠ working.
- **Design:** add TC-E16 (tools round-trip incl. streaming) and TC-E17
  (`response_format: json_schema`) to `multi_model_e2e.sh`/`.ps1` using a
  tool-capable catalog model (qwen3.5 family).

### 2.3 Concurrent-chat e2e + tok/s and TTFT surfacing

- [x] Status (pending commit)
- **Problem:** e2e only proves concurrent *embed*; chat concurrency
  (engine `--parallel`, daemon semaphore 4) is unproven. TTFT/tok/s are not
  surfaced anywhere (hidden inside wall-time aggregates).
- **Design:** TC-E18: N=4 parallel chat, assert no 503/serialisation collapse.
  Emit TTFT + completion tok/s in chat responses' `usage`/logs where cheap.

### 2.4 llama.cpp perf flags: `--flash-attn` (+ optional KV-cache quant)

- [x] Status (pending commit)
- **Problem:** spawn args pass neither; measured decode ~67 % of bandwidth
  ceiling — typical *without* FA. Cheap win on 8B@16k workloads.
- **Design:** add `--flash-attn on` for CUDA variant loads (guard: b9861
  accepts the flag — verify against bundled build before enabling for Vulkan/
  CPU); KV-quant (`--cache-type-k/v q8_0`) only behind an env/config opt-in.

---

## Batch 3 — Responses API (target: v0.2.x)

### 3.1 `/v1/responses` (non-stateful adapter)

- [ ] Status
- **Problem:** Ollama (v0.13.3+), LM Studio, llama-server all ship it; agent
  SDKs are migrating from chat-completions.
- **Design:** thin translation layer over the existing chat path: `input` →
  `messages`, `max_output_tokens` → `max_tokens`, `tools` passthrough, map
  streaming events (`response.output_text.delta` etc.). No
  `previous_response_id` state in v1 — return 400 for it, document.

---

## Explicitly deferred (re-evaluate on demand)

- ROCm engine (AMD covered by Vulkan), on-device quantize, LoRA adapters
  (vLLM opt-in covers), llama.cpp multi-GPU `--tensor-split` (vLLM TP covers),
  CUDA Docker image (tracked separately in README §container).

## Validated as at/above industry bar (no action)

Multi-model residency, VRAM + host-RAM admission & LRU eviction, keep_alive
idle unload, thinking two-call workflow, MTP, VLM (text/remote/base64),
rerank, catalog pull with resume+sha256, systemd user service, Prometheus
`/metrics`, measured perf on RTX 5060 Ti (decode/embed/cold-load all in the
normal band for llama.cpp-class engines).
