mod cli;
mod commands;
mod config;
mod host;
mod metrics;
mod output;
mod state;
#[cfg(feature = "tui")]
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

/// Enable ANSI escape code support on Windows terminals.
/// Modern Windows 10+ supports ANSI via Virtual Terminal Processing,
/// but it must be explicitly enabled.
#[cfg(target_os = "windows")]
fn enable_ansi_support() {
    use std::os::windows::io::AsRawHandle;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    extern "system" {
        fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
    }
    unsafe {
        let handle = std::io::stdout().as_raw_handle();
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn enable_ansi_support() {}

/// Initialize tracing subscriber with appropriate log level filtering.
///
/// When `silent` is true (TUI mode), the fmt writer goes to `std::io::sink()`
/// and an in-memory ring-buffer layer is installed alongside it (AD-15).
/// The returned `LogBufferHandle` provides the ring buffer the TUI reads
/// for the `L` key overlay (§17.2).
///
/// When `silent` is false (CLI mode), only the fmt layer is installed and
/// the returned handle is `None`.
#[cfg(feature = "tui")]
fn init_tracing(verbose: bool, silent: bool) -> Option<crate::tui::log_layer::LogBufferHandle> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter, Layer, Registry};

    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("russh=error,russh_keys=error,ssh_key=error,zeroize=error,info")
    };

    if silent {
        let log_handle = crate::tui::log_layer::LogBufferHandle::new();
        let ring_layer = crate::tui::log_layer::RingBufferLayer::new(log_handle.clone())
            .with_filter(filter.clone());
        let fmt_layer = fmt::layer()
            .with_target(false)
            .with_writer(std::io::sink)
            .with_filter(filter);
        let subscriber = Registry::default().with(ring_layer).with(fmt_layer);
        tracing::subscriber::set_global_default(subscriber)
            .expect("failed to set tracing subscriber");
        Some(log_handle)
    } else {
        let fmt_layer = fmt::layer().with_target(false).with_filter(filter);
        let subscriber = Registry::default().with(fmt_layer);
        tracing::subscriber::set_global_default(subscriber)
            .expect("failed to set tracing subscriber");
        None
    }
}

#[cfg(not(feature = "tui"))]
fn init_tracing(verbose: bool, _silent: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("russh=error,russh_keys=error,ssh_key=error,zeroize=error,info")
    };

    fmt().with_env_filter(filter).with_target(false).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    enable_ansi_support();
    let cli = Cli::parse();

    #[cfg(feature = "tui")]
    let tui_silent = cli.command.is_none();
    #[cfg(not(feature = "tui"))]
    let tui_silent = false;

    #[cfg(feature = "tui")]
    let log_buffer = init_tracing(cli.verbose, tui_silent);
    #[cfg(not(feature = "tui"))]
    init_tracing(cli.verbose, tui_silent);

    let cfg = cli.config.as_deref();

    let command = match cli.command {
        Some(c) => c,
        None => {
            #[cfg(feature = "tui")]
            {
                return tui::entry::run_or_fallback(cli.verbose, cfg).await;
            }
            #[cfg(not(feature = "tui"))]
            {
                eprintln!("TUI not compiled in. Rebuild with --features tui.");
                std::process::exit(1);
            }
        }
    };

    match command {
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
        Commands::Check { target, output } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::check::run(&ctx, &output).await
        }
        Commands::Checkout {
            target,
            history,
            since,
            output,
            ..
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::checkout::run(&ctx, history, since, &output).await
        }
        Commands::Sync {
            target,
            dry_run,
            files,
            no_push_missing,
            source,
            output,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::sync::run(
                &ctx,
                dry_run,
                &files,
                no_push_missing,
                source.as_deref(),
                &output,
            )
            .await
        }
        Commands::Run {
            target,
            command,
            sudo,
            yes,
            output,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::run::run(&ctx, &command, sudo, yes, &output).await
        }
        Commands::Exec {
            target,
            script,
            sudo,
            yes,
            keep,
            dry_run,
            output,
        } => {
            let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
            commands::exec::run(&ctx, &script, sudo, yes, keep, dry_run, &output).await
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_tracing_filter_builds() {
        use tracing_subscriber::EnvFilter;
        let _ = EnvFilter::new("russh=error,russh_keys=error,ssh_key=error,zeroize=error,info");
    }
}
