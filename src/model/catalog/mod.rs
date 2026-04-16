use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// The bundled default MLX catalog — embedded at compile time.
/// This is the authoritative fallback used when no runtime catalog file exists.
/// This means `lmforge pull <shortcut>` works on a fresh install without running `lmforge init` first.
const BUNDLED_MLX: &str = include_str!("../../../data/catalogs/mlx.json");

/// The bundled default GGUF catalog — embedded at compile time.
const BUNDLED_GGUF: &str = include_str!("../../../data/catalogs/gguf.json");

/// Dynamically resolves a model target shortcut from the JSON catalogs,
/// according to the engine format.
/// Supports checking structured keys like `family:size:quantization` (e.g., `qwen3.5:4b:6bit`).
///
/// Resolution order:
/// 1. Runtime file at `catalogs_dir/{format}.json` (allows user overrides)
/// 2. Bundled catalog embedded in the binary at compile time (always fresh)
/// 3. Legacy hard-coded curations (minimal safety net for very old configs)
pub async fn load_catalog_and_resolve(name: &str, format: &str, catalogs_dir: &Path) -> Option<String> {
    let normalized = name.to_lowercase();
    let format_str = format.to_lowercase();

    // --- Step 1: Try the runtime file (user may have customised it) ---
    let json_path = catalogs_dir.join(format!("{}.json", format_str));
    if let Ok(content) = tokio::fs::read_to_string(&json_path).await {
        match serde_json::from_str::<HashMap<String, String>>(&content) {
            Ok(map) => {
                debug!(name = %normalized, format = %format_str, source = "file", "Resolving from runtime catalog");
                if let Some(repo) = map.get(&normalized) {
                    return Some(repo.clone());
                }
                // File exists but key not found — still try the bundled catalog below
                // in case the runtime file is an older version of the binary's catalog.
            }
            Err(e) => {
                warn!(error = %e, path = %json_path.display(), "Runtime catalog JSON parsing failed; falling back to bundled catalog");
            }
        }
    } else {
        debug!(path = %json_path.display(), "Runtime catalog not found; using bundled catalog");
    }

    // --- Step 2: Bundled catalog embedded at compile time ---
    if let Some(repo) = resolve_from_bundled(&normalized, &format_str) {
        return Some(repo);
    }

    // --- Step 3: Legacy safety net ---
    legacy_curations(&normalized, &format_str)
}

/// Resolve against the compile-time embedded catalog for the given format.
fn resolve_from_bundled(normalized: &str, format_str: &str) -> Option<String> {
    let content = match format_str {
        "mlx" => BUNDLED_MLX,
        "gguf" => BUNDLED_GGUF,
        _ => return None,
    };

    match serde_json::from_str::<HashMap<String, String>>(content) {
        Ok(map) => map.get(normalized).cloned(),
        Err(e) => {
            // This can only happen if the source catalog JSON is malformed — caught at compile time
            // via include_str! but not validated. Log the error and return None.
            warn!(error = %e, format = %format_str, "Bundled catalog JSON parsing failed (malformed source file)");
            None
        }
    }
}

/// All available shortcuts across all bundled catalogs for a given format.
/// Used to generate helpful suggestions in error messages.
pub fn bundled_shortcuts(format_str: &str) -> Vec<String> {
    let content = match format_str.to_lowercase().as_str() {
        "mlx" => BUNDLED_MLX,
        "gguf" => BUNDLED_GGUF,
        _ => return vec![],
    };

    serde_json::from_str::<HashMap<String, String>>(content)
        .map(|map| {
            let mut keys: Vec<String> = map
                .keys()
                .filter(|k| !k.starts_with('_')) // skip _comment_ keys
                .cloned()
                .collect();
            keys.sort();
            keys
        })
        .unwrap_or_default()
}

/// Minimal hard-coded fallback for edge cases where even the bundled catalog fails.
fn legacy_curations(normalized: &str, format_str: &str) -> Option<String> {
    match format_str {
        "gguf" => match normalized {
            "qwen3-8b" => Some("bartowski/Qwen3-8B-GGUF".to_string()),
            "llama-3.1-8b" => Some("bartowski/Meta-Llama-3.1-8B-Instruct-GGUF".to_string()),
            _ => None,
        },
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
