//! Speculative-decoding telemetry capture from `llama-server` stderr.
//!
//! `llama-server` prints a one-line acceptance summary at the end of every
//! request when `--spec-type` is active. The canonical format (since b9020):
//!
//! ```text
//! draft acceptance rate = 0.70312 (   90 accepted /   128 generated)
//! ```
//!
//! This module parses those lines, accumulates the per-slot counters, and
//! exposes a thread-safe snapshot for `/lf/status` telemetry. The spawn
//! path tees child stderr through [`SpecObserver::record_line`] before
//! writing the line to the per-model log file, so the parser sees every
//! sample without changing operator-visible logging.
//!
//! Design notes:
//! * **Pure parser** — [`parse_line`] is total + side-effect-free, making
//!   it trivial to fixture-test against any llama-server build.
//! * **Lock-free reads after construction** are not possible because we
//!   accumulate cumulative counters; instead we use a `parking_lot::RwLock`
//!   pattern via `std::sync::RwLock` (acceptable cost — a request emits one
//!   line and a status snapshot reads it once per status notify).
//! * **Bounded memory** — counters are u64; no log buffer is retained.

use std::sync::{Arc, RwLock};

/// Cumulative speculative-decoding stats, summed across every request the
/// engine has served since spawn.
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq)]
pub struct SpecStats {
    /// Total draft tokens generated across all requests.
    pub drafted_total: u64,
    /// Total draft tokens the main model accepted.
    pub accepted_total: u64,
    /// Number of `draft acceptance rate = …` lines parsed. Each line maps
    /// to one request that actually exercised speculation.
    pub samples: u64,
    /// Acceptance ratio from the most recent sample only. Useful for
    /// "is this getting worse over time?" eyeballing in the UI.
    pub last_accept_rate: f32,
    /// Cumulative accept ratio = `accepted_total / drafted_total`. NaN /
    /// infinity-safe: returns 0.0 when no samples yet.
    pub cumulative_accept_rate: f32,
}

/// One parsed sample. Public for unit tests; not surfaced in /lf/status.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecSample {
    pub accepted: u64,
    pub generated: u64,
    pub ratio: f32,
}

/// Parse a single stderr line. Returns `Some(sample)` only when the line
/// matches the canonical `draft acceptance rate = R (A accepted / G generated)`
/// format. Anything else returns None — the tee task drops it on the floor
/// after writing it to the engine log.
///
/// The parser is **deliberately tolerant**: extra whitespace, leading
/// log prefixes, trailing junk are all accepted.
pub fn parse_line(line: &str) -> Option<SpecSample> {
    // Find the anchor phrase. Allowing leading prefixes (timestamps, slot
    // ids, log-level glyphs that llama-server may add before the message)
    // means we don't have to maintain a regex for the prefix shape.
    let idx = line.find("draft acceptance rate")?;
    let tail = &line[idx + "draft acceptance rate".len()..];

    // Expect: ` = 0.NNN (   A accepted /   G generated)` — possibly more
    // whitespace, possibly a trailing newline. We crawl manually instead
    // of pulling in `regex` for one tiny pattern.
    let after_eq = tail.trim_start();
    let after_eq = after_eq.strip_prefix('=')?.trim_start();

    // Pull the ratio: read up to whitespace or `(`.
    let ratio_end = after_eq
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(after_eq.len());
    let ratio: f32 = after_eq[..ratio_end].parse().ok()?;

    // Find the parenthesised counter pair.
    let paren_open = after_eq[ratio_end..].find('(')?;
    let inside_start = ratio_end + paren_open + 1;
    let paren_close = after_eq[inside_start..].find(')')?;
    let inside = &after_eq[inside_start..inside_start + paren_close];

    // Inside looks like `   90 accepted /   128 generated`. We REQUIRE the
    // literal "accepted" on the left and "generated" on the right — these
    // word labels are part of the contract and protect us against future
    // format drift in either direction (swapped order, "tokens" instead of
    // "generated", etc.). A strict parser is preferable to a loose one
    // here: a wrong number is worse than no number.
    let (left, right) = inside.split_once('/')?;
    if !left.contains("accepted") || !right.contains("generated") {
        return None;
    }
    let accepted: u64 = left.split_whitespace().next()?.parse().ok()?;
    let generated: u64 = right.split_whitespace().next()?.parse().ok()?;

    Some(SpecSample {
        accepted,
        generated,
        ratio,
    })
}

