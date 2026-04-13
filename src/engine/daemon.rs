use anyhow::{Context, Result};
use tracing::{debug, info, warn};

/// PID file path for the daemon
pub fn pid_file_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("lmforge.pid")
}

/// Write PID file
pub fn write_pid_file(data_dir: &std::path::Path) -> Result<()> {
    let pid = std::process::id();
    let path = pid_file_path(data_dir);
    std::fs::write(&path, pid.to_string())
        .context("Failed to write PID file")?;
    info!(pid, path = %path.display(), "PID file written");
    Ok(())
}

/// Remove PID file
pub fn remove_pid_file(data_dir: &std::path::Path) {
    let path = pid_file_path(data_dir);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        debug!(path = %path.display(), "PID file removed");
    }
}

/// Read PID from file, returns None if file doesn't exist or is invalid
pub fn read_pid(data_dir: &std::path::Path) -> Option<u32> {
    let path = pid_file_path(data_dir);
    if !path.exists() {
        return None;
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Check if a process with the given PID is running
pub fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks if process exists without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, assume running if PID file exists
        true
    }
}

/// Check if the LMForge daemon is currently running
pub fn is_daemon_running(data_dir: &std::path::Path) -> bool {
    if let Some(pid) = read_pid(data_dir) {
        if is_process_running(pid) {
            return true;
        }
        // Stale PID file — clean it up
        warn!(pid, "Stale PID file found, removing");
        remove_pid_file(data_dir);
    }
    false
}

/// Check if the daemon is reachable via health endpoint
pub async fn is_daemon_healthy(port: u16) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://127.0.0.1:{}/health", port);
    match client.get(&url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Ensure the daemon is running. If not, start it.
///
/// `timeout_secs` — how long to wait for the engine to become healthy.
///   Use a large value for big models (e.g. 120s for 27B+).
///
/// Returns:
///   Ok(true)  — daemon was freshly started and is now healthy.
///   Ok(false) — daemon was already running and healthy.
///   Err(e)    — daemon failed to start or did not become healthy within timeout.
pub async fn ensure_daemon_running(
    data_dir: &std::path::Path,
    port: u16,
    model: Option<&str>,
    timeout_secs: u64,
) -> Result<bool> {
    // Check PID file first
    if is_daemon_running(data_dir) {
        if is_daemon_healthy(port).await {
            debug!("Daemon is running and healthy");
            return Ok(false);
        }
        warn!("Daemon PID exists but not healthy — will restart");
    }

    // Start the daemon
    info!("Auto-starting LMForge daemon...");
    println!("Starting LMForge daemon...");

    let lmforge_bin = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("lmforge"));

    let mut cmd = std::process::Command::new(&lmforge_bin);
    cmd.args(["start"]);
    if let Some(m) = model {
        cmd.args(["--model", m]);
    }

    let child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to start LMForge daemon")?;

    info!(pid = child.id(), "Daemon process spawned");

    // Poll health with a live counter so the user sees progress.
    // Poll every 500ms up to timeout_secs.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    let url = format!("http://127.0.0.1:{}/health", port);
    let max_attempts = timeout_secs * 2; // 500ms each

    for i in 0..max_attempts {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                // Clear the progress line
                print!("\r\x1b[2K");
                std::io::Write::flush(&mut std::io::stdout()).ok();
                info!(elapsed_secs = i / 2, "Daemon is healthy after auto-start");
                return Ok(true);
            }
        }

        // Overwrite the same line with elapsed seconds
        let elapsed = i / 2 + 1;
        print!("\r⚙ Loading model... {}s", elapsed);
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }

    // Timed out — print newline to leave terminal clean and return error
    println!();
    anyhow::bail!(
        "Engine did not become ready within {}s.\n\
         For large models (27B+) this can take 60–90s on first load.\n\
         Check logs: lmforge logs --engine",
        timeout_secs
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_file_path() {
        let path = pid_file_path(std::path::Path::new("/tmp/test"));
        assert_eq!(path, std::path::PathBuf::from("/tmp/test/lmforge.pid"));
    }

    #[test]
    fn test_is_process_running_self() {
        let pid = std::process::id();
        assert!(is_process_running(pid));
    }

    #[test]
    fn test_is_process_running_nonexistent() {
        // PID 99999 is very unlikely to exist
        assert!(!is_process_running(99999));
    }

    #[test]
    fn test_read_pid_nonexistent() {
        assert_eq!(read_pid(std::path::Path::new("/tmp/nonexistent-lmforge-test")), None);
    }
}
