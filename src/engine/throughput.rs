//! Decode-throughput sentinel fed from `llama-server` stderr.
//!
//! Detects the VRAM-spill failure signature observed on Windows/WDDM
//! (2026-07-06 incident): an engine that loads under VRAM pressure gets part
//! of its allocation silently paged into shared system RAM and decodes at a
//! fraction of its real speed until the pressure clears — 10.5 t/s early,
//! 67 t/s later in the same process. On some driver/arch combos (Blackwell,
//! 610.x) the spilled state also corrupts output. Decode speed can only
//! legitimately *decrease* over an engine's lifetime (context growth), so a
//! large mid-lifetime speed-up is anomalous and worth a loud warning.
//!
//! Sample sources, both emitted by `llama-server`:
//!
//! ```text
//! slot print_timing: id 3 | task 2051 | n_decoded = 595, tg =  10.77 t/s, tg_3s = 10.78 t/s
//! slot print_timing: id 3 | task 2051 |  eval time = 95014.10 ms / 1024 tokens (  92.79 ms per token,  10.78 tokens per second)
//! ```
//!
//! Same design rules as [`crate::engine::spec_observer`]: pure total parser,
//! bounded state, telemetry must never take down the spawn path.

use std::sync::{Arc, RwLock};
use tracing::warn;

/// A later sample this many times faster than the slowest one seen so far
/// flags a spill. Context growth explains maybe 2x decay (so 2x "recovery"
/// when a long-context task ends); the observed spill delta is 4-6x.
const SPEEDUP_FACTOR: f32 = 3.0;

/// Only arm the detector when the slow phase was genuinely slow. A healthy
/// GPU engine on any model that fits does > 30 t/s; this keeps big-but-legit
/// models with naturally moderate speeds from tripping on normal variance.
const SLOW_FLOOR_TPS: f32 = 30.0;

/// Pure detection rule, split out for unit tests: does observing `new_tps`
/// after a lifetime minimum of `min_tps` indicate a spill-then-recovery?
pub fn spill_suspected(min_tps: f32, new_tps: f32) -> bool {
    min_tps > 0.0 && min_tps <= SLOW_FLOOR_TPS && new_tps >= min_tps * SPEEDUP_FACTOR
}

/// Parse a decode-throughput sample (tokens/second) from one stderr line.
/// Returns `None` for anything that isn't a decode timing line; prompt-eval
/// (prefill) timings are explicitly excluded.
pub fn parse_decode_tps(line: &str) -> Option<f32> {
    // Periodic progress line: `..., tg =  10.77 t/s, tg_3s = ...`.
    if let Some(idx) = line.find(" tg = ") {
        let tail = line[idx + " tg = ".len()..].trim_start();
        let end = tail
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(tail.len());
        return tail[..end].parse().ok();
    }

    // End-of-request line: `eval time = ... ( ... ms per token, 10.78 tokens per second)`.
    // `prompt eval time` reports prefill speed — not comparable, skip it.
    if line.contains("eval time =") && !line.contains("prompt eval") {
        let idx = line.rfind("tokens per second")?;
        let head = line[..idx].trim_end();
        let start = head
            .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        return head[start..].parse().ok();
    }

    None
}

#[derive(Debug, Default)]
struct State {
    min_tps: Option<f32>,
    warned: bool,
}

/// Per-slot decode-speed watcher. Fed every stderr line by the tee task;
/// warns once per engine lifetime when the spill signature appears.
#[derive(Debug, Clone)]
pub struct ThroughputObserver {
    inner: Arc<RwLock<State>>,
    model_id: String,
    /// Only GPU variants can spill VRAM; on CPU builds the observer is a no-op.
    gpu: bool,
}

impl ThroughputObserver {
    pub fn new(model_id: &str, gpu: bool) -> Self {
        Self {
            inner: Arc::new(RwLock::new(State::default())),
            model_id: model_id.to_string(),
            gpu,
        }
    }

