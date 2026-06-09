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
    /// Layered MTP signal — `Some(true)` means speculative decoding via
    /// the model's own next-N head is known to be available; `Some(false)`
    /// confirms the model has no MTP layers; `None` means "not probed yet"
    /// (the launch path will fall back to the catalog flag, then to a
    /// GGUF tensor probe via `crate::model::gguf_inspect::detect_mtp`).
    pub mtp: Option<bool>,
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
        let mut rm = resolve_hf_repo(input, engine_format, None, None).await?;
        rm.id = input.to_string();
        return Ok(rm);
    }

    // URL
    if input.contains("://") {
        return Ok(ResolvedModel {
            id: input.to_string(),
            dir_name: input.split('/').next_back().unwrap_or("model").to_string(),
            hf_repo: input.to_string(),
            format: detect_format_from_engine(engine_format),
            files: vec![input.to_string()],
            mtp: None,
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
            mtp: None,
        });
    }

    // Logical name — look up in curated catalog
    resolve_logical_name(input, engine_format, catalogs_dir).await
}

/// Resolve an HF repo — query the API for file listing.
/// `quant_hint` comes from the catalog shortcut (e.g. "4bit", "f16") and
/// is used to select the right GGUF file(s) from multi-quant repos.
async fn resolve_hf_repo(
    repo: &str,
    _engine_format: &str,
    quant_hint: Option<&str>,
    catalog_shortcut: Option<&str>,
) -> Result<ResolvedModel> {
    // Support `repo@revision` syntax for engines whose ecosystems store
    // model weights on non-`main` branches. Canonical use case: EXL3 repos
    // (turboderp/*-exl3) publish each bits-per-weight on its own branch
    // (`4.0bpw`, `6.0bpw`, `8.0bpw_H8`, ...) with the `main` branch
    // containing only README.md.
    //
    // The revision is preserved through the pipeline by re-encoding it in
    // the `hf_repo` field of `ResolvedModel`; adapters that need the
    // revision (TabbyAPI) split it back out at `pull_model` time.
    let (repo_base, revision) = split_revision(repo);
    let api_url = match revision {
        Some(rev) => format!(
            "https://huggingface.co/api/models/{}/revision/{}",
            repo_base, rev
        ),
        None => format!("https://huggingface.co/api/models/{}", repo_base),
    };
    info!(repo = %repo_base, revision = ?revision, "Resolving HuggingFace repo");

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
    let repo_lower = repo_base.to_lowercase();

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
        let mut selected = select_gguf_files(&all_format_files, quant_hint);
        append_vlm_sidecars(
            &all_format_files,
            &mut selected,
            catalog_shortcut,
            repo_base,
        );
        selected
    } else {
        all_format_files
    };

    // Derive a friendly dir name. Suffix the revision when set so
    // `turboderp/Qwen3-8B-exl3@6.0bpw` lands at `qwen3-8b-exl3-6.0bpw`
    // and two bpw variants of the same model don't collide on disk.
    let base = repo_base
        .split('/')
        .next_back()
        .unwrap_or(repo_base)
        .to_lowercase();
    let dir_name = match revision {
        Some(rev) => format!("{}-{}", base, sanitize_revision(rev)),
        None => base,
    };

    debug!(
        repo = %repo_base,
        revision = ?revision,
        ?format,
        file_count = files.len(),
        "Resolved HF repo"
    );

    // Re-encode revision into hf_repo so downstream adapters can split it.
    let hf_repo = match revision {
        Some(rev) => format!("{}@{}", repo_base, rev),
        None => repo_base.to_string(),
    };

    Ok(ResolvedModel {
        id: hf_repo.clone(),
        dir_name,
        hf_repo,
        format,
        files,
        mtp: None,
    })
}

/// Split `"repo@revision"` into `("repo", Some("revision"))`. Plain
/// `"org/repo"` returns `("org/repo", None)`. The `@` separator was chosen
/// because HF repo IDs already disallow it in the path component.
pub(crate) fn split_revision(input: &str) -> (&str, Option<&str>) {
    match input.split_once('@') {
        Some((repo, rev)) if !rev.is_empty() => (repo, Some(rev)),
        _ => (input, None),
    }
}

/// Make a revision string filesystem-safe — branch names like
/// `8.0bpw_H8` are already clean, but a future user-typed `@feat/foo`
/// would break dir names without this.
fn sanitize_revision(rev: &str) -> String {
    rev.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | ' ' => '_',
            c => c,
        })
        .collect()
}

