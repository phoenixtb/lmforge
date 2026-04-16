use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn, debug};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, ModelRole};
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
    async fn pull_model(&self, _repo: &str, _dest_dir: &Path, _progress_tx: Sender<DownloadProgress>) -> Result<bool> {
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
        let gguf_path = find_gguf_file(model_dir)
            .ok_or_else(|| anyhow::anyhow!(
                "No .gguf file found in model directory: {}. \
                 Pull the model first with: lmforge pull {}",
                model_dir.display(), model_id
            ))?;

        info!(
            model_id = %model_id,
            port = port,
            gguf = %gguf_path.display(),
            role = ?role,
            "Spawning llama-server"
        );

        let stdout_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stdout.log"))?;
        let stderr_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stderr.log"))?;

        let port_str = port.to_string();
        let gguf_str = gguf_path.to_string_lossy().to_string();

        let mut args: Vec<String> = vec![
            "--port".to_string(), port_str,
            "--model".to_string(), gguf_str,
            "-ngl".to_string(), "99".to_string(), // Auto offload max layers to Metal/CUDA
        ];

        match role {
            ModelRole::Chat => {
                // Default chat behaviour — no extra flags needed.
            }
            ModelRole::Embed => {
                // Enable embedding output mode.
                // Omit --pooling to let llama.cpp use the model's default from GGUF metadata.
                // This avoids second-guessing what the model was trained with.
                args.push("--embeddings".to_string());
            }
            ModelRole::Rerank => {
                // Enable cross-encoder / generative re-ranker mode.
                // Both classic cross-encoders (BGE, Jina) and generative re-rankers
                // (Qwen3-Reranker) work with this flag from llama.cpp b4355+.
                args.push("--reranking".to_string());
            }
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
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = active_engine.process.kill().await;
            }

            match tokio::time::timeout(std::time::Duration::from_secs(5), active_engine.process.wait()).await {
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
fn find_gguf_file(model_dir: &Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(model_dir).ok()?;
    let mut gguf_files: Vec<(u64, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("gguf") {
                let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                Some((size, path))
            } else {
                None
            }
        })
        .collect();

    // Largest file first — single-file models win over small split shards
    gguf_files.sort_by(|a, b| b.0.cmp(&a.0));
    gguf_files.into_iter().next().map(|(_, path)| path)
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
}
