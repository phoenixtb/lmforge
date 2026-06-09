use anyhow::Result;
use tracing::{info, warn};

use crate::config::LmForgeConfig;
use crate::engine;

/// All knobs for `lmforge start`. Grouped into a struct so we don't have to
/// thread eight positional arguments through `dispatch` → `start::run` →
/// Tauri embed; new flags drop in without churning every call site.
#[derive(Debug, Default, Clone)]
pub struct StartOptions {
    /// Model to load on startup (warm-pull).
    pub model: Option<String>,
    /// HTTP API port override.
    pub port: Option<u16>,
    /// Bind address override.
    pub bind: Option<String>,
    /// Run in foreground (default is daemon mode).
    pub foreground: bool,
    /// Force a specific engine id (`sglang`, `vllm`, …). When `None` the
    /// registry's tier-aware auto-selection runs as usual.
    pub engine: Option<String>,
    /// Skip the interactive prompt when `engine` is in the experimental tier.
    pub yes_experimental: bool,
}

/// `lmforge start` — Start the inference engine and API server
///
/// `external_status_tx`: when running embedded inside Tauri, the caller provides
/// a broadcast::Sender so it can subscribe a receiver for Tauri IPC event bridging.
/// When `None` (standalone CLI / daemon mode), an internal channel is created.
pub async fn run(
    config: &LmForgeConfig,
    opts: StartOptions,
    external_status_tx: Option<tokio::sync::broadcast::Sender<crate::engine::manager::EngineState>>,
) -> Result<()> {
    let StartOptions {
        model,
        port,
        bind,
        foreground: _foreground,
        engine: engine_override,
        yes_experimental,
    } = opts;
    // Precedence: CLI flag > LMFORGE_BIND env > config.bind_address.
    // Env override exists primarily for containerised deployments where the
    // image cannot ship a custom config.toml.
    let bind_addr = bind
        .or_else(|| std::env::var("LMFORGE_BIND").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| config.bind_address.clone());
    let api_port = port.unwrap_or(config.port);
    // Same precedence model for the bearer token. CLI has no flag for it
    // (security: no shell history), so this is env > config.
    let resolved_api_key = std::env::var("LMFORGE_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| config.api_key.clone());
    let engine_port: u16 = 11431; // Internal engine port
    let data_dir = config.data_dir();
    let models_dir = config.models_dir();

    info!(port = api_port, bind = %bind_addr, "Starting LMForge...");

    // ── Idempotency guard ─────────────────────────────────────────────────────
    // If a daemon is already responding on this port, exit cleanly.
    // This makes `lmforge start` safe to call unconditionally from consumer apps,
    // install scripts, and alongside a running system service.
    if is_daemon_running(api_port).await {
        println!(
            "✓ LMForge already running at http://{}:{}",
            bind_addr, api_port
        );
        println!("  Use `lmforge status` to see running models.");
        return Ok(());
    }

    // 1. Ensure data directory exists
    std::fs::create_dir_all(data_dir.join("engines"))?;
    std::fs::create_dir_all(&models_dir)?;
    std::fs::create_dir_all(data_dir.join("logs"))?;

    // 1a. Execute pending migration manifest (written by POST /lf/storage/apply).
    //     Must run after dirs exist but before any model-index access.
    if let Ok(Some(manifest)) = crate::model::migration::PendingMigration::load() {
        info!("Pending migration manifest found — executing startup drain...");
        execute_migration_drain(&manifest, &data_dir, &models_dir).await;
        if let Err(e) = crate::model::migration::PendingMigration::clear() {
            warn!(error = %e, "Failed to clear pending-migration.json after drain");
        }
    }

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

    // 4. Select engine — either auto (tier-aware, hardware-gated) or forced
    //    via `--engine <id>`. Forced selection still enforces hardware gates,
    //    but bypasses the tier filter that normally hides experimental engines.
    let user_engines = data_dir.join("engines.toml");
    let registry = engine::EngineRegistry::load(if user_engines.exists() {
        Some(user_engines.as_path())
    } else {
        None
    })?;
    let engine_config = match engine_override.as_deref() {
        Some(id) => {
            let cfg = registry.select_explicit(id, &profile)?.clone();
            confirm_experimental_engine(&cfg, yes_experimental)?;
            // Soft caveats (single-GPU vLLM, NVFP4-on-sm120, etc.). These are
            // printed once at start-time so users running `--engine vllm` see
            // them even if they skipped `engine install` interactivity.
            crate::cli::engine::print_soft_caveats(&cfg, &profile);
            cfg
        }
        None => registry.select(&profile)?.clone(),
    };

    // 5. Check engine is installed (variant tree for llamacpp, legacy path otherwise)
    if !engine_is_ready(&engine_config, &profile, &data_dir) {
        println!("⚙ Engine not installed, running installer...");
        engine::installer::install(&engine_config, &profile, &data_dir).await?;
    }

    // 6. (Optional) validate model if --model was passed
    if let Some(ref m) = model {
        // Verify the model exists in the index before starting so we fail fast
        let idx =
            crate::model::index::ModelIndex::load(&data_dir, &models_dir).unwrap_or_else(|_| {
                crate::model::index::ModelIndex {
                    schema_version: 1,
                    models: vec![],
                }
            });
        if idx.get(m).is_none() {
            anyhow::bail!(
                "Model '{}' not found. Pull it first with:\n  lmforge pull {}",
                m,
                m
            );
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
            let (tx, rx) =
                tokio::sync::broadcast::channel::<crate::engine::manager::EngineState>(16);
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
        models_dir.clone(),
        config.orchestrator.keep_alive.clone(),
        config.orchestrator.max_loaded_models,
        status_tx.clone(),
    );

    println!(
        "⚙ Starting {} v{} Orchestrator...",
        engine_config.name, engine_config.version
    );
    manager.start().await?;

    // 10. Start Orchestrator Control Channel
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(32);

    let state = manager.state();
    let pull_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    println!("\n✓ LMForge Orchestrator ready");
    println!(
        "  Engine:  {} v{}",
        engine_config.name, engine_config.version
    );
    println!(
        "  Mode:    Multi-model (keep_alive={})",
        config.orchestrator.keep_alive
    );
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
        models_dir: models_dir.clone(),
        api_key: resolved_api_key.clone(),
        bind_address: bind_addr.clone(),
        config: std::sync::Arc::new(tokio::sync::RwLock::new(config.clone())),
        command_tx: cmd_tx.clone(),
        status_tx,
        pull_in_flight,
        active_pull: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
    };

    // Install the Prometheus recorder before the router is built so the
    // /metrics endpoint has descriptors already registered.
    crate::server::metrics::init();

    // Build auth policy + emit a startup warning when the daemon is exposed
    // beyond loopback without any auth coverage.
    let auth_policy = std::sync::Arc::new(crate::server::auth::AuthPolicy::from_config(
        resolved_api_key.clone(),
        &config.trusted_networks,
        config.unsafe_disable_auth,
    ));
    if config.unsafe_disable_auth {
        warn!(
            "unsafe_disable_auth is true — every request is allowed unauthenticated. \
             Do NOT use this in production."
        );
        println!(
            "⚠ unsafe_disable_auth=true — daemon is fully open. Disable in config.toml for any non-dev use."
        );
    } else if !is_loopback_bind(&bind_addr)
        && resolved_api_key.is_none()
        && config.trusted_networks.is_empty()
    {
        // No coverage at all: bind public, no token, no allowlist. By default
        // we just warn (every request will 401, daemon stays up). Opt-in
        // strict mode via env refuses startup so misconfigured prod boxes
        // fail loudly instead of silently rejecting traffic.
        let refuse = std::env::var("LMFORGE_REFUSE_UNSAFE_BIND")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        let msg = format!(
            "Bind {bind_addr} has no api_key and no trusted_networks — external requests will 401. \
             Add a CIDR to trusted_networks (e.g. 192.168.0.0/16) or set api_key in config.toml."
        );
        if refuse {
            anyhow::bail!("{msg} Refusing to start because LMFORGE_REFUSE_UNSAFE_BIND is set.");
        }
        warn!(bind = %bind_addr, "{msg}");
        println!("⚠ {msg}");
    }

    // Pre-warm the requested model if provided
    if let Some(m) = model {
        println!("⚙ Pre-warming model: {}", m);
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let _ = app_state_clone.ensure_model(&m, None).await;
        });
    }

    // Auto-load: serial cold-load of configured models so the first request to
    // each is warm. We run this in the background so the API is reachable while
    // models load — clients hitting an unloaded model just pay the usual
    // ensure_model cost on first use.
    if !config.orchestrator.auto_load.is_empty() {
        let auto_load = config.orchestrator.auto_load.clone();
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let total = auto_load.len();
            for (i, model_id) in auto_load.iter().enumerate() {
                let n = i + 1;
                info!(model = %model_id, "auto_load {}/{}: starting cold-load", n, total);
                println!("⚙ auto_load {}/{}: {}", n, total, model_id);
                match app_state_clone.ensure_model(model_id, None).await {
                    Ok(port) => {
                        info!(model = %model_id, port, "auto_load {}/{}: ready", n, total);
                        println!("  ✓ {} ready on port {}", model_id, port);
                    }
                    Err(_) => {
                        warn!(model = %model_id, "auto_load {}/{}: failed", n, total);
                        println!(
                            "  ✗ {} failed (see logs); continuing with next model",
                            model_id
                        );
                    }
                }
            }
            info!("auto_load: all {} models processed", total);
        });
    }

    let concurrency = crate::server::concurrency::ConcurrencyLimit::new(
        config.resources.max_concurrent_requests as usize,
        config.resources.request_queue_size as usize,
    );
    let max_body_bytes =
        crate::server::resolve_max_body_bytes(config.resources.max_request_body_mb);
    let app = crate::server::build_router(app_state, auth_policy, concurrency, max_body_bytes);
    let addr = format!("{}:{}", bind_addr, api_port);
    // Port was verified free in startup_cleanup — bind must succeed now.
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind API port {}: {}", api_port, e))?;

    info!(addr = %addr, "API server listening");

    // ConnectInfo<SocketAddr> is required by the auth middleware to extract the
    // client IP for trusted_networks matching.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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

