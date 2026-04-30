pub mod rotation;

use anyhow::Result;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::cli::Cli;

/// Initialize the tracing/logging system with two layers:
/// 1. Human-readable output to stderr (for CLI)
/// 2. JSON structured output to a log file (~/.lmforge/logs/lmforge.log)
pub fn init(cli: &Cli) -> Result<()> {
    let log_level = cli.log_level.as_deref().unwrap_or("info");

    let env_filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    // Try to set up file logging if data dir exists
    let data_dir = dirs::home_dir().map(|h| h.join(".lmforge").join("logs"));

    if let Some(ref logs_dir) = data_dir
        && (logs_dir.exists() || std::fs::create_dir_all(logs_dir).is_ok())
    {
        let file_appender = rotation::create_appender(logs_dir);
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        // Leak the guard so it lives for the process lifetime.
        // This is intentional — the guard must not be dropped or logs are lost.
        std::mem::forget(_guard);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .with_target(false)
                    .with_ansi(true)
                    .with_writer(std::io::stderr),
            )
            .with(fmt::layer().json().with_writer(non_blocking))
            .init();

        return Ok(());
    }

    // Fallback: stderr only (no file logging)
    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .with_writer(std::io::stderr),
        )
        .init();

    Ok(())
}
