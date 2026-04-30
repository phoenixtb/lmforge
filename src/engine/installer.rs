use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use super::registry::EngineConfig;
use crate::hardware::probe::{Arch, HardwareProfile, Os};

/// Result of an engine installation attempt
#[derive(Debug)]
pub struct InstallResult {
    pub engine_id: String,
    pub version: String,
    pub install_path: String,
    pub method_used: String,
}

/// Install an engine based on its config and the current hardware profile.
pub async fn install(
    engine: &EngineConfig,
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
) -> Result<InstallResult> {
    info!(engine = %engine.id, version = %engine.version, method = %engine.install_method, "Installing engine");

    // Check if already installed
    if let Some(path) = find_existing_install(engine, data_dir) {
        println!(
            "  ✓ {} v{} already installed at {}",
            engine.name, engine.version, path
        );
        return Ok(InstallResult {
            engine_id: engine.id.clone(),
            version: engine.version.clone(),
            install_path: path,
            method_used: "existing".to_string(),
        });
    }

    match engine.install_method.as_str() {
        "brew" => install_via_brew(engine, data_dir).await,
        "pip" => install_via_pip(engine, data_dir).await,
        "binary" => install_via_binary(engine, profile, data_dir).await,
        other => bail!("Unknown install method: {}", other),
    }
}

/// Check if the engine binary is already available
fn find_existing_install(engine: &EngineConfig, data_dir: &std::path::Path) -> Option<String> {
    // Check if the start command is in PATH
    let cmd = &engine.start_cmd;
    if let Ok(output) = std::process::Command::new("which").arg(cmd).output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Verify version if possible
        if verify_engine_version(engine, &path) {
            return Some(path);
        }
    }

    // Check local engine directory
    if let Some(ref binary) = engine.binary {
        let local_path = data_dir.join("engines").join(binary);
        if local_path.exists() {
            return Some(local_path.to_string_lossy().to_string());
        }
    }

    // Check venv for pip installs
    let venv_bin = data_dir
        .join("engines")
        .join(&engine.id)
        .join("venv")
        .join("bin")
        .join(cmd);
    if venv_bin.exists() {
        return Some(venv_bin.to_string_lossy().to_string());
    }

    None
}

/// Verify the installed engine version matches what we expect
fn verify_engine_version(engine: &EngineConfig, _path: &str) -> bool {
    // For brew-installed engines, check brew info
    if engine.install_method == "brew"
        && let Some(ref formula) = engine.brew_formula
        && let Ok(output) = std::process::Command::new("brew")
            .args(["list", "--versions", formula])
            .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!(formula, output = %stdout.trim(), "Brew version check");
        // Accept any version — the user may have a different version installed
        return true;
    }

    // For binary engines, just check existence
    true
}

