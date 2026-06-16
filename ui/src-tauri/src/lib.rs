mod tray;

use tauri::Emitter;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        // Window close button → hide to tray. "Stop Engine" in tray menu is the
        // only way to stop the daemon. Closing the window never kills the service.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            // ── macOS: use Regular policy so the app owns the menu bar when focused.
            // Accessory would prevent the menu bar from updating, leaving the previous
            // app's menu visible. We still hide to tray on close (on_window_event above).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            let app_handle = app.handle().clone();

            // ── System tray ──────────────────────────────────────────────────
            // The tray now drives its own HTTP polling loop internally.
            // Gracefully degrade when tray is unavailable.
            #[cfg(not(target_os = "android"))]
            {
                if let Err(e) = tray::setup_tray(&app_handle) {
                    eprintln!("⚠ Tray unavailable: {}. Running in window-only mode.", e);
                }
                // Window is shown by visible:true in tauri.conf.json.
                // The on_window_event close handler hides it to tray instead of quitting.
            }

            // ── Status bridge: HTTP → Tauri IPC events ───────────────────────
            // Polls /lf/status every 2 s and emits "lf:status" Tauri events to
            // Svelte. Falls back to polling when SSE is not available.
            // The daemon is NOT started here — it must be running independently
            // (via `lmforge start`, `lmforge service`, or the user's shell).
            {
                let app_handle = app_handle.clone();
                tauri::async_runtime::spawn(status_bridge(app_handle));
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_engine,
            stop_engine,
            restart_engine,
            restart_service,
            get_service_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LMForge");
}

