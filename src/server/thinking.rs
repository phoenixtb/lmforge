use tracing::{debug, warn};

use crate::model::index::ModelCapabilities;

/// Extract `<think>...</think>` content from inline text.
/// Returns (reasoning_content, clean_content).
///
/// Used when engines (llama.cpp, SGLang) embed thinking inside the
/// content field rather than using a dedicated `reasoning_content` field.
pub fn extract_think_tags(content: &str) -> (Option<String>, String) {
    if let Some(start) = content.find("<think>")
        && let Some(end) = content.find("</think>")
    {
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
            if let Some(message) = choice.get_mut("message")
                && let Some(content) = message.get("content").and_then(|c| c.as_str())
            {
                let (reasoning, clean) = extract_think_tags(content);
                if let Some(reasoning) = reasoning {
                    message["content"] = serde_json::Value::String(clean);
                    message["reasoning_content"] = serde_json::Value::String(reasoning);
                }
            }
            // If engine already provides reasoning_content, pass it through
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
            if let Some(delta) = choice.get_mut("delta")
                && let Some(content) = delta.get("content").and_then(|c| c.as_str())
            {
                let (reasoning, clean) = extract_think_tags(content);
                if let Some(reasoning) = reasoning {
                    delta["content"] = serde_json::Value::String(clean);
                    delta["reasoning_content"] = serde_json::Value::String(reasoning);
                }
            }
            // Pass through native reasoning_content from the engine
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
            // Strip enable_thinking first — re-apply precisely below.
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

                // Strip num_ctx — it's an Ollama-specific context-window parameter.
                // oMLX uses the OpenAI API and doesn't understand it; context length
                // is set at model-load time, not per-request. Forwarding it is a noop
                // at best, and may interact with oMLX's generation budget at worst.
                obj.remove("num_ctx");

                // ── Penalty translation (oMLX only) ───────────────────────────────
                // oMLX (mlx-lm) silently ignores the OpenAI `frequency_penalty` and
                // `presence_penalty` fields — they are accepted without error but have
                // no effect on generation. The engine-native equivalent is
                // `repetition_penalty` (multiplicative: 1.0 = neutral, >1.0 = penalise).
                //
                // LMForge translates to honour the OpenAI contract for all clients.
                // Formula (documented approximation — exact OpenAI math requires
                // per-token state inside the inference loop):
                //
                //   repetition_penalty = clamp(1.0 + (freq + pres) × 0.33, 1.0, 1.3)
                //
                // Rules:
                //   • Only derive if at least one penalty is non-zero.
                //   • If the client already set `repetition_penalty` explicitly, that
                //     value wins — no override.
                //   • Both OpenAI params are removed after translation; leaving them
                //     would mislead engine logs and future readers.
                //   • llamacpp/SGLang support these natively — no translation there.
                {
                    let freq = obj
                        .get("frequency_penalty")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let pres = obj
                        .get("presence_penalty")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let client_rep = obj.contains_key("repetition_penalty");

                    if (freq > 0.0 || pres > 0.0) && !client_rep {
                        let derived = (1.0_f64 + (freq + pres) * 0.33).clamp(1.0, 1.3);
                        debug!(
                            frequency_penalty = freq,
                            presence_penalty = pres,
                            derived_repetition_penalty = derived,
                            "Derived repetition_penalty from OpenAI penalty params for oMLX"
                        );
                        obj.insert(
                            "repetition_penalty".to_string(),
                            serde_json::Value::from(derived),
                        );
                    }
                    // Always remove — oMLX ignores them, and leaving them pollutes the body.
                    obj.remove("frequency_penalty");
                    obj.remove("presence_penalty");
                }
            }

            match effective_think {
                Some(false) => {
                    // Suppress reasoning — only needed for thinking-capable models.
                    // Non-thinking models (Gemma, Llama, Phi) never generate <think> tokens.
                    let is_thinking = model_caps.map(|c| c.thinking).unwrap_or(false);
                    if is_thinking && let Some(obj) = body.as_object_mut() {
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
                Some(true) | None => {
                    // think:true or absent → omit enable_thinking flag; natural reasoning applies.
                    // enable_thinking:true is NEVER forwarded to oMLX.
                    //
                    // Advisory: Qwen3 models require temperature >= 0.6 in thinking mode.
                    // Low temperature (< 0.6) causes deterministic repetition loops in reasoning.
                    // LMForge does NOT override the client's temperature — it is the client's
                    // responsibility to set the correct temperature for the model and mode.
                    // DocIntel should use a separate `llm_thinking_temperature` setting (>= 0.6).
                    //
                    // We log a warning here so operators can diagnose repetition loops in logs.
                    let is_thinking = model_caps.map(|c| c.thinking).unwrap_or(false);
                    if is_thinking && let Some(obj) = body.as_object_mut() {
                        let current_temp = obj
                            .get("temperature")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(1.0);

                        if current_temp < 0.6 {
                            warn!(
                                current_temp,
                                recommended_minimum = 0.6,
                                "temperature below recommended minimum for Qwen3 thinking mode — \
                                     repetition loops are likely. Set llm_thinking_temperature >= 0.6 \
                                     in the calling client. LMForge will NOT override the client value."
                            );
                        }
                    }
                }
            }
        }

        "llamacpp" | "sglang" => {
            // If explicit `think` field given, set/override enable_thinking (field always wins).
            // If only kwargs form was sent directly, leave it as-is (already correct form).
            if let Some(think) = think_bool_from_field
                && let Some(obj) = body.as_object_mut()
            {
                let kwargs = obj
                    .entry("chat_template_kwargs")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(map) = kwargs.as_object_mut() {
                    map.insert(
                        "enable_thinking".to_string(),
                        serde_json::Value::Bool(think),
                    );
                }
            }
        }

        _ => {
            debug!(
                engine_id,
                "Unknown engine ID in apply_think_for_engine — stripping think field only"
            );
        }
    }
}

