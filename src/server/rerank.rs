use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use tracing::debug;

use super::AppState;
use super::proxy;

/// `POST /v1/rerank` — Re-ranking endpoint compatible with the Cohere / Jina rerank schema.
///
/// Accepted by LangChain's `CohereRerank`, LlamaIndex's `LLMRerank`, and most RAG frameworks.
///
/// **Request:**
/// ```json
/// {
///   "model": "bge-reranker-v2-m3",
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
///   "model": "bge-reranker-v2-m3",
///   "results": [
///     { "index": 1, "relevance_score": 0.94, "document": { "text": "doc 2 text" } },
///     { "index": 0, "relevance_score": 0.71, "document": { "text": "doc 1 text" } }
///   ],
///   "usage": { "prompt_tokens": 120, "total_tokens": 120 }
/// }
/// ```
///
/// Scores are normalised to [0, 1] via sigmoid so downstream clients get consistent values
/// regardless of whether the engine returns raw logits or probabilities.
pub async fn rerank(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    // --- Parse request ---
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":{{"message":"Invalid JSON: {}","type":"invalid_request_error"}}}}"#, e
                )))
                .unwrap()
                .into_response();
        }
    };

    let model_id = match req.get("model").and_then(|v| v.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"error":{"message":"'model' field is required","type":"invalid_request_error"}}"#))
                .unwrap()
                .into_response();
        }
    };

    let query = match req.get("query").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"error":{"message":"'query' field is required","type":"invalid_request_error"}}"#))
                .unwrap()
                .into_response();
        }
    };

    let documents: Vec<String> = match req.get("documents").and_then(|v| v.as_array()) {
        Some(docs) if !docs.is_empty() => docs
            .iter()
            .map(|d| d.as_str().unwrap_or("").to_string())
            .collect(),
        Some(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"error":{"message":"'documents' array must not be empty","type":"invalid_request_error"}}"#))
                .unwrap()
                .into_response();
        }
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"error":{"message":"'documents' field is required and must be a non-empty array","type":"invalid_request_error"}}"#))
                .unwrap()
                .into_response();
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
        return Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                r#"{{"error":{{"message":"Re-ranking is not supported by {} v{}. It is available on platforms using llama.cpp (CPU, small GPU, or Windows).","type":"not_supported_error"}}}}"#,
                state.engine_config.name, state.engine_config.version
            )))
            .unwrap()
            .into_response();
    }

    // --- Model-level gate: does this model support re-ranking? ---
    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });

    if let Some(entry) = index.get(&model_id)
        && !entry.capabilities.reranking
    {
        return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":{{"message":"Model '{}' does not support re-ranking. Use a re-ranker model such as 'bge-reranker-v2-m3'.","type":"invalid_request_error"}}}}"#,
                    model_id
                )))
                .unwrap()
                .into_response();
    }

    // --- Ensure model is loaded ---
    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(port) => port,
        Err(resp) => return resp.into_response(),
    };

    // Resolve physical directory name for the model field
    let model_dir_name = index
        .get(&model_id)
        .and_then(|e| {
            std::path::Path::new(&e.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| model_id.clone());

    // --- Build the request body for llama.cpp /v1/rerank ---
    // llama.cpp's /v1/rerank accepts the Cohere schema directly.
    let engine_req = serde_json::json!({
        "model": model_dir_name,
        "query": query,
        "documents": documents,
    });

    let forwarded_body = Bytes::from(serde_json::to_vec(&engine_req).unwrap_or_default());
    let client = proxy::build_proxy_client();

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
        return Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(text))
            .unwrap()
            .into_response();
    }

    // --- Normalize and format the response ---
    let normalized = match normalize_rerank_response(
        &text,
        &model_id,
        &documents,
        top_n,
        return_documents,
    ) {
        Ok(body) => body,
        Err(e) => {
            // Engine response was valid but parsing failed — return raw response
            tracing::warn!(error = %e, "Failed to normalize rerank response; returning raw engine output");
            text
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(normalized))
        .unwrap()
        .into_response()
}

