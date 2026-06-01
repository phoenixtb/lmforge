//! vLLM engine adapter — opt-in tier.
//!
//! ## Design choices vs. SGLang
//!
//!  * **Spawn binary, not `python -m`**: vLLM ships an entry point named
//!    `vllm` (= `python -m vllm.entrypoints.openai.api_server`). Using the
//!    console script directly means the venv's `activate` script and
//!    `pyvenv.cfg` get honoured automatically, including the right
//!    `sys.path` order for editable installs.
//!
//!  * **OpenAI-server only**: we never spawn `vllm` in non-serve modes
//!    (offline batch, async-llm) because LMForge's HTTP front-end expects
//!    `/v1/chat/completions` + `/v1/completions`. vLLM's serve subcommand
//!    is the right contract here.
//!
//!  * **No embeddings, no reranking**: vLLM has experimental pooled
//!    embedding support (per-model) but our catalog is GGUF-tagged for
//!    embed/rerank — those slots stay on the llama.cpp sidecar. The
//!    `EngineAdapter::start` impl refuses non-Chat roles loudly so the
//!    Manager's slot-router never silently routes the wrong way.
//!
//!  * **No VLM auto-template plumbing**: vLLM auto-detects HF config for
//!    most multimodal models. If a future user-supplied model fails, the
//!    daemon will spawn → vLLM will log "no chat template registered" →
//!    `EngineState.last_errors` surfaces the tail. Cheaper than mirroring
//!    SGLang's `detect_vlm_chat_template` map for an opt-in engine.
//!
//!  * **GPU mem fraction**: vLLM's `--gpu-memory-utilization` defaults to
//!    0.9 (too aggressive for shared boxes; first-OOM victim is the OS
//!    rather than vLLM itself). We pin 0.85 by default, env-overridable
//!    via `LMFORGE_VLLM_GPU_MEM_UTIL` (matches SGLang's knob naming).
//!
//!  * **Tensor parallel**: only when `gpu_count > 1`. Single-GPU users
//!    pay no tensor-parallel overhead.
//!
//!  * **NVFP4 caveat surfaced at start**: Phase 3 M3.4 emits a soft warning
//!    when sm_120 + NVFP4 is detected (open vLLM upstream bug).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, ModelRole};
use crate::model::downloader::DownloadProgress;

/// Env knob: fraction of GPU memory vLLM is allowed to claim at start.
/// Range clamped to [0.05, 0.98]; out-of-range values fall back to default.
const ENV_GPU_MEM_UTIL: &str = "LMFORGE_VLLM_GPU_MEM_UTIL";
/// vLLM defaults to 0.9 which OOMs on any 16 GB consumer card that also
/// renders X11 + chrome/cursor. 0.7 leaves ~4.5 GB headroom on the user's
/// 15.4 GB card — enough for a 4-bit 7B model, the desktop, and the model's
/// KV cache. Tune via `LMFORGE_VLLM_GPU_MEM_UTIL` for dedicated headless
/// servers (0.85+ is fine there).
const DEFAULT_GPU_MEM_UTIL: f32 = 0.7;

/// Env knob: explicit `--max-model-len` override. Useful when the model's
/// HF config advertises a window the GPU can't actually hold (Qwen3 32K on
/// a 16 GB card OOMs at start without this).
const ENV_MAX_MODEL_LEN: &str = "LMFORGE_VLLM_MAX_MODEL_LEN";

/// Env knob: test-only override of the resolved vLLM binary path. Mirrors
/// llama.cpp's `LMFORGE_LLAMACPP_BIN`. Production users should never need it.
const ENV_VLLM_BIN: &str = "LMFORGE_VLLM_BIN";

/// Test-only: where to find `vllm` when neither the venv nor PATH has it.
const FALLBACK_EXECUTABLE: &str = "vllm";

#[derive(Clone)]
pub struct VllmAdapter {
    /// Fallback executable when the managed venv hasn't been resolved.
    /// Production code always prefers `resolve_executable` over this.
    pub executable: String,
}

