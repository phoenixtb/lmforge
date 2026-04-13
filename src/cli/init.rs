use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;
use crate::hardware;

/// `lmforge init` — Probe hardware, select engine, install if needed
pub async fn run(config: &LmForgeConfig) -> Result<()> {
    info!("Running lmforge init...");

    // Ensure data directory exists
    let data_dir = config.data_dir();
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(data_dir.join("engines"))?;
        std::fs::create_dir_all(data_dir.join("models"))?;
        std::fs::create_dir_all(data_dir.join("logs"))?;
        info!("Created LMForge data directory at {}", data_dir.display());
    }

    // Hardware probe
    println!("⚙ Detecting hardware...");
    let profile = hardware::detect()?;
    println!("  {}", profile);
    println!();

    // Save hardware profile for later use
    let profile_json = serde_json::to_string_pretty(&profile)?;
    let profile_path = data_dir.join("hardware.json");
    std::fs::write(&profile_path, &profile_json)?;
    info!(path = %profile_path.display(), "Hardware profile saved");

    // Print summary
    println!("  OS:         {:?}", profile.os);
    println!("  Arch:       {:?}", profile.arch);
    println!("  GPU:        {:?}", profile.gpu_vendor);
    println!("  VRAM:       {:.1} GB{}", profile.vram_gb,
        if profile.unified_mem { " (unified memory)" } else { "" });
    println!("  RAM:        {:.1} GB", profile.total_ram_gb);
    println!("  CPU:        {} ({} cores)", profile.cpu_model, profile.cpu_cores);
    println!("  Quant tier: {}", crate::hardware::vram::quant_tier(profile.vram_gb));
    println!();

    // Engine selection
    println!("⚙ Selecting engine...");
    let user_engines_path = data_dir.join("engines.toml");
    let registry = crate::engine::EngineRegistry::load(
        if user_engines_path.exists() { Some(user_engines_path.as_path()) } else { None }
    )?;
    let selected = registry.select(&profile)?;
    println!("  Selected: {} v{} ({})", selected.name, selected.version, selected.id);
    println!("  Format:   {}", selected.model_format);
    println!("  Install:  {}", selected.install_method);
    println!();

    // Engine installation
    println!("⚙ Installing engine...");
    let install_result = crate::engine::installer::install(selected, &profile, &data_dir).await?;
    println!("  Method: {}", install_result.method_used);
    println!();

    println!("\n✓ LMForge initialized at {}", data_dir.display());
    Ok(())
}
