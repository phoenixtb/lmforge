//! `llama.cpp` variant selector + embedded release manifest.
//!
//! Phase C-2 of the v0.2.0 plan. This module owns the answer to two
//! questions a higher-level component asks:
//!
//!   1. "Given hardware X and a set of already-installed variants, which
//!      `llama.cpp` build should `lmforge start` actually launch?"
//!      → [`select`].
//!
//!   2. "What's the URL + sha256 for the `cuda12` tarball that the CUDA
//!      build workflow published?"
//!      → [`Manifest`] (loaded once from the embedded JSON).
//!
//! C-3 wires the result into the spawn path. This file deliberately stays
//! free of any process / filesystem / network side effects so the matrix
//! tests below can pin down the selector contract.

use serde::{Deserialize, Serialize};

use crate::hardware::probe::{GpuVendor, HardwareProfile, Os};

// ── Driver-floor constants ───────────────────────────────────────────────────

/// Minimum NVIDIA driver tuple `(major, minor, patch)` required to run a
/// CUDA 12.8.1 build. Below this we refuse to install / activate cuda12 —
/// older drivers either lack required symbols or crash on Blackwell PTX.
pub const CUDA12_DRIVER_MIN: (u32, u32, u32) = (570, 26, 0);

/// Minimum NVIDIA driver tuple for the CUDA 13.1.x opt-in variant. Higher
/// floor because NVIDIA only ships a r590+ branch supporting the CUDA 13
/// runtime ABI.
pub const CUDA13_DRIVER_MIN: (u32, u32, u32) = (590, 44, 1);

// ── Variant tag ──────────────────────────────────────────────────────────────

/// Concrete `llama.cpp` build flavour. Distinct from "the engine is
/// `llamacpp`" — every active engine on Linux/Windows picks exactly one
/// of these variants at spawn time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlamaVariant {
    /// Custom-built CUDA 12.8.1 tarball with bundled cudart / cuBLAS.
    /// Default on Linux NVIDIA when driver ≥ r570.26 and compute_cap is
    /// in `{sm_86…sm_120}`. Pre-Ampere users (sm_75 Turing, sm_80 A100)
    /// are routed to Vulkan.
    Cuda12,
    /// CUDA 13.1.x opt-in variant. Adds `sm_100` (B200) to the arch matrix
    /// but requires driver ≥ r590.44.01.
    Cuda13,
    /// Vulkan backend — universal fallback for Linux + Windows AMD/Intel.
    /// Works on NVIDIA too via the proprietary driver's Vulkan ICD; used
    /// as the auto-fallback on Linux NVIDIA below the CUDA12 driver floor.
    Vulkan,
    /// CPU-only build — used when no GPU is present, or the user forced it
    /// via `LMFORGE_LLAMACPP_VARIANT=cpu`.
    Cpu,
}

impl LlamaVariant {
    /// Stable string id used in install paths
    /// (`~/.lmforge/engines/llamacpp/variants/<id>/`), the manifest JSON,
    /// and the `lmforge engine list` / `lmforge doctor` output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cuda12 => "cuda12",
            Self::Cuda13 => "cuda13",
            Self::Vulkan => "vulkan",
            Self::Cpu => "cpu",
        }
    }
}

impl std::fmt::Display for LlamaVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LlamaVariant {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cuda12" => Ok(Self::Cuda12),
            "cuda13" => Ok(Self::Cuda13),
            "vulkan" => Ok(Self::Vulkan),
            "cpu" => Ok(Self::Cpu),
            other => Err(format!(
                "unknown llama.cpp variant `{other}` (expected one of: cuda12, cuda13, vulkan, cpu)"
            )),
        }
    }
}

// ── Selector input + output ──────────────────────────────────────────────────

/// What's currently installed on disk plus the user's preference. Filled
/// by the caller from a directory scan of
/// `~/.lmforge/engines/llamacpp/variants/`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VariantState {
    pub cuda12_installed: bool,
    pub cuda13_installed: bool,
    pub vulkan_installed: bool,
    pub cpu_installed: bool,
    /// If true and `cuda13_installed`, the selector prefers cuda13 over
    /// cuda12. Driven by `LMFORGE_LLAMACPP_VARIANT=cuda13` or an explicit
    /// `lmforge engine install llamacpp --variant cuda13`.
    pub prefer_cuda13: bool,
}