impl Default for VllmAdapter {
    fn default() -> Self {
        Self {
            executable: FALLBACK_EXECUTABLE.to_string(),
        }
    }
}

impl VllmAdapter {
    /// Resolve the vLLM console script.
    ///
    /// Order of preference:
    ///   1. `LMFORGE_VLLM_BIN` env var (tests / advanced overrides).
    ///   2. `<data_dir>/engines/vllm/venv/bin/vllm` — the venv installer
    ///      creates this; it's the canonical path.
    ///   3. `self.executable` (== "vllm") on `PATH` — only useful in
    ///      developer setups that installed vLLM globally.
    fn resolve_executable(&self, data_dir: &Path) -> PathBuf {
        if let Ok(override_path) = std::env::var(ENV_VLLM_BIN)
            && !override_path.trim().is_empty()
        {
            return PathBuf::from(override_path);
        }
        let venv_bin = data_dir
            .join("engines")
            .join("vllm")
            .join("venv")
            .join("bin")
            .join("vllm");
        if venv_bin.is_file() {
            venv_bin
        } else {
            PathBuf::from(&self.executable)
        }
    }

    /// Resolve the venv's `python3` interpreter. Needed for the
    /// `huggingface_hub` pull step which doesn't have a console wrapper.
    fn resolve_python(&self, data_dir: &Path) -> PathBuf {
        let venv_python = data_dir
            .join("engines")
            .join("vllm")
            .join("venv")
            .join("bin")
            .join("python3");
        if venv_python.is_file() {
            venv_python
        } else {
            // Last resort: hope `python3` is on PATH AND has vllm installed.
            // In practice this only matters for tests; the installer
            // *always* creates the venv before the adapter spawns.
            PathBuf::from("python3")
        }
    }

    /// Read `gpu_count` from the cached hardware profile so we can wire
    /// `--tensor-parallel-size` only on multi-GPU hosts.
    ///
    /// Returns 1 on any failure (no profile, parse error). vLLM treats
    /// `tensor-parallel-size=1` as a no-op, so misreading is harmless.
    fn detect_gpu_count(data_dir: &Path) -> u8 {
        let path = data_dir.join("hardware.json");
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(profile) =
                serde_json::from_str::<crate::hardware::probe::HardwareProfile>(&content)
        {
            return profile.gpu_count.max(1);
        }
        1
    }

    /// Look up the configured GPU memory utilization fraction. Capped to a
    /// safe range so a typo can't kill the daemon by setting "1.5".
    fn gpu_mem_util() -> f32 {
        std::env::var(ENV_GPU_MEM_UTIL)
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|f| (0.05..=0.98).contains(f))
            .unwrap_or(DEFAULT_GPU_MEM_UTIL)
    }
}

