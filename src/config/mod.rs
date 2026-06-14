pub mod global;
pub mod project;
pub mod schema;

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli::Cli;

/// Merged LMForge configuration from all sources.
/// Precedence: CLI flags > project yaml > global toml > built-in defaults
///
/// Every top-level field carries `#[serde(default)]` so a partial
/// `config.toml` with only the sections the user wants to override loads
/// cleanly. Missing fields fall back to the matching `default_*` helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmForgeConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub default_chat_model: String,
    #[serde(default)]
    pub default_embed_model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub catalogs_dir: Option<String>,

    /// Override for the LMForge data directory (engines, logs, models.json,
    /// catalogs). When unset, falls back to `LMFORGE_DATA_DIR` env then
    /// `~/.lmforge`. Per-VM local state in the virtio-fs sharing model.
    #[serde(default)]
    pub data_dir: Option<String>,

    /// Override for the model weights directory. When unset, falls back to
    /// `LMFORGE_MODELS_DIR` env then `{data_dir}/models`. Decoupling this from
    /// `data_dir` lets multiple VMs share one weights volume (virtio-fs) while
    /// keeping engines/logs/index local.
    #[serde(default)]
    pub models_dir: Option<String>,

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

    /// Speculative-decoding defaults. The adapter resolves per-launch
    /// overrides on top of this — see `engine::speculative::resolve`.
    #[serde(default)]
    pub speculative: crate::engine::speculative::SpeculativeConfig,

    /// Resolved data directory path (not serialized to file)
    #[serde(skip)]
    data_dir_path: Option<PathBuf>,

    /// Resolved config file path (not serialized to file).
    /// Set by `load()`; defaults to `bootstrap_config_path()` when None.
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

/// All `ResourceConfig` fields are individually `#[serde(default)]` so a
/// `[resources]` table containing only the knobs the operator cares about
/// loads without needing to copy every default into config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceConfig {
    #[serde(default = "default_max_gpu_memory_fraction")]
    pub max_gpu_memory_fraction: f32,
    #[serde(default)]
    pub max_gpu_memory_gb: Option<f32>,
    #[serde(default)]
    pub max_system_memory_gb: Option<f32>,
    #[serde(default = "default_min_free_disk_gb")]
    pub min_free_disk_gb: u32,
    #[serde(default)]
    pub max_model_storage_gb: Option<u32>,
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: u32,
    #[serde(default = "default_request_queue_size")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    #[serde(default = "default_keep_alive")]
    pub keep_alive: String,
    #[serde(default)]
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

// ── Default helpers ─────────────────────────────────────────────────────────
// Kept as free functions because serde's `default = "path"` attribute
// requires a callable. Centralised here so `Default` impls and `#[serde]`
// attributes always agree.

