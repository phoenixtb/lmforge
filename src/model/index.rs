use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// v2 stores per-model `path` (and `mmproj_path`) relative to `models_dir` so a
/// shared weights volume is portable across hosts/OSes with different mount
/// points. In memory we always expose absolute paths; relativization happens
/// only on save. v1 (absolute) indexes load fine and migrate to v2 on next save.
const SCHEMA_VERSION: u32 = 2;

/// Version of the capability *detector* (`detect_capabilities`), independent of
/// the on-disk `SCHEMA_VERSION`. Capabilities are persisted in `models.json` and
/// served verbatim (`GET /lf/model/list` never re-detects), so a detector fix in
/// a new build does **not** reach models pulled by an older build until they are
/// re-detected. Bumping this constant makes the daemon re-detect every model's
/// capabilities once on next start (see `cli::models::heal_capabilities_if_stale`),
/// so fixes self-heal without a manual `lmforge models scan`.
///
/// **Bump this by 1 whenever `detect_capabilities` output changes** (new signal,
/// changed classification, etc.). History:
///   * 1 — locked-thinking / dedicated-reasoning hint detection (GGUF `phi4:reasoning`).
pub const CAPS_DETECTOR_VERSION: u32 = 1;

/// The models.json index
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelIndex {
    pub schema_version: u32,
    pub models: Vec<ModelEntry>,
    /// Detector version that produced the persisted `capabilities`. Absent in
    /// indexes written before this field existed → deserializes to `0`, which is
    /// `< CAPS_DETECTOR_VERSION`, triggering a one-time self-heal re-detect.
    #[serde(default)]
    pub caps_detector_version: u32,
}

impl Default for ModelIndex {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            models: vec![],
            caps_detector_version: CAPS_DETECTOR_VERSION,
        }
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub chat: bool,
    pub embeddings: bool,
    #[serde(default)]
    pub reranking: bool,
    pub thinking: bool,
    /// True for vision-language models (VLMs) that accept image inputs in chat
    /// requests via OpenAI's `image_url` / `input_image` content blocks.
    /// Detected from `model_type` (qwen2_vl, llava, minicpmv, mllama, etc.) or
    /// from a catalog shortcut hint containing `:vl:` / `-vl-` / `vision`.
    #[serde(default)]
    pub vision: bool,
    /// Absolute path to the multimodal projector file (`mmproj-*.gguf`) for
    /// llama.cpp-served VLMs. None for engines that ship the projector inside
    /// the main weights (oMLX/MLX, SGLang/safetensors).
    #[serde(default)]
    pub mmproj_path: Option<String>,
    /// Output vector dimensionality — populated for embedding models.
    #[serde(default)]
    pub embedding_dims: Option<u32>,
    /// Pooling strategy detected from config.json ("mean" | "cls" | "last").
    /// Used by SGLang to set --pooling-method. None means engine default.
    #[serde(default)]
    pub pooling: Option<String>,
    /// Multi-Token Prediction (MTP / nextn) support — drives speculative
    /// decoding on `llama-server`. Resolved with a layered precedence:
    /// (1) catalog `mtp` flag, (2) `gguf_inspect::detect_mtp` probe at
    /// pull time. `None` means "unknown / not yet probed" — the launch
    /// path falls back to spec-dec OFF rather than guessing.
    #[serde(default)]
    pub mtp: Option<bool>,
    /// Turn-delimiter tokens detected from the chat template that mark the end
    /// of an assistant turn (e.g. Phi-4's `<|end|>`). Some models use a turn-end
    /// token that differs from their configured `eos_token`; oMLX only stops on
    /// eos, so without these as explicit `stop` sequences it runs past the turn
    /// end and regenerates. The server injects them as `stop` for oMLX when the
    /// client didn't supply its own.
    #[serde(default)]
    pub stop_tokens: Vec<String>,
    /// True for models that emit `reasoning_content` natively in a single engine
    /// call and do NOT respond to the `enable_thinking` chat-template flag
    /// (Phi-4-reasoning, DeepSeek-R1 distills). The two-call thinking-budget
    /// orchestrator — which prefills a synthetic `<think>…</think>` assistant
    /// turn and toggles `enable_thinking` — breaks these models (they hallucinate
    /// a new turn). The server keeps them on the single-call path.
    #[serde(default)]
    pub native_reasoning: bool,
}

