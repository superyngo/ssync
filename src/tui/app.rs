//! `App` struct + main event loop + render dispatch.
//!
//! Phase 1a scope (per docs/tui_reconstruct_plan.md §19): tab bar,
//! Config/Operate placeholders, minimal Checkout host table, status bar
//! with red `app.error`, terminal-size guard, minimal `?` help popup,
//! signal handlers (SIGHUP/SIGTERM on Unix, ctrl_c on Windows).

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap},
    Terminal,
};

use crate::commands::checkout::{
    extract_metric_value, fetch_latest_snapshots, format_relative_time, metric_header,
    metric_width, DisplayColumns, HostSnapshot,
};
use crate::commands::Context;
use crate::config::schema::AppConfig;

use super::components::popup::centered_rect;
use super::components::target_filter::{FilterPopup, FilterPopupResult};
use super::components::viewport::Viewport;
use super::state::persist::{
    self, ActiveTab, TargetFilterState, TuiPersistedState,
};
use super::tabs::TabId;
use super::theme::Theme;

const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;
const POLL_INTERVAL_MS: u64 = 50;

pub struct App {
    pub active_tab: TabId,
    pub theme: Theme,
    pub error: Option<String>,
    pub help_open: bool,
    pub should_quit: bool,
    pub checkout_viewport: Viewport,
    pub checkout_snapshots: Vec<HostSnapshot>,
    pub checkout_columns: DisplayColumns,
    pub config: AppConfig,
    pub state_file_path: PathBuf,
    pub target_filter: TargetFilterState,
    pub filter_popup: Option<FilterPopup>,
}

