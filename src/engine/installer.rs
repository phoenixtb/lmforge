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
    if let Ok(output) = std::process::Command::new("which").arg(cmd).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // Verify version if possible
            if verify_engine_version(engine, &path) {
                return Some(path);
            }
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
    if engine.install_method == "brew" {
        if let Some(ref formula) = engine.brew_formula {
            if let Ok(output) = std::process::Command::new("brew")
                .args(["list", "--versions", formula])
                .output()
            {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    debug!(formula, output = %stdout.trim(), "Brew version check");
                    // Accept any version — the user may have a different version installed
                    return true;
                }
            }
        }
    }

    // For binary engines, just check existence
    true
}

/// Install via Homebrew (primary method for oMLX on macOS)
async fn install_via_brew(
    engine: &EngineConfig,
    data_dir: &std::path::Path,
) -> Result<InstallResult> {
    // Check if brew is available
    if !command_exists("brew") {
        warn!("Homebrew not found, trying pip fallback");
        return install_via_pip(engine, data_dir).await;
    }

    // Tap the repository if needed
    if let Some(ref tap) = engine.brew_tap {
        println!("  ⚙ Adding Homebrew tap: {}", tap);
        let status = tokio::process::Command::new("brew")
            .args(["tap", tap])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .status()
            .await
            .context("Failed to run 'brew tap'")?;

        if !status.success() {
            warn!(tap, "brew tap failed, trying pip fallback");
            return install_via_pip(engine, data_dir).await;
        }
    }

    // Install the formula
    if let Some(ref formula) = engine.brew_formula {
        println!("  ⚙ Installing {} via Homebrew...", formula);
        let output = tokio::process::Command::new("brew")
            .args(["install", formula])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .context("Failed to run 'brew install'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "already installed" is not an error
            if stderr.contains("already installed") {
                info!(formula, "Already installed via Homebrew");
            } else {
                warn!(formula, stderr = %stderr, "brew install failed, trying pip fallback");
                return install_via_pip(engine, data_dir).await;
            }
        }

        // Verify installation
        let cmd = &engine.start_cmd;
        if let Ok(which_out) = std::process::Command::new("which").arg(cmd).output() {
            if which_out.status.success() {
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
        }
    }

    bail!(
        "Homebrew installation of {} failed — formula or command not found",
        engine.id
    );
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

    // Resolve platform string
    let platform = resolve_platform_string(profile)?;
    let asset_name = asset_pattern.replace("{platform}", &platform);
    let download_url = format!("{}/{}", release_url, asset_name);

    println!("  ⚙ Downloading {} from:", engine.name);
    println!("    {}", download_url);

    // Ensure engines directory exists
    let engines_dir = data_dir.join("engines");
    std::fs::create_dir_all(&engines_dir)?;

    let archive_path = engines_dir.join(&asset_name);

    // Download the archive
    download_file(&download_url, &archive_path).await?;

    // Extract the archive
    println!("  ⚙ Extracting...");
    let extract_dir = engines_dir.join(format!("{}-extract", engine.id));
    std::fs::create_dir_all(&extract_dir)?;

    let status = tokio::process::Command::new("tar")
        .args([
            "xzf",
            &archive_path.to_string_lossy(),
            "-C",
            &extract_dir.to_string_lossy(),
        ])
        .status()
        .await
        .context("Failed to extract archive")?;

    if !status.success() {
        bail!("Failed to extract {}", asset_name);
    }

    // Find the binary in the extracted directory
    let found_binary = find_binary_in_dir(&extract_dir, binary_name)?;
    let dest_path = engines_dir.join(binary_name);

    // Copy binary to engines directory
    std::fs::copy(&found_binary, &dest_path)
        .context("Failed to copy binary to engines directory")?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest_path, perms)?;
    }

    // Cleanup
    let _ = std::fs::remove_file(&archive_path);
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

/// Resolve platform string for llama.cpp release assets
fn resolve_platform_string(profile: &HardwareProfile) -> Result<String> {
    let platform = match (profile.os, profile.arch) {
        (Os::Darwin, Arch::Aarch64) => "macos-arm64",
        (Os::Darwin, Arch::X86_64) => "macos-x64",
        (Os::Linux, Arch::X86_64) => {
            if profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia {
                "ubuntu-x64-cuda"
            } else {
                "ubuntu-x64"
            }
        }
        (Os::Linux, Arch::Aarch64) => "ubuntu-arm64",
        (Os::Windows, Arch::X86_64) => "win-x64",
        _ => bail!(
            "No pre-built binary available for {:?} {:?}",
            profile.os,
            profile.arch
        ),
    };
    Ok(platform.to_string())
}

/// Find a binary by name within a directory (recursive)
fn find_binary_in_dir(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    for entry in walkdir(dir)? {
        if let Some(fname) = entry.file_name().and_then(|n| n.to_str()) {
            if fname == name {
                return Ok(entry);
            }
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
        {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout);
                debug!(python = candidate, version = %version.trim(), "Found Python");
                // Check version >= 3.10
                if let Some(ver_str) = version.split_whitespace().nth(1) {
                    let parts: Vec<&str> = ver_str.split('.').collect();
                    if parts.len() >= 2 {
                        if let (Ok(major), Ok(minor)) =
                            (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                        {
                            if major >= 3 && minor >= 10 {
                                return Ok(candidate.to_string());
                            }
                        }
                    }
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
        if let Ok(output) = std::process::Command::new("df").args(["-g", "."]).output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Parse the available space from df output (4th column, 2nd line)
                if let Some(line) = stdout.lines().nth(1) {
                    if let Some(avail) = line.split_whitespace().nth(3) {
                        if let Ok(free_gb) = avail.parse::<u32>() {
                            if free_gb < min_gb {
                                bail!(
                                    "{} requires ≥{} GB free disk space. Available: {} GB",
                                    engine.name,
                                    min_gb,
                                    free_gb
                                );
                            }
                        }
                    }
                }
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
        assert_eq!(resolve_platform_string(&profile).unwrap(), "macos-arm64");
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
        assert_eq!(
            resolve_platform_string(&profile).unwrap(),
            "ubuntu-x64-cuda"
        );
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
        assert_eq!(resolve_platform_string(&profile).unwrap(), "ubuntu-x64");
    }
}
