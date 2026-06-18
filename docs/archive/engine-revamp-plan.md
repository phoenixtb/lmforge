# LMForge Engine Tier Restructure — Phased Execution Plan

## North Star

> **Default = single bundled binary, zero-config, works on every supported OS.** **Opt-in tiers = isolated venvs, verified downloads, no global state pollution, never offered on a platform where they can't work.**

## Locked decisions (do not re-litigate)


| Decision                                | Value                                                                                                                                                           |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Default engine                          | llama.cpp (CUDA on NVIDIA, Metal on Apple, Vulkan on AMD, CPU everywhere else)                                                                                  |
| llama.cpp pin                           | `b9351` (or latest at phase-1 start; SHA-locked in `<span class="md-inline-path-prefix">data/</span><span class="md-inline-path-filename">engines.toml</span>`) |
| llama.cpp distribution                  | **Bundled** in release artifacts (Linux x64, Windows x64, macOS arm64)                                                                                          |
| Opt-in distribution                     | Verified downloads via SHA256-pinned URLs;**never auto-installed**                                                                                              |
| Opt-in venv layout                      | Per-engine isolated:`<span class="md-inline-path-prefix">~/.lmforge/engines/<engine>/</span><span class="md-inline-path-filename">venv</span>`                  |
| Opt-in tier 1                           | vLLM ≥ 0.20 — Linux only (incl. WSL2), NVIDIA only, sm_75+                                                                                                    |
| Opt-in tier 2                           | ExLlamaV3 + TabbyAPI — Linux + Windows native, NVIDIA only, sm_75+                                                                                             |
| DROP (with documented re-eval triggers) | SGLang, TGI, MLC-LLM, lmdeploy, TRT-LLM, Aphrodite                                                                                                              |
| Primary catalog                         | `<span class="md-inline-path-filename">gguf.json</span>` (was already populated, just gets promoted in defaults)                                                |
| Secondary catalog                       | `<span class="md-inline-path-filename">safetensors.json</span>` (used by vLLM opt-in only)                                                                      |
| New catalog                             | `<span class="md-inline-path-filename">exl3.json</span>` (used by EXL3 opt-in only)                                                                             |
| Embeddings architecture                 | Long-lived llama.cpp sidecar process, runs alongside whatever chat engine is active                                                                             |

## Cross-cutting principles

* **OS matrix is first-class.** Every adapter declares which `(os, arch, gpu_vendor, compute_cap)` tuples it supports. Engine selector and CLI both honor this. Windows users never see a `--engine vllm` option that would route through WSL2 without their knowledge.
* `<span class="md-inline-path-filename">hardware.json</span>` **is the single source of truth.** Probe extends to capture: `compute_cap`, `cuda_runtime_version`, `cuda_driver_version`, `os_family`, `is_wsl`, `system_ram_gb`, `gpu_count`. Engine selection reads this; user never has to repeat themselves.
* **No shared site-packages, ever.** Each opt-in tier installs into `<span class="md-inline-path-prefix">~/.lmforge/engines/<engine>/venv/</span>`. A broken vLLM install cannot take down EXL3 or vice versa.
* **Re-eval triggers are code-adjacent.** Three-line comment block in `<span class="md-inline-path-filename">engines.toml</span>` per dropped engine: `# Dropped because X. Re-evaluate when Y.` Future-you (or future-AI) doesn't re-run this investigation.

---

## Phase 0 — Foundation: probe, decision matrix, ADR

**Goal:** Land the architectural decisions in the repo as code/config/docs *before* touching any engine. Everything that follows depends on this.

**Scope:**