fn default_schema_version() -> u32 {
    2
}
fn default_port() -> u16 {
    11430
}
fn default_bind_address() -> String {
    "127.0.0.1".to_string()
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_keep_alive() -> String {
    "5m".to_string()
}
fn default_max_gpu_memory_fraction() -> f32 {
    0.75
}
fn default_min_free_disk_gb() -> u32 {
    10
}
fn default_max_concurrent_requests() -> u32 {
    4
}
fn default_request_queue_size() -> u32 {
    32
}
pub fn default_max_request_body_mb() -> usize {
    32
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

/// Returns the bootstrap path for LMForge's `config.toml`.
///
/// This path is FIXED at `~/.lmforge/config.toml` regardless of `data_dir`.
/// Decoupling config from data_dir is intentional: changing `data_dir` must not
/// change where the config file lives on next boot, otherwise the new settings
/// would never be read back. Override via `LMFORGE_CONFIG` env or `--config` flag
/// (applied in `load()`). Never derived from `data_dir`.
pub fn bootstrap_config_path() -> PathBuf {
    if let Ok(s) = std::env::var("LMFORGE_CONFIG")
        && !s.trim().is_empty()
    {
        return PathBuf::from(s.trim());
    }
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".lmforge")
        .join("config.toml")
}

/// Returns the path for the pending-migration manifest.
/// Lives next to the bootstrap config (honors `--config` / `LMFORGE_CONFIG`),
/// so it stays consistent with where the config is read/written and remains
/// isolated under a custom config path. Used by `POST /lf/storage/apply` to
/// persist migration intent across restarts.
pub fn pending_migration_path() -> PathBuf {
    let config_path = bootstrap_config_path();
    match config_path.parent() {
        Some(dir) => dir.join("pending-migration.json"),
        None => PathBuf::from("pending-migration.json"),
    }
}

/// Normalize a user-supplied directory path string into a `PathBuf`.
///
/// - Trims surrounding whitespace.
/// - Expands a leading `~` / `~/...` to the user's home directory.
/// - Leaves absolute paths untouched. On Windows this includes drive-letter
///   paths (`D:\...`), extended-length prefixes (`\\?\...`), and UNC shares
///   such as virtio-fs mounts (`\\virtiofs\...`); these all report
///   `is_absolute()` and pass through verbatim.
///
/// Relative inputs are returned as-is (resolved against CWD by the OS at use
/// time) — we intentionally avoid canonicalizing here so a not-yet-mounted
/// shared volume can still be configured.
pub fn normalize_dir(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(trimmed));
    }
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(trimmed)
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            keep_alive: default_keep_alive(),
            max_loaded_models: 0,
            embed_batch_size: default_embed_batch_size(),
            auto_load: Vec::new(),
        }
    }
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            max_gpu_memory_fraction: default_max_gpu_memory_fraction(),
            max_gpu_memory_gb: None,
            max_system_memory_gb: None,
            min_free_disk_gb: default_min_free_disk_gb(),
            max_model_storage_gb: None,
            max_concurrent_requests: default_max_concurrent_requests(),
            request_queue_size: default_request_queue_size(),
            max_request_body_mb: default_max_request_body_mb(),
        }
    }
}

impl Default for LmForgeConfig {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            port: default_port(),
            bind_address: default_bind_address(),
            log_level: default_log_level(),
            default_chat_model: String::new(),
            default_embed_model: String::new(),
            api_key: None,
            catalogs_dir: None,
            data_dir: None,
            models_dir: None,
            trusted_networks: default_trusted_networks(),
            unsafe_disable_auth: false,
            resources: ResourceConfig::default(),
            orchestrator: OrchestratorConfig::default(),
            speculative: crate::engine::speculative::SpeculativeConfig::default(),
            data_dir_path: None,
            config_path: None,
        }
    }
}

impl LmForgeConfig {
    /// Get the LMForge data directory path.
    ///
    /// Precedence: runtime `data_dir_path` override (CLI flag / env, set in
    /// [`load`]) > `data_dir` config field > `LMFORGE_DATA_DIR` env > `~/.lmforge`.
    /// The runtime `--data-dir` / `LMFORGE_DATA_DIR` override (set in [`load`]),
    /// if one is active. `None` means the data dir comes from the config field
    /// or the built-in default. Used by `lmforge init` to pin an install-time
    /// `--data-dir` choice into the persisted config.
    pub fn data_dir_override(&self) -> Option<&std::path::Path> {
        self.data_dir_path.as_deref()
    }

    pub fn data_dir(&self) -> PathBuf {
        if let Some(ref p) = self.data_dir_path {
            return p.clone();
        }
        if let Some(ref s) = self.data_dir {
            return normalize_dir(s);
        }
        if let Ok(s) = std::env::var("LMFORGE_DATA_DIR")
            && !s.trim().is_empty()
        {
            return normalize_dir(&s);
        }
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join(".lmforge")
    }

    /// Get the model weights directory path.
    ///
    /// Precedence: `models_dir` config field > `LMFORGE_MODELS_DIR` env >
    /// `{data_dir}/models`. Decoupled from `data_dir` so a shared virtio-fs
    /// volume can hold only weights while engines/logs/index stay per-VM.
    pub fn models_dir(&self) -> PathBuf {
        if let Some(ref s) = self.models_dir {
            return normalize_dir(s);
        }
        if let Ok(s) = std::env::var("LMFORGE_MODELS_DIR")
            && !s.trim().is_empty()
        {
            return normalize_dir(&s);
        }
        self.data_dir().join("models")
    }

