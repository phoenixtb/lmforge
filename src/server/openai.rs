use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use tracing::debug;

use super::AppState;
use super::image_preflight;
use super::proxy;
use super::thinking;

/// Load the model index, returning an empty index on failure.
fn load_index(
    data_dir: &std::path::Path,
    models_dir: &std::path::Path,
) -> crate::model::index::ModelIndex {
    crate::model::index::ModelIndex::load(data_dir, models_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    })
}

/// Returns true if the request body contains any multimodal image content block.
/// Recognised shapes (per OpenAI Chat Completions spec + common SDK aliases):
///   - `{"type": "image_url",  "image_url": {...}}`
///   - `{"type": "input_image", "image_url": "..."}`  (Responses API style)
///   - `{"type": "image",       "source": {...}}`     (Anthropic-compatible)
///
/// Walks `messages[*].content` when content is an array. String content is
/// always text-only.
pub(crate) fn request_has_image(body: &serde_json::Value) -> bool {
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return false;
    };
    for msg in messages {
        let Some(content) = msg.get("content") else {
            continue;
        };
        let Some(arr) = content.as_array() else {
            continue;
        };
        for block in arr {
            let t = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(t, "image_url" | "input_image" | "image") {
                return true;
            }
        }
    }
    false
}

/// Public re-export for the Ollama handler.
#[allow(clippy::result_large_err)]
pub(crate) fn check_vision_capability_pub(
    index: &crate::model::index::ModelIndex,
    model_id: &str,
    body: &serde_json::Value,
) -> Result<(), Response<Body>> {
    check_vision_capability(index, model_id, body)
}

/// Reject requests that send images to a model without vision capability.
/// Models not in the index are allowed through (engine will surface the error).
#[allow(clippy::result_large_err)]
fn check_vision_capability(
    index: &crate::model::index::ModelIndex,
    model_id: &str,
    body: &serde_json::Value,
) -> Result<(), Response<Body>> {
    if !request_has_image(body) {
        return Ok(());
    }
    let Some(entry) = index.get(model_id) else {
        return Ok(());
    };
    if entry.capabilities.vision {
        return Ok(());
    }
    let body_msg = format!(
        r#"{{"error":{{"message":"Model '{}' does not support image input. Use a vision-language model such as 'qwen2.5-vl:3b:4bit' or 'qwen2.5-vl:7b:4bit'.","type":"invalid_request_error","param":"messages","code":"vision_not_supported"}}}}"#,
        model_id
    );
    Err(Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_msg))
        .unwrap())
}

