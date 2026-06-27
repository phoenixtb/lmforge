use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, EngineAdapterInstance, ModelRole};
use crate::engine::keepalive;
use crate::engine::registry::EngineConfig;

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
    /// config. Defaults to `Off` for embed/rerank slots.
    #[serde(default)]
    pub spec_mode: crate::engine::speculative::SpecMode,
    /// Cumulative speculative-decoding stats parsed from `llama-server`
    /// stderr by the per-slot tee task. `None` until the first request
    /// served by this slot completes (or always `None` for non-llamacpp
    /// engines + spec-disabled slots). See `engine::spec_observer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_stats: Option<crate::engine::spec_observer::SpecStats>,
}

/// One recorded failure for a model load attempt, surfaced in `/lf/status`
/// under `last_errors` so the UI / CLI can show *why* a `lmforge run`
/// command failed without forcing the user to grep through log files.
///
/// Populated on:
///   * `spawn_adapter_process` failure (binary missing, port conflict, etc.)
///   * `wait_slot_health` timeout (engine started but never reached `/health`)
///
/// Cleared on the next successful load of the same `model_id`. Capped to
/// the last `MAX_LAST_ERRORS` model ids globally to bound memory.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelLoadError {
    /// RFC3339 timestamp of when the failure was recorded.
    pub at: String,
    /// Last ~32 lines of the worker's stderr log, capped at 8 KiB.
    /// `None` when no stderr file existed at the time of failure (worker
    /// crashed before writing anything, or adapter never spawned).
    pub stderr_tail: Option<String>,
    /// One-line error message from the orchestrator side
    /// (e.g. "Engine Adapter failed health verify on port 11521").
    pub message: String,
    /// Coarse failure classification so consumers can decide presentation
    /// and retry policy without parsing the message. Derived at record time.
    pub severity: LoadErrorSeverity,
    /// How many consecutive times this same failure (same signature) has
    /// occurred for this model since it was last cleared. Lets the UI render a
    /// single "last seen · Nx" entry instead of stacking duplicate cards.
    pub count: u32,
}

/// Stable grouping key for a failure: severity + the message with digit runs
/// collapsed to `#`, so volatile tokens (ports, sizes, timings) don't fragment
/// otherwise-identical failures. Used to dedupe the occurrence counter and to
/// decide whether a dismissal still applies (same signature) or a genuinely
/// new failure should resurface (different signature).
fn error_signature(severity: LoadErrorSeverity, message: &str) -> String {
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

/// Coarse failure classification for a `ModelLoadError`. Drives UI treatment
/// (a `transient` failure may auto-collapse; a `user_error` stays actionable
/// until the user resolves or dismisses it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadErrorSeverity {
    /// Actionable config / input problem the user must fix (missing weights,
    /// model not pulled, bad model id). Should persist in the UI.
    UserError,
    /// Likely-recoverable runtime hiccup (port race, health timeout, OOM).
    /// Safe to auto-demote in the UI after a short window.
    Transient,
    /// Anything else — treated as a hard error worth surfacing.
    EngineBug,
}

