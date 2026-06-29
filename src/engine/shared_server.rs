use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::engine::keepalive;
use crate::engine::manager::{
    EngineMetrics, EngineState, EngineStatus, ModelHandle, ModelSlot,
};
use crate::engine::registry::EngineConfig;
use crate::engine::residency::{Residency, ResidencyKind};

// ── Per-model tracking ────────────────────────────────────────────────────────

/// LMForge-side tracking for a model the shared server has served.
///
/// Only models that LMForge has routed a request through (via `ensure_model`)
/// appear here.  oMLX may have LRU-evicted a model from memory without telling
/// us; `heartbeat_tick` uses `loaded_count` from oMLX's `/health` endpoint to
/// reconcile and remove stale entries from the UI.
#[derive(Clone)]
struct TrackedModel {
    /// Unix epoch seconds when the last `ensure_model` call was made for this model.
    last_accessed: u64,
    /// Byte-size of the model weights from the model index. Used to estimate VRAM.
    size_bytes: u64,
}

// ── oMLX health response (partial) ───────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct OmlxHealth {
    #[serde(default)]
    engine_pool: OmlxEnginePool,
}

#[derive(serde::Deserialize, Default)]
struct OmlxEnginePool {
    /// How many models are currently hot in oMLX's Metal memory.
    #[serde(default)]
    loaded_count: usize,
    /// Total bytes of model weight memory currently held by oMLX.
    #[serde(default)]
    current_model_memory: u64,
}

// ── SharedServerResidency ─────────────────────────────────────────────────────

/// Shared-server residency strategy for oMLX.
///
/// One long-lived `omlx serve --model-dir <models_dir>` process started lazily
/// on `base_port`.  All model requests go to the same port; oMLX routes by the
/// `model` field and owns LRU/TTL/eviction natively.
///
/// ## What appears in `/lf/status` running_models
/// Only models that LMForge has actually served (via `ensure_model`) appear.
/// All 14 discovered models are never shown as active — discovery ≠ loaded.
/// The `heartbeat_tick` reconciles:
///   - If oMLX reports `loaded_count == 0`, all models are cleared.
///   - If oMLX reports fewer loaded models than we track, we TTL-evict the
///     oldest tracked models to match, assuming oMLX's LRU has reclaimed them.
///   - Models not accessed for `keep_alive_secs` are also removed.
///
/// ## VRAM display
/// `vram_est_gb` is set from `estimate_model_vram(size_bytes)` at serve time —
/// the same analytic prior used by `ProcessPoolResidency` for budgeting.
///
/// ## Pull → discovery
/// oMLX discovers model subdirectories once at startup. Newly-pulled models
/// require a server restart. `ensure_model` restarts automatically when a model
/// dir exists on disk but is absent from oMLX's `/v1/models`.
pub struct SharedServerResidency {
    pub(crate) config: EngineConfig,
    /// Parent directory passed to oMLX as `--model-dir`.
    pub(crate) models_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) logs_dir: PathBuf,
    /// The single fixed port this server listens on.
    pub(crate) port: u16,
    pub(crate) state: Arc<RwLock<EngineState>>,
    pub(crate) status_tx: tokio::sync::broadcast::Sender<EngineState>,
    /// The running `omlx serve` process. `None` until the first `ensure_model`.
    process: Option<tokio::process::Child>,
    http: reqwest::Client,
    executable: String,
    /// LMForge-side tracking for models we have served.
    /// keyed by LMForge model_id (may differ from oMLX subdir name for
    /// non-ascii models, but in practice they match).
    tracked: HashMap<String, TrackedModel>,
    /// How long to keep a model in LMForge's status view after its last
    /// request, even if oMLX hasn't evicted it yet.  Mirrors the global
    /// keep_alive setting; defaults to the same default as the orchestrator.
    keep_alive_secs: u64,
}

