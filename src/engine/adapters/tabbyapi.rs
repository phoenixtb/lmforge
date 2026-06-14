//! TabbyAPI (ExLlamaV3) engine adapter — opt-in tier (Phase 4).
//!
//! ## Why TabbyAPI lives here
//!
//! TabbyAPI is the official OpenAI-compatible front-end for ExLlamaV2/V3.
//! It plays the same role for EXL3 quants that vLLM plays for AWQ/GPTQ
//! safetensors. On consumer Blackwell, ExLlamaV3 0.0.37 ships cu132 wheels
//! with sm_120 cubins baked in — no JIT compile at start.
//!
//! ## Design choices vs. vLLM
//!
//!  * **`python main.py` spawn, not a console script**: TabbyAPI is a
//!    clone-and-run application. Its `pyproject.toml` sets
//!    `py-modules = []`, so `pip install` only pulls *dependencies*
//!    (torch, exllamav3, fastapi). The actual server lives in `main.py`
//!    inside the cloned tree. The adapter therefore needs both the venv
//!    Python AND the cloned source dir on disk.
//!
//!  * **cwd = source repo**: `main.py` imports its sibling modules
//!    (`common/`, `endpoints/`) via relative paths. Running it from any
//!    other cwd raises `ModuleNotFoundError`. We set cwd to the cloned
//!    repo and write the generated `config.yml` next to `main.py`.
//!
//!  * **Generated `config.yml` per slot**: TabbyAPI doesn't have a clean
//!    set of CLI flags for every option — its argparser is auto-generated
//!    from the config schema and the flag names use dotted syntax that's
//!    fragile across releases. A small YAML file is the official knob,
//!    and we own the path so two slots never collide.
//!
//!  * **No embeddings, no reranking**: TabbyAPI supports both via its
//!    `[extras]` group (infinity-emb, sentence-transformers). LMForge
//!    keeps those on the llama.cpp sidecar to match the policy used for
//!    vLLM and SGLang. Saves ~500 MB of install footprint.
//!
//!  * **`cache_mode = "8,8"`**: EXL3's 8-bit KV cache is the sweet spot
//!    for accuracy vs. VRAM on consumer cards. Tunable via env knob.
//!
//!  * **Process-group isolation + PATH injection**: same fix as vLLM.
//!    ExLlamaV3 forks helper workers; `killpg` on stop is the only way
//!    to reliably free VRAM.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};

use crate::engine::adapter::{ActiveEngine, EngineAdapter, ModelRole};
use crate::model::downloader::DownloadProgress;

/// Env knob: EXL3 KV-cache mode (e.g. `"8,8"`, `"6,6"`, `"4,4"`).
/// Format is `<k_bits>,<v_bits>` per TabbyAPI's `model.cache_mode` field.
const ENV_CACHE_MODE: &str = "LMFORGE_TABBYAPI_CACHE_MODE";
const DEFAULT_CACHE_MODE: &str = "8,8";

/// Env knob: prompt-ingestion chunk size (TabbyAPI default 2048; lower
/// values reduce VRAM at the cost of ingestion speed).
const ENV_CHUNK_SIZE: &str = "LMFORGE_TABBYAPI_CHUNK_SIZE";
const DEFAULT_CHUNK_SIZE: u32 = 2048;

/// Test-only override of the venv python path. Mirrors vLLM's knob.
const ENV_TABBY_PYTHON: &str = "LMFORGE_TABBYAPI_PYTHON";

#[derive(Clone, Default)]
pub struct TabbyApiAdapter;

impl TabbyApiAdapter {
    fn resolve_python(&self, data_dir: &Path) -> PathBuf {
        if let Ok(env_path) = std::env::var(ENV_TABBY_PYTHON)
            && !env_path.trim().is_empty()
        {
            return PathBuf::from(env_path.trim());
        }
        if cfg!(windows) {
            data_dir
                .join("engines")
                .join("tabbyapi")
                .join("venv")
                .join("Scripts")
                .join("python.exe")
        } else {
            data_dir
                .join("engines")
                .join("tabbyapi")
                .join("venv")
                .join("bin")
                .join("python3")
        }
    }

