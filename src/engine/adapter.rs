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
    /// Live speculative-decoding telemetry, fed by the stderr tee task
    /// the `llamacpp` adapter sets up. `None` for engines that don't
    /// emit acceptance-rate stats (vLLM, SGLang, oMLX, TabbyAPI today).
    /// Cloning the inner `Arc` is cheap — the manager snapshots from a
    /// clone on every `/lf/status` notify without blocking the spawn.
    pub spec_observer: Option<crate::engine::spec_observer::SpecObserver>,
    /// Which speculative-decoding mode was actually used to spawn this
    /// engine. Surfaced in `/lf/status` so the UI can show "spec=mtp"
    /// vs "spec=off" without re-resolving the config. Also drives the
    /// crash-fallback retry policy in `EngineManager` (S-2.8): if the
    /// engine dies <5s after spawn AND this is anything but
    /// `SpecMode::Off`, the manager retries once with spec disabled.
    pub spec_mode: crate::engine::speculative::SpecMode,
}

#[allow(async_fn_in_trait)]
pub trait EngineAdapter: Send + Sync {
    /// Attempt a native pull for this engine.
    ///
    /// Returns:
    ///   `Ok(true)`  — engine handled the download (success); caller should update ModelIndex.
    ///   `Ok(false)` — engine deferred; caller must fall back to LMForge Rust downloader.
    ///   `Err(e)`    — engine attempted but failed; caller should surface the error.
    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool>;
    async fn start(
        &self,
        model_id: &str,
        model_dir: &Path,
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
        role: ModelRole,
    ) -> Result<ActiveEngine>;
    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()>;
}

/// Static Dispatch Enum to avoid dyn Future object-safety restrictions
#[derive(Clone)]
pub enum EngineAdapterInstance {
    Omlx(crate::engine::adapters::omlx::OmlxAdapter),
    Sglang(crate::engine::adapters::sglang::SglangAdapter),
    Llamacpp(crate::engine::adapters::llamacpp::LlamacppAdapter),
    Vllm(crate::engine::adapters::vllm::VllmAdapter),
    TabbyApi(crate::engine::adapters::tabbyapi::TabbyApiAdapter),
}

impl EngineAdapter for EngineAdapterInstance {
    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool> {
        match self {
            Self::Omlx(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
            Self::Sglang(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
            Self::Llamacpp(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
            Self::Vllm(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
            Self::TabbyApi(ad) => ad.pull_model(repo, dest_dir, progress_tx).await,
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
        match self {
            Self::Omlx(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role)
                    .await
            }
            Self::Sglang(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role)
                    .await
            }
            Self::Llamacpp(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role)
                    .await
            }
            Self::Vllm(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role)
                    .await
            }
            Self::TabbyApi(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role)
                    .await
            }
        }
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        match self {
            Self::Omlx(ad) => ad.stop(active_engine).await,
            Self::Sglang(ad) => ad.stop(active_engine).await,
            Self::Llamacpp(ad) => ad.stop(active_engine).await,
            Self::Vllm(ad) => ad.stop(active_engine).await,
            Self::TabbyApi(ad) => ad.stop(active_engine).await,
        }
    }
}
