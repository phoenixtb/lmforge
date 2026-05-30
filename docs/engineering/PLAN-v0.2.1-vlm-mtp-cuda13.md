# v0.2.1 — VLM + MTP on cuda13

**Status:** planning  
**Depends on:** v0.2.0 cuda13 tarball validated (`TEST-v0.2.0-post-tarball.md`)  
**Host target:** RTX 5060 Ti 16 GB, `LMFORGE_LLAMACPP_VARIANT=cuda13`

---

## Goal

End-to-end on cuda13:

1. VLM pull includes `mmproj-*.gguf` → image chat works  
2. When upstream allows, MTP speculative decoding on vision-capable models with measurable `spec_stats`

---

## Current state (2026-05-30)

| Area | Status |
|------|--------|
| cuda13 chat + MTP (`qwen3.5:4b:mtp:4bit`) | ✅ live — ~93% accept rate |
| cuda13 embed co-load | ✅ `multi_model_e2e` green |
| cuda13 VLM text-only (`qwen2.5-vl:3b:4bit`) | ✅ chat OK |
| cuda13 VLM **image** input | ❌ 500 — mmproj not on disk |
| VLM + MTP combined | ❌ **blocked upstream** |

### Upstream constraint (llama.cpp b9351 / Unsloth docs)

Unsloth explicitly states for MTP builds:

> `-np > 1` and **`--mmproj` are not yet supported with MTP**.

LMForge always passes `--mmproj` for VLMs and may use `n_parallel > 1`. Enabling `draft-mtp` on a loaded VLM is expected to fail or silently break until llama.cpp merges mmproj + MTP compatibility.

**Implication:** v0.2.1 splits into **VLM vision fix** (shippable now) and **VLM+MTP** (track upstream, enable when safe).

---

## Phase V-1 — VLM pull + spawn fix (~1 day)

**Problem:** `select_gguf_files()` pulls one quant-matched weight file; `mmproj-*.gguf` is filtered out of the main selection but **never added to the download list**. Index shows `vision=true`, `mmproj_path=null`.

### Tasks

- [ ] **V-1.1** After `select_gguf_files()`, if resolved model is VLM (catalog `:vl:` hint or repo name), append all `mmproj-*.gguf` siblings from HF repo to download list (`resolver.rs` + test).
- [ ] **V-1.2** Re-run `detect_capabilities` post-pull so `mmproj_path` is populated (`pull.rs` / index).
- [ ] **V-1.3** Spawn path already wires `--mmproj` when sidecar exists (`llamacpp.rs`) — verify on cuda13 with `qwen2.5-vl:3b:4bit`.
- [ ] **V-1.4** Live: image chat (1×1 PNG base64) returns non-empty response; TEST plan V-4 passes.

### Acceptance

```bash
LMFORGE_LLAMACPP_VARIANT=cuda13 lmforge pull qwen2.5-vl:3b:4bit --refresh
ls ~/.lmforge/models/*/mmproj*.gguf   # must exist
# image chat → 200, non-empty content
```

---

## Phase V-2 — Spec policy for VLMs (~0.5 day)

Even after V-1, do **not** auto-enable MTP on VLMs until upstream supports `--mmproj` + `draft-mtp`.

### Tasks

- [ ] **V-2.1** In `speculative::resolve`, if `capabilities.vision == true`, force `SpecMode::Off` with reason `VLM: MTP+mmproj not supported upstream (b9351)` unless env `LMFORGE_SPECULATIVE_MODE=mtp` explicitly overrides (power-user escape hatch).
- [ ] **V-2.2** Document in ADR-005 + INSTALL_LINUX_DEV.
- [ ] **V-2.3** `/lf/status.spec_mode=off` for loaded VLM; no S-2.8 false retry loop.

---

## Phase V-3 — Track upstream VLM+MTP (~ongoing)

### Watch

- llama.cpp releases after b9351: changelog for `--mmproj` + `--spec-type draft-mtp` combo
- Unsloth `*-MTP-GGUF` repos for **VL** families (today: text MTP repos only, e.g. `Qwen3.5-4B-MTP-GGUF`; no `Qwen2.5-VL-*-MTP-GGUF` in catalog)
- Qwen3.5 **unified** VL foundation (single GGUF may carry both VL + nextn tensors — probe via `gguf_inspect`)

### Spike when upstream lands

1. Find GGUF with verified `nextn.*` **and** vision tensors (or unified Qwen3.5-VL MTP repo)
2. Pin new llama.cpp tag; rebuild cuda13 tarball
3. Live matrix: image prompt + `spec_mode=mtp` + `spec_stats` on cuda13
4. VRAM budget: MTP context + mmproj + weights on 16 GB — tune `--spec-draft-n-max` (likely 2–4, not 16)

---

## Phase V-4 — Catalog + shortcuts (when models exist)

| Shortcut (draft) | Repo | Notes |
|--------------------|------|-------|
| `qwen3.5-vl:4b:mtp:4bit` | TBD on HF | unified VL+MTP if Unsloth ships |
| `qwen2.5-vl:3b:4bit` | existing | vision only until MTP-VL GGUF exists |

Add to `gguf.json` only after probe confirms `nextn` tensors on a VLM pull.

---

## Test matrix (cuda13, post V-1)

| # | Model | Vision | MTP | Pass |
|---|-------|--------|-----|------|
| T-1 | `qwen2.5-vl:3b:4bit` | image chat | off | non-empty response |
| T-2 | `qwen2.5-vl:3b:4bit` | text chat | off | baseline |
| T-3 | `qwen3.5:4b:mtp:4bit` | n/a | on | existing MTP suite |
| T-4 | future VLM+MTP GGUF | image chat | on | deferred — upstream |

---

## Sequencing

```
v0.2.0 tarball sign-off (cuda13) ──► V-1 mmproj pull ──► V-2 spec guard
                                              │
                                              ▼
                                    V-3 upstream watch ──► V-4 catalog + live VLM+MTP
```

**v0.2.1 ships:** V-1 + V-2. **VLM+MTP combo:** V-3/V-4 when llama.cpp + HF models ready.

---

## Risks

| Risk | Mitigation |
|------|------------|
| mmproj quant mismatch (f16 proj + Q4 weights) | Pull mmproj matching repo layout; prefer same publisher file naming |
| 16 GB VRAM: VLM + MTP OOM | Lower `spec-draft-n-max`, `-c`, document in doctor |
| User forces MTP on VLM via env | Escape hatch OK; S-2.8 fallback + clear `last_errors` message |