impl ModelIndex {
    /// Load from file, or create empty.
    ///
    /// `models.json` lives in `data_dir`; per-model paths are resolved against
    /// `models_dir`. Relative entries (schema v2) become absolute in memory;
    /// absolute entries (schema v1, or weights outside `models_dir`) are kept
    /// verbatim so existing installs and foreign mounts keep working.
    pub fn load(data_dir: &std::path::Path, models_dir: &std::path::Path) -> Result<Self> {
        let path = data_dir.join("models.json");
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path).context("Failed to read models.json")?;
        let mut index: Self =
            serde_json::from_str(&content).context("Failed to parse models.json")?;

        for entry in &mut index.models {
            entry.path = resolve_model_path(&entry.path, models_dir);
            if let Some(mmproj) = entry.capabilities.mmproj_path.take() {
                entry.capabilities.mmproj_path = Some(resolve_model_path(&mmproj, models_dir));
            }
        }

        Ok(index)
    }

    /// Save atomically (write to temp, then rename).
    ///
    /// Paths are stored relative to `models_dir` (schema v2) when they live
    /// under it; weights outside `models_dir` are written absolute and logged.
    pub fn save(&self, data_dir: &std::path::Path, models_dir: &std::path::Path) -> Result<()> {
        let path = data_dir.join("models.json");
        let tmp_path = data_dir.join("models.json.tmp");

        // Relativize a clone for on-disk persistence; the in-memory index keeps
        // absolute paths so callers are unaffected. `caps_detector_version` is
        // preserved (not force-stamped) so a single-model pull/remove doesn't
        // mark the whole index as freshly detected — only a full re-detect
        // (`scan` / `heal_capabilities_if_stale`) advances it.
        let mut out = Self {
            schema_version: SCHEMA_VERSION,
            models: self.models.clone(),
            caps_detector_version: self.caps_detector_version,
        };
        for entry in &mut out.models {
            entry.path = relativize_model_path(&entry.path, models_dir);
            if let Some(mmproj) = entry.capabilities.mmproj_path.take() {
                entry.capabilities.mmproj_path = Some(relativize_model_path(&mmproj, models_dir));
            }
        }

        let json = serde_json::to_string_pretty(&out)?;
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, &path)?;

        debug!(path = %path.display(), count = out.models.len(), "Saved models.json");
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

/// Resolve a stored model path to absolute. Relative paths are joined onto
/// `models_dir`; absolute paths (v1 indexes or weights outside `models_dir`)
/// pass through unchanged.
fn resolve_model_path(stored: &str, models_dir: &std::path::Path) -> String {
    let p = std::path::Path::new(stored);
    if p.is_absolute() {
        stored.to_string()
    } else {
        models_dir.join(p).to_string_lossy().to_string()
    }
}

