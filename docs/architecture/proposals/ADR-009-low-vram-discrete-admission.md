# ADR-009 — Low-VRAM Discrete GPU Admission & Offload

**Status:** Proposed
**Date:** 2026-07-06
**Author:** lmforge core team
**Related:** [ADR-006](../ADR-006-engine-residency.md) · [ADR-008](../ADR-008-pool-residency.md) · [ADR-005](../ADR-005-speculative-decoding.md)

> Lives under `architecture/proposals/` until Accepted. Not scheduled.

---

## Context

Common target we do not serve well today: a **Windows/Linux box with ~8 GB
system RAM and a low-power discrete NVIDIA GPU (GTX/RTX) with ~4 GB VRAM**.
This ADR captures the current behaviour, the structural gaps, and a proposed
plan. It is analysis + intent — not yet accepted or scheduled.

### How LMForge sees this box

- `hardware/vram.rs` → `vram_gb ≈ 3.5` (4096 MiB − 512 MB reserve).
- `total_ram_gb ≈ 8`; **available RAM is not stored** in `HardwareProfile`
  (only sampled live at load via `free_system_ram_gb()`).
- Vendor = NVIDIA **discrete** (`unified_mem = false`). This flag routes the
  load into the strict discrete-GPU path.
- Engine auto-selects **llamacpp / GGUF** (CUDA build on Windows;
  cuda12-or-Vulkan on Linux — see ADR-004).

---

## Current behavior (end to end)

1. **`init`** (`cli/init.rs`) prints an advisory `Quant tier: Q4_K_S`
   (`vram.rs::quant_tier`, 3.5 GB → `>=3.0` branch). It does **not** pick a
   model, filter the catalog, or wire a default (`default_chat_model` stays
   empty and is unused at runtime).
2. **Pull** (`model/pull.rs`) has **no fit check** — an 8B GGUF downloads with
   no warning.
3. **Load** (`engine/process_pool.rs::ensure_model_inner`):
   - `llamacpp::plan_runtime` (`engine/adapters/llamacpp.rs:656+`) computes
     `-ngl` as a **proportional blob split**: `budget = free_vram − 1 GB
     scratch`; full offload (`99`) if it fits, else
     `floor(budget/needed * 99)` clamped 1–98.
   - **But** the discrete-GPU **hard gate** (`process_pool.rs:707-720`) rejects
     the load when `free_now < weights + KV + scratch` — the **full** footprint
     (`VramFootprint::effective_total_gb`), which is VRAM-placement-agnostic.
     The gate runs *before* spawn and does **not** account for partial offload.

```
let needed_final = plan.footprint.effective_total_gb();
let discrete_gpu = !matches!(profile.gpu_vendor, GpuVendor::None) && !profile.unified_mem;
if discrete_gpu && free_now < needed_final {
    bail!("Insufficient VRAM to load '{}': needs ~{:.1} GB ... only {:.1} GB free ...");
}
```

### Outcome on ~3.5 GB VRAM

| Model | Rough footprint (weights+KV+scratch) | Today |
|---|---|---|
| `qwen3:1.7b:4bit`, small ctx | ~1.5–2 GB | loads, `-ngl 99` |
| `qwen3:4b:4bit`, small ctx | ~2.8–3.5 GB | borderline; rejected as ctx/KV grows |
| `qwen3:8b:4bit` | ~5–6 GB | **hard-rejected** |

The all-or-nothing gate is **intentional**: the 2026-07-06 Blackwell incident
showed WDDM silently paging VRAM overflow into sysmem — 4–6× decode slowdown and
corrupted output. So the conservative reject is defensible. The problem is it
*also* kills the deterministic, we-control-it offload and the CPU fallback.

---

## Gaps

- **G1 — partial offload is effectively dead on discrete GPUs.** The
  proportional `-ngl` path can never fire for a model larger than VRAM; the gate
  rejects first. It only ever yields `99`.
