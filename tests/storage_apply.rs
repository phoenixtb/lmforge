//! Integration tests for the storage-directory change flow.
//!
//! These exercise `POST /lf/storage/apply` end-to-end at the handler level:
//! validation rejections, the apply-now/finish-on-restart migration manifest
//! (scan + re-pull queue), data-loss confirmation (422), and reset-to-default.
//!
//! Isolation: each test points `LMFORGE_CONFIG` (and `LMFORGE_DATA_DIR`, used as
//! the reset-to-default target) at a fresh tempdir, so `config.save()` and the
//! `pending-migration.json` manifest never touch the real `~/.lmforge`. Env is
//! process-global, so a mutex serialises the tests that depend on it.

// Holding the env guard across awaits is the whole point (process-global env
// must stay pinned for the full test). #[tokio::test] runs each test on its
// own current-thread runtime, so this cannot deadlock.
#![allow(clippy::await_holding_lock)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::sync::{RwLock, broadcast, mpsc};

use lmforge::config::LmForgeConfig;
use lmforge::engine::adapter::EngineAdapterInstance;
use lmforge::engine::adapters::llamacpp::LlamacppAdapter;
use lmforge::engine::manager::{EngineMetrics, EngineState, EngineStatus, ManagerCommand};
use lmforge::engine::registry::EngineConfig;
use lmforge::model::index::{ModelCapabilities, ModelEntry, ModelIndex};
use lmforge::model::migration::{MigrationIntent, PendingMigration};
use lmforge::server::AppState;
use lmforge::server::native::storage_apply;

/// Serialises env-var mutation across tests (cargo runs tests in parallel).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Set up an isolated config/manifest location + reset-to-default target.
/// Returns the config root the env now points at.
///
/// SAFETY: callers hold `ENV_LOCK` for the duration so no other test reads or
/// writes these vars concurrently.
fn set_isolated_env(config_root: &Path, default_data: &Path) {
    unsafe {
        std::env::set_var("LMFORGE_CONFIG", config_root.join("config.toml"));
        std::env::set_var("LMFORGE_DATA_DIR", default_data);
        std::env::remove_var("LMFORGE_MODELS_DIR");
    }
}

fn clear_isolated_env() {
    unsafe {
        std::env::remove_var("LMFORGE_CONFIG");
        std::env::remove_var("LMFORGE_DATA_DIR");
    }
}

/// Acquire the env lock for the WHOLE test (returned guard) and point the
/// process-global env at the isolated tempdir. Dropping the guard before the
/// test body finishes would let parallel tests race on LMFORGE_CONFIG /
/// LMFORGE_DATA_DIR and read each other's manifests.
fn setup_env(config_root: &Path, default_data: &Path) -> std::sync::MutexGuard<'static, ()> {
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_isolated_env(config_root, default_data);
    g
}

/// Clear the env vars. Caller still holds the guard from `setup_env`.
fn teardown_env() {
    clear_isolated_env();
}

/// Build a minimal AppState wired with the given live dirs. The config fields
/// are initialised to match the live dirs (as they would be on a real daemon
/// whose `config.toml` pins these paths), so an "unchanged" dir resolves back
/// to the same path. Reset-to-default is then driven by the request flags.
/// The returned `mpsc::Receiver` keeps the `UnloadAll` command channel open.
fn make_state(
    data_dir: PathBuf,
    models_dir: PathBuf,
) -> (AppState, mpsc::Receiver<ManagerCommand>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (status_tx, _status_rx) = broadcast::channel(16);

    let engine_state = EngineState {
        overall_status: EngineStatus::Stopped,
        engine_id: "llamacpp".to_string(),
        engine_version: "test".to_string(),
        running_models: std::collections::HashMap::new(),
        metrics: EngineMetrics::default(),
        last_errors: std::collections::HashMap::new(),
        dismissed_errors: std::collections::HashMap::new(),
    };

    let mut config = LmForgeConfig::default();
    config.data_dir = Some(data_dir.to_string_lossy().to_string());
    config.models_dir = Some(models_dir.to_string_lossy().to_string());

    let state = AppState {
        engine_state: Arc::new(RwLock::new(engine_state)),
        engine_config: EngineConfig::default(),
        adapter: Arc::new(EngineAdapterInstance::Llamacpp(LlamacppAdapter::default())),
        data_dir,
        models_dir,
        api_key: None,
        bind_address: "127.0.0.1".to_string(),
        config: Arc::new(RwLock::new(config)),
        command_tx: cmd_tx,
        status_tx,
        pull_in_flight: Arc::new(AtomicBool::new(false)),
        active_pull: Arc::new(RwLock::new(None)),
        migration_status: Arc::new(RwLock::new(None)),
        migration_cancel: Arc::new(AtomicBool::new(false)),
    };

    (state, cmd_rx)
}

