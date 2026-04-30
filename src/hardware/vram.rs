use tracing::debug;

use super::probe::{GpuVendor, HardwareProfile};

/// Estimate usable VRAM based on hardware profile and platform heuristics.
///
/// Per SRS §3.2:
/// - Apple Silicon: `total_ram * gpu_fraction` (default 0.75) — unified memory
/// - NVIDIA: query via nvidia-smi, subtract 512 MB system overhead
/// - Otherwise: 0.0 (CPU-only)
pub fn estimate_vram(profile: &HardwareProfile) -> f32 {
    let vram = match profile.gpu_vendor {
        GpuVendor::Apple => estimate_apple_vram(profile),
        GpuVendor::Nvidia => estimate_nvidia_vram(),
        GpuVendor::Amd => estimate_amd_vram(),
        GpuVendor::None => 0.0,
    };

    debug!(gpu_vendor = ?profile.gpu_vendor, vram_gb = vram, "VRAM estimated");
    vram
}

/// Determine the quantization tier based on available VRAM and RAM.
///
/// Returns `None` when the system is below the minimum viable threshold for LLM inference.
/// Callers should surface a clear warning and let the user decide rather than attempting to
/// run a Q3 or lower model, which are difficult to find and produce poor output quality.
///
/// Minimum viable thresholds (Q4_K_S floor):
/// - GPU systems:    ≥ 3.0 GB VRAM  (e.g. RTX 3050 4 GB → 3.5 GB usable)
/// - CPU-only:       ≥ 8.0 GB RAM   (→ 4.0 GB effective at 50% budget)
///
/// For CPU-only systems (`vram_gb == 0`): 50% of RAM is used as the effective budget,
/// leaving headroom for OS, KV cache, and background processes.
pub fn quant_tier(vram_gb: f32, total_ram_gb: f32) -> Option<&'static str> {
    let effective_gb = if vram_gb > 0.0 {
        vram_gb
    } else {
        total_ram_gb * 0.5
    };

    if effective_gb >= 48.0 {
        Some("fp16") // Full precision — A6000, M2 Ultra 192 GB, or 96 GB+ RAM
    } else if effective_gb >= 24.0 {
        Some("Q8_0") // High quality — A10G / RTX 3090, or 48 GB+ RAM
    } else if effective_gb >= 8.0 {
        Some("Q4_K_M") // Recommended — RTX 3070/3080/4070, or 16 GB+ RAM
    } else if effective_gb >= 3.0 {
        Some("Q4_K_S") // Minimum viable — RTX 3050 4 GB, or 8 GB RAM
    } else {
        // Below minimum: < 3 GB VRAM or < 6 GB RAM.
        // Q3 models are hard to find and produce poor results. Return None
        // so the caller can warn the user and let them decide.
        None
    }
}

/// Get currently free VRAM in GB at the current moment
pub fn get_free_vram(profile: &HardwareProfile) -> f32 {
    let free_vram = match profile.gpu_vendor {
        GpuVendor::Apple => get_free_apple_vram(),
        GpuVendor::Nvidia => get_free_nvidia_vram(),
        GpuVendor::Amd => get_free_amd_vram(),
        GpuVendor::None => 0.0,
    };
    debug!(gpu_vendor = ?profile.gpu_vendor, free_vram_gb = free_vram, "Free VRAM probed");
    free_vram
}

/// Estimate the VRAM required for a model (in GB)
pub fn estimate_model_vram(size_bytes: u64) -> f32 {
    let size_gb = size_bytes as f32 / (1024.0 * 1024.0 * 1024.0);
    size_gb * 1.2 // 1.2x heuristic for KV cache and context overhead
}

/// Apple Silicon: unified memory architecture.
/// Use 75% of total RAM as usable for GPU inference.
fn estimate_apple_vram(profile: &HardwareProfile) -> f32 {
    // Default fraction: 0.75 (can be overridden via config at runtime)
    const DEFAULT_GPU_FRACTION: f32 = 0.75;
    profile.total_ram_gb * DEFAULT_GPU_FRACTION
}

