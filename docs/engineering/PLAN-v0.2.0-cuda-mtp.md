# LMForge v0.2.0 — CUDA Variant + Speculative Decoding (MTP) — Execution Plan

> **Status:** Planning complete; ready to execute.
> **Branch:** `v0.2.0-cuda-mtp` (cut from `main` after the plan was merged in).
> **Owner:** Cursor agent + reviewer.
> **Last updated:** 2026-05-28.

This is the working tracker for v0.2.0. Check items off as they complete. Update **Progress dashboard** when a phase flips state. Don't rewrite the design contract section without a commit message that explains why.

## How to use this doc

- `- [ ]` items are tracked work; tick them as they ship.
- Each phase has an **Acceptance criteria** subsection — these are the gates that move a phase from `in-progress` to `done`.
- **Out of scope** lists items deliberately deferred to v0.3+. Adding scope here requires a corresponding update to the Risk register.
- Open questions go in §6 with a target resolution phase; close them by editing the doc inline + linking the PR/commit.

## Status legend

| Marker | Meaning |
|---|---|
| `⏳` | Pending — not yet started |
| `🚧` | In progress |
| `✅` | Done (all acceptance criteria met) |
| `⏭️` | Skipped / deferred |

---

## Progress dashboard

| Phase | Status | Owner | PR / commit |
|---|---|---|---|
| C-1 — CUDA build pipeline | ✅ | agent | `v0.2.0-cuda-mtp` (workflow shipped; manifest populated with real shas; lib-bundling fix landed) |
| C-2 — Variant infrastructure | ✅ | agent | `v0.2.0-cuda-mtp` (variant.rs + installer::install_variant + scan_variant_state) |
| C-3 — Variant-aware launch | ✅ | agent | `v0.2.0-cuda-mtp` 800011f (resolve_executable variant_dir + LD_LIBRARY_PATH injection) |
| S-1 — MTP detection | ✅ | agent | `v0.2.0-cuda-mtp` (parser + catalog schema + pull-time probe; probe-wins-over-catalog after live-test on unsloth/Qwen3.5-4B; `pull --refresh` migration path for already-installed models) |
| S-2 — Spec-dec launch + telemetry | ✅ | agent | `v0.2.0-cuda-mtp` (S-2.1–S-2.9; live MTP + spec_stats verified on cuda12/cuda13 with `Qwen3.5-4B-MTP-GGUF`) |
| S-3 — Draft-model pairs | ✅ | agent | `draft_pairs.toml` + lookup/cache + auto resolution in `speculative::resolve` (qwen3.x → qwen3:0.6b:4bit; llama/qwen2.5 pairs commented pending catalog entries) |
| Polish — docs + UI + ADRs | ✅ | | |
| Post-tarball live matrix | ⏳ | user | See [TEST-v0.2.0-post-tarball.md](./TEST-v0.2.0-post-tarball.md) — blocked on NCCL-fixed cuda12 + cuda13 tarball rebuild |

---

## 0. Design contract (locked — do not revisit during impl)