impl EngineAdapter for VllmAdapter {
    /// Reuse SGLang's huggingface_hub strategy verbatim — safetensors repos
    /// share the same shard-manifest convention regardless of which engine
    /// will load them.
    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        data_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool> {
        std::fs::create_dir_all(dest_dir)
            .context("Failed to create model destination directory")?;

        info!(repo, dest = %dest_dir.display(), "vLLM: starting native huggingface_hub pull");

        let _ = progress_tx
            .send(DownloadProgress::Started {
                repo: repo.to_string(),
                files: 0,
            })
            .await;

        let python_snippet = format!(
            "import sys; \
             from huggingface_hub import snapshot_download; \
             snapshot_download(repo_id='{repo}', local_dir='{dest}', local_dir_use_symlinks=False); \
             print('OK')",
            repo = repo,
            dest = dest_dir.to_string_lossy(),
        );

        let python = self.resolve_python(data_dir);
        debug!(python = %python.display(), "vLLM pull: using interpreter");

        let output = Command::new(&python)
            .args(["-c", &python_snippet])
            .output()
            .await
            .context("Failed to spawn python for huggingface_hub pull")?;

        if output.status.success() {
            let total_bytes = dir_size(dest_dir);
            info!(repo, total_bytes, "vLLM: huggingface_hub pull completed");

            let _ = progress_tx
                .send(DownloadProgress::Completed {
                    repo: repo.to_string(),
                    total_bytes,
                })
                .await;

            Ok(true)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!(repo, stderr = %stderr, stdout = %stdout, "vLLM: huggingface_hub pull failed");
            let user_error = extract_python_error(&stderr);

            let _ = progress_tx
                .send(DownloadProgress::Failed {
                    error: user_error.clone(),
                })
                .await;

            anyhow::bail!("huggingface_hub pull failed: {}", user_error)
        }
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
        if role != ModelRole::Chat {
            anyhow::bail!(
                "vLLM only serves Chat models in this build. \
                 Embed/Rerank slots route to llama.cpp — \
                 ensure your model's `engine` field is `llamacpp`."
            );
        }

        info!(model_id = %model_id, port = port, "Spawning vLLM OpenAI-server");

        let stdout_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stdout")?;
        let stderr_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stderr")?;

        let port_str = port.to_string();
        let model_path = model_dir.to_string_lossy().to_string();
        let gpu_mem = format!("{:.3}", Self::gpu_mem_util());

        // Soft warning when an NVFP4-quantized model is paired with sm_120.
        // The cu130 wheel now ships sm_120 cubins (vLLM 0.20+), but the MoE
        // attention path for NVFP4 still hits an upstream bug under batch>1.
        // Standard dense models work; users see this once at start and can
        // ignore if running batch=1.
        if detect_nvfp4_quant(model_dir) && hardware_is_sm120(data_dir) {
            warn!(
                model = %model_id,
                "vLLM: NVFP4 + sm_120 still hits the MoE attention bug under \
                 batch>1. Workaround: keep concurrent requests = 1 OR use a \
                 non-NVFP4 quant (AWQ/GPTQ-4bit). Tracking upstream."
            );
        }

        // vLLM 0.20+ defaults to quiet per-request logging, so we don't need
        // `--disable-log-requests` anymore (and it was removed from the CLI
        // in 0.21). If a future release brings the spam back, gate the flag
        // behind a version check rather than pinning it blindly.
        // `--served-model-name` makes vLLM advertise the model under our
        // canonical id (`RedHatAI/Qwen3-1.7B-...`) instead of the path
        // basename. Without this flag, requests like
        // `{"model": "RedHatAI/Qwen3-1.7B-quantized.w4a16"}` get 404'd
        // because vLLM only knows the model as `qwen3-1.7b-quantized.w4a16`.
        let mut args: Vec<String> = vec![
            "serve".to_string(),
            model_path,
            "--served-model-name".to_string(),
            model_id.to_string(),
            "--port".to_string(),
            port_str,
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--enable-prefix-caching".to_string(),
            "--gpu-memory-utilization".to_string(),
            gpu_mem,
        ];

        // Multi-GPU tensor parallel — only when we *have* multiple GPUs.
        let gpu_count = Self::detect_gpu_count(data_dir);
        if gpu_count > 1 {
            args.push("--tensor-parallel-size".to_string());
            args.push(gpu_count.to_string());
            info!(gpu_count, "vLLM: enabling tensor parallelism");
        }

        // Optional explicit window override (rescues users from
        // HF-config-advertises-32K-but-card-is-16GB OOMs at start).
        if let Ok(max_len) = std::env::var(ENV_MAX_MODEL_LEN)
            && !max_len.trim().is_empty()
        {
            args.push("--max-model-len".to_string());
            args.push(max_len.trim().to_string());
        }

        let bin = self.resolve_executable(data_dir);
        info!(bin = %bin.display(), "vLLM start: using executable");

        // PATH injection: vLLM's FlashInfer JIT calls `subprocess.run("ninja", ...)`
        // for kernel compilation. The `ninja` binary lives in the venv's `bin/`
        // dir, but the child inherits LMForge's PATH (which is whatever the
        // user's shell exported). Prepending the venv bin dir makes ninja —
        // and any other companion binaries — discoverable without forcing
        // users to keep the venv activated.
        let venv_bin_dir = bin
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = if existing_path.is_empty() {
            venv_bin_dir.to_string_lossy().into_owned()
        } else {
            format!("{}:{}", venv_bin_dir.display(), existing_path)
        };

        // Process-group isolation: vLLM internally spawns an `EngineCore`
        // subprocess via Python multiprocessing that holds the bulk of the
        // VRAM (CUDA graphs + KV cache). When we SIGTERM the parent `vllm`
        // CLI, the EngineCore child is reparented to init and keeps holding
        // ~13 GB of VRAM forever. Putting the spawn in its own process group
        // lets `stop()` killpg() the whole tree at once.
        let mut command = Command::new(&bin);
        command
            .args(&args)
            .env("PATH", &new_path)
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0); // become group leader (= own pgid)

