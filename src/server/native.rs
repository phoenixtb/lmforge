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

    // Serialise an EngineState snapshot with the current `active_pull` merged in,
    // so stream consumers (browser/dev UI, CLI watchers) see download progress
    // the same way `GET /lf/status` exposes it. Extra field is ignored by typed
    // EngineState deserializers.
    async fn frame(
        snapshot: &crate::engine::manager::EngineState,
        state: &AppState,
    ) -> String {
        let mut v = serde_json::to_value(snapshot).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            let ap = state.active_pull.read().await.clone();
            obj.insert(
                "active_pull".to_string(),
                serde_json::to_value(ap).unwrap_or(serde_json::Value::Null),
            );
        }
        v.to_string()
    }

    // Capture the current snapshot to emit immediately on connect.
    let initial = state.engine_state.read().await.clone();

    let stream = async_stream::stream! {
        // 1. Immediate snapshot so clients have data right away.
        let json = frame(&initial, &state).await;
        yield Ok::<_, std::convert::Infallible>(
            axum::body::Bytes::from(format!("data: {}\n\n", json))
        );

        // 2. Live updates + heartbeat.
        loop {
            tokio::select! {
                result = rx.recv() => match result {
                    Ok(snapshot) => {
                        let json = frame(&snapshot, &state).await;
                        yield Ok(axum::body::Bytes::from(format!("data: {}\n\n", json)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("SSE /lf/status/stream: client lagged by {} messages", n);
                        // Send current state to re-sync the client.
                        let snap = state.engine_state.read().await.clone();
                        let json = frame(&snap, &state).await;
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
    let config = state.config.read().await;
    let restart_required =
        config.data_dir() != state.data_dir || config.models_dir() != state.models_dir;

    let running_models: Vec<_> = engine_state.running_models.values().collect();
    let active_pull = state.active_pull.read().await.clone();

    let resp = serde_json::json!({
        "overall_status": engine_state.overall_status,
        "engine": {
            "id": engine_state.engine_id,
            "version": engine_state.engine_version,
        },
        "running_models": running_models,
        "metrics": engine_state.metrics,
        // Surface the last load failure per model. Empty when every recent load
        // succeeded. The UI / CLI can show this directly instead of grepping logs.
        "last_errors": engine_state.last_errors,
        "catalogs_dir": config.catalogs_dir().to_string_lossy(),
        "restart_required": restart_required,
        // In-flight model pull (or null). Lets any client show download progress
        // even after the originating SSE stream is gone.
        "active_pull": active_pull,
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

/// `GET /lf/model/list` — List installed models.
///
/// Loads through `ModelIndex` so on-disk relative paths (schema v2) are
/// resolved to absolute before being returned — clients expect usable paths.
pub async fn model_list(State(state): State<AppState>) -> impl IntoResponse {
    let content = match crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir) {
        Ok(idx) => serde_json::to_string(&idx)
            .unwrap_or_else(|_| r#"{"schema_version":2,"models":[]}"#.to_string()),
        Err(_) => r#"{"schema_version":2,"models":[]}"#.to_string(),
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

    // Serialise pulls: only one in-flight at a time (avoids index corruption).
    if state
        .pull_in_flight
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::Acquire,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        let busy = state
            .active_pull
            .read()
            .await
            .as_ref()
            .map(|p| p.model.clone())
            .unwrap_or_default();
        return Response::builder()
            .status(StatusCode::CONFLICT)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "error": "A model download is already in progress",
                    "model": busy,
                }))
                .unwrap(),
            ))
            .unwrap();
    }

    // Publish an initial snapshot so /lf/status reflects the pull immediately,
    // before resolution/streaming begins.
    {
        let mut ap = state.active_pull.write().await;
        *ap = Some(super::ActivePull {
            model: model_id.to_string(),
            file: "Resolving model…".to_string(),
            ..Default::default()
        });
    }

    let engine_format = state.engine_config.model_format.clone();
    let catalogs_dir = state.config.read().await.catalogs_dir();
    let resolved =
        match crate::model::resolver::resolve(model_id, &engine_format, &catalogs_dir).await {
            Ok(r) => r,
            Err(e) => {
                // Release the in-flight lock + clear the snapshot taken above —
                // resolution failed before the pull task was spawned, so nothing
                // else will clear them.
                *state.active_pull.write().await = None;
                state
                    .pull_in_flight
                    .store(false, std::sync::atomic::Ordering::Release);
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from(format!(r#"{{"error":"{}"}}"#, e)))
                    .unwrap();
            }
        };

    let data_dir = state.data_dir.clone();
    let models_dir = state.models_dir.clone();
    let model_dir = state.models_dir.join(&resolved.dir_name);
    // Client-facing SSE channel.
    let (tx, rx) = tokio::sync::mpsc::channel(100);

    let resolved_id = resolved.id.clone();
    let format = resolved.format;
    let engine_id = state.engine_config.id.clone();
    let adapter = state.adapter.clone();
    let in_flight = state.pull_in_flight.clone();
    let active_pull = state.active_pull.clone();

    // Publish the resolved canonical id in the shared snapshot.
    {
        let mut ap = active_pull.write().await;
        if let Some(slot) = ap.as_mut() {
            slot.model = resolved_id.clone();
        }
    }

    let dl_repo = resolved.hf_repo.clone();
    let dl_files = resolved.files.clone();
    let dl_model_dir = model_dir.clone();
    let dl_data_dir = data_dir.clone();

    tokio::spawn(async move {
        // Inner channel: the downloader writes here. We tap every event to update
        // the shared snapshot (survives client disconnect) and forward to the SSE
        // client best-effort. This decouples progress capture from the client so a
        // navigation / second request never loses the in-flight pull.
        let (dl_tx, mut dl_rx) = tokio::sync::mpsc::channel(100);
        let dl_handle = tokio::spawn(async move {
            dispatch_pull(&adapter, &dl_repo, &dl_files, &dl_model_dir, &dl_data_dir, dl_tx).await
        });

        while let Some(prog) = dl_rx.recv().await {
            {
                let mut ap = active_pull.write().await;
                if let Some(slot) = ap.as_mut() {
                    apply_pull_progress(slot, &prog);
                }
            }
            // Forward to the SSE client; ignore send errors (client may be gone).
            let _ = tx.send(prog).await;
        }

        let succeeded = dl_handle.await.unwrap_or(false);

        if succeeded {
            // Update ModelIndex now that weights are on disk
            if let Ok(mut idx) = crate::model::index::ModelIndex::load(&data_dir, &models_dir) {
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
                let _ = idx.save(&data_dir, &models_dir);
            }
        }

        // Clear the shared snapshot and release the in-flight lock so subsequent
        // pulls and storage/apply can proceed.
        *active_pull.write().await = None;
        in_flight.store(false, std::sync::atomic::Ordering::Release);
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

/// Fold a single `DownloadProgress` event into the shared `ActivePull` snapshot.
fn apply_pull_progress(slot: &mut super::ActivePull, prog: &crate::model::downloader::DownloadProgress) {
    use crate::model::downloader::DownloadProgress as P;
    match prog {
        P::Started { files, .. } => {
            slot.file = format!("Preparing {files} file{}…", if *files == 1 { "" } else { "s" });
        }
        P::FileProgress {
            file,
            downloaded,
            total,
        } => {
            slot.file = file.clone();
            slot.downloaded_bytes = *downloaded;
            slot.total_bytes = *total;
        }
        P::FileCompleted { file } => {
            slot.file = format!("{file} ✓");
        }
        P::Completed { .. } => {
            slot.done = true;
        }
        P::Failed { error } => {
            slot.error = Some(error.clone());
        }
    }
}

/// Run the adapter's native pull; if it defers (`Ok(false)`), use LMForge's Rust downloader.
/// Returns `true` if the model is now on disk and ready to index.
async fn dispatch_pull(
    adapter: &crate::engine::adapter::EngineAdapterInstance,
    hf_repo: &str,
    files: &[String],
    model_dir: &std::path::Path,
    data_dir: &std::path::Path,
    tx: tokio::sync::mpsc::Sender<crate::model::downloader::DownloadProgress>,
) -> bool {
    use crate::engine::adapter::EngineAdapter;

    match adapter
        .pull_model(hf_repo, model_dir, data_dir, tx.clone())
        .await
    {
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
    // Unload the engine for this model before removing files. Prevents busy-file
    // errors on Windows and ensures the engine stops serving a half-deleted model.
    let _ = state
        .command_tx
        .send(crate::engine::manager::ManagerCommand::UnloadModel(
            name.clone(),
        ))
        .await;
    // Give the engine a brief window to drain in-flight requests.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut idx = match crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir) {
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

    if let Err(e) = idx.save(&state.data_dir, &state.models_dir) {
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
///
/// Injects `restart_required: true` when the saved config's storage dirs differ
/// from the live AppState dirs (i.e. a UI change is pending a restart).
pub async fn config_get(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.read().await;
    let restart_required =
        config.data_dir() != state.data_dir || config.models_dir() != state.models_dir;

    let mut val: serde_json::Value =
        serde_json::to_value(&*config).unwrap_or(serde_json::Value::Object(Default::default()));
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "restart_required".to_string(),
            serde_json::Value::Bool(restart_required),
        );
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&val).unwrap()))
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

    // Storage dirs (data_dir / models_dir) are captured into AppState at daemon
    // start, so a change only takes effect after a restart. We still create +
    // validate the new directory eagerly here so the user gets immediate
    // feedback (and a ready mount point) rather than a failed pull post-restart.
    let restart_required = {
        let cfg = state.config.read().await;
        let new_models_dir = cfg.models_dir();
        let new_data_dir = cfg.data_dir();
        let mut changed = false;
        if new_models_dir != state.models_dir {
            changed = true;
            if let Err(e) = ensure_writable_dir(&new_models_dir) {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"error":"models_dir not usable: {}"}}"#,
                        e.to_string().replace('"', "'")
                    )))
                    .unwrap();
            }
        }
        if new_data_dir != state.data_dir {
            changed = true;
            if let Err(e) = ensure_writable_dir(&new_data_dir) {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"error":"data_dir not usable: {}"}}"#,
                        e.to_string().replace('"', "'")
                    )))
                    .unwrap();
            }
        }
        changed
    };

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

    info!(restart_required, "Configuration safely mutated via /lf/config API");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"status":"updated","restart_required":{}}}"#,
            restart_required
        )))
        .unwrap()
}