/// Thread-safe accumulator. One per spawned `llama-server` slot; cloning
/// the `Arc` is cheap and lets the manager's status thread snapshot stats
/// without holding up the stderr tee.
#[derive(Debug, Clone)]
pub struct SpecObserver {
    inner: Arc<RwLock<SpecStats>>,
}

impl SpecObserver {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SpecStats::default())),
        }
    }

    /// Feed one stderr line into the observer. Lines that don't match the
    /// acceptance-rate pattern are no-ops. Lock acquisition failures
    /// (poisoned RwLock) silently fall through — telemetry must never
    /// take down the spawn path.
    pub fn record_line(&self, line: &str) {
        let Some(sample) = parse_line(line) else {
            return;
        };
        if let Ok(mut s) = self.inner.write() {
            s.drafted_total = s.drafted_total.saturating_add(sample.generated);
            s.accepted_total = s.accepted_total.saturating_add(sample.accepted);
            s.samples = s.samples.saturating_add(1);
            s.last_accept_rate = sample.ratio;
            s.cumulative_accept_rate = if s.drafted_total > 0 {
                (s.accepted_total as f32) / (s.drafted_total as f32)
            } else {
                0.0
            };
        }
    }

    /// Cheap, allocation-light snapshot for `/lf/status`. Returns the
    /// default (all zeros) when no samples have arrived yet OR the inner
    /// lock is poisoned — both states should render in the UI as "no
    /// telemetry yet" rather than an error.
    pub fn snapshot(&self) -> SpecStats {
        self.inner
            .read()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// True when at least one sample has been captured. Lets the status
    /// builder skip emitting an empty `spec` block on slots that haven't
    /// yet served a spec-active request (or have spec disabled).
    pub fn has_samples(&self) -> bool {
        self.inner.read().map(|s| s.samples > 0).unwrap_or(false)
    }
}

