pub mod auth;
pub mod catalog;
pub mod concurrency;
pub mod health;
pub mod image_preflight;
pub mod logs_api;
pub mod metrics;
pub mod metrics_api;
pub mod native;
pub mod ollama;
pub mod openai;
pub mod proxy;
pub mod rerank;
pub mod sysinfo;
pub mod thinking;

use axum::{
    Router,
    body::Body,
    response::Response,
    routing::{delete, get, post},
};
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::RwLock;
use tracing::info;

use crate::engine::adapter::EngineAdapterInstance;
use crate::engine::manager::{EngineState, ManagerCommand, ModelHandle};
use crate::engine::registry::EngineConfig;

/// RAII marker that a request is actively using a model. While alive it keeps the
/// slot's in-flight count above zero so the orchestrator will not evict the model
/// mid-request. The count is incremented by the orchestrator when the model is
/// ensured `for_request`; this guard performs the matching decrement on drop —
/// including when a streaming response body is dropped (client disconnect) — via
/// [`attach_inflight_guard`].
pub struct InflightGuard {
    port: u16,
    inflight: Arc<AtomicU32>,
}

impl InflightGuard {
    /// Engine port to proxy this request to.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Keep an [`InflightGuard`] alive for the entire lifetime of a response body.
///
/// Streaming responses outlive the handler function, so the guard must ride along
/// with the body and only release (decrement the model's in-flight count) when the
/// stream finishes or is dropped (e.g. the client disconnects mid-generation).
/// Buffered responses are wrapped uniformly — harmless, the guard simply drops a
/// moment later once the single chunk is consumed.
pub fn attach_inflight_guard(resp: Response, guard: InflightGuard) -> Response {
    let (parts, body) = resp.into_parts();
    let guarded = body.into_data_stream().map(move |chunk| {
        // `guard` is moved into this closure; it drops with the stream.
        let _keep = &guard;
        chunk
    });
    Response::from_parts(parts, Body::from_stream(guarded))
}

/// Shared application state passed to all handlers
#[derive(Clone)]
pub struct AppState {
    pub engine_state: Arc<RwLock<EngineState>>,
    pub engine_config: EngineConfig,
    /// Residency strategy active for this engine instance. Used by handlers
    /// to tailor messages (e.g. advisory vs hard unload).
    pub residency_kind: crate::engine::ResidencyKind,
    /// Shared adapter — used by model_pull to attempt engine-native downloads.
    pub adapter: Arc<EngineAdapterInstance>,
    pub data_dir: std::path::PathBuf,
    /// Model weights directory. Defaults to `{data_dir}/models` but can be
    /// relocated (config/env/CLI) — e.g. a shared virtio-fs volume. Captured at
    /// daemon start; changing it via the config API requires a restart.
    pub models_dir: std::path::PathBuf,
    pub api_key: Option<String>,
    pub bind_address: String,
    pub config: Arc<RwLock<crate::config::LmForgeConfig>>,
    pub command_tx: tokio::sync::mpsc::Sender<ManagerCommand>,
    /// Broadcast channel — subscribers receive a full `EngineState` snapshot on every state change.
    /// The tray icon (in-process) subscribes via `status_tx.subscribe()`.
    /// The SSE endpoint `/lf/status/stream` also subscribes for external consumers.
    pub status_tx: tokio::sync::broadcast::Sender<EngineState>,
    /// Optional Tauri AppHandle — `Some` when running embedded inside the Tauri UI process.
    /// Used to emit `"lf:status"` events directly to the Svelte frontend (zero HTTP).
    /// `None` when LMForge runs as a standalone CLI daemon.
    #[cfg(feature = "tauri-embed")]
    pub app_handle: Option<tauri::AppHandle>,

    /// True while a model pull is in-flight. `POST /lf/storage/apply` checks
    /// this before proceeding to avoid partial-write races during an active download.
    /// Set via compare-and-swap in `model_pull`; cleared when the pull task completes.
    pub pull_in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,

    /// Snapshot of the currently in-flight model pull, surfaced in `GET /lf/status`
    /// so any client can observe (and re-attach to) a download regardless of which
    /// component started it. Updated by the pull task from the progress stream
    /// (independent of the SSE client connection); `None` when no pull is active.
    pub active_pull: std::sync::Arc<tokio::sync::RwLock<Option<ActivePull>>>,

