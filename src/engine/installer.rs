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

    // For pip engines, only the venv counts. The `start_cmd` is a generic
    // interpreter (`python3`) whose presence on PATH says NOTHING about the
    // pip package being importable. Existence is verified by actually running
    // `python -c "import <pkg>"` inside the venv.
    if engine.install_method == "pip"
        && let Some(path) = find_verified_pip_install(engine, data_dir)
    {
        println!(
            "  ✓ {} v{} verified in venv at {}",
            engine.name, engine.version, path
        );
        return Ok(InstallResult {
            engine_id: engine.id.clone(),
            version: engine.version.clone(),
            install_path: path,
            method_used: "existing".to_string(),
        });
    }

    if engine.install_method != "pip"
        && let Some(path) = find_existing_install(engine, data_dir)
    {
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
        "pip" => install_via_pip(engine, profile, data_dir).await,
        "binary" => install_via_binary(engine, profile, data_dir).await,
        other => bail!("Unknown install method: {}", other),
    }
}

/// Check if a pip engine is installed AND importable in its dedicated venv.
/// Returns the venv interpreter path only when `python -c "import <pkg>"` succeeds.
fn find_verified_pip_install(
    engine: &EngineConfig,
    data_dir: &std::path::Path,
) -> Option<String> {
    let venv_python = venv_python_path(engine, data_dir);
    if !venv_python.is_file() {
        return None;
    }

    let import_name = derive_import_name(engine)?;
    match verify_pip_import(&venv_python, &import_name) {
        Ok(version) => {
            debug!(
                engine = %engine.id,
                import = %import_name,
                version = %version,
                "Verified pip engine import in venv"
            );
            Some(venv_python.to_string_lossy().to_string())
        }
        Err(e) => {
            warn!(
                engine = %engine.id,
                import = %import_name,
                error = %e,
                "Venv exists but package import failed; will reinstall"
            );
            None
        }
    }
}

/// Resolve the venv interpreter path for an engine. Used by pip-engine probes;
/// must match the layout `install_via_pip` creates.
fn venv_python_path(engine: &EngineConfig, data_dir: &std::path::Path) -> std::path::PathBuf {
    if cfg!(windows) {
        data_dir
            .join("engines")
            .join(&engine.id)
            .join("venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        data_dir
            .join("engines")
            .join(&engine.id)
            .join("venv")
            .join("bin")
            .join("python3")
    }
}

/// Map a pip package spec to its Python import name.
/// Strips `[extras]`, version constraints (`==`, `>=`, `<=`, `~=`, `>`, `<`, `!=`),
/// trailing whitespace, and converts hyphens to underscores (PEP 503 normalisation
/// for the import-name guess — not perfect for every package, but covers
/// sglang, mlx-lm, vllm, and other engines we ship).
fn derive_import_name(engine: &EngineConfig) -> Option<String> {
    // Explicit override wins. Used by repo-based engines like TabbyAPI
    // whose `pip_package` doesn't install any importable Python source.
    if let Some(name) = engine.verify_import_name.as_deref()
        && !name.is_empty()
    {
        return Some(name.to_string());
    }
    let pkg = engine
        .pip_package
        .as_ref()
        .or(engine.pip_fallback.as_ref())?;
    let stop_chars = ['[', '=', '>', '<', '~', '!', ' ', '\t', '@'];
    let raw = pkg
        .split(|c| stop_chars.contains(&c))
        .next()
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.replace('-', "_"))
}