impl App {
    pub fn from_context(ctx: &Context) -> Self {
        let columns = DisplayColumns::from_context(ctx);
        let host_names: Vec<&str> = ctx.config.host.iter().map(|h| h.name.as_str()).collect();
        let snapshots = if host_names.is_empty() {
            Vec::new()
        } else {
            fetch_latest_snapshots(ctx, &host_names).unwrap_or_default()
        };
        let mut viewport = Viewport::new();
        viewport.set_dims(snapshots.len(), 0);

        // Resolve TUI state file path; on failure fall back to a path in the
        // OS temp dir so save/load remain functional even with unusual configs.
        let state_file_path = persist::state_file_path(&ctx.config, ctx.config_path.as_deref())
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to resolve TUI state path; using temp dir: {e}");
                std::env::temp_dir().join("ssync_tui_state.toml")
            });

        // Load persisted state and validate against current config (§16.2).
        let mut persisted = persist::load(&state_file_path);
        persist::validate_filter(&mut persisted.target_filter, &ctx.config);

        let active_tab = persisted.tui_state.active_tab.to_tab_id();

        Self {
            active_tab,
            theme: Theme::default_palette(),
            error: None,
            help_open: false,
            should_quit: false,
            checkout_viewport: viewport,
            checkout_snapshots: snapshots,
            checkout_columns: columns,
            config: ctx.config.clone(),
            state_file_path,
            target_filter: persisted.target_filter,
            filter_popup: None,
        }
    }

    /// Persist current state to disk. Errors are logged but never propagated.
    fn save_state(&self) {
        let state = TuiPersistedState {
            tui_state: super::state::persist::TuiSection {
                active_tab: ActiveTab::from_tab_id(self.active_tab),
            },
            target_filter: self.target_filter.clone(),
            operate: Default::default(),
        };
        if let Err(e) = persist::save(&self.state_file_path, &state) {
            tracing::warn!(
                "Failed to save TUI state to {}: {e}",
                self.state_file_path.display()
            );
        }
    }

    /// Run the main event loop. Returns when the user quits cleanly.
    pub async fn run(&mut self) -> Result<()> {
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;

        // Set up an async signal listener that flips should_quit.
        // This lives for the duration of the loop.
        let (sig_tx, mut sig_rx) = tokio::sync::mpsc::channel::<()>(4);
        spawn_signal_listener(sig_tx);

        let mut dirty = true;
        loop {
            if self.should_quit {
                self.save_state();
                break;
            }

            // Drain any pending signals (non-blocking).
            while let Ok(()) = sig_rx.try_recv() {
                self.should_quit = true;
                dirty = true;
            }

            if dirty {
                terminal.draw(|f| self.render(f.area(), f))?;
                dirty = false;
            }

            // Poll crossterm with a short timeout so signal & dirty paths stay
            // responsive without busy-looping.
            if event::poll(Duration::from_millis(POLL_INTERVAL_MS))? {
                let ev = event::read()?;
                if self.handle_event(ev)? {
                    dirty = true;
                }
            }
        }

        Ok(())
    }

    /// Returns true if the event mutated state (frame should redraw).
    fn handle_event(&mut self, ev: Event) -> Result<bool> {
        match ev {
            Event::Resize(_, _) => Ok(true),
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            _ => Ok(false),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        // Ctrl+C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.should_quit = true;
            return Ok(true);
        }

        // Filter popup is the highest-priority focus root after Ctrl+C.
        if let Some(popup) = self.filter_popup.as_mut() {
            match popup.handle_key(key) {
                FilterPopupResult::Continue => return Ok(true),
                FilterPopupResult::Cancelled => {
                    self.filter_popup = None;
                    return Ok(true);
                }
                FilterPopupResult::Applied => {
                    let popup = self.filter_popup.take().unwrap();
                    self.target_filter = popup.state;
                    persist::validate_filter(&mut self.target_filter, &self.config);
                    self.save_state();
                    return Ok(true);
                }
            }
        }

        // Help popup intercepts: only Esc/? close it.
        if self.help_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => {
                    self.help_open = false;
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                Ok(true)
            }
            KeyCode::Char('?') => {
                self.help_open = true;
                Ok(true)
            }
            KeyCode::Char('f') => {
                // Operate-tab-only per §13 / Phase 2 plan; Checkout filter
                // wiring lands in Phase 6. For now, only Operate opens it.
                if self.active_tab == TabId::Operate {
                    let popup = FilterPopup::new(
                        self.target_filter.clone(),
                        true, // allow_shell on Operate
                        &self.config,
                    );
                    self.filter_popup = Some(popup);
                    return Ok(true);
                }
                Ok(false)
            }
            KeyCode::Esc => {
                if self.error.is_some() {
                    self.error = None;
                    return Ok(true);
                }
                Ok(false)
            }
            KeyCode::Char('1') => {
                self.active_tab = TabId::Config;
                Ok(true)
            }
            KeyCode::Char('2') => {
                self.active_tab = TabId::Operate;
                Ok(true)
            }
            KeyCode::Char('3') => {
                self.active_tab = TabId::Checkout;
                Ok(true)
            }
            KeyCode::Tab => {
                self.active_tab = self.active_tab.next();
                Ok(true)
            }
            KeyCode::BackTab => {
                self.active_tab = self.active_tab.prev();
                Ok(true)
            }
            KeyCode::Up | KeyCode::Char('k') if self.active_tab == TabId::Checkout => {
                self.checkout_viewport.move_up();
                Ok(true)
            }
            KeyCode::Down | KeyCode::Char('j') if self.active_tab == TabId::Checkout => {
                self.checkout_viewport.move_down();
                Ok(true)
            }
            KeyCode::PageUp if self.active_tab == TabId::Checkout => {
                self.checkout_viewport.page_up();
                Ok(true)
            }
            KeyCode::PageDown if self.active_tab == TabId::Checkout => {
                self.checkout_viewport.page_down();
                Ok(true)
            }
            KeyCode::Home if self.active_tab == TabId::Checkout => {
                self.checkout_viewport.home();
                Ok(true)
            }
            KeyCode::End if self.active_tab == TabId::Checkout => {
                self.checkout_viewport.end();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn render(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        // Terminal-size guard (§7.8): below threshold, render only the warning.
        if area.width < MIN_COLS || area.height < MIN_ROWS {
            let msg = format!(
                "Terminal too small (need {}×{}+; have {}×{})\n\nResize the terminal to continue.",
                MIN_COLS, MIN_ROWS, area.width, area.height
            );
            let p = Paragraph::new(msg)
                .style(Style::default().fg(self.theme.error))
                .wrap(Wrap { trim: false });
            frame.render_widget(p, area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(area);

        self.render_tab_bar(chunks[0], frame);
        match self.active_tab {
            TabId::Config => self.render_placeholder(
                chunks[1],
                frame,
                "Config — available in Phase 4",
            ),
            TabId::Operate => self.render_operate_placeholder(chunks[1], frame),
            TabId::Checkout => self.render_checkout(chunks[1], frame),
        }
        self.render_status_bar(chunks[2], frame);

        if self.help_open {
            self.render_help_popup(area, frame);
        }
        if let Some(popup) = &self.filter_popup {
            popup.render(area, &self.theme, frame);
        }
    }

    fn render_tab_bar(&self, area: Rect, frame: &mut ratatui::Frame) {
        let titles: Vec<&str> = TabId::ALL.iter().map(|t| t.label()).collect();
        let selected = TabId::ALL.iter().position(|t| *t == self.active_tab).unwrap_or(0);
        let accent = match self.active_tab {
            TabId::Config => self.theme.accent_config,
            TabId::Operate => self.theme.accent_operate,
            TabId::Checkout => self.theme.accent_checkout,
        };
        let tabs = Tabs::new(titles)
            .block(Block::default().borders(Borders::ALL).title(" ssync "))
            .select(selected)
            .style(Style::default().fg(self.theme.inactive))
            .highlight_style(
                Style::default()
                    .fg(accent)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            );
        frame.render_widget(tabs, area);
    }

    fn render_placeholder(&self, area: Rect, frame: &mut ratatui::Frame, text: &str) {
        let p = Paragraph::new(text)
            .style(Style::default().fg(self.theme.inactive))
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false });
        frame.render_widget(p, area);
    }

    fn render_operate_placeholder(&self, area: Rect, frame: &mut ratatui::Frame) {
        let body = format!(
            "Operate — full UI lands in Phase 3.\n\nCurrent filter:\n  Mode:    {:?}\n  Groups:  {:?}\n  Hosts:   {:?}\n  Serial:  {}\n  Timeout: {}s\n\nPress `f` to open the Target Filter popup.",
            self.target_filter.mode,
            self.target_filter.groups,
            self.target_filter.hosts,
            self.target_filter.serial,
            self.target_filter.timeout,
        );
        let p = Paragraph::new(body)
            .style(Style::default().fg(self.theme.inactive))
            .block(Block::default().borders(Borders::ALL).title(" Operate (Phase 2 preview) "))
            .wrap(Wrap { trim: false });
        frame.render_widget(p, area);
    }

    fn render_checkout(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(" Checkout (latest snapshots) ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // visible_height = inner.height - 1 (header row).
        let visible_height = inner.height.saturating_sub(1) as usize;
        self.checkout_viewport
            .set_dims(self.checkout_snapshots.len(), visible_height);

        if self.checkout_snapshots.is_empty() {
            let p = Paragraph::new(
                "No snapshots available. Run `ssync check --all` to populate the database.",
            )
            .style(Style::default().fg(self.theme.inactive))
            .wrap(Wrap { trim: false });
            frame.render_widget(p, inner);
            return;
        }

        let mut header_cells: Vec<Cell> =
            vec![Cell::from("Host"), Cell::from("Status")];
        let mut constraints: Vec<Constraint> =
            vec![Constraint::Length(16), Constraint::Length(12)];
        for metric in &self.checkout_columns.metrics {
            header_cells.push(Cell::from(metric_header(metric)));
            constraints.push(Constraint::Length(metric_width(metric) as u16));
        }
        header_cells.push(Cell::from("Last Seen"));
        constraints.push(Constraint::Min(10));

        let (start, end) = self.checkout_viewport.visible_range();
        let mut rows = Vec::with_capacity(end - start);
        for (idx, snap) in self.checkout_snapshots[start..end].iter().enumerate() {
            let absolute_idx = start + idx;
            let is_focused = absolute_idx == self.checkout_viewport.selected;
            let prefix = if is_focused { "▶ " } else { "  " };
            let status_text = if snap.online { "✓ online" } else { "✗ offline" };
            let status_style = Style::default().fg(if snap.online {
                self.theme.accent_checkout
            } else {
                self.theme.error
            });

            let mut cells: Vec<Cell> = vec![
                Cell::from(format!("{}{}", prefix, snap.host)),
                Cell::from(status_text).style(status_style),
            ];
            for metric in &self.checkout_columns.metrics {
                let (val, critical) = extract_metric_value(&snap.data, metric);
                let style = if critical {
                    Style::default().fg(self.theme.error)
                } else {
                    Style::default()
                };
                cells.push(Cell::from(val).style(style));
            }
            cells.push(Cell::from(format_relative_time(snap.last_online)));

            let mut row = Row::new(cells);
            if is_focused {
                row = row.style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED));
            }
            rows.push(row);
        }

        let table = Table::new(rows, &constraints).header(
            Row::new(header_cells).style(Style::default().add_modifier(Modifier::BOLD)),
        );
        frame.render_widget(table, inner);
    }

    fn render_status_bar(&self, area: Rect, frame: &mut ratatui::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        if let Some(err) = &self.error {
            let p = Paragraph::new(err.as_str()).style(Style::default().fg(self.theme.error));
            frame.render_widget(p, chunks[0]);
        }

        let hints = match self.active_tab {
            TabId::Config => "1/2/3:Tabs  Tab:Cycle  ?:Help  q:Quit",
            TabId::Operate => "1/2/3:Tabs  Tab:Cycle  f:Filter  ?:Help  q:Quit",
            TabId::Checkout => "↑↓/jk:Rows  PgUp/PgDn  Home/End  Tab:Cycle  ?:Help  q:Quit",
        };
        let p = Paragraph::new(Line::from(vec![Span::styled(
            hints,
            Style::default().fg(self.theme.inactive),
        )]));
        frame.render_widget(p, chunks[1]);
    }

    fn render_help_popup(&self, area: Rect, frame: &mut ratatui::Frame) {
        let popup_area = centered_rect(60, 60, area);
        frame.render_widget(Clear, popup_area);
        let body = "\
Global keys
  1 / 2 / 3   Switch to Config / Operate / Checkout
  Tab         Cycle to next tab
  Shift+Tab   Cycle to previous tab
  q           Quit (state saved)
  Ctrl+C      Quit immediately (state saved)
  Esc         Close popup / clear error
  ?           Toggle this help

Operate tab
  f           Open Target Filter popup

Checkout tab
  ↑↓ / j k    Move row selection
  PgUp/PgDn   Page navigation
  Home/End    Jump to top / bottom

Filter popup
  ↑↓ / Tab    Move between fields
  Space/Enter Toggle / select
  Enter on [Apply]   Commit + persist
  Esc                Cancel

Phase 2 build — operation execution arrives in Phase 3.
";
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(" Keybindings (?) ");
        let p = Paragraph::new(body)
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(p, popup_area);
    }
}

/// Spawn a background task that listens for OS signals and pushes a unit into
/// the channel for each. The main loop drains this channel each iteration.
///
/// Unix: SIGHUP, SIGTERM, SIGINT.
/// Windows: ctrl_c (covers Ctrl+C and CTRL_BREAK_EVENT).
/// CTRL_CLOSE_EVENT (Windows close-button) deferred to post-MVP — see §7.9.
fn spawn_signal_listener(tx: tokio::sync::mpsc::Sender<()>) {
    #[cfg(unix)]
    {
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sighup = match signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to install SIGHUP handler: {e}");
                    return;
                }
            };
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to install SIGTERM handler: {e}");
                    return;
                }
            };
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to install SIGINT handler: {e}");
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = sighup.recv() => { let _ = tx.send(()).await; }
                    _ = sigterm.recv() => { let _ = tx.send(()).await; }
                    _ = sigint.recv() => { let _ = tx.send(()).await; }
                }
            }
        });
    }
    #[cfg(windows)]
    {
        tokio::spawn(async move {
            // TODO(post-MVP windows): CTRL_CLOSE_EVENT via windows-sys for
            // close-button shutdown on Windows.
            loop {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = tx.send(()).await;
                }
            }
        });
    }
}
