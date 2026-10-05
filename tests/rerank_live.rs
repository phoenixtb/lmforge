//! `/v1/rerank` against a live daemon — the fast engine-backed check that a
//! reranker actually discriminates (the mocked contract lives in
//! `tests/rerank_api.rs`; TC-E11 in `tests/multi_model_e2e.*` runs the same
//! assertions across every catalog reranker).
//!
//! ```sh
//! LMFORGE_HOST=http://127.0.0.1:11430 \
//! LMFORGE_RERANK_MODEL=qwen3-reranker:0.6b:8bit \
//! cargo test --test rerank_live -- --ignored --nocapture
//! ```
//!
//! A headless reranker GGUF (every score ≈ equal) fails
//! `relevant_document_outscores_irrelevant_one` — or is refused with 422.

use std::time::Duration;

use serde_json::{Value, json};

const QUERY: &str = "How do I reset a forgotten password?";
const RELEVANT: &str = "To reset a forgotten password, click Forgot password on the sign-in page and follow the link sent to your email.";
const IRRELEVANT: &str = "The museum is closed on public holidays.";

/// Minimum relevant − irrelevant probability gap. Measured 2026-10-05 on the
/// pair above: Qwen3-Reranker 0.9998 vs 0.0000 (llama.cpp) / 1.0 vs 0.0
/// (oMLX), bge-reranker-v2-m3 0.967 vs 0.000, jina-reranker-v2 0.779 vs 0.031.
const MIN_MARGIN: f64 = 0.3;

fn host() -> String {
    std::env::var("LMFORGE_HOST").unwrap_or_else(|_| "http://127.0.0.1:11430".into())
}

fn model() -> String {
    std::env::var("LMFORGE_RERANK_MODEL").unwrap_or_else(|_| "qwen3-reranker:0.6b:8bit".into())
}

fn rerank(documents: &[String]) -> (u16, Value) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap();
    let resp = client
        .post(format!("{}/v1/rerank", host()))
        .json(&json!({"model": model(), "query": QUERY, "documents": documents}))
        .send()
        .expect("daemon reachable");
    let status = resp.status().as_u16();
    (status, resp.json().unwrap_or(Value::Null))
}

fn score_of(body: &Value, index: u64) -> f64 {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["index"] == index)
        .and_then(|r| r["relevance_score"].as_f64())
        .unwrap_or_else(|| panic!("no score for index {index}: {body}"))
}

/// ~`words` English words of filler that is still on-topic for QUERY.
fn long_document(words: usize) -> String {
    let sentence = "Password recovery requires access to the registered email account and a working sign-in page link. ";
    let per = sentence.split_whitespace().count();
    sentence.repeat(words.div_ceil(per))
}

#[test]
#[ignore = "requires a live lmforge daemon with a reranker pulled — run with --ignored"]
fn relevant_document_outscores_irrelevant_one() {
    let (status, body) = rerank(&[IRRELEVANT.to_string(), RELEVANT.to_string()]);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["score_type"], "probability", "{body}");
    let relevant = score_of(&body, 1);
    let irrelevant = score_of(&body, 0);
    println!(
        "{}: relevant={relevant:.6} irrelevant={irrelevant:.6}",
        model()
    );
    for s in [relevant, irrelevant] {
        assert!((0.0..=1.0).contains(&s), "score {s} outside [0, 1]");
    }
    assert!(relevant > 0.5, "relevant document scored {relevant} ≤ 0.5");
    assert!(
        relevant - irrelevant >= MIN_MARGIN,
        "not discriminating: relevant={relevant} irrelevant={irrelevant}"
    );
}

#[test]
#[ignore = "requires a live lmforge daemon with a reranker pulled — run with --ignored"]
fn long_documents_score_instead_of_failing_the_request() {
    // ~600 tokens: above llama-server's old 512-token micro-batch.
    let (status, body) = rerank(&[long_document(450)]);
    assert_eq!(status, 200, "600-token document: {body}");
    assert!((0.0..=1.0).contains(&score_of(&body, 0)));

    // One ~5,000-token document among normal ones.
    let docs = vec![
        RELEVANT.to_string(),
        long_document(4000),
        IRRELEVANT.to_string(),
    ];
    let (status, body) = rerank(&docs);
    assert_eq!(status, 200, "5,000-token document among short ones: {body}");
    assert_eq!(body["results"].as_array().unwrap().len(), 3, "{body}");
    assert!(score_of(&body, 0) > score_of(&body, 2), "{body}");
}