    /// Queue-level status of a background models_dir re-pull migration (the
    /// "delete & re-download" path). Surfaced in `GET /lf/status` so the UI can
    /// render a global banner with overall progress. `None` when no migration is
    /// running. Per-model byte progress comes from `active_pull` (the migration
    /// task drives the same `pull_core` path as manual pulls).
    pub migration_status: std::sync::Arc<tokio::sync::RwLock<Option<MigrationStatus>>>,

    /// Cooperative cancel flag for the background migration task. Set by
    /// `POST /lf/migration/cancel`; the task checks it between models and aborts.
    pub migration_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Queue-level status of a background models_dir re-pull migration.
/// Lives in `AppState.migration_status` and is serialised into `GET /lf/status`.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MigrationStatus {
    /// Total models queued for re-download in this migration.
    pub total: usize,
    /// Models successfully re-downloaded so far.
    pub completed: usize,
    /// Model ids that failed to download (surfaced for manual retry).
    pub failed: Vec<String>,
    /// Model id currently downloading, or `None` between models / when done.
    pub current: Option<String>,
    /// True once the queue is drained (with or without failures).
    pub done: bool,
}

/// A shared, client-independent snapshot of the model pull in progress.
/// Lives in `AppState.active_pull` and is serialised into `GET /lf/status` so the
/// UI can show download progress even after the originating SSE stream is gone.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ActivePull {
    /// Canonical model id being pulled (resolved shortcut, e.g. `qwen2.5-vl:7b:4bit`).
    pub model: String,
    /// Human-readable current step / filename.
    pub file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub done: bool,
    pub error: Option<String>,
}

impl AppState {
    /// Fire a state-change notification to all subscribers:
    /// - `status_tx` broadcast channel → tray icon receiver (in-process)
    /// - `app_handle.emit("lf:status")` → Svelte frontend via Tauri IPC (in-process, zero HTTP)
    /// - SSE `/lf/status/stream` clients also receive via their `broadcast::Receiver`
    ///
    /// This is the single send site for all status updates. Call it after any mutation
    /// to `engine_state`.
    pub fn notify_state(&self, snapshot: EngineState) {
        // Broadcast to tray + SSE subscribers. Ignore errors — lagged receivers are fine,
        // they'll catch up on next change. SendError means no subscribers, also fine.
        let _ = self.status_tx.send(snapshot.clone());

        // Emit to the embedded Tauri frontend (no-op when running headless).
        #[cfg(feature = "tauri-embed")]
        if let Some(handle) = &self.app_handle {
            let _ = handle.emit("lf:status", &snapshot);
        }
    }

    /// Ensure a model is loaded for a *preload* (no request will immediately
    /// follow, e.g. `/lf/model/switch`). Does not touch the in-flight count.
    pub async fn ensure_model(
        &self,
        model_id: &str,
        keep_alive: Option<String>,
    ) -> Result<u16, Response> {
        self.ensure_model_inner(model_id, keep_alive, false)
            .await
            .map(|h| h.port)
    }

    /// Ensure a model is loaded to *serve a request*. Returns an [`InflightGuard`]
    /// that holds the model busy (uneviccible) until dropped — attach it to the
    /// response with [`attach_inflight_guard`] so it survives streaming bodies.
    pub async fn ensure_model_request(
        &self,
        model_id: &str,
        keep_alive: Option<String>,
    ) -> Result<InflightGuard, Response> {
        let handle = self.ensure_model_inner(model_id, keep_alive, true).await?;
        Ok(InflightGuard {
            port: handle.port,
            inflight: handle.inflight,
        })
    }

    async fn ensure_model_inner(
        &self,
        model_id: &str,
        keep_alive: Option<String>,
        for_request: bool,
    ) -> Result<ModelHandle, Response> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = ManagerCommand::EnsureModel {
            model_id: model_id.to_string(),
            keep_alive_override: keep_alive,
            for_request,
            reply: tx,
        };
        if self.command_tx.send(cmd).await.is_err() {
            return Err(axum::http::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::from(
                    r#"{"error":{"message":"Orchestrator channel closed"}}"#,
                ))
                .unwrap());
        }
        match rx.await {
            Ok(Ok(handle)) => Ok(handle),
            Ok(Err(e)) => Err(axum::http::Response::builder()
                .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                .body(axum::body::Body::from(format!(
                    r#"{{"error":{{"message":"Failed to load model: {}"}}}}"#,
                    e
                )))
                .unwrap()),
            Err(_) => Err(axum::http::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::from(
                    r#"{"error":{"message":"Orchestrator failed to reply"}}"#,
                ))
                .unwrap()),
        }
    }
}

