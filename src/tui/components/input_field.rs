//! Single-line text input component for the Operate tab param panel.
//!
//! Per docs/tui_reconstruct_plan.md §14.3: all global single-letter shortcuts
//! are suspended while `InputMode::Active`; callers must check the mode flag
//! before routing any key event to the rest of the app.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// Whether the field is currently capturing keyboard input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    Normal,
    Active,
}

/// A single-line text input with visible cursor and Esc-restore semantics.
#[derive(Debug, Clone, Default)]
pub struct InputField {
    /// Current content.
    pub value: String,
    /// Byte-level cursor position within `value`.
    cursor_pos: usize,
    /// Snapshot saved on `Enter` (active → normal) for Esc-restore.
    saved: String,
    pub mode: InputMode,
}

impl InputField {
    pub fn new(initial: &str) -> Self {
        Self {
            value: initial.to_string(),
            cursor_pos: initial.len(),
            saved: initial.to_string(),
            mode: InputMode::Normal,
        }
    }

    /// Activate the field, saving the current value for Esc-restore.
    pub fn activate(&mut self) {
        self.saved = self.value.clone();
        self.mode = InputMode::Active;
        self.cursor_pos = self.value.len();
    }

    /// Deactivate and confirm (save current value as the new baseline).
    pub fn confirm(&mut self) {
        self.saved = self.value.clone();
        self.mode = InputMode::Normal;
    }

    /// Deactivate and revert to the value saved at `activate` time.
    pub fn cancel(&mut self) {
        self.value = self.saved.clone();
        self.cursor_pos = self.value.len();
        self.mode = InputMode::Normal;
    }

    /// Handle a key event while the field is active.
    ///
    /// Returns `true` if the event was consumed and the field should be
    /// redrawn. Returns `false` for unhandled events.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.mode != InputMode::Active {
            return false;
        }
        match key.code {
            KeyCode::Char(c)
                if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT =>
            {
                let byte_pos = self.char_to_byte(self.cursor_pos);
                self.value.insert(byte_pos, c);
                self.cursor_pos += 1;
                true
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    let byte_pos = self.char_to_byte(self.cursor_pos);
                    self.value.remove(byte_pos);
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor_pos < self.char_count() {
                    let byte_pos = self.char_to_byte(self.cursor_pos);
                    self.value.remove(byte_pos);
                }
                true
            }
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
                true
            }
            KeyCode::Right => {
                if self.cursor_pos < self.char_count() {
                    self.cursor_pos += 1;
                }
                true
            }
            KeyCode::Home => {
                self.cursor_pos = 0;
                true
            }
            KeyCode::End => {
                self.cursor_pos = self.char_count();
                true
            }
            KeyCode::Enter => {
                self.confirm();
                true
            }
            KeyCode::Esc => {
                self.cancel();
                true
            }
            _ => false,
        }
    }

    /// Render the field inside `area`. `focused` controls the border colour.
    pub fn render(&self, frame: &mut Frame, area: Rect, label: &str, focused: bool) {
        let border_style = if self.mode == InputMode::Active {
            Style::default().fg(Color::Yellow)
        } else if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(
                format!(" {} ", label),
                Style::default().add_modifier(Modifier::BOLD),
            ));

        // Build the visible line with a cursor marker when active.
        let display = if self.mode == InputMode::Active {
            let (before, after) = self.split_at_cursor();
            let cursor_char = after.chars().next().unwrap_or(' ');
            let after_cursor: String = after.chars().skip(1).collect();
            Line::from(vec![
                Span::raw(before),
                Span::styled(
                    cursor_char.to_string(),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(after_cursor),
            ])
        } else {
            Line::from(Span::raw(self.value.clone()))
        };

        let para = Paragraph::new(display).block(block);
        frame.render_widget(para, area);
    }

    // ------ helpers ------

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }

    fn split_at_cursor(&self) -> (&str, &str) {
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.value.split_at(byte_pos)
    }
}
