//! Target Filter popup (per docs/tui_reconstruct_plan.md §13).
//!
//! Phase 2 scope: a working multi-mode filter editor — All / Groups / Hosts /
//! Shell. The popup is its own focus root; only Esc dismisses, Enter on
//! `[Apply]` commits to App state. Sealed against arrow-escape per AD-19.
//!
//! Layout (simplified compared to the full §13 mockup; functional in Phase 2):
//!
//!   Mode:
//!     ◉ All hosts
//!     ○ Groups        [chip list of all groups]
//!     ○ Hosts         [chip list of all hosts]
//!     ○ Shell         [chip list of detected shells]
//!
//!   [☐ Serial]    [Timeout: NN]
//!
//!   [Apply]   [Cancel]
//!
//! Shell mode is hidden when the popup is invoked from the Checkout tab
//! (caller passes `allow_shell = false`); see §13.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::config::schema::AppConfig;
use crate::tui::state::persist::{ShellMode, TargetFilterMode, TargetFilterState};
use crate::tui::theme::Theme;

use super::popup::centered_rect;

/// Field currently focused inside the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Mode(TargetFilterMode),
    SerialToggle,
    Apply,
    Cancel,
}

/// Outcome of a key event handed to the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterPopupResult {
    /// User pressed Enter on [Apply]. Caller should adopt `state` and persist.
    Applied,
    /// User pressed Esc or Enter on [Cancel]. Caller should drop the popup.
    Cancelled,
    /// Popup is still active; no externally visible change.
    Continue,
}

pub struct FilterPopup {
    pub state: TargetFilterState,
    pub allow_shell: bool,
    available_groups: Vec<String>,
    available_hosts: Vec<String>,
    field: Field,
}

impl FilterPopup {
    pub fn new(state: TargetFilterState, allow_shell: bool, config: &AppConfig) -> Self {
        let available_groups = collect_groups(config);
        let available_hosts: Vec<String> = config.host.iter().map(|h| h.name.clone()).collect();
        let field = Field::Mode(state.mode);
        Self {
            state,
            allow_shell,
            available_groups,
            available_hosts,
            field,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FilterPopupResult {
        // Ctrl+C inside popup also cancels.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c'))
        {
            return FilterPopupResult::Cancelled;
        }
        match key.code {
            KeyCode::Esc => FilterPopupResult::Cancelled,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_field(-1);
                FilterPopupResult::Continue
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.move_field(1);
                FilterPopupResult::Continue
            }
            KeyCode::BackTab => {
                self.move_field(-1);
                FilterPopupResult::Continue
            }
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                self.toggle_focused();
                FilterPopupResult::Continue
            }
            KeyCode::Enter => self.activate_focused(),
            KeyCode::Left | KeyCode::Right => {
                // Cycle within Mode group; for Hosts/Groups/Shell this also
                // cycles which item is "selected" in the chip list.
                let dir: i32 = if matches!(key.code, KeyCode::Left) { -1 } else { 1 };
                self.cycle_chip(dir);
                FilterPopupResult::Continue
            }
            _ => FilterPopupResult::Continue,
        }
    }

    fn fields_in_order(&self) -> Vec<Field> {
        let mut v = vec![
            Field::Mode(TargetFilterMode::All),
            Field::Mode(TargetFilterMode::Groups),
            Field::Mode(TargetFilterMode::Hosts),
        ];
        if self.allow_shell {
            v.push(Field::Mode(TargetFilterMode::Shell));
        }
        v.push(Field::SerialToggle);
        v.push(Field::Apply);
        v.push(Field::Cancel);
        v
    }