/// Build the full axum Router with all routes.
///
/// `auth_policy` is wrapped in an Axum middleware layer that enforces the
/// trusted_networks + Bearer token decision matrix on every route. The
/// middleware itself short-circuits `/health` and `/metrics` so liveness
/// probes and Prometheus scrapers don't need credentials.
///
/// `concurrency` caps in-flight requests; observe `lmforge_requests_total`
/// with `status="503"` to detect saturation.
///
/// `max_body_bytes` overrides axum's 2 MB default request-body cap. VLM
/// payloads with inline base64 images (especially DocIntel-style high-DPI
/// PDF page renders) routinely exceed the default and trip 413s. Sized via
/// `ResourceConfig.max_request_body_mb` (env: `LMFORGE_MAX_BODY_MB`).
pub fn build_router(
    state: AppState,
    auth_policy: Arc<auth::AuthPolicy>,
    concurrency: concurrency::ConcurrencyLimit,
    max_body_bytes: usize,
) -> Router {
    info!(max_body_bytes, "Building API router");

    let ui_dir = resolve_ui_dir();
    if let Some(d) = &ui_dir {
        info!(ui_dir = %d.display(), "Serving dashboard UI at /ui");
    } else {
        info!("No UI build found; /ui route disabled (set LMFORGE_UI_DIR or run from repo root)");
    }

    let router = Router::new()
        // Health
        .route("/health", get(health::health))
        // Prometheus metrics (auth-bypass at the middleware layer alongside /health)
        .route("/metrics", get(metrics::metrics_handler))
        // OpenAI-compatible endpoints
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/completions", post(openai::completions))
        .route("/v1/embeddings", post(openai::embeddings))
        .route("/v1/rerank", post(rerank::rerank))
        .route("/v1/models", get(openai::models))
        .route("/v1/models/{id}", get(openai::model_get))
        // Ollama-compatible endpoints
        .route("/api/chat", post(ollama::chat))
        .route("/api/generate", post(ollama::generate))
        .route("/api/tags", get(ollama::tags))
        // LMForge native endpoints
        .route("/lf/metrics", get(metrics_api::metrics_digest))
        .route("/lf/logs/list", get(logs_api::logs_list))
        .route("/lf/logs/tail", get(logs_api::logs_tail))
        .route("/lf/logs/stream", get(logs_api::logs_stream))
        .route("/lf/status", get(native::status))
        .route("/lf/status/stream", get(native::status_stream))
        .route("/lf/hardware", get(native::hardware))
        .route("/lf/engines", get(native::engines))
        .route("/lf/sysinfo", get(sysinfo::sysinfo))
        .route("/lf/model/list", get(native::model_list))
        .route("/lf/model/switch", post(native::model_switch))
        .route("/lf/model/pull", post(native::model_pull))
        .route("/lf/model/unload", post(native::model_unload))
        .route("/lf/errors/dismiss", post(native::dismiss_error))
        .route("/lf/model/delete/{name}", delete(native::model_delete))
        .route(
            "/lf/config",
            get(native::config_get).post(native::config_update),
        )
        .route("/lf/shutdown", post(native::shutdown))
        .route("/lf/storage/apply", post(native::storage_apply))
        .route("/lf/migration/cancel", post(native::migration_cancel))
        .route("/lf/migration/retry", post(native::migration_retry))
        .route("/lf/catalog", get(catalog::catalog_list))
        .with_state(state);

    // Optional static dashboard mount. Only when a real ui/build directory
    // is present (env override, repo-relative path in dev, or copied into
    // the container image at /usr/local/share/lmforge/ui).
    let router = if let Some(dir) = ui_dir {
        let serve = tower_http::services::ServeDir::new(&dir)
            .fallback(tower_http::services::ServeFile::new(dir.join("index.html")));
        router.nest_service("/ui", serve)
    } else {
        router
    };

    router
        // Layer order from inner to outer (last applied = outermost):
        //   1. metrics_layer  — observes status & latency for every authed request
        //   2. concurrency    — gates inflight count, returns 503 when saturated
        //   3. auth_layer     — checks Bearer/CIDR before any work happens
        //   4. CORS           — outermost so preflight OPTIONS bypasses everything
        // Concurrency runs after auth (auth=cheap, no point burning a permit
        // on a request that will 401 anyway).
        .layer(axum::middleware::from_fn(metrics::metrics_layer))
        .layer(axum::middleware::from_fn_with_state(
            concurrency,
            concurrency::limit_layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_policy,
            auth::auth_layer,
        ))
        // Body limit is applied OUTSIDE auth so a hostile client can't burn
        // the daemon's auth path with multi-GB upload attempts. Axum rejects
        // with 413 before the request even reaches our middleware stack.
        .layer(axum::extract::DefaultBodyLimit::max(max_body_bytes))
        .layer(tower_http::cors::CorsLayer::permissive())
}