    /// Feed one stderr line. Non-timing lines are no-ops; lock poisoning is
    /// swallowed (telemetry must never break the tee task).
    pub fn record_line(&self, line: &str) {
        if !self.gpu {
            return;
        }
        let Some(tps) = parse_decode_tps(line) else {
            return;
        };
        if !tps.is_finite() || tps <= 0.0 {
            return;
        }
        let Ok(mut s) = self.inner.write() else {
            return;
        };
        if let Some(min) = s.min_tps
            && !s.warned
            && spill_suspected(min, tps)
        {
            s.warned = true;
            warn!(
                model_id = %self.model_id,
                slow_tps = min,
                recovered_tps = tps,
                "Decode throughput jumped >{SPEEDUP_FACTOR}x mid-lifetime — engine likely \
                 loaded under VRAM pressure and spilled into system memory (slow, and \
                 known to corrupt output on some Windows drivers). If this recurs, free \
                 VRAM before loading; on Windows/NVIDIA also consider setting \
                 'CUDA - Sysmem Fallback Policy' to 'Prefer No Sysmem Fallback' in the \
                 NVIDIA Control Panel."
            );
        }
        s.min_tps = Some(s.min_tps.map_or(tps, |m: f32| m.min(tps)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_decode_tps ─────────────────────────────────────────────────

    #[test]
    fn parses_periodic_tg_line() {
        let line = "4.13.008.504 I slot print_timing: id  3 | task 2051 | n_decoded =    595, tg =  10.77 t/s, tg_3s =  10.78 t/s";
        assert_eq!(parse_decode_tps(line), Some(10.77));
    }

    #[test]
    fn parses_final_eval_time_line() {
        let line = "4.52.800.927 I slot print_timing: id  3 | task 2051 |        eval time =   95014.10 ms /  1024 tokens (   92.79 ms per token,    10.78 tokens per second)";
        assert_eq!(parse_decode_tps(line), Some(10.78));
    }

    #[test]
    fn rejects_prompt_eval_line() {
        // Prefill speed, not decode speed — must not be sampled.
        let line = "4.52.800.922 I slot print_timing: id  3 | task 2051 | prompt eval time =     168.19 ms /     4 tokens (   42.05 ms per token,    23.78 tokens per second)";
        assert_eq!(parse_decode_tps(line), None);
    }

    #[test]
    fn rejects_unrelated_lines() {
        for l in [
            "main: HTTP server is listening, hostname: 127.0.0.1, port: 8080",
            "slot launch_slot_: id  0 | task 0 | processing task",
            "graphs reused = 3055",
            "",
        ] {
            assert_eq!(parse_decode_tps(l), None, "should reject: {l:?}");
        }
    }

    // ── spill_suspected ──────────────────────────────────────────────────

    #[test]
    fn incident_signature_trips() {
        // 2026-07-06: qwen3.5:4b:6bit at 10.5 t/s early, 66 t/s after recovery.
        assert!(spill_suspected(10.5, 66.0));
    }

    #[test]
    fn healthy_variance_does_not_trip() {
        // 133 -> 145 t/s (qwen3.5:2b healthy run): fast floor, small delta.
        assert!(!spill_suspected(133.0, 145.0));
        // Moderate model at 40 t/s recovering to 80: floor not met.
        assert!(!spill_suspected(40.0, 80.0));
        // Slow but stable (big model on small card without spill): no jump.
        assert!(!spill_suspected(12.0, 14.0));
    }

    // ── observer behaviour ───────────────────────────────────────────────

    #[test]
    fn observer_warns_once_and_only_on_gpu() {
        let obs = ThroughputObserver::new("m", true);
        obs.record_line("| n_decoded = 100, tg =  10.50 t/s, tg_3s = 10.50 t/s");
        obs.record_line("| n_decoded = 200, tg =  66.00 t/s, tg_3s = 66.00 t/s");
        assert!(obs.inner.read().unwrap().warned);

        let cpu = ThroughputObserver::new("m", false);
        cpu.record_line("| n_decoded = 100, tg =  5.00 t/s, tg_3s = 5.00 t/s");
        cpu.record_line("| n_decoded = 200, tg =  50.00 t/s, tg_3s = 50.00 t/s");
        assert!(!cpu.inner.read().unwrap().warned);
        assert!(cpu.inner.read().unwrap().min_tps.is_none());
    }

    #[test]
    fn observer_tracks_minimum_not_first_sample() {
        let obs = ThroughputObserver::new("m", true);
        obs.record_line("| tg =  60.00 t/s, tg_3s = 60.00 t/s");
        obs.record_line("| tg =  20.00 t/s, tg_3s = 20.00 t/s"); // slowdown: normal
        assert!(!obs.inner.read().unwrap().warned);
        obs.record_line("| tg =  65.00 t/s, tg_3s = 65.00 t/s"); // 3.25x jump off a <=30 floor
        assert!(obs.inner.read().unwrap().warned);
    }
}
