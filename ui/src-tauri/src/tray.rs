//! System tray management for LMForge — Tauri v2 API.
//!
//! The tray is now fully independent of the daemon's lifecycle:
//! - It polls /health + /lf/status on its own 3-second loop.
//! - "Hide UI"     → hides the Tauri window (daemon unaffected).
//! - "Stop Engine" → calls POST /lf/shutdown (explicit user action).
//! - "Open LMForge" → shows + focuses the main window.

use tauri::{
    AppHandle, Manager,
    image::Image,
    menu::{Menu, MenuItem, MenuEvent},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

// Embedded at compile time.
const ICON_READY:    &[u8] = include_bytes!("../icons/tray-ready-32.png");
const ICON_DEGRADED: &[u8] = include_bytes!("../icons/tray-degraded-32.png");
const ICON_ERROR:    &[u8] = include_bytes!("../icons/tray-error-32.png");
const ICON_OFFLINE:  &[u8] = include_bytes!("../icons/tray-offline-32.png");

/// Set up the system tray. Non-fatal: returns Err on platforms without tray support.
pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show_item  = MenuItem::with_id(app, "show",   "Open LMForge", true, None::<&str>)?;
    let hide_item  = MenuItem::with_id(app, "hide",   "Hide UI",      true, None::<&str>)?;
    let stop_item  = MenuItem::with_id(app, "stop",   "Stop Engine",  true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_item, &hide_item, &stop_item])?;
    let icon = Image::from_bytes(ICON_OFFLINE)?;

    let app_menu  = app.clone();
    let app_click = app.clone();

    TrayIconBuilder::with_id("lmforge-tray")
        .icon(icon)
        .tooltip("LMForge — checking...")
        .menu(&menu)
        .on_menu_event(move |_app, event: MenuEvent| {
            match event.id.as_ref() {
                "show" => {
                    if let Some(win) = app_menu.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
                "hide" => {
                    // Hide the UI window — daemon keeps running ✓
                    if let Some(win) = app_menu.get_webview_window("main") {
                        let _ = win.hide();
                    }
                }
                "stop" => {
                    // Explicit user action: shut down the daemon via API.
                    let app_clone = app_menu.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = stop_daemon_via_api().await;
                        // After stopping, hide the window too.
                        if let Some(win) = app_clone.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    });
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |_tray, event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event {
                if let Some(win) = app_click.get_webview_window("main") {
                    let visible = win.is_visible().unwrap_or(false);
                    if visible { let _ = win.hide(); }
                    else { let _ = win.show(); let _ = win.set_focus(); }
                }
            }
        })
        .build(app)?;

    // Start independent tray status polling loop.
    let app_poll = app.clone();
    tauri::async_runtime::spawn(tray_poll_loop(app_poll));

    Ok(())
}

/// Poll /health and /lf/status every 3 s to keep the tray icon up to date.
/// Fully independent — does not depend on the broadcast channel.
async fn tray_poll_loop(app: AppHandle) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    loop {
        let online = client
            .get("http://127.0.0.1:11430/health")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if !online {
            set_tray_offline(&app);
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            continue;
        }

        // Fetch engine state for model count
        let state = client
            .get("http://127.0.0.1:11430/lf/status")
            .send()
            .await
            .ok()
            .and_then(|r| {
                // blocking parse in async context is fine for a small payload
                tauri::async_runtime::block_on(r.json::<serde_json::Value>()).ok()
            });

        update_tray_from_json(&app, state.as_ref());
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

fn set_tray_offline(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("lmforge-tray") else { return };
    if let Ok(icon) = Image::from_bytes(ICON_OFFLINE) {
        let _ = tray.set_icon(Some(icon));
    }
    let _ = tray.set_tooltip(Some("LMForge — Offline"));
}

fn update_tray_from_json(app: &AppHandle, state: Option<&serde_json::Value>) {
    let Some(tray) = app.tray_by_id("lmforge-tray") else { return };

    let status = state
        .and_then(|s| s.get("overall_status"))
        .and_then(|s| s.as_str())
        .unwrap_or("stopped");

    let n = state
        .and_then(|s| s.get("running_models"))
        .and_then(|m| m.as_object())
        .map(|m| m.len())
        .unwrap_or(0);

    let icon_bytes: &[u8] = match status {
        "ready"    => ICON_READY,
        "degraded" => ICON_DEGRADED,
        "error"    => ICON_ERROR,
        _          => ICON_OFFLINE,
    };

    let tooltip = match status {
        "ready" if n == 0 => "LMForge — Ready".to_string(),
        "ready"           => format!("LMForge — {} model{} loaded", n, if n == 1 { "" } else { "s" }),
        "starting"        => "LMForge — Starting…".to_string(),
        "degraded"        => "LMForge — Degraded".to_string(),
        "error"           => "LMForge — Error".to_string(),
        _                 => "LMForge — Offline".to_string(),
    };

    if let Ok(icon) = Image::from_bytes(icon_bytes) {
        let _ = tray.set_icon(Some(icon));
    }
    let _ = tray.set_tooltip(Some(&tooltip));
}

async fn stop_daemon_via_api() -> Result<(), reqwest::Error> {
    reqwest::Client::new()
        .post("http://127.0.0.1:11430/lf/shutdown")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;
    Ok(())
}
