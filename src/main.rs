use anyhow::Result;
use clap::Parser;

use lmforge::{cli, config, logging};

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();

    // Initialize logging
    logging::init(&args)?;

    // Load configuration
    let config = config::load(&args)?;

    // Dispatch to subcommand
    cli::dispatch(args, config).await
}
