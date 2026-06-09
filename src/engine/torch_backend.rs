//! PyTorch wheel-backend resolver for pip-installed engines.
//!
//! `uv pip install --torch-backend=<X>` accepts:
//!   * `auto`     — probe nvidia-smi at install time (uv's own logic)
//!   * `cu130`    — CUDA 13.0 wheels (sm_120 Blackwell, also works on 9.0+)
//!   * `cu128`    — CUDA 12.8 wheels (Hopper sm_90, Ada sm_89, Ampere sm_8x)
//!   * `cu126` / `cu124` / `cu121` / `cu118` — older driver paths
//!   * `rocm6.2` / `rocm6.1` — AMD ROCm
//!   * `cpu`      — no GPU, falls back to CPU torch
//!
//! Why we resolve explicitly instead of always passing `auto`:
//!
//!  1. **Reproducible installs**: `auto` re-probes nvidia-smi at every install,
//!     so two users on the same hardware can end up on different wheels if
//!     their driver was bumped between `lmforge init` calls. The resolver
//!     consults the cached `hardware.json` instead.
//!
//!  2. **Match the engine's torch pin**: each engine pins a specific torch
//!     version (vLLM 0.21 → torch 2.11, vLLM 0.11 → torch 2.8, etc.). The
//!     torch wheel index for `cu130` ships torch 2.9+, `cu128` ships 2.8.x.
//!     Send a stale engine version down the wrong wheel index and uv aborts
//!     with "no solution found". The resolver routes each (cuda runtime, engine
//!     version) pair to the wheel index that actually has the right torch.
//!
//!  3. **Honest fallback**: when we can't tell (no `cuda_runtime_version` in
//!     profile, or GPU vendor is None), we return `"auto"` and let uv handle
//!     it. We never guess.
//!
//! The `UV_TORCH_BACKEND` env var always wins — set it when you need a
//! specific wheel (CI, debugging, pinning to a known-good combo).

use crate::hardware::probe::{ComputeCap, GpuVendor, HardwareProfile};

/// Resolved torch wheel backend string, ready to pass to `uv pip install`.
///
/// Carries the *origin* so the installer can log "from env / from hardware /
/// auto-fallback" — important when triaging a wrong-wheel install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorchBackend {
    pub value: String,
    pub origin: TorchBackendOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorchBackendOrigin {
    /// `UV_TORCH_BACKEND` env var set by the user / CI.
    Env,
    /// Derived from the cached hardware profile (compute_cap + cuda_runtime_version).
    Profile,
    /// Couldn't tell — fell back to uv's own `--torch-backend=auto`.
    AutoFallback,
}

