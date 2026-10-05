//! Re-ranker contract: what a GGUF must carry to be served by llama.cpp
//! `--reranking`, and how each engine's raw score maps onto the relevance
//! probability `/v1/rerank` returns.
//!
//! Calibration is decided per engine + model family, never from value ranges:
//!
//! | Engine | Model family (GGUF evidence) | Engine returns | LMForge applies |
//! |---|---|---|---|
//! | llama.cpp | Qwen3 / Qwen3-VL reranker (`cls.output.weight` = 2 rows, yes/no) | softmax → P(yes) | nothing |
//! | llama.cpp | BERT-style cross-encoder — bge, jina (`cls.output.weight` = 1 row) | raw logit | sigmoid |
//! | oMLX | every reranker it serves | probability (yes/no softmax, or sigmoid inside the model) | nothing |
//!
//! The llama.cpp rows mirror b9861 `src/llama-graph.cpp` (`build_pooling`,
//! RANK case): the softmax is applied for the `qwen3`/`qwen3vl` architectures
//! only, and llama-server returns element 0 of the pooled output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use crate::model::gguf_inspect::{RerankHeadInfo, largest_gguf_in_dir, read_rerank_head};

/// llama.cpp `LLAMA_POOLING_TYPE_RANK`.
const POOLING_TYPE_RANK: u64 = 4;

/// Architectures whose RANK pooling ends in a softmax over the classifier rows.
const SOFTMAX_RANK_ARCHS: &[&str] = &["qwen3", "qwen3vl"];

/// Scale of the raw score an engine returns for one query/document pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKind {
    /// Already a relevance probability in [0, 1].
    Probability,
    /// Unbounded logit; the sigmoid maps it to a probability.
    Logit,
}

impl ScoreKind {
    /// Map a raw engine score to a probability in [0, 1]. Out-of-range input
    /// (e.g. llama-server's `-1e6` failed-row sentinel) is clamped, NaN → 0.
    pub fn to_probability(self, raw: f64) -> f64 {
        let p = match self {
            ScoreKind::Probability => raw,
            ScoreKind::Logit => 1.0 / (1.0 + (-raw).exp()),
        };
        if p.is_nan() { 0.0 } else { p.clamp(0.0, 1.0) }
    }
}

/// How an engine's rerank output must be calibrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineScoring {
    /// Every model on this engine returns the same kind.
    Fixed(ScoreKind),
    /// Depends on the served GGUF's head — see [`check_gguf_head`].
    FromGgufHead,
}

/// Calibration rule for an engine id. `None` for engines without a defined
/// rule: their scores can't be promised as probabilities, so `/v1/rerank`
/// refuses them rather than guessing.
pub fn engine_scoring(engine_id: &str) -> Option<EngineScoring> {
    match engine_id {
        "omlx" => Some(EngineScoring::Fixed(ScoreKind::Probability)),
        "llamacpp" => Some(EngineScoring::FromGgufHead),
        _ => None,
    }
}

/// Why a GGUF can't be served as a reranker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadDefect {
    /// No parseable `.gguf` in the model directory.
    Unreadable,
    /// No `cls.output.weight` — a plain causal-LM / encoder conversion. llama.cpp
    /// would score the pooled hidden state, giving every document ≈ the same value.
    MissingClsOutput,
    /// `pooling_type` declared and not RANK — an embedding (or other) conversion.
    PoolingNotRank(u64),
    /// The classifier shape doesn't match what llama.cpp's RANK path reads as a
    /// single relevance score for this architecture.
    UnexpectedClassifierRows {
        arch: String,
        rows: u64,
        expected: u64,
    },
}

impl std::fmt::Display for HeadDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeadDefect::Unreadable => write!(f, "no readable .gguf file in the model directory"),
            HeadDefect::MissingClsOutput => write!(
                f,
                "GGUF has no reranker head (tensor `cls.output.weight` is missing) — it is a \
                 plain model conversion and llama.cpp would give every document the same score"
            ),
            HeadDefect::PoolingNotRank(p) => write!(
                f,
                "GGUF declares pooling_type={p}, not rank (4) — it is not a reranker conversion"
            ),
            HeadDefect::UnexpectedClassifierRows {
                arch,
                rows,
                expected,
            } => write!(
                f,
                "GGUF classifier head has {rows} output row(s); llama.cpp scores `{arch}` \
                 rerankers from exactly {expected}"
            ),
        }
    }
}

