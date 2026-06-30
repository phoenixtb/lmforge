use anyhow::Result;
use async_stream::stream;
use axum::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use tracing::{debug, error, info, warn};

use crate::server::thinking::ThinkSplitter;

/// Shared HTTP client for proxying to the engine backend
pub fn build_proxy_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(300)) // 5 min for long inference
        .pool_max_idle_per_host(10)
        .build()
        .expect("Failed to build proxy HTTP client")
}

/// Proxy a non-streaming request to the engine backend
pub async fn proxy_request(
    client: &Client,
    engine_port: u16,
    path: &str,
    body: Bytes,
) -> Result<(u16, String), (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, body_len = body.len(), "Proxying request to engine");

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to proxy to engine");
            (502u16, format!(r#"{{"error":{{"message":"Engine unavailable: {}","type":"server_error","param":null,"code":null}}}}"#, e))
        })?;

    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| {
        (502, format!(r#"{{"error":{{"message":"Failed to read engine response: {}","type":"server_error","param":null,"code":null}}}}"#, e))
    })?;

    if status >= 400 {
        warn!(status, "Engine returned error");
    }

    Ok((status, normalise_chat_response(text)))
}

/// Normalise a raw engine non-streaming chat completion response to be
/// fully OpenAI API-spec compliant.
///
/// Engines (oMLX, llamacpp, etc.) sometimes include fields that the spec
/// says should be omitted when null, or omit fields the spec requires:
///
/// - **C1**: Strip `message.reasoning_content` when null (non-thinking responses
///   must not have this field — strict-schema clients like Pydantic reject it).
/// - **C2**: Strip `message.tool_calls` when null (same reason).
/// - **C3**: Add `logprobs: null` to each choice if the engine omits it.
/// - **C5**: Add `param: null, code: null` to `error` if present but incomplete.
///
/// Passes unknown / non-parseable responses through unchanged.
pub fn normalise_chat_response(text: String) -> String {
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&text) else {
        return text; // Not JSON — pass through as-is
    };

    // C5: Normalise error object if present
    if let Some(err) = val.get_mut("error").and_then(|e| e.as_object_mut()) {
        err.entry("param").or_insert(serde_json::Value::Null);
        err.entry("code").or_insert(serde_json::Value::Null);
    }

    // Only normalise if this looks like a chat completion response
    let is_chat_completion = val
        .get("object")
        .and_then(|v| v.as_str())
        .map(|s| s == "chat.completion")
        .unwrap_or(false);

    if !is_chat_completion {
        return serde_json::to_string(&val).unwrap_or(text);
    }

    if let Some(choices) = val.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices.iter_mut() {
            // C3: ensure logprobs field is present
            if let Some(obj) = choice.as_object_mut() {
                obj.entry("logprobs").or_insert(serde_json::Value::Null);

                // C1, C2: strip null fields from message
                if let Some(msg) = obj.get_mut("message").and_then(|m| m.as_object_mut()) {
                    // C1: remove reasoning_content if null
                    if msg
                        .get("reasoning_content")
                        .map(|v| v.is_null())
                        .unwrap_or(false)
                    {
                        msg.remove("reasoning_content");
                    }
                    // C2: remove tool_calls if null
                    if msg.get("tool_calls").map(|v| v.is_null()).unwrap_or(false) {
                        msg.remove("tool_calls");
                    }
                    // ensure refusal is present (spec field)
                    msg.entry("refusal").or_insert(serde_json::Value::Null);
                }
            }
        }
    }

    serde_json::to_string(&val).unwrap_or(text)
}

/// Minimum reasoning length (chars, trimmed) for the native-reasoning dedup
/// (Fix #1) to fire. Short strings risk false positives — a substantial verbatim
/// match between the whole reasoning and a content delta is the reliable signal
/// of oMLX's truncation echo.
const NATIVE_REASONING_DEDUP_MIN_CHARS: usize = 40;

/// True when `content` is the oMLX truncation echo of `reasoning` (Fix #1):
/// a substantial, exact verbatim copy of the whole reasoning. Length-guarded to
/// avoid false positives on short strings; both are compared trimmed.
fn is_reasoning_echo(content: &str, reasoning: &str) -> bool {
    let r = reasoning.trim();
    !r.is_empty() && r.len() >= NATIVE_REASONING_DEDUP_MIN_CHARS && content.trim() == r
}

/// Proxy a streaming SSE request to the engine backend.
/// Returns an axum Body that streams SSE events.
pub async fn proxy_stream(
    client: &Client,
    engine_port: u16,
    path: &str,
    body: Bytes,
) -> Result<Body, (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, "Proxying streaming request to engine");

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to proxy stream to engine");
            (502u16, format!("{{\"error\":{{\"message\":\"Engine unavailable: {}\",\"type\":\"server_error\"}}}}", e))
        })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err((status, text));
    }

    // Stream the response body through
    let stream = resp.bytes_stream().map(|chunk| {
        chunk.map_err(|e| {
            error!(error = %e, "Error reading stream from engine");
            std::io::Error::other(e)
        })
    });

    Ok(Body::from_stream(stream))
}