/// Extract and validate `thinking_budget` from the request body.
///
/// Returns `Some(n)` where `n > 0` if the field is present and positive.
/// Returns `None` if absent, zero, or non-positive.
///
/// The field is NOT removed here — the caller (route handler) is responsible
/// for stripping it from the body before forwarding to the engine, since the
/// engine has no concept of `thinking_budget`.
pub fn extract_thinking_budget(body: &serde_json::Value) -> Option<u32> {
    body.get("thinking_budget")
        .and_then(|v| v.as_u64())
        .filter(|&n| n > 0)
        .map(|n| n as u32)
}

/// Extracts `stream_reasoning_deltas` from the `extra_body` or root.
/// Returns false if not present or not a boolean. This is a read-only
/// operation; it does not remove the field from the request body.
pub fn extract_stream_reasoning_deltas(body: &serde_json::Value) -> bool {
    if let Some(extra) = body.get("extra_body")
        && let Some(v) = extra
            .get("stream_reasoning_deltas")
            .and_then(|v| v.as_bool())
    {
        return v;
    }
    body.get("stream_reasoning_deltas")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
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
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "enable_thinking:true must NEVER reach oMLX"
        );
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
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"], false,
            "enable_thinking:false must be forwarded — empirically confirmed to suppress reasoning"
        );
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
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "non-thinking model: enable_thinking flag is irrelevant"
        );
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
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "enable_thinking:true sent directly must be stripped (infinite loop risk)"
        );
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
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"], false,
            "enable_thinking:false sent directly must be honoured"
        );
    }

    // ── oMLX: other kwargs preserved when enable_thinking is stripped ─────────

    #[test]
    fn test_omlx_strips_enable_thinking_preserves_other_kwargs() {
        let mut body = serde_json::json!({
            "model": "test", "messages": [],
            "chat_template_kwargs": {"enable_thinking": true, "other_key": "value"}
        });
        apply_think_for_engine(&mut body, "omlx", None);
        assert!(
            body["chat_template_kwargs"]
                .get("enable_thinking")
                .is_none()
        );
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
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"], false,
            "explicit think field must override existing kwargs.enable_thinking"
        );
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
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"], false,
            "no think field: existing kwargs pass through unchanged"
        );
    }

    #[test]
    fn test_sglang_think_true_sets_enable_thinking() {
        let mut body = serde_json::json!({"model": "test", "think": true});
        apply_think_for_engine(&mut body, "sglang", None);
        assert!(body.get("think").is_none());
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    // ── oMLX: temperature advisory (warn-only, no modification) ──────────────

    #[test]
    fn test_omlx_think_true_does_not_modify_low_temperature() {
        // LMForge must NOT clamp or override temperature — that belongs to the client.
        // Qwen3's 0.6 minimum is model-specific; applying it universally would break
        // other thinking models. We warn in logs but leave the value untouched.
        let caps = crate::model::index::ModelCapabilities {
            chat: true,
            embeddings: false,
            reranking: false,
            thinking: true,
            embedding_dims: None,
            pooling: None,
        };
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "think": true,
            "temperature": 0.1
        });
        apply_think_for_engine(&mut body, "omlx", Some(&caps));
        // Temperature must be passed through exactly as sent by client
        let t = body["temperature"].as_f64().unwrap();
        assert!(
            (t - 0.1).abs() < 1e-9,
            "LMForge must not modify client temperature; expected 0.1, got {t}"
        );
        // presence_penalty must NOT be injected
        assert!(
            body.get("presence_penalty").is_none(),
            "LMForge must not inject presence_penalty — client owns sampling params"
        );
    }

    #[test]
    fn test_omlx_think_true_high_temperature_unchanged() {
        let caps = crate::model::index::ModelCapabilities {
            chat: true,
            embeddings: false,
            reranking: false,
            thinking: true,
            embedding_dims: None,
            pooling: None,
        };
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "think": true,
            "temperature": 0.8
        });
        apply_think_for_engine(&mut body, "omlx", Some(&caps));
        let t = body["temperature"].as_f64().unwrap();
        assert!(
            (t - 0.8).abs() < 1e-9,
            "High temperature must be left untouched, got {t}"
        );
    }

    // ── Part A: penalty translation (oMLX only) ───────────────────────────────

    #[test]
    fn test_omlx_derives_repetition_penalty_from_freq_penalty() {
        // frequency_penalty:0.3 → repetition_penalty ≈ 1.099
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "frequency_penalty": 0.3
        });
        apply_think_for_engine(&mut body, "omlx", None);
        let rep = body["repetition_penalty"]
            .as_f64()
            .expect("repetition_penalty should be set");
        assert!((rep - 1.099).abs() < 0.001, "Expected ~1.099, got {rep}");
        // OpenAI params must be removed
        assert!(
            body.get("frequency_penalty").is_none(),
            "frequency_penalty must be removed"
        );
        assert!(
            body.get("presence_penalty").is_none(),
            "presence_penalty must be removed"
        );
    }

    #[test]
    fn test_omlx_derives_repetition_penalty_from_both_penalties() {
        // 0.3 + 0.3 = 0.6 × 0.33 = 0.198 → 1.198
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "frequency_penalty": 0.3,
            "presence_penalty": 0.3
        });
        apply_think_for_engine(&mut body, "omlx", None);
        let rep = body["repetition_penalty"]
            .as_f64()
            .expect("repetition_penalty should be set");
        assert!((rep - 1.198).abs() < 0.001, "Expected ~1.198, got {rep}");
    }

    #[test]
    fn test_omlx_client_repetition_penalty_wins_over_derivation() {
        // Client explicitly sets repetition_penalty → must not be overridden
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "frequency_penalty": 0.3,
            "repetition_penalty": 1.15
        });
        apply_think_for_engine(&mut body, "omlx", None);
        let rep = body["repetition_penalty"]
            .as_f64()
            .expect("repetition_penalty should be set");
        assert!(
            (rep - 1.15).abs() < 1e-9,
            "Client's explicit repetition_penalty must be preserved, got {rep}"
        );
    }

    #[test]
    fn test_omlx_zero_penalties_no_derivation() {
        // freq=0 pres=0 → no repetition_penalty should be added
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "frequency_penalty": 0.0,
            "presence_penalty": 0.0
        });
        apply_think_for_engine(&mut body, "omlx", None);
        assert!(
            body.get("repetition_penalty").is_none(),
            "No repetition_penalty should be derived when both penalties are zero"
        );
        // OpenAI params still removed (they're useless for oMLX)
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("presence_penalty").is_none());
    }

    #[test]
    fn test_omlx_clamped_at_1_3() {
        // Large frequency_penalty → clamped at 1.3
        let mut body = serde_json::json!({
            "model": "qwen3.5:4b:4bit",
            "messages": [],
            "frequency_penalty": 2.0
        });
        apply_think_for_engine(&mut body, "omlx", None);
        let rep = body["repetition_penalty"]
            .as_f64()
            .expect("repetition_penalty should be set");
        assert!(
            rep <= 1.3 + 1e-9,
            "repetition_penalty must be clamped at 1.3, got {rep}"
        );
    }

    #[test]
    fn test_llamacpp_freq_penalty_passes_through_unchanged() {
        // llamacpp supports frequency_penalty natively — no translation
        let mut body = serde_json::json!({
            "model": "llama3:8b",
            "messages": [],
            "frequency_penalty": 0.3,
            "presence_penalty": 0.2
        });
        apply_think_for_engine(&mut body, "llamacpp", None);
        assert_eq!(
            body["frequency_penalty"].as_f64().unwrap(),
            0.3,
            "frequency_penalty must pass through unchanged for llamacpp"
        );
        assert_eq!(
            body["presence_penalty"].as_f64().unwrap(),
            0.2,
            "presence_penalty must pass through unchanged for llamacpp"
        );
        assert!(
            body.get("repetition_penalty").is_none(),
            "No repetition_penalty should be derived for llamacpp"
        );
    }

    // ── Part B: extract_thinking_budget ───────────────────────────────────────

    #[test]
    fn test_extract_thinking_budget_present() {
        let body = serde_json::json!({"thinking_budget": 1024});
        assert_eq!(extract_thinking_budget(&body), Some(1024u32));
    }

    #[test]
    fn test_extract_thinking_budget_absent() {
        let body = serde_json::json!({"model": "test"});
        assert_eq!(extract_thinking_budget(&body), None);
    }

    #[test]
    fn test_extract_thinking_budget_zero_returns_none() {
        let body = serde_json::json!({"thinking_budget": 0});
        assert_eq!(extract_thinking_budget(&body), None);
    }

    #[test]
    fn test_extract_thinking_budget_does_not_remove_field() {
        // extract_thinking_budget must be read-only; removal is the caller's job
        let body = serde_json::json!({"thinking_budget": 512, "model": "test"});
        let _ = extract_thinking_budget(&body);
        assert!(
            body.get("thinking_budget").is_some(),
            "field must not be removed by extractor"
        );
    }

    // ── Part C: extract_stream_reasoning_deltas ─────────────────────────────

    #[test]
    fn test_extract_stream_reasoning_deltas_in_extra_body() {
        let body = serde_json::json!({
            "extra_body": {
                "stream_reasoning_deltas": true
            }
        });
        assert_eq!(extract_stream_reasoning_deltas(&body), true);
    }

    #[test]
    fn test_extract_stream_reasoning_deltas_in_root() {
        let body = serde_json::json!({
            "stream_reasoning_deltas": true
        });
        assert_eq!(extract_stream_reasoning_deltas(&body), true);
    }

    #[test]
    fn test_extract_stream_reasoning_deltas_absent() {
        let body = serde_json::json!({"model": "test"});
        assert_eq!(extract_stream_reasoning_deltas(&body), false);
    }

    #[test]
    fn test_extract_stream_reasoning_deltas_wrong_type() {
        let body = serde_json::json!({"stream_reasoning_deltas": "true"});
        assert_eq!(extract_stream_reasoning_deltas(&body), false);
    }
}
