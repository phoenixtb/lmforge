use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::AppState;
use super::proxy;
use crate::model::rerank_head::{self, EngineScoring, GgufRerankProfile, ScoreKind};

/// `POST /v1/rerank` — Re-ranking endpoint compatible with the Cohere / Jina rerank schema.
///
/// Accepted by LangChain's `CohereRerank`, LlamaIndex's `LLMRerank`, and most RAG frameworks.
///
/// **Request:**
/// ```json
/// {
///   "model": "bge-reranker-v2-m3:8bit",
///   "query": "What is quantum computing?",
///   "documents": ["doc 1 text", "doc 2 text"],
///   "top_n": 3,             // optional — limit results returned
///   "return_documents": true // optional — echo document text in response
/// }
/// ```
///
/// **Response:**
/// ```json
/// {
///   "model": "bge-reranker-v2-m3:8bit",
///   "results": [
///     { "index": 1, "relevance_score": 0.94, "document": { "text": "doc 2 text" } },
///     { "index": 0, "relevance_score": 0.03, "document": { "text": "doc 1 text" } }
///   ],
///   "score_type": "probability",
///   "usage": { "prompt_tokens": 120, "total_tokens": 120 },
///   "meta": { "truncated_documents": [1] }   // only when a document was truncated
/// }
/// ```
///
/// `relevance_score` is a calibrated relevance probability in [0, 1] on every
/// engine (see `model::rerank_head` for the per-engine/model rule).
///
/// **Truncation policy.** Each query+document pair must fit the engine's
/// per-pair token window. A longer document is truncated from the end; the
/// query never is, and one long document never fails the request. On
/// llama.cpp LMForge truncates to the pooled window (`pooling_window`, 2048
/// tokens by default) and lists the affected indices in
/// `meta.truncated_documents`; oMLX truncates inside the engine (8192 tokens
/// for causal-LM rerankers, 512 for sequence classifiers). A query that alone
/// leaves no room for a document is rejected with 400.
pub async fn rerank(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    // --- Parse request ---
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                None,
                &format!("Invalid JSON: {e}"),
            );
        }
    };

    let Some(model_id) = req
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            None,
            "'model' field is required",
        );
    };

    let Some(query) = req
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            None,
            "'query' field is required",
        );
    };

    let documents: Vec<String> = match req.get("documents").and_then(|v| v.as_array()) {
        Some(docs) if !docs.is_empty() => docs
            .iter()
            .map(|d| d.as_str().unwrap_or("").to_string())
            .collect(),
        Some(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                None,
                "'documents' array must not be empty",
            );
        }
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                None,
                "'documents' field is required and must be a non-empty array",
            );
        }
    };

    let top_n = req
        .get("top_n")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let return_documents = req
        .get("return_documents")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let keep_alive = req.get("keep_alive").and_then(|v| {
        if v.is_string() {
            Some(v.as_str().unwrap().to_string())
        } else if v.is_number() {
            Some(v.as_i64().unwrap().to_string())
        } else {
            None
        }
    });

    debug!(model = %model_id, docs = documents.len(), "Re-rank request");

    // --- Engine-level gate: does this engine support re-ranking? ---
    if !state.engine_config.supports_reranking {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "not_supported_error",
            None,
            &format!(
                "Re-ranking is not supported by {} v{}. It is available on the llama.cpp \
                 (Linux / Windows) and oMLX (macOS) engines.",
                state.engine_config.name, state.engine_config.version
            ),
        );
    }

    // --- Score calibration must be defined for this engine ---
    let Some(scoring) = rerank_head::engine_scoring(&state.engine_config.id) else {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "not_supported_error",
            None,
            &format!(
                "No relevance-score calibration is defined for engine '{}', so /v1/rerank \
                 cannot return probabilities from it.",
                state.engine_config.id
            ),
        );
    };

    // --- Model-level gate: does this model support re-ranking? ---
    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_default();
    let entry = index.get(&model_id);

    if let Some(entry) = entry
        && !entry.capabilities.reranking
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            None,
            &format!(
                "Model '{model_id}' does not support re-ranking. Use a re-ranker model such as \
                 'qwen3-reranker:0.6b:8bit' or 'bge-reranker-v2-m3:8bit'."
            ),
        );
    }

    // --- llama.cpp: the served GGUF decides calibration; refuse a headless one
    //     before loading it (it would score every document the same). ---
    let gguf_profile: Option<GgufRerankProfile> = match scoring {
        EngineScoring::Fixed(_) => None,
        EngineScoring::FromGgufHead => {
            let Some(entry) = entry else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    "invalid_request_error",
                    Some("model_not_found"),
                    &format!(
                        "Model '{model_id}' is not installed. Pull it with: lmforge pull {model_id}"
                    ),
                );
            };
            match rerank_head::check_model_dir(std::path::Path::new(&entry.path)) {
                Ok(profile) => Some(profile),
                Err(defect) => {
                    warn!(model = %model_id, %defect, "Refusing rerank on a defective reranker GGUF");
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "invalid_request_error",
                        Some("reranker_unusable"),
                        &format!(
                            "Model '{model_id}' cannot be used for re-ranking: {defect}. {}",
                            rerank_head::repull_hint(&model_id)
                        ),
                    );
                }
            }
        }
    };
    let score_kind = match (scoring, &gguf_profile) {
        (EngineScoring::Fixed(kind), _) => kind,
        (EngineScoring::FromGgufHead, Some(p)) => p.score_kind,
        (EngineScoring::FromGgufHead, None) => unreachable!("profile resolved above"),
    };

    // --- Ensure model is loaded ---
    let guard = match state.ensure_model_request(&model_id, keep_alive).await {
        Ok(g) => g,
        Err(resp) => return resp.into_response(),
    };
    let engine_port = guard.port();
    let client = proxy::build_proxy_client();

    // --- llama.cpp: fit every pair into the pooled window ---
    let window = gguf_profile
        .as_ref()
        .map(|p| crate::engine::adapters::llamacpp::pooling_window(p.context_length) as usize);
    let (engine_documents, truncated) = match (&gguf_profile, window) {
        (Some(profile), Some(window)) => {
            match fit_documents(&client, engine_port, &query, &documents, profile, window).await {
                Ok(fitted) => fitted,
                Err(FitError::QueryTooLong {
                    query_tokens,
                    window,
                }) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        Some("query_too_long"),
                        &format!(
                            "The query is {query_tokens} tokens; with the prompt template it \
                             leaves no room for a document in this model's {window}-token \
                             query+document window. Shorten the query."
                        ),
                    );
                }
                Err(FitError::Engine(msg)) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        "server_error",
                        None,
                        &format!("Engine tokenization failed while fitting documents: {msg}"),
                    );
                }
            }
        }
        _ => (documents.clone(), Vec::new()),
    };
    if !truncated.is_empty() {
        debug!(model = %model_id, ?truncated, "Truncated rerank documents to the pooled window");
    }

    // Resolve physical directory name for the model field
    let model_dir_name = entry
        .and_then(|e| {
            std::path::Path::new(&e.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| model_id.clone());

    // Both engines accept the Cohere schema on /v1/rerank.
    let engine_req = json!({
        "model": model_dir_name,
        "query": query,
        "documents": engine_documents,
    });

    let forwarded_body = Bytes::from(serde_json::to_vec(&engine_req).unwrap_or_default());

    let (status, text) =
        match proxy::proxy_request(&client, engine_port, "/v1/rerank", forwarded_body).await {
            Ok(r) => r,
            Err((status, text)) => {
                return Response::builder()
                    .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(text))
                    .unwrap()
                    .into_response();
            }
        };

    if status != 200 {
        return map_engine_error(status, text, window);
    }

    // --- Normalize and format the response ---
    let normalized = match normalize_rerank_response(
        &text,
        &model_id,
        &documents,
        top_n,
        return_documents,
        score_kind,
        &truncated,
    ) {
        Ok(body) => body,
        Err(e) => {
            warn!(error = %e, "Engine rerank response could not be normalized");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "server_error",
                None,
                &format!("Engine returned an unexpected rerank response: {e}"),
            );
        }
    };

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(normalized))
        .unwrap()
        .into_response();
    super::attach_inflight_guard(response, guard)
}