/// Invoke the handler and decode the `(status, json)` response.
async fn call(state: AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = storage_apply(State(state), Bytes::from(body.to_string()))
        .await
        .into_response();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// Write a model index with the given entries and create their on-disk dirs.
fn seed_index(data_dir: &Path, models_dir: &Path, entries: &[(&str, Option<&str>)]) {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::create_dir_all(models_dir).unwrap();
    let mut idx = ModelIndex {
        schema_version: 2,
        models: vec![],
    };
    for (id, hf_repo) in entries {
        let dir_name = id.replace([':', '/'], "-");
        let model_dir = models_dir.join(&dir_name);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("weights.gguf"), b"dummy").unwrap();
        idx.add(ModelEntry {
            id: id.to_string(),
            path: model_dir.to_string_lossy().to_string(),
            format: "gguf".to_string(),
            engine: "llamacpp".to_string(),
            hf_repo: hf_repo.map(|s| s.to_string()),
            size_bytes: 5,
            capabilities: ModelCapabilities::default(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        });
    }
    idx.save(data_dir, models_dir).unwrap();
}

// ── validation rejections ───────────────────────────────────────────────────

#[tokio::test]
async fn noop_when_nothing_changes() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let models = root.path().join("models");
    let (state, _rx) = make_state(data.clone(), models.clone());

    // No dirs supplied and no reset → nothing changed. This is a no-op, not an
    // error: the handler returns 200 with status "unchanged" + restart_required
    // false so the UI can reconcile a stale pending value instead of surfacing
    // an "Apply failed" toast.
    let (status, json) = call(state, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "got: {json}");
    assert_eq!(json["status"].as_str(), Some("unchanged"), "got: {json}");
    assert_eq!(
        json["restart_required"].as_bool(),
        Some(false),
        "got: {json}"
    );

    teardown_env();
}

