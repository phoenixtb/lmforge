use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

/// The bundled default MLX catalog — embedded at compile time.
/// This is the authoritative fallback used when no runtime catalog file exists.
/// This means `lmforge pull <shortcut>` works on a fresh install without running `lmforge init` first.
pub const BUNDLED_MLX: &str = include_str!("../../../data/catalogs/mlx.json");

/// The bundled default safetensors catalog — used by SGLang on Linux + NVIDIA.
/// Same `{ shortcut: "org/repo" }` shape as the MLX catalog.
pub const BUNDLED_SAFETENSORS: &str = include_str!("../../../data/catalogs/safetensors.json");

/// The bundled default GGUF catalog — used by llama.cpp on every platform
/// (Windows / CPU-only Linux / low-VRAM GPUs / macOS). Same `{ shortcut: "org/repo" }`
/// shape; the resolver auto-picks the right `.gguf` file from the `:NNbit` quant
/// suffix (see `gguf_patterns_for_quant` + `select_gguf_files` in resolver.rs).
pub const BUNDLED_GGUF: &str = include_str!("../../../data/catalogs/gguf.json");

// ── Public result type returned to the resolver ───────────────────────────────

/// The result of resolving a catalog shortcut.
///
/// All three supported catalogs (MLX, safetensors, GGUF) share the same shape:
/// `{ shortcut: "org/repo" }`. The downloader then pulls the format-matching
/// files; for GGUF repos with multiple quants, the resolver further narrows
/// the selection via the `:NNbit` suffix in the shortcut (see
/// `gguf_patterns_for_quant` + `select_gguf_files` in `model::resolver`).
#[derive(Debug, Clone)]
pub enum CatalogResult {
    /// Download all format-matching files from this HuggingFace repo.
    AllFiles(String),
}

// ── Resolution API ────────────────────────────────────────────────────────────

/// Dynamically resolves a model target shortcut from the JSON catalogs,
/// according to the engine format.
///
/// Resolution order:
/// 1. Runtime file at `catalogs_dir/{format}.json` (allows user overrides)
/// 2. Bundled catalog embedded in the binary at compile time
///
/// Engine formats without a bundled catalog return `None`; the resolver then
/// bails with an "unknown model" error unless the user provided a direct HF repo.
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
fn resolve_from_content(
    content: &str,
    normalized: &str,
    format_str: &str,
) -> Option<CatalogResult> {
    match format_str {
        // All three catalogs share the same `{ shortcut: "org/repo" }` shape.
        "mlx" | "safetensors" | "gguf" => {
            let map: HashMap<String, String> = serde_json::from_str(content).ok()?;
            map.get(normalized).cloned().map(CatalogResult::AllFiles)
        }
        _ => None,
    }
}

/// Resolve against the compile-time embedded catalog for the given format.
pub fn resolve_from_bundled(normalized: &str, format_str: &str) -> Option<CatalogResult> {
    let content = match format_str {
        "mlx" => BUNDLED_MLX,
        "safetensors" => BUNDLED_SAFETENSORS,
        "gguf" => BUNDLED_GGUF,
        _ => return None,
    };
    resolve_from_content(content, normalized, format_str)
}

// ── Shortcut listing (for suggestions + UI) ───────────────────────────────────

/// All available shortcuts across all bundled catalogs for a given format.
/// Used to generate helpful suggestions in error messages.
pub fn bundled_shortcuts(format_str: &str) -> Vec<String> {
    let format_lower = format_str.to_lowercase();
    let content = match format_lower.as_str() {
        "mlx" => BUNDLED_MLX,
        "safetensors" => BUNDLED_SAFETENSORS,
        "gguf" => BUNDLED_GGUF,
        _ => return vec![],
    };

    let keys: Vec<String> = serde_json::from_str::<HashMap<String, String>>(content)
        .map(|m| m.into_keys().collect())
        .unwrap_or_default();

    let mut filtered: Vec<String> = keys.into_iter().filter(|k| !k.starts_with('_')).collect();
    filtered.sort();
    filtered
}