async fn resolve_logical_name(
    name: &str,
    engine_format: &str,
    catalogs_dir: &std::path::Path,
) -> Result<ResolvedModel> {
    if let Some(catalog_result) =
        crate::model::catalog::load_catalog_and_resolve(name, engine_format, catalogs_dir).await
    {
        let repo = catalog_result.repo().to_string();
        let catalog_mtp = catalog_result.mtp();
        let quant_hint = extract_quant_hint(name);
        debug!(
            name,
            ?quant_hint,
            catalog_mtp = ?catalog_mtp,
            "Resolved repo from catalog, querying HF API"
        );
        let mut rm = resolve_hf_repo(&repo, engine_format, quant_hint, Some(name)).await?;
        rm.id = name.to_string();
        rm.mtp = catalog_mtp;
        return Ok(rm);
    }

    // Build a helpful suggestion list from the bundled catalog so it's always accurate.
    let suggestions = crate::model::catalog::bundled_shortcuts(engine_format);
    let suggestion_hint = if suggestions.is_empty() {
        "run 'lmforge catalog list' to see available shortcuts".to_string()
    } else {
        format!(
            "use a shortcut like: {}",
            suggestions
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    anyhow::bail!(
        "Unknown model '{}'. Try a HF repo like 'Qwen/Qwen3-8B-AWQ' or {}",
        name,
        suggestion_hint
    );
}

fn detect_format_from_engine(engine_format: &str) -> ModelFormat {
    match engine_format {
        "mlx" => ModelFormat::Mlx,
        "gguf" => ModelFormat::Gguf,
        _ => ModelFormat::Safetensors,
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
            "4bit"
                | "5bit"
                | "6bit"
                | "8bit"
                | "q4"
                | "q5"
                | "q6"
                | "q8"
                | "f16"
                | "bf16"
                | "mxfp4"
                | "mxfp8"
                | "fp16"
        )
    })
}

/// Return ordered GGUF filename substrings for a given quant tag.
/// Accepts both long form ("4bit", "8bit") and short form ("q4", "q8").
///
/// Priority: **Unsloth Dynamic (UD-Q*_K_XL)** when available — Unsloth's
/// dynamic quants give better perplexity at ~5–10 % larger size than the stock
/// k-quants. Falls back through standard k-quants (Q*_K_M > Q*_K_S > Q*_K)
/// for repos that don't ship the UD- variants (lmstudio-community, bartowski,
/// mradermacher, gpustack, …).
fn gguf_patterns_for_quant(quant: &str) -> &'static [&'static str] {
    match quant {
        "4bit" | "q4" => &["UD-Q4_K_XL", "Q4_K_M", "Q4_K_S", "Q4_K"],
        "5bit" | "q5" => &["UD-Q5_K_XL", "Q5_K_M", "Q5_K_S", "Q5_K"],
        "6bit" | "q6" => &["UD-Q6_K_XL", "Q6_K"],
        "8bit" | "q8" => &["UD-Q8_K_XL", "Q8_0"],
        "f16" | "bf16" => &["F16", "FP16", "BF16", "f16", "bf16", "fp16"],
        _ => &[],
    }
}

/// Basename of a HF sibling path (`org/repo/file.gguf` → `file.gguf`).
fn gguf_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Multimodal projector sidecar shipped alongside VLM weights.
fn is_mmproj_sidecar(filename: &str) -> bool {
    gguf_basename(filename).starts_with("mmproj-")
}

/// Heuristic: this pull target is a vision-language model.
fn is_vlm_target(catalog_shortcut: Option<&str>, repo: &str) -> bool {
    if let Some(id) = catalog_shortcut {
        let h = id.to_ascii_lowercase();
        if h.contains(":vl:") || h.contains("-vl-") || h.contains("vision") {
            return true;
        }
    }
    let r = repo.to_ascii_lowercase();
    r.contains("-vl-")
        || r.contains("-vl")
        || r.contains("vl-instruct")
        || r.contains("qwen2.5-vl")
        || r.contains("qwen3-vl")
        || r.contains("minicpm-v")
}

/// Pick one `mmproj-*.gguf` from a repo listing. Prefer F16 → BF16 → F32.
fn select_mmproj_sidecar(all_gguf: &[String]) -> Option<String> {
    let mmprojs: Vec<&String> = all_gguf.iter().filter(|f| is_mmproj_sidecar(f)).collect();
    if mmprojs.is_empty() {
        return None;
    }
    for tag in ["F16", "BF16", "F32"] {
        if let Some(f) = mmprojs.iter().find(|f| mmproj_matches_quant_tag(f, tag)) {
            return Some((*f).clone());
        }
    }
    Some(mmprojs[0].clone())
}

fn mmproj_matches_quant_tag(filename: &str, tag: &str) -> bool {
    let base = gguf_basename(filename).to_ascii_uppercase();
    base.contains(&format!("-{tag}.")) || base.ends_with(&format!("-{tag}"))
}

