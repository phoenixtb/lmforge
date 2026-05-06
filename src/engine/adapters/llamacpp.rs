use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, ModelRole};
use crate::hardware::probe::{GpuVendor, HardwareProfile};
use crate::model::downloader::DownloadProgress;

#[derive(Clone)]
pub struct LlamacppAdapter {
    pub executable: String,
}

impl Default for LlamacppAdapter {
    fn default() -> Self {
        Self {
            executable: "llama-server".to_string(), // Typical binary payload
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
        _data_dir: &Path,
        logs_dir: &Path,
        role: ModelRole,
    ) -> Result<ActiveEngine> {
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

        let child = Command::new(&self.executable)
            .args(&args)
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn native Llama-server engine")?;

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
            GpuVendor::Nvidia | GpuVendor::Amd => {
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
    let mut profile =
        crate::hardware::probe::detect_platform().unwrap_or_else(|_| HardwareProfile {
            os: crate::hardware::probe::Os::Unknown,
            arch: crate::hardware::probe::Arch::Unknown,
            is_tegra: false,
            gpu_vendor: GpuVendor::None,
            vram_gb: 0.0,
            unified_mem: false,
            total_ram_gb: 0.0,
            cpu_cores: 0,
            cpu_model: String::new(),
        });
    profile.vram_gb = crate::hardware::vram::estimate_vram(&profile);
    profile
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