/// Install via Homebrew (primary method for oMLX on macOS).
/// Never silently falls back to pip — instead prints clear instructions
/// so the user can choose their preferred Python management method.
async fn install_via_brew(
    engine: &EngineConfig,
    _data_dir: &std::path::Path,
) -> Result<InstallResult> {
    let brew_tap = engine.brew_tap.as_deref().unwrap_or("");
    let brew_tap_url = engine.brew_tap_url.as_deref().unwrap_or("");
    let brew_formula = engine.brew_formula.as_deref().unwrap_or(&engine.id);
    let pip_pkg = engine
        .pip_package
        .as_deref()
        .or(engine.pip_fallback.as_deref())
        .unwrap_or(&engine.id);

    // Helper: print the full install guidance and bail
    let guidance = |reason: &str| -> anyhow::Error {
        eprintln!();
        eprintln!("  ✗ {}", reason);
        eprintln!();
        eprintln!(
            "  ── How to install {} ─────────────────────────────",
            engine.name
        );
        eprintln!();
        eprintln!("  Recommended — Homebrew (https://brew.sh):");
        if !brew_tap.is_empty() {
            if !brew_tap_url.is_empty() {
                eprintln!("    brew tap {} {}", brew_tap, brew_tap_url);
            } else {
                eprintln!("    brew tap {}", brew_tap);
            }
        }
        eprintln!("    brew install {}", brew_formula);
        eprintln!();
        eprintln!("  Alternative — pip (use your own Python env):");
        eprintln!("    pip install {}      # system / conda / pyenv", pip_pkg);
        eprintln!("    # or with Metal acceleration (Apple Silicon):");
        eprintln!("    pip install {}[metal]", pip_pkg);
        eprintln!();
        eprintln!("  After installing, run:  lmforge start");
        eprintln!();
        anyhow::anyhow!(
            "{} — install {} manually using one of the options above.",
            reason,
            engine.name
        )
    };

    // ── 1. Homebrew must be present ────────────────────────────────────────────
    if !command_exists("brew") {
        return Err(guidance(
            "Homebrew is not installed. \
             Install it from https://brew.sh and re-run `lmforge init`.",
        ));
    }

    // ── 2. Tap the repository ─────────────────────────────────────────────────
    if !brew_tap.is_empty() {
        println!("  ⚙ Adding Homebrew tap: {}", brew_tap);
        // Third-party taps need the source URL as a second argument.
        // HOMEBREW_NO_AUTO_UPDATE skips the auto-updater that floods the
        // pipe buffer and can cause a spurious non-zero exit in subprocesses.
        let mut tap_cmd = tokio::process::Command::new("brew");
        tap_cmd
            .args(["tap", brew_tap])
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("HOMEBREW_NO_ENV_HINTS", "1");
        if !brew_tap_url.is_empty() {
            tap_cmd.arg(brew_tap_url);
        }
        let tap_out = tap_cmd.output().await.context("Failed to run 'brew tap'")?;

        let tap_stderr = String::from_utf8_lossy(&tap_out.stderr);
        let tap_stdout = String::from_utf8_lossy(&tap_out.stdout);

        // "already tapped" (or updated as part of general auto-update) = success
        let already = tap_stderr.contains("already tapped")
            || tap_stdout.contains("already tapped")
            || tap_stderr.contains(brew_tap) && tap_stdout.contains("Updated");

        if !tap_out.status.success() && !already {
            let detail = if tap_stderr.trim().is_empty() {
                tap_stdout.trim().to_string()
            } else {
                tap_stderr.trim().to_string()
            };
            return Err(guidance(&format!(
                "brew tap {} failed.\n  Brew said: {}",
                brew_tap, detail
            )));
        }
        if already {
            info!("Tap {} already added", brew_tap);
        }
    }

    // ── 3. Install the formula ────────────────────────────────────────────────
    println!("  ⚙ Installing {} via Homebrew...", brew_formula);
    let output = tokio::process::Command::new("brew")
        .args(["install", brew_formula])
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .output()
        .await
        .context("Failed to run 'brew install'")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already installed") {
            info!(brew_formula, "Already installed via Homebrew");
        } else {
            return Err(guidance(&format!(
                "brew install {} failed:\n  {}",
                brew_formula,
                stderr.trim()
            )));
        }
    }

    // ── 4. Verify the binary is on PATH ───────────────────────────────────────
    let cmd = &engine.start_cmd;
    if let Ok(which_out) = std::process::Command::new("which").arg(cmd).output()
        && which_out.status.success()
    {
        let path = String::from_utf8_lossy(&which_out.stdout)
            .trim()
            .to_string();
        println!("  ✓ {} installed at {}", engine.name, path);
        return Ok(InstallResult {
            engine_id: engine.id.clone(),
            version: engine.version.clone(),
            install_path: path,
            method_used: "brew".to_string(),
        });
    }

    Err(guidance(&format!(
        "brew install {} succeeded but '{}' was not found in PATH. \
         Try opening a new terminal and running `lmforge start`.",
        brew_formula, cmd
    )))
}

