//! Multi-model integration tests — Layer 1 (mocked, no GPU required)
//!
//! These tests exercise the orchestrator and HTTP handler layer without
//! spawning real engine sub-processes. A `FakeEngineManager` task handles
//! `ManagerCommand::EnsureModel` by returning pre-configured ports, while
//! `wiremock` stands up real HTTP servers on those ports to receive proxied
//! requests.
//!
//! Run with:
//!   cargo test multi_model -- --nocapture
//!
//! Default models (used only in doc comments / error messages; no real pull needed):
//!   EMBED_MODEL = qwen3-embed:0.6b:4bit   (overridable via env var)
//!   CHAT_MODEL  = qwen3.5:4b:4bit         (overridable via env var)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::future::join_all;
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};
use tower::ServiceExt; // for `oneshot`
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use lmforge::engine::manager::{EngineMetrics, EngineState, EngineStatus, ManagerCommand};
use lmforge::engine::registry::EngineConfig;
use lmforge::model::index::{ModelCapabilities, ModelEntry, ModelIndex};
use lmforge::server::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Model ID constants — change here to test with different models.
// These can also be overridden via env vars LMFORGE_EMBED_MODEL / LMFORGE_CHAT_MODEL.
// ─────────────────────────────────────────────────────────────────────────────
const DEFAULT_EMBED_MODEL: &str = "qwen3-embed:0.6b:4bit";
const DEFAULT_CHAT_MODEL: &str = "qwen3.5:4b:4bit";

fn embed_model() -> String {
    std::env::var("LMFORGE_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
}

fn chat_model() -> String {
    std::env::var("LMFORGE_CHAT_MODEL").unwrap_or_else(|_| DEFAULT_CHAT_MODEL.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Test Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// A fake engine config — enough to satisfy AppState construction.
fn fake_engine_config() -> EngineConfig {
    EngineConfig {
        id: "omlx".to_string(),
        name: "oMLX (test stub)".to_string(),
        version: "0.3.0".to_string(),
        matches_os: None,
        matches_arch: None,
        matches_gpu: None,
        min_vram_gb: None,
        matches_fallback: true,
        install_method: "brew".to_string(),
        brew_tap: None,
        brew_formula: None,
        pip_fallback: None,
        pip_package: None,
        preflight: vec![],
        min_disk_gb: None,
        binary: None,
        release_url: None,
        asset_pattern: None,
        model_format: "mlx".to_string(),
        hf_org: "mlx-community".to_string(),
        start_cmd: "omlx".to_string(),
        start_args: vec![],
        health_endpoint: "/health".to_string(),
        supports_embeddings: true,
        supports_reranking: false,
        brew_tap_url: None,
        cudart_pattern: None,
        priority: 0,
    }
}

/// Minimal `models.json` written to a temp dir.  
/// Returns the temp dir (must stay alive for the test duration).
fn write_model_index(
    dir: &std::path::Path,
    entries: &[(
        &str, // model id
        &str, // path suffix
        bool, // chat
        bool, // embeddings
    )],
) {
    let models: Vec<ModelEntry> = entries
        .iter()
        .map(|(id, suffix, chat, embeddings)| ModelEntry {
            id: id.to_string(),
            path: dir
                .join("models")
                .join(suffix)
                .to_string_lossy()
                .to_string(),
            format: "mlx".to_string(),
            engine: "omlx".to_string(),
            hf_repo: None,
            size_bytes: 0,
            capabilities: ModelCapabilities {
                chat: *chat,
                embeddings: *embeddings,
                reranking: false,
                thinking: false,
                embedding_dims: if *embeddings { Some(1536) } else { None },
                pooling: None,
            },
            added_at: "2025-01-01".to_string(),
        })
        .collect();

    let index = ModelIndex {
        schema_version: 1,
        models,
    };
    std::fs::write(
        dir.join("models.json"),
        serde_json::to_string_pretty(&index).unwrap(),
    )
    .unwrap();
}

/// A `FakeEngineManager` that runs in a background Tokio task.
///
/// Instead of loading real models, it resolves `EnsureModel` commands by
/// looking up the model ID in a pre-configured `port_map`. This lets tests
/// route requests to wiremock servers without any subprocess spawning.
fn spawn_fake_manager(port_map: HashMap<String, u16>, mut cmd_rx: mpsc::Receiver<ManagerCommand>) {
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                ManagerCommand::EnsureModel {
                    model_id,
                    keep_alive_override: _,
                    reply,
                } => {
                    let result = port_map.get(&model_id).copied().ok_or_else(|| {
                        anyhow::anyhow!("FakeEngineManager: unknown model '{}'", model_id)
                    });
                    let _ = reply.send(result);
                }
                ManagerCommand::UnloadModel(_) | ManagerCommand::UnloadAll => {}
            }
        }
    });
}

