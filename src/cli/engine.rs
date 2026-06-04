//! `lmforge engine list|install|uninstall|status` — opt-in engine management.
//!
//! Why a dedicated subcommand instead of folding into `init` or `start`:
//!   * Default-tier engines (`llamacpp`, `omlx`) are auto-installed by `init`.
//!   * Opt-in tiers (`vllm`, `exl3`, `sglang`) take 5+ GB on disk and ship a
//!     full Python toolchain; users explicitly choose to pay that cost.
//!   * `--engine <id>` at `start` time should *fail fast* if the user picked
//!     an engine they never installed. That requires a separate install verb.
//!
//! Hardware gates are enforced here BEFORE downloading anything — a Windows
//! user asking for vLLM gets a clear refusal instead of a partial 5 GB venv.

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::config::LmForgeConfig;
use crate::engine::registry::{EngineConfig, EngineRegistry, EngineTier};
use crate::hardware::{self, probe::HardwareProfile};

use super::EngineAction;

pub async fn run(config: &LmForgeConfig, action: EngineAction) -> Result<()> {
    let data_dir = config.data_dir();
    let user_engines_toml = data_dir.join("engines.toml");
    let registry = EngineRegistry::load(if user_engines_toml.exists() {
        Some(user_engines_toml.as_path())
    } else {
        None
    })
    .context("Failed to load engine registry")?;

    let profile = load_or_probe_profile(&data_dir)?;

    match action {
        EngineAction::List => list(&registry, &profile),
        EngineAction::Install {
            id,
            yes_experimental,
            variant,
        } => {
            install(
                &registry,
                &profile,
                &data_dir,
                &id,
                yes_experimental,
                variant.as_deref(),
            )
            .await
        }
        EngineAction::Uninstall { id, yes } => uninstall(&registry, &data_dir, &id, yes),
        EngineAction::Status { id } => status(&registry, &profile, &data_dir, &id),
    }
}

/// Prefer the cached probe so we don't hit nvidia-smi twice in the same
/// session; fall back to a live probe when the file is missing (e.g.
/// the user removed `~/.lmforge/` between commands).
fn load_or_probe_profile(data_dir: &std::path::Path) -> Result<HardwareProfile> {
    let path = data_dir.join("hardware.json");
    if path.is_file()
        && let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(p) = serde_json::from_str::<HardwareProfile>(&content)
    {
        return Ok(p);
    }
    hardware::detect()
}

// ── list ───────────────────────────────────────────────────────────────────

fn list(registry: &EngineRegistry, profile: &HardwareProfile) -> Result<()> {
    let data_dir = data_dir_from(profile);

    println!(
        "{:<10} {:<14} {:<13} {:<10} {:<12} NOTE",
        "ID", "TIER", "VERSION", "INSTALLED", "COMPATIBLE"
    );
    println!("{}", "─".repeat(78));

    for engine in registry.all() {
        let installed = install_state(engine, &data_dir);
        let (compat, note) = compatibility(engine, profile);
        println!(
            "{:<10} {:<14} {:<13} {:<10} {:<12} {}",
            engine.id,
            tier_label(engine.tier),
            engine.version,
            if installed { "yes" } else { "no" },
            if compat { "yes" } else { "no" },
            note,
        );

        // llama.cpp: expose installed variant set so the user can tell which
        // build (cuda12 / cuda13 / vulkan / cpu) is actually staged.
        // The legacy flat install at `<data_dir>/engines/llama-server` and
        // the new variant tree at `<data_dir>/engines/llamacpp/variants/`
        // are independent — both layouts are listed.
        if engine.id == "llamacpp" {
            let variants = llamacpp_variant_summary(&data_dir, profile);
            if !variants.is_empty() {
                println!("           variants: {variants}");
            }
        }
    }
    println!();
    println!("  • `default` tier engines are installed automatically by `lmforge init`.");
    println!("  • `opt-in`  tier engines require `lmforge engine install <id>` (uses ~5 GB).");
    println!("  • `experimental` engines are never auto-selected; use `--engine <id>`.");
    println!(
        "  • `lmforge engine install llamacpp --variant <cuda12|cuda13|vulkan|cpu>` \
         pulls a specific build."
    );
    Ok(())
}

