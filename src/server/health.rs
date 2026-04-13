use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;

use super::AppState;
use crate::engine::manager::EngineStatus;

/// `GET /health` — Health check endpoint
pub async fn health(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let engine_state = state.engine_state.read().await;

    match engine_state.overall_status {
        EngineStatus::Ready => {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"status":"ok"}"#))
                .unwrap()
        }
        EngineStatus::Starting => {
            Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Retry-After", "5")
                .body(Body::from(r#"{"status":"starting","message":"Engine is starting up"}"#))
                .unwrap()
        }
        _ => {
            let resp = serde_json::json!({
                "status": "error",
                "engine_status": engine_state.overall_status,
                "message": "Engine is not ready",
            });
            Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&resp).unwrap()))
                .unwrap()
        }
    }
}