    fn source_dir(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("engines").join("tabbyapi").join("source")
    }

    fn cache_mode() -> String {
        std::env::var(ENV_CACHE_MODE)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| is_valid_cache_mode(s))
            .unwrap_or_else(|| DEFAULT_CACHE_MODE.to_string())
    }

    fn chunk_size() -> u32 {
        std::env::var(ENV_CHUNK_SIZE)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| (256..=8192).contains(n))
            .unwrap_or(DEFAULT_CHUNK_SIZE)
    }
}

impl EngineAdapter for TabbyApiAdapter {
    /// Same huggingface_hub strategy as vLLM/SGLang. EXL3 repos follow the
    /// safetensors shard-manifest convention; nothing engine-specific in
    /// the download path.
    async fn pull_model(
        &self,
        repo: &str,
        dest_dir: &Path,
        data_dir: &Path,
        progress_tx: Sender<DownloadProgress>,
    ) -> Result<bool> {
        std::fs::create_dir_all(dest_dir)
            .context("Failed to create model destination directory")?;

        // EXL3 repos use per-bpw git branches (turboderp's convention).
        // The resolver encodes the chosen branch as `repo@revision`; we
        // split it back out and pass it as `revision=` to snapshot_download.
        // Without this, snapshot_download silently pulls `main` — which
        // for an EXL3 repo means README.md only and nothing else.
        let (repo_id, revision) = crate::model::resolver::split_revision(repo);
        info!(
            repo_id,
            revision = ?revision,
            dest = %dest_dir.display(),
            "TabbyAPI: starting huggingface_hub pull"
        );

        let _ = progress_tx
            .send(DownloadProgress::Started {
                repo: repo.to_string(),
                files: 0,
            })
            .await;

        let revision_arg = match revision {
            Some(rev) => format!(", revision='{}'", rev),
            None => String::new(),
        };
        let python_snippet = format!(
            "import sys; \
             from huggingface_hub import snapshot_download; \
             snapshot_download(repo_id='{repo}', local_dir='{dest}', local_dir_use_symlinks=False{rev}); \
             print('OK')",
            repo = repo_id,
            dest = dest_dir.to_string_lossy(),
            rev = revision_arg,
        );

        let python = self.resolve_python(data_dir);
        debug!(python = %python.display(), "TabbyAPI pull: using interpreter");

        let output = crate::util::subprocess::hidden_tokio(&python)
            .args(["-c", &python_snippet])
            .output()
            .await
            .context("Failed to spawn python for huggingface_hub pull")?;

        if output.status.success() {
            let total_bytes = dir_size(dest_dir);
            info!(
                repo,
                total_bytes, "TabbyAPI: huggingface_hub pull completed"
            );

            let _ = progress_tx
                .send(DownloadProgress::Completed {
                    repo: repo.to_string(),
                    total_bytes,
                })
                .await;

            Ok(true)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!(repo, stderr = %stderr, stdout = %stdout, "TabbyAPI: pull failed");
            let user_error = extract_python_error(&stderr);

            let _ = progress_tx
                .send(DownloadProgress::Failed {
                    error: user_error.clone(),
                })
                .await;

            anyhow::bail!("huggingface_hub pull failed: {}", user_error)
        }
    }

