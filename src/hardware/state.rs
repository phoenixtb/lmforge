//! Process-wide cached hardware **identity** — the single owner engines
//! should consult instead of re-probing.
//!
//! Before this module existed, the engine layer called
//! `hardware::probe::detect_platform()` (which shells out to `nvidia-smi`,
//! `rocm-smi`, reads `/sys/class/drm`, etc.) from four different call sites
//! on the hot path: `process_pool::ensure_model_inner`,
//! `process_pool::evict_for_memory`, `llamacpp::plan_load`, and
//! `llamacpp::start`. Every model load or eviction pass re-ran the full
//! platform probe, which is wasteful and — worse — can observe a
//! *different* answer mid-request if the environment flickers (e.g. a
//! `nvidia-smi` hiccup), producing inconsistent planning within a single
//! load.
//!
//! **Identity vs. availability**: this module answers "what GPU/CPU/RAM does
//! this box have" (stable for the process lifetime), never "how much VRAM is
//! free right now" (volatile — see [`crate::hardware::vram::get_free_vram`]).
//! `vram_gb` on the cached profile is the box's *total* VRAM estimate, not a
//! live free-memory reading; callers that need live free VRAM must still go
//! through `vram::get_free_vram`.
//!
//! `lmforge init` intentionally does NOT use this module — it must always
//! re-probe fresh (the whole point of `init` is to (re-)detect hardware
//! after a driver update, GPU swap, etc.) and write a new `hardware.json`.
//! This module only serves the engine layer's read path.

use std::path::Path;
use std::sync::OnceLock;

use tracing::warn;

use super::probe::HardwareProfile;

static IDENTITY: OnceLock<HardwareProfile> = OnceLock::new();

/// Pure loader — no process-wide caching. Reads `<data_dir>/hardware.json`
/// (the same file `lmforge init` / `cli::start` writes) and parses it as a
/// [`HardwareProfile`]. Falls back to a fresh live probe when the file is
/// missing or fails to parse, best-effort persisting the result so the next
/// process start hits the cache.
///
/// Exposed `pub` (rather than only via the [`identity`] wrapper) so unit
/// tests can exercise the fallback/parse logic against a fixture directory
/// without being contaminated by the process-wide [`OnceLock`] — every test
/// in the binary shares one `IDENTITY` cell, so only `identity()` itself is
/// safe to call at most once per test process.
pub fn load_identity(data_dir: &Path) -> HardwareProfile {
    let path = data_dir.join("hardware.json");
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<HardwareProfile>(&content) {
            Ok(profile) => return profile,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "hardware.json present but failed to parse — falling back to a live probe \
                     (hardware identity cache miss)"
                );
            }
        },
        Err(_) => {
            warn!(
                path = %path.display(),
                "hardware.json missing — falling back to a live probe \
                 (hardware identity cache miss; run `lmforge init` to persist it)"
            );
        }
    }

    // Fallback: live probe. Note this intentionally reuses the same
    // detect+estimate pair as `hardware::detect()` rather than calling it
    // directly, so a probe failure degrades to `HardwareProfile::default()`
    // (CPU planning) instead of propagating an error up through the engine
    // spawn path.
    let mut profile = super::probe::detect_platform().unwrap_or_default();
    profile.vram_gb = super::vram::estimate_vram(&profile);

    // Best-effort persist. A write failure here (read-only data dir, race
    // with another process) must not block the caller — it just means the
    // next process start pays the probe cost again.
    if let Ok(json) = serde_json::to_string_pretty(&profile) {
        let _ = std::fs::write(&path, json);
    }

    profile
}

