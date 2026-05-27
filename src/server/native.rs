use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use tracing::{error, info, warn};

use super::AppState;

/// `GET /lf/status/stream` — Persistent SSE stream of EngineState snapshots.
///
/// Transport role: external consumers only (CLI watchers, DocIntel health checks, dev tools).
/// Embedded Tauri frontend uses Tauri Events instead (zero HTTP, in-process).
/// The tray icon uses the broadcast channel directly (also in-process).
///
/// Protocol: `text/event-stream`
///   - Sends the current snapshot immediately on connect.
///   - Sends a new snapshot on every state change (model load/unload/health change).
///   - Sends a heartbeat `event: ping` every 15 s to survive TCP/proxy timeouts.
pub async fn status_stream(State(state): State<AppState>) -> impl IntoResponse {
    let mut rx = state.status_tx.subscribe();

    // Capture the current snapshot to emit immediately on connect.
    let initial = state.engine_state.read().await.clone();

    let stream = async_stream::stream! {
        // 1. Immediate snapshot so clients have data right away.
        let json = serde_json::to_string(&initial).unwrap_or_default();
        yield Ok::<_, std::convert::Infallible>(
            axum::body::Bytes::from(format!("data: {}\n\n", json))
        );

        // 2. Live updates + heartbeat.
        loop {
            tokio::select! {
                result = rx.recv() => match result {
                    Ok(snapshot) => {
                        let json = serde_json::to_string(&snapshot).unwrap_or_default();
                        yield Ok(axum::body::Bytes::from(format!("data: {}\n\n", json)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("SSE /lf/status/stream: client lagged by {} messages", n);
                        // Send current state to re-sync the client.
                        let snap = state.engine_state.read().await.clone();
                        let json = serde_json::to_string(&snap).unwrap_or_default();
                        yield Ok(axum::body::Bytes::from(format!("data: {}\n\n", json)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {
                    // Heartbeat — keeps the connection alive through proxies.
                    yield Ok(axum::body::Bytes::from("event: ping\ndata: {}\n\n"));
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no") // disables nginx response buffering
        .body(Body::from_stream(stream))
        .unwrap()
}

/// `GET /lf/status` — LMForge native status endpoint
pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let engine_state = state.engine_state.read().await;

    let running_models: Vec<_> = engine_state.running_models.values().collect();

    let resp = serde_json::json!({
        "overall_status": engine_state.overall_status,
        "engine": {
            "id": engine_state.engine_id,
            "version": engine_state.engine_version,
        },
        "running_models": running_models,
        "metrics": engine_state.metrics,
        // Phase 2.3: surface the last load failure per model. Empty when
        // every recent load succeeded. The UI / CLI can show this directly
        // instead of telling the user to grep ~/.lmforge/logs/.
        "last_errors": engine_state.last_errors,
        "catalogs_dir": state.config.read().await.catalogs_dir().to_string_lossy(),
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&resp).unwrap()))
        .unwrap()
}

/// `GET /lf/hardware` — Return cached hardware profile
pub async fn hardware(State(state): State<AppState>) -> impl IntoResponse {
    let hw_path = state.data_dir.join("hardware.json");
    let content = std::fs::read_to_string(&hw_path).unwrap_or_else(|_| {
        r#"{"error":"Hardware profile not found. Run lmforge init first."}"#.to_string()
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(content))
        .unwrap()
}

/// `GET /lf/engines` — Engine registry view for the UI.
///
/// Returns the full engine roster (default + opt-in + experimental) annotated
/// with this host's compatibility verdict and install state. The shape and
/// strings match `lmforge engine list` — the UI's tier badges and Install
/// buttons must agree with the CLI verdict, otherwise users would see two
/// different stories for the same engine.
///
/// Why a dedicated endpoint instead of expanding `/lf/status`:
///   * `/lf/status` is per-request hot path (Tauri polls it; SSE clients
///     get a copy on every model load). The engine registry is static for
///     the daemon's lifetime — no reason to ship it on every tick.
///   * Settings → Engine page wants the full roster, not just the active
///     engine. The shapes diverge naturally.
pub async fn engines(State(state): State<AppState>) -> impl IntoResponse {
    use crate::engine::registry::EngineRegistry;

    // Same registry-load pattern as `cli::engine::run`: prefer the user
    // override at `~/.lmforge/engines.toml` if present, else the bundled
    // default. Keeps the UI and CLI in sync when users tweak the registry.
    let user_engines_toml = state.data_dir.join("engines.toml");
    let registry = match EngineRegistry::load(if user_engines_toml.exists() {
        Some(user_engines_toml.as_path())
    } else {
        None
    }) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to load engine registry: {e}");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":"engine registry load failed: {}"}}"#,
                    e.to_string().replace('"', "'")
                )))
                .unwrap();
        }
    };

    // Hardware profile is required for the compatibility verdict. If the
    // user hasn't run `init` yet, fall back to reporting "compatible: null"
    // rather than guessing — the UI then suppresses the Install button.
    let hw_path = state.data_dir.join("hardware.json");
    let profile_opt: Option<crate::hardware::probe::HardwareProfile> =
        std::fs::read_to_string(&hw_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

    let active_engine_id = state.engine_state.read().await.engine_id.clone();

    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(registry.all().len());
    for engine in registry.all() {
        let installed = crate::cli::engine::install_state(engine, &state.data_dir);
        let (compatible, note) = match profile_opt.as_ref() {
            Some(p) => {
                let (ok, why) = crate::cli::engine::compatibility(engine, p);
                (Some(ok), why)
            }
            None => (None, String::new()),
        };

        rows.push(serde_json::json!({
            "id": engine.id,
            "name": engine.name,
            "version": engine.version,
            "tier": crate::cli::engine::tier_label(engine.tier),
            "install_method": engine.install_method,
            "model_format": engine.model_format,
            "matches_gpu": engine.matches_gpu,
            "min_compute_cap": engine.min_compute_cap,
            "max_compute_cap": engine.max_compute_cap,
            "min_vram_gb": engine.min_vram_gb,
            "supported_os_families": engine.supported_os_families,
            "supports_embeddings": engine.supports_embeddings,
            "supports_reranking": engine.supports_reranking,
            "installed": installed,
            "compatible": compatible,
            "incompatible_reason": if note.is_empty() { None } else { Some(note) },
            "active": engine.id == active_engine_id,
        }));
    }

    let resp = serde_json::json!({
        "engines": rows,
        "active_engine_id": active_engine_id,
        "has_hardware_profile": profile_opt.is_some(),
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&resp).unwrap()))
        .unwrap()
}

/// `GET /lf/model/list` — List installed models
pub async fn model_list(State(state): State<AppState>) -> impl IntoResponse {
    let models_file = state.data_dir.join("models.json");
    let content = if models_file.exists() {
        std::fs::read_to_string(&models_file)
            .unwrap_or_else(|_| r#"{"schema_version":1,"models":[]}"#.to_string())
    } else {
        r#"{"schema_version":1,"models":[]}"#.to_string()
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(content))
        .unwrap()
}

/// `POST /lf/model/switch` — Switch active model
pub async fn model_switch(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"error":"Invalid JSON: {}"}}"#, e)))
                .unwrap();
        }
    };
    let model_id = match req.get("model").and_then(|v| v.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(
                    r#"{"error":"Missing or invalid 'model' parameter."}"#,
                ))
                .unwrap();
        }
    };

    info!(model = %model_id, "API request to hot-swap or warm orchestrator model");

    if let Err(resp) = state.ensure_model(&model_id, None).await {
        error!("Failed to route EnsureModel into Orchestrator Control Plane");
        return resp.into_response();
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"status":"Orchestrator VRAM Hot-Swap queued successfully."}"#,
        ))
        .unwrap()
}