// ── Engine-format detection (shared between pull / run / catalog / init) ─────

/// Decide which catalog format (`"mlx" | "safetensors" | "gguf"`) the
/// caller should resolve against on this host.
///
/// Resolution order (fail-soft at every step):
///   1. Read `<data_dir>/hardware.json` and try the registry's `select()`.
///      Returns `selected.model_format` — the engine that *would* be picked
///      for the current hardware, including the Phase-1 tier filter that
///      keeps SGLang out of consumer Blackwell's default path.
///   2. Fall back to a kernel-level heuristic: `gpu_vendor == "apple"` ⇒ MLX.
///   3. Final fallback: GGUF — the universal catalog used by llama.cpp,
///      which is now the default engine on every NVIDIA / AMD / CPU host.
///
/// We deliberately default to GGUF (not safetensors) because the post-Phase-1
/// selector picks `llamacpp` on every non-Apple host until the user opts into
/// `vllm` or `exl3`. Pulling safetensors weights on those hosts would leave the
/// model unloadable by the active engine — the exact mismatch the catalog
/// flip is designed to prevent.
pub fn detect_engine_format(data_dir: &std::path::Path) -> String {
    // 1. Registry-aware path (preferred).
    let hw_path = data_dir.join("hardware.json");
    if let Ok(json) = std::fs::read_to_string(&hw_path)
        && let Ok(profile) =
            serde_json::from_str::<crate::hardware::probe::HardwareProfile>(&json)
    {
        let user_override = data_dir.join("engines.toml");
        let override_path = if user_override.exists() {
            Some(user_override.as_path())
        } else {
            None
        };
        if let Ok(registry) = crate::engine::registry::EngineRegistry::load(override_path)
            && let Ok(selected) = registry.select(&profile)
        {
            return selected.model_format.clone();
        }

        // Registry load failed — fall back to gpu-vendor heuristic on the same profile.
        return format_for_gpu_vendor(profile.gpu_vendor);
    }

    // 2. hardware.json missing OR malformed — try raw JSON for the vendor field
    //    (handles hand-rolled or older-schema files).
    if let Ok(json) = std::fs::read_to_string(&hw_path)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&json)
        && let Some(vendor_str) = v["gpu_vendor"].as_str()
    {
        let vendor = match vendor_str {
            "apple" => crate::hardware::probe::GpuVendor::Apple,
            "nvidia" => crate::hardware::probe::GpuVendor::Nvidia,
            "amd" => crate::hardware::probe::GpuVendor::Amd,
            _ => crate::hardware::probe::GpuVendor::None,
        };
        return format_for_gpu_vendor(vendor);
    }

    // 3. No hardware info at all — GGUF works on every platform.
    "gguf".to_string()
}

/// Pure function: GPU vendor → bundled catalog format. Used as a fallback
/// when the engine registry can't be loaded.
pub fn format_for_gpu_vendor(vendor: crate::hardware::probe::GpuVendor) -> String {
    use crate::hardware::probe::GpuVendor;
    match vendor {
        GpuVendor::Apple => "mlx".to_string(),
        // Everything else lands on llama.cpp by default → GGUF catalog.
        GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::None => "gguf".to_string(),
    }
}

// ── UI catalog listing ────────────────────────────────────────────────────────

/// A structured catalog entry returned by `GET /lf/catalog`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogEntry {
    /// The shortcut key, e.g. `"qwen3:8b:4bit"`.
    pub shortcut: String,
    /// The resolved HuggingFace repo, e.g. `"mlx-community/Qwen3-8B-4bit"`.
    pub hf_repo: String,
    /// Engine format: `"mlx"`, `"safetensors"`, or `"gguf"`.
    pub format: String,
    /// Tags derived by splitting the shortcut on `':'`, e.g. `["qwen3","8b","4bit"]`.
    pub tags: Vec<String>,
    /// Inferred capability role: `"chat"`, `"embed"`, `"rerank"`, `"vision"`, or `"code"`.
    pub role: String,
}

