use anyhow::Result;
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
            (502u16, format!("{{\"error\":{{\"message\":\"Engine unavailable: {}\",\"type\":\"server_error\"}}}}", e))
        })?;

    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| {
        (502, format!("{{\"error\":{{\"message\":\"Failed to read engine response: {}\",\"type\":\"server_error\"}}}}", e))
    })?;

    if status >= 400 {
        warn!(status, "Engine returned error");
    }

    Ok((status, text))
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
        chunk
            .map(|bytes| bytes)
            .map_err(|e| {
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
        (400u16, format!("{{\"error\":{{\"message\":\"Invalid JSON: {}\"}}}}", e))
    })?;
    if let Some(obj) = body_val.as_object_mut() {
        obj.insert("stream".to_string(), serde_json::Value::Bool(true));
    }
    let patched = serde_json::to_vec(&body_val).map_err(|e| {
        (500u16, format!("{{\"error\":{{\"message\":\"JSON serialization failed: {}\"}}}}", e))
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
            (502u16, format!("{{\"error\":{{\"message\":\"Engine unavailable: {}\"}}}}", e))
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

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| {
            (502u16, format!("{{\"error\":{{\"message\":\"Stream read error: {}\"}}}}", e))
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
                        if let Some(r) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                            reasoning_buf.push_str(r);
                        }
                        if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                            content_buf.push_str(c);
                        }
                    }
                    if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                        finish_reason = Some(fr.to_string());
                    }
                }
            }

            // Usage from final chunk
            if let Some(usage) = chunk_val.get("usage") {
                prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(prompt_tokens);
                completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(completion_tokens);
            }
        }
    }

    // Assemble non-streaming response
    let assembled = serde_json::json!({
        "id": completion_id,
        "object": "chat.completion",
        "created": created,
        "model": model_name,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content_buf,
                "reasoning_content": if reasoning_buf.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(reasoning_buf) },
                "tool_calls": null
            },
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
