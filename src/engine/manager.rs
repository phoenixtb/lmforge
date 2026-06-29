use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::engine::adapter::EngineAdapterInstance;
use crate::engine::process_pool::ProcessPoolResidency;
use crate::engine::registry::EngineConfig;
use crate::engine::residency::{Residency, ResidencyKind};
use crate::engine::shared_server::SharedServerResidency;

// ── Public types (shared data model, wire format) ────────────────────────────

/// Engine runtime status
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineStatus {
    Stopped,
    Starting,
    Ready,
    Degraded,
    Error,
}

impl std::fmt::Display for EngineStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "stopped"),
            Self::Starting => write!(f, "starting"),
            Self::Ready => write!(f, "ready"),
            Self::Degraded => write!(f, "degraded"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Runtime metrics for the engine
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EngineMetrics {
    pub requests_total: u64,
    pub ttft_avg_ms: f64,
    pub uptime_secs: u64,
    pub restart_count: u32,
}

/// Runtime state for a single loaded model within the multiplexed pool
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelSlot {
    pub model_id: String,
    pub port: u16,
    pub status: EngineStatus,
    pub idle_secs: u64,
    pub vram_est_gb: f32,
    /// Speculative-decoding mode this slot was started with — one of
    /// `auto` / `mtp` / `draft-model` / `off`. Resolved by
    /// `engine::speculative::resolve` at spawn time. Surfaced so the UI
    /// can show "spec=mtp" badges per slot without re-deriving the
    /// decision.
    #[serde(default)]
    pub spec_mode: crate::engine::speculative::SpecMode,
    /// Cumulative speculative-decoding stats parsed from `llama-server`
    /// stderr by the per-slot tee task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_stats: Option<crate::engine::spec_observer::SpecStats>,
}

/// One recorded failure for a model load attempt, surfaced in `/lf/status`
/// under `last_errors` so the UI / CLI can show *why* a `lmforge run`
/// command failed without forcing the user to grep through log files.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelLoadError {
    pub at: String,
    pub stderr_tail: Option<String>,
    pub message: String,
    pub severity: LoadErrorSeverity,
    pub count: u32,
}

/// Stable grouping key for a failure: severity + the message with digit runs
/// collapsed to `#`.
pub(crate) fn error_signature(severity: LoadErrorSeverity, message: &str) -> String {
    let mut norm = String::with_capacity(message.len());
    let mut in_digits = false;
    for c in message.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                norm.push('#');
            }
            in_digits = true;
        } else {
            norm.push(c);
            in_digits = false;
        }
    }
    format!("{severity:?}|{norm}")
}

pub(crate) const MAX_LAST_ERRORS: usize = 8;

const DEFAULT_LAST_ERROR_TTL_SECS: i64 = 600;

pub(crate) fn last_error_ttl_secs() -> i64 {
    std::env::var("LMFORGE_LAST_ERROR_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_LAST_ERROR_TTL_SECS)
}

/// Coarse failure classification for a `ModelLoadError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadErrorSeverity {
    UserError,
    Transient,
    EngineBug,
}

impl LoadErrorSeverity {
    pub(crate) fn for_spawn_failure(err: &anyhow::Error) -> Self {
        if err
            .downcast_ref::<crate::engine::adapter::EngineLoadError>()
            .is_some()
        {
            Self::UserError
        } else {
            Self::EngineBug
        }
    }
}

/// Shared engine state accessible from API handlers
#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineState {
    pub overall_status: EngineStatus,
    pub engine_id: String,
    pub engine_version: String,
    pub running_models: std::collections::HashMap<String, ModelSlot>,
    pub metrics: EngineMetrics,
    #[serde(default)]
    pub last_errors: std::collections::HashMap<String, ModelLoadError>,
    #[serde(skip)]
    pub dismissed_errors: std::collections::HashMap<String, String>,
}