/// Return all catalog entries for the requested format(s).
/// Pass an empty string to get entries from every bundled catalog.
/// Any unknown format returns an empty vec — fail-closed rather than silently
/// returning unrelated catalogs.
/// `_comment_*` keys and entries without a '/' in the value are silently skipped.
/// Results are sorted by shortcut name.
pub fn list_for_ui(format: &str) -> Vec<CatalogEntry> {
    let formats: &[(&str, &str)] = match format.to_lowercase().as_str() {
        "mlx" => &[("mlx", BUNDLED_MLX)],
        "safetensors" => &[("safetensors", BUNDLED_SAFETENSORS)],
        "gguf" => &[("gguf", BUNDLED_GGUF)],
        "" => &[
            ("mlx", BUNDLED_MLX),
            ("safetensors", BUNDLED_SAFETENSORS),
            ("gguf", BUNDLED_GGUF),
        ],
        _ => &[],
    };

    let mut entries: Vec<CatalogEntry> = Vec::new();

    for &(fmt, content) in formats {
        let map: HashMap<String, String> = match serde_json::from_str(content) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for (shortcut, hf_repo) in map {
            if shortcut.starts_with('_') {
                continue;
            }
            if !hf_repo.contains('/') {
                continue;
            }
            let tags = shortcut.split(':').map(str::to_string).collect();
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
    if s.contains("vision")
        || s.contains("-vl-")
        || s.contains("_vl_")
        || s.contains("-vl:")
        || s.contains("/vl-")
        || s.contains("llava")
        || s.contains("minicpm-v")
        || s.contains("minicpmv")
    {
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
    fn test_bundled_mlx_catalog_is_valid_json() {
        let result: Result<HashMap<String, String>, _> = serde_json::from_str(BUNDLED_MLX);
        assert!(
            result.is_ok(),
            "MLX catalog is not valid JSON: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_bundled_safetensors_catalog_is_valid_json() {
        let result: Result<HashMap<String, String>, _> = serde_json::from_str(BUNDLED_SAFETENSORS);
        assert!(
            result.is_ok(),
            "safetensors catalog is not valid JSON: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_bundled_gguf_catalog_is_valid_json() {
        let result: Result<HashMap<String, String>, _> = serde_json::from_str(BUNDLED_GGUF);
        assert!(
            result.is_ok(),
            "GGUF catalog is not valid JSON: {:?}",
            result.err()
        );
    }

    // ── Critical shortcuts present ────────────────────────────────────────────

    #[test]
    fn test_primary_models_exist_in_mlx() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        assert!(map.contains_key("qwen3.5:4b:4bit"), "MLX missing LLM_MODEL");
        assert!(
            map.contains_key("qwen3.5:2b:4bit"),
            "MLX missing LLM_FALLBACK_MODEL"
        );
        assert!(
            map.contains_key("qwen3-embed:0.6b:4bit"),
            "MLX missing LLM_EMBED_MODEL"
        );
    }

    #[test]
    fn test_primary_models_exist_in_safetensors() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_SAFETENSORS).unwrap();
        // Chat: catalog is quantized-only — every inference shortcut carries
        // a `:4bit` or `:8bit` suffix (no bare `qwen3:8b` aliases).
        assert!(map.contains_key("qwen3:8b:4bit"), "safetensors missing qwen3:8b:4bit");
        assert!(map.contains_key("qwen3:8b:8bit"), "safetensors missing qwen3:8b:8bit");
        // Embeddings stay at native precision (small models, quality-sensitive).
        assert!(map.contains_key("qwen3-embed:0.6b"), "safetensors missing 0.6B embed");
    }

    #[test]
    fn test_safetensors_has_vlm_entries() {
        let map: HashMap<String, String> = serde_json::from_str(BUNDLED_SAFETENSORS).unwrap();
        // VLMs are quantized-only too — `:4bit` maps to the official AWQ build.
        assert_eq!(
            map.get("qwen2.5-vl:3b:4bit").unwrap(),
            "Qwen/Qwen2.5-VL-3B-Instruct-AWQ"
        );
        assert_eq!(
            map.get("qwen2.5-vl:7b:4bit").unwrap(),
            "Qwen/Qwen2.5-VL-7B-Instruct-AWQ"
        );
    }

    // ── Renamed keys must NOT appear in the live catalogs ─────────────────────

    #[test]
    fn test_no_legacy_q4_suffix_in_mlx() {
        let mlx: HashMap<String, String> = serde_json::from_str(BUNDLED_MLX).unwrap();
        for key in mlx.keys() {
            assert!(
                !key.ends_with(":q4"),
                "MLX catalog has legacy :q4 key: {}",
                key
            );
        }
    }

    // ── detect_engine_format (Phase 2.1 catalog priority flip) ────────────────

    fn write_hw(dir: &std::path::Path, json: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("hardware.json"), json).unwrap();
    }

    #[test]
    fn detect_format_consumer_blackwell_picks_gguf() {
        // The P0 fix: an RTX 5060 Ti box (sm_120) must pull GGUF, not safetensors.
        // Pre-Phase-1, the registry would have picked SGLang here and this
        // returned "safetensors". Now llamacpp wins → "gguf".
        let dir = std::env::temp_dir().join("lmforge_test_detect_blackwell");
        let _ = std::fs::remove_dir_all(&dir);
        let hw = r#"{
            "os": "linux", "arch": "x86_64", "is_tegra": false,
            "gpu_vendor": "nvidia", "vram_gb": 15.4, "unified_mem": false,
            "total_ram_gb": 15.6, "cpu_cores": 6, "cpu_model": "i7",
            "schema_version": 2,
            "compute_cap": [12, 0],
            "os_family": "linux",
            "is_wsl": false,
            "gpu_count": 1
        }"#;
        write_hw(&dir, hw);
        assert_eq!(detect_engine_format(&dir), "gguf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_format_apple_silicon_picks_mlx() {
        let dir = std::env::temp_dir().join("lmforge_test_detect_apple");
        let _ = std::fs::remove_dir_all(&dir);
        let hw = r#"{
            "os": "darwin", "arch": "aarch64", "is_tegra": false,
            "gpu_vendor": "apple", "vram_gb": 36.0, "unified_mem": true,
            "total_ram_gb": 48.0, "cpu_cores": 14, "cpu_model": "M3",
            "schema_version": 2,
            "os_family": "darwin",
            "is_wsl": false,
            "gpu_count": 0
        }"#;
        write_hw(&dir, hw);
        assert_eq!(detect_engine_format(&dir), "mlx");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_format_linux_cpu_picks_gguf() {
        let dir = std::env::temp_dir().join("lmforge_test_detect_cpu");
        let _ = std::fs::remove_dir_all(&dir);
        let hw = r#"{
            "os": "linux", "arch": "x86_64", "is_tegra": false,
            "gpu_vendor": "none", "vram_gb": 0.0, "unified_mem": false,
            "total_ram_gb": 16.0, "cpu_cores": 4, "cpu_model": "i5",
            "schema_version": 2,
            "os_family": "linux",
            "is_wsl": false,
            "gpu_count": 0
        }"#;
        write_hw(&dir, hw);
        assert_eq!(detect_engine_format(&dir), "gguf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_format_hopper_picks_gguf_post_phase1() {
        // Even a beefy H100 box doesn't auto-select SGLang anymore — it's
        // experimental. llamacpp wins → GGUF catalog.
        let dir = std::env::temp_dir().join("lmforge_test_detect_hopper");
        let _ = std::fs::remove_dir_all(&dir);
        let hw = r#"{
            "os": "linux", "arch": "x86_64", "is_tegra": false,
            "gpu_vendor": "nvidia", "vram_gb": 80.0, "unified_mem": false,
            "total_ram_gb": 256.0, "cpu_cores": 64, "cpu_model": "EPYC",
            "schema_version": 2,
            "compute_cap": [9, 0],
            "os_family": "linux",
            "is_wsl": false,
            "gpu_count": 1
        }"#;
        write_hw(&dir, hw);
        assert_eq!(detect_engine_format(&dir), "gguf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_format_missing_hardware_json_falls_back_to_gguf() {
        // Fresh box where `lmforge init` hasn't run yet. GGUF works on every
        // platform, so it's the safest universal default.
        let dir = std::env::temp_dir().join("lmforge_test_detect_no_hw");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(detect_engine_format(&dir), "gguf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_format_malformed_hardware_json_falls_back_to_gguf() {
        let dir = std::env::temp_dir().join("lmforge_test_detect_malformed");
        let _ = std::fs::remove_dir_all(&dir);
        write_hw(&dir, "this is not json {{{");
        assert_eq!(detect_engine_format(&dir), "gguf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn format_for_gpu_vendor_matrix() {
        use crate::hardware::probe::GpuVendor;
        assert_eq!(format_for_gpu_vendor(GpuVendor::Apple), "mlx");
        assert_eq!(format_for_gpu_vendor(GpuVendor::Nvidia), "gguf");
        assert_eq!(format_for_gpu_vendor(GpuVendor::Amd), "gguf");
        assert_eq!(format_for_gpu_vendor(GpuVendor::None), "gguf");
    }

    // ── bundled_shortcuts filters comment keys ────────────────────────────────

    #[test]
    fn test_bundled_shortcuts_excludes_comment_keys() {
        for fmt in &["mlx", "safetensors", "gguf"] {
            let shortcuts = bundled_shortcuts(fmt);
            for key in &shortcuts {
                assert!(
                    !key.starts_with('_'),
                    "{} shortcuts contain comment key: {}",
                    fmt,
                    key
                );
            }
            assert!(
                !shortcuts.is_empty(),
                "{} shortcuts should not be empty",
                fmt
            );
        }
    }

    #[test]
    fn test_bundled_shortcuts_returns_empty_for_unknown_format() {
        assert!(bundled_shortcuts("ollama").is_empty());
        assert!(bundled_shortcuts("").is_empty());
    }

    // ── infer_role ────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_role_embed() {
        assert_eq!(
            infer_role("qwen3-embed:0.6b", "Qwen/Qwen3-Embedding-0.6B"),
            "embed"
        );
        assert_eq!(
            infer_role("nomic-embed-text:v1.5", "nomic-ai/nomic-embed-text-v1.5"),
            "embed"
        );
    }

    #[test]
    fn test_infer_role_rerank() {
        assert_eq!(
            infer_role(
                "jina-reranker-v2:multilingual",
                "jinaai/jina-reranker-v2-base-multilingual"
            ),
            "rerank"
        );
        assert_eq!(
            infer_role("bge-reranker-v2-m3", "BAAI/bge-reranker-v2-m3"),
            "rerank"
        );
    }

    #[test]
    fn test_infer_role_chat() {
        assert_eq!(infer_role("qwen3:8b:4bit", "Qwen/Qwen3-8B-AWQ"), "chat");
        assert_eq!(infer_role("gemma3:4b", "google/gemma-3-4b-it"), "chat");
    }

    #[test]
    fn test_infer_role_vision_qwen25_vl() {
        assert_eq!(
            infer_role("qwen2.5-vl:7b", "Qwen/Qwen2.5-VL-7B-Instruct"),
            "vision"
        );
        assert_eq!(
            infer_role("qwen2.5-vl:3b:4bit", "Qwen/Qwen2.5-VL-3B-Instruct-AWQ"),
            "vision"
        );
    }

    // ── resolve_from_bundled ──────────────────────────────────────────────────

    #[test]
    fn test_resolve_from_bundled_safetensors() {
        let r = resolve_from_bundled("qwen2.5-vl:7b:4bit", "safetensors").unwrap();
        let CatalogResult::AllFiles(repo) = r;
        assert_eq!(repo, "Qwen/Qwen2.5-VL-7B-Instruct-AWQ");
    }

    #[test]
    fn test_resolve_from_bundled_mlx() {
        let r = resolve_from_bundled("qwen3.5:4b:4bit", "mlx").unwrap();
        let CatalogResult::AllFiles(repo) = r;
        assert_eq!(repo, "mlx-community/Qwen3.5-4B-4bit");
    }

    #[test]
    fn test_resolve_from_bundled_gguf() {
        // GGUF catalog restored — same shape as MLX/safetensors. Resolver
        // picks the right .gguf file from the :NNbit suffix downstream.
        let r = resolve_from_bundled("qwen3.5:4b:4bit", "gguf").unwrap();
        let CatalogResult::AllFiles(repo) = r;
        assert_eq!(repo, "unsloth/Qwen3.5-4B-GGUF");

        let r = resolve_from_bundled("qwen3.5:4b:6bit", "gguf").unwrap();
        let CatalogResult::AllFiles(repo) = r;
        assert_eq!(repo, "unsloth/Qwen3.5-4B-GGUF");

        let r = resolve_from_bundled("qwen3-embed:0.6b:f16", "gguf").unwrap();
        let CatalogResult::AllFiles(repo) = r;
        assert_eq!(repo, "Qwen/Qwen3-Embedding-0.6B-GGUF");
    }

    #[test]
    fn test_resolve_from_bundled_unknown_format_returns_none() {
        assert!(resolve_from_bundled("qwen3.5:4b:4bit", "ollama").is_none());
        assert!(resolve_from_bundled("qwen3.5:4b:4bit", "").is_none());
    }

    #[test]
    fn test_resolve_from_bundled_missing_key_returns_none() {
        assert!(resolve_from_bundled("nonexistent:model", "safetensors").is_none());
        assert!(resolve_from_bundled("nonexistent:model", "mlx").is_none());
    }

    // ── list_for_ui ───────────────────────────────────────────────────────────

    #[test]
    fn test_list_for_ui_excludes_comment_keys() {
        let entries = list_for_ui("safetensors");
        for e in &entries {
            assert!(
                !e.shortcut.starts_with('_'),
                "list_for_ui returned comment key: {}",
                e.shortcut
            );
        }
    }

    #[test]
    fn test_list_for_ui_is_sorted() {
        let entries = list_for_ui("safetensors");
        let shortcuts: Vec<&str> = entries.iter().map(|e| e.shortcut.as_str()).collect();
        let mut sorted = shortcuts.clone();
        sorted.sort();
        assert_eq!(
            shortcuts, sorted,
            "list_for_ui must return entries sorted by shortcut"
        );
    }

    #[test]
    fn test_list_for_ui_gguf_returns_entries() {
        let entries = list_for_ui("gguf");
        assert!(!entries.is_empty(), "gguf catalog should not be empty");
        for e in &entries {
            assert_eq!(e.format, "gguf");
            assert!(!e.shortcut.starts_with('_'));
        }
    }

    #[test]
    fn test_list_for_ui_empty_format_includes_all_three() {
        let entries = list_for_ui("");
        let formats: std::collections::HashSet<&str> =
            entries.iter().map(|e| e.format.as_str()).collect();
        assert!(formats.contains("mlx"));
        assert!(formats.contains("safetensors"));
        assert!(formats.contains("gguf"));
    }
}