impl TorchBackend {
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Decide which torch wheel backend to pass to `uv pip install`.
///
/// Priority:
///   1. `UV_TORCH_BACKEND` env var (any non-empty value)
///   2. Hardware profile heuristic (CUDA runtime + compute cap)
///   3. `"auto"` — uv probes nvidia-smi itself.
pub fn resolve(profile: &HardwareProfile) -> TorchBackend {
    if let Ok(v) = std::env::var("UV_TORCH_BACKEND")
        && !v.trim().is_empty()
    {
        return TorchBackend {
            value: v.trim().to_string(),
            origin: TorchBackendOrigin::Env,
        };
    }

    if let Some(value) = derive_from_profile(profile) {
        return TorchBackend {
            value,
            origin: TorchBackendOrigin::Profile,
        };
    }

    TorchBackend {
        value: "auto".to_string(),
        origin: TorchBackendOrigin::AutoFallback,
    }
}

/// Pure function: map `HardwareProfile` → wheel-backend string.
/// Returns `None` when we lack the information to decide (let uv `auto` handle it).
fn derive_from_profile(profile: &HardwareProfile) -> Option<String> {
    match profile.gpu_vendor {
        GpuVendor::None => Some("cpu".to_string()),
        GpuVendor::Apple => Some("cpu".to_string()), // torch on Apple uses MPS via the cpu wheel
        GpuVendor::Amd => derive_rocm(profile),
        GpuVendor::Nvidia => derive_cuda(profile),
        // No production-grade Intel-XPU wheel for torch on Linux/Windows yet.
        // vLLM + TabbyAPI never run on Intel iGPUs in practice anyway (the gate
        // matrix in engines.toml already routes Intel users to llama.cpp).
        GpuVendor::Intel => Some("cpu".to_string()),
    }
}

/// NVIDIA: prefer CUDA runtime → wheel mapping. Falls back to compute cap when
/// runtime is unknown but the GPU clearly identifies an arch range.
fn derive_cuda(profile: &HardwareProfile) -> Option<String> {
    if let Some(rt) = profile.cuda_runtime_version.as_deref()
        && let Some(wheel) = cuda_runtime_to_wheel(rt)
    {
        return Some(wheel);
    }

    // Last-ditch by compute cap: sm_120 has no published cu128-stable path that
    // boots vLLM/SGLang reliably; refuse to guess older trains down.
    if let Some(cc) = profile.compute_cap
        && compute_cap_needs_cu130(cc)
    {
        return Some("cu130".to_string());
    }
    None
}

/// Map "MAJOR.MINOR" CUDA runtime strings → uv torch-backend wheel ids.
///
/// We only return a value when the mapping is unambiguous. For point versions
/// uv doesn't publish (e.g. 12.7), we fall back to the closest stable
/// neighbour to avoid wheel-resolution failures.
pub fn cuda_runtime_to_wheel(rt: &str) -> Option<String> {
    let (major, minor) = parse_version(rt)?;
    Some(match (major, minor) {
        // CUDA 13.x — all map to cu130 (only published cu13 wheel as of May 2026).
        (13, _) => "cu130".to_string(),
        // CUDA 12.8 / 12.9 → cu128 (uv doesn't publish cu129 wheels; 128 is
        // ABI-compatible with the 12.9 driver).
        (12, 9) | (12, 8) => "cu128".to_string(),
        (12, 6) | (12, 7) => "cu126".to_string(),
        (12, 4) | (12, 5) => "cu124".to_string(),
        (12, 1) | (12, 2) | (12, 3) => "cu121".to_string(),
        (12, 0) => "cu121".to_string(),
        // CUDA 11.x — uv ships cu118 as the floor; older drivers should
        // upgrade to access modern engines.
        (11, _) => "cu118".to_string(),
        _ => return None,
    })
}

/// Compute caps that REQUIRE cu130 wheels.
///
/// As of vLLM 0.20+ / torch 2.11, sm_120 (consumer Blackwell) is in the
/// default cu130 arch list (vllm-project/vllm#39878). Sending an sm_120 box
/// to a cu128 wheel still works for the simple chat path, but FlashInfer
/// and the NVFP4 kernels are only built for cu130 there. Pinning cu130 for
/// any sm_12x host keeps the install reproducible and avoids the FlashInfer
/// "no sm_120 kernel" runtime warning.
fn compute_cap_needs_cu130(cc: ComputeCap) -> bool {
    let (maj, _min) = cc;
    maj >= 12
}

/// AMD: ROCm version detection isn't wired in HardwareProfile yet — return
/// `None` so uv picks its current default ROCm wheel. Tracked in ADR-001 as
/// "ROCm support deferred to Phase 5".
fn derive_rocm(_profile: &HardwareProfile) -> Option<String> {
    None
}

/// Parse "MAJOR.MINOR[.PATCH]" → `(major, minor)`. Returns `None` on malformed input.
fn parse_version(s: &str) -> Option<(u8, u8)> {
    let trimmed = s.trim();
    let mut parts = trimmed.split('.');
    let major: u8 = parts.next()?.parse().ok()?;
    let minor: u8 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::probe::{Arch, GpuVendor, Os};
    use std::sync::Mutex;

    /// All tests in this module mutate `UV_TORCH_BACKEND`. Serialize them so
    /// running with `--test-threads=N>1` doesn't see torn reads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn nvidia_profile(rt: Option<&str>, cc: Option<ComputeCap>) -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            cuda_runtime_version: rt.map(|s| s.to_string()),
            compute_cap: cc,
            ..Default::default()
        }
    }

    fn clear_env() {
        // SAFETY: `ENV_LOCK` held by the caller serialises env mutation.
        unsafe { std::env::remove_var("UV_TORCH_BACKEND") };
    }

    #[test]
    fn env_var_overrides_everything() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        // SAFETY: lock held.
        unsafe { std::env::set_var("UV_TORCH_BACKEND", "cu126") };

        // Hardware would suggest cu130 — env must win.
        let p = nvidia_profile(Some("13.0"), Some((12, 0)));
        let r = resolve(&p);
        assert_eq!(r.value, "cu126");
        assert_eq!(r.origin, TorchBackendOrigin::Env);

        clear_env();
    }

    #[test]
    fn empty_env_var_is_ignored() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe { std::env::set_var("UV_TORCH_BACKEND", "   ") };
        let p = nvidia_profile(Some("12.8"), Some((9, 0)));
        let r = resolve(&p);
        assert_eq!(r.value, "cu128");
        assert_eq!(r.origin, TorchBackendOrigin::Profile);
        clear_env();
    }

    #[test]
    fn cuda_13_maps_to_cu130() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        // Both points in the CUDA 13 train should land on cu130.
        for rt in ["13.0", "13.2"] {
            let p = nvidia_profile(Some(rt), Some((12, 0)));
            assert_eq!(resolve(&p).value, "cu130", "rt={}", rt);
        }
    }

    #[test]
    fn cuda_12_8_and_12_9_map_to_cu128() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        for rt in ["12.8", "12.9"] {
            let p = nvidia_profile(Some(rt), Some((9, 0)));
            assert_eq!(resolve(&p).value, "cu128", "rt={}", rt);
        }
    }

    #[test]
    fn cuda_12_6_maps_to_cu126() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let p = nvidia_profile(Some("12.6"), Some((8, 6)));
        assert_eq!(resolve(&p).value, "cu126");
    }

    #[test]
    fn unknown_runtime_with_sm120_forces_cu130() {
        // Even with no runtime version, sm_120 must land on cu130 — that's
        // where FlashInfer + NVFP4 kernels live in vLLM 0.20+ / torch 2.11.
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let p = nvidia_profile(None, Some((12, 0)));
        let r = resolve(&p);
        assert_eq!(r.value, "cu130");
        assert_eq!(r.origin, TorchBackendOrigin::Profile);
    }

    #[test]
    fn unknown_runtime_with_hopper_falls_back_to_auto() {
        // Hopper (sm_90) works on cu128 wheels; without runtime version we
        // shouldn't pin a wheel, let uv decide.
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let p = nvidia_profile(None, Some((9, 0)));
        let r = resolve(&p);
        assert_eq!(r.value, "auto");
        assert_eq!(r.origin, TorchBackendOrigin::AutoFallback);
    }

    #[test]
    fn no_gpu_maps_to_cpu() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let mut p = nvidia_profile(None, None);
        p.gpu_vendor = GpuVendor::None;
        assert_eq!(resolve(&p).value, "cpu");
    }

    #[test]
    fn apple_maps_to_cpu_wheel() {
        // torch ships MPS support inside its "cpu" wheel for darwin-arm64;
        // there's no cu/rocm wheel that applies.
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let mut p = nvidia_profile(None, None);
        p.gpu_vendor = GpuVendor::Apple;
        assert_eq!(resolve(&p).value, "cpu");
    }

    #[test]
    fn amd_defers_to_auto() {
        // ROCm version probing isn't wired yet — let uv handle it.
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let mut p = nvidia_profile(None, None);
        p.gpu_vendor = GpuVendor::Amd;
        assert_eq!(resolve(&p).value, "auto");
    }

    #[test]
    fn malformed_runtime_falls_back() {
        assert!(cuda_runtime_to_wheel("garbage").is_none());
        assert!(cuda_runtime_to_wheel("13").is_none());
        assert!(cuda_runtime_to_wheel("").is_none());
    }

    #[test]
    fn cuda_11_maps_to_cu118() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let p = nvidia_profile(Some("11.8"), Some((7, 5)));
        assert_eq!(resolve(&p).value, "cu118");
    }

    #[test]
    fn future_cuda_14_returns_none_so_auto_wins() {
        // Unknown future runtime → resolver shouldn't pretend to know.
        assert!(cuda_runtime_to_wheel("14.0").is_none());
    }
}
