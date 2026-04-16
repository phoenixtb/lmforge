use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::json;
use std::io::Write;

use crate::config::LmForgeConfig;
use crate::engine::daemon;

/// Start an interactive chat session with the specified model
pub async fn run(config: &LmForgeConfig, model_input: &str) -> Result<()> {
    // 1. Resolve model to get its exact ID
    let engine_format = detect_engine_format(&config.data_dir());
    let catalogs_dir = config.catalogs_dir();
    let resolved = crate::model::resolver::resolve(model_input, &engine_format, &catalogs_dir).await?;
    let model_id = resolved.id;

    let mut idx = crate::model::index::ModelIndex::load(&config.data_dir())?;
    if idx.get(&model_id).is_none() {
        println!("\nModel '{}' is not installed locally.", model_id);
        print!("Would you like to pull it now? [y/N]: ");
        std::io::stdout().flush().unwrap();
        
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("y") {
            println!();
            crate::cli::pull::run(config, model_input).await?;
        } else {
            anyhow::bail!("Model required to start interactive session. Exiting.");
        }
    }

    println!("⚙ Entering interactive mode with '{}'", model_id);

    // 2. Ensure daemon is running (auto-start).
    //    Use 120s — large models (27B+) can take 60–90s to load into unified memory.
    match daemon::ensure_daemon_running(&config.data_dir(), config.port, Some(&model_id), 120).await {
        Ok(true) => println!("  ✓ Engine ready."),
        Ok(false) => println!("  ✓ Daemon already running."),
        Err(e) => {
            eprintln!("\n  ✗ {}\n", e);
            anyhow::bail!("Engine failed to start.");
        }
    }

    // 3. Start REPL
    println!("  Type '/bye' to exit, '/clear' to clear history.");
    println!("  Type '/think on' or '/think off' to toggle reasoning mode (if supported).");
    println!("{}\n", "─".repeat(60));

    let mut rl = DefaultEditor::new()?;
    let mut messages = vec![];
    let mut think_mode = true; // Enabled by default
    let api_url = format!("http://127.0.0.1:{}/v1/chat/completions", config.port);
    let client = reqwest::Client::new();

    loop {
        let readline = rl.readline(">>> ");
        match readline {
            Ok(line) => {
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                
                rl.add_history_entry(text)?;

                // Slash commands
                match text {
                    "/bye" | "/exit" | "/quit" => break,
                    "/clear" => {
                        messages.clear();
                        println!("History cleared.\n");
                        continue;
                    }
                    "/think on" => {
                        think_mode = true;
                        println!("Thinking mode ENABLED.\n");
                        continue;
                    }
                    "/think off" => {
                        think_mode = false;
                        println!("Thinking mode DISABLED.\n");
                        continue;
                    }
                    _ => {} // Fall through to standard chat
                }

                messages.push(json!({ "role": "user", "content": text }));

                let req_body = json!({
                    "model": model_id,
                    "messages": messages,
                    "stream": true,
                    "think": think_mode,
                });

                // Send request
                let mut resp = match client.post(&api_url).json(&req_body).send().await {
                    Ok(r) => r,
                    Err(e) => {
                        println!("\n  [Connection error: {}]", e);
                        continue;
                    }
                };

                if !resp.status().is_success() {
                    let status = resp.status();
                    let err_text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                    println!("\n  [API Error: {} - {}]\n", status, err_text);
                    continue;
                }

                let mut assistant_content = String::new();
                let mut reasoning_content = String::new();
                let mut in_thinking = false;

                // Process stream manually
                use bytes::BytesMut;
                let mut buffer = BytesMut::new();

                while let Some(chunk_result) = resp.chunk().await.ok().flatten() {
                    buffer.extend_from_slice(&chunk_result);
                    
                    while let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
                        let line_bytes = buffer.split_to(pos + 2);
                        let msg = String::from_utf8_lossy(&line_bytes).to_string();
                        let msg = msg.trim();
                        
                        if msg.starts_with("data: ") {
                            let data = &msg[6..]; // skip "data: "
                            if data == "[DONE]" {
                                break;
                            }

                            if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) {
                                    if let Some(delta) = choices.get(0).and_then(|c| c.get("delta")).and_then(|d| d.as_object()) {
                                        // Handle reasoning content (thinking)
                                        if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                                            if !reasoning.is_empty() {
                                                if !in_thinking {
                                                    print!("\x1b[2;3m<think>\n"); // Dim & Italic
                                                    in_thinking = true;
                                                }
                                                print!("{}", reasoning);
                                                std::io::stdout().flush().unwrap();
                                                reasoning_content.push_str(reasoning);
                                            }
                                        }

                                        // Handle normal content
                                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                            if !content.is_empty() {
                                                if in_thinking {
                                                    print!("\n</think>\x1b[0m\n\n"); // End gray coloring
                                                    in_thinking = false;
                                                }
                                                print!("{}", content);
                                                std::io::stdout().flush().unwrap();
                                                assistant_content.push_str(content);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if in_thinking {
                    print!("\n</think>\x1b[0m\n\n");
                } else {
                    println!("\n"); // Double newline after generation
                }

                // Add to history
                let mut final_msg = json!({
                    "role": "assistant",
                    "content": assistant_content,
                });
                
                if !reasoning_content.is_empty() {
                    final_msg["reasoning_content"] = json!(reasoning_content);
                }

                messages.push(final_msg);
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}

fn detect_engine_format(data_dir: &std::path::Path) -> String {
    let hw_path = data_dir.join("hardware.json");
    if let Ok(content) = std::fs::read_to_string(&hw_path) {
        if let Ok(profile) = serde_json::from_str::<serde_json::Value>(&content) {
            if profile["gpu_vendor"].as_str() == Some("apple") {
                return "mlx".to_string();
            }
        }
    }
    "gguf".to_string()
}
