use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info};

/// Tracks the last API request timestamp for idle model unloading.
///
/// When no requests arrive within the keepalive duration,
/// the engine process can be stopped to free resources.
/// oMLX handles this natively (LRU eviction) so it's skipped for oMLX.
#[derive(Debug, Clone)]
pub struct KeepaliveTracker {
    last_request: Arc<AtomicU64>,
    keepalive_secs: u64,
    enabled: bool,
}

impl KeepaliveTracker {
    /// Create a new keepalive tracker.
    /// `keepalive_secs = 0` means disabled (never unload).
    pub fn new(keepalive_secs: u64, engine_id: &str) -> Self {
        // oMLX handles model lifecycle natively — skip keepalive
        let enabled = keepalive_secs > 0 && engine_id != "omlx";

        if enabled {
            info!(keepalive_secs, engine_id, "Keepalive timer enabled");
        } else {
            debug!(engine_id, "Keepalive timer disabled (engine handles lifecycle natively or keepalive=0)");
        }

        Self {
            last_request: Arc::new(AtomicU64::new(now_secs())),
            keepalive_secs,
            enabled,
        }
    }

    /// Record that a request was received (resets the idle timer)
    pub fn touch(&self) {
        if self.enabled {
            self.last_request.store(now_secs(), Ordering::Relaxed);
        }
    }

    /// Check if the model has been idle longer than the keepalive duration
    pub fn is_idle(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let last = self.last_request.load(Ordering::Relaxed);
        let elapsed = now_secs().saturating_sub(last);
        elapsed > self.keepalive_secs
    }

    /// Get seconds since last request
    pub fn idle_secs(&self) -> u64 {
        let last = self.last_request.load(Ordering::Relaxed);
        now_secs().saturating_sub(last)
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Parse a keepalive duration string like "5m", "1h", "300", "infinite", "0"
pub fn parse_keepalive(s: &str) -> u64 {
    let s = s.trim().to_lowercase();
    if s == "infinite" || s == "inf" || s == "never" {
        return 0; // 0 means disabled
    }
    if s.ends_with('m') {
        s[..s.len() - 1].parse::<u64>().unwrap_or(300) * 60
    } else if s.ends_with('h') {
        s[..s.len() - 1].parse::<u64>().unwrap_or(1) * 3600
    } else if s.ends_with('s') {
        s[..s.len() - 1].parse::<u64>().unwrap_or(300)
    } else {
        s.parse::<u64>().unwrap_or(300) // default 5 minutes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keepalive_touch_and_idle() {
        let tracker = KeepaliveTracker::new(1, "llamacpp"); // 1 second keepalive
        tracker.touch();
        assert!(!tracker.is_idle());
    }

    #[test]
    fn test_keepalive_disabled_for_omlx() {
        let tracker = KeepaliveTracker::new(300, "omlx");
        assert!(!tracker.enabled);
        assert!(!tracker.is_idle()); // always returns false when disabled
    }

    #[test]
    fn test_keepalive_disabled_when_zero() {
        let tracker = KeepaliveTracker::new(0, "llamacpp");
        assert!(!tracker.enabled);
    }

    #[test]
    fn test_parse_keepalive_minutes() {
        assert_eq!(parse_keepalive("5m"), 300);
        assert_eq!(parse_keepalive("10m"), 600);
    }

    #[test]
    fn test_parse_keepalive_hours() {
        assert_eq!(parse_keepalive("1h"), 3600);
    }

    #[test]
    fn test_parse_keepalive_seconds() {
        assert_eq!(parse_keepalive("120s"), 120);
    }

    #[test]
    fn test_parse_keepalive_infinite() {
        assert_eq!(parse_keepalive("infinite"), 0);
        assert_eq!(parse_keepalive("inf"), 0);
    }

    #[test]
    fn test_parse_keepalive_raw_number() {
        assert_eq!(parse_keepalive("300"), 300);
    }
}
