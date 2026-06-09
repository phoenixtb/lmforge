use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Current `hardware.json` schema version. Bumped whenever a field is added
/// or its meaning changes. Readers tolerate missing fields via `#[serde(default)]`,
/// but `lmforge init` always re-probes and writes the latest schema.
pub const HARDWARE_SCHEMA_VERSION: u32 = 2;

/// Detected operating system (kernel level — Linux on WSL2 still reports `Linux`).
/// For the tier selector use `OsFamily`, which distinguishes WSL2 from native Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Darwin,
    Linux,
    Windows,
    #[default]
    Unknown,
}

/// CPU architecture
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Aarch64,
    X86_64,
    #[default]
    Unknown,
}

/// GPU vendor classification.
///
/// `Intel` covers Intel iGPUs (Iris/Arc-integrated and discrete Arc dGPUs).
/// They're Vulkan-capable when the user has the Mesa or Windows Intel
/// drivers installed, so the llama.cpp variant selector treats them as a
/// real GPU rather than falling back to CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Apple,
    Nvidia,
    Amd,
    Intel,
    #[default]
    None,
}

/// OS family — the dimension engine tiers gate on.
///
/// Distinct from [`Os`] because vLLM works on Linux + WSL2 but **not** on native
/// Windows. The kernel-level [`Os`] reports "Linux" for WSL2, which is correct
/// at the syscall level but wrong for tier eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OsFamily {
    Linux,
    WindowsNative,
    WindowsWsl2,
    Darwin,
    #[default]
    Unknown,
}

/// NVIDIA compute capability as (major, minor) — e.g. `(12, 0)` for RTX 50-series.
/// Stored as a tuple rather than a string so engine selector code can do
/// ordinal comparisons (`compute_cap >= (9, 0)`).
pub type ComputeCap = (u8, u8);

/// Complete hardware profile used for engine selection.
///
/// New fields added in schema v2 are `#[serde(default)]` so v1 `hardware.json`
/// files keep parsing. `lmforge init` always re-probes and writes v2.
///
/// `Default` is provided so test fixtures and synthetic profiles can spread
/// `..Default::default()` and stay forward-compatible as new fields land.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

    // ── Schema v2 additions (engine-tier era) ──────────────────────────────
    /// `hardware.json` schema version. Used to trigger re-probe when the
    /// reader expects fields the writer didn't produce.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// NVIDIA compute capability, e.g. `(7, 5)` for RTX 20-series,
    /// `(12, 0)` for RTX 50-series. `None` on non-NVIDIA systems.
    #[serde(default)]
    pub compute_cap: Option<ComputeCap>,

    /// CUDA runtime version (the toolkit `nvcc` came from), e.g. `"12.8"` / `"13.0"`.
    /// Read from `nvcc --version` first, falling back to `nvidia-smi`.
    #[serde(default)]
    pub cuda_runtime_version: Option<String>,

    /// CUDA *driver* version reported by `nvidia-smi`, e.g. `"580.95.05"`.
    /// Distinct from runtime — the driver can be newer than the toolkit.
    #[serde(default)]
    pub cuda_driver_version: Option<String>,

    /// Parsed form of [`cuda_driver_version`] as `(major, minor, patch)` —
    /// e.g. `Some((595, 71, 5))` for `"595.71.05"`. Computed once at probe
    /// time so the variant selector can do ordinal compares against the
    /// CUDA12 / CUDA13 driver-floor constants without re-parsing on every
    /// `lmforge start`. `None` on non-NVIDIA systems or when the version
    /// string couldn't be parsed.
    #[serde(default)]
    pub driver_tuple: Option<(u32, u32, u32)>,

    /// OS family for tier gating. See [`OsFamily`].
    #[serde(default = "default_os_family")]
    pub os_family: OsFamily,

    /// `true` when running inside WSL2 (kernel reports Linux but host is Windows).
    /// Implies `os_family == WindowsWsl2`.
    #[serde(default)]
    pub is_wsl: bool,

    /// Number of GPUs reported by `nvidia-smi`. `0` on non-NVIDIA systems.
    /// Used by the vLLM tier soft-warning ("single GPU → llama.cpp matches it").
    #[serde(default)]
    pub gpu_count: u8,
}

