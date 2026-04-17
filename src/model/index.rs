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

/// Detect model capabilities from its metadata files plus optional catalog hints.
///
/// Detection signals (applied in order, later signals can override earlier ones):
///
/// **A — Catalog shortcut name** (`model_id_hint`):
///   The logical shortcut used to pull the model (e.g. `"qwen3-embed:0.6b:4bit"`).
///   If the shortcut or the resolved HF repo contains `"embed"/"embedding"`, stamp
///   `embeddings=true, chat=false` regardless of the on-disk architecture.
///   This is the most reliable signal because our catalog is the source of truth.
///
/// **B — Directory / HF repo name heuristic**:
///   Extends the existing reranker name check to also catch embedding models whose
///   directory or repo name contains `"embed"` or `"embedding"`.
///   Covers GGUF models and any HF repo whose architecture is shared with chat models.
///
/// **C — Negative chat signal: no `chat_template` in `tokenizer_config.json`**:
///   Decoder-backbone embedding models (Qwen3-Embedding, E5-Mistral, GTE-Qwen)
///   ship without a `chat_template` key in their tokenizer config. If a model matches
///   a generative architecture but has no `chat_template`, clear `chat=true`.
///   Pure embedding models never have a usable chat template.
///
/// **D — `is_embedding` field in `generation_config.json`**:
///   Forward-compatible: if a model sets `"is_embedding": true` we trust it
///   unconditionally. Qwen3-Embedding does not set this today, but future releases
///   and other embedding families may.
pub fn detect_capabilities(
    model_dir: &std::path::Path,
    model_id_hint: Option<&str>,
    hf_repo_hint: Option<&str>,
) -> ModelCapabilities {
    let mut caps = ModelCapabilities {
        chat: false,
        embeddings: false,
        reranking: false,
        thinking: false,
        embedding_dims: None,
        pooling: None,
    };

    // =========================================================================
    // Signal D — generation_config.json explicit is_embedding flag
    // Checked first: if the model itself says it's an embedding model we set the
    // flag early and later signals will only reinforce it.
    // =========================================================================
    let gen_config_path = model_dir.join("generation_config.json");
    if let Ok(content) = std::fs::read_to_string(&gen_config_path) {
        if let Ok(gc) = serde_json::from_str::<serde_json::Value>(&content) {
            if gc["is_embedding"].as_bool() == Some(true) {
                caps.embeddings = true;
                caps.chat = false;
                debug!("Signal D: is_embedding=true in generation_config.json");
            }
        }
    }

    // =========================================================================
    // config.json — architecture + model_type analysis
    // =========================================================================
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

            // --- Encoder-only embedding model detection (only when not a cross-encoder re-ranker) ---
            // These model types are unambiguously embedding-only (BERT / RoBERTa families).
            if !caps.reranking {
                if [
                    "nomic", "bert", "xlm-roberta", "roberta", "distilbert",
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
                if let Some(hidden_size) = config["hidden_size"].as_u64() {
                    caps.embedding_dims = Some(hidden_size as u32);
                }
                if caps.embedding_dims.is_none() {
                    if let Some(d_model) = config["d_model"].as_u64() {
                        caps.embedding_dims = Some(d_model as u32);
                    }
                }
            }

            // --- Pooling strategy ---
            if let Some(pt) = config["pooling_config"]["pooling_type"].as_str() {
                caps.pooling = Some(pt.to_lowercase());
            }
            if caps.pooling.is_none() {
                if let Some(np) = config["nomic_embed_config"]["pooling"].as_str() {
                    caps.pooling = Some(np.to_lowercase());
                }
            }
        }
    }

    // =========================================================================
    // Signal B — Directory name and HF repo name heuristic
    // Covers: GGUF models (no config.json), and decoder-backbone embed models
    // whose architecture is identical to the chat variant (Qwen3-Embedding,
    // E5-Mistral, GTE-Qwen, etc.).
    // =========================================================================
    let dir_lower = model_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    let repo_lower = hf_repo_hint.unwrap_or("").to_lowercase();

    // Combine dir name + repo into one string to search uniformly
    let name_corpus = format!("{} {}", dir_lower, repo_lower);

    if name_corpus.contains("reranker") || name_corpus.contains("rerank") {
        caps.reranking = true;
        caps.embeddings = false;
        debug!("Signal B: 'rerank(er)' in name — flagging as re-ranker");
    } else if name_corpus.contains("embedding") || name_corpus.contains("-embed-") || name_corpus.contains("_embed_") {
        // "embedding" is unambiguous ("Qwen3-Embedding", "nomic-embed-text")
        // Hyphen/underscore guards prevent false positives on repos like "snowflake-arctic-embeddings-arctic" etc.
        caps.embeddings = true;
        caps.chat = false;
        debug!("Signal B: 'embed(ding)' in name — flagging as embedding model");
    }

    // =========================================================================
    // Signal A — Catalog shortcut name override
    // The logical shortcut used to pull the model is our most reliable signal:
    // we curated the catalog ourselves and named shortcuts explicitly.
    // A shortcut like "qwen3-embed:0.6b:4bit" unambiguously means embedding.
    // =========================================================================
    if let Some(hint) = model_id_hint {
        let hint_lower = hint.to_lowercase();
        if hint_lower.contains("embed") {
            caps.embeddings = true;
            caps.chat = false;
            debug!(hint, "Signal A: catalog shortcut contains 'embed' — overriding to embedding model");
        } else if hint_lower.contains("rerank") {
            caps.reranking = true;
            caps.embeddings = false;
            debug!(hint, "Signal A: catalog shortcut contains 'rerank' — overriding to re-ranker");
        }
    }

    // =========================================================================
    // Signal C — Negative chat signal: no chat_template in tokenizer_config.json
    // If a model matched a generative architecture (e.g. "qwen3", "mistral") and
    // therefore got chat=true, but its tokenizer_config.json has no "chat_template"
    // key, it is almost certainly an embedding model — not a conversational one.
    // Embedding models that share a decoder backbone never include a usable template.
    //
    // Safety guard: only apply this downgrade when embeddings=true has already been
    // set by another signal, to prevent incorrectly clearing chat on decoder models
    // that legitimately lack a template (very rare but possible in custom GGUF pulls).
    // =========================================================================
    let tokenizer_config_path = model_dir.join("tokenizer_config.json");
    if caps.chat && caps.embeddings {
        // Contradictory state: architecture says chat, name says embed.
        // Use tokenizer_config as tiebreaker.
        if let Ok(content) = std::fs::read_to_string(&tokenizer_config_path) {
            if let Ok(tc) = serde_json::from_str::<serde_json::Value>(&content) {
                let has_chat_template = tc.get("chat_template").is_some();
                if !has_chat_template {
                    caps.chat = false;
                    debug!("Signal C: no chat_template in tokenizer_config.json — clearing chat=true for embedding model");
                }
            }
        }
    }

    // =========================================================================
    // Embedding dimensions — re-run after all signals so decoder-backbone embed
    // models (which set embedding_dims=None during config.json parsing because
    // caps.embeddings was false at that point) get their dims populated.
    // =========================================================================
    if caps.embeddings && caps.embedding_dims.is_none() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(hidden_size) = config["hidden_size"].as_u64() {
                    caps.embedding_dims = Some(hidden_size as u32);
                }
                if caps.embedding_dims.is_none() {
                    if let Some(d_model) = config["d_model"].as_u64() {
                        caps.embedding_dims = Some(d_model as u32);
                    }
                }
            }
        }
    }

    // =========================================================================
    // Thinking / reasoning detection (unchanged)
    // =========================================================================
    if let Ok(content) = std::fs::read_to_string(&tokenizer_config_path) {
        if content.contains("<think>")
            || content.contains("enable_thinking")
            || content.contains("reasoning_content")
        {
            // Only set thinking on chat models — embedding models also ship with
            // Qwen3's thinking-capable tokenizer but cannot generate thought tokens.
            if caps.chat {
                caps.thinking = true;
            }
        }
    }

    let jinja_path = model_dir.join("chat_template.jinja");
    if caps.chat {
        if let Ok(content) = std::fs::read_to_string(&jinja_path) {
            if content.contains("<think>") || content.contains("enable_thinking") {
                caps.thinking = true;
            }
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
            capabilities: ModelCapabilities { chat: true, embeddings: false, reranking: false, thinking: false, embedding_dims: None, pooling: None },
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
            capabilities: ModelCapabilities { chat: true, embeddings: false, reranking: false, thinking: false, embedding_dims: None, pooling: None },
            added_at: "2024-01-01".to_string(),
        };

        index.add(entry.clone());
        index.add(ModelEntry { path: "/tmp/v2".to_string(), ..entry });

        assert_eq!(index.list().len(), 1);
        assert_eq!(index.get("test").unwrap().path, "/tmp/v2");
    }
}
