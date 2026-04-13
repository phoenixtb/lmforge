use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;

/// `lmforge status` — Show engine and model status
pub async fn run(config: &LmForgeConfig) -> Result<()> {
    info!("Checking LMForge status...");

    let port = config.port;
    let url = format!("http://127.0.0.1:{}/lf/status", port);

    // Try to connect to running instance (short timeout — don't hang if not running)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        _ => {
            println!("LMForge is not running on port {}", port);
            println!("Start it with: lmforge start");
        }
    }

    Ok(())
}
