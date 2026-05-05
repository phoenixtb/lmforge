//! Inflight-request semaphore middleware.
//!
//! Caps simultaneous in-flight requests at `max_concurrent_requests`. Excess
//! requests wait briefly (up to a small queue timeout derived from
//! `request_queue_size`) before getting a 503. This stops a swarm of slow
//! VLM requests from starving the engine and lets liveness probes still get
//! through (`/health` and `/metrics` skip the limiter — same trust boundary
//! as the auth bypass).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response, StatusCode, header};
use axum::middleware::Next;
use axum::response::IntoResponse;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

/// Per-process inflight gate. Cloned cheaply via the inner Arc.
#[derive(Clone)]
pub struct ConcurrencyLimit {
    inner: Arc<Inner>,
}

struct Inner {
    sem: Semaphore,
    /// Max wait for a permit before bouncing. Computed from `request_queue_size`
    /// so a deeper queue tolerates longer waits.
    queue_wait: Duration,
}

impl ConcurrencyLimit {
    /// Build with `max_concurrent` permits and a queue-size-derived wait
    /// budget. `queue_size` of 0 disables waiting entirely (instant 503).
    pub fn new(max_concurrent: usize, queue_size: usize) -> Self {
        // Defensive: the Semaphore panics on 0; treat 0 as "no limit" by
        // giving it MAX so the gate is effectively a passthrough.
        let permits = if max_concurrent == 0 {
            usize::MAX >> 3
        } else {
            max_concurrent
        };
        // Per-permit wait budget: 100 ms per queued slot, capped at 10 s.
        // Empirically this absorbs short bursts without inflating tail
        // latency under sustained pressure.
        let queue_wait = Duration::from_millis((queue_size as u64).saturating_mul(100).min(10_000));
        Self {
            inner: Arc::new(Inner {
                sem: Semaphore::new(permits),
                queue_wait,
            }),
        }
    }

    /// Snapshot helper for tests + future /lf/status augmentation.
    pub fn available_permits(&self) -> usize {
        self.inner.sem.available_permits()
    }
}

/// Tower middleware function. Skips `/health`, `/metrics`, and the SSE status
/// stream so monitoring traffic isn't gated by inference load.
pub async fn limit_layer(
    State(limit): State<ConcurrencyLimit>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let path = req.uri().path();
    if matches!(path, "/health" | "/metrics" | "/lf/status/stream") {
        return next.run(req).await;
    }

    // Wait up to queue_wait for a permit; on timeout, return 503 with
    // Retry-After so well-behaved clients back off.
    let permit_fut = limit.inner.sem.acquire();
    let permit = match tokio::time::timeout(limit.inner.queue_wait, permit_fut).await {
        Ok(Ok(p)) => p,
        Ok(Err(_)) => {
            // Semaphore closed — only happens at shutdown.
            warn!("Concurrency semaphore closed unexpectedly");
            return service_unavailable("LMForge is shutting down");
        }
        Err(_elapsed) => {
            debug!(
                path = %path,
                "Concurrency limit exhausted — rejecting with 503"
            );
            return service_unavailable(
                "Server is at capacity. Retry after a moment or raise resources.max_concurrent_requests.",
            );
        }
    };

    let resp = next.run(req).await;
    drop(permit);
    resp
}

fn service_unavailable(message: &str) -> Response<Body> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "server_overloaded",
            "code": "concurrency_limit",
        }
    });
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::RETRY_AFTER, "1"),
        ],
        serde_json::to_string(&body).unwrap(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_max_means_unlimited() {
        let l = ConcurrencyLimit::new(0, 0);
        // permits should be huge but finite
        assert!(l.available_permits() > 1_000_000);
    }

    #[test]
    fn max_concurrent_reflects_permits() {
        let l = ConcurrencyLimit::new(4, 32);
        assert_eq!(l.available_permits(), 4);
    }

    #[tokio::test]
    async fn permit_release_returns_capacity() {
        let l = ConcurrencyLimit::new(1, 0);
        assert_eq!(l.available_permits(), 1);
        let p = l.inner.sem.acquire().await.unwrap();
        assert_eq!(l.available_permits(), 0);
        drop(p);
        assert_eq!(l.available_permits(), 1);
    }
}