/// Check that a model's capabilities are appropriate for the requested role.
///
/// Returns `Ok(())` if the model is suitable, or an `Err(Response)` with a 400 body
/// describing why the model cannot be used for this purpose.
/// Models not found in the index are allowed through — the engine will handle the error.
#[allow(clippy::result_large_err)] // Response<Body> is inherently large; boxing at all call sites is more disruptive
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
    let index = load_index(&state.data_dir, &state.models_dir);

    let model_caps = index.get(&model_id).map(|e| &e.capabilities);

    // Prepare body for thinking orchestration: extracts intent fields, applies
    // engine transforms + sampling defaults, strips LMForge-private fields, and
    // resolves routing flags (can_use_budget, inline_think, thinking_budget).
    let thinking_ctx = thinking::prepare_request(&mut body_value, &state.engine_config.id, model_caps);
    let has_think = thinking_ctx.has_think;
    let thinking_budget = thinking_ctx.thinking_budget;
    let stream_reasoning_deltas = thinking_ctx.stream_reasoning_deltas;
    let original_max_tokens = thinking_ctx.original_max_tokens;
    let can_use_budget = thinking_ctx.can_use_budget;
    let inline_think = thinking_ctx.inline_think;
    // oMLX native-reasoning models (reasoning_content field, not inline <think>)
    // need the Fix #1 truncation-dedup on the streaming passthrough path.
    let native_reasoning_dedup = thinking_ctx.is_native_reasoning && !inline_think;

    // oMLX only stops on the configured eos_token. Models whose chat template
    // ends the assistant turn with a different token (e.g. Phi-4's `<|end|>` vs
    // eos `<|endoftext|>`) over-generate: the engine runs past the turn end,
    // emits the role token, and regenerates a duplicate answer. Inject the
    // model's detected turn-end tokens as `stop` (client-supplied `stop` wins).
    if state.engine_config.id == "omlx" {
        let stops: Vec<String> = model_caps.map(|c| c.stop_tokens.clone()).unwrap_or_default();
        if !stops.is_empty()
            && let Some(obj) = body_value.as_object_mut()
            && !obj.contains_key("stop")
        {
            obj.insert(
                "stop".to_string(),
                serde_json::Value::Array(stops.into_iter().map(serde_json::Value::String).collect()),
            );
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

    // Vision capability gate: reject image_url content blocks for non-vision models.
    if let Err(resp) = check_vision_capability(&index, &model_id, &body_value) {
        return resp.into_response();
    }

    // Preflight image URLs: fetch remote `http(s)://` images server-side (with
    // a real User-Agent + size cap), rewrite as `data:` URLs, or 4xx on
    // failure. This stops engines from silently degrading to text-only when
    // they can't fetch the image themselves.
    if request_has_image(&body_value)
        && let Err(resp) = image_preflight::normalise_image_urls(&mut body_value).await
    {
        return resp.into_response();
    }

    let guard = match state.ensure_model_request(&model_id, keep_alive).await {
        Ok(g) => g,
        Err(resp) => return resp.into_response(),
    };
    let engine_port = guard.port();

    // Rewrite model_id to the exact filesystem directory name so engines
    // that key on the path basename (SGLang / llama.cpp / oMLX / TabbyAPI)
    // don't 404.
    //
    // vLLM is the only exception: at spawn time we pass
    // `--served-model-name <model_id>` so vLLM advertises the model under
    // our canonical id. TabbyAPI's `model_name` field is the basename of
    // the model directory, so it shares the SGLang flow.
    let needs_basename_rewrite = state.engine_config.id != "vllm";
    if needs_basename_rewrite
        && let Some(entry) = index.get(&model_id)
        && let Some(dir_name) = std::path::Path::new(&entry.path).file_name()
        && let Some(obj) = body_value.as_object_mut()
    {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(dir_name.to_string_lossy().to_string()),
        );
    }

    let client = proxy::build_proxy_client();

    let response = if is_stream {
        // Streaming routing:
        //   1. think + budget + supported engine → two-call orchestrator
        //   2. think, no budget → tag-rewriter (inline <think> splitting)
        //   3. everything else → plain passthrough
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
                    inline_think,
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
                // Plain passthrough (non-think, or non-orchestrator engine).
                // Native-reasoning oMLX models route through the dedup proxy to
                // strip the truncation echo (Fix #1); everything else streams raw.
                let forwarded_body = match serde_json::to_vec(&body_value) {
                    Ok(b) => Bytes::from(b),
                    Err(e) => {
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!(r#"{{"error":{{"message":"{}","type":"server_error","param":null,"code":null}}}}"#, e)))
                            .unwrap();
                    }
                };
                if native_reasoning_dedup {
                    proxy::proxy_stream_dedup_native_reasoning(&client, engine_port, "/v1/chat/completions", forwarded_body)
                        .await
                } else {
                    proxy::proxy_stream(&client, engine_port, "/v1/chat/completions", forwarded_body)
                        .await
                }
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
        //   1. think + budget + supported engine → two-call orchestrator
        //   2. think, no budget (or unsupported engine) → assembling-stream fallback
        // Hard 120-second timeout prevents runaway reasoning from blocking.
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
                        inline_think,
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
    };
    super::attach_inflight_guard(response, guard)
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
    let guard = match state.ensure_model_request(&model_id, keep_alive).await {
        Ok(g) => g,
        Err(resp) => return resp.into_response(),
    };
    let engine_port = guard.port();

    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_else(|_| crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        });
    // Same engine-aware rewrite as `/v1/chat/completions`. See the comment
    // there for why vLLM is exempt.
    let needs_basename_rewrite = state.engine_config.id != "vllm";
    if needs_basename_rewrite
        && let Some(entry) = index.get(&model_id)
        && let Some(dir_name) = std::path::Path::new(&entry.path).file_name()
        && let Some(obj) = body_value.as_object_mut()
    {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(dir_name.to_string_lossy().to_string()),
        );
    }

    let forwarded_body = Bytes::from(serde_json::to_vec(&body_value).unwrap_or_default());

    let client = proxy::build_proxy_client();
    let response =
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
        };
    super::attach_inflight_guard(response, guard)
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
    let index = load_index(&state.data_dir, &state.models_dir);
    if let Err(resp) = check_model_role(&index, &model_id, false, true) {
        return resp.into_response();
    }

    let guard = match state.ensure_model_request(&model_id, keep_alive).await {
        Ok(g) => g,
        Err(resp) => return resp.into_response(),
    };
    let engine_port = guard.port();

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
        let inputs_opt = body_value.get("input").and_then(|v| v.as_array()).cloned();

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
            if let Some(ref name) = dir_name
                && let Some(obj) = body_value.as_object_mut()
            {
                obj.insert("model".to_string(), serde_json::Value::String(name.clone()));
            }
            let forwarded = Bytes::from(serde_json::to_vec(&body_value).unwrap_or_default());
            proxy::proxy_request(&client, engine_port, "/v1/embeddings", forwarded).await
        }
    };

    let response = match result {
        Ok((status, text)) => {
            // --- Dim auto-detection (fire-and-forget background task) ---
            let data_dir = state.data_dir.clone();
            let models_dir = state.models_dir.clone();
            let mid = model_id.clone();
            let t = text.clone();
            tokio::spawn(async move {
                maybe_update_embedding_dims(&data_dir, &models_dir, &mid, &t).await;
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
    };
    super::attach_inflight_guard(response, guard)
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
    models_dir: &std::path::Path,
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

    let mut index = match crate::model::index::ModelIndex::load(data_dir, models_dir) {
        Ok(idx) => idx,
        Err(_) => return,
    };

    if let Some(entry) = index.models.iter_mut().find(|m| m.id == model_id) {
        if entry.capabilities.embedding_dims == Some(dims) {
            return; // Already correct -- skip write
        }
        debug!(model_id, dims, "Auto-detected embedding dims from response");
        entry.capabilities.embedding_dims = Some(dims);
        let _ = index.save(data_dir, models_dir);
    }
}

/// `GET /v1/models/{id}` — Return capability metadata for a single model.
/// Returns 404 if the model is not in the index.
pub async fn model_get(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_else(|_| crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        });

    let Some(m) = index.get(&id) else {
        let body = format!(
            r#"{{"error":{{"message":"Model '{}' not found","type":"invalid_request_error","code":"model_not_found"}}}}"#,
            id
        );
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
    };

    let payload = serde_json::json!({
        "id": m.id,
        "object": "model",
        "owned_by": "lmforge",
        "engine": m.engine,
        "format": m.format,
        "size_bytes": m.size_bytes,
        "added_at": m.added_at,
        "capabilities": {
            "chat": m.capabilities.chat,
            "embeddings": m.capabilities.embeddings,
            "reranking": m.capabilities.reranking,
            "thinking": m.capabilities.thinking,
            "native_reasoning": m.capabilities.native_reasoning,
            "vision": m.capabilities.vision,
            "embedding_dims": m.capabilities.embedding_dims,
            "mmproj_path": m.capabilities.mmproj_path,
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&payload).unwrap()))
        .unwrap()
}

/// `GET /v1/models` — List available models with capability metadata
pub async fn models(State(state): State<AppState>) -> impl IntoResponse {
    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_else(|_| crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
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
                    "native_reasoning": m.capabilities.native_reasoning,
                    "vision": m.capabilities.vision,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::index::{ModelCapabilities, ModelEntry, ModelIndex};

    fn empty_index() -> ModelIndex {
        ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    }

    fn index_with(model_id: &str, vision: bool) -> ModelIndex {
        ModelIndex {
            schema_version: 1,
            models: vec![ModelEntry {
                id: model_id.to_string(),
                path: format!("/tmp/{model_id}"),
                format: "gguf".to_string(),
                engine: "llamacpp".to_string(),
                hf_repo: None,
                size_bytes: 0,
                capabilities: ModelCapabilities {
                    chat: true,
                    vision,
                    ..Default::default()
                },
                added_at: "2026-01-01".to_string(),
            }],
        }
    }

    #[test]
    fn test_request_has_image_text_only_string() {
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(!request_has_image(&body));
    }

    #[test]
    fn test_request_has_image_text_only_array() {
        let body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello"}]
            }]
        });
        assert!(!request_has_image(&body));
    }

    #[test]
    fn test_request_has_image_image_url_block() {
        let body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what's in this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,XYZ"}}
                ]
            }]
        });
        assert!(request_has_image(&body));
    }

    #[test]
    fn test_request_has_image_input_image_block() {
        // OpenAI Responses API alias.
        let body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "input_image", "image_url": "https://example.com/cat.jpg"}]
            }]
        });
        assert!(request_has_image(&body));
    }

    #[test]
    fn test_request_has_image_anthropic_image_block() {
        let body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/jpeg", "data": "..."}
                }]
            }]
        });
        assert!(request_has_image(&body));
    }

    #[test]
    fn test_request_has_image_no_messages() {
        let body = serde_json::json!({});
        assert!(!request_has_image(&body));
    }

    #[test]
    fn test_vision_gate_text_only_against_non_vision_model() {
        let idx = index_with("qwen3:8b:4bit", false);
        let body = serde_json::json!({
            "model": "qwen3:8b:4bit",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(check_vision_capability(&idx, "qwen3:8b:4bit", &body).is_ok());
    }

    #[test]
    fn test_vision_gate_image_against_vision_model_ok() {
        let idx = index_with("qwen2.5-vl:7b:4bit", true);
        let body = serde_json::json!({
            "model": "qwen2.5-vl:7b:4bit",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,X"}}
            ]}]
        });
        assert!(check_vision_capability(&idx, "qwen2.5-vl:7b:4bit", &body).is_ok());
    }

    #[test]
    fn test_vision_gate_image_against_non_vision_model_rejected() {
        let idx = index_with("qwen3:8b:4bit", false);
        let body = serde_json::json!({
            "model": "qwen3:8b:4bit",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,X"}}
            ]}]
        });
        let err = check_vision_capability(&idx, "qwen3:8b:4bit", &body)
            .expect_err("non-vision model with image must be rejected");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_vision_gate_unknown_model_passes_through() {
        // Unknown model: let the engine handle it (consistent with check_model_role).
        let idx = empty_index();
        let body = serde_json::json!({
            "model": "unknown-model",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "https://x"}}
            ]}]
        });
        assert!(check_vision_capability(&idx, "unknown-model", &body).is_ok());
    }
}
