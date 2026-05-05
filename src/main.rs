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

/// Returns true if the current binary was invoked as `ssync-tui`.
/// Per AD-17, only `ssync-tui` is allowed to enter alternate screen.
#[cfg(feature = "tui")]
fn binary_is_ssync_tui() -> bool {
    std::env::args_os()
        .next()
        .and_then(|p| {
            std::path::Path::new(&p)
                .file_stem()
                .map(|s| s.to_owned())
        })
        .map(|s| s == "ssync-tui")
        .unwrap_or(false)
}

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
/// If RUST_LOG is set, respect it entirely. Otherwise apply defaults:
/// - Verbose mode: DEBUG level to see all logs
/// - Normal mode: INFO level with suppressed russh/zeroize noise (VirtualLock warnings)
fn init_tracing(verbose: bool, silent: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    // If RUST_LOG is set, respect it entirely.
    // Otherwise apply our defaults: suppress russh/zeroize noise unless verbose.
    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        // Suppress VirtualLock warnings and other russh diagnostic noise.
        EnvFilter::new("russh=error,russh_keys=error,ssh_key=error,zeroize=error,info")
    };

    // When `silent` is set (TUI mode), discard log output so it does not leak
    // through the alternate screen. AD-15 calls for an in-memory ring buffer
    // here; the L-key overlay (Phase 7) is post-MVP, so for Phase 1a we just
    // sink the writer.
    if silent {
        fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::sink)
            .init();
    } else {
        fmt().with_env_filter(filter).with_target(false).init();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    enable_ansi_support();
    let cli = Cli::parse();

    // Detect TUI launch path before initialising tracing so we can route
    // log output to a sink when the alternate screen will be active.
    #[cfg(feature = "tui")]
    let tui_silent = cli.command.is_none() && binary_is_ssync_tui();
    #[cfg(not(feature = "tui"))]
    let tui_silent = false;

    init_tracing(cli.verbose, tui_silent);

    let cfg = cli.config.as_deref();

    let command = match cli.command {
        Some(c) => c,
        None => {
            // No subcommand. Per AD-17, only ssync-tui binary may enter TUI.
            #[cfg(feature = "tui")]
            {
                if binary_is_ssync_tui() {
                    return tui::entry::run_or_fallback(cli.verbose, cfg).await;
                }
            }
            eprintln!("Interactive TUI not available. Use the `ssync-tui` binary.");
            std::process::exit(1);
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