/// Apple Silicon Free memory
fn get_free_apple_vram() -> f32 {
    use sysinfo::System;
    let mut sys = System::new_all();
    sys.refresh_memory();

    // On Mac, we just report fully available memory since unified handles it.
    sys.available_memory() as f32 / (1024.0 * 1024.0 * 1024.0)
}

/// NVIDIA: parse nvidia-smi for total GPU memory.
/// Subtract 512 MB for system/driver overhead.
fn estimate_nvidia_vram() -> f32 {
    // Try nvidia-smi
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        && output.status.success()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        // nvidia-smi reports in MiB, may have multiple GPUs (one per line)
        // Take the first GPU's value
        if let Some(first_line) = stdout.lines().next()
            && let Ok(total_mib) = first_line.trim().parse::<f32>()
        {
            let total_gb = total_mib / 1024.0;
            let usable = (total_gb - 0.5).max(0.0); // subtract 512 MB overhead
            return usable;
        }
    }

    // Diagnose why nvidia-smi failed
    let smi_exists = std::process::Command::new("which")
        .arg("nvidia-smi")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !smi_exists {
        tracing::warn!(
            "nvidia-smi not found in PATH. Install the NVIDIA driver utilities \
             (e.g. `sudo apt install nvidia-utils-535`) and re-run `lmforge init`. \
             Defaulting VRAM to 0.0 GB — GPU inference will not be configured."
        );
    } else {
        tracing::warn!(
            "nvidia-smi found but query failed — driver may not be loaded. \
             Try `nvidia-smi` in a terminal to diagnose. Defaulting VRAM to 0.0 GB."
        );
    }
    0.0
}

/// NVIDIA free memory
fn get_free_nvidia_vram() -> f32 {
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        && output.status.success()
        && let Ok(stdout) = String::from_utf8(output.stdout)
        && let Some(first_line) = stdout.lines().next()
        && let Ok(free_mib) = first_line.trim().parse::<f32>()
    {
        let free_gb = free_mib / 1024.0;
        return (free_gb - 0.5).max(0.0); // 512MB safety pad
    }
    0.0
}

/// AMD ROCm: parse rocm-smi for GPU memory.
fn estimate_amd_vram() -> f32 {
    // Try rocm-smi
    if let Ok(output) = std::process::Command::new("rocm-smi")
        .args(["--showmeminfo", "vram"])
        .output()
        && output.status.success()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        // Parse "Total Memory (B): <bytes>" line
        for line in stdout.lines() {
            if line.contains("Total Memory")
                && let Some(bytes_str) = line.split(':').nth(1)
                && let Ok(bytes) = bytes_str.trim().parse::<f64>()
            {
                return (bytes / (1024.0 * 1024.0 * 1024.0)) as f32;
            }
        }
    }

    debug!("rocm-smi query failed, defaulting AMD VRAM to 0");
    0.0
}

