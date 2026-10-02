//! `POST /v1/responses` — stateless OpenAI Responses API adapter.
//!
//! Thin translation layer over the existing chat path: the request is rewritten
//! into a `/v1/chat/completions` body, handed to [`openai::chat_completions`]
//! (model loading, thinking pipeline, engine proxying, metrics all live there),
//! and the returned JSON / SSE body is translated back into Responses shapes.
//!
//! v1 is stateless: `previous_response_id`, `conversation` and `background`
//! are rejected with 400, `store` is accepted and ignored.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_stream::stream;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{Map, Value, json};
use tracing::debug;

use super::AppState;
use super::openai;

/// Upper bound when buffering a non-streaming chat response for translation.
const MAX_CHAT_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct ApiError {
    message: String,
    param: Option<String>,
    code: Option<&'static str>,
}

fn bad(message: impl Into<String>, param: Option<&str>) -> ApiError {
    ApiError {
        message: message.into(),
        param: param.map(str::to_string),
        code: None,
    }
}

fn unsupported(message: impl Into<String>, param: &str) -> ApiError {
    ApiError {
        message: message.into(),
        param: Some(param.to_string()),
        code: Some("unsupported_parameter"),
    }
}

fn error_response(status: StatusCode, err_type: &str, err: &ApiError) -> Response<Body> {
    let body = json!({
        "error": {
            "message": err.message,
            "type": err_type,
            "param": err.param,
            "code": err.code,
        }
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler
// ─────────────────────────────────────────────────────────────────────────────

/// `POST /v1/responses`
pub async fn responses(State(state): State<AppState>, body: Bytes) -> Response<Body> {
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &bad(format!("Invalid JSON: {e}"), None),
            );
        }
    };

    let translated = match translate_request(&req) {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &e),
    };
    debug!(model = %translated.meta.model, stream = translated.stream, "Responses API request");

    let chat_body = Bytes::from(serde_json::to_vec(&translated.chat).unwrap_or_default());
    let chat_resp = openai::chat_completions(State(state), chat_body)
        .await
        .into_response();

    // Chat-path errors (model load, capability gates, engine errors) keep their
    // original status and OpenAI error JSON.
    if !chat_resp.status().is_success() {
        return chat_resp;
    }

    let (_, chat_body) = chat_resp.into_parts();
    if translated.stream {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(translate_chat_stream(chat_body, translated.meta))
            .unwrap();
    }

    let bytes = match axum::body::to_bytes(chat_body, MAX_CHAT_RESPONSE_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "server_error",
                &bad(format!("Failed to read engine response: {e}"), None),
            );
        }
    };
    let chat_json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "server_error",
                &bad(format!("Engine returned invalid JSON: {e}"), None),
            );
        }
    };
    // Some proxy paths report failures as a 200 carrying an `error` object.
    if chat_json.get("choices").is_none() && chat_json.get("error").is_some() {
        return Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap();
    }
    match translate_chat_response(&translated.meta, &chat_json) {
        Ok(resp) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(resp.to_string()))
            .unwrap(),
        Err(msg) => error_response(StatusCode::BAD_GATEWAY, "server_error", &bad(msg, None)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ids / response envelope
// ─────────────────────────────────────────────────────────────────────────────

fn new_id(prefix: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(n);
    h.write_u128(nanos);
    format!("{prefix}_{:016x}{:016x}", h.finish(), n)
}

/// Request-derived fields echoed back on every Response object.
#[derive(Debug, Clone)]
pub(crate) struct ResponseMeta {
    id: String,
    created_at: i64,
    model: String,
    echo: Map<String, Value>,
}

impl ResponseMeta {
    fn from_request(req: &Value) -> Self {
        let get = |k: &str| req.get(k).cloned().unwrap_or(Value::Null);
        let or = |k: &str, d: Value| match req.get(k) {
            Some(v) if !v.is_null() => v.clone(),
            _ => d,
        };
        let mut echo = Map::new();
        echo.insert("instructions".into(), get("instructions"));
        echo.insert("max_output_tokens".into(), get("max_output_tokens"));
        echo.insert("temperature".into(), get("temperature"));
        echo.insert("top_p".into(), get("top_p"));
        echo.insert("tools".into(), or("tools", json!([])));
        echo.insert("tool_choice".into(), or("tool_choice", json!("auto")));
        echo.insert(
            "parallel_tool_calls".into(),
            or("parallel_tool_calls", json!(true)),
        );
        echo.insert(
            "text".into(),
            or("text", json!({"format": {"type": "text"}})),
        );
        echo.insert(
            "reasoning".into(),
            or("reasoning", json!({"effort": null, "summary": null})),
        );
        echo.insert("metadata".into(), or("metadata", json!({})));
        echo.insert("previous_response_id".into(), Value::Null);
        // Nothing is persisted, regardless of what the client asked for.
        echo.insert("store".into(), json!(false));
        echo.insert("background".into(), json!(false));
        echo.insert("truncation".into(), json!("disabled"));
        echo.insert("user".into(), get("user"));

        Self {
            id: new_id("resp"),
            created_at: chrono::Utc::now().timestamp(),
            model: req
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            echo,
        }
    }

    fn envelope(
        &self,
        status: &str,
        output: &[Value],
        usage: Option<Value>,
        incomplete_reason: Option<&str>,
        error: Option<Value>,
    ) -> Value {
        let mut obj = self.echo.clone();
        obj.insert("id".into(), json!(self.id));
        obj.insert("object".into(), json!("response"));
        obj.insert("created_at".into(), json!(self.created_at));
        obj.insert("status".into(), json!(status));
        obj.insert("model".into(), json!(self.model));
        obj.insert("output".into(), Value::Array(output.to_vec()));
        obj.insert("error".into(), error.unwrap_or(Value::Null));
        obj.insert(
            "incomplete_details".into(),
            incomplete_reason
                .map(|r| json!({"reason": r}))
                .unwrap_or(Value::Null),
        );
        obj.insert("usage".into(), usage.unwrap_or(Value::Null));
        // Convenience aggregate (SDKs compute this client-side; harmless extra).
        let text: String = output
            .iter()
            .filter(|i| i["type"] == "message")
            .flat_map(|i| i["content"].as_array().cloned().unwrap_or_default())
            .filter_map(|p| p["text"].as_str().map(str::to_string))
            .collect();
        obj.insert("output_text".into(), json!(text));
        Value::Object(obj)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Request translation
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) struct TranslatedRequest {
    chat: Value,
    stream: bool,
    meta: ResponseMeta,
}

fn translate_request(req: &Value) -> Result<TranslatedRequest, ApiError> {
    let obj = req
        .as_object()
        .ok_or_else(|| bad("Request body must be a JSON object", None))?;

    let present = |k: &str| obj.get(k).is_some_and(|v| !v.is_null());
    if present("previous_response_id") {
        return Err(unsupported(
            "previous_response_id is not supported: LMForge's /v1/responses is stateless. \
             Resend the full conversation in `input` instead.",
            "previous_response_id",
        ));
    }
    if present("conversation") {
        return Err(unsupported(
            "conversation is not supported: LMForge's /v1/responses is stateless.",
            "conversation",
        ));
    }
    if obj.get("background").and_then(|v| v.as_bool()) == Some(true) {
        return Err(unsupported(
            "background mode is not supported by LMForge's /v1/responses.",
            "background",
        ));
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instr) = obj.get("instructions").and_then(|v| v.as_str())
        && !instr.is_empty()
    {
        messages.push(json!({"role": "system", "content": instr}));
    }
    match obj.get("input") {
        Some(Value::String(s)) => messages.push(json!({"role": "user", "content": s})),
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                translate_input_item(item, i, &mut messages)?;
            }
        }
        _ => {
            return Err(bad(
                "`input` must be a string or an array of input items",
                Some("input"),
            ));
        }
    }
    if messages.is_empty() {
        return Err(bad("`input` produced no messages", Some("input")));
    }

    let mut chat = Map::new();
    if let Some(m) = obj.get("model") {
        chat.insert("model".into(), m.clone());
    }
    chat.insert("messages".into(), Value::Array(messages));

    if let Some(n) = obj.get("max_output_tokens").filter(|v| !v.is_null()) {
        chat.insert("max_tokens".into(), n.clone());
    }
    for k in ["temperature", "top_p", "parallel_tool_calls"] {
        if let Some(v) = obj.get(k).filter(|v| !v.is_null()) {
            chat.insert(k.into(), v.clone());
        }
    }
    // LMForge extensions understood by the chat path.
    for k in ["keep_alive", "thinking_budget", "think"] {
        if let Some(v) = obj.get(k).filter(|v| !v.is_null()) {
            chat.insert(k.into(), v.clone());
        }
    }

    let stream = obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    chat.insert("stream".into(), json!(stream));
    if stream {
        // The final usage chunk is opt-in on OpenAI-style streams.
        chat.insert("stream_options".into(), json!({"include_usage": true}));
    }

    if let Some(tools) = obj.get("tools").filter(|v| !v.is_null()) {
        let translated = translate_tools(tools)?;
        if !translated.is_empty() {
            chat.insert("tools".into(), Value::Array(translated));
        }
    }
    if let Some(tc) = obj.get("tool_choice").filter(|v| !v.is_null()) {
        chat.insert("tool_choice".into(), translate_tool_choice(tc)?);
    }
    if let Some(fmt) = obj.get("text").and_then(|t| t.get("format"))
        && let Some(rf) = translate_text_format(fmt)?
    {
        chat.insert("response_format".into(), rf);
    }

    // `reasoning.effort` maps onto the chat path's boolean `think` intent; the
    // effort level itself has no budget equivalent. An explicit `think` wins.
    if !chat.contains_key("think")
        && let Some(effort) = obj
            .get("reasoning")
            .and_then(|r| r.get("effort"))
            .and_then(|e| e.as_str())
    {
        chat.insert("think".into(), json!(effort != "none"));
    }
    if chat.get("think").and_then(|v| v.as_bool()) == Some(true) && stream {
        // Surface reasoning live so it can be emitted as summary deltas.
        chat.insert("stream_reasoning_deltas".into(), json!(true));
    }

    Ok(TranslatedRequest {
        chat: Value::Object(chat),
        stream,
        meta: ResponseMeta::from_request(req),
    })
}

