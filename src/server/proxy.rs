use anyhow::Result;
use async_stream::stream;
use axum::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use tracing::{debug, error, warn};

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
                    if msg.get("reasoning_content").map(|v| v.is_null()).unwrap_or(false) {
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
        chunk.map(|bytes| bytes).map_err(|e| {
            error!(error = %e, "Error reading stream from engine");
            std::io::Error::new(std::io::ErrorKind::Other, e)
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

            if let Some(choices) = chunk_val.get("choices").and_then(|c| c.as_array()) {
                if let Some(choice) = choices.first() {
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
                        if let Some(tc_arr) =
                            delta.get("tool_calls").and_then(|v| v.as_array())
                        {
                            for tc_delta in tc_arr {
                                let idx = tc_delta
                                    .get("index")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let entry = tool_call_map.entry(idx).or_insert_with(|| {
                                    (
                                        String::new(), // id
                                        String::new(), // type
                                        String::new(), // function.name
                                        String::new(), // function.arguments
                                    )
                                });
                                if let Some(id) =
                                    tc_delta.get("id").and_then(|v| v.as_str())
                                {
                                    entry.0 = id.to_string();
                                }
                                if let Some(t) =
                                    tc_delta.get("type").and_then(|v| v.as_str())
                                {
                                    entry.1 = t.to_string();
                                }
                                if let Some(func) = tc_delta.get("function") {
                                    if let Some(name) =
                                        func.get("name").and_then(|v| v.as_str())
                                    {
                                        entry.2 = name.to_string();
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(|v| v.as_str())
                                    {
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
    let (final_reasoning, final_content) = if reasoning_buf.is_empty() && content_buf.contains("<think>") {
        let (r, c) = crate::server::thinking::extract_think_tags(&content_buf);
        (r.unwrap_or_default(), c)
    } else {
        (reasoning_buf, content_buf)
    };

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
// Stateful SSE rewriter — Phase 2 think-tag streaming support
// =============================================================================

/// Whether the rewriter is currently inside a `<think>` block or emitting answer tokens.
#[derive(Debug, Clone, PartialEq)]
enum ThinkMode {
    Answer,
    Thinking,
}

/// Stateful transformer that rewrites `delta.content` SSE chunks containing `<think>` /
/// `</think>` tags into proper `delta.reasoning_content` / `delta.content` delta fields.
///
/// Designed to handle tags that span SSE chunk boundaries: any bytes that could be the
/// beginning of a tag are buffered in `tag_buf` until the next chunk resolves them.
struct ThinkTagRewriter {
    mode: ThinkMode,
    /// Bytes at the tail of the last chunk that could be a partial `<think>` or `</think>`
    tag_buf: String,
}

impl ThinkTagRewriter {
    fn new() -> Self {
        Self {
            mode: ThinkMode::Answer,
            tag_buf: String::new(),
        }
    }

    /// Process one `delta.content` string.
    ///
    /// Returns `(reasoning_addition, content_addition)` — the text that should be
    /// emitted as `reasoning_content` and `content` respectively in this delta.
    /// Either may be empty; the caller decides how to serialise nulls.
    fn process(&mut self, input: &str) -> (String, String) {
        let mut reasoning = String::new();
        let mut content = String::new();

        // Prepend any bytes buffered from the previous chunk
        let full = format!("{}{}", self.tag_buf, input);
        self.tag_buf.clear();

        let mut remaining = full.as_str();

        loop {
            let search = match self.mode {
                ThinkMode::Answer => "<think>",
                ThinkMode::Thinking => "</think>",
            };

            if let Some(pos) = remaining.find(search) {
                let before = &remaining[..pos];
                let after = &remaining[pos + search.len()..];

                match self.mode {
                    ThinkMode::Answer => {
                        content.push_str(before);
                        // Discard the <think> tag itself; switch mode
                        self.mode = ThinkMode::Thinking;
                    }
                    ThinkMode::Thinking => {
                        reasoning.push_str(before);
                        // Discard the </think> tag itself; switch mode
                        self.mode = ThinkMode::Answer;
                    }
                }
                remaining = after;
            } else {
                // Tag not found in `remaining` — but the tail might be a partial tag.
                // Buffer the longest suffix that could still be a tag prefix.
                let partial_len = longest_tag_prefix(remaining, search);
                let safe_len = remaining.len() - partial_len;

                let to_emit = &remaining[..safe_len];
                self.tag_buf = remaining[safe_len..].to_string();

                match self.mode {
                    ThinkMode::Answer => content.push_str(to_emit),
                    ThinkMode::Thinking => reasoning.push_str(to_emit),
                }
                break;
            }
        }

        (reasoning, content)
    }

    /// Flush any remaining bytes in `tag_buf` as literal text (called at stream end).
    fn flush(&mut self) -> (String, String) {
        if self.tag_buf.is_empty() {
            return (String::new(), String::new());
        }
        let leftover = std::mem::take(&mut self.tag_buf);
        match self.mode {
            ThinkMode::Answer => (String::new(), leftover),
            ThinkMode::Thinking => (leftover, String::new()),
        }
    }
}

/// Returns the length of the longest suffix of `text` that is also a prefix of `tag`.
/// Used to identify partial tag bytes that must be buffered rather than emitted.
fn longest_tag_prefix(text: &str, tag: &str) -> usize {
    // Work in bytes (all our tags are ASCII, so byte-safe)
    let text_bytes = text.as_bytes();
    let tag_bytes = tag.as_bytes();
    for len in (1..=tag_bytes.len().min(text_bytes.len())).rev() {
        if text_bytes.ends_with(&tag_bytes[..len]) {
            return len;
        }
    }
    0
}

/// Rewrite a single parsed SSE `data: {...}` JSON value in-place.
///
/// If the delta has a `content` field, run it through the rewriter and replace with:
/// - `delta.reasoning_content` = reasoning text (or `null` if empty)
/// - `delta.content`           = answer text (or `null` if empty)
fn rewrite_sse_chunk(chunk: &mut serde_json::Value, rewriter: &mut ThinkTagRewriter) {
    if let Some(choices) = chunk.get_mut("choices").and_then(|c| c.as_array_mut()) {
        if let Some(choice) = choices.first_mut() {
            if let Some(delta) = choice.get_mut("delta").and_then(|d| d.as_object_mut()) {
                // Only rewrite if there's a content field (skip role-only deltas)
                if let Some(content_val) = delta.remove("content") {
                    let content_str = content_val.as_str().unwrap_or("");
                    let (reasoning, content) = rewriter.process(content_str);

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
            (502u16, format!("{{\"error\":\"Engine unavailable: {}\"}}", e))
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
        let mut rewriter = ThinkTagRewriter::new();
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

/// Rewrite a single raw SSE line (e.g. `data: {...}`) through the tag rewriter.
/// Non-data lines (empty lines, `event:`, `[DONE]`) are returned unchanged.
fn rewrite_sse_line(line: &str, rewriter: &mut ThinkTagRewriter) -> String {
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

    format!("data: {}", serde_json::to_string(&chunk_val).unwrap_or_else(|_| data.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ThinkTagRewriter unit tests ───────────────────────────────────────────

    #[test]
    fn test_rewriter_passthrough_no_tags() {
        let mut r = ThinkTagRewriter::new();
        let (reasoning, content) = r.process("Hello world");
        assert_eq!(reasoning, "");
        assert_eq!(content, "Hello world");
    }

    #[test]
    fn test_rewriter_full_think_block_in_one_chunk() {
        let mut r = ThinkTagRewriter::new();
        let (reasoning, content) = r.process("<think>I reason</think>Answer");
        assert_eq!(reasoning, "I reason");
        assert_eq!(content, "Answer");
    }

    #[test]
    fn test_rewriter_think_open_tag_split_across_chunks() {
        let mut r = ThinkTagRewriter::new();
        // Chunk 1 ends mid-tag
        let (r1, c1) = r.process("<thi");
        assert_eq!(r1, "");
        assert_eq!(c1, ""); // buffered, not emitted yet

        // Chunk 2 completes the tag + reasoning
        let (r2, c2) = r.process("nk>reasoning");
        assert_eq!(r2, "reasoning");
        assert_eq!(c2, "");
    }

    #[test]
    fn test_rewriter_think_close_tag_split_across_chunks() {
        let mut r = ThinkTagRewriter::new();
        // Enter thinking mode
        r.process("<think>");
        // Chunk ends mid-close-tag
        let (r1, c1) = r.process("some reasoning</th");
        assert_eq!(r1, "some reasoning");
        assert_eq!(c1, "");

        // Chunk 2 completes close tag + answer
        let (r2, c2) = r.process("ink>The answer");
        assert_eq!(r2, "");
        assert_eq!(c2, "The answer");
    }

    #[test]
    fn test_rewriter_content_before_think_block() {
        let mut r = ThinkTagRewriter::new();
        let (reasoning, content) = r.process("Prefix<think>reasons</think>Suffix");
        assert_eq!(reasoning, "reasons");
        assert_eq!(content, "PrefixSuffix");
    }

    #[test]
    fn test_rewriter_no_think_tag_non_thinking_model() {
        let mut r = ThinkTagRewriter::new();
        // Non-thinking model: all goes to content
        let (r1, c1) = r.process("Chunk one ");
        let (r2, c2) = r.process("chunk two");
        assert_eq!(r1, ""); assert_eq!(c1, "Chunk one ");
        assert_eq!(r2, ""); assert_eq!(c2, "chunk two");
    }

    #[test]
    fn test_longest_tag_prefix_exact_match() {
        // "<thi" is a 4-char prefix of "<think>"
        assert_eq!(longest_tag_prefix("hello<thi", "<think>"), 4);
    }

    #[test]
    fn test_longest_tag_prefix_no_match() {
        assert_eq!(longest_tag_prefix("hello world", "<think>"), 0);
    }

    #[test]
    fn test_longest_tag_prefix_full_tag() {
        // Full tag at end — entire tag buffered
        assert_eq!(longest_tag_prefix("text<think>", "<think>"), 7);
    }

    #[test]
    fn test_rewrite_sse_line_done_passthrough() {
        let mut r = ThinkTagRewriter::new();
        let result = rewrite_sse_line("data: [DONE]", &mut r);
        assert_eq!(result, "data: [DONE]");
    }

    #[test]
    fn test_rewrite_sse_line_empty_passthrough() {
        let mut r = ThinkTagRewriter::new();
        let result = rewrite_sse_line("", &mut r);
        assert_eq!(result, "");
    }

    #[test]
    fn test_rewrite_sse_line_rewrites_content_to_reasoning() {
        let mut r = ThinkTagRewriter::new();
        // Put rewriter into Thinking mode first
        r.process("<think>");

        let line = r#"data: {"choices":[{"delta":{"content":"I think therefore"}}]}"#;
        let result = rewrite_sse_line(line, &mut r);

        let parsed: serde_json::Value = serde_json::from_str(result.strip_prefix("data: ").unwrap()).unwrap();
        assert!(parsed["choices"][0]["delta"]["content"].is_null());
        assert_eq!(parsed["choices"][0]["delta"]["reasoning_content"], "I think therefore");
    }

    #[test]
    fn test_rewrite_sse_line_rewrites_content_to_content() {
        let mut r = ThinkTagRewriter::new();
        // In Answer mode (default)
        let line = r#"data: {"choices":[{"delta":{"content":"The answer is 4"}}]}"#;
        let result = rewrite_sse_line(line, &mut r);

        let parsed: serde_json::Value = serde_json::from_str(result.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "The answer is 4");
        assert!(parsed["choices"][0]["delta"]["reasoning_content"].is_null());
    }
}
