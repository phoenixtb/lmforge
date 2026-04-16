use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn, debug};

use crate::engine::adapter::{ActiveEngine, EngineAdapter};
use crate::model::downloader::DownloadProgress;

#[derive(Clone)]
pub struct OmlxAdapter {
    pub executable: String,
}

impl Default for OmlxAdapter {
    fn default() -> Self {
        Self {
            executable: "omlx".to_string(),
        }
    }
}

impl EngineAdapter for OmlxAdapter {
    async fn pull_model(&self, _repo: &str, _dest_dir: &Path, _progress_tx: Sender<DownloadProgress>) -> Result<bool> {
        // oMLX's downloader is internal/undocumented with no stable external streaming API.
        // Defer to LMForge's Rust downloader for full SSE progress.
        Ok(false)
    }

    async fn start(
        &self,
        model_id: &str,
        model_subdir: &Path,
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
    ) -> Result<ActiveEngine> {
        // oMLX's --model-dir expects the PARENT directory whose subdirectories are models,
        // e.g. ~/.lmforge/models/ (not ~/.lmforge/models/qwen3.5-27b-...).
        // oMLX discovers all valid models within that parent and serves them by subdirectory name.
        // The specific model is selected per-request via the "model" field.
        let models_parent_dir = model_subdir.parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid model directory structure"))?;

        // Verify the specific model subdirectory exists before starting.
        if !model_subdir.exists() {
            anyhow::bail!(
                "Model directory not found: {}. Pull the model first with: lmforge pull {}",
                model_subdir.display(), model_id
            );
        }

        info!(model_id = %model_id, port = port, models_dir = %models_parent_dir.display(),
              "Spawning native oMLX engine with models parent directory");

        let stdout_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stdout.log"))?;
        let stderr_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stderr.log"))?;

        let child = Command::new(&self.executable)
            .args([
                "serve",
                "--port",
                &port.to_string(),
                "--model-dir",
                &models_parent_dir.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn native oMLX serve")?;

        Ok(ActiveEngine {
            process: child,
            model_id: model_id.to_string(),
        })
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        if let Some(pid) = active_engine.process.id() {
            info!(pid, model = %active_engine.model_id, "Sending SIGTERM to flush oMLX Unified Memory");
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
            
            // Wait for process to fully exit, definitively guaranteeing zero VRAM fragmentation
            match tokio::time::timeout(std::time::Duration::from_secs(5), active_engine.process.wait()).await {
                Ok(_) => debug!("oMLX natively flush-exited"),
                Err(_) => {
                    warn!("oMLX SIGTERM timed out, forcing SIGKILL");
                    let _ = active_engine.process.kill().await;
                }
            }
        }
        Ok(())
    }
}
