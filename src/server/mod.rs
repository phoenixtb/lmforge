pub mod auth;
pub mod health;
pub mod native;
pub mod ollama;
pub mod openai;
pub mod proxy;
pub mod thinking;

use std::sync::Arc;
use axum::{Router, routing::{get, post, delete}};
use tokio::sync::RwLock;
use tracing::info;

use crate::engine::adapter::EngineAdapterInstance;
use crate::engine::manager::{EngineState, ManagerCommand};
use crate::engine::registry::EngineConfig;

/// Shared application state passed to all handlers
#[derive(Clone)]
pub struct AppState {
    pub engine_state: Arc<RwLock<EngineState>>,
    pub engine_config: EngineConfig,
    /// Shared adapter — used by model_pull to attempt engine-native downloads.
    pub adapter: Arc<EngineAdapterInstance>,
    pub data_dir: std::path::PathBuf,
    pub api_key: Option<String>,
    pub bind_address: String,
    pub config: Arc<RwLock<crate::config::LmForgeConfig>>,
    pub command_tx: tokio::sync::mpsc::Sender<ManagerCommand>,
}

impl AppState {
    pub async fn ensure_model(&self, model_id: &str, keep_alive: Option<String>) -> Result<u16, axum::http::Response<axum::body::Body>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = ManagerCommand::EnsureModel {
            model_id: model_id.to_string(),
            keep_alive_override: keep_alive,
            reply: tx,
        };
        if self.command_tx.send(cmd).await.is_err() {
            return Err(
                axum::http::Response::builder()
                    .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .body(axum::body::Body::from(r#"{"error":{"message":"Orchestrator channel closed"}}"#))
                    .unwrap()
            );
        }
        match rx.await {
            Ok(Ok(port)) => Ok(port),
            Ok(Err(e)) => Err(
                axum::http::Response::builder()
                    .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                    .body(axum::body::Body::from(format!(r#"{{"error":{{"message":"Failed to load model: {}"}}}}"#, e)))
                    .unwrap()
            ),
            Err(_) => Err(
                axum::http::Response::builder()
                    .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .body(axum::body::Body::from(r#"{"error":{"message":"Orchestrator failed to reply"}}"#))
                    .unwrap()
            ),
        }
    }
}

/// Build the full axum Router with all routes
pub fn build_router(state: AppState) -> Router {
    info!("Building API router");

    Router::new()
        // OpenAI-compatible endpoints
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/completions", post(openai::completions))
        .route("/v1/embeddings", post(openai::embeddings))
        .route("/v1/models", get(openai::models))
        // Ollama-compatible endpoints
        .route("/api/chat", post(ollama::chat))
        .route("/api/generate", post(ollama::generate))
        .route("/api/tags", get(ollama::tags))
        // LMForge native endpoints
        .route("/lf/status", get(native::status))
        .route("/lf/hardware", get(native::hardware))
        .route("/lf/model/list", get(native::model_list))
        .route("/lf/model/switch", post(native::model_switch))
        .route("/lf/model/pull", post(native::model_pull))
        .route("/lf/model/unload", post(native::model_unload))
        .route("/lf/model/delete/{name}", delete(native::model_delete))
        .route("/lf/config", get(native::config_get).post(native::config_update))
        .route("/lf/shutdown", post(native::shutdown))
        // Health
        .route("/health", get(health::health))
        // State
        .with_state(state)
        // Global CORS to allow local UIs (Tauri, Web) to connect
        .layer(tower_http::cors::CorsLayer::permissive())
}
