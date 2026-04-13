use tracing::debug;

/// Extract `<think>...</think>` content from inline text.
/// Returns (reasoning_content, clean_content).
///
/// Used when engines (llama.cpp, SGLang) embed thinking inside the
/// content field rather than using a dedicated `reasoning_content` field.
pub fn extract_think_tags(content: &str) -> (Option<String>, String) {
    if let Some(start) = content.find("<think>") {
        if let Some(end) = content.find("</think>") {
            let think_start = start + "<think>".len();
            let reasoning = content[think_start..end].trim().to_string();
            let clean = format!(
                "{}{}",
                &content[..start],
                &content[end + "</think>".len()..]
            )
            .trim()
            .to_string();

            debug!(
                reasoning_len = reasoning.len(),
                content_len = clean.len(),
                "Extracted think tags"
            );

            return (
                if reasoning.is_empty() { None } else { Some(reasoning) },
                clean,
            );
        }
    }
    (None, content.to_string())
}

/// Inject `reasoning_content` field into a non-streaming response JSON.
/// Modifies the response in-place if think tags are found in the content.
pub fn inject_reasoning_content(response_json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(response_json) else {
        return response_json.to_string();
    };

    if let Some(choices) = value.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices.iter_mut() {
            if let Some(message) = choice.get_mut("message") {
                if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                    let (reasoning, clean) = extract_think_tags(content);
                    if let Some(reasoning) = reasoning {
                        message["content"] = serde_json::Value::String(clean);
                        message["reasoning_content"] = serde_json::Value::String(reasoning);
                    }
                }
                // If engine already provides reasoning_content, pass it through
            }
        }
    }

    serde_json::to_string(&value).unwrap_or_else(|_| response_json.to_string())
}

/// Inject `reasoning_content` into a streaming SSE delta chunk.
/// Handles the `data: {...}` format from SSE.
pub fn inject_reasoning_content_delta(sse_line: &str) -> String {
    let data = if let Some(stripped) = sse_line.strip_prefix("data: ") {
        stripped
    } else {
        return sse_line.to_string();
    };

    // Skip [DONE] sentinel
    if data.trim() == "[DONE]" {
        return sse_line.to_string();
    }

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) else {
        return sse_line.to_string();
    };

    if let Some(choices) = value.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices.iter_mut() {
            if let Some(delta) = choice.get_mut("delta") {
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    let (reasoning, clean) = extract_think_tags(content);
                    if let Some(reasoning) = reasoning {
                        delta["content"] = serde_json::Value::String(clean);
                        delta["reasoning_content"] = serde_json::Value::String(reasoning);
                    }
                }
                // Pass through native reasoning_content from the engine
            }
        }
    }

    format!(
        "data: {}",
        serde_json::to_string(&value).unwrap_or_else(|_| data.to_string())
    )
}