/// Build a minimal `AppState` wired to a `FakeEngineManager`.
fn build_app_state(
    data_dir: PathBuf,
    port_map: HashMap<String, u16>,
    embed_batch_size: usize,
) -> (axum::Router, broadcast::Sender<EngineState>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (status_tx, _) = broadcast::channel(16);
    let engine_config = fake_engine_config();

    let engine_state = Arc::new(RwLock::new(EngineState {
        overall_status: EngineStatus::Ready,
        engine_id: engine_config.id.clone(),
        engine_version: engine_config.version.clone(),
        running_models: HashMap::new(),
        metrics: EngineMetrics::default(),
    }));

    let mut cfg = lmforge::config::LmForgeConfig::default();
    cfg.orchestrator.embed_batch_size = embed_batch_size;

    let state = AppState {
        engine_state,
        engine_config,
        adapter: Arc::new(lmforge::engine::adapter::EngineAdapterInstance::Omlx(
            lmforge::engine::adapters::omlx::OmlxAdapter::default(),
        )),
        data_dir,
        api_key: None,
        bind_address: "127.0.0.1:11430".to_string(),
        config: Arc::new(RwLock::new(cfg)),
        command_tx: cmd_tx,
        status_tx: status_tx.clone(),
    };

    spawn_fake_manager(port_map, cmd_rx);

    let router = lmforge::server::build_router(state);
    (router, status_tx)
}

