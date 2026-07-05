use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, EngineAdapterInstance, ModelRole};
use crate::engine::keepalive;
use crate::engine::manager::{
    EngineState, EngineStatus, LoadErrorSeverity, ModelHandle, ModelLoadError, ModelSlot,
};
use crate::engine::registry::EngineConfig;
use crate::engine::residency::{Residency, ResidencyKind};

// ── ActiveSlot ───────────────────────────────────────────────────────────────

/// An active, in-process engine slot managed by `ProcessPoolResidency`.
pub struct ActiveSlot {
    pub engine: ActiveEngine,
    pub port: u16,
    pub last_accessed: u64,
    pub keep_alive_secs: u64,
    /// Byte-size of the model weights — used for VRAM budgeting and the
    /// calibration cache. Sourced from the model index at load time.
    pub size_bytes: u64,
    pub status: EngineStatus,
    pub role: ModelRole,
    /// Reference-counted inflight counter. Incremented by the manager before
    /// handing a `ModelHandle` back to the request path; decremented when the
    /// `InflightGuard` that wraps the handle drops. The value is shared between
    /// the slot and all `ModelHandle`s issued for it, so the eviction path
    /// always sees the live count without a separate message round-trip.
    pub inflight: Arc<AtomicU32>,
}

// ── LRU helper ───────────────────────────────────────────────────────────────

/// Pick the least-recently-used model id among *idle* slots (`inflight == 0`).
///
/// Slots that are actively serving a request (`inflight > 0`) are filtered out
/// entirely — they are never eviction victims, because stopping the engine would
/// abort the in-flight request. Returns `None` when every candidate is busy (the
/// caller then enforces admission control: reject rather than over-commit). Pure
/// so the policy is unit-testable without spawning engines. Items are
/// `(id, last_accessed, inflight)`.
pub(crate) fn lru_idle_model_id<'a>(
    slots: impl IntoIterator<Item = (&'a String, u64, u32)>,
) -> Option<String> {
    slots
        .into_iter()
        .filter(|(_, _, inflight)| *inflight == 0)
        .min_by_key(|(_, last_accessed, _)| *last_accessed)
        .map(|(id, _, _)| id.clone())
}

// ── ProcessPoolResidency ──────────────────────────────────────────────────────

/// Per-model-process residency strategy.
///
/// This is the battle-tested pool logic extracted verbatim from `EngineManager`.
/// One OS process (and one TCP port) per loaded model; LMForge owns admission
/// control, LRU eviction, health polling, crash reaping, and TTL sweeps.
///
/// Used by every engine except oMLX (which uses `SharedServerResidency` after
/// Phase 3). During Phase 1–2 oMLX is still routed through this strategy while
/// `SharedServerResidency` is built and validated behind the
/// `LMFORGE_OMLX_SHARED` flag.
pub struct ProcessPoolResidency {
    pub(crate) config: EngineConfig,
    pub(crate) adapter: EngineAdapterInstance,
    pub(crate) base_engine_port: u16,
    pub(crate) data_dir: PathBuf,
    pub(crate) models_dir: PathBuf,
    pub(crate) logs_dir: PathBuf,
    pub(crate) state: Arc<RwLock<EngineState>>,
    pub(crate) active_slots: std::collections::HashMap<String, ActiveSlot>,
    pub(crate) global_keep_alive: String,
    pub(crate) max_loaded_models: u32,
    pub(crate) calibration: crate::engine::calibration::CalibrationStore,
    pub(crate) status_tx: tokio::sync::broadcast::Sender<EngineState>,
}