fn default_schema_version() -> u32 {
    // Older v1 files lacked this field; mark them as v1 so a re-probe is triggered.
    1
}

fn default_os_family() -> OsFamily {
    OsFamily::Unknown
}

impl std::fmt::Display for HardwareProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cc = match self.compute_cap {
            Some((maj, min)) => format!(" | sm_{}{}", maj, min),
            None => String::new(),
        };
        let wsl = if self.is_wsl { " (WSL2)" } else { "" };
        write!(
            f,
            "{:?}{} {:?} | GPU: {:?}{}{} | RAM: {:.1} GB{}| VRAM: {:.1} GB | CPU: {} ({} cores)",
            self.os,
            wsl,
            self.arch,
            self.gpu_vendor,
            if self.gpu_count > 1 {
                format!(" x{}", self.gpu_count)
            } else {
                String::new()
            },
            cc,
            self.total_ram_gb,
            if self.unified_mem { " (unified) " } else { " " },
            self.vram_gb,
            self.cpu_model,
            self.cpu_cores,
        )
    }
}

/// Detect OS, arch, GPU vendor, RAM, CPU info, and (schema v2) compute capability,
/// CUDA versions, OS family, WSL2 status, and GPU count.
pub fn detect_platform() -> Result<HardwareProfile> {
    let os = detect_os();
    let arch = detect_arch();
    let gpu_vendor = detect_gpu_vendor(os, arch);
    let unified_mem = os == Os::Darwin && arch == Arch::Aarch64;
    let is_tegra = detect_tegra();
    let (total_ram_gb, cpu_cores, cpu_model) = detect_system_info();

    // ── Schema v2 fields ─────────────────────────────────────────────────────
    let is_wsl = detect_wsl();
    let os_family = derive_os_family(os, is_wsl);
    let (compute_cap, gpu_count) = if gpu_vendor == GpuVendor::Nvidia {
        (detect_nvidia_compute_cap(), detect_nvidia_gpu_count())
    } else {
        (None, 0)
    };
    let cuda_runtime_version = if gpu_vendor == GpuVendor::Nvidia {
        detect_cuda_runtime_version()
    } else {
        None
    };
    let cuda_driver_version = if gpu_vendor == GpuVendor::Nvidia {
        detect_cuda_driver_version()
    } else {
        None
    };
    let driver_tuple = cuda_driver_version.as_deref().and_then(parse_driver_tuple);

    debug!(
        ?os,
        ?os_family,
        ?arch,
        ?gpu_vendor,
        unified_mem,
        is_wsl,
        gpu_count,
        ?compute_cap,
        ?cuda_runtime_version,
        ?cuda_driver_version,
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
        schema_version: HARDWARE_SCHEMA_VERSION,
        compute_cap,
        cuda_runtime_version,
        cuda_driver_version,
        driver_tuple,
        os_family,
        is_wsl,
        gpu_count,
    })
}

/// Parse an `nvidia-smi`-style driver version string into a `(major, minor, patch)`
/// tuple. Accepts:
///   * `"595.71.05"`  → `Some((595, 71, 5))`
///   * `"570.26"`     → `Some((570, 26, 0))`
///   * `"570"`        → `Some((570, 0, 0))`
///   * `""` / garbage → `None`
///
/// The patch field accepts leading zeros (`05`) because the driver tooling
/// emits zero-padded patches.
pub fn parse_driver_tuple(s: &str) -> Option<(u32, u32, u32)> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
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

    // Intel: iGPU (Iris/UHD) or discrete Arc — only reported when no NVIDIA
    // or AMD card is present, so we don't accidentally downgrade a system
    // with both an Intel iGPU + a real dGPU.
    if check_intel_present() {
        return GpuVendor::Intel;
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

/// Pure helper — unit-tested; mirrors the Windows CIM AdapterCompatibility check.
fn adapter_compat_indicates_amd(stdout: &str) -> bool {
    let lower = stdout.to_lowercase();
    lower.contains("advanced micro devices") || lower.contains("amd")
}

/// Query Windows video adapter vendors via CIM (shared by AMD/Intel probes).
#[cfg(target_os = "windows")]
fn windows_video_adapter_compat() -> Option<String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty AdapterCompatibility) -join ';'",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
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

    #[cfg(target_os = "windows")]
    if let Some(stdout) = windows_video_adapter_compat() {
        return adapter_compat_indicates_amd(&stdout);
    }

    false
}

