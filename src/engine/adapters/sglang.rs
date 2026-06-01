use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, ModelRole};
use crate::model::downloader::DownloadProgress;

#[derive(Clone)]
pub struct SglangAdapter {
    /// Fallback executable when the managed venv hasn't been resolved yet.
    /// At runtime [`resolve_python`] is used in preference; this is only the
    /// last-resort default if data_dir layout can't be detected.
    pub executable: String,
}

impl Default for SglangAdapter {
    fn default() -> Self {
        Self {
            executable: "python3".to_string(),
        }
    }
}

impl SglangAdapter {
    /// Resolve the Python interpreter to spawn SGLang with.
    ///
    /// The installer creates an isolated venv at
    /// `<data_dir>/engines/sglang/venv/bin/python3` and pip-installs SGLang
    /// there. Spawning the system `python3` would invariably fail with
    /// `ModuleNotFoundError: sglang`, so we always prefer the venv path.
    ///
    /// Falls back to `self.executable` only if the venv binary is missing
    /// (which means the install never completed and the daemon should
    /// re-run `lmforge init`).
    fn resolve_python(&self, data_dir: &Path) -> PathBuf {
        let venv = data_dir
            .join("engines")
            .join("sglang")
            .join("venv")
            .join("bin")
            .join("python3");
        if venv.is_file() {
            venv
        } else {
            PathBuf::from(&self.executable)
        }
    }

}

