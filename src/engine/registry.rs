use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::hardware::probe::{Arch, GpuVendor, HardwareProfile, Os};

/// Embedded default engine registry
const DEFAULT_ENGINES: &str = include_str!("../../data/engines.toml");

/// A single engine configuration entry from engines.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub id: String,
    pub name: String,
    pub version: String,

    // Matching criteria
    #[serde(default)]
    pub matches_os: Option<String>,
    #[serde(default)]
    pub matches_arch: Option<String>,
    #[serde(default)]
    pub matches_gpu: Option<String>,
    #[serde(default)]
    pub min_vram_gb: Option<f32>,
    #[serde(default)]
    pub matches_fallback: bool,

    // Installation
    pub install_method: String,
    #[serde(default)]
    pub brew_tap: Option<String>,
    #[serde(default)]
    pub brew_formula: Option<String>,
    #[serde(default)]
    pub pip_fallback: Option<String>,
    #[serde(default)]
    pub pip_package: Option<String>,
    #[serde(default)]
    pub preflight: Vec<String>,
    #[serde(default)]
    pub min_disk_gb: Option<u32>,
    #[serde(default)]
    pub binary: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub asset_pattern: Option<String>,

    // Runtime
    pub model_format: String,
    pub hf_org: String,
    pub start_cmd: String,
    pub start_args: Vec<String>,
    pub health_endpoint: String,
    #[serde(default)]
    pub supports_embeddings: bool,

    /// Lower number = higher priority
    #[serde(default = "default_priority")]
    pub priority: u32,
}

fn default_priority() -> u32 {
    100
}

/// The engine registry — parsed from engines.toml
#[derive(Debug, Deserialize)]
struct EngineRegistryFile {
    engine: Vec<EngineConfig>,
}

#[derive(Debug)]
pub struct EngineRegistry {
    engines: Vec<EngineConfig>,
}

impl EngineRegistry {
    /// Load the registry from the embedded default + optional user override
    pub fn load(user_override_path: Option<&std::path::Path>) -> Result<Self> {
        let mut registry: EngineRegistryFile = toml::from_str(DEFAULT_ENGINES)
            .context("Failed to parse embedded engines.toml")?;

        debug!("Loaded {} default engines", registry.engine.len());

        // Merge user overrides if present
        if let Some(path) = user_override_path {
            if path.exists() {
                let user_content = std::fs::read_to_string(path)
                    .context("Failed to read user engines.toml")?;
                let user_registry: EngineRegistryFile = toml::from_str(&user_content)
                    .context("Failed to parse user engines.toml")?;

                for user_engine in user_registry.engine {
                    // Override existing or add new
                    if let Some(existing) = registry.engine.iter_mut().find(|e| e.id == user_engine.id) {
                        info!(engine = %user_engine.id, "User override for engine");
                        *existing = user_engine;
                    } else {
                        info!(engine = %user_engine.id, "User added custom engine");
                        registry.engine.push(user_engine);
                    }
                }
            }
        }

        Ok(Self {
            engines: registry.engine,
        })
    }

    /// Select the best engine for the given hardware profile.
    /// Returns the highest-priority engine whose matching criteria satisfy the profile.
    pub fn select(&self, profile: &HardwareProfile) -> Result<&EngineConfig> {
        let mut candidates: Vec<&EngineConfig> = self
            .engines
            .iter()
            .filter(|e| engine_matches(e, profile))
            .collect();

        if candidates.is_empty() {
            bail!(
                "No engine matches hardware: {:?} {:?} GPU:{:?} VRAM:{:.1}GB",
                profile.os, profile.arch, profile.gpu_vendor, profile.vram_gb
            );
        }

        // Sort by priority (lower = better)
        candidates.sort_by_key(|e| e.priority);

        let selected = candidates[0];
        info!(
            engine = %selected.id,
            version = %selected.version,
            priority = selected.priority,
            "Engine selected"
        );

        Ok(selected)
    }

    /// Convert a selected EngineConfig into its respective functional Adapter natively
    pub fn create_adapter(config: &EngineConfig) -> Result<crate::engine::adapter::EngineAdapterInstance> {
        match config.id.as_str() {
            "omlx" => Ok(crate::engine::adapter::EngineAdapterInstance::Omlx(crate::engine::adapters::omlx::OmlxAdapter::default())),
            "sglang" => Ok(crate::engine::adapter::EngineAdapterInstance::Sglang(crate::engine::adapters::sglang::SglangAdapter::default())),
            "llamacpp" => Ok(crate::engine::adapter::EngineAdapterInstance::Llamacpp(crate::engine::adapters::llamacpp::LlamacppAdapter::default())),
            _ => bail!("Unrecognized engine adapter ID mapping: {}", config.id),
        }
    }

