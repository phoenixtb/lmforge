use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, ModelRole};
use crate::engine::speculative::{
    ModelSpecInputs, SpecMode, SpecResolved, SpeculativeConfig, VramBudget, detect_moe_by_name,
    resolve as resolve_spec,
};
use crate::hardware::probe::{GpuVendor, HardwareProfile};
use crate::model::downloader::DownloadProgress;

#[derive(Clone)]
pub struct LlamacppAdapter {
    /// The basename of the binary the installer drops into `<data_dir>/engines/`.
    /// We resolve the absolute path at `start()` time using `data_dir`, so the
    /// adapter doesn't need to know the data dir at construction.
    pub executable: String,
}

impl Default for LlamacppAdapter {
    fn default() -> Self {
        Self {
            executable: "llama-server".to_string(),
        }
    }
}

impl EngineAdapter for LlamacppAdapter {
    async fn pull_model(
        &self,
        _repo: &str,
        _dest_dir: &Path,
        _progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool> {
        // llama.cpp's `-hf-repo` flag pulls at startup but has no streaming progress API.
        // Defer to LMForge's Rust downloader for full SSE progress.
        Ok(false)
    }

    async fn start(
        &self,
        model_id: &str,
        model_dir: &Path,
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
        role: ModelRole,
    ) -> Result<ActiveEngine> {
        // Resolve the absolute path to `llama-server` from the install location
        // (`<data_dir>/engines/llama-server`) before any other work. If the
        // installer hasn't run yet — or the binary was removed via
        // `lmforge clean --engines` — fall back to a PATH lookup so users with
        // a system-wide install (homebrew / apt / built-from-source) still work.
        let executable = resolve_executable(&self.executable, data_dir);

        // llama-server requires a single .gguf file path, not a directory.
        // Find the largest .gguf file in the model directory.
        let gguf_path = find_gguf_file(model_dir).ok_or_else(|| {
            anyhow::anyhow!(
                "No .gguf file found in model directory: {}. \
                 Pull the model first with: lmforge pull {}",
                model_dir.display(),
                model_id
            )
        })?;

        info!(
            model_id = %model_id,
            port = port,
            gguf = %gguf_path.display(),
            role = ?role,
            "Spawning llama-server"
        );

        // Per-model log files with size-based rotation; see logging::rotation
        // for the threshold/keep tunables (LMFORGE_ENGINE_LOG_MAX_MB / KEEP).
        let stdout_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stdout")?;
        let stderr_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stderr")?;

        let port_str = port.to_string();
        let gguf_str = gguf_path.to_string_lossy().to_string();

        let mmproj_path = find_mmproj_file(model_dir);
        let model_size_gb = file_size_gb(&gguf_path);
        let mmproj_size_gb = mmproj_path.as_deref().map(file_size_gb).unwrap_or(0.0);

        // Resolve hardware profile (with VRAM filled in). Falls back to a CPU
        // profile so the heuristic still produces a sane ngl=0 and small ctx.
        let profile = resolve_profile_with_vram();
        let free_vram_gb = crate::hardware::vram::get_free_vram(&profile);
        let plan = plan_runtime(
            profile.gpu_vendor,
            profile.total_ram_gb,
            free_vram_gb,
            model_size_gb,
            mmproj_size_gb,
            mmproj_path.is_some(),
        );

        info!(
            model_id = %model_id,
            ngl = plan.ngl,
            ctx_size = plan.ctx_size,
            free_vram_gb = plan.free_vram_gb,
            model_size_gb,
            mmproj_size_gb,
            "llama.cpp runtime plan"
        );

        let mut args: Vec<String> = vec![
            "--port".to_string(),
            port_str,
            "--model".to_string(),
            gguf_str,
            "-ngl".to_string(),
            plan.ngl.to_string(),
        ];

        // --cache-ram (b4400+): host-memory prefix cache. KV blocks for prefixes
        // that fall off the GPU cache are kept in pinned host RAM and re-uploaded
        // on hit instead of re-computed. Closes the "agentic prefix-cache" gap
        // that previously favoured vLLM/SGLang — see ADR-001.
        //
        // Default budget: min(25% of system RAM, 4096 MiB). Aggressive enough to
        // help on dev boxes (16 GB RAM → 4 GiB cap), conservative enough to leave
        // headroom for the OS and the model itself. Chat role only — embed and
        // rerank workloads have negligible prefix-reuse benefit and the cache
        // would just trade RAM for nothing.
        if matches!(role, ModelRole::Chat) {
            let cache_ram_mib = resolve_cache_ram_mib(profile.total_ram_gb);
            if cache_ram_mib > 0 {
                info!(
                    cache_ram_mib,
                    total_ram_gb = profile.total_ram_gb,
                    "llama.cpp host-memory prefix cache enabled"
                );
                args.push("--cache-ram".to_string());
                args.push(cache_ram_mib.to_string());
            }
        }

        match role {
            ModelRole::Chat => {}
            ModelRole::Embed => {
                args.push("--embeddings".to_string());
            }
            ModelRole::Rerank => {
                args.push("--reranking".to_string());
            }
        }

        if let Some(mmproj_path) = mmproj_path {
            info!(
                mmproj = %mmproj_path.display(),
                ctx_size = plan.ctx_size,
                "VLM mmproj sidecar detected — enabling multimodal mode"
            );
            args.push("--mmproj".to_string());
            args.push(mmproj_path.to_string_lossy().to_string());
            args.push("--ctx-size".to_string());
            args.push(plan.ctx_size.to_string());
        }

        // ── Speculative decoding (S-2) ────────────────────────────────────────
        // Only chat workloads benefit from spec-dec — embed/rerank are
        // single-pass scoring with no draft head. Reading the spec config
        // happens here (rather than at adapter construction) so a hot
        // config reload picks it up on next model load.
        if matches!(role, ModelRole::Chat) {
            let spec_inputs = ModelSpecInputs {
                mtp: load_model_mtp(data_dir, model_id),
                is_moe: detect_moe_by_name(model_id),
            };
            let budget = VramBudget {
                gpu_vendor: profile.gpu_vendor,
                free_vram_gb,
                model_size_gb,
                mmproj_size_gb,
            };
            let spec_cfg = load_speculative_config(data_dir);
            let spec = resolve_spec(spec_inputs, &spec_cfg, budget);
            append_spec_args(&mut args, &spec);
            info!(
                model_id = %model_id,
                spec_mode = ?spec.mode,
                draft_max = spec.draft_max,
                draft_min = spec.draft_min,
                draft_p_min = spec.draft_p_min,
                reason = %spec.reason,
                "Speculative-decoding plan"
            );
        }

        info!(
            executable = %executable.display(),
            args = ?args,
            "Spawning llama-server"
        );

        let child = Command::new(&executable)
            .args(&args)
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn llama-server at {}. \
                     Run `lmforge init` to (re-)install the bundled binary.",
                    executable.display()
                )
            })?;

        Ok(ActiveEngine {
            process: child,
            model_id: model_id.to_string(),
        })
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        if let Some(pid) = active_engine.process.id() {
            info!(pid, model = %active_engine.model_id, "Sending SIGTERM to release llama-server mmap memory footprint");
            #[cfg(unix)]
            {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = active_engine.process.kill().await;
            }

            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                active_engine.process.wait(),
            )
            .await
            {
                Ok(_) => debug!("llama-server universally flushed"),
                Err(_) => {
                    warn!("llama-server SIGTERM timed out forcing SIGKILL constraint");
                    let _ = active_engine.process.kill().await;
                }
            }
        }
        Ok(())
    }
}