    /// Get the catalogs directory path
    pub fn catalogs_dir(&self) -> PathBuf {
        self.catalogs_dir
            .as_ref()
            .map(|s| normalize_dir(s))
            .unwrap_or_else(|| self.data_dir().join("catalogs"))
    }

    /// Resolve the effective data/models directories for a hypothetical pair of
    /// field values, using the normal startup precedence (env > field > default)
    /// but WITHOUT any runtime `--data-dir` override. Used by the storage-apply
    /// endpoint to compute the directories the daemon will use post-restart for
    /// given field values (including `None` = reset-to-default).
    pub fn resolve_dirs(data_dir: Option<&str>, models_dir: Option<&str>) -> (PathBuf, PathBuf) {
        let cfg = LmForgeConfig {
            data_dir: data_dir.map(|s| s.to_string()),
            models_dir: models_dir.map(|s| s.to_string()),
            ..Default::default()
        };
        (cfg.data_dir(), cfg.models_dir())
    }

    /// Returns the path this config was loaded from (and should be saved to).
    /// Defaults to `bootstrap_config_path()` when not explicitly resolved.
    pub fn config_path(&self) -> PathBuf {
        self.config_path
            .clone()
            .unwrap_or_else(bootstrap_config_path)
    }

    /// Save current configuration to the bootstrap config path.
    ///
    /// The path is always `~/.lmforge/config.toml` (or `--config` / `LMFORGE_CONFIG`
    /// override) — never inside `data_dir`. This ensures that after a `data_dir`
    /// change the new settings are found on next startup.
    pub fn save(&self) -> Result<()> {
        let path = self.config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("Cannot create config directory {}: {}", parent.display(), e)
            })?;
        }
        crate::config::global::save(&path, self)
    }
}

