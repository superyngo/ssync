//! `App` struct + main event loop + render dispatch.
//!
//! Phase 1a scope (per docs/tui_reconstruct_plan.md §19): tab bar,
//! Config/Operate placeholders, minimal Checkout host table, status bar
//! with red `app.error`, terminal-size guard, minimal `?` help popup,
//! signal handlers (SIGHUP/SIGTERM on Unix, ctrl_c on Windows).
//!
//! Phase 4 (§19): Config tab 3-level read-only browser (section → entry → field)
//! + external editor 4-stage flow (§7.4) with config_mtime change detection.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal,
};
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
use crate::commands::report::{CheckReport, HostStatus};
use crate::commands::{Context, TargetMode};
use crate::config::schema::AppConfig;

use super::async_bridge::{EventSender, RunningOp, TuiEvent};
use super::components::popup::centered_rect;
use super::components::target_filter::{FilterPopup, FilterPopupResult};
use super::components::viewport::Viewport;
use super::state::persist::{
    self, ActiveTab, TargetFilterMode, TargetFilterState, TuiPersistedState,
};
use super::tabs::config_tab::ConfigTabState;
use super::tabs::TabId;
use super::theme::Theme;

const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;
const POLL_INTERVAL_MS: u64 = 50;

/// Operate-tab focused element (Phase 3 minimal model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperateFocus {
    TargetRow,
    Execute,
}

pub struct App {
    pub active_tab: TabId,
    pub theme: Theme,
    pub error: Option<String>,
    pub help_open: bool,
    pub should_quit: bool,
    pub info_open: bool,
    pub checkout_viewport: Viewport,
    pub checkout_snapshots: Vec<HostSnapshot>,
    pub checkout_columns: DisplayColumns,
    pub config: AppConfig,
    pub config_path: Option<PathBuf>,
    /// 3-level Config tab browser state (Phase 4).
    config_tab: ConfigTabState,
    /// Set by `handle_key` when `E` is pressed; drained by `run()` after each event.
    needs_editor_open: bool,
    /// Until this instant the Config tab shows a yellow "Config reloaded" banner.
    config_reload_banner_until: Option<Instant>,
    pub state_file_path: PathBuf,
    pub target_filter: TargetFilterState,
    pub filter_popup: Option<FilterPopup>,
    operate_focus: OperateFocus,
    /// Currently-running operation, if any. Mutually exclusive with starting
    /// a new one (concurrency guard per Phase 3 step 10).
    running_op: Option<RunningOp>,
    /// Bridge channel sender. Spawned tasks clone this via `EventSender`.
    event_tx: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
    /// Bridge receiver, drained by the main loop.
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<TuiEvent>>,
    /// Final report from the most recently completed operation, shown in the
    /// results popup until dismissed.
    completed_report: Option<CheckReport>,
    /// True when a snapshot DB write happened in this session and the
    /// Checkout tab needs to reload before its next render (§18.3).
    db_stale: bool,
    /// Tracks the most recent timeout used (filter timeout or default).
    last_timeout_secs: u64,
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
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let config_tab = ConfigTabState::new(&ctx.config, ctx.config_path.as_deref());
        let timeout = if persisted.target_filter.timeout > 0 {
            persisted.target_filter.timeout
        } else {
            ctx.config.settings.default_timeout
        };

        Self {
            active_tab,
            theme: Theme::default_palette(),
            error: None,
            help_open: false,
            should_quit: false,
            info_open: false,
            checkout_viewport: viewport,
            checkout_snapshots: snapshots,
            checkout_columns: columns,
            config: ctx.config.clone(),
            config_path: ctx.config_path.clone(),
            config_tab,
            needs_editor_open: false,
            config_reload_banner_until: None,
            state_file_path,
            target_filter: persisted.target_filter,
            filter_popup: None,
            operate_focus: OperateFocus::Execute,
            running_op: None,
            event_tx,
            event_rx: Some(event_rx),
            completed_report: None,
            db_stale: false,
            last_timeout_secs: timeout,
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

        // Move the event_rx out of self for the loop's lifetime. Rebuild before any
        // future call would need it (we never re-enter this method).
        let mut event_rx = self
            .event_rx
            .take()
            .expect("event_rx is Some after construction");

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

            // Drain bridge events (non-blocking) before rendering.
            while let Ok(ev) = event_rx.try_recv() {
                if self.handle_tui_event(ev) {
                    dirty = true;
                }
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

            // Open external editor if requested (§7.4 4-stage flow).
            if self.needs_editor_open {
                self.needs_editor_open = false;
                self.do_open_editor(&mut terminal)?;
                dirty = true;
            }

            // Expire the "Config reloaded" banner so it disappears after 2s.
            if let Some(until) = self.config_reload_banner_until {
                if Instant::now() >= until {
                    self.config_reload_banner_until = None;
                    dirty = true;
                }
            }
        }

        Ok(())
    }

