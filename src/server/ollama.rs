use async_stream::stream;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::StreamExt;
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

    // Load the index BEFORE ensure_model so we can reject vision requests for
    // non-vision models without paying the cold-start cost of loading the wrong model.
    let index = crate::model::index::ModelIndex::load(&state.data_dir).unwrap_or_else(|_| {
        crate::model::index::ModelIndex {
            schema_version: 1,
            models: vec![],
        }
    });

    // Vision capability gate: reject image_url content blocks on non-vision models.
    if let Err(resp) =
        crate::server::openai::check_vision_capability_pub(&index, &model_id, &openai_req)
    {
        return resp.into_response();
    }

    // Preflight remote image URLs (Ollama path benefits identically — it
    // shares engines with the OpenAI path, so the same UA/silent-403 problem
    // applies).
    if crate::server::openai::request_has_image(&openai_req)
        && let Err(resp) =
            crate::server::image_preflight::normalise_image_urls(&mut openai_req).await
    {
        return resp.into_response();
    }

    let engine_port = match state.ensure_model(&model_id, keep_alive).await {
        Ok(p) => p,
        Err(resp) => return resp.into_response(),
    };

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
        let openai_stream = if has_think && state.engine_config.id == "omlx" {
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
        match openai_stream {
            Ok(stream_body) => {
                // Translate the OpenAI SSE stream into Ollama-style NDJSON
                // chunks: one JSON object per line, terminated by `done:true`.
                let translated = translate_openai_stream_to_ollama_ndjson(stream_body);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/x-ndjson")
                    .body(translated)
                    .unwrap()
            }
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
/// Translate a single Ollama chat message to an OpenAI multimodal message.
///
/// Ollama wire format: `{"role": "user", "content": "describe", "images": ["<base64>"]}`
/// OpenAI wire format: `{"role": "user", "content": [{"type":"text","text":"describe"},
///                                                     {"type":"image_url","image_url":{"url":"data:image/jpeg;base64,<base64>"}}]}`
///
/// When no `images` field is present the message is returned unchanged.
fn translate_ollama_message_to_openai(msg: &serde_json::Value) -> serde_json::Value {
    let images = msg
        .get("images")
        .and_then(|i| i.as_array())
        .filter(|a| !a.is_empty());
    let Some(images) = images else {
        return msg.clone();
    };

    let role = msg
        .get("role")
        .cloned()
        .unwrap_or(serde_json::json!("user"));
    let text = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let mut content: Vec<serde_json::Value> = Vec::with_capacity(1 + images.len());
    if !text.is_empty() {
        content.push(serde_json::json!({ "type": "text", "text": text }));
    }
    for img in images {
        let s = img.as_str().unwrap_or("");
        // Ollama accepts raw base64 OR a data URL. Normalise to data URL so
        // OpenAI-compatible engines (sglang, llama.cpp) accept it directly.
        let url = if s.starts_with("data:") || s.starts_with("http://") || s.starts_with("https://")
        {
            s.to_string()
        } else {
            format!("data:image/jpeg;base64,{}", s)
        };
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": url }
        }));
    }

    serde_json::json!({ "role": role, "content": content })
}

fn translate_ollama_to_openai(ollama: &serde_json::Value) -> serde_json::Value {
    let mut openai = serde_json::json!({
        "model": ollama.get("model").cloned().unwrap_or(serde_json::Value::String("default".to_string())),
    });

    // Messages — translate Ollama `images: ["base64..."]` per message into
    // OpenAI multimodal content blocks (`{"type":"image_url",...}`). Without
    // this translation Ollama VLM clients silently lost their image inputs.
    if let Some(messages) = ollama.get("messages").and_then(|m| m.as_array()) {
        let translated: Vec<serde_json::Value> = messages
            .iter()
            .map(translate_ollama_message_to_openai)
            .collect();
        openai["messages"] = serde_json::Value::Array(translated);
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

/// Translate an OpenAI SSE chat stream into Ollama's NDJSON streaming format.
///
/// Ollama frames every chunk as a single JSON line:
///   `{"model":"...","created_at":"...","message":{"role":"assistant","content":"..."},"done":false}\n`
/// terminated by a final `done:true` line that includes `total_duration` (ns).
///
/// We map each OpenAI `delta.content` to `message.content`, propagate
/// `delta.reasoning_content` as `message.thinking` (Ollama's convention), and
/// emit a synthetic `done:true` chunk on `[DONE]` so naive Ollama clients
/// don't hang waiting for it.
fn translate_openai_stream_to_ollama_ndjson(openai: Body) -> Body {
    let started = std::time::Instant::now();
    let mut byte_stream = openai.into_data_stream();

    let s = stream! {
        let mut line_buf = String::new();
        let mut model_name = String::new();
        let mut got_terminal = false;

        while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break,
            };
            line_buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(nl) = line_buf.find('\n') {
                let raw = line_buf[..nl].trim_end_matches('\r').to_string();
                line_buf.drain(..=nl);
                let Some(payload) = raw.strip_prefix("data: ") else { continue; };
                let payload = payload.trim();

                if payload == "[DONE]" {
                    let final_chunk = ollama_done_chunk(&model_name, started.elapsed());
                    got_terminal = true;
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(final_chunk));
                    continue;
                }

                let Ok(val) = serde_json::from_str::<serde_json::Value>(payload) else { continue; };

                if model_name.is_empty()
                    && let Some(m) = val.get("model").and_then(|v| v.as_str())
                {
                    model_name = m.to_string();
                }

                let Some(choice) = val.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first())
                    else { continue; };
                let delta = choice.get("delta");
                let content = delta
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let thinking = delta
                    .and_then(|d| d.get("reasoning_content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let finish = choice.get("finish_reason").and_then(|v| v.as_str());

                // Skip empty role-only deltas (Ollama clients ignore them
                // anyway and they bloat the stream with `done:false` noise).
                if content.is_empty() && thinking.is_empty() && finish.is_none() {
                    continue;
                }

                if let Some(reason) = finish {
                    // OpenAI sends a final delta with finish_reason set —
                    // translate it directly to Ollama's done frame so we
                    // don't double-emit when [DONE] also arrives.
                    let chunk = ollama_done_chunk_with_reason(&model_name, started.elapsed(), reason);
                    got_terminal = true;
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(chunk));
                    continue;
                }

                let mut msg = serde_json::json!({
                    "role": "assistant",
                    "content": content,
                });
                if !thinking.is_empty() {
                    msg["thinking"] = serde_json::Value::String(thinking.to_string());
                }
                let chunk = serde_json::json!({
                    "model": model_name,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "message": msg,
                    "done": false,
                });
                let line = format!("{}\n", serde_json::to_string(&chunk).unwrap_or_default());
                yield Ok(Bytes::from(line));
            }
        }

        // If the upstream cut off without [DONE] or a finish_reason, still
        // emit a terminal frame so clients unblock cleanly.
        if !got_terminal {
            let final_chunk = ollama_done_chunk(&model_name, started.elapsed());
            yield Ok(Bytes::from(final_chunk));
        }
    };

    Body::from_stream(s)
}

