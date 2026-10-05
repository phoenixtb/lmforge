//! `/v1/rerank` — mocked-engine integration tests (no GPU).
//!
//! Drives the full router (auth → handler → engine) against a wiremock
//! engine, with synthetic GGUF headers on disk standing in for the served
//! model. Covers per-engine score calibration, the headless-GGUF refusal,
//! per-document truncation to the pooled window, and engine-error mapping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, Respond, ResponseTemplate};

use lmforge::engine::manager::{EngineMetrics, EngineState, EngineStatus, ManagerCommand};
use lmforge::engine::registry::EngineConfig;
use lmforge::model::index::{ModelCapabilities, ModelEntry, ModelIndex};
use lmforge::server::AppState;

const MODEL: &str = "test-reranker:0.6b:8bit";
const MODEL_DIR: &str = "test-reranker-dir";

// ── Synthetic GGUF headers ───────────────────────────────────────────────────

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn kv_str(buf: &mut Vec<u8>, key: &str, val: &str) {
    put_str(buf, key);
    buf.extend_from_slice(&8u32.to_le_bytes());
    put_str(buf, val);
}

fn kv_u32(buf: &mut Vec<u8>, key: &str, val: u32) {
    put_str(buf, key);
    buf.extend_from_slice(&4u32.to_le_bytes());
    buf.extend_from_slice(&val.to_le_bytes());
}

/// Header-only GGUF: arch, optional pooling/context/rerank template, and an
/// optional `cls.output.weight` with the given dims.
fn write_gguf(
    dir: &Path,
    arch: &str,
    pooling: Option<u32>,
    context_length: u32,
    template: Option<&str>,
    cls_output_dims: Option<&[u64]>,
) {
    let mut kvs = Vec::new();
    let mut n_kv = 2u64;
    kv_str(&mut kvs, "general.architecture", arch);
    kv_u32(&mut kvs, &format!("{arch}.context_length"), context_length);
    if let Some(p) = pooling {
        kv_u32(&mut kvs, &format!("{arch}.pooling_type"), p);
        n_kv += 1;
    }
    if let Some(t) = template {
        kv_str(&mut kvs, "tokenizer.chat_template.rerank", t);
        n_kv += 1;
    }
    let mut tensors: Vec<(&str, &[u64])> = vec![("output_norm.weight", &[1024])];
    if let Some(d) = cls_output_dims {
        tensors.push(("cls.output.weight", d));
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(b"GGUF");
    buf.extend_from_slice(&3u32.to_le_bytes());
    buf.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
    buf.extend_from_slice(&n_kv.to_le_bytes());
    buf.extend_from_slice(&kvs);
    for (name, dims) in tensors {
        put_str(&mut buf, name);
        buf.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for d in dims {
            buf.extend_from_slice(&d.to_le_bytes());
        }
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("model.gguf"), buf).unwrap();
}

/// Qwen3-Reranker as converted by llama.cpp: pooling rank, yes/no head.
fn qwen3_reranker(dir: &Path, context_length: u32) {
    write_gguf(
        dir,
        "qwen3",
        Some(4),
        context_length,
        Some("Q: {query} D: {document}"),
        Some(&[1024, 2]),
    );
}

// ── Router harness (mirrors tests/responses_api.rs) ──────────────────────────

fn write_model_index(data_dir: &Path, format: &str, engine: &str) -> PathBuf {
    let model_dir = data_dir.join("models").join(MODEL_DIR);
    let index = ModelIndex {
        schema_version: 1,
        models: vec![ModelEntry {
            id: MODEL.to_string(),
            path: model_dir.to_string_lossy().to_string(),
            format: format.to_string(),
            engine: engine.to_string(),
            hf_repo: None,
            size_bytes: 0,
            capabilities: ModelCapabilities {
                reranking: true,
                ..Default::default()
            },
            added_at: "2026-10-05".to_string(),
        }],
        ..Default::default()
    };
    std::fs::write(
        data_dir.join("models.json"),
        serde_json::to_string_pretty(&index).unwrap(),
    )
    .unwrap();
    model_dir
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

async fn post_rerank(router: &axum::Router, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/rerank")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn rerank_reply(scores: &[f64]) -> ResponseTemplate {
    let results: Vec<Value> = scores
        .iter()
        .enumerate()
        .map(|(i, s)| json!({"index": i, "relevance_score": s}))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({
        "results": results,
        "usage": {"prompt_tokens": 42, "total_tokens": 42}
    }))
}

async fn received_rerank_bodies(engine: &MockServer) -> Vec<Value> {
    engine
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/rerank")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn relevance(body: &Value) -> Vec<(u64, f64)> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["index"].as_u64().unwrap(),
                r["relevance_score"].as_f64().unwrap(),
            )
        })
        .collect()
}

