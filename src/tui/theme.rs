//! Canonical TUI palette (per docs/tui_reconstruct_plan.md §10).
//!
//! 16-color compatible: only ratatui named `Color` variants — no Rgb / Indexed.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent_config: Color,
    pub accent_operate: Color,
    pub accent_checkout: Color,
    pub error: Color,
    pub warning: Color,
    pub inactive: Color,
    pub border_active: Color,
    pub border_inactive: Color,
}

impl Theme {
    pub const fn default_palette() -> Self {
        Self {
            accent_config: Color::Yellow,
            accent_operate: Color::Cyan,
            accent_checkout: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            inactive: Color::DarkGray,
            border_active: Color::Cyan,
            border_inactive: Color::DarkGray,
        }
    }
}
