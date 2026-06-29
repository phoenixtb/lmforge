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

/// Bytes per GiB — used to convert oMLX's byte sizes to the GB float the UI expects.
const BYTES_PER_GIB: f32 = 1_073_741_824.0;

// ── oMLX /v1/models/status response types ────────────────────────────────────

/// Full response from oMLX's public `/v1/models/status` endpoint.
/// This is the ground-truth source for per-model load state.
#[derive(serde::Deserialize)]
struct OmlxModelsStatus {
    models: Vec<OmlxModelEntry>,
    #[serde(default)]
    current_model_memory: u64,
}

/// One entry from `/v1/models/status`.  Fields relevant to LMForge:
/// - `loaded`      — true only when the model is hot in Metal memory
/// - `actual_size` — real bytes once loaded (null when not loaded)
/// - `estimated_size` — pre-load estimate; used for VRAM display before load
/// - `last_access` — oMLX-native unix float; drives idle_secs display
/// - `is_loading`  — model is currently being paged in
#[derive(serde::Deserialize)]
struct OmlxModelEntry {
    id: String,
    loaded: bool,
    #[serde(default)]
    is_loading: bool,
    #[serde(default)]
    estimated_size: u64,
    actual_size: Option<u64>,
    /// oMLX reports this as a float (e.g. 1782766434.02605).
    last_access: Option<f64>,
}

// ── SharedServerResidency ─────────────────────────────────────────────────────

/// Shared-server residency strategy for oMLX.
///
/// One long-lived `omlx serve --model-dir <models_dir>` process started lazily
/// on `base_port`.  All model requests go to the same port; oMLX routes by the
/// `model` field and owns LRU/TTL/eviction natively.
///
/// ## State sync — real, not approximated
/// oMLX exposes `/v1/models/status` (public, no auth) with per-model:
///   - `loaded: bool`      — whether the model is hot in Metal memory right now
///   - `actual_size`       — real bytes in memory (available once loaded)
///   - `estimated_size`    — pre-load byte estimate
///   - `last_access`       — oMLX-native unix float; used for idle_secs
///   - `is_loading`        — model currently being paged in
///
/// `heartbeat_tick` calls this endpoint and directly mirrors the state into
/// `running_models`.  Only `loaded == true` models appear in the UI panel.
/// `idle_secs` is derived from oMLX's own `last_access`, not from LMForge's
/// request timestamps.  VRAM is `actual_size` (real) or `estimated_size`
/// (pre-load) from oMLX — not our generic analytic estimate.
///
/// ## ID mapping
/// oMLX identifies models by their subdirectory name under `--model-dir`.
/// LMForge uses a `:` delimited ID (e.g. `qwen3:1.7b:4bit`).  We build a
/// reverse map at sync time from the model index (path last-component → id).
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
    /// Cached reverse map: oMLX model ID (subdir name) → LMForge model ID.
    /// Built once in `new()` and refreshed after every `spawn_server()` call
    /// (server restarts trigger oMLX to rescan, which may surface new models).
    reverse_map: HashMap<String, String>,
}