impl EngineState {
    pub fn record_error(&mut self, model_id: &str, mut entry: ModelLoadError) -> bool {
        let sig = error_signature(entry.severity, &entry.message);

        if self.dismissed_errors.get(model_id).map(String::as_str) == Some(sig.as_str()) {
            return false;
        }
        self.dismissed_errors.remove(model_id);

        if let Some(existing) = self.last_errors.get(model_id)
            && error_signature(existing.severity, &existing.message) == sig
        {
            entry.count = existing.count.saturating_add(1);
        }

        self.last_errors.insert(model_id.to_string(), entry);
        if self.last_errors.len() > MAX_LAST_ERRORS
            && let Some(oldest_key) = self
                .last_errors
                .iter()
                .min_by(|a, b| a.1.at.cmp(&b.1.at))
                .map(|(k, _)| k.clone())
        {
            self.last_errors.remove(&oldest_key);
        }
        true
    }

    pub fn clear_error(&mut self, model_id: &str) {
        self.last_errors.remove(model_id);
        self.dismissed_errors.remove(model_id);
    }

    pub fn dismiss_error(&mut self, model_id: &str) {
        if let Some(e) = self.last_errors.remove(model_id) {
            self.dismissed_errors.insert(
                model_id.to_string(),
                error_signature(e.severity, &e.message),
            );
        } else {
            self.dismissed_errors.remove(model_id);
        }
    }
}

/// Returned by `ensure_model`; the caller proxies the request to `port` and
/// holds `inflight` to protect the slot from eviction until the request
/// completes.
pub struct ModelHandle {
    pub port: u16,
    pub inflight: Arc<std::sync::atomic::AtomicU32>,
}

pub enum ManagerCommand {
    EnsureModel {
        model_id: String,
        keep_alive_override: Option<String>,
        /// `true` when the caller is about to serve a request through this model
        /// (inference endpoints): the orchestrator bumps the slot's in-flight
        /// count before replying so the model is protected from eviction in the
        /// window before the request path installs its guard. `false` for warm
        /// preloads (e.g. `/lf/model/switch`) which must not leak a count.
        for_request: bool,
        reply: tokio::sync::oneshot::Sender<Result<ModelHandle>>,
    },
    UnloadModel(String),
    UnloadAll,
}

// ── Residency instance (static enum dispatch) ────────────────────────────────

/// Static dispatch over the two residency strategies.
///
/// Using an enum avoids boxing + vtable overhead and follows the same pattern
/// as `EngineAdapterInstance`. Phase 2 adds `SharedServer`; Phase 3 makes it
/// the default for oMLX.
enum ResidencyInstance {
    ProcessPool(ProcessPoolResidency),
    SharedServer(SharedServerResidency),
}

impl ResidencyInstance {
    fn kind(&self) -> ResidencyKind {
        match self {
            Self::ProcessPool(r) => r.kind(),
            Self::SharedServer(r) => r.kind(),
        }
    }