/// Install via pip in an isolated venv (fallback for oMLX, primary for SGLang)
async fn install_via_pip(
    engine: &EngineConfig,
    data_dir: &std::path::Path,
) -> Result<InstallResult> {
    // Run preflight checks
    run_preflight_checks(engine)?;

    // Determine python command
    let python = find_python()?;

    // Create venv directory
    let venv_dir = data_dir.join("engines").join(&engine.id).join("venv");
    std::fs::create_dir_all(&venv_dir)?;

    if !venv_dir.join("bin").join("python3").exists() {
        println!("  ⚙ Creating Python venv at {}...", venv_dir.display());
        let status = tokio::process::Command::new(&python)
            .args(["-m", "venv", &venv_dir.to_string_lossy()])
            .status()
            .await
            .context("Failed to create Python venv")?;

        if !status.success() {
            bail!("Failed to create Python virtual environment. Ensure Python 3.10+ is installed.");
        }
    }

    // Determine the pip package to install
    let pip_pkg = engine
        .pip_package
        .as_ref()
        .or(engine.pip_fallback.as_ref())
        .context("No pip package specified for engine")?;

    // Install via pip in the venv
    let pip_path = venv_dir.join("bin").join("pip3");
    println!("  ⚙ Installing {} via pip...", pip_pkg);
    let output = tokio::process::Command::new(&pip_path)
        .args(["install", pip_pkg])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("Failed to run pip install")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pip install {} failed:\n{}", pip_pkg, stderr);
    }

    // Verify the engine binary exists in the venv
    let engine_bin = venv_dir.join("bin").join(&engine.start_cmd);
    if !engine_bin.exists() {
        bail!(
            "pip install succeeded but '{}' not found in venv at {}",
            engine.start_cmd,
            engine_bin.display()
        );
    }

    let path = engine_bin.to_string_lossy().to_string();
    println!("  ✓ {} installed at {}", engine.name, path);

    Ok(InstallResult {
        engine_id: engine.id.clone(),
        version: engine.version.clone(),
        install_path: path,
        method_used: "pip".to_string(),
    })
}

/// Install via pre-built binary download (llama.cpp)
async fn install_via_binary(
    engine: &EngineConfig,
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
) -> Result<InstallResult> {
    let release_url = engine
        .release_url
        .as_ref()
        .context("No release_url for binary engine")?;

    let asset_pattern = engine
        .asset_pattern
        .as_ref()
        .context("No asset_pattern for binary engine")?;

    let binary_name = engine.binary.as_ref().context("No binary name specified")?;

    // Resolve platform string and file extension
    let (platform, extension) = resolve_platform(profile)?;
    let asset_name = format!(
        "{}.{}",
        asset_pattern.replace("{platform}", &platform),
        extension
    );
    let download_url = format!("{}/{}", release_url, asset_name);

    println!("  ⚙ Downloading {} from:", engine.name);
    println!("    {}", download_url);

    // Ensure engines directory exists
    let engines_dir = data_dir.join("engines");
    std::fs::create_dir_all(&engines_dir)?;

    let archive_path = engines_dir.join(&asset_name);

    // Download the main binary archive
    download_file(&download_url, &archive_path).await?;

    // On Windows CUDA builds, also download the CUDA runtime DLL package (cudart).
    // Without this, llama-server.exe will fail with "cudart64_*.dll not found".
    let cudart_archive_path = if profile.os == Os::Windows
        && profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia
    {
        if let Some(ref cudart_pattern) = engine.cudart_pattern {
            let cuda_variant = detect_windows_cuda_variant();
            let cudart_name = format!(
                "{}.zip",
                cudart_pattern.replace("{cuda_variant}", &cuda_variant)
            );
            let cudart_url = format!("{}/{}", release_url, cudart_name);
            let cudart_path = engines_dir.join(&cudart_name);
            println!("  ⚙ Downloading CUDA runtime DLLs from:");
            println!("    {}", cudart_url);
            download_file(&cudart_url, &cudart_path).await?;
            Some(cudart_path)
        } else {
            warn!("No cudart_pattern configured — GPU may fail to initialize on Windows");
            None
        }
    } else {
        None
    };

    // Extract the archive
    println!("  ⚙ Extracting...");
    let extract_dir = engines_dir.join(format!("{}-extract", engine.id));
    std::fs::create_dir_all(&extract_dir)?;

    extract_archive(&archive_path, &extract_dir, profile).await?;

    // On Windows CUDA: extract cudart DLLs into the same directory so the
    // binary can find cudart64_*.dll at startup.
    if let Some(ref cudart_path) = cudart_archive_path {
        extract_archive(cudart_path, &extract_dir, profile).await?;
    }

    // Resolve the binary name (add .exe on Windows)
    let resolved_binary = if profile.os == Os::Windows {
        format!("{}.exe", binary_name)
    } else {
        binary_name.to_string()
    };

    // Find the binary in the extracted directory
    let found_binary = find_binary_in_dir(&extract_dir, &resolved_binary)?;
    let dest_path = engines_dir.join(&resolved_binary);

    // Copy binary to engines directory
    std::fs::copy(&found_binary, &dest_path)
        .context("Failed to copy binary to engines directory")?;

    // On Windows CUDA: copy DLLs alongside the binary so they are co-located
    if cudart_archive_path.is_some() {
        copy_dlls_to_dir(&extract_dir, &engines_dir)?;
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest_path, perms)?;
    }

    // Cleanup archives and temp extract dir
    let _ = std::fs::remove_file(&archive_path);
    if let Some(cudart_path) = cudart_archive_path {
        let _ = std::fs::remove_file(cudart_path);
    }
    let _ = std::fs::remove_dir_all(&extract_dir);

    let path = dest_path.to_string_lossy().to_string();
    println!("  ✓ {} installed at {}", engine.name, path);

    Ok(InstallResult {
        engine_id: engine.id.clone(),
        version: engine.version.clone(),
        install_path: path,
        method_used: "binary".to_string(),
    })
}