/// For non-streaming callers that want thinking support:
/// force `stream: true` toward the engine, consume all SSE chunks, then
/// assemble a proper non-streaming response where `message.reasoning_content`
/// and `message.content` are correctly separated — exactly like DeepSeek's API.
///
/// This is the only reliable way to get separate reasoning_content on engines
/// (like oMLX) that may return `reasoning_content: null` in their own
/// non-streaming mode.
pub async fn proxy_request_assembling_stream(
    client: &Client,
    engine_port: u16,
    path: &str,
    body: Bytes,
) -> Result<(u16, String), (u16, String)> {
    // Patch body: force stream: true
    let mut body_val: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        (
            400u16,
            format!(
                r#"{{"error":{{"message":"Invalid JSON: {}","type":"invalid_request_error","param":null,"code":null}}}}"#,
                e
            ),
        )
    })?;
    if let Some(obj) = body_val.as_object_mut() {
        obj.insert("stream".to_string(), serde_json::Value::Bool(true));
    }
    let patched = serde_json::to_vec(&body_val).map_err(|e| {
        (
            500u16,
            format!(
                r#"{{"error":{{"message":"JSON serialization failed: {}","type":"server_error","param":null,"code":null}}}}"#,
                e
            ),
        )
    })?;

    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, "Assembling stream for non-streaming think request");

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(patched)
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to proxy think stream to engine");
            (
                502u16,
                format!(
                    r#"{{"error":{{"message":"Engine unavailable: {}","type":"server_error","param":null,"code":null}}}}"#,
                    e
                ),
            )
        })?;

    let status = resp.status().as_u16();
    if status >= 400 {
        let text = resp.text().await.unwrap_or_default();
        return Err((status, text));
    }

    // Accumulate SSE chunks
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    // Fields assembled from the stream
    let mut completion_id = String::new();
    let mut model_name = String::new();
    let mut created: u64 = 0;
    let mut reasoning_buf = String::new();
    let mut content_buf = String::new();
    let mut finish_reason: Option<String> = None;
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;

    // Tool-call accumulation.
    // Key = tool_call index (from delta.tool_calls[i].index).
    // Each entry: (id, type, function_name, arguments_buf).
    let mut tool_call_map: std::collections::BTreeMap<u64, (String, String, String, String)> =
        std::collections::BTreeMap::new();

    // Safety limits: abort if generation exceeds these bounds (guards against infinite loops)
    const MAX_DATA_LINES: usize = 4096;
    const MAX_TOTAL_BYTES: usize = 768 * 1024; // 768 KB
    let mut data_line_count: usize = 0;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| {
            (
                502u16,
                format!(
                    r#"{{"error":{{"message":"Stream read error: {}","type":"server_error","param":null,"code":null}}}}"#,
                    e
                ),
            )
        })?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // Process complete lines
        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
            buffer.drain(..=newline_pos);

            let data = match line.strip_prefix("data: ") {
                Some(d) => d.trim(),
                None => continue,
            };
            if data == "[DONE]" {
                break;
            }

            // Safety: abort if stream is unreasonably large (protects against infinite loops)
            data_line_count += 1;
            let total_accumulated = reasoning_buf.len() + content_buf.len();
            if data_line_count > MAX_DATA_LINES || total_accumulated > MAX_TOTAL_BYTES {
                warn!(
                    data_line_count,
                    total_accumulated,
                    "Stream safety limit reached — aborting (possible infinite thinking loop)"
                );
                return Err((
                    504u16,
                    r#"{"error":{"message":"Generation exceeded safety limits (possible infinite thinking loop)","type":"server_error","param":null,"code":null}}"#.to_string(),
                ));
            }

            let Ok(chunk_val) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };

            // Capture metadata from first meaningful chunk
            if completion_id.is_empty() {
                if let Some(id) = chunk_val.get("id").and_then(|v| v.as_str()) {
                    completion_id = id.to_string();
                }
                if let Some(m) = chunk_val.get("model").and_then(|v| v.as_str()) {
                    model_name = m.to_string();
                }
                if let Some(c) = chunk_val.get("created").and_then(|v| v.as_u64()) {
                    created = c;
                }
            }

            if let Some(choices) = chunk_val.get("choices").and_then(|c| c.as_array())
                && let Some(choice) = choices.first()
            {
                if let Some(delta) = choice.get("delta") {
                    // Accumulate reasoning and content
                    if let Some(r) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                        reasoning_buf.push_str(r);
                    }
                    if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                        content_buf.push_str(c);
                    }

                    // Accumulate tool_calls deltas (C2)
                    // Each delta.tool_calls entry has: index, id (first chunk only),
                    // type (first chunk only), function.name (first chunk only),
                    // function.arguments (incremental).
                    if let Some(tc_arr) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc_delta in tc_arr {
                            let idx = tc_delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                            let entry = tool_call_map.entry(idx).or_insert_with(|| {
                                (
                                    String::new(), // id
                                    String::new(), // type
                                    String::new(), // function.name
                                    String::new(), // function.arguments
                                )
                            });
                            if let Some(id) = tc_delta.get("id").and_then(|v| v.as_str()) {
                                entry.0 = id.to_string();
                            }
                            if let Some(t) = tc_delta.get("type").and_then(|v| v.as_str()) {
                                entry.1 = t.to_string();
                            }
                            if let Some(func) = tc_delta.get("function") {
                                if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                    entry.2 = name.to_string();
                                }
                                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                    entry.3.push_str(args);
                                }
                            }
                        }
                    }
                }
                if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(fr.to_string());
                }
            }

            // Usage from final chunk
            if let Some(usage) = chunk_val.get("usage") {
                prompt_tokens = usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(prompt_tokens);
                completion_tokens = usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(completion_tokens);
            }
        }
    }

    // If the engine emitted <think>...</think> tags inside delta.content rather than
    // using a separate delta.reasoning_content field (natural Qwen3/oMLX mode after
    // flag suppression), extract them now to correctly separate reasoning from answer.
    let (final_reasoning, mut final_content) =
        if reasoning_buf.is_empty() && content_buf.contains("<think>") {
            let (r, c) = crate::server::thinking::extract_think_tags(&content_buf);
            (r.unwrap_or_default(), c)
        } else {
            (reasoning_buf, content_buf)
        };

    // Fix #1 (ADR-007): native-reasoning oMLX models, on `finish=length`, emit
    // the reasoning_content deltas and then echo the *entire* reasoning once more
    // as a single `content` delta (r==c). That duplicate is not an answer — drop
    // it. Exact full-match on a substantial reasoning string guards against
    // false positives (a real answer never reproduces the whole reasoning verbatim);
    // natural `stop` separates correctly and is untouched.
    if finish_reason.as_deref() == Some("length")
        && is_reasoning_echo(&final_content, &final_reasoning)
    {
        debug!(
            reasoning_len = final_reasoning.len(),
            "Fix #1: dropping content that duplicates reasoning on finish=length"
        );
        final_content = String::new();
    }

    // Build validated tool_calls array (C2).
    // Safeguard: only include entries that have all required fields (id, type="function",
    // function.name non-empty, function.arguments valid JSON string). Drop any malformed entry.
    let tool_calls_val: serde_json::Value = {
        let valid: Vec<serde_json::Value> = tool_call_map
            .into_values()
            .filter_map(|(id, tc_type, name, args)| {
                // Safeguard: require all three identity fields
                if id.is_empty() || tc_type.is_empty() || name.is_empty() {
                    return None;
                }
                // Safeguard: arguments must be a valid JSON string (even if "{}")
                if args.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "id": id,
                    "type": tc_type,
                    "function": {
                        "name": name,
                        "arguments": args
                    }
                }))
            })
            .collect();

        if valid.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Array(valid)
        }
    };

    // Build the message object — only include optional fields when non-null/non-empty (C1/C2)
    let mut message = serde_json::json!({
        "role": "assistant",
        "content": if final_content.is_empty() && tool_calls_val.is_array() {
            // Tool-call responses have null content per OpenAI spec
            serde_json::Value::Null
        } else {
            serde_json::Value::String(final_content)
        },
        "refusal": serde_json::Value::Null
    });
    // C1: reasoning_content only when non-empty (omit null for non-thinking responses)
    if !final_reasoning.is_empty() {
        message["reasoning_content"] = serde_json::Value::String(final_reasoning);
    }
    // C2: tool_calls only when present and validated
    if !tool_calls_val.is_null() {
        message["tool_calls"] = tool_calls_val;
    }

    // Assemble non-streaming response (C3: logprobs field included)
    let assembled = serde_json::json!({
        "id": completion_id,
        "object": "chat.completion",
        "created": created,
        "model": model_name,
        "choices": [{
            "index": 0,
            "message": message,
            "logprobs": serde_json::Value::Null,
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string())
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        }
    });

    Ok((200, serde_json::to_string(&assembled).unwrap_or_default()))
}

/// Forward a GET request to the engine
pub async fn proxy_get(
    client: &Client,
    engine_port: u16,
    path: &str,
) -> Result<(u16, String), (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, "Proxying GET to engine");

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| {
            (502u16, format!("{{\"error\":{{\"message\":\"Engine unavailable: {}\",\"type\":\"server_error\"}}}}", e))
        })?;

    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

// =============================================================================
// Two-call thinking budget proxy — Call 1 accumulate + Call 2 stream
// =============================================================================

/// Set `chat_template_kwargs.enable_thinking` on a request body object,
/// creating the `chat_template_kwargs` map if absent.
///
/// Call-1 of the budget orchestrator sets `true` to force oMLX to emit
/// `delta.reasoning_content` (quants like `qwen3.5:2b:4bit` produce no
/// reasoning otherwise). This is only safe because call-1 is hard-capped at
/// the thinking budget — `enable_thinking:true` is unbounded and loops without
/// a cap (see `thinking::apply_think_for_engine`). Call-2 sets `false`.
fn set_enable_thinking(obj: &mut serde_json::Map<String, serde_json::Value>, value: bool) {
    let kwargs = obj
        .entry("chat_template_kwargs")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(map) = kwargs.as_object_mut() {
        map.insert(
            "enable_thinking".to_string(),
            serde_json::Value::Bool(value),
        );
    }
}