impl LoadErrorSeverity {
    /// Severity for a failure during the **spawn** phase (the engine never
    /// reached a health check). Adapters raise `EngineLoadError` for the
    /// user-actionable cases (weights not pulled, engine not installed); we
    /// classify those as `UserError` by error type rather than message text.
    /// Anything else that prevents a spawn is a hard `EngineBug`.
    fn for_spawn_failure(err: &anyhow::Error) -> Self {
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

/// Pick the least-recently-used model id among *idle* slots (`inflight == 0`).
///
/// Slots that are actively serving a request (`inflight > 0`) are filtered out
/// entirely — they are never eviction victims, because stopping the engine would
/// abort the in-flight request. Returns `None` when every candidate is busy (the
/// caller then enforces admission control: reject rather than over-commit). Pure
/// so the policy is unit-testable without spawning engines. Items are
/// `(id, last_accessed, inflight)`.
fn lru_idle_model_id<'a>(
    slots: impl IntoIterator<Item = (&'a String, u64, u32)>,
) -> Option<String> {
    slots
        .into_iter()
        .filter(|(_, _, inflight)| *inflight == 0)
        .min_by_key(|(_, last_accessed, _)| *last_accessed)
        .map(|(id, _, _)| id.clone())
}

const MAX_LAST_ERRORS: usize = 8;

/// How long a `last_errors` entry is retained before the heartbeat sweep
/// evicts it. Bounds "stale failure" noise so a one-off cold-load error does
/// not linger for the whole daemon lifetime. Override via
/// `LMFORGE_LAST_ERROR_TTL_SECS` (0 disables the sweep).
const DEFAULT_LAST_ERROR_TTL_SECS: i64 = 600;

fn last_error_ttl_secs() -> i64 {
    std::env::var("LMFORGE_LAST_ERROR_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_LAST_ERROR_TTL_SECS)
}

/// Shared engine state accessible from API handlers
#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineState {
    pub overall_status: EngineStatus,
    pub engine_id: String,
    pub engine_version: String,
    pub running_models: std::collections::HashMap<String, ModelSlot>,
    pub metrics: EngineMetrics,
    /// Per-model failure context. Keyed by `model_id`. Survives the failed
    /// slot's removal from `running_models` so users can debug the crash.
    /// Cleared individually when a model loads successfully.
    #[serde(default)]
    pub last_errors: std::collections::HashMap<String, ModelLoadError>,
    /// Per-model dismissal: `model_id → dismissed error signature`. Suppresses
    /// re-surfacing only while the SAME failure (same signature) keeps firing —
    /// a genuinely *different* failure, or a success then a new failure, lifts
    /// the suppression. Internal bookkeeping — not part of the wire status.
    #[serde(skip)]
    pub dismissed_errors: std::collections::HashMap<String, String>,
}

impl EngineState {
    /// Record a load failure for `model_id`. Returns whether it was stored.
    ///
    /// Suppressed only while the user has dismissed *this same* failure
    /// signature (the model keeps getting re-attempted per request and would
    /// otherwise reappear on every retry). A different failure signature lifts
    /// the dismissal and surfaces. Repeats of the currently-shown signature
    /// bump the occurrence `count` and refresh the timestamp rather than
    /// stacking. Caps the map at `MAX_LAST_ERRORS`, evicting the oldest by `at`.
    pub fn record_error(&mut self, model_id: &str, mut entry: ModelLoadError) -> bool {
        let sig = error_signature(entry.severity, &entry.message);

        if self.dismissed_errors.get(model_id).map(String::as_str) == Some(sig.as_str()) {
            return false;
        }
        // Different (or first) failure — any prior dismissal no longer applies.
        self.dismissed_errors.remove(model_id);

        // Same signature already showing → it's another occurrence, not a new
        // distinct error: increment the counter instead of resetting to 1.
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

    /// A successful load clears any recorded error AND lifts a prior dismissal,
    /// so a genuine *future* failure for this model resurfaces.
    pub fn clear_error(&mut self, model_id: &str) {
        self.last_errors.remove(model_id);
        self.dismissed_errors.remove(model_id);
    }

    /// User dismissed this model's error in the UI: drop it now and suppress
    /// re-surfacing of the *same* failure signature. A different failure (or a
    /// success then a new failure) will resurface on its own.
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

pub struct ActiveSlot {
    pub engine: ActiveEngine,
    pub port: u16,
    pub last_accessed: u64,
    pub keep_alive_secs: u64,
    pub size_bytes: u64,
    pub status: EngineStatus,
    /// The role this engine process was started with. Used to detect role-mismatch conflicts.
    pub role: ModelRole,
    /// Number of requests currently being served by this slot. Incremented by
    /// the orchestrator when a request is admitted (`for_request`), decremented
    /// by the request path's [`InflightGuard`](crate::server::InflightGuard) on
    /// completion. A slot with `inflight > 0` is **never** an eviction victim —
    /// tearing down an engine mid-request drops the user's in-flight work.
    pub inflight: Arc<AtomicU32>,
}

/// Returned by an `EnsureModel` command: the engine port plus the slot's shared
/// in-flight counter, so the request path can hold the model "busy" for the
/// duration of a request and the orchestrator can refuse to evict it.
pub struct ModelHandle {
    pub port: u16,
    pub inflight: Arc<AtomicU32>,
}

/// The engine manager — spawns, supervises, health-checks, and restarts the engine via Adapters
pub struct EngineManager {
    pub config: EngineConfig,
    adapter: EngineAdapterInstance,
    base_engine_port: u16,
    data_dir: PathBuf,
    models_dir: PathBuf,
    logs_dir: PathBuf,
    pub state: Arc<RwLock<EngineState>>,
    #[allow(dead_code)] // retained for planned restart-supervision logic
    max_restarts: u32,
    health_interval_secs: u64,
    active_slots: std::collections::HashMap<String, ActiveSlot>,
    global_keep_alive: String,
    max_loaded_models: u32,
    /// Empirical VRAM calibration: measured load footprints from prior
    /// successful loads, used to self-correct the analytic admission estimate.
    calibration: crate::engine::calibration::CalibrationStore,
    /// Broadcast channel sender — fires a full EngineState snapshot whenever state changes.
    /// Receivers: tray icon (in-process), SSE `/lf/status/stream` (external).
    status_tx: tokio::sync::broadcast::Sender<EngineState>,
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
        let logs_dir = data_dir.join("logs");
        let calibration = crate::engine::calibration::CalibrationStore::load(&data_dir);
        let state = Arc::new(RwLock::new(EngineState {
            overall_status: EngineStatus::Ready,
            engine_id: config.id.clone(),
            engine_version: config.version.clone(),
            running_models: std::collections::HashMap::new(),
            metrics: EngineMetrics::default(),
            last_errors: std::collections::HashMap::new(),
            dismissed_errors: std::collections::HashMap::new(),
        }));

        Self {
            config,
            adapter,
            base_engine_port,
            data_dir,
            models_dir,
            logs_dir,
            state,
            max_restarts: 3,
            health_interval_secs: 2,
            active_slots: std::collections::HashMap::new(),
            global_keep_alive,
            max_loaded_models,
            calibration,
            status_tx,
        }
    }

    /// Broadcast current state snapshot to all subscribers (tray, SSE, Tauri events).
    /// Call this after every mutation to `self.state`.
    ///
    /// Before the broadcast, every active slot's `spec_observer` is sampled
    /// into the corresponding `ModelSlot.spec_stats` so the snapshot reflects
    /// the latest acceptance-rate counters (S-2.7). Sampling is a single
    /// RwLock read per slot — negligible cost on the heartbeat path and the
    /// only place where live observer → status reconciliation happens.
    async fn notify(&self) {
        {
            let mut state = self.state.write().await;
            for (model_id, active) in &self.active_slots {
                let Some(slot) = state.running_models.get_mut(model_id) else {
                    continue;
                };
                slot.spec_mode = active.engine.spec_mode;
                slot.spec_stats = active
                    .engine
                    .spec_observer
                    .as_ref()
                    .filter(|o| o.has_samples())
                    .map(|o| o.snapshot());
            }
        }
        let snapshot = self.state.read().await.clone();
        // Ignore send errors — no subscribers is fine, lagged is fine.
        let _ = self.status_tx.send(snapshot);
    }

    /// Read this model's stderr log, build a `ModelLoadError`, and insert it
    /// into `EngineState.last_errors`. Caps the map at `MAX_LAST_ERRORS` by
    /// evicting the oldest entries (FIFO on `at`).
    ///
    /// Intentionally swallows all I/O errors — surfacing diagnostics must
    /// never fail the load path. Phase 2.3.
    async fn record_load_failure(
        &self,
        model_id: &str,
        err: &anyhow::Error,
        severity: LoadErrorSeverity,
    ) {
        let stderr_tail = crate::logging::rotation::read_stderr_tail(&self.logs_dir, model_id);
        let entry = ModelLoadError {
            at: chrono::Utc::now().to_rfc3339(),
            stderr_tail,
            message: format!("{err}"),
            severity,
            count: 1,
        };

        self.state.write().await.record_error(model_id, entry);
    }

    pub fn state(&self) -> Arc<RwLock<EngineState>> {
        Arc::clone(&self.state)
    }

    /// Legacy compat method (sets overall status and starts waiting)
    pub async fn start(&mut self) -> Result<()> {
        let mut state = self.state.write().await;
        state.overall_status = EngineStatus::Ready;
        Ok(())
    }

    pub fn set_model(&mut self, _name: String) {
        // Deprecated natively. Models are populated via EnsureModel.
    }

    pub async fn wait_for_ready(&self, _timeout_secs: u64) -> Result<()> {
        // Models are loaded dynamically now, daemon is instantly ready.
        Ok(())
    }

    /// Stop one specific active slot
    async fn stop_slot(&self, active: &mut ActiveSlot) -> Result<()> {
        let _ = self.adapter.stop(&mut active.engine).await;
        let pid_file = self
            .data_dir
            .join("engines")
            .join(format!("{}_{}.pid", self.config.id, active.port));
        let _ = std::fs::remove_file(pid_file);
        Ok(())
    }

    /// Memory (GB) currently free to admit *another* model. Accelerator-aware:
    /// - **Discrete GPU / unified memory**: live free VRAM from
    ///   [`crate::hardware::vram::get_free_vram`], which already nets out our own
    ///   resident models and other GPU consumers.
    /// - **CPU-only** (`GpuVendor::None`): safety-first admission control
    ///   ([`crate::hardware::vram::cpu_residency_free`]) — the tighter of live
    ///   `available` RAM minus an OS reserve and a hard total-RAM footprint cap.
    ///   On a no-GPU box VRAM is 0, so budgeting on it would unload every
    ///   co-resident model on each cold load; budgeting on *total* RAM instead
    ///   over-committed and pushed the host into OOM.
    fn memory_free_gb(&self, profile: &crate::hardware::probe::HardwareProfile) -> f32 {
        use crate::hardware::probe::GpuVendor;
        use crate::hardware::vram;
        if matches!(profile.gpu_vendor, GpuVendor::None) {
            let resident_sum_gb: f32 = self
                .active_slots
                .values()
                .map(|s| vram::estimate_model_vram(s.size_bytes))
                .sum();
            vram::cpu_residency_free(
                vram::free_system_ram_gb(),
                resident_sum_gb,
                profile.total_ram_gb,
            )
        } else {
            vram::get_free_vram(profile)
        }
    }

    /// Evict least-recently-used **idle** models until the new load's memory need
    /// fits, or until only actively-serving models remain.
    ///
    /// Eviction never targets a slot with `inflight > 0` — stopping a busy engine
    /// would abort an in-flight request. If freeing the budget would require
    /// evicting an active model, the loop stops and leaves the shortfall for the
    /// caller's admission check to reject. This co-loads when there's room and
    /// falls back to sequential (evict-idle-then-load) under pressure, without
    /// ever killing live work or over-committing memory.
    async fn evict_for_memory(&mut self, needed_gb: f32) -> Result<()> {
        let profile = crate::hardware::probe::detect_platform().unwrap_or_default();

        loop {
            let free_gb = self.memory_free_gb(&profile);
            if free_gb >= needed_gb || self.active_slots.is_empty() {
                break;
            }

            // Only idle slots are eligible victims. `None` ⇒ everything resident
            // is serving a request; we can't free more without aborting it.
            let Some(victim) = lru_idle_model_id(
                self.active_slots
                    .iter()
                    .map(|(k, s)| (k, s.last_accessed, s.inflight.load(Ordering::Relaxed))),
            ) else {
                break;
            };

            info!(
                "Memory budget low (free: {:.2}GB, need: {:.2}GB). Evicting idle LRU model: {}",
                free_gb, needed_gb, victim
            );
            if let Some(mut slot) = self.active_slots.remove(&victim) {
                let _ = self.stop_slot(&mut slot).await;
                self.state.write().await.running_models.remove(&victim);
            }
        }
        Ok(())
    }

    /// Dynamically spawn an adapter process for a model
    async fn spawn_adapter_process(
        &self,
        model_id: &str,
        model_dir: &Path,
        port: u16,
        role: ModelRole,
        plan: &crate::engine::adapter::LoadPlan,
    ) -> Result<ActiveEngine> {
        let engine_pid_file = self
            .data_dir
            .join("engines")
            .join(format!("{}_{}.pid", self.config.id, port));
        if tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .is_err()
        {
            warn!(
                port,
                "Port is held — attempting orphan engine cleanup via PID file then lsof"
            );
            // Step 1: PID-file based kill (fast path)
            kill_orphan_engine(&engine_pid_file);
            // Step 2: lsof-based kill (catches orphans not tracked by a PID file)
            kill_port_holder_via_lsof(port);
            // Step 3: Wait up to 5s for the OS to release the port
            let mut freed = false;
            for _ in 0..10 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .is_ok()
                {
                    freed = true;
                    break;
                }
            }
            if !freed {
                bail!(
                    "Port {} is still held after cleanup. Cannot spawn engine on this port.",
                    port
                );
            }
            info!(port, "Port freed — proceeding to spawn engine");
        }

        let active = self
            .adapter
            .start(
                model_id,
                model_dir,
                port,
                &self.data_dir,
                &self.logs_dir,
                role,
                plan,
            )
            .await?;

        if let Some(pid) = active.process.id() {
            let _ = std::fs::write(&engine_pid_file, pid.to_string());
        }
        Ok(active)
    }

    /// Wait for health check of a dynamically assigned port.
    ///
    /// Polls the engine's `/health` endpoint at 1s intervals AND polls the
    /// child process for early exit. If the child exits before the endpoint
    /// becomes healthy, bail immediately rather than burning the full
    /// 120s budget — the previous behaviour buried CLI-arg errors (e.g.
    /// vLLM 0.21 dropping `--disable-log-requests`) under a useless "health
    /// check timed out" message that arrived two minutes late.
    ///
    /// The `child` is borrowed mutably because `try_wait` requires it; we
    /// don't take ownership so the caller can still SIGTERM the process
    /// on graceful shutdown paths.
    async fn wait_slot_health(&self, port: u16, child: &mut tokio::process::Child) -> Result<()> {
        let health_url = format!("http://127.0.0.1:{}{}", port, self.config.health_endpoint);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()?;
        let start = std::time::Instant::now();
        // Tunable: vLLM cold-start (CUDA graph capture + JIT) can hit 60-120s
        // on a 7B model. The default keeps llama.cpp's old 120s budget and
        // lets users bump it for vLLM via env without touching code.
        let budget_secs: u64 = std::env::var("LMFORGE_HEALTH_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| (5..=900).contains(n))
            .unwrap_or(180);
        let budget = std::time::Duration::from_secs(budget_secs);
        loop {
            // Fast-fail if the engine process has already died — the user
            // gets the actual exit code instead of a wall-clock timeout.
            if let Ok(Some(status)) = child.try_wait() {
                bail!(
                    "Engine process exited before health-check passed \
                     (exit={:?}, port={}). Check stderr log.",
                    status.code(),
                    port
                );
            }
            if start.elapsed() > budget {
                bail!(
                    "Engine Adapter failed health verify on port {} after {}s",
                    port,
                    budget_secs
                );
            }
            if let Ok(resp) = client.get(&health_url).send().await
                && resp.status().is_success()
            {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    /// Get next available port — checks both active_slots AND whether the OS port is actually free.
    fn allocate_port(&self) -> u16 {
        let used_ports: std::collections::HashSet<u16> =
            self.active_slots.values().map(|s| s.port).collect();
        let mut port = self.base_engine_port;
        loop {
            if used_ports.contains(&port) {
                port += 1;
                continue;
            }
            // Verify the port is truly free at the OS level (guards against un-tracked orphans)
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                break;
            }
            warn!(
                port,
                "Allocated port is held by an un-tracked process — skipping"
            );
            port += 1;
        }
        port
    }

    /// Process ensure model logic
    async fn handle_ensure_model(
        &mut self,
        model_id: &str,
        keep_alive_override: &Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle> {
        let now = keepalive::now_secs();

        let keep_alive_secs = if let Some(ov) = keep_alive_override {
            keepalive::parse_keepalive(ov)
        } else {
            keepalive::parse_keepalive(&self.global_keep_alive)
        };

        info!(model_id, "Model requested — checking active slots");

        // Derive the model's functional role from its capabilities up front.
        // We need this before the early-return check so we can detect role mismatches on cached slots.
        // Rerank takes priority (cross-encoders may also have chat=true for generative re-rankers).
        // Unknown models (not in index) default to Chat for backward compatibility.
        let index = match crate::model::index::ModelIndex::load(&self.data_dir, &self.models_dir) {
            Ok(idx) => idx,
            Err(e) => {
                warn!(error = %e, "Failed to load models.json — index will be empty");
                crate::model::index::ModelIndex {
                    schema_version: 1,
                    models: vec![],
                }
            }
        };
        let role = index
            .get(model_id)
            .map(|m| {
                if m.capabilities.reranking {
                    ModelRole::Rerank
                } else if m.capabilities.embeddings {
                    ModelRole::Embed
                } else {
                    ModelRole::Chat
                }
            })
            .unwrap_or(ModelRole::Chat);

        if let Some(slot) = self.active_slots.get_mut(model_id) {
            // Guard: if the same model is already loaded but in a different role (e.g. was loaded
            // as Embed but caller wants Chat), refuse and surface a 409-style error. The user
            // must explicitly unload the model first.
            if slot.role != role {
                bail!(
                    "Model '{}' is already loaded as {:?} on port {}. \
                     Unload it first: POST /lf/model/unload with {{\"model\":\"{}\"}}",
                    model_id,
                    slot.role,
                    slot.port,
                    model_id
                );
            }
            slot.last_accessed = now;
            slot.keep_alive_secs = keep_alive_secs;
            // Mark busy before replying so a concurrent cold load can't pick this
            // warm slot as an eviction victim before the request guard is installed.
            if for_request {
                slot.inflight.fetch_add(1, Ordering::Relaxed);
            }
            return Ok(ModelHandle {
                port: slot.port,
                inflight: slot.inflight.clone(),
            });
        }

        info!(model_id, "Cold load request for model");
        let load_started = std::time::Instant::now();

        let size_bytes = index.get(model_id).map(|m| m.size_bytes).unwrap_or(0);

        let entry_path = match index.get(model_id).map(|m| m.path.clone()) {
            Some(p) => p,
            None => {
                let fallback = self.models_dir.join(model_id).to_string_lossy().to_string();
                warn!(
                    model_id,
                    fallback_path = %fallback,
                    "Model not found in index — using fallback path. \
                     This will fail if the model ID contains characters invalid for filesystem paths (e.g. colons). \
                     Pull the model first with: lmforge pull {}",
                    model_id
                );
                fallback
            }
        };
        let model_dir = PathBuf::from(entry_path);

        // ── VRAM-aware admission ─────────────────────────────────────────────
        // Resolve the full load plan (runtime + spec-dec + analytic footprint)
        // BEFORE eviction so we budget for the TRUE footprint — weights + KV +
        // spec-dec overhead — instead of a flat size×1.2 heuristic. A prior
        // measured footprint (calibration cache) overrides the analytic prior.
        let profile = crate::hardware::probe::detect_platform().unwrap_or_default();
        let free_before = self.memory_free_gb(&profile);
        let mut plan = self.adapter.plan_load(
            model_id,
            &model_dir,
            &self.data_dir,
            role,
            size_bytes,
            free_before,
        );

        // Calibration key (spec mode + ctx) is free-VRAM-independent, so the
        // pre-eviction plan is fine to derive it.
        let cal_ctx = plan.runtime.ctx_size;
        let cal_key =
            crate::engine::calibration::signature(model_id, cal_ctx, plan.spec.mode, role);
        if let Some(measured) = self.calibration.get(&cal_key) {
            plan.footprint.calibrated_total_gb = Some(measured);
        }

        // Evict idle LRU models until the FULL footprint (incl. spec-dec) fits,
        // or only actively-serving models remain. Spec-dec is first-class: idle
        // models are evicted to make room for it. The footprint is
        // free-VRAM-independent, so the pre-eviction plan budgets correctly.
        self.evict_for_memory(plan.footprint.effective_total_gb())
            .await?;

        // Honour the optional hard count cap (max_loaded_models). Only idle slots
        // may be evicted to make room; if every loaded model is busy, refuse the
        // new load rather than abort an in-flight request.
        if self.max_loaded_models > 0 && self.active_slots.len() >= self.max_loaded_models as usize
        {
            match lru_idle_model_id(
                self.active_slots
                    .iter()
                    .map(|(k, s)| (k, s.last_accessed, s.inflight.load(Ordering::Relaxed))),
            ) {
                Some(victim) => {
                    if let Some(mut slot) = self.active_slots.remove(&victim) {
                        let _ = self.stop_slot(&mut slot).await;
                        self.state.write().await.running_models.remove(&victim);
                    }
                }
                None => bail!(
                    "Cannot load '{}': model slot limit ({}) reached and all loaded \
                     models are serving requests. Retry once one becomes idle.",
                    model_id,
                    self.max_loaded_models
                ),
            }
        }

        // Re-resolve the plan AFTER eviction against the now-current free VRAM.
        // CRITICAL: the runtime params (`-ngl`, ctx) must reflect the memory we
        // just freed. Planning them against the pre-eviction free VRAM offloads
        // zero layers to the GPU and loads the whole model on CPU
        // (catastrophically slow — the original cause of the MTP warm timeout).
        // The footprint is free-VRAM-independent, so admission above stays valid.
        let free_now = self.memory_free_gb(&profile);
        let mut plan = self.adapter.plan_load(
            model_id,
            &model_dir,
            &self.data_dir,
            role,
            size_bytes,
            free_now,
        );
        if let Some(measured) = self.calibration.get(&cal_key) {
            plan.footprint.calibrated_total_gb = Some(measured);
        }
        let needed = plan.footprint.effective_total_gb();
        let base = plan.footprint.base_gb();

        // Tiered admission after eviction:
        //   free >= needed → load with spec-dec.
        //   base <= free   → degrade spec-dec to off and still serve (never lose
        //                    serve-ability for an optimization).
        //   free < base    → can't fit even the base; deny (busy vs host).
        if free_now < base {
            let busy = self
                .active_slots
                .values()
                .filter(|s| s.inflight.load(Ordering::Relaxed) > 0)
                .count();
            if busy > 0 {
                bail!(
                    "Insufficient memory to load '{}': needs ~{:.1} GB but only \
                     {:.1} GB free; {} loaded model(s) are serving requests and \
                     won't be evicted. Retry once they're idle.",
                    model_id,
                    base,
                    free_now,
                    busy
                );
            } else {
                bail!(
                    "Insufficient memory to load '{}': needs ~{:.1} GB but only \
                     {:.1} GB available on this host.",
                    model_id,
                    base,
                    free_now
                );
            }
        } else if free_now < needed
            && !matches!(plan.spec.mode, crate::engine::speculative::SpecMode::Off)
        {
            warn!(
                model_id,
                free_gb = free_now,
                needed_gb = needed,
                base_gb = base,
                spec_mode = ?plan.spec.mode,
                "Insufficient VRAM headroom for speculative decoding — loading with spec-dec disabled"
            );
            plan.spec = crate::engine::speculative::SpecResolved::off(
                "degraded: insufficient VRAM headroom for spec-dec",
            );
            plan.footprint.spec_gb = 0.0;
            plan.footprint.calibrated_total_gb = None;
        }

        let vram_est_gb = plan.footprint.effective_total_gb();
        let port = self.allocate_port();

        {
            let mut state = self.state.write().await;
            state.running_models.insert(
                model_id.to_string(),
                ModelSlot {
                    model_id: model_id.to_string(),
                    port,
                    status: EngineStatus::Starting,
                    idle_secs: 0,
                    vram_est_gb,
                    spec_mode: plan.spec.mode,
                    spec_stats: None,
                },
            );
        }

        // Snapshot free VRAM just before spawning so we can measure the real
        // load footprint after health passes and feed it back to the
        // calibration cache (GPU only).
        let on_gpu = profile.gpu_vendor != crate::hardware::probe::GpuVendor::None;
        let free_pre_spawn = if on_gpu { free_now } else { 0.0 };

        // Spawn and wait for engine health. On any failure, clean up the dangling Starting slot
        // so the next EnsureModel call can retry a clean cold load.
        let mut engine = match self
            .spawn_adapter_process(model_id, &model_dir, port, role, &plan)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                let sev = LoadErrorSeverity::for_spawn_failure(&e);
                self.record_load_failure(model_id, &e, sev).await;
                self.state.write().await.running_models.remove(model_id);
                crate::server::metrics::observe_model_load(
                    model_id,
                    false,
                    load_started.elapsed().as_secs_f64(),
                );
                self.notify().await;
                return Err(e);
            }
        };

        // S-2.8 retry book-keeping: capture whether spec-dec was active
        // for THIS spawn attempt. If the slot dies before health passes
        // AND elapsed is < 5s, we'll retry once with spec forced off —
        // the most common cause of fast crashes on opt-in spec-dec is a
        // misconfigured draft head / MoE clamp / VRAM headroom misread.
        let spec_was_on_first_attempt =
            engine.spec_mode != crate::engine::speculative::SpecMode::Off;

        if let Err(e) = self.wait_slot_health(port, &mut engine.process).await {
            // Health failed — kill the orphan in-place (same as before),
            // then decide whether to retry with spec=off.
            warn!(model_id, port, error = %e, "Engine health check failed — cleaning up orphan");
            let _ = self.adapter.stop(&mut engine).await;

            let elapsed = load_started.elapsed();
            let early_crash = elapsed < std::time::Duration::from_secs(5);

            if spec_was_on_first_attempt && early_crash {
                warn!(
                    model_id,
                    port,
                    elapsed_ms = elapsed.as_millis() as u64,
                    first_error = %e,
                    "Spec-dec engine died <5s after spawn — retrying once with spec=off (S-2.8)"
                );

                if engine.spec_mode == crate::engine::speculative::SpecMode::DraftModel {
                    let hf_repo =
                        crate::model::index::ModelIndex::load(&self.data_dir, &self.models_dir)
                            .ok()
                            .and_then(|idx| idx.get(model_id).and_then(|e| e.hf_repo.clone()));
                    if let Some(draft_id) =
                        crate::engine::draft_pairs::lookup_draft_pair(model_id, hf_repo.as_deref())
                        && let Err(rec_err) = crate::engine::draft_pairs::record_broken_pair(
                            &self.data_dir,
                            model_id,
                            &draft_id,
                            &e.to_string(),
                        )
                    {
                        warn!(model_id, error = %rec_err, "Failed to record broken draft pair");
                    }
                }

                // Force spec-dec off for the retry by mutating the threaded
                // plan — no global env juggling. The downgraded plan flows
                // straight into `start`, so the retry spawns with spec=off.
                plan.spec = crate::engine::speculative::SpecResolved::off(
                    "S-2.8 retry: spec-dec disabled after early crash",
                );
                plan.footprint.spec_gb = 0.0;

                let retry_result = self
                    .spawn_adapter_process(model_id, &model_dir, port, role, &plan)
                    .await;

                match retry_result {
                    Ok(mut retry_engine) => {
                        if let Err(e2) =
                            self.wait_slot_health(port, &mut retry_engine.process).await
                        {
                            let _ = self.adapter.stop(&mut retry_engine).await;
                            // Annotate the message so users see both
                            // attempts when reading `/lf/status`.
                            let combined = anyhow::anyhow!(
                                "spec-dec retry with spec=off also failed: {e2} \
                                 (first attempt failed in {}ms with: {e})",
                                elapsed.as_millis()
                            );
                            // Engine spawned but never went healthy on either
                            // attempt — a runtime crash/timeout, not a config error.
                            self.record_load_failure(
                                model_id,
                                &combined,
                                LoadErrorSeverity::Transient,
                            )
                            .await;
                            self.state.write().await.running_models.remove(model_id);
                            crate::server::metrics::observe_model_load(
                                model_id,
                                false,
                                load_started.elapsed().as_secs_f64(),
                            );
                            self.notify().await;
                            return Err(combined);
                        }
                        info!(
                            model_id,
                            "Spec-dec retry succeeded — slot is Ready with spec=off"
                        );
                        engine = retry_engine;
                    }
                    Err(e2) => {
                        let sev = LoadErrorSeverity::for_spawn_failure(&e2);
                        let combined = anyhow::anyhow!(
                            "spec-dec retry spawn failed: {e2} \
                             (first attempt failed in {}ms with: {e})",
                            elapsed.as_millis()
                        );
                        self.record_load_failure(model_id, &combined, sev).await;
                        self.state.write().await.running_models.remove(model_id);
                        crate::server::metrics::observe_model_load(
                            model_id,
                            false,
                            load_started.elapsed().as_secs_f64(),
                        );
                        self.notify().await;
                        return Err(combined);
                    }
                }
            } else {
                // Engine spawned but failed its health check — runtime
                // crash/timeout, classified transient (may recover on retry).
                self.record_load_failure(model_id, &e, LoadErrorSeverity::Transient)
                    .await;
                self.state.write().await.running_models.remove(model_id);
                crate::server::metrics::observe_model_load(
                    model_id,
                    false,
                    load_started.elapsed().as_secs_f64(),
                );
                self.notify().await;
                return Err(e);
            }
        }

        // Health passed — measure the real VRAM footprint and feed it back to
        // the calibration cache so the next load of this exact configuration
        // budgets on ground truth instead of the analytic prior. Keyed by the
        // spec mode actually in effect (the retry/degrade path may have flipped
        // it to Off). GPU only; CPU residency is RAM-accounted separately.
        if on_gpu {
            let free_after = self.memory_free_gb(&profile);
            let measured = free_pre_spawn - free_after;
            let key = crate::engine::calibration::signature(
                model_id,
                cal_ctx,
                engine.spec_mode,
                role,
            );
            self.calibration.record(key, measured);
        }

        self.active_slots.insert(
            model_id.to_string(),
            ActiveSlot {
                engine,
                port,
                last_accessed: keepalive::now_secs(),
                keep_alive_secs,
                size_bytes,
                status: EngineStatus::Ready,
                role,
                inflight: Arc::new(AtomicU32::new(0)),
            },
        );

        {
            let mut state = self.state.write().await;
            if let Some(slot) = state.running_models.get_mut(model_id) {
                slot.status = EngineStatus::Ready;
            }
            // The previous load attempt (if any) succeeded — drop its stderr tail
            // and lift any user dismissal so a genuine future failure resurfaces.
            state.clear_error(model_id);
        }

        // Notify all status subscribers that a new model is ready.
        self.notify().await;
        crate::server::metrics::observe_model_load(
            model_id,
            true,
            load_started.elapsed().as_secs_f64(),
        );
        crate::server::metrics::set_active_models(self.active_slots.len() as u64);

        // Hand back a handle. Mark busy before replying (same race guard as the
        // warm path) so this freshly-loaded slot can't be evicted out from under
        // the request that triggered the load.
        let inflight = self
            .active_slots
            .get(model_id)
            .map(|s| s.inflight.clone())
            .unwrap_or_else(|| Arc::new(AtomicU32::new(0)));
        if for_request {
            inflight.fetch_add(1, Ordering::Relaxed);
        }
        Ok(ModelHandle { port, inflight })
    }

    pub async fn supervise(mut self, mut cmd_rx: tokio::sync::mpsc::Receiver<ManagerCommand>) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(ManagerCommand::EnsureModel { model_id, keep_alive_override, for_request, reply }) => {
                            let res = self.handle_ensure_model(&model_id, &keep_alive_override, for_request).await;
                            let _ = reply.send(res);
                        }
                        Some(ManagerCommand::UnloadModel(model_id)) => {
                            if let Some(mut slot) = self.active_slots.remove(&model_id) {
                                let _ = self.stop_slot(&mut slot).await;
                                self.state.write().await.running_models.remove(&model_id);
                                crate::server::metrics::set_active_models(self.active_slots.len() as u64);
                                self.notify().await;
                            }
                        }
                        Some(ManagerCommand::UnloadAll) => {
                            // Collect first to release the mutable borrow on active_slots
                            // before stop_slot() needs to borrow self immutably.
                            let slots: Vec<_> = self.active_slots.drain().map(|(_, v)| v).collect();
                            for mut slot in slots {
                                let _ = self.stop_slot(&mut slot).await;
                            }
                            self.state.write().await.running_models.clear();
                            crate::server::metrics::set_active_models(0);
                            self.notify().await;
                        }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(self.health_interval_secs)) => {
                    // Check TTL
                    let now = keepalive::now_secs();
                    let mut to_evict = Vec::new();
                    for (id, slot) in self.active_slots.iter() {
                        // Never TTL-evict a model that is mid-request: a long
                        // stream can outlive its keep-alive window, and stamping
                        // last_accessed only happens at request start.
                        if slot.keep_alive_secs > 0 && (now.saturating_sub(slot.last_accessed) > slot.keep_alive_secs)
                            && self.config.id != "omlx"
                            && slot.inflight.load(Ordering::Relaxed) == 0 {
                                to_evict.push(id.clone());
                            }
                    }

                    for id in to_evict {
                        info!("TTL expired for {}, unloading...", id);
                        if let Some(mut slot) = self.active_slots.remove(&id) {
                            let _ = self.stop_slot(&mut slot).await;
                            self.state.write().await.running_models.remove(&id);
                        }
                    }
                    crate::server::metrics::set_active_models(self.active_slots.len() as u64);

                    // Sync State Update
                    let mut state = self.state.write().await;
                    for (id, slot) in self.active_slots.iter() {
                        if let Some(public_slot) = state.running_models.get_mut(id) {
                            public_slot.idle_secs = now.saturating_sub(slot.last_accessed);
                        }
                    }
                    // Evict stale load errors (TTL). Successful loads already clear
                    // their entry; this bounds one-off failures that never recover.
                    let ttl = last_error_ttl_secs();
                    if ttl > 0 && !state.last_errors.is_empty() {
                        let now_ts = chrono::Utc::now().timestamp();
                        state.last_errors.retain(|_, e| {
                            match chrono::DateTime::parse_from_rfc3339(&e.at) {
                                Ok(at) => now_ts - at.timestamp() < ttl,
                                // Unparseable timestamp: keep it rather than risk
                                // dropping a real error on a parse quirk.
                                Err(_) => true,
                            }
                        });
                    }
                    // Notify after every heartbeat tick so idle_secs stays fresh
                    // in the tray / SSE stream without requiring a state-change event.
                    drop(state);
                    self.notify().await;
                }
            }
        }
    }
}