/// One-line summary of which `llama.cpp` variants are present on disk.
/// Returns an empty string when no variant directory exists, so callers
/// can suppress the row entirely. Active variant (per
/// `variant::select`) is marked with `*`.
fn llamacpp_variant_summary(data_dir: &std::path::Path, profile: &HardwareProfile) -> String {
    use crate::engine::variant::{LlamaVariant, VariantState, select};

    let cuda12 =
        crate::engine::installer::variant_installed(data_dir, LlamaVariant::Cuda12, profile);
    let cuda13 =
        crate::engine::installer::variant_installed(data_dir, LlamaVariant::Cuda13, profile);
    let vulkan =
        crate::engine::installer::variant_installed(data_dir, LlamaVariant::Vulkan, profile);
    let cpu = crate::engine::installer::variant_installed(data_dir, LlamaVariant::Cpu, profile);

    let prefer_cuda13 = std::env::var("LMFORGE_LLAMACPP_VARIANT")
        .map(|s| s.eq_ignore_ascii_case("cuda13"))
        .unwrap_or(false);

    let state = VariantState {
        cuda12_installed: cuda12,
        cuda13_installed: cuda13,
        vulkan_installed: vulkan,
        cpu_installed: cpu,
        prefer_cuda13,
    };
    let active = select(profile, &state);

    let mut parts: Vec<String> = Vec::new();
    for (label, installed) in [
        (LlamaVariant::Cuda12, cuda12),
        (LlamaVariant::Cuda13, cuda13),
        (LlamaVariant::Vulkan, vulkan),
        (LlamaVariant::Cpu, cpu),
    ] {
        if installed {
            let marker = if label == active { "*" } else { "" };
            parts.push(format!("{label}{marker}"));
        }
    }
    parts.join(", ")
}

/// Locate `~/.lmforge/` from a probe — the registry-list view doesn't have a
/// LmForgeConfig handy. Lifting this here keeps the call sites symmetric.
fn data_dir_from(_profile: &HardwareProfile) -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".lmforge")
    } else {
        std::path::PathBuf::from(".lmforge")
    }
}

// ── install ────────────────────────────────────────────────────────────────

async fn install(
    registry: &EngineRegistry,
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
    id: &str,
    yes_experimental: bool,
    variant: Option<&str>,
) -> Result<()> {
    // Variant flag is llamacpp-only — refuse early if applied elsewhere
    // (otherwise the user thinks it took effect on, say, `vllm`).
    if variant.is_some() && id != "llamacpp" {
        bail!(
            "`--variant` is only valid for the `llamacpp` engine (got engine `{id}`). \
             Remove `--variant` or pass `--engine llamacpp`."
        );
    }

    // Llamacpp + variant: bypass the legacy `install_via_binary` flow
    // entirely and use the new manifest-driven variant installer.
    if id == "llamacpp"
        && let Some(variant_str) = variant
    {
        return install_llamacpp_variant(profile, data_dir, variant_str).await;
    }

    // `select_explicit` enforces v1 + v2 hardware gates and returns a clear
    // error like "Engine `vllm` does not support this hardware: Os=Windows ...".
    // That's exactly the message we want surfaced to the user — no need to
    // reimplement the gate matrix here.
    let engine = registry.select_explicit(id, profile).with_context(|| {
        format!(
            "Cannot install engine `{}` — it does not support this hardware",
            id
        )
    })?;

    if engine.tier == EngineTier::Default {
        println!(
            "  ℹ `{}` is a default-tier engine. It's installed automatically by `lmforge init`.",
            engine.id
        );
        println!("  Run that instead — no separate install step needed.");
        return Ok(());
    }

    if engine.tier == EngineTier::Experimental && !yes_experimental {
        confirm_experimental(engine)?;
    }

    // Soft caveats — printed before the install kicks off so the user can
    // ^C before paying the ~5 GB / ~5 min cost. Currently the only engine
    // with a soft caveat is vLLM (single-GPU win is marginal vs. llama.cpp).
    print_soft_caveats(engine, profile);

    println!(
        "  ⚙ Installing {} v{} (tier={}, install_method={})...",
        engine.name,
        engine.version,
        tier_label(engine.tier),
        engine.install_method
    );

    let result = crate::engine::installer::install(engine, profile, data_dir)
        .await
        .with_context(|| format!("Engine installation failed for {}", engine.id))?;

    println!();
    println!("  ✓ Installed: {} ({})", engine.name, result.method_used);
    println!("    Path:    {}", result.install_path);
    println!(
        "    Use it:  lmforge start --engine {}  (or --model <id> --engine {})",
        engine.id, engine.id
    );
    Ok(())
}