impl ProcessPoolResidency {
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
            metrics: crate::engine::manager::EngineMetrics::default(),
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
            active_slots: std::collections::HashMap::new(),
            global_keep_alive,
            max_loaded_models,
            calibration,
            status_tx,
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Broadcast current state snapshot to all subscribers (tray, SSE, Tauri events).
    /// Call this after every mutation to `self.state`.
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
        let _ = self.status_tx.send(snapshot);
    }

    /// Read this model's stderr log, build a `ModelLoadError`, and insert it
    /// into `EngineState.last_errors`. Caps the map at `MAX_LAST_ERRORS` by
    /// evicting the oldest entries (FIFO on `at`).
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

    /// Record a cold-load failure, remove the in-progress slot, emit metrics, and
    /// broadcast state. All four cold-load error paths call this before returning Err.
    async fn fail_load(
        &mut self,
        model_id: &str,
        err: &anyhow::Error,
        severity: LoadErrorSeverity,
        load_started: std::time::Instant,
    ) {
        self.record_load_failure(model_id, err, severity).await;
        self.state.write().await.running_models.remove(model_id);
        crate::server::metrics::observe_model_load(
            model_id,
            false,
            load_started.elapsed().as_secs_f64(),
        );
        self.notify().await;
    }

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
    async fn evict_for_memory(&mut self, needed_gb: f32) -> Result<()> {
        let profile = crate::hardware::probe::detect_platform().unwrap_or_default();

        loop {
            let free_gb = self.memory_free_gb(&profile);
            if free_gb >= needed_gb || self.active_slots.is_empty() {
                break;
            }

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
                let victim_gb = crate::hardware::vram::estimate_model_vram(slot.size_bytes);
                let _ = self.stop_slot(&mut slot).await;
                self.state.write().await.running_models.remove(&victim);
                self.wait_for_memory_release(&profile, free_gb, victim_gb, needed_gb)
                    .await;
            }
        }
        Ok(())
    }

    /// Block briefly until an eviction is actually reflected in the free-memory
    /// probe. The OS releases a killed engine's memory asynchronously (WDDM in
    /// particular lags process exit), so re-reading free memory immediately
    /// after `stop_slot` sees a stale figure — the eviction loop then either
    /// over-evicts or the admission gate below rejects a load that would fit.
    /// Exits as soon as free memory covers the need or shows a meaningful chunk
    /// of the victim's footprint back; gives up after ~3s and lets the caller
    /// proceed with whatever the probe reports.
    async fn wait_for_memory_release(
        &self,
        profile: &crate::hardware::probe::HardwareProfile,
        free_before_gb: f32,
        victim_gb: f32,
        needed_gb: f32,
    ) {
        let target = free_before_gb + (victim_gb * 0.5).max(0.1);
        for _ in 0..10 {
            let free = self.memory_free_gb(profile);
            if free >= needed_gb || free >= target {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        warn!(
            free_before_gb,
            victim_gb, "Evicted model's memory not yet visible as free after 3s — proceeding"
        );
    }

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
            kill_orphan_engine(&engine_pid_file);
            kill_port_holder_via_lsof(port);
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
    /// child process for early exit.
    async fn wait_slot_health(&self, port: u16, child: &mut tokio::process::Child) -> Result<()> {
        let health_url = format!("http://127.0.0.1:{}{}", port, self.config.health_endpoint);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()?;
        let start = std::time::Instant::now();
        let budget_secs: u64 = std::env::var("LMFORGE_HEALTH_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| (5..=900).contains(n))
            .unwrap_or(180);
        let budget = std::time::Duration::from_secs(budget_secs);
        loop {
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
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                break;
            }
            warn!(
                port,
                "Port held by un-tracked process — attempting orphan cleanup"
            );
            let pid_file = self
                .data_dir
                .join("engines")
                .join(format!("{}_{}.pid", self.config.id, port));
            kill_orphan_engine(&pid_file);
            kill_port_holder_via_lsof(port);
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                info!(port, "Port freed after orphan cleanup");
                break;
            }
            warn!(
                port,
                "Port still occupied after cleanup — skipping to next port"
            );
            port += 1;
        }
        port
    }

    /// S-2.8 fallback: retry the spawn with speculative decoding disabled.
    ///
    /// Called when the first spawn dies in <5s while spec-dec was active. Mutates
    /// `plan` to disable spec, records any broken draft pair, spawns once more, and
    /// waits for health.  Returns the new `ActiveEngine` on success.
    async fn try_speculative_retry(
        &mut self,
        model_id: &str,
        model_dir: &Path,
        port: u16,
        role: ModelRole,
        plan: &mut crate::engine::adapter::LoadPlan,
        first_error: &anyhow::Error,
        first_spec_mode: crate::engine::speculative::SpecMode,
        elapsed: std::time::Duration,
        load_started: std::time::Instant,
    ) -> Result<crate::engine::adapter::ActiveEngine> {
        warn!(
            model_id,
            port,
            elapsed_ms = elapsed.as_millis() as u64,
            first_error = %first_error,
            "Spec-dec engine died <5s after spawn — retrying once with spec=off (S-2.8)"
        );

        if first_spec_mode == crate::engine::speculative::SpecMode::DraftModel {
            let hf_repo = crate::model::index::ModelIndex::load(&self.data_dir, &self.models_dir)
                .ok()
                .and_then(|idx| idx.get(model_id).and_then(|e| e.hf_repo.clone()));
            if let Some(draft_id) =
                crate::engine::draft_pairs::lookup_draft_pair(model_id, hf_repo.as_deref())
                && let Err(rec_err) = crate::engine::draft_pairs::record_broken_pair(
                    &self.data_dir,
                    model_id,
                    &draft_id,
                    &first_error.to_string(),
                )
            {
                warn!(model_id, error = %rec_err, "Failed to record broken draft pair");
            }
        }

        plan.spec = crate::engine::speculative::SpecResolved::off(
            "S-2.8 retry: spec-dec disabled after early crash",
        );
        plan.footprint.spec_gb = 0.0;

        match self
            .spawn_adapter_process(model_id, model_dir, port, role, plan)
            .await
        {
            Ok(mut retry_engine) => {
                if let Err(e2) = self.wait_slot_health(port, &mut retry_engine.process).await {
                    let _ = self.adapter.stop(&mut retry_engine).await;
                    let combined = anyhow::anyhow!(
                        "spec-dec retry with spec=off also failed: {e2} \
                         (first attempt failed in {}ms with: {first_error})",
                        elapsed.as_millis()
                    );
                    self.fail_load(
                        model_id,
                        &combined,
                        LoadErrorSeverity::Transient,
                        load_started,
                    )
                    .await;
                    return Err(combined);
                }
                info!(
                    model_id,
                    "Spec-dec retry succeeded — slot is Ready with spec=off"
                );
                Ok(retry_engine)
            }
            Err(e2) => {
                let sev = LoadErrorSeverity::for_spawn_failure(&e2);
                let combined = anyhow::anyhow!(
                    "spec-dec retry spawn failed: {e2} \
                     (first attempt failed in {}ms with: {first_error})",
                    elapsed.as_millis()
                );
                self.fail_load(model_id, &combined, sev, load_started).await;
                Err(combined)
            }
        }
    }

    /// Core model-load path: warm-hit fast path, then VRAM-aware cold load with
    /// speculative-decoding retry (S-2.8) and calibration feedback.
    async fn ensure_model_inner(
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

        let cal_ctx = plan.runtime.ctx_size;
        let cal_key =
            crate::engine::calibration::signature(model_id, cal_ctx, plan.spec.mode, role);
        if let Some(measured) = self.calibration.get(&cal_key) {
            plan.footprint.calibrated_total_gb = Some(measured);
        }

        self.evict_for_memory(plan.footprint.effective_total_gb())
            .await?;

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

        // Discrete-GPU hard gate on the FULL footprint (weights + KV + scratch,
        // spec already degraded above), not just `base`. Proceeding when the KV
        // and compute buffers don't fit doesn't fail cleanly on Windows — WDDM
        // silently pages the overflow into shared system RAM, which costs 4-6x
        // decode throughput and has corrupted engine output on Blackwell
        // (2026-07-06 incident). Reject loudly instead; the eviction pass above
        // already freed everything idle. Apple unified memory is exempt (Metal
        // paging is safe and the historical behaviour is correct there), as is
        // CPU-only (its admission maths live in `cpu_residency_free`).
        let needed_final = plan.footprint.effective_total_gb();
        let discrete_gpu = !matches!(profile.gpu_vendor, crate::hardware::probe::GpuVendor::None)
            && !profile.unified_mem;
        if discrete_gpu && free_now < needed_final {
            bail!(
                "Insufficient VRAM to load '{}': needs ~{:.1} GB (weights + KV cache \
                 + compute buffers) but only {:.1} GB free after evicting idle models. \
                 Loading anyway would spill into system memory (slow, and unstable on \
                 some drivers). Unload a model or wait for busy ones to go idle.",
                model_id,
                needed_final,
                free_now
            );
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

        let on_gpu = profile.gpu_vendor != crate::hardware::probe::GpuVendor::None;
        let free_pre_spawn = if on_gpu { free_now } else { 0.0 };

        let mut engine = match self
            .spawn_adapter_process(model_id, &model_dir, port, role, &plan)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                let sev = LoadErrorSeverity::for_spawn_failure(&e);
                self.fail_load(model_id, &e, sev, load_started).await;
                return Err(e);
            }
        };

        let first_spec_mode = engine.spec_mode;

        if let Err(e) = self.wait_slot_health(port, &mut engine.process).await {
            warn!(model_id, port, error = %e, "Engine health check failed — cleaning up orphan");
            let _ = self.adapter.stop(&mut engine).await;
            let elapsed = load_started.elapsed();
            let early_crash = elapsed < std::time::Duration::from_secs(5);

            if first_spec_mode != crate::engine::speculative::SpecMode::Off && early_crash {
                engine = self
                    .try_speculative_retry(
                        model_id,
                        &model_dir,
                        port,
                        role,
                        &mut plan,
                        &e,
                        first_spec_mode,
                        elapsed,
                        load_started,
                    )
                    .await?;
            } else {
                self.fail_load(model_id, &e, LoadErrorSeverity::Transient, load_started)
                    .await;
                return Err(e);
            }
        }

        if on_gpu {
            let free_after = self.memory_free_gb(&profile);
            let measured = free_pre_spawn - free_after;
            let key =
                crate::engine::calibration::signature(model_id, cal_ctx, engine.spec_mode, role);
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
            state.clear_error(model_id);
        }

        self.notify().await;
        crate::server::metrics::observe_model_load(
            model_id,
            true,
            load_started.elapsed().as_secs_f64(),
        );
        crate::server::metrics::set_active_models(self.active_slots.len() as u64);

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
}

