use tracing::debug;

use crate::model::index::ModelCapabilities;

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
                if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                },
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

/// Engine-aware think-mode translation. Must be called AFTER `request_has_think` captures
/// the original client intent, as this function removes the `think` field from the body.
///
/// **oMLX** — only `enable_thinking: false` is safe to forward. `enable_thinking: true`
/// activates oMLX's extended thinking mode which bypasses the model's generation budget
/// and causes non-deterministic infinite loops. Confirmed by live engine testing:
///   • `enable_thinking: true`  → infinite loop — NEVER send
///   • `enable_thinking: false` → 0 reasoning tokens, terminates cleanly ✓
///   • no flag                  → natural Qwen3 reasoning, budget enforced by weights ✓
///
/// The `<nothink>` prefix approach was also tested empirically and does NOT work —
/// oMLX renders the partial assistant message as a completed turn then generates a
/// fresh response with full reasoning regardless.
///
/// **llamacpp / sglang** — both engines use HF Jinja templates, budget-bounded and safe.
/// The top-level `think` field always overrides any existing `chat_template_kwargs`.
///
/// **Effective intent**: top-level `think` field > `chat_template_kwargs.enable_thinking`.
/// If only the kwargs form is present (direct send), that intent is honoured.
pub fn apply_think_for_engine(
    body: &mut serde_json::Value,
    engine_id: &str,
    model_caps: Option<&ModelCapabilities>,
) {
    // Capture think intent from both sources BEFORE any mutation.
    // Top-level `think` field takes precedence over chat_template_kwargs.enable_thinking.
    // We read kwargs NOW (before stripping) so a bare `enable_thinking: false` in kwargs
    // (no `think` field) is not silently lost — that is a meaningful suppress request.
    let think_from_field = body.as_object_mut().and_then(|obj| obj.remove("think"));
    let think_bool_from_field = think_from_field.as_ref().and_then(|v| v.as_bool());

    let think_bool_from_kwargs = body
        .get("chat_template_kwargs")
        .and_then(|k| k.get("enable_thinking"))
        .and_then(|v| v.as_bool());

    // Effective intent: field wins; fall back to kwargs; None = absent
    let effective_think: Option<bool> = think_bool_from_field.or(think_bool_from_kwargs);

    match engine_id {
        "omlx" => {
            // Always strip enable_thinking first — re-apply precisely below.
            if let Some(obj) = body.as_object_mut() {
                if let Some(kwargs) = obj
                    .get_mut("chat_template_kwargs")
                    .and_then(|k| k.as_object_mut())
                {
                    kwargs.remove("enable_thinking");
                }
                let empty = obj
                    .get("chat_template_kwargs")
                    .and_then(|k| k.as_object())
                    .map(|m| m.is_empty())
                    .unwrap_or(false);
                if empty {
                    obj.remove("chat_template_kwargs");
                }
            }

            match effective_think {
                Some(false) => {
                    // Suppress reasoning — only needed for thinking-capable models.
                    // Non-thinking models (Gemma, Llama, Phi) never generate <think> tokens.
                    let is_thinking = model_caps.map(|c| c.thinking).unwrap_or(false);
                    if is_thinking {
                        if let Some(obj) = body.as_object_mut() {
                            let kwargs = obj
                                .entry("chat_template_kwargs")
                                .or_insert_with(|| serde_json::json!({}));
                            if let Some(map) = kwargs.as_object_mut() {
                                map.insert(
                                    "enable_thinking".to_string(),
                                    serde_json::Value::Bool(false),
                                );
                            }
                        }
                    }
                }
                Some(true) | None => {
                    // think:true or absent → omit flag; natural reasoning applies.
                    // enable_thinking:true is NEVER forwarded to oMLX.
                }
            }
        }

        "llamacpp" | "sglang" => {
            // If explicit `think` field given, set/override enable_thinking (field always wins).
            // If only kwargs form was sent directly, leave it as-is (already correct form).
            if let Some(think) = think_bool_from_field {
                if let Some(obj) = body.as_object_mut() {
                    let kwargs = obj
                        .entry("chat_template_kwargs")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(map) = kwargs.as_object_mut() {
                        map.insert("enable_thinking".to_string(), serde_json::Value::Bool(think));
                    }
                }
            }
        }

        _ => {
            debug!(engine_id, "Unknown engine ID in apply_think_for_engine — stripping think field only");
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
        assert_eq!(
            parsed["choices"][0]["message"]["reasoning_content"],
            "step 1"
        );
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
        let line =
            r#"data: {"choices":[{"delta":{"content":"<think>reasoning here</think>answer"}}]}"#;
        let result = inject_reasoning_content_delta(line);
        let data = result.strip_prefix("data: ").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "answer");
        assert_eq!(
            parsed["choices"][0]["delta"]["reasoning_content"],
            "reasoning here"
        );
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
        let body =
            serde_json::json!({"model": "test", "chat_template_kwargs": {"enable_thinking": true}});
        assert!(request_has_think(&body));

        // chat_template_kwargs takes precedence over think
        let body = serde_json::json!({"model": "test", "think": false, "chat_template_kwargs": {"enable_thinking": true}});
        assert!(request_has_think(&body));
    }

    fn thinking_caps() -> ModelCapabilities {
        ModelCapabilities {
            chat: true,
            embeddings: false,
            reranking: false,
            thinking: true,
            embedding_dims: None,
            pooling: None,
        }
    }

    fn non_thinking_caps() -> ModelCapabilities {
        ModelCapabilities {
            chat: true,
            embeddings: false,
            reranking: false,
            thinking: false,
            embedding_dims: None,
            pooling: None,
        }
    }

    // ── oMLX case 1: think:true, thinking model → no flag ────────────────────

    #[test]
    fn test_omlx_think_true_thinking_model_no_flag() {
        let mut body = serde_json::json!({
            "model": "test",
            "messages": [{"role": "user", "content": "hi"}],
            "think": true
        });
        apply_think_for_engine(&mut body, "omlx", Some(&thinking_caps()));
        assert!(body.get("think").is_none());
        assert!(body.get("chat_template_kwargs").is_none(),
            "enable_thinking:true must NEVER reach oMLX");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    // ── oMLX case 2: think:true, non-thinking model → no flag ────────────────

    #[test]
    fn test_omlx_think_true_non_thinking_model_no_flag() {
        let mut body = serde_json::json!({
            "model": "test", "messages": [], "think": true
        });
        apply_think_for_engine(&mut body, "omlx", Some(&non_thinking_caps()));
        assert!(body.get("think").is_none());
        assert!(body.get("chat_template_kwargs").is_none());
    }

    // ── oMLX case 3: think:false, thinking model → enable_thinking:false ─────
    // Confirmed by direct engine test: 0 reasoning tokens, clean termination.

    #[test]
    fn test_omlx_think_false_thinking_model_sends_enable_thinking_false() {
        let mut body = serde_json::json!({
            "model": "test",
            "messages": [{"role": "user", "content": "hi"}],
            "think": false
        });
        apply_think_for_engine(&mut body, "omlx", Some(&thinking_caps()));
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false,
            "enable_thinking:false must be forwarded — empirically confirmed to suppress reasoning");
        // No <nothink> prefix — tested empirically, does NOT suppress reasoning on oMLX
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    // ── oMLX case 4: think:false, non-thinking model → no flag ───────────────

    #[test]
    fn test_omlx_think_false_non_thinking_model_no_flag() {
        let mut body = serde_json::json!({
            "model": "test", "messages": [], "think": false
        });
        apply_think_for_engine(&mut body, "omlx", Some(&non_thinking_caps()));
        assert!(body.get("think").is_none());
        assert!(body.get("chat_template_kwargs").is_none(),
            "non-thinking model: enable_thinking flag is irrelevant");
    }

    // ── oMLX case 5: no think field → no flag ────────────────────────────────

    #[test]
    fn test_omlx_no_think_field_no_flag() {
        for caps in [Some(thinking_caps()), Some(non_thinking_caps())] {
            let mut body = serde_json::json!({"model": "test", "messages": []});
            apply_think_for_engine(&mut body, "omlx", caps.as_ref());
            assert!(body.get("think").is_none());
            assert!(body.get("chat_template_kwargs").is_none());
        }
    }

    // ── oMLX case 6: direct kwargs.enable_thinking:true → strip (dangerous) ──

    #[test]
    fn test_omlx_direct_kwargs_enable_thinking_true_stripped() {
        let mut body = serde_json::json!({
            "model": "test", "messages": [],
            "chat_template_kwargs": {"enable_thinking": true}
        });
        apply_think_for_engine(&mut body, "omlx", Some(&thinking_caps()));
        assert!(body.get("chat_template_kwargs").is_none(),
            "enable_thinking:true sent directly must be stripped (infinite loop risk)");
    }

    // ── oMLX case 7: direct kwargs.enable_thinking:false → preserve ──────────
    // Client intent is to suppress reasoning; effective_think = Some(false).

    #[test]
    fn test_omlx_direct_kwargs_enable_thinking_false_preserved() {
        let mut body = serde_json::json!({
            "model": "test", "messages": [],
            "chat_template_kwargs": {"enable_thinking": false}
        });
        apply_think_for_engine(&mut body, "omlx", Some(&thinking_caps()));
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false,
            "enable_thinking:false sent directly must be honoured");
    }

    // ── oMLX: other kwargs preserved when enable_thinking is stripped ─────────

    #[test]
    fn test_omlx_strips_enable_thinking_preserves_other_kwargs() {
        let mut body = serde_json::json!({
            "model": "test", "messages": [],
            "chat_template_kwargs": {"enable_thinking": true, "other_key": "value"}
        });
        apply_think_for_engine(&mut body, "omlx", None);
        assert!(body["chat_template_kwargs"].get("enable_thinking").is_none());
        assert_eq!(body["chat_template_kwargs"]["other_key"], "value");
    }

    // ── oMLX: unknown caps (model not in index) → no flag ────────────────────

    #[test]
    fn test_omlx_think_false_unknown_caps_no_flag() {
        let mut body = serde_json::json!({"model": "test", "messages": [], "think": false});
        apply_think_for_engine(&mut body, "omlx", None);
        // model_caps = None → unwrap_or(false) → not a thinking model → no flag injected
        assert!(body.get("chat_template_kwargs").is_none());
    }

    // ── llamacpp / sglang case 8: think:true → enable_thinking:true ──────────

    #[test]
    fn test_llamacpp_think_true_sets_enable_thinking() {
        let mut body = serde_json::json!({"model": "test", "messages": [], "think": true});
        apply_think_for_engine(&mut body, "llamacpp", Some(&thinking_caps()));
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    // ── llamacpp / sglang case 9: think:false → enable_thinking:false ─────────

    #[test]
    fn test_llamacpp_think_false_sets_enable_thinking_false() {
        let mut body = serde_json::json!({"model": "test", "think": false});
        apply_think_for_engine(&mut body, "llamacpp", None);
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    }

    // ── llamacpp / sglang case 10: think field overrides existing kwargs ──────
    // think:false field + existing kwargs:true → false wins (field always wins).

    #[test]
    fn test_llamacpp_think_field_overrides_existing_kwargs() {
        let mut body = serde_json::json!({
            "model": "test", "think": false,
            "chat_template_kwargs": {"enable_thinking": true}
        });
        apply_think_for_engine(&mut body, "llamacpp", None);
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false,
            "explicit think field must override existing kwargs.enable_thinking");
    }

    // ── llamacpp / sglang case 11: no think field, kwargs present → pass through

    #[test]
    fn test_llamacpp_no_think_field_preserves_existing_kwargs() {
        let mut body = serde_json::json!({
            "model": "test",
            "chat_template_kwargs": {"enable_thinking": false}
        });
        apply_think_for_engine(&mut body, "llamacpp", None);
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false,
            "no think field: existing kwargs pass through unchanged");
    }

    #[test]
    fn test_sglang_think_true_sets_enable_thinking() {
        let mut body = serde_json::json!({"model": "test", "think": true});
        apply_think_for_engine(&mut body, "sglang", None);
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    // ── All engines: absent think = noop ─────────────────────────────────────

    #[test]
    fn test_no_think_key_is_noop_for_all_engines() {
        for engine in ["omlx", "llamacpp", "sglang"] {
            let mut body = serde_json::json!({"model": "test", "messages": []});
            apply_think_for_engine(&mut body, engine, None);
            assert!(body.get("think").is_none());
            assert!(body.get("chat_template_kwargs").is_none(), "engine={engine}");
            assert_eq!(body["messages"].as_array().unwrap().len(), 0, "engine={engine}");
        }
    }
}
