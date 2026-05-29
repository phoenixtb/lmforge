use anyhow::{Context, Result};

use crate::config::LmForgeConfig;
use crate::engine::registry::EngineRegistry;
use crate::model::{downloader, index, resolver};

/// `lmforge pull <model> [--engine <id>] [--refresh]` — Download a model.
///
/// When `engine_override` is `None`, the format is determined by the
/// hardware-aware auto-selector (same as `lmforge run` / `lmforge catalog`).
/// When set, the registry's `select_explicit` is used so the user can pull
/// safetensors for vLLM on a host where llamacpp is the default.
///
/// `refresh` re-evaluates capabilities for an already-present model without
/// re-downloading the weights. Migration story for users who pulled a model
/// before a capability detector landed (e.g. MTP detection in S-1).
pub async fn run(
    config: &LmForgeConfig,
    model_input: &str,
    engine_override: Option<&str>,
    refresh: bool,
) -> Result<()> {
    let data_dir = config.data_dir();
    std::fs::create_dir_all(data_dir.join("models"))?;

    // Determine the engine format. Shared with `run`, `catalog`, and the UI
    // so a fresh-pull / fresh-run / fresh-list never disagree — UNLESS the
    // user explicitly forces an engine, in which case the format follows
    // that engine's `model_format` field.
    let engine_format = match engine_override {
        Some(id) => resolve_format_for_engine(id, &data_dir)?,
        None => crate::model::catalog::detect_engine_format(&data_dir),
    };

    // Resolve model input
    println!("⚙ Resolving model: {}", model_input);
    let catalogs_dir = config.catalogs_dir();
    let resolved = resolver::resolve(model_input, &engine_format, &catalogs_dir).await?;
    println!("  ID:     {}", resolved.id);
    println!("  Dir:    {}", resolved.dir_name);
    println!("  Repo:   {}", resolved.hf_repo);
    println!("  Format: {}", resolved.format);
    println!("  Files:  {}", resolved.files.len());
    println!();

    if !formats_compatible(&resolved.format.to_string(), &engine_format) {
        anyhow::bail!(
            "❌ INCOMPATIBLE MODEL FORMAT: You are attempting to pull a '{}' model, \
             but the engine selected for your hardware requires '{}'.\n  \
             Please search HuggingFace for a version of this model in '{}' format.",
            resolved.format,
            engine_format,
            engine_format
        );
    }

    // Check if already downloaded
    let model_dir = data_dir.join("models").join(&resolved.dir_name);
    let mut idx = index::ModelIndex::load(&data_dir)?;

    if let Some(existing) = idx.get(&resolved.id) {
        if refresh {
            // Re-evaluate capabilities for an already-downloaded model.
            // Same detection pipeline as a fresh pull (incl. layered MTP),
            // but no network I/O — we only touch the on-disk weights and
            // the catalog metadata that came with `resolved`.
            println!(
                "  Model '{}' present at {} — refreshing capabilities (no re-download)",
                resolved.id, existing.path
            );

            let mut caps = index::detect_capabilities(
                &model_dir,
                Some(&resolved.id),
                Some(&resolved.hf_repo),
            );
            if matches!(resolved.format, resolver::ModelFormat::Gguf) {
                caps.mtp =
                    crate::model::gguf_inspect::resolve_mtp_for_model(&model_dir, resolved.mtp);
            }
            println!(
                "  Capabilities: chat={} embeddings={} reranking={} thinking={} vision={} dims={:?} mtp={:?}",
                caps.chat,
                caps.embeddings,
                caps.reranking,
                caps.thinking,
                caps.vision,
                caps.embedding_dims,
                caps.mtp,
            );
            if let Some(mmproj) = caps.mmproj_path.as_deref() {
                println!("  mmproj:       {}", mmproj);
            }

            let entry = index::ModelEntry {
                id: resolved.id.clone(),
                path: existing.path.clone(),
                format: resolved.format.to_string(),
                engine: engine_format.clone(),
                hf_repo: Some(resolved.hf_repo.clone()),
                size_bytes: index::dir_size(&model_dir),
                capabilities: caps,
                added_at: existing.added_at.clone(),
            };
            idx.add(entry);
            idx.save(&data_dir)?;

            println!("\n✓ Model '{}' capabilities refreshed.", resolved.id);
            return Ok(());
        }
        println!(
            "  Model '{}' already installed at {}",
            resolved.id, existing.path
        );
        println!(
            "  To re-download, remove it first: lmforge models remove {}",
            resolved.id
        );
        println!(
            "  To update capabilities (e.g. mtp) without re-downloading: lmforge pull {} --refresh",
            resolved.id
        );
        return Ok(());
    }

    if refresh {
        anyhow::bail!(
            "--refresh requires an already-installed model. \
             '{}' is not in the index — drop --refresh to download it.",
            resolved.id
        );
    }

    // Download
    println!("⚙ Downloading to: {}", model_dir.display());

    use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel(100);
    let hf_repo = resolved.hf_repo.clone();
    let files = resolved.files.clone();
    let model_dir_clone = model_dir.clone();

    let download_task = tokio::spawn(async move {
        downloader::download_model(&hf_repo, &files, &model_dir_clone, Some(tx)).await
    });

    let multi = MultiProgress::new();
    let style = ProgressStyle::default_bar()
        .template("    {prefix:>30} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("█▓░");

    let mut bars = HashMap::new();

    while let Some(msg) = rx.recv().await {
        use downloader::DownloadProgress::*;
        match msg {
            Started { repo, files } => {
                println!("  Downloading {} files from {}", files, repo);
            }
            FileProgress {
                file,
                downloaded,
                total,
            } => {
                let pb = bars.entry(file.clone()).or_insert_with(|| {
                    let pb = multi.add(ProgressBar::new(total));
                    pb.set_style(style.clone());
                    let mut display_name = file.clone();
                    if display_name.len() > 30 {
                        display_name = format!("...{}", &display_name[display_name.len() - 27..]);
                    }
                    pb.set_prefix(display_name);
                    pb
                });
                pb.set_length(total);
                pb.set_position(downloaded);
            }
            FileCompleted { file } => {
                if let Some(pb) = bars.get(&file) {
                    pb.finish_with_message("✓");
                }
            }
            Completed {
                repo: _,
                total_bytes: _,
            } => {}
            Failed { error } => {
                multi
                    .println(format!("✗ Download error: {}", error))
                    .unwrap();
            }
        }
    }

    let total_bytes = download_task.await??;

    let size_mb = total_bytes / (1024 * 1024);
    println!("\n  ✓ Downloaded {} MB", size_mb);

    // Detect capabilities
    let mut caps =
        index::detect_capabilities(&model_dir, Some(&resolved.id), Some(&resolved.hf_repo));
    // Layered MTP detection (S-1.7): catalog flag wins, GGUF probe fills
    // the blank, None when both are silent. Only meaningful for GGUF;
    // safetensors / MLX engines don't consume this signal today.
    if matches!(resolved.format, resolver::ModelFormat::Gguf) {
        caps.mtp = crate::model::gguf_inspect::resolve_mtp_for_model(&model_dir, resolved.mtp);
    }
    println!(
        "  Capabilities: chat={} embeddings={} reranking={} thinking={} vision={} dims={:?} mtp={:?}",
        caps.chat,
        caps.embeddings,
        caps.reranking,
        caps.thinking,
        caps.vision,
        caps.embedding_dims,
        caps.mtp,
    );
    if let Some(mmproj) = caps.mmproj_path.as_deref() {
        println!("  mmproj:       {}", mmproj);
    }

    // Add to index
    let entry = index::ModelEntry {
        id: resolved.id.clone(),
        path: model_dir.to_string_lossy().to_string(),
        format: resolved.format.to_string(),
        engine: engine_format.clone(),
        hf_repo: Some(resolved.hf_repo),
        size_bytes: index::dir_size(&model_dir),
        capabilities: caps,
        added_at: chrono::Utc::now().to_rfc3339(),
    };

    idx.add(entry);
    idx.save(&data_dir)?;

    println!("\n✓ Model '{}' is ready. Start with:", resolved.id);
    println!("  lmforge start --model {}", resolved.id);

    Ok(())
}

// `detect_engine_format` lives in `crate::model::catalog` so pull / run /
// catalog / init / UI all agree on which catalog the host should resolve
// against. Phase 2.1 (catalog priority flip).

/// When the user passes `--engine <id>` to `pull`, route the format through
/// that engine's `model_format` directly rather than the auto-selector.
///
/// Why a stand-alone helper rather than calling `select_explicit` and reading
/// `cfg.model_format`: hardware probing has its own error surface ("CUDA
/// driver missing"). At pull-time the user only cares about format. We honour
/// hardware gates implicitly because the user must `engine install <id>`
/// first — which DOES enforce the gates.
/// Whether a detected on-disk format is loadable by an engine that
/// advertises `engine_format`. Strict equality used to be the rule, but
/// EXL3 broke that: EXL3 model dirs are physically `safetensors` files
/// (`model.safetensors` + `config.json` + `quantization_config.json`),
/// the "exl3" label is a quantization-method tag, not a file-layout tag.
///
/// Keep this allowlist explicit so future format aliases (e.g. "gptq" or
/// "awq" living inside safetensors) need a conscious decision to add.
fn formats_compatible(detected: &str, engine: &str) -> bool {
    if detected == engine {
        return true;
    }
    // TabbyAPI / ExLlamaV3.
    if engine == "exl3" && detected == "safetensors" {
        return true;
    }
    false
}

fn resolve_format_for_engine(id: &str, data_dir: &std::path::Path) -> Result<String> {
    let user_engines = data_dir.join("engines.toml");
    let registry = EngineRegistry::load(if user_engines.exists() {
        Some(user_engines.as_path())
    } else {
        None
    })
    .context("Failed to load engine registry")?;
    let engine = registry
        .get(id)
        .with_context(|| format!("Unknown engine id: {}", id))?;
    Ok(engine.model_format.clone())
}

#[cfg(test)]
mod tests {
    use super::formats_compatible;

    #[test]
    fn formats_compatible_strict_equality() {
        assert!(formats_compatible("safetensors", "safetensors"));
        assert!(formats_compatible("gguf", "gguf"));
        assert!(formats_compatible("mlx", "mlx"));
    }

    #[test]
    fn formats_compatible_exl3_accepts_safetensors() {
        // EXL3 repos store weights as safetensors on disk.
        assert!(formats_compatible("safetensors", "exl3"));
    }

    #[test]
    fn formats_compatible_no_other_aliases() {
        // Don't silently accept other cross-format pulls.
        assert!(!formats_compatible("gguf", "safetensors"));
        assert!(!formats_compatible("safetensors", "gguf"));
        assert!(!formats_compatible("mlx", "safetensors"));
        assert!(!formats_compatible("safetensors", "mlx"));
        // Reverse direction is not allowed — safetensors engine must not
        // accept an exl3 label without explicit thought.
        assert!(!formats_compatible("exl3", "safetensors"));
    }
}
