use anyhow::{Result, bail};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, EngineAdapterInstance};
use crate::engine::registry::EngineConfig;
use crate::engine::keepalive;

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
}

/// Shared engine state accessible from API handlers
#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineState {
    pub overall_status: EngineStatus,
    pub engine_id: String,
    pub engine_version: String,
    pub running_models: std::collections::HashMap<String, ModelSlot>,
    pub metrics: EngineMetrics,
}

pub struct ActiveSlot {
    pub engine: ActiveEngine,
    pub port: u16,
    pub last_accessed: u64,
    pub keep_alive_secs: u64,
    pub size_bytes: u64,
    pub status: EngineStatus,
}

/// The engine manager — spawns, supervises, health-checks, and restarts the engine via Adapters
pub struct EngineManager {
    pub config: EngineConfig,
    adapter: EngineAdapterInstance,
    base_engine_port: u16,
    data_dir: PathBuf,
    logs_dir: PathBuf,
    pub state: Arc<RwLock<EngineState>>,
    max_restarts: u32,
    health_interval_secs: u64,
    active_slots: std::collections::HashMap<String, ActiveSlot>,
    global_keep_alive: String,
    max_loaded_models: u32,
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
    pub fn new(
        config: EngineConfig,
        adapter: EngineAdapterInstance,
        base_engine_port: u16,
        data_dir: PathBuf,
        global_keep_alive: String,
        max_loaded_models: u32,
    ) -> Self {
        let logs_dir = data_dir.join("logs");
        let state = Arc::new(RwLock::new(EngineState {
            overall_status: EngineStatus::Ready,
            engine_id: config.id.clone(),
            engine_version: config.version.clone(),
            running_models: std::collections::HashMap::new(),
            metrics: EngineMetrics::default(),
        }));

        Self {
            config,
            adapter,
            base_engine_port,
            data_dir,
            logs_dir,
            state,
            max_restarts: 3,
            health_interval_secs: 2,
            active_slots: std::collections::HashMap::new(),
            global_keep_alive,
            max_loaded_models,
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
        let pid_file = self.data_dir.join("engines").join(format!("{}_{}.pid", self.config.id, active.port));
        let _ = std::fs::remove_file(pid_file);
        Ok(())
    }

    /// Evict least recently used models until needed VRAM is free
    async fn evict_for_vram(&mut self, needed_vram_gb: f32) -> Result<()> {
        let profile = crate::hardware::probe::detect_platform().unwrap_or_else(|_| crate::hardware::probe::HardwareProfile {
            os: crate::hardware::probe::Os::Unknown, arch: crate::hardware::probe::Arch::Unknown, is_tegra: false, gpu_vendor: crate::hardware::probe::GpuVendor::None, vram_gb: 0.0, unified_mem: false, total_ram_gb: 0.0, cpu_cores: 0, cpu_model: String::new()
        });

        loop {
            let free_vram = crate::hardware::vram::get_free_vram(&profile);
            if free_vram >= needed_vram_gb || self.active_slots.is_empty() {
                break;
            }

            // Find oldest accessed
            if let Some((oldest_id, _)) = self.active_slots.iter()
                .min_by_key(|(_, slot)| slot.last_accessed)
                .map(|(k, v)| (k.clone(), v.last_accessed)) {
                
                info!("VRAM starved (free: {:.2}GB, need: {:.2}GB). Evicting LRU model: {}", free_vram, needed_vram_gb, oldest_id);
                if let Some(mut slot) = self.active_slots.remove(&oldest_id) {
                    let _ = self.stop_slot(&mut slot).await;
                    self.state.write().await.running_models.remove(&oldest_id);
                }
            }
        }
        Ok(())
    }

    /// Dynamically spawn an adapter process for a model
    async fn spawn_adapter_process(&self, model_id: &str, port: u16) -> Result<ActiveEngine> {
        let engine_pid_file = self.data_dir.join("engines").join(format!("{}_{}.pid", self.config.id, port));
        if tokio::net::TcpListener::bind(("127.0.0.1", port)).await.is_err() {
            warn!("Port {} is held — attempting orphan engine cleanup", port);
            kill_orphan_engine(&engine_pid_file);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        let mut active = self.adapter.start(model_id, port, &self.data_dir, &self.logs_dir).await?;

        if let Some(pid) = active.process.id() {
            let _ = std::fs::write(&engine_pid_file, pid.to_string());
        }
        Ok(active)
    }

    /// Wait for health check of a dynamically assigned port
    async fn wait_slot_health(&self, port: u16) -> Result<()> {
        let health_url = format!("http://127.0.0.1:{}{}", port, self.config.health_endpoint);
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)).build()?;
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > std::time::Duration::from_secs(120) {
                bail!("Engine Adapter failed health verify on port {}", port);
            }
            if let Ok(resp) = client.get(&health_url).send().await {
                if resp.status().is_success() { return Ok(()); }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    /// Get next available port
    fn allocate_port(&self) -> u16 {
        let mut port = self.base_engine_port;
        let mut used_ports: std::collections::HashSet<u16> = self.active_slots.values().map(|s| s.port).collect();
        while used_ports.contains(&port) { port += 1; }
        port
    }

    /// Process ensure model logic
    async fn handle_ensure_model(&mut self, model_id: &str, keep_alive_override: &Option<String>) -> Result<u16> {
        let now = keepalive::now_secs();
        
        let keep_alive_secs = if let Some(ov) = keep_alive_override {
            keepalive::parse_keepalive(ov)
        } else {
            keepalive::parse_keepalive(&self.global_keep_alive)
        };

        if let Some(slot) = self.active_slots.get_mut(model_id) {
            slot.last_accessed = now;
            slot.keep_alive_secs = keep_alive_secs;
            return Ok(slot.port);
        }

        info!("Cold load request for model: {}", model_id);

        let index = crate::model::index::ModelIndex::load(&self.data_dir)
            .unwrap_or_else(|_| crate::model::index::ModelIndex { schema_version: 1, models: vec![] });
        let size_bytes = index.get(model_id).map(|m| m.size_bytes).unwrap_or(0);
        let needed_vram_gb = crate::hardware::vram::estimate_model_vram(size_bytes);

        self.evict_for_vram(needed_vram_gb).await?;

        if self.max_loaded_models > 0 && self.active_slots.len() >= self.max_loaded_models as usize {
            if let Some((oldest_id, _)) = self.active_slots.iter()
                .min_by_key(|(_, slot)| slot.last_accessed)
                .map(|(k, v)| (k.clone(), v.last_accessed)) {
                if let Some(mut slot) = self.active_slots.remove(&oldest_id) {
                    let _ = self.stop_slot(&mut slot).await;
                    self.state.write().await.running_models.remove(&oldest_id);
                }
            }
        }

        let port = self.allocate_port();
        
        let mut state = self.state.write().await;
        state.running_models.insert(model_id.to_string(), ModelSlot {
            model_id: model_id.to_string(), port, status: EngineStatus::Starting, idle_secs: 0, vram_est_gb: needed_vram_gb
        });
        drop(state);

        let engine = self.spawn_adapter_process(model_id, port).await?;
        self.wait_slot_health(port).await?;

        self.active_slots.insert(model_id.to_string(), ActiveSlot {
            engine, port, last_accessed: keepalive::now_secs(), keep_alive_secs, size_bytes, status: EngineStatus::Ready
        });

        let mut state = self.state.write().await;
        if let Some(slot) = state.running_models.get_mut(model_id) {
            slot.status = EngineStatus::Ready;
        }

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
                            }
                        }
                        Some(ManagerCommand::UnloadAll) => {
                            for (_, mut slot) in self.active_slots.drain() {
                                let _ = self.stop_slot(&mut slot).await;
                            }
                            self.state.write().await.running_models.clear();
                        }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(self.health_interval_secs)) => {
                    // Check TTL
                    let now = keepalive::now_secs();
                    let mut to_evict = Vec::new();
                    for (id, slot) in self.active_slots.iter() {
                        if slot.keep_alive_secs > 0 && (now.saturating_sub(slot.last_accessed) > slot.keep_alive_secs) {
                            if self.config.id != "omlx" {
                                to_evict.push(id.clone());
                            }
                        }
                    }

                    for id in to_evict {
                        info!("TTL expired for {}, unloading...", id);
                        if let Some(mut slot) = self.active_slots.remove(&id) {
                            let _ = self.stop_slot(&mut slot).await;
                            self.state.write().await.running_models.remove(&id);
                        }
                    }

                    // Sync State Update
                    let mut state = self.state.write().await;
                    for (id, slot) in self.active_slots.iter() {
                        if let Some(public_slot) = state.running_models.get_mut(id) {
                            public_slot.idle_secs = now.saturating_sub(slot.last_accessed);
                        }
                    }
                }
            }
        }
    }
}

fn kill_orphan_engine(pid_file: &std::path::Path) {
    if let Ok(content) = std::fs::read_to_string(pid_file) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
            }
            let _ = std::fs::remove_file(pid_file);
        }
    }
}