/// Pure variant selection — no env reads, no I/O, no panics.
///
/// Rules (Linux NVIDIA path):
///   1. If `compute_cap` is outside `{sm_7x, sm_8x, sm_9x, sm_12x}`, the
///      CUDA path is rejected outright (older Volta/Pascal cards are
///      handled by the universal Vulkan binary instead).
///   2. If both `cuda12_installed` && `cuda13_installed`:
///      - `prefer_cuda13 && driver ≥ CUDA13_DRIVER_MIN` → `Cuda13`
///      - else if `driver ≥ CUDA12_DRIVER_MIN` → `Cuda12`
///   3. If only `cuda13_installed && driver ≥ CUDA13_DRIVER_MIN` → `Cuda13`.
///      The user explicitly opted into cuda13 by installing it; we don't
///      silently downgrade to Vulkan just because `prefer_cuda13` defaults
///      to false (see scan_variant_state — it only flips on env override).
///   4. If only `cuda12_installed && driver ≥ CUDA12_DRIVER_MIN` → `Cuda12`.
///   5. Otherwise fall through to platform fallback (Vulkan / CPU).
///
/// Non-Linux NVIDIA and every other vendor / OS combination falls through
/// directly to the platform fallback. Windows NVIDIA still goes through
/// the upstream `win-cuda-*` build path owned by
/// `installer::resolve_platform` — this selector intentionally does NOT
/// touch Windows so the existing flow stays as-is.
pub fn select(profile: &HardwareProfile, state: &VariantState) -> LlamaVariant {
    if matches!(profile.os, Os::Linux) && profile.gpu_vendor == GpuVendor::Nvidia {
        let driver = profile
            .driver_tuple
            .or_else(|| {
                profile
                    .cuda_driver_version
                    .as_deref()
                    .and_then(crate::hardware::probe::parse_driver_tuple)
            })
            .unwrap_or((0, 0, 0));

        let cap_ok = matches!(
            profile.compute_cap,
            Some((cc_maj, _)) if matches!(cc_maj, 7 | 8 | 9 | 12)
        );

        if cap_ok {
            // When both are installed the user is explicitly tie-breaking,
            // so honour `prefer_cuda13`. Driver floors gate each variant
            // independently — a driver that's r580 (below cuda13 floor)
            // but ≥ cuda12 floor will pick cuda12 even with prefer_cuda13.
            if state.cuda13_installed
                && state.cuda12_installed
                && state.prefer_cuda13
                && driver >= CUDA13_DRIVER_MIN
            {
                return LlamaVariant::Cuda13;
            }
            if state.cuda12_installed && driver >= CUDA12_DRIVER_MIN {
                return LlamaVariant::Cuda12;
            }
            // Only one CUDA variant installed: use it. Don't downgrade to
            // Vulkan just because the OTHER CUDA variant isn't on disk.
            // Installation was an explicit user act.
            if state.cuda13_installed && driver >= CUDA13_DRIVER_MIN {
                return LlamaVariant::Cuda13;
            }
        }
    }

    fallback_variant(profile)
}

/// Variant we'd pick if no CUDA variant is installed (or the driver / cap
/// gates refuse it). Mirrors `installer::resolve_platform`'s GPU-vs-CPU
/// decision so the two stay aligned.
fn fallback_variant(profile: &HardwareProfile) -> LlamaVariant {
    let has_gpu = matches!(
        profile.gpu_vendor,
        GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel
    );
    if has_gpu {
        LlamaVariant::Vulkan
    } else {
        LlamaVariant::Cpu
    }
}

// ── Below-floor refusal helper ───────────────────────────────────────────────

/// Reasons we'd refuse to install / activate a CUDA variant. Lifts the
/// driver-floor check out of `installer::install_variant` so both the CLI
/// (`engine install llamacpp --variant cuda13`) and the post-install
/// activation path share the same error message.
#[derive(Debug)]
pub enum RefuseReason {
    DriverBelowFloor {
        required: (u32, u32, u32),
        actual: Option<(u32, u32, u32)>,
    },
    NotNvidia,
    UnsupportedComputeCap(Option<crate::hardware::probe::ComputeCap>),
}