impl SharedServerResidency {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: EngineConfig,
        models_dir: PathBuf,
        data_dir: PathBuf,
        port: u16,
        executable: String,
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
            dismissed_errors: std::collections::HashMap::new(),
        }));
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("reqwest client");
        Self {
            config,
            models_dir,
            data_dir,
            logs_dir,
            port,
            state,
            status_tx,
            process: None,
            http,
            executable,
            tracked: HashMap::new(),
            keep_alive_secs: keepalive::parse_keepalive("5m"),
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn health_url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.config.health_endpoint)
    }

    fn models_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/models", self.port)
    }

    async fn is_healthy(&self) -> bool {
        self.http
            .get(&self.health_url())
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Fetch oMLX's pool status (loaded_count, current_model_memory).
    async fn fetch_health(&self) -> OmlxEnginePool {
        let Ok(resp) = self.http.get(&self.health_url()).send().await else {
            return OmlxEnginePool::default();
        };
        resp.json::<OmlxHealth>()
            .await
            .map(|h| h.engine_pool)
            .unwrap_or_default()
    }

    /// Start `omlx serve --model-dir <models_dir> --port <port>` and wait for health.
    async fn spawn_server(&mut self) -> Result<()> {
        let pid_file = self
            .data_dir
            .join("engines")
            .join(format!("{}_shared.pid", self.config.id));

        let stdout_file =
            crate::logging::rotation::prepare_engine_log(&self.logs_dir, "omlx-shared", "stdout")?;
        let stderr_file =
            crate::logging::rotation::prepare_engine_log(&self.logs_dir, "omlx-shared", "stderr")?;

        info!(
            port = self.port,
            models_dir = %self.models_dir.display(),
            "Starting shared oMLX server"
        );

        let child = crate::util::subprocess::hidden_tokio(&self.executable)
            .args([
                "serve",
                "--port",
                &self.port.to_string(),
                "--model-dir",
                &self.models_dir.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn omlx serve: {e}"))?;

        if let Some(pid) = child.id() {
            let _ = std::fs::create_dir_all(pid_file.parent().unwrap());
            let _ = std::fs::write(&pid_file, pid.to_string());
        }
        self.process = Some(child);

        // Wait for health (up to 30s — oMLX starts fast, ~1s in spike).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if std::time::Instant::now() > deadline {
                bail!("oMLX shared server did not become healthy within 30s");
            }
            if let Some(p) = self.process.as_mut()
                && let Ok(Some(status)) = p.try_wait()
            {
                bail!(
                    "oMLX shared server process exited before health-check passed (exit={:?})",
                    status.code()
                );
            }
            if self.is_healthy().await {
                info!(port = self.port, "oMLX shared server is healthy");
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Ensure the shared server is up and healthy. Lazily spawns if needed.
    async fn ensure_server(&mut self) -> Result<()> {
        if self.process.is_some() && self.is_healthy().await {
            return Ok(());
        }
        if self.process.is_some() {
            warn!("oMLX shared server is not responding — restarting");
            self.kill_process().await;
        }
        self.spawn_server().await
    }

    async fn kill_process(&mut self) {
        if let Some(mut p) = self.process.take() {
            #[cfg(unix)]
            if let Some(pid) = p.id() {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = p.kill().await;
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), p.wait()).await;
        }
    }

    /// Fetch the model list from oMLX's `/v1/models` endpoint.
    async fn fetch_omlx_models(&self) -> Vec<String> {
        let Ok(resp) = self.http.get(&self.models_url()).send().await else {
            return vec![];
        };
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            return vec![];
        };
        body["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn notify(&self) {
        let snapshot = self.state.read().await.clone();
        let _ = self.status_tx.send(snapshot);
    }

    /// Upsert a served model into `running_models` with an accurate VRAM estimate.
    fn model_slot_for(model_id: &str, port: u16, size_bytes: u64) -> ModelSlot {
        ModelSlot {
            model_id: model_id.to_string(),
            port,
            status: EngineStatus::Ready,
            idle_secs: 0,
            vram_est_gb: crate::hardware::vram::estimate_model_vram(size_bytes),
            spec_mode: crate::engine::speculative::SpecMode::Off,
            spec_stats: None,
        }
    }

    /// Reconcile LMForge's `running_models` with oMLX's actual memory state.
    ///
    /// Strategy:
    /// 1. **TTL sweep**: remove models not accessed for `keep_alive_secs`.
    /// 2. **loaded_count reconciliation**: if oMLX reports fewer loaded models
    ///    than we track, assume LRU evicted the oldest — drop from the bottom.
    /// 3. **All-clear**: if `loaded_count == 0`, clear everything.
    /// 4. **idle_secs refresh**: update `idle_secs` for remaining entries.
    async fn reconcile(&mut self, pool: &OmlxEnginePool) {
        let now = keepalive::now_secs();

        // ── 1. TTL sweep ──────────────────────────────────────────────────────
        let ttl = self.keep_alive_secs;
        self.tracked.retain(|_, t| {
            ttl == 0 || now.saturating_sub(t.last_accessed) <= ttl
        });

        // ── 2 & 3. loaded_count reconciliation ───────────────────────────────
        if pool.loaded_count == 0 {
            self.tracked.clear();
        } else if pool.loaded_count < self.tracked.len() {
            // oMLX has evicted some models. We don't know which ones, so drop
            // the oldest (by last_accessed) until our count matches.
            let excess = self.tracked.len() - pool.loaded_count;
            let mut by_age: Vec<(String, u64)> = self
                .tracked
                .iter()
                .map(|(id, t)| (id.clone(), t.last_accessed))
                .collect();
            by_age.sort_by_key(|(_, ts)| *ts);
            for (id, _) in by_age.into_iter().take(excess) {
                self.tracked.remove(&id);
            }
        }

        // ── 4. Sync running_models to match tracked ───────────────────────────
        let mut state = self.state.write().await;
        state.running_models.retain(|id, _| self.tracked.contains_key(id));
        for (model_id, t) in &self.tracked {
            let idle_secs = now.saturating_sub(t.last_accessed);
            let slot = state
                .running_models
                .entry(model_id.clone())
                .or_insert_with(|| Self::model_slot_for(model_id, self.port, t.size_bytes));
            slot.idle_secs = idle_secs;
        }
    }
}

// ── Residency impl ────────────────────────────────────────────────────────────

impl Residency for SharedServerResidency {
    fn kind(&self) -> ResidencyKind {
        ResidencyKind::SharedServer
    }

    /// Ensure the model can be served: server must be healthy and the model
    /// subdir must exist on disk (pulled). Returns a handle pointing to the
    /// single shared port.
    ///
    /// If the model dir is present but absent from oMLX's `/v1/models`
    /// (pulled while the server was running), the server is restarted so oMLX
    /// rescans and discovers the new subdir.
    async fn ensure_model(
        &mut self,
        model_id: &str,
        keep_alive_override: &Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle> {
        let index = crate::model::index::ModelIndex::load(&self.data_dir, &self.models_dir)
            .unwrap_or(crate::model::index::ModelIndex {
                schema_version: 1,
                models: vec![],
            });

        // Verify the model has been pulled (subdir on disk).
        let model_dir = index
            .get(model_id)
            .map(|m| PathBuf::from(&m.path))
            .unwrap_or_else(|| self.models_dir.join(model_id));

        if !model_dir.exists() {
            bail!(
                "{}",
                crate::engine::adapter::EngineLoadError::NotMaterialized(format!(
                    "Model directory not found: {}. Pull the model first with: lmforge pull {}",
                    model_dir.display(),
                    model_id
                ))
            );
        }

        let size_bytes = index.get(model_id).map(|m| m.size_bytes).unwrap_or(0);

        // Derive the subdir name that oMLX uses as the model ID.
        let omlx_model_id = model_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| model_id.to_string());

        self.ensure_server().await?;

        // Check if oMLX already knows the model. If not, the model was likely
        // pulled while the server was already running — restart to trigger rescan.
        let known = self.fetch_omlx_models().await;
        if !known.contains(&omlx_model_id) {
            info!(
                model_id,
                omlx_model_id,
                "Model not yet in oMLX discovery list — restarting server to rescan"
            );
            self.kill_process().await;
            self.spawn_server().await?;
        }

        // Apply keep_alive override if provided.
        if let Some(ov) = keep_alive_override {
            let secs = keepalive::parse_keepalive(ov);
            if secs > 0 {
                self.keep_alive_secs = secs;
            }
        }

        // ── Track this model ──────────────────────────────────────────────────
        let now = keepalive::now_secs();
        self.tracked
            .entry(model_id.to_string())
            .and_modify(|t| t.last_accessed = now)
            .or_insert(TrackedModel {
                last_accessed: now,
                size_bytes,
            });

        // ── Upsert running_models with accurate VRAM estimate ─────────────────
        {
            let mut state = self.state.write().await;
            let slot = state
                .running_models
                .entry(model_id.to_string())
                .or_insert_with(|| Self::model_slot_for(model_id, self.port, size_bytes));
            // Refresh vram_est_gb in case we now have size_bytes we didn't before.
            if slot.vram_est_gb == 0.0 && size_bytes > 0 {
                slot.vram_est_gb = crate::hardware::vram::estimate_model_vram(size_bytes);
            }
            slot.idle_secs = 0;
            state.clear_error(model_id);
        }
        crate::server::metrics::set_active_models(self.tracked.len() as u64);
        self.notify().await;

        // Each request gets its own inflight counter (uniform InflightGuard contract).
        let inflight = Arc::new(AtomicU32::new(0));
        if for_request {
            inflight.fetch_add(1, Ordering::Relaxed);
        }
        Ok(ModelHandle {
            port: self.port,
            inflight,
        })
    }

    /// Advisory unload: oMLX manages its own memory. Removes the model from
    /// LMForge's tracking and status view immediately.
    async fn unload_model(&mut self, model_id: &str) {
        warn!(
            model_id,
            "unload_model called on SharedServerResidency — oMLX manages its own memory; \
             removing from LMForge status view (oMLX will evict under memory pressure)"
        );
        self.tracked.remove(model_id);
        self.state.write().await.running_models.remove(model_id);
        crate::server::metrics::set_active_models(self.tracked.len() as u64);
        self.notify().await;
    }

    /// Kill the oMLX server process. Clears all tracking. The next
    /// `ensure_model` will restart it with fresh model discovery.
    async fn unload_all(&mut self) {
        info!("Stopping shared oMLX server (unload_all)");
        self.kill_process().await;
        self.tracked.clear();
        self.state.write().await.running_models.clear();
        crate::server::metrics::set_active_models(0);
        self.notify().await;
    }

    /// Periodic heartbeat: crash detection + oMLX-aware TTL reconciliation.
    async fn heartbeat_tick(&mut self) {
        // ── Crash detection ───────────────────────────────────────────────────
        if let Some(p) = self.process.as_mut() {
            if let Ok(Some(status)) = p.try_wait() {
                warn!(
                    ?status,
                    "oMLX shared server exited unexpectedly — clearing state, \
                     will restart on next request"
                );
                self.process = None;
                self.tracked.clear();
                self.state.write().await.running_models.clear();
                crate::server::metrics::set_active_models(0);
                self.notify().await;
                return;
            }
        }

        // ── Reconcile with oMLX health ────────────────────────────────────────
        if self.process.is_some() && !self.tracked.is_empty() {
            let pool = self.fetch_health().await;
            self.reconcile(&pool).await;
            crate::server::metrics::set_active_models(self.tracked.len() as u64);
        }
        self.notify().await;
    }

    fn state(&self) -> Arc<RwLock<EngineState>> {
        Arc::clone(&self.state)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::engine::residency::ResidencyKind;

    use super::*;

    fn make_residency() -> SharedServerResidency {
        let (tx, _rx) = tokio::sync::broadcast::channel(1);
        let config = crate::engine::registry::EngineConfig {
            id: "omlx".into(),
            version: "0.4.4".into(),
            health_endpoint: "/health".into(),
            ..Default::default()
        };
        SharedServerResidency::new(
            config,
            PathBuf::from("/tmp/lmforge_test_models"),
            PathBuf::from("/tmp/lmforge_test_data"),
            19998,
            "omlx".into(),
            tx,
        )
    }

    #[test]
    fn kind_is_shared_server() {
        let r = make_residency();
        assert_eq!(r.kind(), ResidencyKind::SharedServer);
    }

    #[test]
    fn state_starts_empty() {
        let r = make_residency();
        let state = r.state();
        let guard = state.blocking_read();
        assert!(guard.running_models.is_empty());
        assert_eq!(guard.overall_status, EngineStatus::Ready);
    }

    #[test]
    fn tracked_starts_empty() {
        let r = make_residency();
        assert!(r.tracked.is_empty());
    }

    #[tokio::test]
    async fn unload_all_clears_state_and_tracked() {
        let mut r = make_residency();
        r.tracked.insert(
            "test-model".into(),
            TrackedModel {
                last_accessed: 100,
                size_bytes: 1_000_000,
            },
        );
        r.state.write().await.running_models.insert(
            "test-model".into(),
            ModelSlot {
                model_id: "test-model".into(),
                port: 19998,
                status: EngineStatus::Ready,
                idle_secs: 0,
                vram_est_gb: 1.2,
                spec_mode: crate::engine::speculative::SpecMode::Off,
                spec_stats: None,
            },
        );
        r.unload_all().await;
        assert!(r.tracked.is_empty());
        assert!(r.state.read().await.running_models.is_empty());
    }

    #[tokio::test]
    async fn unload_model_removes_from_tracked_and_state() {
        let mut r = make_residency();
        r.tracked.insert(
            "m1".into(),
            TrackedModel {
                last_accessed: 100,
                size_bytes: 0,
            },
        );
        r.state.write().await.running_models.insert(
            "m1".into(),
            ModelSlot {
                model_id: "m1".into(),
                port: 19998,
                status: EngineStatus::Ready,
                idle_secs: 0,
                vram_est_gb: 0.0,
                spec_mode: crate::engine::speculative::SpecMode::Off,
                spec_stats: None,
            },
        );
        r.unload_model("m1").await;
        assert!(!r.tracked.contains_key("m1"));
        assert!(!r.state.read().await.running_models.contains_key("m1"));
    }

    #[tokio::test]
    async fn reconcile_clears_all_when_loaded_count_zero() {
        let mut r = make_residency();
        let now = keepalive::now_secs();
        // Seed 2 tracked models.
        r.tracked.insert("a".into(), TrackedModel { last_accessed: now, size_bytes: 0 });
        r.tracked.insert("b".into(), TrackedModel { last_accessed: now, size_bytes: 0 });
        r.state.write().await.running_models.insert("a".into(), ModelSlot {
            model_id: "a".into(), port: 19998, status: EngineStatus::Ready,
            idle_secs: 0, vram_est_gb: 0.0,
            spec_mode: crate::engine::speculative::SpecMode::Off, spec_stats: None,
        });
        r.state.write().await.running_models.insert("b".into(), ModelSlot {
            model_id: "b".into(), port: 19998, status: EngineStatus::Ready,
            idle_secs: 0, vram_est_gb: 0.0,
            spec_mode: crate::engine::speculative::SpecMode::Off, spec_stats: None,
        });
        let pool = OmlxEnginePool { loaded_count: 0, current_model_memory: 0 };
        r.reconcile(&pool).await;
        assert!(r.tracked.is_empty(), "all tracked cleared on loaded_count=0");
        assert!(r.state.read().await.running_models.is_empty());
    }

    #[tokio::test]
    async fn reconcile_drops_oldest_on_lru_eviction() {
        let mut r = make_residency();
        r.keep_alive_secs = 0; // disable TTL so only loaded_count logic runs
        let now = keepalive::now_secs();
        // 3 tracked, oMLX reports only 1 loaded.
        r.tracked.insert("old".into(), TrackedModel { last_accessed: now - 30, size_bytes: 0 });
        r.tracked.insert("mid".into(), TrackedModel { last_accessed: now - 20, size_bytes: 0 });
        r.tracked.insert("new".into(), TrackedModel { last_accessed: now - 10, size_bytes: 0 });
        let pool = OmlxEnginePool { loaded_count: 1, current_model_memory: 0 };
        r.reconcile(&pool).await;
        assert_eq!(r.tracked.len(), 1, "2 oldest evicted");
        assert!(r.tracked.contains_key("new"), "newest retained");
    }

    #[tokio::test]
    async fn reconcile_ttl_evicts_stale() {
        let mut r = make_residency();
        r.keep_alive_secs = 60; // 1 minute TTL
        let now = keepalive::now_secs();
        // "stale" was last accessed 2 minutes ago — beyond TTL
        r.tracked.insert("stale".into(), TrackedModel { last_accessed: now - 120, size_bytes: 0 });
        r.tracked.insert("fresh".into(), TrackedModel { last_accessed: now, size_bytes: 0 });
        let pool = OmlxEnginePool { loaded_count: 10, current_model_memory: 0 };
        r.reconcile(&pool).await;
        assert!(!r.tracked.contains_key("stale"), "stale model evicted by TTL");
        assert!(r.tracked.contains_key("fresh"), "fresh model retained");
    }

    #[test]
    fn model_slot_vram_nonzero_for_nonzero_size() {
        let slot = SharedServerResidency::model_slot_for("m", 19998, 4_000_000_000);
        assert!(slot.vram_est_gb > 0.0, "VRAM estimate should be nonzero");
    }
}
