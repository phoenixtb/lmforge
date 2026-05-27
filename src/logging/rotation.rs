use std::fs;
use std::path::{Path, PathBuf};

use tracing_appender::rolling;

/// Create a rolling file appender for the main lmforge log.
/// Per SRS §12.4: rotates at daily boundary; keeps 5 files.
///
/// Note: tracing-appender doesn't support size-based rotation natively.
/// For v0.1, we use daily rotation as a reasonable approximation.
/// Size-based rotation (50 MB / 5 files) can be added in v0.2 with
/// a custom appender or a crate like `rolling-file`.
pub fn create_appender(logs_dir: &Path) -> rolling::RollingFileAppender {
    rolling::daily(logs_dir, "lmforge.log")
}

/// Default size threshold for engine log rotation: 50 MB.
/// Override with `LMFORGE_ENGINE_LOG_MAX_MB`.
const DEFAULT_ENGINE_LOG_MAX_MB: u64 = 50;

/// Default number of rotated copies to keep per stream.
/// Override with `LMFORGE_ENGINE_LOG_KEEP`.
const DEFAULT_ENGINE_LOG_KEEP: usize = 3;

/// Sanitize a model id into a path-safe filename component.
/// Replaces `:` `/` `\` and any other path-unsafe chars with `_`.
pub fn sanitize_model_id(model_id: &str) -> String {
    model_id
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.' | '_' => c,
            _ => '_',
        })
        .collect()
}

/// Compute the per-model log path for a given stream (`stdout` | `stderr`).
pub fn engine_log_path(logs_dir: &Path, model_id: &str, stream: &str) -> PathBuf {
    let safe = sanitize_model_id(model_id);
    logs_dir.join(format!("engine-{safe}.{stream}.log"))
}

/// If the existing log file exceeds `max_mb`, rename it to a `.N` suffix and
/// prune copies beyond `keep`. Safe to call with a non-existent path. Errors
/// are best-effort — failing rotation should never block engine startup.
pub fn rotate_if_oversize(path: &Path, max_mb: u64, keep: usize) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() < max_mb.saturating_mul(1024 * 1024) {
        return;
    }
    // Shift .N → .N+1 from highest down so we never overwrite a kept copy.
    for i in (1..=keep).rev() {
        let from = with_suffix(path, i);
        let to = with_suffix(path, i + 1);
        if from.exists() {
            let _ = fs::rename(&from, &to);
        }
    }
    // Move current → .1
    let one = with_suffix(path, 1);
    let _ = fs::rename(path, one);
    // Drop anything beyond keep
    let stale = with_suffix(path, keep + 1);
    if stale.exists() {
        let _ = fs::remove_file(stale);
    }
}

