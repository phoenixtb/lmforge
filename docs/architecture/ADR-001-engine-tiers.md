# ADR-001: Engine tier model

- **Status:** Accepted (2026-05-27)
- **Supersedes:** the implicit "SGLang on Linux+NVIDIA / oMLX on macOS / llama.cpp fallback" policy that existed before
- **Stakeholders:** core, install, UI, release

## Context

Up through May 2026, `lmforge` shipped three engines selected by a flat priority list:

| Engine | OS | GPU | Priority | Role |
| --- | --- | --- | --- | --- |
| oMLX | macOS aarch64 | Apple | 10 | Default on Apple Silicon |
| SGLang | Linux x86_64 | NVIDIA | 20 | Default on Linux+NVIDIA ≥ 8 GB VRAM |
| llama.cpp | any | any | 100 | Fallback |

Two issues forced this revision:

1. **SGLang broken on Consumer Blackwell (sm_120).** The upstream `sgl-kernel`
   wheels ship `sm_90` and `sm_100` cubins only; an RTX 5060 Ti / 5090 user
   gets a working install but every chat-completion hangs at first token.
   Tested combinations: SGLang 0.5.10.post1 + CUDA 12.8/13.0 + driver 580.95
   on a 5060 Ti — all fail with `ImportError: libnvrtc.so.12: cannot open
   shared object file` (cu130 toolkit ships `libnvrtc.so.13`) or
   `Could not load any common_ops library!` (no sm_120 binary). See
   `docs/temp/engine_research_in_cur.md` for the reproduction.
2. **No platform matrix.** The flat priority list cannot express "this engine
   works on Linux + WSL2 but not native Windows," so the install path
   silently degrades on unsupported hardware instead of refusing cleanly.

## Decision

Adopt a **three-tier engine model** with a first-class platform matrix.

### Tier definitions

- **`default`** — bundled with `lmforge` releases. No Python, no `pip`, no
  network at first run after the install tarball is extracted. Currently:
  `llama.cpp` (CUDA on NVIDIA, Metal on Apple, Vulkan on AMD, CPU
  elsewhere) and `omlx` (Apple Silicon).
- **`opt-in`** — high-performance specialised engines. **Never** installed
  automatically. User runs `lmforge engine install <id>`. Lives in its own
  isolated venv at `~/.lmforge/engines/<id>/venv/`. Verified-download with
  SHA256-pinned wheels.
- **`experimental`** — kept in the codebase as cheap insurance but **never**
  auto-selected. User must pass `--engine <id>` explicitly and accept a
  prompt. Documented re-evaluation trigger so we know when to promote.

### Engine roster

| Engine | Tier | Supported `(os_family, gpu_vendor, compute_cap)` | Catalog |
| --- | --- | --- | --- |
| `llamacpp` | default | every platform | `gguf.json` |
| `omlx` | default | `(darwin, apple, *)` | `mlx.json` |
| `vllm` | opt-in | `(linux \| windows-wsl2, nvidia, sm75+)` | `safetensors.json` |
| `exl3` | opt-in | `(linux \| windows-native \| windows-wsl2, nvidia, sm75+)` | `exl3.json` |
| `sglang` | experimental | `(linux, nvidia, sm90..=sm103)` | `safetensors.json` |

### Hardware probe extensions

`hardware.json` schema v2 captures the dimensions the tier matcher needs:

- `compute_cap: Option<(u8, u8)>` — parsed from `nvidia-smi --query-gpu=compute_cap`
- `cuda_runtime_version: Option<String>` — `nvcc --version`, fallback to `nvidia-smi`
- `cuda_driver_version: Option<String>` — `nvidia-smi --query-gpu=driver_version`
- `os_family: OsFamily` — `linux | windows-native | windows-wsl2 | darwin`
- `is_wsl: bool` — derived from `/proc/sys/kernel/osrelease`
- `gpu_count: u8` — `nvidia-smi -L | wc -l`
- `schema_version: u32` — bumped to `2`

Engine selection consumes these fields; the user never re-states their
hardware.

### Selector rule

```
1. Filter engines by tier ∈ {default, opt-in_already_installed, experimental_if_--engine_X}
2. Filter by supported_platforms matches probed (os_family, gpu_vendor, compute_cap)
3. Filter by min_vram_gb ≤ probed.vram_gb
4. Sort by priority (lower = better); ties broken by id (deterministic).
```

`default` engines are always considered. `opt-in` engines are considered
only after `lmforge engine install <id>` succeeded (we check for the venv).
`experimental` engines are considered only when `--engine <id>` is passed
explicitly *and* the user confirmed the warning prompt.

## Consequences

### Positive

- **P0 fix.** RTX 50-series users (consumer Blackwell, sm_120) get a working
  chat on the first run — `llama.cpp` ships pre-built `sm_120` cubins and
  has done so since `b8870`.
- **Zero Python in the default path.** Cuts install-failure surface on
  fresh boxes by ~90% — no `python3-venv`, no `ensurepip`, no `flash-attn`
  pre-release dance.
