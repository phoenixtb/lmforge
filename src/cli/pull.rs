use anyhow::Result;

use crate::config::LmForgeConfig;
use crate::model::{downloader, index, resolver};

/// `lmforge pull <model>` — Download a model
pub async fn run(config: &LmForgeConfig, model_input: &str) -> Result<()> {
    let data_dir = config.data_dir();
    std::fs::create_dir_all(data_dir.join("models"))?;

    // Determine the engine format (default to mlx on this platform)
    let engine_format = detect_engine_format(&data_dir);

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

    if resolved.format.to_string() != engine_format {
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
        println!(
            "  Model '{}' already installed at {}",
            resolved.id, existing.path
        );
        println!(
            "  To re-download, remove it first: lmforge models remove {}",
            resolved.id
        );
        return Ok(());
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
    let caps = index::detect_capabilities(&model_dir, Some(&resolved.id), Some(&resolved.hf_repo));
    println!(
        "  Capabilities: chat={} embeddings={} reranking={} thinking={} dims={:?}",
        caps.chat, caps.embeddings, caps.reranking, caps.thinking, caps.embedding_dims
    );

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

/// Detect engine format from hardware profile
fn detect_engine_format(data_dir: &std::path::Path) -> String {
    let hw_path = data_dir.join("hardware.json");
    if let Ok(content) = std::fs::read_to_string(&hw_path) {
        if let Ok(profile) = serde_json::from_str::<serde_json::Value>(&content) {
            if profile["gpu_vendor"].as_str() == Some("apple") {
                return "mlx".to_string();
            }
        }
    }
    "gguf".to_string() // fallback
}
