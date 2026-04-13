use anyhow::Result;
use std::io::Write;

use crate::config::LmForgeConfig;
use crate::model::index::{dir_size, ModelIndex};

pub struct CleanOptions {
    pub dry_run: bool,
    pub yes: bool,
    pub logs: bool,
    pub partial: bool,
    pub hf_cache: bool,
    pub all: bool,
}

/// `lmforge clean` — Disk usage audit and cleanup
pub async fn run(config: &LmForgeConfig, opts: CleanOptions) -> Result<()> {
    let data_dir = config.data_dir();
    let do_all = opts.all;

    // ── Audit phase ──────────────────────────────────────────────────────────

    println!("Auditing disk usage...\n");

    // 1. Indexed models
    let idx = ModelIndex::load(&data_dir)?;
    let indexed: Vec<_> = idx.list().iter().map(|m| (m.id.clone(), m.path.clone(), m.size_bytes)).collect();
    let indexed_total: u64 = indexed.iter().map(|(_, _, s)| s).sum();

    println!("Indexed models ({}):", indexed.len());
    for (id, _, size) in &indexed {
        println!("  {:40} {:>8}", id, fmt_size(*size));
    }
    println!("  {:40} {:>8}", "Total", fmt_size(indexed_total));

    // 2. Orphaned model directories — on disk but not in index
    //    These are most commonly partial/interrupted downloads.
    let models_dir = data_dir.join("models");
    let indexed_paths: std::collections::HashSet<String> =
        indexed.iter().map(|(_, p, _)| p.clone()).collect();

    let mut orphans: Vec<(String, u64)> = Vec::new();
    if models_dir.exists() {
        for entry in std::fs::read_dir(&models_dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let path_str = path.to_string_lossy().to_string();
                if !indexed_paths.contains(&path_str) {
                    let size = dir_size(&path);
                    orphans.push((path_str, size));
                }
            }
        }
    }
    let orphan_total: u64 = orphans.iter().map(|(_, s)| s).sum();

    println!("\nOrphaned model directories ({}) [not in index — likely incomplete downloads]:", orphans.len());
    if orphans.is_empty() {
        println!("  None.");
    } else {
        for (path, size) in &orphans {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            println!("  {:40} {:>8}  {}", name, fmt_size(*size), path);
        }
        println!("  {:40} {:>8}", "Recoverable", fmt_size(orphan_total));
    }

    // 3. Stale index entries — in index but directory missing from disk
    let mut stale: Vec<String> = Vec::new();
    for (id, path, _) in &indexed {
        if !std::path::Path::new(path).exists() {
            stale.push(id.clone());
        }
    }
    println!("\nStale index entries ({}) [in models.json but missing from disk]:", stale.len());
    if stale.is_empty() {
        println!("  None.");
    } else {
        for id in &stale {
            println!("  {}", id);
        }
    }

    // 4. Log files
    let logs_dir = data_dir.join("logs");
    let log_size = dir_size(&logs_dir);
    println!("\nLog files:  {}", fmt_size(log_size));
    if log_size > 0 {
        for entry in std::fs::read_dir(&logs_dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if let Ok(m) = p.metadata() {
                println!("  {:40} {:>8}", p.file_name().unwrap_or_default().to_string_lossy(), fmt_size(m.len()));
            }
        }
    }

    // 5. HuggingFace cache — check for models already mirrored in ~/.cache/huggingface/hub/
    let hf_cache_dir = dirs::home_dir()
        .map(|h| h.join(".cache").join("huggingface").join("hub"))
        .filter(|p| p.exists());

    let mut hf_duplicates: Vec<(String, u64)> = Vec::new();
    let hf_total = if let Some(ref hf_dir) = hf_cache_dir {
        let total = dir_size(hf_dir);

        // Check which indexed model HF repos have a corresponding cache entry
        for m in idx.list() {
            if let Some(ref repo) = m.hf_repo {
                // HF stores as models--{org}--{repo_name}
                let cache_name = format!("models--{}", repo.replace('/', "--"));
                let cache_path = hf_dir.join(&cache_name);
                if cache_path.exists() {
                    let size = dir_size(&cache_path);
                    hf_duplicates.push((repo.clone(), size));
                }
            }
        }

        total
    } else {
        0
    };

    println!("\nHuggingFace cache (~/.cache/huggingface/hub/): {}", fmt_size(hf_total));
    if !hf_duplicates.is_empty() {
        println!("  Confirmed duplicates (also in ~/.lmforge/models/):");
        for (repo, size) in &hf_duplicates {
            println!("    {:40} {:>8}", repo, fmt_size(*size));
        }
        let dup_total: u64 = hf_duplicates.iter().map(|(_, s)| s).sum();
        println!("  Recoverable from HF cache duplicates: {}", fmt_size(dup_total));
    } else if hf_total > 0 {
        println!("  No confirmed duplicates with indexed models.");
    }

    // ── Summary ───────────────────────────────────────────────────────────────

    let recoverable = orphan_total + log_size
        + hf_duplicates.iter().map(|(_, s)| s).sum::<u64>();

    println!("\n{}", "─".repeat(55));
    println!("Total indexed model storage:  {:>10}", fmt_size(indexed_total));
    println!("Total recoverable (cleanup):  {:>10}", fmt_size(recoverable));
    println!("{}", "─".repeat(55));

    if recoverable == 0 && stale.is_empty() {
        println!("\nNothing to clean up.");
        return Ok(());
    }

    if opts.dry_run {
        println!("\n(dry-run — no changes made)");
        return Ok(());
    }

    // ── Cleanup phase ─────────────────────────────────────────────────────────

    println!();

    // Orphaned dirs
    if !orphans.is_empty() && (do_all || opts.partial || confirm("Remove orphaned model directories?")?) {
        for (path, size) in &orphans {
            std::fs::remove_dir_all(path)?;
            println!("  ✓ Removed {} ({})", std::path::Path::new(path).file_name().unwrap_or_default().to_string_lossy(), fmt_size(*size));
        }
    }

    // Stale index entries
    if !stale.is_empty() {
        let mut idx2 = ModelIndex::load(&data_dir)?;
        for id in &stale {
            idx2.remove(id);
            println!("  ✓ Removed stale index entry '{}'", id);
        }
        idx2.save(&data_dir)?;
    }

    // Logs
    if log_size > 0 && (do_all || opts.logs || confirm("Clear log files?")?) {
        truncate_logs(&logs_dir)?;
        println!("  ✓ Cleared {} of logs", fmt_size(log_size));
    }

    // HF cache duplicates
    if !hf_duplicates.is_empty() && (do_all || opts.hf_cache || confirm("Remove HuggingFace cache entries that are already in ~/.lmforge/models/?")?) {
        if let Some(ref hf_dir) = hf_cache_dir {
            for (repo, size) in &hf_duplicates {
                let cache_name = format!("models--{}", repo.replace('/', "--"));
                let cache_path = hf_dir.join(&cache_name);
                std::fs::remove_dir_all(&cache_path)?;
                println!("  ✓ Removed HF cache entry {} ({})", repo, fmt_size(*size));
            }
        }
    }

    println!("\nDone.");
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{} [y/N]: ", prompt);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

fn truncate_logs(logs_dir: &std::path::Path) -> Result<()> {
    for entry in std::fs::read_dir(logs_dir)?.flatten() {
        let p = entry.path();
        if p.is_file() {
            // Truncate to empty rather than delete — preserves log rotation configs
            std::fs::write(&p, b"")?;
        }
    }
    Ok(())
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}
