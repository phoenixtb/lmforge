use anyhow::Result;
use tracing::{info, warn};

use crate::config::LmForgeConfig;
use crate::engine;

/// `lmforge start` — Start the inference engine and API server
///
/// `external_status_tx`: when running embedded inside Tauri, the caller provides
/// a broadcast::Sender so it can subscribe a receiver for Tauri IPC event bridging.
/// When `None` (standalone CLI / daemon mode), an internal channel is created.
pub async fn run(
    config: &LmForgeConfig,
    model: Option<String>,
    port: Option<u16>,
    bind: Option<String>,
    _foreground: bool,
    external_status_tx: Option<tokio::sync::broadcast::Sender<crate::engine::manager::EngineState>>,
) -> Result<()> {
    let api_port = port.unwrap_or(config.port);
    let bind_addr = bind.unwrap_or_else(|| config.bind_address.clone());
    let engine_port: u16 = 11431; // Internal engine port
    let data_dir = config.data_dir();

    info!(port = api_port, bind = %bind_addr, "Starting LMForge...");

    // ── Idempotency guard ─────────────────────────────────────────────────────
    // If a daemon is already responding on this port, exit cleanly.
    // This makes `lmforge start` safe to call unconditionally from consumer apps,
    // install scripts, and alongside a running system service.
    if is_daemon_running(api_port).await {
        println!("✓ LMForge already running at http://{}:{}", bind_addr, api_port);
        println!("  Use `lmforge status` to see running models.");
        return Ok(());
    }

    // 1. Ensure data directory exists
    std::fs::create_dir_all(data_dir.join("engines"))?;
    std::fs::create_dir_all(data_dir.join("models"))?;
    std::fs::create_dir_all(data_dir.join("logs"))?;

    // 2. Proactive startup cleanup — kill any stale LMForge or engine processes
    //    and verify both ports are free BEFORE we do anything expensive.
    //    This must happen before hardware probe, engine spawn, or model load.
    startup_cleanup(&data_dir, api_port, engine_port).await?;


    // 3. Load or probe hardware
    let profile = if data_dir.join("hardware.json").exists() {
        let json = std::fs::read_to_string(data_dir.join("hardware.json"))?;
        serde_json::from_str(&json)?
    } else {
        println!("⚙ First run — detecting hardware...");
        let profile = crate::hardware::detect()?;
        let json = serde_json::to_string_pretty(&profile)?;
        std::fs::write(data_dir.join("hardware.json"), &json)?;
        profile
    };

    // 4. Select engine
    let user_engines = data_dir.join("engines.toml");
    let registry = engine::EngineRegistry::load(
        if user_engines.exists() { Some(user_engines.as_path()) } else { None }
    )?;
    let engine_config = registry.select(&profile)?.clone();

    // 5. Check engine is installed
    let engine_cmd = resolve_engine_cmd(&engine_config, &data_dir);
    if !command_exists(&engine_cmd) {
        println!("⚙ Engine not installed, running installer...");
        engine::installer::install(&engine_config, &profile, &data_dir).await?;
    }

    // 6. (Optional) validate model if --model was passed
    if let Some(ref m) = model {
        // Verify the model exists in the index before starting so we fail fast
        let idx = crate::model::index::ModelIndex::load(&data_dir)
            .unwrap_or_else(|_| crate::model::index::ModelIndex { schema_version: 1, models: vec![] });
        if idx.get(m).is_none() {
            anyhow::bail!("Model '{}' not found. Pull it first with:\n  lmforge pull {}", m, m);
        }
    };

    // 7. Write PID file
    engine::daemon::write_pid_file(&data_dir)?;

    let (status_tx, _status_rx0) = match external_status_tx {
        Some(tx) => {
            // Tauri embed mode — use the externally-provided sender so lib.rs
            // can subscribe and bridge state changes to app_handle.emit().
            let _dummy_rx = tx.subscribe(); // keep channel alive
            (tx, None)
        }
        None => {
            // Standalone CLI mode — create our own channel.
            let (tx, rx) = tokio::sync::broadcast::channel::<crate::engine::manager::EngineState>(16);
            (tx, Some(rx))
        }
    };

    // 8. Extract the specialized adapter and start the unified manager constraint
    let adapter = engine::EngineRegistry::create_adapter(&engine_config)?;
    let shared_adapter = std::sync::Arc::new(adapter.clone());
    let mut manager = engine::EngineManager::new(
        engine_config.clone(),
        adapter,
        engine_port,
        data_dir.clone(),
        config.orchestrator.keep_alive.clone(),
        config.orchestrator.max_loaded_models,
        status_tx.clone(),
    );

    println!("⚙ Starting {} v{} Orchestrator...", engine_config.name, engine_config.version);
    manager.start().await?;



    // 10. Start Orchestrator Control Channel
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(32);

    let state = manager.state();
    println!("\n✓ LMForge Orchestrator ready");
    println!("  Engine:  {} v{}", engine_config.name, engine_config.version);
    println!("  Mode:    Multi-model (keep_alive={})", config.orchestrator.keep_alive);
    println!("  API:     http://{}:{}", bind_addr, api_port);
    println!("  Health:  http://{}:{}/health", bind_addr, api_port);
    println!("\n  Press Ctrl+C to stop.\n");

    // Spawn orchestration supervision in background
    let supervise_handle = tokio::spawn(async move {
        manager.supervise(cmd_rx).await;
    });

    // 11. Start API server
    let app_state = crate::server::AppState {
        engine_state: state,
        engine_config: engine_config.clone(),
        adapter: shared_adapter,
        data_dir: data_dir.clone(),
        api_key: None,
        bind_address: bind_addr.clone(),
        config: std::sync::Arc::new(tokio::sync::RwLock::new(config.clone())),
        command_tx: cmd_tx.clone(),
        status_tx,
    };

    // Pre-warm the requested model if provided
    if let Some(m) = model {
        println!("⚙ Pre-warming model: {}", m);
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let _ = app_state_clone.ensure_model(&m, None).await;
        });
    }

    let app = crate::server::build_router(app_state);
    let addr = format!("{}:{}", bind_addr, api_port);
    // Port was verified free in startup_cleanup — bind must succeed now.
    let listener = tokio::net::TcpListener::bind(&addr).await
        .map_err(|e| anyhow::anyhow!("Failed to bind API port {}: {}", api_port, e))?;

    info!(addr = %addr, "API server listening");

    // Run server with graceful shutdown on Ctrl+C
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            println!("\n⚙ Shutting down...");
        })
        .await?;

    supervise_handle.abort();
    engine::daemon::remove_pid_file(&data_dir);
    println!("✓ LMForge stopped.");

    Ok(())
}