fn translate_input_item(
    item: &Value,
    idx: usize,
    messages: &mut Vec<Value>,
) -> Result<(), ApiError> {
    let param = format!("input[{idx}]");
    let kind = item
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            if item.get("role").is_some() {
                "message"
            } else {
                ""
            }
        });
    match kind {
        "message" => {
            let role = match item.get("role").and_then(|v| v.as_str()) {
                Some("developer") | Some("system") => "system",
                Some("user") => "user",
                Some("assistant") => "assistant",
                other => {
                    return Err(bad(
                        format!("Unsupported message role {other:?}"),
                        Some(&param),
                    ));
                }
            };
            let content = translate_content(
                item.get("content").unwrap_or(&Value::Null),
                &format!("{param}.content"),
            )?;
            messages.push(json!({"role": role, "content": content}));
        }
        "function_call" => {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| bad("function_call item requires `call_id`", Some(&param)))?;
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| bad("function_call item requires `name`", Some(&param)))?;
            let arguments = match item.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => "{}".to_string(),
                Some(other) => other.to_string(),
            };
            let call = json!({
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": arguments},
            });
            // Parallel calls arrive as consecutive items; chat expects one
            // assistant turn carrying all of them.
            if let Some(last) = messages.last_mut()
                && last["role"] == "assistant"
            {
                match last.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                    Some(arr) => arr.push(call),
                    None => last["tool_calls"] = json!([call]),
                }
            } else {
                messages.push(json!({"role": "assistant", "content": "", "tool_calls": [call]}));
            }
        }
        "function_call_output" => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| bad("function_call_output item requires `call_id`", Some(&param)))?;
            let content = match item.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(Value::Array(parts))
                    if parts.iter().all(|p| {
                        matches!(
                            p["type"].as_str(),
                            Some("input_text") | Some("output_text") | Some("text")
                        )
                    }) =>
                {
                    parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<String>()
                }
                Some(other) => other.to_string(),
            };
            messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
        }
        // Prior reasoning can't be replayed to a chat template.
        "reasoning" => {}
        "item_reference" => {
            return Err(unsupported(
                "item_reference is not supported: LMForge's /v1/responses is stateless.",
                &param,
            ));
        }
        other => {
            return Err(bad(
                format!("Unsupported input item type '{other}'"),
                Some(&param),
            ));
        }
    }
    Ok(())
}

/// Responses content (string | parts[]) -> chat content (string | parts[]).
/// Text-only content is collapsed to a plain string for the widest engine
/// template compatibility; images keep the parts array.
fn translate_content(content: &Value, param: &str) -> Result<Value, ApiError> {
    let parts = match content {
        Value::String(_) => return Ok(content.clone()),
        Value::Null => return Ok(json!("")),
        Value::Array(a) => a,
        _ => {
            return Err(bad(
                "`content` must be a string or an array of content parts",
                Some(param),
            ));
        }
    };
    let mut out: Vec<Value> = Vec::with_capacity(parts.len());
    let mut has_image = false;
    for part in parts {
        match part.get("type").and_then(|v| v.as_str()) {
            Some("input_text") | Some("output_text") | Some("text") => {
                let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                out.push(json!({"type": "text", "text": text}));
            }
            Some("refusal") => {
                let text = part.get("refusal").and_then(|v| v.as_str()).unwrap_or("");
                out.push(json!({"type": "text", "text": text}));
            }
            Some("input_image") => {
                let url = match part.get("image_url") {
                    Some(Value::String(s)) => s.as_str(),
                    Some(Value::Object(o)) => o.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                    _ => "",
                };
                if url.is_empty() {
                    return Err(bad(
                        "input_image requires `image_url` (URL or data URL); `file_id` is not supported",
                        Some(param),
                    ));
                }
                let mut image = json!({"url": url});
                if let Some(d) = part.get("detail").and_then(|d| d.as_str()) {
                    image["detail"] = json!(d);
                }
                has_image = true;
                out.push(json!({"type": "image_url", "image_url": image}));
            }
            other => {
                return Err(bad(
                    format!("Unsupported content part type {other:?}"),
                    Some(param),
                ));
            }
        }
    }
    if has_image {
        Ok(Value::Array(out))
    } else {
        Ok(json!(
            out.iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<String>()
        ))
    }
}

