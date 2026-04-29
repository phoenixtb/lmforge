use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use tracing::debug;

use super::AppState;
use super::proxy;
use super::thinking;

/// Load the model index, returning an empty index on failure.
fn load_index(data_dir: &std::path::Path) -> crate::model::index::ModelIndex {
    crate::model::index::ModelIndex::load(data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    })
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
        let kind = if entry.capabilities.reranking {
            "re-ranking"
        } else {
            "embedding"
        };
        let body = format!(
            r#"{{"error":{{"message":"Model '{}' is an {} model and cannot be used for chat completions.","type":"invalid_request_error","param":null,"code":null}}}}"#,
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
            r#"{{"error":{{"message":"Model '{}' does not support embeddings. Use an embedding model such as 'nomic-embed-text:v1.5'.","type":"invalid_request_error","param":null,"code":null}}}}"#,
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
pub async fn chat_completions(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let mut body_value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"error":{{"message":"Invalid JSON: {}","type":"invalid_request_error","param":null,"code":null}}}}"#,
                    e
                )))
                .unwrap();
        }
    };

    // Read model_id early — needed for capability lookup before think translation.
    let model_id = body_value
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Load the model index early — needed for model capabilities before think translation.
    let index = load_index(&state.data_dir);

    // Engine-aware think translation:
    // oMLX  → strip enable_thinking flag; translate penalties; inject enable_thinking:false for think:false
    // other → translate think → chat_template_kwargs.enable_thinking
    // Must capture has_think BEFORE apply_think_for_engine removes the field.
    let has_think = thinking::request_has_think(&body_value);
    let model_caps = index.get(&model_id).map(|e| &e.capabilities);

    // Read original_max_tokens before apply_think_for_engine (which may modify body).
    // Needed to calculate remaining tokens for call-2 in the budget path.
    let original_max_tokens: u32 = body_value
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(2048)
        .min(u32::MAX as u64) as u32;

    // Extract thinking_budget BEFORE apply_think_for_engine (read-only call).
    let thinking_budget = thinking::extract_thinking_budget(&body_value);

    // Extract stream_reasoning_deltas BEFORE apply_think_for_engine.
    let stream_reasoning_deltas = thinking::extract_stream_reasoning_deltas(&body_value);

    // Apply engine-specific transformations (strips think, num_ctx, translates penalties).
    thinking::apply_think_for_engine(&mut body_value, &state.engine_config.id, model_caps);

    // Strip thinking_budget and stream_reasoning_deltas — engine has no concept of them.
    if let Some(obj) = body_value.as_object_mut() {
        obj.remove("thinking_budget");
        obj.remove("stream_reasoning_deltas");
        if let Some(extra) = obj.get_mut("extra_body").and_then(|v| v.as_object_mut()) {
            extra.remove("stream_reasoning_deltas");
        }
    }

    let is_stream = body_value
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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

    // Determine if the two-call budget path is applicable:
    //   • engine must be oMLX (only engine with native thinking support we manage)
    //   • model must have thinking capability in the index
    //   • think must be requested (has_think=true)
    //   • thinking_budget must be explicitly provided
    let is_omlx = state.engine_config.id == "omlx";
    let is_thinking_model = model_caps.map(|c| c.thinking).unwrap_or(false);
    let can_use_budget = has_think && is_omlx && is_thinking_model;

    debug!(
        stream = is_stream,
        think = has_think,
        thinking_budget = ?thinking_budget,
        stream_reasoning_deltas = stream_reasoning_deltas,
        model = %model_id,
        "Chat completion request"
    );

    // Capability gate: reject embedding and re-ranking models sent to the chat endpoint.
    // (index already loaded above)
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
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(dir_name.to_string_lossy().to_string()),
                );
            }
        }
    }

    let client = proxy::build_proxy_client();

    if is_stream {
        // Streaming routing:
        //   1. oMLX + think + budget → two-call stitching (thinking_budget enforcement)
        //   2. oMLX + think, no budget → existing rewriter (tag splitting)
        //   3. everything else → plain proxy_stream passthrough
        let stream_result = match (can_use_budget, thinking_budget) {
            (true, Some(budget)) => {
                // Two-call streaming: budget enforcement path.
                // body_value is passed as Value (not re-serialized Bytes) so call-2
                // can clone and modify it without re-parsing.
                proxy::proxy_stream_with_thinking_budget(
                    &client,
                    engine_port,
                    "/v1/chat/completions",
                    body_value,
                    original_max_tokens,
                    budget,
                    stream_reasoning_deltas,
                )
                .await
            }
            (true, None) => {
                // Existing think-tag rewriter (no budget cap)
                let forwarded_body = match serde_json::to_vec(&body_value) {
                    Ok(b) => Bytes::from(b),
                    Err(e) => {
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!(r#"{{"error":{{"message":"{}","type":"server_error","param":null,"code":null}}}}"#, e)))
                            .unwrap();
                    }
                };
                proxy::proxy_stream_rewriting_think_tags(
                    &client,
                    engine_port,
                    "/v1/chat/completions",
                    forwarded_body,
                )
                .await
            }
            _ => {
                // Plain passthrough (non-think, or non-oMLX engine)
                let forwarded_body = match serde_json::to_vec(&body_value) {
                    Ok(b) => Bytes::from(b),
                    Err(e) => {
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!(r#"{{"error":{{"message":"{}","type":"server_error","param":null,"code":null}}}}"#, e)))
                            .unwrap();
                    }
                };
                proxy::proxy_stream(&client, engine_port, "/v1/chat/completions", forwarded_body)
                    .await
            }
        };
        match stream_result {
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
        // Non-streaming + thinking routing:
        //   1. oMLX + thinking model + budget → two-call non-streaming
        //   2. oMLX + think, no budget → existing assembler
        //   3. non-oMLX → existing assembler
        // Hard 120-second timeout prevents infinite oMLX loops from blocking.
        let result = match (can_use_budget, thinking_budget) {
            (true, Some(budget)) => {
                tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    proxy::proxy_nonstream_with_thinking_budget(
                        &client,
                        engine_port,
                        "/v1/chat/completions",
                        body_value,
                        original_max_tokens,
                        budget,
                    ),
                )
                .await
            }
            _ => {
                let forwarded_body = match serde_json::to_vec(&body_value) {
                    Ok(b) => Bytes::from(b),
                    Err(e) => {
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!(r#"{{"error":{{"message":"{}","type":"server_error","param":null,"code":null}}}}"#, e)))
                            .unwrap();
                    }
                };
                tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    proxy::proxy_request_assembling_stream(
                        &client,
                        engine_port,
                        "/v1/chat/completions",
                        forwarded_body,
                    ),
                )
                .await
            }
        };
        match result {
            Ok(Ok((status, text))) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
            Ok(Err((status, text))) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
            Err(_elapsed) => Response::builder()
                .status(StatusCode::GATEWAY_TIMEOUT)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"error":{"message":"Inference timed out after 120 seconds","type":"server_error","param":null,"code":null}}"#,
                ))
                .unwrap(),
        }
    } else {
        // Standard non-streaming pass-through
        let forwarded_body = match serde_json::to_vec(&body_value) {
            Ok(b) => Bytes::from(b),
            Err(e) => {
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(format!(r#"{{"error":{{"message":"{}","type":"server_error","param":null,"code":null}}}}"#, e)))
                    .unwrap();
            }
        };
        match proxy::proxy_request(&client, engine_port, "/v1/chat/completions", forwarded_body)
            .await
        {
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
pub async fn completions(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let mut body_value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let model_id = body_value
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let keep_alive = body_value.get("keep_alive").and_then(|v| {
        if v.is_string() {
            Some(v.as_str().unwrap().to_string())
        } else if v.is_number() {
            Some(v.as_i64().unwrap().to_string())
        } else {
            None
        }
    });
    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(port) => port,
        Err(resp) => return resp.into_response(),
    };

    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });
    if let Some(entry) = index.get(&model_id) {
        if let Some(dir_name) = std::path::Path::new(&entry.path).file_name() {
            if let Some(obj) = body_value.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(dir_name.to_string_lossy().to_string()),
                );
            }
        }
    }

    let forwarded_body = Bytes::from(serde_json::to_vec(&body_value).unwrap_or_default());

    let client = proxy::build_proxy_client();
    match proxy::proxy_request(&client, engine_port, "/v1/completions", forwarded_body).await {
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

/// `POST /v1/embeddings` — OpenAI-compatible embeddings with batch chunking and dim auto-detection
pub async fn embeddings(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let mut body_value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let model_id = body_value
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let keep_alive = body_value.get("keep_alive").and_then(|v| {
        if v.is_string() {
            Some(v.as_str().unwrap().to_string())
        } else if v.is_number() {
            Some(v.as_i64().unwrap().to_string())
        } else {
            None
        }
    });

    // Engine-level gate: does this engine support embeddings at all?
    if !state.engine_config.supports_embeddings {
        return Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                r#"{{"error":{{"message":"Embeddings are not supported by {} v{}. This capability is available on oMLX (macOS), SGLang (Linux/NVIDIA), and llama.cpp platforms.","type":"not_supported_error","param":null,"code":null}}}}"#,
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

    // Resolve the engine-facing model directory name (needed by oMLX)
    let dir_name = index.get(&model_id).and_then(|e| {
        std::path::Path::new(&e.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    });

    let client = proxy::build_proxy_client();
    let batch_size = state.config.read().await.orchestrator.embed_batch_size;

    // --- Batch chunking ---
    // If `input` is an array with more than `batch_size` items, split into sub-batches,
    // call the engine for each, merge `data[]` arrays and sum usage tokens.
    let result: Result<(u16, String), (u16, String)> = {
        let inputs_opt = body_value
            .get("input")
            .and_then(|v| v.as_array())
            .map(|a| a.clone());

        if let Some(inputs) = inputs_opt.filter(|a| a.len() > batch_size) {
            proxy_embeddings_batched(
                &client,
                engine_port,
                dir_name.as_deref(),
                inputs,
                batch_size,
                &body_value,
            )
            .await
        } else {
            // Single string or small array -- pass through unchanged
            if let Some(ref name) = dir_name {
                if let Some(obj) = body_value.as_object_mut() {
                    obj.insert("model".to_string(), serde_json::Value::String(name.clone()));
                }
            }
            let forwarded = Bytes::from(serde_json::to_vec(&body_value).unwrap_or_default());
            proxy::proxy_request(&client, engine_port, "/v1/embeddings", forwarded).await
        }
    };

    match result {
        Ok((status, text)) => {
            // --- Dim auto-detection (fire-and-forget background task) ---
            let data_dir = state.data_dir.clone();
            let mid = model_id.clone();
            let t = text.clone();
            tokio::spawn(async move {
                maybe_update_embedding_dims(&data_dir, &mid, &t).await;
            });

            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap()
        }
        Err((status, text)) => Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(text))
            .unwrap(),
    }
}

/// Split a large input array across multiple engine calls of at most `batch_size` items,
/// merge the resulting `data[]` arrays (re-indexing `index` fields), and sum usage tokens.
async fn proxy_embeddings_batched(
    client: &reqwest::Client,
    engine_port: u16,
    dir_name: Option<&str>,
    inputs: Vec<serde_json::Value>,
    batch_size: usize,
    template: &serde_json::Value,
) -> Result<(u16, String), (u16, String)> {
    let mut merged_data: Vec<serde_json::Value> = Vec::with_capacity(inputs.len());
    let mut total_prompt_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut last_model = String::new();

    for chunk in inputs.chunks(batch_size) {
        let mut req = template.clone();
        if let Some(obj) = req.as_object_mut() {
            obj.insert(
                "input".to_string(),
                serde_json::Value::Array(chunk.to_vec()),
            );
            if let Some(name) = dir_name {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(name.to_string()),
                );
            }
        }
        let body_bytes = Bytes::from(serde_json::to_vec(&req).unwrap_or_default());

        match proxy::proxy_request(client, engine_port, "/v1/embeddings", body_bytes).await {
            Ok((status, text)) => {
                if status >= 400 {
                    return Err((status, text));
                }
                if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&text) {
                    let offset = merged_data.len();
                    if let Some(data) = resp.get("data").and_then(|d| d.as_array()) {
                        for (i, item) in data.iter().enumerate() {
                            let mut entry = item.clone();
                            if let Some(obj) = entry.as_object_mut() {
                                obj.insert("index".to_string(), serde_json::json!(offset + i));
                            }
                            merged_data.push(entry);
                        }
                    }
                    if let Some(usage) = resp.get("usage") {
                        total_prompt_tokens += usage
                            .get("prompt_tokens")
                            .and_then(|t| t.as_u64())
                            .unwrap_or(0);
                        total_tokens += usage
                            .get("total_tokens")
                            .and_then(|t| t.as_u64())
                            .unwrap_or(0);
                    }
                    if let Some(m) = resp.get("model").and_then(|m| m.as_str()) {
                        last_model = m.to_string();
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }

    let merged = serde_json::json!({
        "object": "list",
        "data": merged_data,
        "model": last_model,
        "usage": {
            "prompt_tokens": total_prompt_tokens,
            "total_tokens": total_tokens,
        }
    });

    Ok((200, serde_json::to_string(&merged).unwrap_or_default()))
}

/// Lazily update embedding_dims in models.json from the first successful /v1/embeddings response.
/// Fire-and-forget background task -- errors are silently ignored.
async fn maybe_update_embedding_dims(
    data_dir: &std::path::Path,
    model_id: &str,
    response_text: &str,
) {
    let resp: serde_json::Value = match serde_json::from_str(response_text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let dims = resp
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .and_then(|item| item.get("embedding"))
        .and_then(|e| e.as_array())
        .map(|a| a.len() as u32);

    let dims = match dims {
        Some(d) => d,
        None => return,
    };

    let mut index = match crate::model::index::ModelIndex::load(data_dir) {
        Ok(idx) => idx,
        Err(_) => return,
    };

    if let Some(entry) = index.models.iter_mut().find(|m| m.id == model_id) {
        if entry.capabilities.embedding_dims == Some(dims) {
            return; // Already correct -- skip write
        }
        debug!(model_id, dims, "Auto-detected embedding dims from response");
        entry.capabilities.embedding_dims = Some(dims);
        let _ = index.save(data_dir);
    }
}

/// `GET /v1/models` — List available models with capability metadata
pub async fn models(State(state): State<AppState>) -> impl IntoResponse {
    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });

    let data: Vec<serde_json::Value> = index
        .list()
        .iter()
        .map(|m| {
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
        })
        .collect();

    let resp = serde_json::json!({ "object": "list", "data": data });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&resp).unwrap()))
        .unwrap()
}
