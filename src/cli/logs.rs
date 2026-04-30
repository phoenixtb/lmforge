use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;

/// `lmforge logs` — View logs
pub async fn run(
    config: &LmForgeConfig,
    follow: bool,
    component: Option<String>,
    tail: usize,
    engine: bool,
    json: bool,
) -> Result<()> {
    let logs_dir = config.data_dir().join("logs");

    // The rolling appender creates date-suffixed files like lmforge.log.2026-03-28
    // Find the most recent log file(s) by listing the directory
    let log_prefix = if engine {
        "engine-stdout.log"
    } else {
        "lmforge.log"
    };

    let mut log_files: Vec<_> = std::fs::read_dir(&logs_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with(log_prefix))
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    log_files.sort(); // chronological order (date suffix sorts naturally)

    if log_files.is_empty() {
        println!("No log files found in {}", logs_dir.display());
        println!("Start LMForge first with: lmforge start");
        return Ok(());
    }

    // Read from the most recent log file
    let log_file = log_files.last().unwrap();

    info!(
        file = %log_file.display(),
        follow,
        tail,
        "Reading logs"
    );

    // Read last N lines
    let content = std::fs::read_to_string(log_file)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(tail);

    for line in &lines[start..] {
        // Apply component filter if set
        if let Some(ref comp) = component
            && !line.contains(comp)
        {
            continue;
        }

        if json {
            // Output raw JSON lines as-is
            println!("{}", line);
        } else {
            // Basic human-readable reformatting
            // Try to parse JSON and format nicely
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                // tracing-subscriber JSON format uses: timestamp, level, target, fields.message
                let ts = val["timestamp"].as_str().unwrap_or("");
                let level = val["level"].as_str().unwrap_or("");
                let target = val["target"].as_str().unwrap_or("");
                let msg = val["fields"]["message"].as_str().unwrap_or(line);
                println!("{} {} [{}] {}", ts, level, target, msg);
            } else {
                println!("{}", line);
            }
        }
    }

    if follow {
        // TODO(M8): Implement tail -f style following
        println!("-- follow mode not yet implemented --");
    }

    Ok(())
}