/// Inverse of [`resolve_model_path`]: strip the `models_dir` prefix to store a
/// portable relative path. Paths outside `models_dir` are kept absolute (and a
/// warning logged) so cross-OS shared volumes remain self-describing.
fn relativize_model_path(abs: &str, models_dir: &std::path::Path) -> String {
    let p = std::path::Path::new(abs);
    match p.strip_prefix(models_dir) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => {
            if p.is_absolute() {
                tracing::warn!(
                    path = %abs,
                    models_dir = %models_dir.display(),
                    "Model path is outside models_dir — keeping absolute in index (not portable across hosts)"
                );
            }
            abs.to_string()
        }
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

fn thinking_hint_corpus(model_id_hint: Option<&str>, hf_repo_hint: Option<&str>) -> String {
    format!(
        "{} {}",
        model_id_hint.unwrap_or(""),
        hf_repo_hint.unwrap_or("")
    )
    .to_lowercase()
}

/// True when the catalog shortcut / HF repo name identifies a *dedicated*
/// reasoning model — one that always emits reasoning and cannot be toggled off:
///   * `Qwen3-*-Thinking-2507` (`:thinking` shortcut / `-thinking` repo)
///   * `Phi-4-*-reasoning`      (`:reasoning` shortcut / `reasoning` repo)
///   * DeepSeek-R1 distills, QwQ
///
/// This is deliberately name-based: hybrid models (base Qwen3, pulled as
/// `qwen3:4b:4bit`) carry none of these markers and are classified toggleable
/// via the template's `enable_thinking` switch instead.
fn is_dedicated_reasoning_hint(hint: &str) -> bool {
    hint.contains(":thinking")
        || hint.contains("-thinking")
        || hint.contains("reasoning")
        || hint.contains("deepseek-r1")
        || hint.contains("-r1-")
        || hint.contains("qwq")
}

/// Catalog shortcuts for dedicated reasoning models. Runs late in detection so
/// GGUF-only dirs (Signal E) already have `chat=true` before we stamp thinking —
/// this is what makes `phi4:4b:reasoning:4bit` expose the toggle on the GGUF
/// path (Linux / Windows llama.cpp), not just on MLX.
fn apply_catalog_thinking_hints(caps: &mut ModelCapabilities, hint: &str) {
    if caps.embeddings || caps.reranking {
        return;
    }
    if is_dedicated_reasoning_hint(hint) {
        caps.chat = true;
        caps.thinking = true;
        caps.native_reasoning = true;
        debug!("Catalog hint dedicated-reasoning — always-on (locked) thinking model");
    }
}

/// Resolve `native_reasoning` (locked) vs toggleable after all thinking signals.
///
///   * Dedicated reasoning models (name marker)  → locked, regardless of template.
///   * Hybrid models (base Qwen3, etc.)          → toggleable iff the template
///     exposes the `enable_thinking` switch.
fn resolve_native_reasoning(caps: &mut ModelCapabilities, template: &str, hint: &str) {
    if !caps.thinking {
        caps.native_reasoning = false;
        return;
    }
    if is_dedicated_reasoning_hint(hint) {
        caps.native_reasoning = true;
        return;
    }
    // No name marker: a template that lacks the `enable_thinking` switch means
    // the model reasons in a single hardwired pass (locked); one that has it is
    // a hybrid the user can toggle.
    caps.native_reasoning = !template.contains("enable_thinking");
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
    let mut caps = ModelCapabilities::default();

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

        // --- Vision-language model (VLM) detection ---
        // VLMs declare a multimodal model_type in their config.json. We flag chat=true
        // here too because every VLM we ship is also a generative chat model — the
        // vision flag layers on top of chat. The list is intentionally explicit so
        // we don't accidentally flag every "vl"-named architecture.
        let vlm_model_types = [
            "qwen2_vl",
            "qwen2_5_vl",
            "qwen3_vl",
            "llava",
            "llava_next",
            "llava_onevision",
            "internvl",
            "internvl_chat",
            "minicpmv",
            "minicpm_v",
            "mllama",
            "phi3_v",
            "pixtral",
            "idefics",
            "idefics2",
            "idefics3",
        ];
        if vlm_model_types.iter().any(|t| model_type.contains(t)) {
            caps.vision = true;
            caps.chat = true;
            caps.embeddings = false;
            debug!(
                model_type,
                "VLM model_type detected — flagging vision=true, chat=true"
            );
        }

        // Architecture-array fallback: catches custom forks where model_type is generic
        // but the architecture name encodes the modality (e.g. *ForConditionalGeneration
        // containing 'vl' or 'vision'). We require *both* substrings to limit false hits.
        if !caps.vision
            && let Some(archs) = config["architectures"].as_array()
        {
            let arch_blob = archs
                .iter()
                .filter_map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            if arch_blob.contains("forconditionalgeneration")
                && (arch_blob.contains("vl") || arch_blob.contains("vision"))
            {
                caps.vision = true;
                caps.chat = true;
                caps.embeddings = false;
                debug!(
                    arch = %arch_blob,
                    "VLM architecture pattern detected — flagging vision=true"
                );
            }
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

    // Signal B-vision: vision-language model name heuristic.
    // Layered separately so VLM flags activate even on chat-typed catalog hints.
    // Skipped when the model has already been classified as embed/rerank.
    if !caps.embeddings
        && !caps.reranking
        && (name_corpus.contains("-vl-")
            || name_corpus.contains("_vl_")
            || name_corpus.ends_with("-vl")
            || name_corpus.contains("vision")
            || name_corpus.contains("llava")
            || name_corpus.contains("minicpm-v")
            || name_corpus.contains("minicpmv"))
    {
        caps.vision = true;
        caps.chat = true;
        debug!("Signal B-vision: VL/vision marker in name — flagging vision=true");
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
            // VL-embedding models stay vision-capable on the embedding side too.
            if hint_lower.contains(":vl:")
                || hint_lower.contains("-vl-")
                || hint_lower.contains("vision")
            {
                caps.vision = true;
            }
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
        } else if hint_lower.contains(":vl:")
            || hint_lower.contains("-vl-")
            || hint_lower.contains("vision")
            || hint_lower.contains("llava")
            || hint_lower.contains("minicpm-v")
            || hint_lower.contains("minicpmv")
        {
            // Catalog VLM shortcut: e.g. "qwen2.5-vl:7b:4bit", "minicpm-v:2.6:4bit"
            caps.vision = true;
            caps.chat = true;
            caps.embeddings = false;
            debug!(
                hint,
                "Signal A: catalog shortcut contains VL marker — flagging vision=true"
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

    // Reasoning models whose tokenizer/template lack the literal markers above
    // (e.g. Phi-4-mini-reasoning, DeepSeek-R1 distills) are handled in the
    // final catalog-hint pass after Signal E (GGUF chat promotion).

    // Turn-delimiter stop tokens. Some models terminate the assistant turn with
    // a token that is NOT their configured eos_token (Phi-4: turn end `<|end|>`
    // vs eos `<|endoftext|>`). oMLX only stops on eos, so it runs past the turn
    // end, self-prompts a new `<|assistant|>` turn and regenerates — duplicated
    // output + leaked special tokens. Detect the turn-end markers present in the
    // chat template so the server can pass them as `stop`.
    if caps.chat {
        let mut template = String::new();
        if let Ok(c) = std::fs::read_to_string(&tokenizer_config_path) {
            template.push_str(&c);
        }
        if let Ok(c) = std::fs::read_to_string(&jinja_path) {
            template.push_str(&c);
        }
        const TURN_END_MARKERS: &[&str] = &[
            "<|end|>",         // Phi-4
            "<|im_end|>",      // ChatML / Qwen
            "<|eot_id|>",      // Llama 3
            "<|end_of_turn|>", // some chat templates
            "<end_of_turn>",   // Gemma
        ];
        for m in TURN_END_MARKERS {
            if template.contains(m) && !caps.stop_tokens.iter().any(|s| s == m) {
                caps.stop_tokens.push((*m).to_string());
            }
        }
    }

    // =========================================================================
    // Signal E — GGUF chat fallback.
    //
    // GGUF model dirs typically contain just the `.gguf` weights file and no
    // `config.json`, so the architecture-based chat detection above never
    // fires. Without this fallback, every GGUF chat model would land here
    // with `chat=false` and the OpenAI API would refuse chat completions
    // ("Model 'foo' is an embedding model and cannot be used for chat
    // completions.") — see src/server/openai.rs::check_model_role.
    //
    // Rule: a model directory containing one or more `.gguf` weight files
    // (excluding `mmproj-*.gguf`) and no `config.json` is presumed to be a
    // chat model UNLESS earlier signals already classified it as embed or
    // rerank. This matches the curated catalog: gguf.json's `chat`,
    // `embed`, and `rerank` namespaces are explicit, and Signal A/B already
    // tagged non-chat shortcuts before we get here.
    // =========================================================================
    if !caps.chat
        && !caps.embeddings
        && !caps.reranking
        && !config_path.exists()
        && has_gguf_weights(model_dir)
    {
        caps.chat = true;
        debug!("Signal E: GGUF-only dir with no embed/rerank markers — defaulting chat=true");
    }

    // =========================================================================
    // Signal E-thinking — GGUF embedded chat-template reasoning probe.
    //
    // GGUF dirs carry no tokenizer_config.json / chat_template.jinja, so the
    // file-based thinking detection above never fires. The chat template is
    // embedded in GGUF metadata (`tokenizer.chat_template`). Probe it so the
    // Playground exposes the think toggle for GGUF reasoning models — notably
    // base Qwen3, a hybrid thinking model pulled as `qwen3:4b:4bit` (no
    // `:thinking` shortcut marker for the id-hint heuristic to catch).
    // =========================================================================
    if caps.chat
        && !caps.thinking
        && has_gguf_weights(model_dir)
        && let Some(tmpl) = crate::model::gguf_inspect::read_chat_template_for_model(model_dir)
        && (tmpl.contains("<think>")
            || tmpl.contains("enable_thinking")
            || tmpl.contains("reasoning_content"))
    {
        caps.thinking = true;
    }

    // Final thinking classification — catalog hints + native_reasoning resolution.
    // Must run after Signal E so GGUF-only models (Windows/llama.cpp) get thinking
    // flags from shortcuts like `phi4:4b:reasoning:4bit`.
    let hint = thinking_hint_corpus(model_id_hint, hf_repo_hint);
    apply_catalog_thinking_hints(&mut caps, &hint);

    let mut template_corpus = String::new();
    if let Ok(c) = std::fs::read_to_string(&tokenizer_config_path) {
        template_corpus.push_str(&c);
    }
    if let Ok(c) = std::fs::read_to_string(&jinja_path) {
        template_corpus.push_str(&c);
    }
    if has_gguf_weights(model_dir)
        && let Some(tmpl) = crate::model::gguf_inspect::read_chat_template_for_model(model_dir)
    {
        template_corpus.push_str(&tmpl);
    }
    resolve_native_reasoning(&mut caps, &template_corpus, &hint);

    // =========================================================================
    // VLM mmproj sidecar lookup (llama.cpp-served VLMs only).
    // Convention: `mmproj-*.gguf` next to the main weights file. If we already
    // know vision=true we look unconditionally; if vision is unset but a
    // projector is present we promote vision=true and chat=true.
    // =========================================================================
    let mmproj_path = find_mmproj_file(model_dir);
    if let Some(path) = mmproj_path {
        if !caps.vision {
            // mmproj presence is unambiguous evidence of a VLM
            caps.vision = true;
            if !caps.embeddings && !caps.reranking {
                caps.chat = true;
            }
            debug!(
                path = %path.display(),
                "mmproj sidecar found — flagging vision=true"
            );
        }
        caps.mmproj_path = Some(path.to_string_lossy().to_string());
    }

    caps
}

/// True when `model_dir` contains at least one `.gguf` weights file that is
/// *not* a multimodal projector sidecar. Used by the GGUF chat fallback in
/// `detect_capabilities`.
fn has_gguf_weights(model_dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(model_dir) else {
        return false;
    };
    entries.filter_map(|e| e.ok()).any(|e| {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("gguf") {
            return false;
        }
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| !n.starts_with("mmproj-"))
            .unwrap_or(false)
    })
}

/// Find the multimodal projector sidecar (`mmproj-*.gguf`) in the given dir.
/// Returns the first match (sorted lexicographically for deterministic results).
fn find_mmproj_file(model_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut matches: Vec<std::path::PathBuf> = std::fs::read_dir(model_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("mmproj-") && n.ends_with(".gguf"))
                    .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── GGUF chat fallback (Signal E) ────────────────────────────────────────
    //
    // Regression guard: every GGUF chat model in the curated catalog must be
    // classified as chat=true even though it ships with no config.json. If
    // this breaks, /v1/chat/completions starts 400ing with "is an embedding
    // model and cannot be used for chat completions."

    fn make_gguf_dir(name: &str, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lmforge_test_caps_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (fname, body) in files {
            std::fs::write(dir.join(fname), body).unwrap();
        }
        dir
    }

    #[test]
    fn gguf_only_dir_defaults_to_chat() {
        let dir = make_gguf_dir("gguf_chat", &[("Qwen3-1.7B-Q4_K_M.gguf", b"fake-weights")]);
        let caps = detect_capabilities(
            &dir,
            Some("qwen3:1.7b:4bit"),
            Some("unsloth/Qwen3-1.7B-GGUF"),
        );
        assert!(
            caps.chat,
            "GGUF model without embed/rerank markers must default chat=true"
        );
        assert!(!caps.embeddings);
        assert!(!caps.reranking);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gguf_embed_dir_stays_embed() {
        let dir = make_gguf_dir(
            "gguf_embed",
            &[("Qwen3-Embedding-0.6B-Q8_0.gguf", b"fake-weights")],
        );
        let caps = detect_capabilities(
            &dir,
            Some("qwen3-embed:0.6b:8bit"),
            Some("Qwen/Qwen3-Embedding-0.6B-GGUF"),
        );
        assert!(
            caps.embeddings,
            "Catalog 'embed' shortcut must keep embeddings=true"
        );
        assert!(!caps.chat, "Embedding model must not be flagged as chat");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gguf_rerank_dir_stays_rerank() {
        let dir = make_gguf_dir(
            "gguf_rerank",
            &[("Qwen3-Reranker-4B-Q4_K_M.gguf", b"fake-weights")],
        );
        let caps = detect_capabilities(
            &dir,
            Some("qwen3-rerank:4b:4bit"),
            Some("Qwen/Qwen3-Reranker-4B-GGUF"),
        );
        assert!(caps.reranking);
        assert!(!caps.chat, "Re-ranker must not be flagged as chat");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn safetensors_dir_with_no_config_does_not_get_chat_fallback() {
        // The fallback is GGUF-only — a stray safetensors dir without
        // config.json (broken state) must NOT get chat=true.
        let dir = make_gguf_dir(
            "safetensors_no_config",
            &[("model.safetensors", b"fake-weights")],
        );
        let caps = detect_capabilities(&dir, Some("some-model:latest"), None);
        assert!(
            !caps.chat,
            "Safetensors-only dir without config.json must not be flagged chat"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn has_gguf_weights_distinguishes_main_from_mmproj() {
        let dir = make_gguf_dir(
            "gguf_weights_probe",
            &[
                ("Qwen2.5-VL-7B-Q4_K_M.gguf", b"fake-weights"),
                ("mmproj-Qwen2.5-VL-7B-f16.gguf", b"fake-proj"),
            ],
        );
        assert!(has_gguf_weights(&dir));
        std::fs::remove_dir_all(&dir).unwrap();

        let dir = make_gguf_dir("gguf_no_weights", &[]);
        assert!(!has_gguf_weights(&dir));
        std::fs::remove_dir_all(&dir).unwrap();

        let dir = make_gguf_dir(
            "gguf_only_mmproj",
            &[("mmproj-something-f16.gguf", b"fake-proj")],
        );
        assert!(!has_gguf_weights(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ── Relative-path index (schema v2) round-trip + migration ───────────────

    fn make_index_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lmforge_index_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_entry(id: &str, path: &str) -> ModelEntry {
        ModelEntry {
            id: id.to_string(),
            path: path.to_string(),
            format: "gguf".to_string(),
            engine: "gguf".to_string(),
            hf_repo: Some("org/repo".to_string()),
            size_bytes: 10,
            capabilities: ModelCapabilities {
                chat: true,
                ..Default::default()
            },
            added_at: "2025-01-01".to_string(),
        }
    }

    #[test]
    fn index_v2_round_trip_stores_relative_resolves_absolute() {
        let data_dir = make_index_test_dir("v2_roundtrip_data");
        let models_dir = make_index_test_dir("v2_roundtrip_models");
        let abs = models_dir.join("qwen3-8b").to_string_lossy().to_string();

        let idx = ModelIndex {
            schema_version: 2,
            models: vec![sample_entry("qwen3:8b", &abs)],
            ..Default::default()
        };
        idx.save(&data_dir, &models_dir).unwrap();

        // On disk the path must be relative to models_dir.
        let raw = std::fs::read_to_string(data_dir.join("models.json")).unwrap();
        assert!(
            raw.contains("\"qwen3-8b\""),
            "expected relative path on disk: {raw}"
        );
        assert!(!raw.contains(&abs), "absolute path must not be persisted");

        // On load it resolves back to the absolute path under models_dir.
        let loaded = ModelIndex::load(&data_dir, &models_dir).unwrap();
        assert_eq!(loaded.get("qwen3:8b").unwrap().path, abs);

        std::fs::remove_dir_all(&data_dir).ok();
        std::fs::remove_dir_all(&models_dir).ok();
    }

    #[test]
    fn index_v1_absolute_migrates_to_relative_on_save() {
        let data_dir = make_index_test_dir("v1_migrate_data");
        let models_dir = make_index_test_dir("v1_migrate_models");
        let abs = models_dir.join("gemma3-4b").to_string_lossy().to_string();

        // Simulate a v1 index file with an absolute path.
        let v1 = format!(
            r#"{{"schema_version":1,"models":[{{"id":"gemma3:4b","path":"{}","format":"gguf","engine":"gguf","hf_repo":null,"size_bytes":1,"capabilities":{{"chat":true,"embeddings":false,"thinking":false}},"added_at":"x"}}]}}"#,
            abs.replace('\\', "\\\\")
        );
        std::fs::write(data_dir.join("models.json"), v1).unwrap();

        // Load keeps it absolute in memory.
        let idx = ModelIndex::load(&data_dir, &models_dir).unwrap();
        assert_eq!(idx.get("gemma3:4b").unwrap().path, abs);

        // Saving migrates to v2 relative.
        idx.save(&data_dir, &models_dir).unwrap();
        let raw = std::fs::read_to_string(data_dir.join("models.json")).unwrap();
        assert!(raw.contains("\"schema_version\": 2"), "must be v2: {raw}");
        assert!(raw.contains("\"gemma3-4b\""), "must be relative: {raw}");

        std::fs::remove_dir_all(&data_dir).ok();
        std::fs::remove_dir_all(&models_dir).ok();
    }

    #[test]
    fn index_foreign_path_kept_absolute() {
        let data_dir = make_index_test_dir("foreign_data");
        let models_dir = make_index_test_dir("foreign_models");
        // Must be absolute on the host platform: "/some/..." is drive-relative
        // on Windows and would get resolved against the current drive.
        let foreign = if cfg!(windows) {
            "Z:/some/other/volume/llama-3".to_string()
        } else {
            "/some/other/volume/llama-3".to_string()
        };

        let idx = ModelIndex {
            schema_version: 2,
            models: vec![sample_entry("llama3", &foreign)],
            ..Default::default()
        };
        idx.save(&data_dir, &models_dir).unwrap();

        // Outside models_dir → kept absolute on disk (portable across hosts only
        // when standardized, but never silently broken).
        let raw = std::fs::read_to_string(data_dir.join("models.json")).unwrap();
        assert!(
            raw.contains(&foreign),
            "foreign abs path must be preserved: {raw}"
        );

        let loaded = ModelIndex::load(&data_dir, &models_dir).unwrap();
        assert_eq!(loaded.get("llama3").unwrap().path, foreign);

        std::fs::remove_dir_all(&data_dir).ok();
        std::fs::remove_dir_all(&models_dir).ok();
    }

    #[test]
    fn test_index_crud() {
        let mut index = ModelIndex {
            schema_version: 1,
            models: vec![],
            ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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
                ..Default::default()
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
        assert!(
            !caps.native_reasoning,
            "enable_thinking in template must keep model toggleable"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_qwen3_thinking_2507_shortcut_is_locked_native() {
        // Qwen3-4B-Thinking-2507 is a dedicated always-thinking model: the
        // `:thinking` shortcut must lock the toggle on (native_reasoning=true).
        let dir = make_model_dir("qwen3_thinking_shortcut");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"]}"#,
        );
        write_json(
            &dir,
            "tokenizer_config.json",
            r#"{"chat_template":"...<think>...</think>..."}"#,
        );

        let caps = detect_capabilities(&dir, Some("qwen3:4b:thinking:4bit"), None);
        assert!(caps.thinking, ":thinking shortcut must expose thinking");
        assert!(
            caps.native_reasoning,
            "dedicated Thinking-2507 must be locked always-on"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_qwen3_hybrid_base_is_toggleable() {
        // Base Qwen3 (hybrid) has the enable_thinking switch and no dedicated
        // name marker → toggleable (native_reasoning=false).
        let dir = make_model_dir("qwen3_hybrid_base");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"]}"#,
        );
        write_json(
            &dir,
            "tokenizer_config.json",
            r#"{"chat_template":"...{% if enable_thinking %}<think>{% endif %}..."}"#,
        );

        let caps = detect_capabilities(&dir, Some("qwen3:4b:4bit"), None);
        assert!(caps.thinking, "hybrid base must expose thinking");
        assert!(
            !caps.native_reasoning,
            "hybrid base with enable_thinking must stay toggleable"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_phi_reasoning_gguf_shortcut_is_native_after_signal_e() {
        let dir = make_gguf_dir(
            "phi_reasoning_gguf",
            &[("Phi-4-mini-reasoning-Q4_K_M.gguf", b"fake-weights")],
        );
        let caps = detect_capabilities(
            &dir,
            Some("phi4:4b:reasoning:4bit"),
            Some("unsloth/Phi-4-mini-reasoning-GGUF"),
        );
        assert!(caps.chat, "GGUF chat model");
        assert!(caps.thinking, "reasoning shortcut must expose think toggle");
        assert!(
            caps.native_reasoning,
            "phi reasoning must be locked always-on"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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

    // ── VLM (vision) detection ────────────────────────────────────────────────

    #[test]
    fn test_vlm_detection_qwen2_5_vl_from_config() {
        let dir = make_model_dir("vlm_qwen25_config");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen2_5_vl","architectures":["Qwen2_5_VLForConditionalGeneration"]}"#,
        );
        write_json(
            &dir,
            "tokenizer_config.json",
            r#"{"chat_template":"{% for m in messages %}{{m}}{% endfor %}"}"#,
        );

        let caps = detect_capabilities(&dir, None, None);
        assert!(caps.vision, "qwen2_5_vl model_type must set vision=true");
        assert!(caps.chat, "VLM must also be chat=true");
        assert!(!caps.embeddings, "VLM must not be flagged as embedding");
        cleanup(&dir);
    }

    #[test]
    fn test_vlm_detection_minicpmv_from_config() {
        let dir = make_model_dir("vlm_minicpmv");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"minicpmv","architectures":["MiniCPMV"]}"#,
        );
        let caps = detect_capabilities(&dir, None, None);
        assert!(caps.vision, "minicpmv must set vision=true");
        assert!(caps.chat);
        cleanup(&dir);
    }

    #[test]
    fn test_vlm_detection_llava_from_config() {
        let dir = make_model_dir("vlm_llava");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"llava_next","architectures":["LlavaNextForConditionalGeneration"]}"#,
        );
        let caps = detect_capabilities(&dir, None, None);
        assert!(caps.vision);
        assert!(caps.chat);
        cleanup(&dir);
    }

    #[test]
    fn test_vlm_detection_mllama_from_config() {
        let dir = make_model_dir("vlm_mllama");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"mllama","architectures":["MllamaForConditionalGeneration"]}"#,
        );
        let caps = detect_capabilities(&dir, None, None);
        assert!(caps.vision);
        assert!(caps.chat);
        cleanup(&dir);
    }

    #[test]
    fn test_vlm_detection_via_catalog_hint_only() {
        // Path: GGUF VLM with no config.json — must still detect from catalog hint.
        let dir = make_model_dir("vlm_catalog_hint");
        let caps = detect_capabilities(
            &dir,
            Some("qwen2.5-vl:7b:4bit"),
            Some("bartowski/Qwen2.5-VL-7B-Instruct-GGUF"),
        );
        assert!(caps.vision, "Catalog VL hint must set vision=true");
        assert!(caps.chat, "Catalog VL hint must set chat=true");
        assert!(!caps.embeddings, "Catalog VL hint must not set embeddings");
        cleanup(&dir);
    }

    #[test]
    fn test_vlm_architecture_fallback_pattern() {
        // model_type empty/generic but architecture name carries the modality marker.
        let dir = make_model_dir("vlm_arch_fallback");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"custom","architectures":["MyCustomVLForConditionalGeneration"]}"#,
        );
        let caps = detect_capabilities(&dir, None, None);
        assert!(
            caps.vision,
            "Architecture pattern *VLForConditionalGeneration must set vision=true"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_vl_embedding_keeps_vision_flag() {
        // Qwen3-VL-Embedding: VL embedder, not chat.
        let dir = make_model_dir("vl_embed");
        let caps = detect_capabilities(
            &dir,
            Some("qwen3-vl-embed:2b:4bit"),
            Some("mlx-community/Qwen3-VL-Embedding-2B-4bit"),
        );
        assert!(caps.embeddings, "VL embedding must be embeddings=true");
        assert!(!caps.chat, "VL embedding must not be chat");
        assert!(
            caps.vision,
            "VL embedding must keep vision=true for image inputs"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_mmproj_sidecar_promotes_vision_and_sets_path() {
        // GGUF VLM scenario: mmproj sidecar present alongside main weights.
        let dir = make_model_dir("vlm_mmproj_promote");
        // No config.json, no catalog hint — mmproj alone must be sufficient.
        std::fs::write(dir.join("Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"), b"weights").unwrap();
        std::fs::write(dir.join("mmproj-Qwen2.5-VL-7B-Instruct-f16.gguf"), b"proj").unwrap();

        let caps = detect_capabilities(&dir, None, None);
        assert!(caps.vision, "mmproj sidecar must promote vision=true");
        assert!(caps.chat, "mmproj sidecar must promote chat=true");
        let mmproj = caps.mmproj_path.expect("mmproj_path must be set");
        assert!(
            mmproj.ends_with("mmproj-Qwen2.5-VL-7B-Instruct-f16.gguf"),
            "mmproj_path was: {mmproj}"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_mmproj_path_set_when_vision_already_known() {
        // Catalog hint already sets vision; mmproj_path must still be populated.
        let dir = make_model_dir("vlm_mmproj_known");
        std::fs::write(dir.join("mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"), b"proj").unwrap();

        let caps = detect_capabilities(&dir, Some("qwen2.5-vl:3b:4bit"), None);
        assert!(caps.vision);
        assert!(caps.mmproj_path.is_some());
        cleanup(&dir);
    }

    #[test]
    fn test_no_mmproj_does_not_set_vision() {
        let dir = make_model_dir("no_mmproj");
        std::fs::write(dir.join("Qwen3-8B-Q4_K_M.gguf"), b"weights").unwrap();
        let caps = detect_capabilities(&dir, Some("qwen3:8b:4bit"), None);
        assert!(!caps.vision);
        assert!(caps.mmproj_path.is_none());
        cleanup(&dir);
    }

    #[test]
    fn test_chat_model_does_not_trigger_vision() {
        // Use a benign dir name so Signal B-vision doesn't false-trigger on the
        // test scaffold path itself (production dirs are e.g. `Qwen3-8B-4bit`).
        let dir = make_model_dir("plain_chat_8b");
        write_json(
            &dir,
            "config.json",
            r#"{"model_type":"qwen3","architectures":["Qwen3ForCausalLM"]}"#,
        );
        write_json(&dir, "tokenizer_config.json", r#"{"chat_template":"..."}"#);
        let caps = detect_capabilities(&dir, Some("qwen3.5:4b:4bit"), None);
        assert!(caps.chat);
        assert!(!caps.vision, "Plain chat model must not set vision");
        assert!(caps.mmproj_path.is_none());
        cleanup(&dir);
    }
}