fn translate_tools(tools: &Value) -> Result<Vec<Value>, ApiError> {
    let arr = tools
        .as_array()
        .ok_or_else(|| bad("`tools` must be an array", Some("tools")))?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, tool) in arr.iter().enumerate() {
        let param = format!("tools[{i}]");
        let kind = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "function" {
            return Err(bad(
                format!(
                    "Unsupported tool type '{kind}': only `function` tools are supported by LMForge's /v1/responses"
                ),
                Some(&param),
            ));
        }
        let name = tool
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("function tool requires `name`", Some(&param)))?;
        let mut func = Map::new();
        func.insert("name".into(), json!(name));
        if let Some(d) = tool.get("description").filter(|v| !v.is_null()) {
            func.insert("description".into(), d.clone());
        }
        func.insert(
            "parameters".into(),
            tool.get("parameters")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        );
        out.push(json!({"type": "function", "function": Value::Object(func)}));
    }
    Ok(out)
}

fn translate_tool_choice(tc: &Value) -> Result<Value, ApiError> {
    match tc {
        Value::String(s) if matches!(s.as_str(), "auto" | "none" | "required") => Ok(tc.clone()),
        Value::Object(o) => match o.get("type").and_then(|v| v.as_str()) {
            Some("function") => {
                // Accept an already-nested chat shape too.
                let name = o
                    .get("name")
                    .or_else(|| o.get("function").and_then(|f| f.get("name")))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        bad("tool_choice function requires `name`", Some("tool_choice"))
                    })?;
                Ok(json!({"type": "function", "function": {"name": name}}))
            }
            other => Err(bad(
                format!("Unsupported tool_choice type {other:?}"),
                Some("tool_choice"),
            )),
        },
        _ => Err(bad("Invalid `tool_choice`", Some("tool_choice"))),
    }
}