/// Cached hardware identity for the lifetime of this process.
///
/// First call reads (or falls back to probing) `<data_dir>/hardware.json`
/// and caches the result in a process-wide [`OnceLock`]; every subsequent
/// call — regardless of `data_dir` — returns the same cached value. This is
/// intentional: a single LMForge daemon process only ever runs against one
/// `data_dir`, so per-argument caching would be over-engineering, and the
/// `OnceLock` gives us the "probe once" property engines actually want.
///
/// NOTE ON `vram_gb == 0` WITH A NON-`None` `gpu_vendor`: if the persisted
/// profile recorded a GPU vendor but `vram_gb == 0` (e.g. the box was probed
/// while the driver was down), this function keeps it as-is rather than
/// "fixing" it — this is *identity* (what hardware exists), not
/// *availability* (how much is usable right now). Callers that need a
/// trustworthy live VRAM figure must go through
/// [`crate::hardware::vram::get_free_vram`], which now surfaces probe
/// failure as `None` instead of silently coercing to `0.0`.
pub fn identity(data_dir: &Path) -> HardwareProfile {
    IDENTITY.get_or_init(|| load_identity(data_dir)).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::probe::{Arch, GpuVendor, Os};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_identity_reads_fixture_hardware_json() {
        let dir = temp_dir("lmforge_test_hw_state_fixture");
        let fixture = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 15.4,
            total_ram_gb: 15.6,
            cpu_cores: 6,
            cpu_model: "Test CPU".to_string(),
            ..Default::default()
        };
        std::fs::write(
            dir.join("hardware.json"),
            serde_json::to_string_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let loaded = load_identity(&dir);
        assert_eq!(loaded.gpu_vendor, GpuVendor::Nvidia);
        assert_eq!(loaded.vram_gb, 15.4);
        assert_eq!(loaded.total_ram_gb, 15.6);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_identity_falls_back_to_probe_when_missing() {
        let dir = temp_dir("lmforge_test_hw_state_missing");
        // No hardware.json written — must fall back to a live probe rather
        // than panicking or returning a garbage default.
        let loaded = load_identity(&dir);
        // The live probe always fills in a real OS/arch on any dev/CI box.
        assert_ne!(loaded.os, Os::default());
        // Best-effort persistence: the fallback should have written the file
        // so a second call (in a fresh process) would hit the cache path.
        assert!(dir.join("hardware.json").is_file());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_identity_falls_back_to_probe_on_parse_failure() {
        let dir = temp_dir("lmforge_test_hw_state_corrupt");
        std::fs::write(dir.join("hardware.json"), "{ not valid json").unwrap();

        let loaded = load_identity(&dir);
        assert_ne!(loaded.os, Os::default());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_identity_keeps_zero_vram_with_gpu_vendor_as_is() {
        // Identity, not availability: a cached profile that says "NVIDIA but
        // 0 VRAM" (e.g. probed while the driver was down) must round-trip
        // unchanged — this function must not "fix" it.
        let dir = temp_dir("lmforge_test_hw_state_zero_vram_gpu");
        let fixture = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 0.0,
            total_ram_gb: 15.6,
            cpu_cores: 6,
            cpu_model: "Test CPU".to_string(),
            ..Default::default()
        };
        std::fs::write(
            dir.join("hardware.json"),
            serde_json::to_string_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let loaded = load_identity(&dir);
        assert_eq!(loaded.gpu_vendor, GpuVendor::Nvidia);
        assert_eq!(loaded.vram_gb, 0.0, "identity must not paper over 0 VRAM");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn identity_is_cached_across_calls() {
        // We can't easily reset the process-wide OnceLock between tests, but
        // we CAN assert the basic contract: two calls (even with different
        // data_dirs) return byte-identical HardwareProfile field values,
        // proving the second call didn't re-probe.
        let dir_a = temp_dir("lmforge_test_hw_state_cache_a");
        let dir_b = temp_dir("lmforge_test_hw_state_cache_b");

        let first = identity(&dir_a);
        let second = identity(&dir_b);
        assert_eq!(first.os, second.os);
        assert_eq!(first.gpu_vendor, second.gpu_vendor);
        assert_eq!(first.total_ram_gb, second.total_ram_gb);

        std::fs::remove_dir_all(&dir_a).unwrap();
        std::fs::remove_dir_all(&dir_b).unwrap();
    }
}
