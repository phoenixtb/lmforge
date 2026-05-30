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

    // On Windows + NVIDIA: also pull the matching CUDA-runtime DLL companion
    // archive. Without it, llama-server.exe fails with "cudart64_*.dll not
    // found" at first chat. Only NVIDIA needs this — AMD and Intel iGPUs on
    // Windows route to the Vulkan build via `resolve_platform()`.
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
            println!("  ⚙ Downloading CUDA runtime DLLs ({}):", cuda_variant);
            println!("    {}", cudart_url);
            download_file(&cudart_url, &cudart_path).await?;
            Some(cudart_path)
        } else {
            warn!(
                "engines.toml is missing cudart_pattern for llamacpp — GPU may fail to initialize on Windows NVIDIA"
            );
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

    // On Windows NVIDIA: extract cudart DLLs into the same temp extract dir
    // so the generic copy_shared_libs_to_dir() step below picks them up
    // alongside the upstream .dll companions.
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
///
/// Honours `LMFORGE_LLAMACPP_VARIANT` env override before consulting the
/// hardware profile:
///   - unset / "auto" → probe-based selection (the matrix below)
///   - "gpu"          → force the Vulkan/CUDA build for this OS+arch
///   - "cpu"          → force the CPU-only build
///
/// On Linux/Windows GPU paths, emits a soft warning when the Vulkan loader
/// (`libvulkan.so.1` / `vulkan-1.dll`) is missing — that's a strong signal
/// the user has a GPU but no working driver, which would crash `llama-server`
/// on first chat with a confusing message.
///
/// Exposed `pub(crate)` so `cli::init` can preview the selection (and any
/// preflight warnings) before the download starts.
pub(crate) fn resolve_platform(profile: &HardwareProfile) -> Result<(String, &'static str)> {
    let override_val = std::env::var("LMFORGE_LLAMACPP_VARIANT").ok();
    resolve_platform_with_override(profile, override_val.as_deref())
}

/// Pure version of `resolve_platform` that takes the variant override
/// explicitly. Used by tests so we don't have to mutate `LMFORGE_LLAMACPP_VARIANT`
/// at process scope (which races with other parallel-running tests).
fn resolve_platform_with_override(
    profile: &HardwareProfile,
    variant_override: Option<&str>,
) -> Result<(String, &'static str)> {
    let forced_gpu = variant_override
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "gpu" | "vulkan" | "cuda"));
    let forced_cpu = variant_override.is_some_and(|s| s.eq_ignore_ascii_case("cpu"));

    // Synthesize a profile that respects the override. We only flip GPU vendor —
    // os/arch always come from the real probe.
    let effective_gpu = if forced_cpu {
        crate::hardware::probe::GpuVendor::None
    } else if forced_gpu && profile.gpu_vendor == crate::hardware::probe::GpuVendor::None {
        // User asserts they have a GPU we didn't detect — assume Vulkan-capable.
        // Treating it as AMD routes Windows users to Vulkan (not the cudart-needing
        // CUDA path that would fail without nvidia-smi), which is the safer default.
        crate::hardware::probe::GpuVendor::Amd
    } else {
        profile.gpu_vendor
    };

    let (platform, ext) = match (profile.os, profile.arch) {
        (Os::Darwin, Arch::Aarch64) => ("macos-arm64".to_string(), "tar.gz"),
        (Os::Darwin, Arch::X86_64) => ("macos-x64".to_string(), "tar.gz"),
        (Os::Linux, Arch::X86_64) => {
            // Upstream dropped Linux CUDA prebuilts around b8370. Vulkan is the
            // GPU-accelerated path on Linux — one binary covers NVIDIA + AMD +
            // Intel iGPU through the system's installed Vulkan loader.
            let p = matches!(
                effective_gpu,
                crate::hardware::probe::GpuVendor::Nvidia
                    | crate::hardware::probe::GpuVendor::Amd
                    | crate::hardware::probe::GpuVendor::Intel
            )
            .then_some("ubuntu-vulkan-x64")
            .unwrap_or("ubuntu-x64");
            (p.to_string(), "tar.gz")
        }
        (Os::Linux, Arch::Aarch64) => {
            // Mirror of x86_64: Vulkan if any GPU vendor detected, CPU otherwise.
            // Covers AGX Orin / Jetson Nano (NVIDIA), Rockchip RK3588 with Mali
            // (Vulkan-capable via panfrost), and AWS Graviton CPU-only boxes.
            let p = matches!(
                effective_gpu,
                crate::hardware::probe::GpuVendor::Nvidia
                    | crate::hardware::probe::GpuVendor::Amd
                    | crate::hardware::probe::GpuVendor::Intel
            )
            .then_some("ubuntu-vulkan-arm64")
            .unwrap_or("ubuntu-arm64");
            (p.to_string(), "tar.gz")
        }
        (Os::Windows, Arch::X86_64) => {
            // Windows variant matrix:
            //   NVIDIA  → CUDA build (peak perf; needs cudart-* DLL companion)
            //   AMD     → Vulkan (HIP exists but is heavy + opt-in territory)
            //   Intel   → Vulkan (covers Iris/UHD iGPUs and discrete Arc)
            //   None    → CPU build
            //
            // Vulkan-on-Windows is one binary that runs on AMD + Intel + (older)
            // NVIDIA — we route NVIDIA users to CUDA explicitly because the
            // throughput delta is meaningful (~15–25% on consumer Ada/Blackwell).
            let p = match effective_gpu {
                crate::hardware::probe::GpuVendor::Nvidia => {
                    let cuda_variant = detect_windows_cuda_variant();
                    format!("win-cuda-{}-x64", cuda_variant)
                }
                crate::hardware::probe::GpuVendor::Amd
                | crate::hardware::probe::GpuVendor::Intel => "win-vulkan-x64".to_string(),
                _ => "win-cpu-x64".to_string(),
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

    // Soft preflight: if we picked a Vulkan-based GPU variant but the system
    // Vulkan loader isn't installed, warn loudly. llama-server will otherwise
    // crash on its first chat with "cannot find libvulkan.so.1" / "vulkan-1.dll
    // not found", which is a far worse UX than telling the user up-front.
    // Skip the check when the user explicitly forced GPU via env override —
    // they presumably know what they're doing.
    let is_vulkan_variant = platform.contains("vulkan");
    if is_vulkan_variant && !forced_gpu && !vulkan_loader_available(profile.os) {
        warn!(
            "Selected GPU variant ({}) but no Vulkan loader detected on system. \
             llama-server will fail to initialize at first chat. Install your GPU's \
             vendor driver (NVIDIA proprietary / AMD Mesa / Intel Mesa) before using \
             GPU mode, or set LMFORGE_LLAMACPP_VARIANT=cpu to opt out.",
            platform
        );
    }

    Ok((platform, ext))
}

/// Probe the system for a usable Vulkan loader. Returns false when we're
/// confident there's no Vulkan ICD available; returns true on macOS (which
/// doesn't use the Vulkan path) and on unknown OSes (to avoid false alarms).
fn vulkan_loader_available(os: Os) -> bool {
    match os {
        Os::Linux => {
            // libvulkan.so.1 is the SONAME shipped by every loader (NVIDIA's
            // proprietary, Mesa Vulkan, Intel Mesa-Iris, AMD AMDVLK/RADV).
            // Check the dynamic linker cache via ldconfig; fall back to a
            // couple of common file-system paths if ldconfig isn't available.
            if let Ok(output) = std::process::Command::new("ldconfig").arg("-p").output()
                && let Ok(stdout) = String::from_utf8(output.stdout)
                && stdout.contains("libvulkan.so.1")
            {
                return true;
            }
            for p in &[
                "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
                "/usr/lib64/libvulkan.so.1",
                "/usr/lib/libvulkan.so.1",
                "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
            ] {
                if std::path::Path::new(p).exists() {
                    return true;
                }
            }
            false
        }
        Os::Windows => {
            // vulkan-1.dll lives in System32 when any vendor driver is installed.
            // SystemRoot is set to C:\Windows on every supported Windows release.
            let system_root =
                std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
            std::path::Path::new(&format!("{}\\System32\\vulkan-1.dll", system_root)).exists()
        }
        // Darwin uses Metal via omlx, not Vulkan; never warn for it.
        Os::Darwin | Os::Unknown => true,
    }
}

/// Detect the CUDA runtime version reported by the NVIDIA driver and map it
/// to one of upstream llama.cpp's Windows CUDA variants. Upstream ships two
/// variants at b9351:
///   - `win-cuda-12.4-x64` (CUDA 12.x systems)
///   - `win-cuda-13.1-x64` (CUDA 13.x systems)
///
/// We pick by the driver-reported runtime version. Default = 12.4 (safer
/// floor — older driver releases are far more common on consumer cards).
fn detect_windows_cuda_variant() -> String {
    if let Ok(output) = std::process::Command::new("nvidia-smi").output()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        for line in stdout.lines() {
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

/// Linux `lmforge init` path for `llamacpp`: manifest-driven CUDA install
/// when hardware allows, legacy upstream binary otherwise (Vulkan / CPU).
pub async fn install_llamacpp_on_init(
    engine: &EngineConfig,
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
) -> Result<InstallResult> {
    use crate::engine::variant::init_target_variant;

    let plan = init_target_variant(profile);
    if let Some(ref hint) = plan.hint {
        println!("  ℹ {hint}");
    }

    println!(
        "  Target variant: {} ({})",
        plan.variant,
        if plan.use_manifest {
            "manifest"
        } else {
            "legacy upstream"
        }
    );

    if plan.use_manifest {
        if let Err(reason) = crate::engine::variant::refuse_reason(plan.variant, profile) {
            println!(
                "  ⚠ Cannot install `{}`: {reason}",
                plan.variant
            );
            println!("  ↪ Falling back to legacy Vulkan/CPU binary install...");
            return install_via_binary(engine, profile, data_dir).await;
        }

        let result = install_variant(profile, plan.variant, data_dir).await?;
        println!(
            "  ✓ {} ({}, tag={}) at {}",
            plan.variant,
            engine.name,
            result.llamacpp_tag,
            result.install_dir.display()
        );
        return Ok(InstallResult {
            engine_id: engine.id.clone(),
            version: engine.version.clone(),
            install_path: result.binary_path.to_string_lossy().to_string(),
            method_used: "binary-variant".to_string(),
        });
    }

    install_via_binary(engine, profile, data_dir).await
}

// ── llama.cpp variant installer (C-2.4 / C-2.5) ────────────────────────────────
//
// Installs ONE `llama.cpp` variant tarball from the embedded manifest into
// `<data_dir>/engines/llamacpp/variants/<id>/`. Independent of the legacy
// `install_via_binary` flow (which stages a single binary at
// `<data_dir>/engines/llama-server`) — both layouts coexist until C-3
// consolidates them. Today's call sites:
//   * `lmforge engine install llamacpp --variant cuda12` (CLI / interactive)
//   * `lmforge init` auto-install on Linux NVIDIA (planned in C-3)

/// Result of a variant install. Like [`InstallResult`] but specific to
/// the variant-aware layout — callers can render either a friendly path
/// or a path under the `variants/<id>/` namespace.
#[derive(Debug)]
pub struct VariantInstallResult {
    pub engine_id: String,
    pub variant: crate::engine::variant::LlamaVariant,
    pub install_dir: std::path::PathBuf,
    pub binary_path: std::path::PathBuf,
    pub llamacpp_tag: String,
    pub size_bytes: u64,
}

/// Install one `llama.cpp` variant into the variant-aware layout.
///
/// Idempotency: if `<install_dir>/VERSION` already records the requested
/// `llamacpp_tag`, the function short-circuits and returns Ok with
/// `method_used = "existing"` — safe to call from `lmforge init` on every
/// boot.
pub async fn install_variant(
    profile: &HardwareProfile,
    variant: crate::engine::variant::LlamaVariant,
    data_dir: &std::path::Path,
) -> Result<VariantInstallResult> {
    use crate::engine::variant::Manifest;

    // Hardware gates first — refuse early before touching the network.
    if let Err(reason) = crate::engine::variant::refuse_reason(variant, profile) {
        bail!("Cannot install variant `{variant}`: {reason}");
    }

    let manifest = Manifest::embedded()
        .context("Bundled variants-manifest.json failed to parse — this is a build defect")?;

    let entry = manifest.find(variant.as_str()).with_context(|| {
        format!(
            "Variant `{variant}` is not listed in the bundled variants manifest. \
             Known entries: {}",
            manifest
                .variants
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    if !manifest.is_ready() {
        bail!(
            "Bundled variants-manifest.json still has `<populated-by-ci>` sha256 placeholders. \
             The CUDA build workflow (`.github/workflows/build-llamacpp-cuda.yml`) has not yet been \
             dispatched, or the manifest has not been updated with the published shas. \
             Refusing to download an unverified tarball.\n  \
             Maintainer: dispatch the workflow, then paste each `<tarball>.sha256` value into \
             `data/engines/llamacpp/variants-manifest.json` and rebuild."
        );
    }

    let install_dir = variant_install_dir(data_dir, variant);
    if let Some(installed_tag) = read_installed_tag(&install_dir)
        && installed_tag == manifest.llamacpp_tag
    {
        let binary_path = install_dir.join(variant_binary_name(profile));
        info!(
            engine = "llamacpp",
            variant = %variant,
            tag = %installed_tag,
            path = %install_dir.display(),
            "Variant already installed at requested tag — skipping download"
        );
        return Ok(VariantInstallResult {
            engine_id: "llamacpp".to_string(),
            variant,
            install_dir: install_dir.clone(),
            binary_path,
            llamacpp_tag: installed_tag,
            size_bytes: dir_size(&install_dir),
        });
    }

    let download_url = entry
        .download_url(manifest.cdn_base.as_deref())
        .with_context(|| format!("Cannot resolve download URL for variant `{variant}`"))?;

    info!(
        engine = "llamacpp",
        variant = %variant,
        tag = %manifest.llamacpp_tag,
        url = %download_url,
        "Downloading llama.cpp variant"
    );
    println!(
        "  ⚙ Downloading llama.cpp {} ({})",
        variant, manifest.llamacpp_tag
    );
    println!("    {}", download_url);

    // Stage download in a tmp dir, hash on the fly, then atomically move
    // the extracted payload into place. Failure leaves the existing
    // install (if any) untouched.
    let staging_root = data_dir.join("engines").join("llamacpp").join("staging");
    std::fs::create_dir_all(&staging_root)
        .context("Failed to create variant staging directory")?;
    let archive_path = staging_root.join(format!("{}.tar.gz", variant.as_str()));

    download_with_sha256(&download_url, &archive_path, &entry.sha256).await?;

    let extract_root = staging_root.join(format!("{}-extract", variant.as_str()));
    let _ = std::fs::remove_dir_all(&extract_root);
    std::fs::create_dir_all(&extract_root)?;
    extract_archive(&archive_path, &extract_root, profile).await?;

    // The tarball wraps a single top-level dir
    // (`lmforge-llamacpp-<tag>-<variant>-linux-x64/`). Find it.
    let inner = find_single_subdir(&extract_root)
        .context("Tarball layout unexpected — no single top-level directory found")?;

    // Replace any existing install atomically-ish: remove + rename.
    let _ = std::fs::remove_dir_all(&install_dir);
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&inner, &install_dir).with_context(|| {
        format!(
            "Failed to move extracted variant into place: {} → {}",
            inner.display(),
            install_dir.display()
        )
    })?;

    // Make every binary executable (the tarball preserves mode but
    // belt-and-suspenders for ZIP / extracted-via-PowerShell paths).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["llama-server", "llama-cli", "llama-bench", "llama-quantize"] {
            let p = install_dir.join(name);
            if let Ok(meta) = std::fs::metadata(&p) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&p, perms);
            }
        }
    }

    // Cleanup staging.
    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_root);

    let binary_path = install_dir.join(variant_binary_name(profile));
    let size_bytes = dir_size(&install_dir);

    println!(
        "  ✓ llama.cpp {} installed at {} ({} MB)",
        variant,
        install_dir.display(),
        size_bytes / (1024 * 1024)
    );

    Ok(VariantInstallResult {
        engine_id: "llamacpp".to_string(),
        variant,
        install_dir,
        binary_path,
        llamacpp_tag: manifest.llamacpp_tag,
        size_bytes,
    })
}

/// Where a specific variant lives on disk:
/// `<data_dir>/engines/llamacpp/variants/<id>/`.
pub fn variant_install_dir(
    data_dir: &std::path::Path,
    variant: crate::engine::variant::LlamaVariant,
) -> std::path::PathBuf {
    data_dir
        .join("engines")
        .join("llamacpp")
        .join("variants")
        .join(variant.as_str())
}

/// True when a complete variant install exists at `variant_install_dir` —
/// i.e. the marker `llama-server` binary is present. Used by
/// `lmforge engine list` and the variant selector to fill `VariantState`.
pub fn variant_installed(
    data_dir: &std::path::Path,
    variant: crate::engine::variant::LlamaVariant,
    profile: &HardwareProfile,
) -> bool {
    variant_install_dir(data_dir, variant)
        .join(variant_binary_name(profile))
        .is_file()
}

/// Snapshot the on-disk variant tree into a [`VariantState`] suitable for
/// [`crate::engine::variant::select`]. Centralises the directory scan +
/// `LMFORGE_LLAMACPP_VARIANT` env-override parsing so `lmforge doctor`,
/// `lmforge engine list`, and the runtime spawn path (C-3) all see the
/// same view.
pub fn scan_variant_state(
    data_dir: &std::path::Path,
    profile: &HardwareProfile,
) -> crate::engine::variant::VariantState {
    use crate::engine::variant::{LlamaVariant, VariantState};
    VariantState {
        cuda12_installed: variant_installed(data_dir, LlamaVariant::Cuda12, profile),
        cuda13_installed: variant_installed(data_dir, LlamaVariant::Cuda13, profile),
        vulkan_installed: variant_installed(data_dir, LlamaVariant::Vulkan, profile),
        cpu_installed: variant_installed(data_dir, LlamaVariant::Cpu, profile),
        prefer_cuda13: std::env::var("LMFORGE_LLAMACPP_VARIANT")
            .map(|s| s.eq_ignore_ascii_case("cuda13"))
            .unwrap_or(false),
    }
}

fn variant_binary_name(profile: &HardwareProfile) -> &'static str {
    if profile.os == Os::Windows {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// Read `VERSION` from a variant install, returning the `llamacpp_tag`
/// line value when present. Used for idempotency in `install_variant`.
fn read_installed_tag(install_dir: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(install_dir.join("VERSION")).ok()?;
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("llamacpp_tag=") {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Recursive size of a directory, in bytes. Returns 0 on read errors so
/// the caller can still print a result.
fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let walker = match std::fs::read_dir(dir) {
        Ok(w) => w,
        Err(_) => return 0,
    };
    for entry in walker.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size(&entry.path());
            }
        }
    }
    total
}

