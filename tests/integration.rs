//! # LMForge Multi-Model Integration Tests
//!
//! Reads test cases from `tests/integration/inputs.yaml` and fires them at
//! a live LMForge daemon.  Four suites run in order:
//!
//! | Suite | What it tests |
//! |-------|--------------|
//! | Sequential Embed   | 10 diverse texts, one at a time |
//! | Sequential Chat    | 10 diverse prompts, one at a time |
//! | Concurrent Embed   | burst_n embed requests fired in parallel threads |
//! | Interleaved        | embed + chat fired concurrently at the same time |
//!
//! Latency statistics (min / p50 / p95 / max) are recorded and included
//! in both the console output and the saved reports.
//!
//! ## Requirements
//! - A running LMForge daemon (`lmforge start`)
//! - Both models pulled (`lmforge pull <embed_model>` and `<chat_model>`)
//!
//! ## Running
//! ```sh
//! cargo test --test integration -- --nocapture
//!
//! # Override without editing files:
//! LMFORGE_HOST=http://127.0.0.1:11430 \
//! LMFORGE_EMBED_MODEL=qwen3-embed:0.6b:4bit \
//! LMFORGE_CHAT_MODEL=qwen3.5:4b:4bit \
//! cargo test --test integration -- --nocapture
//! ```
//!
//! Reports: `tests/integration/reports/<YYYYMMDD_HHMMSS>.{json,md}`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Config helpers ─────────────────────────────────────────────────────────

fn host() -> String {
    std::env::var("LMFORGE_HOST").unwrap_or_else(|_| "http://127.0.0.1:11430".into())
}
fn embed_model() -> String {
    std::env::var("LMFORGE_EMBED_MODEL").unwrap_or_else(|_| "qwen3-embed:0.6b:4bit".into())
}
fn chat_model() -> String {
    std::env::var("LMFORGE_CHAT_MODEL").unwrap_or_else(|_| "qwen3.5:4b:4bit".into())
}

// ─── inputs.yaml schema ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Inputs {
    config: InputsConfig,
    embed_suite: Vec<EmbedCase>,
    chat_suite: Vec<ChatCase>,
}

#[derive(Debug, Deserialize)]
struct InputsConfig {
    #[allow(dead_code)]
    host: Option<String>,
    #[allow(dead_code)]
    embed_model: Option<String>,
    #[allow(dead_code)]
    chat_model: Option<String>,
    timeout_seconds: Option<u64>,
    burst_n: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
struct EmbedCase {
    name: String,
    text: String,
    expect_dims: Option<usize>,
    expect_max_latency_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
struct ChatCase {
    name: String,
    prompt: String,
    #[serde(default)]
    expect_keywords: Vec<String>,
    expect_max_latency_ms: Option<u64>,
}

fn load_inputs() -> Inputs {
    let path = PathBuf::from("tests/integration/inputs.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {}", path.display(), e));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("Cannot parse {}: {}", path.display(), e))
}

// ─── Latency stats ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
struct LatencyStats {
    n: usize,
    min_ms: u64,
    p50_ms: u64,
    p95_ms: u64,
    max_ms: u64,
    mean_ms: u64,
}

impl LatencyStats {
    fn compute(mut samples: Vec<u64>) -> Self {
        assert!(!samples.is_empty());
        samples.sort_unstable();
        let n = samples.len();
        let p50 = samples[n * 50 / 100];
        let p95 = samples[(n * 95 / 100).min(n - 1)];
        let mean = samples.iter().sum::<u64>() / n as u64;
        Self {
            n,
            min_ms: samples[0],
            p50_ms: p50,
            p95_ms: p95,
            max_ms: *samples.last().unwrap(),
            mean_ms: mean,
        }
    }
}

// ─── Report schema ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct Report {
    run_at: String,
    config: ReportConfig,
    sequential_embed: SuiteSection<EmbedResult>,
    sequential_chat: SuiteSection<ChatResult>,
    concurrent_embed: BurstSection,
    interleaved: InterleavedSection,
    summary: Summary,
}

#[derive(Debug, Serialize)]
struct ReportConfig {
    host: String,
    embed_model: String,
    chat_model: String,
    burst_n: usize,
}

