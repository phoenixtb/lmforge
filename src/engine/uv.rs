//! uv bootstrap — vendored Python toolchain manager.
//!
//! `uv` is Astral's static Rust binary that replaces pip/venv/pyenv for the
//! Python side of LMForge engines (currently SGLang on Linux+NVIDIA, future
//! mlx-lm fallback). Using uv eliminates the dependency on system
//! `python3-venv` / `ensurepip` / per-distro Python packaging quirks that
//! make `python3 -m venv` unreliable on fresh Debian/Ubuntu/RHEL boxes.
//!
//! The binary is downloaded once on first `lmforge init` to
//! `~/.lmforge/bin/uv`, verified via the `.sha256` companion file published
//! alongside each GitHub release, and reused for every subsequent engine
//! install. Uninstall is a single `rm -rf ~/.lmforge`.
//!
//! Pinning policy: a single `UV_VERSION` constant. Bumping requires a
//! deliberate code change so the version cannot drift via "latest" tags.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Pinned uv release. Update intentionally; release notes:
/// https://github.com/astral-sh/uv/releases
pub const UV_VERSION: &str = "0.11.16";

/// Resolve the rust-style target triple for the current platform.
/// Returns `(triple, archive_ext, binary_name)`.
fn current_target() -> Result<(&'static str, &'static str, &'static str)> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        (os, arch) => bail!(
            "No uv binary available for {} {}. \
             Manually install uv from https://astral.sh/uv and place it at ~/.lmforge/bin/uv.",
            os,
            arch
        ),
    };
    let (ext, bin) = if cfg!(windows) {
        ("zip", "uv.exe")
    } else {
        ("tar.gz", "uv")
    };
    Ok((triple, ext, bin))
}

/// Ensure `uv` is available; download + checksum-verify if missing or outdated.
/// Returns the absolute path to the uv executable.
///
/// Idempotent: a present-and-working uv at the expected version is a no-op.
/// Re-uses the cached binary across all engine installs.
pub async fn ensure_uv(data_dir: &Path) -> Result<PathBuf> {
    let (target, ext, bin_name) = current_target()?;
    let bin_dir = data_dir.join("bin");
    let uv_path = bin_dir.join(bin_name);

    if uv_path.is_file() {
        if let Ok(version) = run_uv_version(&uv_path) {
            if version.contains(UV_VERSION) {
                debug!(path = %uv_path.display(), version = %version.trim(), "uv already installed");
                return Ok(uv_path);
            }
            warn!(
                installed = %version.trim(),
                want = %UV_VERSION,
                "Cached uv version differs from pinned; re-downloading"
            );
        } else {
            warn!(path = %uv_path.display(), "Cached uv exists but `uv --version` failed; re-downloading");
        }
    }

    std::fs::create_dir_all(&bin_dir)
        .with_context(|| format!("Cannot create {}", bin_dir.display()))?;

    let archive_name = format!("uv-{}.{}", target, ext);
    let base_url = format!(
        "https://github.com/astral-sh/uv/releases/download/{}",
        UV_VERSION
    );
    let archive_url = format!("{}/{}", base_url, archive_name);
    let sha_url = format!("{}.sha256", archive_url);

    println!("  ⚙ Downloading uv {} ({})...", UV_VERSION, target);
    debug!(url = %archive_url, "Fetching uv archive");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    // Download companion checksum first so we know what to expect.
    let expected_hash = fetch_expected_sha256(&client, &sha_url, &archive_name).await?;
    debug!(hash = %expected_hash, "Got expected sha256");

    // Stream the archive into a temp file (avoids holding 25 MB in memory).
    let tmp_dir = bin_dir.join(".uv-download-tmp");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)?;
    let archive_path = tmp_dir.join(&archive_name);

    download_and_hash(&client, &archive_url, &archive_path, &expected_hash).await?;

    // Extract — uv archives contain a single top-level directory with `uv`/`uvx`.
    println!("  ⚙ Extracting uv to {}...", bin_dir.display());
    extract_uv(&archive_path, &tmp_dir, bin_name)?;

    // Move the binary into place (atomic rename within same filesystem).
    let extracted_bin = locate_extracted_bin(&tmp_dir, bin_name)?;
    if uv_path.exists() {
        std::fs::remove_file(&uv_path).ok();
    }
    std::fs::rename(&extracted_bin, &uv_path).with_context(|| {
        format!(
            "Cannot move uv from {} to {}",
            extracted_bin.display(),
            uv_path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&uv_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&uv_path, perms)?;
    }

    // Best-effort cleanup of the temp dir.
    let _ = std::fs::remove_dir_all(&tmp_dir);

    // Sanity-verify by actually invoking the binary.
    let version = run_uv_version(&uv_path)
        .context("uv was downloaded and extracted, but `uv --version` failed")?;
    if !version.contains(UV_VERSION) {
        bail!(
            "Downloaded uv reports version '{}' but expected '{}'",
            version.trim(),
            UV_VERSION
        );
    }

    // `uv --version` prints "uv X.Y.Z (target)" — strip the leading "uv " so
    // our log doesn't read "uv uv 0.11.16 ...".
    let pretty = version.trim().strip_prefix("uv ").unwrap_or(version.trim());
    info!(path = %uv_path.display(), version = %pretty, "uv installed");
    println!("  ✓ uv {} ready at {}", pretty, uv_path.display());
    Ok(uv_path)
}