    /// Get all registered engines
    pub fn all(&self) -> &[EngineConfig] {
        &self.engines
    }

    /// Get a specific engine by ID
    pub fn get(&self, id: &str) -> Option<&EngineConfig> {
        self.engines.iter().find(|e| e.id == id)
    }
}

/// Check if an engine's matching criteria are satisfied by the profile
fn engine_matches(engine: &EngineConfig, profile: &HardwareProfile) -> bool {
    // Fallback engines always match (lowest priority)
    if engine.matches_fallback {
        return true;
    }

    // Check OS
    if let Some(ref required_os) = engine.matches_os {
        let profile_os = match profile.os {
            Os::Darwin => "darwin",
            Os::Linux => "linux",
            Os::Windows => "windows",
            Os::Unknown => "unknown",
        };
        if profile_os != required_os {
            return false;
        }
    }

    // Check architecture
    if let Some(ref required_arch) = engine.matches_arch {
        let profile_arch = match profile.arch {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
            Arch::Unknown => "unknown",
        };
        if profile_arch != required_arch {
            return false;
        }
    }

    // Check GPU vendor
    if let Some(ref required_gpu) = engine.matches_gpu {
        let profile_gpu = match profile.gpu_vendor {
            GpuVendor::Apple => "apple",
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
            GpuVendor::None => "none",
        };
        if profile_gpu != required_gpu {
            return false;
        }
    }

    // Check minimum VRAM
    if let Some(min_vram) = engine.min_vram_gb {
        if profile.vram_gb < min_vram {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple_silicon() -> HardwareProfile {
        HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            is_tegra: false,
            gpu_vendor: GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".to_string(),
        }
    }

    fn nvidia_large() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 48.0,
            unified_mem: false,
            total_ram_gb: 64.0,
            cpu_cores: 16,
            cpu_model: "AMD Ryzen 9".to_string(),
        }
    }

    fn nvidia_small() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 8.0,
            unified_mem: false,
            total_ram_gb: 32.0,
            cpu_cores: 8,
            cpu_model: "Intel i7".to_string(),
        }
    }

    fn cpu_only() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            is_tegra: false,
            gpu_vendor: GpuVendor::None,
            vram_gb: 0.0,
            unified_mem: false,
            total_ram_gb: 16.0,
            cpu_cores: 4,
            cpu_model: "Intel i5".to_string(),
        }
    }

    #[test]
    fn test_parse_default_registry() {
        let registry = EngineRegistry::load(None).unwrap();
        assert_eq!(registry.all().len(), 3);
        assert!(registry.get("omlx").is_some());
        assert!(registry.get("sglang").is_some());
        assert!(registry.get("llamacpp").is_some());
    }

    #[test]
    fn test_select_omlx_on_apple_silicon() {
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&apple_silicon()).unwrap();
        assert_eq!(selected.id, "omlx");
        assert_eq!(selected.version, "0.3.0");
    }

    #[test]
    fn test_select_sglang_on_large_nvidia() {
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&nvidia_large()).unwrap();
        assert_eq!(selected.id, "sglang");
    }

    #[test]
    fn test_select_llamacpp_on_small_nvidia() {
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&nvidia_small()).unwrap();
        // SGLang needs 24GB+ VRAM, so 8GB falls to llama.cpp
        assert_eq!(selected.id, "llamacpp");
    }

    #[test]
    fn test_select_llamacpp_on_cpu_only() {
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&cpu_only()).unwrap();
        assert_eq!(selected.id, "llamacpp");
    }

    #[test]
    fn test_engine_priority_order() {
        let registry = EngineRegistry::load(None).unwrap();
        let omlx = registry.get("omlx").unwrap();
        let sglang = registry.get("sglang").unwrap();
        let llama = registry.get("llamacpp").unwrap();
        assert!(omlx.priority < sglang.priority);
        assert!(sglang.priority < llama.priority);
    }

    #[test]
    fn test_pinned_versions() {
        let registry = EngineRegistry::load(None).unwrap();
        assert_eq!(registry.get("omlx").unwrap().version, "0.3.0");
        assert_eq!(registry.get("llamacpp").unwrap().version, "b8558");
        assert_eq!(registry.get("sglang").unwrap().version, "0.5.9");
    }
}
