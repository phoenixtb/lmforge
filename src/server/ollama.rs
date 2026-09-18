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
    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_default();

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

    let guard = match state.ensure_model_request(&model_id, keep_alive).await {
        Ok(g) => g,
        Err(resp) => return resp.into_response(),
    };
    let engine_port = guard.port();

    // Seed anti-loop sampling defaults for thinking requests with no sampling
    // (fills absent fields only; client/Ollama-options values win). Before
    // apply_think_for_engine so oMLX penalty translation sees them.
    thinking::apply_thinking_sampling_defaults(&mut openai_req, has_think);

    // Engine-aware think translation (Ollama path was previously missing this entirely)
    let model_caps = index.get(&model_id).map(|e| &e.capabilities);
    thinking::apply_think_for_engine(&mut openai_req, &state.engine_config.id, model_caps);

    // Fix #5c: floor max_tokens for native-reasoning models so reasoning can't
    // starve the answer (mirrors the OpenAI path's prepare_request).
    thinking::apply_native_reasoning_floor(&mut openai_req, model_caps);

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

    let thinking_adapter = thinking::adapter_for_engine(&state.engine_config.id);
    // oMLX native-reasoning models (dedicated reasoning_content field, not inline
    // <think>) re-emit the whole reasoning as a single content delta on
    // finish=length. Route them through the dedup proxy so the echo is stripped
    // here too (Fix #5a parity with the OpenAI path).
    let native_reasoning_dedup =
        model_caps.map(|c| c.native_reasoning).unwrap_or(false) && !thinking_adapter.inline_think();

    let response = if is_stream {
        // Streaming routing (the Ollama path does NOT use the two-call budget
        // orchestrator — that's OpenAI-path only):
        //   1. native-reasoning oMLX → dedup proxy (strip truncation echo, Fix #5a)
        //   2. think + orchestrator engine + non-inline (oMLX) → SSE think rewriter
        //   3. everything else → plain passthrough
        let openai_stream = if native_reasoning_dedup {
            proxy::proxy_stream_dedup_native_reasoning(
                &client,
                engine_port,
                "/v1/chat/completions",
                Bytes::from(openai_body),
            )
            .await
        } else if (has_think
            && thinking_adapter.supports_orchestrator()
            && !thinking_adapter.inline_think())
            // Native-reasoning models on inline-think engines (llama.cpp /
            // SGLang) emit `<think>` tags in content regardless of the think
            // flag — split them so raw tags never reach the client.
            || (model_caps.map(|c| c.native_reasoning).unwrap_or(false)
                && thinking_adapter.inline_think())
        {
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
                // Native-reasoning + inline-think engine: split `<think>` tags
                // out of content before translation (parity with streaming).
                let text = if model_caps.map(|c| c.native_reasoning).unwrap_or(false)
                    && thinking_adapter.inline_think()
                    && (200..300).contains(&status)
                {
                    thinking::split_think_in_response(&text).unwrap_or(text)
                } else {
                    text
                };
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
    };
    super::attach_inflight_guard(response, guard)
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

    let guard = match state.ensure_model_request(&model_id, keep_alive).await {
        Ok(g) => g,
        Err(resp) => return resp.into_response(),
    };
    let engine_port = guard.port();

    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_default();
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

/// `GET /api/tags` — Ollama-compatible model list
pub async fn tags(State(state): State<AppState>) -> impl IntoResponse {
    let index = crate::model::index::ModelIndex::load(&state.data_dir, &state.models_dir)
        .unwrap_or_default();

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

    // Batch 2 §2.1 — `tools` passthrough. Ollama clients (Open WebUI,
    // Continue) send `tools` already in OpenAI function-calling shape; the
    // translator previously dropped the field entirely (only
    // model/messages/stream/think/options were copied), silently breaking
    // function calling for every Ollama-API client. llama.cpp's
    // `/v1/chat/completions` accepts it verbatim.
    if let Some(tools) = ollama.get("tools") {
        openai["tools"] = tools.clone();
    }

    // Batch 2 §2.1 — `format` → `response_format`. Ollama's structured-output
    // knob: the string `"json"` is legacy whole-response JSON mode; a JSON
    // Schema object is Ollama's typed structured-output mode (same feature
    // OpenAI calls `json_schema`). Map both to the OpenAI shape the engine
    // already accepts on the OpenAI path (`chat_completions` forwards
    // `response_format` through untouched — verified in `openai.rs`).
    // Anything else (missing/empty/non-string-non-object) is left
    // untranslated rather than guessed at.
    if let Some(format) = ollama.get("format") {
        match format {
            serde_json::Value::String(s) if s == "json" => {
                openai["response_format"] = serde_json::json!({ "type": "json_object" });
            }
            serde_json::Value::Object(_) => {
                openai["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": format,
                        "strict": true
                    }
                });
            }
            _ => {}
        }
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
        if let Some(top_k) = options.get("top_k") {
            openai["top_k"] = top_k.clone();
        }
        if let Some(min_p) = options.get("min_p") {
            openai["min_p"] = min_p.clone();
        }
        // Ollama spells the repetition penalty `repeat_penalty`; map it to the
        // OpenAI-side `repetition_penalty` the engines understand. This is the
        // loop-breaker for thinking models, so it must survive translation.
        if let Some(repeat_penalty) = options.get("repeat_penalty") {
            openai["repetition_penalty"] = repeat_penalty.clone();
        }
        if let Some(presence_penalty) = options.get("presence_penalty") {
            openai["presence_penalty"] = presence_penalty.clone();
        }
        if let Some(frequency_penalty) = options.get("frequency_penalty") {
            openai["frequency_penalty"] = frequency_penalty.clone();
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
        // Batch 2 §2.1 — tool_calls accumulation (response path). OpenAI
        // streams `delta.tool_calls[i]` incrementally (name on the first
        // chunk, `arguments` appended char-by-char across subsequent
        // chunks, keyed by `index`). Ollama has no incremental tool-call
        // wire format — it surfaces the whole call once assembled — so we
        // buffer here and attach the finished array to the terminal chunk,
        // mirroring `proxy.rs`'s Call-2 tool_call_map accumulator.
        let mut tool_call_map: std::collections::BTreeMap<u64, (String, String)> =
            std::collections::BTreeMap::new();

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
                    let tool_calls = build_ollama_tool_calls(&tool_call_map);
                    let final_chunk =
                        ollama_done_chunk(&model_name, started.elapsed(), tool_calls);
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

                // Batch 2 §2.1 — accumulate tool_calls deltas regardless of
                // the empty-delta skip below (a tool-call-only delta has
                // empty content/thinking and no finish_reason, so it would
                // otherwise be dropped entirely — the bug this item fixes).
                if let Some(tc_arr) = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array()) {
                    for tc in tc_arr {
                        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let entry = tool_call_map
                            .entry(idx)
                            .or_insert_with(|| (String::new(), String::new()));
                        if let Some(func) = tc.get("function") {
                            if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                entry.0 = name.to_string();
                            }
                            if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                entry.1.push_str(args);
                            }
                        }
                    }
                }

                // Skip empty role-only deltas (Ollama clients ignore them
                // anyway and they bloat the stream with `done:false` noise).
                // A tool-call-only delta is empty by this measure too — it's
                // already captured above — so it's safe to skip here.
                if content.is_empty() && thinking.is_empty() && finish.is_none() {
                    continue;
                }

                if let Some(reason) = finish {
                    // OpenAI sends a final delta with finish_reason set —
                    // translate it directly to Ollama's done frame so we
                    // don't double-emit when [DONE] also arrives.
                    let tool_calls = build_ollama_tool_calls(&tool_call_map);
                    let chunk = ollama_done_chunk_with_reason(
                        &model_name,
                        started.elapsed(),
                        reason,
                        tool_calls,
                    );
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
            let tool_calls = build_ollama_tool_calls(&tool_call_map);
            let final_chunk = ollama_done_chunk(&model_name, started.elapsed(), tool_calls);
            yield Ok(Bytes::from(final_chunk));
        }
    };

    Body::from_stream(s)
}