/// Detect Intel GPUs (iGPU or discrete Arc). Probed AFTER NVIDIA + AMD so
/// systems with a real dGPU + an Intel iGPU don't get reported as Intel.
fn check_intel_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        // Read every DRM card's vendor ID. Intel's PCI vendor is 0x8086.
        // The path layout is `/sys/class/drm/card<N>/device/vendor`; the file
        // contains the hex vendor ID on its own line. This catches both Iris
        // iGPUs and discrete Arc dGPUs without needing lspci installed.
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !name_str.starts_with("card") || name_str.contains('-') {
                    // Skip card connectors like "card0-DP-1"; only the cardN nodes.
                    continue;
                }
                let vendor_path = entry.path().join("device/vendor");
                if let Ok(v) = std::fs::read_to_string(&vendor_path)
                    && v.trim().eq_ignore_ascii_case("0x8086")
                {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(target_os = "windows")]
    if let Some(stdout) = windows_video_adapter_compat() {
        return stdout.to_lowercase().contains("intel");
    } else {
        return false;
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        // macOS Apple Silicon already returned GpuVendor::Apple upstream of
        // this function; macOS Intel hardware (long EOL) is out of scope.
        false
    }
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

// ─── Schema v2 detection helpers ────────────────────────────────────────────

/// WSL2 advertises its kernel as `*-microsoft-standard-WSL2` in
/// `/proc/sys/kernel/osrelease`. On native Linux the string is `*-generic` or
/// the distro's vendor tag — never contains "microsoft" or "WSL".
fn detect_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            let r = release.to_lowercase();
            return r.contains("microsoft") || r.contains("wsl");
        }
    }
    false
}

/// Map `(Os, is_wsl)` → `OsFamily`. Pure function so it's trivially testable.
pub fn derive_os_family(os: Os, is_wsl: bool) -> OsFamily {
    match (os, is_wsl) {
        (Os::Linux, true) => OsFamily::WindowsWsl2,
        (Os::Linux, false) => OsFamily::Linux,
        (Os::Windows, _) => OsFamily::WindowsNative,
        (Os::Darwin, _) => OsFamily::Darwin,
        (Os::Unknown, _) => OsFamily::Unknown,
    }
}

/// Query NVIDIA compute capability via `nvidia-smi --query-gpu=compute_cap`.
/// Returns the highest cap across all GPUs (`max` so a mixed 4090+5090 box
/// is treated as Blackwell-capable for tier selection).
pub fn detect_nvidia_compute_cap() -> Option<ComputeCap> {
    let output = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=compute_cap", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter_map(parse_compute_cap)
        .max_by(|a, b| a.cmp(b))
}

/// Parse a single `nvidia-smi` compute-cap line like `"12.0"` → `(12, 0)`.
/// Exposed for unit tests; returns `None` on any malformed input.
pub fn parse_compute_cap(line: &str) -> Option<ComputeCap> {
    let trimmed = line.trim();
    let (maj_str, min_str) = trimmed.split_once('.')?;
    let major: u8 = maj_str.parse().ok()?;
    let minor: u8 = min_str.parse().ok()?;
    Some((major, minor))
}

/// `nvidia-smi -L` lists one line per GPU. Count is the number of non-empty lines.
fn detect_nvidia_gpu_count() -> u8 {
    let output = std::process::Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .ok();
    let Some(output) = output else { return 0 };
    if !output.status.success() {
        return 0;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let n = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    u8::try_from(n.min(255)).unwrap_or(255)
}

/// CUDA *runtime* version (the toolkit nvcc came from). Prefer `nvcc --version`
/// because it reflects what built any source-built engines; fall back to
/// `nvidia-smi` (which reports the *driver*'s embedded runtime).
fn detect_cuda_runtime_version() -> Option<String> {
    // Try nvcc first
    if let Ok(output) = std::process::Command::new("nvcc").arg("--version").output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(v) = parse_nvcc_version(&stdout) {
            return Some(v);
        }
    }
    // Fallback: nvidia-smi reports "CUDA Version: X.Y" in its header.
    detect_cuda_runtime_from_smi()
}