/// Locate the SvelteKit static build for the dashboard.
///
/// Resolution order:
///   1. `LMFORGE_UI_DIR` env var (used by the Docker image to point at
///      `/usr/local/share/lmforge/ui` after the multi-stage `npm run build`).
///   2. `$CARGO_MANIFEST_DIR/ui/build` for `cargo run` from the repo root.
///
/// Returns `None` when neither path resolves to a directory containing
/// `index.html` — the `/ui` route is then skipped entirely so the daemon
/// keeps booting in environments without a UI bundle.
pub fn resolve_ui_dir() -> Option<std::path::PathBuf> {
    if let Ok(s) = std::env::var("LMFORGE_UI_DIR") {
        let p = std::path::PathBuf::from(s);
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join("build");
    if dev.join("index.html").is_file() {
        return Some(dev);
    }
    None
}

/// Resolve the effective request body cap in bytes from config + env.
/// `LMFORGE_MAX_BODY_MB`, when set, wins over the config value so ops can
/// bump the limit without editing config.toml. Bounded to a sane minimum
/// (1 MB) to prevent footgun configs from rejecting normal chat traffic.
pub fn resolve_max_body_bytes(config_mb: usize) -> usize {
    let env_mb = std::env::var("LMFORGE_MAX_BODY_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let mb = env_mb.unwrap_or(config_mb).max(1);
    mb * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Use a guard to serialise env-var mutation across tests; cargo runs
    /// tests in parallel and `set_var` is process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn body_limit_uses_config_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised via ENV_LOCK; no concurrent reads of this var
        // happen elsewhere in the test binary.
        unsafe {
            std::env::remove_var("LMFORGE_MAX_BODY_MB");
        }
        assert_eq!(resolve_max_body_bytes(32), 32 * 1024 * 1024);
        assert_eq!(resolve_max_body_bytes(64), 64 * 1024 * 1024);
    }

    #[test]
    fn body_limit_env_overrides_config() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            std::env::set_var("LMFORGE_MAX_BODY_MB", "128");
        }
        assert_eq!(resolve_max_body_bytes(32), 128 * 1024 * 1024);
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            std::env::remove_var("LMFORGE_MAX_BODY_MB");
        }
    }

    #[test]
    fn body_limit_floors_to_one_mb() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            std::env::remove_var("LMFORGE_MAX_BODY_MB");
        }
        assert_eq!(resolve_max_body_bytes(0), 1024 * 1024);
    }

    #[test]
    fn body_limit_ignores_malformed_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            std::env::set_var("LMFORGE_MAX_BODY_MB", "not-a-number");
        }
        assert_eq!(resolve_max_body_bytes(16), 16 * 1024 * 1024);
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            std::env::remove_var("LMFORGE_MAX_BODY_MB");
        }
    }

    // ── pull_in_flight guard behaviour ─────────────────────────────────────────

    #[test]
    fn pull_in_flight_starts_false() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn pull_in_flight_cas_blocks_second_pull() {
        use std::sync::atomic::Ordering;
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // First caller: compare-and-swap false -> true succeeds.
        let first = flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
        assert!(first.is_ok(), "first acquire must succeed");

        // Second caller while first holds the flag: must fail (already true).
        let second = flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
        assert!(
            second.is_err(),
            "second acquire must fail while flag is held"
        );

        // Release.
        flag.store(false, Ordering::SeqCst);

        // Third caller after release: must succeed again.
        let third = flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
        assert!(third.is_ok(), "third acquire must succeed after release");
    }

    #[test]
    fn pull_in_flight_is_arc_shareable() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag2 = flag.clone();
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(flag2.load(std::sync::atomic::Ordering::SeqCst));
    }
}
