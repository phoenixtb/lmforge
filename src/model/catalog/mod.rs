use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// The bundled default MLX catalog — embedded at compile time.
/// This is the authoritative fallback used when no runtime catalog file exists.
/// This means `lmforge pull <shortcut>` works on a fresh install without running `lmforge init` first.
pub const BUNDLED_MLX: &str = include_str!("../../../data/catalogs/mlx.json");

/// The bundled default GGUF catalog — embedded at compile time.
pub const BUNDLED_GGUF: &str = include_str!("../../../data/catalogs/gguf.json");

/// Dynamically resolves a model target shortcut from the JSON catalogs,
/// according to the engine format.
/// Supports checking structured keys like `family:size:quantization` (e.g., `qwen3.5:4b:6bit`).
///
/// Resolution order:
/// 1. Runtime file at `catalogs_dir/{format}.json` (allows user overrides)
/// 2. Bundled catalog embedded in the binary at compile time (always fresh)
/// 3. Legacy hard-coded curations (minimal safety net for very old configs)
pub async fn load_catalog_and_resolve(
    name: &str,
    format: &str,
    catalogs_dir: &Path,
) -> Option<String> {
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

// ── UI catalog listing ────────────────────────────────────────────────────────

/// A structured catalog entry returned by `GET /lf/catalog`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogEntry {
    /// The shortcut key, e.g. `"qwen3:8b:4bit"`.
    pub shortcut: String,
    /// The resolved HuggingFace repo, e.g. `"mlx-community/Qwen3-8B-4bit"`.
    pub hf_repo: String,
    /// Engine format: `"mlx"` or `"gguf"`.
    pub format: String,
    /// Tags derived by splitting the shortcut on `':'`, e.g. `["qwen3","8b","4bit"]`.
    pub tags: Vec<String>,
    /// Inferred capability role: `"chat"`, `"embed"`, `"rerank"`, `"vision"`, or `"code"`.
    pub role: String,
}

/// Return all catalog entries for the requested format(s).
/// Pass an empty string to get entries from both `mlx` and `gguf` catalogs.
/// `_comment_*` keys and entries without a '/' in the value are silently skipped.
/// Results are sorted by shortcut name.
pub fn list_for_ui(format: &str) -> Vec<CatalogEntry> {
    let formats: &[(&str, &str)] = match format.to_lowercase().as_str() {
        "mlx"  => &[("mlx",  BUNDLED_MLX)],
        "gguf" => &[("gguf", BUNDLED_GGUF)],
        _      => &[("mlx",  BUNDLED_MLX), ("gguf", BUNDLED_GGUF)],
    };

    let mut entries: Vec<CatalogEntry> = Vec::new();

    for &(fmt, content) in formats {
        let map: HashMap<String, String> = match serde_json::from_str(content) {
            Ok(m) => m,
            Err(_) => continue,
        };

        for (shortcut, hf_repo) in map {
            if shortcut.starts_with('_') { continue; } // skip _comment_ keys
            if !hf_repo.contains('/') { continue; }    // skip comment values

            let tags: Vec<String> = shortcut.split(':').map(str::to_string).collect();
            let role = infer_role(&shortcut, &hf_repo);

            entries.push(CatalogEntry {
                shortcut,
                hf_repo,
                format: fmt.to_string(),
                tags,
                role,
            });
        }
    }

    entries.sort_by(|a, b| a.shortcut.cmp(&b.shortcut));
    entries
}

/// Infer the primary capability role from keywords in the shortcut + repo name.
fn infer_role(shortcut: &str, repo: &str) -> String {
    let s = format!("{shortcut} {repo}").to_lowercase();
    if s.contains("rerank") || s.contains("reranker") {
        return "rerank".to_string();
    }
    // vl-embed must come before the generic embed check
    if s.contains("vl-embed") || s.contains("vl_embed") {
        return "vision".to_string();
    }
    if s.contains("embed") || s.contains("embedding") {
        return "embed".to_string();
    }
    if s.contains("vision") || s.contains("-vl") || s.contains("_vl") {
        return "vision".to_string();
    }
    if s.contains("code") || s.contains("coder") {
        return "code".to_string();
    }
    "chat".to_string()
}

// ── Legacy safety net ─────────────────────────────────────────────────────────

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