    async fn ensure_model(
        &mut self,
        model_id: &str,
        keep_alive_override: &Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle> {
        match self {
            Self::ProcessPool(r) => r.ensure_model(model_id, keep_alive_override, for_request).await,
            Self::SharedServer(r) => r.ensure_model(model_id, keep_alive_override, for_request).await,
        }
    }

    async fn unload_model(&mut self, model_id: &str) {
        match self {
            Self::ProcessPool(r) => r.unload_model(model_id).await,
            Self::SharedServer(r) => r.unload_model(model_id).await,
        }
    }

    async fn unload_all(&mut self) {
        match self {
            Self::ProcessPool(r) => r.unload_all().await,
            Self::SharedServer(r) => r.unload_all().await,
        }
    }

    async fn heartbeat_tick(&mut self) {
        match self {
            Self::ProcessPool(r) => r.heartbeat_tick().await,
            Self::SharedServer(r) => r.heartbeat_tick().await,
        }
    }

    fn state(&self) -> Arc<RwLock<EngineState>> {
        match self {
            Self::ProcessPool(r) => r.state(),
            Self::SharedServer(r) => r.state(),
        }
    }

    /// Direct write access to the inner EngineState for legacy compat methods.
    async fn set_overall_status(&self, status: EngineStatus) {
        match self {
            Self::ProcessPool(r) => r.state.write().await.overall_status = status,
            Self::SharedServer(r) => r.state.write().await.overall_status = status,
        }
    }
}

// ── Thin EngineManager dispatcher ────────────────────────────────────────────

/// The engine manager — a thin dispatcher over a `ResidencyInstance`.
///
/// oMLX uses `SharedServerResidency` by default (one shared server, native LRU).
/// All other engines use `ProcessPoolResidency` (per-model process, LMForge LRU).
/// Set `LMFORGE_OMLX_SHARED=0` to revert oMLX to `ProcessPoolResidency` for debugging.
pub struct EngineManager {
    pub config: EngineConfig,
    residency: ResidencyInstance,
    health_interval_secs: u64,
}

impl EngineManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: EngineConfig,
        adapter: EngineAdapterInstance,
        base_engine_port: u16,
        data_dir: PathBuf,
        models_dir: PathBuf,
        global_keep_alive: String,
        max_loaded_models: u32,
        status_tx: tokio::sync::broadcast::Sender<EngineState>,
    ) -> Self {
        // Phase 3: SharedServerResidency is the default for oMLX.
        // Set LMFORGE_OMLX_SHARED=0 to revert to ProcessPoolResidency for debugging.
        let use_shared = config.id == "omlx"
            && std::env::var("LMFORGE_OMLX_SHARED").as_deref() != Ok("0");

        let residency = if use_shared {
            // Resolve the oMLX executable from the adapter (it's an OmlxAdapter).
            let executable = match &adapter {
                EngineAdapterInstance::Omlx(a) => a.executable.clone(),
                _ => "omlx".to_string(),
            };
            tracing::info!(
                port = base_engine_port,
                models_dir = %models_dir.display(),
                "oMLX SharedServerResidency active (set LMFORGE_OMLX_SHARED=0 to disable)"
            );
            ResidencyInstance::SharedServer(SharedServerResidency::new(
                config.clone(),
                models_dir,
                data_dir,
                base_engine_port,
                executable,
                &global_keep_alive,
                status_tx,
            ))
        } else {
            ResidencyInstance::ProcessPool(ProcessPoolResidency::new(
                config.clone(),
                adapter,
                base_engine_port,
                data_dir,
                models_dir,
                global_keep_alive,
                max_loaded_models,
                status_tx,
            ))
        };

        Self {
            config,
            residency,
            health_interval_secs: 2,
        }
    }

    pub fn state(&self) -> Arc<RwLock<EngineState>> {
        self.residency.state()
    }

    /// Returns the active residency kind (for diagnostics / doctor).
    pub fn residency_kind(&self) -> ResidencyKind {
        self.residency.kind()
    }

    /// Legacy compat: sets overall status to Ready (models are loaded dynamically).
    pub async fn start(&mut self) -> Result<()> {
        self.residency.set_overall_status(EngineStatus::Ready).await;
        Ok(())
    }

    pub fn set_model(&mut self, _name: String) {
        // Deprecated — models are populated via EnsureModel.
    }

    pub async fn wait_for_ready(&self, _timeout_secs: u64) -> Result<()> {
        // Models are loaded dynamically; daemon is instantly ready.
        Ok(())
    }

    pub async fn supervise(mut self, mut cmd_rx: tokio::sync::mpsc::Receiver<ManagerCommand>) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(ManagerCommand::EnsureModel { model_id, keep_alive_override, for_request, reply }) => {
                            let res = self.residency.ensure_model(&model_id, &keep_alive_override, for_request).await;
                            let _ = reply.send(res);
                        }
                        Some(ManagerCommand::UnloadModel(model_id)) => {
                            self.residency.unload_model(&model_id).await;
                        }
                        Some(ManagerCommand::UnloadAll) => {
                            self.residency.unload_all().await;
                        }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(self.health_interval_secs)) => {
                    self.residency.heartbeat_tick().await;
                }
            }
        }
    }
}

