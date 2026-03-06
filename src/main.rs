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
            tracing_subscriber::EnvFilter::from_default_env().add_directive(if cli.verbose {
                tracing::Level::DEBUG.into()
            } else {
                tracing::Level::INFO.into()
            }),
        )
        .with_target(false)
        .init();

    let cfg = cli.config.as_deref();

    match cli.command {
        Commands::Init {
            update,
            dry_run,
            skip,
        } => {
            let ctx = commands::Context::new_without_targets(cli.verbose, cfg).await?;
            commands::init::run(&ctx, update, dry_run, skip).await
        }
        Commands::Config => commands::config::run(cfg).await,
        Commands::Check { target } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::check::run(&ctx).await
        }
        Commands::Checkout {
            target,
            format,
            history,
            since,
            out,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::checkout::run(&ctx, format, history, since, out).await
        }
        Commands::Sync {
            target,
            dry_run,
            files,
            no_push_missing,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::sync::run(&ctx, dry_run, &files, no_push_missing).await
        }
        Commands::Run {
            target,
            command,
            sudo,
            yes,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::run::run(&ctx, &command, sudo, yes).await
        }
        Commands::Exec {
            target,
            script,
            sudo,
            yes,
            keep,
            dry_run,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::exec::run(&ctx, &script, sudo, yes, keep, dry_run).await
        }
        Commands::Log {
            last,
            since,
            host,
            action,
            errors,
        } => {
            let ctx = commands::Context::new_without_targets(cli.verbose, cfg).await?;
            commands::log::run(&ctx, last, since, host, action, errors).await
        }
    }
}
