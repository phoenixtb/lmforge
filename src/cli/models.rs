use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;
use crate::model::index::ModelIndex;

use super::ModelsAction;

/// `lmforge models list/remove/unload` — Manage installed models
pub async fn run(config: &LmForgeConfig, action: ModelsAction) -> Result<()> {
    let data_dir = config.data_dir();

    match action {
        ModelsAction::List => {
            let idx = ModelIndex::load(&data_dir)?;
            let models = idx.list();

            if models.is_empty() {
                println!("No models installed.");
                println!("Pull a model with: lmforge pull mlx-community/Qwen3.5-4B-OptiQ-4bit");
                return Ok(());
            }

            println!(
                "{:<42} {:<8} {:<8} {:<6} {:<6} {:<7} {:<6} {:<6} {:>9}",
                "MODEL", "FORMAT", "ENGINE", "CHAT", "EMBED", "RERANK", "THINK", "DIMS", "SIZE"
            );
            println!("{}", "─".repeat(110));

            let mut total_bytes: u64 = 0;
            for m in models {
                total_bytes += m.size_bytes;
                let dims = m
                    .capabilities
                    .embedding_dims
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "–".to_string());
                println!(
                    "{:<42} {:<8} {:<8} {:<6} {:<6} {:<7} {:<6} {:<6} {:>9}",
                    m.id,
                    m.format,
                    m.engine,
                    if m.capabilities.chat { "✓" } else { "–" },
                    if m.capabilities.embeddings {
                        "✓"
                    } else {
                        "–"
                    },
                    if m.capabilities.reranking {
                        "✓"
                    } else {
                        "–"
                    },
                    if m.capabilities.thinking {
                        "✓"
                    } else {
                        "–"
                    },
                    dims,
                    fmt_size(m.size_bytes),
                );
            }

            println!("{}", "─".repeat(110));
            println!(
                "{:<42} {:>9}  ({} model(s))",
                "Total",
                fmt_size(total_bytes),
                models.len()
            );
        }

        ModelsAction::Remove { name } => {
            let mut idx = ModelIndex::load(&data_dir)?;

            if let Some(entry) = idx.remove(&name) {
                let model_path = std::path::Path::new(&entry.path);
                if model_path.exists() {
                    std::fs::remove_dir_all(model_path)?;
                    info!(path = %entry.path, "Deleted model files");
                }
                idx.save(&data_dir)?;
                println!(
                    "✓ Model '{}' removed ({}).",
                    name,
                    fmt_size(entry.size_bytes)
                );
            } else {
                println!("Model '{}' not found.", name);
                println!("List installed models with: lmforge models list");
            }
        }

        ModelsAction::Unload => {
            let port = config.port;
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()?;
            let url = format!("http://127.0.0.1:{}/lf/model/unload", port);

            match client.post(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    println!("✓ Engine unloading from VRAM. Model files remain on disk.");
                    println!("  Reload with: lmforge models switch <model-name>");
                }
                Ok(resp) => {
                    let body = resp.text().await.unwrap_or_default();
                    anyhow::bail!("Daemon returned error: {}", body);
                }
                Err(e) => {
                    anyhow::bail!("Could not reach daemon (is it running?): {}", e);
                }
            }
        }
    }

    Ok(())
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes / 1024)
    }
}