/// Proactive startup cleanup — runs BEFORE any expensive operations.
///
/// Sequence:
///   1. Kill any LMForge daemon recorded in lmforge.pid (SIGTERM → 3s → SIGKILL)
///   2. Kill any engine process recorded in engines/<id>.pid (SIGKILL immediately)
///   3. For any port still occupied after PID-based kills, use `lsof` to find
///      and kill the holding process as a last resort.
///   4. Wait until both the API port and engine port are confirmed free (up to 10s).
///      Returns Err if either port is still occupied after all attempts.
async fn startup_cleanup(data_dir: &std::path::Path, api_port: u16, engine_port: u16) -> anyhow::Result<()> {
    kill_pid_file_process(engine::daemon::pid_file_path(data_dir), true).await;
    kill_engine_pid_files(data_dir);

    // Give the OS a moment to release file descriptors
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify both ports are free, with last-resort lsof kill if not
    ensure_port_free(api_port).await?;
    ensure_port_free(engine_port).await?;

    Ok(())
}

/// Kill the process recorded in `pid_file`.
/// If `graceful` is true, sends SIGTERM first and waits up to 3s before SIGKILL.
async fn kill_pid_file_process(pid_file: std::path::PathBuf, graceful: bool) {
    let Ok(content) = std::fs::read_to_string(&pid_file) else { return };
    let Ok(pid) = content.trim().parse::<u32>() else { return };

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        let nix_pid = Pid::from_raw(pid as i32);

        if graceful {
            let _ = kill(nix_pid, Signal::SIGTERM);
            warn!(pid, "Sent SIGTERM to stale LMForge daemon, waiting for clean exit");
            // Wait up to 3s for graceful exit
            for _ in 0..6 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if unsafe { libc::kill(pid as i32, 0) } != 0 { break; } // process gone
            }
            // If still running, force kill
            let _ = kill(nix_pid, Signal::SIGKILL);
            warn!(pid, "Sent SIGKILL to stale LMForge daemon");
        } else {
            let _ = kill(nix_pid, Signal::SIGKILL);
            warn!(pid, "Sent SIGKILL to stale engine process");
        }
    }
    let _ = std::fs::remove_file(&pid_file);
}

