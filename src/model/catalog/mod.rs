use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// Dynamically resolves a model target shortcut from the JSON catalogs,
/// according to the engine format.
/// Supports checking structured keys like `family:size:quantization` (e.g., `qwen3.5:4b:6bit`).
pub async fn load_catalog_and_resolve(name: &str, format: &str, catalogs_dir: &Path) -> Option<String> {
    let normalized = name.to_lowercase();
    let format_str = format.to_lowercase();

    let json_path = catalogs_dir.join(format!("{}.json", format_str));

    // Support fallback reading via Tokio 
    let content = match tokio::fs::read_to_string(&json_path).await {
        Ok(c) => c,
        Err(e) => {
            debug!(error = %e, path = %json_path.display(), "Catalog file not found or unreadable, falling back to empty mapping");
            return fallback_curations(&normalized, &format_str);
        }
    };

    let map: HashMap<String, String> = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, path = %json_path.display(), "Catalog JSON parsing failed!");
            return fallback_curations(&normalized, &format_str);
        }
    };

    if let Some(repo) = map.get(&normalized) {
        return Some(repo.clone());
    }

    fallback_curations(&normalized, &format_str)
}

fn fallback_curations(normalized: &str, format_str: &str) -> Option<String> {
    // Only fall back if someone relies on deep defaults untouched by json
    match format_str {
        "gguf" => {
            match normalized {
                "qwen3-8b" => Some("bartowski/Qwen3-8B-GGUF".to_string()),
                "llama-3.1-8b" => Some("bartowski/Meta-Llama-3.1-8B-Instruct-GGUF".to_string()),
                _ => None,
            }
        }
        "safetensors" => {
            if normalized == "nomic-embed-text-v1" {
                Some("nomic-ai/nomic-embed-text-v1".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}
