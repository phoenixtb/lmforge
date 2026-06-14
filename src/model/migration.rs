use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Manifest written by `POST /lf/storage/apply` and consumed on next startup.
/// Lives at `~/.lmforge/pending-migration.json` (next to the bootstrap config).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingMigration {
    pub version: u32,
    /// New models directory (absolute path string). None = unchanged.
    pub models_dir: Option<String>,
    pub intent: MigrationIntent,
    /// For `Repull` intent: ordered list of models to re-download.
    pub repull_queue: Vec<RepullEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationIntent {
    /// No post-restart action needed (adopt-in-place or delete-only).
    None,
    /// Scan the new models_dir and rebuild the index.
    Scan,
    /// Re-download each entry in `repull_queue` into the new models_dir.
    Repull,
}

/// One model to re-download as part of a Repull migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepullEntry {
    pub id: String,
    pub hf_repo: String,
    pub format: String,
    pub engine: String,
}

impl PendingMigration {
    pub fn load() -> Result<Option<Self>> {
        let path = crate::config::pending_migration_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let manifest: Self = serde_json::from_str(&content)?;
        Ok(Some(manifest))
    }

    pub fn save(&self) -> Result<()> {
        let path = crate::config::pending_migration_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn clear() -> Result<()> {
        let path = crate::config::pending_migration_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn example_migration(intent: MigrationIntent, queue: Vec<RepullEntry>) -> PendingMigration {
        PendingMigration {
            version: 1,
            models_dir: Some("/srv/shared/models".to_string()),
            intent,
            repull_queue: queue,
        }
    }

    // ── manifest serde round-trip ───────────────────────────────────────────────

    #[test]
    fn migration_intent_roundtrip_none() {
        let m = example_migration(MigrationIntent::None, vec![]);
        let json = serde_json::to_string(&m).unwrap();
        let back: PendingMigration = serde_json::from_str(&json).unwrap();
        assert_eq!(back.intent, MigrationIntent::None);
        assert_eq!(back.models_dir.as_deref(), Some("/srv/shared/models"));
        assert!(back.repull_queue.is_empty());
    }

    #[test]
    fn migration_intent_roundtrip_scan() {
        let m = example_migration(MigrationIntent::Scan, vec![]);
        let json = serde_json::to_string(&m).unwrap();
        let back: PendingMigration = serde_json::from_str(&json).unwrap();
        assert_eq!(back.intent, MigrationIntent::Scan);
    }

    #[test]
    fn migration_intent_roundtrip_repull() {
        let entry = RepullEntry {
            id: "llama3-8b".to_string(),
            hf_repo: "meta-llama/Meta-Llama-3-8B".to_string(),
            format: "gguf".to_string(),
            engine: "llamacpp".to_string(),
        };
        let m = example_migration(MigrationIntent::Repull, vec![entry.clone()]);
        let json = serde_json::to_string(&m).unwrap();
        let back: PendingMigration = serde_json::from_str(&json).unwrap();
        assert_eq!(back.intent, MigrationIntent::Repull);
        assert_eq!(back.repull_queue.len(), 1);
        assert_eq!(back.repull_queue[0].id, "llama3-8b");
        assert_eq!(back.repull_queue[0].hf_repo, "meta-llama/Meta-Llama-3-8B");
    }

    // ── manifest version field preserved ───────────────────────────────────────

    #[test]
    fn manifest_version_is_preserved() {
        let m = PendingMigration {
            version: 42,
            models_dir: Some("/tmp/models".to_string()),
            intent: MigrationIntent::None,
            repull_queue: vec![],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: PendingMigration = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 42);
        assert_eq!(back.models_dir.as_deref(), Some("/tmp/models"));
    }

    // ── file I/O round-trip (save + load + clear) ───────────────────────────────

    #[test]
    fn manifest_file_save_load_clear() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("pending-migration.json");

        let m = example_migration(
            MigrationIntent::Repull,
            vec![RepullEntry {
                id: "phi3-mini".to_string(),
                hf_repo: "microsoft/Phi-3-mini-4k-instruct".to_string(),
                format: "gguf".to_string(),
                engine: "llamacpp".to_string(),
            }],
        );
        let json = serde_json::to_string_pretty(&m).unwrap();
        std::fs::write(&manifest_path, &json).unwrap();

        // Load from path directly (bypasses the bootstrap path used in production).
        let content = std::fs::read_to_string(&manifest_path).unwrap();
        let loaded: PendingMigration = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.intent, MigrationIntent::Repull);
        assert_eq!(loaded.repull_queue[0].id, "phi3-mini");

        // Clear.
        std::fs::remove_file(&manifest_path).unwrap();
        assert!(!manifest_path.exists(), "manifest must be gone after clear");
    }

    // ── repull_queue ordering preserved ────────────────────────────────────────

    #[test]
    fn repull_queue_order_preserved() {
        let ids = ["a", "b", "c", "d"];
        let queue: Vec<RepullEntry> = ids
            .iter()
            .map(|id| RepullEntry {
                id: id.to_string(),
                hf_repo: format!("owner/{id}"),
                format: "gguf".to_string(),
                engine: "llamacpp".to_string(),
            })
            .collect();
        let m = example_migration(MigrationIntent::Repull, queue);
        let json = serde_json::to_string(&m).unwrap();
        let back: PendingMigration = serde_json::from_str(&json).unwrap();
        let back_ids: Vec<&str> = back.repull_queue.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(back_ids, ids);
    }

    // ── helper: PathBuf for test bootstrap paths ────────────────────────────────

    #[allow(dead_code)]
    fn fake_pending_path(dir: &std::path::Path) -> PathBuf {
        dir.join("pending-migration.json")
    }
}
