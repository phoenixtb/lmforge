use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::LmForgeConfig;

/// Project-level config loaded from lmforge.yaml in the working directory.
/// Only a subset of fields are supported (project-level overrides).
#[derive(Debug, Deserialize)]
struct ProjectConfig {
    pub port: Option<u16>,
    pub bind_address: Option<String>,
    pub log_level: Option<String>,
    pub default_chat_model: Option<String>,
    pub default_embed_model: Option<String>,
}

/// Load project config from lmforge.yaml
pub fn load(path: &Path) -> Result<LmForgeConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read project config: {}", path.display()))?;

    // Parse YAML-style (we use serde_json for simplicity since YAML is a superset of JSON;
    // for proper YAML support we'd add the `serde_yaml` crate later)
    // For now, treat the file as TOML (same key=value structure works)
    let project: ProjectConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse project config: {}", path.display()))?;

    // Build a partial config — only override fields that are set
    let mut config = LmForgeConfig::default();

    if let Some(port) = project.port {
        config.port = port;
    }
    if let Some(bind) = project.bind_address {
        config.bind_address = bind;
    }
    if let Some(level) = project.log_level {
        config.log_level = level;
    }
    if let Some(model) = project.default_chat_model {
        config.default_chat_model = model;
    }
    if let Some(embed) = project.default_embed_model {
        config.default_embed_model = embed;
    }

    Ok(config)
}