/// `POST /lf/storage/apply` — Apply a storage directory change with migration intent.
///
/// Request body (all fields optional):
/// ```json
/// {
///   "models_dir": "/new/models",    // new models dir (omit to keep unchanged)
///   "data_dir": "/new/data",        // new data dir (omit to keep unchanged)
///   "models_action": "adopt"|"delete"|"repull",  // default: "adopt"
///   "data_action": "relocate"|"keep",             // default: "keep"
///   "copy_logs": false,             // copy logs/ when relocating data_dir
///   "exclude_from_repull": ["model-id"]  // models to NOT re-download (will be lost)
/// }
/// ```
///
/// Behavior:
/// - `adopt`: no file op; scan intent written so new dir is indexed on restart.
/// - `delete`: remove model files from old dir now; new dir empty after restart.
/// - `repull`: same as delete + queue re-downloads into new dir on next startup.
///
/// Returns `{ restart_required: true, would_lose: [...] }`. If `models_action`
/// is `repull` and some models have no `hf_repo` (can't re-download) and they
/// are not listed in `exclude_from_repull`, the request returns 422 with
/// `{ would_lose: [...] }` so the UI can prompt the user for confirmation.
pub async fn storage_apply(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    #[derive(serde::Deserialize, Default)]
    struct StorageApplyReq {
        models_dir: Option<String>,
        data_dir: Option<String>,
        /// Clear `models_dir` back to its built-in default (`{data_dir}/models`).
        /// Takes precedence over `models_dir`.
        #[serde(default)]
        reset_models_dir: bool,
        /// Clear `data_dir` back to its built-in default (`~/.lmforge`).
        /// Takes precedence over `data_dir`.
        #[serde(default)]
        reset_data_dir: bool,
        #[serde(default = "default_models_action")]
        models_action: String,
        #[serde(default = "default_data_action")]
        data_action: String,
        #[serde(default)]
        copy_logs: bool,
        #[serde(default)]
        exclude_from_repull: Vec<String>,
    }
    fn default_models_action() -> String {
        "adopt".to_string()
    }
    fn default_data_action() -> String {
        "keep".to_string()
    }

    let req: StorageApplyReq = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return apply_err(StatusCode::BAD_REQUEST, &format!("Invalid JSON: {e}"));
        }
    };

    // Reject while a pull is in-flight to avoid index corruption.
    if state
        .pull_in_flight
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return apply_err(
            StatusCode::CONFLICT,
            "A model pull is in progress; wait for it to complete before changing storage dirs",
        );
    }

    let old_models_dir = state.models_dir.clone();
    let old_data_dir = state.data_dir.clone();

    // Trim a path string and treat empty as "absent".
    let trimmed = |o: &Option<String>| {
        o.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };

    // Resulting config FIELD values after this apply (None = built-in default).
    // `reset_*` wins over an explicit path; absent path leaves the field as-is.
    let (cur_data_field, cur_models_field) = {
        let c = state.config.read().await;
        (c.data_dir.clone(), c.models_dir.clone())
    };
    let new_data_field = if req.reset_data_dir {
        None
    } else if let Some(d) = trimmed(&req.data_dir) {
        Some(d)
    } else {
        cur_data_field
    };
    let new_models_field = if req.reset_models_dir {
        None
    } else if let Some(m) = trimmed(&req.models_dir) {
        Some(m)
    } else {
        cur_models_field
    };

    // Resolve the resulting fields to effective paths with the same precedence
    // the daemon uses at startup (env > field > default). A fresh default config
    // carries no runtime `--data-dir` override, matching post-restart behavior.
    let (resolved_new_data_dir, resolved_new_models_dir) = crate::config::LmForgeConfig::resolve_dirs(
        new_data_field.as_deref(),
        new_models_field.as_deref(),
    );

    // Preserve no-auto-follow semantics: a data_dir change does NOT silently
    // relocate a default models_dir here (that would wipe the index). The models
    // dir is only considered changed when its field was explicitly touched.
    let models_field_touched = req.reset_models_dir || trimmed(&req.models_dir).is_some();
    let new_data_dir = resolved_new_data_dir;
    let new_models_dir = if models_field_touched {
        resolved_new_models_dir
    } else {
        old_models_dir.clone()
    };

    let models_dir_changed = new_models_dir != old_models_dir;
    let data_dir_changed = new_data_dir != old_data_dir;

    if !models_dir_changed && !data_dir_changed {
        return apply_err(
            StatusCode::BAD_REQUEST,
            "Neither models_dir nor data_dir changed",
        );
    }

    // Overlap checks for models_dir change.
    if models_dir_changed {
        if new_models_dir.starts_with(&old_models_dir) && new_models_dir != old_models_dir {
            return apply_err(
                StatusCode::BAD_REQUEST,
                "New models_dir is nested inside old models_dir — not supported",
            );
        }
        if old_models_dir.starts_with(&new_models_dir) {
            return apply_err(
                StatusCode::BAD_REQUEST,
                "Old models_dir is nested inside new models_dir — not supported",
            );
        }
        if let Err(e) = ensure_writable_dir(&new_models_dir) {
            return apply_err(
                StatusCode::BAD_REQUEST,
                &format!("New models_dir not usable: {e}"),
            );
        }
    }
    if data_dir_changed {
        if let Err(e) = ensure_writable_dir(&new_data_dir) {
            return apply_err(
                StatusCode::BAD_REQUEST,
                &format!("New data_dir not usable: {e}"),
            );
        }
    }

    // Load current index before any destructive action.
    let idx =
        crate::model::index::ModelIndex::load(&old_data_dir, &old_models_dir).unwrap_or_else(
            |_| crate::model::index::ModelIndex {
                schema_version: 2,
                models: vec![],
            },
        );

    // Build repull queue and collect models that would be permanently lost.
    let mut would_lose: Vec<String> = vec![];
    let mut repull_queue: Vec<crate::model::migration::RepullEntry> = vec![];

    if models_dir_changed && req.models_action == "repull" {
        for entry in idx.list() {
            if req.exclude_from_repull.contains(&entry.id) {
                would_lose.push(entry.id.clone());
                continue;
            }
            match &entry.hf_repo {
                Some(repo) if !repo.is_empty() => {
                    repull_queue.push(crate::model::migration::RepullEntry {
                        id: entry.id.clone(),
                        hf_repo: repo.clone(),
                        format: entry.format.clone(),
                        engine: entry.engine.clone(),
                    });
                }
                _ => {
                    would_lose.push(entry.id.clone());
                }
            }
        }
        // If models would be permanently lost and the caller hasn't explicitly
        // acknowledged them via exclude_from_repull, return 422 with the list.
        let unacknowledged: Vec<&String> = would_lose
            .iter()
            .filter(|id| !req.exclude_from_repull.contains(id))
            .collect();
        if !unacknowledged.is_empty() {
            return Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "error": "Some models cannot be re-downloaded (no hf_repo recorded). \
                                  Add their IDs to exclude_from_repull to confirm you accept the loss, \
                                  or change models_action to 'adopt' or 'delete'.",
                        "would_lose": unacknowledged,
                    }))
                    .unwrap(),
                ))
                .unwrap();
        }
    }

    // Unload all engines before any destructive file operations.
    let _ = state
        .command_tx
        .send(crate::engine::manager::ManagerCommand::UnloadAll)
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // Do-now: delete model files from the OLD models_dir when action requires it.
    if models_dir_changed && (req.models_action == "delete" || req.models_action == "repull") {
        for entry in idx.list() {
            let model_path = std::path::Path::new(&entry.path);
            if model_path.starts_with(&old_models_dir) && model_path.exists() {
                if let Err(e) = std::fs::remove_dir_all(model_path) {
                    warn!(
                        path = %model_path.display(),
                        error = %e,
                        "Failed to delete model dir during storage apply"
                    );
                }
            }
        }
        let empty_idx = crate::model::index::ModelIndex {
            schema_version: 2,
            models: vec![],
        };
        let _ = empty_idx.save(&old_data_dir, &old_models_dir);
    }

    // Do-now: copy regenerable data artifacts to the new data_dir.
    if data_dir_changed && req.data_action == "relocate" {
        for filename in &["models.json", "hardware.json"] {
            let src = old_data_dir.join(filename);
            let dst = new_data_dir.join(filename);
            if src.exists() {
                if let Err(e) = std::fs::copy(&src, &dst) {
                    warn!(src = %src.display(), dst = %dst.display(), error = %e, "Copy failed during data_dir relocation");
                }
            }
        }
        if req.copy_logs {
            let src_logs = old_data_dir.join("logs");
            let dst_logs = new_data_dir.join("logs");
            if src_logs.exists() {
                let _ = copy_dir_all(&src_logs, &dst_logs);
            }
        }
        // Engines NOT copied — venvs are not relocatable. Warn in the manifest.
        info!(
            old = %old_data_dir.display(),
            new = %new_data_dir.display(),
            "data_dir relocated (models.json + hardware.json). \
             Engines need reinstall: run `lmforge init` after restart."
        );
    }

    // Determine migration intent for the startup drain.
    let intent = if models_dir_changed {
        match req.models_action.as_str() {
            "repull" => crate::model::migration::MigrationIntent::Repull,
            _ => crate::model::migration::MigrationIntent::Scan,
        }
    } else {
        crate::model::migration::MigrationIntent::None
    };

    let manifest = crate::model::migration::PendingMigration {
        version: 1,
        models_dir: if models_dir_changed {
            Some(new_models_dir.to_string_lossy().to_string())
        } else {
            None
        },
        data_dir: if data_dir_changed {
            Some(new_data_dir.to_string_lossy().to_string())
        } else {
            None
        },
        intent,
        repull_queue,
    };

    if let Err(e) = manifest.save() {
        error!(error = %e, "Failed to write pending-migration.json");
        return apply_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to save migration manifest: {e}"),
        );
    }

    // Persist new dirs to the bootstrap config.toml. Assigning the resolved
    // field values (including `None` for a reset-to-default) ensures a cleared
    // directory is actually written back as "use the built-in default".
    {
        let mut config = state.config.write().await;
        config.models_dir = new_models_field.clone();
        config.data_dir = new_data_field.clone();
        if let Err(e) = config.save() {
            error!(error = %e, "Failed to save config after storage apply");
            return apply_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to save config: {e}"),
            );
        }
    }

    info!(
        models_action = %req.models_action,
        data_action = %req.data_action,
        models_dir_changed,
        data_dir_changed,
        "Storage apply complete — restart required to activate new dirs"
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "status": "applied",
                "restart_required": true,
                "would_lose": would_lose,
            }))
            .unwrap(),
        ))
        .unwrap()
}

fn apply_err(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({ "error": msg })).unwrap(),
        ))
        .unwrap()
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

/// Create `dir` (recursively) if missing and verify it is writable by touching
/// a temp probe file. Used when relocating data/models dirs via the config API
/// so failures surface immediately instead of on the next pull after restart.
fn ensure_writable_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".lmforge-write-probe");
    std::fs::write(&probe, b"ok")?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_writable_dir_creates_and_cleans_probe() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/data");
        ensure_writable_dir(&target).expect("should create + verify writable");
        assert!(target.is_dir(), "target dir must be created");
        assert!(
            !target.join(".lmforge-write-probe").exists(),
            "probe file must be removed after the check"
        );
    }

    #[test]
    fn copy_dir_all_copies_nested_tree() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
        std::fs::write(src.path().join("sub/b.txt"), b"beta").unwrap();

        let dst_root = dst.path().join("copied");
        copy_dir_all(src.path(), &dst_root).expect("copy must succeed");

        assert_eq!(std::fs::read(dst_root.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(dst_root.join("sub/b.txt")).unwrap(), b"beta");
    }

    #[test]
    fn apply_err_sets_status_and_json_body() {
        let resp = apply_err(StatusCode::BAD_REQUEST, "boom");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }
}
