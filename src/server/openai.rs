use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use tracing::debug;

use super::proxy;
use super::thinking;
use super::AppState;

/// Load the model index, returning an empty index on failure.
fn load_index(data_dir: &std::path::Path) -> crate::model::index::ModelIndex {
    crate::model::index::ModelIndex::load(data_dir)
        .unwrap_or_else(|_| crate::model::index::ModelIndex { schema_version: 1, models: vec![] })
}

/// Check that a model's capabilities are appropriate for the requested role.
///
/// Returns `Ok(())` if the model is suitable, or an `Err(Response)` with a 400 body
/// describing why the model cannot be used for this purpose.
/// Models not found in the index are allowed through — the engine will handle the error.
fn check_model_role(
    index: &crate::model::index::ModelIndex,
    model_id: &str,
    require_chat: bool,
    require_embed: bool,
) -> Result<(), Response<Body>> {
    let Some(entry) = index.get(model_id) else {
        return Ok(()); // unknown model: let the engine surface the error
    };

    if require_chat && !entry.capabilities.chat {
        let kind = if entry.capabilities.reranking { "re-ranking" } else { "embedding" };
        let body = format!(
            r#"{{"error":{{"message":"Model '{}' is an {} model and cannot be used for chat completions.","type":"invalid_request_error"}}}}"#,
            model_id, kind
        );
        return Err(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap());
    }

    if require_embed && !entry.capabilities.embeddings {
        let body = format!(
            r#"{{"error":{{"message":"Model '{}' does not support embeddings. Use an embedding model such as 'nomic-embed-text:v1.5'.","type":"invalid_request_error"}}}}"#,
            model_id
        );
        return Err(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap());
    }

    Ok(())
}