        let child = command.spawn().with_context(|| {
            format!(
                "Failed to spawn vLLM at {}. Is the engine installed? Run: lmforge engine install vllm",
                bin.display()
            )
        })?;

        Ok(ActiveEngine {
            process: child,
            model_id: model_id.to_string(),
            spec_observer: None,
            spec_mode: crate::engine::speculative::SpecMode::Off,
        })
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        if let Some(pid) = active_engine.process.id() {
            info!(pid, model = %active_engine.model_id, "vLLM: SIGTERM (process-group)");
            // killpg the WHOLE group — see `start()` for why vLLM's EngineCore
            // child is otherwise reparented to init holding ~13 GB of VRAM.
            // Negative PID == killpg in POSIX `kill(2)`.
            #[cfg(unix)]
            {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = active_engine.process.kill().await;
            }

            // vLLM's PagedAttention + CUDA-graph teardown runs ~5-10s on
            // 7B+ models. 10s budget gives the child a real shot at clean
            // exit before we escalate.
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                active_engine.process.wait(),
            )
            .await
            {
                Ok(_) => debug!("vLLM exited cleanly"),
                Err(_) => {
                    warn!("vLLM SIGTERM timed out; sending SIGKILL to group");
                    #[cfg(unix)]
                    {
                        use nix::sys::signal::{Signal, kill};
                        use nix::unistd::Pid;
                        let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGKILL);
                    }
                    let _ = active_engine.process.kill().await;
                }
            }
        }
        Ok(())
    }
}

// ── helpers (file-private) ────────────────────────────────────────────────

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn extract_python_error(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "huggingface_hub pull failed with no output".to_string())
}

/// Detect NVFP4 quantization from the model's HF config.json.
///
/// vLLM caveat: NVFP4 + Blackwell sm_120 MoE attention is buggy under batch>1
/// (see ADR-001 §"vLLM caveats"). We only need a binary signal here — actual
/// behaviour selection is left to the user (workaround: batch=1 or AWQ).
fn detect_nvfp4_quant(model_dir: &Path) -> bool {
    let content = match std::fs::read_to_string(model_dir.join("config.json")) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    // Check both common locations: `quantization_config.quant_method` (newer)
    // and the top-level `quantization` string (older models).
    let q1 = v["quantization_config"]["quant_method"].as_str().unwrap_or("");
    let q2 = v["quantization_config"]["quant_type"].as_str().unwrap_or("");
    let q3 = v["quantization"].as_str().unwrap_or("");
    let combined = format!("{} {} {}", q1, q2, q3).to_lowercase();
    combined.contains("nvfp4") || combined.contains("fp4")
}

