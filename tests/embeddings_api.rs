//! `/v1/embeddings` — mocked-engine tests for the oversize-input mapping (no GPU).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use lmforge::engine::manager::{EngineMetrics, EngineState, EngineStatus, ManagerCommand};
use lmforge::engine::registry::EngineConfig;
use lmforge::model::index::{ModelCapabilities, ModelEntry, ModelIndex};
use lmforge::server::AppState;

const MODEL: &str = "test-embed:0.6b:8bit";

fn write_model_index(data_dir: &Path) {
    let index = ModelIndex {
        schema_version: 1,
        models: vec![ModelEntry {
            id: MODEL.to_string(),
            path: data_dir
                .join("models/test-embed-dir")
                .to_string_lossy()
                .to_string(),
            format: "gguf".to_string(),
            engine: "llamacpp".to_string(),
            hf_repo: None,
            size_bytes: 0,
            capabilities: ModelCapabilities {
                embeddings: true,
                ..Default::default()
            },
            added_at: "2026-10-06".to_string(),
        }],
        ..Default::default()
    };
    std::fs::write(
        data_dir.join("models.json"),
        serde_json::to_string_pretty(&index).unwrap(),
    )
    .unwrap();
}

fn build_router(data_dir: PathBuf, engine_id: &str, engine_port: u16) -> axum::Router {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ManagerCommand>(64);
    let (status_tx, _) = broadcast::channel(16);
    let engine_config = EngineConfig {
        id: engine_id.to_string(),
        name: format!("{engine_id} (test stub)"),
        version: "0.0.0".to_string(),
        matches_fallback: true,
        health_endpoint: "/health".to_string(),
        supports_embeddings: true,
        supports_reranking: true,
        ..Default::default()
    };
    let engine_state = Arc::new(RwLock::new(EngineState {
        overall_status: EngineStatus::Ready,
        engine_id: engine_config.id.clone(),
        engine_version: engine_config.version.clone(),
        running_models: HashMap::new(),
        metrics: EngineMetrics::default(),
        last_errors: HashMap::new(),
        dismissed_errors: HashMap::new(),
    }));

    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if let ManagerCommand::EnsureModel { reply, .. } = cmd {
                let _ = reply.send(Ok(lmforge::engine::manager::ModelHandle {
                    port: engine_port,
                    inflight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
                }));
            }
        }
    });

    let state = AppState {
        engine_state,
        engine_config,
        residency_kind: lmforge::engine::ResidencyKind::ProcessPool,
        adapter: Arc::new(lmforge::engine::adapter::EngineAdapterInstance::Omlx(
            lmforge::engine::adapters::omlx::OmlxAdapter::default(),
        )),
        models_dir: data_dir.join("models"),
        data_dir,
        api_key: None,
        bind_address: "127.0.0.1:11430".to_string(),
        config: Arc::new(RwLock::new(lmforge::config::LmForgeConfig::default())),
        command_tx: cmd_tx,
        status_tx,
        pull_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        active_pull: Arc::new(RwLock::new(None)),
        migration_status: Arc::new(RwLock::new(None)),
        migration_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let auth_policy = Arc::new(lmforge::server::auth::AuthPolicy::from_config(
        None,
        &[],
        true,
    ));
    let concurrency = lmforge::server::concurrency::ConcurrencyLimit::new(0, 0);
    lmforge::server::build_router(state, auth_policy, concurrency, 32 * 1024 * 1024)
}

async fn post_embeddings(router: &axum::Router, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/embeddings")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn oversize_500() -> ResponseTemplate {
    ResponseTemplate::new(500).set_body_json(json!({"error": {
        "code": 500, "type": "server_error",
        "message": "input (9000 tokens) is larger than the max context size (8192 tokens). skipping"
    }}))
}

#[tokio::test]
async fn oversize_embedding_input_is_a_400_not_an_engine_500() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(oversize_500())
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) =
        post_embeddings(&router, json!({"model": MODEL, "input": "long text"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "input_too_long");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("LMFORGE_LLAMACPP_EMBED_CTX")
    );
}

#[tokio::test]
async fn oversize_input_in_a_batched_request_is_also_a_400() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(oversize_500())
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    // More inputs than embed_batch_size (32) → the batched path.
    let inputs: Vec<String> = (0..40).map(|i| format!("chunk {i}")).collect();
    let (status, body) = post_embeddings(&router, json!({"model": MODEL, "input": inputs})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "input_too_long");
}

#[tokio::test]
async fn other_engine_errors_keep_their_status() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({"error": {"message": "loading"}})),
        )
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, _) = post_embeddings(&router, json!({"model": MODEL, "input": "x"})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
