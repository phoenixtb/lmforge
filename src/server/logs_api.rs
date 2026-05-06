//! `GET /lf/logs/*` — log discovery, tail, and follow endpoints.
//!
//! Powers the dashboard's log viewer. Read-only; mutation paths (rotation,
//! pruning) live in `crate::logging::rotation` and `crate::cli::clean`.
//!
//! Path resolution is restricted to the daemon's `logs_dir` and reuses
//! `engine_log_path` so we accept the same sanitised filenames the
//! adapters write — no arbitrary path traversal possible from the API.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};

use crate::logging::rotation::engine_log_path;

use super::AppState;

/// Special component name for the main daemon log (`lmforge.log` and its
/// `.YYYY-MM-DD` rotated copies). Adapter logs use the model id.
const DAEMON_COMPONENT: &str = "daemon";

/// Hard cap on `lines` query parameter for `/tail`. Higher requests are
/// silently clamped — protects against accidental "give me 1M lines" calls
/// that would push the response above the body limit.
const MAX_TAIL_LINES: usize = 5000;

/// Hard cap on tail response body in bytes. If the requested line range
/// exceeds this, we truncate to the most recent lines that fit.
const MAX_TAIL_BYTES: usize = 2 * 1024 * 1024;

/// Default tail length when the client omits `lines`.
const DEFAULT_TAIL_LINES: usize = 200;