// ── Residency impl ────────────────────────────────────────────────────────────

impl Residency for ProcessPoolResidency {
    fn kind(&self) -> ResidencyKind {
        ResidencyKind::ProcessPool
    }

    async fn ensure_model(
        &mut self,
        model_id: &str,
        keep_alive_override: &Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle> {
        self.ensure_model_inner(model_id, keep_alive_override, for_request)
            .await
    }

    async fn unload_model(&mut self, model_id: &str) {
        if let Some(mut slot) = self.active_slots.remove(model_id) {
            let _ = self.stop_slot(&mut slot).await;
            self.state.write().await.running_models.remove(model_id);
            crate::server::metrics::set_active_models(self.active_slots.len() as u64);
            self.notify().await;
        }
    }

    async fn unload_all(&mut self) {
        let slots: Vec<_> = self.active_slots.drain().map(|(_, v)| v).collect();
        for mut slot in slots {
            let _ = self.stop_slot(&mut slot).await;
        }
        self.state.write().await.running_models.clear();
        crate::server::metrics::set_active_models(0);
        self.notify().await;
    }

    /// Periodic heartbeat: reap crashed engines, sweep TTL-expired slots,
    /// evict stale error entries, and broadcast the updated state.
    async fn heartbeat_tick(&mut self) {
        // ── crash reap ────────────────────────────────────────────────────────
        // Detect engine processes that exited without going through stop_slot
        // (crash, OOM killer, external kill). Without this the UI keeps showing
        // "loaded" slots whose ports are dead.
        let mut crashed = Vec::new();
        for (id, slot) in self.active_slots.iter_mut() {
            if let Ok(Some(status)) = slot.engine.process.try_wait() {
                warn!(model_id = %id, ?status, "Engine process exited unexpectedly — removing slot");
                crashed.push(id.clone());
            }
        }
        for id in crashed {
            if let Some(slot) = self.active_slots.remove(&id) {
                let pid_file = self
                    .data_dir
                    .join("engines")
                    .join(format!("{}_{}.pid", self.config.id, slot.port));
                let _ = std::fs::remove_file(pid_file);
                self.state.write().await.running_models.remove(&id);
            }
        }

        // ── TTL sweep ─────────────────────────────────────────────────────────
        // oMLX in ProcessPool mode (LMFORGE_OMLX_SHARED=0 fallback) is exempt
        // from LMForge TTL eviction: the shared-server architecture means oMLX
        // owns LRU/TTL internally. Under ProcessPool this causes slot
        // accumulation, but that is acceptable for a debug/fallback path only.
        // SharedServerResidency (the default) handles oMLX lifecycle correctly.
        let now = keepalive::now_secs();
        let mut to_evict = Vec::new();
        for (id, slot) in self.active_slots.iter() {
            if slot.keep_alive_secs > 0
                && (now.saturating_sub(slot.last_accessed) > slot.keep_alive_secs)
                && self.config.id != "omlx"
                && slot.inflight.load(Ordering::Relaxed) == 0
            {
                to_evict.push(id.clone());
            }
        }

        for id in to_evict {
            info!("TTL expired for {}, unloading...", id);
            if let Some(mut slot) = self.active_slots.remove(&id) {
                let _ = self.stop_slot(&mut slot).await;
                let mut state = self.state.write().await;
                state.running_models.remove(&id);
            }
        }

        // ── idle_secs refresh ─────────────────────────────────────────────────
        {
            let mut state = self.state.write().await;
            for (id, slot) in self.active_slots.iter() {
                if let Some(ms) = state.running_models.get_mut(id) {
                    ms.idle_secs = now.saturating_sub(slot.last_accessed);
                }
            }

            // Evict stale last_errors entries to avoid noise from one-off
            // cold-load failures that never recover.
            let ttl = crate::engine::manager::last_error_ttl_secs();
            if ttl > 0 && !state.last_errors.is_empty() {
                let now_ts = chrono::Utc::now().timestamp();
                state.last_errors.retain(|_, e| {
                    match chrono::DateTime::parse_from_rfc3339(&e.at) {
                        Ok(at) => now_ts - at.timestamp() < ttl,
                        Err(_) => true,
                    }
                });
            }
        }
        self.notify().await;
    }

    fn state(&self) -> Arc<RwLock<EngineState>> {
        Arc::clone(&self.state)
    }
}

// ── OS-level orphan helpers ───────────────────────────────────────────────────

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
            let _ = crate::util::subprocess::hidden("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
        let _ = std::fs::remove_file(pid_file);
    }
}

/// Use `lsof -ti :PORT` to find any process holding a port and send SIGKILL.
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