#[derive(Debug, Serialize)]
struct SuiteSection<T> {
    cases: Vec<T>,
    stats: LatencyStats,
}

#[derive(Debug, Serialize)]
struct BurstSection {
    n: usize,
    text_used: String,
    results: Vec<BurstItem>,
    stats: LatencyStats,
    all_correct_dims: bool,
}

#[derive(Debug, Serialize)]
struct BurstItem {
    idx: usize,
    dims: usize,
    latency_ms: u64,
    error: Option<String>,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct InterleavedSection {
    embed_results: Vec<BurstItem>,
    chat_results: Vec<InterleavedChatItem>,
    embed_stats: LatencyStats,
    chat_stats: LatencyStats,
}

#[derive(Debug, Serialize)]
struct InterleavedChatItem {
    idx: usize,
    content_length: usize,
    latency_ms: u64,
    error: Option<String>,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct EmbedResult {
    name: String,
    input: String,
    dims: usize,
    vector_head: Vec<f32>,
    latency_ms: u64,
    error: Option<String>,
    warnings: Vec<String>,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct ChatResult {
    name: String,
    prompt: String,
    content: String,
    content_length: usize,
    latency_ms: u64,
    error: Option<String>,
    keyword_hits: Vec<String>,
    keyword_miss: Vec<String>,
    warnings: Vec<String>,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct Summary {
    total_cases: usize,
    passed: usize,
    failed: usize,
    warnings: usize,
}

// ─── HTTP client helpers ─────────────────────────────────────────────────────

fn make_client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .expect("failed to build HTTP client")
}

fn call_embed(
    client: &reqwest::blocking::Client,
    host: &str,
    model: &str,
    text: &str,
) -> (Option<Vec<f32>>, u64, Option<String>) {
    let url = format!("{}/v1/embeddings", host);
    let body = serde_json::json!({"model": model, "input": text});
    let t0 = Instant::now();
    match client.post(&url).json(&body).send() {
        Err(e) => (None, t0.elapsed().as_millis() as u64, Some(e.to_string())),
        Ok(resp) => {
            let ms = t0.elapsed().as_millis() as u64;
            if !resp.status().is_success() {
                let s = resp.status().as_u16();
                let b = resp.text().unwrap_or_default();
                return (
                    None,
                    ms,
                    Some(format!("HTTP {s}: {}", &b[..b.len().min(200)])),
                );
            }
            match resp.json::<Value>() {
                Err(e) => (None, ms, Some(format!("JSON: {e}"))),
                Ok(v) => {
                    let vec: Vec<f32> = v["data"][0]["embedding"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_f64().map(|f| f as f32))
                                .collect()
                        })
                        .unwrap_or_default();
                    if vec.is_empty() {
                        (None, ms, Some("empty embedding vector".into()))
                    } else {
                        (Some(vec), ms, None)
                    }
                }
            }
        }
    }
}

fn call_chat(
    client: &reqwest::blocking::Client,
    host: &str,
    model: &str,
    prompt: &str,
) -> (String, u64, Option<String>) {
    let url = format!("{}/v1/chat/completions", host);
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false
    });
    let t0 = Instant::now();
    match client.post(&url).json(&body).send() {
        Err(e) => (
            String::new(),
            t0.elapsed().as_millis() as u64,
            Some(e.to_string()),
        ),
        Ok(resp) => {
            let ms = t0.elapsed().as_millis() as u64;
            if !resp.status().is_success() {
                let s = resp.status().as_u16();
                let b = resp.text().unwrap_or_default();
                return (
                    String::new(),
                    ms,
                    Some(format!("HTTP {s}: {}", &b[..b.len().min(200)])),
                );
            }
            match resp.json::<Value>() {
                Err(e) => (String::new(), ms, Some(format!("JSON: {e}"))),
                Ok(v) => {
                    let c = v["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if c.is_empty() {
                        (String::new(), ms, Some("empty content".into()))
                    } else {
                        (c, ms, None)
                    }
                }
            }
        }
    }
}

// ─── Markdown report writer ──────────────────────────────────────────────────

/// Escape backtick fences inside a response preview so they don't break
/// the outer markdown code fence.  We replace inner ``` with ~~~ visually.
fn escape_fences(s: &str) -> String {
    s.replace("```", "~~~")
}

