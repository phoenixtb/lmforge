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
        .invoke_handler(tauri::generate_handler![start_engine, stop_engine])
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    let base_url = "http://127.0.0.1:11430";

    // Let the WebView mount and register listen() before the first event.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    loop {
        // 1. Health check — emit every cycle; the store handles idempotent updates.
        let health_ok = client
            .get(format!("{}/health", base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        let _ = app.emit("lf:health", serde_json::json!({ "online": health_ok }));

        if !health_ok {
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
                    }
                }),
            );
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        }

        // 2. Fetch full status snapshot when online.
        if let Ok(resp) = client.get(format!("{}/lf/status", base_url)).send().await {
            if let Ok(body) = resp.text().await {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    let _ = app.emit("lf:status", &json);
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Tauri command: start the LMForge engine (called from the "Engine offline" screen).
/// Uses `lmforge start` which is idempotent — safe to call even if already running.
#[tauri::command]
async fn start_engine() -> Result<String, String> {
    let lmforge_bin = find_lmforge_binary();
    let output = tokio::process::Command::new(&lmforge_bin)
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
/// 2. PATH (developer install via `cargo install`)
fn find_lmforge_binary() -> String {
    // Binary name differs by platform
    let bin_name = if cfg!(windows) {
        "lmforge.exe"
    } else {
        "lmforge"
    };

    // 1. Sibling of the current exe (bundled install)
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(bin_name);
        if sibling.exists() {
            return sibling.to_string_lossy().to_string();
        }
    }
    // 2. Rely on PATH
    bin_name.to_string()
}