/// `lmforge engine install llamacpp --variant <id>` — manifest-driven
/// install of a specific `llama.cpp` build flavour. Wraps
/// `installer::install_variant` with hardware-gate refusal messages that
/// match the rest of `lmforge engine`'s output.
async fn install_llamacpp_variant(
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
    variant_str: &str,
) -> Result<()> {
    use std::str::FromStr;

    let variant = crate::engine::variant::LlamaVariant::from_str(variant_str)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Pre-flight gate check — gives a CUDA-aware error before we even
    // touch the network. Mirrors the refusal in `installer::install_variant`
    // but presents it as a CLI message rather than a generic bail.
    if let Err(reason) = crate::engine::variant::refuse_reason(variant, profile) {
        bail!("Cannot install llamacpp variant `{variant}`: {reason}");
    }

    println!(
        "  ⚙ Installing llama.cpp variant `{}` (tier=default, install_method=binary-variant)...",
        variant
    );

    let result = crate::engine::installer::install_variant(profile, variant, data_dir).await?;

    println!();
    println!(
        "  ✓ Installed: llamacpp ({}, tag={})",
        result.variant, result.llamacpp_tag
    );
    println!("    Path:    {}", result.binary_path.display());
    println!("    Size:    {} MB", result.size_bytes / (1024 * 1024));
    println!(
        "    Activate: this variant will be picked automatically by `lmforge start` \
         when hardware allows. Override with `LMFORGE_LLAMACPP_VARIANT={}`.",
        result.variant
    );
    Ok(())
}

/// Block scripted-mode installs of experimental engines unless `--yes-experimental`
/// or `LMFORGE_YES_EXPERIMENTAL=1` is set. Same posture as `start::run`.
fn confirm_experimental(engine: &EngineConfig) -> Result<()> {
    if std::env::var("LMFORGE_YES_EXPERIMENTAL")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        .unwrap_or(false)
    {
        info!(engine = %engine.id, "LMFORGE_YES_EXPERIMENTAL set — skipping confirm prompt");
        return Ok(());
    }
    if !is_stdin_tty() {
        bail!(
            "Engine `{}` is marked experimental. Re-run with `--yes-experimental` (or set LMFORGE_YES_EXPERIMENTAL=1) to install non-interactively.",
            engine.id
        );
    }

    use std::io::{BufRead, Write};
    eprintln!();
    eprintln!(
        "  ⚠ `{}` is an experimental engine — it may fail at runtime on this hardware.",
        engine.id
    );
    eprintln!("    See data/engines.toml for the documented caveats.");
    eprint!("  Continue installing? [y/N] ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        bail!("Aborted (could not read stdin)");
    }
    let ans = line.trim().to_lowercase();
    if ans == "y" || ans == "yes" {
        Ok(())
    } else {
        bail!("Aborted by user")
    }
}