/// Execute the pending migration drain after dirs are ready.
/// - Scan: rebuilds the model index from the new models_dir (prune=true).
/// - Repull: re-downloads each queued model into the new models_dir.
/// - None: no-op (adopt-in-place or delete-only — dirs are already right).
async fn execute_migration_drain(
    manifest: &crate::model::migration::PendingMigration,
    data_dir: &std::path::Path,
    models_dir: &std::path::Path,
) {
    use crate::model::migration::MigrationIntent;

    match manifest.intent {
        MigrationIntent::None | MigrationIntent::Scan => {
            info!("Migration drain: scanning new models_dir for models...");
            if let Err(e) = crate::cli::models::scan(data_dir, models_dir, true) {
                warn!(error = %e, "Migration drain: index scan failed");
            }
        }
        MigrationIntent::Repull => {
            info!(
                count = manifest.repull_queue.len(),
                "Migration drain: re-pulling models into new directory..."
            );
            for entry in &manifest.repull_queue {
                // Sanitise the model id into a filesystem-safe dir name.
                let dir_name = entry.id.replace([':', '/'], "-");
                let model_dir = models_dir.join(&dir_name);
                if let Err(e) = std::fs::create_dir_all(&model_dir) {
                    warn!(model = %entry.id, error = %e, "Migration drain: could not create model dir");
                    continue;
                }
                info!(model = %entry.id, repo = %entry.hf_repo, "Migration drain: re-pulling...");
                let (tx, _rx) = tokio::sync::mpsc::channel(32);
                match crate::model::downloader::download_model(
                    &entry.hf_repo,
                    &[],
                    &model_dir,
                    Some(tx),
                )
                .await
                {
                    Ok(_) => {
                        if let Ok(mut idx) =
                            crate::model::index::ModelIndex::load(data_dir, models_dir)
                        {
                            let caps = crate::model::index::detect_capabilities(
                                &model_dir,
                                Some(&entry.id),
                                Some(&entry.hf_repo),
                            );
                            idx.add(crate::model::index::ModelEntry {
                                id: entry.id.clone(),
                                path: model_dir.to_string_lossy().to_string(),
                                format: entry.format.clone(),
                                engine: entry.engine.clone(),
                                hf_repo: Some(entry.hf_repo.clone()),
                                size_bytes: crate::model::index::dir_size(&model_dir),
                                capabilities: caps,
                                added_at: chrono::Utc::now().to_rfc3339(),
                            });
                            let _ = idx.save(data_dir, models_dir);
                        }
                    }
                    Err(e) => {
                        warn!(
                            model = %entry.id,
                            error = %e,
                            "Migration drain: re-pull failed — model will not be available until manually re-downloaded"
                        );
                    }
                }
            }
        }
    }
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
async fn startup_cleanup(
    data_dir: &std::path::Path,
    api_port: u16,
    engine_port: u16,
) -> anyhow::Result<()> {
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
    let Ok(content) = std::fs::read_to_string(&pid_file) else {
        return;
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return;
    };

    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let nix_pid = Pid::from_raw(pid as i32);

        if graceful {
            let _ = kill(nix_pid, Signal::SIGTERM);
            warn!(
                pid,
                "Sent SIGTERM to stale LMForge daemon, waiting for clean exit"
            );
            for _ in 0..6 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if unsafe { libc::kill(pid as i32, 0) } != 0 {
                    break;
                }
            }
            let _ = kill(nix_pid, Signal::SIGKILL);
            warn!(pid, "Sent SIGKILL to stale LMForge daemon");
        } else {
            let _ = kill(nix_pid, Signal::SIGKILL);
            warn!(pid, "Sent SIGKILL to stale engine process");
        }
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
        warn!(pid, "Sent taskkill /F to stale LMForge daemon");
    }

    let _ = std::fs::remove_file(&pid_file);
}

