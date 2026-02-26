mod cli;
mod commands;
mod config;
mod host;
mod metrics;
mod output;
mod state;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(if cli.verbose {
                    tracing::Level::DEBUG.into()
                } else {
                    tracing::Level::INFO.into()
                }),
        )
        .with_target(false)
        .init();

    // Build shared context
    let ctx = commands::Context::new(&cli).await?;

    match cli.command {
        Commands::Init { update, dry_run } => {
            commands::init::run(&ctx, update, dry_run).await
        }
        Commands::Check => {
            commands::check::run(&ctx).await
        }
        Commands::Checkout { format, history, since, out } => {
            commands::checkout::run(&ctx, format, history, since, out).await
        }
        Commands::Sync { dry_run } => {
            commands::sync::run(&ctx, dry_run).await
        }
        Commands::Run { command, sudo, yes } => {
            commands::run::run(&ctx, &command, sudo, yes).await
        }
        Commands::Exec { script, sudo, yes, keep, dry_run } => {
            commands::exec::run(&ctx, &script, sudo, yes, keep, dry_run).await
        }
        Commands::Log { last, since, host, action, errors } => {
            commands::log::run(&ctx, last, since, host, action, errors).await
        }
    }
}
