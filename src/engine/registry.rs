use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::hardware::probe::{Arch, ComputeCap, GpuVendor, HardwareProfile, Os};

/// Embedded default engine registry
const DEFAULT_ENGINES: &str = include_str!("../../data/engines.toml");

/// Tier classification for an engine. Drives the selector and the install UX.
/// See `docs/architecture/ADR-001-engine-tiers.md` for the canonical definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EngineTier {
    /// Bundled with releases. Auto-selected. Currently `llamacpp` and `omlx`.
    Default,
    /// Requires explicit `lmforge engine install <id>`. Lives in its own venv.
    OptIn,
    /// Kept in-tree as cheap insurance but never auto-selected. User must
    /// pass `--engine <id>` and confirm the warning prompt.
    Experimental,
    /// Not yet classified. Treated as `Default` for backward compatibility
    /// with v1 `engines.toml` entries that pre-date this field.
    #[default]
    Unspecified,
}

/// A single engine configuration entry from engines.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineConfig {
    pub id: String,
    pub name: String,
    pub version: String,

    /// Minimum validated installed version (inclusive), dotted-numeric.
    /// `None` means no lower bound. Used by the install-time / preflight
    /// version gate to refuse known-incompatible engine builds.
    #[serde(default)]
    pub min_version: Option<String>,
    /// Maximum validated installed version (inclusive), dotted-numeric.
    /// `None` means no upper bound.
    #[serde(default)]
    pub max_version: Option<String>,
    /// The last version we verified end-to-end. Surfaced in the out-of-range
    /// warning as the concrete build to pin to (e.g. `brew install omlx@<x>`).
    /// Defaults to `version` when unset.
    #[serde(default)]
    pub last_known_good_version: Option<String>,

    // ── Matching criteria (v1, retained for back-compat) ───────────────────
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

    // ── Matching criteria (v2 additions — tier-aware selection) ────────────
    /// Tier classification — see [`EngineTier`].
    #[serde(default)]
    pub tier: EngineTier,
    /// Minimum NVIDIA compute capability, e.g. `"9.0"` for Hopper.
    /// `None` means no compute-cap requirement.
    #[serde(default)]
    pub min_compute_cap: Option<String>,
    /// Maximum NVIDIA compute capability (inclusive), e.g. `"10.3"` to
    /// exclude consumer Blackwell (`sm_120`). `None` means no upper bound.
    #[serde(default)]
    pub max_compute_cap: Option<String>,
    /// OS families this engine supports — `["linux", "windows-wsl2"]` etc.
    /// Empty means "no OS-family gate" (legacy `matches_os` is still honoured).
    #[serde(default)]
    pub supported_os_families: Vec<String>,

    // ── Installation ──────────────────────────────────────────────────────
    pub install_method: String,
    #[serde(default)]
    pub brew_tap: Option<String>,
    #[serde(default)]
    pub brew_tap_url: Option<String>, // optional GitHub URL for 3rd-party taps
    #[serde(default)]
    pub brew_formula: Option<String>,
    #[serde(default)]
    pub pip_fallback: Option<String>,
    #[serde(default)]
    pub pip_package: Option<String>,
    /// Additional pip packages installed *in the same venv* alongside
    /// `pip_package`. Used for runtime build-deps the engine's wheel can't
    /// declare (e.g. vLLM's FlashInfer JIT needs the `ninja` build tool at
    /// first model load — see vllm-project/vllm#XXXXX). Empty by default.
    #[serde(default)]
    pub pip_extras: Vec<String>,

    /// Override for the post-install `python -c "import <name>"` probe.
    ///
    /// `install_via_pip` defaults to deriving the import name from
    /// `pip_package` (e.g. `vllm==0.21.0` → `import vllm`). That breaks
    /// for engines like TabbyAPI whose `pip_package` is just a metadata
    /// shell (`py-modules = []`) — `import tabbyAPI` returns ModuleNotFound
    /// even on a perfectly-installed venv. Set this to the import name of
    /// a *core dependency* (TabbyAPI ⇒ `exllamav3`) to prove the install.
    #[serde(default)]
    pub verify_import_name: Option<String>,

    // ── Repo-based engines (Phase 4 — TabbyAPI/ExLlamaV3) ─────────────────
    /// HTTPS URL of a git repo to clone alongside the venv.
    ///
    /// Some engines ship as a "clone-and-run" application rather than a
    /// proper pip package — TabbyAPI is the canonical example: its
    /// `pyproject.toml` sets `py-modules = []`, meaning `pip install`
    /// only pulls dependencies (torch, exllamav3, fastapi). The actual
    /// `main.py` server lives only in the git tree.
    ///
    /// When set, the installer clones to `<data_dir>/engines/<id>/source/`
    /// and the adapter spawns Python with that path on `sys.path`.
    #[serde(default)]
    pub source_repo: Option<String>,
    /// Pinned git ref (commit SHA, tag, or branch). Defaults to `main` when
    /// `source_repo` is set and this is `None`. For reproducibility we
    /// recommend pinning a SHA in production `engines.toml`.
    #[serde(default)]
    pub source_revision: Option<String>,
    /// Minimum Python version for the venv, e.g. `"3.12"`. `None` keeps
    /// the system default that `uv` picks.
    ///
    /// TabbyAPI's `[cu13]` extra requires 3.12+ (uses `python_version >=
    /// '3.12'` markers). vLLM and SGLang work on 3.10. Specifying a
    /// minimum lets each engine demand its own toolchain without forcing
    /// every other engine to bump.
    #[serde(default)]
    pub min_python_version: Option<String>,
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
    /// Windows + NVIDIA only: pattern for the CUDA-runtime DLL companion zip
    /// (upstream ships `cudart-llama-bin-win-cuda-<variant>-x64.zip`). Must be
    /// downloaded alongside the main `win-cuda-*` zip and extracted into the
    /// same directory or `llama-server.exe` fails with "cudart64_*.dll missing".
    /// `{cuda_variant}` is resolved at install time from `nvidia-smi`.
    #[serde(default)]
    pub cudart_pattern: Option<String>,

    // ── Runtime ───────────────────────────────────────────────────────────
    pub model_format: String,
    pub hf_org: String,
    pub start_cmd: String,
    pub start_args: Vec<String>,
    pub health_endpoint: String,
    #[serde(default)]
    pub supports_embeddings: bool,
    #[serde(default)]
    pub supports_reranking: bool,

    /// Lower number = higher priority
    #[serde(default = "default_priority")]
    pub priority: u32,
}