/// SSE follow poll interval. Matches the cadence of `tail -f`-style tools.
const FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Serialize)]
pub struct LogStream {
    pub stream: String,
    pub size_bytes: u64,
    pub mtime_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct LogComponent {
    /// Either `"daemon"` or a model id (the original, not sanitized).
    pub component: String,
    /// Sanitized form actually present on disk (path-safe). Equals
    /// `component` for the daemon stream.
    pub component_safe: String,
    pub streams: Vec<LogStream>,
}

#[derive(Debug, Serialize)]
pub struct LogIndex {
    pub components: Vec<LogComponent>,
}

/// `GET /lf/logs/list` — enumerate available log streams under `~/.lmforge/logs`.
pub async fn logs_list(State(state): State<AppState>) -> impl IntoResponse {
    let logs_dir = state.data_dir.join("logs");
    let index = match scan_logs(&logs_dir) {
        Ok(i) => i,
        Err(e) => return error_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    json_ok(&index)
}

#[derive(Debug, Deserialize)]
pub struct TailQuery {
    pub component: String,
    /// `stdout` | `stderr`. Daemon component accepts `main` and treats it
    /// as the live `lmforge.log`.
    pub stream: Option<String>,
    pub lines: Option<usize>,
}

/// `GET /lf/logs/tail` — return the last N lines of one log stream as plain text.
pub async fn logs_tail(
    State(state): State<AppState>,
    Query(q): Query<TailQuery>,
) -> impl IntoResponse {
    let logs_dir = state.data_dir.join("logs");
    let path = match resolve_log_path(&logs_dir, &q.component, q.stream.as_deref()) {
        Ok(p) => p,
        Err(msg) => return error_text(StatusCode::BAD_REQUEST, msg),
    };
    if !path.exists() {
        return error_text(
            StatusCode::NOT_FOUND,
            format!("log not found: {}", path.display()),
        );
    }

    let lines = q
        .lines
        .unwrap_or(DEFAULT_TAIL_LINES)
        .clamp(1, MAX_TAIL_LINES);

    match tail_lines_bounded(&path, lines, MAX_TAIL_BYTES).await {
        Ok(text) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from(text))
            .unwrap(),
        Err(e) => error_text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub component: String,
    pub stream: Option<String>,
}

/// `GET /lf/logs/stream` — SSE that pushes each appended line as it arrives.
/// Starts from end-of-file so clients only see new output (matches `tail -f`
/// without `-n`). Pair with `/tail` for a backfilled scroll-back buffer.
pub async fn logs_stream(
    State(state): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> impl IntoResponse {
    let logs_dir = state.data_dir.join("logs");
    let path = match resolve_log_path(&logs_dir, &q.component, q.stream.as_deref()) {
        Ok(p) => p,
        Err(msg) => return error_text(StatusCode::BAD_REQUEST, msg),
    };

    let stream = async_stream::stream! {
        // Start at end-of-file so clients receive only new lines. Re-open
        // each time the file is missing so a not-yet-created log doesn't
        // 404 the connection — just waits for the first write.
        let mut last_pos: u64 = 0;
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            last_pos = meta.len();
        }
        let mut current_inode = inode_of(&path).await;
        let mut leftover = String::new();

        loop {
            tokio::time::sleep(FOLLOW_POLL_INTERVAL).await;

            // Detect log rotation: if inode changed (or file vanished and a
            // new one took its place), reset to read from the start of the
            // new file.
            let now_inode = inode_of(&path).await;
            if now_inode != current_inode {
                current_inode = now_inode;
                last_pos = 0;
                leftover.clear();
            }

            let Ok(file) = tokio::fs::File::open(&path).await else {
                yield Ok::<_, std::convert::Infallible>(
                    axum::body::Bytes::from("event: ping\ndata: {}\n\n")
                );
                continue;
            };
            let len = match file.metadata().await {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if len < last_pos {
                last_pos = 0;
                leftover.clear();
            }
            if len == last_pos {
                yield Ok(axum::body::Bytes::from("event: ping\ndata: {}\n\n"));
                continue;
            }

            let mut reader = BufReader::new(file);
            if reader.seek(SeekFrom::Start(last_pos)).await.is_err() {
                continue;
            }

            let mut buf = String::new();
            let read: usize = reader.read_to_string(&mut buf).await.unwrap_or_default();
            if read == 0 {
                continue;
            }
            last_pos += read as u64;

            leftover.push_str(&buf);
            // Emit one SSE event per complete line; carry partial trailing
            // content forward so split writes don't spam half-lines.
            let mut tail = String::new();
            for line in leftover.split_inclusive('\n') {
                if line.ends_with('\n') {
                    let payload = serde_json::json!({ "line": line.trim_end_matches('\n') });
                    let frame = format!("data: {}\n\n", payload);
                    yield Ok(axum::body::Bytes::from(frame));
                } else {
                    tail.push_str(line);
                }
            }
            leftover = tail;
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap()
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn json_ok<T: Serialize>(value: &T) -> Response<Body> {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn error_json(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": msg }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn error_text(status: StatusCode, msg: impl Into<String>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(msg.into()))
        .unwrap()
}

/// Validate the requested stream name. Stops the API from being used as a
/// generic path filter (the input maps to a filename suffix).
fn normalise_stream(stream: Option<&str>) -> Result<&str, String> {
    match stream.unwrap_or("stderr") {
        s @ ("stdout" | "stderr" | "main") => Ok(s),
        bad => Err(format!(
            "invalid stream {bad:?}; expected stdout|stderr|main"
        )),
    }
}

/// Resolve `(component, stream)` → on-disk path under `logs_dir`. Daemon
/// uses `lmforge.log`; engines use the per-model file written by the
/// adapter.
fn resolve_log_path(
    logs_dir: &Path,
    component: &str,
    stream: Option<&str>,
) -> Result<PathBuf, String> {
    let stream = normalise_stream(stream)?;
    if component == DAEMON_COMPONENT {
        // Pick the most recent rotated file; tracing-appender uses
        // `lmforge.log.YYYY-MM-DD`. Fall back to plain `lmforge.log` if
        // no rotated copies exist yet.
        return Ok(latest_daemon_log(logs_dir));
    }
    Ok(engine_log_path(logs_dir, component, stream))
}

fn latest_daemon_log(logs_dir: &Path) -> PathBuf {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(read) = std::fs::read_dir(logs_dir) {
        for entry in read.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("lmforge.log") {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            match &best {
                Some((bt, _)) if *bt >= mtime => {}
                _ => best = Some((mtime, entry.path())),
            }
        }
    }
    best.map(|(_, p)| p)
        .unwrap_or_else(|| logs_dir.join("lmforge.log"))
}

async fn inode_of(path: &Path) -> Option<u64> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(meta.ino())
    }
    #[cfg(not(unix))]
    {
        // No inode concept; rely on size+mtime tuple as a weaker proxy.
        Some(
            meta.len()
                ^ meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
        )
    }
}

/// Read the last `n` lines (or up to `max_bytes`, whichever is tighter).
/// Implements a backwards block read so we don't load multi-GB log files
/// into memory.
async fn tail_lines_bounded(path: &Path, n: usize, max_bytes: usize) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len() as usize;
    let read_len = len.min(max_bytes);
    let start = (len - read_len) as u64;
    file.seek(SeekFrom::Start(start)).await?;
    let mut buf = vec![0u8; read_len];
    file.read_exact(&mut buf).await?;

    // Trim leading partial line if we cut into the middle of one.
    let text = if start == 0 {
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        let lossy = String::from_utf8_lossy(&buf);
        match lossy.find('\n') {
            Some(idx) => lossy[idx + 1..].to_string(),
            None => lossy.into_owned(),
        }
    };

    // Keep only the last `n` lines.
    let lines: Vec<&str> = text.lines().collect();
    let take = lines.len().min(n);
    let slice = &lines[lines.len() - take..];
    Ok(slice.join("\n"))
}

/// Walk `logs_dir` and group files by component (model id) and stream.
fn scan_logs(logs_dir: &Path) -> std::io::Result<LogIndex> {
    if !logs_dir.exists() {
        return Ok(LogIndex { components: vec![] });
    }
    let mut by_component: std::collections::BTreeMap<String, LogComponent> =
        std::collections::BTreeMap::new();

    for entry in std::fs::read_dir(logs_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy().to_string();
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Daemon log family: lmforge.log, lmforge.log.YYYY-MM-DD, lmforge.log.N
        if name.starts_with("lmforge.log") {
            let entry = by_component
                .entry(DAEMON_COMPONENT.to_string())
                .or_insert_with(|| LogComponent {
                    component: DAEMON_COMPONENT.to_string(),
                    component_safe: DAEMON_COMPONENT.to_string(),
                    streams: vec![],
                });
            entry.streams.push(LogStream {
                stream: name,
                size_bytes: meta.len(),
                mtime_secs,
            });
            continue;
        }

        // Engine log family: engine-<sanitized>.{stdout,stderr}.log[.N]
        if let Some(rest) = name.strip_prefix("engine-") {
            // Find the `.{stdout,stderr}.log` suffix
            let stream = if rest.contains(".stdout.log") {
                "stdout"
            } else if rest.contains(".stderr.log") {
                "stderr"
            } else {
                continue;
            };
            // Component is everything up to `.{stream}.log`
            let needle = format!(".{stream}.log");
            let Some(end) = rest.find(&needle) else {
                continue;
            };
            let safe = &rest[..end];
            let entry = by_component
                .entry(safe.to_string())
                .or_insert_with(|| LogComponent {
                    component: safe.to_string(),
                    component_safe: safe.to_string(),
                    streams: vec![],
                });
            entry.streams.push(LogStream {
                stream: stream.to_string(),
                size_bytes: meta.len(),
                mtime_secs,
            });
        }
    }

    let components: Vec<LogComponent> = by_component.into_values().collect();
    Ok(LogIndex { components })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lmforge_logs_api_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn normalise_stream_rejects_traversal_attempts() {
        assert!(normalise_stream(Some("../etc/passwd")).is_err());
        assert!(normalise_stream(Some("stdout")).is_ok());
        assert!(normalise_stream(Some("stderr")).is_ok());
        assert!(normalise_stream(Some("main")).is_ok());
    }

    #[test]
    fn resolve_log_path_uses_sanitised_engine_name() {
        let dir = unique_dir("resolve");
        let p = resolve_log_path(&dir, "qwen2.5-vl:3b:4bit", Some("stderr")).unwrap();
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("engine-qwen2.5-vl_3b_4bit.stderr.log")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_log_path_handles_daemon_with_no_files() {
        let dir = unique_dir("daemon_empty");
        let p = resolve_log_path(&dir, DAEMON_COMPONENT, Some("main")).unwrap();
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "lmforge.log");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_groups_engine_streams_by_component() {
        let dir = unique_dir("scan");
        fs::write(dir.join("engine-qwen3_8b_4bit.stdout.log"), b"a\n").unwrap();
        fs::write(dir.join("engine-qwen3_8b_4bit.stderr.log"), b"b\n").unwrap();
        fs::write(dir.join("lmforge.log"), b"daemon\n").unwrap();
        fs::write(dir.join("unrelated.txt"), b"ignore\n").unwrap();

        let idx = scan_logs(&dir).unwrap();
        assert_eq!(idx.components.len(), 2);
        let engine = idx
            .components
            .iter()
            .find(|c| c.component == "qwen3_8b_4bit")
            .expect("engine component");
        assert_eq!(engine.streams.len(), 2);
        let daemon = idx
            .components
            .iter()
            .find(|c| c.component == DAEMON_COMPONENT)
            .expect("daemon component");
        assert_eq!(daemon.streams.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tail_returns_last_n_lines() {
        let dir = unique_dir("tail");
        let path = dir.join("engine-x.stderr.log");
        let content: String = (1..=50).map(|i| format!("line{i}\n")).collect();
        fs::write(&path, content.as_bytes()).unwrap();

        let out = tail_lines_bounded(&path, 5, 1024 * 1024).await.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines.first().copied(), Some("line46"));
        assert_eq!(lines.last().copied(), Some("line50"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tail_clamps_to_max_bytes() {
        let dir = unique_dir("tail_clamp");
        let path = dir.join("engine-y.stderr.log");
        // 2 MB of "x\n" lines (each line = 2 bytes)
        let mut buf = String::with_capacity(2 * 1024 * 1024);
        for _ in 0..(1024 * 1024) {
            buf.push_str("x\n");
        }
        fs::write(&path, buf.as_bytes()).unwrap();

        // Ask for 1M lines, cap at 4 KB so the clamp must kick in.
        let out = tail_lines_bounded(&path, 1_000_000, 4096).await.unwrap();
        assert!(out.len() <= 4096);
        // Must be a clean cut: no leading partial line.
        assert!(!out.starts_with("xx"));
        let _ = fs::remove_dir_all(&dir);
    }
}
