use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

const SCHEMA_VERSION: u32 = 1;

/// The models.json index
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelIndex {
    pub schema_version: u32,
    pub models: Vec<ModelEntry>,
}

/// A single model entry in the index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub path: String,
    pub format: String,
    pub engine: String,
    pub hf_repo: Option<String>,
    pub size_bytes: u64,
    pub capabilities: ModelCapabilities,
    pub added_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub chat: bool,
    pub embeddings: bool,
    #[serde(default)]
    pub reranking: bool,
    pub thinking: bool,
    /// Output vector dimensionality — populated for embedding models.
    #[serde(default)]
    pub embedding_dims: Option<u32>,
    /// Pooling strategy detected from config.json ("mean" | "cls" | "last").
    /// Used by SGLang to set --pooling-method. None means engine default.
    #[serde(default)]
    pub pooling: Option<String>,
}

impl ModelIndex {
    /// Load from file, or create empty
    pub fn load(data_dir: &std::path::Path) -> Result<Self> {
        let path = data_dir.join("models.json");
        if !path.exists() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                models: vec![],
            });
        }

        let content = std::fs::read_to_string(&path)
            .context("Failed to read models.json")?;
        let index: Self = serde_json::from_str(&content)
            .context("Failed to parse models.json")?;

        Ok(index)
    }

    /// Save atomically (write to temp, then rename)
    pub fn save(&self, data_dir: &std::path::Path) -> Result<()> {
        let path = data_dir.join("models.json");
        let tmp_path = data_dir.join("models.json.tmp");

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, &path)?;

        debug!(path = %path.display(), count = self.models.len(), "Saved models.json");
        Ok(())
    }

    /// Add a model entry (replace if same ID exists)
    pub fn add(&mut self, entry: ModelEntry) {
        self.models.retain(|m| m.id != entry.id);
        info!(id = %entry.id, format = %entry.format, "Model added to index");
        self.models.push(entry);
    }

    /// Remove a model by ID
    pub fn remove(&mut self, id: &str) -> Option<ModelEntry> {
        if let Some(pos) = self.models.iter().position(|m| m.id == id) {
            let entry = self.models.remove(pos);
            info!(id, "Model removed from index");
            Some(entry)
        } else {
            None
        }
    }

    /// Get a model by ID (with fallback to hf_repo or dir boundary name)
    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| {
            if m.id == id { return true; }
            if let Some(repo) = &m.hf_repo {
                if repo == id { return true; }
            }
            if m.path.ends_with(&format!("/{}", id)) { return true; }
            false
        })
    }

    /// List all models
    pub fn list(&self) -> &[ModelEntry] {
        &self.models
    }

    /// Get the first available model (for default selection)
    pub fn first(&self) -> Option<&ModelEntry> {
        self.models.first()
    }
}

/// Calculate directory size recursively
pub fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    total += dir_size(&p);
                } else if let Ok(meta) = p.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Detect model capabilities from its metadata files.
