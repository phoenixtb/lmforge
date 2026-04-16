pub mod global;
pub mod project;
pub mod schema;

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli::Cli;

/// Merged LMForge configuration from all sources.
/// Precedence: CLI flags > project yaml > global toml > built-in defaults
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmForgeConfig {
    pub schema_version: u32,
    pub port: u16,
    pub bind_address: String,
    pub log_level: String,
    pub default_chat_model: String,
    pub default_embed_model: String,
    pub api_key: Option<String>,
    pub catalogs_dir: Option<String>,

    #[serde(default)]
    pub resources: ResourceConfig,

    #[serde(default)]
    pub orchestrator: OrchestratorConfig,

    /// Resolved data directory path (not serialized to file)
    #[serde(skip)]
    data_dir_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceConfig {
    pub max_gpu_memory_fraction: f32,
    pub max_gpu_memory_gb: Option<f32>,
    pub max_system_memory_gb: Option<f32>,
    pub min_free_disk_gb: u32,
    pub max_model_storage_gb: Option<u32>,
    pub max_concurrent_requests: u32,
    pub request_queue_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    pub keep_alive: String,
    pub max_loaded_models: u32,
    /// Maximum number of inputs per engine call for /v1/embeddings.
    /// Larger batches may OOM or timeout on oMLX/SGLang. Default: 32.
    #[serde(default = "default_embed_batch_size")]
    pub embed_batch_size: usize,
}

fn default_embed_batch_size() -> usize { 32 }

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            keep_alive: "5m".to_string(),
            max_loaded_models: 0,
            embed_batch_size: default_embed_batch_size(),
        }
    }
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            max_gpu_memory_fraction: 0.75,
            max_gpu_memory_gb: None,
            max_system_memory_gb: None,
            min_free_disk_gb: 10,
            max_model_storage_gb: None,
            max_concurrent_requests: 4,
            request_queue_size: 32,
        }
    }
}

impl Default for LmForgeConfig {
    fn default() -> Self {
        Self {
            schema_version: 2,
            port: 11430,
            bind_address: "127.0.0.1".to_string(),
            log_level: "info".to_string(),
            default_chat_model: String::new(),
            default_embed_model: String::new(),
            api_key: None,
            catalogs_dir: None,
            resources: ResourceConfig::default(),
            orchestrator: OrchestratorConfig::default(),
            data_dir_path: None,
        }
    }
}

impl LmForgeConfig {
    /// Get the LMForge data directory path (~/.lmforge/)
    pub fn data_dir(&self) -> PathBuf {
        self.data_dir_path
            .clone()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .expect("Could not determine home directory")
                    .join(".lmforge")
            })
    }

    /// Get the catalogs directory path
    pub fn catalogs_dir(&self) -> PathBuf {
        self.catalogs_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.data_dir().join("catalogs"))
    }

    /// Save current configuration globally mapping to ~/.lmforge/config.toml
    pub fn save(&self) -> Result<()> {
        let path = self.data_dir().join("config.toml");
        crate::config::global::save(&path, self)
    }
}

/// Load configuration with full precedence chain:
/// CLI flags > project yaml > global toml > built-in defaults
pub fn load(cli: &Cli) -> Result<LmForgeConfig> {
    // Start with defaults
    let mut config = LmForgeConfig::default();

    // Layer 1: Global config (~/.lmforge/config.toml)
    let global_path = if let Some(ref p) = cli.config {
        PathBuf::from(p)
    } else {
        config.data_dir().join("config.toml")
    };

    if global_path.exists() {
        let global = global::load(&global_path)?;
        config = merge_config(config, global);
    }

    // Layer 2: Project config (lmforge.yaml in cwd)
    let project_path = std::env::current_dir()?.join("lmforge.yaml");
    if project_path.exists() {
        let project = project::load(&project_path)?;
        config = merge_config(config, project);
    }

    // Layer 3: CLI flag overrides
    if let Some(ref cat_dir) = cli.catalogs_dir {
        config.catalogs_dir = Some(cat_dir.clone());
    }
    if let Some(ref level) = cli.log_level {
        config.log_level = level.clone();
    }

    Ok(config)
}

/// Merge two configs. `overlay` values override `base` for non-default fields.
fn merge_config(base: LmForgeConfig, overlay: LmForgeConfig) -> LmForgeConfig {
    LmForgeConfig {
        schema_version: overlay.schema_version,
        port: overlay.port,
        bind_address: if overlay.bind_address.is_empty() {
            base.bind_address
        } else {
            overlay.bind_address
        },
        log_level: if overlay.log_level.is_empty() {
            base.log_level
        } else {
            overlay.log_level
        },
        default_chat_model: if overlay.default_chat_model.is_empty() {
            base.default_chat_model
        } else {
            overlay.default_chat_model
        },
        default_embed_model: if overlay.default_embed_model.is_empty() {
            base.default_embed_model
        } else {
            overlay.default_embed_model
        },
        api_key: overlay.api_key.or(base.api_key),
        catalogs_dir: overlay.catalogs_dir.or(base.catalogs_dir),
        resources: overlay.resources,
        orchestrator: OrchestratorConfig {
            keep_alive: if overlay.orchestrator.keep_alive.is_empty() {
                base.orchestrator.keep_alive
            } else {
                overlay.orchestrator.keep_alive
            },
            max_loaded_models: if overlay.orchestrator.max_loaded_models == 0 {
                base.orchestrator.max_loaded_models
            } else {
                overlay.orchestrator.max_loaded_models
            },
            embed_batch_size: if overlay.orchestrator.embed_batch_size == default_embed_batch_size() {
                // If overlay is at default, keep base (allows user to lower it)
                base.orchestrator.embed_batch_size
            } else {
                overlay.orchestrator.embed_batch_size
            },
        },
        data_dir_path: overlay.data_dir_path.or(base.data_dir_path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = LmForgeConfig::default();
        assert_eq!(config.port, 11430);
        assert_eq!(config.bind_address, "127.0.0.1");
        assert_eq!(config.log_level, "info");
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.resources.max_gpu_memory_fraction, 0.75);
        assert_eq!(config.resources.max_concurrent_requests, 4);
    }

    #[test]
    fn test_merge_config_overlay_wins() {
        let base = LmForgeConfig::default();
        let mut overlay = LmForgeConfig::default();
        overlay.port = 9999;
        overlay.log_level = "debug".to_string();

        let merged = merge_config(base, overlay);
        assert_eq!(merged.port, 9999);
        assert_eq!(merged.log_level, "debug");
    }

    #[test]
    fn test_merge_config_base_kept_when_overlay_empty() {
        let mut base = LmForgeConfig::default();
        base.default_chat_model = "qwen3-8b".to_string();

        let overlay = LmForgeConfig::default(); // default_chat_model is empty
        let merged = merge_config(base, overlay);
        assert_eq!(merged.default_chat_model, "qwen3-8b");
    }

    #[test]
    fn test_data_dir_default() {
        let config = LmForgeConfig::default();
        let data_dir = config.data_dir();
        assert!(data_dir.ends_with(".lmforge"));
    }
}