#[tokio::test]
async fn rejects_overlapping_models_dir() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let models = root.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let nested = models.join("inner");
    let (state, _rx) = make_state(data, models);

    let (status, json) = call(
        state,
        serde_json::json!({ "models_dir": nested.to_string_lossy(), "models_action": "adopt" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"].as_str().unwrap().contains("nested"),
        "got: {json}"
    );

    teardown_env();
}

#[tokio::test]
async fn rejects_unwritable_target() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let models = root.path().join("models");
    // Make a file, then try to use a path *under* that file as the new dir.
    // create_dir_all must fail because a component is not a directory.
    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let bad_target = blocker.join("subdir");
    let (state, _rx) = make_state(data, models);

    let (status, json) = call(
        state,
        serde_json::json!({ "models_dir": bad_target.to_string_lossy(), "models_action": "adopt" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"].as_str().unwrap().contains("not usable"),
        "got: {json}"
    );

    teardown_env();
}

#[tokio::test]
async fn rejects_when_pull_in_flight() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let models = root.path().join("models");
    let new_models = root.path().join("new-models");
    let (state, _rx) = make_state(data, models);
    state
        .pull_in_flight
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (status, _json) = call(
        state,
        serde_json::json!({ "models_dir": new_models.to_string_lossy(), "models_action": "adopt" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    teardown_env();
}

// ── migration manifest: scan + delete ────────────────────────────────────────

#[tokio::test]
async fn adopt_writes_scan_manifest_and_persists_config() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let old_models = root.path().join("models");
    let new_models = root.path().join("shared-weights");
    seed_index(&data, &old_models, &[("llama3:8b", Some("meta/llama3"))]);
    let (state, _rx) = make_state(data, old_models.clone());

    let (status, json) = call(
        state.clone(),
        serde_json::json!({ "models_dir": new_models.to_string_lossy(), "models_action": "adopt" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {json}");
    assert_eq!(json["restart_required"], true);

    // Adopt → no deletion: old model files survive.
    assert!(old_models.join("llama3-8b/weights.gguf").exists());

    // Manifest written with Scan intent + new dir.
    let manifest = PendingMigration::load().unwrap().expect("manifest present");
    assert_eq!(manifest.intent, MigrationIntent::Scan);
    assert_eq!(
        manifest.models_dir.as_deref(),
        Some(new_models.to_string_lossy().as_ref())
    );

    // Config field persisted in memory.
    assert_eq!(
        state.config.read().await.models_dir.as_deref(),
        Some(new_models.to_string_lossy().as_ref())
    );

    teardown_env();
}

#[tokio::test]
async fn delete_removes_old_files_and_clears_index() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let old_models = root.path().join("models");
    let new_models = root.path().join("new-models");
    seed_index(&data, &old_models, &[("llama3:8b", Some("meta/llama3"))]);
    let old_model_dir = old_models.join("llama3-8b");
    assert!(old_model_dir.exists());
    let (state, _rx) = make_state(data.clone(), old_models.clone());

    let (status, _json) = call(
        state,
        serde_json::json!({ "models_dir": new_models.to_string_lossy(), "models_action": "delete" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Old model dir removed; index emptied.
    assert!(!old_model_dir.exists(), "old model dir must be deleted");
    let idx = ModelIndex::load(&data, &old_models).unwrap();
    assert!(idx.list().is_empty(), "index must be cleared after delete");

    teardown_env();
}

// ── migration manifest: re-pull + data-loss confirmation ─────────────────────

#[tokio::test]
async fn repull_returns_422_for_models_without_hf_repo() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let old_models = root.path().join("models");
    let new_models = root.path().join("new-models");
    // One model has no hf_repo → cannot be re-downloaded.
    seed_index(&data, &old_models, &[("local-only", None)]);
    let (state, _rx) = make_state(data, old_models.clone());

    let (status, json) = call(
        state,
        serde_json::json!({ "models_dir": new_models.to_string_lossy(), "models_action": "repull" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {json}");
    let lose: Vec<String> = serde_json::from_value(json["would_lose"].clone()).unwrap();
    assert_eq!(lose, vec!["local-only".to_string()]);

    // No destructive action yet — old files still present.
    assert!(old_models.join("local-only/weights.gguf").exists());

    teardown_env();
}

#[tokio::test]
async fn repull_with_ack_queues_repullable_and_drops_rest() {
    let root = tempfile::tempdir().unwrap();
    let _env = setup_env(root.path(), &root.path().join("defaultdata"));

    let data = root.path().join("data");
    let old_models = root.path().join("models");
    let new_models = root.path().join("new-models");
    seed_index(
        &data,
        &old_models,
        &[("has-repo", Some("org/has-repo")), ("local-only", None)],
    );
    let (state, _rx) = make_state(data, old_models.clone());

    let (status, json) = call(
        state,
        serde_json::json!({
            "models_dir": new_models.to_string_lossy(),
            "models_action": "repull",
            "exclude_from_repull": ["local-only"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {json}");

    // Confirmed-loss model returned in would_lose.
    let lose: Vec<String> = serde_json::from_value(json["would_lose"].clone()).unwrap();
    assert!(lose.contains(&"local-only".to_string()));

    // Manifest: Repull intent, queue holds only the repo-backed model.
    let manifest = PendingMigration::load().unwrap().expect("manifest present");
    assert_eq!(manifest.intent, MigrationIntent::Repull);
    assert_eq!(manifest.repull_queue.len(), 1);
    assert_eq!(manifest.repull_queue[0].id, "has-repo");
    assert_eq!(manifest.repull_queue[0].hf_repo, "org/has-repo");

    teardown_env();
}

// ── reset-to-default ─────────────────────────────────────────────────────────

#[tokio::test]
async fn reset_models_dir_persists_none_and_resolves_default() {
    let root = tempfile::tempdir().unwrap();
    let default_data = root.path().join("defaultdata");
    let _env = setup_env(root.path(), &default_data);

    // Live daemon was using a custom models dir (config field Some).
    let custom_models = root.path().join("custom-weights");
    let data = root.path().join("data");
    std::fs::create_dir_all(&custom_models).unwrap();
    let (state, _rx) = make_state(data.clone(), custom_models.clone());

    let (status, json) = call(
        state.clone(),
        serde_json::json!({ "reset_models_dir": true, "models_action": "adopt" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {json}");
    assert_eq!(json["restart_required"], true);

    // Config field cleared to None (= built-in default).
    assert!(
        state.config.read().await.models_dir.is_none(),
        "models_dir field must be reset to None"
    );

    // models_dir field is None now, so it resolves to {data_dir}/models, where
    // data_dir is still the (unchanged) configured data dir.
    let manifest = PendingMigration::load().unwrap().expect("manifest present");
    let expected = data.join("models");
    assert_eq!(
        manifest.models_dir.as_deref(),
        Some(expected.to_string_lossy().as_ref()),
        "reset must resolve to the default models dir under data_dir"
    );

    teardown_env();
}
