use anyhow::{Result, bail};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::engine::manager::{
    EngineMetrics, EngineState, EngineStatus, ModelHandle, ModelSlot,
};
use crate::engine::registry::EngineConfig;
use crate::engine::residency::{Residency, ResidencyKind};

/// Shared-server residency strategy for oMLX.
///
/// One long-lived `omlx serve --model-dir <models_dir>` process started
/// lazily on `base_port`. All model requests go to the same port; oMLX routes
/// by the `model` field and owns LRU/TTL/eviction natively.
///
/// Enabled via `LMFORGE_OMLX_SHARED=1`. This strategy is wired as the default
/// for oMLX in Phase 3 once the Mac e2e baseline confirms it.
///
/// ## Pull → discovery
/// oMLX discovers model subdirectories once at startup (see
/// `docs/dev/OMLX_SHARED_SERVER_FINDINGS.md`). Newly-pulled models require a
/// server restart to be served. `ensure_model` detects missing dirs and
/// surfaces a clear "not materialized" error; it also triggers a restart when
/// the model dir was pulled while the server was already running.
///
/// ## unload semantics
/// oMLX owns memory management; `unload_model` is advisory and has no effect
/// on what oMLX keeps in memory. `unload_all` kills the server process so the
/// next `ensure_model` triggers a fresh restart with updated model discovery.
pub struct SharedServerResidency {
    pub(crate) config: EngineConfig,
    /// Parent directory that oMLX receives as `--model-dir`.
    /// Subdirectory names are the model IDs that oMLX serves.
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
    /// The oMLX executable name (usually "omlx").
    executable: String,
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

    /// Start `omlx serve --model-dir <models_dir> --port <port>` and wait for health.
    async fn spawn_server(&mut self) -> Result<()> {
        // Write the PID file after spawn so a daemon restart can clean up.
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
            // Fast-fail if process already died.
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
        // Fast path: process is known alive and health passes.
        if self.process.is_some() && self.is_healthy().await {
            return Ok(());
        }
        // Process died or was never started.
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
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), p.wait()).await;
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

    /// Refresh `EngineState.running_models` from oMLX's `/v1/models` so that
    /// `/lf/status` reflects the models the shared server knows about.
    async fn sync_running_models(&self) {
        let omlx_models = self.fetch_omlx_models().await;
        let mut state = self.state.write().await;
        // Remove stale entries (models that disappeared from oMLX — unlikely
        // during a run, but keeps the snapshot consistent on restart).
        state
            .running_models
            .retain(|id, _| omlx_models.contains(id));
        // Upsert all discovered models as Ready slots on the shared port.
        for model_id in &omlx_models {
            state
                .running_models
                .entry(model_id.clone())
                .or_insert_with(|| ModelSlot {
                    model_id: model_id.clone(),
                    port: self.port,
                    status: EngineStatus::Ready,
                    idle_secs: 0,
                    vram_est_gb: 0.0,
                    spec_mode: crate::engine::speculative::SpecMode::Off,
                    spec_stats: None,
                });
        }
    }

    async fn notify(&self) {
        let snapshot = self.state.read().await.clone();
        let _ = self.status_tx.send(snapshot);
    }
}

// ── Residency impl ────────────────────────────────────────────────────────────

impl Residency for SharedServerResidency {
    fn kind(&self) -> ResidencyKind {
        ResidencyKind::SharedServer
    }

    /// Ensure the model can be served: server must be healthy and the model
    /// subdir must exist on disk (i.e. it was pulled). Returns a handle
    /// pointing to the single shared port.
    ///
    /// If the model dir is present but not yet in oMLX's `/v1/models` list
    /// (pulled while the server was already running), we restart the server
    /// so oMLX rescans and discovers the new subdir.
    async fn ensure_model(
        &mut self,
        model_id: &str,
        _keep_alive_override: &Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle> {
        // Map LMForge model ID to oMLX subdir name. The model index stores
        // the filesystem path; we derive oMLX's expected subdir name from it.
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

        // Derive the subdir name that oMLX uses as the model ID (last component).
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

        // Upsert the model in running_models so /lf/status shows it.
        {
            let mut state = self.state.write().await;
            state
                .running_models
                .entry(model_id.to_string())
                .or_insert_with(|| ModelSlot {
                    model_id: model_id.to_string(),
                    port: self.port,
                    status: EngineStatus::Ready,
                    idle_secs: 0,
                    vram_est_gb: 0.0,
                    spec_mode: crate::engine::speculative::SpecMode::Off,
                    spec_stats: None,
                });
            state.clear_error(model_id);
        }
        self.notify().await;

        // Each request gets its own inflight counter (uniform InflightGuard contract).
        // SharedServer never uses this for eviction (oMLX owns it) but callers
        // depend on the counter to protect the slot until their guard drops.
        let inflight = Arc::new(AtomicU32::new(0));
        if for_request {
            inflight.fetch_add(1, Ordering::Relaxed);
        }
        Ok(ModelHandle {
            port: self.port,
            inflight,
        })
    }

    /// Advisory unload: oMLX manages its own memory, so this is a no-op in
    /// SharedServer mode. Logs a warning so operators know the call was
    /// received but had no effect.
    async fn unload_model(&mut self, model_id: &str) {
        warn!(
            model_id,
            "unload_model called on SharedServerResidency — oMLX manages its own memory; \
             the model will be evicted by oMLX's native LRU when memory pressure requires it"
        );
        // Remove from our view (status display only).
        self.state.write().await.running_models.remove(model_id);
        self.notify().await;
    }

    /// Kill the oMLX server process. The next `ensure_model` will restart it
    /// with fresh model discovery.
    async fn unload_all(&mut self) {
        info!("Stopping shared oMLX server (unload_all)");
        self.kill_process().await;
        self.state.write().await.running_models.clear();
        crate::server::metrics::set_active_models(0);
        self.notify().await;
    }

    /// Periodic heartbeat: check server health, restart if dead, sync model list.
    async fn heartbeat_tick(&mut self) {
        // If the process has exited unexpectedly, clear state and let the next
        // ensure_model restart it lazily.
        if let Some(p) = self.process.as_mut() {
            if let Ok(Some(status)) = p.try_wait() {
                warn!(
                    ?status,
                    "oMLX shared server exited unexpectedly — will restart on next request"
                );
                self.process = None;
                self.state.write().await.running_models.clear();
                self.notify().await;
                return;
            }
        }

        // If the server is running, sync the model list from oMLX.
        if self.process.is_some() && self.is_healthy().await {
            self.sync_running_models().await;
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

    #[tokio::test]
    async fn unload_all_clears_state() {
        let mut r = make_residency();
        // Seed a model entry directly.
        r.state.write().await.running_models.insert(
            "test-model".into(),
            ModelSlot {
                model_id: "test-model".into(),
                port: 19998,
                status: EngineStatus::Ready,
                idle_secs: 0,
                vram_est_gb: 0.0,
                spec_mode: crate::engine::speculative::SpecMode::Off,
                spec_stats: None,
            },
        );
        r.unload_all().await;
        assert!(r.state.read().await.running_models.is_empty());
    }

    #[tokio::test]
    async fn unload_model_removes_from_state() {
        let mut r = make_residency();
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
        assert!(!r.state.read().await.running_models.contains_key("m1"));
    }
}