/// Validate a GGUF's reranker head and derive how llama.cpp's score must be
/// calibrated.
///
/// Required: a `cls.output.weight` tensor, and `pooling_type` either absent
/// (BERT-family conversions omit it; `--reranking` forces RANK pooling) or
/// RANK. Qwen3-family heads must have 2 rows (yes/no — llama.cpp softmaxes
/// them and returns P(yes)); every other architecture must have 1 (a logit).
pub fn check_gguf_head(info: &RerankHeadInfo) -> Result<ScoreKind, HeadDefect> {
    if let Some(p) = info.pooling_type
        && p != POOLING_TYPE_RANK
    {
        return Err(HeadDefect::PoolingNotRank(p));
    }
    let rows = info.cls_output_rows.ok_or(HeadDefect::MissingClsOutput)?;
    let arch = info.architecture.as_deref().unwrap_or("");
    let (expected, kind) = if SOFTMAX_RANK_ARCHS.contains(&arch) {
        (2, ScoreKind::Probability)
    } else {
        (1, ScoreKind::Logit)
    };
    if rows != expected {
        return Err(HeadDefect::UnexpectedClassifierRows {
            arch: arch.to_string(),
            rows,
            expected,
        });
    }
    Ok(kind)
}

/// What the rerank path needs to know about a served GGUF reranker.
#[derive(Debug, Clone, PartialEq)]
pub struct GgufRerankProfile {
    pub score_kind: ScoreKind,
    /// Trained context window (`{arch}.context_length`).
    pub context_length: Option<u64>,
    /// Prompt template llama-server wraps around each pair, if the GGUF has one.
    pub rerank_template: Option<String>,
}

/// Probe and validate the reranker GGUF in `model_dir`.
///
/// Header reads are cached per file (keyed by size + mtime), so calling this
/// on every `/v1/rerank` request costs one `stat` after the first.
pub fn check_model_dir(model_dir: &Path) -> Result<GgufRerankProfile, HeadDefect> {
    let gguf = largest_gguf_in_dir(model_dir).ok_or(HeadDefect::Unreadable)?;
    let info = cached_head(&gguf).ok_or(HeadDefect::Unreadable)?;
    let score_kind = check_gguf_head(&info)?;
    Ok(GgufRerankProfile {
        score_kind,
        context_length: info.context_length,
        rerank_template: info.rerank_template,
    })
}

/// Header facts of the largest GGUF in `model_dir` (cached like
/// [`check_model_dir`]). The llama.cpp adapter sizes embed / rerank loads
/// from them.
pub fn model_gguf_facts(model_dir: &Path) -> Option<RerankHeadInfo> {
    cached_head(&largest_gguf_in_dir(model_dir)?)
}

/// Remediation for a defective reranker install, shared by every surface
/// that reports one (pull, load, `/v1/rerank`, `doctor`, `models list`).
pub fn repull_hint(model_id: &str) -> String {
    format!(
        "Re-pull it with a current LMForge (its catalog points at a GGUF with a reranker head): \
         `lmforge models remove {model_id} && lmforge pull {model_id}`"
    )
}

/// Pull-time gate: the message to refuse a freshly downloaded GGUF reranker
/// with, or `None` when it is servable (or not a GGUF reranker at all).
pub fn pull_rejection(
    model_id: &str,
    hf_repo: &str,
    model_dir: &Path,
    is_gguf: bool,
    is_reranker: bool,
) -> Option<String> {
    if !is_gguf || !is_reranker {
        return None;
    }
    let defect = check_model_dir(model_dir).err()?;
    Some(format!(
        "'{model_id}' from {hf_repo} cannot be served as a reranker: {defect}. It was not added \
         to the model index (files remain in {dir}; `lmforge clean --partial` removes them). A \
         usable GGUF is one converted by llama.cpp's convert_hf_to_gguf.py, which writes \
         `cls.output.weight` and pooling_type=rank — see the rerank shortcuts in `lmforge catalog`.",
        dir = model_dir.display()
    ))
}

/// GGUF rerankers in the index whose head fails [`check_gguf_head`] —
/// typically Qwen3-Reranker pulled from the pre-0.3.0 catalog.
pub fn defective_gguf_rerankers(
    index: &crate::model::index::ModelIndex,
) -> Vec<(String, HeadDefect)> {
    index
        .list()
        .iter()
        .filter(|m| m.capabilities.reranking && m.format == "gguf")
        .filter_map(|m| match check_model_dir(Path::new(&m.path)) {
            // A missing directory is a different problem (not materialized);
            // the load path already reports it.
            Err(HeadDefect::Unreadable) | Ok(_) => None,
            Err(d) => Some((m.id.clone(), d)),
        })
        .collect()
}

type HeadCache = HashMap<PathBuf, (u64, Option<SystemTime>, RerankHeadInfo)>;