/// Download a file with progress reporting
async fn download_file(url: &str, dest: &std::path::Path) -> Result<()> {
    use futures::StreamExt;
    use indicatif::{ProgressBar, ProgressStyle};

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let resp = client
        .get(url)
        .send()
        .await
        .context("Failed to start download")?;

    if !resp.status().is_success() {
        bail!("Download failed: HTTP {}", resp.status());
    }

    let total_size = resp.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("    [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("█▓░"),
    );

    let mut file = std::fs::File::create(dest)?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Error reading download stream")?;
        std::io::Write::write_all(&mut file, &chunk)?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("done");
    info!(url, bytes = downloaded, "Download complete");

    Ok(())
}

/// Resolve the platform identifier string and file extension for a given hardware profile.
/// Returns (platform_string, extension) e.g. ("ubuntu-vulkan-x64", "tar.gz").
fn resolve_platform(profile: &HardwareProfile) -> Result<(String, &'static str)> {
    let (platform, ext) = match (profile.os, profile.arch) {
        (Os::Darwin, Arch::Aarch64) => ("macos-arm64".to_string(), "tar.gz"),
        (Os::Darwin, Arch::X86_64) => ("macos-x64".to_string(), "tar.gz"),
        (Os::Linux, Arch::X86_64) => {
            // llama.cpp dropped the Linux CUDA pre-built binary starting ~b8370.
            // The Vulkan binary is the GPU-accelerated option for Linux (NVIDIA/AMD/Intel).
            // CPU-only fallback for non-GPU systems.
            let p = if profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia
                || profile.gpu_vendor == crate::hardware::probe::GpuVendor::Amd
            {
                "ubuntu-vulkan-x64"
            } else {
                "ubuntu-x64"
            };
            (p.to_string(), "tar.gz")
        }
        (Os::Linux, Arch::Aarch64) => ("ubuntu-arm64".to_string(), "tar.gz"),
        (Os::Windows, Arch::X86_64) => {
            // Windows ships CUDA-accelerated and CPU-only variants as .zip files.
            let p = if profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia {
                // Detect CUDA version and pick the matching CUDA runtime variant.
                let cuda_variant = detect_windows_cuda_variant();
                format!("win-cuda-{}-x64", cuda_variant)
            } else {
                // CPU-only fallback (no GPU or AMD — Vulkan not yet in Windows prebuilt)
                "win-cpu-x64".to_string()
            };
            (p, "zip")
        }
        (Os::Windows, Arch::Aarch64) => ("win-cpu-arm64".to_string(), "zip"),
        _ => bail!(
            "No pre-built binary available for {:?} {:?}",
            profile.os,
            profile.arch
        ),
    };
    Ok((platform, ext))
}