    async fn start(
        &self,
        model_id: &str,
        model_dir: &Path,
        port: u16,
        data_dir: &Path,
        logs_dir: &Path,
        role: ModelRole,
    ) -> Result<ActiveEngine> {
        if role != ModelRole::Chat {
            anyhow::bail!(
                "TabbyAPI in this build only serves Chat models. \
                 Embed/Rerank slots route to llama.cpp — ensure your \
                 model's `engine` field is `llamacpp`."
            );
        }

        let python = self.resolve_python(data_dir);
        let source = self.source_dir(data_dir);
        let main_py = source.join("main.py");

        if !python.is_file() {
            anyhow::bail!(
                "TabbyAPI venv Python not found at {}. Run: lmforge engine install tabbyapi",
                python.display()
            );
        }
        if !main_py.is_file() {
            anyhow::bail!(
                "TabbyAPI source not found at {}. Run: lmforge engine install tabbyapi",
                main_py.display()
            );
        }

        info!(model_id = %model_id, port = port, "Spawning TabbyAPI (ExLlamaV3)");

        let stdout_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stdout")?;
        let stderr_file =
            crate::logging::rotation::prepare_engine_log(logs_dir, model_id, "stderr")?;

        // model_dir is `.../models/<dir_name>`. TabbyAPI wants the parent
        // (model_dir field) plus the basename (model_name).
        let model_parent = model_dir
            .parent()
            .context("model_dir has no parent — bad layout")?;
        let model_name = model_dir
            .file_name()
            .and_then(|n| n.to_str())
            .context("model_dir basename is not valid UTF-8")?;

        let cache_mode = Self::cache_mode();
        let chunk_size = Self::chunk_size();

        // Write per-slot config.yml into a work dir scoped to this model.
        // Two concurrent loads of the same model would collide; the
        // Manager's slot router prevents that today.
        let work_dir = data_dir
            .join("engines")
            .join("tabbyapi")
            .join("work")
            .join(safe_dir_component(model_id));
        std::fs::create_dir_all(&work_dir).with_context(|| {
            format!(
                "Failed to create TabbyAPI work dir at {}",
                work_dir.display()
            )
        })?;
        let config_path = work_dir.join("config.yml");
        let config_yaml = render_config_yaml(
            port,
            &model_parent.to_string_lossy(),
            model_name,
            &cache_mode,
            chunk_size,
        );
        std::fs::write(&config_path, config_yaml)
            .with_context(|| format!("Failed to write config.yml at {}", config_path.display()))?;
        info!(config = %config_path.display(), "TabbyAPI config rendered");

        // PATH injection — same rationale as vLLM. Some ExLlamaV3 helper
        // paths shell out to `ninja` / `nvcc` if a kernel needs JIT (rare
        // on cu132 wheels, but cheap insurance).
        let venv_bin_dir = python
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = if existing_path.is_empty() {
            venv_bin_dir.to_string_lossy().into_owned()
        } else {
            format!("{}:{}", venv_bin_dir.display(), existing_path)
        };

        // TabbyAPI loads `config.yml` from the *current working directory*
        // (see `common.tabby_config.TabbyConfig._from_file`). It has no
        // `--config-file` flag. Approach: cwd = our per-slot work dir
        // (which holds the generated `config.yml`), and invoke `main.py`
        // by absolute path. Python automatically prepends the script's
        // parent directory to `sys.path`, so `import common.*` resolves
        // back to the cloned source tree.
        //
        // Process-group isolation — exllamav3 spawns worker processes for
        // tensor-parallel layouts (and a stream-decoder helper even on a
        // single GPU). killpg() on stop catches the lot.
        let mut command = crate::util::subprocess::hidden_tokio(&python);
        command
            .arg(main_py.to_string_lossy().as_ref())
            .current_dir(&work_dir)
            .env("PATH", &new_path)
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let child = command.spawn().with_context(|| {
            format!(
                "Failed to spawn TabbyAPI at {}. Is the engine installed? Run: lmforge engine install tabbyapi",
                main_py.display()
            )
        })?;

        Ok(ActiveEngine {
            process: child,
            model_id: model_id.to_string(),
            spec_observer: None,
            spec_mode: crate::engine::speculative::SpecMode::Off,
        })
    }

