use axum::body::Body;
use axum::http::{Response, StatusCode, header};
use axum::{extract::State, response::IntoResponse};
use serde::Serialize;

use super::AppState;

// ── Output types ──────────────────────────────────────────────────────────────

/// Live system telemetry returned by GET /lf/sysinfo (polled every 2 s).
#[derive(Debug, Serialize)]
pub struct SysStats {
    pub cpu_pct: f32,
    pub cpu_cores_pct: Vec<f32>,
    pub mem_total_gb: f32,
    /// System-wide used — ALL processes.  Matches macOS Activity Monitor.
    pub mem_used_gb: f32,
    pub mem_avail_gb: f32,
    pub mem_pct: f32,
    pub gpu: GpuStats,
    /// Measured RSS of each model-server child process (the real memory each model holds).
    pub model_procs: Vec<ModelProcMem>,
    /// Sum of model_procs RSS in GiB — quick access for the UI bar.
    pub model_rss_gb: f32,
}

/// Per-model-server memory reading from the OS process table.
#[derive(Debug, Serialize, Clone)]
pub struct ModelProcMem {
    /// The model_id string (from the slot map).  Used as the display label.
    pub model_id: String,
    /// Resident Set Size in MiB — physical RAM the process is currently holding.
    /// On Apple Silicon this is unified memory (CPU + GPU portion combined).
    pub rss_mb: f32,
}

#[derive(Debug, Serialize, Default)]
pub struct GpuStats {
    pub util_pct: Option<f32>,
    pub mem_used_mb: Option<f32>,
    pub mem_total_mb: Option<f32>,
    pub source: String,
    pub note: String,
}

// ── Raw shape parsed from the Swift probe's JSON ──────────────────────────────

#[derive(serde::Deserialize, Default)]
struct ProbeOutput {
    gpu_util_pct: Option<f64>,
    gpu_mem_used_mb: Option<f64>,
    gpu_mem_total_mb: Option<f64>,
    source: Option<String>,
}

// ── CPU + system memory sampling ──────────────────────────────────────────────

fn sample_sys() -> (f32, Vec<f32>, f32, f32, f32, f32) {
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, System};

    let mut sys = System::new();
    sys.refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage());
    std::thread::sleep(std::time::Duration::from_millis(100));
    sys.refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage());
    sys.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());

    let cpu_pct = sys.global_cpu_usage();
    let cores: Vec<f32> = sys.cpus().iter().take(32).map(|c| c.cpu_usage()).collect();
    let b2g = |b: u64| b as f32 / 1_073_741_824.0;
    let total = b2g(sys.total_memory());
    let used = b2g(sys.used_memory());
    let avail = b2g(sys.available_memory());
    let pct = if total > 0.0 {
        (used / total * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    (cpu_pct, cores, total, used, avail, pct)
}

// ── Model process memory (child-process RSS) ──────────────────────────────────

/// Read the RSS of every child process spawned by this daemon and pair it with
/// the model_id it is serving (looked up by port from the slot map).
///
/// Strategy: sysinfo gives us all processes including their parent PID.
/// We walk all processes whose PPID == our PID and match them to model slots
/// by port (the slot port is embedded in the process command-line).
///
/// If a process cannot be matched to a slot, it is still counted but shown as
/// "engine / other".
fn sample_model_procs(slot_info: &[(String, u16)]) -> Vec<ModelProcMem> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let my_pid = sysinfo::Pid::from_u32(std::process::id());

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_cmd(UpdateKind::Always),
    );

    let b2mb = |bytes: u64| bytes as f32 / 1_048_576.0;

    // Find all direct children of this process.
    let children: Vec<_> = sys
        .processes()
        .values()
        .filter(|p| p.parent() == Some(my_pid))
        .collect();

    if children.is_empty() {
        return Vec::new();
    }

    children
        .iter()
        .map(|proc| {
            // Try to match to a model slot by finding the slot port in the cmdline.
            let cmdline: String = proc
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            let model_id = slot_info
                .iter()
                .find(|(_, port)| cmdline.contains(&port.to_string()))
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| "engine/other".to_string());

            ModelProcMem {
                model_id,
                rss_mb: b2mb(proc.memory()),
            }
        })
        .collect()
}

// ── GPU probe (macOS) ─────────────────────────────────────────────────────────

#[allow(dead_code)]
const PROBE_COMPILE_TIME_PATH: Option<&str> = option_env!("LMFORGE_GPU_PROBE_PATH");
const PROBE_BIN_NAME: &str = "lmforge-gpu-probe-aarch64-apple-darwin";