/// Kill all engine PID files found under <data_dir>/engines/*.pid
fn kill_engine_pid_files(data_dir: &std::path::Path) {
    let engines_dir = data_dir.join("engines");
    let Ok(entries) = std::fs::read_dir(&engines_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "pid") {
            if let Ok(content) = std::fs::read_to_string(&path)
                && let Ok(pid) = content.trim().parse::<u32>()
            {
                #[cfg(unix)]
                {
                    use nix::sys::signal::{Signal, kill};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                    warn!(pid, path = %path.display(), "Sent SIGKILL to stale engine process");
                }

                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/PID", &pid.to_string()])
                        .output();
                    warn!(pid, path = %path.display(), "Sent taskkill /F to stale engine process");
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
    if is_port_free(port).await {
        return Ok(());
    }

    // Last resort: ask lsof who is holding the port and kill it
    warn!(
        port,
        "Port still occupied after PID cleanup — using lsof to identify holder"
    );
    kill_port_holder_via_lsof(port);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    for _ in 0..9 {
        if is_port_free(port).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    #[cfg(unix)]
    let manual_hint = format!("lsof -ti :{port} | xargs kill -9");
    #[cfg(windows)]
    let manual_hint = format!(
        "for /f \"tokens=5\" %a in ('netstat -ano ^| findstr :{port}') do taskkill /F /PID %a"
    );

    anyhow::bail!(
        "Port {} is still occupied after cleanup. Kill it manually:\n  {}",
        port,
        manual_hint
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
    )
    .await;

    let mut stream: tokio::net::TcpStream = match connect {
        Ok(Ok(s)) => s,
        _ => return false,
    };

    let req = format!("GET /health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    if stream.write_all(req.as_bytes()).await.is_err() {
        return false;
    }

    let mut buf = [0u8; 32];
    let read =
        tokio::time::timeout(std::time::Duration::from_millis(400), stream.read(&mut buf)).await;

    let n = match read {
        Ok(Ok(n)) if n > 0 => n,
        _ => return false,
    };

    let resp = std::str::from_utf8(&buf[..n]).unwrap_or("");
    resp.starts_with("HTTP/1") && resp.contains(" 2")
}

async fn is_port_free(port: u16) -> bool {
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_ok()
}

/// Use `lsof` (Unix) or `netstat`+`taskkill` (Windows) to free a held port.
fn kill_port_holder_via_lsof(port: u16) {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{}", port)])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    use nix::sys::signal::{Signal, kill};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                    warn!(pid, port, "Sent SIGKILL to port holder (via lsof)");
                }
            }
        }
    }

    #[cfg(windows)]
    {
        // netstat -ano | findstr :<port>  → last column is PID
        let output = std::process::Command::new("netstat")
            .args(["-ano"])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if line.contains(&format!(":{} ", port)) || line.contains(&format!(":{}	", port)) {
                    if let Some(pid_str) = line.split_whitespace().last() {
                        if let Ok(pid) = pid_str.trim().parse::<u32>() {
                            let _ = std::process::Command::new("taskkill")
                                .args(["/F", "/PID", &pid.to_string()])
                                .output();
                            warn!(pid, port, "Sent taskkill /F to port holder (via netstat)");
                        }
                    }
                }
            }
        }
    }
}

