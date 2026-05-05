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

    /// CIDR ranges that bypass `api_key` enforcement entirely.
    /// Defaults cover loopback + RFC1918 private LAN + IPv6 ULA, so a fresh
    /// install binding `0.0.0.0` works on any home/office network without a
    /// token while still rejecting requests from the public internet.
    /// Set to `[]` to require a token from every source.
    #[serde(default = "default_trusted_networks")]
    pub trusted_networks: Vec<String>,

    /// Escape hatch: when true, all requests are allowed without authentication
    /// regardless of `api_key` / `trusted_networks`. A loud warning is logged at
    /// startup. Intended for local development only.
    #[serde(default)]
    pub unsafe_disable_auth: bool,

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
    /// Maximum HTTP request body size accepted by the API, in MB.
    /// Sized for VLM workloads: a 32 MB cap fits ~8 inline base64 images at
    /// typical sizes or a single 300-DPI A4 PDF page render. Raise via
    /// config or `LMFORGE_MAX_BODY_MB` env when serving documents heavy on
    /// dense text/figures; lower it on hostile networks to shrink DoS
    /// surface. Effective cap = `max(this, LMFORGE_MAX_BODY_MB)`.
    #[serde(default = "default_max_request_body_mb")]
    pub max_request_body_mb: usize,
}

pub fn default_max_request_body_mb() -> usize {
    32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    pub keep_alive: String,
    pub max_loaded_models: u32,
    /// Maximum number of inputs per engine call for /v1/embeddings.
    /// Larger batches may OOM or timeout on oMLX/SGLang. Default: 32.
    #[serde(default = "default_embed_batch_size")]
    pub embed_batch_size: usize,
    /// Models to cold-load at daemon startup (serial, with logged progress).
    /// Loading order matters when VRAM is tight — load larger models first so
    /// LRU eviction doesn't churn them. Empty by default (lazy load on first use).
    /// Example: `["qwen3:4b:4bit", "qwen2.5-vl:7b:4bit", "qwen3-embed:0.6b:8bit"]`
    #[serde(default)]
    pub auto_load: Vec<String>,
}

fn default_embed_batch_size() -> usize {
    32
}

/// Default trusted CIDRs for the `trusted_networks` auth allowlist.
/// These ranges are reserved for private use by IANA / RFC1918 / RFC4193 and
/// are unreachable from the public internet without explicit NAT/port-forward.
pub fn default_trusted_networks() -> Vec<String> {
    vec![
        "127.0.0.0/8".to_string(),    // IPv4 loopback
        "::1/128".to_string(),        // IPv6 loopback
        "10.0.0.0/8".to_string(),     // RFC1918 private
        "172.16.0.0/12".to_string(),  // RFC1918 private
        "192.168.0.0/16".to_string(), // RFC1918 private
        "fc00::/7".to_string(),       // RFC4193 IPv6 ULA
        "fe80::/10".to_string(),      // IPv6 link-local
    ]
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            keep_alive: "5m".to_string(),
            max_loaded_models: 0,
            embed_batch_size: default_embed_batch_size(),
            auto_load: Vec::new(),
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
            max_request_body_mb: default_max_request_body_mb(),
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
            trusted_networks: default_trusted_networks(),
            unsafe_disable_auth: false,
            resources: ResourceConfig::default(),
            orchestrator: OrchestratorConfig::default(),
            data_dir_path: None,
        }
    }
}

impl LmForgeConfig {
    /// Get the LMForge data directory path (~/.lmforge/)
    pub fn data_dir(&self) -> PathBuf {
        self.data_dir_path.clone().unwrap_or_else(|| {
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
    // Skip gracefully if the working directory is unavailable (e.g. piped bash execution).
    if let Ok(cwd) = std::env::current_dir() {
        let project_path = cwd.join("lmforge.yaml");
        if project_path.exists() {
            let project = project::load(&project_path)?;
            config = merge_config(config, project);
        }
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
        trusted_networks: if overlay.trusted_networks == default_trusted_networks() {
            // Overlay didn't set it explicitly (still defaults) — keep base.
            base.trusted_networks
        } else {
            overlay.trusted_networks
        },
        unsafe_disable_auth: overlay.unsafe_disable_auth || base.unsafe_disable_auth,
        resources: ResourceConfig {
            max_request_body_mb: if overlay.resources.max_request_body_mb
                == default_max_request_body_mb()
            {
                base.resources.max_request_body_mb
            } else {
                overlay.resources.max_request_body_mb
            },
            ..overlay.resources
        },
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
            embed_batch_size: if overlay.orchestrator.embed_batch_size == default_embed_batch_size()
            {
                // If overlay is at default, keep base (allows user to lower it)
                base.orchestrator.embed_batch_size
            } else {
                overlay.orchestrator.embed_batch_size
            },
            auto_load: if overlay.orchestrator.auto_load.is_empty() {
                base.orchestrator.auto_load
            } else {
                overlay.orchestrator.auto_load
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
        assert_eq!(config.schema_version, 2);
        assert_eq!(config.resources.max_gpu_memory_fraction, 0.75);
        assert_eq!(config.resources.max_concurrent_requests, 4);
    }

    #[test]
    fn test_merge_config_overlay_wins() {
        let base = LmForgeConfig::default();
        let overlay = LmForgeConfig {
            port: 9999,
            log_level: "debug".to_string(),
            ..Default::default()
        };

        let merged = merge_config(base, overlay);
        assert_eq!(merged.port, 9999);
        assert_eq!(merged.log_level, "debug");
    }

    #[test]
    fn test_merge_config_base_kept_when_overlay_empty() {
        let base = LmForgeConfig {
            default_chat_model: "qwen3-8b".to_string(),
            ..Default::default()
        };

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