fn with_suffix(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Read the rotation threshold (MB) from env, falling back to default.
pub fn engine_log_max_mb() -> u64 {
    std::env::var("LMFORGE_ENGINE_LOG_MAX_MB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_ENGINE_LOG_MAX_MB)
}

/// Read the rotation keep count from env, falling back to default.
pub fn engine_log_keep() -> usize {
    std::env::var("LMFORGE_ENGINE_LOG_KEEP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ENGINE_LOG_KEEP)
}

/// Open a per-model engine log file for append, rotating it first if it has
/// outgrown the configured threshold. Used by every adapter's `start()` so
/// Linux-side llama.cpp (which can stream multi-GB of decode logs) doesn't
/// fill the disk.
pub fn prepare_engine_log(
    logs_dir: &Path,
    model_id: &str,
    stream: &str,
) -> std::io::Result<std::fs::File> {
    fs::create_dir_all(logs_dir)?;
    let path = engine_log_path(logs_dir, model_id, stream);
    rotate_if_oversize(&path, engine_log_max_mb(), engine_log_keep());
    fs::OpenOptions::new().create(true).append(true).open(path)
}

/// Default max lines for stderr tail propagation into `/lf/status`.
/// Override with `LMFORGE_STDERR_TAIL_LINES`.
pub const DEFAULT_STDERR_TAIL_LINES: usize = 32;

/// Default max bytes for stderr tail. Caps memory in the EngineState
/// snapshot regardless of how long each stderr line is.
pub const DEFAULT_STDERR_TAIL_MAX_BYTES: usize = 8 * 1024;

/// Read the last `max_lines` (capped at `max_bytes`) of a model's stderr log.
///
/// Why this exists: worker engines stream multi-MB of stderr but our
/// `/lf/status` response should fit in a single HTTP page. Tailing on demand
/// keeps the orchestrator's memory footprint flat — we only materialise the
/// tail when surfacing an error, not on every heartbeat.
///
/// Returns `None` when:
///   - the log file doesn't exist (worker never started, or stderr was swallowed)
///   - the log is empty
///   - any I/O error occurs (treated as "no useful context to surface")
///
/// The returned string preserves the original line ordering (oldest → newest).
pub fn read_stderr_tail(logs_dir: &Path, model_id: &str) -> Option<String> {
    let max_lines = std::env::var("LMFORGE_STDERR_TAIL_LINES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_STDERR_TAIL_LINES);
    read_stderr_tail_with_limits(logs_dir, model_id, max_lines, DEFAULT_STDERR_TAIL_MAX_BYTES)
}

/// Explicit-limits variant of [`read_stderr_tail`] — exposed for testability.
pub fn read_stderr_tail_with_limits(
    logs_dir: &Path,
    model_id: &str,
    max_lines: usize,
    max_bytes: usize,
) -> Option<String> {
    let path = engine_log_path(logs_dir, model_id, "stderr");
    let content = fs::read_to_string(&path).ok()?;
    if content.is_empty() {
        return None;
    }
    let tail = tail_lines(&content, max_lines, max_bytes);
    if tail.trim().is_empty() {
        None
    } else {
        Some(tail)
    }
}

/// Return the last `max_lines` non-empty lines of `s`, capped at `max_bytes`
/// (older lines drop first when the byte cap is hit). Pure function — exposed
/// for unit tests.
pub fn tail_lines(s: &str, max_lines: usize, max_bytes: usize) -> String {
    if max_lines == 0 || max_bytes == 0 {
        return String::new();
    }
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    let mut selected: Vec<&str> = lines[start..].to_vec();

    // Trim from the FRONT until we fit in max_bytes (accounting for newlines).
    while selected.iter().map(|l| l.len() + 1).sum::<usize>() > max_bytes && !selected.is_empty() {
        selected.remove(0);
    }
    selected.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_colons_and_slashes() {
        assert_eq!(
            sanitize_model_id("qwen2.5-vl:3b:4bit"),
            "qwen2.5-vl_3b_4bit"
        );
        assert_eq!(sanitize_model_id("foo/bar"), "foo_bar");
    }

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize_model_id("safe-name_v1.5"), "safe-name_v1.5");
    }

    #[test]
    fn engine_log_path_uses_sanitized_id() {
        let p = engine_log_path(Path::new("/tmp"), "qwen2.5-vl:3b:4bit", "stderr");
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "engine-qwen2.5-vl_3b_4bit.stderr.log"
        );
    }

    #[test]
    fn rotate_skips_small_files() {
        let dir = std::env::temp_dir().join("lmforge_rotate_small");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("engine-x.stderr.log");
        fs::write(&p, b"tiny").unwrap();
        rotate_if_oversize(&p, 1, 3);
        assert!(p.exists(), "small file must not be rotated");
        assert!(!with_suffix(&p, 1).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_renames_oversize_and_caps_keep() {
        let dir = std::env::temp_dir().join("lmforge_rotate_big");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("engine-y.stderr.log");
        // 2 MB of zeros — comfortably above the 1 MB threshold we'll set
        fs::write(&p, vec![0u8; 2 * 1024 * 1024]).unwrap();

        // First rotation: current → .1
        rotate_if_oversize(&p, 1, 2);
        assert!(!p.exists(), "current must be renamed away");
        assert!(with_suffix(&p, 1).exists());

        // Write again, rotate again → .1 becomes .2
        fs::write(&p, vec![0u8; 2 * 1024 * 1024]).unwrap();
        rotate_if_oversize(&p, 1, 2);
        assert!(with_suffix(&p, 1).exists());
        assert!(with_suffix(&p, 2).exists());

        // Third rotation must drop .3 (keep=2)
        fs::write(&p, vec![0u8; 2 * 1024 * 1024]).unwrap();
        rotate_if_oversize(&p, 1, 2);
        assert!(!with_suffix(&p, 3).exists(), "keep=2 must drop the oldest");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_engine_log_creates_dir_and_opens_file() {
        let dir = std::env::temp_dir().join("lmforge_prepare_log");
        let _ = fs::remove_dir_all(&dir);
        let f = prepare_engine_log(&dir, "qwen3:8b:4bit", "stdout");
        assert!(f.is_ok());
        assert!(dir.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    // ── Stderr tail propagation (Phase 2.3) ───────────────────────────────────

    #[test]
    fn tail_lines_returns_last_n() {
        let body = "a\nb\nc\nd\ne";
        assert_eq!(tail_lines(body, 3, 1024), "c\nd\ne");
        assert_eq!(tail_lines(body, 100, 1024), "a\nb\nc\nd\ne");
    }

    #[test]
    fn tail_lines_zero_limits_return_empty() {
        assert_eq!(tail_lines("a\nb", 0, 1024), "");
        assert_eq!(tail_lines("a\nb", 5, 0), "");
    }

    #[test]
    fn tail_lines_drops_oldest_to_fit_max_bytes() {
        // Each line is 9 bytes + newline; max_bytes=20 fits ~2 lines.
        let body = "111111111\n222222222\n333333333\n444444444";
        let out = tail_lines(body, 10, 20);
        assert_eq!(out, "333333333\n444444444");
    }

    #[test]
    fn read_stderr_tail_returns_none_when_missing() {
        let dir = std::env::temp_dir().join("lmforge_stderr_tail_missing");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(read_stderr_tail(&dir, "no-such-model").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_stderr_tail_returns_none_when_empty() {
        let dir = std::env::temp_dir().join("lmforge_stderr_tail_empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(engine_log_path(&dir, "x", "stderr"), "").unwrap();
        assert!(read_stderr_tail(&dir, "x").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_stderr_tail_picks_last_lines() {
        let dir = std::env::temp_dir().join("lmforge_stderr_tail_lastn");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let body = "INFO loading model\nERROR cuda kernel sm_120 missing\nFATAL aborting";
        fs::write(engine_log_path(&dir, "qwen3:8b:4bit", "stderr"), body).unwrap();
        let tail = read_stderr_tail_with_limits(&dir, "qwen3:8b:4bit", 2, 1024).unwrap();
        assert_eq!(tail, "ERROR cuda kernel sm_120 missing\nFATAL aborting");
        let _ = fs::remove_dir_all(&dir);
    }
}