impl EngineAdapter for SglangAdapter {
    /// Use huggingface_hub.snapshot_download to pull the model natively.
    ///
    /// SGLang's ecosystem is Python/HF-native and safetensors repos use a shard manifest
    /// (model.safetensors.index.json) that huggingface_hub handles transparently — including
    /// shard discovery, LFS pointers, token auth, and resume. Replicating this in Rust would
    /// be significant complexity for no gain.
    ///
    /// We emit coarse SSE events (Started + Completed/Failed) since tqdm output is not
    /// parseable into per-file JSON progress. The caller's SSE stream will show start and end.
    ///
    /// Returns:
    ///   Ok(true)  — native pull succeeded; caller updates ModelIndex.
    ///   Err(e)    — native pull failed; caller surfaces error via SSE.
    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        data_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool> {
        std::fs::create_dir_all(dest_dir)
            .context("Failed to create model destination directory")?;

        info!(repo, dest = %dest_dir.display(), "SGLang: starting native huggingface_hub pull");

        let _ = progress_tx
            .send(DownloadProgress::Started {
                repo: repo.to_string(),
                files: 0, // unknown until snapshot_download resolves the manifest
            })
            .await;

        // Build the inline Python snippet. We call snapshot_download directly so we can
        // pass local_dir to put weights into LMForge's managed models directory instead of
        // the default ~/.cache/huggingface/hub (which would give us no control over path).
        let python_snippet = format!(
            "import sys; \
             from huggingface_hub import snapshot_download; \
             snapshot_download(repo_id='{repo}', local_dir='{dest}', local_dir_use_symlinks=False); \
             print('OK')",
            repo = repo,
            dest = dest_dir.to_string_lossy(),
        );

        let python = self.resolve_python(data_dir);
        debug!(python = %python.display(), "SGLang pull: using interpreter");

        let output = Command::new(&python)
            .args(["-c", &python_snippet])
            .output()
            .await
            .context("Failed to spawn python for huggingface_hub pull")?;

        if output.status.success() {
            let total_bytes = dir_size(dest_dir);
            info!(repo, total_bytes, "SGLang: huggingface_hub pull completed");

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

            // Log full output for debugging
            error!(repo, stderr = %stderr, stdout = %stdout, "SGLang: huggingface_hub pull failed");

            // Surface a clean error message — strip Python traceback boilerplate if present
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
        if role == ModelRole::Rerank {
            anyhow::bail!(
                "Re-ranking is not supported by SGLang v0.5.9 \
                 (cross-encoder support is experimental). \
                 It is available on platforms using llama.cpp."
            );
        }

        info!(model_id = %model_id, port = port, role = ?role, "Spawning native SGLang python instance");

        // Per-model log files with size-based rotation; see logging::rotation
        // for the threshold/keep tunables (LMFORGE_ENGINE_LOG_MAX_MB / KEEP).
        let stdout_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stdout")?;
        let stderr_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stderr")?;

        let port_str = port.to_string();
        let model_path = model_dir.to_string_lossy().to_string();

        // Build args dynamically based on role
        let mut args: Vec<String> = vec![
            "-m".to_string(),
            "sglang.launch_server".to_string(),
            "--port".to_string(),
            port_str,
            "--model-path".to_string(),
            model_path,
        ];

        if role == ModelRole::Embed {
            args.push("--is-embedding".to_string());
            // Detect pooling from model config.json; default to "mean" (correct for most
            // retrieval models: e5-mistral, nomic, GTE families).
            let pooling = read_pooling_from_config(model_dir).unwrap_or_else(|| "mean".to_string());
            args.push("--pooling-method".to_string());
            args.push(pooling);
        }

        // VLM: SGLang needs a `--chat-template` matching the model's multimodal
        // input convention. SGLang ships a curated registry of templates; pick
        // by config.json model_type. If the model isn't a VLM (or model_type is
        // unknown), we leave the flag off and SGLang falls back to its default.
        if let Some(template) = detect_vlm_chat_template(model_dir) {
            info!(template, "VLM detected — passing --chat-template to SGLang");
            args.push("--chat-template".to_string());
            args.push(template);
        }

        // VRAM: SGLang reserves a fraction of *total* card memory at startup for
        // KV cache. With multiple models co-resident this fraction must be tuned
        // per-slot, otherwise the second `launch_server` OOMs even if there's
        // free VRAM. Until full per-slot orchestration lands (tracked in next
        // iteration's multi-engine routing), expose this as a config knob:
        //   LMFORGE_SGLANG_MEM_FRACTION = "0.5"  (default; safe for 2 co-resident slots)
        // Operators with single-slot deployments can bump to 0.85 for max throughput.
        let mem_fraction = std::env::var("LMFORGE_SGLANG_MEM_FRACTION")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|f| (0.05..=0.95).contains(f))
            .unwrap_or(0.5);
        args.push("--mem-fraction-static".to_string());
        args.push(format!("{:.3}", mem_fraction));

        let python = self.resolve_python(data_dir);
        info!(python = %python.display(), "SGLang start: using interpreter");

        let child = Command::new(&python)
            .args(&args)
            // Future parity params: --tp 2 --chunked-prefill-size
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn native SGLang launch_server")?;

        Ok(ActiveEngine {
            process: child,
            model_id: model_id.to_string(),
            spec_observer: None,
            spec_mode: crate::engine::speculative::SpecMode::Off,
        })
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        if let Some(pid) = active_engine.process.id() {
            info!(pid, model = %active_engine.model_id, "Sending SIGTERM to violently flush RadixAttention NVIDIA VRAM block");
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

            // Wait for CUDA process to completely teardown to eliminate OOM frag errors on respawn
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                active_engine.process.wait(),
            )
            .await
            {
                Ok(_) => debug!("SGLang natively flush-exited"),
                Err(_) => {
                    warn!(
                        "SGLang SIGTERM timed out or hung on GPU free, forcing SIGKILL constraint"
                    );
                    let _ = active_engine.process.kill().await;
                }
            }
        }
        Ok(())
    }
}

/// Compute directory size recursively (used for Completed total_bytes).
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

/// Map a VLM's HuggingFace `model_type` to the SGLang chat-template name.
///
/// SGLang ships a registry of templates in `python/sglang/srt/conversation.py`.
/// Returns None for non-VLMs (no flag is passed; SGLang uses its default).
fn detect_vlm_chat_template(model_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(model_dir.join("config.json")).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    let model_type = config["model_type"].as_str()?.to_lowercase();

    let template = match model_type.as_str() {
        "qwen2_vl" | "qwen2_5_vl" | "qwen3_vl" => "qwen2-vl",
        "llava_onevision" => "llava_onevision",
        "llava_next" => "llava_next",
        "llava" => "vicuna_v1.1",
        "minicpmv" | "minicpm_v" => "minicpmv",
        "mllama" => "llama_3_vision",
        "internvl" | "internvl_chat" => "internvl-2-5",
        "phi3_v" => "phi-3-vision",
        "pixtral" => "pixtral",
        _ => return None,
    };
    Some(template.to_string())
}