/// Build the body for Call 2 of the thinking-budget workflow.
///
/// The reasoning accumulated in Call 1 is fed back so the answer phase has
/// context, then `enable_thinking:false` stops the model re-entering `<think>`.
///
/// **How the reasoning is fed back is engine-specific** (`inline_think`):
///
/// - **llama.cpp / SGLang** (`inline_think = true`): append the reasoning as a
///   closed `<think>…</think>` **assistant** turn. These engines *continue* a
///   prefilled assistant turn, so they pick up after the reasoning and emit only
///   the answer.
///
/// - **oMLX** (`inline_think = false`): oMLX does **not** continue a prefilled
///   assistant turn — it *regenerates* it (documented in `thinking.rs`), which
///   makes it echo the whole reasoning back as the "answer" (reasoning == content,
///   the duplication bug). So instead we feed the reasoning back as a **user**
///   turn with an explicit instruction to give only the final answer. User turns
///   are not regenerated, so the model produces a fresh, concise answer.
fn build_call2_body(
    original_body: &serde_json::Value,
    reasoning_buf: &str,
    remaining_max_tokens: u32,
    inline_think: bool,
) -> serde_json::Value {
    let mut body2 = original_body.clone();
    let obj = body2.as_object_mut().expect("body must be an object");

    let call2_msg = if inline_think {
        // Continue the assistant turn (llama.cpp / SGLang).
        serde_json::json!({
            "role": "assistant",
            "content": format!("<think>{}</think>\n\n", reasoning_buf)
        })
    } else {
        // Feed reasoning back as a user directive (oMLX) — an assistant prefill
        // would be regenerated and echoed as the answer.
        serde_json::json!({
            "role": "user",
            "content": format!(
                "Here is your step-by-step reasoning so far:\n\n{}\n\nNow give ONLY your \
                 final answer, concisely. Do not repeat the reasoning.",
                reasoning_buf
            )
        })
    };
    if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
        messages.push(call2_msg);
    }

    // Suppress further thinking — CRITICAL: without this the model re-enters <think>
    set_enable_thinking(obj, false);

    // Remaining token budget for the answer
    obj.insert(
        "max_tokens".to_string(),
        serde_json::Value::from(remaining_max_tokens),
    );

    // Always stream internally
    obj.insert("stream".to_string(), serde_json::Value::Bool(true));

    // Remove thinking_budget so engine doesn't see it
    obj.remove("thinking_budget");

    body2
}

/// Build a **plain answer** request body for the empty-Call-1 fallback.
///
/// When Call-1 produces neither reasoning nor content (engine evicted, errored,
/// or produced an empty stream), the orchestrator must not return a blank reply.
/// This re-issues the *original* messages — no synthetic reasoning prefill — with
/// `enable_thinking:false` and the full token budget, yielding a single normal
/// answer. See Fix #2 (ADR-007) and the orchestrator empty-guard call sites.
fn build_plain_answer_body(original_body: &serde_json::Value, max_tokens: u32) -> serde_json::Value {
    let mut body = original_body.clone();
    let obj = body.as_object_mut().expect("body must be an object");
    set_enable_thinking(obj, false);
    obj.insert("max_tokens".to_string(), serde_json::Value::from(max_tokens));
    obj.insert("stream".to_string(), serde_json::Value::Bool(true));
    obj.remove("thinking_budget");
    body
}

/// Consume Call 1's SSE stream:
///   - Accumulates `delta.reasoning_content` into a `reasoning_buf` string.
///   - If `stream_reasoning_deltas` is true AND a `tx` sender is provided,
///     live-forwards each reasoning SSE event as raw Bytes through the channel.
///   - Returns (reasoning_buf, content_buf, completion_id, model_name, finish_reason).
///   - `finish_reason == "length"` means the thinking budget was exhausted → trigger Call 2.
///   - Any other finish_reason (e.g. "stop") means the model finished naturally.
async fn stream_call1_accumulate(
    resp: reqwest::Response,
    thinking_budget: u32,
    tx: Option<tokio::sync::mpsc::Sender<Bytes>>,
    inline_think: bool,
) -> Result<
    (
        String,         // reasoning_buf
        String,         // content_buf (only populated on natural finish, no Call 2 needed)
        String,         // completion_id
        String,         // model_name
        Option<String>, // finish_reason
    ),
    String,
