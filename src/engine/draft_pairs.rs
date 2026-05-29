//! Curated draft-model pair lookup + broken-pair cache (S-3).
//!
//! When MTP is unavailable, `mode=auto` may promote to `DraftModel` if:
//!   1. A pair exists in the embedded `draft_pairs.toml` for this target.
//!   2. The draft model is installed (`models.json` + on-disk GGUF).
//!   3. VRAM headroom fits both models (see `speculative::vram_fits_draft`).
//!   4. The pair is not marked broken in `draft_pairs_status.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Runtime context passed into `speculative::resolve` when a draft pair
/// is eligible.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftResolveContext {
    pub draft_id: String,
    pub gguf_path: PathBuf,
    pub draft_size_gb: f32,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DraftPairEntry {
    pub(crate) target_family: String,
    pub(crate) draft_id: String,
    #[serde(default)]
    pub(crate) note: String,
}

#[derive(Debug, Deserialize)]
struct DraftPairsFile {
    pair: Vec<DraftPairEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DraftPairStatusFile {
    #[serde(default)]
    broken: HashMap<String, BrokenPairRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrokenPairRecord {
    reason: String,
    recorded_at: String,
}

fn embedded_pairs() -> Result<DraftPairsFile> {
    let raw = include_str!("../../data/draft_pairs.toml");
    toml::from_str(raw).context("parse embedded draft_pairs.toml")
}

fn pair_key(target_id: &str, draft_id: &str) -> String {
    format!("{target_id}|{draft_id}")
}

fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join("draft_pairs_status.json")
}

fn load_status(data_dir: &Path) -> DraftPairStatusFile {
    let path = status_path(data_dir);
    if !path.is_file() {
        return DraftPairStatusFile::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_status(data_dir: &Path, status: &DraftPairStatusFile) -> Result<()> {
    let path = status_path(data_dir);
    let json = serde_json::to_string_pretty(status)?;
    std::fs::write(path, json).context("write draft_pairs_status.json")
}

/// Whether `target_family` matches the given model id / HF repo.
pub fn matches_target_family(family: &str, model_id: &str, hf_repo: Option<&str>) -> bool {
    let id = model_id.to_ascii_lowercase();
    let repo = hf_repo.map(str::to_ascii_lowercase).unwrap_or_default();

    match family {
        "llama-3.x" => {
            (id.starts_with("llama3.") || repo.contains("llama-3"))
                && !id.contains("llama4")
                && !repo.contains("llama-4")
        }
        "qwen3.x" => {
            (id.starts_with("qwen3:") || id.starts_with("qwen3-"))
                && !id.starts_with("qwen3.5")
                && !id.contains("coder")
                && !repo.contains("Qwen3.5")
                && !repo.contains("Coder-Next")
        }
        "qwen2.5" => id.starts_with("qwen2.5:") || repo.contains("Qwen2.5"),
        other => id.contains(other) || repo.contains(other),
    }
}

/// Look up the draft catalog id for the first matching pair.
pub fn lookup_draft_pair(model_id: &str, hf_repo: Option<&str>) -> Option<String> {
    let file = embedded_pairs().ok()?;
    file.pair
        .into_iter()
        .find(|p| matches_target_family(&p.target_family, model_id, hf_repo))
        .map(|p| p.draft_id)
}

pub fn is_pair_broken(data_dir: &Path, target_id: &str, draft_id: &str) -> bool {
    let status = load_status(data_dir);
    status.broken.contains_key(&pair_key(target_id, draft_id))
}

/// Record a pair as broken so auto resolution never retries it (S-3.5).
pub fn record_broken_pair(
    data_dir: &Path,
    target_id: &str,
    draft_id: &str,
    reason: &str,
) -> Result<()> {
    let mut status = load_status(data_dir);
    status.broken.insert(
        pair_key(target_id, draft_id),
        BrokenPairRecord {
            reason: reason.to_string(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        },
    );
    save_status(data_dir, &status)
}

fn find_largest_gguf(model_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(model_dir).ok()?;
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("mmproj") {
            continue;
        }
        let size = entry.metadata().ok()?.len();
        if best.as_ref().map(|(s, _)| size > *s).unwrap_or(true) {
            best = Some((size, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Build runtime draft context when the pair is configured, installed, and
/// not marked broken.
pub fn build_draft_context(
    data_dir: &Path,
    model_id: &str,
    hf_repo: Option<&str>,
) -> Option<DraftResolveContext> {
    let draft_id = lookup_draft_pair(model_id, hf_repo)?;
    if is_pair_broken(data_dir, model_id, &draft_id) {
        return None;
    }

    let idx = crate::model::index::ModelIndex::load(data_dir).ok()?;
    let draft_entry = idx.get(&draft_id)?;
    let draft_dir = PathBuf::from(&draft_entry.path);
    let gguf_path = find_largest_gguf(&draft_dir)?;
    let draft_size_gb = gguf_path.metadata().ok()?.len() as f32 / (1024.0 * 1024.0 * 1024.0);
    let note = embedded_pairs()
        .ok()?
        .pair
        .into_iter()
        .find(|p| p.draft_id == draft_id)
        .map(|p| p.note)
        .unwrap_or_default();

    Some(DraftResolveContext {
        draft_id,
        gguf_path,
        draft_size_gb,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_pairs_parse() {
        let f = embedded_pairs().unwrap();
        assert!(!f.pair.is_empty());
    }

    #[test]
    fn qwen3_family_matches_qwen3_not_qwen35() {
        assert!(matches_target_family(
            "qwen3.x",
            "qwen3:8b:4bit",
            Some("unsloth/Qwen3-8B-GGUF")
        ));
        assert!(!matches_target_family(
            "qwen3.x",
            "qwen3.5:4b:6bit",
            Some("unsloth/Qwen3.5-4B-GGUF")
        ));
    }

    #[test]
    fn lookup_finds_qwen3_draft_for_qwen3_target() {
        let pair = lookup_draft_pair("qwen3:8b:4bit", Some("unsloth/Qwen3-8B-GGUF")).unwrap();
        assert_eq!(pair, "qwen3:0.6b:4bit");
    }

    #[test]
    fn broken_pair_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_pair_broken(dir.path(), "qwen3:8b:4bit", "qwen3:0.6b:4bit"));
        record_broken_pair(
            dir.path(),
            "qwen3:8b:4bit",
            "qwen3:0.6b:4bit",
            "synthetic tokenizer mismatch",
        )
        .unwrap();
        assert!(is_pair_broken(dir.path(), "qwen3:8b:4bit", "qwen3:0.6b:4bit"));
    }
}