fn default_priority() -> u32 {
    100
}

/// Parse a `"MAJOR.MINOR"` compute-cap spec (`"9.0"`, `"10.3"`) into a tuple.
/// Used at registry-load time to validate the v2 `min_compute_cap` /
/// `max_compute_cap` fields. Returns `None` for malformed input so the
/// caller can fail loudly.
pub fn parse_compute_cap_spec(s: &str) -> Option<ComputeCap> {
    crate::hardware::probe::parse_compute_cap(s)
}

/// Compare two dotted version strings numerically (e.g. `"0.3.6"` vs `"0.4.1"`).
///
/// Each segment is split on `.`, `-`, `+` and the leading ASCII digits of the
/// segment are parsed; non-numeric trailing text (`".post1"`, `"b9351"`) maps
/// to `0`. Missing trailing segments are treated as `0`, so `"0.4"` == `"0.4.0"`.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map(|seg| {
                let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse::<u64>().unwrap_or(0)
            })
            .collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            ord => return ord,
        }
    }
    std::cmp::Ordering::Equal
}

/// `true` if `installed` lies within `[min, max]` (inclusive). `None` bounds are
/// open-ended. Comparison is numeric/dotted (see [`compare_versions`]).
pub fn version_in_range(installed: &str, min: Option<&str>, max: Option<&str>) -> bool {
    use std::cmp::Ordering::{Greater, Less};
    if let Some(mn) = min
        && compare_versions(installed, mn) == Less
    {
        return false;
    }
    if let Some(mx) = max
        && compare_versions(installed, mx) == Greater
    {
        return false;
    }
    true
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
        let mut registry: EngineRegistryFile =
            toml::from_str(DEFAULT_ENGINES).context("Failed to parse embedded engines.toml")?;

        debug!("Loaded {} default engines", registry.engine.len());

        // Merge user overrides if present
        if let Some(path) = user_override_path
            && path.exists()
        {
            let user_content =
                std::fs::read_to_string(path).context("Failed to read user engines.toml")?;
            let user_registry: EngineRegistryFile =
                toml::from_str(&user_content).context("Failed to parse user engines.toml")?;

            for user_engine in user_registry.engine {
                // Override existing or add new
                if let Some(existing) = registry.engine.iter_mut().find(|e| e.id == user_engine.id)
                {
                    info!(engine = %user_engine.id, "User override for engine");
                    *existing = user_engine;
                } else {
                    info!(engine = %user_engine.id, "User added custom engine");
                    registry.engine.push(user_engine);
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
                profile.os,
                profile.arch,
                profile.gpu_vendor,
                profile.vram_gb
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

    /// Explicit engine override (`lmforge run --engine <id>`).
    ///
    /// Bypasses tier gating (so `experimental` engines like SGLang are reachable)
    /// but still enforces the hardware gates (`min_compute_cap`, etc.) — we'd
    /// rather refuse fast than fail at chat-time with a kernel ImportError.
    ///
    /// Returns the config or an error explaining *why* the requested engine
    /// can't run here.
    pub fn select_explicit(&self, id: &str, profile: &HardwareProfile) -> Result<&EngineConfig> {
        let engine = self
            .engines
            .iter()
            .find(|e| e.id == id)
            .with_context(|| format!("Unknown engine id: {}", id))?;

        // Hardware gates still apply on explicit override.
        if !v1_matches(engine, profile) {
            bail!(
                "Engine `{}` does not support this hardware: {:?} {:?} GPU:{:?} VRAM:{:.1}GB",
                id,
                profile.os,
                profile.arch,
                profile.gpu_vendor,
                profile.vram_gb
            );
        }
        if !v2_matches(engine, profile) {
            bail!(
                "Engine `{}` is gated out on this hardware (compute_cap or OS family mismatch). \
                 See `data/engines.toml` for the supported window.",
                id
            );
        }

        if engine.tier == EngineTier::Experimental {
            info!(
                engine = %id,
                "Experimental engine requested via --engine override; proceeding"
            );
        }
        Ok(engine)
    }

    /// Convert a selected EngineConfig into its respective functional Adapter natively
    pub fn create_adapter(
        config: &EngineConfig,
    ) -> Result<crate::engine::adapter::EngineAdapterInstance> {
        match config.id.as_str() {
            "omlx" => Ok(crate::engine::adapter::EngineAdapterInstance::Omlx(
                crate::engine::adapters::omlx::OmlxAdapter::default(),
            )),
            "sglang" => Ok(crate::engine::adapter::EngineAdapterInstance::Sglang(
                crate::engine::adapters::sglang::SglangAdapter::default(),
            )),
            "llamacpp" => Ok(crate::engine::adapter::EngineAdapterInstance::Llamacpp(
                crate::engine::adapters::llamacpp::LlamacppAdapter::default(),
            )),
            "vllm" => Ok(crate::engine::adapter::EngineAdapterInstance::Vllm(
                crate::engine::adapters::vllm::VllmAdapter::default(),
            )),
            "tabbyapi" => Ok(crate::engine::adapter::EngineAdapterInstance::TabbyApi(
                crate::engine::adapters::tabbyapi::TabbyApiAdapter,
            )),
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

/// Check if an engine's matching criteria are satisfied by the profile.
///
/// Combines the v1 `matches_*` gates with the v2 schema additions
/// (`min_compute_cap`, `max_compute_cap`, `supported_os_families`).
/// `experimental` engines are filtered out unconditionally — they are only
/// reachable via the explicit `--engine <id>` override path.
fn engine_matches(engine: &EngineConfig, profile: &HardwareProfile) -> bool {
    // Opt-in and experimental tiers: never auto-select.
    //   * OptIn       → reachable via `--engine <id>` after `engine install`.
    //                   No confirmation prompt; "install means you meant it".
    //   * Experimental→ reachable via `--engine <id>` plus a y/N prompt
    //                   (`LMFORGE_YES_EXPERIMENTAL=1` to skip).
    // Both are filtered out unconditionally from the auto-selector to keep
    // `lmforge init` → `lmforge run` predictable.
    if matches!(engine.tier, EngineTier::OptIn | EngineTier::Experimental) {
        return false;
    }

    // Fallback engines always match the v1 gates — but we still apply v2 gates
    // below so a fallback can be excluded from, e.g., a hardware mismatch.
    let v1_ok = if engine.matches_fallback {
        true
    } else {
        v1_matches(engine, profile)
    };
    if !v1_ok {
        return false;
    }

    v2_matches(engine, profile)
}

/// V1 (legacy) gate set: `matches_os` / `matches_arch` / `matches_gpu` / `min_vram_gb`.
///
/// Exposed `pub(crate)` so the `cli::engine` "engine status/list" command can
/// reuse the exact same gate evaluation the selector uses — keeps the verdicts
/// shown to the user identical to what `select_explicit` would do.
pub(crate) fn v1_matches(engine: &EngineConfig, profile: &HardwareProfile) -> bool {
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

    if let Some(ref required_gpu) = engine.matches_gpu {
        let profile_gpu = match profile.gpu_vendor {
            GpuVendor::Apple => "apple",
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
            GpuVendor::Intel => "intel",
            GpuVendor::None => "none",
        };
        if profile_gpu != required_gpu {
            return false;
        }
    }

    if let Some(min_vram) = engine.min_vram_gb
        && profile.vram_gb < min_vram
    {
        return false;
    }

    true
}

/// V2 gate set: `min_compute_cap`, `max_compute_cap`, `supported_os_families`.
/// Each field is optional and only filters when set, so legacy `engines.toml`
/// entries that don't declare them are unaffected.
pub(crate) fn v2_matches(engine: &EngineConfig, profile: &HardwareProfile) -> bool {
    // Compute-cap bounds (NVIDIA only — silently passes on non-NVIDIA).
    if profile.gpu_vendor == GpuVendor::Nvidia {
        let probed = profile.compute_cap;
        if let Some(ref spec) = engine.min_compute_cap {
            let bound = match parse_compute_cap_spec(spec) {
                Some(b) => b,
                None => {
                    tracing::warn!(
                        engine = %engine.id,
                        spec, "Malformed min_compute_cap; ignoring gate"
                    );
                    return true;
                }
            };
            match probed {
                Some(cc) if cc < bound => return false,
                None => return false, // engine requires a cap; probe failed → exclude
                _ => {}
            }
        }
        if let Some(ref spec) = engine.max_compute_cap {
            let bound = match parse_compute_cap_spec(spec) {
                Some(b) => b,
                None => {
                    tracing::warn!(
                        engine = %engine.id,
                        spec, "Malformed max_compute_cap; ignoring gate"
                    );
                    return true;
                }
            };
            if let Some(cc) = probed
                && cc > bound
            {
                return false;
            }
        }
    }

    // OS-family allowlist. Skipped when the probed family is Unknown so a
    // probe failure or v1 fixture (where `os_family` defaults to Unknown)
    // doesn't accidentally exclude every engine — v1 gates still apply.
    if !engine.supported_os_families.is_empty()
        && profile.os_family != crate::hardware::probe::OsFamily::Unknown
    {
        let family = os_family_to_str(profile.os_family);
        if !engine
            .supported_os_families
            .iter()
            .any(|f| f.as_str() == family)
        {
            return false;
        }
    }

    true
}

fn os_family_to_str(family: crate::hardware::probe::OsFamily) -> &'static str {
    use crate::hardware::probe::OsFamily::*;
    match family {
        Linux => "linux",
        WindowsNative => "windows-native",
        WindowsWsl2 => "windows-wsl2",
        Darwin => "darwin",
        Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple_silicon() -> HardwareProfile {
        HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            gpu_vendor: GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3 Max".to_string(),
            ..Default::default()
        }
    }

    fn nvidia_large() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 48.0,
            total_ram_gb: 64.0,
            cpu_cores: 16,
            cpu_model: "AMD Ryzen 9".to_string(),
            ..Default::default()
        }
    }

    fn nvidia_small() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 8.0,
            total_ram_gb: 32.0,
            cpu_cores: 8,
            cpu_model: "Intel i7".to_string(),
            ..Default::default()
        }
    }

    fn nvidia_tiny() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 4.0,
            total_ram_gb: 16.0,
            cpu_cores: 4,
            cpu_model: "Intel i5".to_string(),
            ..Default::default()
        }
    }

    fn cpu_only() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::None,
            total_ram_gb: 16.0,
            cpu_cores: 4,
            cpu_model: "Intel i5".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_parse_default_registry() {
        let registry = EngineRegistry::load(None).unwrap();
        assert_eq!(registry.all().len(), 5);
        assert!(registry.get("omlx").is_some());
        assert!(registry.get("sglang").is_some());
        assert!(registry.get("llamacpp").is_some());
        assert!(registry.get("vllm").is_some());
        assert!(registry.get("tabbyapi").is_some());
    }

    #[test]
    fn test_tabbyapi_is_opt_in_with_correct_gates() {
        let registry = EngineRegistry::load(None).unwrap();
        let tabby = registry
            .get("tabbyapi")
            .expect("tabbyapi must be registered");
        assert_eq!(tabby.tier, EngineTier::OptIn);
        assert_eq!(tabby.install_method, "pip");
        assert_eq!(tabby.matches_gpu.as_deref(), Some("nvidia"));
        assert_eq!(tabby.min_compute_cap.as_deref(), Some("7.5"));
        assert_eq!(tabby.min_python_version.as_deref(), Some("3.12"));
        assert_eq!(tabby.verify_import_name.as_deref(), Some("exllamav3"));
        assert!(
            tabby.source_repo.is_some(),
            "tabbyapi must have source_repo"
        );
        assert_eq!(tabby.model_format, "exl3");
        // Native Windows is excluded — same triton/uvloop reasons as vLLM.
        assert!(
            !tabby
                .supported_os_families
                .iter()
                .any(|f| f == "windows-native")
        );
        assert!(tabby.supported_os_families.iter().any(|f| f == "linux"));
    }

    #[test]
    fn test_tabbyapi_never_auto_selected_on_compatible_hw() {
        // sm_120 + Linux is fully compatible with TabbyAPI, but it must
        // stay opt-in — never auto-selected over llama.cpp.
        let registry = EngineRegistry::load(None).unwrap();
        let mut profile = make_blackwell_profile();
        profile.gpu_vendor = GpuVendor::Nvidia;
        let auto = registry.select(&profile).unwrap();
        assert_ne!(
            auto.id, "tabbyapi",
            "tabbyapi must never be auto-selected; got {}",
            auto.id
        );
    }

    fn make_blackwell_profile() -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            total_ram_gb: 32.0,
            cpu_cores: 8,
            cpu_model: "test".to_string(),
            vram_gb: 16.0,
            compute_cap: Some((12, 0)),
            cuda_runtime_version: Some("13.0".to_string()),
            gpu_count: 1,
            ..Default::default()
        }
    }

    #[test]
    fn test_vllm_is_opt_in_with_correct_gates() {
        let registry = EngineRegistry::load(None).unwrap();
        let vllm = registry.get("vllm").expect("vllm must be registered");
        assert_eq!(vllm.tier, EngineTier::OptIn);
        assert_eq!(vllm.install_method, "pip");
        assert_eq!(vllm.matches_gpu.as_deref(), Some("nvidia"));
        assert_eq!(vllm.min_compute_cap.as_deref(), Some("7.5"));
        // Native Windows is explicitly excluded — triton + UNIX ipc.
        assert!(
            !vllm
                .supported_os_families
                .iter()
                .any(|f| f == "windows-native")
        );
        assert!(vllm.supported_os_families.iter().any(|f| f == "linux"));
        assert!(
            vllm.supported_os_families
                .iter()
                .any(|f| f == "windows-wsl2")
        );
        // Embeddings stay on the sidecar — gate must reflect that.
        assert!(
            !vllm.supports_embeddings,
            "vllm must not advertise embeddings; sidecar handles them"
        );
    }

    #[test]
    fn test_vllm_never_auto_selected_even_on_compatible_hardware() {
        // Opt-in tier means a Hopper box with 48 GB still gets llamacpp by default.
        let registry = EngineRegistry::load(None).unwrap();
        let profile = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 48.0,
            compute_cap: Some((9, 0)),
            cuda_runtime_version: Some("12.8".to_string()),
            total_ram_gb: 128.0,
            cpu_cores: 32,
            cpu_model: "Test".to_string(),
            ..Default::default()
        };
        let selected = registry.select(&profile).unwrap();
        assert_ne!(
            selected.id, "vllm",
            "vLLM must never be auto-selected — it's opt-in"
        );
    }

    #[test]
    fn test_vllm_explicit_select_succeeds_on_blackwell() {
        let registry = EngineRegistry::load(None).unwrap();
        let profile = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 15.4,
            compute_cap: Some((12, 0)),
            cuda_runtime_version: Some("13.0".to_string()),
            os_family: crate::hardware::probe::OsFamily::Linux,
            total_ram_gb: 16.0,
            cpu_cores: 12,
            cpu_model: "Test".to_string(),
            ..Default::default()
        };
        let vllm = registry.select_explicit("vllm", &profile).unwrap();
        assert_eq!(vllm.id, "vllm");
    }

    /// Regression guard: full gate matrix across every engine × every
    /// representative host profile. Anyone editing the v1/v2 gate logic
    /// will break this if their change has unintended fallout.
    #[test]
    fn test_gate_matrix_v1_plus_v2_full_coverage() {
        let registry = EngineRegistry::load(None).unwrap();

        // Build the host profiles we explicitly support.
        let apple = HardwareProfile {
            os: Os::Darwin,
            arch: Arch::Aarch64,
            gpu_vendor: GpuVendor::Apple,
            vram_gb: 36.0,
            unified_mem: true,
            total_ram_gb: 48.0,
            cpu_cores: 14,
            cpu_model: "Apple M3".into(),
            os_family: crate::hardware::probe::OsFamily::Darwin,
            ..Default::default()
        };
        let linux_blackwell = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 15.4,
            compute_cap: Some((12, 0)),
            cuda_runtime_version: Some("13.0".into()),
            total_ram_gb: 16.0,
            cpu_cores: 12,
            cpu_model: "Test".into(),
            os_family: crate::hardware::probe::OsFamily::Linux,
            ..Default::default()
        };
        let linux_hopper = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 48.0,
            compute_cap: Some((9, 0)),
            cuda_runtime_version: Some("12.8".into()),
            total_ram_gb: 128.0,
            cpu_cores: 32,
            cpu_model: "Test".into(),
            os_family: crate::hardware::probe::OsFamily::Linux,
            ..Default::default()
        };
        let windows_native = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 24.0,
            compute_cap: Some((8, 9)),
            cuda_runtime_version: Some("12.8".into()),
            total_ram_gb: 32.0,
            cpu_cores: 16,
            cpu_model: "Test".into(),
            os_family: crate::hardware::probe::OsFamily::WindowsNative,
            ..Default::default()
        };
        let wsl2 = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 24.0,
            compute_cap: Some((8, 9)),
            cuda_runtime_version: Some("12.8".into()),
            is_wsl: true,
            total_ram_gb: 32.0,
            cpu_cores: 16,
            cpu_model: "Test".into(),
            os_family: crate::hardware::probe::OsFamily::WindowsWsl2,
            ..Default::default()
        };
        let cpu_only = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::None,
            vram_gb: 0.0,
            total_ram_gb: 16.0,
            cpu_cores: 8,
            cpu_model: "Test".into(),
            os_family: crate::hardware::probe::OsFamily::Linux,
            ..Default::default()
        };

        // (engine_id, profile_name, profile, expect_explicit_select_ok)
        // We assert against `select_explicit` rather than `select` because
        // explicit selection isolates gate evaluation from tier filtering.
        // Each row was traced by hand against engines.toml.
        let matrix: &[(&str, &str, &HardwareProfile, bool)] = &[
            // omlx — darwin-aarch64 only
            ("omlx", "apple", &apple, true),
            ("omlx", "linux-blackwell", &linux_blackwell, false),
            ("omlx", "windows-native", &windows_native, false),
            ("omlx", "cpu-only", &cpu_only, false),
            // llamacpp — universal fallback on Linux + Windows; macOS is
            // explicitly excluded because oMLX is the default there (better
            // perf, native Metal). matches_fallback=true within its supported
            // OS families means it's never refused on hardware grounds.
            ("llamacpp", "apple", &apple, false),
            ("llamacpp", "linux-blackwell", &linux_blackwell, true),
            ("llamacpp", "linux-hopper", &linux_hopper, true),
            ("llamacpp", "windows-native", &windows_native, true),
            ("llamacpp", "wsl2", &wsl2, true),
            ("llamacpp", "cpu-only", &cpu_only, true),
            // sglang — Linux + NVIDIA sm_9.0..=10.3 only. Blackwell sm_120
            // and macOS are out; native Windows is out.
            ("sglang", "apple", &apple, false),
            ("sglang", "linux-blackwell", &linux_blackwell, false),
            ("sglang", "linux-hopper", &linux_hopper, true),
            ("sglang", "windows-native", &windows_native, false),
            ("sglang", "wsl2", &wsl2, false),
            ("sglang", "cpu-only", &cpu_only, false),
            // vllm — Linux + WSL2, NVIDIA sm_7.5+, ≥12 GB VRAM.
            ("vllm", "apple", &apple, false),
            ("vllm", "linux-blackwell", &linux_blackwell, true),
            ("vllm", "linux-hopper", &linux_hopper, true),
            ("vllm", "windows-native", &windows_native, false),
            ("vllm", "wsl2", &wsl2, true),
            ("vllm", "cpu-only", &cpu_only, false),
        ];

        let mut failures: Vec<String> = Vec::new();
        for (engine_id, profile_name, profile, expect_ok) in matrix {
            let actual = registry.select_explicit(engine_id, profile).is_ok();
            if actual != *expect_ok {
                failures.push(format!(
                    "  {} × {}: expected {}, got {}",
                    engine_id,
                    profile_name,
                    if *expect_ok { "OK" } else { "REFUSED" },
                    if actual { "OK" } else { "REFUSED" },
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "Gate matrix mismatches:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn test_vllm_explicit_select_refuses_windows_native() {
        let registry = EngineRegistry::load(None).unwrap();
        let profile = HardwareProfile {
            os: Os::Windows,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 24.0,
            compute_cap: Some((8, 9)),
            cuda_runtime_version: Some("12.8".to_string()),
            os_family: crate::hardware::probe::OsFamily::WindowsNative,
            total_ram_gb: 32.0,
            cpu_cores: 16,
            cpu_model: "Test".to_string(),
            ..Default::default()
        };
        let err = registry
            .select_explicit("vllm", &profile)
            .expect_err("vllm must refuse native Windows");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("compute") || msg.contains("os") || msg.contains("support"),
            "Error should mention OS gate: {}",
            err
        );
    }

    #[test]
    fn test_select_omlx_on_apple_silicon() {
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&apple_silicon()).unwrap();
        assert_eq!(selected.id, "omlx");
        assert_eq!(selected.version, "0.4.4");
        // Version gate: floor at the build that fixed the Qwen3-VL stream crash.
        assert_eq!(selected.min_version.as_deref(), Some("0.4.4"));
        assert_eq!(selected.last_known_good_version.as_deref(), Some("0.4.4"));
    }

    #[test]
    fn test_select_llamacpp_on_large_nvidia_post_phase1() {
        // Post Phase 1: SGLang is experimental and never auto-selected,
        // even on a Hopper-class GPU (sm_90, 48 GB VRAM). llamacpp wins
        // by virtue of `matches_fallback = true` and a permissive platform.
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&nvidia_large()).unwrap();
        assert_eq!(selected.id, "llamacpp");
    }

    #[test]
    fn test_select_llamacpp_on_8gb_nvidia_post_phase1() {
        // 8 GB cards (RTX 3060 8GB, 4060 8GB, 5060 Ti 16GB) now pick llamacpp.
        // SGLang is opt-in for these users — they can request `--engine sglang`.
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&nvidia_small()).unwrap();
        assert_eq!(selected.id, "llamacpp");
    }

    #[test]
    fn test_select_llamacpp_on_consumer_blackwell_sm120() {
        // The P0 fix this whole change exists for. RTX 5060 Ti / 5070 / 5080
        // / 5090 are sm_120 — SGLang's sgl-kernel has no cubin for them, so
        // we must never auto-select SGLang on these cards.
        let registry = EngineRegistry::load(None).unwrap();
        let mut blackwell = nvidia_small();
        blackwell.compute_cap = Some((12, 0));
        blackwell.os_family = crate::hardware::probe::OsFamily::Linux;
        let selected = registry.select(&blackwell).unwrap();
        assert_eq!(
            selected.id, "llamacpp",
            "consumer Blackwell must not auto-select SGLang"
        );
    }

    #[test]
    fn test_sglang_never_auto_selected_even_on_hopper() {
        // Even with the perfect SGLang profile (sm_90 + 48 GB VRAM + Linux),
        // SGLang stays opt-in. The tier=experimental filter is unconditional.
        let registry = EngineRegistry::load(None).unwrap();
        let mut hopper = nvidia_large();
        hopper.compute_cap = Some((9, 0));
        hopper.os_family = crate::hardware::probe::OsFamily::Linux;
        let selected = registry.select(&hopper).unwrap();
        assert_ne!(selected.id, "sglang");
    }

    #[test]
    fn test_select_llamacpp_on_tiny_nvidia() {
        // < 8 GB NVIDIA: same answer as before (llamacpp).
        let registry = EngineRegistry::load(None).unwrap();
        let selected = registry.select(&nvidia_tiny()).unwrap();
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
        // Numbers haven't changed — sglang's tier=experimental does the gating,
        // not the priority. Priorities still drive tie-breaking when multiple
        // engines pass the gates (omlx + llamacpp both match on macOS).
        assert!(omlx.priority < sglang.priority);
        assert!(sglang.priority < llama.priority);
    }

    #[test]
    fn test_select_explicit_sglang_on_hopper() {
        // Explicit `--engine sglang` on a sm_90 box must succeed.
        let registry = EngineRegistry::load(None).unwrap();
        let mut hopper = nvidia_large();
        hopper.compute_cap = Some((9, 0));
        hopper.os_family = crate::hardware::probe::OsFamily::Linux;
        let selected = registry.select_explicit("sglang", &hopper).unwrap();
        assert_eq!(selected.id, "sglang");
    }

    #[test]
    fn test_select_explicit_sglang_refused_on_blackwell() {
        // Explicit `--engine sglang` on a sm_120 box must REFUSE — we know
        // the import will fail at runtime, and a fast no is better than a
        // mysterious hang.
        let registry = EngineRegistry::load(None).unwrap();
        let mut blackwell = nvidia_small();
        blackwell.compute_cap = Some((12, 0));
        blackwell.os_family = crate::hardware::probe::OsFamily::Linux;
        let err = registry
            .select_explicit("sglang", &blackwell)
            .expect_err("must refuse SGLang on sm_120");
        assert!(
            err.to_string().contains("gated out"),
            "error message must mention the gate: {}",
            err
        );
    }

    #[test]
    fn test_select_explicit_unknown_engine() {
        let registry = EngineRegistry::load(None).unwrap();
        let err = registry
            .select_explicit("does-not-exist", &nvidia_large())
            .expect_err("must reject unknown engine");
        assert!(err.to_string().contains("Unknown engine"));
    }

    #[test]
    fn test_sglang_is_experimental_in_engines_toml() {
        // Guard against accidental promotion: if a future contributor re-flags
        // SGLang as default, this test fails loudly and ADR-001 stays consistent.
        let registry = EngineRegistry::load(None).unwrap();
        let sglang = registry.get("sglang").unwrap();
        assert_eq!(sglang.tier, EngineTier::Experimental);
        assert_eq!(sglang.min_compute_cap.as_deref(), Some("9.0"));
        assert_eq!(sglang.max_compute_cap.as_deref(), Some("10.3"));
    }

    #[test]
    fn test_pinned_versions() {
        let registry = EngineRegistry::load(None).unwrap();
        assert_eq!(registry.get("omlx").unwrap().version, "0.4.4");
        assert_eq!(registry.get("llamacpp").unwrap().version, "b9861");
        assert_eq!(registry.get("sglang").unwrap().version, "0.5.10.post1");
    }

    // ── Schema v2 (tier + supported_os_families) parsing tests ──────────────

    #[test]
    fn test_v2_fields_parse_from_engines_toml() {
        // Regression guard: if a future change drops the serde tags or
        // renames the fields without updating engines.toml, this test fails
        // loudly. Reflects the post-Phase-1 tier model.
        let registry = EngineRegistry::load(None).unwrap();
        let omlx = registry.get("omlx").unwrap();
        assert_eq!(omlx.tier, EngineTier::Default);
        assert_eq!(omlx.supported_os_families, vec!["darwin"]);

        let sglang = registry.get("sglang").unwrap();
        assert_eq!(sglang.tier, EngineTier::Experimental);
        assert_eq!(sglang.supported_os_families, vec!["linux"]);

        let llama = registry.get("llamacpp").unwrap();
        assert_eq!(llama.tier, EngineTier::Default);
        // darwin intentionally absent — macOS defaults to oMLX, not llama.cpp.
        assert_eq!(
            llama.supported_os_families,
            vec!["linux", "windows-native", "windows-wsl2"]
        );
    }

    #[test]
    fn test_tier_default_is_unspecified_for_v1_entries() {
        // Synthetic v1-style entry without the `tier` field — must default
        // to Unspecified so the back-compat path is never accidentally
        // promoted to Experimental or worse.
        let v1_toml = r#"
            [[engine]]
            id = "legacy"
            name = "Legacy"
            version = "1.0"
            install_method = "pip"
            pip_package = "legacy"
            model_format = "safetensors"
            hf_org = "ignored"
            start_cmd = "legacy"
            start_args = []
            health_endpoint = "/health"
        "#;
        let parsed: EngineRegistryFile = toml::from_str(v1_toml).unwrap();
        assert_eq!(parsed.engine[0].tier, EngineTier::Unspecified);
        assert!(parsed.engine[0].supported_os_families.is_empty());
        assert!(parsed.engine[0].min_compute_cap.is_none());
    }

    #[test]
    fn test_tier_parsing_all_variants() {
        for (literal, expected) in [
            (r#"tier = "default""#, EngineTier::Default),
            (r#"tier = "opt-in""#, EngineTier::OptIn),
            (r#"tier = "experimental""#, EngineTier::Experimental),
        ] {
            let toml_str = format!(
                r#"
                [[engine]]
                id = "x"
                name = "x"
                version = "1.0"
                {literal}
                install_method = "binary"
                model_format = "gguf"
                hf_org = "x"
                start_cmd = "x"
                start_args = []
                health_endpoint = "/health"
                "#
            );
            let parsed: EngineRegistryFile = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("failed to parse `{literal}`: {e}"));
            assert_eq!(parsed.engine[0].tier, expected, "literal: {literal}");
        }
    }

    // ── v2 selector gating tests ────────────────────────────────────────────

    fn linux_nvidia_with_cc(cc: ComputeCap, vram_gb: f32) -> HardwareProfile {
        HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb,
            total_ram_gb: 32.0,
            cpu_cores: 8,
            cpu_model: "Test".to_string(),
            os_family: crate::hardware::probe::OsFamily::Linux,
            compute_cap: Some(cc),
            gpu_count: 1,
            ..Default::default()
        }
    }

    #[test]
    fn test_v2_min_compute_cap_excludes_below_bound() {
        let mut engine = EngineConfig {
            id: "test-min".to_string(),
            tier: EngineTier::Default,
            min_compute_cap: Some("9.0".to_string()),
            matches_gpu: Some("nvidia".to_string()),
            install_method: "binary".to_string(),
            ..Default::default()
        };
        // Don't set `matches_os` so v1 gate is permissive — isolate the v2 check.
        engine.min_vram_gb = None;

        // Turing (sm_75) → below bound, excluded.
        assert!(!engine_matches(
            &engine,
            &linux_nvidia_with_cc((7, 5), 24.0)
        ));
        // Hopper (sm_90) → at bound, included.
        assert!(engine_matches(&engine, &linux_nvidia_with_cc((9, 0), 80.0)));
        // Blackwell (sm_120) → above bound, included.
        assert!(engine_matches(
            &engine,
            &linux_nvidia_with_cc((12, 0), 16.0)
        ));
    }

    #[test]
    fn test_v2_max_compute_cap_excludes_above_bound() {
        let engine = EngineConfig {
            id: "test-max".to_string(),
            tier: EngineTier::Default,
            min_compute_cap: Some("9.0".to_string()),
            max_compute_cap: Some("10.3".to_string()),
            matches_gpu: Some("nvidia".to_string()),
            install_method: "binary".to_string(),
            ..Default::default()
        };
        // Hopper (sm_90) — in window.
        assert!(engine_matches(&engine, &linux_nvidia_with_cc((9, 0), 80.0)));
        // Datacenter Blackwell (sm_100) — in window.
        assert!(engine_matches(
            &engine,
            &linux_nvidia_with_cc((10, 0), 80.0)
        ));
        // sm_103 — in window (inclusive max).
        assert!(engine_matches(
            &engine,
            &linux_nvidia_with_cc((10, 3), 80.0)
        ));
        // Consumer Blackwell (sm_120) — OUT of window. This is the bug fix:
        // SGLang on RTX 5060 Ti must not be auto-selected.
        assert!(!engine_matches(
            &engine,
            &linux_nvidia_with_cc((12, 0), 16.0)
        ));
    }

    #[test]
    fn test_v2_excludes_when_probe_failed_to_get_compute_cap() {
        // Probe reported NVIDIA but couldn't read compute_cap (e.g. driver
        // out of date). An engine that requires a specific cap range cannot
        // be safely selected.
        let engine = EngineConfig {
            id: "needs-cc".to_string(),
            tier: EngineTier::Default,
            min_compute_cap: Some("9.0".to_string()),
            matches_gpu: Some("nvidia".to_string()),
            install_method: "binary".to_string(),
            ..Default::default()
        };
        let profile = HardwareProfile {
            os: Os::Linux,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            vram_gb: 24.0,
            total_ram_gb: 32.0,
            cpu_cores: 8,
            cpu_model: "Test".to_string(),
            os_family: crate::hardware::probe::OsFamily::Linux,
            compute_cap: None, // <- probe failed
            ..Default::default()
        };
        assert!(!engine_matches(&engine, &profile));
    }

    #[test]
    fn test_v2_supported_os_families_gates_correctly() {
        let engine = EngineConfig {
            id: "linux-only".to_string(),
            tier: EngineTier::Default,
            supported_os_families: vec!["linux".to_string(), "windows-wsl2".to_string()],
            install_method: "binary".to_string(),
            matches_fallback: true,
            ..Default::default()
        };

        let mut linux_native = linux_nvidia_with_cc((9, 0), 24.0);
        linux_native.os_family = crate::hardware::probe::OsFamily::Linux;
        assert!(engine_matches(&engine, &linux_native));

        let mut wsl = linux_nvidia_with_cc((9, 0), 24.0);
        wsl.os_family = crate::hardware::probe::OsFamily::WindowsWsl2;
        assert!(engine_matches(&engine, &wsl));

        let mut win = linux_nvidia_with_cc((9, 0), 24.0);
        win.os = Os::Windows;
        win.os_family = crate::hardware::probe::OsFamily::WindowsNative;
        assert!(!engine_matches(&engine, &win));
    }

    #[test]
    fn test_v2_experimental_tier_never_auto_selected() {
        let engine = EngineConfig {
            id: "experimental-engine".to_string(),
            tier: EngineTier::Experimental,
            install_method: "binary".to_string(),
            matches_fallback: true,
            ..Default::default()
        };
        // Even with `matches_fallback = true`, the experimental gate slams the door.
        assert!(!engine_matches(
            &engine,
            &linux_nvidia_with_cc((9, 0), 24.0)
        ));
    }

    #[test]
    fn test_v2_unspecified_tier_treated_as_selectable() {
        let engine = EngineConfig {
            id: "v1-legacy".to_string(),
            // tier defaults to Unspecified
            install_method: "binary".to_string(),
            matches_fallback: true,
            ..Default::default()
        };
        assert!(engine_matches(&engine, &linux_nvidia_with_cc((9, 0), 24.0)));
    }

    #[test]
    fn test_v2_unknown_os_family_does_not_kill_selection() {
        // In tests + on probe failure, os_family is Unknown. Engines with
        // `supported_os_families = ["linux"]` must still be selectable so
        // the v1 path (used by older fixtures) keeps working.
        let engine = EngineConfig {
            id: "linux-required".to_string(),
            tier: EngineTier::Default,
            supported_os_families: vec!["linux".to_string()],
            install_method: "binary".to_string(),
            matches_fallback: true,
            ..Default::default()
        };
        let mut profile = linux_nvidia_with_cc((9, 0), 24.0);
        profile.os_family = crate::hardware::probe::OsFamily::Unknown;
        assert!(engine_matches(&engine, &profile));
    }

    #[test]
    fn test_compute_cap_spec_roundtrip() {
        let toml_str = r#"
            [[engine]]
            id = "x"
            name = "x"
            version = "1.0"
            tier = "experimental"
            min_compute_cap = "9.0"
            max_compute_cap = "10.3"
            install_method = "pip"
            pip_package = "x"
            model_format = "safetensors"
            hf_org = "x"
            start_cmd = "x"
            start_args = []
            health_endpoint = "/health"
        "#;
        let parsed: EngineRegistryFile = toml::from_str(toml_str).unwrap();
        let e = &parsed.engine[0];
        assert_eq!(e.min_compute_cap.as_deref(), Some("9.0"));
        assert_eq!(e.max_compute_cap.as_deref(), Some("10.3"));
        // The spec parser is the same function used at hardware-probe time —
        // verify the bounds tuples are well-formed.
        assert_eq!(parse_compute_cap_spec("9.0"), Some((9, 0)));
        assert_eq!(parse_compute_cap_spec("10.3"), Some((10, 3)));
    }

    #[test]
    fn test_compare_versions() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        assert_eq!(compare_versions("0.3.6", "0.4.1"), Less);
        assert_eq!(compare_versions("0.4.1", "0.3.8"), Greater);
        assert_eq!(compare_versions("0.4", "0.4.0"), Equal);
        assert_eq!(compare_versions("0.5.10.post1", "0.5.10"), Equal);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Greater);
    }

    #[test]
    fn test_version_in_range() {
        let (min, max) = (Some("0.3.6"), Some("0.3.8"));
        assert!(version_in_range("0.3.6", min, max)); // lower bound inclusive
        assert!(version_in_range("0.3.8", min, max)); // upper bound inclusive
        assert!(version_in_range("0.3.7", min, max));
        assert!(!version_in_range("0.3.5", min, max)); // below
        assert!(!version_in_range("0.4.0", min, max)); // above — the #1685 regression
        assert!(!version_in_range("0.4.1", min, max));
        // Open-ended bounds.
        assert!(version_in_range("9.9.9", Some("1.0.0"), None));
        assert!(version_in_range("0.0.1", None, Some("1.0.0")));
    }
}