/// Build the Ollama-shaped `tool_calls` array from the streaming
/// accumulator (index → (name, arguments_buf)). Returns `None` when no
/// tool call was ever started (the common, non-tool-calling case), so
/// callers can omit the field entirely rather than emit `tool_calls: []`.
///
/// Mirrors the validation discipline of `proxy.rs`'s Call-2 accumulator
/// (C2): entries with no name are dropped rather than emitted malformed.
/// `arguments` is parsed into a JSON *object* here — see
/// `translate_tool_call_to_ollama` for why (Ollama's native shape, unlike
/// OpenAI's, never stringifies `function.arguments`).
fn build_ollama_tool_calls(
    tool_call_map: &std::collections::BTreeMap<u64, (String, String)>,
) -> Option<serde_json::Value> {
    let calls: Vec<serde_json::Value> = tool_call_map
        .values()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, args_str)| {
            let arguments: serde_json::Value =
                serde_json::from_str(args_str).unwrap_or_else(|_| serde_json::json!({}));
            serde_json::json!({ "function": { "name": name, "arguments": arguments } })
        })
        .collect();
    if calls.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(calls))
    }
}

fn ollama_done_chunk(
    model: &str,
    elapsed: std::time::Duration,
    tool_calls: Option<serde_json::Value>,
) -> String {
    ollama_done_chunk_with_reason(model, elapsed, "stop", tool_calls)
}