fn error_response(
    status: StatusCode,
    kind: &str,
    code: Option<&str>,
    message: &str,
) -> axum::response::Response {
    let body = json!({"error": {"message": message, "type": kind, "param": null, "code": code}});
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
        .into_response()
}

/// Map a non-200 engine reply. llama-server reports an over-long pair as a
/// 500 (`input (N tokens) is too large to process…`); that is the caller's
/// input, so it becomes a 400. Anything else passes through unchanged.
fn map_engine_error(status: u16, text: String, window: Option<usize>) -> axum::response::Response {
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| text.clone());
    if is_input_too_long(&message) {
        let window = window.map_or_else(|| "per-pair".to_string(), |w| format!("{w}-token"));
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            Some("input_too_long"),
            &format!("A query+document pair exceeds the model's {window} window: {message}"),
        );
    }
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(text))
        .unwrap()
        .into_response()
}

fn is_input_too_long(engine_message: &str) -> bool {
    engine_message.contains("too large to process")
        || engine_message.contains("larger than the max context size")
}

// ── Document fitting (llama.cpp) ─────────────────────────────────────────────

/// Allowance for tokenising template, query and document separately instead
/// of as one prompt (merges at the seams).
const SEAM_SLACK_TOKENS: usize = 16;

/// Specials llama-server adds around a pair when the GGUF has no rerank
/// template: `[BOS] query [EOS] [SEP] document [EOS]`.
const UNTEMPLATED_PAIR_SPECIALS: usize = 4;