1. `<span class="md-inline-path-prefix">src/hardware/</span><span class="md-inline-path-filename">probe.rs</span>` **extensions**
   * Add `compute_cap: Option<(u8, u8)>` (parsed from `nvidia-smi --query-gpu=compute_cap`)
   * Add `cuda_runtime_version: Option<String>` (parsed from `nvcc --version` *or* `nvidia-smi`)
   * Add `cuda_driver_version: Option<String>`
   * Add `os_family: enum { Linux, WindowsNative, WindowsWSL2, MacOS }`
   * Add `is_wsl: bool` (read `<span class="md-inline-path-prefix">/proc/sys/kernel/</span><span class="md-inline-path-filename">osrelease</span>` for `Microsoft`/`WSL`)
   * Add `gpu_count: u8`
   * Update `<span class="md-inline-path-filename">hardware.json</span>` schema + bump version field
2. `<span class="md-inline-path-prefix">docs/architecture/</span><span class="md-inline-path-filename">ADR-001-engine-tiers.md</span>` — captures: tier model, drop list with re-eval triggers, OS matrix, sm_120 lesson learned. Single source for "why is it shaped this way".
3. `<span class="md-inline-path-prefix">data/</span><span class="md-inline-path-filename">engines.toml</span>` **skeleton rewrite** (no behavior change yet)
   * Add `[engine.llamacpp]`, `[engine.vllm]`, `[engine.exl3]`, `[engine.sglang]` blocks
   * Each declares `supported_platforms = [...]` with `(os, arch, gpu_vendor, min_compute_cap)` tuples
   * Each declares `tier = "default" | "opt-in" | "experimental"`
   * Each declares `priority` (selector tie-break)
   * Re-eval-trigger comments for dropped engines
4. **Tests:** unit tests for probe parsing on synthetic inputs; integration test that `<span class="md-inline-path-filename">hardware.json</span>` round-trips.

**Out of scope:** any actual engine installation logic changes.

**Estimated effort:** 0.5 day. Pure plumbing.

**Done criteria:**

* `lmforge init` on Linux+NVIDIA writes the new `<span class="md-inline-path-filename">hardware.json</span>` fields without errors.
* `cargo test --lib hardware::probe` green.
* `<span class="md-inline-path-filename">ADR-001-engine-tiers.md</span>` reviewed and committed.

---

## Phase 1 — Default tier: llama.cpp bundled (the zero-config promise)

**Goal:** New users on Linux, Windows, and macOS run `lmforge init` and get a working chat *with no Python, no pip, no venv* . This is the phase that closes the P0 bug (SGLang broken on sm_120) for everyone.

**Scope:**

### 1.1 Bundled binary pipeline

* New `<span class="md-inline-path-prefix">scripts/</span><span class="md-inline-path-filename">bundle-llamacpp.sh</span>`: pulls `llama.cpp b9351` (or pinned) artifacts from upstream GH releases for:
  * `linux-x64-cuda` (CUDA 12.x build, includes sm_75 through sm_120)
  * `windows-x64-cuda` (same)
  * `linux-x64-cpu` (universal fallback)
  * `windows-x64-cpu`
  * `macos-arm64-metal`
* Each download is SHA256-verified against a manifest in `<span class="md-inline-path-prefix">data/</span><span class="md-inline-path-filename">engines.toml</span>`.
* Bundle into release tarball/installer at `lmforge/bin/llama-server[.exe]`.
* Optional offline mode: `lmforge init --offline-bundle ./bundle.tar.gz` for air-gapped installs.

### 1.2 sm_120 detection guard (the silent-CPU-fallback footgun)

* At `lmforge init`, after probe runs: if `compute_cap == (12, 0)`, exec `llama-server --version` and grep for `sm_120` in the supported-arch list. If absent, refuse to start with a clear error: `"This llama.cpp build lacks sm_120 kernels. Download a CUDA build from ... or run lmforge upgrade-engine llamacpp."`
* Same guard for `compute_cap == (12, 1)` (DGX Spark).

### 1.3 `--cache-ram` wiring