/// Check if the effective thinking mode is enabled, considering both
/// the Ollama-standard `think` field and the explicit `chat_template_kwargs`.
pub fn request_has_think(body: &serde_json::Value) -> bool {
    // Explicit chat_template_kwargs takes precedence
    if let Some(enabled) = body
        .get("chat_template_kwargs")
        .and_then(|k| k.get("enable_thinking"))
        .and_then(|v| v.as_bool())
    {
        return enabled;
    }
    // Ollama-standard top-level field
    body.get("think").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Translate Ollama-standard `think: true/false` into the oMLX
/// `chat_template_kwargs: {"enable_thinking": ...}` form, then strip `think`
/// so the engine never sees an unknown field.
///
/// Rules:
/// - If `chat_template_kwargs.enable_thinking` is already present → leave it alone.
/// - If `think` is present and `chat_template_kwargs.enable_thinking` is absent → copy value over.
/// - Always remove the top-level `think` field before forwarding.
pub fn translate_think_field(body: &mut serde_json::Value) {
    let think_val = body.as_object_mut().and_then(|obj| obj.remove("think"));

    let Some(think_bool) = think_val.as_ref().and_then(|v| v.as_bool()) else {
        return; // Nothing to translate
    };

    let obj = body.as_object_mut().unwrap();

    // Only set chat_template_kwargs if enable_thinking is not already there
    let already_set = obj
        .get("chat_template_kwargs")
        .and_then(|k| k.get("enable_thinking"))
        .is_some();

    if !already_set {
        let kwargs = obj
            .entry("chat_template_kwargs")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(map) = kwargs.as_object_mut() {
            map.insert("enable_thinking".to_string(), serde_json::Value::Bool(think_bool));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_think_tags_with_thinking() {
        let content = "<think>Let me work through this step by step.</think>The answer is 42.";
        let (reasoning, clean) = extract_think_tags(content);
        assert_eq!(reasoning.unwrap(), "Let me work through this step by step.");
        assert_eq!(clean, "The answer is 42.");
    }

    #[test]
    fn test_extract_think_tags_no_thinking() {
        let content = "The answer is 42.";
        let (reasoning, clean) = extract_think_tags(content);
        assert!(reasoning.is_none());
        assert_eq!(clean, "The answer is 42.");
    }

    #[test]
    fn test_extract_think_tags_empty_think() {
        let content = "<think></think>The answer is 42.";
        let (reasoning, clean) = extract_think_tags(content);
        assert!(reasoning.is_none());
        assert_eq!(clean, "The answer is 42.");
    }

    #[test]
    fn test_inject_reasoning_content() {
        let response = r#"{"choices":[{"message":{"role":"assistant","content":"<think>step 1</think>The answer."}}]}"#;
        let result = inject_reasoning_content(response);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["choices"][0]["message"]["content"], "The answer.");
        assert_eq!(parsed["choices"][0]["message"]["reasoning_content"], "step 1");
    }

    #[test]
    fn test_inject_reasoning_content_no_think() {
        let response = r#"{"choices":[{"message":{"role":"assistant","content":"Hello!"}}]}"#;
        let result = inject_reasoning_content(response);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["choices"][0]["message"]["content"], "Hello!");
        assert!(parsed["choices"][0]["message"]["reasoning_content"].is_null());
    }

    #[test]
    fn test_inject_reasoning_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"<think>reasoning here</think>answer"}}]}"#;
        let result = inject_reasoning_content_delta(line);
        let data = result.strip_prefix("data: ").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "answer");
        assert_eq!(parsed["choices"][0]["delta"]["reasoning_content"], "reasoning here");
    }

    #[test]
    fn test_inject_reasoning_delta_done() {
        let line = "data: [DONE]";
        assert_eq!(inject_reasoning_content_delta(line), line);
    }

    #[test]
    fn test_request_has_think_top_level() {
        let body = serde_json::json!({"model": "test", "think": true});
        assert!(request_has_think(&body));

        let body = serde_json::json!({"model": "test"});
        assert!(!request_has_think(&body));

        let body = serde_json::json!({"model": "test", "think": false});
        assert!(!request_has_think(&body));
    }

    #[test]
    fn test_request_has_think_via_kwargs() {
        let body = serde_json::json!({"model": "test", "chat_template_kwargs": {"enable_thinking": true}});
        assert!(request_has_think(&body));

        // chat_template_kwargs takes precedence over think
        let body = serde_json::json!({"model": "test", "think": false, "chat_template_kwargs": {"enable_thinking": true}});
        assert!(request_has_think(&body));
    }

    #[test]
    fn test_translate_think_field_sets_kwargs() {
        let mut body = serde_json::json!({"model": "test", "messages": [], "think": true});
        translate_think_field(&mut body);

        // think removed
        assert!(body.get("think").is_none());
        // chat_template_kwargs set
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    #[test]
    fn test_translate_think_field_false() {
        let mut body = serde_json::json!({"model": "test", "think": false});
        translate_think_field(&mut body);
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn test_translate_think_field_does_not_override_existing_kwargs() {
        let mut body = serde_json::json!({
            "model": "test",
            "think": false,
            "chat_template_kwargs": {"enable_thinking": true}
        });
        translate_think_field(&mut body);
        // Existing explicit kwargs wins; think removed
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    #[test]
    fn test_translate_think_field_no_think_key() {
        let mut body = serde_json::json!({"model": "test", "messages": []});
        translate_think_field(&mut body);
        // No changes
        assert!(body.get("think").is_none());
        assert!(body.get("chat_template_kwargs").is_none());
    }
}
