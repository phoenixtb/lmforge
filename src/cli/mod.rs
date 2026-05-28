pub mod catalog;
pub mod clean;
pub mod doctor;
pub mod engine;
pub mod init;
pub mod logs;
pub mod models;
pub mod pull;
pub mod run;
pub mod service;
pub mod start;
pub mod status;
pub mod stop;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::LmForgeConfig;

/// LMForge — Hardware-aware LLM inference orchestrator
#[derive(Parser, Debug)]
#[command(name = "lmforge", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Log level override (error, warn, info, debug, trace)
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    /// Path to config file (default: ~/.lmforge/config.toml)
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// Path to catalogs directory (default: ~/.lmforge/catalogs)
    #[arg(long, global = true)]
    pub catalogs_dir: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Probe hardware, select engine, and install if needed
    Init,

    /// Start the inference engine and API server
    Start {
        /// Model to load on startup
        #[arg(long)]
        model: Option<String>,

        /// Port for the API server
        #[arg(long)]
        port: Option<u16>,

        /// Address to bind to
        #[arg(long)]
        bind: Option<String>,

        /// Run in foreground (default: daemon mode)
        #[arg(long)]
        foreground: bool,

        /// Force a specific engine (e.g. `sglang`, `vllm`) instead of the
        /// auto-selected default. Hardware gates still apply. When the engine
        /// is in the `experimental` tier you'll be prompted to confirm; pass
        /// `--yes-experimental` (or set `LMFORGE_YES_EXPERIMENTAL=1`) to skip
        /// the prompt in non-interactive shells.
        #[arg(long)]
        engine: Option<String>,

        /// Skip the experimental-engine confirmation prompt. Useful for
        /// scripts / systemd units that explicitly opt into a tier.
        #[arg(long)]
        yes_experimental: bool,
    },

    /// Stop a running LMForge instance
    Stop,

    /// Show engine and model status
    Status,

    /// Download a model
    Pull {
        /// Model name, HF repo, URL, or local path
        model: String,

        /// Force a specific engine when resolving the model's format.
        ///
        /// Without this flag, `pull` uses the engine the auto-selector would
        /// pick (default tier). Use `--engine vllm` to pull a safetensors
        /// model when llamacpp is the default — otherwise pull refuses the
        /// format mismatch.
        #[arg(long)]
        engine: Option<String>,
    },

    /// Run an interactive REPL with a model
    Run {
        /// Model name or alias to run
        model: String,
    },

    /// Manage system services
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Manage installed models
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },

    /// Install / uninstall / inspect opt-in inference engines (vLLM, EXL3, ...)
    Engine {
        #[command(subcommand)]
        action: EngineAction,
    },

    /// List available model shortcuts from the bundled catalog
    Catalog {
        /// Engine format to list (mlx, safetensors, gguf). Defaults to current platform format.
        #[arg(long)]
        format: Option<String>,

        /// Filter shortcuts by keyword (searches shortcut name and repo)
        #[arg(long)]
        search: Option<String>,
    },

    /// Disk usage audit and cleanup (orphans, logs, HuggingFace cache)
    Clean {
        /// Show what would be cleaned without making changes
        #[arg(long)]
        dry_run: bool,

        /// Skip all confirmation prompts
        #[arg(short, long)]
        yes: bool,

        /// Clean everything (orphaned dirs, logs, HF cache duplicates)
        #[arg(long)]
        all: bool,

        /// Remove orphaned model directories (on disk but not in index)
        #[arg(long)]
        partial: bool,

        /// Truncate log files
        #[arg(long)]
        logs: bool,

        /// When pruning logs, keep at most this many MB total (oldest deleted
        /// first). 0 = truncate everything (legacy behaviour).
        #[arg(long, default_value = "0")]
        max_mb: u64,

        /// Remove HuggingFace cache entries duplicated in ~/.lmforge/models/
        #[arg(long)]
        hf_cache: bool,

        /// Remove engine installs (SGLang venv, bundled uv, etc.). Next
        /// `lmforge init` will re-download uv (~24 MB) and rebuild the venv
        /// (~2 min for SGLang). Use this after a torch/CUDA version mismatch
        /// or to reclaim ~5 GB of disk per pip engine.
        #[arg(long)]
        engines: bool,
    },

    /// Diagnose hardware + engine state (driver, compute_cap, glibc,
    /// Vulkan loader, active `llama.cpp` variant, speculative-decoding
    /// status). Mirrors what's surfaced in `/lf/status` so the same
    /// information is reachable without the daemon running.
    Doctor,

    /// View logs
    Logs {
        /// Tail the log continuously
        #[arg(short, long)]
        follow: bool,

        /// Filter by component name
        #[arg(long)]
        component: Option<String>,

        /// Show last N lines
        #[arg(long, default_value = "50")]
        tail: usize,

        /// Show engine stdout/stderr instead of main log
        #[arg(long)]
        engine: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    /// Install LMForge to run automatically on boot
    Install,
    /// Uninstall the LMForge service
    Uninstall,
    /// Start the LMForge service (if service-managed)
    Start,
    /// Stop the LMForge service (daemon process; models will be unloaded)
    Stop,
    /// Show current service status
    Status,
}