fn translate_text_format(fmt: &Value) -> Result<Option<Value>, ApiError> {
    match fmt.get("type").and_then(|v| v.as_str()) {
        None | Some("text") => Ok(None),
        Some("json_object") => Ok(Some(json!({"type": "json_object"}))),
        Some("json_schema") => {
            let schema = fmt.get("schema").ok_or_else(|| {
                bad(
                    "text.format json_schema requires `schema`",
                    Some("text.format"),
                )
            })?;
            let mut js = Map::new();
            js.insert(
                "name".into(),
                fmt.get("name")
                    .cloned()
                    .unwrap_or_else(|| json!("response")),
            );
            js.insert("schema".into(), schema.clone());
            for k in ["strict", "description"] {
                if let Some(v) = fmt.get(k).filter(|v| !v.is_null()) {
                    js.insert(k.into(), v.clone());
                }
            }
            Ok(Some(
                json!({"type": "json_schema", "json_schema": Value::Object(js)}),
            ))
        }
        Some(other) => Err(bad(
            format!("Unsupported text.format type '{other}'"),
            Some("text.format"),
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Output item builders
// ─────────────────────────────────────────────────────────────────────────────

fn reasoning_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": id,
        "status": status,
        "summary": [{"type": "summary_text", "text": text}],
    })
}

fn message_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "status": status,
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

fn function_call_item(id: &str, call_id: &str, name: &str, args: &str, status: &str) -> Value {
    json!({
        "type": "function_call",
        "id": id,
        "call_id": call_id,
        "name": name,
        "arguments": args,
        "status": status,
    })
}

fn convert_usage(chat_usage: Option<&Value>) -> Value {
    let get = |k: &str| {
        chat_usage
            .and_then(|u| u.get(k))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    let input = get("prompt_tokens");
    let output = get("completion_tokens");
    let total = chat_usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(input + output);
    let reasoning = chat_usage
        .and_then(|u| u.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    json!({
        "input_tokens": input,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": output,
        "output_tokens_details": {"reasoning_tokens": reasoning},
        "total_tokens": total,
    })
}

/// `(status, incomplete_reason)` for a chat `finish_reason`.
fn status_for_finish(finish: Option<&str>) -> (&'static str, Option<&'static str>) {
    match finish {
        Some("length") => ("incomplete", Some("max_output_tokens")),
        Some("content_filter") => ("incomplete", Some("content_filter")),
        _ => ("completed", None),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Non-streaming response translation
// ─────────────────────────────────────────────────────────────────────────────

fn translate_chat_response(meta: &ResponseMeta, chat: &Value) -> Result<Value, String> {
    let choice = chat
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| "Engine response has no choices".to_string())?;
    let message = choice.get("message").unwrap_or(&Value::Null);
    let finish = choice.get("finish_reason").and_then(|v| v.as_str());
    let (status, incomplete) = status_for_finish(finish);

    let mut output: Vec<Value> = Vec::new();
    let reasoning = message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    if let Some(r) = reasoning {
        output.push(reasoning_item(&new_id("rs"), r, "completed"));
    }

    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls: Vec<&Value> = message
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if !content.is_empty() || (tool_calls.is_empty() && reasoning.is_none()) {
        let item_status = if status == "incomplete" {
            "incomplete"
        } else {
            "completed"
        };
        output.push(message_item(&new_id("msg"), content, item_status));
    }
    for tc in tool_calls {
        let Some(name) = tc.pointer("/function/name").and_then(|v| v.as_str()) else {
            continue;
        };
        let args = match tc.pointer("/function/arguments") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => "{}".to_string(),
            Some(other) => other.to_string(),
        };
        let call_id = tc
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| new_id("call"));
        output.push(function_call_item(
            &new_id("fc"),
            &call_id,
            name,
            &args,
            "completed",
        ));
    }

    Ok(meta.envelope(
        status,
        &output,
        Some(convert_usage(chat.get("usage"))),
        incomplete,
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming translation
// ─────────────────────────────────────────────────────────────────────────────

struct TextItem {
    id: String,
    output_index: usize,
    text: String,
}

enum Open {
    Reasoning(TextItem),
    Message(TextItem),
    /// Index into `StreamTranslator::tools`.
    Tool(u64),
}

#[derive(Default)]
struct PendingTool {
    call_id: String,
    name: String,
    args: String,
    item_id: String,
    output_index: usize,
    opened: bool,
    /// Argument bytes already sent as deltas.
    sent: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum TextKind {
    Reasoning,
    Message,
}

/// Incremental OpenAI-chat-SSE -> Responses-SSE state machine.
///
/// Items are strictly sequential (one open at a time). Reasoning is streamed as
/// `reasoning_summary_text` events when the chat path surfaces it; a new item of
/// a different kind closes the current one. Tool-call argument fragments for an
/// index whose item was already closed (interleaved parallel calls) are kept
/// for the aggregate but not re-emitted.
pub(crate) struct StreamTranslator {
    meta: ResponseMeta,
    seq: u64,
    output: Vec<Value>,
    open: Option<Open>,
    tools: BTreeMap<u64, PendingTool>,
    usage: Option<Value>,
    finish_reason: Option<String>,
    buf: Vec<u8>,
    terminal: bool,
}

impl StreamTranslator {
    pub(crate) fn new(meta: ResponseMeta) -> Self {
        Self {
            meta,
            seq: 0,
            output: Vec::new(),
            open: None,
            tools: BTreeMap::new(),
            usage: None,
            finish_reason: None,
            buf: Vec::new(),
            terminal: false,
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn emit(&mut self, out: &mut Vec<String>, ty: &str, mut data: Value) {
        data["type"] = json!(ty);
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        out.push(format!("event: {ty}\ndata: {data}\n\n"));
    }

    pub(crate) fn start(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let snap = self.meta.envelope("in_progress", &[], None, None, None);
        self.emit(&mut out, "response.created", json!({"response": snap}));
        self.emit(&mut out, "response.in_progress", json!({"response": snap}));
        out
    }

    /// Feed raw upstream bytes. Lines are split on `\n` at the byte level so
    /// multi-byte characters split across chunks decode intact.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        if self.terminal {
            return out;
        }
        self.buf.extend_from_slice(bytes);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            self.handle_line(&line, &mut out);
            if self.terminal {
                self.buf.clear();
                break;
            }
        }
        out
    }

    /// Upstream closed cleanly.
    pub(crate) fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.terminal {
            return out;
        }
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.handle_line(&line, &mut out);
            if self.terminal {
                return out;
            }
        }
        // Finish reason without [DONE] is a clean end; neither is a cut-off.
        if self.finish_reason.is_some() {
            self.finalize(&mut out);
        } else {
            self.fail(
                &mut out,
                "server_error",
                "Engine stream ended before completion",
            );
        }
        out
    }

    /// Upstream read error.
    pub(crate) fn abort(&mut self, message: &str) -> Vec<String> {
        let mut out = Vec::new();
        if !self.terminal {
            self.fail(&mut out, "server_error", message);
        }
        out
    }

    fn handle_line(&mut self, raw: &[u8], out: &mut Vec<String>) {
        let line = String::from_utf8_lossy(raw);
        let line = line.trim_end_matches(['\r', '\n']);
        let Some(payload) = line.strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload.is_empty() {
            return;
        }
        if payload == "[DONE]" {
            self.finalize(out);
            return;
        }
        let Ok(val) = serde_json::from_str::<Value>(payload) else {
            return;
        };

        if val.get("choices").is_none()
            && let Some(err) = val.get("error")
        {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| err.as_str())
                .unwrap_or("Engine error");
            self.fail(out, "server_error", msg);
            return;
        }

        if let Some(u) = val.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(u.clone());
        }
        let Some(choice) = val
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
        else {
            return;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(r) = delta.get("reasoning_content").and_then(|v| v.as_str())
                && !r.is_empty()
            {
                self.on_text(out, TextKind::Reasoning, r);
            }
            if let Some(c) = delta.get("content").and_then(|v| v.as_str())
                && !c.is_empty()
            {
                self.on_text(out, TextKind::Message, c);
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    self.on_tool_delta(out, tc);
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(fr.to_string());
        }
    }

    fn on_text(&mut self, out: &mut Vec<String>, kind: TextKind, delta: &str) {
        let same = matches!(
            (&self.open, kind),
            (Some(Open::Reasoning(_)), TextKind::Reasoning)
                | (Some(Open::Message(_)), TextKind::Message)
        );
        if !same {
            self.close_open(out, "completed");
            self.open_text(out, kind);
        }
        let (event, item_id, output_index) = match self.open.as_mut() {
            Some(Open::Reasoning(t)) => {
                t.text.push_str(delta);
                (
                    "response.reasoning_summary_text.delta",
                    t.id.clone(),
                    t.output_index,
                )
            }
            Some(Open::Message(t)) => {
                t.text.push_str(delta);
                ("response.output_text.delta", t.id.clone(), t.output_index)
            }
            _ => return,
        };
        let data = match kind {
            TextKind::Reasoning => json!({
                "item_id": item_id, "output_index": output_index,
                "summary_index": 0, "delta": delta,
            }),
            TextKind::Message => json!({
                "item_id": item_id, "output_index": output_index,
                "content_index": 0, "delta": delta, "logprobs": [],
            }),
        };
        self.emit(out, event, data);
    }

    fn open_text(&mut self, out: &mut Vec<String>, kind: TextKind) {
        let output_index = self.output.len();
        match kind {
            TextKind::Reasoning => {
                let id = new_id("rs");
                let item =
                    json!({"type": "reasoning", "id": id, "status": "in_progress", "summary": []});
                self.emit(
                    out,
                    "response.output_item.added",
                    json!({"output_index": output_index, "item": item}),
                );
                self.emit(
                    out,
                    "response.reasoning_summary_part.added",
                    json!({
                        "item_id": id, "output_index": output_index, "summary_index": 0,
                        "part": {"type": "summary_text", "text": ""},
                    }),
                );
                self.open = Some(Open::Reasoning(TextItem {
                    id,
                    output_index,
                    text: String::new(),
                }));
            }
            TextKind::Message => {
                let id = new_id("msg");
                let item = json!({
                    "type": "message", "id": id, "role": "assistant",
                    "status": "in_progress", "content": [],
                });
                self.emit(
                    out,
                    "response.output_item.added",
                    json!({"output_index": output_index, "item": item}),
                );
                self.emit(
                    out,
                    "response.content_part.added",
                    json!({
                        "item_id": id, "output_index": output_index, "content_index": 0,
                        "part": {"type": "output_text", "text": "", "annotations": []},
                    }),
                );
                self.open = Some(Open::Message(TextItem {
                    id,
                    output_index,
                    text: String::new(),
                }));
            }
        }
    }

    fn on_tool_delta(&mut self, out: &mut Vec<String>, tc: &Value) {
        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
        {
            let t = self.tools.entry(idx).or_default();
            if let Some(id) = tc.get("id").and_then(|v| v.as_str())
                && !id.is_empty()
                && t.call_id.is_empty()
            {
                t.call_id = id.to_string();
            }
            if let Some(f) = tc.get("function") {
                if let Some(n) = f.get("name").and_then(|v| v.as_str())
                    && !n.is_empty()
                {
                    t.name = n.to_string();
                }
                if let Some(a) = f.get("arguments").and_then(|v| v.as_str()) {
                    t.args.push_str(a);
                }
            }
        }

        let is_open = matches!(self.open, Some(Open::Tool(i)) if i == idx);
        let (opened, has_name) = {
            let t = &self.tools[&idx];
            (t.opened, !t.name.is_empty())
        };
        if !opened {
            // Wait for the name: the added event must carry it.
            if !has_name {
                return;
            }
            self.close_open(out, "completed");
            let output_index = self.output.len();
            let t = self.tools.get_mut(&idx).unwrap();
            if t.call_id.is_empty() {
                t.call_id = new_id("call");
            }
            t.item_id = new_id("fc");
            t.output_index = output_index;
            t.opened = true;
            let item = function_call_item(&t.item_id, &t.call_id, &t.name, "", "in_progress");
            self.emit(
                out,
                "response.output_item.added",
                json!({"output_index": output_index, "item": item}),
            );
            self.open = Some(Open::Tool(idx));
        } else if !is_open {
            return;
        }

        let t = self.tools.get_mut(&idx).unwrap();
        if t.args.len() > t.sent {
            let delta = t.args[t.sent..].to_string();
            t.sent = t.args.len();
            let data = json!({
                "item_id": t.item_id, "output_index": t.output_index, "delta": delta,
            });
            self.emit(out, "response.function_call_arguments.delta", data);
        }
    }

    /// Close the open item (if any), pushing its final form into `output`.
    fn close_open(&mut self, out: &mut Vec<String>, status: &str) {
        match self.open.take() {
            None => {}
            Some(Open::Message(t)) => {
                let part = json!({"type": "output_text", "text": t.text, "annotations": []});
                self.emit(
                    out,
                    "response.output_text.done",
                    json!({
                        "item_id": t.id, "output_index": t.output_index,
                        "content_index": 0, "text": t.text, "logprobs": [],
                    }),
                );
                self.emit(
                    out,
                    "response.content_part.done",
                    json!({
                        "item_id": t.id, "output_index": t.output_index,
                        "content_index": 0, "part": part,
                    }),
                );
                let item = message_item(&t.id, &t.text, status);
                self.emit(
                    out,
                    "response.output_item.done",
                    json!({"output_index": t.output_index, "item": item}),
                );
                self.output.push(item);
            }
            Some(Open::Reasoning(t)) => {
                let part = json!({"type": "summary_text", "text": t.text});
                self.emit(
                    out,
                    "response.reasoning_summary_text.done",
                    json!({
                        "item_id": t.id, "output_index": t.output_index,
                        "summary_index": 0, "text": t.text,
                    }),
                );
                self.emit(
                    out,
                    "response.reasoning_summary_part.done",
                    json!({
                        "item_id": t.id, "output_index": t.output_index,
                        "summary_index": 0, "part": part,
                    }),
                );
                let item = reasoning_item(&t.id, &t.text, status);
                self.emit(
                    out,
                    "response.output_item.done",
                    json!({"output_index": t.output_index, "item": item}),
                );
                self.output.push(item);
            }
            Some(Open::Tool(idx)) => {
                let Some(t) = self.tools.get(&idx) else {
                    return;
                };
                let (item_id, output_index, name, args, call_id) = (
                    t.item_id.clone(),
                    t.output_index,
                    t.name.clone(),
                    t.args.clone(),
                    t.call_id.clone(),
                );
                self.emit(
                    out,
                    "response.function_call_arguments.done",
                    json!({
                        "item_id": item_id, "output_index": output_index,
                        "name": name, "arguments": args,
                    }),
                );
                let item = function_call_item(&item_id, &call_id, &name, &args, status);
                self.emit(
                    out,
                    "response.output_item.done",
                    json!({"output_index": output_index, "item": item}),
                );
                self.output.push(item);
            }
        }
    }

    fn finalize(&mut self, out: &mut Vec<String>) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let (status, incomplete) = status_for_finish(self.finish_reason.as_deref());
        let item_status = if status == "incomplete" {
            "incomplete"
        } else {
            "completed"
        };
        self.close_open(out, item_status);
        let usage = convert_usage(self.usage.as_ref());
        let resp = self
            .meta
            .envelope(status, &self.output, Some(usage), incomplete, None);
        let ty = if status == "completed" {
            "response.completed"
        } else {
            "response.incomplete"
        };
        self.emit(out, ty, json!({"response": resp}));
    }

    fn fail(&mut self, out: &mut Vec<String>, code: &str, message: &str) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        self.close_open(out, "incomplete");
        let err = json!({"code": code, "message": message});
        let resp = self
            .meta
            .envelope("failed", &self.output, None, None, Some(err));
        self.emit(out, "response.failed", json!({"response": resp}));
    }
}