- **G2 — no CPU fallback for discrete-GPU hosts.** A GPU-*less* 8 GB box runs a
  4B/7B Q4 on CPU via `cpu_residency_free` (slow but works). The same box *with*
  a weak GPU is excluded from that path (`discrete_gpu` branch) and bails.
  Net: adding a 4 GB GPU makes the machine run **fewer** models.
- **G3 — quant tier is advisory only.** No mapping `Q4_K_S → shortcut`, no
  catalog filter, `default_chat_model` unused. User discovers fit by
  trial-and-error rejections.
- **G4 — context size is never a fit lever.** KV dominates at 4 GB; nothing
  auto-reduces ctx (32k → 8k → 4k) before rejecting.
- **G5 — no pull-time preflight.** GBs downloaded before the load-time reject.
- **G6 — RAM + VRAM are not modeled as a combined budget** for a split; and
  available RAM isn't in the profile.

---

## Decision (proposed)

Core reframe: **distinguish "driver silently spills" (unsafe — keep rejecting)
from "we deterministically place N layers on GPU, rest on CPU" (safe — allow).**
The current gate conflates the two.

1. **Hardware-aware recommendation, not just a quant string.** Map profile →
   concrete shortcut (4 GB VRAM / 8 GB RAM → e.g. `qwen3:1.7b:4bit`, or
   `4b:4bit` at reduced ctx) and sort the UI catalog into *fits /
   fits-with-offload / CPU-only / won't-fit*.
2. **First-class deterministic partial offload for low-VRAM discrete.** Size
   `-ngl` from **real GGUF layer geometry** (per-layer bytes), not a blob ratio.
   Admit a GPU+CPU split when *(GPU portion ≤ free VRAM)* **and** *(CPU portion +
   KV ≤ `cpu_residency_free`)*. Keep the loud reject only for the genuine spill
   case.
3. **CPU fallback on discrete GPUs.** When a model won't fit VRAM but fits system
   RAM, route through the same `cpu_residency_free` path GPU-less hosts use,
   behind an explicit "will run at ~X t/s on CPU" warning — not a hard bail.
4. **Context autoscaling as a fit lever.** Before rejecting, try dropping ctx to
   fit KV; surface the chosen ctx.
5. **Pull-time preflight.** Warn/confirm before downloading a model that will be
   CPU-bound or won't fit, reusing the load-time estimator.
6. **Combined RAM+VRAM budget + honest messaging.** Track available RAM in the
   profile; present "hybrid mode, ~X t/s" / "CPU-only, ~Y t/s" up front instead
   of a bare "Insufficient VRAM".

---

## Consequences

### Positive

- The 8 GB / 4 GB box runs useful models instead of hard-rejecting everything
  above ~3.5 GB.
- Adding a weak GPU never makes a host *worse* than GPU-less.
- Users get a recommended model + honest speed expectation, not trial-and-error.

### Negative / risks

- Partial offload + CPU fallback are genuinely slow; must be clearly labelled to
  avoid "LMForge is slow" perception.
- Deterministic split still touches the driver allocation path that caused the
  Blackwell incident — needs validation on real hardware before the gate is
  loosened (see ADR-008 validation matrix; add a 4 GB-class GPU host).
- Per-layer geometry estimation adds GGUF-metadata parsing to the load path.

---

## References

- `src/engine/process_pool.rs` — `ensure_model_inner`, discrete-GPU gate (707–720)
- `src/engine/adapters/llamacpp.rs` — `plan_runtime` / `-ngl` planner (656+)
- `src/hardware/vram.rs` — `VramFootprint`, `quant_tier`, `cpu_residency_free`
- `src/hardware/probe.rs` — `HardwareProfile` (no available-RAM field)
- `src/cli/init.rs` — quant-tier advisory + below-min warning
- ADR-006 (residency), ADR-008 (pool = residency + validation matrix)
