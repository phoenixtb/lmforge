pub mod probe;
pub mod vram;

pub use probe::{Arch, GpuVendor, HardwareProfile, Os};

use anyhow::Result;

/// Probe the current hardware and return a HardwareProfile.
/// This is the main entry point for hardware detection.
pub fn detect() -> Result<HardwareProfile> {
    let mut profile = probe::detect_platform()?;
    profile.vram_gb = vram::estimate_vram(&profile);
    Ok(profile)
}