/// Fire a `POST` request through the axum router as a `tower::Service`.
async fn post(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-01 — Embed model rejected at /v1/chat/completions
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc01_embed_model_rejected_at_chat_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let embed = embed_model();

    write_model_index(tmp.path(), &[(embed.as_str(), "embed", false, true)]);

    let (router, _) = build_app_state(tmp.path().to_owned(), HashMap::new(), 128);

    let (status, body) = post(
        &router,
        "/v1/chat/completions",
        json!({
            "model": embed,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "Expected 400, got: {body}");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("embedding") || msg.contains("embed"),
        "Error message should mention 'embedding', got: {msg}"
    );
    println!("✓ TC-01 passed — embed model correctly rejected at chat endpoint");
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-02 — Chat model rejected at /v1/embeddings
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc02_chat_model_rejected_at_embeddings_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let chat = chat_model();

    write_model_index(tmp.path(), &[(chat.as_str(), "chat", true, false)]);

    let (router, _) = build_app_state(tmp.path().to_owned(), HashMap::new(), 128);

    let (status, body) = post(
        &router,
        "/v1/embeddings",
        json!({"model": chat, "input": "hello world"}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "Expected 400, got: {body}");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("embedding") || msg.contains("embed"),
        "Error message should mention embedding capability, got: {msg}"
    );
    println!("✓ TC-02 passed — chat model correctly rejected at embeddings endpoint");
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-03 — Correct port dispatch: embed → port-A, chat → port-B
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc03_correct_port_dispatch_two_models() {
    let tmp = tempfile::tempdir().unwrap();
    let embed = embed_model();
    let chat = chat_model();

    write_model_index(
        tmp.path(),
        &[
            (embed.as_str(), "embed", false, true),
            (chat.as_str(), "chat", true, false),
        ],
    );

    // Start two mock engine servers
    let embed_server = MockServer::start().await;
    let chat_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "embedding": vec![0.1f32; 16], "index": 0}],
            "model": embed,
            "usage": {"prompt_tokens": 5, "total_tokens": 5}
        })))
        .mount(&embed_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "choices": [{"message": {"role": "assistant", "content": "hello"}, "index": 0, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
        })))
        .mount(&chat_server)
        .await;

    let port_map = HashMap::from([
        (embed.clone(), embed_server.address().port()),
        (chat.clone(), chat_server.address().port()),
    ]);

    let (router, _) = build_app_state(tmp.path().to_owned(), port_map, 128);

    let (s1, b1) = post(
        &router,
        "/v1/embeddings",
        json!({"model": embed, "input": "test embedding"}),
    )
    .await;

    let (s2, b2) = post(
        &router,
        "/v1/chat/completions",
        json!({"model": chat, "messages": [{"role": "user", "content": "hi"}], "stream": false}),
    )
    .await;

    assert_eq!(s1, StatusCode::OK, "Embed failed: {b1}");
    assert_eq!(s2, StatusCode::OK, "Chat failed: {b2}");

    // Validate each landed on its own server
    assert_eq!(
        embed_server.received_requests().await.unwrap().len(),
        1,
        "Embed server should have 1 request"
    );
    assert_eq!(
        chat_server.received_requests().await.unwrap().len(),
        1,
        "Chat server should have 1 request"
    );

    println!("✓ TC-03 passed — embed and chat requests correctly dispatched to separate ports");
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-04 — Concurrent 10-embed burst
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc04_concurrent_10_embed_requests() {
    const N: usize = 10;
    let tmp = tempfile::tempdir().unwrap();
    let embed = embed_model();

    write_model_index(tmp.path(), &[(embed.as_str(), "embed", false, true)]);

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "embedding": vec![0.0f32; 1536], "index": 0}],
            "model": embed,
            "usage": {"prompt_tokens": 3, "total_tokens": 3}
        })))
        .expect(N as u64)
        .mount(&mock_server)
        .await;

    let port_map = HashMap::from([(embed.clone(), mock_server.address().port())]);
    let (router, _) = build_app_state(tmp.path().to_owned(), port_map, 128);

    let start = Instant::now();
    let tasks: Vec<_> = (0..N)
        .map(|i| {
            let r = router.clone();
            let m = embed.clone();
            tokio::spawn(async move {
                post(
                    &r,
                    "/v1/embeddings",
                    json!({"model": m, "input": format!("concurrent test sentence {i}")}),
                )
                .await
            })
        })
        .collect();

    let results = join_all(tasks).await;
    let elapsed = start.elapsed();

    let mut ok_count = 0;
    for (i, r) in results.iter().enumerate() {
        let (status, body) = r.as_ref().unwrap();
        assert_eq!(*status, StatusCode::OK, "Request {i} failed: {body}");
        assert!(
            body["data"].is_array(),
            "Request {i} response missing 'data': {body}"
        );
        ok_count += 1;
    }

    mock_server.verify().await;
    println!("✓ TC-04 passed — {ok_count}/{N} concurrent embed requests succeeded in {elapsed:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-05 — Sequential 10-chat completions
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc05_sequential_10_chat_completions() {
    const N: usize = 10;
    let tmp = tempfile::tempdir().unwrap();
    let chat = chat_model();

    write_model_index(tmp.path(), &[(chat.as_str(), "chat", true, false)]);

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-seq",
            "object": "chat.completion",
            "choices": [{"message": {"role": "assistant", "content": "pong"}, "index": 0, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5}
        })))
        .expect(N as u64)
        .mount(&mock_server)
        .await;

    let port_map = HashMap::from([(chat.clone(), mock_server.address().port())]);
    let (router, _) = build_app_state(tmp.path().to_owned(), port_map, 128);

    let mut latencies = Vec::with_capacity(N);
    for i in 0..N {
        let t = Instant::now();
        let (status, body) = post(
            &router,
            "/v1/chat/completions",
            json!({
                "model": chat,
                "messages": [{"role": "user", "content": format!("ping {i}")}],
                "stream": false
            }),
        )
        .await;
        latencies.push(t.elapsed());
        assert_eq!(status, StatusCode::OK, "Request {i} failed: {body}");
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        assert!(!content.is_empty(), "Request {i} returned empty content");
    }

    mock_server.verify().await;

    let avg_ms = latencies.iter().map(|d| d.as_millis()).sum::<u128>() / N as u128;
    println!("✓ TC-05 passed — {N} sequential chat completions, avg latency: {avg_ms}ms");
    for (i, l) in latencies.iter().enumerate() {
        println!("   req[{i:02}] = {}ms", l.as_millis());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-06 — Interleaved embed + chat, 10 of each (20 total, sequential)
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc06_interleaved_embed_and_chat_requests() {
    const N: usize = 10;
    let tmp = tempfile::tempdir().unwrap();
    let embed = embed_model();
    let chat = chat_model();

    write_model_index(
        tmp.path(),
        &[
            (embed.as_str(), "embed", false, true),
            (chat.as_str(), "chat", true, false),
        ],
    );

    let embed_server = MockServer::start().await;
    let chat_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "embedding": vec![0.5f32; 1536], "index": 0}],
            "model": embed,
            "usage": {"prompt_tokens": 3, "total_tokens": 3}
        })))
        .expect(N as u64)
        .mount(&embed_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-interleaved",
            "object": "chat.completion",
            "choices": [{"message": {"role": "assistant", "content": "ack"}, "index": 0, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5}
        })))
        .expect(N as u64)
        .mount(&chat_server)
        .await;

    let port_map = HashMap::from([
        (embed.clone(), embed_server.address().port()),
        (chat.clone(), chat_server.address().port()),
    ]);
    let (router, _) = build_app_state(tmp.path().to_owned(), port_map, 128);

    // Alternate: embed, chat, embed, chat... × N
    for i in 0..N {
        let (s1, b1) = post(
            &router,
            "/v1/embeddings",
            json!({"model": embed, "input": format!("interleaved embed {i}")}),
        )
        .await;
        assert_eq!(s1, StatusCode::OK, "Embed req {i} failed: {b1}");

        let (s2, b2) = post(
            &router,
            "/v1/chat/completions",
            json!({"model": chat, "messages": [{"role": "user", "content": format!("turn {i}")}], "stream": false}),
        )
        .await;
        assert_eq!(s2, StatusCode::OK, "Chat req {i} failed: {b2}");
    }

    embed_server.verify().await;
    chat_server.verify().await;

    println!(
        "✓ TC-06 passed — {N} interleaved embed+chat pairs, all routed correctly (embed→port:{}, chat→port:{})",
        embed_server.address().port(),
        chat_server.address().port()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-07 — Role mismatch: same model loaded as Embed, chat request should fail
//
// Note: because we are using a FakeEngineManager (not the real EngineManager),
// the role-mismatch guard inside `handle_ensure_model` is not executed here.
// What we DO validate is the *capability gate* in the HTTP handler — the
// models.json says chat=false for the embed model, so the handler rejects the
// request with 400 before it ever reaches the manager.
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc07_role_mismatch_embed_model_cannot_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let embed = embed_model();

    // The model is registered as embed-only
    write_model_index(tmp.path(), &[(embed.as_str(), "embed", false, true)]);

    let (router, _) = build_app_state(tmp.path().to_owned(), HashMap::new(), 128);

    let (status, body) = post(
        &router,
        "/v1/chat/completions",
        json!({
            "model": embed,
            "messages": [{"role": "user", "content": "are you a chat model?"}],
            "stream": false
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "Should be rejected before reaching manager. Got: {body}"
    );
    println!("✓ TC-07 passed — role mismatch rejected at capability gate (HTTP 400)");
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-08 — Batch embedding chunking: 30 inputs split across 3 engine calls
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn tc08_batch_embedding_chunking() {
    const TOTAL_INPUTS: usize = 30;
    const BATCH_SIZE: usize = 10;
    const EXPECTED_CALLS: u64 = (TOTAL_INPUTS / BATCH_SIZE) as u64; // 3

    let tmp = tempfile::tempdir().unwrap();
    let embed = embed_model();

    write_model_index(tmp.path(), &[(embed.as_str(), "embed", false, true)]);

    let mock_server = MockServer::start().await;

    // The mock returns a single-item data[] per call; the orchestrator merges them.
    // We use a batch_size=10 mock that returns 10 items each time.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": (0..BATCH_SIZE).map(|i| json!({
                "object": "embedding",
                "embedding": vec![0.1f32; 16],
                "index": i
            })).collect::<Vec<_>>(),
            "model": embed,
            "usage": {"prompt_tokens": 10, "total_tokens": 10}
        })))
        .expect(EXPECTED_CALLS)
        .mount(&mock_server)
        .await;

    let port_map = HashMap::from([(embed.clone(), mock_server.address().port())]);
    let (router, _) = build_app_state(tmp.path().to_owned(), port_map, BATCH_SIZE);

    let inputs: Vec<Value> = (0..TOTAL_INPUTS)
        .map(|i| json!(format!("sentence number {i}")))
        .collect();

    let (status, body) = post(
        &router,
        "/v1/embeddings",
        json!({"model": embed, "input": inputs}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "Batch embed failed: {body}");

    let data = body["data"]
        .as_array()
        .expect("Response should have 'data' array");
    assert_eq!(
        data.len(),
        TOTAL_INPUTS,
        "Merged response should have {TOTAL_INPUTS} items, got {}",
        data.len()
    );

    // Verify index fields are re-numbered sequentially 0..29
    for (i, item) in data.iter().enumerate() {
        let idx = item["index"]
            .as_u64()
            .expect("each item should have an 'index'");
        assert_eq!(idx, i as u64, "Item {i} has wrong index: {idx}");
    }

    mock_server.verify().await;
    println!(
        "✓ TC-08 passed — {TOTAL_INPUTS} inputs split into {EXPECTED_CALLS} engine calls of {BATCH_SIZE}, \
         merged with correct sequential indices"
    );
}