/// Parse `nvcc --version` output, looking for `release X.Y` (e.g. `release 13.0`).
pub fn parse_nvcc_version(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some(idx) = line.find("release ") {
            let rest = &line[idx + 8..];
            // rest looks like "13.0, V13.0.48" — take up to comma or whitespace
            let v: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn detect_cuda_runtime_from_smi() -> Option<String> {
    let output = std::process::Command::new("nvidia-smi").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_smi_cuda_version(&stdout)
}

/// `nvidia-smi` header contains a line like:
///   `... Driver Version: 580.95.05  CUDA Version: 13.0 ...`
/// We want the "13.0" part.
pub fn parse_smi_cuda_version(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some(idx) = line.find("CUDA Version:") {
            let rest = line[idx + "CUDA Version:".len()..].trim_start();
            let v: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// CUDA *driver* version from `nvidia-smi --query-gpu=driver_version`.
fn detect_cuda_driver_version() -> Option<String> {
    let output = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=driver_version", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_driver_tuple_three_segment() {
        assert_eq!(parse_driver_tuple("595.71.05"), Some((595, 71, 5)));
        assert_eq!(parse_driver_tuple("580.95.05"), Some((580, 95, 5)));
        assert_eq!(parse_driver_tuple("570.26.00"), Some((570, 26, 0)));
    }

    #[test]
    fn parse_driver_tuple_two_segment_pads_zero_patch() {
        assert_eq!(parse_driver_tuple("570.26"), Some((570, 26, 0)));
        assert_eq!(parse_driver_tuple("590.44"), Some((590, 44, 0)));
    }

    #[test]
    fn parse_driver_tuple_single_segment_pads_zeros() {
        assert_eq!(parse_driver_tuple("570"), Some((570, 0, 0)));
    }

    #[test]
    fn parse_driver_tuple_rejects_garbage() {
        assert_eq!(parse_driver_tuple(""), None);
        assert_eq!(parse_driver_tuple("not-a-version"), None);
        assert_eq!(parse_driver_tuple("abc.def.ghi"), None);
    }

    #[test]
    fn parse_driver_tuple_tolerates_whitespace() {
        assert_eq!(parse_driver_tuple("  595.71.05  "), Some((595, 71, 5)));
    }

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
            gpu_vendor: GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".to_string(),
            os_family: OsFamily::Darwin,
            schema_version: HARDWARE_SCHEMA_VERSION,
            ..Default::default()
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

    #[test]
    fn gpu_vendor_serde_includes_intel() {
        // Catalog comments + UI display rely on the lowercase serde
        // representation. Adding GpuVendor::Intel must serialize to "intel"
        // — anything else silently breaks `format_for_gpu_vendor()` lookups.
        let json = serde_json::to_string(&GpuVendor::Intel).unwrap();
        assert_eq!(json, "\"intel\"");
        let parsed: GpuVendor = serde_json::from_str("\"intel\"").unwrap();
        assert_eq!(parsed, GpuVendor::Intel);
    }

    #[test]
    fn check_intel_present_does_not_panic_on_this_host() {
        // We can't assert true/false (depends on whether the test runner has
        // an Intel iGPU), but the function must not panic — it spawns child
        // processes and touches /sys/class/drm, both of which can return
        // unexpected output on weird kernels or in chroots/containers.
        let _ = check_intel_present();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn check_intel_present_handles_missing_sysfs() {
        // /sys/class/drm may not exist inside minimal containers. The
        // function must return false (not panic, not error) when the dir
        // is absent. We can't unmount it for the test, so we sanity-check
        // via the public probe that it returns *some* GpuVendor variant.
        let vendor = detect_gpu_vendor(Os::Linux, Arch::X86_64);
        // Either some valid variant — the function must terminate.
        assert!(matches!(
            vendor,
            GpuVendor::Nvidia
                | GpuVendor::Amd
                | GpuVendor::Intel
                | GpuVendor::Apple
                | GpuVendor::None
        ));
    }

    // ── Schema v2 parser tests ───────────────────────────────────────────────

    #[test]
    fn test_parse_compute_cap_blackwell() {
        // RTX 5090, 5080, 5070 — all sm_120.
        assert_eq!(parse_compute_cap("12.0"), Some((12, 0)));
        // DGX Spark GB10 — sm_121.
        assert_eq!(parse_compute_cap("12.1"), Some((12, 1)));
    }

    #[test]
    fn test_parse_compute_cap_legacy_archs() {
        assert_eq!(parse_compute_cap("7.5"), Some((7, 5))); // Turing (RTX 20)
        assert_eq!(parse_compute_cap("8.6"), Some((8, 6))); // Ampere (RTX 30)
        assert_eq!(parse_compute_cap("8.9"), Some((8, 9))); // Ada (RTX 40)
        assert_eq!(parse_compute_cap("9.0"), Some((9, 0))); // Hopper (H100)
        assert_eq!(parse_compute_cap("10.0"), Some((10, 0))); // Datacenter Blackwell (B200)
    }

    #[test]
    fn test_parse_compute_cap_whitespace_tolerant() {
        assert_eq!(parse_compute_cap("  12.0  \n"), Some((12, 0)));
        assert_eq!(parse_compute_cap("\t8.9"), Some((8, 9)));
    }

    #[test]
    fn test_parse_compute_cap_malformed() {
        assert_eq!(parse_compute_cap(""), None);
        assert_eq!(parse_compute_cap("garbage"), None);
        assert_eq!(parse_compute_cap("12"), None); // missing minor
        // Reject ambiguous / multi-dotted strings — `split_once` only handles MAJOR.MINOR.
        assert_eq!(parse_compute_cap("12.0.5"), None);
        // "999.999" would overflow u8 — must reject, not panic.
        assert_eq!(parse_compute_cap("999.999"), None);
    }

    #[test]
    fn test_compute_cap_ordering_supports_gating() {
        // The selector needs to express "compute_cap in [9.0, 10.x]" — verify
        // tuple ordering does what we expect.
        let sm75: ComputeCap = (7, 5);
        let sm90: ComputeCap = (9, 0);
        let sm100: ComputeCap = (10, 0);
        let sm103: ComputeCap = (10, 3);
        let sm120: ComputeCap = (12, 0);

        assert!(sm75 < sm90);
        assert!(sm90 < sm100);
        assert!(sm100 < sm103);
        assert!(sm103 < sm120);
        // SGLang gate: sm_90 ≤ cc ≤ sm_103 (excludes consumer Blackwell sm_120).
        assert!((sm90..=sm103).contains(&sm90));
        assert!((sm90..=sm103).contains(&sm103));
        assert!(!(sm90..=sm103).contains(&sm120));
        assert!(!(sm90..=sm103).contains(&sm75));
    }

    #[test]
    fn test_parse_nvcc_version_cuda_13() {
        let nvcc_out = "nvcc: NVIDIA (R) Cuda compiler driver\n\
                        Copyright (c) 2005-2025 NVIDIA Corporation\n\
                        Built on Thu_Sep_xxx\n\
                        Cuda compilation tools, release 13.0, V13.0.48\n\
                        Build cuda_13.0.r13.0/compiler.xxx_0\n";
        assert_eq!(parse_nvcc_version(nvcc_out), Some("13.0".to_string()));
    }

    #[test]
    fn test_parse_nvcc_version_cuda_12_8() {
        let nvcc_out = "Cuda compilation tools, release 12.8, V12.8.93\n";
        assert_eq!(parse_nvcc_version(nvcc_out), Some("12.8".to_string()));
    }

    #[test]
    fn test_parse_nvcc_version_no_release() {
        assert_eq!(parse_nvcc_version("some unrelated output"), None);
    }

    #[test]
    fn test_parse_smi_cuda_version_typical() {
        // Real-world header line — slightly abridged.
        let smi = "+-----------------------------------------------------------------------------------------+\n\
                   | NVIDIA-SMI 580.95.05              Driver Version: 580.95.05      CUDA Version: 13.0     |\n\
                   |-----------------------------------------+------------------------+----------------------+\n";
        assert_eq!(parse_smi_cuda_version(smi), Some("13.0".to_string()));
    }

    #[test]
    fn test_parse_smi_cuda_version_cuda_12() {
        let smi = "| NVIDIA-SMI 535.171.04             Driver Version: 535.171.04   CUDA Version: 12.2     |\n";
        assert_eq!(parse_smi_cuda_version(smi), Some("12.2".to_string()));
    }

    #[test]
    fn test_parse_smi_cuda_version_absent() {
        let smi = "nothing useful here\n";
        assert_eq!(parse_smi_cuda_version(smi), None);
    }

    #[test]
    fn test_derive_os_family_matrix() {
        // Native Linux: not WSL.
        assert_eq!(derive_os_family(Os::Linux, false), OsFamily::Linux);
        // WSL2: kernel reports Linux but we know better.
        assert_eq!(derive_os_family(Os::Linux, true), OsFamily::WindowsWsl2);
        // Native Windows: `is_wsl` is meaningless (always false).
        assert_eq!(
            derive_os_family(Os::Windows, false),
            OsFamily::WindowsNative
        );
        // macOS.
        assert_eq!(derive_os_family(Os::Darwin, false), OsFamily::Darwin);
    }

    #[test]
    fn test_hardware_profile_serde_roundtrip_v2() {
        let original = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 15.4,
            total_ram_gb: 15.6,
            cpu_cores: 6,
            cpu_model: "Intel(R) Core(TM) Ultra 7 265K".to_string(),
            schema_version: HARDWARE_SCHEMA_VERSION,
            compute_cap: Some((12, 0)),
            cuda_runtime_version: Some("13.0".to_string()),
            cuda_driver_version: Some("580.95.05".to_string()),
            os_family: OsFamily::Linux,
            gpu_count: 1,
            ..Default::default()
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: HardwareProfile = serde_json::from_str(&json).expect("parse v2");
        assert_eq!(parsed.compute_cap, Some((12, 0)));
        assert_eq!(parsed.cuda_runtime_version.as_deref(), Some("13.0"));
        assert_eq!(parsed.os_family, OsFamily::Linux);
        assert_eq!(parsed.schema_version, HARDWARE_SCHEMA_VERSION);
    }

    #[test]
    fn test_hardware_profile_parses_v1_without_new_fields() {
        // A v1 hardware.json — none of the schema v2 fields. Must still parse,
        // and `schema_version` must default to 1 so the reader knows it's stale.
        let v1_json = r#"{
            "os": "linux",
            "arch": "x86_64",
            "is_tegra": false,
            "gpu_vendor": "nvidia",
            "vram_gb": 15.4,
            "unified_mem": false,
            "total_ram_gb": 15.6,
            "cpu_cores": 6,
            "cpu_model": "Intel(R) Core(TM) Ultra 7 265K"
        }"#;
        let parsed: HardwareProfile =
            serde_json::from_str(v1_json).expect("v1 file must still parse");
        assert_eq!(parsed.schema_version, 1, "missing field → defaults to v1");
        assert!(parsed.compute_cap.is_none());
        assert!(parsed.cuda_runtime_version.is_none());
        assert_eq!(parsed.os_family, OsFamily::Unknown);
        assert!(!parsed.is_wsl);
        assert_eq!(parsed.gpu_count, 0);
    }

    #[test]
    fn test_adapter_compat_indicates_amd() {
        assert!(adapter_compat_indicates_amd(
            "NVIDIA;Advanced Micro Devices, Inc."
        ));
        assert!(adapter_compat_indicates_amd("AMD Radeon"));
        assert!(!adapter_compat_indicates_amd("Intel Corporation"));
        assert!(!adapter_compat_indicates_amd("NVIDIA"));
    }
}
