use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

/// The bundled default MLX catalog — embedded at compile time.
/// This is the authoritative fallback used when no runtime catalog file exists.
/// This means `lmforge pull <shortcut>` works on a fresh install without running `lmforge init` first.
pub const BUNDLED_MLX: &str = include_str!("../../../data/catalogs/mlx.json");

/// The bundled default GGUF catalog — embedded at compile time.
pub const BUNDLED_GGUF: &str = include_str!("../../../data/catalogs/gguf.json");

// ── GGUF raw catalog types ────────────────────────────────────────────────────

/// A single GGUF catalog entry: repo + exact filename to download.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GgufEntry {
    pub repo: String,
    pub file: String,
}

/// Untagged serde type so we can handle both model entries (`{repo, file}`)
/// and comment entries (plain strings like `"--- Chat models ---"`).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum RawGgufValue {
    Entry(GgufEntry),
    #[allow(dead_code)]
    Comment(String),
}

// ── Public result type returned to the resolver ───────────────────────────────

/// The result of resolving a catalog shortcut.
#[derive(Debug, Clone)]
pub enum CatalogResult {
    /// MLX: download all files from this HuggingFace repo.
    AllFiles(String),
    /// GGUF: download exactly this one file from this HuggingFace repo.
    SingleFile(GgufEntry),
}

// ── Resolution API ────────────────────────────────────────────────────────────

/// Dynamically resolves a model target shortcut from the JSON catalogs,
/// according to the engine format.
///
/// Resolution order:
/// 1. Runtime file at `catalogs_dir/{format}.json` (allows user overrides)
/// 2. Bundled catalog embedded in the binary at compile time
pub async fn load_catalog_and_resolve(
    name: &str,
    format: &str,
    catalogs_dir: &Path,
) -> Option<CatalogResult> {
    let normalized = name.to_lowercase();
    let format_str = format.to_lowercase();

    // --- Step 1: Try the runtime file (user may have customised it) ---
    let json_path = catalogs_dir.join(format!("{}.json", format_str));
    if let Ok(content) = tokio::fs::read_to_string(&json_path).await {
        debug!(name = %normalized, format = %format_str, source = "file", "Resolving from runtime catalog");
        if let Some(result) = resolve_from_content(&content, &normalized, &format_str) {
            return Some(result);
        }
        // File exists but key not found — still try bundled below
        // in case the runtime file is an older version.
    } else {
        debug!(path = %json_path.display(), "Runtime catalog not found; using bundled catalog");
    }

    // --- Step 2: Bundled catalog embedded at compile time ---
    resolve_from_bundled(&normalized, &format_str)
}

/// Resolve a key from a JSON string for the given format.
fn resolve_from_content(content: &str, normalized: &str, format_str: &str) -> Option<CatalogResult> {
    match format_str {
        "mlx" => {
            let map: HashMap<String, String> = serde_json::from_str(content).ok()?;
            map.get(normalized).cloned().map(CatalogResult::AllFiles)
        }
        "gguf" => {
            let map: HashMap<String, RawGgufValue> = serde_json::from_str(content).ok()?;
            match map.get(normalized)? {
                RawGgufValue::Entry(e) => Some(CatalogResult::SingleFile(e.clone())),
                RawGgufValue::Comment(_) => None,
            }
        }
        _ => None,
    }
}

/// Resolve against the compile-time embedded catalog for the given format.
pub fn resolve_from_bundled(normalized: &str, format_str: &str) -> Option<CatalogResult> {
    let content = match format_str {
        "mlx"  => BUNDLED_MLX,
        "gguf" => BUNDLED_GGUF,
        _      => return None,
    };
    resolve_from_content(content, normalized, format_str)
}

// ── Shortcut listing (for suggestions + UI) ───────────────────────────────────