/// Fake llama-server tokenizer: one token per whitespace-separated word.
struct WordTokenizer;

impl Respond for WordTokenizer {
    fn respond(&self, req: &MockRequest) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        if req.url.path() == "/tokenize" {
            let n = body["content"].as_str().unwrap().split_whitespace().count();
            ResponseTemplate::new(200).set_body_json(json!({"tokens": (0..n).collect::<Vec<_>>()}))
        } else {
            let n = body["tokens"].as_array().unwrap().len();
            ResponseTemplate::new(200).set_body_json(json!({"content": vec!["w"; n].join(" ")}))
        }
    }
}

async fn mount_tokenizer(engine: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/tokenize"))
        .respond_with(WordTokenizer)
        .mount(engine)
        .await;
    Mock::given(method("POST"))
        .and(path("/detokenize"))
        .respond_with(WordTokenizer)
        .mount(engine)
        .await;
}

// ── Calibration ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn omlx_probabilities_are_returned_unchanged_with_score_type() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(write_model_index(tmp.path(), "mlx", "omlx")).unwrap();
    let engine = MockServer::start().await;
    // oMLX 0.7.0, measured for the DocIntel acceptance pair.
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(rerank_reply(&[0.69140625, 0.0000214577]))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "omlx", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "blockchain payment provisions",
               "documents": ["The fund shall pay the blockchain administrator a fee", "Employees accrue annual leave"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["score_type"], "probability");
    assert_eq!(relevance(&body), vec![(0, 0.69140625), (1, 0.0000214577)]);
    assert_eq!(body["usage"]["total_tokens"], 42);
    // Sent to the engine under the on-disk dir name, documents untouched.
    let sent = &received_rerank_bodies(&engine).await[0];
    assert_eq!(sent["model"], MODEL_DIR);
    assert_eq!(sent["documents"][1], "Employees accrue annual leave");
}

#[tokio::test]
async fn llamacpp_qwen3_head_probabilities_are_not_sigmoided() {
    let tmp = tempfile::tempdir().unwrap();
    qwen3_reranker(&write_model_index(tmp.path(), "gguf", "llamacpp"), 40960);
    let engine = MockServer::start().await;
    // llama.cpp b9861 + ggml-org Qwen3-Reranker-0.6B Q8_0, measured.
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(rerank_reply(&[0.2204371690750122, 0.000014171997463563457]))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["relevant", "irrelevant"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["score_type"], "probability");
    // Unchanged up to JSON float round-tripping (last-digit only).
    let s = relevance(&body);
    assert_eq!((s[0].0, s[1].0), (0, 1));
    assert!((s[0].1 / 0.2204371690750122 - 1.0).abs() < 1e-12, "{s:?}");
    assert!(
        (s[1].1 / 0.000014171997463563457 - 1.0).abs() < 1e-12,
        "{s:?}"
    );
}

#[tokio::test]
async fn llamacpp_cross_encoder_logits_are_mapped_through_the_sigmoid() {
    let tmp = tempfile::tempdir().unwrap();
    // gpustack bge-reranker-v2-m3: bert, no pooling key, 1-D cls.output.
    write_gguf(
        &write_model_index(tmp.path(), "gguf", "llamacpp"),
        "bert",
        None,
        8192,
        None,
        Some(&[1024]),
    );
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(rerank_reply(&[3.3804, -11.0331]))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["relevant", "irrelevant"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let s = relevance(&body);
    assert!((s[0].1 - 0.96707).abs() < 1e-4, "{s:?}");
    assert!(s[1].1 < 1e-4, "{s:?}");
    assert_eq!(body["score_type"], "probability");
}

#[tokio::test]
async fn engine_without_a_calibration_rule_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(write_model_index(tmp.path(), "safetensors", "sglang")).unwrap();
    let engine = MockServer::start().await;
    let router = build_router(tmp.path().to_owned(), "sglang", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["d"]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(engine.received_requests().await.unwrap().is_empty());
}

// ── Headless GGUF ────────────────────────────────────────────────────────────

#[tokio::test]
async fn headless_reranker_gguf_is_refused_before_reaching_the_engine() {
    let tmp = tempfile::tempdir().unwrap();
    // mradermacher Qwen3-Reranker: arch qwen3, no pooling key, no cls tensor.
    write_gguf(
        &write_model_index(tmp.path(), "gguf", "llamacpp"),
        "qwen3",
        None,
        40960,
        None,
        None,
    );
    let engine = MockServer::start().await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["d"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "reranker_unusable");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("cls.output.weight"), "{msg}");
    assert!(
        msg.contains(&format!(
            "lmforge models remove {MODEL} && lmforge pull {MODEL}"
        )),
        "{msg}"
    );
    assert!(engine.received_requests().await.unwrap().is_empty());
}

