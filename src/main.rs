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

/// Enable ANSI escape code support on Windows terminals.
/// Modern Windows 10+ supports ANSI via Virtual Terminal Processing,
/// but it must be explicitly enabled.
#[cfg(target_os = "windows")]
fn enable_ansi_support() {
    #[cfg(feature = "tui")]
    {
        // crossterm (already a dependency via TUI feature) handles ANSI automatically
        // No need to do anything explicitly
    }
    #[cfg(not(feature = "tui"))]
    {
        // Without TUI/crossterm, use raw Win32 API via FFI.
        use std::os::windows::io::AsRawHandle;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
        extern "system" {
            fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
        }
        unsafe {
            let handle = std::io::stdout().as_raw_handle() as *mut std::ffi::c_void;
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn enable_ansi_support() {}

#[tokio::main]
async fn main() -> Result<()> {
    enable_ansi_support();
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
            timeout,
            ..
        } => {
            let ctx = commands::Context::new_without_targets(cli.verbose, cfg, timeout).await?;
            commands::init::run(&ctx, update, dry_run, skip).await
        }
        Commands::Config { .. } => commands::config::run(cfg).await,
        Commands::List { target } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::list::run(&ctx).await
        }
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
            source,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::sync::run(&ctx, dry_run, &files, no_push_missing, source.as_deref()).await
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
            ..
        } => {
            let ctx = commands::Context::new_without_targets(cli.verbose, cfg, None).await?;
            commands::log::run(&ctx, last, since, host, action, errors).await
        }
    }
}
