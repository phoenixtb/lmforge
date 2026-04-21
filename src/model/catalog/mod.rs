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
/// Also provides backward-compatibility aliases for renamed shortcuts.
fn legacy_curations(normalized: &str, format_str: &str) -> Option<String> {
    match format_str {
        "gguf" => match normalized {
            // Legacy shortcut aliases (kept for backward compatibility)
            "qwen3-8b"      => Some("bartowski/Qwen3-8B-GGUF".to_string()),
            "llama-3.1-8b"  => Some("bartowski/Meta-Llama-3.1-8B-Instruct-GGUF".to_string()),
            // qwencode3 → qwen3-coder rename (v0.1.0)
            "qwencode3:4bit" => Some("bartowski/Qwen3-Coder-Next-GGUF".to_string()),
            "qwencode3:8bit" => Some("bartowski/Qwen3-Coder-Next-GGUF".to_string()),
            // :q4 → catalog keys were renamed/replaced (v0.1.0+)
            "qwen3-embed:0.6b:q4"    => Some("Qwen/Qwen3-Embedding-0.6B-GGUF".to_string()),
            "qwen3-embed:0.6b:4bit"  => Some("Qwen/Qwen3-Embedding-0.6B-GGUF".to_string()), // no Q4 in repo; falls back to Q8_0
            "qwen3-embed:0.6b:8bit"  => Some("Qwen/Qwen3-Embedding-0.6B-GGUF".to_string()), // renamed :8bit → :q8
            "qwen3-embed:4b:q4"      => Some("Qwen/Qwen3-Embedding-4B-GGUF".to_string()),   // confirmed Q4_K_M
            "qwen3-embed:8b:q4"      => Some("Qwen/Qwen3-Embedding-8B-GGUF".to_string()),   // confirmed Q4_K_M
            "qwen3-reranker:0.6b:q4" => Some("mradermacher/Qwen3-Reranker-0.6B-GGUF".to_string()), // confirmed Q4_K_S
            "qwen3-reranker:4b:q4"   => Some("mradermacher/Qwen3-Reranker-4B-GGUF".to_string()),   // confirmed Q4_K_S
            // 1.7B reranker removed — no verified GGUF repo with Q4 found
            _ => None,
        },
        "mlx" => match normalized {
            // qwencode3 → qwen3-coder rename (v0.1.0)
            "qwencode3:4bit" => Some("mlx-community/Qwen3-Coder-Next-4bit".to_string()),
            "qwencode3:8bit" => Some("mlx-community/Qwen3-Coder-Next-8bit".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── JSON validity ─────────────────────────────────────────────────────────

    #[test]
    fn test_bundled_gguf_catalog_is_valid_json() {
        let result: Result<HashMap<String, String>, _> = serde_json::from_str(BUNDLED_GGUF);
        assert!(result.is_ok(), "GGUF catalog is not valid JSON: {:?}", result.err());
    }

    #[test]
    fn test_bundled_mlx_catalog_is_valid_json() {
        let result: Result<HashMap<String, String>, _> = serde_json::from_str(BUNDLED_MLX);
        assert!(result.is_ok(), "MLX catalog is not valid JSON: {:?}", result.err());
    }

    // ── Three critical shortcuts must exist in both catalogs ──────────────────

    #[test]
    fn test_primary_models_exist_in_gguf() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        assert!(map.contains_key("qwen3.5:4b:4bit"),      "GGUF missing LLM_MODEL");
        assert!(map.contains_key("qwen3.5:2b:4bit"),      "GGUF missing LLM_FALLBACK_MODEL");
        // LLM_EMBED_MODEL (qwen3-embed:0.6b:4bit) resolves via legacy alias; live catalog has :q8
        assert!(map.contains_key("qwen3-embed:0.6b:q8"),  "GGUF missing primary embed (0.6B Q8)");
        assert!(legacy_curations("qwen3-embed:0.6b:4bit", "gguf").is_some(),
            "Legacy alias for LLM_EMBED_MODEL must resolve");
    }

    #[test]
    fn test_primary_models_exist_in_mlx() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        assert!(map.contains_key("qwen3.5:4b:4bit"),      "MLX missing LLM_MODEL");
        assert!(map.contains_key("qwen3.5:2b:4bit"),      "MLX missing LLM_FALLBACK_MODEL");
        assert!(map.contains_key("qwen3-embed:0.6b:4bit"),"MLX missing LLM_EMBED_MODEL");
    }

    // ── Cross-catalog key consistency (same key works on all platforms) ────────

    #[test]
    fn test_embed_keys_are_consistent_across_catalogs() {
        let gguf: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        // 4B and 8B embed models exist in both catalogs with matching :4bit keys
        for key in &["qwen3-embed:4b:4bit", "qwen3-embed:8b:4bit"] {
            assert!(gguf.contains_key(*key), "GGUF missing embed key: {}", key);
            assert!(mlx.contains_key(*key),  "MLX  missing embed key: {}", key);
        }
        // 0.6B: GGUF has :q8/:f16 only (no Q4 in HF repo); MLX has :4bit and :8bit
        assert!(gguf.contains_key("qwen3-embed:0.6b:q8"),
            "GGUF must have 0.6B :q8 (the actual available quantization)");
    }

    #[test]
    fn test_inference_keys_are_consistent_across_catalogs() {
        let gguf: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        for key in &["qwen3:1.7b:4bit", "qwen3:4b:4bit", "qwen3:8b:4bit",
                     "qwen3-coder:next:4bit", "qwen3-coder:next:8bit"] {
            assert!(gguf.contains_key(*key), "GGUF missing key: {}", key);
            assert!(mlx.contains_key(*key),  "MLX  missing key: {}", key);
        }
    }

    // ── Renamed keys must NOT appear in the live catalogs ─────────────────────

    #[test]
    fn test_no_legacy_q4_suffix_in_catalogs() {
        let gguf: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        for (fmt, map) in &[("gguf", &gguf), ("mlx", &mlx)] {
            for key in map.keys() {
                assert!(!key.ends_with(":q4"), "{} catalog has legacy :q4 key: {}", fmt, key);
            }
        }
    }

    #[test]
    fn test_no_qwencode3_keys_in_live_catalogs() {
        let gguf: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        for (fmt, map) in &[("gguf", &gguf), ("mlx", &mlx)] {
            for key in map.keys() {
                assert!(!key.starts_with("qwencode3"),
                    "{} catalog still has legacy qwencode3 key: {}", fmt, key);
            }
        }
    }

    // ── Reranker catalog correctness ──────────────────────────────────────────

    #[test]
    fn test_gguf_has_reranker_models() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        assert!(map.contains_key("qwen3-reranker:0.6b:4bit"), "missing 0.6B reranker");
        // 1.7B removed — no verified GGUF repo with Q4 found
        assert!(map.contains_key("qwen3-reranker:4b:4bit"),   "missing 4B reranker");
    }

    #[test]
    fn test_mlx_has_jina_reranker_only() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        // Jina (encoder-based) — confirmed supported by oMLX 0.3.6
        assert!(map.contains_key("jina-reranker-v2:multilingual"),
            "MLX catalog must contain Jina reranker (oMLX 0.3.6 JinaForRanking support)");
        // Qwen3-Reranker (decoder-based) — NOT supported by oMLX; should be absent
        assert!(!map.contains_key("qwen3-reranker:0.6b:4bit"),
            "Generative decoder rerankers must NOT be in MLX catalog (oMLX doesn't support them)");
    }

    // ── Small models for 4 GB VRAM / Q4_K_S tier ─────────────────────────────

    #[test]
    fn test_small_models_for_constrained_hardware() {
        let gguf: HashMap<String, String> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        assert!(gguf.contains_key("qwen3:1.7b:4bit"), "Missing 1.7B model for 4GB VRAM tier");
        assert!(gguf.contains_key("qwen3:4b:4bit"),   "Missing 4B model for 4GB VRAM tier");
        assert!(gguf.contains_key("gemma3:1b:4bit"),  "Missing 1B model for minimal hardware");
    }

    // ── bundled_shortcuts filters comment keys ────────────────────────────────

    #[test]
    fn test_bundled_shortcuts_excludes_comment_keys() {
        for fmt in &["gguf", "mlx"] {
            let shortcuts = bundled_shortcuts(fmt);
            for key in &shortcuts {
                assert!(!key.starts_with('_'),
                    "{} shortcuts contain comment key: {}", fmt, key);
            }
            assert!(!shortcuts.is_empty(), "{} shortcuts should not be empty", fmt);
            assert!(shortcuts.contains(&"qwen3.5:4b:4bit".to_string()),
                "{} shortcuts missing qwen3.5:4b:4bit", fmt);
        }
    }

    // ── infer_role ────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_role_embed() {
        assert_eq!(infer_role("qwen3-embed:0.6b:4bit", "Qwen/Qwen3-Embedding-0.6B-GGUF"), "embed");
        assert_eq!(infer_role("nomic-embed-text:v1.5", "nomic-ai/nomic-embed-text-v1.5-GGUF"), "embed");
        assert_eq!(infer_role("snowflake-arctic-embed-l:v2:4bit", "mlx-community/snowflake-arctic-embed-l-v2.0-4bit"), "embed");
    }

    #[test]
    fn test_infer_role_rerank() {
        assert_eq!(infer_role("jina-reranker-v2:multilingual", "jinaai/jina-reranker-v2-base-multilingual"), "rerank");
        assert_eq!(infer_role("qwen3-reranker:0.6b:4bit", "Qwen/Qwen3-Reranker-0.6B-GGUF"), "rerank");
        assert_eq!(infer_role("bge-reranker-v2-m3:f16", "gpustack/bge-reranker-v2-m3-GGUF"), "rerank");
    }

    #[test]
    fn test_infer_role_chat() {
        assert_eq!(infer_role("qwen3.5:4b:4bit", "Qwen/Qwen3.5-4B-GGUF"), "chat");
        assert_eq!(infer_role("llama3.1:8b:4bit", "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF"), "chat");
        assert_eq!(infer_role("gemma3:4b:4bit", "bartowski/gemma-3-4b-it-GGUF"), "chat");
    }

    #[test]
    fn test_infer_role_code() {
        assert_eq!(infer_role("qwen3-coder:next:4bit", "bartowski/Qwen3-Coder-Next-GGUF"), "code");
    }

    #[test]
    fn test_infer_role_vision_vl_embed() {
        // vl-embed must be "vision", not "embed" (multimodal, different use case)
        assert_eq!(infer_role("qwen3-vl-embed:2b:4bit", "mlx-community/Qwen3-VL-Embedding-2B-4bit"), "vision");
    }

    // ── legacy_curations backward compatibility ───────────────────────────────

    #[test]
    fn test_legacy_q4_embed_keys_resolve() {
        assert_eq!(legacy_curations("qwen3-embed:0.6b:q4", "gguf"),
            Some("Qwen/Qwen3-Embedding-0.6B-GGUF".to_string()));
        // :4bit alias for 0.6B also exists (no Q4 in repo, falls back to Q8_0 at runtime)
        assert_eq!(legacy_curations("qwen3-embed:0.6b:4bit", "gguf"),
            Some("Qwen/Qwen3-Embedding-0.6B-GGUF".to_string()));
        // 4B/8B now point to official Qwen repos (confirmed Q4_K_M exists)
        assert_eq!(legacy_curations("qwen3-embed:4b:q4", "gguf"),
            Some("Qwen/Qwen3-Embedding-4B-GGUF".to_string()));
        assert_eq!(legacy_curations("qwen3-embed:8b:q4", "gguf"),
            Some("Qwen/Qwen3-Embedding-8B-GGUF".to_string()));
    }

    #[test]
    fn test_legacy_q4_reranker_keys_resolve() {
        // 0.6B and 4B now point to mradermacher (confirmed Q4_K_S exists)
        assert_eq!(legacy_curations("qwen3-reranker:0.6b:q4", "gguf"),
            Some("mradermacher/Qwen3-Reranker-0.6B-GGUF".to_string()));
        assert_eq!(legacy_curations("qwen3-reranker:4b:q4", "gguf"),
            Some("mradermacher/Qwen3-Reranker-4B-GGUF".to_string()));
        // 1.7B removed — no verified Q4 source found
    }

    #[test]
    fn test_legacy_qwencode3_resolves_to_qwen3_coder() {
        assert_eq!(legacy_curations("qwencode3:4bit", "gguf"),
            Some("bartowski/Qwen3-Coder-Next-GGUF".to_string()));
        assert_eq!(legacy_curations("qwencode3:8bit", "gguf"),
            Some("bartowski/Qwen3-Coder-Next-GGUF".to_string()));
        assert_eq!(legacy_curations("qwencode3:4bit", "mlx"),
            Some("mlx-community/Qwen3-Coder-Next-4bit".to_string()));
        assert_eq!(legacy_curations("qwencode3:8bit", "mlx"),
            Some("mlx-community/Qwen3-Coder-Next-8bit".to_string()));
    }

    #[test]
    fn test_legacy_unknown_keys_return_none() {
        // :q8 is now a live catalog key, not a legacy alias — must return None from legacy
        assert_eq!(legacy_curations("qwen3-embed:0.6b:q8", "gguf"), None);
        assert_eq!(legacy_curations("nonexistent:model", "gguf"), None);
    }

    // ── resolve_from_bundled ──────────────────────────────────────────────────

    #[test]
    fn test_resolve_from_bundled_gguf_finds_primary_models() {
        assert_eq!(resolve_from_bundled("qwen3.5:4b:4bit", "gguf"),
            Some("mradermacher/Qwen3.5-4B-GGUF".to_string()));
        assert_eq!(resolve_from_bundled("qwen3.5:2b:4bit", "gguf"),
            Some("mradermacher/Qwen3.5-2B-GGUF".to_string()));
        // 0.6B uses :q8 in the live catalog (no Q4 exists in repo)
        assert_eq!(resolve_from_bundled("qwen3-embed:0.6b:q8", "gguf"),
            Some("Qwen/Qwen3-Embedding-0.6B-GGUF".to_string()));
        // :4bit resolves via legacy_curations, not bundled catalog
        assert_eq!(resolve_from_bundled("qwen3-embed:0.6b:4bit", "gguf"), None);
    }

    #[test]
    fn test_resolve_from_bundled_mlx_finds_primary_models() {
        assert_eq!(resolve_from_bundled("qwen3.5:4b:4bit", "mlx"),
            Some("mlx-community/Qwen3.5-4B-4bit".to_string()));
        assert_eq!(resolve_from_bundled("qwen3-embed:0.6b:4bit", "mlx"),
            Some("mlx-community/Qwen3-Embedding-0.6B-4bit-DWQ".to_string()));
        assert_eq!(resolve_from_bundled("jina-reranker-v2:multilingual", "mlx"),
            Some("jinaai/jina-reranker-v2-base-multilingual".to_string()));
    }

    #[test]
    fn test_resolve_from_bundled_old_q4_key_returns_none() {
        // Old :q4 keys must NOT resolve from the live catalog (only from legacy_curations)
        assert_eq!(resolve_from_bundled("qwen3-embed:0.6b:q4", "gguf"), None);
        assert_eq!(resolve_from_bundled("qwencode3:4bit", "gguf"), None);
    }

    // ── list_for_ui ───────────────────────────────────────────────────────────

    #[test]
    fn test_list_for_ui_excludes_comment_keys() {
        let entries = list_for_ui("gguf");
        for e in &entries {
            assert!(!e.shortcut.starts_with('_'),
                "list_for_ui returned comment key: {}", e.shortcut);
        }
    }

    #[test]
    fn test_list_for_ui_assigns_correct_roles() {
        let entries = list_for_ui("gguf");
        let find = |key: &str| entries.iter().find(|e| e.shortcut == key)
            .unwrap_or_else(|| panic!("list_for_ui missing entry: {}", key));

        assert_eq!(find("qwen3-embed:0.6b:q8").role, "embed");
        assert_eq!(find("qwen3-reranker:0.6b:4bit").role, "rerank");
        assert_eq!(find("jina-reranker-v2:multilingual:f16").role, "rerank");
        assert_eq!(find("qwen3.5:4b:4bit").role, "chat");
        assert_eq!(find("qwen3-coder:next:4bit").role, "code");
    }

    #[test]
    fn test_list_for_ui_is_sorted() {
        let entries = list_for_ui("gguf");
        let shortcuts: Vec<&str> = entries.iter().map(|e| e.shortcut.as_str()).collect();
        let mut sorted = shortcuts.clone();
        sorted.sort();
        assert_eq!(shortcuts, sorted, "list_for_ui must return entries sorted by shortcut");
    }
}