// ── Truncation (llama.cpp) ───────────────────────────────────────────────────

#[tokio::test]
async fn one_long_document_is_truncated_without_failing_the_request() {
    let tmp = tempfile::tempdir().unwrap();
    // Trained context 256 → pooled window 256 tokens (= words here).
    qwen3_reranker(&write_model_index(tmp.path(), "gguf", "llamacpp"), 256);
    let engine = MockServer::start().await;
    mount_tokenizer(&engine).await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(rerank_reply(&[0.1, 0.8, 0.3]))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let long_doc = vec!["fee"; 5000].join(" ");
    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "monthly fee", "documents": ["short a", long_doc, "short b"],
               "return_documents": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["meta"]["truncated_documents"], json!([1]));
    // The client gets its own text back, not the truncated copy.
    let echoed = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["index"] == 1)
        .unwrap();
    assert_eq!(
        echoed["document"]["text"].as_str().unwrap().len(),
        long_doc.len()
    );

    // Budget = window 256 − template skeleton "Q: D:" (2) − query (2) − slack (16).
    let sent = &received_rerank_bodies(&engine).await[0];
    let words = sent["documents"][1]
        .as_str()
        .unwrap()
        .split_whitespace()
        .count();
    assert_eq!(words, 256 - 2 - 2 - 16);
    assert_eq!(sent["documents"][0], "short a");
    assert_eq!(sent["documents"][2], "short b");
}

#[tokio::test]
async fn short_documents_do_not_touch_the_tokenizer() {
    let tmp = tempfile::tempdir().unwrap();
    qwen3_reranker(&write_model_index(tmp.path(), "gguf", "llamacpp"), 40960);
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(rerank_reply(&[0.5]))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["a typical retrieval chunk"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("meta").is_none());
    let paths: Vec<String> = engine
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(paths, ["/v1/rerank"]);
}

#[tokio::test]
async fn a_query_that_fills_the_window_is_a_400() {
    let tmp = tempfile::tempdir().unwrap();
    qwen3_reranker(&write_model_index(tmp.path(), "gguf", "llamacpp"), 256);
    let engine = MockServer::start().await;
    mount_tokenizer(&engine).await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": vec!["word"; 300].join(" "), "documents": ["d"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "query_too_long");
    assert!(received_rerank_bodies(&engine).await.is_empty());
}

// ── Engine errors ────────────────────────────────────────────────────────────

#[tokio::test]
async fn engine_oversize_500_becomes_a_400() {
    let tmp = tempfile::tempdir().unwrap();
    qwen3_reranker(&write_model_index(tmp.path(), "gguf", "llamacpp"), 40960);
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": {
            "code": 500, "type": "server_error",
            "message": "input (2100 tokens) is too large to process. increase the physical batch size (current batch size: 2048)"
        }})))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "llamacpp", engine.address().port());

    let (status, body) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["d"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "input_too_long");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("2048-token window")
    );
}

#[tokio::test]
async fn other_engine_errors_pass_through() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(write_model_index(tmp.path(), "mlx", "omlx")).unwrap();
    let engine = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"detail": "busy"})))
        .mount(&engine)
        .await;
    let router = build_router(tmp.path().to_owned(), "omlx", engine.address().port());

    let (status, _) = post_rerank(
        &router,
        json!({"model": MODEL, "query": "q", "documents": ["d"]}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