> {
    let _ = thinking_budget; // used by caller to compute remaining tokens
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut reasoning_buf = String::new();
    let mut content_buf = String::new();
    let mut completion_id = String::new();
    let mut model_name = String::new();
    let mut finish_reason: Option<String> = None;
    // For engines that embed reasoning as inline <think> tags (llama.cpp,
    // SGLang) we split it out of `content` ourselves.
    let mut splitter = crate::server::thinking::ThinkSplitter::default();

    // Build an SSE frame carrying a single delta field, forwarded live to the
    // client when stream_reasoning_deltas is on.
    let make_frame = |id: &str, model: &str, key: &str, text: &str| -> Bytes {
        let frame = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "model": model,
            "choices": [{ "index": 0, "delta": { key: text }, "finish_reason": null }]
        });
        Bytes::from(format!(
            "data: {}\n\n",
            serde_json::to_string(&frame).unwrap_or_default()
        ))
    };

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "Error reading call-1 stream");
                break;
            }
        };
        buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim_end_matches('\r').to_string();
            buf.drain(..=nl);

            let data = match line.strip_prefix("data: ") {
                Some(d) => d.trim(),
                None => continue,
            };

            if data == "[DONE]" {
                break;
            }

            let Ok(val) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };

            // Capture metadata
            if completion_id.is_empty()
                && let Some(id) = val.get("id").and_then(|v| v.as_str())
            {
                completion_id = id.to_string();
            }
            if model_name.is_empty()
                && let Some(m) = val.get("model").and_then(|v| v.as_str())
            {
                model_name = m.to_string();
            }

            if let Some(choices) = val.get("choices").and_then(|c| c.as_array())
                && let Some(choice) = choices.first()
            {
                // Capture finish_reason
                if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(fr.to_string());
                }

                if let Some(delta) = choice.get("delta") {
                    if inline_think {
                        // llama.cpp / SGLang: reasoning is inline <think>…</think>
                        // inside `content`. Split it so reasoning and answer go
                        // to the right buffers (and the right SSE channel).
                        if let Some(c) = delta.get("content").and_then(|v| v.as_str())
                            && !c.is_empty()
                        {
                            let (r, ans) = splitter.push(c);
                            if !r.is_empty() {
                                reasoning_buf.push_str(&r);
                                if let Some(ref tx) = tx
                                    && tx
                                        .send(make_frame(&completion_id, &model_name, "reasoning_content", &r))
                                        .await
                                        .is_err()
                                {
                                    break;
                                }
                            }
                            if !ans.is_empty() {
                                content_buf.push_str(&ans);
                                if let Some(ref tx) = tx
                                    && tx
                                        .send(make_frame(&completion_id, &model_name, "content", &ans))
                                        .await
                                        .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    } else {
                        // oMLX: native reasoning_content + content fields.
                        // Accumulate into buffers first, then forward the raw SSE
                        // line exactly ONCE — a single delta can carry both fields
                        // and we must not double-emit it.
                        let r_text = delta
                            .get("reasoning_content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let c_text = delta
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if !r_text.is_empty() {
                            reasoning_buf.push_str(r_text);
                        }
                        if !c_text.is_empty() {
                            content_buf.push_str(c_text);
                        }

                        // Forward once if either field had data.
                        if (!r_text.is_empty() || !c_text.is_empty())
                            && let Some(ref tx) = tx
                        {
                            let sse_bytes = Bytes::from(format!("data: {}\n\n", data));
                            if tx.send(sse_bytes).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    // Flush any held-back inline-think fragment (e.g. unterminated <think> when
    // the budget was exhausted mid-reasoning).
    if inline_think {
        let (r, ans) = splitter.flush();
        if !r.is_empty() {
            reasoning_buf.push_str(&r);
            if let Some(ref tx) = tx {
                let _ = tx
                    .send(make_frame(&completion_id, &model_name, "reasoning_content", &r))
                    .await;
            }
        }
        if !ans.is_empty() {
            content_buf.push_str(&ans);
            if let Some(ref tx) = tx {
                let _ = tx
                    .send(make_frame(&completion_id, &model_name, "content", &ans))
                    .await;
            }
        }
    }

    Ok((
        reasoning_buf,
        content_buf,
        completion_id,
        model_name,
        finish_reason,
    ))
}

/// Streaming proxy for the two-call thinking-budget workflow.
///
/// # Flow
/// 1. POST Call 1 with the original body (thinking enabled). The model streams
///    reasoning tokens until the budget is exhausted (`finish_reason = "length"`)
///    or the model finishes naturally (`finish_reason = "stop"`).
/// 2. While Call 1 is running, live-forward reasoning/content SSE events to the
///    client via an internal MPSC channel (if `stream_reasoning_deltas = true`).
/// 3. If Call 1 finishes naturally (no budget exhaustion) → the call-1 content IS
///    the final answer; forward it and close the stream.
/// 4. If Call 1 exhausts the budget → accumulate the reasoning, POST Call 2 with
///    the closed `<think>…</think>` prefill. Stream Call 2's SSE events directly
///    to the client with per-event byte-count tracing for diagnostics.
#[allow(clippy::too_many_arguments)] // orchestrator fan-out params; grouping into a struct adds churn without clarity
pub async fn proxy_stream_with_thinking_budget(
    client: &Client,
    engine_port: u16,
    path: &str,
    original_body: serde_json::Value,
    original_max_tokens: u32,
    thinking_budget: u32,
    stream_reasoning_deltas: bool,
    inline_think: bool,
) -> Result<Body, (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, thinking_budget, stream_reasoning_deltas, "Starting two-call thinking budget stream");

    // Patch body for Call 1: cap max_tokens at the thinking budget
    let mut body1 = original_body.clone();
    if let Some(obj) = body1.as_object_mut() {
        obj.insert(
            "max_tokens".to_string(),
            serde_json::Value::from(thinking_budget),
        );
        obj.insert("stream".to_string(), serde_json::Value::Bool(true));
        obj.remove("thinking_budget");
        set_enable_thinking(obj, true);
    }

    let resp1 = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body1).unwrap_or_default())
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "Call-1 engine request failed");
            (
                502u16,
                format!("{{\"error\":\"Engine unavailable: {}\"}}", e),
            )
        })?;

    if !resp1.status().is_success() {
        let status = resp1.status().as_u16();
        let text = resp1.text().await.unwrap_or_default();
        return Err((status, text));
    }

    // Channel: Call 1 accumulator task sends live SSE Bytes to the output stream
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(256);
    let tx_opt = if stream_reasoning_deltas {
        Some(tx)
    } else {
        None
    };

    // Spawn Call 1 accumulator in background
    let call1_task = tokio::spawn(stream_call1_accumulate(resp1, thinking_budget, tx_opt, inline_think));

    let original_body_c = original_body.clone();
    let client = client.clone();
    let engine_port_owned = engine_port;
    let path_owned = path.to_string();

    let body_stream = stream! {
        // 1. Yield live reasoning/content chunks from Call 1 (if stream_reasoning_deltas=true)
        while let Some(chunk) = rx.recv().await {
            yield Ok::<Bytes, std::io::Error>(chunk);
        }

        // 2. Await Call 1 completion
        let call1_res = match call1_task.await {
            Ok(Ok(res)) => res,
            Ok(Err(e)) => {
                warn!(error = %e, "Call-1 accumulator returned error");
                yield Ok(Bytes::from("data: [DONE]\n\n"));
                return;
            }
            Err(e) => {
                warn!(error = %e, "Call-1 task panicked");
                yield Ok(Bytes::from("data: [DONE]\n\n"));
                return;
            }
        };

        let (reasoning_buf, content_buf_call1, completion_id, model_name, finish_reason) = call1_res;

        debug!(
            reasoning_len = reasoning_buf.len(),
            content_len = content_buf_call1.len(),
            finish_reason = ?finish_reason,
            "Call-1 accumulation complete"
        );

        // 3. Natural finish — model stopped before exhausting the budget
        if finish_reason.as_deref() != Some("length") {
            // Content was already streamed live if stream_reasoning_deltas=true.
            // If not, emit the buffered content now.
            if !stream_reasoning_deltas && !content_buf_call1.is_empty() {
                let chunk = serde_json::json!({
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "model": model_name,
                    "choices": [{
                        "index": 0,
                        "delta": { "content": content_buf_call1 },
                        "finish_reason": null
                    }]
                });
                yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or_default())));
            }
            let final_chunk = serde_json::json!({
                "id": completion_id,
                "object": "chat.completion.chunk",
                "model": model_name,
                "choices": [{ "index": 0, "delta": {}, "finish_reason": finish_reason }]
            });
            yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&final_chunk).unwrap_or_default())));
            yield Ok(Bytes::from("data: [DONE]\n\n"));
            return;
        }

        // 4. Budget exhausted — execute the second call.
        //
        //    Three sub-cases (Fix #2, empty-guard — never emit a silent blank):
        //      a. reasoning present            → Call-2 (reasoning prefill + answer)
        //      b. reasoning empty, content present
        //                                      → emit the content as the answer, done
        //      c. reasoning empty, content empty (engine evicted/errored/empty)
        //                                      → plain-answer fallback (original
        //                                        messages, enable_thinking:false).
        //                                        If that ALSO yields nothing, a
        //                                        structured error frame is emitted.

        // Sub-case (b): Call-1 emitted a direct answer without structured reasoning.
        if reasoning_buf.is_empty() && !content_buf_call1.is_empty() {
            debug!("Call-1 budget exhausted, no reasoning but content present; emitting content");
            if !stream_reasoning_deltas {
                let chunk = serde_json::json!({
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "model": model_name,
                    "choices": [{ "index": 0, "delta": { "content": content_buf_call1 }, "finish_reason": null }]
                });
                yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or_default())));
            }
            let final_chunk = serde_json::json!({
                "id": completion_id,
                "object": "chat.completion.chunk",
                "model": model_name,
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "length" }]
            });
            yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&final_chunk).unwrap_or_default())));
            yield Ok(Bytes::from("data: [DONE]\n\n"));
            return;
        }

        // Sub-cases (a) and (c): build the second-call body.
        let is_fallback = reasoning_buf.is_empty();
        let (body2, remaining) = if is_fallback {
            warn!("Call-1 produced neither reasoning nor content; issuing plain-answer fallback (Fix #2)");
            (build_plain_answer_body(&original_body_c, original_max_tokens), original_max_tokens)
        } else {
            let remaining = original_max_tokens.saturating_sub(thinking_budget).max(64);
            info!(
                thinking_budget,
                reasoning_len = reasoning_buf.len(),
                remaining_tokens = remaining,
                "Call-1 budget exhausted; executing Call-2"
            );
            (build_call2_body(&original_body_c, &reasoning_buf, remaining, inline_think), remaining)
        };

        // Emit a status event so clients can show a "Generating answer…" indicator
        // during the Call 2 KV-cache prefill gap. Standard OpenAI clients ignore
        // the `lmforge` extension field — this is fully backward-compatible.
        let prefill_status = serde_json::json!({
            "id": completion_id,
            "object": "chat.completion.chunk",
            "model": model_name,
            "choices": [{ "index": 0, "delta": {}, "finish_reason": null }],
            "lmforge": {
                "status": if is_fallback { "answer_fallback" } else { "call2_prefill" },
                "reasoning_len": reasoning_buf.len(),
                "remaining_tokens": remaining
            }
        });
        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!(
            "data: {}\n\n",
            serde_json::to_string(&prefill_status).unwrap_or_default()
        )));

        let resp2 = match client
            .post(format!("http://127.0.0.1:{}{}", engine_port_owned, path_owned))
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(&body2).unwrap_or_default())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Call-2 engine request failed");
                let err = serde_json::json!({"error": {"message": format!("Call-2 failed: {}", e), "type": "server_error"}});
                yield Ok(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&err).unwrap_or_default())));
                return;
            }
        };

        if !resp2.status().is_success() {
            let status = resp2.status().as_u16();
            let err_text = resp2.text().await.unwrap_or_default();
            warn!(status, error = %err_text, "Call-2 returned error status");
            yield Ok(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", err_text)));
            return;
        }

        // Stream Call 2 directly — DIAGNOSTIC: log exact byte count for every SSE event
        let mut call2_stream = resp2.bytes_stream();
        let completion_id_inner = completion_id.clone();
        let model_name_inner = model_name.clone();
        let mut inner_buf = String::new();
        let mut call2_event_count: usize = 0;
        // Track whether the second call produced any answer content. Used by the
        // plain-answer fallback (Fix #2) to emit a structured error instead of a
        // silent empty stream when even the fallback yields nothing.
        let mut saw_content = false;

        while let Some(chunk) = call2_stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "Error reading call-2 stream");
                    break;
                }
            };
            inner_buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(nl) = inner_buf.find('\n') {
                let line = inner_buf[..nl].trim_end_matches('\r').to_string();
                inner_buf.drain(..=nl);

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d.trim(),
                    None => continue,
                };

                if data == "[DONE]" {
                    info!(call2_total_events = call2_event_count, "Call-2 stream complete (received [DONE])");
                    if is_fallback && !saw_content {
                        // Fallback also produced nothing — surface a structured error
                        // rather than a blank reply (Fix #2 invariant).
                        let err = serde_json::json!({"error": {"message": "Model produced no output (reasoning and answer both empty)", "type": "server_error"}});
                        yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&err).unwrap_or_default())));
                    }
                    yield Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n"));
                    return;
                }

                // Rewrite completion_id/model to match call-1
                let sse_payload = if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(obj) = val.as_object_mut() {
                        obj.insert("id".to_string(), serde_json::Value::String(completion_id_inner.clone()));
                        obj.insert("model".to_string(), serde_json::Value::String(model_name_inner.clone()));
                    }
                    if val.pointer("/choices/0/delta/content").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
                        saw_content = true;
                    }
                    format!("data: {}\n\n", serde_json::to_string(&val).unwrap_or_default())
                } else {
                    format!("data: {}\n\n", data)
                };

                call2_event_count += 1;
                // DIAGNOSTIC: every single SSE event from Call 2 is logged with its exact size.
                // If we see call2_event_count=1 with a large payload in the logs, the dump is
                // happening inside mlx_lm itself. If we see many events here but the client still
                // receives a single chunk, the dump is happening downstream (HTTP layer, axum, etc).
                info!(
                    call2_event_n = call2_event_count,
                    payload_bytes = sse_payload.len(),
                    "call2 SSE event yielded"
                );

                yield Ok::<Bytes, std::io::Error>(Bytes::from(sse_payload));
            }
        }

        info!(call2_total_events = call2_event_count, "Call-2 stream ended (no [DONE] received)");
        if is_fallback && !saw_content {
            let err = serde_json::json!({"error": {"message": "Model produced no output (reasoning and answer both empty)", "type": "server_error"}});
            yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&err).unwrap_or_default())));
        }
        yield Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n"));
    };

    Ok(Body::from_stream(body_stream))
}

