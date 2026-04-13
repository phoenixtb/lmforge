pub mod clean;
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
    },

    /// Stop a running LMForge instance
    Stop,

    /// Show engine and model status
    Status,

    /// Download a model
    Pull {
        /// Model name, HF repo, URL, or local path
        model: String,
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

        /// Remove HuggingFace cache entries duplicated in ~/.lmforge/models/
        #[arg(long)]
        hf_cache: bool,
    },

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

/// Dispatch CLI command to the appropriate handler
pub async fn dispatch(cli: Cli, config: LmForgeConfig) -> Result<()> {
    match cli.command {
        Command::Init => init::run(&config).await,
        Command::Start { model, port, bind, foreground } => start::run(&config, model, port, bind, foreground).await,
        Command::Stop => stop::run(&config).await,
        Command::Status => status::run(&config).await,
        Command::Pull { model } => pull::run(&config, &model).await,
        Command::Models { action } => models::run(&config, action).await,
        Command::Clean { dry_run, yes, all, partial, logs, hf_cache } => {
            clean::run(&config, clean::CleanOptions { dry_run, yes, all, partial, hf_cache, logs }).await
        }
        Command::Run { model } => run::run(&config, &model).await,
        Command::Service { action } => match action {
            ServiceAction::Install => service::install(),
            ServiceAction::Uninstall => service::uninstall(),
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