/// Load configuration with full precedence chain:
/// CLI flags > project yaml > global toml > built-in defaults
pub fn load(cli: &Cli) -> Result<LmForgeConfig> {
    // Start with defaults
    let mut config = LmForgeConfig::default();

    // Apply `--data-dir` early as the runtime override for the data_dir() accessor.
    // Note: config.toml is bootstrap-pinned at `~/.lmforge/config.toml` (or
    // LMFORGE_CONFIG / --config) and is NOT located relative to data_dir.
    // This decoupling lets a UI-driven data_dir change be read back on restart.
    if let Some(ref d) = cli.data_dir {
        config.data_dir_path = Some(normalize_dir(d));
    }

    // Layer 1: Global config (bootstrap path — fixed at ~/.lmforge/config.toml
    // unless --config or LMFORGE_CONFIG overrides it; never inside data_dir).
    let global_path = if let Some(ref p) = cli.config {
        PathBuf::from(p)
    } else {
        bootstrap_config_path()
    };
    // Store the resolved config path so save() writes back to the same location.
    config.config_path = Some(global_path.clone());

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
    // `--data-dir` was applied to `data_dir_path` before layer 1; re-assert it
    // here so a `data_dir` in the loaded config.toml can't shadow the flag.
    if let Some(ref d) = cli.data_dir {
        config.data_dir_path = Some(normalize_dir(d));
    }
    // `--models-dir` wins over env and config file (accessor checks the field
    // first), so store it directly on the config.
    if let Some(ref m) = cli.models_dir {
        config.models_dir = Some(m.clone());
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
        data_dir: overlay.data_dir.or(base.data_dir),
        models_dir: overlay.models_dir.or(base.models_dir),
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
        speculative: overlay.speculative,
        data_dir_path: overlay.data_dir_path.or(base.data_dir_path),
        config_path: overlay.config_path.or(base.config_path),
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

    #[test]
    fn test_models_dir_defaults_under_data_dir() {
        let config = LmForgeConfig::default();
        assert_eq!(config.models_dir(), config.data_dir().join("models"));
    }

    #[test]
    fn test_models_dir_field_overrides_default() {
        let config = LmForgeConfig {
            models_dir: Some("/srv/shared/weights".to_string()),
            ..Default::default()
        };
        assert_eq!(config.models_dir(), PathBuf::from("/srv/shared/weights"));
    }

    #[test]
    fn test_data_dir_field_and_models_dir_relationship() {
        // models_dir defaults relative to the configured data_dir field.
        let config = LmForgeConfig {
            data_dir: Some("/var/lib/lmforge".to_string()),
            ..Default::default()
        };
        assert_eq!(config.data_dir(), PathBuf::from("/var/lib/lmforge"));
        assert_eq!(
            config.models_dir(),
            PathBuf::from("/var/lib/lmforge/models")
        );
    }

    #[test]
    fn test_data_dir_path_runtime_override_wins_over_field() {
        // The runtime override (CLI --data-dir) takes precedence over the field.
        let config = LmForgeConfig {
            data_dir: Some("/from/config".to_string()),
            data_dir_path: Some(PathBuf::from("/from/flag")),
            ..Default::default()
        };
        assert_eq!(config.data_dir(), PathBuf::from("/from/flag"));
    }

    #[test]
    fn test_merge_config_carries_storage_fields() {
        let base = LmForgeConfig {
            data_dir: Some("/base/data".to_string()),
            models_dir: Some("/base/models".to_string()),
            ..Default::default()
        };
        let overlay = LmForgeConfig::default(); // both None
        let merged = merge_config(base, overlay);
        assert_eq!(merged.data_dir.as_deref(), Some("/base/data"));
        assert_eq!(merged.models_dir.as_deref(), Some("/base/models"));
    }

    #[test]
    fn bootstrap_config_path_is_not_inside_data_dir() {
        // Even when data_dir is overridden, the bootstrap config path must stay
        // at ~/.lmforge/config.toml so a data_dir change is readable on restart.
        let config = LmForgeConfig {
            data_dir: Some("/custom/data".to_string()),
            ..Default::default()
        };
        let config_p = config.config_path();
        let data_p = config.data_dir();
        assert!(
            !config_p.starts_with(&data_p),
            "config.toml must not be inside data_dir: config_path={config_p:?}, data_dir={data_p:?}"
        );
        assert!(
            config_p.ends_with("config.toml"),
            "must end with config.toml: {config_p:?}"
        );
    }

    #[test]
    fn config_path_from_load_is_persisted_through_merge() {
        // A config_path set during load() must survive merge_config().
        let base = LmForgeConfig {
            config_path: Some(PathBuf::from("/custom/path/config.toml")),
            ..Default::default()
        };
        let overlay = LmForgeConfig::default(); // config_path = None
        let merged = merge_config(base, overlay);
        assert_eq!(
            merged.config_path,
            Some(PathBuf::from("/custom/path/config.toml")),
            "config_path must survive merge when overlay has None"
        );
    }

    #[test]
    fn test_normalize_dir_passthrough_and_tilde() {
        assert_eq!(
            normalize_dir("  /srv/models  "),
            PathBuf::from("/srv/models")
        );
        let home = dirs::home_dir().unwrap();
        assert_eq!(normalize_dir("~"), home);
        assert_eq!(normalize_dir("~/models"), home.join("models"));
    }

    /// Regression: a config.toml that only contains `[orchestrator]` (or any
    /// other subset) used to fail with `missing field schema_version` on
    /// startup. With per-field `#[serde(default)]` it must now load and fall
    /// back to defaults for everything the operator left unset.
    #[test]
    fn partial_toml_loads_with_defaults() {
        let toml_str = r#"
[orchestrator]
keep_alive = "10m"
max_loaded_models = 4
auto_load = ["qwen3-embed:0.6b:8bit", "qwen2.5-vl:3b:4bit"]
"#;
        let cfg: LmForgeConfig = toml::from_str(toml_str).expect("partial toml must parse");
        assert_eq!(cfg.schema_version, 2);
        assert_eq!(cfg.port, 11430);
        assert_eq!(cfg.bind_address, "127.0.0.1");
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.orchestrator.keep_alive, "10m");
        assert_eq!(cfg.orchestrator.max_loaded_models, 4);
        assert_eq!(cfg.orchestrator.auto_load.len(), 2);
        assert_eq!(cfg.resources.max_request_body_mb, 32);
        assert_eq!(cfg.resources.max_concurrent_requests, 4);
    }

    /// Empty TOML must also work — every field has a default.
    #[test]
    fn empty_toml_loads_as_defaults() {
        let cfg: LmForgeConfig = toml::from_str("").expect("empty toml must parse");
        let defaults = LmForgeConfig::default();
        assert_eq!(cfg.schema_version, defaults.schema_version);
        assert_eq!(cfg.port, defaults.port);
        assert_eq!(
            cfg.resources.max_request_body_mb,
            defaults.resources.max_request_body_mb
        );
        assert_eq!(
            cfg.orchestrator.embed_batch_size,
            defaults.orchestrator.embed_batch_size
        );
    }

    /// `[resources]` with only one knob set: rest must inherit defaults.
    #[test]
    fn partial_resources_table_inherits_defaults() {
        let toml_str = r#"
[resources]
max_request_body_mb = 64
"#;
        let cfg: LmForgeConfig = toml::from_str(toml_str).expect("must parse");
        assert_eq!(cfg.resources.max_request_body_mb, 64);
        assert_eq!(cfg.resources.max_concurrent_requests, 4);
        assert_eq!(cfg.resources.request_queue_size, 32);
        assert_eq!(cfg.resources.max_gpu_memory_fraction, 0.75);
    }

    /// Empty `[speculative]` block must load and inherit every default.
    #[test]
    fn empty_speculative_block_inherits_defaults() {
        let cfg: LmForgeConfig = toml::from_str(
            r#"
[speculative]
"#,
        )
        .expect("must parse");
        let defaults = crate::engine::speculative::SpeculativeConfig::default();
        assert_eq!(cfg.speculative.mode, defaults.mode);
        assert_eq!(cfg.speculative.draft_max, defaults.draft_max);
        assert_eq!(cfg.speculative.draft_min, defaults.draft_min);
        assert!((cfg.speculative.draft_p_min - defaults.draft_p_min).abs() < 1e-6);
        assert_eq!(cfg.speculative.draft_gpu_layers, defaults.draft_gpu_layers);
        assert_eq!(cfg.speculative.vram_safety_mib, defaults.vram_safety_mib);
        assert!(cfg.speculative.draft_model.is_none());
    }

    /// Partial `[speculative]` — only `mode` overridden, rest must
    /// inherit defaults. Regression guard against accidentally moving
    /// to `#[serde(default = ...)]` on the wrong layer.
    #[test]
    fn partial_speculative_block_inherits_defaults() {
        let cfg: LmForgeConfig = toml::from_str(
            r#"
[speculative]
mode = "off"
"#,
        )
        .expect("must parse");
        assert_eq!(
            cfg.speculative.mode,
            crate::engine::speculative::SpecMode::Off
        );
        assert_eq!(cfg.speculative.draft_max, 16);
        assert!((cfg.speculative.draft_p_min - 0.75).abs() < 1e-6);
        assert_eq!(cfg.speculative.vram_safety_mib, 1024);
    }

    /// Full override — every speculative knob is set.
    #[test]
    fn full_speculative_block_overrides_all_fields() {
        let cfg: LmForgeConfig = toml::from_str(
            r#"
[speculative]
mode = "mtp"
draft_max = 4
draft_min = 1
draft_p_min = 0.65
draft_gpu_layers = 99
vram_safety_mib = 2048
draft_model = "/models/draft.gguf"
"#,
        )
        .expect("must parse");
        use crate::engine::speculative::SpecMode;
        assert_eq!(cfg.speculative.mode, SpecMode::Mtp);
        assert_eq!(cfg.speculative.draft_max, 4);
        assert_eq!(cfg.speculative.draft_min, 1);
        assert!((cfg.speculative.draft_p_min - 0.65).abs() < 1e-6);
        assert_eq!(cfg.speculative.draft_gpu_layers, 99);
        assert_eq!(cfg.speculative.vram_safety_mib, 2048);
        assert_eq!(
            cfg.speculative.draft_model.as_deref(),
            Some("/models/draft.gguf")
        );
    }

    /// `draft-model` (kebab-case) is the documented variant — must parse.
    #[test]
    fn speculative_mode_accepts_draft_model_kebab() {
        let cfg: LmForgeConfig = toml::from_str(
            r#"
[speculative]
mode = "draft-model"
"#,
        )
        .expect("must parse");
        assert_eq!(
            cfg.speculative.mode,
            crate::engine::speculative::SpecMode::DraftModel
        );
    }
}
