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

        let content = std::fs::read_to_string(&path).context("Failed to read models.json")?;
        let index: Self = serde_json::from_str(&content).context("Failed to parse models.json")?;

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
            if m.id == id {
                return true;
            }
            if let Some(repo) = &m.hf_repo
                && repo == id
            {
                return true;
            }
            if m.path.ends_with(&format!("/{}", id)) {
                return true;
            }
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
    if path.is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
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
    if let Ok(content) = std::fs::read_to_string(&gen_config_path)
        && let Ok(gc) = serde_json::from_str::<serde_json::Value>(&content)
        && gc["is_embedding"].as_bool() == Some(true)
    {
        caps.embeddings = true;
        caps.chat = false;
        debug!("Signal D: is_embedding=true in generation_config.json");
    }

    // =========================================================================
    // config.json — architecture + model_type analysis
    // =========================================================================
    let config_path = model_dir.join("config.json");
    if let Ok(content) = std::fs::read_to_string(&config_path)
        && let Ok(config) = serde_json::from_str::<serde_json::Value>(&content)
    {
        let model_type = config["model_type"].as_str().unwrap_or("").to_lowercase();
        let num_labels = config["num_labels"].as_u64().unwrap_or(0);

        // --- Re-ranker detection (highest priority — must precede embed check) ---
        // Signal 1: architectures array contains a sequence-classification head
        let is_seq_classifier = config["architectures"]
            .as_array()
            .map(|archs| {
                archs.iter().any(|a| {
                    a.as_str()
                        .unwrap_or("")
                        .contains("ForSequenceClassification")
                })
            })
            .unwrap_or(false);

        // num_labels == 1 → binary relevance score (cross-encoder re-ranker)
        if is_seq_classifier && num_labels == 1 {
            caps.reranking = true;
        }

        // --- Chat model detection ---
        // Generative re-rankers (Qwen3-Reranker etc.) intentionally set both flags so
        // llama.cpp can load them with --reranking while still being a decoder model.
        // Guard: do not set chat=true if embeddings was already forced by a high-priority
        // signal (Signal D: is_embedding=true in generation_config.json). That signal is
        // explicitly trusted and must not be overridden by architecture detection.
        if !caps.embeddings
            && [
                "qwen3",
                "qwen2",
                "llama",
                "mistral",
                "deepseek",
                "phi",
                "gemma",
                "granite",
                "starcoder",
                "falcon",
            ]
            .iter()
            .any(|t| model_type.contains(t))
        {
            caps.chat = true;
        }

        // --- Encoder-only embedding model detection (only when not a cross-encoder re-ranker) ---
        // These model types are unambiguously embedding-only (BERT / RoBERTa families).
        if !caps.reranking
            && ["nomic", "bert", "xlm-roberta", "roberta", "distilbert"]
                .iter()
                .any(|t| model_type.contains(t))
        {
            caps.embeddings = true;
            caps.chat = false; // pure embedding models do not support chat
        }

        // --- Embedding dimensions ---
        if caps.embeddings {
            if let Some(hidden_size) = config["hidden_size"].as_u64() {
                caps.embedding_dims = Some(hidden_size as u32);
            }
            if caps.embedding_dims.is_none()
                && let Some(d_model) = config["d_model"].as_u64()
            {
                caps.embedding_dims = Some(d_model as u32);
            }
        }

        // --- Pooling strategy ---
        if let Some(pt) = config["pooling_config"]["pooling_type"].as_str() {
            caps.pooling = Some(pt.to_lowercase());
        }
        if caps.pooling.is_none()
            && let Some(np) = config["nomic_embed_config"]["pooling"].as_str()
        {
            caps.pooling = Some(np.to_lowercase());
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
    } else if name_corpus.contains("embedding")
        || name_corpus.contains("-embed-")
        || name_corpus.contains("_embed_")
    {
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
            debug!(
                hint,
                "Signal A: catalog shortcut contains 'embed' — overriding to embedding model"
            );
        } else if hint_lower.contains("rerank") {
            caps.reranking = true;
            caps.embeddings = false;
            debug!(
                hint,
                "Signal A: catalog shortcut contains 'rerank' — overriding to re-ranker"
            );
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
        if let Ok(content) = std::fs::read_to_string(&tokenizer_config_path)
            && let Ok(tc) = serde_json::from_str::<serde_json::Value>(&content)
        {
            let has_chat_template = tc.get("chat_template").is_some();
            if !has_chat_template {
                caps.chat = false;
                debug!(
                    "Signal C: no chat_template in tokenizer_config.json — clearing chat=true for embedding model"
                );
            }
        }
    }

    // =========================================================================
    // Embedding dimensions — re-run after all signals so decoder-backbone embed
    // models (which set embedding_dims=None during config.json parsing because
    // caps.embeddings was false at that point) get their dims populated.
    // =========================================================================
    if caps.embeddings
        && caps.embedding_dims.is_none()
        && let Ok(content) = std::fs::read_to_string(&config_path)
        && let Ok(config) = serde_json::from_str::<serde_json::Value>(&content)
    {
        if let Some(hidden_size) = config["hidden_size"].as_u64() {
            caps.embedding_dims = Some(hidden_size as u32);
        }
        if caps.embedding_dims.is_none()
            && let Some(d_model) = config["d_model"].as_u64()
        {
            caps.embedding_dims = Some(d_model as u32);
        }
    }

    // =========================================================================
    // Thinking / reasoning detection (unchanged)
    // =========================================================================
    if let Ok(content) = std::fs::read_to_string(&tokenizer_config_path)
        && (content.contains("<think>")
            || content.contains("enable_thinking")
            || content.contains("reasoning_content"))
    {
        // Only set thinking on chat models — embedding models also ship with
        // Qwen3's thinking-capable tokenizer but cannot generate thought tokens.
        if caps.chat {
            caps.thinking = true;
        }
    }

    let jinja_path = model_dir.join("chat_template.jinja");
    if caps.chat
        && let Ok(content) = std::fs::read_to_string(&jinja_path)
        && (content.contains("<think>") || content.contains("enable_thinking"))
    {
        caps.thinking = true;
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
            capabilities: ModelCapabilities {
                chat: true,
                embeddings: false,
                reranking: false,
                thinking: false,
                embedding_dims: None,
                pooling: None,
            },
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
            capabilities: ModelCapabilities {
                chat: true,
                embeddings: false,
                reranking: false,
                thinking: false,
                embedding_dims: None,
                pooling: None,
            },
            added_at: "2024-01-01".to_string(),
        };

        index.add(entry.clone());
        index.add(ModelEntry {
            path: "/tmp/v2".to_string(),
            ..entry
        });

        assert_eq!(index.list().len(), 1);
        assert_eq!(index.get("test").unwrap().path, "/tmp/v2");
    }

    // ── detect_capabilities helpers ───────────────────────────────────────────

    fn make_model_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lmforge_caps_test_{}", name));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_json(dir: &std::path::Path, filename: &str, content: &str) {
        std::fs::write(dir.join(filename), content).unwrap();
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── Signal A: catalog shortcut overrides architecture ─────────────────────

    #[test]
    fn test_signal_a_embed_shortcut_overrides_decoder_arch() {
        // Qwen3-Embedding uses the same decoder arch as Qwen3-Chat.
        // Signal A (shortcut containing "embed") must force embeddings=true, chat=false.
        let dir = make_model_dir("signal_a_embed");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"],"hidden_size":1024}"#,
        );
        write_json(&dir, "tokenizer_config.json", r#"{}"#);

        let caps = detect_capabilities(&dir, Some("qwen3-embed:0.6b:4bit"), None);
        assert!(
            caps.embeddings,
            "Signal A: embed shortcut must set embeddings=true"
        );
        assert!(!caps.chat, "Signal A: embed shortcut must clear chat=false");
        assert!(
            !caps.reranking,
            "Embed model must not be flagged as reranker"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_signal_a_reranker_shortcut_overrides_decoder_arch() {
        // Qwen3-Reranker is also decoder-based. Signal A must identify it as a reranker.
        let dir = make_model_dir("signal_a_rerank");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"]}"#,
        );
        write_json(&dir, "tokenizer_config.json", r#"{}"#);

        let caps = detect_capabilities(&dir, Some("qwen3-reranker:0.6b:4bit"), None);
        assert!(
            caps.reranking,
            "Signal A: reranker shortcut must set reranking=true"
        );
        assert!(
            !caps.embeddings,
            "Reranker must not be flagged as embedding model"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_signal_a_chat_shortcut_does_not_trigger_embed() {
        // A plain chat shortcut (qwen3.5:4b:4bit) must not activate embed/rerank flags.
        let dir = make_model_dir("signal_a_chat");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"]}"#,
        );
        write_json(
            &dir,
            "tokenizer_config.json",
            r#"{"chat_template":"{% for m in messages %}{{m}}{% endfor %}"}"#,
        );

        let caps = detect_capabilities(&dir, Some("qwen3.5:4b:4bit"), None);
        assert!(caps.chat, "Chat shortcut + chat_template = chat model");
        assert!(
            !caps.embeddings,
            "Chat shortcut must not set embeddings=true"
        );
        assert!(!caps.reranking, "Chat shortcut must not set reranking=true");
        cleanup(&dir);
    }

    // ── Signal B: directory / repo name heuristic ─────────────────────────────

    #[test]
    fn test_signal_b_embedding_in_dir_name() {
        // GGUF models have no config.json. Name heuristic is the only signal.
        let dir = std::env::temp_dir().join("Qwen3-Embedding-0.6B");
        std::fs::create_dir_all(&dir).unwrap();

        let caps = detect_capabilities(&dir, None, Some("Qwen/Qwen3-Embedding-0.6B-GGUF"));
        assert!(
            caps.embeddings,
            "Signal B: 'Embedding' in repo name must set embeddings=true"
        );
        assert!(!caps.chat, "Signal B: embedding dir must clear chat=false");
        cleanup(&dir);
    }

    #[test]
    fn test_signal_b_reranker_in_repo_name() {
        let dir = make_model_dir("signal_b_rerank");
        // No config.json — GGUF scenario
        let caps = detect_capabilities(
            &dir,
            None,
            Some("jinaai/jina-reranker-v2-base-multilingual"),
        );
        assert!(
            caps.reranking,
            "Signal B: 'reranker' in repo must set reranking=true"
        );
        assert!(
            !caps.embeddings,
            "Signal B: reranker must not be flagged as embed"
        );
        cleanup(&dir);
    }

    // ── Signal C: no chat_template tiebreaker ─────────────────────────────────

    #[test]
    fn test_signal_c_no_chat_template_clears_chat_for_embed_model() {
        // Decoder arch (sets chat=true) + embed in repo (sets embed=true) → contradictory.
        // Signal C: no chat_template in tokenizer_config.json → clears chat=false.
        let dir = make_model_dir("signal_c_tiebreak");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"],"hidden_size":1024}"#,
        );
        write_json(&dir, "tokenizer_config.json", r#"{}"#); // no chat_template key

        let caps = detect_capabilities(&dir, None, Some("Qwen/Qwen3-Embedding-0.6B-GGUF"));
        assert!(caps.embeddings, "Signal B+C: repo name sets embed=true");
        assert!(
            !caps.chat,
            "Signal C: no chat_template must clear chat=false"
        );
        cleanup(&dir);
    }

    // ── Signal D: explicit is_embedding flag ──────────────────────────────────

    #[test]
    fn test_signal_d_is_embedding_flag() {
        let dir = make_model_dir("signal_d_is_embed");
        write_json(&dir, "generation_config.json", r#"{"is_embedding":true}"#);
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"],"hidden_size":768}"#,
        );

        let caps = detect_capabilities(&dir, None, None);
        assert!(
            caps.embeddings,
            "Signal D: is_embedding=true must set embeddings=true"
        );
        assert!(!caps.chat, "Signal D: is_embedding must clear chat=false");
        cleanup(&dir);
    }

    // ── Encoder-based reranker via config.json (Jina / BGE style) ────────────

    #[test]
    fn test_encoder_reranker_detected_from_config() {
        let dir = make_model_dir("encoder_reranker");
        write_json(
            &dir,
            "config.json",
            r#"{
            "model_type": "bert",
            "architectures": ["BertForSequenceClassification"],
            "num_labels": 1
        }"#,
        );

        let caps = detect_capabilities(&dir, None, None);
        assert!(
            caps.reranking,
            "ForSequenceClassification + num_labels=1 → reranker"
        );
        assert!(
            !caps.embeddings,
            "Reranker must not be flagged as embedding model"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_encoder_classifier_with_multiple_labels_is_not_reranker() {
        // num_labels > 1 = multi-class classifier, not a relevance reranker
        let dir = make_model_dir("multi_class_classifier");
        write_json(
            &dir,
            "config.json",
            r#"{
            "model_type": "bert",
            "architectures": ["BertForSequenceClassification"],
            "num_labels": 5
        }"#,
        );

        let caps = detect_capabilities(&dir, None, None);
        assert!(
            !caps.reranking,
            "num_labels=5 must not be flagged as re-ranker"
        );
        cleanup(&dir);
    }

    // ── Thinking token detection ──────────────────────────────────────────────

    #[test]
    fn test_thinking_detected_on_chat_model() {
        let dir = make_model_dir("thinking_chat");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"]}"#,
        );
        write_json(
            &dir,
            "tokenizer_config.json",
            r#"{"chat_template":"...<think>...</think>...","enable_thinking":true}"#,
        );

        let caps = detect_capabilities(&dir, Some("qwen3:8b:4bit"), None);
        assert!(caps.chat, "Thinking model must still be a chat model");
        assert!(
            caps.thinking,
            "<think> in tokenizer_config must set thinking=true"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_thinking_not_set_on_embedding_model() {
        // Qwen3-Embedding uses Qwen3's tokenizer (which has <think>) but is NOT a thinking model.
        let dir = make_model_dir("no_thinking_embed");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"],"hidden_size":1024}"#,
        );
        write_json(&dir, "tokenizer_config.json", r#"{"enable_thinking":true}"#); // Qwen3 tokenizer — no chat_template

        let caps = detect_capabilities(&dir, Some("qwen3-embed:0.6b:4bit"), None);
        assert!(caps.embeddings, "Must be embedding model");
        assert!(!caps.chat, "Must not be chat model");
        assert!(
            !caps.thinking,
            "Thinking must not be set on non-chat models"
        );
        cleanup(&dir);
    }

    // ── Embedding dims population ─────────────────────────────────────────────

    #[test]
    fn test_embedding_dims_populated_for_embed_model() {
        let dir = make_model_dir("embed_dims");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"],"hidden_size":1024}"#,
        );
        write_json(&dir, "tokenizer_config.json", r#"{}"#);

        let caps = detect_capabilities(&dir, Some("qwen3-embed:0.6b:4bit"), None);
        assert_eq!(
            caps.embedding_dims,
            Some(1024),
            "Embedding dims must be populated from hidden_size for embed models"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_embedding_dims_not_populated_for_chat_model() {
        let dir = make_model_dir("chat_no_dims");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"],"hidden_size":4096}"#,
        );
        write_json(
            &dir,
            "tokenizer_config.json",
            r#"{"chat_template":"{% for m in messages %}{{m}}{% endfor %}"}"#,
        );

        let caps = detect_capabilities(&dir, Some("qwen3.5:4b:4bit"), None);
        assert!(caps.chat, "Must be chat model");
        assert_eq!(
            caps.embedding_dims, None,
            "Embedding dims must not be set for chat models"
        );
        cleanup(&dir);
    }
}