/// `POST /lf/shutdown` — Graceful shutdown (loopback only).
///
/// Drains active engine slots through the manager BEFORE calling
/// `process::exit`. Without this, vLLM's `EngineCore` subprocess (which
/// holds the bulk of the VRAM) gets reparented to init and lingers until
/// the user manually `kill -9`s it. SGLang has a milder version of the
/// same problem.
pub async fn shutdown(State(state): State<AppState>) -> impl IntoResponse {
    info!("Shutdown requested via API");

    let cmd_tx = state.command_tx.clone();

    tokio::spawn(async move {
        // Best-effort drain: ask the manager to stop every active slot
        // (which calls `adapter.stop()` → killpg on the engine's process
        // group). Cap the wait at 15s so a wedged adapter can't hold the
        // shutdown forever; if it times out we still hard-exit and any
        // surviving children get reaped by `kill_on_drop` on the daemon's
        // own `Child` handles.
        if let Err(e) = cmd_tx
            .send(crate::engine::manager::ManagerCommand::UnloadAll)
            .await
        {
            warn!(error = %e, "Could not send UnloadAll to manager during shutdown");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        std::process::exit(0);
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"status":"shutting_down"}"#))
        .unwrap()
}

/// `POST /lf/model/pull` — Download a model with SSE progress.
///
/// Dispatch strategy:
///   1. Ask the engine adapter if it can handle the pull natively (e.g. SGLang via huggingface_hub).
///      - `Ok(true)`  → adapter succeeded; update ModelIndex.
///      - `Err(e)`    → adapter failed; surface error via SSE.
///   2. If the adapter returns `Ok(false)` (oMLX, llama.cpp), fall back to LMForge's Rust
///      downloader which emits rich per-file SSE progress.
pub async fn model_pull(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(r#"{"error":"Invalid JSON"}"#))
                .unwrap();
        }
    };

    let model_id = req.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if model_id.is_empty() {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(r#"{"error":"Missing model field"}"#))
            .unwrap();
    }

    let engine_format = state.engine_config.model_format.clone();
    let catalogs_dir = state.config.read().await.catalogs_dir();
    let resolved =
        match crate::model::resolver::resolve(model_id, &engine_format, &catalogs_dir).await {
            Ok(r) => r,
            Err(e) => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from(format!(r#"{{"error":"{}"}}"#, e)))
                    .unwrap();
            }
        };

    let data_dir = state.data_dir.clone();
    let model_dir = data_dir.join("models").join(&resolved.dir_name);
    let (tx, rx) = tokio::sync::mpsc::channel(100);

    let resolved_id = resolved.id.clone();
    let format = resolved.format;
    let engine_id = state.engine_config.id.clone();
    let adapter = state.adapter.clone();

    tokio::spawn(async move {
        let succeeded = dispatch_pull(
            &adapter,
            &resolved.hf_repo,
            &resolved.files,
            &model_dir,
            tx.clone(),
        )
        .await;

        if succeeded {
            // Update ModelIndex now that weights are on disk
            if let Ok(mut idx) = crate::model::index::ModelIndex::load(&data_dir) {
                let caps = crate::model::index::detect_capabilities(
                    &model_dir,
                    Some(&resolved_id),
                    Some(&resolved.hf_repo),
                );
                idx.add(crate::model::index::ModelEntry {
                    id: resolved_id,
                    path: model_dir.to_string_lossy().to_string(),
                    format: format.to_string(),
                    engine: engine_id,
                    hf_repo: Some(resolved.hf_repo),
                    size_bytes: crate::model::index::dir_size(&model_dir),
                    capabilities: caps,
                    added_at: chrono::Utc::now().to_rfc3339(),
                });
                let _ = idx.save(&data_dir);
            }
        }
    });

    use tokio_stream::StreamExt;
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(|prog| {
        let json = serde_json::to_string(&prog).unwrap();
        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(format!("data: {}\n\n", json)))
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Run the adapter's native pull; if it defers (`Ok(false)`), use LMForge's Rust downloader.
/// Returns `true` if the model is now on disk and ready to index.
async fn dispatch_pull(
    adapter: &crate::engine::adapter::EngineAdapterInstance,
    hf_repo: &str,
    files: &[String],
    model_dir: &std::path::Path,
    tx: tokio::sync::mpsc::Sender<crate::model::downloader::DownloadProgress>,
) -> bool {
    use crate::engine::adapter::EngineAdapter;

    match adapter.pull_model(hf_repo, model_dir, tx.clone()).await {
        Ok(true) => {
            // Adapter handled it and succeeded
            true
        }
        Ok(false) => {
            // Adapter deferred — use LMForge's Rust downloader (rich per-file progress)
            crate::model::downloader::download_model(hf_repo, files, model_dir, Some(tx))
                .await
                .is_ok()
        }
        Err(e) => {
            // Adapter attempted but failed — error already sent to SSE channel by the adapter
            error!(error = %e, "Engine adapter pull failed");
            false
        }
    }
}

/// `POST /lf/model/unload` — Stop the engine and free VRAM without removing model files.
/// The daemon stays alive. Use `/lf/model/switch` to reload a model afterward.
pub async fn model_unload(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let mut unload_all = true;
    let mut target_model = String::new();

    if !body.is_empty()
        && let Ok(req) = serde_json::from_slice::<serde_json::Value>(&body)
        && let Some(m) = req.get("model").and_then(|v| v.as_str())
    {
        unload_all = false;
        target_model = m.to_string();
    }

    let cmd = if unload_all {
        info!("API request to unload all engines from VRAM");
        crate::engine::manager::ManagerCommand::UnloadAll
    } else {
        info!(model = %target_model, "API request to unload specific engine from VRAM");
        crate::engine::manager::ManagerCommand::UnloadModel(target_model.clone())
    };

    if let Err(e) = state.command_tx.send(cmd).await {
        error!(error = %e, "Failed to send Unload to Orchestrator Control Plane");
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"error":"Orchestrator Control Plane is dead"}"#,
            ))
            .unwrap();
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"status":"unloading","message":"Engine stop queued. Use /lf/model/switch to reload."}"#))
        .unwrap()
}

/// `DELETE /lf/model/:name` — Remove a model from the index and optionally from disk
pub async fn model_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let mut idx = match crate::model::index::ModelIndex::load(&state.data_dir) {
        Ok(i) => i,
        Err(e) => {
            error!(error = %e, "Failed to load model index for deletion");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"error":"Failed to load model index"}"#))
                .unwrap();
        }
    };

    let entry = match idx.remove(&name) {
        Some(e) => e,
        None => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":"Model '{}' not found"}}"#,
                    name
                )))
                .unwrap();
        }
    };

    if let Err(e) = idx.save(&state.data_dir) {
        error!(error = %e, "Failed to save model index after deletion");
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"error":"Failed to save model index"}"#))
            .unwrap();
    }

    // Remove from disk
    let model_path = std::path::Path::new(&entry.path);
    if model_path.exists()
        && let Err(e) = std::fs::remove_dir_all(model_path)
    {
        warn!(error = %e, path = %entry.path, "Failed to delete model files from disk");
        // Index is already updated — return partial success
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                r#"{{"status":"removed_from_index","warning":"Could not delete files: {}"}}"#,
                e
            )))
            .unwrap();
    }

    info!(model = %name, "Model deleted from index and disk");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"status":"deleted","model":"{}"}}"#,
            name
        )))
        .unwrap()
}