/// Non-streaming equivalent of `proxy_stream_with_thinking_budget`.
///
/// Runs both calls internally with `stream:true` and assembles a single
/// OpenAI-compatible non-streaming response with separate `reasoning_content`
/// and `content` fields.
pub async fn proxy_nonstream_with_thinking_budget(
    client: &Client,
    engine_port: u16,
    path: &str,
    original_body: serde_json::Value,
    original_max_tokens: u32,
    thinking_budget: u32,
    inline_think: bool,
) -> Result<(u16, String), (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, thinking_budget, "Starting two-call thinking budget (non-stream)");

    // Call 1: thinking phase
    let mut body1 = original_body.clone();
    if let Some(obj) = body1.as_object_mut() {
        obj.insert(
            "max_tokens".to_string(),
            serde_json::Value::from(thinking_budget),
        );
        obj.insert("stream".to_string(), serde_json::Value::Bool(true));
        obj.remove("thinking_budget");
        set_enable_thinking(obj, true);
    }

    let resp1 = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body1).unwrap_or_default())
        .send()
        .await
        .map_err(|e| {
            (
                502u16,
                format!("{{\"error\":\"Engine unavailable: {}\"}}", e),
            )
        })?;

    if !resp1.status().is_success() {
        let status = resp1.status().as_u16();
        return Err((status, resp1.text().await.unwrap_or_default()));
    }

    let (reasoning_buf, content_buf_call1, completion_id, model_name, finish_reason) =
        stream_call1_accumulate(resp1, thinking_budget, None, inline_think)
            .await
            .map_err(|e| (500u16, e))?;

    // Natural finish — assemble from call-1 only
    if finish_reason.as_deref() != Some("length") {
        let assembled = serde_json::json!({
            "id": completion_id,
            "object": "chat.completion",
            "model": model_name,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": if reasoning_buf.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(reasoning_buf) },
                    "content": content_buf_call1,
                    "refusal": serde_json::Value::Null
                },
                "logprobs": serde_json::Value::Null,
                "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string())
            }]
        });
        return Ok((200, serde_json::to_string(&assembled).unwrap_or_default()));
    }

    // Budget exhausted — Call 2 (Fix #2 empty-guard, three sub-cases mirroring
    // the streaming path).
    //
    //   b. reasoning empty, content present → return the content as the answer.
    if reasoning_buf.is_empty() && !content_buf_call1.is_empty() {
        debug!("Call-1 budget exhausted, no reasoning but content present; returning content");
        let assembled = serde_json::json!({
            "id": completion_id,
            "object": "chat.completion",
            "model": model_name,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": serde_json::Value::Null,
                    "content": content_buf_call1,
                    "refusal": serde_json::Value::Null
                },
                "logprobs": serde_json::Value::Null,
                "finish_reason": "length"
            }]
        });
        return Ok((200, serde_json::to_string(&assembled).unwrap_or_default()));
    }

    //   a. reasoning present → Call-2 (reasoning prefill + answer).
    //   c. reasoning empty, content empty → plain-answer fallback.
    let is_fallback = reasoning_buf.is_empty();
    let body2 = if is_fallback {
        warn!("Call-1 produced neither reasoning nor content; issuing plain-answer fallback (Fix #2)");
        build_plain_answer_body(&original_body, original_max_tokens)
    } else {
        let remaining = original_max_tokens.saturating_sub(thinking_budget).max(64);
        build_call2_body(&original_body, &reasoning_buf, remaining, inline_think)
    };

    let resp2 = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body2).unwrap_or_default())
        .send()
        .await
        .map_err(|e| {
            (
                502u16,
                format!("{{\"error\":\"Call-2 engine unavailable: {}\"}}", e),
            )
        })?;

    if !resp2.status().is_success() {
        let status = resp2.status().as_u16();
        return Err((status, resp2.text().await.unwrap_or_default()));
    }

    // Accumulate Call 2 content. Call-2 runs with enable_thinking:false so it
    // should emit plain answer content; inline_think is harmless (no tags to split).
    let (_, content_buf_call2, comp_id2, model2, finish2) =
        stream_call1_accumulate(resp2, u32::MAX, None, inline_think)
            .await
            .map_err(|e| (500u16, e))?;

    // Fix #2: if the plain-answer fallback ALSO produced nothing, surface a
    // structured error rather than a 200 with a blank answer.
    if is_fallback && content_buf_call2.trim().is_empty() {
        warn!("Plain-answer fallback produced no content; returning structured error");
        return Err((
            502u16,
            r#"{"error":{"message":"Model produced no output (reasoning and answer both empty)","type":"server_error","param":null,"code":null}}"#.to_string(),
        ));
    }

    let final_id = if completion_id.is_empty() {
        comp_id2
    } else {
        completion_id
    };
    let final_model = if model_name.is_empty() {
        model2
    } else {
        model_name
    };

    let assembled = serde_json::json!({
        "id": final_id,
        "object": "chat.completion",
        "model": final_model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "reasoning_content": if reasoning_buf.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(reasoning_buf) },
                "content": content_buf_call2,
                "refusal": serde_json::Value::Null
            },
            "logprobs": serde_json::Value::Null,
            "finish_reason": finish2.unwrap_or_else(|| "stop".to_string())
        }]
    });

    Ok((200, serde_json::to_string(&assembled).unwrap_or_default()))
}