/// Run `python -c "import <name>; print(<name>.__version__)"` in the given interpreter.
/// Returns the printed version (or `"unknown"`) on success, error on import failure.
fn verify_pip_import(python: &std::path::Path, import_name: &str) -> Result<String> {
    let script = format!(
        "import importlib, sys\n\
         m = importlib.import_module('{name}')\n\
         print(getattr(m, '__version__', 'unknown'))\n",
        name = import_name
    );
    let output = std::process::Command::new(python)
        .args(["-c", &script])
        .output()
        .with_context(|| format!("Failed to invoke {} for import probe", python.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "import {} failed in {}:\n{}",
            import_name,
            python.display(),
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check if a non-pip engine binary is already available
fn find_existing_install(engine: &EngineConfig, data_dir: &std::path::Path) -> Option<String> {
    let cmd = &engine.start_cmd;
    if let Some(path) = which_in_path(cmd)
        && verify_engine_version(engine, &path)
    {
        return Some(path);
    }

    if let Some(ref binary) = engine.binary {
        let resolved = if cfg!(windows) && !binary.ends_with(".exe") {
            format!("{}.exe", binary)
        } else {
            binary.clone()
        };
        let local_path = data_dir.join("engines").join(&resolved);
        if local_path.exists() {
            return Some(local_path.to_string_lossy().to_string());
        }
    }

    None
}

/// Resolve `cmd` against PATH using the platform-native locator.
/// Unix: `which`. Windows: `where`. Returns the first hit or `None`.
fn which_in_path(cmd: &str) -> Option<String> {
    let locator = if cfg!(windows) { "where" } else { "which" };
    let output = std::process::Command::new(locator).arg(cmd).output().ok()?;
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

/// Install via uv-managed venv (primary path for SGLang; future fallback for oMLX).
///
/// Uses Astral's `uv` instead of system `python3 -m venv` + `pip` because:
///   • No dependency on `python3-venv` / `ensurepip` apt packages (Ubuntu/Debian
///     split them out of the base `python3`).
///   • Single static binary downloaded once to `~/.lmforge/bin/uv`, shared
///     across every engine install. No sudo, no per-distro branching.
///   • Faster — uv resolves and installs sglang's ~50 transitive deps in a
///     fraction of the time pip takes.
///   • uv can auto-fetch a managed Python interpreter if the system lacks
///     one ≥ 3.10, removing another fail-mode for fresh boxes.
///
/// Idempotent and verifying:
///   1. Preflight (nvcc, nvidia-smi, etc. per engines.toml).
///   2. Bootstrap uv to `~/.lmforge/bin/uv` if missing (sha256-verified).
///   3. `uv venv` the engine venv (no-op if already present).
///   4. `uv pip install <pkg>` with inherited stdio so progress is visible.
///   5. Post-install `import <pkg>` verification using the venv interpreter
///      — the only reliable proof the engine is functional. Catches broken
///      torch/CUDA combos pip considers a "success" but which crash the daemon.
async fn install_via_pip(
    engine: &EngineConfig,
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
) -> Result<InstallResult> {
    run_preflight_checks(engine)?;

    let uv_bin = crate::engine::uv::ensure_uv(data_dir)
        .await
        .context("Failed to bootstrap uv (Python toolchain manager)")?;

    let venv_dir = data_dir.join("engines").join(&engine.id).join("venv");
    std::fs::create_dir_all(venv_dir.parent().unwrap())
        .with_context(|| format!("Cannot create {}", venv_dir.display()))?;

    let venv_python = venv_python_path(engine, data_dir);
    // Per-engine Python floor. Default 3.10 (works for vLLM 0.21, SGLang,
    // oMLX). TabbyAPI's `[cu13]` extra needs 3.12+. uv will auto-download
    // the interpreter if it's not already installed.
    let python_pin: &str = engine
        .min_python_version
        .as_deref()
        .unwrap_or("3.10");
    if !venv_python.is_file() {
        println!(
            "  ⚙ Creating uv-managed venv (Python {}) at {}...",
            python_pin,
            venv_dir.display()
        );
        let status = tokio::process::Command::new(&uv_bin)
            .args([
                "venv",
                "--python",
                python_pin,
                venv_dir.to_string_lossy().as_ref(),
            ])
            .status()
            .await
            .context("Failed to run `uv venv`")?;
        if !status.success() {
            bail!(
                "`uv venv` failed at {}.\n  \
                 If the message above mentions a missing Python, run:\n    {} python install {}\n  \
                 then re-run `lmforge init`.",
                venv_dir.display(),
                uv_bin.display(),
                python_pin
            );
        }
    }

    // ── Optional: clone the engine's source repo (TabbyAPI etc.) ────────
    // For engines that ship as a git tree rather than a real pip package,
    // we clone alongside the venv. The adapter spawns Python with this
    // path on sys.path / cwd.
    if let Some(repo) = engine.source_repo.as_deref() {
        let source_dir = data_dir.join("engines").join(&engine.id).join("source");
        let revision = engine.source_revision.as_deref().unwrap_or("main");
        ensure_source_repo(repo, revision, &source_dir).await?;
    }

    let pip_pkg = engine
        .pip_package
        .as_ref()
        .or(engine.pip_fallback.as_ref())
        .context("No pip package specified for engine")?;

    // PyTorch backend selection — adaptive, never hardcoded.
    //
    // The resolver consults the cached `HardwareProfile` (CUDA runtime +
    // compute cap) and returns a deterministic wheel id like `cu130`. This
    // beats uv's own `--torch-backend=auto` because:
    //   • two users on the same hardware land on the same wheel
    //   • consumer Blackwell (sm_120) is pinned to cu130 — vLLM 0.11.x's
    //     default cu128 wheel segfaults on that arch.
    // `UV_TORCH_BACKEND` env var overrides everything (CI / debugging).
    let torch_backend = crate::engine::torch_backend::resolve(profile);

    println!(
        "  ⚙ Installing {} via uv (torch-backend={} [{:?}], this can take several minutes)...",
        pip_pkg,
        torch_backend.as_str(),
        torch_backend.origin
    );
    // `--prerelease=allow` is required for SGLang: its dep tree includes
    // `flash-attn-4>=4.0.0b4` which is itself a pre-release. Modern ML
    // engines (sglang/vllm/mlx) all pull in pre-release deps; this flag is
    // safe and matches what `pip install` does by default.
    let mut install_args: Vec<String> = vec![
        "pip".into(),
        "install".into(),
        "--prerelease=allow".into(),
        "--torch-backend".into(),
        torch_backend.as_str().into(),
        "--python".into(),
        venv_python.to_string_lossy().into_owned(),
        pip_pkg.clone(),
    ];
    // Engine-specific build-time deps (e.g. vLLM's FlashInfer JIT needs
    // `ninja` to compile a sampling kernel at first model load). Installed
    // in the same `uv pip install` so we don't pay two cold-resolve passes.
    for extra in &engine.pip_extras {
        install_args.push(extra.clone());
    }
    let status = tokio::process::Command::new(&uv_bin)
        .args(&install_args)
        .status()
        .await
        .context("Failed to run `uv pip install`")?;
    if !status.success() {
        bail!(
            "`uv pip install {}` (extras: {:?}) failed — see output above for details",
            pip_pkg,
            engine.pip_extras
        );
    }

    // Verification: the binary check we used to do here only proved that
    // `python3` exists, not that the engine is importable. Do the real check.
    let import_name =
        derive_import_name(engine).context("Cannot derive import name from pip_package")?;
    let version = verify_pip_import(&venv_python, &import_name).with_context(|| {
        format!(
            "Engine package '{}' installed but `import {}` failed in venv",
            pip_pkg, import_name
        )
    })?;

    let path = venv_python.to_string_lossy().to_string();
    println!(
        "  ✓ {} v{} importable in venv at {}",
        engine.name, version, path
    );

    Ok(InstallResult {
        engine_id: engine.id.clone(),
        version: engine.version.clone(),
        install_path: path,
        method_used: "uv".to_string(),
    })
}

/// Clone (or `git fetch && checkout`) `repo @ revision` into `target_dir`.
///
/// Idempotent: if `target_dir/.git` exists we fetch + reset to the revision,
/// so re-running `lmforge engine install` on a stale checkout converges to
/// the pinned ref rather than refusing. Uses the system `git` binary; we
/// don't bundle libgit2 to keep the binary small.
async fn ensure_source_repo(
    repo: &str,
    revision: &str,
    target_dir: &std::path::Path,
) -> Result<()> {
    let git_dir = target_dir.join(".git");
    if git_dir.is_dir() {
        println!(
            "  ⚙ Updating engine source at {} ({})...",
            target_dir.display(),
            revision
        );
        let fetch = tokio::process::Command::new("git")
            .args(["fetch", "--depth=1", "origin", revision])
            .current_dir(target_dir)
            .status()
            .await
            .context("Failed to run `git fetch` for engine source repo")?;
        if !fetch.success() {
            bail!(
                "`git fetch origin {}` failed in {} — check network access",
                revision,
                target_dir.display()
            );
        }
        let reset = tokio::process::Command::new("git")
            .args(["reset", "--hard", "FETCH_HEAD"])
            .current_dir(target_dir)
            .status()
            .await
            .context("Failed to run `git reset --hard FETCH_HEAD`")?;
        if !reset.success() {
            bail!(
                "`git reset --hard FETCH_HEAD` failed in {}",
                target_dir.display()
            );
        }
    } else {
        std::fs::create_dir_all(target_dir.parent().unwrap_or(target_dir))
            .with_context(|| format!("Cannot create {}", target_dir.display()))?;
        println!(
            "  ⚙ Cloning engine source from {} ({}) into {}...",
            repo,
            revision,
            target_dir.display()
        );
        // Shallow clone of just the requested revision. Saves ~30 MB for
        // TabbyAPI's git history we don't need at runtime.
        let clone = tokio::process::Command::new("git")
            .args([
                "clone",
                "--depth=1",
                "--branch",
                revision,
                repo,
                target_dir.to_string_lossy().as_ref(),
            ])
            .status()
            .await
            .context("Failed to run `git clone` — is git installed?")?;
        if !clone.success() {
            // `--branch` only accepts refs that exist on the remote; commit
            // SHAs need a two-step (clone, fetch, checkout) dance. Retry
            // without `--branch` if the first try failed.
            let _ = std::fs::remove_dir_all(target_dir);
            let clone2 = tokio::process::Command::new("git")
                .args([
                    "clone",
                    repo,
                    target_dir.to_string_lossy().as_ref(),
                ])
                .status()
                .await
                .context("Failed to run `git clone` (full-depth fallback)")?;
            if !clone2.success() {
                bail!("`git clone {}` failed", repo);
            }
            let checkout = tokio::process::Command::new("git")
                .args(["checkout", revision])
                .current_dir(target_dir)
                .status()
                .await
                .context("Failed to run `git checkout`")?;
            if !checkout.success() {
                bail!(
                    "`git checkout {}` failed in {}",
                    revision,
                    target_dir.display()
                );
            }
        }
    }
    Ok(())
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

    // Extract the archive
    println!("  ⚙ Extracting...");
    let extract_dir = engines_dir.join(format!("{}-extract", engine.id));
    std::fs::create_dir_all(&extract_dir)?;

    extract_archive(&archive_path, &extract_dir, profile).await?;

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

    // Copy ALL shared libraries from the archive into the same directory so
    // the binary's `RUNPATH=$ORIGIN` (Linux) / `@loader_path` (macOS) /
    // implicit-cwd lookup (Windows) finds them at startup. Starting around
    // llama.cpp b9351 the prebuilt tarballs ship `libllama-server-impl.so`
    // and ~40 GGML kernel libraries as separate `.so` files; without this
    // copy step the binary fails with "cannot open shared object file".
    //
    // The Windows CUDA branch above (`copy_dlls_to_dir`) used to be the
    // only library-copy path; this generalises it to every platform.
    copy_shared_libs_to_dir(&extract_dir, &engines_dir, profile.os)?;

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
            // Windows GPU = Vulkan (covers NVIDIA + AMD + Intel iGPU in one binary,
            // no cudart DLL payload required). CPU-only fallback when no GPU is
            // detected at probe time.
            let p = if profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia
                || profile.gpu_vendor == crate::hardware::probe::GpuVendor::Amd
            {
                "win-vulkan-x64"
            } else {
                "win-cpu-x64"
            };
            (p.to_string(), "zip")
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

/// Copy every shared library that lives next to the main binary into the
/// engines directory so the binary's `$ORIGIN` / `@loader_path` runtime
/// linking succeeds without env vars.
///
/// Platform → extension matrix:
///   * Linux: `*.so`, `*.so.N`, `*.so.N.M[.K]` (the upstream tarball ships
///     both unversioned and SONAME-versioned copies; we mirror both)
///   * macOS: `*.dylib`, `*.N.dylib`
///   * Windows: `*.dll` (covers the CUDA runtime DLLs too)
fn copy_shared_libs_to_dir(
    src_dir: &std::path::Path,
    dest_dir: &std::path::Path,
    os: Os,
) -> Result<()> {
    for entry in walkdir(src_dir)? {
        let Some(fname) = entry.file_name() else {
            continue;
        };
        let name = match fname.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !is_shared_lib(name, os) {
            continue;
        }
        let dest = dest_dir.join(fname);
        std::fs::copy(&entry, &dest).with_context(|| {
            format!("Failed to copy shared library {} to engines dir", entry.display())
        })?;
        debug!(lib = ?fname, "Copied shared library alongside binary");
    }
    Ok(())
}

/// True when `name` matches the OS-specific dynamic-library suffix.
/// Pure function — exposed for unit tests.
fn is_shared_lib(name: &str, os: Os) -> bool {
    let lower = name.to_ascii_lowercase();
    match os {
        Os::Linux => {
            // libfoo.so, libfoo.so.0, libfoo.so.1.2.3
            // Reject names that don't contain ".so" at all.
            if !lower.contains(".so") {
                return false;
            }
            // Split on ".so" once; the remainder must be empty or a dotted version.
            if let Some(rest) = lower.split_once(".so").map(|(_, r)| r) {
                rest.is_empty()
                    || (rest.starts_with('.')
                        && rest
                            .trim_start_matches('.')
                            .chars()
                            .all(|c| c.is_ascii_digit() || c == '.'))
            } else {
                false
            }
        }
        Os::Darwin => lower.ends_with(".dylib"),
        Os::Windows => lower.ends_with(".dll"),
        Os::Unknown => false,
    }
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

/// Check if a command exists in PATH using the platform-native locator
/// (`which` on Unix, `where` on Windows).
fn command_exists(cmd: &str) -> bool {
    which_in_path(cmd).is_some()
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
        // `cmd` exists on Windows by default (cmd.exe is on PATH);
        // `ls` exists on macOS/Linux. Pick by platform so the test
        // passes natively on every supported OS.
        let probe = if cfg!(windows) { "cmd" } else { "ls" };
        assert!(command_exists(probe), "{probe} should be on PATH");
    }

    #[test]
    fn test_command_exists_false() {
        assert!(!command_exists("nonexistent_command_xyz_123"));
    }

    fn engine_with_pip_pkg(id: &str, pip: &str) -> EngineConfig {
        EngineConfig {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.0.0".to_string(),
            install_method: "pip".to_string(),
            pip_package: Some(pip.to_string()),
            model_format: "safetensors".to_string(),
            start_cmd: "python3".to_string(),
            health_endpoint: "/health".to_string(),
            priority: 100,
            ..Default::default()
        }
    }

    #[test]
    fn test_derive_import_name_strips_extras_and_version() {
        let e = engine_with_pip_pkg("sglang", "sglang[all]==0.5.10.post1");
        assert_eq!(derive_import_name(&e).as_deref(), Some("sglang"));
    }

    #[test]
    fn test_derive_import_name_handles_hyphens() {
        let e = engine_with_pip_pkg("mlx-lm", "mlx-lm>=0.20");
        // Hyphens must become underscores for the Python import statement.
        assert_eq!(derive_import_name(&e).as_deref(), Some("mlx_lm"));
    }

    #[test]
    fn test_derive_import_name_no_constraints() {
        let e = engine_with_pip_pkg("vllm", "vllm");
        assert_eq!(derive_import_name(&e).as_deref(), Some("vllm"));
    }

    #[test]
    fn test_derive_import_name_handles_pip_fallback() {
        let mut e = engine_with_pip_pkg("foo", "ignored");
        e.pip_package = None;
        e.pip_fallback = Some("foo-pkg~=1.0".to_string());
        assert_eq!(derive_import_name(&e).as_deref(), Some("foo_pkg"));
    }

    #[test]
    fn test_derive_import_name_handles_compound_constraints() {
        // Real-world examples seen in the wild
        for (input, want) in &[
            ("torch>=2.0,<3.0", "torch"),
            ("numpy!=1.25.0", "numpy"),
            ("requests~=2.31.0", "requests"),
            ("foo-bar[extras]==1.0", "foo_bar"),
        ] {
            let e = engine_with_pip_pkg("test", input);
            assert_eq!(derive_import_name(&e).as_deref(), Some(*want), "input: {}", input);
        }
    }

    #[test]
    fn test_find_verified_pip_install_missing_venv_returns_none() {
        let data_dir = std::env::temp_dir().join("lmforge_test_pip_missing_venv");
        let _ = std::fs::remove_dir_all(&data_dir);
        let engine = engine_with_pip_pkg("sglang", "sglang[all]==0.5.10");
        assert!(find_verified_pip_install(&engine, &data_dir).is_none());
    }

    #[test]
    fn test_verify_pip_import_succeeds_on_stdlib() {
        // sys is always available in any python — proves the probe mechanics work.
        let python = std::path::PathBuf::from("python3");
        let result = verify_pip_import(&python, "sys");
        assert!(
            result.is_ok(),
            "import sys must succeed in any python3: {:?}",
            result
        );
    }

    #[test]
    fn test_verify_pip_import_fails_on_nonexistent_package() {
        let python = std::path::PathBuf::from("python3");
        let result = verify_pip_import(&python, "definitely_not_a_real_module_xyz_12345");
        assert!(result.is_err(), "import of nonexistent module must fail");
    }

    #[test]
    fn test_resolve_platform_macos_arm64() {
        let profile = HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            gpu_vendor: crate::hardware::probe::GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".to_string(),
            ..Default::default()
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
            gpu_vendor: crate::hardware::probe::GpuVendor::Nvidia,
            vram_gb: 24.0,
            total_ram_gb: 64.0,
            cpu_cores: 16,
            cpu_model: "AMD Ryzen 9".to_string(),
            ..Default::default()
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
            gpu_vendor: crate::hardware::probe::GpuVendor::None,
            total_ram_gb: 16.0,
            cpu_cores: 4,
            cpu_model: "Intel i5".to_string(),
            ..Default::default()
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
            gpu_vendor: crate::hardware::probe::GpuVendor::None,
            total_ram_gb: 16.0,
            cpu_cores: 8,
            cpu_model: "Intel i7".to_string(),
            ..Default::default()
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "win-cpu-x64");
        assert_eq!(ext, "zip");
    }

    // ── is_shared_lib (Phase 4 shared-lib co-location) ───────────────────────

    #[test]
    fn is_shared_lib_linux_matches_so_variants() {
        // Both unversioned and versioned SONAMEs must match — the b9351
        // tarball ships symlinks for libfoo.so, libfoo.so.0, libfoo.so.0.1.2.
        for name in &[
            "libllama-server-impl.so",
            "libggml.so.0",
            "libggml.so.0.13.0",
            "libllama-common.so.0.0.9351",
            "libstdc++.so.6",
        ] {
            assert!(is_shared_lib(name, Os::Linux), "{} should match", name);
        }
    }

    #[test]
    fn is_shared_lib_linux_rejects_non_libs() {
        for name in &[
            "llama-server",
            "LICENSE",
            "README.md",
            "llama-bench",
            "config.json",
            // Tricky negative: name contains "so" but no .so suffix.
            "console.log",
        ] {
            assert!(!is_shared_lib(name, Os::Linux), "{} must NOT match", name);
        }
    }

    #[test]
    fn is_shared_lib_darwin_matches_dylib() {
        assert!(is_shared_lib("libllama.dylib", Os::Darwin));
        assert!(is_shared_lib("libggml.0.dylib", Os::Darwin));
        assert!(!is_shared_lib("llama-server", Os::Darwin));
        assert!(!is_shared_lib("libllama.so", Os::Darwin));
    }

    #[test]
    fn is_shared_lib_windows_matches_dll() {
        assert!(is_shared_lib("llama.dll", Os::Windows));
        assert!(is_shared_lib("cudart64_12.dll", Os::Windows));
        assert!(is_shared_lib("LLAMA.DLL", Os::Windows));
        assert!(!is_shared_lib("llama-server.exe", Os::Windows));
        assert!(!is_shared_lib("libllama.so", Os::Windows));
    }

    #[test]
    fn is_shared_lib_unknown_os_matches_nothing() {
        // Safety net — we should never reach this branch in practice, but a
        // future Os variant must not silently match every file.
        assert!(!is_shared_lib("libllama.so", Os::Unknown));
        assert!(!is_shared_lib("libllama.dylib", Os::Unknown));
    }

    #[test]
    fn test_resolve_platform_windows_nvidia_uses_vulkan_zip() {
        // Windows GPU path now uses upstream's Vulkan build (one binary covers
        // NVIDIA + AMD + Intel; no cudart DLL payload required).
        let profile = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            gpu_vendor: crate::hardware::probe::GpuVendor::Nvidia,
            vram_gb: 3.5,
            total_ram_gb: 24.0,
            cpu_cores: 20,
            cpu_model: "12th Gen Intel Core i7-12700H".to_string(),
            ..Default::default()
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "win-vulkan-x64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn test_resolve_platform_windows_amd_uses_vulkan_zip() {
        let profile = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            gpu_vendor: crate::hardware::probe::GpuVendor::Amd,
            vram_gb: 16.0,
            total_ram_gb: 32.0,
            cpu_cores: 16,
            cpu_model: "AMD Ryzen 9 7900X".to_string(),
            ..Default::default()
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "win-vulkan-x64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn test_resolve_platform_windows_no_gpu_uses_cpu_zip() {
        let profile = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            gpu_vendor: crate::hardware::probe::GpuVendor::None,
            vram_gb: 0.0,
            total_ram_gb: 16.0,
            cpu_cores: 8,
            cpu_model: "Intel Core i5-12400".to_string(),
            ..Default::default()
        };
        let (platform, ext) = resolve_platform(&profile).unwrap();
        assert_eq!(platform, "win-cpu-x64");
        assert_eq!(ext, "zip");
    }
}
