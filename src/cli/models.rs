use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;
use crate::model::index::{ModelEntry, ModelIndex, detect_capabilities, dir_size};

use super::ModelsAction;

/// `lmforge models list/remove/unload` — Manage installed models
pub async fn run(config: &LmForgeConfig, action: ModelsAction) -> Result<()> {
    let data_dir = config.data_dir();
    let models_dir = config.models_dir();

    match action {
        ModelsAction::List => {
            let idx = ModelIndex::load(&data_dir, &models_dir)?;
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
            let mut idx = ModelIndex::load(&data_dir, &models_dir)?;

            if let Some(entry) = idx.remove(&name) {
                let model_path = std::path::Path::new(&entry.path);
                if model_path.exists() {
                    std::fs::remove_dir_all(model_path)?;
                    info!(path = %entry.path, "Deleted model files");
                }
                idx.save(&data_dir, &models_dir)?;
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

        ModelsAction::Scan { prune } => {
            scan(&data_dir, &models_dir, prune)?;
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

/// Rebuild the model index from the model directories on disk.
///
/// For each subdirectory under `models_dir` we (re-)detect capabilities and
/// recompute size. Existing entries keep their `hf_repo`, `format`/`engine`,
/// and `added_at`; brand-new directories get a best-effort format inferred from
/// their files. With `prune`, index entries whose directory is gone are dropped.
pub(crate) fn scan(data_dir: &std::path::Path, models_dir: &std::path::Path, prune: bool) -> Result<()> {
    let mut idx = ModelIndex::load(data_dir, models_dir).unwrap_or_else(|_| ModelIndex {
        schema_version: 2,
        models: vec![],
    });

    // Index existing entries by the on-disk directory name for quick lookup.
    let existing: std::collections::HashMap<String, ModelEntry> = idx
        .list()
        .iter()
        .filter_map(|e| {
            std::path::Path::new(&e.path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| (n.to_string(), e.clone()))
        })
        .collect();

    if !models_dir.exists() {
        println!("Models directory does not exist: {}", models_dir.display());
        return Ok(());
    }

    let mut added = 0usize;
    let mut refreshed = 0usize;
    let mut found_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

    for dir_entry in std::fs::read_dir(models_dir)?.flatten() {
        let path = dir_entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        found_dirs.insert(dir_name.clone());

        let prior = existing.get(&dir_name);
        let hf_repo = prior.and_then(|e| e.hf_repo.clone());
        let caps = detect_capabilities(&path, Some(&dir_name), hf_repo.as_deref());
        let format = prior
            .map(|e| e.format.clone())
            .unwrap_or_else(|| infer_format(&path));
        let engine = prior.map(|e| e.engine.clone()).unwrap_or_else(|| format.clone());
        let added_at = prior
            .map(|e| e.added_at.clone())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let id = prior.map(|e| e.id.clone()).unwrap_or_else(|| dir_name.clone());

        if prior.is_some() {
            refreshed += 1;
        } else {
            added += 1;
            println!("  + {} ({})", id, format);
        }

        idx.add(ModelEntry {
            id,
            path: path.to_string_lossy().to_string(),
            format,
            engine,
            hf_repo,
            size_bytes: dir_size(&path),
            capabilities: caps,
            added_at,
        });
    }

    let mut pruned = 0usize;
    if prune {
        let stale: Vec<String> = idx
            .list()
            .iter()
            .filter(|e| {
                std::path::Path::new(&e.path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| !found_dirs.contains(n))
                    .unwrap_or(true)
            })
            .map(|e| e.id.clone())
            .collect();
        for id in stale {
            idx.remove(&id);
            pruned += 1;
            println!("  - {} (directory missing)", id);
        }
    }

    idx.save(data_dir, models_dir)?;
    println!(
        "\n✓ Index scan complete: {} added, {} refreshed{}, {} total.",
        added,
        refreshed,
        if prune {
            format!(", {} pruned", pruned)
        } else {
            String::new()
        },
        idx.list().len()
    );
    Ok(())
}

/// Best-effort model format inference from the files in a directory. Used for
/// directories that aren't already in the index (e.g. a freshly mounted shared
/// volume). GGUF is unambiguous; everything else defaults to `safetensors`.
fn infer_format(dir: &std::path::Path) -> String {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("gguf") {
                let is_mmproj = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("mmproj-"))
                    .unwrap_or(false);
                if !is_mmproj {
                    return "gguf".to_string();
                }
            }
        }
    }
    "safetensors".to_string()
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