/// Pick the absolute path to `llama-server` to spawn.
///
/// Resolution order (first hit wins):
///   1. `LMFORGE_LLAMACPP_BIN` env override — absolute path. Useful for
///      hacking on a locally-built `llama.cpp` without re-running `init`.
///   2. `<data_dir>/engines/<basename>` — where `installer::install_via_binary`
///      stages the prebuilt download. Primary path for end users.
///   3. `<data_dir>/engines/<basename>.exe` — Windows fallback when `.exe`
///      wasn't already on `basename`.
///   4. Bare `basename` — relies on PATH. Works for system-wide installs
///      (homebrew / apt / built-from-source).
///
/// Pure function — exposed for unit tests.
pub(crate) fn resolve_executable(basename: &str, data_dir: &Path) -> PathBuf {
    if let Ok(s) = std::env::var("LMFORGE_LLAMACPP_BIN")
        && !s.is_empty()
    {
        let p = PathBuf::from(s);
        if p.is_file() {
            return p;
        }
    }

    let engines_dir = data_dir.join("engines");
    let primary = engines_dir.join(basename);
    if primary.is_file() {
        return primary;
    }

    // Windows: try the `.exe` suffix if the caller passed bare `llama-server`.
    if cfg!(windows) && !basename.ends_with(".exe") {
        let win = engines_dir.join(format!("{basename}.exe"));
        if win.is_file() {
            return win;
        }
    }

    // Last resort — PATH lookup. Spawn will fail with a clear error if missing.
    PathBuf::from(basename)
}