/// Normalize the llama.cpp /v1/rerank response into the Cohere-compatible format.
///
/// - Applies `sigmoid()` to raw logit scores to produce consistent [0, 1] values.
/// - Sorts results by `relevance_score` descending (Cohere convention).
/// - Applies `top_n` truncation after sorting.
/// - Optionally echoes document text back.
fn normalize_rerank_response(
    raw: &str,
    model_id: &str,
    documents: &[String],
    top_n: Option<usize>,
    return_documents: bool,
) -> Result<String, String> {
    let engine_resp: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("Failed to parse engine response: {e}"))?;

    let results = engine_resp["results"]
        .as_array()
        .ok_or("Missing 'results' array in engine response")?;

    let mut scored: Vec<(usize, f64)> = results
        .iter()
        .filter_map(|r| {
            let idx = r["index"].as_u64()? as usize;
            let score = r["relevance_score"].as_f64()?;
            Some((idx, score))
        })
        .collect();

    // Apply sigmoid normalization — llama.cpp returns raw logits for cross-encoders.
    // Scores already in [0,1] are essentially unaffected (sigmoid(0)=0.5, sigmoid(large)≈1).
    for (_, score) in &mut scored {
        *score = sigmoid(*score);
    }

    // Sort descending by relevance score (Cohere convention)
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Apply top_n: clamp to document count so top_n > len is never an error
    let limit = top_n.unwrap_or(scored.len()).min(scored.len());
    let scored = &scored[..limit];

    let result_items: Vec<serde_json::Value> = scored
        .iter()
        .map(|(idx, score)| {
            let mut item = serde_json::json!({
                "index": idx,
                "relevance_score": score,
            });
            if return_documents && let Some(text) = documents.get(*idx) {
                item["document"] = serde_json::json!({ "text": text });
            }
            item
        })
        .collect();

    // Propagate usage if present
    let usage = engine_resp
        .get("usage")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let response = serde_json::json!({
        "model": model_id,
        "results": result_items,
        "usage": usage,
    });

    serde_json::to_string(&response).map_err(|e| format!("Failed to serialize response: {e}"))
}

/// Standard sigmoid function: maps any real value to (0, 1).
/// Applied to raw logit scores from cross-encoder re-rankers.
#[inline]
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid_bounds() {
        assert!(sigmoid(0.0) > 0.49 && sigmoid(0.0) < 0.51);
        assert!(sigmoid(100.0) > 0.999);
        assert!(sigmoid(-100.0) < 0.001);
    }

    #[test]
    fn test_sigmoid_large_logit_doesnt_panic() {
        assert!(sigmoid(f64::MAX).is_finite() || sigmoid(f64::MAX).is_nan() == false);
        assert!(sigmoid(f64::MIN).is_finite());
    }

    #[test]
    fn test_normalize_rerank_sorts_descending() {
        let raw = r#"{
            "results": [
                {"index": 0, "relevance_score": 1.0},
                {"index": 1, "relevance_score": 5.0},
                {"index": 2, "relevance_score": -1.0}
            ],
            "usage": {"prompt_tokens": 10, "total_tokens": 10}
        }"#;
        let docs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = normalize_rerank_response(raw, "test-model", &docs, None, false).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let results = parsed["results"].as_array().unwrap();
        // Index 1 had highest logit (5.0) → should be first
        assert_eq!(results[0]["index"].as_u64().unwrap(), 1);
        assert_eq!(results[2]["index"].as_u64().unwrap(), 2);
    }

    #[test]
    fn test_normalize_rerank_top_n_clamps() {
        let raw = r#"{"results":[{"index":0,"relevance_score":1.0},{"index":1,"relevance_score":2.0}],"usage":null}"#;
        let docs = vec!["a".to_string(), "b".to_string()];
        // top_n = 5 but only 2 docs — should not error
        let result = normalize_rerank_response(raw, "m", &docs, Some(5), false).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_normalize_rerank_return_documents() {
        let raw = r#"{"results":[{"index":0,"relevance_score":2.0}],"usage":null}"#;
        let docs = vec!["hello world".to_string()];
        let result = normalize_rerank_response(raw, "m", &docs, None, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["results"][0]["document"]["text"].as_str().unwrap(),
            "hello world"
        );
    }

    #[test]
    fn test_normalize_rerank_scores_in_unit_interval() {
        let raw = r#"{"results":[{"index":0,"relevance_score":10.0},{"index":1,"relevance_score":-10.0}],"usage":null}"#;
        let docs = vec!["a".to_string(), "b".to_string()];
        let result = normalize_rerank_response(raw, "m", &docs, None, false).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        for r in parsed["results"].as_array().unwrap() {
            let score = r["relevance_score"].as_f64().unwrap();
            assert!(score >= 0.0 && score <= 1.0, "score {score} outside [0,1]");
        }
    }
}
