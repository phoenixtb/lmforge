use std::path::Path;

use anyhow::{Context, Result};

use super::{schema, LmForgeConfig};

/// Load global config from ~/.lmforge/config.toml
pub fn load(path: &Path) -> Result<LmForgeConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let config: LmForgeConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

    // Check schema version
    schema::check_version("config.toml", config.schema_version, 2)?;

    Ok(config)
}

/// Save config to a TOML file (used by `lmforge init` to write defaults)
pub fn save(path: &Path, config: &LmForgeConfig) -> Result<()> {
    let content = toml::to_string_pretty(config)
        .context("Failed to serialize config to TOML")?;

    // Atomic write: write to temp file then rename
    let temp_path = path.with_extension("toml.tmp");
    std::fs::write(&temp_path, &content)
        .with_context(|| format!("Failed to write temp config: {}", temp_path.display()))?;
    std::fs::rename(&temp_path, path)
        .with_context(|| format!("Failed to rename config file: {}", path.display()))?;

    Ok(())
}
