use anyhow::{Context, Result};
use tracing::{debug, info};

/// Resolved model target — what to download and where
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub id: String,
    pub dir_name: String,
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
pub async fn resolve(input: &str, engine_format: &str, catalogs_dir: &std::path::Path) -> Result<ResolvedModel> {
    // HuggingFace repo
    if input.contains('/') && !input.contains("://") {
        let mut rm = resolve_hf_repo(input, engine_format).await?;
        rm.id = input.to_string();
        return Ok(rm);
    }

    // URL
    if input.contains("://") {
        return Ok(ResolvedModel {
            id: input.to_string(),
            dir_name: input.split('/').last().unwrap_or("model").to_string(),
            hf_repo: input.to_string(),
            format: detect_format_from_engine(engine_format),
            files: vec![input.to_string()],
        });
    }

    // Local path
    if input.starts_with('/') || input.starts_with('~') {
        let path = shellexpand::tilde(input).to_string();
        let dir_name = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("local-model")
            .to_string();
        return Ok(ResolvedModel {
            id: input.to_string(),
            dir_name,
            hf_repo: path,
            format: detect_format_from_engine(engine_format),
            files: vec![],
        });
    }

    // Logical name — look up in curated catalog
    resolve_logical_name(input, engine_format, catalogs_dir).await
}

/// Resolve an HF repo — query the API for file listing
async fn resolve_hf_repo(repo: &str, engine_format: &str) -> Result<ResolvedModel> {
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
    let library = data["library_name"].as_str().unwrap_or("").to_lowercase();
    let repo_lower = repo.to_lowercase();
    
    let is_mlx = library == "mlx" 
        || library == "mlx-lm" 
        || repo_lower.contains("mlx")
        || data["tags"].as_array().map(|t| t.iter().any(|v| v.as_str().unwrap_or("").to_lowercase() == "mlx")).unwrap_or(false);

    let format = if is_mlx {
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

    // Derive a friendly dir name
    let dir_name = repo.split('/').last().unwrap_or(repo).to_lowercase();

    debug!(repo, ?format, file_count = files.len(), "Resolved HF repo");

    Ok(ResolvedModel {
        id: repo.to_string(),
        dir_name,
        hf_repo: repo.to_string(),
        format,
        files,
    })
}

async fn resolve_logical_name(name: &str, engine_format: &str, catalogs_dir: &std::path::Path) -> Result<ResolvedModel> {
    if let Some(repo) = crate::model::catalog::load_catalog_and_resolve(name, engine_format, catalogs_dir).await {
        let mut rm = resolve_hf_repo(&repo, engine_format).await?;
        rm.id = name.to_string();
        return Ok(rm);
    }

    // Build a helpful suggestion list from the bundled catalog so it's always accurate.
    let suggestions = crate::model::catalog::bundled_shortcuts(engine_format);
    let suggestion_str = if suggestions.is_empty() {
        "run 'lmforge models' to see available shortcuts".to_string()
    } else {
        suggestions.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
    };

    anyhow::bail!(
        "Unknown model '{}'. Try a HF repo like 'mlx-community/Qwen3.5-4B-OptiQ-4bit' \
         or use a shortcut like: {}",
        name,
        suggestion_str
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