// ── Tests (use super::* — sees all public + pub(crate) symbols above) ─────────

// Re-exported for tests in this module that directly call lru_idle_model_id.
#[allow(unused_imports)]
pub(crate) use crate::engine::process_pool::lru_idle_model_id;

#[cfg(test)]
mod last_error_tests {
    use super::*;

    fn blank_state() -> EngineState {
        EngineState {
            overall_status: EngineStatus::Ready,
            engine_id: "test".into(),
            engine_version: "0".into(),
            running_models: std::collections::HashMap::new(),
            metrics: EngineMetrics::default(),
            last_errors: std::collections::HashMap::new(),
            dismissed_errors: std::collections::HashMap::new(),
        }
    }

    fn err(at: &str, sev: LoadErrorSeverity) -> ModelLoadError {
        msg_err(at, sev, "boom")
    }

    fn msg_err(at: &str, sev: LoadErrorSeverity, message: &str) -> ModelLoadError {
        ModelLoadError {
            at: at.into(),
            stderr_tail: None,
            message: message.into(),
            severity: sev,
            count: 1,
        }
    }

    #[test]
    fn record_error_stores_when_not_dismissed() {
        let mut s = blank_state();
        assert!(s.record_error("m", err("t0", LoadErrorSeverity::Transient)));
        assert!(s.last_errors.contains_key("m"));
    }

    #[test]
    fn dismiss_removes_current_error_and_suppresses_same_signature() {
        let mut s = blank_state();
        s.record_error("m", err("t0", LoadErrorSeverity::Transient));

        s.dismiss_error("m");
        assert!(!s.last_errors.contains_key("m"), "current error cleared");
        assert!(
            s.dismissed_errors.contains_key("m"),
            "model marked dismissed"
        );

        let stored = s.record_error("m", err("t1", LoadErrorSeverity::Transient));
        assert!(!stored, "same-signature failure should be suppressed");
        assert!(!s.last_errors.contains_key("m"), "no new error surfaced");
    }

    #[test]
    fn volatile_numbers_do_not_break_suppression() {
        let mut s = blank_state();
        s.record_error(
            "m",
            msg_err(
                "t0",
                LoadErrorSeverity::Transient,
                "exited port=11432 in 4563ms",
            ),
        );
        s.dismiss_error("m");
        let stored = s.record_error(
            "m",
            msg_err(
                "t1",
                LoadErrorSeverity::Transient,
                "exited port=11888 in 9012ms",
            ),
        );
        assert!(!stored, "digit-only differences must not defeat dismissal");
    }

    #[test]
    fn different_signature_resurfaces_without_a_successful_load() {
        let mut s = blank_state();
        s.record_error(
            "m",
            msg_err("t0", LoadErrorSeverity::UserError, "no .gguf found"),
        );
        s.dismiss_error("m");

        assert!(s.record_error(
            "m",
            msg_err("t1", LoadErrorSeverity::Transient, "out of VRAM")
        ));
        assert!(s.last_errors.contains_key("m"));
        assert!(
            !s.dismissed_errors.contains_key("m"),
            "stale dismissal lifted"
        );
    }

    #[test]
    fn successful_load_lifts_dismissal_so_real_failures_resurface() {
        let mut s = blank_state();
        s.record_error("m", err("t0", LoadErrorSeverity::Transient));
        s.dismiss_error("m");

        s.clear_error("m");
        assert!(
            !s.dismissed_errors.contains_key("m"),
            "dismissal lifted on success"
        );

        assert!(s.record_error("m", err("t2", LoadErrorSeverity::Transient)));
        assert!(s.last_errors.contains_key("m"));
    }

