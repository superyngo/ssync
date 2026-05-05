//! TerminalGuard + panic hook (per docs/tui_reconstruct_plan.md §7.1, §7.2; AD-9).
//!
//! Mandatory: a panicked TUI MUST leave a usable terminal. The Drop impl and
//! the panic hook are intentionally redundant.

use std::io;

use anyhow::Result;
use crossterm::{event::DisableMouseCapture, execute, terminal};

/// RAII guard that puts the terminal in alternate-screen + raw mode on
/// install and restores it on drop (normal exit, `?` early return, panic
/// unwind — all run Drop).
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn install() -> Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            DisableMouseCapture,
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// Install a panic hook that restores the terminal before printing the
/// backtrace. Idempotent if called multiple times (the previous hook is
/// chained, so the original default still fires last).
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));
}