fn kill_orphan_engine(pid_file: &std::path::Path) {
    if let Ok(content) = std::fs::read_to_string(pid_file)
        && let Ok(pid) = content.trim().parse::<u32>()
    {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        }
        #[cfg(windows)]
        {
            // taskkill /F (force) /PID <pid> — equivalent of SIGKILL on Windows
            let _ = crate::util::subprocess::hidden("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
        let _ = std::fs::remove_file(pid_file);
    }
}

/// Use `lsof -ti :PORT` to find any process holding a port and send SIGKILL.
/// This catches orphans that were never tracked in a PID file (e.g. engine processes
/// that survived a daemon restart via a prior binary version).
fn kill_port_holder_via_lsof(port: u16) {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{}", port)])
        .output();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                warn!(
                    pid,
                    port, "Sending SIGKILL to un-tracked port holder (via lsof)"
                );
                #[cfg(unix)]
                {
                    use nix::sys::signal::{Signal, kill};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                }
                #[cfg(windows)]
                {
                    let _ = crate::util::subprocess::hidden("taskkill")
                        .args(["/F", "/PID", &pid.to_string()])
                        .output();
                }
            }
        }
    }
}

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

        // Same failure keeps firing (fresh timestamp, same message) — stays quiet.
        let stored = s.record_error("m", err("t1", LoadErrorSeverity::Transient));
        assert!(!stored, "same-signature failure should be suppressed");
        assert!(!s.last_errors.contains_key("m"), "no new error surfaced");
    }

    #[test]
    fn volatile_numbers_do_not_break_suppression() {
        // Same failure kind, different port/timing → same signature → suppressed.
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

        // A genuinely different failure mode should surface immediately.
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

        // Model finally loads OK → clears error and the suppression.
        s.clear_error("m");
        assert!(
            !s.dismissed_errors.contains_key("m"),
            "dismissal lifted on success"
        );

        // The same failure after a good load should surface again.
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
        // 'b' is untouched and still surfaces new failures.
        assert!(s.record_error("b", err("t1", LoadErrorSeverity::UserError)));
        assert!(s.last_errors.contains_key("b"));
        assert!(!s.last_errors.contains_key("a"));
    }

    #[test]
    fn lru_idle_picks_oldest_idle() {
        // (id, last_accessed, inflight). All idle → smallest last_accessed wins.
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
        // The true LRU ("embed") is actively serving → it must be skipped, and
        // the oldest *idle* model ("vlm") chosen instead. This is the core of the
        // "never evict a model that's mid-request" guarantee.
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
        // Every slot busy → no eviction victim → caller must reject the load
        // rather than abort in-flight work.
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
            // Lexicographically sortable timestamps so the oldest evicts first.
            s.record_error(
                &format!("m{i:02}"),
                err(
                    &format!("2026-01-01T00:00:{i:02}Z"),
                    LoadErrorSeverity::Transient,
                ),
            );
        }
        assert_eq!(s.last_errors.len(), MAX_LAST_ERRORS);
        // Oldest (m00) evicted, newest retained.
        assert!(!s.last_errors.contains_key("m00"));
        assert!(
            s.last_errors
                .contains_key(&format!("m{:02}", MAX_LAST_ERRORS + 3))
        );
    }
}