    async fn stop(&self, active_engine: &mut ActiveEngine) -> Result<()> {
        if let Some(pid) = active_engine.process.id() {
            info!(pid, model = %active_engine.model_id, "TabbyAPI: SIGTERM (process-group)");
            #[cfg(unix)]
            {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = active_engine.process.kill().await;
            }

            // ExLlamaV3 teardown is fast (~2-3s on a single-GPU host) but
            // tensor-parallel workers can take longer to flush. Budget 10s
            // to match vLLM, then SIGKILL the group.
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                active_engine.process.wait(),
            )
            .await
            {
                Ok(_) => debug!("TabbyAPI exited cleanly"),
                Err(_) => {
                    warn!("TabbyAPI SIGTERM timed out; SIGKILL to group");
                    #[cfg(unix)]
                    {
                        use nix::sys::signal::{Signal, kill};
                        use nix::unistd::Pid;
                        let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGKILL);
                    }
                    let _ = active_engine.process.kill().await;
                }
            }
        }
        Ok(())
    }
}

/// Render a minimal TabbyAPI `config.yml`. We pin only the fields LMForge
/// actually drives; everything else falls back to TabbyAPI defaults.
///
/// The YAML is hand-written (not via serde-yaml) because it's a tiny
/// fixed schema and pulling in a YAML lib for this one file is overkill.
/// Strings are quoted to defang accidental booleans (e.g. a model_name of
/// `Yes-7B-exl3` becoming the YAML boolean `Yes`).
fn render_config_yaml(
    port: u16,
    model_dir: &str,
    model_name: &str,
    cache_mode: &str,
    chunk_size: u32,
) -> String {
    format!(
        r#"# Auto-generated by LMForge — do not edit by hand.
network:
  host: 127.0.0.1
  port: {port}
  disable_auth: true
  send_tracebacks: false
  api_servers: ["OAI"]

logging:
  log_prompt: false
  log_generation_params: false
  log_requests: false

model:
  model_dir: {model_dir_quoted}
  model_name: {model_name_quoted}
  backend: exllamav3
  cache_mode: {cache_mode_quoted}
  chunk_size: {chunk_size}
  output_chunking: true
  gpu_split_auto: true

developer:
  unsafe_launch: false
  disable_request_streaming: false

memory:
  cuda_malloc_async: true
"#,
        port = port,
        model_dir_quoted = yaml_quote(model_dir),
        model_name_quoted = yaml_quote(model_name),
        cache_mode_quoted = yaml_quote(cache_mode),
        chunk_size = chunk_size,
    )
}

/// Quote a value as a YAML double-quoted scalar. Escapes `\` and `"` so a
/// path with quotes (Windows shenanigans, mostly) survives the round-trip.
fn yaml_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

/// Filesystem-safe rewrite of a model id for use as a directory name. The
/// HF id `RedHatAI/Qwen3-1.7B-quantized.w4a16` would otherwise create a
/// `RedHatAI/` subdir under work/.
fn safe_dir_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            c => c,
        })
        .collect()
}

/// Validate a cache_mode string like `"8,8"`. EXL3 accepts k_bits,v_bits
/// pairs where each is an integer in 2..=8. We refuse malformed values so
/// a typo in env vars doesn't make TabbyAPI bail at startup with a less
/// helpful error than ours.
fn is_valid_cache_mode(s: &str) -> bool {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return false;
    }
    parts.iter().all(|p| {
        p.trim()
            .parse::<u8>()
            .map(|n| (2..=8).contains(&n))
            .unwrap_or(false)
    })
}

/// Recursive directory size — same helper as vLLM. Could share, but a 12-line
/// helper isn't worth a `pub` API surface.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total = total.saturating_add(dir_size(&p));
            } else if let Ok(meta) = p.metadata() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Pull a one-line user-facing error out of a noisy Python traceback.