/// AMD ROCm free memory
fn get_free_amd_vram() -> f32 {
    if let Ok(output) = std::process::Command::new("rocm-smi")
        .args(["--showmeminfo", "vram"])
        .output()
        && output.status.success()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        let mut total = 0.0;
        let mut used = 0.0;
        for line in stdout.lines() {
            if line.contains("Total Memory") {
                if let Some(bytes_str) = line.split(':').nth(1)
                    && let Ok(bytes) = bytes_str.trim().parse::<f64>()
                {
                    total = bytes / (1024.0 * 1024.0 * 1024.0);
                }
            } else if line.contains("Total Used Memory")
                && let Some(bytes_str) = line.split(':').nth(1)
                && let Ok(bytes) = bytes_str.trim().parse::<f64>()
            {
                used = bytes / (1024.0 * 1024.0 * 1024.0);
            }
        }
        if total > 0.0 {
            return (total - used - 0.5).max(0.0) as f32; // 512MB safety pad
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::probe::{Arch, Os};

    fn make_profile(gpu: GpuVendor, ram_gb: f32) -> HardwareProfile {
        HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            is_tegra: false,
            gpu_vendor: gpu,
            vram_gb: 0.0,
            unified_mem: gpu == GpuVendor::Apple,
            total_ram_gb: ram_gb,
            cpu_cores: 10,
            cpu_model: "Test".to_string(),
        }
    }

    #[test]
    fn test_apple_vram_48gb() {
        let profile = make_profile(GpuVendor::Apple, 48.0);
        let vram = estimate_apple_vram(&profile);
        assert!((vram - 36.0).abs() < 0.01, "48 * 0.75 = 36.0, got {}", vram);
    }

    #[test]
    fn test_apple_vram_16gb() {
        let profile = make_profile(GpuVendor::Apple, 16.0);
        let vram = estimate_apple_vram(&profile);
        assert!((vram - 12.0).abs() < 0.01, "16 * 0.75 = 12.0, got {}", vram);
    }

    #[test]
    fn test_no_gpu_vram_zero() {
        let profile = make_profile(GpuVendor::None, 32.0);
        let vram = estimate_vram(&profile);
        assert_eq!(vram, 0.0);
    }

    // --- quant_tier tests (GPU path: vram > 0) ---

    #[test]
    fn test_quant_tier_48gb() {
        assert_eq!(quant_tier(48.0, 64.0), Some("fp16"));
    }

    #[test]
    fn test_quant_tier_24gb() {
        assert_eq!(quant_tier(24.0, 64.0), Some("Q8_0"));
    }

    #[test]
    fn test_quant_tier_12gb() {
        assert_eq!(quant_tier(12.0, 32.0), Some("Q4_K_M"));
    }

    #[test]
    fn test_quant_tier_8gb() {
        assert_eq!(quant_tier(8.0, 32.0), Some("Q4_K_M")); // exactly at 8 GB threshold
    }

    #[test]
    fn test_quant_tier_4gb_vram() {
        // RTX 3050 4GB → ~3.5 GB usable → Q4_K_S (4-bit minimum)
        assert_eq!(quant_tier(3.5, 24.0), Some("Q4_K_S"));
        assert_eq!(quant_tier(4.0, 24.0), Some("Q4_K_S"));
    }

    #[test]
    fn test_quant_tier_below_minimum_vram() {
        // < 3 GB VRAM → below minimum → None (no Q3 recommendation)
        assert_eq!(quant_tier(2.0, 8.0), None);
        assert_eq!(quant_tier(1.0, 4.0), None);
    }

    // --- quant_tier tests (CPU-only path: vram == 0, RAM-based) ---

    #[test]
    fn test_quant_tier_cpu_only_24gb_ram() {
        // 24 GB RAM → 12 GB effective → Q4_K_M
        assert_eq!(quant_tier(0.0, 24.0), Some("Q4_K_M"));
    }

    #[test]
    fn test_quant_tier_cpu_only_16gb_ram() {
        // 16 GB RAM → 8 GB effective → Q4_K_M
        assert_eq!(quant_tier(0.0, 16.0), Some("Q4_K_M"));
    }

    #[test]
    fn test_quant_tier_cpu_only_8gb_ram() {
        // 8 GB RAM → 4 GB effective → Q4_K_S (minimum viable)
        assert_eq!(quant_tier(0.0, 8.0), Some("Q4_K_S"));
    }

    #[test]
    fn test_quant_tier_cpu_only_below_minimum() {
        // < 8 GB RAM → below minimum → None (warn user, don't recommend Q3)
        // 5 GB RAM → 2.5 GB effective → below 3.0 GB threshold → None
        assert_eq!(quant_tier(0.0, 5.0), None);
        assert_eq!(quant_tier(0.0, 4.0), None);
    }
}