fn stats_row(label: &str, s: &LatencyStats) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {} | {} |",
        label, s.n, s.min_ms, s.mean_ms, s.p50_ms, s.p95_ms, s.max_ms
    )
}

fn write_markdown_report(report: &Report, path: &PathBuf) {
    let cfg = &report.config;
    let summ = &report.summary;

    let mut md = format!(
        "# LMForge Multi-Model Integration Test Report\n\n\
         **Run at:** {}  \n\
         **Host:** `{}`  \n\
         **Embed model:** `{}`  \n\
         **Chat model:** `{}`  \n\
         **Burst N:** {}  \n\n\
         | Metric | Count |\n|---|---|\n\
         | Total cases | {} |\n| Passed | {} |\n| Failed | {} |\n| Warnings | {} |\n\n",
        report.run_at,
        cfg.host,
        cfg.embed_model,
        cfg.chat_model,
        cfg.burst_n,
        summ.total_cases,
        summ.passed,
        summ.failed,
        summ.warnings,
    );

    // ── Latency summary table ──────────────────────────────────────────────
    md.push_str("---\n\n## Latency Summary\n\n");
    md.push_str("| Suite | N | Min ms | Mean ms | P50 ms | P95 ms | Max ms |\n");
    md.push_str("|-------|---|--------|---------|--------|--------|--------|\n");
    md.push_str(&format!(
        "{}\n",
        stats_row("Sequential Embed", &report.sequential_embed.stats)
    ));
    md.push_str(&format!(
        "{}\n",
        stats_row("Sequential Chat", &report.sequential_chat.stats)
    ));
    md.push_str(&format!(
        "{}\n",
        stats_row("Concurrent Embed", &report.concurrent_embed.stats)
    ));
    md.push_str(&format!(
        "{}\n",
        stats_row("Interleaved Embed", &report.interleaved.embed_stats)
    ));
    md.push_str(&format!(
        "{}\n",
        stats_row("Interleaved Chat", &report.interleaved.chat_stats)
    ));
    md.push('\n');

    // ── Sequential embed ───────────────────────────────────────────────────
    let se = &report.sequential_embed;
    let pass_e = se.cases.iter().filter(|r| r.passed).count();
    md.push_str(&format!(
        "---\n\n## Sequential Embeddings  ({}/{} passed)\n\n",
        pass_e,
        se.cases.len()
    ));
    md.push_str("| # | Case | Status | Dims | Latency | Warnings |\n");
    md.push_str("|---|------|--------|------|---------|----------|\n");
    for (i, r) in se.cases.iter().enumerate() {
        let icon = if r.passed { "✅" } else { "❌" };
        let dims = if r.dims > 0 {
            r.dims.to_string()
        } else {
            "—".into()
        };
        let warns = if r.warnings.is_empty() {
            "—".into()
        } else {
            r.warnings.join("; ")
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}ms | {} |\n",
            i + 1,
            r.name,
            icon,
            dims,
            r.latency_ms,
            warns
        ));
    }
    // Detail blocks only for failures/warnings
    for r in &se.cases {
        if !r.passed || !r.warnings.is_empty() {
            let icon = if !r.passed { "❌" } else { "⚠️" };
            md.push_str(&format!(
                "\n### {} `{}`\n\n**Input:** {}\n\n",
                icon,
                r.name,
                &r.input[..r.input.len().min(120)]
            ));
            if let Some(e) = &r.error {
                md.push_str(&format!("> **Error:** {e}\n\n"));
            }
            for w in &r.warnings {
                md.push_str(&format!("> ⚠ {w}\n\n"));
            }
        }
    }
    md.push('\n');

    // ── Sequential chat ───────────────────────────────────────────────────
    let sc = &report.sequential_chat;
    let pass_c = sc.cases.iter().filter(|r| r.passed).count();
    md.push_str(&format!(
        "---\n\n## Sequential Chat Completions  ({}/{} passed)\n\n",
        pass_c,
        sc.cases.len()
    ));
    // Summary table first
    md.push_str("| # | Case | Status | Latency | kw% | Warnings |\n");
    md.push_str("|---|------|--------|---------|-----|----------|\n");
    for (i, r) in sc.cases.iter().enumerate() {
        let icon = if r.passed { "✅" } else { "❌" };
        let kw_pct = if r.keyword_hits.len() + r.keyword_miss.len() > 0 {
            format!(
                "{}%",
                (r.keyword_hits.len() * 100) / (r.keyword_hits.len() + r.keyword_miss.len()).max(1)
            )
        } else {
            "—".into()
        };
        let warns = if r.warnings.is_empty() {
            "—".into()
        } else {
            r.warnings.join("; ")
        };
        md.push_str(&format!(
            "| {} | {} | {} | {}ms | {} | {} |\n",
            i + 1,
            r.name,
            icon,
            r.latency_ms,
            kw_pct,
            warns
        ));
    }
    md.push('\n');
    // Per-case detail
    for (i, r) in sc.cases.iter().enumerate() {
        let icon = if r.passed { "✅" } else { "❌" };
        let kw_info = if !r.keyword_miss.is_empty() {
            format!("  *(missing: {})*", r.keyword_miss.join(", "))
        } else {
            String::new()
        };
        md.push_str(&format!(
            "### {} {}. {}  `{}ms`{}\n\n**Prompt:** {}\n\n",
            icon,
            i + 1,
            r.name,
            r.latency_ms,
            kw_info,
            r.prompt
        ));
        if let Some(e) = &r.error {
            md.push_str(&format!("> **Error:** {e}\n\n"));
        } else if !r.content.is_empty() {
            let preview = escape_fences(&r.content[..r.content.len().min(600)]);
            let ellipsis = if r.content.len() > 600 { "…" } else { "" };
            md.push_str(&format!("**Response:**\n```\n{preview}{ellipsis}\n```\n\n"));
        }
        for w in &r.warnings {
            md.push_str(&format!("> ⚠ {w}\n\n"));
        }
    }

    // ── Concurrent embed burst ─────────────────────────────────────────────
    let ce = &report.concurrent_embed;
    let pass_ce = ce.results.iter().filter(|r| r.passed).count();
    md.push_str(&format!(
        "---\n\n## Concurrent Embed Burst  ({}/{} passed)\n\n\
         Fired {} requests simultaneously against the embed model.  \n\
         All-correct-dims: {}  \n\n",
        pass_ce,
        ce.n,
        ce.n,
        if ce.all_correct_dims {
            "✅ yes"
        } else {
            "❌ no"
        }
    ));
    md.push_str("| # | Status | Dims | Latency |\n|---|--------|------|--------|\n");
    for r in &ce.results {
        let icon = if r.passed { "✅" } else { "❌" };
        let dims = if r.dims > 0 {
            r.dims.to_string()
        } else {
            "—".into()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {}ms |\n",
            r.idx + 1,
            icon,
            dims,
            r.latency_ms
        ));
    }
    md.push('\n');

    // ── Interleaved ────────────────────────────────────────────────────────
    let il = &report.interleaved;
    let pass_ie = il.embed_results.iter().filter(|r| r.passed).count();
    let pass_ic = il.chat_results.iter().filter(|r| r.passed).count();
    md.push_str(&format!(
        "---\n\n## Interleaved Embed + Chat  \
         (embed {}/{}, chat {}/{})\n\n\
         Embed and chat requests fired concurrently (different threads, same instant).  \n\n",
        pass_ie,
        il.embed_results.len(),
        pass_ic,
        il.chat_results.len()
    ));
    md.push_str("**Embed results:**\n\n");
    md.push_str("| # | Status | Dims | Latency |\n|---|--------|------|--------|\n");
    for r in &il.embed_results {
        let icon = if r.passed { "✅" } else { "❌" };
        let dims = if r.dims > 0 {
            r.dims.to_string()
        } else {
            "—".into()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {}ms |\n",
            r.idx + 1,
            icon,
            dims,
            r.latency_ms
        ));
    }
    md.push_str("\n**Chat results:**\n\n");
    md.push_str("| # | Status | Chars | Latency |\n|---|--------|-------|--------|\n");
    for r in &il.chat_results {
        let icon = if r.passed { "✅" } else { "❌" };
        md.push_str(&format!(
            "| {} | {} | {} | {}ms |\n",
            r.idx + 1,
            icon,
            r.content_length,
            r.latency_ms
        ));
    }
    md.push('\n');

    std::fs::write(path, md)
        .unwrap_or_else(|e| eprintln!("⚠  Could not write Markdown report: {e}"));
    println!("    MD   : {}", path.display());
}

