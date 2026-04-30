use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Detected operating system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Darwin,
    Linux,
    Windows,
    Unknown,
}

/// CPU architecture
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Aarch64,
    X86_64,
    Unknown,
}

/// GPU vendor classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Apple,
    Nvidia,
    Amd,
    None,
}

/// Complete hardware profile used for engine selection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub os: Os,
    pub arch: Arch,
    pub is_tegra: bool,
    pub gpu_vendor: GpuVendor,
    pub vram_gb: f32,
    pub unified_mem: bool,
    pub total_ram_gb: f32,
    pub cpu_cores: usize,
    pub cpu_model: String,
}

impl std::fmt::Display for HardwareProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} {:?} | GPU: {:?} | RAM: {:.1} GB{}| VRAM: {:.1} GB | CPU: {} ({} cores)",
            self.os,
            self.arch,
            self.gpu_vendor,
            self.total_ram_gb,
            if self.unified_mem { " (unified) " } else { " " },
            self.vram_gb,
            self.cpu_model,
            self.cpu_cores,
        )
    }
}

/// Detect OS, arch, GPU vendor, RAM, and CPU info
pub fn detect_platform() -> Result<HardwareProfile> {
    let os = detect_os();
    let arch = detect_arch();
    let gpu_vendor = detect_gpu_vendor(os, arch);
    let unified_mem = os == Os::Darwin && arch == Arch::Aarch64;
    let is_tegra = detect_tegra();
    let (total_ram_gb, cpu_cores, cpu_model) = detect_system_info();

    debug!(
        ?os,
        ?arch,
        ?gpu_vendor,
        unified_mem,
        total_ram_gb,
        "Hardware detected"
    );

    Ok(HardwareProfile {
        os,
        arch,
        is_tegra,
        gpu_vendor,
        vram_gb: 0.0, // filled in by vram::estimate_vram()
        unified_mem,
        total_ram_gb,
        cpu_cores,
        cpu_model,
    })
}

fn detect_os() -> Os {
    match std::env::consts::OS {
        "macos" => Os::Darwin,
        "linux" => Os::Linux,
        "windows" => Os::Windows,
        _ => Os::Unknown,
    }
}

fn detect_arch() -> Arch {
    match std::env::consts::ARCH {
        "aarch64" => Arch::Aarch64,
        "x86_64" => Arch::X86_64,
        _ => Arch::Unknown,
    }
}

fn detect_gpu_vendor(os: Os, arch: Arch) -> GpuVendor {
    // Apple Silicon: macOS + aarch64
    if os == Os::Darwin && arch == Arch::Aarch64 {
        return GpuVendor::Apple;
    }

    // NVIDIA: check for nvidia-smi or /dev/nvidia*
    if check_nvidia_present() {
        return GpuVendor::Nvidia;
    }

    // AMD: check for /dev/dri/renderD128 (ROCm) or rocm-smi
    if check_amd_present() {
        return GpuVendor::Amd;
    }

    GpuVendor::None
}

fn check_nvidia_present() -> bool {
    // Try nvidia-smi first
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output()
        && output.status.success()
    {
        return true;
    }

    // Check for /dev/nvidia0
    std::path::Path::new("/dev/nvidia0").exists()
}

fn check_amd_present() -> bool {
    // Check for ROCm SMI
    if let Ok(output) = std::process::Command::new("rocm-smi").output()
        && output.status.success()
    {
        return true;
    }

    // Check for DRI render device (common on AMD Linux)
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/dev/kfd").exists() {
            return true;
        }
    }

    false
}

fn detect_tegra() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/device-tree/compatible") {
            return content.contains("nvidia,tegra");
        }
    }
    false
}

fn detect_system_info() -> (f32, usize, String) {
    use sysinfo::System;

    let mut sys = System::new_all();
    sys.refresh_all();

    let total_ram_gb = sys.total_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
    let cpu_cores = sys.cpus().len();

    let cpu_model = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    (total_ram_gb, cpu_cores, cpu_model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_os_is_known() {
        let os = detect_os();
        assert_ne!(os, Os::Unknown, "OS should be detected");
    }

    #[test]
    fn test_detect_arch_is_known() {
        let arch = detect_arch();
        assert_ne!(arch, Arch::Unknown, "Arch should be detected");
    }

    #[test]
    fn test_system_info_reasonable() {
        let (ram, cores, model) = detect_system_info();
        assert!(ram > 0.5, "Should have more than 0.5 GB RAM");
        assert!(cores > 0, "Should have at least 1 CPU core");
        assert!(!model.is_empty(), "CPU model should not be empty");
    }

    #[test]
    fn test_hardware_profile_display() {
        let profile = HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            is_tegra: false,
            gpu_vendor: GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".to_string(),
        };
        let s = format!("{}", profile);
        assert!(s.contains("Apple"));
        assert!(s.contains("48.0 GB"));
        assert!(s.contains("unified"));
    }

    #[test]
    fn test_gpu_vendor_apple_silicon() {
        let vendor = detect_gpu_vendor(Os::Darwin, Arch::Aarch64);
        assert_eq!(vendor, GpuVendor::Apple);
    }

    #[test]
    fn test_gpu_vendor_macos_x86_not_apple() {
        // Intel Mac should not report Apple GPU
        let vendor = detect_gpu_vendor(Os::Darwin, Arch::X86_64);
        // On a real Intel Mac this would be None; on Apple Silicon it doesn't matter
        assert_ne!(vendor, GpuVendor::Apple);
    }
}