- **Honest OS support.** Native Windows users never see `--engine vllm` as
  an option; they get `llama.cpp` or `exl3`. WSL2 users get the full menu.
- **Per-engine venv isolation.** A broken vLLM install cannot poison EXL3
  (or vice versa). The `uv`-managed venv at `~/.lmforge/engines/<id>/venv/`
  is the unit of teardown via `lmforge clean --engines`.
- **Re-evaluation triggers in source.** Three-line trigger block per dropped
  engine in `data/engines.toml` (or this ADR for fully-dropped ones).
  Future contributors don't have to re-run this investigation.

### Negative / costs

- **Bundle size grows.** Linux x64 GA tarball gains ~250 MB for the
  `llama.cpp` CUDA build. Acceptable trade — the alternative is a 5–10
  minute first-run download that fails on metered links.
- **Two install paths to test.** Default (bundled binary) and opt-in
  (`uv` venv) both need CI coverage. Mitigated by having one `uv` bootstrap
  shared across all opt-in tiers.
- **Catalog priority flips.** `gguf.json` becomes the primary catalog for
  Linux+NVIDIA / Windows; `safetensors.json` moves to vLLM-only.
  `lmforge catalog` without arguments now shows GGUF entries.

### Neutral

- SGLang **stays in the codebase**. Demoted to `experimental`, gated to
  `sm_90..=sm_103`, never auto-selected on consumer Blackwell.
  Cost-to-keep is negligible.

## Re-evaluation triggers

These engines were considered and **not adopted**. We re-check the listed
condition annually (see `docs/engineering/ANNUAL_ENGINE_REVIEW.md`, landed
in Phase 5):

| Engine | Why not adopted | Re-evaluate when |
| --- | --- | --- |
| **SGLang** | `sgl-kernel` ships `sm_90`/`sm_100` only as of v0.5.10.post1 (May 2026). Consumer Blackwell users get `ImportError: common_ops`. | `curl https://docs.sglang.ai/whl/cu130/ \| grep sm120` returns a wheel |
| **lmdeploy** | No `sm_120` prebuilt as of v0.7. Closest re-entry candidate. | v0.13+ ships `sm_120` cubins AND the AWQ/MXFP4 path matches vLLM tok/s within 15% |
| **TGI** | HuggingFace officially deprecated in favour of `text-generation-server` integration into transformers. | Never (HF roadmap removes it). |
| **MLC-LLM** | No GGUF import; NVFP4 path still experimental. | NVFP4 dense path lands AND GGUF import lands AND `mlc_llm serve` runs on `sm_120` |
| **TRT-LLM** | Static-link build for `sm_120` not published; trtllm-gen FMHA cubins absent. | NVIDIA publishes `sm_120` `trtllm-gen` cubins AND a static-link build |
| **Aphrodite** | Pure vLLM fork with no unique value-add at the moment. | Never. |

## OS / hardware support matrix (consolidated)

```
                       Default       Opt-in              Experimental
                       ─────────     ──────────          ─────────────
Linux + NVIDIA sm_75+  llamacpp      vllm, exl3          sglang (sm_90..sm_103 only)
Linux + AMD            llamacpp      —                   —
Linux + CPU only       llamacpp      —                   —
WSL2  + NVIDIA sm_75+  llamacpp      vllm, exl3          sglang (same gate)
Win   + NVIDIA sm_75+  llamacpp      exl3                —     (vLLM NOT offered on native Windows)
Win   + CPU only       llamacpp      —                   —
macOS + Apple Silicon  omlx, llamacpp —                  —     (no NVIDIA tiers on Darwin)
macOS + Intel          llamacpp      —                   —
```

## Implementation phases

1. **Phase 0 (this ADR + probe extension)** — schema v2, ADR landed. No
   behaviour change. `data/engines.toml` gets the new fields (`tier`,
   `supported_platforms`, re-eval-trigger comments) but defaults preserve
   existing selection.
2. **Phase 1 — llamacpp default.** Bundle `llama.cpp` (b9351), wire
   `--cache-ram`, flip selector. Closes P0.
3. **Phase 2 — Catalog promotion + embed sidecar + worker stderr
   propagation.** GGUF becomes primary catalog; `llama.cpp` embed sidecar
   serves `/v1/embeddings` regardless of chat engine.
4. **Phase 3 — vLLM opt-in tier** (Linux + WSL2, NVIDIA only).
5. **Phase 4 — ExLlamaV3 + TabbyAPI opt-in tier** (Linux + Windows + WSL2,
   NVIDIA only).
6. **Phase 5 — SGLang demoted, drop-list comments committed.**
7. **Phase 6 — UI, release pipeline, docs polish.**

## References

- `docs/temp/engine_revamp_plan.md` — phased execution plan
- `docs/temp/engine_research_in_cur.md` — sm_120 failure reproduction
- `compass_artifact_wf-09e2410d-4316-4e11-ba12-470e1e5a9379_text_markdown.md`
  — external research backing the `llama.cpp`-as-default position
- SGLang GitHub issue tracker — search "sm120" / "Blackwell" for upstream status