/// Engine-specific soft caveats. These are advisory — they don't gate the
/// install — but they're loud enough that nobody installs vLLM thinking it's
/// strictly better than llama.cpp on a single-GPU desktop.
///
/// Exposed `pub(crate)` so `cli::start` can reuse the exact same wording
/// when `--engine vllm` is passed at run time.
pub(crate) fn print_soft_caveats(engine: &EngineConfig, profile: &HardwareProfile) {
    if engine.id != "vllm" {
        return;
    }
    let single_gpu = profile.gpu_count <= 1;
    let sm120 = matches!(profile.compute_cap, Some((12, _)));

    if single_gpu {
        eprintln!();
        eprintln!("  ℹ vLLM caveat: single-GPU host detected.");
        eprintln!(
            "    vLLM's main win is concurrent batching (4+ in-flight requests).\n    \
             For single-stream desktop use, llama.cpp is typically within ~15%\n    \
             of vLLM throughput with a ~10x faster cold start. Install only if\n    \
             you're benchmarking concurrency or running a multi-tenant frontend."
        );
    }
    if sm120 {
        eprintln!();
        eprintln!("  ⚠ vLLM on consumer Blackwell (sm_120):");
        eprintln!(
            "    NVFP4-quantized MoE models still hit a known attention-path bug\n    \
             under batch>1 (upstream tracker, not yet released). Workaround:\n    \
             stay at batch=1 OR use AWQ/GPTQ-4bit quants. Standard dense models\n    \
             work fine on sm_120 since vLLM 0.20."
        );
    }
}

