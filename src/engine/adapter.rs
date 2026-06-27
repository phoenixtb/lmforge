use anyhow::Result;
use std::path::Path;
use tokio::sync::mpsc::Sender;

use crate::model::downloader::DownloadProgress;

/// Typed, user-actionable load failures raised by adapters' `start()` paths.
///
/// Lets `EngineManager` set `ModelLoadError.severity = user_error` by
/// downcasting the `anyhow::Error` instead of grepping the message text
/// (ADR-003). Anything an adapter raises as a plain `anyhow!`/`bail!` stays
/// unclassified and is treated as a (loud) engine failure.
#[derive(Debug, thiserror::Error)]
pub enum EngineLoadError {
    /// Model weights are not present on disk (e.g. no `.gguf`, missing model
    /// directory). The fix is `lmforge pull <model>`.
    #[error("{0}")]
    NotMaterialized(String),
    /// The engine runtime itself is not installed (missing venv / source).
    /// The fix is `lmforge engine install <engine>`.
    #[error("{0}")]
    EngineNotInstalled(String),
}

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

/// Resolved-once plan for a single model load.
///
/// `plan_load` computes it (VRAM footprint the manager budgets with, the
/// runtime params, and the speculative-decoding decision) and the manager
/// threads it into `start`, so the admission estimate and the actual spawn
/// configuration can never drift. The manager may mutate `spec`/`footprint`
/// (e.g. degrade spec-dec to off) between planning and spawning.
pub struct LoadPlan {
    pub footprint: crate::hardware::vram::VramFootprint,
    pub spec: crate::engine::speculative::SpecResolved,
    pub runtime: crate::engine::adapters::llamacpp::RuntimePlan,
}

#[allow(async_fn_in_trait)]
pub trait EngineAdapter: Send + Sync {
    /// Estimate the VRAM footprint and resolve the runtime + spec-dec plan for
    /// a load, without spawning anything. The manager calls this BEFORE
    /// eviction/admission so it can budget for the true footprint (weights + KV
    /// + spec overhead) and evict idle models / deny / degrade accordingly.
    ///
    /// Default: a size-only footprint with spec-dec off, for engines that don't
    /// do VRAM-aware planning. The llama.cpp adapter overrides this with an
    /// analytic GGUF-metadata estimate.
    fn plan_load(
        &self,
        _model_id: &str,
        _model_dir: &Path,
        _data_dir: &Path,
        _role: ModelRole,
        size_bytes: u64,
        _free_vram_gb: f32,
    ) -> LoadPlan {
        LoadPlan {
            footprint: crate::hardware::vram::VramFootprint::from_size_bytes(size_bytes),
            spec: crate::engine::speculative::SpecResolved::off(
                "engine does not use speculative decoding",
            ),
            runtime: crate::engine::adapters::llamacpp::RuntimePlan::default(),
        }
    }

    /// Attempt a native pull for this engine.
    ///
    /// Returns:
    ///   `Ok(true)`  — engine handled the download (success); caller should update ModelIndex.
    ///   `Ok(false)` — engine deferred; caller must fall back to LMForge Rust downloader.
    ///   `Err(e)`    — engine attempted but failed; caller should surface the error.
    ///
    /// `data_dir` is passed explicitly (rather than derived from `dest_dir`)
    /// because the weights dir can live outside the data dir (e.g. a shared
    /// virtio-fs volume). Adapters that spawn a managed venv (SGLang, vLLM)
    /// need the real data dir to resolve their interpreter.
    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        data_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool>;
    #[allow(clippy::too_many_arguments)]
    async fn start(
        &self,
        model_id: &str,
        model_dir: &Path,
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
        role: ModelRole,
        plan: &LoadPlan,
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
    fn plan_load(
        &self,
        model_id: &str,
        model_dir: &Path,
        data_dir: &Path,
        role: ModelRole,
        size_bytes: u64,
        free_vram_gb: f32,
    ) -> LoadPlan {
        match self {
            Self::Omlx(ad) => {
                ad.plan_load(model_id, model_dir, data_dir, role, size_bytes, free_vram_gb)
            }
            Self::Sglang(ad) => {
                ad.plan_load(model_id, model_dir, data_dir, role, size_bytes, free_vram_gb)
            }
            Self::Llamacpp(ad) => {
                ad.plan_load(model_id, model_dir, data_dir, role, size_bytes, free_vram_gb)
            }
            Self::Vllm(ad) => {
                ad.plan_load(model_id, model_dir, data_dir, role, size_bytes, free_vram_gb)
            }
            Self::TabbyApi(ad) => {
                ad.plan_load(model_id, model_dir, data_dir, role, size_bytes, free_vram_gb)
            }
        }
    }

    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        data_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool> {
        match self {
            Self::Omlx(ad) => ad.pull_model(repo, dest_dir, data_dir, progress_tx).await,
            Self::Sglang(ad) => ad.pull_model(repo, dest_dir, data_dir, progress_tx).await,
            Self::Llamacpp(ad) => ad.pull_model(repo, dest_dir, data_dir, progress_tx).await,
            Self::Vllm(ad) => ad.pull_model(repo, dest_dir, data_dir, progress_tx).await,
            Self::TabbyApi(ad) => ad.pull_model(repo, dest_dir, data_dir, progress_tx).await,
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
        plan: &LoadPlan,
    ) -> Result<ActiveEngine> {
        match self {
            Self::Omlx(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role, plan)
                    .await
            }
            Self::Sglang(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role, plan)
                    .await
            }
            Self::Llamacpp(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role, plan)
                    .await
            }
            Self::Vllm(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role, plan)
                    .await
            }
            Self::TabbyApi(ad) => {
                ad.start(model_id, model_dir, port, data_dir, logs_dir, role, plan)
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