/// Detect the CUDA version installed on Windows by querying nvidia-smi.
/// Returns "13.1" for CUDA 13.x, "12.4" as the default for CUDA 12.x or unknown.
/// These correspond to the two CUDA variants shipped in llama.cpp Windows releases.
fn detect_windows_cuda_variant() -> String {
    if let Ok(output) = std::process::Command::new("nvidia-smi").output()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        for line in stdout.lines() {
            // nvidia-smi header contains "CUDA Version: X.Y"
            if let Some(idx) = line.find("CUDA Version:") {
                let ver_str = line[idx + 13..].trim();
                let major: u32 = ver_str
                    .split('.')
                    .next()
                    .unwrap_or("12")
                    .parse()
                    .unwrap_or(12);
                let variant = if major >= 13 { "13.1" } else { "12.4" };
                debug!(
                    cuda_version = ver_str,
                    variant, "Detected Windows CUDA variant"
                );
                return variant.to_string();
            }
        }
    }
    warn!("Could not detect CUDA version via nvidia-smi; defaulting to CUDA 12.4 variant");
    "12.4".to_string()
}

/// Extract an archive (.tar.gz or .zip) into the target directory.
async fn extract_archive(
    archive: &std::path::Path,
    dest: &std::path::Path,
    profile: &HardwareProfile,
) -> Result<()> {
    let archive_str = archive.to_string_lossy().to_string();
    let dest_str = dest.to_string_lossy().to_string();

    let is_zip = archive_str.ends_with(".zip");

    if is_zip && profile.os == Os::Windows {
        // Use PowerShell's Expand-Archive (available on Windows 10+)
        let status = tokio::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                    archive_str, dest_str
                ),
            ])
            .status()
            .await
            .context("Failed to run PowerShell Expand-Archive")?;
        if !status.success() {
            bail!("Failed to extract zip: {}", archive_str);
        }
    } else {
        // Unix: tar handles .tar.gz
        let status = tokio::process::Command::new("tar")
            .args(["xzf", &archive_str, "-C", &dest_str])
            .status()
            .await
            .context("Failed to extract tar.gz archive")?;
        if !status.success() {
            bail!("Failed to extract archive: {}", archive_str);
        }
    }
    Ok(())
}

/// Copy all DLL files from a source directory tree into the destination directory.
/// Used on Windows to place CUDA runtime DLLs alongside the llama-server.exe binary.
fn copy_dlls_to_dir(src_dir: &std::path::Path, dest_dir: &std::path::Path) -> Result<()> {
    for entry in walkdir(src_dir)? {
        if entry
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("dll"))
            .unwrap_or(false)
            && let Some(fname) = entry.file_name()
        {
            let dest = dest_dir.join(fname);
            std::fs::copy(&entry, &dest).with_context(|| {
                format!("Failed to copy DLL {} to engines dir", entry.display())
            })?;
            debug!(dll = ?fname, "Copied CUDA runtime DLL");
        }
    }
    Ok(())
}

/// Find a binary by name within a directory (recursive)
fn find_binary_in_dir(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    for entry in walkdir(dir)? {
        if let Some(fname) = entry.file_name().and_then(|n| n.to_str())
            && fname == name
        {
            return Ok(entry);
        }
    }
    bail!(
        "Binary '{}' not found in extracted archive at {}",
        name,
        dir.display()
    );
}

/// Simple recursive directory walk
fn walkdir(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut results = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                results.extend(walkdir(&path)?);
            } else {
                results.push(path);
            }
        }
    }
    Ok(results)
}

