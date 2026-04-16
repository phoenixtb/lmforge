use anyhow::{Context, Result};
use tracing::{debug, info};
use tokio::sync::mpsc;

#[derive(Clone, Debug, serde::Serialize)]
pub enum DownloadProgress {
    Started { repo: String, files: usize },
    FileProgress { file: String, downloaded: u64, total: u64 },
    FileCompleted { file: String },
    Completed { repo: String, total_bytes: u64 },
    Failed { error: String },
}

/// Download all files for a model from HuggingFace, emitting progress via channel
pub async fn download_model(
    hf_repo: &str,
    files: &[String],
    dest_dir: &std::path::Path,
    progress_tx: Option<mpsc::Sender<DownloadProgress>>,
) -> Result<u64> {
    std::fs::create_dir_all(dest_dir)?;

    // Read HuggingFace token from env — supports both common names
    let hf_token = std::env::var("HF_TOKEN")
        .or_else(|_| std::env::var("HUGGING_FACE_HUB_TOKEN"))
        .ok();

    if hf_token.is_some() {
        debug!("HF token found in environment — will authenticate download requests");
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800)) // 30 min max per file
        .build()?;

    let mut total_bytes: u64 = 0;

    if let Some(tx) = &progress_tx {
        let _ = tx.send(DownloadProgress::Started {
            repo: hf_repo.to_string(),
            files: files.len(),
        }).await;
    }

    // If it's a URL (not an HF repo), download directly
    if hf_repo.contains("://") {
        let filename = hf_repo.split('/').last().unwrap_or("model");
        let dest = dest_dir.join(filename);
        total_bytes += download_file(&client, hf_repo, filename, &dest, progress_tx.clone(), hf_token.as_deref()).await?;
        
        if let Some(tx) = &progress_tx {
            let _ = tx.send(DownloadProgress::Completed { repo: hf_repo.to_string(), total_bytes }).await;
        }
        return Ok(total_bytes);
    }

    // Download each file from HF sequentially
    for file in files {
        let url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            hf_repo, file
        );

        let dest = dest_dir.join(file);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Check if file already exists with correct size 
        // (Full logic for size checking removed for brevity, assuming exists means done for chunks)
        if dest.exists() && std::fs::metadata(&dest)?.len() > 0 {
            // Very naive check, assumes existing files are complete
            // In a real system, we'd check sha256 or sizes from HF
        }

        match download_file(&client, &url, file, &dest, progress_tx.clone(), hf_token.as_deref()).await {
            Ok(bytes) => {
                total_bytes += bytes;
                if let Some(tx) = &progress_tx {
                    let _ = tx.send(DownloadProgress::FileCompleted { file: file.clone() }).await;
                }
            }
            Err(e) => {
                if let Some(tx) = &progress_tx {
                    let _ = tx.send(DownloadProgress::Failed { error: e.to_string() }).await;
                }
                return Err(e).context(format!("Failed to download {}", file));
            }
        }
    }

    info!(
        repo = hf_repo,
        total_mb = total_bytes / (1024 * 1024),
        "Download complete"
    );

    if let Some(tx) = &progress_tx {
        let _ = tx.send(DownloadProgress::Completed { repo: hf_repo.to_string(), total_bytes }).await;
    }

    Ok(total_bytes)
}

/// Download a single file with progress bar and resume support
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    file_name: &str,
    dest: &std::path::Path,
    progress_tx: Option<mpsc::Sender<DownloadProgress>>,
    hf_token: Option<&str>,
) -> Result<u64> {
    use futures::StreamExt;

    let mut downloaded: u64 = 0;
    let mut request = client.get(url);

    // Inject HF token if available
    if let Some(token) = hf_token {
        request = request.bearer_auth(token);
    }

    if dest.exists() {
        let existing = std::fs::metadata(dest)?.len();
        if existing > 0 {
            debug!(url, existing_bytes = existing, "Resuming download");
            request = request.header("Range", format!("bytes={}-", existing));
            downloaded = existing;
        }
    }

    let resp = request.send().await.context("Failed to start download")?;

    if resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok(0);
    }

    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // HuggingFace returns 401 in two very different situations:
            // 1. The repo does not exist (body: "Invalid username or password.")
            // 2. The repo is gated and requires access approval (body: "Access to model ... is restricted")
            // Read the body to tell the user which one it is.
            let body = resp.text().await.unwrap_or_default();
            if body.contains("Invalid username") || body.contains("password") {
                anyhow::bail!(
                    "Model '{}' not found on HuggingFace. \
                     The repository does not exist or the model ID is misspelled.",
                    url.split("/resolve/").next().unwrap_or(url)
                        .trim_start_matches("https://huggingface.co/")
                );
            } else {
                // Gated model — needs HF token
                anyhow::bail!(
                    "Model '{}' is gated on HuggingFace and requires access approval. \
                     Visit https://huggingface.co/{} to request access, \
                     then set HF_TOKEN=your_hf_token in your environment.",
                    url.split("/resolve/").next().unwrap_or(url)
                        .trim_start_matches("https://huggingface.co/"),
                    url.split("/resolve/").next().unwrap_or(url)
                        .trim_start_matches("https://huggingface.co/")
                );
            }
        }
        anyhow::bail!("Download failed: HTTP {}", status);
    }

    let total_size = if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        resp.content_length().unwrap_or(0) + downloaded
    } else {
        resp.content_length().unwrap_or(0)
    };

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dest)?;

    let mut stream = resp.bytes_stream();
    let mut last_emit = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Error reading download stream")?;
        std::io::Write::write_all(&mut file, &chunk)?;
        downloaded += chunk.len() as u64;

        // Emit at most every 100ms
        if last_emit.elapsed().as_millis() > 100 {
            if let Some(tx) = &progress_tx {
                let _ = tx.send(DownloadProgress::FileProgress {
                    file: file_name.to_string(),
                    downloaded,
                    total: total_size
                }).await;
            }
            last_emit = std::time::Instant::now();
        }
    }

    Ok(downloaded)
}
