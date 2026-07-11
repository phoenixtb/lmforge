# Annual engine review

> **Maintainer-only.** Cadence checklist for re-evaluating the engine tier
> roster. Contributors building or debugging engines should start with
> [`ADR-001`](../architecture/ADR-001-engine-tiers.md) instead.

**Cadence:** every 12 months, plus on any of the [re-evaluation triggers](#re-evaluation-triggers) below.
**Owner:** project maintainer (currently @phoenixt).
**Source of truth:** [`docs/architecture/ADR-001-engine-tiers.md`](../architecture/ADR-001-engine-tiers.md), [`data/engines.toml`](../../data/engines.toml).

The engine roster is **not** static. New compute capabilities ship, upstream projects rotate hot/cold, and quant formats die. This review is the forcing function that keeps the `default` / `opt-in` / `experimental` partition honest.

## When to run

1. **Annual checkpoint** — first review every January. Set a calendar reminder.
2. **Triggered review** — any of the following also forces a review:
   - A new NVIDIA architecture ships (sm\_X20 / sm\_X30 etc.) AND `llama.cpp` releases a binary for it.
   - An engine currently in `experimental` ships a release with sm\_120 (or the latest consumer arch) in its prebuilt kernel matrix.
   - `vLLM` reaches `>= 0.12` AND publishes a Blackwell-supported wheel without source-build steps.
   - `llama.cpp` removes or breaks `--cache-ram` / `llama-server` (would force a default-tier swap).
   - A new top-1 model family (next-gen Qwen / Llama / DeepSeek) ships without a viable GGUF quant.

## Review checklist

Run the questions below for **every** engine currently in `data/engines.toml`, plus any new candidate the community is pushing into the issue tracker.

### 1. Hardware coverage

- [ ] Compute capability window: does `min_compute_cap` / `max_compute_cap` still match the engine's actual prebuilt kernel matrix? Verify against the upstream release page, not docs.
- [ ] `supported_os_families`: did upstream drop or add any OS this cycle?
- [ ] Does upstream still publish prebuilt wheels / binaries, or did they regress to source-only builds?

### 2. Performance + feature parity

- [ ] OpenAI-compatible `/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank` (if applicable) still work?
- [ ] Prefix caching (`--cache-ram` / `--enable-prefix-caching` / equivalent) still present?
- [ ] KV multiplexing — does the engine still serve multiple concurrent models, or has it regressed to single-model-only?
- [ ] Quantization formats supported: 4-bit / 8-bit / FP8 / native-FP16 — match against the model catalog's `:NNbit` suffixes.

### 3. Project health

- [ ] Last release date — anything older than 6 months counts as **stale**; flag for demotion.
- [ ] Open critical issues in the last 90 days — look for "fails to load on $YOUR_GPU".
- [ ] License — has it changed (e.g. moved to a non-permissive license)?
- [ ] Security advisories — any CVEs unpatched in `main`?

### 4. Tier placement

After answering 1-3, place each engine in one of:

| Tier | Criteria |
| --- | --- |
| `default` | Bundled with `lmforge`. Works on every `supported_os_families` × `supported_compute_cap` pair without user intervention. Ships its own runtime. Zero external dependencies (no system Python, no CUDA toolkit). |
| `opt-in` | Requires explicit `--engine X`. Has a clear performance / feature win vs default on specific hardware. Install is automated by `lmforge` (uv venv + pip, or download + extract). |
| `experimental` | Known to be broken on at least one supported hardware/OS combo. Users must `--yes-experimental` to use it. Demote here BEFORE a release if any review question turned up red. |

### 5. Catalog impact

- [ ] If you demoted or removed an engine, scan the matching catalog (`gguf.json` / `safetensors.json` / `mlx.json` / `exl3.json`) for shortcut/repo pairs that are now orphaned.
- [ ] Run `scripts/util/pre-commit-check-catalog.sh --all` to spot drift.
- [ ] Verify `bundle-llamacpp.sh` (or its sibling) still pins a version supporting the current consumer arch.

## Re-evaluation triggers (auto-promote `sglang` worked example)

The Phase 1 demotion of SGLang to `experimental` is documented in [ADR-001](../architecture/ADR-001-engine-tiers.md#consequences). To re-promote, **all four** must be true:

1. SGLang ships prebuilt `sgl_kernel` wheels for the dominant consumer arch (currently sm\_120) **and** the next-gen datacenter arch (sm\_103+).
2. SGLang's CUDA wheel index matches the user's installed CUDA major version (no `libnvrtc.so.X` mismatch).
3. Decode throughput on the project's reference RTX 5060 Ti benchmark beats `llamacpp + --cache-ram` by >= 25%, **OR** vLLM by >= 15%, on Qwen3-8B / Qwen3-32B / DeepSeek-V3-distill.
4. Last release within 90 days.

If 1+2+4 are true but 3 is false, SGLang stays in `experimental`. If 3 is true but 1 or 2 is false, the engine is unshippable to the install base.

## Closing the review

1. Update `data/engines.toml`:
   - Set new `tier`, `min_compute_cap`, `max_compute_cap`, `version`.
   - Tag re-evaluation comment with the review date so the next reviewer has a breadcrumb.
2. Update [ADR-001](../architecture/ADR-001-engine-tiers.md): add a row to the **Consequences** changelog at the bottom (tier flip, version bump, etc.).
3. If a model catalog had to shrink, regenerate it with `scripts/util/pre-commit-check-catalog.sh --gated-probe`.
4. Run the full test matrix:
   ```bash
   cargo test --offline
   ./scripts/util/dev_test.sh --unit --catalog
   ```
5. Open a single PR titled `engine: $YEAR annual review`. Reviewers should cross-check the ADR changelog against the `engines.toml` diff.

## Inputs to consult

- Upstream release pages: `llama.cpp`, `vLLM`, `ExLlamaV3`, `SGLang`, `TensorRT-LLM`, `MLC-LLM`, `TGI`, `lmdeploy`.
- NVIDIA developer blog (Blackwell / Rubin announcements).
- Hugging Face's "leaderboard" pages for quant format adoption.
- The project's own `~/.lmforge/logs/engine-*.stderr.log` over the last release cycle — recurring crashes localise the weakest engine.
