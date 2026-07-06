use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::engine::manager::{EngineState, ModelHandle};

/// How a given engine's models are resident in memory.
///
/// - `SharedServer` — one long-lived server process handles all models for
///   the engine; the `model` field in each request selects which weights to
///   use. oMLX is the canonical example: one `omlx serve --model-dir <dir>`
///   on a fixed port, native in-process LRU/TTL/eviction.
///
/// - `ProcessPool` — one OS process per loaded model, each on its own port.
///   LMForge manages admission (VRAM budgeting), eviction (LRU TTL sweep),
///   and health. llama.cpp and SGLang use this mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyKind {
    /// Single shared server; routing by `model` field; native LRU managed by the engine.
    SharedServer,
    /// Per-model OS process, LMForge-managed LRU and admission control.
    ProcessPool,
}

/// The contract every residency strategy must satisfy.
///
/// The manager calls these from its `supervise` loop; each strategy is free to
/// implement them however suits its engine's lifecycle model. Callers outside
/// the manager interact only through the `ManagerCommand` channel, which the
/// manager translates into these calls.
///
/// Using async fn in trait (stable since Rust 1.75 / edition 2021+).
#[allow(async_fn_in_trait)]
pub trait Residency: Send {
    /// Returns the residency kind (static property of the implementation).
    fn kind(&self) -> ResidencyKind;

    /// Ensure a model is loaded and return a `ModelHandle` pointing to the
    /// port where inference can be proxied. If `for_request` is `true` the
    /// inflight counter is incremented before returning so the slot is
    /// protected from eviction until the caller's `InflightGuard` drops.
    async fn ensure_model(
        &mut self,
        model_id: &str,
        keep_alive_override: &Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle>;

    /// Stop and remove the named model. No-op if the model is not loaded.
    async fn unload_model(&mut self, model_id: &str);

    /// Stop and remove all loaded models.
    async fn unload_all(&mut self);

    /// Periodic maintenance: TTL sweep, crash reap, metrics update, state
    /// broadcast. Called every `health_interval_secs` by the supervise loop.
    async fn heartbeat_tick(&mut self);

    /// Access to the shared engine state (running models, errors, …).
    /// Used by `EngineManager::state()` and by the supervisor loop for
    /// broadcasting status snapshots.
    fn state(&self) -> Arc<RwLock<EngineState>>;
}