fn is_stdin_tty() -> bool {
    #[cfg(unix)]
    unsafe {
        libc::isatty(0) == 1
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// ── uninstall ──────────────────────────────────────────────────────────────

fn uninstall(
    registry: &EngineRegistry,
    data_dir: &std::path::Path,
    id: &str,
    yes: bool,
) -> Result<()> {
    let engine = registry
        .get(id)
        .with_context(|| format!("Unknown engine id: {}", id))?;

    let venv_dir = data_dir.join("engines").join(&engine.id).join("venv");
    let bin_path = engine
        .binary
        .as_ref()
        .map(|b| data_dir.join("engines").join(b));

    let mut targets: Vec<std::path::PathBuf> = Vec::new();
    if venv_dir.is_dir() {
        targets.push(venv_dir.clone());
    }
    if let Some(p) = bin_path.as_ref()
        && p.is_file()
    {
        targets.push(p.clone());
    }

    if targets.is_empty() {
        println!(
            "  ℹ No install artefacts found for `{}` — nothing to do.",
            id
        );
        return Ok(());
    }

    println!("  Will remove:");
    for t in &targets {
        println!("    - {}", t.display());
    }
    println!();

    if !yes && is_stdin_tty() {
        use std::io::{BufRead, Write};
        eprint!("  Proceed? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            bail!("Aborted (could not read stdin)");
        }
        let ans = line.trim().to_lowercase();
        if ans != "y" && ans != "yes" {
            bail!("Aborted by user");
        }
    }

    for t in targets {
        if t.is_dir() {
            std::fs::remove_dir_all(&t)
                .with_context(|| format!("Failed to remove {}", t.display()))?;
        } else if t.is_file() {
            std::fs::remove_file(&t)
                .with_context(|| format!("Failed to remove {}", t.display()))?;
        }
        println!("  ✓ Removed {}", t.display());
    }

    // For binary engines, sweep the sibling shared libraries too. They're
    // useless without the main binary and waste ~150 MB on disk.
    if engine.install_method == "binary"
        && let Some(bin) = bin_path.as_ref()
        && let Some(parent) = bin.parent()
        && parent.is_dir()
    {
        let removed = sweep_shared_libs(parent)?;
        if removed > 0 {
            println!("  ✓ Removed {} sibling shared libraries", removed);
        }
    }

    Ok(())
}

/// Delete every `.so / .so.N / .dylib / .dll` file directly inside `dir`.
/// Mirrors the matcher in `engine::installer::is_shared_lib` but applies it
/// to the *removal* side.
fn sweep_shared_libs(dir: &std::path::Path) -> Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_lowercase(),
            None => continue,
        };
        let is_lib = name.ends_with(".dylib")
            || name.ends_with(".dll")
            || (name.contains(".so")
                && name
                    .split_once(".so")
                    .map(|(_, rest)| {
                        rest.is_empty()
                            || (rest.starts_with('.')
                                && rest
                                    .trim_start_matches('.')
                                    .chars()
                                    .all(|c| c.is_ascii_digit() || c == '.'))
                    })
                    .unwrap_or(false));
        if is_lib && std::fs::remove_file(&path).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

// ── status ─────────────────────────────────────────────────────────────────

fn status(
    registry: &EngineRegistry,
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
    id: &str,
) -> Result<()> {
    let engine = registry
        .get(id)
        .with_context(|| format!("Unknown engine id: {}", id))?;

    let installed = install_state(engine, data_dir);
    let (compat, note) = compatibility(engine, profile);

    println!("  Engine:     {} ({})", engine.name, engine.id);
    println!("  Version:    {}", engine.version);
    println!("  Tier:       {}", tier_label(engine.tier));
    println!("  Install:    {}", engine.install_method);
    println!("  Format:     {}", engine.model_format);
    println!("  Embed:      {}", yes_no(engine.supports_embeddings));
    println!("  Rerank:     {}", yes_no(engine.supports_reranking));
    println!("  Installed:  {}", if installed { "yes" } else { "no" });
    println!(
        "  Compatible: {}{}",
        yes_no(compat),
        if note.is_empty() {
            String::new()
        } else {
            format!(" — {}", note)
        }
    );

    if !installed && compat && engine.tier == EngineTier::OptIn {
        println!();
        println!("  Install with: lmforge engine install {}", engine.id);
    }
    if !compat {
        warn!(engine = %engine.id, "Engine is not compatible with this hardware");
    }

    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Human-readable tier label. Exposed `pub(crate)` so the HTTP `/lf/engines`
/// endpoint and the CLI emit the exact same strings — UI badges should
/// match `engine list` rows.
pub(crate) fn tier_label(t: EngineTier) -> &'static str {
    match t {
        EngineTier::Default => "default",
        EngineTier::OptIn => "opt-in",
        EngineTier::Experimental => "experimental",
        EngineTier::Unspecified => "default*",
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// True if THIS host has a usable install of `engine`. For pip engines that's
/// "venv exists with the right python interpreter inside"; for binary engines
/// it's "the staged binary exists at `<data_dir>/engines/<bin>`".
///
/// Exposed `pub(crate)` so the HTTP `/lf/engines` endpoint surfaces the same
/// verdict as the CLI — UI install/uninstall buttons must agree with what
/// `lmforge engine status` says.
pub(crate) fn install_state(engine: &EngineConfig, data_dir: &std::path::Path) -> bool {
    match engine.install_method.as_str() {
        "pip" => {
            let venv_python = if cfg!(windows) {
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
            };
            venv_python.is_file()
        }
        "binary" => {
            if let Some(bin) = engine.binary.as_ref() {
                let resolved = if cfg!(windows) && !bin.ends_with(".exe") {
                    format!("{}.exe", bin)
                } else {
                    bin.clone()
                };
                data_dir.join("engines").join(resolved).is_file()
            } else {
                false
            }
        }
        "brew" => {
            // Brew installs to the global prefix; consider it installed iff
            // `brew list --versions <formula>` succeeds.
            engine
                .brew_formula
                .as_ref()
                .map(|f| {
                    std::process::Command::new("brew")
                        .args(["list", "--versions", f])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Run the hardware gates without actually selecting the engine. Returns
/// `(compatible, why)`. We re-use `select_explicit` so the verdict matches
/// what `lmforge start --engine <id>` would produce.
///
/// Exposed `pub(crate)` so the HTTP `/lf/engines` endpoint reports the same
/// compatibility verdict the CLI does — otherwise the UI would claim an
/// engine is installable while `start --engine <id>` would refuse it.
pub(crate) fn compatibility(engine: &EngineConfig, profile: &HardwareProfile) -> (bool, String) {
    // Borrow-trick: construct a single-engine registry without copying state.
    // Cheaper to use a focused helper than to pull `select_explicit` apart.
    let temp = SingleEngineProbe::new(engine.clone());
    match temp.matches(profile) {
        Ok(()) => (true, String::new()),
        Err(e) => (false, e.to_string()),
    }
}

/// Lightweight wrapper around the gate matrix so `compatibility()` and tests
/// can both call into it without touching the full `EngineRegistry`.
struct SingleEngineProbe {
    engine: EngineConfig,
}

impl SingleEngineProbe {
    fn new(engine: EngineConfig) -> Self {
        Self { engine }
    }

    fn matches(&self, profile: &HardwareProfile) -> Result<()> {
        use crate::engine::registry::{v1_matches, v2_matches};
        if !v1_matches(&self.engine, profile) {
            bail!(
                "OS/arch/gpu mismatch ({:?} {:?} GPU:{:?})",
                profile.os,
                profile.arch,
                profile.gpu_vendor
            );
        }
        if !v2_matches(&self.engine, profile) {
            bail!("Compute-capability or OS-family gate refused this combo");
        }
        Ok(())
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::probe::{Arch, GpuVendor, Os};

    fn make_profile_linux_nvidia_sm120() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            compute_cap: Some((12, 0)),
            vram_gb: 15.4,
            total_ram_gb: 16.0,
            cpu_cores: 12,
            cpu_model: "Test".into(),
            ..Default::default()
        }
    }

    fn make_profile_macos() -> HardwareProfile {
        HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            gpu_vendor: GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".into(),
            ..Default::default()
        }
    }

    fn make_profile_linux_amd() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Amd,
            vram_gb: 24.0,
            total_ram_gb: 64.0,
            cpu_cores: 16,
            cpu_model: "AMD".into(),
            ..Default::default()
        }
    }

    #[test]
    fn list_renders_without_panic() {
        // Smoke: registry loads and the renderer doesn't crash on the
        // default catalog. We don't assert on stdout because the column
        // widths drift; the value is "this never silently breaks".
        let registry = EngineRegistry::load(None).unwrap();
        let profile = make_profile_linux_nvidia_sm120();
        list(&registry, &profile).unwrap();
    }

    #[test]
    fn compatibility_default_engine_passes_on_linux_nvidia() {
        let registry = EngineRegistry::load(None).unwrap();
        let llama = registry.get("llamacpp").unwrap();
        let profile = make_profile_linux_nvidia_sm120();
        let (ok, _why) = compatibility(llama, &profile);
        assert!(ok, "llama.cpp should match Linux + NVIDIA sm_120");
    }

    #[test]
    fn compatibility_sglang_refuses_sm120() {
        let registry = EngineRegistry::load(None).unwrap();
        let sglang = registry.get("sglang").unwrap();
        let profile = make_profile_linux_nvidia_sm120();
        let (ok, why) = compatibility(sglang, &profile);
        assert!(!ok, "SGLang must refuse sm_120 (max_compute_cap=10.3)");
        assert!(!why.is_empty(), "Refusal must carry a reason");
    }

    #[test]
    fn compatibility_omlx_refuses_linux() {
        // oMLX is darwin-only; gate should fire even though llama.cpp
        // happily matches the same Linux box.
        let registry = EngineRegistry::load(None).unwrap();
        let omlx = registry.get("omlx").unwrap();
        let profile = make_profile_linux_nvidia_sm120();
        let (ok, _why) = compatibility(omlx, &profile);
        assert!(!ok, "oMLX must refuse non-Darwin hosts");
    }

    #[test]
    fn install_state_no_artifacts_returns_false() {
        let registry = EngineRegistry::load(None).unwrap();
        let sglang = registry.get("sglang").unwrap();
        let tmp = std::env::temp_dir().join("lmforge_test_engine_cmd_no_artifacts");
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(!install_state(sglang, &tmp));
    }

    #[test]
    fn install_state_pip_detects_venv() {
        let registry = EngineRegistry::load(None).unwrap();
        let sglang = registry.get("sglang").unwrap();
        let tmp = std::env::temp_dir().join("lmforge_test_engine_cmd_pip");
        let _ = std::fs::remove_dir_all(&tmp);
        let venv_bin = tmp.join("engines").join("sglang").join("venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::fs::write(venv_bin.join("python3"), b"#!/bin/sh\n").unwrap();
        assert!(install_state(sglang, &tmp));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn install_state_binary_detects_staged_bin() {
        let registry = EngineRegistry::load(None).unwrap();
        let llama = registry.get("llamacpp").unwrap();
        let tmp = std::env::temp_dir().join("lmforge_test_engine_cmd_bin");
        let _ = std::fs::remove_dir_all(&tmp);
        let engines = tmp.join("engines");
        std::fs::create_dir_all(&engines).unwrap();
        std::fs::write(engines.join("llama-server"), b"#!/bin/sh\n").unwrap();
        assert!(install_state(llama, &tmp));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn sweep_shared_libs_removes_so_files_only() {
        let tmp = std::env::temp_dir().join("lmforge_test_sweep_libs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("libfoo.so"), b"x").unwrap();
        std::fs::write(tmp.join("libfoo.so.0"), b"x").unwrap();
        std::fs::write(tmp.join("libfoo.so.0.13.0"), b"x").unwrap();
        std::fs::write(tmp.join("libbar.dylib"), b"x").unwrap();
        std::fs::write(tmp.join("llama-server"), b"x").unwrap();
        std::fs::write(tmp.join("README"), b"x").unwrap();

        let removed = sweep_shared_libs(&tmp).unwrap();
        assert_eq!(removed, 4, "must remove .so / .so.N / .so.N.M.K / .dylib");
        assert!(tmp.join("llama-server").is_file());
        assert!(tmp.join("README").is_file());

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn print_soft_caveats_is_noop_for_non_vllm() {
        // Just must not panic for non-vLLM engines. The function writes to
        // stderr; we don't capture it here — verifying no-panic is enough
        // for a regression guard.
        let registry = EngineRegistry::load(None).unwrap();
        let llama = registry.get("llamacpp").unwrap();
        let mut profile = make_profile_linux_nvidia_sm120();
        profile.gpu_count = 1;
        print_soft_caveats(llama, &profile);
    }

    #[test]
    fn print_soft_caveats_fires_for_vllm_single_gpu() {
        let registry = EngineRegistry::load(None).unwrap();
        let vllm = registry.get("vllm").unwrap();
        let mut profile = make_profile_linux_nvidia_sm120();
        profile.gpu_count = 1;
        // No assertion on stderr content (would require redirection); we
        // exercise the branch so any future panic surface is caught.
        print_soft_caveats(vllm, &profile);
    }

    #[test]
    fn print_soft_caveats_fires_nvfp4_branch_for_sm120() {
        let registry = EngineRegistry::load(None).unwrap();
        let vllm = registry.get("vllm").unwrap();
        let profile = make_profile_linux_nvidia_sm120();
        // sm_120 → NVFP4 warning branch is taken; combined with default
        // gpu_count=0 we also hit the single-GPU branch. Both must run
        // without panicking.
        print_soft_caveats(vllm, &profile);
    }

    #[test]
    fn print_soft_caveats_skips_when_multi_gpu_and_not_sm120() {
        let registry = EngineRegistry::load(None).unwrap();
        let vllm = registry.get("vllm").unwrap();
        let mut profile = make_profile_linux_nvidia_sm120();
        profile.compute_cap = Some((9, 0));
        profile.gpu_count = 4;
        // Both branches skipped — function returns silently.
        print_soft_caveats(vllm, &profile);
    }

    #[test]
    fn confirm_experimental_env_override_skips_prompt() {
        // SAFETY: process-global env mutation; isolated by the deterministic
        // var name. No need for the shared ENV_LOCK because this var is only
        // read inside `confirm_experimental` itself.
        unsafe { std::env::set_var("LMFORGE_YES_EXPERIMENTAL", "1") };
        let registry = EngineRegistry::load(None).unwrap();
        let sglang = registry.get("sglang").unwrap();
        let result = confirm_experimental(sglang);
        unsafe { std::env::remove_var("LMFORGE_YES_EXPERIMENTAL") };
        assert!(result.is_ok(), "env override must bypass prompt");
    }

    // Reference the unused profile constructors so the file documents the
    // gate-matrix we ALREADY validate elsewhere — kills clippy's
    // dead_code on future-only branches without removing useful helpers.
    #[allow(dead_code)]
    fn _gate_matrix_used() {
        let _ = make_profile_macos();
        let _ = make_profile_linux_amd();
    }
}
