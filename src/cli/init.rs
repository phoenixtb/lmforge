use anyhow::{Context, Result};
use tracing::info;

use crate::config::LmForgeConfig;
use crate::hardware;

/// `lmforge init` — Probe hardware, select engine, install if needed
pub async fn run(config: &LmForgeConfig) -> Result<()> {
    info!("Running lmforge init...");

    // Ensure data directory exists
    let data_dir = config.data_dir();
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Cannot create data dir: {}", data_dir.display()))?;
        std::fs::create_dir_all(data_dir.join("engines"))
            .with_context(|| format!("Cannot create engines dir: {}", data_dir.display()))?;
        std::fs::create_dir_all(data_dir.join("models"))
            .with_context(|| format!("Cannot create models dir: {}", data_dir.display()))?;
        std::fs::create_dir_all(data_dir.join("logs"))
            .with_context(|| format!("Cannot create logs dir: {}", data_dir.display()))?;
        info!("Created LMForge data directory at {}", data_dir.display());
    }

    let catalogs_dir = config.catalogs_dir();
    if !catalogs_dir.exists() {
        std::fs::create_dir_all(&catalogs_dir)
            .with_context(|| format!("Cannot create catalogs dir: {}", catalogs_dir.display()))?;
    }

    let mlx_defaults = include_str!("../../data/catalogs/mlx.json");
    std::fs::write(catalogs_dir.join("mlx.json"), mlx_defaults)
        .with_context(|| format!("Cannot write mlx.json to {}", catalogs_dir.display()))?;

    let gguf_defaults = include_str!("../../data/catalogs/gguf.json");
    std::fs::write(catalogs_dir.join("gguf.json"), gguf_defaults)
        .with_context(|| format!("Cannot write gguf.json to {}", catalogs_dir.display()))?;

    // Hardware probe
    println!("⚙ Detecting hardware...");
    let profile = hardware::detect()?;
    println!("  {}", profile);
    println!();

    // Save hardware profile for later use
    let profile_json = serde_json::to_string_pretty(&profile)
        .context("Failed to serialize hardware profile")?;
    let profile_path = data_dir.join("hardware.json");
    std::fs::write(&profile_path, &profile_json)
        .with_context(|| format!("Cannot write hardware.json to {}", profile_path.display()))?;
    info!(path = %profile_path.display(), "Hardware profile saved");

    // Print summary
    println!("  OS:         {:?}", profile.os);
    println!("  Arch:       {:?}", profile.arch);
    println!("  GPU:        {:?}", profile.gpu_vendor);
    println!(
        "  VRAM:       {:.1} GB{}",
        profile.vram_gb,
        if profile.unified_mem {
            " (unified memory)"
        } else {
            ""
        }
    );
    println!("  RAM:        {:.1} GB", profile.total_ram_gb);
    println!(
        "  CPU:        {} ({} cores)",
        profile.cpu_model, profile.cpu_cores
    );
    let tier = crate::hardware::vram::quant_tier(profile.vram_gb, profile.total_ram_gb);
    match tier {
        Some(t) => println!("  Quant tier: {}", t),
        None => {
            println!("  Quant tier: ⚠  Below minimum");
            eprintln!();
            eprintln!("  ⚠  Warning: This system is below the minimum recommended specification");
            eprintln!("     for LLM inference (8 GB RAM or 3 GB VRAM).");
            eprintln!();
            eprintln!("     LMForge will still install the engine. You can:");
            eprintln!("     • Run a small model manually:  lmforge pull <model>");
            eprintln!("       (look for 1B or 3B models with Q4_K_S quantization)");
            eprintln!("     • Upgrade to a machine with ≥ 8 GB RAM for a better experience");
            eprintln!();
        }
    }
    println!();

    // Engine selection
    println!("⚙ Selecting engine...");
    let user_engines_path = data_dir.join("engines.toml");
    let registry = crate::engine::EngineRegistry::load(if user_engines_path.exists() {
        Some(user_engines_path.as_path())
    } else {
        None
    })
    .context("Failed to load engine registry")?;
    let selected = registry.select(&profile).context("No compatible engine found for this hardware")?;
    println!(
        "  Selected: {} v{} ({})",
        selected.name, selected.version, selected.id
    );
    println!("  Format:   {}", selected.model_format);
    println!("  Install:  {}", selected.install_method);
    println!();

    // Engine installation
    println!("⚙ Installing engine...");
    let install_result = crate::engine::installer::install(selected, &profile, &data_dir)
        .await
        .with_context(|| format!("Engine installation failed for {}", selected.id))?;
    println!("  Method: {}", install_result.method_used);
    println!();

    println!("\n✓ LMForge initialized at {}", data_dir.display());
    Ok(())
}