fn ollama_done_chunk_with_reason(
    model: &str,
    elapsed: std::time::Duration,
    finish_reason: &str,
    tool_calls: Option<serde_json::Value>,
) -> String {
    let total_ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
    let mut message = serde_json::json!({ "role": "assistant", "content": "" });
    if let Some(tc) = tool_calls {
        message["tool_calls"] = tc;
    }
    let chunk = serde_json::json!({
        "model": model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "message": message,
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

    // Batch 2 §2.1 — tool_calls back-translation. llama.cpp b9861 supports
    // OpenAI-shape tool calling natively and the proxy forwards it
    // (`proxy.rs` C2), but this translator previously only read
    // `message.content`/`reasoning_content` — a tool-calling Ollama client
    // (Open WebUI, Continue) got an empty answer with no indication a tool
    // was invoked. Translate each entry to Ollama's shape (see
    // `translate_tool_call_to_ollama`).
    if let Some(tool_calls) = openai["choices"][0]["message"]["tool_calls"].as_array() {
        let translated: Vec<serde_json::Value> = tool_calls
            .iter()
            .filter_map(translate_tool_call_to_ollama)
            .collect();
        if !translated.is_empty() {
            resp["message"]["tool_calls"] = serde_json::Value::Array(translated);
        }
    }

    serde_json::to_string(&resp).unwrap_or_else(|_| openai_text.to_string())
}

/// Translate one OpenAI `tool_calls[i]` entry to Ollama's native shape.
///
/// OpenAI: `{"id":"...", "type":"function", "function":{"name":"...",
/// "arguments":"{\"a\":1}"}}` — `arguments` is a JSON-encoded *string*.
///
/// Ollama (per its documented `/api/chat` tool-calling response —
/// `docs/api.md` §"Chat request (with tools)"): `{"function":{"name":"...",
/// "arguments":{"a":1}}}` — no `id`/`type`, and `arguments` is a JSON
/// *object*, not a string. Returns `None` (dropping the entry) when the
/// function name is missing or `arguments` doesn't parse as JSON — a
/// malformed entry is worse than a missing one.
fn translate_tool_call_to_ollama(tc: &serde_json::Value) -> Option<serde_json::Value> {
    let name = tc.get("function")?.get("name")?.as_str()?;
    let args_str = tc.get("function")?.get("arguments")?.as_str()?;
    let arguments: serde_json::Value = serde_json::from_str(args_str).ok()?;
    Some(serde_json::json!({ "function": { "name": name, "arguments": arguments } }))
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

    // ── Batch 2 §2.1: tools / format passthrough ────────────────────────────

    #[test]
    fn test_translate_tools_passthrough() {
        let req = serde_json::json!({
            "model": "qwen3.5:4b",
            "messages": [{"role": "user", "content": "weather in Paris?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the weather for a location",
                    "parameters": {
                        "type": "object",
                        "properties": { "location": { "type": "string" } },
                        "required": ["location"]
                    }
                }
            }]
        });
        let out = translate_ollama_to_openai(&req);
        assert_eq!(
            out["tools"], req["tools"],
            "tools must pass through verbatim"
        );
    }

    #[test]
    fn test_translate_no_tools_field_when_absent() {
        let req = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let out = translate_ollama_to_openai(&req);
        assert!(out.get("tools").is_none());
    }

    #[test]
    fn test_translate_format_json_string_maps_to_json_object() {
        let req = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "format": "json"
        });
        let out = translate_ollama_to_openai(&req);
        assert_eq!(
            out["response_format"],
            serde_json::json!({"type": "json_object"})
        );
    }

    #[test]
    fn test_translate_format_schema_object_maps_to_json_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "age": { "type": "integer" } },
            "required": ["age"]
        });
        let req = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "format": schema.clone()
        });
        let out = translate_ollama_to_openai(&req);
        assert_eq!(out["response_format"]["type"], "json_schema");
        assert_eq!(out["response_format"]["json_schema"]["schema"], schema);
    }

    #[test]
    fn test_translate_format_absent_omits_response_format() {
        let req = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let out = translate_ollama_to_openai(&req);
        assert!(out.get("response_format").is_none());
    }

    // ── Batch 2 §2.1: tool_calls back-translation (non-stream response) ─────

    #[test]
    fn test_translate_tool_calls_back_to_ollama_shape() {
        let openai_text = serde_json::json!({
            "model": "qwen3.5:4b",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"Paris\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();

        let ollama_resp = translate_openai_to_ollama_chat(&openai_text);
        let v: serde_json::Value = serde_json::from_str(&ollama_resp).unwrap();

        let tool_calls = v["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        // Ollama shape: no id/type, arguments is an OBJECT not a string.
        assert!(tool_calls[0].get("id").is_none());
        assert!(tool_calls[0].get("type").is_none());
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert_eq!(tool_calls[0]["function"]["arguments"]["location"], "Paris");
        assert!(tool_calls[0]["function"]["arguments"].is_object());
    }

    #[test]
    fn test_translate_no_tool_calls_field_when_absent() {
        let openai_text = serde_json::json!({
            "model": "m",
            "choices": [{
                "message": { "role": "assistant", "content": "hello" },
                "finish_reason": "stop"
            }]
        })
        .to_string();
        let ollama_resp = translate_openai_to_ollama_chat(&openai_text);
        let v: serde_json::Value = serde_json::from_str(&ollama_resp).unwrap();
        assert!(v["message"].get("tool_calls").is_none());
    }

    #[test]
    fn test_translate_tool_call_to_ollama_drops_malformed_arguments() {
        // arguments isn't valid JSON — must be dropped, not passed through broken.
        let tc = serde_json::json!({
            "id": "x", "type": "function",
            "function": { "name": "f", "arguments": "not json" }
        });
        assert!(translate_tool_call_to_ollama(&tc).is_none());
    }

    // ── Batch 2 §2.1: tool_calls back-translation (streaming NDJSON) ────────

    #[tokio::test]
    async fn translate_stream_accumulates_and_emits_tool_calls_on_terminal_chunk() {
        let sse = "data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n\
                   data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"location\\\":\"}}]}}]}\n\n\
                   data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Paris\\\"}\"}}]}}]}\n\n\
                   data: {\"id\":\"x\",\"model\":\"m1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
                   data: [DONE]\n\n";
        let body = Body::from(sse.to_string());
        let translated = translate_openai_stream_to_ollama_ndjson(body);
        let bytes = axum::body::to_bytes(translated, 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();

        let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(last["done"], true);
        let tool_calls = last["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert_eq!(tool_calls[0]["function"]["arguments"]["location"], "Paris");
    }

    #[test]
    fn test_build_ollama_tool_calls_none_when_empty() {
        let map = std::collections::BTreeMap::new();
        assert!(build_ollama_tool_calls(&map).is_none());
    }

    #[test]
    fn test_build_ollama_tool_calls_drops_unnamed_entries() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(0u64, (String::new(), "{}".to_string()));
        assert!(
            build_ollama_tool_calls(&map).is_none(),
            "entry with empty name must be dropped"
        );
    }
}
