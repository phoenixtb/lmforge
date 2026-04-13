use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn, debug};

use crate::engine::adapter::{ActiveEngine, EngineAdapter};
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
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
    ) -> Result<ActiveEngine> {
        let model_dir = data_dir.join("models").join(model_id);

        // llama-server requires a single .gguf file path, not a directory.
        // Find the largest .gguf file in the model directory.
        let gguf_path = find_gguf_file(&model_dir)
            .ok_or_else(|| anyhow::anyhow!(
                "No .gguf file found in model directory: {}. \
                 Pull the model first with: lmforge pull {}",
                model_dir.display(), model_id
            ))?;

        info!(model_id = %model_id, port = port, gguf = %gguf_path.display(), "Spawning llama-server bound explicitly to mmap GGUF tensors");

        let stdout_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stdout.log"))?;
        let stderr_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stderr.log"))?;

        let child = Command::new(&self.executable)
            .args([
                "--port", &port.to_string(),
                "--model", &gguf_path.to_string_lossy(),
                "-ngl", "99", // Auto offload max layers to Metal/CUDA
            ])
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