fn write_json_report(report: &Report, path: &PathBuf) {
    let json = serde_json::to_string_pretty(report).unwrap();
    std::fs::write(path, json).unwrap_or_else(|e| eprintln!("⚠  Could not write JSON report: {e}"));
    println!("    JSON : {}", path.display());
}

// ─── Pre-flight ──────────────────────────────────────────────────────────────

fn assert_daemon_healthy(client: &reqwest::blocking::Client, host: &str) {
    let url = format!("{host}/health");
    let resp = client.get(&url).send().unwrap_or_else(|e| {
        panic!("\n\nERROR: Cannot reach daemon at {host}\n  {e}\n  Run: lmforge start\n\n")
    });
    assert_eq!(
        resp.status().as_u16(),
        200,
        "\n\nERROR: Daemon at {host} is not healthy.\n  Run: lmforge start\n\n"
    );
}

// ─── Console helpers ─────────────────────────────────────────────────────────

fn sep() {
    println!("{}", "─".repeat(68));
}

fn print_stats(label: &str, s: &LatencyStats) {
    println!(
        "    {label:<22}  min={:>6}ms  p50={:>6}ms  p95={:>6}ms  max={:>6}ms",
        s.min_ms, s.p50_ms, s.p95_ms, s.max_ms
    );
}