* New `[engine.llamacpp].cache_ram_mib` field in `<span class="md-inline-path-filename">engines.toml</span>`.
* Default computed at `lmforge init`: `min(0.25 * total_ram_gb * 1024, 4096)` — tighter than the research's 8 GiB cap to protect 16 GB RAM systems (the 5060 Ti target).
* Override via env var `LMFORGE_LLAMACPP_CACHE_RAM_MIB` or CLI `--cache-ram-mib`.
* Pass-through to `llama-server` via `--cache-ram <mib>` flag.

### 1.4 `<span class="md-inline-path-filename">engines.toml</span>` rewrite (behavioral)

* `llamacpp`: `tier = "default"`, `priority = 30`, supported on every `(os, gpu)` combo.
* `sglang`: `tier = "experimental"`, `priority = 10`, gated to `compute_cap ∈ [(9,0), (10,3)]`. Re-eval-trigger comment.
* `vllm`: stub with `tier = "opt-in"`, `priority = 25`, `supported_platforms = [Linux+NVIDIA+sm75]`. **Not yet implemented** — Phase 3.
* `exl3`: stub with `tier = "opt-in"`, `priority = 20`, `supported_platforms = [Linux+NVIDIA+sm75, Windows+NVIDIA+sm75]`. **Not yet implemented** — Phase 4.
* Dropped engines: `tgi`, `mlc`, `lmdeploy`, `trtllm`, `aphrodite` — not even stubbed. Re-eval-trigger comments in ADR-001 only.

### 1.5 Engine selector update

