//! Integration test for QUALITY-PLAN-2026-09 §1.1 — Call-2 transport-error
//! retry + guaranteed terminal `finish_reason`.
//!
//! Exercises `proxy_stream_with_thinking_budget` directly (no live engine
//! required) against a hand-rolled raw-TCP harness that:
//!   - answers the first connection (Call-1) with a valid SSE stream that
//!     exhausts the thinking budget (`finish_reason: "length"`), forcing the
//!     orchestrator into Call-2;
//!   - drops every subsequent connection (Call-2's initial attempt AND its
//!     retry) without writing a response, reproducing the transport-level
//!     failure observed live: think_bench 2026-09-07,
//!     `qwen3.5:4b:6bit`/`seq_next`/think=on r2 — Call-2's `reqwest` send
//!     failed once on a transient same-port hop.
//!
//! Asserts the client-facing invariant: even after both Call-2 attempts fail,
//! the stream still ends with a terminal chunk carrying a non-null
//! `finish_reason` before `[DONE]` — never a silent blank.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Spawns a raw TCP server on 127.0.0.1 (OS-assigned port) that:
///   - connection #0 (Call-1): responds with a valid SSE stream ending
///     `finish_reason: "length"` (budget exhausted → triggers Call-2).
///   - every later connection (Call-2's attempt + its retry): accepted, then
///     dropped without any response bytes — reqwest surfaces this as a
///     transport error on `.send()`.
async fn spawn_call2_failure_harness() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let conn_count = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let idx = conn_count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                // Best-effort drain of whatever the client already wrote —
                // irrelevant to the test, just avoids RST noise in logs.
                let mut buf = vec![0u8; 65536];
                let _ =
                    tokio::time::timeout(Duration::from_millis(200), socket.read(&mut buf)).await;

                if idx == 0 {
                    // Call-1: valid SSE stream, budget exhausted.
                    let sse_body = concat!(
                        "data: {\"id\":\"cmpl-test-1\",\"model\":\"test-model\",",
                        "\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking...\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"id\":\"cmpl-test-1\",\"model\":\"test-model\",",
                        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
                        "data: [DONE]\n\n"
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                        sse_body.len(),
                        sse_body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                } else {
                    // Call-2 attempt (idx==1) and its retry (idx==2): drop the
                    // connection with no response — transport failure.
                    drop(socket);
                }
            });
        }
    });

    port
}

/// Parse an SSE byte stream into the list of parsed JSON `data:` payloads
/// (skipping `[DONE]` and raw error lines that aren't JSON).
fn parse_sse_json_events(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .collect()
}

#[tokio::test]
async fn call2_transport_error_after_retry_still_emits_terminal_finish_reason() {
    let port = spawn_call2_failure_harness().await;
    let client = lmforge::server::proxy::build_proxy_client();

    let original_body = serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 100,
        "thinking_budget": 16,
        "stream": true
    });

    let started = std::time::Instant::now();
    let result = lmforge::server::proxy::proxy_stream_with_thinking_budget(
        &client,
        port,
        "/v1/chat/completions",
        original_body,
        /* original_max_tokens */ 100,
        /* thinking_budget */ 16,
        /* stream_reasoning_deltas */ false,
        /* inline_think */ true,
    )
    .await;

    // Call-1 succeeded, so the orchestrator must still return an Ok stream —
    // the Call-2 failure surfaces *inside* the SSE stream, not as an HTTP
    // error to the caller.
    let body =
        result.expect("Call-1 succeeded; streaming body must be Ok even though Call-2 fails");
    let bytes = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).expect("stream must be valid UTF-8");

    // Sanity: the retry actually happened (200ms backoff), not an instant fail.
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "expected the single 200ms retry backoff to have elapsed, took {:?}",
        started.elapsed()
    );

    assert!(
        text.contains("[DONE]"),
        "stream must end with [DONE]: {text}"
    );
    assert!(
        text.contains("Call-2 failed"),
        "expected the Call-2 error object to be surfaced: {text}"
    );

    let events = parse_sse_json_events(&text);
    let terminal = events.iter().find(|v| {
        v["choices"][0]["finish_reason"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    });
    assert!(
        terminal.is_some(),
        "expected a terminal chunk with non-null finish_reason before [DONE]; got events: {events:#?}\nfull stream: {text}"
    );
    assert_eq!(
        terminal.unwrap()["choices"][0]["finish_reason"],
        "length",
        "reasoning was produced and the budget was consumed — finish_reason must be \"length\", not a silent blank"
    );

    // [DONE] must be the very last frame, after the terminal chunk.
    let done_pos = text.rfind("data: [DONE]").unwrap();
    let terminal_pos = text.rfind("\"finish_reason\":\"length\"").unwrap();
    assert!(
        terminal_pos < done_pos,
        "terminal finish_reason chunk must appear before [DONE]"
    );
}
