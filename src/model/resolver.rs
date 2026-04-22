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
pub async fn resolve(
    input: &str,
    engine_format: &str,
    catalogs_dir: &std::path::Path,
) -> Result<ResolvedModel> {
    // HuggingFace repo
    if input.contains('/') && !input.contains("://") {
        // Direct HF repo — no quant hint available
        let mut rm = resolve_hf_repo(input, engine_format, None).await?;
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

/// Resolve an HF repo — query the API for file listing.
/// `quant_hint` comes from the catalog shortcut (e.g. "4bit", "f16") and
/// is used to select the right GGUF file(s) from multi-quant repos.
async fn resolve_hf_repo(repo: &str, _engine_format: &str, quant_hint: Option<&str>) -> Result<ResolvedModel> {
    let api_url = format!("https://huggingface.co/api/models/{}", repo);
    info!(repo, "Resolving HuggingFace repo");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client
        .get(&api_url)
        .send()
        .await
        .context("Failed to query HuggingFace API")?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "Model '{}' not found on HuggingFace (HTTP {})",
            repo,
            resp.status()
        );
    }

    let data: serde_json::Value = resp.json().await?;

    // Detect format from library/tags
    let library = data["library_name"].as_str().unwrap_or("").to_lowercase();
    let repo_lower = repo.to_lowercase();

    let is_mlx = library == "mlx"
        || library == "mlx-lm"
        || repo_lower.contains("mlx")
        || data["tags"]
            .as_array()
            .map(|t| {
                t.iter()
                    .any(|v| v.as_str().unwrap_or("").to_lowercase() == "mlx")
            })
            .unwrap_or(false);

    let format = if is_mlx {
        ModelFormat::Mlx
    } else if data["siblings"]
        .as_array()
        .map(|s| {
            s.iter()
                .any(|f| f["rfilename"].as_str().unwrap_or("").ends_with(".gguf"))
        })
        .unwrap_or(false)
    {
        ModelFormat::Gguf
    } else {
        ModelFormat::Safetensors
    };

    // Collect all files that match the format, then apply quant filtering for GGUF
    let all_format_files: Vec<String> = data["siblings"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|f| f["rfilename"].as_str())
        .filter(|f| should_download_file(f, format))
        .map(|f| f.to_string())
        .collect();

    let files = if format == ModelFormat::Gguf {
        // For GGUF repos, select only the files matching the requested quantization.
        // This prevents downloading f16 + Q8 + Q4 all at once.
        select_gguf_files(&all_format_files, quant_hint)
    } else {
        all_format_files
    };

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

async fn resolve_logical_name(
    name: &str,
    engine_format: &str,
    catalogs_dir: &std::path::Path,
) -> Result<ResolvedModel> {
    use crate::model::catalog::CatalogResult;

    if let Some(result) =
        crate::model::catalog::load_catalog_and_resolve(name, engine_format, catalogs_dir).await
    {
        match result {
            // GGUF explicit entry: exact file pinned in catalog — no HF API call needed.
            CatalogResult::SingleFile(entry) => {
                let dir_name = name.replace(':', "-");
                debug!(name, repo = %entry.repo, file = %entry.file, "Resolved GGUF explicit file from catalog");
                return Ok(ResolvedModel {
                    id:       name.to_string(),
                    dir_name,
                    hf_repo:  entry.repo,
                    format:   ModelFormat::Gguf,
                    files:    vec![entry.file],
                });
            }

            // MLX (or legacy string): query HF API for file listing as before.
            CatalogResult::AllFiles(repo) => {
                let quant_hint = extract_quant_hint(name);
                debug!(name, ?quant_hint, "Resolved repo from catalog, querying HF API");
                let mut rm = resolve_hf_repo(&repo, engine_format, quant_hint).await?;
                rm.id = name.to_string();
                return Ok(rm);
            }
        }
    }

    // Build a helpful suggestion list from the bundled catalog so it's always accurate.
    let suggestions = crate::model::catalog::bundled_shortcuts(engine_format);
    let suggestion_str = if suggestions.is_empty() {
        "run 'lmforge models' to see available shortcuts".to_string()
    } else {
        suggestions
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };

    anyhow::bail!(
        "Unknown model '{}'. Try a HF repo like 'mlx-community/Qwen3.5-4B-OptiQ-4bit' \
         or use a shortcut like: {}",
        name,
        suggestion_str
    );
}



fn detect_format_from_engine(engine_format: &str) -> ModelFormat {
    match engine_format {
        "mlx"  => ModelFormat::Mlx,
        "gguf" => ModelFormat::Gguf,
        _      => ModelFormat::Safetensors,
    }
}

/// Extract a quantization token from a catalog shortcut.
/// Recognises both the long form (4bit, 8bit) and short form (q4, q8, f16).
/// e.g. "qwen3-embed:0.6b:q8"  → Some("q8")   → Q8_0
///      "qwen3-embed:0.6b:4bit" → Some("4bit")  → Q4_K_S
///      "bge-m3:f16"             → Some("f16")   → F16
///      "nomic-embed-text:v1.5" → None
fn extract_quant_hint(shortcut: &str) -> Option<&str> {
    shortcut.split(':').find(|part| {
        matches!(
            *part,
            "4bit" | "5bit" | "6bit" | "8bit"
                | "q4"  | "q5"  | "q6"  | "q8"
                | "f16" | "bf16"
        )
    })
}

/// Return ordered GGUF filename substrings for a given quant tag.
/// Accepts both long form ("4bit", "8bit") and short form ("q4", "q8").
fn gguf_patterns_for_quant(quant: &str) -> &'static [&'static str] {
    match quant {
        "4bit" | "q4" => &["Q4_K_S", "Q4_K_M", "Q4_K"],
        "5bit" | "q5" => &["Q5_K_S", "Q5_K_M", "Q5_K"],
        "6bit" | "q6" => &["Q6_K"],
        "8bit" | "q8" => &["Q8_0"],
        "f16" | "bf16" => &["F16", "BF16", "f16", "bf16"],
        _ => &[],
    }
}