/// Find the best .gguf file in a model directory.
/// Picks the largest file (prefers full-weight over split shards).
/// Skips `mmproj-*.gguf` projector files — those are handled separately
/// by `find_mmproj_file` and must never be passed as `--model`.
fn find_gguf_file(model_dir: &Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(model_dir).ok()?;
    let mut gguf_files: Vec<(u64, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("gguf") {
                return None;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("mmproj-") {
                return None;
            }
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            Some((size, path))
        })
        .collect();

    // Largest file first — single-file models win over small split shards
    gguf_files.sort_by_key(|b| std::cmp::Reverse(b.0));
    gguf_files.into_iter().next().map(|(_, path)| path)
}

/// File size in GB for a model artifact. Returns 0.0 when stat fails so the
/// runtime planner falls through to CPU defaults instead of panicking.
fn file_size_gb(path: &Path) -> f32 {
    std::fs::metadata(path)
        .map(|m| m.len() as f32 / (1024.0 * 1024.0 * 1024.0))
        .unwrap_or(0.0)
}

/// VRAM-aware llama-server runtime parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RuntimePlan {
    /// `-ngl` (number of GPU offload layers). 99 means "offload everything".
    ngl: u32,
    /// `--ctx-size` to pass when VLM/mmproj is active. For chat/embed/rerank
    /// without mmproj llama.cpp uses the GGUF metadata default and this value
    /// is ignored.
    ctx_size: u32,
    /// Free VRAM (GB) observed at planning time. Recorded for log telemetry.
    free_vram_gb: f32,
}