/// `GET /lf/config` — Get the current LMForge config
pub async fn config_get(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.read().await;
    let json = serde_json::to_string(&*config).unwrap_or_else(|_| "{}".to_string());

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json))
        .unwrap()
}

/// `POST /lf/config` — Update the current LMForge config
pub async fn config_update(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let req: crate::config::LmForgeConfig = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":"Invalid config payload: {}"}}"#,
                    e
                )))
                .unwrap();
        }
    };

    // Update in-memory state
    {
        let mut config = state.config.write().await;
        *config = req.clone();

        // Persist to disk natively
        if let Err(e) = config.save() {
            error!(error = %e, "Failed to persist LMForgeConfig disk after update");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":"Failed to save configuration: {}"}}"#,
                    e
                )))
                .unwrap();
        }
    }

    // If catalogs_dir changed, create the new directory and seed it with the bundled catalogs
    // so the user gets a ready-to-customise starting point without running lmforge init.
    let new_catalogs_dir = req.catalogs_dir().clone();
    if let Err(e) = std::fs::create_dir_all(&new_catalogs_dir) {
        tracing::warn!(error = %e, dir = %new_catalogs_dir.display(), "Could not create new catalogs directory");
    } else {
        let mlx_path = new_catalogs_dir.join("mlx.json");
        if !mlx_path.exists() {
            let _ = std::fs::write(&mlx_path, include_str!("../../data/catalogs/mlx.json"));
        }
        let safetensors_path = new_catalogs_dir.join("safetensors.json");
        if !safetensors_path.exists() {
            let _ = std::fs::write(
                &safetensors_path,
                include_str!("../../data/catalogs/safetensors.json"),
            );
        }
        let gguf_path = new_catalogs_dir.join("gguf.json");
        if !gguf_path.exists() {
            let _ = std::fs::write(
                &gguf_path,
                include_str!("../../data/catalogs/gguf.json"),
            );
        }
        let exl3_path = new_catalogs_dir.join("exl3.json");
        if !exl3_path.exists() {
            let _ = std::fs::write(
                &exl3_path,
                include_str!("../../data/catalogs/exl3.json"),
            );
        }
    }

    info!("Configuration safely mutated via /lf/config API");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"status":"updated"}"#))
        .unwrap()
}
