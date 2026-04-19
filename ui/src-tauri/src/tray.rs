//! System tray management for LMForge — Tauri v2 API.
//!
//! The tray is fully independent of the daemon's lifecycle. The daemon runs
//! as a background service and is NOT coupled to the UI process.
//!
//! Menu items:
//!   "Open LMForge UI" → show + focus the main window.
//!   "Quit LMForge UI" → exit the Tauri shell (daemon keeps running).
//!
//! Left-clicking the tray icon toggles window visibility (show ↔ hide).

use tauri::{
    image::Image,
    menu::{Menu, MenuEvent, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

// Embedded at compile time.
const ICON_READY: &[u8] = include_bytes!("../icons/tray-ready-32.png");
const ICON_DEGRADED: &[u8] = include_bytes!("../icons/tray-degraded-32.png");
const ICON_ERROR: &[u8] = include_bytes!("../icons/tray-error-32.png");
const ICON_OFFLINE: &[u8] = include_bytes!("../icons/tray-offline-32.png");

/// Set up the system tray. Non-fatal: returns Err on platforms without tray support.
pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let open_item = MenuItem::with_id(app, "open", "Open LMForge UI", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit LMForge UI", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;
    let icon = Image::from_bytes(ICON_OFFLINE)?;

    let app_menu = app.clone();
    let app_click = app.clone();

    TrayIconBuilder::with_id("lmforge-tray")
        .icon(icon)
        .tooltip("LMForge — checking...")
        .menu(&menu)
        .on_menu_event(move |_app, event: MenuEvent| {
            match event.id.as_ref() {
                "open" => {
                    if let Some(win) = app_menu.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
                "quit" => {
                    // Exit the Tauri UI process only — the daemon keeps running.
                    app_menu.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |_tray, event: TrayIconEvent| {
            // Left-click toggles window visibility.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(win) = app_click.get_webview_window("main") {
                    let visible = win.is_visible().unwrap_or(false);
                    if visible {
                        let _ = win.hide();
                    } else {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
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

        let state: Option<serde_json::Value> = async {
            let resp = client
                .get("http://127.0.0.1:11430/lf/status")
                .send()
                .await
                .ok()?;
            resp.json::<serde_json::Value>().await.ok()
        }
        .await;

        update_tray_from_json(&app, state.as_ref());
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

fn set_tray_offline(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("lmforge-tray") else {
        return;
    };
    if let Ok(icon) = Image::from_bytes(ICON_OFFLINE) {
        let _ = tray.set_icon(Some(icon));
    }
    let _ = tray.set_tooltip(Some("LMForge — Offline"));
}

fn update_tray_from_json(app: &AppHandle, state: Option<&serde_json::Value>) {
    let Some(tray) = app.tray_by_id("lmforge-tray") else {
        return;
    };

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
        "ready" => ICON_READY,
        "degraded" => ICON_DEGRADED,
        "error" => ICON_ERROR,
        _ => ICON_OFFLINE,
    };

    let tooltip = match status {
        "ready" if n == 0 => "LMForge — Ready".to_string(),
        "ready" => format!(
            "LMForge — {} model{} loaded",
            n,
            if n == 1 { "" } else { "s" }
        ),
        "starting" => "LMForge — Starting…".to_string(),
        "degraded" => "LMForge — Degraded".to_string(),
        "error" => "LMForge — Error".to_string(),
        _ => "LMForge — Offline".to_string(),
    };

    if let Ok(icon) = Image::from_bytes(icon_bytes) {
        let _ = tray.set_icon(Some(icon));
    }
    let _ = tray.set_tooltip(Some(&tooltip));
}
