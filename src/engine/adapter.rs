use anyhow::Result;
use std::path::Path;
use tokio::sync::mpsc::Sender;

use crate::model::downloader::DownloadProgress;

/// The functional role an engine slot is serving.
///
/// The role is derived from `ModelCapabilities` at model-load time and determines
/// which engine startup flags are selected. It is stable for the lifetime of a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    /// Standard text-generation with system-prompt support.
    Chat,
    /// Pooled vector embeddings via `/v1/embeddings`.
    Embed,
    /// Cross-encoder relevance scoring via `/v1/rerank`.
    Rerank,
}

/// Represents an active subprocess managed by an adapter
pub struct ActiveEngine {
    pub process: tokio::process::Child,
    /// The unique model footprint this process is bound to
    pub model_id: String,
}

pub trait EngineAdapter: Send + Sync {
    /// Attempt a native pull for this engine.
    ///
    /// Returns:
    ///   `Ok(true)`  — engine handled the download (success); caller should update ModelIndex.
    ///   `Ok(false)` — engine deferred; caller must fall back to LMForge Rust downloader.
    ///   `Err(e)`    — engine attempted but failed; caller should surface the error.
    async fn pull_model(&self, repo: &str, dest_dir: &Path, progress_tx: Sender<DownloadProgress>) -> Result<bool>;
    async fn start(&self, model_id: &str, model_dir: &Path, port: u16, data_dir: &Path, logs_dir: &Path, role: ModelRole) -> Result<ActiveEngine>;
    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()>;
}

/// Static Dispatch Enum to avoid dyn Future object-safety restrictions
#[derive(Clone)]
pub enum EngineAdapterInstance {
    Omlx(crate::engine::adapters::omlx::OmlxAdapter),
    Sglang(crate::engine::adapters::sglang::SglangAdapter),
    Llamacpp(crate::engine::adapters::llamacpp::LlamacppAdapter),
}

impl EngineAdapter for EngineAdapterInstance {
    async fn pull_model(&self, repo: &str, dest_dir: &Path, progress_tx: Sender<DownloadProgress>) -> Result<bool> {
        match self {
            Self::Omlx(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
            Self::Sglang(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
            Self::Llamacpp(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
        }
    }

    async fn start(&self, model_id: &str, model_dir: &Path, port: u16, data_dir: &Path, logs_dir: &Path, role: ModelRole) -> Result<ActiveEngine> {
        match self {
            Self::Omlx(ad) => ad.start(model_id, model_dir, port, data_dir, logs_dir, role).await,
            Self::Sglang(ad) => ad.start(model_id, model_dir, port, data_dir, logs_dir, role).await,
            Self::Llamacpp(ad) => ad.start(model_id, model_dir, port, data_dir, logs_dir, role).await,
        }
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        match self {
            Self::Omlx(ad) => ad.stop(active_engine).await,
            Self::Sglang(ad) => ad.stop(active_engine).await,
            Self::Llamacpp(ad) => ad.stop(active_engine).await,
        }
    }
}