/// Read the pooling strategy from the model's config.json.
/// Checks `pooling_config.pooling_type` (sentence-transformers / GTE convention),
/// then `nomic_embed_config.pooling` (nomic convention).
/// Returns None if the config is absent or the field is not present.
fn read_pooling_from_config(model_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(model_dir.join("config.json")).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;

    if let Some(pt) = config["pooling_config"]["pooling_type"].as_str() {
        return Some(pt.to_lowercase());
    }
    if let Some(np) = config["nomic_embed_config"]["pooling"].as_str() {
        return Some(np.to_lowercase());
    }
    None
}

/// Extract the last meaningful line from a Python traceback for a clean user-facing error.
fn extract_python_error(stderr: &str) -> String {
    // Python tracebacks end with the actual exception on the last non-empty line.
    let last_error = stderr.lines().rev().find(|l| !l.trim().is_empty());

    match last_error {
        Some(line) => line.trim().to_string(),
        None => "huggingface_hub pull failed with no output".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_python_error_gets_last_line() {
        let stderr = "Traceback (most recent call last):\n  File ...\nRepositoryNotFoundError: 'bad/repo' not found\n";
        assert_eq!(
            extract_python_error(stderr),
            "RepositoryNotFoundError: 'bad/repo' not found"
        );
    }

    #[test]
    fn test_extract_python_error_empty() {
        assert_eq!(
            extract_python_error("   \n  \n"),
            "huggingface_hub pull failed with no output"
        );
    }

    #[test]
    fn test_dir_size_empty_dir() {
        let dir = std::env::temp_dir().join("lmforge_sglang_test_empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(dir_size(&dir), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_dir_size_counts_files() {
        let dir = std::env::temp_dir().join("lmforge_sglang_test_size");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 1024]).unwrap();
        std::fs::write(dir.join("b.bin"), vec![0u8; 512]).unwrap();
        assert_eq!(dir_size(&dir), 1536);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_resolve_python_prefers_venv_when_present() {
        let data_dir = std::env::temp_dir().join("lmforge_sglang_test_venv_present");
        let venv_bin = data_dir
            .join("engines")
            .join("sglang")
            .join("venv")
            .join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        let venv_python = venv_bin.join("python3");
        std::fs::write(&venv_python, b"#!/bin/sh\nexit 0\n").unwrap();

        let adapter = SglangAdapter::default();
        let resolved = adapter.resolve_python(&data_dir);
        assert_eq!(resolved, venv_python);
        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn test_resolve_python_falls_back_when_venv_missing() {
        let data_dir = std::env::temp_dir().join("lmforge_sglang_test_venv_missing");
        // No venv created — fallback should be the default `python3`.
        let adapter = SglangAdapter::default();
        let resolved = adapter.resolve_python(&data_dir);
        assert_eq!(resolved, PathBuf::from("python3"));
    }

    #[test]
    fn test_detect_vlm_chat_template_qwen2_5_vl() {
        let dir = std::env::temp_dir().join("lmforge_sglang_vlm_qwen25");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"model_type":"qwen2_5_vl","architectures":["Qwen2_5_VLForConditionalGeneration"]}"#,
        )
        .unwrap();

        assert_eq!(detect_vlm_chat_template(&dir), Some("qwen2-vl".to_string()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_detect_vlm_chat_template_minicpmv() {
        let dir = std::env::temp_dir().join("lmforge_sglang_vlm_minicpmv");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"model_type":"minicpmv"}"#).unwrap();
        assert_eq!(detect_vlm_chat_template(&dir), Some("minicpmv".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_detect_vlm_chat_template_none_for_chat_only() {
        let dir = std::env::temp_dir().join("lmforge_sglang_chat_only");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"model_type":"qwen3"}"#).unwrap();
        assert!(
            detect_vlm_chat_template(&dir).is_none(),
            "Plain chat models must not trigger --chat-template"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_detect_vlm_chat_template_none_when_config_missing() {
        let dir = std::env::temp_dir().join("lmforge_sglang_no_config");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(detect_vlm_chat_template(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