/// Select the right GGUF files given a quant hint.
///
/// - If a quant hint is present, tries each pattern in priority order and
///   returns the first set of files that match. Supports split shards.
/// - Falls back through Q4_K_S → Q4_K_M → Q8_0 → smallest file if the
///   exact quant is unavailable, printing a clear warning.
/// - If no hint (direct HF repo pull), defaults to Q4_K_S → Q4_K_M to
///   avoid downloading every quant in the repo.
fn select_gguf_files(all_gguf: &[String], quant_hint: Option<&str>) -> Vec<String> {
    // Helper: find all files containing a case-insensitive pattern
    let matches_pattern = |files: &[String], pat: &str| -> Vec<String> {
        let pat_up = pat.to_uppercase();
        files
            .iter()
            .filter(|f| f.to_uppercase().contains(&pat_up))
            .cloned()
            .collect()
    };

    // Determine which patterns to try first
    let requested_patterns: &[&str] = quant_hint
        .map(gguf_patterns_for_quant)
        .unwrap_or(&["Q4_K_S", "Q4_K_M"]); // default for bare HF repo pulls

    for &pat in requested_patterns {
        let found = matches_pattern(all_gguf, pat);
        if !found.is_empty() {
            return found;
        }
    }

    // Requested quant not found → warn and fall back
    if let Some(q) = quant_hint {
        eprintln!(
            "\n  ⚠  Quantization '{}' not available in this repo.",
            q
        );
        eprintln!("     Attempting fallback (Q4_K_S → Q4_K_M → Q8_0) …");
        for &pat in &["Q4_K_S", "Q4_K_M", "Q8_0"] {
            let found = matches_pattern(all_gguf, pat);
            if !found.is_empty() {
                eprintln!("     Using: {:?}", found);
                return found;
            }
        }
    }

    // Last resort: return everything (repo has unusual naming)
    eprintln!(
        "  ⚠  Could not identify a specific quantization file. Downloading all GGUF files."
    );
    all_gguf.to_vec()
}