    #[test]
    fn repeat_of_same_signature_bumps_count() {
        let mut s = blank_state();
        s.record_error("m", err("t0", LoadErrorSeverity::Transient));
        s.record_error("m", err("t1", LoadErrorSeverity::Transient));
        s.record_error("m", err("t2", LoadErrorSeverity::Transient));
        assert_eq!(s.last_errors["m"].count, 3);
        assert_eq!(s.last_errors["m"].at, "t2", "timestamp refreshed to latest");
    }

    #[test]
    fn different_signature_resets_count() {
        let mut s = blank_state();
        s.record_error("m", msg_err("t0", LoadErrorSeverity::Transient, "kind A"));
        s.record_error("m", msg_err("t1", LoadErrorSeverity::Transient, "kind A"));
        s.record_error("m", msg_err("t2", LoadErrorSeverity::Transient, "kind B"));
        assert_eq!(
            s.last_errors["m"].count, 1,
            "new signature starts a fresh count"
        );
    }

    #[test]
    fn dismissal_is_per_model() {
        let mut s = blank_state();
        s.record_error("a", err("t0", LoadErrorSeverity::Transient));
        s.record_error("b", err("t0", LoadErrorSeverity::UserError));

        s.dismiss_error("a");
        assert!(!s.record_error("a", err("t1", LoadErrorSeverity::Transient)));
        assert!(s.record_error("b", err("t1", LoadErrorSeverity::UserError)));
        assert!(s.last_errors.contains_key("b"));
        assert!(!s.last_errors.contains_key("a"));
    }

    #[test]
    fn lru_idle_picks_oldest_idle() {
        let slots = [
            ("chat".to_string(), 300u64, 0u32),
            ("embed".to_string(), 100u64, 0u32),
            ("vlm".to_string(), 200u64, 0u32),
        ];
        let id = lru_idle_model_id(slots.iter().map(|(k, ts, n)| (k, *ts, *n)));
        assert_eq!(
            id.as_deref(),
            Some("embed"),
            "smallest idle last_accessed wins"
        );
    }

    #[test]
    fn lru_idle_skips_active_models() {
        let slots = [
            ("chat".to_string(), 300u64, 0u32),
            ("embed".to_string(), 100u64, 2u32),
            ("vlm".to_string(), 200u64, 0u32),
        ];
        let id = lru_idle_model_id(slots.iter().map(|(k, ts, n)| (k, *ts, *n)));
        assert_eq!(
            id.as_deref(),
            Some("vlm"),
            "active LRU skipped for oldest idle"
        );
    }

    #[test]
    fn lru_idle_none_when_all_active() {
        let slots = [
            ("chat".to_string(), 300u64, 1u32),
            ("embed".to_string(), 100u64, 3u32),
        ];
        assert_eq!(
            lru_idle_model_id(slots.iter().map(|(k, ts, n)| (k, *ts, *n))),
            None
        );
    }

    #[test]
    fn lru_idle_empty_is_none() {
        let slots: Vec<(String, u64, u32)> = Vec::new();
        assert_eq!(
            lru_idle_model_id(slots.iter().map(|(k, ts, n)| (k, *ts, *n))),
            None
        );
    }

    #[test]
    fn lru_idle_single_idle() {
        let slots = [("only".to_string(), 42u64, 0u32)];
        assert_eq!(
            lru_idle_model_id(slots.iter().map(|(k, ts, n)| (k, *ts, *n))).as_deref(),
            Some("only")
        );
    }

    #[test]
    fn record_error_caps_at_max() {
        let mut s = blank_state();
        for i in 0..(MAX_LAST_ERRORS + 4) {
            s.record_error(
                &format!("m{i:02}"),
                err(
                    &format!("2026-01-01T00:00:{i:02}Z"),
                    LoadErrorSeverity::Transient,
                ),
            );
        }
        assert_eq!(s.last_errors.len(), MAX_LAST_ERRORS);
        assert!(!s.last_errors.contains_key("m00"));
        assert!(
            s.last_errors
                .contains_key(&format!("m{:02}", MAX_LAST_ERRORS + 3))
        );
    }
}