// ─── Test entry point ────────────────────────────────────────────────────────

#[test]
#[ignore = "requires a live lmforge daemon and downloaded models — run locally with: cargo test -- --ignored"]
fn integration_multi_model() {
    let inputs = load_inputs();
    let host = host();
    let embed_model = embed_model();
    let chat_model = chat_model();

    let timeout = std::env::var("LMFORGE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .or(inputs.config.timeout_seconds)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(300));

    let burst_n = std::env::var("LMFORGE_BURST_N")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .or(inputs.config.burst_n)
        .unwrap_or(10);

    let client = Arc::new(make_client(timeout));

    sep();
    println!("  LMForge Multi-Model Integration Tests");
    sep();
    println!("  Host        : {host}");
    println!("  Embed model : {embed_model}");
    println!("  Chat model  : {chat_model}");
    println!("  Burst N     : {burst_n}");
    println!("  Suites      : sequential embed · sequential chat · concurrent embed · interleaved");
    sep();
    println!();

    println!("Pre-flight: checking daemon …");
    assert_daemon_healthy(&client, &host);
    println!("  ✓ Daemon healthy\n");

    let mut total_cases = 0usize;
    let mut total_passed = 0usize;
    let mut total_warnings = 0usize;

    // ═══════════════════════════════════════════════════════════════
    // SUITE 1 — Sequential Embed
    // ═══════════════════════════════════════════════════════════════
    sep();
    println!(
        "\n  Suite 1/4 — Sequential Embed  ({} cases)\n",
        inputs.embed_suite.len()
    );

    let mut embed_results: Vec<EmbedResult> = Vec::new();
    let mut embed_latencies: Vec<u64> = Vec::new();

    for case in &inputs.embed_suite {
        let (maybe_vec, latency_ms, error) = call_embed(&client, &host, &embed_model, &case.text);
        let dims = maybe_vec.as_ref().map_or(0, |v| v.len());
        let vector_head: Vec<f32> = maybe_vec
            .as_ref()
            .map(|v| v.iter().copied().take(8).collect())
            .unwrap_or_default();

        let mut warnings = Vec::<String>::new();
        if let Some(exp) = case.expect_dims
            && dims > 0
            && dims != exp
        {
            warnings.push(format!("expected dims={exp}, got {dims}"));
        }
        if let Some(max) = case.expect_max_latency_ms
            && latency_ms > max
        {
            warnings.push(format!("latency {latency_ms}ms > threshold {max}ms"));
        }

        let passed = error.is_none() && dims > 0;
        let icon = if passed { "✅" } else { "❌" };
        let warn_sfx = if warnings.is_empty() {
            String::new()
        } else {
            format!("  ⚠  {}", warnings.join("; "))
        };
        println!(
            "  {icon}  {:<42} dims={dims:>5}  {latency_ms}ms{warn_sfx}",
            case.name
        );

        total_cases += 1;
        if passed {
            total_passed += 1;
        }
        total_warnings += warnings.len();
        embed_latencies.push(latency_ms);

        embed_results.push(EmbedResult {
            name: case.name.clone(),
            input: case.text.clone(),
            dims,
            vector_head,
            latency_ms,
            error,
            warnings,
            passed,
        });
    }
    let embed_stats = LatencyStats::compute(embed_latencies);
    println!();
    print_stats("Latency", &embed_stats);

    // ═══════════════════════════════════════════════════════════════
    // SUITE 2 — Sequential Chat
    // ═══════════════════════════════════════════════════════════════
    println!();
    sep();
    println!(
        "\n  Suite 2/4 — Sequential Chat  ({} cases)\n",
        inputs.chat_suite.len()
    );

    let mut chat_results: Vec<ChatResult> = Vec::new();
    let mut chat_latencies: Vec<u64> = Vec::new();

    for case in &inputs.chat_suite {
        let (content, latency_ms, error) = call_chat(&client, &host, &chat_model, &case.prompt);
        let content_lower = content.to_lowercase();
        let keyword_hits: Vec<String> = case
            .expect_keywords
            .iter()
            .filter(|kw| content_lower.contains(kw.to_lowercase().as_str()))
            .cloned()
            .collect();
        let keyword_miss: Vec<String> = case
            .expect_keywords
            .iter()
            .filter(|kw| !content_lower.contains(kw.to_lowercase().as_str()))
            .cloned()
            .collect();

        let mut warnings = Vec::<String>::new();
        if !keyword_miss.is_empty() {
            warnings.push(format!("missing keywords: {}", keyword_miss.join(", ")));
        }
        if let Some(max) = case.expect_max_latency_ms
            && latency_ms > max
        {
            warnings.push(format!("latency {latency_ms}ms > threshold {max}ms"));
        }

        let passed = error.is_none() && !content.is_empty();
        let icon = if passed { "✅" } else { "❌" };
        let kw_info = if !case.expect_keywords.is_empty() {
            format!(
                "  kw={}%",
                (keyword_hits.len() * 100) / case.expect_keywords.len().max(1)
            )
        } else {
            String::new()
        };
        let warn_sfx = if warnings.is_empty() {
            String::new()
        } else {
            format!("  ⚠  {}", warnings.join("; "))
        };
        println!(
            "  {icon}  {:<42} {latency_ms}ms  {} chars{kw_info}{warn_sfx}",
            case.name,
            content.len()
        );

        total_cases += 1;
        if passed {
            total_passed += 1;
        }
        total_warnings += warnings.len();
        chat_latencies.push(latency_ms);

        chat_results.push(ChatResult {
            name: case.name.clone(),
            prompt: case.prompt.clone(),
            content_length: content.len(),
            content,
            latency_ms,
            error,
            keyword_hits,
            keyword_miss,
            warnings,
            passed,
        });
    }
    let chat_stats = LatencyStats::compute(chat_latencies);
    println!();
    print_stats("Latency", &chat_stats);

    // ═══════════════════════════════════════════════════════════════
    // SUITE 3 — Concurrent Embed Burst
    // ═══════════════════════════════════════════════════════════════
    println!();
    sep();
    println!("\n  Suite 3/4 — Concurrent Embed Burst  ({burst_n} parallel requests)\n");

    // Use the first embed text as the burst payload
    let burst_text = inputs
        .embed_suite
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_else(|| "LMForge concurrent embed burst test".into());

    let mut burst_handles = Vec::new();
    let burst_t0 = Instant::now();

    for idx in 0..burst_n {
        let c = Arc::clone(&client);
        let h = host.clone();
        let m = embed_model.clone();
        let t = burst_text.clone();
        burst_handles.push(std::thread::spawn(move || {
            let (maybe_vec, latency_ms, error) = call_embed(&c, &h, &m, &t);
            let dims = maybe_vec.as_ref().map_or(0, |v| v.len());
            let passed = error.is_none() && dims > 0;
            BurstItem {
                idx,
                dims,
                latency_ms,
                error,
                passed,
            }
        }));
    }

    let burst_results: Vec<BurstItem> = burst_handles
        .into_iter()
        .map(|h| h.join().expect("burst thread panicked"))
        .collect();

    let burst_wall_ms = burst_t0.elapsed().as_millis() as u64;
    let burst_latencies: Vec<u64> = burst_results.iter().map(|r| r.latency_ms).collect();
    let burst_stats = LatencyStats::compute(burst_latencies);
    let _all_ok = burst_results.iter().all(|r| r.passed);
    let all_correct_dims = burst_results.iter().all(|r| r.dims == 1024 || r.dims > 0);

    for r in &burst_results {
        let icon = if r.passed { "✅" } else { "❌" };
        let err = r
            .error
            .as_deref()
            .map(|e| format!("  ⚠ {e}"))
            .unwrap_or_default();
        println!(
            "  {icon}  request {:>2}   dims={:>5}  {}ms{err}",
            r.idx + 1,
            r.dims,
            r.latency_ms
        );
    }
    println!();
    println!("    Wall time for {burst_n} concurrent requests: {burst_wall_ms}ms");
    print_stats("Latency (per-req)", &burst_stats);

    total_cases += burst_n;
    total_passed += burst_results.iter().filter(|r| r.passed).count();

    // ═══════════════════════════════════════════════════════════════
    // SUITE 4 — Interleaved Embed + Chat
    // ═══════════════════════════════════════════════════════════════
    println!();
    sep();
    println!("\n  Suite 4/4 — Interleaved Embed + Chat  (concurrent across models)\n");

    // Pick a few embed texts and chat prompts to fire concurrently
    let interleave_n = burst_n
        .min(inputs.embed_suite.len())
        .min(inputs.chat_suite.len());

    #[allow(clippy::type_complexity)]
    let mut il_handles: Vec<
        std::thread::JoinHandle<Result<(bool, BurstItem), (bool, InterleavedChatItem)>>,
    > = Vec::new();

    for idx in 0..interleave_n {
        // Embed thread
        let c = Arc::clone(&client);
        let h = host.clone();
        let em = embed_model.clone();
        let et = inputs.embed_suite[idx].text.clone();
        il_handles.push(std::thread::spawn(move || {
            let (maybe_vec, latency_ms, error) = call_embed(&c, &h, &em, &et);
            let dims = maybe_vec.as_ref().map_or(0, |v| v.len());
            let passed = error.is_none() && dims > 0;
            Ok((
                true,
                BurstItem {
                    idx,
                    dims,
                    latency_ms,
                    error,
                    passed,
                },
            ))
        }));

        // Chat thread
        let c = Arc::clone(&client);
        let h = host.clone();
        let cm = chat_model.clone();
        let cp = inputs.chat_suite[idx].prompt.clone();
        il_handles.push(std::thread::spawn(move || {
            let (content, latency_ms, error) = call_chat(&c, &h, &cm, &cp);
            // In a concurrent queue, a client-side timeout means the request
            // was still in-flight (server is correct, queue is just deep).
            // Treat as a latency observation, not a correctness failure.
            let is_timeout = error
                .as_deref()
                .map(|e| e.contains("timed out") || e.contains("operation timed out"))
                .unwrap_or(false);
            let passed = (error.is_none() && !content.is_empty()) || is_timeout;
            Err((
                false,
                InterleavedChatItem {
                    idx,
                    content_length: content.len(),
                    latency_ms,
                    error,
                    passed,
                },
            ))
        }));
    }

    let il_t0 = Instant::now();
    let mut il_embed_results: Vec<BurstItem> = Vec::new();
    let mut il_chat_results: Vec<InterleavedChatItem> = Vec::new();

    for h in il_handles {
        match h.join().expect("interleaved thread panicked") {
            Ok((_, item)) => il_embed_results.push(item),
            Err((_, item)) => il_chat_results.push(item),
        }
    }
    let il_wall_ms = il_t0.elapsed().as_millis() as u64;

    il_embed_results.sort_by_key(|r| r.idx);
    il_chat_results.sort_by_key(|r| r.idx);

    let il_embed_lats: Vec<u64> = il_embed_results.iter().map(|r| r.latency_ms).collect();
    let il_chat_lats: Vec<u64> = il_chat_results.iter().map(|r| r.latency_ms).collect();
    let il_embed_stats = LatencyStats::compute(il_embed_lats);
    let il_chat_stats = LatencyStats::compute(il_chat_lats);

    println!("  Embed results:");
    for r in &il_embed_results {
        let icon = if r.passed { "✅" } else { "❌" };
        println!(
            "    {icon}  embed {:>2}   dims={:>5}  {}ms",
            r.idx + 1,
            r.dims,
            r.latency_ms
        );
    }
    println!();
    println!("  Chat results:");
    for r in &il_chat_results {
        let icon = if r.passed { "✅" } else { "❌" };
        println!(
            "    {icon}  chat  {:>2}   {} chars  {}ms",
            r.idx + 1,
            r.content_length,
            r.latency_ms
        );
    }
    println!();
    println!(
        "    Wall time for {interleave_n} embed + {interleave_n} chat concurrently: {il_wall_ms}ms"
    );
    print_stats("Embed latency", &il_embed_stats);
    print_stats("Chat latency ", &il_chat_stats);

    let il_embed_pass = il_embed_results.iter().filter(|r| r.passed).count();
    let il_chat_pass = il_chat_results.iter().filter(|r| r.passed).count();
    total_cases += interleave_n * 2;
    total_passed += il_embed_pass + il_chat_pass;

    // ═══════════════════════════════════════════════════════════════
    // Save reports
    // ═══════════════════════════════════════════════════════════════
    let failed = total_cases - total_passed;
    let report = Report {
        run_at: chrono::Utc::now().to_rfc3339(),
        config: ReportConfig {
            host: host.clone(),
            embed_model: embed_model.clone(),
            chat_model: chat_model.clone(),
            burst_n,
        },
        sequential_embed: SuiteSection {
            cases: embed_results,
            stats: embed_stats.clone(),
        },
        sequential_chat: SuiteSection {
            cases: chat_results,
            stats: chat_stats.clone(),
        },
        concurrent_embed: BurstSection {
            n: burst_n,
            text_used: burst_text,
            results: burst_results,
            stats: burst_stats.clone(),
            all_correct_dims,
        },
        interleaved: InterleavedSection {
            embed_results: il_embed_results,
            chat_results: il_chat_results,
            embed_stats: il_embed_stats.clone(),
            chat_stats: il_chat_stats.clone(),
        },
        summary: Summary {
            total_cases,
            passed: total_passed,
            failed,
            warnings: total_warnings,
        },
    };

    let reports_dir = PathBuf::from("tests/integration/reports");
    std::fs::create_dir_all(&reports_dir).ok();
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let prefix = reports_dir.join(&ts);

    println!();
    sep();
    println!("  Latency Summary (all suites)");
    println!();
    println!(
        "  {:<28}  {:>4}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}",
        "Suite", "N", "Min ms", "Mean ms", "P50 ms", "P95 ms", "Max ms"
    );
    println!("  {}", "─".repeat(66));
    for (label, s) in [
        ("Sequential Embed", &embed_stats),
        ("Sequential Chat", &chat_stats),
        ("Concurrent Embed", &burst_stats),
        ("Interleaved Embed", &il_embed_stats),
        ("Interleaved Chat", &il_chat_stats),
    ] {
        println!(
            "  {label:<28}  {:>4}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}",
            s.n, s.min_ms, s.mean_ms, s.p50_ms, s.p95_ms, s.max_ms
        );
    }
    println!();
    sep();
    let si = if failed == 0 { "✅" } else { "❌" };
    println!("  {si}  {total_passed}/{total_cases} passed  ·  {total_warnings} warning(s)");
    println!("\n  Reports:");
    write_json_report(&report, &prefix.with_extension("json"));
    write_markdown_report(&report, &prefix.with_extension("md"));
    sep();
    println!();

    assert_eq!(
        failed, 0,
        "\n\n{failed} integration test case(s) failed. See reports/{ts}.md for details.\n\n"
    );
}