/// Read the cached hardware profile and check whether the box is sm_120.
/// Used by the NVFP4 caveat warning. Defaults to `false` on any read error.
fn hardware_is_sm120(data_dir: &Path) -> bool {
    let path = data_dir.join("hardware.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(profile) = serde_json::from_str::<crate::hardware::probe::HardwareProfile>(&content)
    else {
        return false;
    };
    matches!(profile.compute_cap, Some((12, _)))
}

// ── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests in this file mutate `LMFORGE_VLLM_*` env vars. Serialize so
    /// `--test-threads=N>1` doesn't tear them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_executable_prefers_venv() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var(ENV_VLLM_BIN) };

        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_venv_present");
        let venv_bin = data_dir
            .join("engines")
            .join("vllm")
            .join("venv")
            .join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        let venv_vllm = venv_bin.join("vllm");
        std::fs::write(&venv_vllm, b"#!/bin/sh\nexit 0\n").unwrap();

        let adapter = VllmAdapter::default();
        assert_eq!(adapter.resolve_executable(&data_dir), venv_vllm);

        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn resolve_executable_falls_back_to_path_when_venv_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var(ENV_VLLM_BIN) };

        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_venv_missing");
        let _ = std::fs::remove_dir_all(&data_dir);

        let adapter = VllmAdapter::default();
        assert_eq!(
            adapter.resolve_executable(&data_dir),
            PathBuf::from(FALLBACK_EXECUTABLE)
        );
    }

    #[test]
    fn resolve_executable_env_override_wins() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var(ENV_VLLM_BIN, "/tmp/some/vllm-shim") };

        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_env_override");
        // Build a real venv too — env override must beat it.
        let venv_bin = data_dir
            .join("engines")
            .join("vllm")
            .join("venv")
            .join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::fs::write(venv_bin.join("vllm"), b"x").unwrap();

        let adapter = VllmAdapter::default();
        let resolved = adapter.resolve_executable(&data_dir);
        assert_eq!(resolved, PathBuf::from("/tmp/some/vllm-shim"));

        unsafe { std::env::remove_var(ENV_VLLM_BIN) };
        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn gpu_mem_util_defaults_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var(ENV_GPU_MEM_UTIL) };
        assert_eq!(VllmAdapter::gpu_mem_util(), DEFAULT_GPU_MEM_UTIL);
    }

    #[test]
    fn gpu_mem_util_honors_in_range_env() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var(ENV_GPU_MEM_UTIL, "0.5") };
        assert!((VllmAdapter::gpu_mem_util() - 0.5).abs() < 1e-6);
        unsafe { std::env::remove_var(ENV_GPU_MEM_UTIL) };
    }

    #[test]
    fn gpu_mem_util_rejects_out_of_range() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var(ENV_GPU_MEM_UTIL, "1.5") };
        assert_eq!(VllmAdapter::gpu_mem_util(), DEFAULT_GPU_MEM_UTIL);
        unsafe { std::env::set_var(ENV_GPU_MEM_UTIL, "0.01") };
        assert_eq!(VllmAdapter::gpu_mem_util(), DEFAULT_GPU_MEM_UTIL);
        unsafe { std::env::set_var(ENV_GPU_MEM_UTIL, "garbage") };
        assert_eq!(VllmAdapter::gpu_mem_util(), DEFAULT_GPU_MEM_UTIL);
        unsafe { std::env::remove_var(ENV_GPU_MEM_UTIL) };
    }

    #[test]
    fn detect_gpu_count_defaults_to_1_without_profile() {
        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_no_profile");
        let _ = std::fs::remove_dir_all(&data_dir);
        assert_eq!(VllmAdapter::detect_gpu_count(&data_dir), 1);
    }

    #[test]
    fn detect_gpu_count_reads_profile() {
        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_profile_read");
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).unwrap();
        let profile = r#"{
            "os": "linux",
            "arch": "x86_64",
            "is_tegra": false,
            "gpu_vendor": "nvidia",
            "vram_gb": 24.0,
            "unified_mem": false,
            "total_ram_gb": 64.0,
            "cpu_cores": 16,
            "cpu_model": "Test",
            "schema_version": 2,
            "compute_cap": [9, 0],
            "cuda_runtime_version": "12.8",
            "cuda_driver_version": "560.0",
            "os_family": "linux",
            "is_wsl": false,
            "gpu_count": 4
        }"#;
        std::fs::write(data_dir.join("hardware.json"), profile).unwrap();
        assert_eq!(VllmAdapter::detect_gpu_count(&data_dir), 4);
        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn detect_nvfp4_quant_positive() {
        let dir = std::env::temp_dir().join("lmforge_vllm_test_nvfp4");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"quantization_config":{"quant_method":"nvfp4"}}"#,
        )
        .unwrap();
        assert!(detect_nvfp4_quant(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_nvfp4_quant_negative_awq() {
        let dir = std::env::temp_dir().join("lmforge_vllm_test_awq");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"quantization_config":{"quant_method":"awq"}}"#,
        )
        .unwrap();
        assert!(!detect_nvfp4_quant(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_nvfp4_quant_negative_no_config() {
        let dir = std::env::temp_dir().join("lmforge_vllm_test_no_config");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!detect_nvfp4_quant(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hardware_is_sm120_positive() {
        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_sm120");
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).unwrap();
        let profile = r#"{
            "os":"linux","arch":"x86_64","is_tegra":false,"gpu_vendor":"nvidia",
            "vram_gb":15.4,"unified_mem":false,"total_ram_gb":16.0,"cpu_cores":12,
            "cpu_model":"Test","schema_version":2,"compute_cap":[12,0],
            "cuda_runtime_version":"13.0","cuda_driver_version":"580.0",
            "os_family":"linux","is_wsl":false,"gpu_count":1
        }"#;
        std::fs::write(data_dir.join("hardware.json"), profile).unwrap();
        assert!(hardware_is_sm120(&data_dir));
        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn hardware_is_sm120_negative_hopper() {
        let data_dir = std::env::temp_dir().join("lmforge_vllm_test_hopper");
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).unwrap();
        let profile = r#"{
            "os":"linux","arch":"x86_64","is_tegra":false,"gpu_vendor":"nvidia",
            "vram_gb":80.0,"unified_mem":false,"total_ram_gb":256.0,"cpu_cores":64,
            "cpu_model":"Test","schema_version":2,"compute_cap":[9,0],
            "cuda_runtime_version":"12.8","cuda_driver_version":"550.0",
            "os_family":"linux","is_wsl":false,"gpu_count":1
        }"#;
        std::fs::write(data_dir.join("hardware.json"), profile).unwrap();
        assert!(!hardware_is_sm120(&data_dir));
        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn extract_python_error_gets_last_line() {
        let s = "Traceback (most recent call last):\n  File ...\nRepoNotFound: bad\n";
        assert_eq!(extract_python_error(s), "RepoNotFound: bad");
    }

    #[test]
    fn extract_python_error_empty() {
        assert_eq!(
            extract_python_error(" \n  "),
            "huggingface_hub pull failed with no output"
        );
    }

    /// vLLM must HARD-refuse embed/rerank roles. We can't actually spawn
    /// vLLM in CI (no GPU), so we verify the role-check by constructing
    /// the role enum and asserting the start() short-circuits before any
    /// process spawn. Using `tokio::runtime::Builder` here keeps the test
    /// sync-runnable from `cargo test --lib`.
    #[test]
    fn start_refuses_embed_role() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let adapter = VllmAdapter::default();
            let tmp = std::env::temp_dir().join("lmforge_vllm_test_start_embed");
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(tmp.join("logs")).unwrap();
            let result = adapter
                .start(
                    "test-embed",
                    &tmp,
                    9999,
                    &tmp,
                    &tmp.join("logs"),
                    ModelRole::Embed,
                )
                .await;
            let err = match result {
                Ok(_) => panic!("Embed role must be refused, got Ok"),
                Err(e) => e,
            };
            let msg = err.to_string();
            assert!(
                msg.contains("Chat") && msg.contains("llama.cpp"),
                "Refusal must steer users to the sidecar: {}",
                msg
            );
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    #[test]
    fn start_refuses_rerank_role() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let adapter = VllmAdapter::default();
            let tmp = std::env::temp_dir().join("lmforge_vllm_test_start_rerank");
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(tmp.join("logs")).unwrap();
            let result = adapter
                .start(
                    "test-rerank",
                    &tmp,
                    9998,
                    &tmp,
                    &tmp.join("logs"),
                    ModelRole::Rerank,
                )
                .await;
            let err = match result {
                Ok(_) => panic!("Rerank role must be refused, got Ok"),
                Err(e) => e,
            };
            assert!(err.to_string().contains("Chat"));
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }
}
