use anyhow::Result;
use tracing::info;

use crate::config::LmForgeConfig;
use crate::model::index::{
    CAPS_DETECTOR_VERSION, ModelEntry, ModelIndex, detect_capabilities, dir_size,
};

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
pub(crate) fn scan(
    data_dir: &std::path::Path,
    models_dir: &std::path::Path,
    prune: bool,
) -> Result<()> {
    let mut idx = ModelIndex::load(data_dir, models_dir).unwrap_or_default();

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
        let mut caps = detect_capabilities(&path, Some(&dir_name), hf_repo.as_deref());
        // `detect_capabilities` does not resolve MTP (probed separately at pull
        // time via gguf_inspect). Carry forward any previously-resolved value so
        // a rescan never silently drops speculative-decoding support.
        if caps.mtp.is_none() {
            caps.mtp = prior.and_then(|e| e.capabilities.mtp);
        }
        let format = prior
            .map(|e| e.format.clone())
            .unwrap_or_else(|| infer_format(&path));
        let engine = prior
            .map(|e| e.engine.clone())
            .unwrap_or_else(|| format.clone());
        let added_at = prior
            .map(|e| e.added_at.clone())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let id = prior
            .map(|e| e.id.clone())
            .unwrap_or_else(|| dir_name.clone());

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

    // A full rescan re-detected every model with the current detector, so the
    // persisted capabilities are now current.
    idx.caps_detector_version = CAPS_DETECTOR_VERSION;
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

/// Self-healing capability re-detection, run once on daemon start.
///
/// Capabilities are persisted in `models.json` and served verbatim, so a
/// detector fix in a new build does not reach models pulled by an older build.
/// When the index's `caps_detector_version` is behind the current
/// [`CAPS_DETECTOR_VERSION`], re-detect every model's capabilities **in place**
/// (weights untouched — this only rewrites the index) and stamp the version so
/// the work runs at most once per detector bump.
///
/// Unlike [`scan`], this never adds/prunes directories and never rewrites
/// `id`/`format`/`engine`/`hf_repo`/`added_at`/`size_bytes` — it only refreshes
/// `capabilities`, and preserves the externally-resolved `mtp` flag. Missing
/// directories are skipped (left as-is), not pruned.
///
/// Returns `Ok(true)` if a heal ran, `Ok(false)` if the index was already current
/// (or empty). Failures are non-fatal to startup — the caller logs and continues.
pub(crate) fn heal_capabilities_if_stale(
    data_dir: &std::path::Path,
    models_dir: &std::path::Path,
) -> Result<bool> {
    let mut idx = ModelIndex::load(data_dir, models_dir)?;
    if idx.caps_detector_version >= CAPS_DETECTOR_VERSION {
        return Ok(false);
    }
    let from = idx.caps_detector_version;

    if idx.list().is_empty() {
        // Nothing to re-detect; just stamp so we don't re-check every boot.
        idx.caps_detector_version = CAPS_DETECTOR_VERSION;
        idx.save(data_dir, models_dir)?;
        return Ok(false);
    }

    let mut updated = 0usize;
    for entry in idx.models.iter_mut() {
        let path = std::path::Path::new(&entry.path);
        if !path.is_dir() {
            // Weights missing (unmounted volume, mid-migration) — leave the
            // entry untouched rather than clobbering it with empty caps.
            continue;
        }
        // Primary hint is the catalog shortcut `id` (e.g. `phi4:4b:reasoning:4bit`)
        // — the same signal the pull path uses, and it carries markers like
        // `:reasoning` / `:thinking` that a bare directory name may drop.
        let mut caps = detect_capabilities(path, Some(&entry.id), entry.hf_repo.as_deref());
        // Preserve the separately-probed MTP flag (see `scan`).
        if caps.mtp.is_none() {
            caps.mtp = entry.capabilities.mtp;
        }
        if !capabilities_eq(&entry.capabilities, &caps) {
            updated += 1;
        }
        entry.capabilities = caps;
    }

    idx.caps_detector_version = CAPS_DETECTOR_VERSION;
    idx.save(data_dir, models_dir)?;
    info!(
        from,
        to = CAPS_DETECTOR_VERSION,
        updated,
        total = idx.list().len(),
        "Capability detector changed — re-detected model capabilities in place"
    );
    Ok(true)
}

/// Structural equality of two capability records, used only to count how many
/// entries a heal actually changed (for logging).
fn capabilities_eq(
    a: &crate::model::index::ModelCapabilities,
    b: &crate::model::index::ModelCapabilities,
) -> bool {
    a.chat == b.chat
        && a.embeddings == b.embeddings
        && a.reranking == b.reranking
        && a.thinking == b.thinking
        && a.native_reasoning == b.native_reasoning
        && a.vision == b.vision
        && a.mmproj_path == b.mmproj_path
        && a.embedding_dims == b.embedding_dims
        && a.pooling == b.pooling
        && a.mtp == b.mtp
        && a.stop_tokens == b.stop_tokens
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::index::ModelCapabilities;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lmforge_heal_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn stale_phi_entry(model_dir: &std::path::Path) -> ModelEntry {
        // Simulate caps written by a pre-fix build: reasoning model with the
        // thinking toggle wrongly absent, and an MTP flag that must survive.
        ModelEntry {
            id: "phi4:4b:reasoning:4bit".to_string(),
            path: model_dir.to_string_lossy().to_string(),
            format: "gguf".to_string(),
            engine: "llamacpp".to_string(),
            hf_repo: Some("unsloth/Phi-4-mini-reasoning-GGUF".to_string()),
            size_bytes: 4,
            capabilities: ModelCapabilities {
                chat: true,
                thinking: false,
                native_reasoning: false,
                mtp: Some(true),
                ..Default::default()
            },
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn heal_redetects_stale_reasoning_model_and_preserves_mtp() {
        let data_dir = tmp_dir("data");
        let models_dir = tmp_dir("models");
        let model_dir = models_dir.join("phi-4-mini-reasoning-gguf");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(
            model_dir.join("Phi-4-mini-reasoning-Q4_K_M.gguf"),
            b"fake-weights",
        )
        .unwrap();

        // Persist an index stamped with an old detector version (0).
        let mut idx = ModelIndex {
            models: vec![stale_phi_entry(&model_dir)],
            ..Default::default()
        };
        idx.caps_detector_version = 0;
        idx.save(&data_dir, &models_dir).unwrap();

        let healed = heal_capabilities_if_stale(&data_dir, &models_dir).unwrap();
        assert!(healed, "stale index must be healed");

        let reloaded = ModelIndex::load(&data_dir, &models_dir).unwrap();
        let e = reloaded.get("phi4:4b:reasoning:4bit").unwrap();
        assert!(e.capabilities.thinking, "reasoning toggle must be restored");
        assert!(
            e.capabilities.native_reasoning,
            "reasoning model must be locked (native)"
        );
        assert_eq!(
            e.capabilities.mtp,
            Some(true),
            "separately-probed MTP flag must be preserved across heal"
        );
        assert_eq!(
            reloaded.caps_detector_version, CAPS_DETECTOR_VERSION,
            "detector version must be stamped current"
        );

        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&models_dir);
    }

    #[test]
    fn heal_is_noop_when_already_current() {
        let data_dir = tmp_dir("data_cur");
        let models_dir = tmp_dir("models_cur");
        let model_dir = models_dir.join("phi-4-mini-reasoning-gguf");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("w.gguf"), b"fake-weights").unwrap();

        let mut idx = ModelIndex {
            models: vec![stale_phi_entry(&model_dir)],
            ..Default::default()
        };
        idx.caps_detector_version = CAPS_DETECTOR_VERSION; // already current
        idx.save(&data_dir, &models_dir).unwrap();

        let healed = heal_capabilities_if_stale(&data_dir, &models_dir).unwrap();
        assert!(!healed, "current index must not be re-detected");

        // Caps left untouched (still the stale values we wrote).
        let reloaded = ModelIndex::load(&data_dir, &models_dir).unwrap();
        let e = reloaded.get("phi4:4b:reasoning:4bit").unwrap();
        assert!(
            !e.capabilities.thinking,
            "no-op heal must not rewrite capabilities"
        );

        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&models_dir);
    }

    #[test]
    fn old_index_json_without_field_reads_as_version_zero() {
        let data_dir = tmp_dir("data_old");
        let models_dir = tmp_dir("models_old");
        // Index JSON as written before caps_detector_version existed.
        let json = r#"{"schema_version":2,"models":[]}"#;
        std::fs::write(data_dir.join("models.json"), json).unwrap();

        let idx = ModelIndex::load(&data_dir, &models_dir).unwrap();
        assert_eq!(
            idx.caps_detector_version, 0,
            "missing field must default to 0 (stale)"
        );
        assert!(idx.caps_detector_version < CAPS_DETECTOR_VERSION);

        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&models_dir);
    }
}
