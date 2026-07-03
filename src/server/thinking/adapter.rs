/// Per-engine thinking strategy, surfacing two orthogonal properties needed
/// by the orchestrator routing logic in `openai.rs` and `ollama.rs`.
///
/// # Engine matrix
///
/// | Engine        | `supports_orchestrator` | `inline_think` |
/// |---------------|-------------------------|----------------|
/// | oMLX          | true                    | false          |
/// | llama.cpp     | true                    | true           |
/// | SGLang        | true                    | true           |
/// | vLLM / others | false                   | false          |
///
/// **`supports_orchestrator`** — the engine is wired into the two-call budget
/// path (`proxy_stream_with_thinking_budget` /
/// `proxy_nonstream_with_thinking_budget`). Requires the model to also have
/// `thinking` capability and the request to include `think: true`.
///
/// **`inline_think`** — reasoning MAY arrive as `<think>…</think>` tags embedded
/// in `delta.content` (llama.cpp / SGLang). When false (oMLX) the engine emits
/// `delta.reasoning_content` natively and call-1 of the orchestrator accumulates
/// it directly.
///
/// Note: modern llama-server (`--jinja` default-on, b9xxx) parses reasoning
/// itself and emits `delta.reasoning_content` — Call-1 accumulation therefore
/// reads BOTH channels on the inline path (verified against b9351).
pub trait ThinkingAdapter: Send + Sync {
    fn supports_orchestrator(&self) -> bool;
    fn inline_think(&self) -> bool;
}

/// oMLX (Apple Silicon / Metal) — native `reasoning_content` field.
struct OmlxAdapter;
impl ThinkingAdapter for OmlxAdapter {
    fn supports_orchestrator(&self) -> bool { true }
    fn inline_think(&self) -> bool { false }
}

/// llama.cpp and SGLang — reasoning embedded as `<think>` tags inside `content`.
struct TemplateAdapter;
impl ThinkingAdapter for TemplateAdapter {
    fn supports_orchestrator(&self) -> bool { true }
    fn inline_think(&self) -> bool { true }
}

/// All other engines (vLLM, TabbyAPI, unknown) — plain passthrough, no
/// structured thinking support.
struct PassthroughAdapter;
impl ThinkingAdapter for PassthroughAdapter {
    fn supports_orchestrator(&self) -> bool { false }
    fn inline_think(&self) -> bool { false }
}

/// Return the engine-specific thinking adapter for a given `engine_id`.
///
/// Returns a `'static` reference so callers get zero-cost dispatch via dynamic
/// trait objects without any heap allocation.
pub fn adapter_for_engine(engine_id: &str) -> &'static dyn ThinkingAdapter {
    match engine_id {
        "omlx" => &OmlxAdapter,
        "llamacpp" | "sglang" => &TemplateAdapter,
        _ => &PassthroughAdapter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omlx_supports_orchestrator_not_inline() {
        let a = adapter_for_engine("omlx");
        assert!(a.supports_orchestrator());
        assert!(!a.inline_think());
    }

    #[test]
    fn llamacpp_supports_orchestrator_and_inline() {
        let a = adapter_for_engine("llamacpp");
        assert!(a.supports_orchestrator());
        assert!(a.inline_think());
    }

    #[test]
    fn sglang_supports_orchestrator_and_inline() {
        let a = adapter_for_engine("sglang");
        assert!(a.supports_orchestrator());
        assert!(a.inline_think());
    }

    #[test]
    fn unknown_engine_passthrough() {
        let a = adapter_for_engine("vllm");
        assert!(!a.supports_orchestrator());
        assert!(!a.inline_think());

        let a = adapter_for_engine("tabbyapi");
        assert!(!a.supports_orchestrator());
        assert!(!a.inline_think());

        let a = adapter_for_engine("");
        assert!(!a.supports_orchestrator());
    }
}
