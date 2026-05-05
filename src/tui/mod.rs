//! Interactive TUI for ssync. Feature-gated under `tui`.
//!
//! Public surface:
//! - [`entry::run_or_fallback`] — performs §4 TTY/TERM detection and either
//!   launches the TUI or prints help and exits 2 (clap convention for non-TTY).
//!
//! See `docs/tui_reconstruct_plan.md` for the design spec.

pub mod app;
pub mod components;
pub mod entry;
pub mod event;
pub mod focus;
pub mod state;
pub mod tabs;
pub mod terminal;
pub mod theme;