    /// Handle an inbound TuiEvent from a running operation. Returns true if
    /// state changed and a redraw is needed.
    fn handle_tui_event(&mut self, ev: TuiEvent) -> bool {
        match ev {
            TuiEvent::HostStarted(_host) => {
                // Phase 3: rendering reads from running_op.host_outcomes; the
                // started signal is informational. Future phases may track
                // in-flight hosts explicitly.
                true
            }
            TuiEvent::HostCompleted {
                host,
                status,
                detail,
                duration_ms,
            } => {
                if let Some(op) = self.running_op.as_mut() {
                    op.record_completed(&host, status, &detail, duration_ms);
                }
                true
            }
            TuiEvent::OperationFinished(report) => {
                self.running_op = None;
                self.db_stale = true;
                self.completed_report = Some(report);
                true
            }
            TuiEvent::OperationCancelled => {
                self.running_op = None;
                self.db_stale = true;
                self.error = Some("Operation cancelled".to_string());
                true
            }
            TuiEvent::OperationError(msg) => {
                self.running_op = None;
                self.error = Some(format!("Operation failed: {msg}"));
                true
            }
        }
    }

    /// Execute a `check` operation against the current target filter. Returns
    /// false (no-op) if an operation is already running (concurrency guard).
    fn execute_check(&mut self) -> bool {
        if self.running_op.is_some() {
            self.error = Some("Operation already running".to_string());
            return true;
        }

        let target_mode = build_target_mode(&self.target_filter, &self.config);
        let targets: Vec<String> = match resolve_target_names(&target_mode, &self.config) {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => {
                self.error = Some("No hosts matched the current filter.".to_string());
                return true;
            }
            Err(e) => {
                self.error = Some(format!("Filter error: {e}"));
                return true;
            }
        };

        let serial = self.target_filter.serial;
        let timeout = self.last_timeout_secs;
        let verbose = false;
        let cfg = self.config.clone();
        let cfg_path = self.config_path.clone();
        let event_tx = self.event_tx.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel.clone();

        // Run the operation on a dedicated OS thread with its own
        // current-thread tokio runtime. This sidesteps the Send constraint
        // imposed by tokio::spawn on the main multi-thread runtime
        // (rusqlite::Connection is Send but !Sync, so &Context is !Send and
        // check_core's future cannot be sent between threads). A current-
        // thread runtime never moves the future across threads.
        let _ = std::thread::Builder::new()
            .name("ssync-op".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let ctx = match Context::from_tui_parts(
                        cfg, cfg_path, target_mode, serial, timeout, verbose,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                            return;
                        }
                    };
                    let sink = EventSender::new(event_tx.clone());
                    let outcome = tokio::select! {
                        res = crate::commands::check::check_core(&ctx, Some(&sink)) => res,
                        _ = cancel_for_task.cancelled() => {
                            let _ = event_tx.send(TuiEvent::OperationCancelled);
                            return;
                        }
                    };
                    match outcome {
                        Ok(crate::commands::report::CommandReport::Check(report)) => {
                            let _ = event_tx.send(TuiEvent::OperationFinished(report));
                        }
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                        }
                    }
                });
            });

        self.running_op = Some(RunningOp {
            cancel,
            started_at: std::time::Instant::now(),
            targets,
            host_outcomes: Vec::new(),
        });
        true
    }

    /// Reload Checkout snapshots if the DB is marked stale. Called immediately
    /// before rendering the Checkout tab (§18.3 lazy reopen).
    fn maybe_reload_checkout(&mut self) {
        if !self.db_stale {
            return;
        }
        match crate::state::db::open(self.config.settings.state_dir.as_deref()) {
            Ok(conn) => {
                let host_names: Vec<&str> =
                    self.config.host.iter().map(|h| h.name.as_str()).collect();
                // Build a temporary minimal Context for fetch_latest_snapshots.
                let tmp_ctx = Context {
                    config: self.config.clone(),
                    config_path: self.config_path.clone(),
                    db: conn,
                    timeout: self.last_timeout_secs,
                    mode: TargetMode::All,
                    serial: false,
                    verbose: false,
                };
                if let Ok(snaps) = fetch_latest_snapshots(&tmp_ctx, &host_names) {
                    self.checkout_snapshots = snaps;
                }
                self.db_stale = false;
            }
            Err(e) => {
                tracing::warn!("Checkout DB reload failed: {e}");
                // Leave db_stale true so the next OperationFinished retries.
            }
        }
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

        // Info popup intercepts: only Esc/i close it.
        if self.info_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('i') => {
                    self.info_open = false;
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }

        // Completed report popup: Esc / Enter dismisses it.
        if self.completed_report.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.completed_report = None;
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }

        // Running operation: Esc cancels (cooperatively).
        if let Some(op) = self.running_op.as_ref() {
            if key.code == KeyCode::Esc {
                op.cancel.cancel();
                return Ok(true);
            }
            // While running, ignore most keys except 1/2/3 tab switches and Ctrl+C.
            match key.code {
                KeyCode::Char('1') => {
                    self.active_tab = TabId::Config;
                    return Ok(true);
                }
                KeyCode::Char('2') => {
                    self.active_tab = TabId::Operate;
                    return Ok(true);
                }
                KeyCode::Char('3') => {
                    self.active_tab = TabId::Checkout;
                    return Ok(true);
                }
                KeyCode::Tab => {
                    self.active_tab = self.active_tab.next();
                    return Ok(true);
                }
                KeyCode::BackTab => {
                    self.active_tab = self.active_tab.prev();
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }

        match key.code {
            // ── Global keys (always first; work from any tab) ──────────────
            KeyCode::Char('q') => {
                self.should_quit = true;
                Ok(true)
            }
            KeyCode::Char('?') => {
                self.help_open = true;
                Ok(true)
            }
            KeyCode::Char('i') => {
                self.info_open = true;
                Ok(true)
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
            // Tab/BackTab on Config tab cycle zones (§8.2); on other tabs
            // cycle the tab bar.
            KeyCode::Tab if self.active_tab != TabId::Config => {
                self.active_tab = self.active_tab.next();
                Ok(true)
            }
            KeyCode::BackTab if self.active_tab != TabId::Config => {
                self.active_tab = self.active_tab.prev();
                Ok(true)
            }

            // ── Config tab (§8.6, §12.2, Phase 4) ─────────────────────────
            // E opens external editor (§7.4 4-stage flow).
            KeyCode::Char('E') if self.active_tab == TabId::Config => {
                if self.running_op.is_some() {
                    self.error =
                        Some("Cannot edit config while an operation is running.".to_string());
                } else if self.config_path.is_none() {
                    self.error =
                        Some("No config path set — cannot open editor.".to_string());
                } else {
                    self.needs_editor_open = true;
                }
                Ok(true)
            }
            // All other Config tab keys routed to ConfigTabState.
            _ if self.active_tab == TabId::Config => {
                Ok(self.config_tab.handle_key(key, &self.config))
            }

            // ── Operate tab ────────────────────────────────────────────────
            KeyCode::Up | KeyCode::Char('k') if self.active_tab == TabId::Operate => {
                self.operate_focus = OperateFocus::TargetRow;
                Ok(true)
            }
            KeyCode::Down | KeyCode::Char('j') if self.active_tab == TabId::Operate => {
                self.operate_focus = OperateFocus::Execute;
                Ok(true)
            }
            KeyCode::Enter
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::Execute =>
            {
                Ok(self.execute_check())
            }
            KeyCode::Char('f') if self.active_tab == TabId::Operate => {
                let popup = FilterPopup::new(
                    self.target_filter.clone(),
                    true,
                    &self.config,
                );
                self.filter_popup = Some(popup);
                Ok(true)
            }

            // ── Checkout tab ───────────────────────────────────────────────
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
            TabId::Config => self.render_config(chunks[1], frame),
            TabId::Operate => self.render_operate(chunks[1], frame),
            TabId::Checkout => {
                self.maybe_reload_checkout();
                self.render_checkout(chunks[1], frame);
            }
        }
        self.render_status_bar(chunks[2], frame);

        if self.help_open {
            self.render_help_popup(area, frame);
        }
        if self.info_open {
            self.render_info_popup(area, frame);
        }
        if let Some(popup) = &self.filter_popup {
            popup.render(area, &self.theme, frame);
        }
        if self.running_op.is_some() {
            self.render_progress_popup(area, frame);
        }
        if let Some(report) = self.completed_report.clone() {
            self.render_results_popup(area, frame, &report);
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

    fn render_config(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        let banner_until = self.config_reload_banner_until;
        self.config_tab.render(
            area,
            frame,
            &self.theme,
            &self.config,
            self.config_path.as_deref(),
            banner_until,
        );
    }

    /// §7.4 external editor 4-stage flow.
    ///
    /// Called from `run()` when `needs_editor_open` is set — giving access to
    /// the `Terminal` object needed for `terminal.clear()` after restore.
    fn do_open_editor(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let path = match &self.config_path {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        // Resolve editor: $VISUAL → $EDITOR → platform default.
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| {
                if cfg!(windows) {
                    "notepad".to_string()
                } else {
                    "vi".to_string()
                }
            });

        // Stage 1 — PAUSE: leave alternate screen + disable raw mode.
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().flush();

        // Stage 2 — EXECUTE.
        let status = std::process::Command::new(&editor).arg(&path).status();

        // Stage 3 — RESTORE: re-enter alternate screen.
        let _ = terminal::enable_raw_mode();
        let _ = execute!(io::stdout(), terminal::EnterAlternateScreen);
        terminal.clear()?;

        if let Err(e) = &status {
            self.error = Some(format!("Failed to launch '{editor}': {e}"));
            return Ok(());
        }

        // Detect mtime change and reload config if file was modified.
        let new_mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok();

        let should_reload = match (self.config_tab.config_mtime, new_mtime) {
            (Some(old), Some(new)) => old != new,
            // On Windows mtime granularity is 2s; also treat a successful exit as reload signal.
            _ => status.map(|s| s.success()).unwrap_or(false),
        };

        if should_reload {
            match crate::config::app::load(Some(&path)) {
                Ok(Some(new_config)) => {
                    self.config = new_config;
                    self.config_tab.reload(&self.config, Some(&path));
                    self.config_reload_banner_until =
                        Some(Instant::now() + Duration::from_secs(2));
                }
                Ok(None) => {
                    self.error = Some("Config file disappeared after editor exit.".to_string());
                }
                Err(e) => {
                    self.error = Some(format!("Config reload failed: {e}"));
                }
            }
        }

        Ok(())
    }

    fn render_operate(&self, area: Rect, frame: &mut ratatui::Frame) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(" Operate ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // operation row
                Constraint::Length(4), // target row + summary
                Constraint::Min(0),    // applicable entries panel
                Constraint::Length(3), // execute button
            ])
            .split(inner);

        // Operation row (only `check` is wired in MVP).
        let op_row = Paragraph::new(Line::from(vec![
            Span::raw(" Operation: "),
            Span::styled(
                "[◉ check]",
                Style::default()
                    .fg(self.theme.accent_operate)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("[ run ]", Style::default().fg(self.theme.inactive)),
            Span::raw("  "),
            Span::styled("[ exec ]", Style::default().fg(self.theme.inactive)),
            Span::raw("  "),
            Span::styled("[ sync ]", Style::default().fg(self.theme.inactive)),
        ]));
        frame.render_widget(op_row, chunks[0]);

        // Target row.
        let target_focused = self.operate_focus == OperateFocus::TargetRow;
        let mode_summary = match self.target_filter.mode {
            TargetFilterMode::All => "all hosts".to_string(),
            TargetFilterMode::Groups => format!("groups:{}", self.target_filter.groups.join(",")),
            TargetFilterMode::Hosts => format!("hosts:{}", self.target_filter.hosts.join(",")),
            TargetFilterMode::Shell => format!("shell:{:?}", self.target_filter.shell),
        };
        let target_count = match resolve_target_names(
            &build_target_mode(&self.target_filter, &self.config),
            &self.config,
        ) {
            Ok(t) => t.len(),
            Err(_) => 0,
        };
        let target_text = format!(
            " Target: {}  ({} hosts)    [f] Filter   serial={}   timeout={}s",
            mode_summary,
            target_count,
            self.target_filter.serial,
            self.target_filter.timeout,
        );
        let target_p = Paragraph::new(target_text).style(if target_focused {
            Style::default()
                .fg(self.theme.accent_operate)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        });
        frame.render_widget(target_p, chunks[1]);

        // Applicable entries panel — read-only summary.
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            "─ Applicable [[check]] entries ─",
            Style::default().fg(self.theme.inactive),
        )));
        if self.config.check.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no [[check]] entries — add one to config.toml)",
                Style::default().fg(self.theme.inactive),
            )));
        } else {
            for (i, entry) in self.config.check.iter().enumerate().take(6) {
                let label = entry
                    .name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| format!("Check #{}", i + 1));
                let metrics = if entry.enabled.is_empty() {
                    "(no metrics)".to_string()
                } else {
                    entry.enabled.join(",")
                };
                let groups = if entry.groups.is_empty() {
                    "unscoped".to_string()
                } else {
                    format!("groups:[{}]", entry.groups.join(","))
                };
                lines.push(Line::from(format!(
                    "  ▸ {} — {}  metrics:{}",
                    label, groups, metrics
                )));
            }
            if self.config.check.len() > 6 {
                lines.push(Line::from(format!(
                    "  ... ({} more not shown)",
                    self.config.check.len() - 6
                )));
            }
        }
        let panel = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(panel, chunks[2]);

        // [Execute] button.
        let execute_focused = self.operate_focus == OperateFocus::Execute;
        let exec_label = if self.running_op.is_some() {
            " [ running... — Esc to cancel ] "
        } else {
            " [ Execute (Enter) ] "
        };
        let exec_style = if execute_focused && self.running_op.is_none() {
            Style::default()
                .fg(self.theme.accent_operate)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if self.running_op.is_some() {
            Style::default().fg(self.theme.warning)
        } else {
            Style::default().fg(self.theme.inactive)
        };
        let exec = Paragraph::new(Line::from(Span::styled(exec_label, exec_style)))
            .block(Block::default().borders(Borders::TOP));
        frame.render_widget(exec, chunks[3]);
    }

    fn render_progress_popup(&self, area: Rect, frame: &mut ratatui::Frame) {
        let popup_area = centered_rect(70, 70, area);
        frame.render_widget(Clear, popup_area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(" Running check — Esc to cancel ");
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let Some(op) = &self.running_op else {
            return;
        };
        let mut lines: Vec<Line> = Vec::new();
        let elapsed = op.started_at.elapsed().as_secs();
        lines.push(Line::from(format!(
            "Targets: {}    Completed: {}    Elapsed: {}s",
            op.targets.len(),
            op.completed_count(),
            elapsed,
        )));
        lines.push(Line::from(""));

        // Show last 12 outcomes.
        let take = 12;
        let start = op.host_outcomes.len().saturating_sub(take);
        for (host, status, detail, ms) in &op.host_outcomes[start..] {
            let glyph = match status {
                HostStatus::Online => "✓",
                HostStatus::Partial => "⚠",
                HostStatus::Offline => "✗",
                HostStatus::Unreachable => "⊘",
                HostStatus::TimedOut => "⏱",
                HostStatus::Error => "✗",
            };
            let color = match status {
                HostStatus::Online => self.theme.accent_checkout,
                HostStatus::Partial => self.theme.warning,
                _ => self.theme.error,
            };
            let line = format!(
                "  {} {:<16} ({:>4}ms) — {}",
                glyph,
                truncate(host, 16),
                ms,
                truncate(detail, 60),
            );
            lines.push(Line::from(Span::styled(line, Style::default().fg(color))));
        }

        let p = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(p, inner);
    }

    fn render_results_popup(&self, area: Rect, frame: &mut ratatui::Frame, report: &CheckReport) {
        let popup_area = centered_rect(75, 75, area);
        frame.render_widget(Clear, popup_area);
        let title = format!(
            " Results — {} hosts  (Enter / Esc to dismiss) ",
            report.hosts.len()
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(title);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let mut lines: Vec<Line> = Vec::new();
        let total = report.hosts.len();
        let online = report
            .hosts
            .iter()
            .filter(|h| matches!(h.status, HostStatus::Online | HostStatus::Partial))
            .count();
        let offline = total - online;
        lines.push(Line::from(format!(
            "Summary: {} ok / {} fail    Executed: {}",
            online, offline, report.executed_at
        )));
        lines.push(Line::from(""));

        for h in &report.hosts {
            let glyph = match h.status {
                HostStatus::Online => "✓",
                HostStatus::Partial => "⚠",
                HostStatus::Offline => "✗",
                HostStatus::Unreachable => "⊘",
                HostStatus::TimedOut => "⏱",
                HostStatus::Error => "✗",
            };
            let color = match h.status {
                HostStatus::Online => self.theme.accent_checkout,
                HostStatus::Partial => self.theme.warning,
                _ => self.theme.error,
            };
            let ms = h.duration_ms.unwrap_or(0);
            let line = format!(
                "  {} {:<16} ({:>4}ms) — {}",
                glyph,
                truncate(&h.host, 16),
                ms,
                truncate(&h.detail, 80),
            );
            lines.push(Line::from(Span::styled(line, Style::default().fg(color))));
        }

        let p = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(p, inner);
    }

    fn render_info_popup(&self, area: Rect, frame: &mut ratatui::Frame) {
        let popup_area = centered_rect(60, 50, area);
        frame.render_widget(Clear, popup_area);
        let body = match self.active_tab {
            TabId::Operate => format!(
                "Operate tab\n\nThe currently configured operation is `check`.\nUse `f` to change the target filter; press Enter on the [Execute] button to run.\nWhile a check is in progress, Esc cancels cooperatively (may take up to {}s per host).\n\nResults appear in a popup when the operation completes; the Checkout tab is automatically refreshed.",
                self.last_timeout_secs
            ),
            TabId::Checkout => "Checkout tab\n\nLatest snapshot per host. Use ↑↓/jk/PgUp/PgDn/Home/End to scroll.\nData refreshes automatically after each `check` run from Operate.".to_string(),
            TabId::Config => format!(
                "Config tab (read-only browser)\n\n\
                 Sidebar: ↑↓ / jk to move between sections and entries.\n\
                 Field table: → or Tab to enter, ← to return to sidebar.\n\
                 Within each pane: ↑↓ / jk / PgUp / PgDn / Home / End.\n\n\
                 E  — open config in $VISUAL / $EDITOR / vi (TUI suspends,\n\
                      resumes after exit; config reloads if file was changed).\n\n\
                 Config path: {}",
                self.config_path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(default — ~/.config/ssync/config.toml)".to_string())
            ),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(" Info (i) ");
        let p = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
        frame.render_widget(p, popup_area);
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
            TabId::Config => "↑↓/jk:Rows  ←→:Zones  E:EditInEditor  1/2/3:Tabs  ?:Help  q:Quit",
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

Config tab
  ↑↓ / j k    Move sidebar / field rows
  ← / →       Switch zones (Sidebar ↔ FieldTable)
  Tab         Switch zone (Sidebar → FieldTable)
  PgUp/PgDn   Page navigation
  Home/End    Jump to top / bottom
  E           Open config in $VISUAL/$EDITOR (TUI suspends, reloads on change)
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

/// Build a `TargetMode` from the persisted filter state and current config.
/// Empty Groups/Hosts → falls back to All.
fn build_target_mode(filter: &TargetFilterState, _config: &AppConfig) -> TargetMode {
    match filter.mode {
        TargetFilterMode::All => TargetMode::All,
        TargetFilterMode::Groups => {
            if filter.groups.is_empty() {
                TargetMode::All
            } else {
                TargetMode::Groups(filter.groups.clone())
            }
        }
        TargetFilterMode::Hosts => {
            if filter.hosts.is_empty() {
                TargetMode::All
            } else {
                TargetMode::Hosts(filter.hosts.clone())
            }
        }
        TargetFilterMode::Shell => TargetMode::Shell(vec![filter.shell.to_shell_type()]),
    }
}

/// Resolve the matching host names for a TargetMode against a config.
fn resolve_target_names(mode: &TargetMode, config: &AppConfig) -> anyhow::Result<Vec<String>> {
    let names: Vec<String> = match mode {
        TargetMode::All => config.host.iter().map(|h| h.name.clone()).collect(),
        TargetMode::Hosts(specs) => config
            .host
            .iter()
            .filter(|h| specs.contains(&h.name))
            .map(|h| h.name.clone())
            .collect(),
        TargetMode::Groups(groups) => config
            .host
            .iter()
            .filter(|h| h.groups.iter().any(|g| groups.contains(g)))
            .map(|h| h.name.clone())
            .collect(),
        TargetMode::Shell(shells) => config
            .host
            .iter()
            .filter(|h| shells.contains(&h.shell))
            .map(|h| h.name.clone())
            .collect(),
    };
    Ok(names)
}

fn truncate(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if s.width() <= max {
        return s.to_string();
    }
    let mut w = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
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