fn cached_head(gguf: &Path) -> Option<RerankHeadInfo> {
    static CACHE: OnceLock<Mutex<HeadCache>> = OnceLock::new();
    let meta = std::fs::metadata(gguf).ok()?;
    let stamp = (meta.len(), meta.modified().ok());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((len, mtime, info)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(gguf)
        && (*len, *mtime) == stamp
    {
        return Some(info.clone());
    }
    let info = read_rerank_head(gguf)?;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(gguf.to_path_buf(), (stamp.0, stamp.1, info.clone()));
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(arch: &str, pooling: Option<u64>, rows: Option<u64>) -> RerankHeadInfo {
        RerankHeadInfo {
            architecture: Some(arch.to_string()),
            pooling_type: pooling,
            cls_output_rows: rows,
            ..Default::default()
        }
    }

    #[test]
    fn qwen3_reranker_with_yes_no_head_scores_as_probability() {
        // ggml-org/Qwen3-Reranker-0.6B-Q8_0-GGUF: pooling 4, cls.output [1024, 2].
        assert_eq!(
            check_gguf_head(&head("qwen3", Some(4), Some(2))),
            Ok(ScoreKind::Probability)
        );
        assert_eq!(
            check_gguf_head(&head("qwen3vl", Some(4), Some(2))),
            Ok(ScoreKind::Probability)
        );
    }

    #[test]
    fn bert_cross_encoder_without_pooling_key_scores_as_logit() {
        // gpustack bge-reranker-v2-m3 / jina-reranker-v2: no pooling_type key,
        // cls.output [hidden] (1-D → 1 row).
        assert_eq!(
            check_gguf_head(&head("bert", None, Some(1))),
            Ok(ScoreKind::Logit)
        );
    }

    #[test]
    fn plain_causal_conversion_is_rejected_as_missing_head() {
        // mradermacher/Qwen3-Reranker-0.6B-GGUF: arch qwen3, no pooling, no cls.
        assert_eq!(
            check_gguf_head(&head("qwen3", None, None)),
            Err(HeadDefect::MissingClsOutput)
        );
    }

    #[test]
    fn non_rank_pooling_is_rejected_even_with_a_head() {
        assert_eq!(
            check_gguf_head(&head("bert", Some(1), Some(1))),
            Err(HeadDefect::PoolingNotRank(1))
        );
    }

    #[test]
    fn classifier_shape_must_match_the_architecture() {
        // A 1-row head on qwen3 would softmax to a constant 1.0.
        assert!(matches!(
            check_gguf_head(&head("qwen3", Some(4), Some(1))),
            Err(HeadDefect::UnexpectedClassifierRows {
                rows: 1,
                expected: 2,
                ..
            })
        ));
        // A multi-class BERT head: llama-server would return class 0's logit.
        assert!(matches!(
            check_gguf_head(&head("bert", None, Some(3))),
            Err(HeadDefect::UnexpectedClassifierRows {
                rows: 3,
                expected: 1,
                ..
            })
        ));
    }

    #[test]
    fn engine_scoring_is_explicit_per_engine() {
        assert_eq!(
            engine_scoring("omlx"),
            Some(EngineScoring::Fixed(ScoreKind::Probability))
        );
        assert_eq!(
            engine_scoring("llamacpp"),
            Some(EngineScoring::FromGgufHead)
        );
        assert_eq!(engine_scoring("sglang"), None);
        assert_eq!(engine_scoring("vllm"), None);
    }

    #[test]
    fn probabilities_pass_through_unchanged() {
        // oMLX 0.7.0, measured: 0.69140625 / 0.0000214577 must not be squeezed
        // into [0.5, 0.731] (the pre-0.3.0 unconditional sigmoid).
        assert_eq!(
            ScoreKind::Probability.to_probability(0.69140625),
            0.69140625
        );
        assert_eq!(
            ScoreKind::Probability.to_probability(0.0000214577),
            0.0000214577
        );
    }

    #[test]
    fn logits_are_mapped_through_the_sigmoid() {
        assert!((ScoreKind::Logit.to_probability(0.0) - 0.5).abs() < 1e-12);
        assert!(ScoreKind::Logit.to_probability(7.78) > 0.999);
        assert!(ScoreKind::Logit.to_probability(-11.03) < 1e-4);
        assert!(ScoreKind::Logit.to_probability(f64::MAX).is_finite());
    }

    #[test]
    fn out_of_range_raw_scores_are_clamped_into_the_unit_interval() {
        // llama-server reports -1e6 for a row it failed to pool.
        assert_eq!(ScoreKind::Probability.to_probability(-1e6), 0.0);
        assert_eq!(ScoreKind::Probability.to_probability(1.5), 1.0);
        assert_eq!(ScoreKind::Probability.to_probability(f64::NAN), 0.0);
        assert_eq!(ScoreKind::Logit.to_probability(f64::NAN), 0.0);
    }
}