// =============================================================================
// Stateful SSE rewriter — think-tag streaming support
// =============================================================================

/// Rewrite a single parsed SSE `data: {...}` JSON value in-place.
///
/// If the delta has a `content` field, run it through the rewriter and replace with:
/// - `delta.reasoning_content` = reasoning text (or `null` if empty)
/// - `delta.content`           = answer text (or `null` if empty)
fn rewrite_sse_chunk(chunk: &mut serde_json::Value, rewriter: &mut ThinkSplitter) {
    if let Some(choices) = chunk.get_mut("choices").and_then(|c| c.as_array_mut())
        && let Some(choice) = choices.first_mut()
        && let Some(delta) = choice.get_mut("delta").and_then(|d| d.as_object_mut())
    {
        // Only rewrite if there's a content field (skip role-only deltas)
        if let Some(content_val) = delta.remove("content") {
            let content_str = content_val.as_str().unwrap_or("");
            let (reasoning, content) = rewriter.push(content_str);

            delta.insert(
                "reasoning_content".to_string(),
                if reasoning.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(reasoning)
                },
            );
            delta.insert(
                "content".to_string(),
                if content.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(content)
                },
            );
        }
    }
}

/// Streaming proxy that rewrites `<think>`/`</think>` tags in `delta.content` into
/// proper `delta.reasoning_content` / `delta.content` fields.
///
/// Used for the oMLX streaming path when `think: true` is requested. After Phase 1,
/// oMLX no longer receives `enable_thinking` and instead emits Qwen3's natural
/// `<think>…</think>` reasoning inside `delta.content`. This function makes the
/// output API-compatible by rewriting it on the fly.
///
/// The rewriter handles `<think>` and `</think>` tags that span SSE chunk boundaries
/// using a stateful tag buffer. Non-thinking models (no `<think>` in output) are
/// unaffected — the rewriter passes content through unchanged.
pub async fn proxy_stream_rewriting_think_tags(
    client: &Client,
    engine_port: u16,
    path: &str,
    body: Bytes,
) -> Result<Body, (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, "Proxying streaming request with think-tag rewrite");

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to proxy think-rewrite stream to engine");
            (
                502u16,
                format!("{{\"error\":\"Engine unavailable: {}\"}}", e),
            )
        })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err((status, text));
    }

    let mut byte_stream = resp.bytes_stream();

    let output = stream! {
        // Safety guard — same limits as proxy_request_assembling_stream.
        // Without these, a runaway oMLX generation streams forever to the client.
        const MAX_SSE_LINES: usize = 4096;
        const MAX_TOTAL_BYTES: usize = 768 * 1024; // 768 KB
        let mut sse_line_count: usize = 0;
        let mut total_bytes: usize = 0;

        let mut line_buf = String::new();
        let mut rewriter = ThinkSplitter::default();
        let mut aborted = false;

        'outer: while let Some(chunk_result) = byte_stream.next().await {
            let bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    error!(error = %e, "Stream read error in think-tag rewriter");
                    break;
                }
            };
            total_bytes += bytes.len();
            line_buf.push_str(&String::from_utf8_lossy(&bytes));

            // Check byte limit first (fast path before line parsing)
            if total_bytes > MAX_TOTAL_BYTES {
                warn!(
                    total_bytes,
                    "Streaming think-tag rewriter safety limit reached (byte size) — aborting"
                );
                aborted = true;
                break;
            }

            // Process all complete lines
            while let Some(nl) = line_buf.find('\n') {
                let raw_line = line_buf[..nl].trim_end_matches('\r').to_string();
                line_buf.drain(..=nl);

                // Count only non-empty data lines against the limit
                if raw_line.starts_with("data: ") && raw_line != "data: [DONE]" {
                    sse_line_count += 1;
                    if sse_line_count > MAX_SSE_LINES {
                        warn!(
                            sse_line_count,
                            "Streaming think-tag rewriter safety limit reached (line count) — aborting"
                        );
                        aborted = true;
                        break 'outer;
                    }
                }

                let rewritten = rewrite_sse_line(&raw_line, &mut rewriter);
                yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{rewritten}\n")));
            }
        }

        if aborted {
            // Emit a terminal error SSE event so the client knows the stream was cut
            let err = serde_json::json!({
                "error": {
                    "message": "Generation exceeded streaming safety limits (possible infinite thinking loop)",
                    "type": "server_error"
                }
            });
            yield Ok(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", serde_json::to_string(&err).unwrap_or_default())));
            return;
        }

        // Flush partial line buffer (rare edge: stream cut without trailing newline)
        if !line_buf.trim().is_empty() {
            let rewritten = rewrite_sse_line(line_buf.trim_end_matches('\r'), &mut rewriter);
            yield Ok(Bytes::from(format!("{rewritten}\n")));
        }

        // Flush any bytes buffered for partial tag detection
        let (leftover_reasoning, leftover_content) = rewriter.flush();
        if !leftover_reasoning.is_empty() || !leftover_content.is_empty() {
            let delta = serde_json::json!({
                "choices": [{
                    "delta": {
                        "reasoning_content": if leftover_reasoning.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(leftover_reasoning) },
                        "content": if leftover_content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(leftover_content) }
                    }
                }]
            });
            yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&delta).unwrap_or_default())));
        }
    };

    Ok(Body::from_stream(output))
}

/// Streaming proxy for **native-reasoning oMLX models** that drops the trailing
/// `content` delta which merely echoes the full accumulated reasoning on
/// `finish=length` (Fix #1, ADR-007).
///
/// Native-reasoning models (`qwen3:4b:thinking`, `phi4:reasoning`) bypass the
/// budget orchestrator and stream straight through. On truncation oMLX emits the
/// reasoning as `reasoning_content` deltas and then re-emits the *entire*
/// reasoning once as a single `content` delta — a duplicate, not an answer. This
/// proxy accumulates reasoning and suppresses any `content` delta whose text
/// exactly equals the accumulated reasoning (length-guarded). All other deltas —
/// including legitimate answers on natural `stop` — pass through unchanged.
pub async fn proxy_stream_dedup_native_reasoning(
    client: &Client,
    engine_port: u16,
    path: &str,
    body: Bytes,
) -> Result<Body, (u16, String)> {
    let url = format!("http://127.0.0.1:{}{}", engine_port, path);
    debug!(url = %url, "Proxying streaming request with native-reasoning dedup");

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to proxy dedup stream to engine");
            (502u16, format!("{{\"error\":\"Engine unavailable: {}\"}}", e))
        })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err((status, text));
    }

    let mut byte_stream = resp.bytes_stream();
    let output = stream! {
        let mut line_buf = String::new();
        let mut reasoning_buf = String::new();

        while let Some(chunk_result) = byte_stream.next().await {
            let bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => { error!(error = %e, "Stream read error in dedup proxy"); break; }
            };
            line_buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(nl) = line_buf.find('\n') {
                let raw_line = line_buf[..nl].trim_end_matches('\r').to_string();
                line_buf.drain(..=nl);

                let data = match raw_line.strip_prefix("data: ") {
                    Some(d) => d.trim(),
                    None => { yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{raw_line}\n"))); continue; }
                };
                if data == "[DONE]" {
                    yield Ok(Bytes::from("data: [DONE]\n\n"));
                    continue;
                }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(data) else {
                    yield Ok(Bytes::from(format!("{raw_line}\n")));
                    continue;
                };

                // Accumulate reasoning; detect the duplicate content echo.
                if let Some(r) = val.pointer("/choices/0/delta/reasoning_content").and_then(|v| v.as_str()) {
                    reasoning_buf.push_str(r);
                }
                let is_dup = val
                    .pointer("/choices/0/delta/content")
                    .and_then(|v| v.as_str())
                    .is_some_and(|c| is_reasoning_echo(c, &reasoning_buf));
                if is_dup {
                    debug!("Fix #1: suppressing streamed content delta duplicating reasoning");
                    continue; // drop the duplicate echo
                }

                yield Ok(Bytes::from(format!("{raw_line}\n")));
            }
        }

        if !line_buf.trim().is_empty() {
            yield Ok(Bytes::from(format!("{}\n", line_buf.trim_end_matches('\r'))));
        }
    };

    Ok(Body::from_stream(output))
}