/// Smallest document budget worth scoring; a query leaving less is rejected.
const MIN_DOC_TOKENS: usize = 32;

#[derive(Debug, PartialEq)]
enum FitError {
    QueryTooLong { query_tokens: usize, window: usize },
    Engine(String),
}

/// The rerank template with its `{query}` / `{document}` slots removed — the
/// fixed text llama-server adds to every pair.
fn template_skeleton(template: Option<&str>) -> String {
    template
        .unwrap_or("")
        .replace("{query}", "")
        .replace("{document}", "")
}

/// A token is at least one byte for llama.cpp's byte-level / byte-fallback
/// tokenizers, so byte length bounds token count: a pair whose bytes fit the
/// window needs no tokenizer round-trip.
fn may_exceed_window(fixed_bytes: usize, doc_bytes: usize, window: usize) -> bool {
    fixed_bytes + doc_bytes + SEAM_SLACK_TOKENS > window
}

/// Tokens left for a document once the template, query and slack are paid.
fn doc_token_budget(window: usize, overhead_tokens: usize) -> Option<usize> {
    window
        .checked_sub(overhead_tokens + SEAM_SLACK_TOKENS)
        .filter(|b| *b >= MIN_DOC_TOKENS)
}

/// Truncate (from the end) every document whose pair would exceed `window`
/// tokens. Returns the documents to send and the indices that were cut.
async fn fit_documents(
    client: &reqwest::Client,
    port: u16,
    query: &str,
    documents: &[String],
    profile: &GgufRerankProfile,
    window: usize,
) -> Result<(Vec<String>, Vec<usize>), FitError> {
    let skeleton = template_skeleton(profile.rerank_template.as_deref());
    let specials = if profile.rerank_template.is_some() {
        0
    } else {
        UNTEMPLATED_PAIR_SPECIALS
    };
    let fixed_bytes = skeleton.len() + specials + query.len();
    let candidates: Vec<usize> = (0..documents.len())
        .filter(|&i| may_exceed_window(fixed_bytes, documents[i].len(), window))
        .collect();
    if candidates.is_empty() {
        return Ok((documents.to_vec(), Vec::new()));
    }

    let query_tokens = tokenize(client, port, query).await?.len();
    let skeleton_tokens = if skeleton.is_empty() {
        0
    } else {
        tokenize(client, port, &skeleton).await?.len()
    };
    let budget = doc_token_budget(window, skeleton_tokens + specials + query_tokens).ok_or(
        FitError::QueryTooLong {
            query_tokens,
            window,
        },
    )?;

    let mut fitted = documents.to_vec();
    let mut truncated = Vec::new();
    for i in candidates {
        let tokens = tokenize(client, port, &documents[i]).await?;
        if tokens.len() > budget {
            fitted[i] = detokenize(client, port, &tokens[..budget]).await?;
            truncated.push(i);
        }
    }
    Ok((fitted, truncated))
}