#[derive(Subcommand, Debug)]
pub enum ModelsAction {
    /// List installed models
    List,

    /// Remove an installed model from disk
    Remove {
        /// Model name to remove
        name: String,
    },

    /// Unload the active model from VRAM (keeps files on disk)
    Unload,
}

#[derive(Subcommand, Debug)]
pub enum EngineAction {
    /// List every engine in the registry with tier, install status, and
    /// hardware-compatibility verdict for THIS host.
    List,

    /// Install an opt-in engine into its own isolated venv at
    /// `~/.lmforge/engines/<id>/venv/`. Hardware gates are enforced.
    /// Default-tier engines are auto-installed by `lmforge init`; calling
    /// `engine install <default-id>` is a no-op once the binary is staged.
    Install {
        /// Engine id (e.g. `vllm`, `exl3`)
        id: String,

        /// Skip the experimental-tier confirmation prompt.
        #[arg(long)]
        yes_experimental: bool,

        /// For `llamacpp` only — install a specific build variant under
        /// `~/.lmforge/engines/llamacpp/variants/<id>/`. Accepted values:
        /// `cuda12` (default for Linux NVIDIA, sm_75..sm_120, driver≥570.26),
        /// `cuda13` (opt-in, driver≥590.44.01, adds sm_100 B200),
        /// `vulkan` (universal fallback, Linux/Windows AMD/Intel), `cpu`.
        /// Ignored for non-`llamacpp` engines.
        #[arg(long)]
        variant: Option<String>,
    },

    /// Remove an engine install (venv + cached wheels for pip engines,
    /// or the staged binary + sibling libraries for binary engines).
    /// Models on disk are NOT touched.
    Uninstall {
        /// Engine id to remove
        id: String,

        /// Skip the "are you sure?" prompt (non-interactive scripts).
        #[arg(short, long)]
        yes: bool,
    },

    /// Show install state + hardware compatibility for a single engine.
    /// Useful for debugging selector decisions.
    Status {
        /// Engine id to inspect
        id: String,
    },
}

/// Dispatch CLI command to the appropriate handler
pub async fn dispatch(cli: Cli, config: LmForgeConfig) -> Result<()> {
    match cli.command {
        Command::Init => init::run(&config).await,
        Command::Start {
            model,
            port,
            bind,
            foreground,
            engine,
            yes_experimental,
        } => {
            start::run(
                &config,
                start::StartOptions {
                    model,
                    port,
                    bind,
                    foreground,
                    engine,
                    yes_experimental,
                },
                None,
            )
            .await
        }
        Command::Stop => stop::run(&config).await,
        Command::Status => status::run(&config).await,
        Command::Pull { model, engine } => pull::run(&config, &model, engine.as_deref()).await,
        Command::Models { action } => models::run(&config, action).await,
        Command::Catalog { format, search } => catalog::run(&config, format, search).await,
        Command::Clean {
            dry_run,
            yes,
            all,
            partial,
            logs,
            max_mb,
            hf_cache,
            engines,
        } => {
            clean::run(
                &config,
                clean::CleanOptions {
                    dry_run,
                    yes,
                    all,
                    partial,
                    hf_cache,
                    engines,
                    logs,
                    max_mb,
                },
            )
            .await
        }
        Command::Run { model } => run::run(&config, &model).await,
        Command::Engine { action } => engine::run(&config, action).await,
        Command::Doctor => doctor::run(&config).await,
        Command::Service { action } => match action {
            ServiceAction::Install => service::install(),
            ServiceAction::Uninstall => service::uninstall(),
            ServiceAction::Start => service::service_start(),
            ServiceAction::Stop => service::service_stop(),
            ServiceAction::Status => service::service_status(),
        },
        Command::Logs {
            follow,
            component,
            tail,
            engine,
            json,
        } => logs::run(&config, follow, component, tail, engine, json).await,
    }
}