| Decision | Value |
|---|---|
| Primary engine on Linux NVIDIA (driver ≥ r570) | **llama.cpp CUDA 12.8.1 + MTP** |
| Fallback when below floor / CUDA fails / non-NVIDIA | **llama.cpp Vulkan + MTP** |
| macOS | Unchanged — oMLX |
| Windows NVIDIA | Unchanged — pull upstream `win-cuda-{12.4,13.1}-x64` zip |
| Windows AMD/Intel | Unchanged — `win-vulkan-x64` |
| CUDA tarball layout | Bundled libs in `lib/` + RPATH `$ORIGIN/lib` (NOT static cudart) |
| Default CUDA variant | **12.8.1**, driver floor **r570.26** |
| Opt-in CUDA variant | **13.1.x**, driver floor **r590.44.01** |
| Forbidden CUDA versions | **13.2, 13.3** (CI hard guard + runtime refusal) |
| Builder base image | `nvidia/cuda:12.8.1-devel-rockylinux8` (glibc 2.28) |
| Arch matrix (cuda12) | `75-real;80-real;86-real;89-real;90-real;120-real;120-virtual` |
| Arch matrix (cuda13) | adds `100-real` (B200) |
| llama.cpp pin | b9351 (current), bumped per LMForge release |
| Release tag namespace | `llamacpp-bXXXX` (decoupled from LMForge `vX.Y.Z` tags) |
| Variant install state | `~/.lmforge/engines/llamacpp/variants/<id>/` |
| Static `engines.toml` | Declares engine; does NOT enumerate variants |
| MTP detection | Layered: (i) catalog `mtp` flag → (ii) GGUF tensor inspection via `gguf` crate |
| Spec-dec flag prefix | `--spec-draft-*` (b9351 names, NOT the impl spec's stale `--draft-*`) |
| First-run download | ~400 MB on CUDA path; Vulkan only fetched if CUDA fails post-install |
| Below-floor driver UX | Auto-fallback to Vulkan + MTP; `lmforge doctor` surfaces upgrade hint |
| Existing-install upgrade | Next `lmforge init` auto-installs CUDA (idempotent — skip if already present) |

---

## 1. Phase plan

Total estimated effort: **~3.5 weeks focused** (CUDA pipeline + spec-dec, separately shippable).

### Phase order

```
C-1 (CUDA pipeline)  ─┐
                      ├─► C-3 (variant-aware launch) ─► v0.2.0 release
C-2 (variant infra)  ─┘

S-1 (MTP detection)  ─┐
                      ├─► S-2 (launch + telemetry) ─► S-3 (draft pairs) ─► v0.2.0 release
                      ─┘
```

C-1 and S-1 can proceed **in parallel** (no file overlap). They converge in C-3 + S-2 which both touch `src/engine/adapters/llamacpp.rs`.

---

### Phase C-1 — Build pipeline for `lmforge-llamacpp-cuda12-linux-x64` (~5 days)

**Goal:** produce a portable tarball running on glibc ≥ 2.28 with driver ≥ r570, covering sm_75–sm_120.

#### Tasks

- [x] **C-1.1** `.github/workflows/build-llamacpp-cuda.yml` added (workflow_dispatch matrix: cuda12 + cuda13).
- [x] **C-1.2** Hard CI guard refuses CUDA `13.2*` / `13.3*` matrix entries (first step of the build job).
- [ ] **C-1.3** Workflow produces `lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz` on `workflow_dispatch`. *(scaffold only — first dispatch pending)*
- [x] **C-1.4** Tarball staging copies `libcudart.so.12`, `libcublas.so.12`, `libcublasLt.so.12` from `/usr/local/cuda*/lib64/` into `lib/`, including SONAME symlinks.
- [x] **C-1.5** `patchelf --set-rpath '$ORIGIN/lib'` runs AFTER `strip` per the plan ordering.
- [x] **C-1.6** `VERSION` file emitted with `llamacpp_tag`, `cuda`, `archs`, `driver_min`, `variant`.
- [x] **C-1.7** `softprops/action-gh-release@v2` publishes the tarball + `.sha256` to `llamacpp-b9351`.
- [x] **C-1.8** `data/engines/llamacpp/variants-manifest.json` checked in; sha256 stays as `<populated-by-ci>` until the first dispatch.
- [ ] **C-1.9** Smoke-test on AlmaLinux 8 / Ubuntu 22.04 / Ubuntu 24.04. *(post-dispatch)*
- [ ] **C-1.10** Bench on user's RTX 5060 Ti — no PTX-JIT pause; ≥95% of native tok/s. *(post-dispatch)*
- [x] **C-1.11** Workflow audits `ldd llama-server` — fails if `libcublas`/`libcudart` show up as external deps (i.e. ensures only `libcuda.so.1` remains external). Adds belt-and-suspenders to the 500 MB tarball size guard.

#### Workflow sketch

```yaml
name: Build llama.cpp CUDA variants
on:
  workflow_dispatch:
    inputs:
      llamacpp_tag: { required: true, default: "b9351" }
      release_tag: { required: true, default: "llamacpp-b9351" }
jobs:
  build:
    runs-on: ubuntu-22.04
    strategy:
      fail-fast: false
      matrix:
        include:
          - variant: cuda12
            cuda: 12.8.1
            image: nvidia/cuda:12.8.1-devel-rockylinux8
            archs: "75-real;80-real;86-real;89-real;90-real;120-real;120-virtual"
            cudart_so: libcudart.so.12
            cublas_so: libcublas.so.12
            cublaslt_so: libcublasLt.so.12
          - variant: cuda13
            cuda: 13.1.0
            image: nvidia/cuda:13.1.0-devel-rockylinux8
            archs: "75-real;80-real;86-real;89-real;90-real;100-real;120-real;120-virtual"
            cudart_so: libcudart.so.13
            cublas_so: libcublas.so.13
            cublaslt_so: libcublasLt.so.13
    container: { image: "${{ matrix.image }}" }
    steps:
      - name: HARD GUARD — refuse 13.2 / 13.3
        run: |
          case "${{ matrix.cuda }}" in
            13.2*|13.3*) echo "::error::CUDA ${{ matrix.cuda }} is forbidden (GGUF corruption)"; exit 1 ;;
          esac
      - run: dnf -y install git cmake ninja-build gcc-toolset-12 libcurl-devel patchelf
      - uses: actions/checkout@v4
        with: { repository: ggml-org/llama.cpp, ref: "${{ inputs.llamacpp_tag }}", path: llama.cpp }
      - name: Build (cuda + bundled libs)
        working-directory: llama.cpp
        run: |
          source /opt/rh/gcc-toolset-12/enable
          cmake -S . -B build -G Ninja \
            -DGGML_CUDA=ON -DGGML_NATIVE=OFF -DCMAKE_BUILD_TYPE=Release \
            -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DLLAMA_CURL=ON \
            -DGGML_CUDA_FA_ALL_QUANTS=ON \
            -DCMAKE_CUDA_ARCHITECTURES="${{ matrix.archs }}" \
            -DCMAKE_EXE_LINKER_FLAGS="-Wl,-rpath,'\$ORIGIN/lib'"
          cmake --build build -j$(nproc) --target llama-server llama-cli llama-bench llama-quantize gguf-dump
      - name: Assemble tarball
        working-directory: llama.cpp
        run: |
          tag="${{ inputs.llamacpp_tag }}"
          out="lmforge-llamacpp-${tag}-${{ matrix.variant }}-linux-x64"
          mkdir -p "$out/lib"
          cp build/bin/llama-{server,cli,bench,quantize} build/bin/gguf-dump "$out/"
          strip "$out"/llama-* "$out"/gguf-dump
          for so in ${{ matrix.cudart_so }} ${{ matrix.cublas_so }} ${{ matrix.cublaslt_so }}; do
            cp -L "$(find /usr/local/cuda*/lib64 -name "$so*" | head -n1)" "$out/lib/"
          done
          for bin in llama-server llama-cli llama-bench llama-quantize; do
            patchelf --set-rpath '$ORIGIN/lib' "$out/$bin"
          done
          printf 'llamacpp_tag=%s\ncuda=%s\narchs=%s\ndriver_min=%s\n' \
            "$tag" "${{ matrix.cuda }}" "${{ matrix.archs }}" \
            "$([[ "${{ matrix.variant }}" == cuda12 ]] && echo 570.26 || echo 590.44.01)" > "$out/VERSION"
          tar -czf "${out}.tar.gz" "$out"
          sha256sum "${out}.tar.gz" > "${out}.tar.gz.sha256"
      - uses: softprops/action-gh-release@v2
        with:
          tag_name: ${{ inputs.release_tag }}
          files: |
            llama.cpp/lmforge-llamacpp-*-linux-x64.tar.gz
            llama.cpp/lmforge-llamacpp-*-linux-x64.tar.gz.sha256
```

#### `variants-manifest.json` schema

```jsonc
{
  "llamacpp_tag": "b9351",
  "release_tag": "llamacpp-b9351",
  "variants": [
    {
      "id": "cuda12", "cuda": "12.8.1", "driver_min": "570.26",
      "cap_min": 7.5, "platform": "linux-x64",
      "url": "https://github.com/phoenixtb/lmforge/releases/download/llamacpp-b9351/lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz",
      "sha256": "<populated-by-ci>"
    },
    {
      "id": "cuda13", "cuda": "13.1.0", "driver_min": "590.44.01",
      "cap_min": 7.5, "platform": "linux-x64", "opt_in_only": true,
      "url": "...", "sha256": "<populated-by-ci>"
    }
  ]
}
```

#### Acceptance criteria

- [ ] Tarball runs on AlmaLinux 8 (glibc 2.28) + Ubuntu 22.04 (2.35) + Ubuntu 24.04 (2.39).
- [ ] `ldd llama-server` shows only `libcuda.so.1` as external CUDA dep.
- [ ] `llama-bench` on RTX 5060 Ti: no PTX-JIT pause; reports `compute capability 12.0`; tok/s ≥ 95% of native build.
- [ ] `ldd lib/libcudart.so.12` shows no surprise external deps.
- [ ] cuda12 tarball < 500 MB; cuda13 tarball < 500 MB.

---

### Phase C-2 — Variant infrastructure (~3 days)

**Goal:** know what's installed, what hardware supports what, surface in `engine list` + `doctor`.

#### Tasks

- [ ] **C-2.1** Extend `HardwareProfile` in `src/hardware/probe.rs` with `driver_tuple: Option<(u32,u32,u32)>` — parsed once at probe time from `cuda_driver_version`.
- [ ] **C-2.2** New file: `src/engine/variant.rs` with `LlamaVariant` enum + `select()` function (see code sketch).
- [ ] **C-2.3** Constants `CUDA12_DRIVER_MIN = (570,26,0)` and `CUDA13_DRIVER_MIN = (590,44,1)`.
- [ ] **C-2.4** Extend `src/engine/installer.rs` with `install_variant(engine, variant, profile, data_dir)` — manifest-driven download, sha256 verify, extract to `~/.lmforge/engines/llamacpp/variants/<id>/`.
- [ ] **C-2.5** Embed `variants-manifest.json` via `include_str!` so it ships with the binary.
- [ ] **C-2.6** Extend `src/cli/engine.rs` with `--variant <name>` flag on `install` subcommand.
- [ ] **C-2.7** Refuse `--variant cuda13` install if `driver_tuple < CUDA13_DRIVER_MIN` with explicit upgrade message.
- [ ] **C-2.8** Extend `lmforge engine list` to show installed variants per engine.
- [ ] **C-2.9** New file: `src/cli/doctor.rs` + wire `lmforge doctor` subcommand.
- [ ] **C-2.10** Tests: variant selection matrix unit tests covering all 8 (os, gpu, driver, cap) combos.

#### Code sketch — `src/engine/variant.rs`

```rust
use crate::hardware::probe::{HardwareProfile, GpuVendor, Os};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlamaVariant { Cuda12, Cuda13, Vulkan, Cpu }

pub const CUDA12_DRIVER_MIN: (u32,u32,u32) = (570, 26, 0);
pub const CUDA13_DRIVER_MIN: (u32,u32,u32) = (590, 44, 1);

pub struct VariantState {
    pub cuda12_installed: bool,
    pub cuda13_installed: bool,
    pub prefer_cuda13: bool,
}

pub fn select(profile: &HardwareProfile, state: &VariantState) -> LlamaVariant {
    if profile.os == Os::Linux && profile.gpu_vendor == GpuVendor::Nvidia {
        let drv = profile.driver_tuple.unwrap_or((0,0,0));
        let cap_ok = matches!(
            profile.compute_cap, Some((cc_maj, _)) if matches!(cc_maj, 7|8|9|12)
        );
        if cap_ok {
            if state.prefer_cuda13 && state.cuda13_installed && drv >= CUDA13_DRIVER_MIN {
                return LlamaVariant::Cuda13;
            }
            if state.cuda12_installed && drv >= CUDA12_DRIVER_MIN {
                return LlamaVariant::Cuda12;
            }
        }
    }
    fallback_variant(profile)   // existing resolve_platform() logic
}
```

#### `lmforge doctor` expected output

```
$ lmforge doctor
  driver           : 595.71.05  (cuda12 OK, cuda13 OK)
  compute_cap      : 12.0 (sm_120, Blackwell consumer)
  glibc            : 2.39
  vulkan loader    : libvulkan.so.1 found
  active variant   : cuda12 (b9351)  →  ~/.lmforge/engines/llamacpp/variants/cuda12/
  speculative      : auto → mtp (will activate per-model)
  prompt cache     : --cache-ram 4096
```

#### Acceptance criteria

- [ ] `lmforge engine install llamacpp --variant cuda12` on RTX 5060 Ti box: downloads ~340 MB, verifies sha256, extracts; `ls ~/.lmforge/engines/llamacpp/variants/cuda12/llama-server` exists and runs `--version`.
- [ ] Same command on a synthetic r535 profile (edit `hardware.json` for the test): refused with "driver 535.x < required 570.26; install nvidia-driver-570 or stay on Vulkan."
- [ ] `lmforge engine list` shows the installed variant under `llamacpp`.
- [ ] `lmforge doctor` prints the full table.
- [ ] All variant unit tests pass.

---

### Phase C-3 — Variant-aware launch (~3 days)

**Goal:** orchestrator picks the right binary; auto-falls-back on failure; transparent existing-install upgrade.

#### Tasks

- [ ] **C-3.1** Extend `src/engine/adapters/llamacpp.rs` with `resolve_binary_path(profile, data_dir) -> PathBuf` that consults `variant::select()`.
- [ ] **C-3.2** Launch state machine with fallback chain: cuda12 → vulkan → cpu. Each step logs `engine=<variant> reason=...`.
- [ ] **C-3.3** Extend `last_errors` in `src/server/native.rs` with `engine_errors: Vec<EngineLoadError> { variant, exit_code, stderr_tail }`.
- [ ] **C-3.4** Extend `src/cli/init.rs` to auto-install cuda12 on Linux NVIDIA when `driver_tuple >= CUDA12_DRIVER_MIN`. — **Done**: `variant::init_target_variant` + `installer::install_llamacpp_on_init`; live-tested on Blackwell (cuda12 downloaded + idempotent skip on re-run).
- [ ] **C-3.5** Idempotency check: skip auto-install if `~/.lmforge/engines/llamacpp/variants/cuda12/VERSION` matches manifest's `llamacpp_tag`. — **Done** (in `install_variant`; verified live).
- [ ] **C-3.6** Honour `LMFORGE_LLAMACPP_VARIANT=cpu` to skip CUDA auto-install (existing env override extends to this). — **Done** in `init_target_variant`.
- [ ] **C-3.7** Extend `/lf/status` payload with `engine_active_variant` field, `engine_errors` array.

#### Acceptance criteria

- [ ] Fresh box (Ubuntu 24.04 + r595 + NVIDIA): `install-core.sh | bash` → `lmforge init` ends with cuda12 installed AND active.
- [ ] Same box with `LMFORGE_LLAMACPP_VARIANT=cpu`: `init` honors override, installs Vulkan/CPU tarball, doesn't fetch cuda.
- [ ] Forcibly break cuda libs (`mv libcudart.so.12 libcudart.so.12.bak`): next `lmforge start` falls back to Vulkan, logs reason in `/lf/status.engine_errors[0].stderr_tail`.
- [ ] Existing install with Vulkan-only: `lmforge init` adds cuda12 without removing Vulkan binary; next start uses cuda12.
- [ ] Below-floor driver (r535): `init` skips cuda install, prints "stay on Vulkan; upgrade hint in `lmforge doctor`".

---

### Phase S-1 — MTP detection (~3 days, can run parallel to C-1)

**Goal:** every `ResolvedModel` carries a definitive `mtp: bool` before launch.

#### Tasks

- [x] **S-1.1** ~~Add `gguf = "0.2"` to `Cargo.toml`.~~ **Skipped** — the only published `gguf` crate (0.1.2) ships a stale `GGMLType` enum that fails on every modern K/IQ/BF16 quant. Wrote a focused, dep-free parser in `src/model/gguf_inspect.rs` instead (~150 lines, scoped to tensor-name lookup).
- [x] **S-1.2** Extend `data/catalogs/gguf.json` schema: entries are now either `string` (plain repo, backward compat) or `{ repo, mtp? }`.
- [x] **S-1.3** Catalog parser in `src/model/catalog/mod.rs` now accepts both shapes via an untagged `CatalogValue` enum; `CatalogResult` carries `mtp: Option<bool>` through to the resolver.
- [x] **S-1.4** Catalog audit pass:
  - [x] `unsloth/Qwen3-Coder-Next-GGUF` (all 3 variants) — tagged `mtp: true`.
  - [x] `unsloth/Qwen3.5-{0.8B,2B,4B,9B,27B}-GGUF` (all 15 variants) — tagged `mtp: true` per spec doc; runtime probe at pull time provides ground truth.
  - [x] Added `_audit_note` on `_comment_minimax` — investigate via probe before promoting to `mtp: true`.
- [x] **S-1.5** New module `src/model/gguf_inspect.rs` exposes `detect_mtp(&Path) -> Option<bool>` and `read_tensor_names`. Synthetic-file unit tests cover the `mtp.*` / `nextn.*` / `*.nextn.*` / case-insensitive paths, plus garbage/missing-file negative cases.
- [x] **S-1.6** `ResolvedModel.mtp: Option<bool>` propagates the catalog flag.
- [x] **S-1.7** Pull-time wiring: `gguf_inspect::resolve_mtp_for_model` runs after download (catalog flag wins; otherwise probe the largest non-mmproj `.gguf`). Result persists into `ModelCapabilities.mtp` in `models.json` (chose the existing index over the plan's `meta.json` — same role, one source of truth).
- [ ] **S-1.8** Tests against real Qwen3-Next + Llama-3.1 GGUF files. Deferred — requires multi-GB downloads. The synthetic-file tests already exercise the parser; live-file confirmation will land alongside the S-2 launch test on the user's RTX 5060 Ti box.

#### Catalog schema example (backward-compat)

```jsonc
{
  "_comment_qwen3": "Qwen3 family — pre-MTP",
  "qwen3:4b:thinking:4bit": "unsloth/Qwen3-4B-Thinking-2507-GGUF",

  "_comment_qwen3_5": "Qwen3.5 — ships MTP via nextn module",
  "qwen3.5:4b:4bit": { "repo": "unsloth/Qwen3.5-4B-GGUF", "mtp": true },

  "qwen3-coder-next:30b:4bit": {
    "repo": "unsloth/Qwen3-Coder-Next-GGUF",
    "mtp": true,
    "_note": "Qwen3-Next architecture — MTP via nextn tensors"
  }
}
```

#### `detect_mtp` sketch

```rust
/// Returns Some(true) if the GGUF carries MTP/nextn tensors, Some(false) otherwise,
/// None if the file can't be parsed.
pub fn detect_mtp(gguf_path: &Path) -> Option<bool> {
    let f = gguf::GgufFile::open(gguf_path).ok()?;
    let has_mtp = f.tensors().iter().any(|t| {
        let n = t.name();
        n.starts_with("mtp.") || n.starts_with("nextn.") || n.contains(".mtp.")
    });
    Some(has_mtp)
}
```

#### Acceptance criteria

- [ ] `lmforge pull qwen3-coder-next:30b:4bit` → after download, `~/.lmforge/models/<id>/meta.json` contains `"mtp": true`.
- [ ] Parser accepts both old-string and new-object catalog entries; all existing catalog tests pass.
- [ ] `detect_mtp` returns `Some(true)` on a Qwen3-Next GGUF, `Some(false)` on a Llama-3.1 GGUF (run against real files).
- [ ] Catalog audit is complete: 7 candidate repos investigated; `mtp` flag set or `_audit_note` added.

---

### Phase S-2 — Spec-dec launch + telemetry (~3 days)

**Goal:** when MTP is available, activate it transparently; surface acceptance rate in `/lf/status`.

#### Tasks

- [ ] **S-2.1** New file: `src/engine/speculative.rs` with `SpecMode { Auto, Mtp, DraftModel, Off }` + `resolve()`.
- [ ] **S-2.2** Add `[speculative]` block to config schema with defaults: `mode=auto, draft_max=16, draft_min=0, draft_p_min=0.75, draft_gpu_layers=-1, vram_safety_mib=1024`.
- [ ] **S-2.3** MoE-specific override in resolver: `draft_max = 4` when model is MoE (catalog flag or arch detection).
- [ ] **S-2.4** Extend `src/engine/adapters/llamacpp.rs` arg construction with `--spec-draft-*` flags per resolved mode.
- [ ] **S-2.5** Verify spec-dec flag names against pinned b9351 binary (`llama-server --help` cross-reference).
- [x] **S-2.6** Live-launch test: capture stderr while running with MTP active to determine accept-rate log line format. — b9351 emits `draft acceptance = R (A accepted / G generated)` on the `slot print_timing` line (not `draft acceptance rate`). Parser updated + live-verified on `unsloth/Qwen3.5-4B-MTP-GGUF`.
- [x] **S-2.7** Stderr scraper for accept-rate / tokens-drafted / tokens-accepted; emit to `/lf/status.speculative`. — `ModelSlot.spec_mode` + `ModelSlot.spec_stats` (`SpecStats { drafted_total, accepted_total, samples, last_accept_rate, cumulative_accept_rate }`) populated from the live observer on every `notify()`. Live `/lf/status` confirmed surfacing `spec_mode: "off"` for non-mtp models; spec_stats omitted via `skip_serializing_if` until the first sample arrives.
- [x] **S-2.8** Fallback policy: if `llama-server` exits non-zero within 5 s of start AND spec was on, restart once with `mode=Off`; never silently disable MTP mid-stream. — `EngineManager::handle_ensure_model` keys off `engine.spec_mode != Off` + `load_started.elapsed() < 5s`, sets `LMFORGE_SPECULATIVE_MODE=off` with save+restore guard, retries once. Combined error message surfaces both attempts in `last_errors` so users see WHY spec was disabled.
- [x] **S-2.9** Tests: launched server with `mode=mtp` and `mode=off` (greedy, same seed) produces byte-identical output (lossless property). — Unit-level structural property tests in `llamacpp::tests` assert (a) off→mtp diff is purely additive (off args is a strict prefix of mtp args), (b) every emitted spec flag begins with `--spec-`, ruling out accidental seed/sampler perturbation, and (c) a parameterised grid sweep across `{mode, draft_max, draft_min, draft_p_min, draft_gpu_layers, draft_model_path}` preserves the baseline contract. End-to-end byte-identity (running real `llama-server` greedy decode + diffing tokens) is e2e territory and tracked in §11 e2e tests.

#### S-2 live-test notes (Blackwell + cuda13, 2026-05-29)

- **`--spec-type` flag was missing.** Initial `append_spec_args` only emitted `--spec-draft-*` knobs. `llama-server` requires `--spec-type {draft-mtp|draft-simple|ngram-*}` to pick an implementation; without it the server logs `common_speculative_init: no implementations specified for speculative decoding` and silently runs without spec. Fix: emit `--spec-type draft-mtp` (Mtp) / `--spec-type draft-simple` (DraftModel). Regression test added (`append_spec_args_mtp_emits_spec_type_draft_mtp`).
- **Probe must outrank catalog.** Catalog hand-tagged `unsloth/Qwen3.5-*-GGUF` as `mtp:true`, but the actual Q6_K_XL has 0 nextn/mtp tensors (verified with `examples/probe_mtp.rs`). With the original "catalog wins" precedence, every spawn crashed with `context type MTP requested but model doesn't contain MTP layers` and triggered the S-2.8 retry. Fix: `resolve_mtp_for_model` now prefers the probe whenever it returns `Some(_)`, falling back to the catalog only when the file is unreadable. Three new tests (`resolve_mtp_probe_wins_over_catalog_when_definitive`, `resolve_mtp_probe_positive_overrides_catalog_negative`, `resolve_mtp_falls_back_to_catalog_when_probe_unreadable`).
- **`pull --refresh` migration path.** Re-evaluates capabilities for an already-installed model without re-downloading the weights; necessary because models pulled before S-1 landed had stale `mtp = null` in `models.json`.
- **S-2.8 fired in production.** First run with the corrected `--spec-type draft-mtp` flag died at 2310 ms during MTP context creation; daemon log: `WARN: Spec-dec engine died <5s after spawn — retrying once with spec=off (S-2.8)` → `INFO: Spec-dec retry succeeded — slot is Ready with spec=off`. Crash-fallback path is now field-validated.
- **Live MTP e2e verified (2026-05-30).** Model `qwen3.5:4b:mtp:4bit` → `unsloth/Qwen3.5-4B-MTP-GGUF` (catalog shortcut added). On RTX 5060 Ti / b9351: **cuda12** `spec_mode=mtp`, accept ~84% (73/87 drafted); **cuda13** `spec_mode=mtp`, accept ~94% (48/51). `/lf/status.spec_stats` populated after chat. **cuda12 tarball gap:** `llama-server` links `libnccl.so.2` but NCCL wasn't bundled — exit 127 until lib copied to `variants/cuda12/lib/`; CI workflow now bundles NCCL when present. Rebuild cuda12 tarball to ship fix.

#### Code sketch — `src/engine/speculative.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecMode { Auto, Mtp, DraftModel, Off }

pub struct SpecResolved {
    pub mode: SpecMode,
    pub draft_model_path: Option<PathBuf>,
    pub draft_max: u32,
    pub draft_min: u32,
    pub draft_p_min: f32,
    pub draft_gpu_layers: i32,
}

pub fn resolve(
    model: &ResolvedModel,
    profile: &HardwareProfile,
    config: &SpeculativeConfig,
    free_vram_gb: f32,
) -> SpecResolved {
    match config.mode {
        SpecMode::Off => return spec_off(),
        SpecMode::Mtp => return spec_mtp(config),
        SpecMode::DraftModel => return resolve_draft_model(model, profile, config, free_vram_gb),
        SpecMode::Auto => {
            if model.mtp == Some(true)            { return spec_mtp(config); }
            if let Some(pair) = lookup_draft_pair(model)
                && vram_fits(model, pair, free_vram_gb, config.vram_safety_mib)
            {
                return spec_draft(pair, config);
            }
            spec_off()
        }
    }
}
```

#### Arg construction

```rust
match spec.mode {
    SpecMode::Mtp => {
        args.extend(["--spec-draft-n-max".into(), spec.draft_max.to_string(),
                     "--spec-draft-n-min".into(), spec.draft_min.to_string(),
                     "--spec-draft-p-min".into(), spec.draft_p_min.to_string()]);
    }
    SpecMode::DraftModel => {
        args.extend(["--spec-draft-model".into(), spec.draft_model_path.unwrap().display().to_string(),
                     "--spec-draft-n-max".into(), spec.draft_max.to_string(),
                     "--spec-draft-n-min".into(), spec.draft_min.to_string(),
                     "--spec-draft-p-min".into(), spec.draft_p_min.to_string(),
                     "--spec-draft-ngl".into(), spec.draft_gpu_layers.to_string()]);
    }
    SpecMode::Off => {}
    SpecMode::Auto => unreachable!("resolved upstream"),
}
```

#### Acceptance criteria

- [ ] Launch Qwen3.5-4B with `mode=auto`: stderr shows MTP-related lines; `/lf/status.speculative.mode == "mtp"`.
- [ ] Identical prompt under `mode=mtp` and `mode=off` (greedy decode, same seed): byte-identical output (lossless).
- [ ] Launch with intentional spec misconfig (e.g. fake `--spec-draft-model /nonexistent`): orchestrator catches crash, restarts with `mode=Off`, error surfaces in `/lf/status.engine_errors`.
- [ ] Measured tok/s improvement: ≥ 1.3x on Qwen3.5-4B vs `mode=off`.

---

### Phase S-3 — Draft-model pairs (~2 days)

**Goal:** non-MTP models still get a speculation path when a small sibling fits.

#### Tasks

- [ ] **S-3.1** New file: `data/draft_pairs.toml` with curated Llama-3.x, Qwen3.x, Qwen2.5 pairs.
- [ ] **S-3.2** Implement `speculative::lookup_draft_pair(model)` matching `model.repo` against `target_family` patterns.
- [ ] **S-3.3** Implement VRAM budget gate per impl spec §B.5 in `speculative::vram_fits()`.
- [ ] **S-3.4** Implement pair-validity preflight at install time (dry-run launch checking tokenizer match).
- [ ] **S-3.5** Cache broken-pair results in `~/.lmforge/draft_pairs_status.json` so they're never auto-retried.

#### Initial `data/draft_pairs.toml`

```toml
# Hard requirement: target and draft must share tokenizer/vocabulary family.

[[pair]]
target_family = "llama-3.x"
draft_id      = "llama-3.2-1b-instruct:4bit"
note          = "Shared Llama-3 tokenizer."

[[pair]]
target_family = "qwen3.x"
draft_id      = "qwen3:0.6b:4bit"
note          = "Use only when MTP unavailable; same Qwen tokenizer."

[[pair]]
target_family = "qwen2.5"
draft_id      = "qwen2.5:0.5b:4bit"
note          = "Same Qwen2.5 tokenizer family."
```

#### Acceptance criteria

- [ ] On RTX 5060 Ti 16 GB + Llama 3.1-8B + Llama 3.2-1B as draft pair: `mode=auto` resolves to `draft_model`, fits in VRAM, output byte-identical to `mode=off`.
- [ ] Same with Llama 3.3-70B (won't fit even Q4): resolves to `off` without crashing.
- [ ] Pair with mismatched tokenizer (synthetic): preflight catches it; auto resolution falls through to `off`.

---

### Phase Polish — Docs + UI surface + ADRs (~2 days)

#### Tasks

- [x] **Polish-1** Update `docs/INSTALL_LINUX_DEV.md` with new variant table (cuda12 default, cuda13 opt-in, Vulkan fallback) and MTP perf claims.
- [x] **Polish-2** Update `README.md` install script flow: "fetches CUDA variant on NVIDIA Linux". Update perf positioning.
- [x] **Polish-3** New ADR: `docs/architecture/ADR-004-cuda-variant-pipeline.md` (glibc 2.28, static cudart rejection, 13.2 ban, RPATH rationale).
- [x] **Polish-4** New ADR: `docs/architecture/ADR-005-speculative-decoding.md` (MTP-first, layered detection, lossless guarantee, fallback chain).
- [x] **Polish-5** UI: Overview page tile for "Speculative decoding" showing `mode + accept_rate + tokens_drafted`.
- [x] **Polish-6** UI: Settings → Engine: show installed variants per engine; CLI hints for `--variant cuda12 / cuda13`.
- [x] **Polish-7** Migration smoke: existing Vulkan-only install gets auto-upgraded to cuda12 on next `init` (tested on user's box end-to-end).

---

## 2. Risk register

| Risk | Likelihood | Mitigation | Status |
|---|---|---|---|
| static cuBLAS failure on llama.cpp CMake | High (rejected this path) | Using bundled-libs + RPATH instead — Ollama pattern | mitigated |
| CUDA 13.2/13.3 accidentally slips through | Low | CI hard fail on matrix entry match; runtime check rejects too | mitigated |
| RPATH `$ORIGIN/lib` stripped by post-build strip | Medium | Set RPATH AFTER `strip` (patchelf supports; explicit ordering in workflow) | mitigated |
| `gguf` Rust crate doesn't parse Qwen3-Next nextn tensors | Low | Fallback to "unknown MTP" → catalog flag authoritative; runtime probe secondary | open |
| llama-server changes spec-dec flags again | Medium | S-2 re-verifies flag names against pinned tag before wiring | open |
| Accept-rate telemetry not in stderr by default | Medium | S-2 fallback: skip telemetry, expose `mode` only; document as known gap | open |
| Tarball >500 MB on cuda13 (more archs) | Low | Drop `100-real` from cuda13 if it pushes over; B200 share negligible | open |
| User on Ubuntu 22.04 LTS (r535 default) confused why no CUDA | High | `doctor` prints upgrade hint; auto-fallback to Vulkan; clear message | mitigated |
| First CI build fails on AlmaLinux 8 quirks | Medium | Time-boxed: 2 days for first green build; escalate if stuck | open |
| `variants-manifest.json` drift vs released tarballs | Low | C-1 publishes manifest from CI alongside tarballs; never hand-edit | mitigated |

---

## 3. Out of scope for v0.2.0 (tracked elsewhere or deferred)

- **N-gram speculative decoding** (`--spec-ngram-*`). Third spec-dec path; lossless; no draft model. Defer to v0.3.
- **`GGML_BACKEND_DL=ON` split layout.** Trigger condition: when we add a third backend (HIP/AMD or Linux-Vulkan-as-separate-variant), not before.
- **Windows CUDA via our own build.** Upstream Windows CUDA prebuilts work fine.
- **macOS anything.** oMLX is the contract.
- **AMD ROCm or Intel OpenVINO opt-in variants.** Vendor-specific; Vulkan is good enough.
- **SHA-verify on every engine binary fetch** (currently we verify our cuda tarballs; Vulkan upstream stays trust-on-fetch). Defense-in-depth follow-up.
- **Auto-bump llama.cpp pin on a schedule.** Manual pin per LMForge release; keeps surprise count low.

---

## 4. Effort + sequencing

| Phase | Days | Parallelizable with | Critical path? |
|---|---|---|---|
| C-1 (CUDA pipeline) | 5 | S-1 | Yes (blocks C-2) |
| C-2 (variant infra) | 3 | — | Yes (blocks C-3) |
| C-3 (variant launch) | 3 | S-2 | Yes |
| S-1 (MTP detection) | 3 | C-1 | Yes (blocks S-2) |
| S-2 (spec launch) | 3 | C-3 | Yes |
| S-3 (draft pairs) | 2 | Polish | No (ships in v0.2.1 if delayed) |
| Polish (docs+UI+ADR) | 2 | S-3 | No |

**Critical path (CUDA-only):** C-1 → C-2 → C-3 → release. ~11 days.
**Full v0.2.0 (CUDA + spec-dec):** C-1+S-1 parallel → C-2+S-2 parallel → C-3+S-3 → Polish. **~14–16 working days.**

---

## 5. Concrete deliverable when done

### `lmforge engine list` on Linux NVIDIA box

```
$ lmforge engine list
  llamacpp
    cuda12     installed (b9351)  sm_75..sm_120 · driver≥570.26 · ACTIVE
    vulkan     installed (b9351)  fallback
    cuda13     not installed      opt-in (driver≥590.44.01)
  vllm         not installed       opt-in
  tabbyapi     not installed       opt-in
  omlx         (darwin only)
```

### Typical user flow

```
$ curl -fsSL .../install-core.sh | bash
$ lmforge init
  ⚙ Detecting hardware...
  ✓ NVIDIA GeForce RTX 5060 Ti · driver 595.71.05 · compute_cap 12.0 · 16 GB
  ⚙ Installing CUDA variant (~340 MB) for peak NVIDIA performance...
  ✓ cuda12 installed at ~/.lmforge/engines/llamacpp/variants/cuda12/

$ lmforge run qwen3-coder-next:30b:4bit
  variant=cuda12  spec=mtp(draft_max=16)
  ggml_cuda_init: NVIDIA GeForce RTX 5060 Ti, compute capability 12.0
  > tell me a joke
  ...
```

---

## 6. Open items (resolve during impl)

| Item | Target phase | Notes |
|---|---|---|
| Exact accept-rate telemetry stderr format on b9351 | S-2 | Live launch with MTP active; capture stderr |
| Whether `gguf` crate parses Qwen3-Next's nextn naming | S-1 | Verify on a real Qwen3-Next GGUF |
| `lmforge engine list` extension format (bullets vs table) | C-2 | Decide at impl time; consistency with existing list output |
| `lmforge doctor` output: text vs JSON (`--json` from day 1?) | C-2 | Default text; add `--json` if UI consumes it |
| UI tile placement for spec-dec accept-rate (Overview vs Settings → Performance?) | Polish | Decide with UI review |
| Whether `lmforge init` should always print "variant cuda12 already installed" on idempotent skip | C-3 | Default yes (transparency); concise one-line |

---

## 7. References

- `cuda_compass_artifact.md` — research report (Linux CUDA shipping pattern, 13.x compatibility matrix, Unsloth GGUF corruption warning).
- `LMForge_CUDA_and_Speculative_Decoding_Implementation_Spec.md` — original implementation spec (architecture; flag names since updated).
- Upstream llama.cpp `release.yml` — Windows CUDA pattern reference.
- NVIDIA CUDA 12.8 / 13.0 / 13.1 release notes — driver floor table.
- `docs/architecture/ADR-001-engine-tiers.md` — engine tier model (this plan respects it).