    fn move_field(&mut self, delta: i32) {
        let order = self.fields_in_order();
        let idx = order
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0);
        let len = order.len() as i32;
        let new = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.field = order[new];
    }

    fn toggle_focused(&mut self) {
        match self.field {
            Field::Mode(mode) => {
                self.state.mode = mode;
            }
            Field::SerialToggle => {
                self.state.serial = !self.state.serial;
            }
            _ => {}
        }
    }

    fn activate_focused(&mut self) -> FilterPopupResult {
        match self.field {
            Field::Apply => FilterPopupResult::Applied,
            Field::Cancel => FilterPopupResult::Cancelled,
            Field::Mode(m) => {
                self.state.mode = m;
                FilterPopupResult::Continue
            }
            Field::SerialToggle => {
                self.state.serial = !self.state.serial;
                FilterPopupResult::Continue
            }
        }
    }

    /// When focused on a Mode row, ←→ toggles inclusion of the *first* item
    /// of the mode's chip list. Phase 2 keeps this minimal — the full chip
    /// editor is in Phase 3 alongside Operate UI, where we wire `f` from
    /// Operate's `[Filter]` button. Until then, the Apply path captures
    /// whatever Mode the user picked; group/host details remain whatever
    /// was persisted from a prior session.
    fn cycle_chip(&mut self, _dir: i32) {
        match self.field {
            Field::Mode(TargetFilterMode::Groups) => {
                if self.state.groups.is_empty() {
                    if let Some(first) = self.available_groups.first() {
                        self.state.groups.push(first.clone());
                    }
                }
            }
            Field::Mode(TargetFilterMode::Hosts) => {
                if self.state.hosts.is_empty() {
                    if let Some(first) = self.available_hosts.first() {
                        self.state.hosts.push(first.clone());
                    }
                }
            }
            _ => {}
        }
    }

    pub fn render(&self, area: Rect, theme: &Theme, frame: &mut ratatui::Frame) {
        let popup = centered_rect(70, 70, area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_active))
            .title(" Target Filter (Esc to cancel) ");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        // Vertical layout: mode rows, gap, options row, gap, buttons row.
        let mode_count = if self.allow_shell { 4 } else { 3 };
        let mut constraints = vec![Constraint::Length(1); mode_count];
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1)); // serial
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1)); // buttons
        constraints.push(Constraint::Min(0));
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);

        let mut row = 0;
        for &m in &[
            TargetFilterMode::All,
            TargetFilterMode::Groups,
            TargetFilterMode::Hosts,
            TargetFilterMode::Shell,
        ] {
            if matches!(m, TargetFilterMode::Shell) && !self.allow_shell {
                continue;
            }
            let selected = self.state.mode == m;
            let focused = self.field == Field::Mode(m);
            let glyph = if selected { "◉" } else { "○" };
            let label = format!(" {} {}", glyph, mode_label(m));
            let extra = match m {
                TargetFilterMode::Groups => format_chips(&self.state.groups, "no groups"),
                TargetFilterMode::Hosts => format_chips(&self.state.hosts, "no hosts"),
                TargetFilterMode::Shell => format!("[{}]", shell_label(self.state.shell)),
                TargetFilterMode::All => String::new(),
            };
            let mut spans = vec![Span::styled(label, focus_style(focused, theme))];
            if !extra.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(extra, Style::default().fg(theme.inactive)));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), chunks[row]);
            row += 1;
        }

        // gap row already counted into constraints
        row += 1;
        let serial_glyph = if self.state.serial { "☑" } else { "☐" };
        let serial_text = format!(" {} Serial execution    Timeout: {}s", serial_glyph, self.state.timeout);
        let serial_focused = self.field == Field::SerialToggle;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                serial_text,
                focus_style(serial_focused, theme),
            ))),
            chunks[row],
        );
        row += 2;

        let apply_focused = self.field == Field::Apply;
        let cancel_focused = self.field == Field::Cancel;
        let buttons = Line::from(vec![
            Span::styled(" [Apply]", focus_style(apply_focused, theme)),
            Span::raw("   "),
            Span::styled("[Cancel]", focus_style(cancel_focused, theme)),
        ]);
        frame.render_widget(Paragraph::new(buttons).wrap(Wrap { trim: false }), chunks[row]);
    }
}

fn mode_label(m: TargetFilterMode) -> &'static str {
    match m {
        TargetFilterMode::All => "All hosts",
        TargetFilterMode::Groups => "Groups",
        TargetFilterMode::Hosts => "Hosts",
        TargetFilterMode::Shell => "Shell",
    }
}

fn shell_label(s: ShellMode) -> &'static str {
    match s {
        ShellMode::Sh => "sh",
        ShellMode::PowerShell => "powershell",
        ShellMode::Cmd => "cmd",
    }
}

fn format_chips(items: &[String], empty: &str) -> String {
    if items.is_empty() {
        format!("({})", empty)
    } else {
        items.join(", ")
    }
}

fn focus_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        Style::default()
            .fg(theme.accent_operate)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::default()
    }
}

fn collect_groups(config: &AppConfig) -> Vec<String> {
    let s: BTreeSet<String> = config
        .host
        .iter()
        .flat_map(|h| h.groups.iter().filter(|g| !g.is_empty()).cloned())
        .collect();
    s.into_iter().collect()
}
