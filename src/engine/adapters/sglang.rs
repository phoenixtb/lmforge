use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn, debug, error};

use crate::engine::adapter::{ActiveEngine, EngineAdapter};
use crate::model::downloader::DownloadProgress;

#[derive(Clone)]
pub struct SglangAdapter {
    pub executable: String,
}

impl Default for SglangAdapter {
    fn default() -> Self {
        Self {
            executable: "python3".to_string(),
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
    async fn pull_model(&self, repo: &str, dest_dir: &Path, progress_tx: Sender<DownloadProgress>) -> Result<bool> {
        std::fs::create_dir_all(dest_dir)
            .context("Failed to create model destination directory")?;

        info!(repo, dest = %dest_dir.display(), "SGLang: starting native huggingface_hub pull");

        let _ = progress_tx.send(DownloadProgress::Started {
            repo: repo.to_string(),
            files: 0, // unknown until snapshot_download resolves the manifest
        }).await;

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

        let output = Command::new(&self.executable)
            .args(["-c", &python_snippet])
            .output()
            .await
            .context("Failed to spawn python3 for huggingface_hub pull")?;

        if output.status.success() {
            let total_bytes = dir_size(dest_dir);
            info!(repo, total_bytes, "SGLang: huggingface_hub pull completed");

            let _ = progress_tx.send(DownloadProgress::Completed {
                repo: repo.to_string(),
                total_bytes,
            }).await;

            Ok(true)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);

            // Log full output for debugging
            error!(repo, stderr = %stderr, stdout = %stdout, "SGLang: huggingface_hub pull failed");

            // Surface a clean error message — strip Python traceback boilerplate if present
            let user_error = extract_python_error(&stderr);

            let _ = progress_tx.send(DownloadProgress::Failed {
                error: user_error.clone(),
            }).await;

            anyhow::bail!("huggingface_hub pull failed: {}", user_error)
        }
    }

    async fn start(
        &self,
        model_id: &str,
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
    ) -> Result<ActiveEngine> {
        let model_dir = data_dir.join("models").join(model_id);

        info!(model_id = %model_id, port = port, "Spawning native SGLang python instance bound to model Radix KV Cache");

        let stdout_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stdout.log"))?;
        let stderr_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(logs_dir.join("engine-stderr.log"))?;

        let child = Command::new(&self.executable)
            .args([
                "-m", "sglang.launch_server",
                "--port", &port.to_string(),
                "--model-path", &model_dir.to_string_lossy(),
            ])
            // Future parity params: --tp 2 --gpu-memory-utilization 0.9 --chunked-prefill-size
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn native SGLang launch_server")?;

        Ok(ActiveEngine {
            process: child,
            model_id: model_id.to_string(),
        })
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        if let Some(pid) = active_engine.process.id() {
            info!(pid, model = %active_engine.model_id, "Sending SIGTERM to violently flush RadixAttention NVIDIA VRAM block");
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

            // Wait for CUDA process to completely teardown to eliminate OOM frag errors on respawn
            match tokio::time::timeout(std::time::Duration::from_secs(5), active_engine.process.wait()).await {
                Ok(_) => debug!("SGLang natively flush-exited"),
                Err(_) => {
                    warn!("SGLang SIGTERM timed out or hung on GPU free, forcing SIGKILL constraint");
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

/// Extract the last meaningful line from a Python traceback for a clean user-facing error.
fn extract_python_error(stderr: &str) -> String {
    // Python tracebacks end with the actual exception on the last non-empty line.
    let last_error = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty());

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
        assert_eq!(extract_python_error(stderr), "RepositoryNotFoundError: 'bad/repo' not found");
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
}