/// True when the bind address is a loopback address.
/// Used at startup to decide whether to print the non-loopback auth warning.
fn is_loopback_bind(bind: &str) -> bool {
    matches!(bind, "127.0.0.1" | "localhost" | "::1")
        || bind
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Block forced selection of an `experimental` tier engine behind a prompt
/// or env flag. Phase 5.1.
///
/// Decision matrix:
///   * Engine is NOT experimental → no-op, returns Ok.
///   * `--yes-experimental` was passed → warn-only, returns Ok.
///   * `LMFORGE_YES_EXPERIMENTAL=1` is set → warn-only, returns Ok.
///   * Non-interactive (no TTY) → bails. We never want a systemd unit to
///     silently fall through to an unsupported engine.
///   * Interactive TTY → reads y/N from stdin.
fn confirm_experimental_engine(cfg: &engine::EngineConfig, yes_flag: bool) -> anyhow::Result<()> {
    use crate::engine::registry::EngineTier;

    if cfg.tier != EngineTier::Experimental {
        return Ok(());
    }

    let banner = format!(
        "⚠ Engine `{}` is in the EXPERIMENTAL tier.\n  \
         These engines are known to break on at least one supported \
         hardware/OS combo. See `data/engines.toml` for the platform window \
         and `docs/architecture/ADR-001-engine-tiers.md` for the policy.",
        cfg.id
    );

    if yes_flag
        || std::env::var("LMFORGE_YES_EXPERIMENTAL")
            .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    {
        warn!(engine_id = %cfg.id, "Experimental engine forced via flag/env");
        eprintln!("{banner}");
        return Ok(());
    }

    if !is_stdin_tty() {
        anyhow::bail!(
            "{banner}\n  \
             Refusing to start in non-interactive mode without explicit \
             opt-in. Pass --yes-experimental or set LMFORGE_YES_EXPERIMENTAL=1."
        );
    }

    eprintln!("{banner}");
    eprint!("  Continue with `{}`? [y/N]: ", cfg.id);
    use std::io::Write;
    std::io::stderr().flush().ok();

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        anyhow::bail!(
            "Aborted by user — experimental engine `{}` not started.",
            cfg.id
        );
    }
}