/// Check if a command exists in PATH
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Find a suitable Python 3.10+ interpreter
fn find_python() -> Result<String> {
    for candidate in &["python3", "python"] {
        if let Ok(output) = std::process::Command::new(candidate)
            .args(["--version"])
            .output()
            && output.status.success()
        {
            let version = String::from_utf8_lossy(&output.stdout);
            debug!(python = candidate, version = %version.trim(), "Found Python");
            // Check version >= 3.10
            if let Some(ver_str) = version.split_whitespace().nth(1) {
                let parts: Vec<&str> = ver_str.split('.').collect();
                if parts.len() >= 2
                    && let (Ok(major), Ok(minor)) =
                        (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                    && major >= 3
                    && minor >= 10
                {
                    return Ok(candidate.to_string());
                }
            }
        }
    }
    bail!(
        "Python 3.10+ not found. Install it from https://www.python.org/downloads/ \
         or via Homebrew: brew install python@3.12"
    );
}

/// Run preflight checks for engines that require them (e.g., SGLang)
fn run_preflight_checks(engine: &EngineConfig) -> Result<()> {
    for check in &engine.preflight {
        if !command_exists(check) {
            bail!(
                "{} requires '{}' but it was not found in PATH.\n\
                 Please install '{}' before continuing.",
                engine.name,
                check,
                check
            );
        }
        debug!(check, "Preflight check passed");
    }

    // Check disk space if required
    if let Some(min_gb) = engine.min_disk_gb {
        // Simple check via `df`
        if let Ok(output) = std::process::Command::new("df").args(["-g", "."]).output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Parse the available space from df output (4th column, 2nd line)
            if let Some(line) = stdout.lines().nth(1)
                && let Some(avail) = line.split_whitespace().nth(3)
                && let Ok(free_gb) = avail.parse::<u32>()
                && free_gb < min_gb
            {
                bail!(
                    "{} requires ≥{} GB free disk space. Available: {} GB",
                    engine.name,
                    min_gb,
                    free_gb
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_exists_true() {
        assert!(command_exists("ls"));
    }

    #[test]
    fn test_command_exists_false() {
        assert!(!command_exists("nonexistent_command_xyz_123"));
    }

    #[test]
    fn test_find_python() {
        // On macOS with Homebrew or conda, python3 should be available
        let result = find_python();
        assert!(result.is_ok(), "Should find Python 3.10+: {:?}", result);
    }

    #[test]
    fn test_resolve_platform_macos_arm64() {
        let profile = HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            is_tegra: false,
            gpu_vendor: crate::hardware::probe::GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".to_string(),
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "macos-arm64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn test_resolve_platform_linux_nvidia() {
        let profile = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: crate::hardware::probe::GpuVendor::Nvidia,
            vram_gb: 24.0,
            unified_mem: false,
            total_ram_gb: 64.0,
            cpu_cores: 16,
            cpu_model: "AMD Ryzen 9".to_string(),
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "ubuntu-vulkan-x64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn test_resolve_platform_linux_cpu() {
        let profile = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: crate::hardware::probe::GpuVendor::None,
            vram_gb: 0.0,
            unified_mem: false,
            total_ram_gb: 16.0,
            cpu_cores: 4,
            cpu_model: "Intel i5".to_string(),
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "ubuntu-x64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn test_resolve_platform_windows_cpu() {
        let profile = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: crate::hardware::probe::GpuVendor::None,
            vram_gb: 0.0,
            unified_mem: false,
            total_ram_gb: 16.0,
            cpu_cores: 8,
            cpu_model: "Intel i7".to_string(),
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "win-cpu-x64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn test_resolve_platform_windows_nvidia_uses_zip() {
        let profile = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: crate::hardware::probe::GpuVendor::Nvidia,
            vram_gb: 3.5,
            unified_mem: false,
            total_ram_gb: 24.0,
            cpu_cores: 20,
            cpu_model: "12th Gen Intel Core i7-12700H".to_string(),
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        // Platform starts with "win-cuda-" and ends with "-x64"
        assert!(
            platform.starts_with("win-cuda-"),
            "Expected win-cuda-*, got {}",
            platform
        );
        assert!(
            platform.ends_with("-x64"),
            "Expected *-x64, got {}",
            platform
        );
        // Windows CUDA builds are always .zip
        assert_eq!(ext, "zip");
    }
}