impl Default for SpecObserver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_line — canonical format ────────────────────────────────────

    #[test]
    fn parses_canonical_line_from_llama_server() {
        // Real output from llama.cpp b9351 with --spec-type mtp.
        let line = "draft acceptance rate = 0.70312 (   90 accepted /   128 generated)";
        let s = parse_line(line).expect("must parse");
        assert_eq!(s.accepted, 90);
        assert_eq!(s.generated, 128);
        assert!((s.ratio - 0.70312).abs() < 1e-5);
    }

    #[test]
    fn parses_high_ratio_with_more_padding() {
        // Bigger numbers, varying whitespace — the docs/speculative.md sample.
        let line = "draft acceptance rate = 0.57576 (  171 accepted /   297 generated)";
        let s = parse_line(line).expect("must parse");
        assert_eq!(s.accepted, 171);
        assert_eq!(s.generated, 297);
        assert!((s.ratio - 0.57576).abs() < 1e-5);
    }

    #[test]
    fn parses_with_leading_log_prefix() {
        // llama-server has emitted variants prefixed with slot id glyphs,
        // timestamps, or `srv` tags. The parser must tolerate any prefix.
        let lines = [
            "[2026-05-29 12:34:56] slot release | id 0 | draft acceptance rate = 0.5 (   5 accepted /    10 generated)",
            "srv  log_server: draft acceptance rate = 0.42857 (    3 accepted /     7 generated)",
            "          draft acceptance rate = 1.0 (   8 accepted /     8 generated)",
        ];
        for l in &lines {
            let s = parse_line(l).expect(l);
            assert!(s.generated > 0);
        }
    }

    #[test]
    fn parses_zero_acceptance() {
        let line = "draft acceptance rate = 0.00000 (    0 accepted /    16 generated)";
        let s = parse_line(line).expect("must parse");
        assert_eq!(s.accepted, 0);
        assert_eq!(s.generated, 16);
        assert_eq!(s.ratio, 0.0);
    }

    #[test]
    fn parses_perfect_acceptance() {
        let line = "draft acceptance rate = 1.00000 (   16 accepted /    16 generated)";
        let s = parse_line(line).expect("must parse");
        assert_eq!(s.accepted, 16);
        assert_eq!(s.generated, 16);
        assert_eq!(s.ratio, 1.0);
    }

    // ── parse_line — rejection cases ─────────────────────────────────────

    #[test]
    fn rejects_unrelated_lines() {
        let lines = [
            "main: HTTP server is listening, hostname: 127.0.0.1, port: 8080",
            "slot launch_slot_: id  0 | task 0 | processing task",
            "print_timings: load time =     124.30 ms",
            "",
            "draft acceptance rate", // truncated
            "draft acceptance rate =", // missing value
            "draft acceptance rate = abc (1 accepted / 2 generated)",
            "draft acceptance rate = 0.5", // no parens
            "draft acceptance rate = 0.5 (5 / 10)", // wrong inside
        ];
        for l in &lines {
            assert!(parse_line(l).is_none(), "should reject: {l:?}");
        }
    }

    #[test]
    fn rejects_inverted_inside_format() {
        // Some hypothetical future format with reversed words. We don't try
        // to be smart — exact word-order is part of the contract.
        let line = "draft acceptance rate = 0.5 (   5 generated /    10 accepted)";
        assert!(parse_line(line).is_none());
    }

    // ── SpecObserver — accumulation ──────────────────────────────────────

    #[test]
    fn observer_starts_empty() {
        let obs = SpecObserver::new();
        assert!(!obs.has_samples());
        let snap = obs.snapshot();
        assert_eq!(snap, SpecStats::default());
    }

    #[test]
    fn observer_accumulates_samples() {
        let obs = SpecObserver::new();
        obs.record_line("draft acceptance rate = 0.70312 (   90 accepted /   128 generated)");
        obs.record_line("draft acceptance rate = 0.50000 (    8 accepted /    16 generated)");

        let snap = obs.snapshot();
        assert_eq!(snap.samples, 2);
        assert_eq!(snap.accepted_total, 98);
        assert_eq!(snap.drafted_total, 144);
        assert_eq!(snap.last_accept_rate, 0.5);
        // 98 / 144 ≈ 0.6806
        assert!((snap.cumulative_accept_rate - 0.6805_f32).abs() < 1e-3);
    }

    #[test]
    fn observer_ignores_unrelated_lines() {
        let obs = SpecObserver::new();
        obs.record_line("ggml_cuda_init: found 1 CUDA devices");
        obs.record_line("HTTP/1.1 200 OK");
        obs.record_line("draft acceptance rate = 0.5 (    1 accepted /     2 generated)");
        obs.record_line("");

        let snap = obs.snapshot();
        assert_eq!(snap.samples, 1, "only the matching line counts");
        assert_eq!(snap.accepted_total, 1);
        assert_eq!(snap.drafted_total, 2);
    }

    #[test]
    fn observer_handles_zero_division_safely() {
        // A pathological line with generated=0 SHOULD be accepted by the
        // parser (it's syntactically valid) but must not produce NaN in
        // cumulative_accept_rate. We assert no division-by-zero crash.
        let obs = SpecObserver::new();
        obs.record_line("draft acceptance rate = 0.0 (    0 accepted /     0 generated)");
        let snap = obs.snapshot();
        assert!(snap.cumulative_accept_rate.is_finite());
        assert_eq!(snap.cumulative_accept_rate, 0.0);
    }

    #[test]
    fn observer_clones_share_state() {
        // Arc semantics — cloning the observer must give us a view onto
        // the same stats. The status thread takes a clone; the tee task
        // keeps the original.
        let a = SpecObserver::new();
        let b = a.clone();
        a.record_line("draft acceptance rate = 0.5 (    1 accepted /     2 generated)");
        let snap_b = b.snapshot();
        assert_eq!(snap_b.samples, 1);
    }
}