fn ollama_done_chunk(model: &str, elapsed: std::time::Duration) -> String {
    ollama_done_chunk_with_reason(model, elapsed, "stop")
}

fn ollama_done_chunk_with_reason(
    model: &str,
    elapsed: std::time::Duration,
    finish_reason: &str,
) -> String {
    let total_ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
    let chunk = serde_json::json!({
        "model": model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "message": { "role": "assistant", "content": "" },
        "done": true,
        "done_reason": finish_reason,
        "total_duration": total_ns,
    });
    format!("{}\n", serde_json::to_string(&chunk).unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_text_only_message_unchanged() {
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let out = translate_ollama_message_to_openai(&msg);
        assert_eq!(out, msg);
    }

    #[test]
    fn test_translate_message_with_raw_base64_image_becomes_image_url_block() {
        let msg = serde_json::json!({
            "role": "user",
            "content": "what's this?",
            "images": ["AAAA"]
        });
        let out = translate_ollama_message_to_openai(&msg);
        assert_eq!(out["role"], "user");
        let content = out["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what's this?");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/jpeg;base64,AAAA"
        );
    }

    #[test]
    fn test_translate_message_with_data_url_passes_through() {
        let msg = serde_json::json!({
            "role": "user",
            "content": "describe",
            "images": ["data:image/png;base64,XYZ"]
        });
        let out = translate_ollama_message_to_openai(&msg);
        assert_eq!(
            out["content"].as_array().unwrap()[1]["image_url"]["url"],
            "data:image/png;base64,XYZ"
        );
    }

    #[test]
    fn test_translate_message_image_only_omits_empty_text_block() {
        let msg = serde_json::json!({
            "role": "user",
            "content": "",
            "images": ["AAAA"]
        });
        let out = translate_ollama_message_to_openai(&msg);
        let content = out["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "empty text should be omitted");
        assert_eq!(content[0]["type"], "image_url");
    }

    #[tokio::test]
    async fn translate_stream_emits_ndjson_with_terminal_done() {
        let sse = "data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                   data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
        let body = Body::from(sse.to_string());
        let translated = translate_openai_stream_to_ollama_ndjson(body);
        let bytes = axum::body::to_bytes(translated, 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);

        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert!(lines.len() >= 2, "expected ≥2 NDJSON lines, got: {text}");

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["message"]["content"], "hi");
        assert_eq!(first["done"], false);

        let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(last["done"], true);
        assert!(last["total_duration"].as_u64().is_some());
    }

    #[tokio::test]
    async fn translate_stream_synthesises_done_when_upstream_cuts_early() {
        let sse = "data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n";
        let body = Body::from(sse.to_string());
        let translated = translate_openai_stream_to_ollama_ndjson(body);
        let bytes = axum::body::to_bytes(translated, 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(last["done"], true);
    }

    #[tokio::test]
    async fn translate_stream_propagates_thinking_field() {
        let sse = "data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{\"reasoning_content\":\"musing\"}}]}\n\n\
                   data: [DONE]\n\n";
        let body = Body::from(sse.to_string());
        let translated = translate_openai_stream_to_ollama_ndjson(body);
        let bytes = axum::body::to_bytes(translated, 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let first_line = text.lines().find(|l| !l.is_empty()).unwrap();
        let v: serde_json::Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(v["message"]["thinking"], "musing");
    }

    #[test]
    fn test_translate_full_request_translates_each_message() {
        let req = serde_json::json!({
            "model": "qwen2.5-vl:7b:4bit",
            "messages": [
                {"role": "user", "content": "describe", "images": ["AAAA"]},
                {"role": "assistant", "content": "ok"}
            ]
        });
        let out = translate_ollama_to_openai(&req);
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[0]["content"].is_array());
        assert_eq!(messages[1]["content"], "ok");
    }
}