/// Same heuristic as vLLM's helper.
fn extract_python_error(stderr: &str) -> String {
    let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
    last_line
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "Python process failed without a clear error message".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_config_yaml_includes_required_fields() {
        let yaml = render_config_yaml(11431, "/data/models", "Qwen3-7B-exl3", "8,8", 2048);
        // Sanity-check the fields TabbyAPI requires to load a model at
        // startup. If any of these drift, the daemon will see "model not
        // loaded" health-fail without a clear root cause.
        assert!(yaml.contains("port: 11431"));
        assert!(yaml.contains("backend: exllamav3"));
        assert!(yaml.contains("model_name: \"Qwen3-7B-exl3\""));
        assert!(yaml.contains("model_dir: \"/data/models\""));
        assert!(yaml.contains("cache_mode: \"8,8\""));
        assert!(yaml.contains("chunk_size: 2048"));
        assert!(yaml.contains("disable_auth: true"));
    }

    #[test]
    fn yaml_quote_escapes_specials() {
        assert_eq!(yaml_quote("simple"), "\"simple\"");
        assert_eq!(yaml_quote(r#"path\with"quotes"#), r#""path\\with\"quotes""#);
    }

    #[test]
    fn safe_dir_component_neutralises_slashes() {
        assert_eq!(
            safe_dir_component("RedHatAI/Qwen3-1.7B"),
            "RedHatAI_Qwen3-1.7B"
        );
        assert_eq!(safe_dir_component("vendor:1.0/model"), "vendor_1.0_model");
    }

    #[test]
    fn is_valid_cache_mode_accepts_valid_pairs() {
        for ok in &["2,2", "4,4", "6,6", "8,8", "4,8", "8,2"] {
            assert!(is_valid_cache_mode(ok), "should accept {}", ok);
        }
    }

    #[test]
    fn is_valid_cache_mode_rejects_garbage() {
        for bad in &["", "8", "8,8,8", "9,8", "1,8", "abc", "FP16", "8/8"] {
            assert!(!is_valid_cache_mode(bad), "should reject {:?}", bad);
        }
    }

    #[test]
    fn cache_mode_env_override_respected() {
        let key = ENV_CACHE_MODE;
        let prev = std::env::var(key).ok();
        // Valid override.
        unsafe { std::env::set_var(key, "6,6") };
        assert_eq!(TabbyApiAdapter::cache_mode(), "6,6");
        // Invalid override falls back to default.
        unsafe { std::env::set_var(key, "garbage") };
        assert_eq!(TabbyApiAdapter::cache_mode(), DEFAULT_CACHE_MODE);
        // Restore.
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn chunk_size_clamps_to_safe_range() {
        let key = ENV_CHUNK_SIZE;
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, "1024") };
        assert_eq!(TabbyApiAdapter::chunk_size(), 1024);
        unsafe { std::env::set_var(key, "999999") };
        assert_eq!(TabbyApiAdapter::chunk_size(), DEFAULT_CHUNK_SIZE);
        unsafe { std::env::set_var(key, "32") };
        assert_eq!(TabbyApiAdapter::chunk_size(), DEFAULT_CHUNK_SIZE);
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[tokio::test]
    async fn refuses_non_chat_role() {
        // Role guard short-circuits before any filesystem check, so we
        // can pass any path here.
        let adapter = TabbyApiAdapter;
        let logs = std::env::temp_dir();
        let data = std::env::temp_dir();
        let model_dir = data.join("models").join("nonexistent");
        let result = adapter
            .start(
                "test-model",
                &model_dir,
                11999,
                &data,
                &logs,
                ModelRole::Embed,
            )
            .await;
        match result {
            Ok(_) => panic!("Embed role must be refused"),
            Err(e) => assert!(
                e.to_string().contains("only serves Chat models"),
                "Wrong error: {}",
                e
            ),
        }
    }

    #[test]
    fn extract_python_error_picks_last_nonblank() {
        let stderr =
            "Traceback (most recent call last):\n  File ...\nValueError: model not found\n\n";
        assert_eq!(extract_python_error(stderr), "ValueError: model not found");
    }

    #[test]
    fn extract_python_error_handles_empty() {
        assert!(extract_python_error("").contains("without a clear"));
    }
}
