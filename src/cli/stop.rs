use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;
use crate::engine;

/// `lmforge stop` — Stop a running LMForge instance
pub async fn run(config: &LmForgeConfig) -> Result<()> {
    info!("Stopping LMForge...");

    let data_dir = config.data_dir();
    let pid_path = engine::daemon::pid_file_path(&data_dir);

    // Try graceful shutdown via API first
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;

    let url = format!("http://127.0.0.1:{}/lf/shutdown", config.port);
    match client.post(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("✓ LMForge is shutting down gracefully.");
            engine::daemon::remove_pid_file(&data_dir);
            return Ok(());
        }
        _ => {}
    }

    // Fallback: kill by PID
    if let Some(pid) = engine::daemon::read_pid(&data_dir) {
        if engine::daemon::is_process_running(pid) {
            println!("Stopping LMForge (PID {})...", pid);

            #[cfg(unix)]
            {
                unsafe { libc::kill(pid as i32, libc::SIGTERM); }
            }

            // Wait a bit then check
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            if engine::daemon::is_process_running(pid) {
                println!("Force killing (PID {})...", pid);
                #[cfg(unix)]
                {
                    unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                }
            }

            engine::daemon::remove_pid_file(&data_dir);
            println!("✓ LMForge stopped.");
        } else {
            engine::daemon::remove_pid_file(&data_dir);
            println!("No running LMForge instance found (stale PID file removed).");
        }
    } else {
        println!("No running LMForge instance found (no PID file at {})", pid_path.display());
    }

    Ok(())
}
