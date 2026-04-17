use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;

use super::AppState;
use crate::engine::manager::EngineStatus;

/// `GET /health` — Health check endpoint
///
/// Returns version and min_ui_version so the desktop client can perform
/// a compatibility check on startup before showing the main UI.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let engine_state = state.engine_state.read().await;

    // Semver from Cargo.toml; injected at compile time.
    let version = env!("CARGO_PKG_VERSION");
    // Bump this when there is a breaking API change that requires a UI update.
    let min_ui_version = "0.3.0";

    match engine_state.overall_status {
        EngineStatus::Ready => {
            let body = serde_json::json!({
                "status": "ok",
                "version": version,
                "min_ui_version": min_ui_version,
            });
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap()
        }
        EngineStatus::Starting => {
            let body = serde_json::json!({
                "status": "starting",
                "version": version,
                "min_ui_version": min_ui_version,
                "message": "Engine is starting up",
            });
            Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Retry-After", "5")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap()
        }
        _ => {
            let body = serde_json::json!({
                "status": "error",
                "version": version,
                "min_ui_version": min_ui_version,
                "engine_status": engine_state.overall_status,
                "message": "Engine is not ready",
            });
            Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap()
        }
    }
}