async fn engine_post(
    client: &reqwest::Client,
    port: u16,
    path: &str,
    body: &Value,
) -> Result<Value, FitError> {
    let resp = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(body)
        .send()
        .await
        .map_err(|e| FitError::Engine(format!("{path}: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| FitError::Engine(format!("{path}: {e}")))?;
    if !status.is_success() {
        return Err(FitError::Engine(format!("{path}: HTTP {status}: {text}")));
    }
    serde_json::from_str(&text).map_err(|e| FitError::Engine(format!("{path}: {e}")))
}

async fn tokenize(client: &reqwest::Client, port: u16, text: &str) -> Result<Vec<Value>, FitError> {
    let v = engine_post(
        client,
        port,
        "/tokenize",
        &json!({"content": text, "add_special": false}),
    )
    .await?;
    match v.get("tokens").and_then(|t| t.as_array()) {
        Some(tokens) => Ok(tokens.clone()),
        None => Err(FitError::Engine(
            "/tokenize: response has no 'tokens'".into(),
        )),
    }
}

async fn detokenize(
    client: &reqwest::Client,
    port: u16,
    tokens: &[Value],
) -> Result<String, FitError> {
    let v = engine_post(client, port, "/detokenize", &json!({"tokens": tokens})).await?;
    v.get("content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| FitError::Engine("/detokenize: response has no 'content'".into()))
}

// ── Response normalisation ───────────────────────────────────────────────────

/// Normalize an engine `/v1/rerank` response into the Cohere-compatible format.
///
/// - Maps each raw score to a probability per `score_kind` (sigmoid for
///   logits, pass-through for probabilities).
/// - Sorts results by `relevance_score` descending (Cohere convention).
/// - Applies `top_n` truncation after sorting.
/// - Optionally echoes the original (untruncated) document text back.
fn normalize_rerank_response(
    raw: &str,
    model_id: &str,
    documents: &[String],
    top_n: Option<usize>,
    return_documents: bool,
    score_kind: ScoreKind,
    truncated: &[usize],
) -> Result<String, String> {
    let engine_resp: Value =
        serde_json::from_str(raw).map_err(|e| format!("Failed to parse engine response: {e}"))?;

    let results = engine_resp["results"]
        .as_array()
        .ok_or("Missing 'results' array in engine response")?;

    let mut scored: Vec<(usize, f64)> = results
        .iter()
        .filter_map(|r| {
            let idx = r["index"].as_u64()? as usize;
            let raw = r["relevance_score"].as_f64()?;
            if score_kind == ScoreKind::Probability && !(-1e-6..=1.0 + 1e-6).contains(&raw) {
                warn!(
                    index = idx,
                    raw, "Engine probability outside [0, 1]; clamping"
                );
            }
            Some((idx, score_kind.to_probability(raw)))
        })
        .collect();

    // Sort descending by relevance score (Cohere convention)
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Apply top_n: clamp to document count so top_n > len is never an error
    let limit = top_n.unwrap_or(scored.len()).min(scored.len());
    let scored = &scored[..limit];

    let result_items: Vec<Value> = scored
        .iter()
        .map(|(idx, score)| {
            let mut item = json!({
                "index": idx,
                "relevance_score": score,
            });
            if return_documents && let Some(text) = documents.get(*idx) {
                item["document"] = json!({ "text": text });
            }
            item
        })
        .collect();

    // Propagate usage if present
    let usage = engine_resp.get("usage").cloned().unwrap_or(Value::Null);

    let mut response = json!({
        "model": model_id,
        "results": result_items,
        "score_type": "probability",
        "usage": usage,
    });
    if !truncated.is_empty() {
        response["meta"] = json!({ "truncated_documents": truncated });
    }

    serde_json::to_string(&response).map_err(|e| format!("Failed to serialize response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(raw: &str, docs: &[&str], kind: ScoreKind) -> Value {
        let docs: Vec<String> = docs.iter().map(|d| d.to_string()).collect();
        let out = normalize_rerank_response(raw, "m", &docs, None, false, kind, &[]).unwrap();
        serde_json::from_str(&out).unwrap()
    }

    fn scores(v: &Value) -> Vec<(u64, f64)> {
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["index"].as_u64().unwrap(),
                    r["relevance_score"].as_f64().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn probabilities_are_returned_unchanged_not_squeezed() {
        // oMLX 0.7.0 measured output for the DocIntel pair.
        let raw = r#"{"results":[{"index":0,"relevance_score":0.69140625},{"index":1,"relevance_score":0.0000214577}],"usage":null}"#;
        let v = normalize(raw, &["a", "b"], ScoreKind::Probability);
        assert_eq!(scores(&v), vec![(0, 0.69140625), (1, 0.0000214577)]);
        assert_eq!(v["score_type"], "probability");
    }

    #[test]
    fn logits_are_converted_with_the_sigmoid() {
        // bge-reranker-v2-m3 on llama.cpp b9861, measured raw logits.
        let raw = r#"{"results":[{"index":0,"relevance_score":-11.03},{"index":1,"relevance_score":3.38}],"usage":null}"#;
        let v = normalize(raw, &["a", "b"], ScoreKind::Logit);
        let s = scores(&v);
        assert_eq!(s[0].0, 1);
        assert!((s[0].1 - 0.9671).abs() < 1e-3, "{s:?}");
        assert!(s[1].1 < 1e-4, "{s:?}");
        assert_eq!(v["score_type"], "probability");
    }

    #[test]
    fn results_sort_descending() {
        let raw = r#"{
            "results": [
                {"index": 0, "relevance_score": 1.0},
                {"index": 1, "relevance_score": 5.0},
                {"index": 2, "relevance_score": -1.0}
            ],
            "usage": {"prompt_tokens": 10, "total_tokens": 10}
        }"#;
        let v = normalize(raw, &["a", "b", "c"], ScoreKind::Logit);
        let order: Vec<u64> = scores(&v).iter().map(|s| s.0).collect();
        assert_eq!(order, vec![1, 0, 2]);
        assert_eq!(v["usage"]["total_tokens"], 10);
    }

    #[test]
    fn top_n_larger_than_document_count_clamps() {
        let raw = r#"{"results":[{"index":0,"relevance_score":0.1},{"index":1,"relevance_score":0.2}],"usage":null}"#;
        let docs = vec!["a".to_string(), "b".to_string()];
        let out =
            normalize_rerank_response(raw, "m", &docs, Some(5), false, ScoreKind::Probability, &[])
                .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn return_documents_echoes_the_original_untruncated_text() {
        let raw = r#"{"results":[{"index":0,"relevance_score":0.9}],"usage":null}"#;
        let docs = vec!["hello world".to_string()];
        let out =
            normalize_rerank_response(raw, "m", &docs, None, true, ScoreKind::Probability, &[0])
                .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"][0]["document"]["text"], "hello world");
        assert_eq!(v["meta"]["truncated_documents"], json!([0]));
    }

    #[test]
    fn meta_is_omitted_when_nothing_was_truncated() {
        let raw = r#"{"results":[{"index":0,"relevance_score":0.9}],"usage":null}"#;
        let v = normalize(raw, &["a"], ScoreKind::Probability);
        assert!(v.get("meta").is_none());
    }

    #[test]
    fn every_score_lands_in_the_unit_interval() {
        let raw = r#"{"results":[{"index":0,"relevance_score":10.0},{"index":1,"relevance_score":-1000000.0}],"usage":null}"#;
        for kind in [ScoreKind::Logit, ScoreKind::Probability] {
            for (_, s) in scores(&normalize(raw, &["a", "b"], kind)) {
                assert!((0.0..=1.0).contains(&s), "{kind:?}: {s}");
            }
        }
    }

    #[test]
    fn template_skeleton_strips_the_query_and_document_slots() {
        let t = "<Query>: {query}\n<Document>: {document}<|im_end|>";
        assert_eq!(
            template_skeleton(Some(t)),
            "<Query>: \n<Document>: <|im_end|>"
        );
        assert_eq!(template_skeleton(None), "");
    }

    #[test]
    fn short_pairs_skip_the_tokenizer_round_trip() {
        // 368-byte Qwen3 template + query + a 1 KB chunk fits a 2048 window.
        assert!(!may_exceed_window(400, 1000, 2048));
        assert!(may_exceed_window(400, 1700, 2048));
    }

    #[test]
    fn doc_budget_reserves_template_query_and_slack() {
        assert_eq!(
            doc_token_budget(2048, 80),
            Some(2048 - 80 - SEAM_SLACK_TOKENS)
        );
        // A query eating the whole window leaves no usable budget.
        assert_eq!(doc_token_budget(2048, 2020), None);
        assert_eq!(doc_token_budget(512, 600), None);
    }

    #[test]
    fn llama_server_oversize_errors_are_recognised() {
        assert!(is_input_too_long(
            "input (796 tokens) is too large to process. increase the physical batch size (current batch size: 512)"
        ));
        assert!(is_input_too_long(
            "input (9000 tokens) is larger than the max context size (8192 tokens). skipping"
        ));
        assert!(!is_input_too_long("model not loaded"));
    }
}