#[cfg(unix)]
fn is_stdin_tty() -> bool {
    // SAFETY: isatty is a simple syscall on a stable fd number.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

#[cfg(windows)]
fn is_stdin_tty() -> bool {
    // The Windows equivalent (GetFileType + FILE_TYPE_CHAR) requires the
    // winapi crate. For the CLI we just refuse non-interactive opt-in on
    // Windows; cheap and safe.
    true
}

/// True when the selected engine has a runnable install on disk.
fn engine_is_ready(
    engine: &engine::EngineConfig,
    profile: &crate::hardware::probe::HardwareProfile,
    data_dir: &std::path::Path,
) -> bool {
    if engine.id == "llamacpp" {
        let legacy = data_dir.join("engines").join("llama-server");
        if legacy.is_file() {
            return true;
        }
        let state = engine::installer::scan_variant_state(data_dir, profile);
        let active = engine::variant::select(profile, &state);
        return engine::installer::variant_installed(data_dir, active, profile);
    }

    let cmd = resolve_engine_cmd(engine, data_dir);
    if std::path::Path::new(&cmd).is_file() {
        return true;
    }
    command_exists(&cmd)
}

/// Resolve the engine command path
fn resolve_engine_cmd(engine: &engine::EngineConfig, data_dir: &std::path::Path) -> String {
    let cmd = &engine.start_cmd;

    // Check venv
    let venv_bin = data_dir
        .join("engines")
        .join(&engine.id)
        .join("venv")
        .join("bin")
        .join(cmd);
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

/// Check if a command exists (cross-platform)
fn command_exists(cmd: &str) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("which")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::registry::{EngineConfig, EngineTier};

    fn experimental_cfg() -> EngineConfig {
        EngineConfig {
            id: "sglang".to_string(),
            tier: EngineTier::Experimental,
            ..Default::default()
        }
    }

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            id: "llamacpp".to_string(),
            tier: EngineTier::Default,
            ..Default::default()
        }
    }

    #[test]
    fn confirm_skips_for_non_experimental_engines() {
        let cfg = default_cfg();
        assert!(confirm_experimental_engine(&cfg, false).is_ok());
        assert!(confirm_experimental_engine(&cfg, true).is_ok());
    }

    #[test]
    fn confirm_allows_experimental_with_yes_flag() {
        let cfg = experimental_cfg();
        assert!(confirm_experimental_engine(&cfg, true).is_ok());
    }

    #[test]
    fn confirm_allows_experimental_with_env() {
        // The function reads LMFORGE_YES_EXPERIMENTAL — we set it, run, unset.
        // SAFETY: process-global env access is sound on single-threaded test
        // exec and our test runner doesn't fork between tests.
        unsafe { std::env::set_var("LMFORGE_YES_EXPERIMENTAL", "1") };
        let cfg = experimental_cfg();
        let result = confirm_experimental_engine(&cfg, false);
        unsafe { std::env::remove_var("LMFORGE_YES_EXPERIMENTAL") };
        assert!(result.is_ok());
    }
}