/// All available shortcuts across all bundled catalogs for a given format.
/// Used to generate helpful suggestions in error messages.
pub fn bundled_shortcuts(format_str: &str) -> Vec<String> {
    let content = match format_str.to_lowercase().as_str() {
        "mlx"  => BUNDLED_MLX,
        "gguf" => BUNDLED_GGUF,
        _      => return vec![],
    };

    let keys: Vec<String> = match format_str {
        "mlx" => {
            serde_json::from_str::<HashMap<String, String>>(content)
                .map(|m| m.into_keys().collect())
                .unwrap_or_default()
        }
        _ => {
            serde_json::from_str::<HashMap<String, RawGgufValue>>(content)
                .map(|m| m.into_keys().collect())
                .unwrap_or_default()
        }
    };

    let mut filtered: Vec<String> = keys
        .into_iter()
        .filter(|k| !k.starts_with('_'))
        .collect();
    filtered.sort();
    filtered
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
        match fmt {
            "mlx" => {
                let map: HashMap<String, String> = match serde_json::from_str(content) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                for (shortcut, hf_repo) in map {
                    if shortcut.starts_with('_') { continue; }
                    if !hf_repo.contains('/') { continue; }
                    let tags  = shortcut.split(':').map(str::to_string).collect();
                    let role  = infer_role(&shortcut, &hf_repo);
                    entries.push(CatalogEntry { shortcut, hf_repo, format: fmt.to_string(), tags, role });
                }
            }
            "gguf" => {
                let map: HashMap<String, RawGgufValue> = match serde_json::from_str(content) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                for (shortcut, raw) in map {
                    if shortcut.starts_with('_') { continue; }
                    let RawGgufValue::Entry(entry) = raw else { continue };
                    if !entry.repo.contains('/') { continue; }
                    let tags = shortcut.split(':').map(str::to_string).collect();
                    let role = infer_role(&shortcut, &entry.repo);
                    entries.push(CatalogEntry {
                        shortcut,
                        hf_repo: entry.repo,
                        format: fmt.to_string(),
                        tags,
                        role,
                    });
                }
            }
            _ => {}
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


#[cfg(test)]
mod tests {
    use super::*;

    // ── JSON validity ─────────────────────────────────────────────────────────

    #[test]
    fn test_bundled_gguf_catalog_is_valid_json() {
        let result: Result<HashMap<String, RawGgufValue>, _> = serde_json::from_str(BUNDLED_GGUF);
        assert!(result.is_ok(), "GGUF catalog is not valid JSON: {:?}", result.err());
    }

    #[test]
    fn test_bundled_mlx_catalog_is_valid_json() {
        let result: Result<HashMap<String, String>, _> = serde_json::from_str(BUNDLED_MLX);
        assert!(result.is_ok(), "MLX catalog is not valid JSON: {:?}", result.err());
    }

    #[test]
    fn test_gguf_all_entries_have_repo_and_file() {
        let map: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        for (key, val) in &map {
            if key.starts_with('_') { continue; }
            match val {
                RawGgufValue::Entry(e) => {
                    assert!(e.repo.contains('/'), "GGUF entry '{}' has invalid repo: '{}'", key, e.repo);
                    assert!(e.file.ends_with(".gguf"), "GGUF entry '{}' file must end in .gguf: '{}'", key, e.file);
                }
                RawGgufValue::Comment(_) => {
                    panic!("Non-comment key '{}' has a string value — must be {{repo, file}} object", key);
                }
            }
        }
    }

    // ── Three critical shortcuts must exist in both catalogs ──────────────────

    #[test]
    fn test_primary_models_exist_in_gguf() {
        let map: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        assert!(map.contains_key("qwen3.5:4b:4bit"),        "GGUF missing LLM_MODEL");
        assert!(map.contains_key("qwen3.5:2b:4bit"),        "GGUF missing LLM_FALLBACK_MODEL");
        assert!(map.contains_key("qwen3-embed:0.6b:8bit"), "GGUF missing primary embed (0.6B 8bit)");
    }

    #[test]
    fn test_primary_models_exist_in_mlx() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        assert!(map.contains_key("qwen3.5:4b:4bit"),       "MLX missing LLM_MODEL");
        assert!(map.contains_key("qwen3.5:2b:4bit"),       "MLX missing LLM_FALLBACK_MODEL");
        assert!(map.contains_key("qwen3-embed:0.6b:4bit"), "MLX missing LLM_EMBED_MODEL");
    }

    // ── Cross-catalog key consistency ─────────────────────────────────────────

    #[test]
    fn test_embed_keys_are_consistent_across_catalogs() {
        let gguf: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String>        = serde_json::from_str(BUNDLED_MLX).unwrap();
        // 4B and 8B embed models exist in both catalogs with matching :4bit keys
        for key in &["qwen3-embed:4b:4bit", "qwen3-embed:8b:4bit"] {
            assert!(gguf.contains_key(*key), "GGUF missing embed key: {}", key);
            assert!(mlx.contains_key(*key),  "MLX  missing embed key: {}", key);
        }
        // 0.6B embed: GGUF uses :8bit (Q8_0 is the small available quant)
        assert!(gguf.contains_key("qwen3-embed:0.6b:8bit"),
            "GGUF must have 0.6B :8bit key");
    }

    #[test]
    fn test_inference_keys_are_consistent_across_catalogs() {
        let gguf: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String>        = serde_json::from_str(BUNDLED_MLX).unwrap();
        // Keys that exist in BOTH catalogs without requiring an HF token
        for key in &["qwen3.5:2b:4bit", "qwen3.5:4b:4bit",
                     "qwen3-embed:4b:4bit", "qwen3-embed:8b:4bit"] {
            assert!(gguf.contains_key(*key), "GGUF missing key: {}", key);
            assert!(mlx.contains_key(*key),  "MLX  missing key: {}", key);
        }
        // These exist in MLX only (bartowski GGUF repos require HF token)
        for key in &["qwen3:1.7b:4bit", "qwen3:4b:4bit", "qwen3:8b:4bit",
                     "qwen3-coder:next:4bit", "qwen3-coder:next:8bit"] {
            assert!(mlx.contains_key(*key), "MLX missing key: {}", key);
            assert!(!gguf.contains_key(*key), "GGUF must NOT have HF-token-required key: {}", key);
        }
    }

    // ── Renamed keys must NOT appear in the live catalogs ─────────────────────

    #[test]
    fn test_no_legacy_q_suffix_in_gguf() {
        let map: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        for key in map.keys() {
            assert!(!key.ends_with(":q4"), "GGUF has legacy :q4 key: {}", key);
            assert!(!key.ends_with(":q8"), "GGUF has legacy :q8 key (should be :8bit): {}", key);
        }
    }

    #[test]
    fn test_no_legacy_q4_suffix_in_catalogs() {
        let mlx: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        for key in mlx.keys() {
            assert!(!key.ends_with(":q4"), "MLX catalog has legacy :q4 key: {}", key);
        }
    }

    #[test]
    fn test_no_qwencode3_keys_in_live_catalogs() {
        let gguf: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        let mlx:  HashMap<String, String>        = serde_json::from_str(BUNDLED_MLX).unwrap();
        for (fmt, keys) in &[
            ("gguf", gguf.keys().map(|s| s.as_str()).collect::<Vec<_>>()),
            ("mlx",  mlx.keys().map(|s| s.as_str()).collect::<Vec<_>>()),
        ] {
            for key in keys {
                assert!(!key.starts_with("qwencode3"),
                    "{} catalog still has legacy qwencode3 key: {}", fmt, key);
            }
        }
    }

    // ── Reranker catalog correctness ──────────────────────────────────────────

    #[test]
    fn test_gguf_has_reranker_models() {
        let map: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        assert!(map.contains_key("qwen3-reranker:0.6b:4bit"), "missing 0.6B reranker");
        assert!(map.contains_key("qwen3-reranker:4b:4bit"),   "missing 4B reranker");
    }

    #[test]
    fn test_mlx_has_jina_reranker_only() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        assert!(map.contains_key("jina-reranker-v2:multilingual"),
            "MLX catalog must contain Jina reranker (oMLX 0.3.6 JinaForRanking support)");
        assert!(!map.contains_key("qwen3-reranker:0.6b:4bit"),
            "Generative decoder rerankers must NOT be in MLX catalog (oMLX doesn't support them)");
    }

    // ── Small models for 4 GB VRAM / Q4_K_S tier ─────────────────────────────

    #[test]
    fn test_small_models_for_constrained_hardware() {
        // GGUF: only freely accessible models (no HF token required)
        // mradermacher Qwen3.5-2B is the smallest confirmed-200 GGUF chat model
        let gguf: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF).unwrap();
        assert!(gguf.contains_key("qwen3.5:2b:4bit"), "Missing smallest confirmed-free GGUF chat model");
        assert!(gguf.contains_key("qwen3-embed:0.6b:8bit"), "Missing smallest embed model");

        // MLX still has the full small-model lineup
        let mlx: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        assert!(mlx.contains_key("qwen3:1.7b:4bit"), "MLX missing 1.7B model");
        assert!(mlx.contains_key("gemma3:1b:4bit"),  "MLX missing 1B model");
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
        assert_eq!(infer_role("qwen3-embed:0.6b:8bit", "Qwen/Qwen3-Embedding-0.6B-GGUF"), "embed");
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
        assert_eq!(infer_role("qwen3.5:4b:4bit", "mradermacher/Qwen3.5-4B-GGUF"), "chat");
        assert_eq!(infer_role("llama3.1:8b:4bit", "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF"), "chat");
        assert_eq!(infer_role("gemma3:4b:4bit", "bartowski/gemma-3-4b-it-GGUF"), "chat");
    }

    #[test]
    fn test_infer_role_code() {
        assert_eq!(infer_role("qwen3-coder:next:4bit", "bartowski/Qwen3-Coder-Next-GGUF"), "code");
    }

    #[test]
    fn test_infer_role_vision_vl_embed() {
        assert_eq!(infer_role("qwen3-vl-embed:2b:4bit", "mlx-community/Qwen3-VL-Embedding-2B-4bit"), "vision");
    }

    // ── resolve_from_bundled ──────────────────────────────────────────────────

    #[test]
    fn test_resolve_from_bundled_gguf_finds_primary_models() {
        let r = resolve_from_bundled("qwen3.5:4b:4bit", "gguf").unwrap();
        let CatalogResult::SingleFile(e) = r else { panic!("expected SingleFile") };
        assert_eq!(e.repo, "mradermacher/Qwen3.5-4B-GGUF");
        assert_eq!(e.file, "Qwen3.5-4B.Q4_K_S.gguf");

        let r2 = resolve_from_bundled("qwen3-embed:0.6b:8bit", "gguf").unwrap();
        let CatalogResult::SingleFile(e2) = r2 else { panic!("expected SingleFile") };
        assert_eq!(e2.repo, "Qwen/Qwen3-Embedding-0.6B-GGUF");
        assert_eq!(e2.file, "Qwen3-Embedding-0.6B-Q8_0.gguf");
    }

    #[test]
    fn test_resolve_from_bundled_mlx_finds_primary_models() {
        let r = resolve_from_bundled("qwen3.5:4b:4bit", "mlx").unwrap();
        let CatalogResult::AllFiles(repo) = r else { panic!("expected AllFiles") };
        assert_eq!(repo, "mlx-community/Qwen3.5-4B-4bit");

        let r2 = resolve_from_bundled("jina-reranker-v2:multilingual", "mlx").unwrap();
        let CatalogResult::AllFiles(repo2) = r2 else { panic!("expected AllFiles") };
        assert_eq!(repo2, "jinaai/jina-reranker-v2-base-multilingual");
    }

    #[test]
    fn test_resolve_from_bundled_missing_key_returns_none() {
        assert!(resolve_from_bundled("qwen3-embed:0.6b:q8", "gguf").is_none(),
            "old :q8 key must not resolve (renamed to :8bit)");
        assert!(resolve_from_bundled("qwencode3:4bit", "gguf").is_none());
        assert!(resolve_from_bundled("nonexistent:model", "gguf").is_none());
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

        assert_eq!(find("qwen3-embed:0.6b:8bit").role, "embed");
        assert_eq!(find("qwen3-reranker:0.6b:4bit").role, "rerank");
        assert_eq!(find("jina-reranker-v2:multilingual:f16").role, "rerank");
        assert_eq!(find("qwen3.5:4b:4bit").role, "chat");
        // qwen3-coder removed (bartowski repos require HF token)
    }

    #[test]
    fn test_list_for_ui_is_sorted() {
        let entries = list_for_ui("gguf");
        let shortcuts: Vec<&str> = entries.iter().map(|e| e.shortcut.as_str()).collect();
        let mut sorted = shortcuts.clone();
        sorted.sort();
        assert_eq!(shortcuts, sorted, "list_for_ui must return entries sorted by shortcut");
    }

    // ── Network integration tests (ignored by default) ────────────────────────
    //
    // Run with:
    //   cargo test -p lmforge -- --ignored --nocapture
    //
    // These tests hit the live HuggingFace CDN WITHOUT an HF token and verify
    // that every catalog entry is freely downloadable.
    //
    // Rules:
    //   HTTP 200 / 206 / 302       →  PASS  (file accessible without auth)
    //   HTTP 401 / 403             →  FAIL  (requires HF token — remove from catalog)
    //   HTTP 404                   →  FAIL  (file missing from repo)

    fn plain_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("lmforge-catalog-verifier/1.0")
            .build()
            .expect("failed to build reqwest client")
    }

    async fn check_url(client: &reqwest::Client, key: &str, url: &str, failures: &mut Vec<String>) {
        match client.head(url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                match status {
                    200 | 206 | 301 | 302 | 307 | 308 => {
                        println!("  ✓ [{status:3}]  {key}");
                    }
                    401 | 403 => {
                        let msg = format!("  ✗ [{status}]  {key}  →  requires HF token — remove from catalog\n         url: {url}");
                        eprintln!("{msg}");
                        failures.push(msg);
                    }
                    404 => {
                        let msg = format!("  ✗ [404]  {key}  →  file not found\n         url: {url}");
                        eprintln!("{msg}");
                        failures.push(msg);
                    }
                    other => {
                        println!("  ? [{other:3}]  {key}  (unexpected status)");
                    }
                }
            }
            Err(e) => {
                let msg = format!("  ✗ [ERR]  {key}  →  {e}");
                eprintln!("{msg}");
                failures.push(msg);
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires network; run with: cargo test -- --ignored --nocapture"]
    async fn verify_gguf_catalog_files_exist() {
        let map: HashMap<String, RawGgufValue> = serde_json::from_str(BUNDLED_GGUF)
            .expect("GGUF catalog must be valid JSON");
        let client = plain_client();
        let mut failures: Vec<String> = Vec::new();

        let mut entries: Vec<(&String, &GgufEntry)> = map.iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .filter_map(|(k, v)| { if let RawGgufValue::Entry(e) = v { Some((k, e)) } else { None } })
            .collect();
        entries.sort_by_key(|(k, _)| k.as_str());

        println!("\n=== GGUF catalog ({} entries) ===", entries.len());
        for (key, entry) in &entries {
            let url = format!("https://huggingface.co/{}/resolve/main/{}", entry.repo, entry.file);
            check_url(&client, key, &url, &mut failures).await;
        }
        println!("Checked {} GGUF entries.", entries.len());

        if !failures.is_empty() {
            panic!("\n\n{} GGUF entr{} require an HF token or are missing — remove from gguf.json:\n\n{}",
                failures.len(), if failures.len() == 1 { "y" } else { "ies" }, failures.join("\n"));
        }
    }

    #[tokio::test]
    #[ignore = "requires network; run with: cargo test -- --ignored --nocapture"]
    async fn verify_mlx_catalog_repos_accessible() {
        // For MLX repos we probe config.json — present in every safetensors MLX model.
        // A 401 means the repo requires an HF license agreement and should be removed.
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX)
            .expect("MLX catalog must be valid JSON");
        let client = plain_client();
        let mut failures: Vec<String> = Vec::new();

        let mut entries: Vec<(&String, &String)> = map.iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .collect();
        entries.sort_by_key(|(k, _)| k.as_str());

        println!("\n=== MLX catalog ({} entries) ===", entries.len());
        for (key, repo) in &entries {
            let url = format!("https://huggingface.co/{}/resolve/main/config.json", repo);
            check_url(&client, key, &url, &mut failures).await;
        }
        println!("Checked {} MLX entries.", entries.len());

        if !failures.is_empty() {
            panic!("\n\n{} MLX entr{} require an HF token or are missing — remove from mlx.json:\n\n{}",
                failures.len(), if failures.len() == 1 { "y" } else { "ies" }, failures.join("\n"));
        }
    }
}