/// Kill all engine PID files found under <data_dir>/engines/*.pid
fn kill_engine_pid_files(data_dir: &std::path::Path) {
    let engines_dir = data_dir.join("engines");
    let Ok(entries) = std::fs::read_dir(&engines_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "pid") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    #[cfg(unix)]
                    {
                        use nix::sys::signal::{kill, Signal};
                        use nix::unistd::Pid;
                        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                        warn!(pid, path = %path.display(), "Sent SIGKILL to stale engine process");
                    }
                }
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Ensure a TCP port is free.  If it is still bound after PID-based kills,
/// use `lsof` to find and kill the holding process, then wait up to 10s.
async fn ensure_port_free(port: u16) -> anyhow::Result<()> {
    // Fast path — already free
    if is_port_free(port).await { return Ok(()); }

    // Last resort: ask lsof who is holding the port and kill it
    warn!(port, "Port still occupied after PID cleanup — using lsof to identify holder");
    kill_port_holder_via_lsof(port);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    for _ in 0..9 {
        if is_port_free(port).await { return Ok(()); }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    anyhow::bail!(
        "Port {} is still occupied after cleanup. Kill it manually:\n  lsof -ti :{} | xargs kill -9",
        port, port
    )
}

/// Check if a LMForge daemon is already listening and healthy on this port.
/// Performs a raw TCP GET /health with short timeouts — no extra HTTP deps.
/// Returns true only if the /health endpoint responds with a 2xx status.
async fn is_daemon_running(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let connect = tokio::time::timeout(
        std::time::Duration::from_millis(400),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    ).await;

    let mut stream: tokio::net::TcpStream = match connect {
        Ok(Ok(s)) => s,
        _ => return false,
    };

    let req = format!("GET /health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    if stream.write_all(req.as_bytes()).await.is_err() {
        return false;
    }

    let mut buf = [0u8; 32];
    let read = tokio::time::timeout(
        std::time::Duration::from_millis(400),
        stream.read(&mut buf),
    ).await;

    let n = match read {
        Ok(Ok(n)) if n > 0 => n,
        _ => return false,
    };

    let resp = std::str::from_utf8(&buf[..n]).unwrap_or("");
    resp.starts_with("HTTP/1") && resp.contains(" 2")
}

async fn is_port_free(port: u16) -> bool {
    tokio::net::TcpListener::bind(("127.0.0.1", port)).await.is_ok()
}

/// Use `lsof -ti :PORT` to find the PID holding the port and send SIGKILL.
fn kill_port_holder_via_lsof(port: u16) {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{}", port)])
        .output();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                #[cfg(unix)]
                {
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                    warn!(pid, port, "Sent SIGKILL to unknown port holder (via lsof)");
                }
            }
        }
    }
}

/// Resolve the engine command path
fn resolve_engine_cmd(engine: &engine::EngineConfig, data_dir: &std::path::Path) -> String {
    let cmd = &engine.start_cmd;

    // Check venv
    let venv_bin = data_dir.join("engines").join(&engine.id)
        .join("venv").join("bin").join(cmd);
    if venv_bin.exists() {
        return venv_bin.to_string_lossy().to_string();
    }

    // Check local engines dir
    let local_bin = data_dir.join("engines").join(cmd);
    if local_bin.exists() {
        return local_bin.to_string_lossy().to_string();
    }

    cmd.clone()
}

/// Check if a command exists
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