impl std::fmt::Display for RefuseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DriverBelowFloor { required, actual } => {
                let (rmaj, rmin, rpat) = *required;
                match actual {
                    Some((maj, min, pat)) => write!(
                        f,
                        "NVIDIA driver {maj}.{min}.{pat} is below the required floor {rmaj}.{rmin}.{rpat} for this CUDA variant. \
                         Upgrade with `sudo apt install nvidia-driver-{rmaj}` (Ubuntu) or your distro's equivalent, or stay on Vulkan."
                    ),
                    None => write!(
                        f,
                        "No usable NVIDIA driver detected — this CUDA variant requires ≥ {rmaj}.{rmin}.{rpat}. \
                         Install the proprietary NVIDIA driver, or stay on Vulkan."
                    ),
                }
            }
            Self::NotNvidia => write!(
                f,
                "CUDA variants are only supported on NVIDIA GPUs. Use the `vulkan` variant on AMD/Intel/CPU."
            ),
            Self::UnsupportedComputeCap(cc) => match cc {
                Some((maj, min)) => write!(
                    f,
                    "GPU compute capability sm_{maj}{min} is outside the supported set for our CUDA builds \
                     (sm_86..sm_120). Stay on Vulkan, or open an issue if you want sm_{maj}{min} support."
                ),
                None => write!(
                    f,
                    "Compute capability not detected — cannot decide if this CUDA variant supports your GPU. \
                     Stay on Vulkan, or re-run `lmforge init` after installing nvidia-smi."
                ),
            },
        }
    }
}

/// Check whether `variant` is installable on `profile`. Returns `Ok(())`
/// when every gate (vendor / compute_cap / driver floor) is satisfied;
/// returns the first violated gate otherwise.
pub fn refuse_reason(variant: LlamaVariant, profile: &HardwareProfile) -> Result<(), RefuseReason> {
    match variant {
        LlamaVariant::Vulkan | LlamaVariant::Cpu => Ok(()),
        LlamaVariant::Cuda12 | LlamaVariant::Cuda13 => {
            if profile.gpu_vendor != GpuVendor::Nvidia {
                return Err(RefuseReason::NotNvidia);
            }
            let cap_ok = matches!(
                profile.compute_cap,
                Some((cc_maj, _)) if matches!(cc_maj, 7 | 8 | 9 | 12)
            );
            if !cap_ok {
                return Err(RefuseReason::UnsupportedComputeCap(profile.compute_cap));
            }
            let actual_driver = profile.driver_tuple.or_else(|| {
                profile
                    .cuda_driver_version
                    .as_deref()
                    .and_then(crate::hardware::probe::parse_driver_tuple)
            });
            let floor = match variant {
                LlamaVariant::Cuda12 => CUDA12_DRIVER_MIN,
                LlamaVariant::Cuda13 => CUDA13_DRIVER_MIN,
                _ => unreachable!(),
            };
            if actual_driver.unwrap_or((0, 0, 0)) < floor {
                return Err(RefuseReason::DriverBelowFloor {
                    required: floor,
                    actual: actual_driver,
                });
            }
            Ok(())
        }
    }
}

// ── Embedded manifest ────────────────────────────────────────────────────────

/// JSON shape of `data/engines/llamacpp/variants-manifest.json`, embedded
/// into the binary so a fresh install can resolve URLs offline.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub llamacpp_tag: String,
    pub release_tag: String,
    pub variants: Vec<ManifestEntry>,
    #[serde(default, rename = "_comment")]
    pub _comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    pub cuda: String,
    pub driver_min: String,
    pub cap_min: f32,
    pub platform: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub opt_in_only: bool,
}

const BUNDLED_MANIFEST: &str =
    include_str!("../../data/engines/llamacpp/variants-manifest.json");

impl Manifest {
    /// Parse the manifest baked into the binary at build time. Cheap —
    /// safe to call per-invocation rather than caching.
    pub fn embedded() -> anyhow::Result<Self> {
        serde_json::from_str(BUNDLED_MANIFEST)
            .map_err(|e| anyhow::anyhow!("Invalid bundled variants-manifest.json: {e}"))
    }