/// Compute `-ngl` and `--ctx-size` from the live VRAM budget and model size.
///
/// Operator escape hatches (always win when set):
///   * `LMFORGE_LLAMACPP_NGL` — integer 0..=99 layers to offload.
///   * `LMFORGE_LLAMACPP_CTX` — integer context size (used only for VLM mode).
///
/// Fallback heuristic (in order):
///   * No GPU — ngl = 0, ctx = 2048 (CPU baseline).
///   * Apple unified memory — ngl = 99 (Metal handles paging transparently);
///     ctx scaled by total RAM.
///   * Discrete GPU, model fits in `free - 1.0 GB` — ngl = 99 (full offload);
///     ctx scaled by post-load free VRAM.
///   * Discrete GPU with tight VRAM — ngl proportional to
///     `budget / model_size`; ctx falls back to 2048 or lower.
fn plan_runtime(
    gpu: GpuVendor,
    total_ram_gb: f32,
    free_vram_gb: f32,
    model_size_gb: f32,
    mmproj_size_gb: f32,
    is_vlm: bool,
) -> RuntimePlan {
    let ngl_override = std::env::var("LMFORGE_LLAMACPP_NGL")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|n| n.min(99));
    let ctx_override = std::env::var("LMFORGE_LLAMACPP_CTX")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n >= 512);

    let ngl = if let Some(n) = ngl_override {
        n
    } else {
        match gpu {
            GpuVendor::None => 0,
            GpuVendor::Apple => 99,
            // Vulkan-capable discrete + integrated GPUs all use the same
            // proportional-offload heuristic. Intel iGPUs share system RAM
            // (see hardware::vram::estimate_intel_vram), so `free_vram_gb`
            // is already a conservative shared-RAM-based number.
            GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel => {
                // 1.0 GB compute-scratch + KV-growth headroom on top of
                // the weights themselves (mmproj also lives in VRAM).
                const SCRATCH_GB: f32 = 1.0;
                let needed = model_size_gb + mmproj_size_gb;
                let budget = (free_vram_gb - SCRATCH_GB).max(0.0);
                if needed <= 0.0 || budget <= 0.0 {
                    0
                } else if needed <= budget {
                    99
                } else {
                    // Proportional partial offload; clamp 1..=98 so we never
                    // claim "all layers" when we can't actually fit them, and
                    // never go to 0 when we have *some* budget.
                    let fraction = (budget / needed).clamp(0.0, 1.0);
                    ((fraction * 99.0).floor() as u32).clamp(1, 98)
                }
            }
        }
    };

    let ctx_size = if let Some(n) = ctx_override {
        n
    } else if !is_vlm {
        // Non-VLM: llama.cpp uses the GGUF metadata default; this value
        // is not actually emitted as --ctx-size. Pick 4096 as a stable
        // value for telemetry/tests.
        4096
    } else {
        // Estimate VRAM left after the model is loaded. Image tiles can
        // consume thousands of context tokens, so scale aggressively.
        let post_load_free = match gpu {
            GpuVendor::Apple => total_ram_gb * 0.5,
            GpuVendor::None => 0.0,
            _ => (free_vram_gb - model_size_gb - mmproj_size_gb).max(0.0),
        };
        if post_load_free >= 6.0 {
            8192
        } else if post_load_free >= 3.0 {
            4096
        } else if post_load_free >= 1.5 {
            2048
        } else {
            1024
        }
    };

    RuntimePlan {
        ngl,
        ctx_size,
        free_vram_gb,
    }
}

/// Build a `HardwareProfile` with VRAM populated. Falls back to a "no GPU"
/// profile when probing fails so the planner picks the CPU branch instead of
/// crashing the engine spawn.
fn resolve_profile_with_vram() -> HardwareProfile {
    let mut profile = crate::hardware::probe::detect_platform().unwrap_or_default();
    profile.vram_gb = crate::hardware::vram::estimate_vram(&profile);
    profile
}

/// Compute the `--cache-ram` budget in MiB.
///
/// Default heuristic: `min(0.25 * total_ram_gb * 1024, 4096)`.
/// On 16 GB RAM systems (the 5060 Ti target) this gives 4 GiB; on 64 GB+
/// systems it caps at 4 GiB to leave room for the model + OS + workload.
///
/// Overrides (always win):
///   * `LMFORGE_LLAMACPP_CACHE_RAM_MIB=<n>` — exact MiB budget; `0` disables.
///
/// Returns `0` when caching should be disabled (no RAM info, or operator set to 0).
pub(crate) fn resolve_cache_ram_mib(total_ram_gb: f32) -> u32 {
    if let Ok(s) = std::env::var("LMFORGE_LLAMACPP_CACHE_RAM_MIB")
        && let Ok(n) = s.parse::<u32>()
    {
        return n;
    }
    if total_ram_gb <= 0.0 || !total_ram_gb.is_finite() {
        return 0;
    }
    let quarter_mib = (total_ram_gb * 1024.0 * 0.25) as u32;
    quarter_mib.min(4096)
}

/// Look up the model's `capabilities.mtp` flag from the on-disk index.
/// Returns `None` when the index is missing, unreadable, or the entry
/// lacks an MTP capability (e.g. pulled with a pre-v0.2.0 binary). The
/// resolver treats `None` as "unknown" and falls back to spec-dec OFF.
fn load_model_mtp(data_dir: &Path, model_id: &str) -> Option<bool> {
    let index = crate::model::index::ModelIndex::load(data_dir).ok()?;
    let entry = index.get(model_id)?;
    entry.capabilities.mtp
}