#[cfg(target_os = "macos")]
fn find_probe() -> Option<std::path::PathBuf> {
    if let Some(p) = PROBE_COMPILE_TIME_PATH {
        let pb = std::path::Path::new(p);
        if pb.exists() {
            return Some(pb.to_path_buf());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let c = exe.with_file_name(PROBE_BIN_NAME);
        if c.exists() {
            return Some(c);
        }
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let c = std::path::PathBuf::from(&manifest)
            .join("ui/src-tauri/binaries")
            .join(PROBE_BIN_NAME);
        if c.exists() {
            return Some(c);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn run_gpu_probe() -> GpuStats {
    let path = match find_probe() {
        Some(p) => p,
        None => {
            return GpuStats {
                source: "unavailable".into(),
                note: "GPU probe binary not found".into(),
                ..Default::default()
            };
        }
    };

    let output = match std::process::Command::new(&path).output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            return GpuStats {
                source: "unavailable".into(),
                note: format!("probe exited {}", o.status),
                ..Default::default()
            };
        }
        Err(e) => {
            return GpuStats {
                source: "unavailable".into(),
                note: format!("probe spawn failed: {e}"),
                ..Default::default()
            };
        }
    };

    let parsed: ProbeOutput = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let src = parsed.source.clone().unwrap_or_else(|| "unknown".into());
    let note = if src == "unavailable" {
        "IOAccelerator returned no data".into()
    } else {
        format!("via {src}")
    };

    GpuStats {
        util_pct: parsed.gpu_util_pct.map(|v| v as f32),
        mem_used_mb: parsed.gpu_mem_used_mb.map(|v| v as f32),
        mem_total_mb: parsed.gpu_mem_total_mb.map(|v| v as f32),
        source: src,
        note,
    }
}

#[cfg(not(target_os = "macos"))]
fn run_gpu_probe() -> GpuStats {
    run_nvidia_smi()
        .or_else(run_rocm_smi)
        .unwrap_or_else(|| GpuStats {
            source: "unavailable".into(),
            note: "no GPU probe available on this platform".into(),
            ..Default::default()
        })
}

#[cfg(not(target_os = "macos"))]
fn run_nvidia_smi() -> Option<GpuStats> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let p: Vec<&str> = s.trim().split(',').collect();
    if p.len() < 3 {
        return None;
    }
    Some(GpuStats {
        util_pct: p[0].trim().parse().ok(),
        mem_used_mb: p[1].trim().parse().ok(),
        mem_total_mb: p[2].trim().parse().ok(),
        source: "nvidia-smi".into(),
        note: "via NVML".into(),
    })
}

#[cfg(not(target_os = "macos"))]
fn run_rocm_smi() -> Option<GpuStats> {
    let out = std::process::Command::new("rocm-smi")
        .args(["--showuse", "--showmemuse", "--csv"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() >= 2 {
            if let Ok(pct) = cols[1].trim().trim_end_matches('%').parse::<f32>() {
                return Some(GpuStats {
                    util_pct: Some(pct),
                    source: "rocm-smi".into(),
                    note: "via ROCm".into(),
                    ..Default::default()
                });
            }
        }
    }
    None
}

// ── Full sample ───────────────────────────────────────────────────────────────

fn full_sample(slot_info: Vec<(String, u16)>) -> SysStats {
    let gpu_result = std::sync::Arc::new(std::sync::Mutex::new(GpuStats::default()));
    let gpu_clone = gpu_result.clone();

    let gpu_handle = std::thread::spawn(move || {
        *gpu_clone.lock().unwrap() = run_gpu_probe();
    });

    let (cpu_pct, cores, mem_total, mem_used, mem_avail, mem_pct) = sample_sys();
    let model_procs = sample_model_procs(&slot_info);

    gpu_handle.join().ok();
    let gpu = std::sync::Arc::try_unwrap(gpu_result)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();

    let model_rss_gb = model_procs.iter().map(|p| p.rss_mb).sum::<f32>() / 1024.0;

    SysStats {
        cpu_pct,
        cpu_cores_pct: cores,
        mem_total_gb: mem_total,
        mem_used_gb: mem_used,
        mem_avail_gb: mem_avail,
        mem_pct,
        gpu,
        model_procs,
        model_rss_gb,
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `GET /lf/sysinfo` — Live system telemetry + model process memory.
pub async fn sysinfo(State(state): State<AppState>) -> impl IntoResponse {
    // Snapshot current slot info (model_id → port) while we still hold the async lock.
    let slot_info: Vec<(String, u16)> = {
        let es = state.engine_state.read().await;
        es.running_models
            .iter()
            .map(|(id, slot)| (id.clone(), slot.port))
            .collect()
    };

    let stats = tokio::task::spawn_blocking(move || full_sample(slot_info))
        .await
        .unwrap_or_else(|_| SysStats {
            cpu_pct: 0.0,
            cpu_cores_pct: vec![],
            mem_total_gb: 0.0,
            mem_used_gb: 0.0,
            mem_avail_gb: 0.0,
            mem_pct: 0.0,
            gpu: GpuStats {
                source: "sampling error".into(),
                note: String::new(),
                ..Default::default()
            },
            model_procs: vec![],
            model_rss_gb: 0.0,
        });

    let json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".into());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(json))
        .unwrap()
}
