# ADR-006 — Engine Residency Strategies

**Status:** Accepted  
**Date:** 2026-06-30  
**Author:** lmforge core team

---

## Context

LMForge's `EngineManager` spawns and supervises inference backends. The original
design (SRS §4.4) had two distinct models:

- **oMLX (Apple Silicon):** one shared `omlx serve --model-dir <parent>` process;
  the request `model` field selects which weights; oMLX's native in-process pool
  handles LRU/TTL.
- **llama.cpp / SGLang (all other platforms):** one process per loaded model,
  each on its own TCP port; LMForge manages admission control and LRU eviction.

A "strict-adapter" refactor (`docs/archive/implementation-plan-omlx-adapter.md`)
inadvertently forced the per-process pattern onto oMLX too, causing each cold load
to spawn another `omlx serve --model-dir <parent>` instance. Because every instance
received the **same** parent directory, N loaded models produced N redundant processes
each discovering all models — contrary to the design intent and wasteful of memory
and file descriptors.

Phase 0 empirical validation confirmed (see `docs/archive/OMLX_SHARED_SERVER_FINDINGS.md`):
- One `omlx serve --model-dir <dir>` discovers all model subdirs at startup.
- Requests to the same port with different `model` fields load different weights on-demand.
- Embeddings, reranking, VLM, and reasoning all work from one process.
- Discovery is startup-only; a newly-pulled model requires a server restart.

---

## Decision

Introduce **residency** as a first-class abstraction (`src/engine/residency.rs`):

```
enum ResidencyKind { SharedServer, ProcessPool }

trait Residency {
    fn kind() -> ResidencyKind;
    async fn ensure_model(model_id, keep_alive_override, for_request) -> Result<ModelHandle>;
    async fn unload_model(model_id);
    async fn unload_all();
    async fn heartbeat_tick();
    fn state() -> Arc<RwLock<EngineState>>;
}
```

Two concrete strategies:

### `ProcessPoolResidency` (unchanged pool logic)

- One OS process per loaded model.
- LMForge manages VRAM admission (`evict_for_memory`), LRU TTL sweep, crash reaping,
  speculative-decoding S-2.8 retry, calibration feedback.
- Used by: llama.cpp (Linux/Windows production default), vLLM, TabbyAPI.
  SGLang reuses this path when explicitly installed (`tier = experimental`).

### `SharedServerResidency` (new, oMLX)

- **Lazy start:** `omlx serve --model-dir <models_dir> --port <base_port>` on first
  `ensure_model` call. Stays resident.
- **Model routing:** `ensure_model` verifies the model dir on disk, checks
  `/v1/models` for discovery. If newly-pulled model is missing from oMLX's list,
  restarts the server (triggering a rescan).
- **No LMForge eviction:** oMLX's `EnginePool` owns LRU/TTL. The keepalive tracker
  is exempt for `engine_id = "omlx"` (already correct).
- **Crash supervision:** `heartbeat_tick` detects exited process, clears state; next
  `ensure_model` restarts lazily.
- **Status sync:** `heartbeat_tick` calls `/v1/models` to update `running_models` so
  `/lf/status` reflects what oMLX actually knows about.
- **`unload_model` semantics:** advisory only. The model is removed from LMForge's
  status view; oMLX evicts it under memory pressure. A warning is logged and
  surfaced in the API response.
- **`unload_all`:** kills the server process. Next `ensure_model` restarts it with
  fresh discovery.

### Dispatch

`EngineManager` holds a `ResidencyInstance` enum (static dispatch, same pattern as
`EngineAdapterInstance`). Selection:

```
oMLX + LMFORGE_OMLX_SHARED != "0"  →  SharedServerResidency
everything else                     →  ProcessPoolResidency
```

Both strategies return a uniform `ModelHandle { port, inflight }` — the proxy layer
and all callers are unaware of which is active.

---

## Consequences

### Positive

- **oMLX process count:** 1 shared process (vs N per loaded model). Idle model
  accumulation bug resolved at the root.
- **Memory:** oMLX's own Metal memory guard and SSD-tiered KV cache work correctly
  (they operate inside the single process).
- **Correctness:** LMForge no longer applies a TTL sweep that contradicts oMLX's
  native residency policy.
- **Extensibility:** adding a new residency strategy (e.g. for a future multi-process
  llama.cpp pool) requires implementing `Residency` and adding a variant to
  `ResidencyInstance` — no changes to callers.

### Negative / Trade-offs

- **`unload_model` is advisory for SharedServer.** Clients calling
  `POST /lf/model/unload` on an oMLX engine cannot force immediate eviction.
  The API response surfaces this clearly.
- **Pull → discovery requires restart.** Newly-pulled models cause a brief server
  restart (loss of in-memory KV caches). This is inherent to oMLX's startup-scan
  model and is the simplest reliable discovery strategy.
- **Admin API not usable.** oMLX 0.4.4's `/admin/api/models/{id}/load` requires
  a browser-session cookie; not automatable by LMForge without implementing an
  auth flow. Restart is the only reliable discovery trigger.

---

## Alternatives considered

### Keep ProcessPool for oMLX (status quo)

Rejected: contradicts the foundation principle; causes process accumulation and
memory waste; the exemptions added for oMLX (keepalive skip) were workarounds not
fixes.

### Replace oMLX with bare `mlx_lm.server`

Rejected: `mlx_lm.server` is single-model/text-only. Re-implementing oMLX's pool,
LRU, VLM, embeddings, and reranking is a larger and riskier change. oMLX 0.4.4 is
the correct tool; the problem was how LMForge managed it. Documented in
`docs/archive/OMLX_SHARED_SERVER_FINDINGS.md`.

### `Box<dyn Residency>` (dynamic dispatch)

Rejected in favour of `ResidencyInstance` enum (static dispatch) to avoid vtable
overhead and align with the existing `EngineAdapterInstance` pattern.

---

## References

- `src/engine/residency.rs` — `Residency` trait + `ResidencyKind`
- `src/engine/process_pool.rs` — `ProcessPoolResidency`
- `src/engine/shared_server.rs` — `SharedServerResidency`
- `src/engine/manager.rs` — `EngineManager` (thin dispatcher)
- `docs/archive/OMLX_SHARED_SERVER_FINDINGS.md` — Phase 0 empirical spike results
