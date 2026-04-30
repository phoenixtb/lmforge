use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use tracing::debug;

use super::AppState;
use super::proxy;
use super::thinking;

/// `POST /api/chat` — Ollama-compatible chat endpoint
/// Translates between Ollama and OpenAI formats
pub async fn chat(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    // Parse Ollama request
    let ollama_req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"error":"Invalid JSON: {}"}}"#, e)))
                .unwrap();
        }
    };

    debug!(model = ?ollama_req.get("model"), "Ollama /api/chat request");

    // Translate to OpenAI format (also copies think field if present)
    let mut openai_req = translate_ollama_to_openai(&ollama_req);

    // Capture think intent before apply_think_for_engine removes the field
    let has_think = thinking::request_has_think(&openai_req);

    let model_id = ollama_req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let keep_alive = ollama_req.get("keep_alive").and_then(|v| {
        if v.is_string() {
            Some(v.as_str().unwrap().to_string())
        } else if v.is_number() {
            Some(v.as_i64().unwrap().to_string())
        } else {
            None
        }
    });

    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(p) => p,
        Err(resp) => return resp.into_response(),
    };

    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });

    // Engine-aware think translation (Ollama path was previously missing this entirely)
    let model_caps = index.get(&model_id).map(|e| &e.capabilities);
    thinking::apply_think_for_engine(&mut openai_req, &state.engine_config.id, model_caps);

    if let Some(entry) = index.get(&model_id)
        && let Some(dir_name) = std::path::Path::new(&entry.path).file_name()
        && let Some(obj) = openai_req.as_object_mut()
    {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(dir_name.to_string_lossy().to_string()),
        );
    }

    let openai_body = serde_json::to_vec(&openai_req).unwrap_or_default();

    let is_stream = ollama_req
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let client = proxy::build_proxy_client();

    if is_stream {
        // For oMLX+think, rewrite <think> tags in delta.content into delta.reasoning_content.
        // All other streaming combinations use plain passthrough.
        let stream_result = if has_think && state.engine_config.id == "omlx" {
            proxy::proxy_stream_rewriting_think_tags(
                &client,
                engine_port,
                "/v1/chat/completions",
                Bytes::from(openai_body),
            )
            .await
        } else {
            proxy::proxy_stream(
                &client,
                engine_port,
                "/v1/chat/completions",
                Bytes::from(openai_body),
            )
            .await
        };
        match stream_result {
            Ok(stream_body) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/x-ndjson")
                .body(stream_body)
                .unwrap(),
            Err((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
        }
    } else if has_think {
        // Non-streaming + think: assemble stream internally, then translate to Ollama format
        match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            proxy::proxy_request_assembling_stream(
                &client,
                engine_port,
                "/v1/chat/completions",
                Bytes::from(openai_body),
            ),
        )
        .await
        {
            Ok(Ok((status, text))) => {
                let ollama_resp = translate_openai_to_ollama_chat(&text);
                Response::builder()
                    .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(ollama_resp))
                    .unwrap()
            }
            Ok(Err((status, text))) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
            Err(_elapsed) => Response::builder()
                .status(StatusCode::GATEWAY_TIMEOUT)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"error":{"message":"Inference timed out after 120 seconds","type":"server_error"}}}"#,
                ))
                .unwrap(),
        }
    } else {
        match proxy::proxy_request(
            &client,
            engine_port,
            "/v1/chat/completions",
            Bytes::from(openai_body),
        )
        .await
        {
            Ok((status, text)) => {
                // Translate OpenAI response back to Ollama format
                let ollama_resp = translate_openai_to_ollama_chat(&text);
                Response::builder()
                    .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(ollama_resp))
                    .unwrap()
            }
            Err((status, text)) => Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(text))
                .unwrap(),
        }
    }
}

/// `POST /api/generate` — Ollama-compatible generate endpoint
pub async fn generate(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
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
    if let Some(obj) = body_value.as_object_mut() {
        obj.remove("keep_alive");
    }

    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(p) => p,
        Err(resp) => return resp.into_response(),
    };

    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });
    if let Some(entry) = index.get(&model_id)
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

/// `GET /api/tags` — Ollama-compatible model list
pub async fn tags(State(state): State<AppState>) -> impl IntoResponse {
    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });

    let models: Vec<serde_json::Value> = index
        .list()
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.id,
                "model": m.id,
                "modified_at": null,
            })
        })
        .collect();

    let resp = serde_json::json!({ "models": models });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&resp).unwrap()))
        .unwrap()
}

/// Translate Ollama chat request to OpenAI format
fn translate_ollama_to_openai(ollama: &serde_json::Value) -> serde_json::Value {
    let mut openai = serde_json::json!({
        "model": ollama.get("model").cloned().unwrap_or(serde_json::Value::String("default".to_string())),
    });

    // Messages
    if let Some(messages) = ollama.get("messages") {
        openai["messages"] = messages.clone();
    }

    // Stream
    if let Some(stream) = ollama.get("stream") {
        openai["stream"] = stream.clone();
    }

    // Think mode
    if let Some(think) = ollama.get("think") {
        openai["think"] = think.clone();
    }

    // Options translation
    if let Some(options) = ollama.get("options").and_then(|o| o.as_object()) {
        if let Some(temp) = options.get("temperature") {
            openai["temperature"] = temp.clone();
        }
        if let Some(num_predict) = options.get("num_predict") {
            openai["max_tokens"] = num_predict.clone();
        }
        if let Some(num_ctx) = options.get("num_ctx") {
            openai["num_ctx"] = num_ctx.clone();
        }
        if let Some(top_p) = options.get("top_p") {
            openai["top_p"] = top_p.clone();
        }
    }

    openai
}

/// Translate OpenAI chat response to Ollama format
fn translate_openai_to_ollama_chat(openai_text: &str) -> String {
    let Ok(openai) = serde_json::from_str::<serde_json::Value>(openai_text) else {
        return openai_text.to_string();
    };

    let content = openai["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");

    let reasoning = openai["choices"][0]["message"]["reasoning_content"].as_str();

    let mut resp = serde_json::json!({
        "model": openai.get("model").cloned().unwrap_or(serde_json::Value::Null),
        "message": {
            "role": "assistant",
            "content": content,
        },
        "done": true,
    });

    // Include thinking field if present
    if let Some(reasoning) = reasoning {
        resp["message"]["thinking"] = serde_json::Value::String(reasoning.to_string());
    }

    serde_json::to_string(&resp).unwrap_or_else(|_| openai_text.to_string())
}
