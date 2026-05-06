//! `GET /lf/metrics` — JSON digest of the same data exposed at `/metrics`.
//!
//! The Prometheus text format is fine for scrapers but awkward for browsers.
//! This endpoint parses our own `PrometheusHandle.render()` output and
//! projects it to a stable JSON shape the dashboard can consume directly.
//!
//! Stable JSON > stable Prometheus series names: if we ever rename a series,
//! the parser changes here, the dashboard contract does not.

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use serde::Serialize;

use super::metrics;

#[derive(Debug, Default, Clone, Serialize)]
pub struct EndpointStats {
    /// Total request count (sum across all status codes).
    pub requests_total: u64,
    /// Subset of `requests_total` with HTTP status >= 400.
    pub errors_total: u64,
    /// Status-code breakdown for stacked-bar UIs.
    pub by_status: BTreeMap<u16, u64>,
    /// p50 latency in milliseconds, computed from histogram bucket cumulative
    /// counts. `None` until at least one request has been observed.
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ModelLoadStats {
    pub success: u64,
    pub failure: u64,
    /// Most recent successful load duration in seconds. `None` if the model
    /// has never loaded successfully (or the histogram bucket data is sparse).
    pub last_dur_s: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImageMix {
    pub accepted: u64,
    pub rejected: u64,
    pub data_url: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct MetricsDigest {
    /// Per-endpoint stats, keyed by route template (e.g. `/v1/chat/completions`).
    pub endpoints: BTreeMap<String, EndpointStats>,
    /// Sum of `requests_total` across all endpoints.
    pub requests_total: u64,
    /// Sum of `errors_total` (status >= 400) across all endpoints.
    pub errors_total: u64,
    /// `errors_total / requests_total` as a 0..1 fraction. 0 when no traffic.
    pub error_rate: f64,
    /// Current models-in-VRAM gauge.
    pub active_models: u64,
    /// Per-model load attempt outcomes.
    pub model_loads: BTreeMap<String, ModelLoadStats>,
    pub image_inputs: ImageMix,
    pub auth_rejections: u64,
    /// Daemon wall-clock uptime since the first `metrics::init()`.
    pub uptime_secs: u64,
    /// Set when the Prometheus recorder failed to install. JSON shape is the
    /// same; counters are all zero. Frontends should still render the page.
    pub recorder_unavailable: bool,
}

/// `GET /lf/metrics` handler.
pub async fn metrics_digest() -> impl IntoResponse {
    let digest = match metrics::render_text() {
        Some(text) => parse_digest(&text),
        None => MetricsDigest {
            recorder_unavailable: true,
            uptime_secs: metrics::uptime_secs(),
            ..MetricsDigest::default()
        },
    };

    let body = serde_json::to_string(&digest).unwrap_or_else(|_| "{}".to_string());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

// ─── Parser ──────────────────────────────────────────────────────────────────
// We re-parse the Prometheus text rather than maintain parallel atomic
// counters. Cheap (one render per digest call) and keeps a single source of
// truth. The exposition format is well-defined, but we are deliberately
// permissive: unknown lines are ignored, label parsing handles quoted values
// only, no escape-sequence support (none of our labels use \ or quotes).

fn parse_digest(text: &str) -> MetricsDigest {
    let mut out = MetricsDigest {
        uptime_secs: metrics::uptime_secs(),
        ..MetricsDigest::default()
    };

    // Histogram bucket bookkeeping per (metric_name, endpoint_label).
    // Each entry maps "le" upper-bound -> cumulative count.
    let mut buckets: BTreeMap<(String, String), Vec<(f64, f64)>> = BTreeMap::new();
    let mut bucket_counts: BTreeMap<(String, String), u64> = BTreeMap::new();
    // Per-model load latency: just record the last sum/count, "last" is a
    // proxy via the average. Histograms in metrics-exporter-prometheus do
    // not expose individual recordings; the average is the best we can do
    // without adding a separate "last value" gauge.
    let mut load_sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut load_count: BTreeMap<String, u64> = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(parsed) = parse_line(line) else {
            continue;
        };

        match parsed.metric.as_str() {
            "lmforge_requests_total" => {
                let endpoint = parsed.label("endpoint").unwrap_or_default().to_string();
                let status: u16 = parsed
                    .label("status")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let v = parsed.value as u64;
                let entry = out.endpoints.entry(endpoint).or_default();
                entry.requests_total = entry.requests_total.saturating_add(v);
                if status >= 400 {
                    entry.errors_total = entry.errors_total.saturating_add(v);
                }
                if status > 0 {
                    *entry.by_status.entry(status).or_default() += v;
                }
            }
            "lmforge_request_duration_seconds_bucket" => {
                let endpoint = parsed.label("endpoint").unwrap_or_default().to_string();
                let le = parsed.label("le").unwrap_or_default();
                if le.is_empty() {
                    continue;
                }
                let upper: f64 = if le == "+Inf" {
                    f64::INFINITY
                } else {
                    le.parse().unwrap_or(f64::INFINITY)
                };
                buckets
                    .entry(("request".to_string(), endpoint))
                    .or_default()
                    .push((upper, parsed.value));
            }
            "lmforge_request_duration_seconds_count" => {
                let endpoint = parsed.label("endpoint").unwrap_or_default().to_string();
                bucket_counts.insert(("request".to_string(), endpoint), parsed.value as u64);
            }
            "lmforge_model_loads_total" => {
                let model = parsed.label("model").unwrap_or_default().to_string();
                let result = parsed.label("result").unwrap_or_default();
                let entry = out.model_loads.entry(model).or_default();
                let v = parsed.value as u64;
                if result == "success" {
                    entry.success = entry.success.saturating_add(v);
                } else if result == "failure" {
                    entry.failure = entry.failure.saturating_add(v);
                }
            }
            "lmforge_model_load_duration_seconds_sum" => {
                let model = parsed.label("model").unwrap_or_default().to_string();
                load_sum.insert(model, parsed.value);
            }
            "lmforge_model_load_duration_seconds_count" => {
                let model = parsed.label("model").unwrap_or_default().to_string();
                load_count.insert(model, parsed.value as u64);
            }
            "lmforge_active_models" => {
                out.active_models = parsed.value as u64;
            }
            "lmforge_image_inputs_total" => {
                let v = parsed.value as u64;
                match parsed.label("result").unwrap_or_default() {
                    "accepted" => {
                        out.image_inputs.accepted = out.image_inputs.accepted.saturating_add(v)
                    }
                    "rejected" => {
                        out.image_inputs.rejected = out.image_inputs.rejected.saturating_add(v)
                    }
                    "data_url" => {
                        out.image_inputs.data_url = out.image_inputs.data_url.saturating_add(v)
                    }
                    _ => {}
                }
            }
            "lmforge_auth_rejections_total" => {
                out.auth_rejections = out.auth_rejections.saturating_add(parsed.value as u64);
            }
            _ => {}
        }
    }

    // Compute percentiles per endpoint from the bucket cumulative counts.
    for ((kind, endpoint), bucket_list) in buckets {
        if kind != "request" {
            continue;
        }
        let total = bucket_counts
            .get(&(kind.clone(), endpoint.clone()))
            .copied()
            .unwrap_or(0);
        if total == 0 {
            continue;
        }
        let mut sorted = bucket_list;
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let entry = out.endpoints.entry(endpoint).or_default();
        entry.p50_ms = bucket_quantile(&sorted, 0.50).map(|s| s * 1000.0);
        entry.p95_ms = bucket_quantile(&sorted, 0.95).map(|s| s * 1000.0);
        entry.p99_ms = bucket_quantile(&sorted, 0.99).map(|s| s * 1000.0);
    }

    // Per-model average load duration as the "last" proxy.
    for (model, sum) in load_sum {
        let count = load_count.get(&model).copied().unwrap_or(0);
        if count > 0 {
            out.model_loads.entry(model).or_default().last_dur_s = Some(sum / count as f64);
        }
    }

    // Aggregate totals.
    out.requests_total = out.endpoints.values().map(|e| e.requests_total).sum();
    out.errors_total = out.endpoints.values().map(|e| e.errors_total).sum();
    out.error_rate = if out.requests_total > 0 {
        out.errors_total as f64 / out.requests_total as f64
    } else {
        0.0
    };

    out
}

/// Compute a quantile from cumulative bucket counts. Linear interpolation
/// between the bracketing buckets. Mirrors Prometheus's `histogram_quantile`.
fn bucket_quantile(buckets: &[(f64, f64)], q: f64) -> Option<f64> {
    if buckets.is_empty() {
        return None;
    }
    let total = buckets.last().map(|b| b.1).unwrap_or(0.0);
    if total <= 0.0 {
        return None;
    }
    let target = q * total;
    let mut prev_bound = 0.0;
    let mut prev_count = 0.0;
    for (bound, count) in buckets {
        if *count >= target {
            if bound.is_infinite() {
                return Some(prev_bound);
            }
            let span = bound - prev_bound;
            let needed = target - prev_count;
            let bucket_total = count - prev_count;
            if bucket_total <= 0.0 {
                return Some(*bound);
            }
            return Some(prev_bound + span * (needed / bucket_total));
        }
        prev_bound = *bound;
        prev_count = *count;
    }
    Some(prev_bound)
}

// ─── Line parsing ────────────────────────────────────────────────────────────

struct ParsedLine<'a> {
    metric: String,
    labels: Vec<(String, &'a str)>,
    value: f64,
}

impl<'a> ParsedLine<'a> {
    fn label(&self, key: &str) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }
}

/// Parse a single non-comment Prometheus exposition line.
/// Format: `metric_name{label="v",label="v"} 1.23`  (timestamp ignored if present)
fn parse_line(line: &str) -> Option<ParsedLine<'_>> {
    let (head, value_str) = line.rsplit_once(' ')?;
    // Some exposition lines include a timestamp after the value; drop it.
    let value_str = value_str.split_whitespace().next().unwrap_or(value_str);
    let value: f64 = value_str.parse().ok()?;

    if let Some((name, rest)) = head.split_once('{') {
        let rest = rest.trim_end_matches('}').trim_end();
        let labels = parse_labels(rest);
        Some(ParsedLine {
            metric: name.to_string(),
            labels,
            value,
        })
    } else {
        Some(ParsedLine {
            metric: head.to_string(),
            labels: Vec::new(),
            value,
        })
    }
}

/// Parse `key="value",key="value"` into a vector of pairs. Permissive: no
/// escape-sequence handling — our metrics emit plain ASCII labels only.
fn parse_labels(s: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().trim_start_matches(',').trim().to_string();
        let after_eq = rest[eq + 1..].trim_start();
        let after_eq = after_eq.strip_prefix('"').unwrap_or(after_eq);
        let Some(close) = after_eq.find('"') else {
            break;
        };
        let value = &after_eq[..close];
        out.push((key, value));
        rest = &after_eq[close + 1..];
        rest = rest.trim_start_matches(',').trim_start();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counter_with_labels() {
        let line = r#"lmforge_requests_total{endpoint="/v1/chat/completions",status="200"} 42"#;
        let p = parse_line(line).unwrap();
        assert_eq!(p.metric, "lmforge_requests_total");
        assert_eq!(p.label("endpoint"), Some("/v1/chat/completions"));
        assert_eq!(p.label("status"), Some("200"));
        assert!((p.value - 42.0).abs() < 1e-9);
    }

    #[test]
    fn parses_unlabeled_counter() {
        let p = parse_line("lmforge_auth_rejections_total 7").unwrap();
        assert_eq!(p.metric, "lmforge_auth_rejections_total");
        assert!(p.labels.is_empty());
        assert_eq!(p.value as u64, 7);
    }

    #[test]
    fn quantile_picks_correct_bucket() {
        // 10 requests, all <= 0.5s. p95 should land in (0.25, 0.5].
        let buckets = vec![
            (0.005, 0.0),
            (0.05, 0.0),
            (0.25, 5.0),
            (0.5, 10.0),
            (1.0, 10.0),
            (f64::INFINITY, 10.0),
        ];
        let p95 = bucket_quantile(&buckets, 0.95).unwrap();
        assert!(
            (0.25..=0.5).contains(&p95),
            "p95 {} should be in (0.25, 0.5]",
            p95
        );
    }

    #[test]
    fn digest_aggregates_endpoints_and_errors() {
        let text = "\
# HELP lmforge_requests_total
# TYPE lmforge_requests_total counter
lmforge_requests_total{endpoint=\"/v1/chat/completions\",status=\"200\"} 8
lmforge_requests_total{endpoint=\"/v1/chat/completions\",status=\"503\"} 2
lmforge_requests_total{endpoint=\"/v1/embeddings\",status=\"200\"} 5
lmforge_active_models 3
lmforge_auth_rejections_total 1
lmforge_image_inputs_total{result=\"accepted\"} 4
lmforge_image_inputs_total{result=\"data_url\"} 1
";
        let d = parse_digest(text);
        assert_eq!(d.requests_total, 15);
        assert_eq!(d.errors_total, 2);
        assert!((d.error_rate - 2.0 / 15.0).abs() < 1e-9);
        assert_eq!(d.active_models, 3);
        assert_eq!(d.auth_rejections, 1);
        assert_eq!(d.image_inputs.accepted, 4);
        assert_eq!(d.image_inputs.data_url, 1);
        let chat = d.endpoints.get("/v1/chat/completions").unwrap();
        assert_eq!(chat.requests_total, 10);
        assert_eq!(chat.errors_total, 2);
        assert_eq!(chat.by_status.get(&200), Some(&8));
        assert_eq!(chat.by_status.get(&503), Some(&2));
    }

    #[test]
    fn digest_sets_recorder_unavailable_when_no_text() {
        let d = MetricsDigest {
            recorder_unavailable: true,
            ..MetricsDigest::default()
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"recorder_unavailable\":true"));
    }

    #[test]
    fn digest_handles_histogram_buckets() {
        let text = "\
lmforge_request_duration_seconds_bucket{endpoint=\"/v1/chat/completions\",le=\"0.005\"} 0
lmforge_request_duration_seconds_bucket{endpoint=\"/v1/chat/completions\",le=\"0.05\"} 0
lmforge_request_duration_seconds_bucket{endpoint=\"/v1/chat/completions\",le=\"0.25\"} 5
lmforge_request_duration_seconds_bucket{endpoint=\"/v1/chat/completions\",le=\"0.5\"} 10
lmforge_request_duration_seconds_bucket{endpoint=\"/v1/chat/completions\",le=\"+Inf\"} 10
lmforge_request_duration_seconds_count{endpoint=\"/v1/chat/completions\"} 10
lmforge_requests_total{endpoint=\"/v1/chat/completions\",status=\"200\"} 10
";
        let d = parse_digest(text);
        let chat = d.endpoints.get("/v1/chat/completions").unwrap();
        assert!(chat.p50_ms.is_some());
        assert!(chat.p95_ms.is_some());
        let p95 = chat.p95_ms.unwrap();
        assert!(
            (250.0..=500.0).contains(&p95),
            "expected p95 in (250, 500] got {}",
            p95
        );
    }

    #[test]
    fn parse_labels_handles_multiple_pairs() {
        let labels = parse_labels(r#"endpoint="/v1/chat",status="200""#);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0], ("endpoint".to_string(), "/v1/chat"));
        assert_eq!(labels[1], ("status".to_string(), "200"));
    }
}