/// Wrap the chat path's SSE body in a Responses event stream.
fn translate_chat_stream(chat: Body, meta: ResponseMeta) -> Body {
    let mut upstream = chat.into_data_stream();
    let mut t = StreamTranslator::new(meta);
    let s = stream! {
        for f in t.start() {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(f));
        }
        while let Some(chunk) = upstream.next().await {
            let frames = match chunk {
                Ok(b) => t.push(&b),
                Err(e) => t.abort(&format!("Engine stream error: {e}")),
            };
            for f in frames {
                yield Ok(Bytes::from(f));
            }
            if t.is_terminal() {
                break;
            }
        }
        for f in t.finish() {
            yield Ok(Bytes::from(f));
        }
    };
    Body::from_stream(s)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(req: Value) -> Result<Value, ApiError> {
        translate_request(&req).map(|t| t.chat)
    }

    fn meta() -> ResponseMeta {
        ResponseMeta::from_request(&json!({"model": "m", "input": "hi"}))
    }

    // ── request translation ──

    #[test]
    fn string_input_becomes_user_message() {
        let c = tr(json!({"model": "m", "input": "hello"})).unwrap();
        assert_eq!(c["model"], "m");
        assert_eq!(c["messages"], json!([{"role": "user", "content": "hello"}]));
        assert_eq!(c["stream"], false);
        assert!(c.get("stream_options").is_none());
    }

    #[test]
    fn instructions_become_leading_system_message() {
        let c = tr(json!({"model": "m", "instructions": "be brief", "input": "hi"})).unwrap();
        assert_eq!(
            c["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(c["messages"][1]["role"], "user");
    }

    #[test]
    fn scalar_params_map_across() {
        let c = tr(json!({
            "model": "m", "input": "hi", "max_output_tokens": 77,
            "temperature": 0.2, "top_p": 0.9, "stream": true, "store": true,
        }))
        .unwrap();
        assert_eq!(c["max_tokens"], 77);
        assert_eq!(c["temperature"], 0.2);
        assert_eq!(c["top_p"], 0.9);
        assert_eq!(c["stream"], true);
        assert_eq!(c["stream_options"]["include_usage"], true);
        assert!(c.get("store").is_none());
        assert!(c.get("max_output_tokens").is_none());
    }

    #[test]
    fn item_array_with_images_and_roles() {
        let c = tr(json!({
            "model": "m",
            "input": [
                {"role": "developer", "content": "rules"},
                {"role": "user", "content": [
                    {"type": "input_text", "text": "what is this?"},
                    {"type": "input_image", "image_url": "https://x/y.png"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "low"},
                ]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "a cat"}]},
                {"role": "user", "content": [{"type": "input_text", "text": "a"}, {"type": "input_text", "text": "b"}]},
            ]
        }))
        .unwrap();
        let m = c["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({"role": "system", "content": "rules"}));
        assert_eq!(
            m[1]["content"][0],
            json!({"type": "text", "text": "what is this?"})
        );
        assert_eq!(
            m[1]["content"][1],
            json!({"type": "image_url", "image_url": {"url": "https://x/y.png"}})
        );
        assert_eq!(
            m[1]["content"][2],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA", "detail": "low"}})
        );
        assert_eq!(m[2], json!({"role": "assistant", "content": "a cat"}));
        // Text-only parts collapse to a string.
        assert_eq!(m[3]["content"], "ab");
        // The chat path's image gates must see the translated image parts.
        assert!(openai::request_has_image(&c));
    }

    #[test]
    fn input_image_without_url_is_400() {
        let e = tr(json!({"model": "m", "input": [
            {"role": "user", "content": [{"type": "input_image", "file_id": "file-1"}]}
        ]}))
        .unwrap_err();
        assert!(e.message.contains("file_id"));
    }

    #[test]
    fn function_call_and_output_items() {
        let c = tr(json!({
            "model": "m",
            "input": [
                {"role": "user", "content": "weather?"},
                {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"},
                {"type": "function_call", "call_id": "call_2", "name": "get_time", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "sunny"},
                {"type": "function_call_output", "call_id": "call_2", "output": "noon"},
            ]
        }))
        .unwrap();
        let m = c["messages"].as_array().unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m[1]["role"], "assistant");
        assert_eq!(m[1]["content"], "");
        let calls = m[1]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert_eq!(calls[0]["function"]["arguments"], "{\"city\":\"Paris\"}");
        assert_eq!(
            m[2],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"})
        );
        assert_eq!(m[3]["tool_call_id"], "call_2");
    }

    #[test]
    fn function_call_merges_into_preceding_assistant_text() {
        let c = tr(json!({"model": "m", "input": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": "calling"},
            {"type": "function_call", "call_id": "c", "name": "f", "arguments": "{}"},
        ]}))
        .unwrap();
        let m = c["messages"].as_array().unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[1]["content"], "calling");
        assert_eq!(m[1]["tool_calls"][0]["id"], "c");
    }

    #[test]
    fn flat_tools_become_nested() {
        let c = tr(json!({
            "model": "m", "input": "hi",
            "tools": [{
                "type": "function", "name": "get_weather", "description": "d",
                "strict": true,
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}},
            }],
            "tool_choice": {"type": "function", "name": "get_weather"},
        }))
        .unwrap();
        assert_eq!(
            c["tools"][0],
            json!({"type": "function", "function": {
                "name": "get_weather", "description": "d",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}},
            }})
        );
        assert_eq!(
            c["tool_choice"],
            json!({"type": "function", "function": {"name": "get_weather"}})
        );
    }

    #[test]
    fn tool_choice_string_modes_pass_through() {
        for mode in ["auto", "none", "required"] {
            let c = tr(json!({"model": "m", "input": "hi", "tool_choice": mode})).unwrap();
            assert_eq!(c["tool_choice"], mode);
        }
        assert!(tr(json!({"model": "m", "input": "hi", "tool_choice": "bogus"})).is_err());
    }

    #[test]
    fn non_function_tool_is_400() {
        let e = tr(json!({"model": "m", "input": "hi", "tools": [{"type": "web_search"}]}))
            .unwrap_err();
        assert!(e.message.contains("web_search"), "{}", e.message);
        assert_eq!(e.param.as_deref(), Some("tools[0]"));
    }

    #[test]
    fn text_format_maps_to_response_format() {
        let c = tr(json!({"model": "m", "input": "hi", "text": {"format": {
            "type": "json_schema", "name": "person", "strict": true,
            "schema": {"type": "object"},
        }}}))
        .unwrap();
        assert_eq!(
            c["response_format"],
            json!({"type": "json_schema", "json_schema": {
                "name": "person", "schema": {"type": "object"}, "strict": true,
            }})
        );
        let c =
            tr(json!({"model": "m", "input": "hi", "text": {"format": {"type": "json_object"}}}))
                .unwrap();
        assert_eq!(c["response_format"], json!({"type": "json_object"}));
        let c =
            tr(json!({"model": "m", "input": "hi", "text": {"format": {"type": "text"}}})).unwrap();
        assert!(c.get("response_format").is_none());
    }

    #[test]
    fn reasoning_effort_maps_to_think() {
        let c = tr(json!({"model": "m", "input": "hi", "reasoning": {"effort": "high"}})).unwrap();
        assert_eq!(c["think"], true);
        assert!(c.get("stream_reasoning_deltas").is_none());
        let c = tr(
            json!({"model": "m", "input": "hi", "stream": true, "reasoning": {"effort": "low"}}),
        )
        .unwrap();
        assert_eq!(c["stream_reasoning_deltas"], true);
        let c = tr(json!({"model": "m", "input": "hi", "reasoning": {"effort": "none"}})).unwrap();
        assert_eq!(c["think"], false);
        let c = tr(json!({"model": "m", "input": "hi", "reasoning": {"summary": "auto"}})).unwrap();
        assert!(c.get("think").is_none());
    }

    #[test]
    fn stateful_and_background_are_400() {
        let e =
            tr(json!({"model": "m", "input": "hi", "previous_response_id": "resp_1"})).unwrap_err();
        assert!(e.message.contains("previous_response_id"));
        assert_eq!(e.param.as_deref(), Some("previous_response_id"));
        assert_eq!(e.code, Some("unsupported_parameter"));
        assert!(tr(json!({"model": "m", "input": "hi", "background": true})).is_err());
        assert!(tr(json!({"model": "m", "input": "hi", "conversation": "conv_1"})).is_err());
        // null / false are fine
        assert!(
            tr(json!({"model": "m", "input": "hi", "previous_response_id": null, "background": false}))
                .is_ok()
        );
    }

    #[test]
    fn bad_input_shapes_are_400() {
        assert!(tr(json!({"model": "m"})).is_err());
        assert!(tr(json!({"model": "m", "input": 5})).is_err());
        assert!(tr(json!({"model": "m", "input": []})).is_err());
        assert!(
            tr(json!({"model": "m", "input": [{"type": "item_reference", "id": "x"}]})).is_err()
        );
        assert!(tr(json!({"model": "m", "input": [{"type": "computer_call"}]})).is_err());
    }

    // ── non-stream response translation ──

    #[test]
    fn response_text_only() {
        let chat = json!({
            "choices": [{"message": {"role": "assistant", "content": "hi there"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8},
        });
        let r = translate_chat_response(&meta(), &chat).unwrap();
        assert_eq!(r["object"], "response");
        assert!(r["id"].as_str().unwrap().starts_with("resp_"));
        assert_eq!(r["status"], "completed");
        assert_eq!(r["model"], "m");
        assert_eq!(r["incomplete_details"], Value::Null);
        assert_eq!(r["output"].as_array().unwrap().len(), 1);
        let msg = &r["output"][0];
        assert_eq!(msg["type"], "message");
        assert!(msg["id"].as_str().unwrap().starts_with("msg_"));
        assert_eq!(msg["status"], "completed");
        assert_eq!(msg["content"][0]["type"], "output_text");
        assert_eq!(msg["content"][0]["text"], "hi there");
        assert_eq!(msg["content"][0]["annotations"], json!([]));
        assert_eq!(r["output_text"], "hi there");
        assert_eq!(r["usage"]["input_tokens"], 5);
        assert_eq!(r["usage"]["output_tokens"], 3);
        assert_eq!(r["usage"]["total_tokens"], 8);
    }

    #[test]
    fn response_with_reasoning() {
        let chat = json!({
            "choices": [{"message": {"role": "assistant", "content": "42", "reasoning_content": "think think"}, "finish_reason": "stop"}],
        });
        let r = translate_chat_response(&meta(), &chat).unwrap();
        let out = r["output"].as_array().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["type"], "reasoning");
        assert!(out[0]["id"].as_str().unwrap().starts_with("rs_"));
        assert_eq!(
            out[0]["summary"],
            json!([{"type": "summary_text", "text": "think think"}])
        );
        assert_eq!(out[1]["type"], "message");
        assert_eq!(r["usage"]["total_tokens"], 0);
    }

    #[test]
    fn response_with_tool_calls() {
        let chat = json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_a", "type": "function", "function": {"name": "get_weather", "arguments": "{\"q\":1}"}},
                {"id": "call_b", "type": "function", "function": {"name": "get_time", "arguments": "{}"}},
            ]}, "finish_reason": "tool_calls"}],
        });
        let r = translate_chat_response(&meta(), &chat).unwrap();
        let out = r["output"].as_array().unwrap();
        assert_eq!(out.len(), 2, "no empty message alongside tool calls");
        assert_eq!(out[0]["type"], "function_call");
        assert!(out[0]["id"].as_str().unwrap().starts_with("fc_"));
        assert_eq!(out[0]["call_id"], "call_a");
        assert_eq!(out[0]["name"], "get_weather");
        assert_eq!(out[0]["arguments"], "{\"q\":1}");
        assert_eq!(out[0]["status"], "completed");
        assert_eq!(r["status"], "completed");
    }

    #[test]
    fn response_length_is_incomplete() {
        let chat = json!({
            "choices": [{"message": {"role": "assistant", "content": "trunc"}, "finish_reason": "length"}],
        });
        let r = translate_chat_response(&meta(), &chat).unwrap();
        assert_eq!(r["status"], "incomplete");
        assert_eq!(
            r["incomplete_details"],
            json!({"reason": "max_output_tokens"})
        );
        assert_eq!(r["output"][0]["status"], "incomplete");
    }

    #[test]
    fn response_without_choices_is_error() {
        assert!(translate_chat_response(&meta(), &json!({"id": "x"})).is_err());
    }

    #[test]
    fn envelope_echoes_request_fields() {
        let m = ResponseMeta::from_request(&json!({
            "model": "m", "input": "hi", "instructions": "sys", "max_output_tokens": 9,
            "temperature": 0.5, "store": true,
        }));
        let r = m.envelope("completed", &[], None, None, None);
        assert_eq!(r["instructions"], "sys");
        assert_eq!(r["max_output_tokens"], 9);
        assert_eq!(r["temperature"], 0.5);
        assert_eq!(r["store"], false);
        assert_eq!(r["parallel_tool_calls"], true);
        assert_eq!(r["tool_choice"], "auto");
        assert_eq!(r["tools"], json!([]));
    }

    // ── stream translation ──

    fn parse_frames(frames: &[String]) -> Vec<(String, Value)> {
        frames
            .iter()
            .map(|f| {
                let mut lines = f.lines();
                let ev = lines
                    .next()
                    .unwrap()
                    .strip_prefix("event: ")
                    .unwrap()
                    .to_string();
                let data = lines.next().unwrap().strip_prefix("data: ").unwrap();
                assert!(f.ends_with("\n\n"));
                let v: Value = serde_json::from_str(data).unwrap();
                assert_eq!(v["type"], ev, "event name must match data.type");
                (ev, v)
            })
            .collect()
    }

    fn run(chunks: &[&[u8]]) -> Vec<(String, Value)> {
        let mut t = StreamTranslator::new(meta());
        let mut frames = t.start();
        for c in chunks {
            frames.extend(t.push(c));
        }
        frames.extend(t.finish());
        parse_frames(&frames)
    }

    fn types(evs: &[(String, Value)]) -> Vec<&str> {
        evs.iter().map(|(e, _)| e.as_str()).collect()
    }

    fn assert_seq_monotonic(evs: &[(String, Value)]) {
        for (i, (_, v)) in evs.iter().enumerate() {
            assert_eq!(v["sequence_number"], i as u64, "sequence gap at {i}");
        }
    }

    const TEXT_SSE: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo é\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn stream_text_only_event_order() {
        let evs = run(&[TEXT_SSE.as_bytes()]);
        assert_eq!(
            types(&evs),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_seq_monotonic(&evs);
        assert_eq!(evs[0].1["response"]["status"], "in_progress");
        let d1 = &evs[4].1;
        assert_eq!(d1["delta"], "Hel");
        assert_eq!(d1["output_index"], 0);
        assert_eq!(d1["content_index"], 0);
        assert!(d1["item_id"].as_str().unwrap().starts_with("msg_"));
        assert_eq!(evs[6].1["text"], "Hello é");
        let last = &evs.last().unwrap().1["response"];
        assert_eq!(last["status"], "completed");
        assert_eq!(last["output"][0]["content"][0]["text"], "Hello é");
        assert_eq!(last["output"][0]["status"], "completed");
        assert_eq!(last["usage"]["input_tokens"], 4);
        assert_eq!(last["usage"]["output_tokens"], 2);
        assert_eq!(last["usage"]["total_tokens"], 6);
        assert_eq!(last["output_text"], "Hello é");
    }

    #[test]
    fn stream_tolerates_arbitrary_byte_splits() {
        let whole = run(&[TEXT_SSE.as_bytes()]);
        let bytes = TEXT_SSE.as_bytes();
        // Every single-byte split (includes mid-UTF-8 and mid-line).
        let one_byte: Vec<&[u8]> = bytes.chunks(1).collect();
        let split = run(&one_byte);
        assert_eq!(types(&whole), types(&split));
        let text = |evs: &[(String, Value)]| {
            evs.iter()
                .filter(|(e, _)| e == "response.output_text.delta")
                .map(|(_, v)| v["delta"].as_str().unwrap().to_string())
                .collect::<String>()
        };
        assert_eq!(text(&split), "Hello é");
        assert_seq_monotonic(&split);
        // CRLF framing also works.
        let crlf = TEXT_SSE.replace('\n', "\r\n");
        let evs = run(&[crlf.as_bytes()]);
        assert_eq!(types(&evs), types(&whole));
    }

    #[test]
    fn stream_without_trailing_done_or_newline() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n\
                   data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}";
        let evs = run(&[sse.as_bytes()]);
        assert_eq!(evs.last().unwrap().0, "response.completed");
    }

    #[test]
    fn stream_length_is_incomplete() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"abc\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let evs = run(&[sse.as_bytes()]);
        let (ev, v) = evs.last().unwrap();
        assert_eq!(ev, "response.incomplete");
        assert_eq!(v["response"]["status"], "incomplete");
        assert_eq!(
            v["response"]["incomplete_details"],
            json!({"reason": "max_output_tokens"})
        );
        assert_eq!(v["response"]["output"][0]["status"], "incomplete");
        assert!(evs.iter().all(|(e, _)| e != "response.completed"));
    }

    #[test]
    fn stream_reasoning_then_text() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hm\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"mm\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let evs = run(&[sse.as_bytes()]);
        assert_eq!(
            types(&evs),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.reasoning_summary_part.added",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.done",
                "response.reasoning_summary_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_seq_monotonic(&evs);
        assert_eq!(evs[2].1["output_index"], 0);
        assert_eq!(evs[9].1["output_index"], 1);
        assert_eq!(evs[6].1["text"], "hmmm");
        let out = &evs.last().unwrap().1["response"]["output"];
        assert_eq!(out[0]["type"], "reasoning");
        assert_eq!(out[0]["summary"][0]["text"], "hmmm");
        assert_eq!(out[1]["type"], "message");
    }

    #[test]
    fn stream_tool_call_event_order() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null,\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Paris\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":9}}\n\n",
            "data: [DONE]\n\n",
        );
        let evs = run(&[sse.as_bytes()]);
        assert_eq!(
            types(&evs),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_seq_monotonic(&evs);
        let added = &evs[2].1["item"];
        assert_eq!(added["type"], "function_call");
        assert_eq!(added["call_id"], "call_9");
        assert_eq!(added["name"], "get_weather");
        assert_eq!(added["status"], "in_progress");
        assert_eq!(evs[3].1["delta"], "{\"q\":");
        assert_eq!(evs[5].1["arguments"], "{\"q\":\"Paris\"}");
        assert_eq!(evs[5].1["name"], "get_weather");
        let last = &evs.last().unwrap().1["response"];
        assert_eq!(last["status"], "completed");
        let fc = &last["output"][0];
        assert_eq!(fc["arguments"], "{\"q\":\"Paris\"}");
        assert_eq!(fc["status"], "completed");
        assert_eq!(last["usage"]["total_tokens"], 16);
    }

    #[test]
    fn stream_tool_call_args_before_name_are_flushed_on_open() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"f\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let evs = run(&[sse.as_bytes()]);
        let added = evs
            .iter()
            .position(|(e, _)| e == "response.output_item.added")
            .unwrap();
        assert_eq!(evs[added + 1].0, "response.function_call_arguments.delta");
        assert_eq!(evs[added + 1].1["delta"], "{}");
    }

    #[test]
    fn stream_text_then_two_tool_calls() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"let me check\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"b\",\"function\":{\"name\":\"g\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let evs = run(&[sse.as_bytes()]);
        assert_seq_monotonic(&evs);
        let out = evs.last().unwrap().1["response"]["output"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["type"], "message");
        assert_eq!(out[1]["call_id"], "a");
        assert_eq!(out[2]["call_id"], "b");
        let done_idx: Vec<u64> = evs
            .iter()
            .filter(|(e, _)| e == "response.output_item.done")
            .map(|(_, v)| v["output_index"].as_u64().unwrap())
            .collect();
        assert_eq!(done_idx, [0, 1, 2]);
    }

    #[test]
    fn stream_upstream_error_frame_fails() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"par\"},\"finish_reason\":null}]}\n\n",
            "data: {\"error\":{\"message\":\"engine exploded\",\"type\":\"server_error\"}}\n\n",
        );
        let evs = run(&[sse.as_bytes()]);
        assert_seq_monotonic(&evs);
        let (ev, v) = evs.last().unwrap();
        assert_eq!(ev, "response.failed");
        assert_eq!(v["response"]["status"], "failed");
        assert_eq!(v["response"]["error"]["message"], "engine exploded");
        assert!(evs.iter().all(|(e, _)| e != "response.completed"));
    }

    #[test]
    fn stream_cut_off_without_finish_fails() {
        let sse =
            "data: {\"choices\":[{\"delta\":{\"content\":\"par\"},\"finish_reason\":null}]}\n\n";
        let evs = run(&[sse.as_bytes()]);
        assert_eq!(evs.last().unwrap().0, "response.failed");
        // The partial item is still closed out before the terminal event.
        assert!(evs.iter().any(|(e, _)| e == "response.output_item.done"));
    }

    #[test]
    fn stream_no_events_after_terminal() {
        let mut t = StreamTranslator::new(meta());
        t.start();
        let a = t.push(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
        assert!(!a.is_empty() && t.is_terminal());
        assert!(
            t.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n")
                .is_empty()
        );
        assert!(t.finish().is_empty());
        assert!(t.abort("x").is_empty());
    }

    #[test]
    fn no_done_sentinel_in_output() {
        let mut t = StreamTranslator::new(meta());
        let mut frames = t.start();
        frames.extend(t.push(TEXT_SSE.as_bytes()));
        assert!(frames.iter().all(|f| !f.contains("[DONE]")));
    }

    #[tokio::test]
    async fn translate_chat_stream_body_end_to_end() {
        let body = Body::from(TEXT_SSE);
        let out = translate_chat_stream(body, meta());
        let bytes = axum::body::to_bytes(out, 1024 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.starts_with("event: response.created\n"));
        assert!(text.trim_end().ends_with('}'));
        assert!(text.contains("event: response.completed\n"));
    }
}