* `src/engine/installer.rs::select_engine()`: pick highest-priority engine whose `supported_platforms` matches the probed hardware. Result: Linux+NVIDIA → llamacpp (was sglang). Windows+NVIDIA → llamacpp. macOS+Apple → mlx (unchanged, Phase 1 doesn't touch macOS).

### 1.6 Smoke test for `--cache-ram` reuse

* New `<span class="md-inline-path-prefix">tests/integration/</span><span class="md-inline-path-filename">llamacpp_cache_ram.rs</span>`: spawn `llama-server` with `--cache-ram 512`, send two requests sharing a 500-token prefix, assert the second TTFT < 50% of the first (KV reuse fired). Runs in CI on Linux+CPU build (no GPU dependency).

### 1.7 Install script updates

* `<span class="md-inline-path-prefix">scripts/</span><span class="md-inline-path-filename">install-core.sh</span>`: removes the `uv` / Python bootstrap from the default path. Mentions `uv` only in the "opt-in engines" section.
* `<span class="md-inline-path-prefix">windows/</span><span class="md-inline-path-filename">install.ps1</span>`: new file. Mirrors install-core.sh for Windows. Detects CUDA driver via registry, sets up `<span class="md-inline-path-filename">lmforge.exe</span>` + bundled `<span class="md-inline-path-filename">llama-server.exe</span>`.
* `docs/INSTALL_*.md`: top section reads "this works out of the box, no Python required" — that's now true.

**Out of scope:**

* vLLM / EXL3 install code (Phase 3 / 4).
* Embeddings sidecar refactor (Phase 2).
* UI changes (Phase 6).

**Estimated effort:** 1.5–2 days.

**Done criteria:**

* Fresh Ubuntu 24.04 + NVIDIA 5060 Ti / 5090 box: `curl … | bash` → `lmforge init` → `lmforge run qwen3:8b:Q4_K_M` produces tokens. No `python3`, `pip`, `uv`, or `venv` invocations anywhere.
* Fresh Windows 11 + NVIDIA RTX 5070: same flow via `<span class="md-inline-path-filename">install.ps1</span>`. Works.
* sm_120 guard fires on a synthetic broken bundle (test fixture).
* `--cache-ram` smoke test green.
* Existing `<span class="md-inline-path-filename">dev_test.sh</span>` unit + integration suites still pass.

**This phase alone closes P0 and P1 from the bug list.**

---

## Phase 2 — Catalog promotion + embeddings sidecar + stderr propagation

**Goal:** Make the catalog/UX consistent with the new tier model. Fix the P2/P3/P4 issues from previous session.

**Scope:**

### 2.1 Catalog priority flip

* `src/cli/{catalog,pull,run}.rs::detect_platform_format()`: default to `gguf` for Linux+NVIDIA, Windows+NVIDIA, Linux+AMD, Linux+CPU. Default to `mlx` for macOS+Apple. `safetensors` is only returned when `--engine vllm` is active.
* `<span class="md-inline-path-prefix">src/cli/</span><span class="md-inline-path-filename">init.rs</span>` / `<span class="md-inline-path-prefix">src/server/</span><span class="md-inline-path-filename">native.rs</span>`: seed `<span class="md-inline-path-filename">gguf.json</span>` as the primary; still write `<span class="md-inline-path-filename">safetensors.json</span>` and `<span class="md-inline-path-filename">exl3.json</span>` for opt-in tiers.
* `lmforge catalog` default-without-flag lists GGUF entries.

### 2.2 Embeddings sidecar architecture

* New module `<span class="md-inline-path-prefix">src/engine/</span><span class="md-inline-path-filename">embed_sidecar.rs</span>`.
* Even when `--engine vllm` is active, a small llama.cpp process serves `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">embeddings</span>` on an internal port using a dedicated embed model (e.g. `bge-m3:Q8_0` GGUF).
* Rationale: vLLM serving chat + a second vLLM for embeddings doubles VRAM cost; llama-server's embed mode is ~200 MB VRAM for bge-m3 Q8.
* `<span class="md-inline-path-prefix">src/server/</span><span class="md-inline-path-filename">embed.rs</span>`: routes `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">embeddings</span>` requests to the sidecar regardless of which chat engine is active.
* Catalog: `embed/*` shortcuts in `<span class="md-inline-path-filename">gguf.json</span>` (already there — verify).

### 2.3 Worker stderr propagation (P4 fix)

* `src/engine/runner.rs::SpawnedWorker`: instead of writing stderr to `*.stderr.log` only, tail the last N lines into a ring buffer.
* `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">status</span>` response: add `engines[].workers[].last_error_tail: Option<String>` (last 32 lines of stderr if `exit_code != 0` or process is unhealthy).
* UI displays this when a model fails to load — closes the "stderr.log 17 KB, status shows nothing" black-box bug.

### 2.4 `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">sysinfo</span>` aggregation fix (P3)

* Group worker processes by `(engine, model)` instead of dumping each PID as `<span class="md-inline-path-prefix">engine/</span><span class="md-inline-path-filename">other</span>`. Trivial query/grouping fix.

### 2.5 Tests + dev scripts

* `<span class="md-inline-path-filename">dev_test.sh</span>`: assertion thresholds updated. `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">catalog</span>` count is now ~162 (GGUF primary) instead of ~75 (safetensors).
* `<span class="md-inline-path-filename">pre-commit-check-catalog.sh</span>`: re-run after promotion, ensure no regressions.
* `<span class="md-inline-path-prefix">scripts/util/</span><span class="md-inline-path-filename">cheat-sheet</span>`: update default-engine references.

**Out of scope:**

* vLLM/EXL3 adapter code.

**Estimated effort:** 1 day.

**Done criteria:**

* `lmforge catalog` lists GGUF entries by default.
* A chat request with a non-existent model returns the stderr tail in `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">status</span>`.
* Embed sidecar boots on `lmforge init`, serves `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">embeddings</span>` at idle <300 MB VRAM.
* `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">sysinfo</span>` groups correctly.
* All P2/P3/P4 bugs marked closed.

---

## Phase 3 — Opt-in: vLLM (Linux + NVIDIA, isolated venv)

**Goal:** Power users with concurrent workloads can `lmforge engine install vllm` and switch via `--engine vllm`. Never offered on Windows native or macOS.

**Scope:**

### 3.1 OS / hardware gate

* `src/cli/engine.rs::install`: if `os_family != Linux` (incl. `WindowsWSL2`), refuse: `"vLLM is only available on Linux (including WSL2). On native Windows, use llama.cpp (default) or ExLlamaV3 (--engine exl3)."`
* If `gpu_vendor != Nvidia` or `compute_cap < (7,5)`: refuse with explanation.
* `--engine vllm` at runtime: same checks, hard fail with clear remediation.

### 3.2 Isolated venv installer

* New `<span class="md-inline-path-prefix">src/engine/adapters/vllm/</span><span class="md-inline-path-filename">installer.rs</span>`:
  * `uv` bootstrap (reuse `<span class="md-inline-path-prefix">src/engine/</span><span class="md-inline-path-filename">uv.rs</span>` from prior work)
  * Venv created at `<span class="md-inline-path-prefix">~/.lmforge/engines/vllm/venv/</span>` (never under `<span class="md-inline-path-prefix">~/.lmforge/engines/sglang/</span>` — different engine, different home)
  * **Explicit** `--torch-backend` **pin** based on probed CUDA driver:
    * `cuda_driver_version >= 13.0` → `--torch-backend=cu130`
    * `cuda_driver_version >= 12.9` → `--torch-backend=cu129`
    * `cuda_driver_version >= 12.8` → `--torch-backend=cu128`
    * else → refuse install, ask user to upgrade driver to ≥ 580.x
  * Install command: `uv pip install vllm --torch-backend=cu130 --prerelease=allow`
  * **No** `auto` **flag, anywhere.** The whole point of the explicit pin is to avoid the cu128/cu130 mismatch crash the research flagged.

### 3.3 vLLM adapter

* `<span class="md-inline-path-prefix">src/engine/adapters/vllm/</span><span class="md-inline-path-filename">runner.rs</span>`: spawns `vllm serve <model_path> --port <port> --host 127.0.0.1` with defaults:
  * `--enable-prefix-caching`
  * `--quantization awq_marlin` (sm_120-safe default; overridable via `lmforge run --quant <fp8|nvfp4|...>`)
  * `--dtype float16` (sm_120-stable, FP8 only when user opts in)
  * `--enforce-eager=false`
  * `--max-model-len <derived from VRAM>`
* Spawn-per-model orchestration: reuses the existing port-pool + idle-eviction supervisor.
* Cold start: 30–90s expected; orchestrator reports `loading` state in `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">status</span>` during this window.

### 3.4 Soft warning (the user's caveat #2)

* At `lmforge run --engine vllm`:
  * If `gpu_count == 1` AND probed concurrent-request history shows mostly batch=1: print **warning** (not refusal): `"vLLM's edge is concurrent serving. At single-user batch=1 on this card, llama.cpp matches it without the 5 GB install. Proceed anyway? [Y/n]"` (interactive shells only; non-interactive proceeds silently).
  * Never a hard block. User opted in explicitly.

### 3.5 NVFP4 as advanced flag (the user's caveat #4)

* `--quantization nvfp4` is supported but **not the default**.
* Catalog: NVFP4 model entries in `<span class="md-inline-path-filename">safetensors.json</span>` get a `_warning_nvfp4_moe_broken` comment annotation. CLI warns before pulling NVFP4 MoE: `"vLLM issue #35065: NVFP4 MoE backend is broken on sm_120 as of vLLM 0.20. Dense NVFP4 works. Continue?"`
* Phase 6 catalog work re-evaluates NVFP4 entries quarterly.

### 3.6 Catalog wiring

* `<span class="md-inline-path-filename">safetensors.json</span>` is the catalog when `--engine vllm` is active. Already populated from prior sessions — verify the quantized-only policy still holds.
* New entries audited for sm_120 sanity (no NVFP4 MoE in defaults).

### 3.7 Embeddings stay on the sidecar (the user's smaller point #1)

* vLLM serves `<span class="md-inline-path-prefix">/v1/chat/</span><span class="md-inline-path-filename">completions</span>` only. `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">embeddings</span>` and `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">rerank</span>` continue to route to the llama.cpp embed sidecar from Phase 2. Simpler, cheaper, no double-VRAM tax.
* Documented in `<span class="md-inline-path-prefix">docs/architecture/</span><span class="md-inline-path-filename">ADR-002-embeddings-routing.md</span>` (new).

### 3.8 Tests

* `<span class="md-inline-path-prefix">tests/integration/</span><span class="md-inline-path-filename">vllm_install.rs</span>`: gated behind `--features vllm-test` and a `CI_HAS_NVIDIA=1` env var. Builds venv, pulls smallest model, sends a chat request, asserts non-empty response.
* `<span class="md-inline-path-prefix">tests/unit/</span><span class="md-inline-path-filename">vllm_gate.rs</span>`: hardware-gate matrix (Windows, macOS, AMD GPU, sm_70 → all rejected).

**Out of scope:**

* ExLlamaV3 (Phase 4).
* UI integration (Phase 6).

**Estimated effort:** 2 days.

**Done criteria:**

* Linux + NVIDIA sm_120 box: `lmforge engine install vllm` → venv created at `<span class="md-inline-path-prefix">~/.lmforge/engines/vllm/venv/</span>` → `lmforge run qwen3:8b:4bit --engine vllm` produces tokens.
* Windows native box: `lmforge engine install vllm` refuses with clear remediation.
* macOS: same.
* AMD GPU: same.
* Soft warning fires on single-user batch=1 with `--engine vllm`.
* NVFP4 MoE entry attempted: warning shown before pull.

---

## Phase 4 — Opt-in: ExLlamaV3 + TabbyAPI (Linux + Windows, isolated venv)

**Goal:** Enthusiasts can `lmforge engine install exl3` and get state-of-the-art INT-quant decode tok/s on a single GPU. **Works on native Windows** (unlike vLLM) because TabbyAPI is a plain uvicorn app with no WSL-style paravirt dependencies.

**Scope:**

### 4.1 OS / hardware gate

* Supports: `Linux+NVIDIA+sm75+`, `Windows+NVIDIA+sm75+`, `WSL2+NVIDIA+sm75+`.
* Refuses: macOS, AMD GPU, CPU-only.

### 4.2 Isolated venv installer

* Venv at `<span class="md-inline-path-prefix">~/.lmforge/engines/exl3/venv/</span>`.
* Pinned wheel URLs (SHA-verified) for `torch+cu128`, `exllamav3+cu128.torch2.8.0`, `tabbyapi`.
* ExLlamaV3 wheel selection: pick by `(python_version, cuda_version)` from `<span class="md-inline-path-filename">hardware.json</span>`. Linux: `cp313-cp313-linux_x86_64`. Windows: `cp313-cp313-win_amd64`.
* Install command: `uv pip install --no-deps -r exl3_requirements.lock` (a pinned lockfile in `<span class="md-inline-path-prefix">data/engines/exl3/</span>`).
* **Never resolves into the vLLM venv.** Separate Python install entirely.

### 4.3 EXL3 catalog

* New `<span class="md-inline-path-prefix">data/catalogs/</span><span class="md-inline-path-filename">exl3.json</span>`. Curated list of ~20 EXL3 entries from `turboderp`, `bartowski` (where they publish EXL3), and `lmstudio-community` (if any).
* Schema same as other catalogs: `{ shortcut: "org/repo" }`.
* Examples: `qwen3:8b:exl3-4.0bpw`, `llama3.3:70b:exl3-3.5bpw`, etc.
* `<span class="md-inline-path-filename">pre-commit-check-catalog.sh</span>` extended to validate EXL3 entries.

### 4.4 TabbyAPI adapter

* `<span class="md-inline-path-prefix">src/engine/adapters/exl3/</span><span class="md-inline-path-filename">runner.rs</span>`: spawns `python -m tabby.server --config <generated.yaml>` with defaults:
  * `cache_mode: q4` (KV quantization, EXL3's sweet spot)
  * `max_seq_len: <derived from VRAM>`
  * `draft_model: null` (user opts into spec decode separately)
* OpenAI-compat routes pass through.
* Health probe: TabbyAPI's `<span class="md-inline-path-prefix">/</span><span class="md-inline-path-filename">health</span>` endpoint.

### 4.5 Auto-suggest

* `lmforge run <model>`: if `<model>` resolves to a path ending in `-exl3` or `EXL3`, and `exl3` engine is installed, prefer it. Otherwise suggest: `"This model is EXL3 quant; install with: lmforge engine install exl3"`.

### 4.6 Embeddings continue via llama.cpp sidecar (same as vLLM tier).

### 4.7 Tests

* Mirror Phase 3.8 but for EXL3. Smaller smoke set (TabbyAPI loads, single chat request works).
* Cross-platform gating tests.

**Out of scope:**

* Multi-LoRA, draft-model speculative decoding (advanced features for v1.2+).

**Estimated effort:** 1.5 days.

**Done criteria:**

* Linux + Windows + NVIDIA: `lmforge engine install exl3` works on both.
* macOS: refused.
* AMD GPU: refused.
* An EXL3 model resolves and runs end-to-end.

---

## Phase 5 — SGLang to experimental + drop-list documentation

**Goal:** Clean up the legacy SGLang path without deleting it (in case sgl-kernel ships sm_120 wheels later) and bake the re-eval triggers into the repo.

**Scope:**

### 5.1 SGLang to experimental

* `<span class="md-inline-path-filename">engines.toml</span>`: `tier = "experimental"`, `priority = 10`, `supported_platforms = [(Linux, NVIDIA, sm90), (Linux, NVIDIA, sm100)]` only. RTX 50-series boxes will *never* auto-select it.
* CLI: `--engine sglang` works (no removal) but prints: `"SGLang's sgl-kernel ships only sm_90 and sm_100 prebuilts. Your GPU (sm_120) will fail at runtime. See ADR-001-engine-tiers.md. Continue anyway? [y/N]"`.
* Re-eval trigger in `<span class="md-inline-path-filename">engines.toml</span>`:

  # DROPPED FROM DEFAULTS: sgl-kernel only ships sm_90 + sm_100 prebuilts (May 2026).

  # RE-EVALUATE WHEN: docs.sglang.ai/whl/cu130/ index lists sm_120 cubins.

  # Verified: curl https://docs.sglang.ai/whl/cu130/ | grep sm120

### 5.2 Drop-list documentation

* `<span class="md-inline-path-filename">ADR-001-engine-tiers.md</span>` gets a "Re-evaluation triggers" section, one entry per dropped engine:
  * **lmdeploy**: re-evaluate if we want a lighter vLLM alternative for AWQ/MXFP4. v0.13+ has sm_120. Closest to coming back.
  * **SGLang**: re-evaluate when sm_120 wheels ship.
  * **TGI**: re-evaluate never; HF officially deprecated.
  * **MLC-LLM**: re-evaluate if NVFP4 path lands AND GGUF import lands.
  * **TRT-LLM**: re-evaluate if NVIDIA publishes sm_120 trtllm-gen FMHA cubins AND a static-link build.
  * **Aphrodite**: re-evaluate never; pure vLLM fork with no unique value.

### 5.3 Annual review reminder

* `<span class="md-inline-path-prefix">docs/engineering/</span><span class="md-inline-path-filename">ANNUAL_ENGINE_REVIEW.md</span>` template — list of `curl` commands and issue tracker URLs to re-check yearly.

**Out of scope:**

* Removing SGLang code (keep it; cheap insurance).

**Estimated effort:** 0.5 day.

**Done criteria:**

* `lmforge run --engine sglang` on sm_120 prompts before proceeding.
* ADR-001 has all re-eval triggers documented.
* A `make engine-review-status` target prints which dropped engines have changed status since last review.

---

## Phase 6 — Polish, UI, release

**Goal:** Ship-quality finish.

**Scope:**

### 6.1 UI integration

* Settings panel shows: active engine, available tiers, install/uninstall buttons for opt-ins.
* Model browser badges: which tier each model belongs to (default/vllm/exl3).
* Status panel surfaces `last_error_tail` from Phase 2.3.

### 6.2 Status / observability

* `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">status</span>` extended: `default_engine`, `available_tiers`, `installed_tiers`, `embed_sidecar`, `cache_ram_stats` (hits/misses/bytes).

### 6.3 Documentation

* `<span class="md-inline-path-prefix">docs/</span><span class="md-inline-path-filename">INSTALL_LINUX.md</span>`, `<span class="md-inline-path-prefix">docs/</span><span class="md-inline-path-filename">INSTALL_WINDOWS.md</span>`, `<span class="md-inline-path-prefix">docs/</span><span class="md-inline-path-filename">INSTALL_MACOS.md</span>` rewritten around the tier model.
* `<span class="md-inline-path-prefix">docs/architecture/</span>` gets ADR-001 (tiers), ADR-002 (embed sidecar), ADR-003 (sm_120 lessons learned).
* `<span class="md-inline-path-prefix">scripts/util/</span><span class="md-inline-path-filename">cheat-sheet</span>` updated with all new commands.

### 6.4 Dev tooling

* `<span class="md-inline-path-filename">dev_test.sh</span>` extended: matrix runs (default tier on every CI runner; vLLM tier when `CI_HAS_NVIDIA=1`; EXL3 tier when `CI_HAS_NVIDIA=1`).
* `<span class="md-inline-path-filename">dev_bench.sh</span>` extended: per-tier benchmark targets.

### 6.5 Release pipeline

* GitHub Actions builds 5 artifacts: `<span class="md-inline-path-filename">linux-x64.tar.gz</span>`, `<span class="md-inline-path-filename">linux-x64-cpu.tar.gz</span>`, `<span class="md-inline-path-filename">windows-x64.msi</span>`, `<span class="md-inline-path-filename">windows-x64-cpu.msi</span>`, `<span class="md-inline-path-filename">macos-arm64.dmg</span>`. Each bundles the right llama.cpp variant.
* Verified-download manifests for opt-in tiers committed at `data/engines/*/SHA256SUMS`.

**Estimated effort:** 2 days.

**Done criteria:**

* Three platforms install cleanly from release artifacts.
* UI tier switching works end-to-end.
* All documentation cross-references valid.

---

## Total estimate & sequencing


| Phase                           | Effort         | Can run in parallel with |
| --------------------------------- | ---------------- | -------------------------- |
| 0 — Foundation                 | 0.5d           | — (gates everything)    |
| 1 — llamacpp default           | 1.5–2d        | — (gates 2/3/4)         |
| 2 — Catalog + sidecar + stderr | 1d             | 3 (after 1)              |
| 3 — vLLM opt-in                | 2d             | 4 (after 2)              |
| 4 — EXL3 opt-in                | 1.5d           | 3 (after 2)              |
| 5 — SGLang demote + drop docs  | 0.5d           | any (after 1)            |
| 6 — UI + release               | 2d             | — (last)                |
| **Total**                       | **9–10 days** |                          |

**Critical path:** 0 → 1 → 2 → (3 ‖ 4 ‖ 5) → 6.

**Recommended split for separate sessions:**

* **Session A:** Phase 0 + Phase 1 (lands the new default; biggest user-visible win)
* **Session B:** Phase 2 + Phase 5 (cleanup wave; bug-bash for old issues)
* **Session C:** Phase 3 (vLLM tier)
* **Session D:** Phase 4 (EXL3 tier)
* **Session E:** Phase 6 (polish + release)

Each session is self-contained with this doc + the ADRs from Phase 0 as full context.
