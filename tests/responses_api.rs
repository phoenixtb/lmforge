//! `/v1/responses` adapter — mocked-engine integration tests (no GPU).
//!
//! Drives the full router (auth → handler → chat path → proxy) against a
//! wiremock engine, asserting both what the engine receives (translated chat
//! body) and what the client gets back (Responses shapes).

use std::collections::HashMap;
use std::path::PathBuf;
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

const MODEL: &str = "test-chat:1b:4bit";

fn write_model_index(dir: &std::path::Path) {
    let index = ModelIndex {
        schema_version: 1,
        models: vec![ModelEntry {
            id: MODEL.to_string(),
            path: dir
                .join("models")
                .join("test-chat-dir")
                .to_string_lossy()
                .to_string(),
            format: "mlx".to_string(),
            engine: "omlx".to_string(),
            hf_repo: None,
            size_bytes: 0,
            capabilities: ModelCapabilities {
                chat: true,
                ..Default::default()
            },
            added_at: "2025-01-01".to_string(),
        }],
        ..Default::default()
    };
    std::fs::write(
        dir.join("models.json"),
        serde_json::to_string_pretty(&index).unwrap(),
    )
    .unwrap();
}

fn build_router(data_dir: PathBuf, engine_port: u16) -> axum::Router {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ManagerCommand>(64);
    let (status_tx, _) = broadcast::channel(16);
    let engine_config = EngineConfig {
        id: "omlx".to_string(),
        name: "oMLX (test stub)".to_string(),
        version: "0.3.0".to_string(),
        matches_fallback: true,
        install_method: "brew".to_string(),
        model_format: "mlx".to_string(),
        hf_org: "mlx-community".to_string(),
        start_cmd: "omlx".to_string(),
        health_endpoint: "/health".to_string(),
        supports_embeddings: true,
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

async fn post_raw(router: &axum::Router, body: Value) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn post_json(router: &axum::Router, body: Value) -> (StatusCode, Value) {
    let (s, t) = post_raw(router, body).await;
    (s, serde_json::from_str(&t).unwrap_or(Value::Null))
}

fn parse_sse(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter(|f| !f.trim().is_empty())
        .map(|f| {
            let mut lines = f.lines();
            let ev = lines
                .next()
                .unwrap()
                .strip_prefix("event: ")
                .unwrap()
                .to_string();
            let data = lines.next().unwrap().strip_prefix("data: ").unwrap();
            (ev, serde_json::from_str(data).unwrap())
        })
        .collect()
}

async fn engine_request_body(server: &MockServer) -> Value {
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "engine should see exactly one request");
    serde_json::from_slice(&reqs[0].body).unwrap()
}

#[tokio::test]
async fn non_stream_text_roundtrip_and_translated_engine_body() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1", "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13}
        })))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), engine.address().port());

    let (status, body) = post_json(
        &router,
        json!({
            "model": MODEL, "input": "ping", "instructions": "be terse",
            "max_output_tokens": 32, "temperature": 0.1, "store": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(body["output"][0]["content"][0]["text"], "pong");
    assert_eq!(body["output_text"], "pong");
    assert_eq!(body["usage"]["input_tokens"], 11);
    assert_eq!(body["usage"]["output_tokens"], 2);
    assert_eq!(body["usage"]["total_tokens"], 13);

    // Reaches the engine through the real chat path: model rewritten to the
    // on-disk dir name, Responses fields translated.
    let sent = engine_request_body(&engine).await;
    assert_eq!(sent["model"], "test-chat-dir");
    assert_eq!(sent["max_tokens"], 32);
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(
        sent["messages"][1],
        json!({"role": "user", "content": "ping"})
    );
    assert!(sent.get("input").is_none());
}

#[tokio::test]
async fn non_stream_tool_call_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"location\":\"Paris\"}"}}
            ]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7, "total_tokens": 12}
        })))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), engine.address().port());

    let (status, body) = post_json(
        &router,
        json!({
            "model": MODEL, "input": "weather in Paris?",
            "tools": [{"type": "function", "name": "get_weather", "parameters": {"type": "object"}}],
            "tool_choice": {"type": "function", "name": "get_weather"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["output"][0]["type"], "function_call");
    assert_eq!(body["output"][0]["call_id"], "call_1");
    assert_eq!(body["output"][0]["name"], "get_weather");
    assert_eq!(body["output"][0]["arguments"], "{\"location\":\"Paris\"}");

    let sent = engine_request_body(&engine).await;
    assert_eq!(sent["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(sent["tool_choice"]["function"]["name"], "get_weather");
}

#[tokio::test]
async fn stream_text_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c\",\"model\":\"test-chat-dir\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"po\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ng\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"c\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), engine.address().port());

    let (status, text) = post_raw(
        &router,
        json!({"model": MODEL, "input": "ping", "stream": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!text.contains("[DONE]"));
    let evs = parse_sse(&text);
    for (i, (ev, v)) in evs.iter().enumerate() {
        assert_eq!(v["type"], ev.as_str());
        assert_eq!(v["sequence_number"], i as u64);
    }
    assert_eq!(evs[0].0, "response.created");
    assert_eq!(evs[1].0, "response.in_progress");
    let deltas: String = evs
        .iter()
        .filter(|(e, _)| e == "response.output_text.delta")
        .map(|(_, v)| v["delta"].as_str().unwrap())
        .collect();
    assert_eq!(deltas, "pong");
    let (last_ev, last) = evs.last().unwrap();
    assert_eq!(last_ev, "response.completed");
    assert_eq!(last["response"]["status"], "completed");
    assert_eq!(last["response"]["usage"]["total_tokens"], 5);
    assert_eq!(last["response"]["output"][0]["content"][0]["text"], "pong");

    let sent = engine_request_body(&engine).await;
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn previous_response_id_is_400_and_never_reaches_engine() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    let router = build_router(tmp.path().to_owned(), engine.address().port());

    let (status, body) = post_json(
        &router,
        json!({"model": MODEL, "input": "hi", "previous_response_id": "resp_123"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "previous_response_id");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stateless")
    );
    assert!(engine.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_json_is_400() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    let router = build_router(tmp.path().to_owned(), engine.address().port());
    let req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn engine_error_keeps_status_and_openai_error_shape() {
    let tmp = tempfile::tempdir().unwrap();
    write_model_index(tmp.path());
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "slow down", "type": "rate_limit_error", "param": null, "code": null}
        })))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), engine.address().port());

    let (status, body) = post_json(&router, json!({"model": MODEL, "input": "hi"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["message"], "slow down");
    assert_eq!(body["error"]["type"], "rate_limit_error");
}
