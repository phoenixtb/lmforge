use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
}

const MAX_LAST_ERRORS: usize = 8;

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
    /// Broadcast channel sender — fires a full EngineState snapshot whenever state changes.
    /// Receivers: tray icon (in-process), SSE `/lf/status/stream` (external).
    status_tx: tokio::sync::broadcast::Sender<EngineState>,
}

pub enum ManagerCommand {
    EnsureModel {
        model_id: String,
        keep_alive_override: Option<String>,
        reply: tokio::sync::oneshot::Sender<Result<u16>>,
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
        let state = Arc::new(RwLock::new(EngineState {
            overall_status: EngineStatus::Ready,
            engine_id: config.id.clone(),
            engine_version: config.version.clone(),
            running_models: std::collections::HashMap::new(),
            metrics: EngineMetrics::default(),
            last_errors: std::collections::HashMap::new(),
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
    async fn record_load_failure(&self, model_id: &str, err: &anyhow::Error) {
        let stderr_tail = crate::logging::rotation::read_stderr_tail(&self.logs_dir, model_id);
        let entry = ModelLoadError {
            at: chrono::Utc::now().to_rfc3339(),
            stderr_tail,
            message: format!("{err}"),
        };

        let mut state = self.state.write().await;
        state.last_errors.insert(model_id.to_string(), entry);

        // Cap at MAX_LAST_ERRORS — evict oldest by `at`.
        if state.last_errors.len() > MAX_LAST_ERRORS
            && let Some(oldest_key) = state
                .last_errors
                .iter()
                .min_by(|a, b| a.1.at.cmp(&b.1.at))
                .map(|(k, _)| k.clone())
        {
            state.last_errors.remove(&oldest_key);
        }
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

    /// Evict least recently used models until needed VRAM is free
    async fn evict_for_vram(&mut self, needed_vram_gb: f32) -> Result<()> {
        let profile = crate::hardware::probe::detect_platform().unwrap_or_default();

        loop {
            let free_vram = crate::hardware::vram::get_free_vram(&profile);
            if free_vram >= needed_vram_gb || self.active_slots.is_empty() {
                break;
            }

            // Find oldest accessed
            if let Some((oldest_id, _)) = self
                .active_slots
                .iter()
                .min_by_key(|(_, slot)| slot.last_accessed)
                .map(|(k, v)| (k.clone(), v.last_accessed))
            {
                info!(
                    "VRAM starved (free: {:.2}GB, need: {:.2}GB). Evicting LRU model: {}",
                    free_vram, needed_vram_gb, oldest_id
                );
                if let Some(mut slot) = self.active_slots.remove(&oldest_id) {
                    let _ = self.stop_slot(&mut slot).await;
                    self.state.write().await.running_models.remove(&oldest_id);
                }
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
    ) -> Result<u16> {
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
            return Ok(slot.port);
        }

        info!(model_id, "Cold load request for model");
        let load_started = std::time::Instant::now();

        let size_bytes = index.get(model_id).map(|m| m.size_bytes).unwrap_or(0);
        let needed_vram_gb = crate::hardware::vram::estimate_model_vram(size_bytes);

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

        self.evict_for_vram(needed_vram_gb).await?;

        if self.max_loaded_models > 0
            && self.active_slots.len() >= self.max_loaded_models as usize
            && let Some((oldest_id, _)) = self
                .active_slots
                .iter()
                .min_by_key(|(_, slot)| slot.last_accessed)
                .map(|(k, v)| (k.clone(), v.last_accessed))
            && let Some(mut slot) = self.active_slots.remove(&oldest_id)
        {
            let _ = self.stop_slot(&mut slot).await;
            self.state.write().await.running_models.remove(&oldest_id);
        }

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
                    vram_est_gb: needed_vram_gb,
                    // spec mode + stats are filled in after the engine
                    // becomes Ready (we don't know spec.mode until the
                    // adapter resolves it inside `start()`).
                    spec_mode: crate::engine::speculative::SpecMode::Off,
                    spec_stats: None,
                },
            );
        }

        // Spawn and wait for engine health. On any failure, clean up the dangling Starting slot
        // so the next EnsureModel call can retry a clean cold load.
        let mut engine = match self
            .spawn_adapter_process(model_id, &model_dir, port, role)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                self.record_load_failure(model_id, &e).await;
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

                // Save + restore the existing env override so an operator
                // who set `LMFORGE_SPECULATIVE_MODE=mtp` explicitly isn't
                // permanently overridden by our retry. The supervisor
                // task is the single writer of this var, and command
                // dispatch is sequential, so the restore window can't
                // race with another spawn.
                //
                // SAFETY: `set_var` / `remove_var` are marked `unsafe` in
                // Rust 1.84+ to flag the thread hazard. We're on the
                // supervisor task, which is the only thread that calls
                // `engine::speculative::resolve`.
                let saved = std::env::var("LMFORGE_SPECULATIVE_MODE").ok();
                unsafe { std::env::set_var("LMFORGE_SPECULATIVE_MODE", "off") };

                let retry_result = self
                    .spawn_adapter_process(model_id, &model_dir, port, role)
                    .await;

                unsafe {
                    match saved.as_deref() {
                        Some(v) => std::env::set_var("LMFORGE_SPECULATIVE_MODE", v),
                        None => std::env::remove_var("LMFORGE_SPECULATIVE_MODE"),
                    }
                }

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
                            self.record_load_failure(model_id, &combined).await;
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
                        let combined = anyhow::anyhow!(
                            "spec-dec retry spawn failed: {e2} \
                             (first attempt failed in {}ms with: {e})",
                            elapsed.as_millis()
                        );
                        self.record_load_failure(model_id, &combined).await;
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
                self.record_load_failure(model_id, &e).await;
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
            },
        );

        {
            let mut state = self.state.write().await;
            if let Some(slot) = state.running_models.get_mut(model_id) {
                slot.status = EngineStatus::Ready;
            }
            // The previous load attempt (if any) succeeded — drop its stderr tail.
            state.last_errors.remove(model_id);
        }

        // Notify all status subscribers that a new model is ready.
        self.notify().await;
        crate::server::metrics::observe_model_load(
            model_id,
            true,
            load_started.elapsed().as_secs_f64(),
        );
        crate::server::metrics::set_active_models(self.active_slots.len() as u64);

        Ok(port)
    }

    pub async fn supervise(mut self, mut cmd_rx: tokio::sync::mpsc::Receiver<ManagerCommand>) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(ManagerCommand::EnsureModel { model_id, keep_alive_override, reply }) => {
                            let res = self.handle_ensure_model(&model_id, &keep_alive_override).await;
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
                        if slot.keep_alive_secs > 0 && (now.saturating_sub(slot.last_accessed) > slot.keep_alive_secs)
                            && self.config.id != "omlx" {
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