/// Determine which files to download.
/// For GGUF, quant filtering is handled by select_gguf_files; this function
/// only decides format-level inclusion (MLX / safetensors).
fn should_download_file(filename: &str, format: ModelFormat) -> bool {
    match format {
        ModelFormat::Mlx => {
            filename.ends_with(".safetensors")
                || filename.ends_with(".json")
                || filename.ends_with(".jinja")
                || filename == "tokenizer.model"
        }
        // GGUF: selection is done by select_gguf_files, not here
        ModelFormat::Gguf => filename.ends_with(".gguf"),
        ModelFormat::Safetensors => {
            filename.ends_with(".safetensors")
                || filename.ends_with(".json")
                || filename == "tokenizer.model"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── should_download_file ──────────────────────────────────────────────────

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

    // ── extract_quant_hint ────────────────────────────────────────────────────

    #[test]
    fn test_extract_quant_hint_4bit() {
        assert_eq!(extract_quant_hint("qwen3-embed:0.6b:4bit"), Some("4bit"));
        assert_eq!(extract_quant_hint("qwen3.5:4b:4bit"), Some("4bit"));
    }

    #[test]
    fn test_extract_quant_hint_8bit() {
        assert_eq!(extract_quant_hint("qwen3-embed:0.6b:8bit"), Some("8bit"));
    }

    #[test]
    fn test_extract_quant_hint_q8_shortform() {
        // Catalog keys use "q8" short form — must be recognized
        assert_eq!(extract_quant_hint("qwen3-embed:0.6b:q8"), Some("q8"));
        assert_eq!(extract_quant_hint("qwen3-reranker:0.6b:q4"), Some("q4"));
    }

    #[test]
    fn test_extract_quant_hint_f16() {
        assert_eq!(extract_quant_hint("bge-m3:f16"), Some("f16"));
        assert_eq!(extract_quant_hint("nomic-modernbert-embed:f16"), Some("f16"));
    }

    #[test]
    fn test_extract_quant_hint_none() {
        // Version tag like v1.5 must not be confused with a quant
        assert_eq!(extract_quant_hint("nomic-embed-text:v1.5"), None);
        assert_eq!(extract_quant_hint("jina-reranker-v2:multilingual"), None);
        assert_eq!(extract_quant_hint("llama4:17b:4bit:scout"), Some("4bit"));
    }

    // ── gguf_patterns_for_quant ───────────────────────────────────────────────

    #[test]
    fn test_gguf_patterns_4bit() {
        let p = gguf_patterns_for_quant("4bit");
        assert!(p.contains(&"Q4_K_S"));
        assert!(p.contains(&"Q4_K_M"));
    }

    #[test]
    fn test_gguf_patterns_unknown_is_empty() {
        assert!(gguf_patterns_for_quant("3bit").is_empty());
        assert!(gguf_patterns_for_quant("").is_empty());
    }

    // ── select_gguf_files ─────────────────────────────────────────────────────

    fn gguf_repo_files() -> Vec<String> {
        vec![
            "Qwen3-Embedding-0.6B-Q4_K_S.gguf".to_string(),
            "Qwen3-Embedding-0.6B-Q8_0.gguf".to_string(),
            "Qwen3-Embedding-0.6B-F16.gguf".to_string(),
        ]
    }

    #[test]
    fn test_select_gguf_4bit_picks_q4_ks() {
        let files = select_gguf_files(&gguf_repo_files(), Some("4bit"));
        assert_eq!(files, vec!["Qwen3-Embedding-0.6B-Q4_K_S.gguf"]);
    }

    #[test]
    fn test_select_gguf_8bit_picks_q8() {
        let files = select_gguf_files(&gguf_repo_files(), Some("8bit"));
        assert_eq!(files, vec!["Qwen3-Embedding-0.6B-Q8_0.gguf"]);
    }

    #[test]
    fn test_select_gguf_f16_picks_f16() {
        let files = select_gguf_files(&gguf_repo_files(), Some("f16"));
        assert_eq!(files, vec!["Qwen3-Embedding-0.6B-F16.gguf"]);
    }

    #[test]
    fn test_select_gguf_4bit_fallback_when_q4ks_missing() {
        // Repo only has Q4_K_M and Q8 — should fall back to Q4_K_M
        let files = vec![
            "model-Q4_K_M.gguf".to_string(),
            "model-Q8_0.gguf".to_string(),
        ];
        let selected = select_gguf_files(&files, Some("4bit"));
        assert_eq!(selected, vec!["model-Q4_K_M.gguf"]);
    }

    #[test]
    fn test_select_gguf_no_hint_defaults_to_q4ks() {
        // Direct HF repo pull (no shortcut): should default to Q4_K_S
        let files = select_gguf_files(&gguf_repo_files(), None);
        assert_eq!(files, vec!["Qwen3-Embedding-0.6B-Q4_K_S.gguf"]);
    }

    #[test]
    fn test_select_gguf_no_hint_falls_back_to_q4km() {
        // No Q4_K_S available: default falls back to Q4_K_M
        let files = vec![
            "model-Q4_K_M.gguf".to_string(),
            "model-F16.gguf".to_string(),
        ];
        let selected = select_gguf_files(&files, None);
        assert_eq!(selected, vec!["model-Q4_K_M.gguf"]);
    }

    #[test]
    fn test_select_gguf_requested_quant_missing_falls_back() {
        // Repo only has F16 — 4bit fallback chain eventually lands on best available
        let files = vec!["model-F16.gguf".to_string()];
        let selected = select_gguf_files(&files, Some("4bit"));
        // Falls back through Q4_K_S→Q4_K_M→Q8_0, none found, last-resort: all files
        assert_eq!(selected, vec!["model-F16.gguf"]);
    }

    #[test]
    fn test_select_gguf_picks_correct_not_f16_when_4bit_requested() {
        // The bug: before fix, ALL files were downloaded including F16 and Q8
        let all_files = gguf_repo_files();
        let selected = select_gguf_files(&all_files, Some("4bit"));
        assert!(!selected.iter().any(|f| f.contains("F16")),  "Must not download F16 when 4bit requested");
        assert!(!selected.iter().any(|f| f.contains("Q8_0")), "Must not download Q8_0 when 4bit requested");
        assert!(selected.iter().any(|f| f.contains("Q4_K_S")), "Must download Q4_K_S for 4bit");
    }
}