/// Find the single subdirectory of `parent` (the tarball convention is
/// one top-level directory). Returns its full path. Errors when there's
/// not exactly one subdirectory — that means the tarball layout changed
/// or the extraction failed.
fn find_single_subdir(parent: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(parent)
        .with_context(|| format!("Cannot read extracted tarball at {}", parent.display()))?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.is_dir() { Some(p) } else { None }
        })
        .collect();
    match dirs.len() {
        1 => Ok(dirs.pop().unwrap()),
        n => bail!(
            "Expected 1 top-level directory in tarball, found {} at {}",
            n,
            parent.display()
        ),
    }
}

/// Streaming download + on-the-fly sha256 verification. Mirrors the
/// pattern in `crate::engine::uv::download_with_sha256` but lives here so
/// the variant installer doesn't pull in the uv module's other helpers.
async fn download_with_sha256(
    url: &str,
    dest: &std::path::Path,
    expected_hex: &str,
) -> Result<()> {
    use futures::StreamExt;
    use indicatif::{ProgressBar, ProgressStyle};
    use sha2::{Digest, Sha256};
    use std::io::Write;

    let client = reqwest::Client::builder()
        .user_agent(format!("lmforge/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to start download: {url}"))?;

    if !resp.status().is_success() {
        bail!(
            "Variant download failed: HTTP {} at {}",
            resp.status(),
            url
        );
    }

    let total_size = resp.content_length().unwrap_or(0);
    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("    [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("█▓░"),
    );

    let mut file = std::fs::File::create(dest)
        .with_context(|| format!("Cannot create {}", dest.display()))?;
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
    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        let _ = std::fs::remove_file(dest);
        bail!(
            "Checksum mismatch for {url}\n  expected: {expected_hex}\n  actual:   {actual_hex}\n\
             The release tarball may have been re-published — open an issue at \
             https://github.com/phoenixtb/lmforge/issues."
        );
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

    // ── resolve_platform: full variant matrix ────────────────────────────
    //
    // Each (OS, arch, gpu) combination has exactly one expected upstream
    // asset; these tests are the executable spec of that mapping. If we ever
    // re-tier the engines or upstream drops/adds a variant, edit BOTH the
    // matrix in resolve_platform and the corresponding test below.

    fn mk(os: Os, arch: Arch, gpu: crate::hardware::probe::GpuVendor) -> HardwareProfile {
        HardwareProfile {
            os,
            arch,
            gpu_vendor: gpu,
            total_ram_gb: 16.0,
            cpu_cores: 8,
            cpu_model: "synthetic test cpu".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_platform_linux_x64_nvidia_uses_vulkan() {
        // Linux + NVIDIA: upstream dropped CUDA prebuilts ~b8370; Vulkan is
        // the best GPU path available.
        let (p, ext) = resolve_platform(&mk(
            Os::Linux,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Nvidia,
        ))
        .unwrap();
        assert_eq!(p, "ubuntu-vulkan-x64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn resolve_platform_linux_x64_amd_uses_vulkan() {
        let (p, ext) = resolve_platform(&mk(
            Os::Linux,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Amd,
        ))
        .unwrap();
        assert_eq!(p, "ubuntu-vulkan-x64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn resolve_platform_linux_x64_intel_igpu_uses_vulkan() {
        // Intel iGPUs (Iris/Arc) are Vulkan-capable via Mesa Iris driver.
        // Must route here rather than the CPU fallback or perf cratters.
        let (p, ext) = resolve_platform(&mk(
            Os::Linux,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Intel,
        ))
        .unwrap();
        assert_eq!(p, "ubuntu-vulkan-x64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn resolve_platform_linux_x64_no_gpu_uses_cpu() {
        let (p, ext) = resolve_platform(&mk(
            Os::Linux,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::None,
        ))
        .unwrap();
        assert_eq!(p, "ubuntu-x64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn resolve_platform_linux_arm64_with_gpu_uses_vulkan_arm() {
        // Jetson AGX Orin (sm_87) reports as NVIDIA on aarch64; should
        // pull the Ubuntu ARM Vulkan build.
        let (p, ext) = resolve_platform(&mk(
            Os::Linux,
            Arch::Aarch64,
            crate::hardware::probe::GpuVendor::Nvidia,
        ))
        .unwrap();
        assert_eq!(p, "ubuntu-vulkan-arm64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn resolve_platform_linux_arm64_no_gpu_uses_cpu_arm() {
        // AWS Graviton, RPi5 without GPU driver, etc.
        let (p, ext) = resolve_platform(&mk(
            Os::Linux,
            Arch::Aarch64,
            crate::hardware::probe::GpuVendor::None,
        ))
        .unwrap();
        assert_eq!(p, "ubuntu-arm64");
        assert_eq!(ext, "tar.gz");
    }

    #[test]
    fn resolve_platform_windows_x64_nvidia_uses_cuda() {
        // Windows + NVIDIA = CUDA build. cudart-* DLLs are pulled separately
        // by the install flow (see cudart_archive_path block in `install`).
        let (p, ext) = resolve_platform(&mk(
            Os::Windows,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Nvidia,
        ))
        .unwrap();
        assert!(
            p.starts_with("win-cuda-") && p.ends_with("-x64"),
            "expected win-cuda-*-x64, got {}",
            p
        );
        assert_eq!(ext, "zip");
    }

    #[test]
    fn resolve_platform_windows_x64_amd_uses_vulkan() {
        // Windows + AMD = Vulkan (HIP is heavy + opt-in territory).
        let (p, ext) = resolve_platform(&mk(
            Os::Windows,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Amd,
        ))
        .unwrap();
        assert_eq!(p, "win-vulkan-x64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn resolve_platform_windows_x64_intel_igpu_uses_vulkan() {
        let (p, ext) = resolve_platform(&mk(
            Os::Windows,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Intel,
        ))
        .unwrap();
        assert_eq!(p, "win-vulkan-x64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn resolve_platform_windows_x64_no_gpu_uses_cpu() {
        let (p, ext) = resolve_platform(&mk(
            Os::Windows,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::None,
        ))
        .unwrap();
        assert_eq!(p, "win-cpu-x64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn resolve_platform_windows_arm64_uses_cpu_arm() {
        // Upstream ships no GPU build for ARM Windows; CPU only.
        let (p, ext) = resolve_platform(&mk(
            Os::Windows,
            Arch::Aarch64,
            crate::hardware::probe::GpuVendor::Nvidia,
        ))
        .unwrap();
        assert_eq!(p, "win-cpu-arm64");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn variant_override_auto_matches_probe_selection() {
        // auto override (or unset) must match the no-override matrix exactly.
        // Use the pure helper rather than mutating LMFORGE_LLAMACPP_VARIANT,
        // since that env var is process-global and races other parallel tests.
        let p = mk(
            Os::Linux,
            Arch::X86_64,
            crate::hardware::probe::GpuVendor::Nvidia,
        );
        let (asset, _) = resolve_platform_with_override(&p, Some("auto")).unwrap();
        assert_eq!(asset, "ubuntu-vulkan-x64");
        let (asset, _) = resolve_platform_with_override(&p, None).unwrap();
        assert_eq!(asset, "ubuntu-vulkan-x64");
    }

    #[test]
    fn variant_override_cpu_forces_cpu_build() {
        // cpu override must downgrade even an NVIDIA-equipped profile to the
        // CPU build on both Linux and Windows.
        let (asset, _) = resolve_platform_with_override(
            &mk(
                Os::Linux,
                Arch::X86_64,
                crate::hardware::probe::GpuVendor::Nvidia,
            ),
            Some("cpu"),
        )
        .unwrap();
        assert_eq!(asset, "ubuntu-x64");

        let (asset, _) = resolve_platform_with_override(
            &mk(
                Os::Windows,
                Arch::X86_64,
                crate::hardware::probe::GpuVendor::Nvidia,
            ),
            Some("cpu"),
        )
        .unwrap();
        assert_eq!(asset, "win-cpu-x64");
    }

    #[test]
    fn variant_override_gpu_forces_vulkan_when_no_vendor_detected() {
        // gpu override must upgrade a vendor=None profile to a Vulkan build.
        // On Windows specifically, the override must NOT pick CUDA — that
        // path needs cudart DLLs which 404 without nvidia-smi at install.
        let (asset, _) = resolve_platform_with_override(
            &mk(
                Os::Linux,
                Arch::X86_64,
                crate::hardware::probe::GpuVendor::None,
            ),
            Some("gpu"),
        )
        .unwrap();
        assert_eq!(asset, "ubuntu-vulkan-x64");

        let (asset, _) = resolve_platform_with_override(
            &mk(
                Os::Windows,
                Arch::X86_64,
                crate::hardware::probe::GpuVendor::None,
            ),
            Some("gpu"),
        )
        .unwrap();
        assert_eq!(
            asset, "win-vulkan-x64",
            "gpu override on Windows must pick Vulkan, not CUDA"
        );
    }

    #[test]
    fn cudart_pattern_matches_upstream_asset_format() {
        // The llama.cpp release page ships:
        //   cudart-llama-bin-win-cuda-12.4-x64.zip
        //   cudart-llama-bin-win-cuda-13.1-x64.zip
        // The pattern in engines.toml must expand to those names byte-exact
        // or Windows installs will 404 silently.
        let registry = crate::engine::EngineRegistry::load(None).unwrap();
        let llama = registry.get("llamacpp").expect("llamacpp engine");
        let pattern = llama
            .cudart_pattern
            .as_ref()
            .expect("cudart_pattern must be set for Windows NVIDIA installs");
        for variant in &["12.4", "13.1"] {
            let resolved = format!("{}.zip", pattern.replace("{cuda_variant}", variant));
            let expected = format!("cudart-llama-bin-win-cuda-{}-x64.zip", variant);
            assert_eq!(resolved, expected, "cudart pattern drift detected");
        }
    }
}