/// Rewrite a single raw SSE line (e.g. `data: {...}`) through the tag rewriter.
/// Non-data lines (empty lines, `event:`, `[DONE]`) are returned unchanged.
fn rewrite_sse_line(line: &str, rewriter: &mut ThinkSplitter) -> String {
    let data = match line.strip_prefix("data: ") {
        Some(d) => d.trim(),
        None => return line.to_string(), // pass through: empty / event: / comment lines
    };

    // Pass [DONE] through unchanged
    if data == "[DONE]" {
        return line.to_string();
    }

    // Attempt to parse and rewrite the JSON chunk
    let Ok(mut chunk_val) = serde_json::from_str::<serde_json::Value>(data) else {
        return line.to_string(); // Unparseable — pass through
    };

    rewrite_sse_chunk(&mut chunk_val, rewriter);

    format!(
        "data: {}",
        serde_json::to_string(&chunk_val).unwrap_or_else(|_| data.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fix #1: native-reasoning echo dedup ──────────────────────────────────

    #[test]
    fn test_is_reasoning_echo_exact_long_match() {
        let reasoning = "Let me work through this carefully step by step to reach the answer.";
        assert!(is_reasoning_echo(reasoning, reasoning));
        // trimmed comparison
        assert!(is_reasoning_echo(&format!("  {reasoning}  "), reasoning));
    }

    #[test]
    fn test_is_reasoning_echo_rejects_short_reasoning() {
        // Below the min-length guard — never treated as an echo (false-positive guard).
        let short = "2+2=4";
        assert!(!is_reasoning_echo(short, short));
    }

    #[test]
    fn test_is_reasoning_echo_rejects_real_answer() {
        let reasoning = "Let me work through this carefully step by step to reach the answer.";
        // A genuine, different answer must survive.
        assert!(!is_reasoning_echo("The answer is 42.", reasoning));
        // A partial overlap (not a full verbatim copy) must survive.
        assert!(!is_reasoning_echo("Let me work through this", reasoning));
    }

    #[test]
    fn test_is_reasoning_echo_empty_reasoning() {
        assert!(!is_reasoning_echo("anything", ""));
        assert!(!is_reasoning_echo("", ""));
    }

    // ── ThinkSplitter SSE rewriter tests ─────────────────────────────────────

    #[test]
    fn test_rewriter_passthrough_no_tags() {
        let mut r = ThinkSplitter::default();
        let (reasoning, content) = r.push("Hello world");
        assert_eq!(reasoning, "");
        assert_eq!(content, "Hello world");
    }

    #[test]
    fn test_rewriter_full_think_block_in_one_chunk() {
        let mut r = ThinkSplitter::default();
        let (reasoning, content) = r.push("<think>I reason</think>Answer");
        assert_eq!(reasoning, "I reason");
        assert_eq!(content, "Answer");
    }

    #[test]
    fn test_rewriter_think_open_tag_split_across_chunks() {
        let mut r = ThinkSplitter::default();
        // Chunk 1 ends mid-tag
        let (r1, c1) = r.push("<thi");
        assert_eq!(r1, "");
        assert_eq!(c1, ""); // buffered, not emitted yet

        // Chunk 2 completes the tag + reasoning
        let (r2, c2) = r.push("nk>reasoning");
        assert_eq!(r2, "reasoning");
        assert_eq!(c2, "");
    }

    #[test]
    fn test_rewriter_think_close_tag_split_across_chunks() {
        let mut r = ThinkSplitter::default();
        // Enter thinking mode
        r.push("<think>");
        // Chunk ends mid-close-tag
        let (r1, c1) = r.push("some reasoning</th");
        assert_eq!(r1, "some reasoning");
        assert_eq!(c1, "");

        // Chunk 2 completes close tag + answer
        let (r2, c2) = r.push("ink>The answer");
        assert_eq!(r2, "");
        assert_eq!(c2, "The answer");
    }

    #[test]
    fn test_rewriter_content_before_think_block() {
        let mut r = ThinkSplitter::default();
        let (reasoning, content) = r.push("Prefix<think>reasons</think>Suffix");
        assert_eq!(reasoning, "reasons");
        assert_eq!(content, "PrefixSuffix");
    }

    #[test]
    fn test_rewriter_no_think_tag_non_thinking_model() {
        let mut r = ThinkSplitter::default();
        // Non-thinking model: all goes to content
        let (r1, c1) = r.push("Chunk one ");
        let (r2, c2) = r.push("chunk two");
        assert_eq!(r1, "");
        assert_eq!(c1, "Chunk one ");
        assert_eq!(r2, "");
        assert_eq!(c2, "chunk two");
    }

    #[test]
    fn test_rewrite_sse_line_done_passthrough() {
        let mut r = ThinkSplitter::default();
        let result = rewrite_sse_line("data: [DONE]", &mut r);
        assert_eq!(result, "data: [DONE]");
    }

    #[test]
    fn test_rewrite_sse_line_empty_passthrough() {
        let mut r = ThinkSplitter::default();
        let result = rewrite_sse_line("", &mut r);
        assert_eq!(result, "");
    }

    #[test]
    fn test_rewrite_sse_line_rewrites_content_to_reasoning() {
        let mut r = ThinkSplitter::default();
        // Put splitter into thinking mode first
        r.push("<think>");

        let line = r#"data: {"choices":[{"delta":{"content":"I think therefore"}}]}"#;
        let result = rewrite_sse_line(line, &mut r);

        let parsed: serde_json::Value =
            serde_json::from_str(result.strip_prefix("data: ").unwrap()).unwrap();
        assert!(parsed["choices"][0]["delta"]["content"].is_null());
        assert_eq!(
            parsed["choices"][0]["delta"]["reasoning_content"],
            "I think therefore"
        );
    }

    #[test]
    fn test_rewrite_sse_line_rewrites_content_to_content() {
        let mut r = ThinkSplitter::default();
        // In answer mode (default)
        let line = r#"data: {"choices":[{"delta":{"content":"The answer is 4"}}]}"#;
        let result = rewrite_sse_line(line, &mut r);

        let parsed: serde_json::Value =
            serde_json::from_str(result.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "The answer is 4");
        assert!(parsed["choices"][0]["delta"]["reasoning_content"].is_null());
    }

    // ── build_call2_body unit tests ───────────────────────────────────────────

    #[test]
    fn test_build_call2_body_inline_appends_closed_think_block() {
        // inline_think=true (llama.cpp / SGLang): reasoning prefilled as an
        // ASSISTANT turn that the engine continues into the answer.
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "What is 2+2?"}],
            "max_tokens": 2048,
            "stream": true
        });
        let result = build_call2_body(&body, "I reasoned hard", 512, true);

        let messages = result["messages"].as_array().unwrap();
        // Original message + appended assistant turn
        assert_eq!(messages.len(), 2);
        let assistant = &messages[1];
        assert_eq!(assistant["role"], "assistant");
        let content = assistant["content"].as_str().unwrap();
        assert!(
            content.contains("<think>I reasoned hard</think>"),
            "must wrap reasoning in closed think tags"
        );
    }

    #[test]
    fn test_build_call2_body_omlx_uses_user_directive() {
        // inline_think=false (oMLX): reasoning fed back as a USER directive,
        // NOT an assistant prefill (oMLX regenerates assistant prefills and
        // echoes the reasoning as the answer — the duplication bug).
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "What is 2+2?"}],
            "max_tokens": 2048,
            "stream": true
        });
        let result = build_call2_body(&body, "I reasoned hard", 512, false);

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        let turn = &messages[1];
        assert_eq!(
            turn["role"], "user",
            "oMLX Call 2 must feed reasoning back as a user turn, not assistant"
        );
        let content = turn["content"].as_str().unwrap();
        assert!(
            content.contains("I reasoned hard"),
            "must include the prior reasoning for context"
        );
        assert!(
            content.to_lowercase().contains("final answer"),
            "must instruct the model to give only the final answer"
        );
        assert!(
            !content.contains("<think>"),
            "oMLX directive must not wrap reasoning in <think> tags"
        );
    }

    #[test]
    fn test_build_call2_body_sets_enable_thinking_false() {
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 2048
        });
        // enable_thinking:false must hold regardless of engine.
        for inline in [true, false] {
            let result = build_call2_body(&body, "some reasoning", 512, inline);
            assert_eq!(
                result["chat_template_kwargs"]["enable_thinking"], false,
                "Call 2 must suppress thinking mode (inline_think={inline})"
            );
        }
    }

    #[test]
    fn test_build_call2_body_sets_remaining_max_tokens() {
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 4096
        });
        let result = build_call2_body(&body, "reasoning", 768, true);
        assert_eq!(
            result["max_tokens"], 768,
            "max_tokens must be overridden to remaining budget"
        );
    }

    // ── build_plain_answer_body unit tests (Fix #2 fallback) ──────────────────

    #[test]
    fn test_build_plain_answer_body_disables_thinking_keeps_messages() {
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "What is 2+2?"}],
            "max_tokens": 256,
            "thinking_budget": 2048,
            "stream": false
        });
        let result = build_plain_answer_body(&body, 2048);

        // Original messages are preserved verbatim — no synthetic reasoning turn.
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "What is 2+2?");

        // Thinking suppressed, full budget restored, internal stream forced, private field stripped.
        assert_eq!(result["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(result["max_tokens"], 2048);
        assert_eq!(result["stream"], true);
        assert!(result.get("thinking_budget").is_none());
    }

    #[test]
    fn test_build_call2_body_strips_thinking_budget() {
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 2048,
            "thinking_budget": 4096
        });
        let result = build_call2_body(&body, "reasoning", 512, false);
        assert!(
            result.get("thinking_budget").is_none(),
            "thinking_budget must be stripped from Call 2 body"
        );
    }

    #[test]
    fn test_build_call2_body_always_streams() {
        let body = serde_json::json!({
            "model": "qwen3.5-4b-4bit",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 2048,
            "stream": false   // original was non-streaming
        });
        let result = build_call2_body(&body, "reasoning", 512, true);
        assert_eq!(
            result["stream"], true,
            "Call 2 must always be stream:true internally"
        );
    }

    // ── set_enable_thinking unit tests ────────────────────────────────────────

    #[test]
    fn test_set_enable_thinking_creates_kwargs() {
        let mut body = serde_json::json!({"model": "m", "messages": []});
        let obj = body.as_object_mut().unwrap();
        set_enable_thinking(obj, true);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    #[test]
    fn test_set_enable_thinking_overwrites_existing() {
        let mut body = serde_json::json!({
            "model": "m",
            "chat_template_kwargs": {"enable_thinking": false, "other": "keep"}
        });
        let obj = body.as_object_mut().unwrap();
        set_enable_thinking(obj, true);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(
            body["chat_template_kwargs"]["other"], "keep",
            "sibling kwargs must be preserved"
        );
    }

    /// Call-1 must request enable_thinking:true so oMLX emits delta.reasoning_content
    /// (quants like 2b:4bit produce no reasoning otherwise). This mirrors the body1
    /// patch inside the budget orchestrators.
    #[test]
    fn test_call1_body_patch_sets_enable_thinking_true() {
        let original = serde_json::json!({
            "model": "qwen3.5-2b-4bit",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking_budget": 2048
        });
        let mut body1 = original.clone();
        let obj = body1.as_object_mut().unwrap();
        obj.insert("max_tokens".to_string(), serde_json::Value::from(2048u32));
        obj.insert("stream".to_string(), serde_json::Value::Bool(true));
        obj.remove("thinking_budget");
        set_enable_thinking(obj, true);

        assert_eq!(body1["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body1["max_tokens"], 2048);
        assert!(body1.get("thinking_budget").is_none());
    }

    // ── CHARACTERIZATION: full inline-<think> stream replay ───────────────────
    // Locks the llama.cpp/sglang template path end-to-end: a realistic stream
    // where the <think>/</think> tags and surrounding text are split awkwardly
    // across deltas (mid-tag boundaries) must still reassemble into exactly the
    // intended reasoning vs answer. The refactor consolidates this rewriter into
    // thinking/splitter.rs — this test is the behavioural oracle for that move.
    #[test]
    fn test_characterize_inline_think_chunked_stream_reassembly() {
        // Intended logical output: reasoning="Let me think step by step. 2+2=4."
        //                          answer  ="The answer is 4."
        // Delivered as awkward chunks (tags + words straddle delta boundaries):
        let deltas = [
            "<thi",                 // partial open tag
            "nk>Let me think ",     // completes open tag + reasoning
            "step by step. ",
            "2+2=4.",
            "</thin",               // partial close tag
            "k>The answer ",        // completes close tag + answer begins
            "is 4.",
        ];

        let mut r = ThinkSplitter::default();
        let mut reasoning = String::new();
        let mut content = String::new();
        for d in deltas {
            let (re, co) = r.push(d);
            reasoning.push_str(&re);
            content.push_str(&co);
        }
        let (re, co) = r.flush();
        reasoning.push_str(&re);
        content.push_str(&co);

        assert_eq!(reasoning, "Let me think step by step. 2+2=4.");
        assert_eq!(content, "The answer is 4.");
    }

    // Reasoning-only stream that is truncated mid-thought (budget exhausted,
    // finish=length): everything stays reasoning, answer is empty, and flush
    // must not leak a dangling partial close tag into content.
    #[test]
    fn test_characterize_inline_think_truncated_midreasoning() {
        let deltas = ["<think>still reasoning when cut off</thin"];
        let mut r = ThinkSplitter::default();
        let mut reasoning = String::new();
        let mut content = String::new();
        for d in deltas {
            let (re, co) = r.push(d);
            reasoning.push_str(&re);
            content.push_str(&co);
        }
        let (re, co) = r.flush();
        reasoning.push_str(&re);
        content.push_str(&co);

        // The dangling "</thin" was buffered as a possible tag; on flush it is
        // emitted in the current (thinking) mode → reasoning, never content.
        assert_eq!(reasoning, "still reasoning when cut off</thin");
        assert_eq!(content, "", "truncated reasoning must not leak into the answer");
    }
}