/// Poll /lf/status every 2 s and emit "lf:status" / "lf:health" Tauri events.
///
/// Key design decisions:
///  1. 800 ms startup delay — gives the WebView time to mount and register
///     its listen() handlers before the first event fires. Without this there
///     is a race: the event fires before Svelte attaches its listener, the UI
///     never receives it, and daemonOnline stays null → stuck "connecting".
///  2. Emit lf:health EVERY cycle — not just on state change — so any
///     late-registering listener is caught on the next tick.
async fn status_bridge(app: tauri::AppHandle) {
    // Generous timeout: during a large model pull the daemon saturates the
    // link/CPU and a short timeout makes /health or /lf/status flap. A single
    // flap must NOT wipe the in-flight pull snapshot from the UI.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .unwrap_or_default();

    let base_url = "http://127.0.0.1:11430";

    // Number of consecutive failed health probes before we declare the daemon
    // offline. Tolerating a transient blip keeps the status store (and the
    // active_pull snapshot it carries) intact under heavy download load.
    const OFFLINE_THRESHOLD: u32 = 3;
    let mut consecutive_fail: u32 = 0;

    // Let the WebView mount and register listen() before the first event.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    loop {
        let health_ok = client
            .get(format!("{}/health", base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if health_ok {
            consecutive_fail = 0;
            let _ = app.emit("lf:health", serde_json::json!({ "online": true }));

            // Fetch the full status snapshot (includes active_pull). If this
            // request itself fails, emit nothing — the store retains its last
            // good value rather than dropping the in-flight pull.
            if let Ok(resp) = client.get(format!("{}/lf/status", base_url)).send().await {
                if let Ok(body) = resp.text().await {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        let _ = app.emit("lf:status", &json);
                    }
                }
            }
        } else {
            consecutive_fail = consecutive_fail.saturating_add(1);
            // Only declare offline (and wipe status) after sustained failures.
            // A single timeout under download load is treated as transient and
            // leaves the existing status — and its active_pull — untouched.
            if consecutive_fail >= OFFLINE_THRESHOLD {
                let _ = app.emit("lf:health", serde_json::json!({ "online": false }));
                let _ = app.emit(
                    "lf:status",
                    serde_json::json!({
                        "overall_status": "stopped",
                        "engine_id": "—",
                        "engine_version": "—",
                        "running_models": {},
                        "metrics": {
                            "requests_total": 0,
                            "ttft_avg_ms": 0.0,
                            "uptime_secs": 0,
                            "restart_count": 0
                        },
                        "last_errors": {},
                        "active_pull": null
                    }),
                );
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Build a `lmforge` command that never flashes a console window on Windows.
/// The UI is a GUI-subsystem app; spawning a console-subsystem exe without
/// CREATE_NO_WINDOW makes Windows allocate a visible console for it.
fn lmforge_command(bin: &str) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(bin);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Poll the daemon `/health` endpoint until reachability equals `want_up`.
///
/// Lets callers wait for the daemon to come UP after a start, or go DOWN after a
/// stop. Returns `true` once the desired state is observed, `false` on timeout.
#[cfg(windows)]
async fn wait_for_health(want_up: bool, timeout: std::time::Duration) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let up = client
            .get("http://127.0.0.1:11430/health")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if up == want_up {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Spawn `lmforge <args>` fully detached, WITHOUT capturing stdout/stderr.
///
/// CRITICAL on Windows: `lmforge start` / `service restart` spawn a long-lived
/// *detached* daemon. If the UI launches them through a captured pipe
/// (`.output()`), that daemon inherits the pipe's write handle and holds it open
/// for its whole lifetime — so the parent's pipe read never sees EOF and
/// `.output().await` blocks forever (the command logic already finished, but the
/// handle is still held by the daemon that outlives it). Spawning with null
/// stdio creates no pipe to leak; callers detect success by polling `/health`
/// (see `wait_for_health`) rather than reading stdout.
#[cfg(windows)]
fn spawn_lmforge_detached(bin: &str, args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to spawn `lmforge {}`: {e}", args.join(" ")))
}

/// Tauri command: start the LMForge engine (called from the "Engine offline" screen).
/// Uses `lmforge start` which is idempotent — safe to call even if already running.
#[tauri::command]
async fn start_engine() -> Result<String, String> {
    let lmforge_bin = find_lmforge_binary();

    // Windows: spawn detached + poll /health. Never capture stdout — the detached
    // daemon would inherit the pipe and hang `.output().await` forever.
    #[cfg(windows)]
    {
        spawn_lmforge_detached(&lmforge_bin, &["start"])?;
        return if wait_for_health(true, std::time::Duration::from_secs(60)).await {
            Ok("LMForge daemon started.".to_string())
        } else {
            Err(
                "Daemon did not become reachable within 60s. Check %USERPROFILE%\\.lmforge\\logs."
                    .to_string(),
            )
        };
    }

    #[cfg(not(windows))]
    {
        let output = lmforge_command(&lmforge_bin)
            .arg("start")
            .output()
            .await
            .map_err(|e| format!("Failed to spawn lmforge: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() || stdout.contains("already running") {
            Ok(stdout)
        } else {
            Err(format!("{stdout}{stderr}"))
        }
    }
}

/// Tauri command: restart the LMForge daemon (run-mode aware).
///
/// If a service unit is installed, delegates to `lmforge service restart` to
/// avoid KeepAlive / `Restart=always` respawn races. Otherwise falls back to
/// `lmforge stop` + `lmforge start` (foreground mode).
#[tauri::command]
async fn restart_engine() -> Result<String, String> {
    let lmforge_bin = find_lmforge_binary();

    // Windows: explicit stop → wait DOWN → start → wait UP, all via detached
    // spawns and /health polling. Capturing stdout (`.output()`) would deadlock
    // because the restarted daemon inherits the pipe handle (see
    // spawn_lmforge_detached). This is deterministic and independent of whether
    // the autostart Run key is registered.
    #[cfg(windows)]
    {
        spawn_lmforge_detached(&lmforge_bin, &["stop"])?;
        // Confirm the old daemon released the port before starting a new one.
        wait_for_health(false, std::time::Duration::from_secs(15)).await;
        spawn_lmforge_detached(&lmforge_bin, &["start"])?;
        return if wait_for_health(true, std::time::Duration::from_secs(60)).await {
            Ok("LMForge daemon restarted.".to_string())
        } else {
            Err("Daemon did not become reachable within 60s after restart. Check %USERPROFILE%\\.lmforge\\logs.".to_string())
        };
    }

    #[cfg(not(windows))]
    {
        if is_service_installed(&lmforge_bin).await {
            let output = lmforge_command(&lmforge_bin)
                .args(["service", "restart"])
                .output()
                .await
                .map_err(|e| format!("Failed to spawn lmforge: {e}"))?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return if output.status.success() {
                Ok(stdout)
            } else {
                Err(format!("{stdout}{stderr}"))
            };
        }

        // Foreground mode: graceful stop + start.
        let _ = lmforge_command(&lmforge_bin).arg("stop").output().await;
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

        let output = lmforge_command(&lmforge_bin)
            .arg("start")
            .output()
            .await
            .map_err(|e| format!("Failed to spawn lmforge: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() || stdout.contains("already running") {
            Ok(stdout)
        } else {
            Err(format!("{stdout}{stderr}"))
        }
    }
}

/// Tauri command: restart the LMForge service via the native service manager.
/// Requires the service to be installed; returns an error otherwise.
#[tauri::command]
async fn restart_service() -> Result<String, String> {
    let lmforge_bin = find_lmforge_binary();

    // Windows: same detached-spawn + health-poll strategy as restart_engine.
    // `lmforge service restart` via a captured pipe deadlocks (the respawned
    // daemon inherits the stdout handle), so we drive stop/start ourselves.
    #[cfg(windows)]
    {
        spawn_lmforge_detached(&lmforge_bin, &["service", "stop"])?;
        wait_for_health(false, std::time::Duration::from_secs(15)).await;
        spawn_lmforge_detached(&lmforge_bin, &["service", "start"])?;
        return if wait_for_health(true, std::time::Duration::from_secs(60)).await {
            Ok("LMForge service restarted.".to_string())
        } else {
            Err("Daemon did not become reachable within 60s after restart. Check %USERPROFILE%\\.lmforge\\logs.".to_string())
        };
    }

    #[cfg(not(windows))]
    {
        let output = lmforge_command(&lmforge_bin)
            .args(["service", "restart"])
            .output()
            .await
            .map_err(|e| format!("Failed to spawn lmforge: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            Err(format!("{stdout}{stderr}"))
        }
    }
}

/// Tauri command: query the service installation and running state.
/// Returns `{ installed: bool, running: bool, output: string }`.
#[tauri::command]
async fn get_service_status() -> serde_json::Value {
    let lmforge_bin = find_lmforge_binary();
    match lmforge_command(&lmforge_bin)
        .args(["service", "status"])
        .output()
        .await
    {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).to_string();
            // macOS/Linux print "installed ✓"; Windows prints
            // "registered (HKCU Run key) ✓" — match the parenthetical so we don't
            // miss the ✓ that sits after it (and never match "not registered").
            let installed = out.contains("installed ✓") || out.contains("(HKCU Run key)");
            // Windows has no separate "running" line for autostart; treat the
            // daemon API reachability line as the running signal.
            let running = out.contains("running ✓")
                || out.contains("active (running) ✓")
                || out.contains("reachable at http");
            serde_json::json!({ "installed": installed, "running": running, "output": out })
        }
        Err(e) => {
            serde_json::json!({ "installed": false, "running": false, "error": e.to_string() })
        }
    }
}

/// Check if a LMForge service unit is installed on this host.
/// Only consulted on non-Windows: on Windows restart always drives stop/start
/// detached regardless of the autostart Run key.
#[cfg(not(windows))]
async fn is_service_installed(lmforge_bin: &str) -> bool {
    match lmforge_command(lmforge_bin)
        .args(["service", "status"])
        .output()
        .await
    {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            // macOS/Linux print "installed ✓"; Windows prints
            // "registered (HKCU Run key) ✓".
            out.contains("installed ✓") || out.contains("(HKCU Run key)")
        }
        Err(_) => false,
    }
}

/// Tauri command: stop the LMForge engine via the shutdown API.
/// Only called from the explicit "Stop Engine" tray menu item.
#[tauri::command]
async fn stop_engine() -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    client
        .post("http://127.0.0.1:11430/lf/shutdown")
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;

    Ok(())
}

/// Find the `lmforge` binary. Checks:
/// 1. Next to the current executable (production install)
/// 2. Bundled `bin/` sibling (NSIS UI installer layout)
/// 3. `%LOCALAPPDATA%\lmforge\bin` (install-core.ps1 default)
/// 4. PATH (developer install via `cargo install`)
fn find_lmforge_binary() -> String {
    use std::path::PathBuf;

    let bin_name = if cfg!(windows) {
        "lmforge.exe"
    } else {
        "lmforge"
    };

    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name(bin_name));
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("bin").join(bin_name));
        }
    }

    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("lmforge")
                .join("bin")
                .join(bin_name),
        );
    }

    for path in candidates {
        if path.exists() {
            return path.to_string_lossy().to_string();
        }
    }

    bin_name.to_string()
}