/// Fetch the `<archive>.sha256` companion file and parse out the expected hex digest.
/// The format is the same as `sha256sum`: `<64-hex-chars>  <filename>\n`.
async fn fetch_expected_sha256(
    client: &reqwest::Client,
    sha_url: &str,
    archive_name: &str,
) -> Result<String> {
    let resp = client
        .get(sha_url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch uv checksum from {}", sha_url))?;
    if !resp.status().is_success() {
        bail!("Checksum file fetch returned HTTP {}", resp.status());
    }
    let text = resp.text().await?;
    let line = text
        .lines()
        .next()
        .context("Empty .sha256 file from upstream")?;
    let hash = line
        .split_whitespace()
        .next()
        .context("Malformed .sha256 line")?
        .to_lowercase();
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "Bad sha256 in .sha256 file for {}: '{}'",
            archive_name,
            hash
        );
    }
    Ok(hash)
}

/// Stream the archive into `dest`, computing sha256 as we go.
/// Errors out if the computed hash mismatches `expected_hex`.
async fn download_and_hash(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_hex: &str,
) -> Result<()> {
    use futures::StreamExt;

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to start uv download from {}", url))?;
    if !resp.status().is_success() {
        bail!("Download failed: HTTP {}", resp.status());
    }
    let total = resp.content_length().unwrap_or(0);

    let pb = indicatif::ProgressBar::new(total);
    pb.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("    [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("█▓░"),
    );

    let mut file =
        std::fs::File::create(dest).with_context(|| format!("Cannot create {}", dest.display()))?;
    let mut stream = resp.bytes_stream();
    let mut hasher = Sha256::new();
    let mut got: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("Network error mid-download")?;
        hasher.update(&bytes);
        file.write_all(&bytes)?;
        got += bytes.len() as u64;
        pb.set_position(got);
    }
    pb.finish_and_clear();

    let actual_hex = format!("{:x}", hasher.finalize());
    if actual_hex != expected_hex {
        let _ = std::fs::remove_file(dest);
        bail!(
            "Checksum mismatch for uv archive — refusing to install.\n  \
             expected: {}\n  actual:   {}",
            expected_hex,
            actual_hex
        );
    }
    debug!(hex = %actual_hex, bytes = got, "uv archive integrity verified");
    Ok(())
}

/// Extract uv archive into `dest_dir`. Supports `.tar.gz` (Unix) and `.zip` (Windows).
fn extract_uv(archive: &Path, dest_dir: &Path, _bin_name: &str) -> Result<()> {
    let archive_str = archive.to_string_lossy().to_string();
    let dest_str = dest_dir.to_string_lossy().to_string();

    if cfg!(windows) {
        let status = crate::util::subprocess::hidden("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                    archive_str, dest_str
                ),
            ])
            .status()
            .context("Failed to run PowerShell Expand-Archive for uv")?;
        if !status.success() {
            bail!("Failed to extract uv zip at {}", archive_str);
        }
    } else {
        let status = std::process::Command::new("tar")
            .args(["xzf", &archive_str, "-C", &dest_str])
            .status()
            .context("Failed to extract uv tarball")?;
        if !status.success() {
            bail!("Failed to extract uv tarball at {}", archive_str);
        }
    }
    Ok(())
}

/// Recursively walk `dir` looking for the uv binary.
fn locate_extracted_bin(dir: &Path, bin_name: &str) -> Result<PathBuf> {
    fn walk(dir: &Path, name: &str, hits: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                walk(&p, name, hits)?;
            } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
                hits.push(p);
            }
        }
        Ok(())
    }
    let mut hits = Vec::new();
    walk(dir, bin_name, &mut hits)?;
    hits.into_iter().next().with_context(|| {
        format!(
            "uv binary '{}' not found in extracted archive at {}",
            bin_name,
            dir.display()
        )
    })
}

/// Run `<uv> --version` and return stdout. Used both for cache validation and
/// for post-install sanity verification.
fn run_uv_version(uv_path: &Path) -> Result<String> {
    let output = std::process::Command::new(uv_path)
        .arg("--version")
        .output()
        .with_context(|| format!("Failed to invoke {}", uv_path.display()))?;
    if !output.status.success() {
        bail!("`uv --version` exited non-zero");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_target_returns_known_triple() {
        // On any host CI/dev machine we should resolve to a known triple.
        let result = current_target();
        if cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )) {
            let (triple, ext, bin) = result.expect("known triple required on common platforms");
            assert!(
                triple.contains("linux") || triple.contains("apple") || triple.contains("windows"),
                "unexpected triple: {triple}"
            );
            assert!(ext == "tar.gz" || ext == "zip");
            assert!(bin == "uv" || bin == "uv.exe");
        }
    }

    #[test]
    fn test_uv_version_is_well_formed() {
        // X.Y.Z pinning rule. Any drift to "latest" / branch tags is a bug.
        let parts: Vec<&str> = UV_VERSION.split('.').collect();
        assert_eq!(
            parts.len(),
            3,
            "UV_VERSION must be X.Y.Z, got '{UV_VERSION}'"
        );
        for p in parts {
            assert!(
                p.chars().all(|c| c.is_ascii_digit()),
                "UV_VERSION segment '{p}' must be all digits"
            );
        }
    }
}