/// `POST /v1/chat/completions` — OpenAI-compatible chat completions
pub async fn chat_completions(
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {

    let mut body_value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":{{"message":"Invalid JSON: {}","type":"invalid_request_error"}}}}"#,
                    e
                )))
                .unwrap();
        }
    };

    // Translate Ollama-standard `think: true/false` → `chat_template_kwargs.enable_thinking`
    // and strip `think` so the engine never sees an unknown field.
    // Must happen before has_think is read so both forms are detected.
    let has_think = thinking::request_has_think(&body_value);
    thinking::translate_think_field(&mut body_value);

    let is_stream = body_value.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let model_id = body_value.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let keep_alive = body_value.get("keep_alive").and_then(|v| {
        if v.is_string() {
            Some(v.as_str().unwrap().to_string())
        } else if v.is_number() {
            Some(v.as_i64().unwrap().to_string())
        } else {
            None
        }
    });

    if let Some(obj) = body_value.as_object_mut() {
        obj.remove("keep_alive");
    }

    debug!(stream = is_stream, think = has_think, model = %model_id, "Chat completion request");

    // Capability gate: reject embedding and re-ranking models sent to the chat endpoint.
    let index = load_index(&state.data_dir);
    if let Err(resp) = check_model_role(&index, &model_id, true, false) {
        return resp.into_response();
    }

    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(port) => port,
        Err(resp) => return resp.into_response(),
    };

    // Rewrite model_id to the exact filesystem directory name so engines don't panic
    if let Some(entry) = index.get(&model_id) {
        if let Some(dir_name) = std::path::Path::new(&entry.path).file_name() {
            if let Some(obj) = body_value.as_object_mut() {
                obj.insert("model".to_string(), serde_json::Value::String(dir_name.to_string_lossy().to_string()));
            }
        }
    }

    // Re-serialize with translations applied
    let forwarded_body = match serde_json::to_vec(&body_value) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!(r#"{{"error":{{"message":"{}"}}}}"#, e)))
                .unwrap();
        }
    };

    let client = proxy::build_proxy_client();

    if is_stream {
        // Streaming: pass through directly — oMLX natively emits delta.reasoning_content
        match proxy::proxy_stream(&client, engine_port, "/v1/chat/completions", forwarded_body).await {
            Ok(stream_body) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache")
                .header(header::CONNECTION, "keep-alive")
                .body(stream_body)
                .unwrap(),
            Err((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
        }
    } else if has_think {
        // Non-streaming + thinking: force stream internally so we can accumulate
        // reasoning_content and content chunks into separate fields
        match proxy::proxy_request_assembling_stream(
            &client,
            engine_port,
            "/v1/chat/completions",
            forwarded_body,
        ).await {
            Ok((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
            Err((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
        }
    } else {
        // Standard non-streaming pass-through
        match proxy::proxy_request(&client, engine_port, "/v1/chat/completions", forwarded_body).await {
            Ok((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
            Err((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
        }
    }
}

/// `POST /v1/completions` — OpenAI-compatible text completions
pub async fn completions(
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let mut body_value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let model_id = body_value.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    
    let keep_alive = body_value.get("keep_alive").and_then(|v| {
        if v.is_string() { Some(v.as_str().unwrap().to_string()) }
        else if v.is_number() { Some(v.as_i64().unwrap().to_string()) }
        else { None }
    });
    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(port) => port,
        Err(resp) => return resp.into_response(),
    };

    let index = crate::model::index::ModelIndex::load(&state.data_dir)
        .unwrap_or_else(|_| crate::model::index::ModelIndex { schema_version: 1, models: vec![] });
    if let Some(entry) = index.get(&model_id) {
        if let Some(dir_name) = std::path::Path::new(&entry.path).file_name() {
            if let Some(obj) = body_value.as_object_mut() {
                obj.insert("model".to_string(), serde_json::Value::String(dir_name.to_string_lossy().to_string()));
            }
        }
    }
    
    let forwarded_body = Bytes::from(serde_json::to_vec(&body_value).unwrap_or_default());

    let client = proxy::build_proxy_client();
    match proxy::proxy_request(&client, engine_port, "/v1/completions", forwarded_body).await {
        Ok((status, text)) => {
            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap()
        }
        Err((status, text)) => {
            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap()
        }
    }
}

/// `POST /v1/embeddings` — OpenAI-compatible embeddings
pub async fn embeddings(
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let mut body_value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let model_id = body_value.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let keep_alive = body_value.get("keep_alive").and_then(|v| {
        if v.is_string() { Some(v.as_str().unwrap().to_string()) }
        else if v.is_number() { Some(v.as_i64().unwrap().to_string()) }
        else { None }
    });

    // Engine-level gate: does this engine support embeddings at all?
    if !state.engine_config.supports_embeddings {
        return Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                r#"{{"error":{{"message":"Embeddings are not supported by {} v{}. This capability is available on oMLX (macOS), SGLang (Linux/NVIDIA), and llama.cpp platforms.","type":"not_supported_error"}}}}"#,
                state.engine_config.name, state.engine_config.version
            )))
            .unwrap()
            .into_response();
    }

    // Model-level gate: does this specific model support embeddings?
    let index = load_index(&state.data_dir);
    if let Err(resp) = check_model_role(&index, &model_id, false, true) {
        return resp.into_response();
    }

    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(port) => port,
        Err(resp) => return resp.into_response(),
    };

    if let Some(entry) = index.get(&model_id) {
        if let Some(dir_name) = std::path::Path::new(&entry.path).file_name() {
            if let Some(obj) = body_value.as_object_mut() {
                obj.insert("model".to_string(), serde_json::Value::String(dir_name.to_string_lossy().to_string()));
            }
        }
    }

    let forwarded_body = Bytes::from(serde_json::to_vec(&body_value).unwrap_or_default());

    let client = proxy::build_proxy_client();
    match proxy::proxy_request(&client, engine_port, "/v1/embeddings", forwarded_body).await {
        Ok((status, text)) => {
            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap()
        }
        Err((status, text)) => {
            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap()
        }
    }
}

/// `GET /v1/models` — List available models with capability metadata
pub async fn models(
    State(state): State<AppState>,
) -> impl IntoResponse {

    let index = crate::model::index::ModelIndex::load(&state.data_dir)
        .unwrap_or_else(|_| crate::model::index::ModelIndex { schema_version: 1, models: vec![] });

    let data: Vec<serde_json::Value> = index.list().iter().map(|m| {
        serde_json::json!({
            "id": m.id,
            "object": "model",
            "owned_by": "lmforge",
            "capabilities": {
                "chat": m.capabilities.chat,
                "embeddings": m.capabilities.embeddings,
                "reranking": m.capabilities.reranking,
                "thinking": m.capabilities.thinking,
                "embedding_dims": m.capabilities.embedding_dims,
            }
        })
    }).collect();

    let resp = serde_json::json!({ "object": "list", "data": data });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&resp).unwrap()))
        .unwrap()
}