///
/// Detection strategy (applied in order, results accumulate):
///
/// **Chat**: model_type matches known generative families.
/// **Re-ranker**: `architectures` contains `ForSequenceClassification` with `num_labels == 1`,
///   OR the model directory name contains "reranker"/"rerank" (catches GGUF and generative re-rankers).
/// **Embedding**: model_type matches retrieval families AND it is NOT a re-ranker.
/// **Dims/Pooling**: parsed from `hidden_size` / `pooling_config` in config.json.
pub fn detect_capabilities(model_dir: &std::path::Path) -> ModelCapabilities {
    let mut caps = ModelCapabilities {
        chat: false,
        embeddings: false,
        reranking: false,
        thinking: false,
        embedding_dims: None,
        pooling: None,
    };

    let config_path = model_dir.join("config.json");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
            let model_type = config["model_type"].as_str().unwrap_or("").to_lowercase();
            let num_labels  = config["num_labels"].as_u64().unwrap_or(0);

            // --- Re-ranker detection (highest priority — must precede embed check) ---
            // Signal 1: architectures array contains a sequence-classification head
            let is_seq_classifier = config["architectures"]
                .as_array()
                .map(|archs| archs.iter().any(|a| {
                    a.as_str().unwrap_or("").contains("ForSequenceClassification")
                }))
                .unwrap_or(false);

            // num_labels == 1 → binary relevance score (cross-encoder re-ranker)
            if is_seq_classifier && num_labels == 1 {
                caps.reranking = true;
            }

            // --- Chat model detection ---
            // Generative re-rankers (Qwen3-Reranker etc.) intentionally set both flags so
            // llama.cpp can load them with --reranking while still being a decoder model.
            if [
                "qwen3", "qwen2", "llama", "mistral", "deepseek",
                "phi", "gemma", "granite", "starcoder", "falcon",
            ]
            .iter()
            .any(|t| model_type.contains(t))
            {
                caps.chat = true;
            }

            // --- Embedding model detection (only when not a cross-encoder re-ranker) ---
            if !caps.reranking {
                if [
                    "nomic", "bert", "embed", "gte", "e5",
                    "xlm-roberta", "roberta", "distilbert",
                ]
                .iter()
                .any(|t| model_type.contains(t))
                {
                    caps.embeddings = true;
                    caps.chat = false; // pure embedding models do not support chat
                }
            }

            // --- Embedding dimensions ---
            if caps.embeddings {
                // Standard field
                if let Some(hidden_size) = config["hidden_size"].as_u64() {
                    caps.embedding_dims = Some(hidden_size as u32);
                }
                // Nomic models use d_model instead
                if caps.embedding_dims.is_none() {
                    if let Some(d_model) = config["d_model"].as_u64() {
                        caps.embedding_dims = Some(d_model as u32);
                    }
                }
            }

            // --- Pooling strategy ---
            // Standard pooling_config (e.g. sentence-transformers, GTE)
            if let Some(pt) = config["pooling_config"]["pooling_type"].as_str() {
                caps.pooling = Some(pt.to_lowercase());
            }
            // Nomic models use a separate config block
            if caps.pooling.is_none() {
                if let Some(np) = config["nomic_embed_config"]["pooling"].as_str() {
                    caps.pooling = Some(np.to_lowercase());
                }
            }
        }
    }

    // --- Name heuristic (covers GGUF and generative re-rankers lacking config.json) ---
    if let Some(dir_name) = model_dir.file_name().and_then(|n| n.to_str()) {
        let lower = dir_name.to_lowercase();
        if lower.contains("reranker") || lower.contains("rerank") {
            caps.reranking = true;
            caps.embeddings = false;
            // Generative re-rankers (Qwen3-Reranker) are decoder models so caps.chat stays true
            // if it was already set; pure cross-encoders won't have chat set.
        }
    }

    // --- Thinking / reasoning detection ---
    let template_path = model_dir.join("tokenizer_config.json");
    if let Ok(content) = std::fs::read_to_string(&template_path) {
        if content.contains("<think>")
            || content.contains("enable_thinking")
            || content.contains("reasoning_content")
        {
            caps.thinking = true;
        }
    }

    let jinja_path = model_dir.join("chat_template.jinja");
    if let Ok(content) = std::fs::read_to_string(&jinja_path) {
        if content.contains("<think>") || content.contains("enable_thinking") {
            caps.thinking = true;
        }
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_crud() {
        let mut index = ModelIndex {
            schema_version: 1,
            models: vec![],
        };

        assert_eq!(index.list().len(), 0);

        index.add(ModelEntry {
            id: "test".to_string(),
            path: "/tmp/test".to_string(),
            format: "mlx".to_string(),
            engine: "omlx".to_string(),
            hf_repo: Some("test/test".to_string()),
            size_bytes: 1000,
            capabilities: ModelCapabilities { chat: true, embeddings: false, thinking: false },
            added_at: "2024-01-01".to_string(),
        });

        assert_eq!(index.list().len(), 1);
        assert!(index.get("test").is_some());
        assert!(index.get("nonexistent").is_none());

        let removed = index.remove("test");
        assert!(removed.is_some());
        assert_eq!(index.list().len(), 0);
    }

    #[test]
    fn test_index_add_replaces_duplicate() {
        let mut index = ModelIndex {
            schema_version: 1,
            models: vec![],
        };

        let entry = ModelEntry {
            id: "test".to_string(),
            path: "/tmp/v1".to_string(),
            format: "mlx".to_string(),
            engine: "omlx".to_string(),
            hf_repo: None,
            size_bytes: 1000,
            capabilities: ModelCapabilities { chat: true, embeddings: false, thinking: false },
            added_at: "2024-01-01".to_string(),
        };

        index.add(entry.clone());
        index.add(ModelEntry { path: "/tmp/v2".to_string(), ..entry });

        assert_eq!(index.list().len(), 1);
        assert_eq!(index.get("test").unwrap().path, "/tmp/v2");
    }
}
