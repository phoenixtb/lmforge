# ADR-008 — “Pool” Is Residency, Not a Separate Subsystem

**Status:** Accepted  
**Date:** 2026-07-02  
**Author:** lmforge core team  
**Related:** [ADR-006](./ADR-006-engine-residency.md) · [ADR-007](./ADR-007-thinking-pipeline.md)

---

## Context

Contributors and docs sometimes refer to a **“pool workstream”** as if it were a
greenfield feature (llama.cpp + SGLang multi-model serving). In the shipped
architecture, **pool behaviour is already implemented** as **engine residency**
(ADR-006). The Mac workstream completed **SharedServer** for oMLX; the
Linux/Windows workstream is **validation and hardening** of **ProcessPool** for
llama.cpp — not a new pool design.

SGLang is **not first-class**. Production defaults:

| Platform | Default engine | Residency |
|---|---|---|
| macOS (Apple Silicon) | oMLX | `SharedServerResidency` |
| Linux | llama.cpp (CUDA / Vulkan / CPU variant) | `ProcessPoolResidency` |
| Windows | llama.cpp (CUDA / Vulkan / CPU variant) | `ProcessPoolResidency` |

Experimental engines (SGLang, vLLM, TabbyAPI) reuse `ProcessPoolResidency` when
explicitly installed; they are out of scope for release gates unless promoted in
ADR-001.

---

## Decision

1. **Use “residency” in architecture docs**, not “pool”, except when quoting
   oMLX’s internal `EnginePool` or historical discussion.
2. **Do not build a llama.cpp SharedServer.** `llama-server` is one-model-per-process;
   `ProcessPoolResidency` is the correct mapping.
3. **First-class validation matrix** (operator lab — not CI-enforced):

| Host | GPU | Engine | Residency | Primary gate |
|---|---|---|---|---|
| macOS (dev) | Metal | oMLX | SharedServer | `think_bench --assert` + `multi_model_e2e.sh` |
| Ubuntu 24 | RTX 5060 Ti 16 GB (passthrough) | llama.cpp CUDA | ProcessPool | Same + burst co-residency |
| Windows 11 | RTX 5060 Ti 16 GB (passthrough) | llama.cpp CUDA | ProcessPool | `lmforge.ps1 e2e -Source local` |
| Fedora | none | llama.cpp CPU/Vulkan | ProcessPool | `multi_model_e2e.sh --no-burst` (eviction path) |
| Windows 11 | none | llama.cpp CPU/Vulkan | ProcessPool | Same as Fedora |

**CUDA first** on the NVIDIA VMs; CPU/Vulkan validates RAM admission and sequential
eviction when VRAM is unavailable.

4. **Develop residency/thinking changes on macOS** (Rust + scripts). **Run llamacpp
   gates on Linux/Windows VMs** for variant install, DLL/Vulkan quirks, and
   ProcessPool integration — platform bugs are fixed on the machine that reproduces them.

---

## What ProcessPool already owns

Implemented in `src/engine/process_pool.rs` and documented in
[ARCHITECTURE.md §3](./ARCHITECTURE.md#3-model-lifecycle):

- One OS process + TCP port per loaded model
- VRAM/RAM admission (`evict_for_memory`) before spawn
- LRU eviction of idle slots only (`inflight == 0`)
- 503 reject when all slots are busy (no OOM kill of live requests)
- Keepalive TTL, crash reaping, calibration feedback, spec-dec downgrade retry

Callers receive `ModelHandle { port, inflight }` regardless of residency kind.

---

## What SharedServer owns (oMLX only)

Implemented in `src/engine/shared_server.rs`:

- Lazy single `omlx serve --model-dir <parent>`
- Model routing via request `model` field
- oMLX native LRU/TTL (LMForge keepalive exempt)
- Pull → discovery via controlled server restart

---

## Consequences

### Positive

- No duplicate “pool” subsystem to design or maintain.
- Clear ownership: engine-native multi-model → SharedServer; single-model backends → ProcessPool.
- Validation matrix maps 1:1 to available lab hosts.

### Negative / limits

- llama.cpp embed + chat = two processes (higher RAM than oMLX shared server).
- Linux/Windows thinking on llamacpp (`inline_think` orchestrator) must be validated on
  real `llama-server` — Mac cannot substitute for that gate.

---

## References

- `src/engine/residency.rs` — trait + `ResidencyKind`
- `src/engine/process_pool.rs` — ProcessPool
- `src/engine/shared_server.rs` — SharedServer
- `src/engine/manager.rs` — dispatch (`LMFORGE_OMLX_SHARED=0` debug fallback)
- `docs/dev/RELEASE.md` — per-platform release smoke