/// Load the `[speculative]` block from `<data_dir>/config.toml`. Falls
/// back to defaults when the file is missing or unparseable — startup
/// must not abort over a config typo. The full config is *not* cached
/// because launches are infrequent and the cost is negligible.
fn load_speculative_config(data_dir: &Path) -> SpeculativeConfig {
    let path = data_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return SpeculativeConfig::default();
    };
    // Parse into the partial-permissive shape used by `LmForgeConfig` —
    // a missing `[speculative]` table falls back to defaults via serde.
    #[derive(serde::Deserialize, Default)]
    struct PartialConfig {
        #[serde(default)]
        speculative: SpeculativeConfig,
    }
    toml::from_str::<PartialConfig>(&content)
        .map(|p| p.speculative)
        .unwrap_or_default()
}

/// Translate a resolved spec-dec plan into `llama-server` flags. The
/// flag names match the b9351 release; if upstream renames them we'll
/// see test failures on the first dispatch and bump the names here.
pub(crate) fn append_spec_args(args: &mut Vec<String>, spec: &SpecResolved) {
    match spec.mode {
        SpecMode::Off => {}
        SpecMode::Mtp => {
            args.push("--spec-draft-n-max".to_string());
            args.push(spec.draft_max.to_string());
            args.push("--spec-draft-n-min".to_string());
            args.push(spec.draft_min.to_string());
            args.push("--spec-draft-p-min".to_string());
            args.push(format!("{:.3}", spec.draft_p_min));
        }
        SpecMode::DraftModel => {
            if let Some(path) = spec.draft_model_path.as_deref() {
                args.push("--spec-draft-model".to_string());
                args.push(path.to_string());
            }
            args.push("--spec-draft-n-max".to_string());
            args.push(spec.draft_max.to_string());
            args.push("--spec-draft-n-min".to_string());
            args.push(spec.draft_min.to_string());
            args.push("--spec-draft-p-min".to_string());
            args.push(format!("{:.3}", spec.draft_p_min));
            args.push("--spec-draft-ngl".to_string());
            args.push(spec.draft_gpu_layers.to_string());
        }
        SpecMode::Auto => unreachable!(
            "SpecResolved::mode is never Auto post-resolve — resolver normalises to Off/Mtp/DraftModel"
        ),
    }
}

