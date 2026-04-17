use anyhow::Result;
use std::collections::HashMap;

use crate::config::LmForgeConfig;

/// `lmforge catalog list` — List available model shortcuts from the bundled catalog
pub async fn run(
    config: &LmForgeConfig,
    format: Option<String>,
    search: Option<String>,
) -> Result<()> {
    // Determine engine format for this platform if not specified
    let format_str = match format {
        Some(f) => f.to_lowercase(),
        None => detect_platform_format(),
    };

    // Try runtime file first (user may have customized it), fall back to bundled
    let catalogs_dir = config.catalogs_dir();
    let runtime_file = catalogs_dir.join(format!("{}.json", format_str));

    let raw = if runtime_file.exists() {
        tokio::fs::read_to_string(&runtime_file)
            .await
            .ok()
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    let raw = raw.unwrap_or_else(|| bundled_for_format(&format_str).to_string());

    let map: HashMap<String, String> = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => anyhow::bail!("Failed to parse catalog for format '{}': {}", format_str, e),
    };

    if map.is_empty() {
        println!("No shortcuts found for format '{}'.", format_str);
        return Ok(());
    }

    // Apply search filter
    let search_lower = search.as_deref().map(str::to_lowercase);

    // Sort entries, separate _comment_ keys (section headers) from real shortcuts
    let mut sections: Vec<(Option<String>, Vec<(String, String)>)> = Vec::new();
    let mut current_section: Option<String> = None;
    let mut current_items: Vec<(String, String)> = Vec::new();

    // Build a sorted list preserving _comment_ grouping order
    // Since HashMap doesn't preserve order, re-parse as array of (key, value) by reading raw JSON
    let ordered = parse_ordered(&raw);

    for (key, value) in ordered {
        if key.starts_with("_comment") {
            // Flush current section
            if !current_items.is_empty() || current_section.is_some() {
                sections.push((current_section.take(), std::mem::take(&mut current_items)));
            }
            // Strip the marker prefix from the human-readable label
            let label = value.trim_matches('-').trim().to_string();
            current_section = Some(label);
        } else {
            // Apply search filter
            if let Some(ref filter) = search_lower {
                if !key.to_lowercase().contains(filter) && !value.to_lowercase().contains(filter) {
                    continue;
                }
            }
            current_items.push((key, value));
        }
    }
    // Flush last section
    sections.push((current_section, current_items));

    // Count total matching
    let total: usize = sections.iter().map(|(_, items)| items.len()).sum();
    if total == 0 {
        if let Some(ref q) = search {
            println!(
                "No shortcuts matching '{}' in {} catalog.",
                q,
                format_str.to_uppercase()
            );
        } else {
            println!("No shortcuts found.");
        }
        return Ok(());
    }

    println!();
    println!(
        "  Catalog: {}  ({})",
        format_str.to_uppercase(),
        runtime_file.display()
    );
    if let Some(ref q) = search {
        println!(
            "  Filter:  \"{}\"  ({} result{})",
            q,
            total,
            if total == 1 { "" } else { "s" }
        );
    }
    println!();

    let col_w = 40usize;
    println!("  {:<col_w$}  {}", "SHORTCUT", "REPO");
    println!("  {}", "─".repeat(90));

    for (section_label, items) in &sections {
        if items.is_empty() {
            continue;
        }
        if let Some(label) = section_label {
            println!();
            println!("  \x1b[2m— {} —\x1b[0m", label);
        }
        for (shortcut, repo) in items {
            println!("  {:<col_w$}  {}", shortcut, repo);
        }
    }

    println!();
    println!("  Pull any model with: lmforge pull <SHORTCUT>");
    println!();

    Ok(())
}

/// Parse JSON object preserving insertion order (serde_json preserves order in Value::Object).
fn parse_ordered(raw: &str) -> Vec<(String, String)> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .map(|obj| {
            obj.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Detect the engine format for the current platform.
/// On macOS (Apple Silicon) → mlx, Linux x86_64 → safetensors/gguf → gguf fallback.
fn detect_platform_format() -> String {
    #[cfg(target_os = "macos")]
    {
        // Check if oMLX is available (installed by lmforge init on Apple Silicon)
        if std::process::Command::new("omlx")
            .arg("--version")
            .output()
            .is_ok()
        {
            return "mlx".to_string();
        }
    }
    // Default to gguf (covers Linux, Windows, CPU-only macOS)
    "gguf".to_string()
}

/// Returns the bundled catalog string for the given format.
fn bundled_for_format(format: &str) -> &'static str {
    match format {
        "mlx" => crate::model::catalog::BUNDLED_MLX,
        "gguf" => crate::model::catalog::BUNDLED_GGUF,
        _ => "{}",
    }
}
