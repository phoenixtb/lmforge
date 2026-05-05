//! Prometheus `/metrics` endpoint and metric helpers.
//!
//! Recorder install is idempotent and best-effort: if Prometheus init fails
//! (e.g. running embedded inside Tauri where another recorder is already in
//! place) we log a warning and skip — the daemon must keep serving requests.
//!
//! Counters / histograms are thin macros from the `metrics` crate so handlers
//! can emit without dragging the recorder around.

use std::sync::OnceLock;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use axum::middleware::Next;
use axum::response::IntoResponse;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing::{info, warn};

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Counter / histogram names. Kept here so renames stay consistent across
/// emit sites. Industry naming convention: `lmforge_<subsystem>_<unit>`.
pub mod names {
    pub const REQUESTS_TOTAL: &str = "lmforge_requests_total";
    pub const REQUEST_DURATION_SECONDS: &str = "lmforge_request_duration_seconds";
    pub const MODEL_LOADS_TOTAL: &str = "lmforge_model_loads_total";
    pub const MODEL_LOAD_DURATION_SECONDS: &str = "lmforge_model_load_duration_seconds";
    pub const ACTIVE_MODELS: &str = "lmforge_active_models";
    pub const IMAGE_INPUTS_TOTAL: &str = "lmforge_image_inputs_total";
    pub const AUTH_REJECTIONS_TOTAL: &str = "lmforge_auth_rejections_total";
}

/// Install the Prometheus recorder once. Subsequent calls are no-ops.
/// Always returns Ok — failures are logged and the server boots without
/// metrics, never blocked by them.
pub fn init() {
    if HANDLE.get().is_some() {
        return;
    }
    match PrometheusBuilder::new().install_recorder() {
        Ok(handle) => {
            // Pre-register descriptions so they appear in /metrics output even
            // before the first counter increment.
            metrics::describe_counter!(
                names::REQUESTS_TOTAL,
                "Total HTTP requests served, labelled by endpoint, model, and status."
            );
            metrics::describe_histogram!(
                names::REQUEST_DURATION_SECONDS,
                "End-to-end request latency in seconds (handler entry to response)."
            );
            metrics::describe_counter!(
                names::MODEL_LOADS_TOTAL,
                "Cold-load attempts, labelled by model and result (success|failure)."
            );
            metrics::describe_histogram!(
                names::MODEL_LOAD_DURATION_SECONDS,
                "Cold-load wall-clock time in seconds, labelled by model."
            );
            metrics::describe_gauge!(
                names::ACTIVE_MODELS,
                "Number of models currently loaded into VRAM."
            );
            metrics::describe_counter!(
                names::IMAGE_INPUTS_TOTAL,
                "Image content blocks observed in chat requests, labelled by result."
            );
            metrics::describe_counter!(
                names::AUTH_REJECTIONS_TOTAL,
                "Requests rejected by the auth middleware."
            );
            let _ = HANDLE.set(handle);
            info!("Prometheus recorder installed at /metrics");
        }
        Err(e) => {
            warn!(error = %e, "Failed to install Prometheus recorder — /metrics will return 503");
        }
    }
}

/// `GET /metrics` — Prometheus text exposition format.
/// Returns 503 when the recorder failed to install (defensive — usually 200).
pub async fn metrics_handler() -> impl IntoResponse {
    match HANDLE.get() {
        Some(h) => Response::builder()
            .status(StatusCode::OK)
            // Standard Prometheus exposition content type.
            .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
            .body(Body::from(h.render()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(
                "metrics recorder unavailable (install_recorder failed at startup)",
            ))
            .unwrap(),
    }
}

/// Record a cold-load attempt result.
pub fn observe_model_load(model: &str, success: bool, elapsed_secs: f64) {
    let labels = [
        ("model", model.to_string()),
        (
            "result",
            if success { "success" } else { "failure" }.to_string(),
        ),
    ];
    metrics::counter!(names::MODEL_LOADS_TOTAL, &labels).increment(1);
    if success {
        let lat_labels = [("model", model.to_string())];
        metrics::histogram!(names::MODEL_LOAD_DURATION_SECONDS, &lat_labels).record(elapsed_secs);
    }
}

/// Update the active-models gauge.
pub fn set_active_models(n: u64) {
    metrics::gauge!(names::ACTIVE_MODELS).set(n as f64);
}

/// Record one image preflight result. `result` is `accepted`, `rejected`, or
/// `data_url` (no fetch needed).
pub fn observe_image(result: &'static str) {
    metrics::counter!(names::IMAGE_INPUTS_TOTAL, "result" => result).increment(1);
}

/// Record an auth rejection.
pub fn observe_auth_rejection() {
    metrics::counter!(names::AUTH_REJECTIONS_TOTAL).increment(1);
}

/// Tower middleware: records every request's elapsed time and status code.
/// Endpoint label is the matched route template (or path if unmatched).
/// We deliberately do *not* extract the model — that requires parsing the
/// JSON body, which is expensive on the hot path. Per-model granularity
/// (load events, active count) is recorded by the engine manager directly.
pub async fn metrics_layer(req: Request<Body>, next: Next) -> Response<Body> {
    // Skip self-observation: would create a counter that increments every
    // scrape, which is noise.
    let path = req.uri().path().to_string();
    if path == "/metrics" {
        return next.run(req).await;
    }

    let endpoint = match path.as_str() {
        // Compact label set: collapse path params into the route template so
        // /v1/models/{id} doesn't blow up cardinality.
        p if p.starts_with("/v1/models/") => "/v1/models/{id}".to_string(),
        p if p.starts_with("/lf/model/delete/") => "/lf/model/delete/{name}".to_string(),
        p => p.to_string(),
    };

    let started = Instant::now();
    let resp = next.run(req).await;
    let elapsed = started.elapsed().as_secs_f64();
    let status = resp.status().as_u16();

    let labels = [
        ("endpoint", endpoint.clone()),
        ("status", status.to_string()),
    ];
    metrics::counter!(names::REQUESTS_TOTAL, &labels).increment(1);
    let lat_labels = [("endpoint", endpoint)];
    metrics::histogram!(names::REQUEST_DURATION_SECONDS, &lat_labels).record(elapsed);

    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handler_returns_text_when_recorder_installed() {
        init();
        let resp = metrics_handler().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/plain"));
    }

    /// Regression test for the silent-counter bug we hit with
    /// `metrics-exporter-prometheus` 0.16.x + `metrics-util` 0.19.x: each
    /// `counter!()` macro invocation re-registered the handle and the cache
    /// lookup mis-hashed, so all increments landed on a fresh counter that
    /// was immediately discarded — `/metrics` always showed `1`.
    /// 0.18.x + util 0.20.x fixes the hashing path; this test pins us to a
    /// release that actually accumulates.
    #[test]
    fn macro_increments_accumulate() {
        use metrics_exporter_prometheus::PrometheusBuilder;
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            for _ in 0..7 {
                metrics::counter!("regression_total", "k" => "v").increment(1);
            }
        });
        let body = handle.render();
        let line = body
            .lines()
            .find(|l| l.starts_with("regression_total"))
            .expect("counter line missing");
        assert!(line.ends_with(" 7"), "expected 7 got {line}");
    }

    #[test]
    fn observe_helpers_do_not_panic_without_init() {
        // Even if init was never called, these macros should be no-ops via the
        // global `metrics` recorder facade. Verify they don't blow up.
        observe_model_load("model-a", true, 1.0);
        observe_image("accepted");
        observe_auth_rejection();
        set_active_models(3);
    }
}
