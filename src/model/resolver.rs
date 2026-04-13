use anyhow::{Context, Result};
use tracing::{debug, info};

/// Resolved model target — what to download and where
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub name: String,
    pub hf_repo: String,
    pub format: ModelFormat,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    Mlx,
    Gguf,
    Safetensors,
}

impl std::fmt::Display for ModelFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mlx => write!(f, "mlx"),
            Self::Gguf => write!(f, "gguf"),
            Self::Safetensors => write!(f, "safetensors"),
        }
    }
}

/// Resolve a model input string to a download target.
///
/// Supports:
/// 1. HF repo (contains `/`): e.g. `mlx-community/Qwen3.5-4B-OptiQ-4bit`
/// 2. Logical name (no `/`): look up in curated catalog
/// 3. URL (contains `://`): direct download
/// 4. Local path (starts with `/` or `~`): symlink into models dir
pub async fn resolve(input: &str, engine_format: &str) -> Result<ResolvedModel> {
    // HuggingFace repo
    if input.contains('/') && !input.contains("://") {
        return resolve_hf_repo(input).await;
    }

    // URL
    if input.contains("://") {
        return Ok(ResolvedModel {
            name: input.split('/').last().unwrap_or("model").to_string(),
            hf_repo: input.to_string(),
            format: detect_format_from_engine(engine_format),
            files: vec![input.to_string()],
        });
    }

    // Local path
    if input.starts_with('/') || input.starts_with('~') {
        let path = shellexpand::tilde(input).to_string();
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("local-model")
            .to_string();
        return Ok(ResolvedModel {
            name,
            hf_repo: path,
            format: detect_format_from_engine(engine_format),
            files: vec![],
        });
    }

    // Logical name — look up in curated catalog
    resolve_logical_name(input, engine_format).await
}

/// Resolve an HF repo — query the API for file listing
async fn resolve_hf_repo(repo: &str) -> Result<ResolvedModel> {
    let api_url = format!("https://huggingface.co/api/models/{}", repo);
    info!(repo, "Resolving HuggingFace repo");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client.get(&api_url).send().await
        .context("Failed to query HuggingFace API")?;

    if !resp.status().is_success() {
        anyhow::bail!("Model '{}' not found on HuggingFace (HTTP {})", repo, resp.status());
    }

    let data: serde_json::Value = resp.json().await?;

    // Detect format from library/tags
    let library = data["library_name"].as_str().unwrap_or("");
    let format = if library == "mlx" || library == "mlx-lm" {
        ModelFormat::Mlx
    } else if data["siblings"].as_array().map(|s| s.iter().any(|f|
        f["rfilename"].as_str().unwrap_or("").ends_with(".gguf")
    )).unwrap_or(false) {
        ModelFormat::Gguf
    } else {
        ModelFormat::Safetensors
    };

    // Get file listing
    let files: Vec<String> = data["siblings"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|f| f["rfilename"].as_str())
        .filter(|f| should_download_file(f, format))
        .map(|f| f.to_string())
        .collect();

    // Derive a friendly name
    let name = repo.split('/').last().unwrap_or(repo).to_lowercase();

    debug!(repo, ?format, file_count = files.len(), "Resolved HF repo");

    Ok(ResolvedModel {
        name,
        hf_repo: repo.to_string(),
        format,
        files,
    })
}

/// Resolve a logical name (e.g. "qwen3-8b") to an HF repo
async fn resolve_logical_name(name: &str, engine_format: &str) -> Result<ResolvedModel> {
    // Built-in mappings for common models
    let mappings = vec![
        ("qwen3-8b", "mlx", "mlx-community/Qwen3-8B-4bit"),
        ("qwen3-8b", "gguf", "bartowski/Qwen3-8B-GGUF"),
        ("qwen3.5-4b", "mlx", "mlx-community/Qwen3.5-4B-OptiQ-4bit"),
        ("qwen3.5-2b", "mlx", "mlx-community/Qwen3.5-2B-OptiQ-4bit"),
        ("qwen3.5-9b", "mlx", "mlx-community/Qwen3.5-9B-MLX-4bit"),
        ("qwen3.5-27b", "mlx", "mlx-community/Qwen3.5-27B-Claude-4.6-Opus-Distilled-MLX-4bit"),
        ("qwen2.5-coder-7b", "mlx", "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit"),
        ("nomic-embed-text", "mlx", "mlx-community/nomic-embed-text-v1.5-mlx"),
        ("nomic-modernbert", "mlx", "mlx-community/nomicai-modernbert-embed-base-4bit"),
        ("nomic-modernbert-6bit", "mlx", "mlx-community/nomicai-modernbert-embed-base-6bit"),
        ("nomic-embed-text-v1", "safetensors", "nomic-ai/nomic-embed-text-v1"),
        ("deepseek-r1-8b", "mlx", "mlx-community/DeepSeek-R1-Distill-Qwen-8B-4bit"),
        ("llama-3.1-8b", "mlx", "mlx-community/Meta-Llama-3.1-8B-Instruct-4bit"),
        ("llama-3.1-8b", "gguf", "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF"),
    ];

    let normalized = name.to_lowercase();
    let format_str = engine_format.to_lowercase();

    // Try exact format match first
    if let Some((_, _, repo)) = mappings.iter().find(|(n, f, _)| *n == normalized && *f == format_str) {
        return resolve_hf_repo(repo).await;
    }

    // Try any format match
    if let Some((_, _, repo)) = mappings.iter().find(|(n, _, _)| *n == normalized) {
        return resolve_hf_repo(repo).await;
    }

    anyhow::bail!(
        "Unknown model '{}'. Try a HF repo like 'mlx-community/Qwen3.5-4B-OptiQ-4bit' \
         or one of: qwen3-8b, qwen3.5-4b, llama-3.1-8b, nomic-embed-text",
        name
    );
}

/// Determine which files to download based on format
fn should_download_file(filename: &str, format: ModelFormat) -> bool {
    match format {
        ModelFormat::Mlx => {
            // MLX: download all model files
            filename.ends_with(".safetensors")
                || filename.ends_with(".json")
                || filename.ends_with(".jinja")
                || filename == "tokenizer.model"
        }
        ModelFormat::Gguf => {
            filename.ends_with(".gguf")
        }
        ModelFormat::Safetensors => {
            filename.ends_with(".safetensors")
                || filename.ends_with(".json")
                || filename == "tokenizer.model"
        }
    }
}

fn detect_format_from_engine(engine_format: &str) -> ModelFormat {
    match engine_format {
        "mlx" => ModelFormat::Mlx,
        "gguf" => ModelFormat::Gguf,
        _ => ModelFormat::Safetensors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_download_mlx() {
        assert!(should_download_file("model.safetensors", ModelFormat::Mlx));
        assert!(should_download_file("config.json", ModelFormat::Mlx));
        assert!(should_download_file("tokenizer.json", ModelFormat::Mlx));
        assert!(!should_download_file("README.md", ModelFormat::Mlx));
        assert!(!should_download_file(".gitattributes", ModelFormat::Mlx));
    }

    #[test]
    fn test_should_download_gguf() {
        assert!(should_download_file("model-Q4_K_M.gguf", ModelFormat::Gguf));
        assert!(!should_download_file("config.json", ModelFormat::Gguf));
    }

    #[test]
    fn test_detect_format() {
        assert_eq!(detect_format_from_engine("mlx"), ModelFormat::Mlx);
        assert_eq!(detect_format_from_engine("gguf"), ModelFormat::Gguf);
    }
}