impl SharedServerResidency {
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
        let reverse_map = Self::build_reverse_map_from(&data_dir, &models_dir);
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
            reverse_map,
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn health_url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.config.health_endpoint)
    }

    fn models_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/models", self.port)
    }

    fn models_status_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/models/status", self.port)
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
                // Rebuild reverse map: new server scan may have surfaced new model subdirs.
                self.reverse_map =
                    Self::build_reverse_map_from(&self.data_dir, &self.models_dir);
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
            // Process handle exists but health check failed — crashed or hung.
            warn!("oMLX shared server is not responding — restarting");
            self.kill_process().await;
        }
        // Either never started or just killed above.
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

    /// Fetch the model list from oMLX's `/v1/models` endpoint (discovery list).
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

    /// Build a reverse map: oMLX model ID (subdir name) → LMForge model ID.
    ///
    /// oMLX uses the last path component of each model dir as its model ID.
    /// The LMForge model index stores both the LMForge ID and the full `path`.
    /// This is a pure function; callers cache the result in `self.reverse_map`
    /// and only rebuild it after a server restart (new models may be discovered).
    fn build_reverse_map_from(data_dir: &PathBuf, models_dir: &PathBuf) -> HashMap<String, String> {
        let index = crate::model::index::ModelIndex::load(data_dir, models_dir)
            .unwrap_or(crate::model::index::ModelIndex {
                schema_version: 1,
                models: vec![],
            });
        index
            .models
            .into_iter()
            .filter_map(|m| {
                let omlx_id = PathBuf::from(&m.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())?;
                Some((omlx_id, m.id))
            })
            .collect()
    }

    /// Sync `running_models` directly from oMLX's `/v1/models/status`.
    ///
    /// - Only models with `loaded == true` (or `is_loading == true`) appear.
    /// - `idle_secs` comes from oMLX's own `last_access` timestamp — real data.
    /// - `vram_est_gb` uses `actual_size` when loaded, `estimated_size` otherwise.
    /// - LMForge model IDs are resolved via the model index path → id map.
    async fn sync_from_omlx_status(&self) {
        let Ok(resp) = self.http.get(&self.models_status_url()).send().await else {
            return;
        };
        let Ok(status) = resp.json::<OmlxModelsStatus>().await else {
            return;
        };

        let now = keepalive::now_secs();

        let mut state = self.state.write().await;

        // ── Build the desired state from oMLX ground truth ────────────────────
        // Only models oMLX considers loaded (or loading) should appear.
        let mut desired: HashMap<String, ModelSlot> = HashMap::new();

        for entry in &status.models {
            if !entry.loaded && !entry.is_loading {
                continue;
            }
            // Map oMLX model ID → LMForge model ID (fall back to oMLX id if unmapped).
            let lmforge_id = self
                .reverse_map
                .get(&entry.id)
                .cloned()
                .unwrap_or_else(|| entry.id.clone());

            let vram_gb = entry
                .actual_size
                .unwrap_or(entry.estimated_size) as f32
                / BYTES_PER_GIB;

            let idle_secs = entry
                .last_access
                .map(|ts| now.saturating_sub(ts as u64))
                .unwrap_or(0);

            let engine_status = if entry.is_loading {
                EngineStatus::Starting
            } else {
                EngineStatus::Ready
            };

            desired.insert(
                lmforge_id.clone(),
                ModelSlot {
                    model_id: lmforge_id,
                    port: self.port,
                    status: engine_status,
                    idle_secs,
                    vram_est_gb: vram_gb,
                    spec_mode: crate::engine::speculative::SpecMode::Off,
                    spec_stats: None,
                },
            );
        }

        // ── Atomically replace running_models with ground truth ───────────────
        state.running_models = desired;
        crate::server::metrics::set_active_models(
            state.running_models.values().filter(|s| s.status == EngineStatus::Ready).count() as u64,
        );
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
    /// If the model dir is present but absent from oMLX's discovery list
    /// (pulled while the server was running), the server is restarted so oMLX
    /// rescans and discovers the new subdir.
    ///
    /// Note: oMLX loads models lazily on first inference request — the model
    /// will appear as `loaded` in the UI status after the first actual inference
    /// call, not at ensure_model time.
    async fn ensure_model(
        &mut self,
        model_id: &str,
        _keep_alive_override: &Option<String>,
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

        let omlx_model_id = model_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| model_id.to_string());

        self.ensure_server().await?;

        // If oMLX doesn't know this model yet (pulled while server was live),
        // restart to trigger a fresh subdir scan.
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

        // Add an optimistic "loading" slot so the UI shows something immediately.
        // The next heartbeat sync will replace it with real data from oMLX.
        {
            let mut state = self.state.write().await;
            state.running_models.entry(model_id.to_string()).or_insert_with(|| {
                let size_bytes = index.get(model_id).map(|m| m.size_bytes).unwrap_or(0);
                ModelSlot {
                    model_id: model_id.to_string(),
                    port: self.port,
                    status: EngineStatus::Starting,
                    idle_secs: 0,
                    vram_est_gb: size_bytes as f32 / BYTES_PER_GIB,
                    spec_mode: crate::engine::speculative::SpecMode::Off,
                    spec_stats: None,
                }
            });
            state.clear_error(model_id);
        }
        self.notify().await;

        let inflight = Arc::new(AtomicU32::new(0));
        if for_request {
            inflight.fetch_add(1, Ordering::Relaxed);
        }
        Ok(ModelHandle {
            port: self.port,
            inflight,
        })
    }

    /// Advisory unload: removes from LMForge's view immediately. oMLX will
    /// evict the model under memory pressure on its own schedule.
    async fn unload_model(&mut self, model_id: &str) {
        warn!(
            model_id,
            "unload_model called on SharedServerResidency — oMLX manages its own memory; \
             removing from LMForge status view"
        );
        self.state.write().await.running_models.remove(model_id);
        self.notify().await;
    }

    /// Kill the oMLX server process. Clears all state. The next `ensure_model`
    /// will restart it with fresh model discovery.
    async fn unload_all(&mut self) {
        info!("Stopping shared oMLX server (unload_all)");
        self.kill_process().await;
        self.state.write().await.running_models.clear();
        crate::server::metrics::set_active_models(0);
        self.notify().await;
    }

    /// Periodic heartbeat: crash detection + real state sync from oMLX.
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
                self.state.write().await.running_models.clear();
                crate::server::metrics::set_active_models(0);
                self.notify().await;
                return;
            }
        }

        // ── Real state sync from oMLX /v1/models/status ───────────────────────
        // This drives the UI truthfully: loaded_count, actual VRAM, real idle_secs.
        if self.process.is_some() {
            self.sync_from_omlx_status().await;
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
                vram_est_gb: 2.0,
                spec_mode: crate::engine::speculative::SpecMode::Off,
                spec_stats: None,
            },
        );
        r.unload_model("m1").await;
        assert!(!r.state.read().await.running_models.contains_key("m1"));
    }

    #[test]
    fn vram_gb_conversion_is_correct() {
        // 4 GiB = 4_294_967_296 bytes
        let vram = 4_294_967_296u64 as f32 / BYTES_PER_GIB;
        assert!((vram - 4.0).abs() < 0.01);
    }
}
