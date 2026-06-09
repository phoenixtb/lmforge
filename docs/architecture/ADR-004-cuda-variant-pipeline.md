# ADR-004: Linux NVIDIA CUDA variant pipeline

- **Status:** Accepted (2026-05-30)
- **Follows:** [PLAN-v0.2.0-cuda-mtp](../engineering/PLAN-v0.2.0-cuda-mtp.md) Phase C-1

## Context

Linux NVIDIA hosts previously used upstream Vulkan `llama.cpp` builds. Blackwell
(`sm_120`) and MTP speculative decoding need a CUDA path with pinned toolkit
versions and bundled runtime libraries.

## Decision

1. **Tarball delivery, not fat binary in release artifact.** `lmforge` release
   ships naked; `lmforge init` downloads `cuda12` (default) or opt-in `cuda13`
   from `variants-manifest.json` (SHA256-verified).

2. **Bundled libs + RPATH, not static cudart.** Each variant tarball ships
   `libcudart`, `libcublas`, `libcublasLt`, and all `llama.cpp` `.so` files
   under `lib/` with `RUNPATH=$ORIGIN/lib`. Only `libcuda.so.1` comes from the
   host driver.

3. **Builder base:** `nvidia/cuda:12.8.1-devel-rockylinux8` (glibc 2.28) for
   broad distro compatibility.

4. **Hard ban:** CUDA 13.2 and 13.3 — confirmed GGUF corruption. CI refuses
   those matrix entries.

5. **Driver floors:** cuda12 ≥ r570.26; cuda13 ≥ r590.44.01. Below floor →
   Vulkan fallback, upgrade hint in `lmforge doctor`.

6. **Object storage:** Cloudflare R2 bucket `lmforge-engine-assets`, public via CDN subdomain (`cdn_base` in manifest). Decoupled from LMForge semver. See [R2-ENGINE-ASSETS.md](../engineering/R2-ENGINE-ASSETS.md).

## Consequences

- First `init` on NVIDIA Linux downloads ~1 GB (cuda12). Idempotent on re-run.
- Windows NVIDIA unchanged (upstream CUDA zips).
- macOS unchanged (oMLX only).