/// Append VLM sidecars (mmproj) to the download list when resolving a VL repo.
fn append_vlm_sidecars(
    all_gguf: &[String],
    files: &mut Vec<String>,
    catalog_shortcut: Option<&str>,
    repo: &str,
) {
    if !is_vlm_target(catalog_shortcut, repo) {
        return;
    }
    if let Some(mmproj) = select_mmproj_sidecar(all_gguf)
        && !files.contains(&mmproj)
    {
        debug!(mmproj = %mmproj, "Appending VLM mmproj sidecar to download list");
        files.push(mmproj);
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
    // Never treat mmproj sidecars as main model weights — a `:f16` pull on a
    // VLM repo would otherwise match `mmproj-F16.gguf` instead of the backbone.
    let weights: Vec<String> = all_gguf
        .iter()
        .filter(|f| !is_mmproj_sidecar(f))
        .cloned()
        .collect();

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
        let found = matches_pattern(&weights, pat);
        if !found.is_empty() {
            return found;
        }
    }

    // Requested quant not found → warn and fall back
    if let Some(q) = quant_hint {
        eprintln!("\n  ⚠  Quantization '{}' not available in this repo.", q);
        eprintln!("     Attempting fallback (Q4_K_S → Q4_K_M → Q8_0) …");
        for &pat in &["Q4_K_S", "Q4_K_M", "Q8_0"] {
            let found = matches_pattern(&weights, pat);
            if !found.is_empty() {
                eprintln!("     Using: {:?}", found);
                return found;
            }
        }
    }

    // Last resort: return everything (repo has unusual naming)
    eprintln!("  ⚠  Could not identify a specific quantization file. Downloading all GGUF files.");
    weights
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

    // ── split_revision ────────────────────────────────────────────────────────

    #[test]
    fn split_revision_plain_repo() {
        assert_eq!(split_revision("org/repo"), ("org/repo", None));
    }

    #[test]
    fn split_revision_with_branch() {
        assert_eq!(
            split_revision("turboderp/Qwen3-8B-exl3@6.0bpw"),
            ("turboderp/Qwen3-8B-exl3", Some("6.0bpw"))
        );
    }

    #[test]
    fn split_revision_with_h8_suffix() {
        assert_eq!(
            split_revision("turboderp/Qwen3-8B-exl3@8.0bpw_H8"),
            ("turboderp/Qwen3-8B-exl3", Some("8.0bpw_H8"))
        );
    }

    #[test]
    fn split_revision_empty_after_at_falls_back() {
        // A trailing `@` with no revision is treated as "no revision"
        // rather than `Some("")` so an accidental typo doesn't crash the
        // HF API call with /revision/ (empty path component → 404).
        assert_eq!(split_revision("org/repo@"), ("org/repo@", None));
    }

    #[test]
    fn sanitize_revision_replaces_filesystem_specials() {
        assert_eq!(sanitize_revision("6.0bpw"), "6.0bpw");
        assert_eq!(sanitize_revision("feat/foo"), "feat_foo");
        assert_eq!(sanitize_revision("8.0bpw_H8"), "8.0bpw_H8");
    }

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
        assert_eq!(
            detect_format_from_engine("safetensors"),
            ModelFormat::Safetensors
        );
    }

    // GGUF catalog tests removed alongside the catalog. Direct HF repo pulls of
    // GGUF format are still exercised by `should_download_file` + the live HF
    // API path, but no longer have shortcut entries.

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
        assert_eq!(
            extract_quant_hint("nomic-modernbert-embed:f16"),
            Some("f16")
        );
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
        assert!(
            !selected.iter().any(|f| f.contains("F16")),
            "Must not download F16 when 4bit requested"
        );
        assert!(
            !selected.iter().any(|f| f.contains("Q8_0")),
            "Must not download Q8_0 when 4bit requested"
        );
        assert!(
            selected.iter().any(|f| f.contains("Q4_K_S")),
            "Must download Q4_K_S for 4bit"
        );
    }

    #[test]
    fn select_gguf_f16_on_vlm_repo_skips_mmproj_sidecar() {
        let files = vec![
            "Qwen2.5-VL-3B-Instruct-UD-Q4_K_XL.gguf".to_string(),
            "mmproj-F16.gguf".to_string(),
            "mmproj-BF16.gguf".to_string(),
        ];
        let selected = select_gguf_files(&files, Some("f16"));
        assert_eq!(
            selected,
            vec!["Qwen2.5-VL-3B-Instruct-UD-Q4_K_XL.gguf"],
            "f16 quant must not pick mmproj-F16 as backbone"
        );
    }

    #[test]
    fn append_vlm_sidecars_adds_preferred_mmproj() {
        let all = vec![
            "Qwen2.5-VL-3B-Instruct-UD-Q4_K_XL.gguf".to_string(),
            "mmproj-BF16.gguf".to_string(),
            "mmproj-F16.gguf".to_string(),
        ];
        let mut selected = select_gguf_files(&all, Some("4bit"));
        append_vlm_sidecars(
            &all,
            &mut selected,
            Some("qwen2.5-vl:3b:4bit"),
            "unsloth/Qwen2.5-VL-3B-Instruct-GGUF",
        );
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|f| f.contains("UD-Q4_K_XL")));
        assert!(selected.contains(&"mmproj-F16.gguf".to_string()));
    }

    #[test]
    fn append_vlm_sidecars_skips_non_vlm_repo() {
        let all = vec![
            "Qwen3.5-4B-UD-Q4_K_XL.gguf".to_string(),
            "mmproj-F16.gguf".to_string(),
        ];
        let mut selected = vec!["Qwen3.5-4B-UD-Q4_K_XL.gguf".to_string()];
        append_vlm_sidecars(
            &all,
            &mut selected,
            Some("qwen3.5:4b:4bit"),
            "unsloth/Qwen3.5-4B-GGUF",
        );
        assert_eq!(selected.len(), 1);
    }
}