    /// Find a variant entry by id, e.g. `lookup("cuda12")`.
    pub fn find(&self, id: &str) -> Option<&ManifestEntry> {
        self.variants.iter().find(|v| v.id == id)
    }

    /// Whether every entry's sha256 has been populated by CI (i.e. the
    /// manifest file is no longer a stub). `false` means
    /// `lmforge engine install llamacpp --variant cuda12` should refuse
    /// to download — the file at `url` may not exist yet.
    pub fn is_ready(&self) -> bool {
        self.variants
            .iter()
            .all(|v| !v.sha256.is_empty() && !v.sha256.contains("populated"))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::probe::{Arch, GpuVendor, Os};

    fn linux_nvidia_blackwell(driver: (u32, u32, u32)) -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            compute_cap: Some((12, 0)),
            cuda_driver_version: Some(format!("{}.{}.{}", driver.0, driver.1, driver.2)),
            driver_tuple: Some(driver),
            vram_gb: 16.0,
            total_ram_gb: 16.0,
            cpu_cores: 12,
            cpu_model: "test".into(),
            ..Default::default()
        }
    }

    fn linux_nvidia_pascal() -> HardwareProfile {
        // sm_61 — outside the cap_ok matrix, must reject CUDA path.
        let mut p = linux_nvidia_blackwell((595, 71, 5));
        p.compute_cap = Some((6, 1));
        p
    }

    fn linux_amd() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Amd,
            vram_gb: 24.0,
            total_ram_gb: 64.0,
            cpu_cores: 16,
            cpu_model: "AMD".into(),
            ..Default::default()
        }
    }

    fn linux_cpu_only() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::None,
            ..Default::default()
        }
    }

    fn macos_arm() -> HardwareProfile {
        HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            gpu_vendor: GpuVendor::Apple,
            ..Default::default()
        }
    }

    fn windows_nvidia() -> HardwareProfile {
        let mut p = linux_nvidia_blackwell((595, 71, 5));
        p.os = Os::Windows;
        p
    }

    // ── select() matrix ──────────────────────────────────────────────────

    #[test]
    fn select_linux_nvidia_blackwell_cuda12_installed_picks_cuda12() {
        let p = linux_nvidia_blackwell((595, 71, 5));
        let s = VariantState {
            cuda12_installed: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Cuda12);
    }

    #[test]
    fn select_linux_nvidia_prefers_cuda13_when_requested_and_installed() {
        let p = linux_nvidia_blackwell((595, 71, 5));
        let s = VariantState {
            cuda12_installed: true,
            cuda13_installed: true,
            prefer_cuda13: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Cuda13);
    }

    #[test]
    fn select_linux_nvidia_ignores_prefer_cuda13_when_below_floor() {
        // Driver r580 is below r590.44.01 floor for cuda13 → fall to cuda12.
        let p = linux_nvidia_blackwell((580, 95, 5));
        let s = VariantState {
            cuda12_installed: true,
            cuda13_installed: true,
            prefer_cuda13: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Cuda12);
    }

    #[test]
    fn select_linux_nvidia_below_cuda12_floor_falls_back_to_vulkan() {
        // r535 (Ubuntu 22.04 LTS default) → below r570.26 → Vulkan.
        let p = linux_nvidia_blackwell((535, 0, 0));
        let s = VariantState {
            cuda12_installed: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Vulkan);
    }

    #[test]
    fn select_linux_nvidia_pascal_skips_cuda_uses_vulkan() {
        let p = linux_nvidia_pascal();
        let s = VariantState {
            cuda12_installed: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Vulkan);
    }

    #[test]
    fn select_linux_nvidia_no_cuda_installed_falls_back_to_vulkan() {
        let p = linux_nvidia_blackwell((595, 71, 5));
        let s = VariantState::default();
        assert_eq!(select(&p, &s), LlamaVariant::Vulkan);
    }

    #[test]
    fn select_linux_nvidia_only_cuda13_installed_picks_cuda13_without_prefer_flag() {
        // Regression: when the user explicitly installed cuda13 (and
        // didn't install cuda12), the selector must pick cuda13 even when
        // prefer_cuda13 is false. Otherwise installing the variant would
        // appear to do nothing — the runtime would silently downgrade to
        // Vulkan. Discovered live during S-2 verification.
        let p = linux_nvidia_blackwell((595, 71, 5));
        let s = VariantState {
            cuda12_installed: false,
            cuda13_installed: true,
            prefer_cuda13: false,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Cuda13);
    }

    #[test]
    fn select_linux_nvidia_only_cuda13_installed_below_floor_falls_to_vulkan() {
        // Same as above but the driver is below the cuda13 floor — we
        // can't run cuda13 safely. With cuda12 also missing, only Vulkan
        // is available. (This is the "user installed cuda13 on an old
        // driver" footgun; better to fail-safe to Vulkan than to crash.)
        let p = linux_nvidia_blackwell((580, 0, 0));
        let s = VariantState {
            cuda13_installed: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Vulkan);
    }

    #[test]
    fn select_linux_amd_always_vulkan() {
        assert_eq!(select(&linux_amd(), &VariantState::default()), LlamaVariant::Vulkan);
    }

    #[test]
    fn select_linux_cpu_only_picks_cpu() {
        assert_eq!(select(&linux_cpu_only(), &VariantState::default()), LlamaVariant::Cpu);
    }

    #[test]
    fn select_macos_skips_cuda_path_entirely() {
        // macOS NEVER hits the variant path (oMLX is the engine). The
        // selector still returns *something* deterministic on Apple
        // Silicon — Apple is not in the `has_gpu` set used by
        // `fallback_variant`, so we end up at `Cpu`. This is fine; the
        // result is never read on Darwin.
        assert_eq!(select(&macos_arm(), &VariantState::default()), LlamaVariant::Cpu);
    }

    #[test]
    fn select_windows_nvidia_left_to_upstream_path() {
        // Windows NVIDIA falls through to fallback_variant (which says
        // Vulkan since Nvidia is in the "has_gpu" set). The actual Windows
        // CUDA path is owned by `installer::resolve_platform`, not this
        // selector — see the module doc.
        let p = windows_nvidia();
        let s = VariantState {
            cuda12_installed: true,
            ..Default::default()
        };
        assert_eq!(select(&p, &s), LlamaVariant::Vulkan);
    }

    // ── refuse_reason ────────────────────────────────────────────────────

    #[test]
    fn refuse_reason_vulkan_and_cpu_never_refuse() {
        let p = linux_nvidia_pascal();
        assert!(refuse_reason(LlamaVariant::Vulkan, &p).is_ok());
        assert!(refuse_reason(LlamaVariant::Cpu, &p).is_ok());
    }

    #[test]
    fn refuse_reason_cuda13_below_floor_refuses_with_upgrade_hint() {
        let p = linux_nvidia_blackwell((580, 95, 5));
        let err = refuse_reason(LlamaVariant::Cuda13, &p).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("590"), "must mention required floor");
        assert!(msg.contains("580"), "must mention actual driver");
    }

    #[test]
    fn refuse_reason_cuda_on_amd_refuses_with_not_nvidia() {
        let err = refuse_reason(LlamaVariant::Cuda12, &linux_amd()).unwrap_err();
        assert!(matches!(err, RefuseReason::NotNvidia));
    }

    #[test]
    fn refuse_reason_cuda_on_pascal_refuses_with_unsupported_cap() {
        let err = refuse_reason(LlamaVariant::Cuda12, &linux_nvidia_pascal()).unwrap_err();
        assert!(matches!(err, RefuseReason::UnsupportedComputeCap(_)));
    }

    #[test]
    fn refuse_reason_cuda12_ok_when_driver_at_floor() {
        let p = linux_nvidia_blackwell(CUDA12_DRIVER_MIN);
        assert!(refuse_reason(LlamaVariant::Cuda12, &p).is_ok());
    }

    #[test]
    fn refuse_reason_cuda13_ok_when_driver_at_floor() {
        let p = linux_nvidia_blackwell(CUDA13_DRIVER_MIN);
        assert!(refuse_reason(LlamaVariant::Cuda13, &p).is_ok());
    }

    // ── LlamaVariant str round-trip ──────────────────────────────────────

    #[test]
    fn llama_variant_str_round_trip() {
        use std::str::FromStr;
        for v in [
            LlamaVariant::Cuda12,
            LlamaVariant::Cuda13,
            LlamaVariant::Vulkan,
            LlamaVariant::Cpu,
        ] {
            assert_eq!(LlamaVariant::from_str(v.as_str()).unwrap(), v);
        }
    }

    #[test]
    fn llama_variant_from_str_rejects_garbage() {
        use std::str::FromStr;
        assert!(LlamaVariant::from_str("rocm").is_err());
        assert!(LlamaVariant::from_str("").is_err());
    }

    // ── Manifest ─────────────────────────────────────────────────────────

    #[test]
    fn embedded_manifest_parses() {
        let m = Manifest::embedded().expect("bundled manifest must parse");
        assert!(!m.llamacpp_tag.is_empty());
        assert!(m.find("cuda12").is_some(), "cuda12 entry must exist");
        assert!(m.find("cuda13").is_some(), "cuda13 entry must exist");
    }

    #[test]
    fn embedded_manifest_has_real_shas_post_ci() {
        // Manifest was populated with real sha256s after the first
        // successful build-llamacpp-cuda.yml run on b9351. This test
        // is now a regression guard — if someone reverts the manifest
        // to placeholders or empties a sha, this fails loud.
        let m = Manifest::embedded().unwrap();
        assert!(
            m.is_ready(),
            "embedded manifest must have real sha256 values for every variant; \
             did someone revert variants-manifest.json to <populated-by-ci>?"
        );
        // Spot-check format: 64 lowercase hex chars per sha256.
        for v in &m.variants {
            assert_eq!(v.sha256.len(), 64, "{} sha256 must be 64 hex chars", v.id);
            assert!(
                v.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "{} sha256 must be lowercase hex",
                v.id
            );
        }
    }

    #[test]
    fn manifest_is_ready_true_when_all_shas_populated() {
        let json = r#"{
            "llamacpp_tag": "b9351",
            "release_tag": "llamacpp-b9351",
            "variants": [
                {"id":"cuda12","cuda":"12.8.1","driver_min":"570.26","cap_min":7.5,
                 "platform":"linux-x64","url":"https://example/x.tar.gz",
                 "sha256":"a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00"},
                {"id":"cuda13","cuda":"13.1.0","driver_min":"590.44.01","cap_min":7.5,
                 "platform":"linux-x64","url":"https://example/y.tar.gz",
                 "sha256":"deadbeefcafebabefeedfacefacefacefacefacefacefacefacefacefacefa"}
            ]
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(m.is_ready(), "shas populated → ready");
        assert_eq!(m.find("cuda12").unwrap().platform, "linux-x64");
        assert!(m.find("cuda13").unwrap().sha256.starts_with("deadbeef"));
    }

    #[test]
    fn manifest_is_ready_false_when_any_sha_is_placeholder() {
        let json = r#"{
            "llamacpp_tag": "b9351",
            "release_tag": "llamacpp-b9351",
            "variants": [
                {"id":"cuda12","cuda":"12.8.1","driver_min":"570.26","cap_min":7.5,
                 "platform":"linux-x64","url":"https://example/x.tar.gz",
                 "sha256":"a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00"},
                {"id":"cuda13","cuda":"13.1.0","driver_min":"590.44.01","cap_min":7.5,
                 "platform":"linux-x64","url":"https://example/y.tar.gz",
                 "sha256":"<populated-by-ci>"}
            ]
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(!m.is_ready(), "any placeholder → not ready");
    }

    #[test]
    fn manifest_is_ready_false_when_any_sha_empty() {
        let json = r#"{
            "llamacpp_tag": "b9351",
            "release_tag": "llamacpp-b9351",
            "variants": [
                {"id":"cuda12","cuda":"12.8.1","driver_min":"570.26","cap_min":7.5,
                 "platform":"linux-x64","url":"https://example/x.tar.gz","sha256":""}
            ]
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(!m.is_ready(), "empty sha → not ready");
    }

    #[test]
    fn manifest_find_returns_none_for_unknown_variant() {
        let m = Manifest::embedded().unwrap();
        assert!(m.find("rocm").is_none());
        assert!(m.find("").is_none());
    }
}