/// Find the multimodal projector sidecar (`mmproj-*.gguf`) in the model dir.
/// llama.cpp loads this via `--mmproj` to enable image input on VLMs.
fn find_mmproj_file(model_dir: &Path) -> Option<std::path::PathBuf> {
    let mut matches: Vec<std::path::PathBuf> = std::fs::read_dir(model_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("mmproj-") && n.ends_with(".gguf"))
                    .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Spec-dec args ────────────────────────────────────────────────────

    #[test]
    fn append_spec_args_off_emits_nothing() {
        let mut args: Vec<String> = vec!["--model".into(), "/m".into()];
        append_spec_args(&mut args, &SpecResolved::off("test"));
        assert_eq!(args, vec!["--model".to_string(), "/m".to_string()]);
    }

    #[test]
    fn append_spec_args_mtp_emits_three_flags_no_draft_model() {
        let mut args: Vec<String> = Vec::new();
        let spec = SpecResolved {
            mode: SpecMode::Mtp,
            draft_max: 16,
            draft_min: 0,
            draft_p_min: 0.75,
            draft_model_path: None,
            draft_gpu_layers: -1,
            reason: "auto → MTP".into(),
        };
        append_spec_args(&mut args, &spec);
        assert_eq!(
            args,
            vec![
                "--spec-draft-n-max".to_string(),
                "16".to_string(),
                "--spec-draft-n-min".to_string(),
                "0".to_string(),
                "--spec-draft-p-min".to_string(),
                "0.750".to_string(),
            ]
        );
        assert!(
            !args.iter().any(|a| a == "--spec-draft-model"),
            "MTP must not emit --spec-draft-model"
        );
    }

    #[test]
    fn append_spec_args_draft_model_emits_path_and_ngl() {
        let mut args: Vec<String> = Vec::new();
        let spec = SpecResolved {
            mode: SpecMode::DraftModel,
            draft_max: 8,
            draft_min: 0,
            draft_p_min: 0.6,
            draft_model_path: Some("/m/draft.gguf".into()),
            draft_gpu_layers: 99,
            reason: "draft-model".into(),
        };
        append_spec_args(&mut args, &spec);
        assert!(args.iter().any(|a| a == "--spec-draft-model"));
        assert!(args.iter().any(|a| a == "/m/draft.gguf"));
        assert!(args.iter().any(|a| a == "--spec-draft-ngl"));
        assert!(args.iter().any(|a| a == "99"));
    }

    #[test]
    fn append_spec_args_moe_mtp_clamped_draft_max() {
        // SpecResolved is already post-clamp; verify the value is emitted as-is.
        let mut args: Vec<String> = Vec::new();
        let spec = SpecResolved {
            mode: SpecMode::Mtp,
            draft_max: 4,
            draft_min: 0,
            draft_p_min: 0.75,
            draft_model_path: None,
            draft_gpu_layers: -1,
            reason: "MoE-conservative".into(),
        };
        append_spec_args(&mut args, &spec);
        let max_idx = args.iter().position(|a| a == "--spec-draft-n-max").unwrap();
        assert_eq!(args[max_idx + 1], "4");
    }

    #[test]
    fn test_find_gguf_no_files() {
        let dir = std::env::temp_dir().join("lmforge_test_empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_gguf_file(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_find_gguf_picks_largest() {
        let dir = std::env::temp_dir().join("lmforge_test_gguf");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("model-small.gguf"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("model-large.gguf"), vec![0u8; 500]).unwrap();

        let result = find_gguf_file(&dir).unwrap();
        assert_eq!(result.file_name().unwrap(), "model-large.gguf");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_find_gguf_skips_mmproj_for_main_weights() {
        // VLM scenario: main weights + mmproj sidecar in the same dir.
        // find_gguf_file must NEVER return the mmproj file (would break llama-server).
        let dir = std::env::temp_dir().join("lmforge_test_gguf_vlm_main");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"),
            vec![0u8; 100],
        )
        .unwrap();
        // mmproj is intentionally LARGER than weights to confirm it's skipped
        // even when it would win the largest-file selection.
        std::fs::write(
            dir.join("mmproj-Qwen2.5-VL-7B-Instruct-f16.gguf"),
            vec![0u8; 5000],
        )
        .unwrap();

        let result = find_gguf_file(&dir).unwrap();
        assert_eq!(
            result.file_name().unwrap(),
            "Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf",
            "find_gguf_file must skip mmproj sidecars"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_find_mmproj_file_finds_sidecar() {
        let dir = std::env::temp_dir().join("lmforge_test_find_mmproj");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf"), b"weights").unwrap();
        std::fs::write(dir.join("mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"), b"proj").unwrap();

        let result = find_mmproj_file(&dir).unwrap();
        assert_eq!(
            result.file_name().unwrap(),
            "mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_find_mmproj_file_none_for_chat_only_model() {
        let dir = std::env::temp_dir().join("lmforge_test_no_mmproj");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Qwen3-8B-Q4_K_M.gguf"), b"weights").unwrap();

        assert!(find_mmproj_file(&dir).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ── plan_runtime ──────────────────────────────────────────────────────
    //
    // All plan_runtime tests share a single mutex because they all read (and
    // some write) `LMFORGE_LLAMACPP_NGL` / `LMFORGE_LLAMACPP_CTX`. Cargo runs
    // unit tests in parallel by default; without serialisation the env-var
    // overrides from one test would bleed into another.

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_overrides() {
        // SAFETY: process-global env mutation, gated by `ENV_LOCK`.
        unsafe {
            std::env::remove_var("LMFORGE_LLAMACPP_NGL");
            std::env::remove_var("LMFORGE_LLAMACPP_CTX");
        }
    }

    #[test]
    fn plan_no_gpu_returns_zero_ngl() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();
        let p = plan_runtime(GpuVendor::None, 16.0, 0.0, 4.0, 0.0, false);
        assert_eq!(p.ngl, 0);
    }

    #[test]
    fn plan_apple_unified_full_offload() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();
        let p = plan_runtime(GpuVendor::Apple, 36.0, 0.0, 8.0, 0.0, false);
        assert_eq!(p.ngl, 99);
    }

    #[test]
    fn plan_nvidia_fits_full_offload() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 16.0, 5.0, 0.0, false);
        assert_eq!(p.ngl, 99);
    }

    #[test]
    fn plan_nvidia_partial_offload_when_tight() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();
        // RTX 3060 4 GB: 3.5 free, 5 GB model → must spill.
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 3.5, 5.0, 0.0, false);
        assert!((1..=98).contains(&p.ngl), "got ngl={}", p.ngl);
    }

    #[test]
    fn plan_nvidia_zero_when_no_budget() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 0.5, 5.0, 0.0, false);
        assert_eq!(p.ngl, 0);
    }

    #[test]
    fn plan_vlm_ctx_scales_with_post_load_free() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();

        // 16 GB free, 4 GB model + 0.5 GB mmproj → 11.5 GB after load → 8192.
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 16.0, 4.0, 0.5, true);
        assert_eq!(p.ctx_size, 8192);

        // 4 GB free, 2.4 GB model + 0.6 GB mmproj → 1.0 GB → 1024.
        let p = plan_runtime(GpuVendor::Nvidia, 16.0, 4.0, 2.4, 0.6, true);
        assert_eq!(p.ctx_size, 1024);

        // 6.5 GB free, 2.4 GB + 0.6 GB → 3.5 GB after → 4096.
        let p = plan_runtime(GpuVendor::Nvidia, 16.0, 6.5, 2.4, 0.6, true);
        assert_eq!(p.ctx_size, 4096);
    }

    // ── resolve_cache_ram_mib ────────────────────────────────────────────────

    fn clear_cache_ram_override() {
        // SAFETY: process-global env mutation, gated by `ENV_LOCK`.
        unsafe { std::env::remove_var("LMFORGE_LLAMACPP_CACHE_RAM_MIB") }
    }

    #[test]
    fn cache_ram_default_on_16gb_box() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_cache_ram_override();
        // 16 GB RAM → 25% = 4 GiB, exactly at the cap.
        assert_eq!(resolve_cache_ram_mib(16.0), 4096);
    }

    #[test]
    fn cache_ram_default_on_8gb_box() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_cache_ram_override();
        // 8 GB RAM → 25% = 2 GiB, below cap.
        assert_eq!(resolve_cache_ram_mib(8.0), 2048);
    }

    #[test]
    fn cache_ram_caps_on_large_ram() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_cache_ram_override();
        // 128 GB RAM → would be 32 GiB unbounded, but cap is 4 GiB.
        assert_eq!(resolve_cache_ram_mib(128.0), 4096);
    }

    #[test]
    fn cache_ram_zero_on_no_ram_info() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_cache_ram_override();
        assert_eq!(resolve_cache_ram_mib(0.0), 0);
        assert_eq!(resolve_cache_ram_mib(-1.0), 0);
        assert_eq!(resolve_cache_ram_mib(f32::NAN), 0);
    }

    #[test]
    fn cache_ram_env_override_wins() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_cache_ram_override();

        unsafe { std::env::set_var("LMFORGE_LLAMACPP_CACHE_RAM_MIB", "8192") }
        // Override exceeds the default cap — operator opt-in to bigger cache.
        assert_eq!(resolve_cache_ram_mib(16.0), 8192);

        // 0 disables the cache entirely.
        unsafe { std::env::set_var("LMFORGE_LLAMACPP_CACHE_RAM_MIB", "0") }
        assert_eq!(resolve_cache_ram_mib(16.0), 0);

        clear_cache_ram_override();
    }

    // ── resolve_executable ───────────────────────────────────────────────────
    //
    // All four tests mutate `LMFORGE_LLAMACPP_BIN`. Reuse the module-level
    // `ENV_LOCK` mutex so cargo's parallel test runner can't interleave them:
    // one test setting the var while another reads it would otherwise produce
    // intermittent failures like
    //   `assertion left == right failed` (saw the override path on a "no env"
    //   test, or saw "llama-server" on the override test).

    #[test]
    fn resolve_executable_prefers_engines_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("LMFORGE_LLAMACPP_BIN") };

        let dir = std::env::temp_dir().join("lmforge_test_resolve_engines_dir");
        let _ = std::fs::remove_dir_all(&dir);
        let engines = dir.join("engines");
        std::fs::create_dir_all(&engines).unwrap();
        let bin = engines.join("llama-server");
        std::fs::write(&bin, "fake").unwrap();

        let resolved = resolve_executable("llama-server", &dir);
        assert_eq!(resolved, bin);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_executable_falls_back_to_path_when_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("LMFORGE_LLAMACPP_BIN") };

        let dir = std::env::temp_dir().join("lmforge_test_resolve_no_binary");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("engines")).unwrap();

        let resolved = resolve_executable("llama-server", &dir);
        // No file at <data_dir>/engines/llama-server → PATH fallback.
        assert_eq!(resolved, PathBuf::from("llama-server"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_executable_env_override_wins() {
        let _g = ENV_LOCK.lock().unwrap();

        let dir = std::env::temp_dir().join("lmforge_test_resolve_env_override");
        let _ = std::fs::remove_dir_all(&dir);
        let engines = dir.join("engines");
        std::fs::create_dir_all(&engines).unwrap();
        let staged = engines.join("llama-server");
        std::fs::write(&staged, "staged").unwrap();
        let custom = dir.join("custom-llama-server");
        std::fs::write(&custom, "custom").unwrap();

        unsafe { std::env::set_var("LMFORGE_LLAMACPP_BIN", custom.to_string_lossy().to_string()) };
        let resolved = resolve_executable("llama-server", &dir);
        unsafe { std::env::remove_var("LMFORGE_LLAMACPP_BIN") };

        assert_eq!(resolved, custom);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_executable_env_override_ignored_when_missing_file() {
        let _g = ENV_LOCK.lock().unwrap();

        let dir = std::env::temp_dir().join("lmforge_test_resolve_env_override_bad");
        let _ = std::fs::remove_dir_all(&dir);
        let engines = dir.join("engines");
        std::fs::create_dir_all(&engines).unwrap();
        let staged = engines.join("llama-server");
        std::fs::write(&staged, "staged").unwrap();

        unsafe { std::env::set_var("LMFORGE_LLAMACPP_BIN", "/nonexistent/path") };
        let resolved = resolve_executable("llama-server", &dir);
        unsafe { std::env::remove_var("LMFORGE_LLAMACPP_BIN") };

        // Bad override → fall through to engines dir.
        assert_eq!(resolved, staged);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plan_env_overrides_behave_correctly() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_overrides();

        unsafe {
            std::env::set_var("LMFORGE_LLAMACPP_NGL", "32");
            std::env::set_var("LMFORGE_LLAMACPP_CTX", "16384");
        }
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 16.0, 5.0, 0.0, true);
        assert_eq!(p.ngl, 32);
        assert_eq!(p.ctx_size, 16384);

        unsafe {
            std::env::set_var("LMFORGE_LLAMACPP_NGL", "9999");
            std::env::remove_var("LMFORGE_LLAMACPP_CTX");
        }
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 16.0, 5.0, 0.0, false);
        assert_eq!(p.ngl, 99, "ngl override must clamp to 99");

        unsafe {
            std::env::remove_var("LMFORGE_LLAMACPP_NGL");
            std::env::set_var("LMFORGE_LLAMACPP_CTX", "128");
        }
        let p = plan_runtime(GpuVendor::Nvidia, 32.0, 16.0, 4.0, 0.5, true);
        assert_eq!(
            p.ctx_size, 8192,
            "ctx override below 512 floor must be ignored"
        );

        clear_overrides();
    }
}
