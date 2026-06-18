use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

#[derive(Clone, Debug, serde::Serialize)]
pub enum DownloadProgress {
    Started {
        repo: String,
        files: usize,
    },
    FileProgress {
        file: String,
        downloaded: u64,
        total: u64,
    },
    FileCompleted {
        file: String,
    },
    Completed {
        repo: String,
        total_bytes: u64,
    },
    Failed {
        error: String,
    },
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
        let _ = tx
            .send(DownloadProgress::Started {
                repo: hf_repo.to_string(),
                files: files.len(),
            })
            .await;
    }

    // If it's a URL (not an HF repo), download directly
    if hf_repo.contains("://") {
        let filename = hf_repo.split('/').next_back().unwrap_or("model");
        let dest = dest_dir.join(filename);
        total_bytes += download_file(
            &client,
            hf_repo,
            filename,
            &dest,
            progress_tx.clone(),
            hf_token.as_deref(),
        )
        .await?;

        if let Some(tx) = &progress_tx {
            let _ = tx
                .send(DownloadProgress::Completed {
                    repo: hf_repo.to_string(),
                    total_bytes,
                })
                .await;
        }
        return Ok(total_bytes);
    }

    // Download each file from HF sequentially
    // `hf_repo` may be `org/name` OR `org/name@revision`. The latter is
    // used by EXL3 repos (turboderp's convention puts each bits-per-weight
    // on its own git branch with `main` holding only README.md).
    let (repo_id, revision) = crate::model::resolver::split_revision(hf_repo);
    let resolve_ref = revision.unwrap_or("main");

    for file in files {
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            repo_id, resolve_ref, file
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

        match download_file(
            &client,
            &url,
            file,
            &dest,
            progress_tx.clone(),
            hf_token.as_deref(),
        )
        .await
        {
            Ok(bytes) => {
                total_bytes += bytes;
                if let Some(tx) = &progress_tx {
                    let _ = tx
                        .send(DownloadProgress::FileCompleted { file: file.clone() })
                        .await;
                }
            }
            Err(e) => {
                if let Some(tx) = &progress_tx {
                    let _ = tx
                        .send(DownloadProgress::Failed {
                            error: e.to_string(),
                        })
                        .await;
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
        let _ = tx
            .send(DownloadProgress::Completed {
                repo: hf_repo.to_string(),
                total_bytes,
            })
            .await;
    }

    Ok(total_bytes)
}

/// Extract the LFS sha256 hex digest that HuggingFace exposes via the
/// `X-Linked-Etag` header (for LFS-tracked files). The header value is a
/// 64-char hex sha256 wrapped in quotes, e.g. `"abc...def"`. Non-LFS files
/// (small `.json`, tokenizer text) lack this header and the function
/// returns None — verification is skipped for them.
fn parse_hf_lfs_sha256(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let raw = headers.get("X-Linked-Etag")?.to_str().ok()?;
    let trimmed = raw.trim_matches('"');
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(trimmed.to_lowercase())
    } else {
        None
    }
}

/// Compute sha256 of a file on disk in 1 MB chunks. Used to verify completed
/// downloads against the LFS sha256 advertised by HuggingFace. Streaming
/// avoids loading the whole file into memory — important for multi-GB
/// safetensors shards.
fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Verify a finished download is complete against the advertised size.
///
/// `total_size == 0` means the server sent no usable Content-Length (rare for
/// HF's `resolve` endpoint, but possible for chunked transfers) — we cannot
/// verify and must accept what we got. A non-zero mismatch is always a
/// truncated/corrupt download.
fn verify_complete(file_name: &str, downloaded: u64, total_size: u64) -> Result<()> {
    if total_size > 0 && downloaded != total_size {
        anyhow::bail!(
            "incomplete download for {file_name}: got {downloaded} of {total_size} bytes \
             ({:.1}%). Removed the partial file — re-run `lmforge pull` to retry.",
            (downloaded as f64 / total_size as f64) * 100.0
        );
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
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
                    url.split("/resolve/")
                        .next()
                        .unwrap_or(url)
                        .trim_start_matches("https://huggingface.co/")
                );
            } else {
                // Gated model — needs HF token
                anyhow::bail!(
                    "Model '{}' is gated on HuggingFace and requires access approval. \
                     Visit https://huggingface.co/{} to request access, \
                     then set HF_TOKEN=your_hf_token in your environment.",
                    url.split("/resolve/")
                        .next()
                        .unwrap_or(url)
                        .trim_start_matches("https://huggingface.co/"),
                    url.split("/resolve/")
                        .next()
                        .unwrap_or(url)
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

    // Capture LFS sha256 (if HuggingFace exposed one) BEFORE we consume the
    // response stream — once we move into bytes_stream() the headers are gone.
    let expected_sha256 = parse_hf_lfs_sha256(resp.headers());
    let is_partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;

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
                let _ = tx
                    .send(DownloadProgress::FileProgress {
                        file: file_name.to_string(),
                        downloaded,
                        total: total_size,
                    })
                    .await;
            }
            last_emit = std::time::Instant::now();
        }
    }

    // Drop the file handle before re-opening it for hashing so the OS flushes.
    drop(file);

    // Guard against truncated downloads. A dropped connection or an early
    // stream close (proxy, flaky network, interrupted run) can leave a short
    // file on disk that later fails to load with a confusing
    // "tensor data is not within the file bounds" error. When the server
    // advertised a Content-Length, require the on-disk byte count to match
    // exactly — this catches truncation even for non-LFS files that carry no
    // sha256 to verify against.
    if let Err(e) = verify_complete(file_name, downloaded, total_size) {
        let _ = std::fs::remove_file(dest);
        return Err(e);
    }

    // Verify sha256 when HF advertised one and the response was a complete
    // (non-resumed) download. For resumed downloads we'd need to re-hash from
    // the start, which we still do — see below — but we skip silently if the
    // resumed bytes were obtained from a different mirror with stale metadata.
    if let Some(expected) = expected_sha256 {
        match sha256_file(dest) {
            Ok(actual) if actual == expected => {
                debug!(
                    file = file_name,
                    "sha256 verified against HuggingFace LFS metadata"
                );
            }
            Ok(actual) => {
                let context = if is_partial {
                    "resumed download (mirror metadata may be stale)"
                } else {
                    "full download"
                };
                // Mismatch is critical: corrupt weights → silent inference garbage.
                // Delete the file so the next pull re-downloads cleanly, then bail.
                let _ = std::fs::remove_file(dest);
                anyhow::bail!(
                    "sha256 mismatch for {file_name} ({context}): expected {expected}, got {actual}. \
                     Removed corrupt file. Re-run `lmforge pull` to retry."
                );
            }
            Err(e) => {
                warn!(file = file_name, error = %e, "Failed to verify sha256 — keeping file but skipping integrity check");
            }
        }
    }

    Ok(downloaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn parse_hf_lfs_sha256_extracts_64_char_hex() {
        let mut h = HeaderMap::new();
        let s = "a".repeat(64);
        h.insert(
            "X-Linked-Etag",
            HeaderValue::from_str(&format!("\"{s}\"")).unwrap(),
        );
        assert_eq!(parse_hf_lfs_sha256(&h), Some(s));
    }

    #[test]
    fn parse_hf_lfs_sha256_rejects_non_hex() {
        let mut h = HeaderMap::new();
        h.insert(
            "X-Linked-Etag",
            HeaderValue::from_static(
                "\"not-a-real-sha-just-text-padding-to-the-right-length-12345678\"",
            ),
        );
        assert_eq!(parse_hf_lfs_sha256(&h), None);
    }

    #[test]
    fn parse_hf_lfs_sha256_returns_none_when_header_missing() {
        let h = HeaderMap::new();
        assert_eq!(parse_hf_lfs_sha256(&h), None);
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        let dir = std::env::temp_dir().join("lmforge_sha_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hello.bin");
        std::fs::write(&p, b"hello").unwrap();

        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(sha256_file(&p).unwrap(), expected);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn verify_complete_accepts_exact_match() {
        assert!(verify_complete("m.gguf", 2_500_000, 2_500_000).is_ok());
    }

    #[test]
    fn verify_complete_accepts_unknown_total() {
        // Server sent no Content-Length — cannot verify, must accept.
        assert!(verify_complete("m.gguf", 760_000_000, 0).is_ok());
    }

    #[test]
    fn verify_complete_rejects_truncated() {
        // The real-world case: 760 MB of an expected 2.5 GB file.
        let err = verify_complete("Qwen3.5-4B.gguf", 760_000_000, 2_500_000_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("incomplete download"), "got: {err}");
        assert!(err.contains("Qwen3.5-4B.gguf"), "got: {err}");
    }

    #[test]
    fn verify_complete_rejects_overrun() {
        // Defensive: more bytes than advertised is also a mismatch.
        assert!(verify_complete("m.gguf", 3_000, 2_000).is_err());
    }

    #[test]
    fn sha256_file_handles_empty_file() {
        let dir = std::env::temp_dir().join("lmforge_sha_empty");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty.bin");
        std::fs::write(&p, b"").unwrap();

        assert_eq!(
            sha256_file(&p).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
