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
    execute, terminal,
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
use crate::commands::report::{CommandReport, HostStatus};
use crate::commands::{Context, TargetMode};
use crate::config::schema::AppConfig;

use super::async_bridge::{EventSender, RunningOp, TuiEvent};
use super::components::input_field::{InputField, InputMode};
use super::components::popup::centered_rect;
use super::components::target_filter::{FilterPopup, FilterPopupResult};
use super::components::viewport::Viewport;
use super::log_layer::LogBufferHandle;
use super::state::persist::{
    self, ActiveTab, OperationKind, SyncMode, TargetFilterMode, TargetFilterState,
    TuiPersistedState,
};
use super::tabs::config_tab::trunc;
use super::tabs::config_tab::ConfigTabState;
use super::tabs::operate_tab::{self, OperateFocus, OperateRenderData, ParamPanelField};
use super::tabs::TabId;
use super::theme::Theme;
use crate::host::auth::{SshAuthRequest, SshAuthSender};
use operate_tab::truncate;

const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;
const POLL_INTERVAL_MS: u64 = 50;

/// State for the masked SSH auth credential popup.
struct AuthPopup {
    prompt: String,
    input: InputField,
    responder: Option<tokio::sync::oneshot::Sender<String>>,
}

impl AuthPopup {
    fn new(req: SshAuthRequest) -> Self {
        let mut input = InputField::new("");
        input.activate();
        Self {
            prompt: req.prompt,
            input,
            responder: Some(req.responder),
        }
    }

    /// Consume the popup, sending the credential. Zeroizes the input buffer.
    fn submit(&mut self) {
        let credential = std::mem::take(&mut self.input.value);
        if let Some(tx) = self.responder.take() {
            let _ = tx.send(credential);
        }
    }

    /// Dismiss without sending (drops sender → auth failure).
    fn cancel(&mut self) {
        self.input.value.clear();
        self.responder = None;
    }
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
    /// Unfiltered cache for Checkout; `checkout_snapshots` is the filtered view.
    checkout_all_snapshots: Vec<HostSnapshot>,
    pub checkout_columns: DisplayColumns,
    pub config: AppConfig,
    pub config_path: Option<PathBuf>,
    /// 3-level Config tab browser state (Phase 4).
    config_tab: ConfigTabState,
    /// Set by `handle_key` when `E` is pressed; drained by `run()` after each event.
    needs_editor_open: bool,
    pub state_file_path: PathBuf,
    pub target_filter: TargetFilterState,
    pub filter_popup: Option<FilterPopup>,
    operate_focus: OperateFocus,
    /// Currently-selected operation on the Operate tab.
    operate_operation: OperationKind,
    /// Text input for the `run` command field (NOT persisted per AD-12).
    run_command: InputField,
    /// Text input for the `exec` script path field (NOT persisted per AD-12).
    exec_script: InputField,
    /// Sudo / yes / keep boolean params (persisted per AD-12).
    run_sudo: bool,
    run_yes: bool,
    exec_sudo: bool,
    exec_keep: bool,
    /// Sync params (sync_mode and sync_dry_run persisted; adhoc_files NOT persisted per AD-12).
    sync_mode: SyncMode,
    sync_dry_run: bool,
    sync_adhoc_files: Vec<String>,
    sync_adhoc_input: InputField,
    /// Source host override for sync (NOT persisted per AD-12).
    sync_source_input: InputField,
    /// Which param panel field is focused (when operate_focus == ParamPanel).
    param_field: ParamPanelField,
    /// Currently-running operation, if any. Mutually exclusive with starting
    /// a new one (concurrency guard per Phase 3 step 10).
    running_op: Option<RunningOp>,
    /// Bridge channel sender. Spawned tasks clone this via `EventSender`.
    event_tx: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
    /// Bridge receiver, drained by the main loop.
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<TuiEvent>>,
    /// Final report from the most recently completed operation, shown in the
    /// results popup until dismissed.
    completed_report: Option<CommandReport>,
    /// True when a snapshot DB write happened in this session and the
    /// Checkout tab needs to reload before its next render (§18.3).
    db_stale: bool,
    /// Tracks the most recent timeout used (filter timeout or default).
    last_timeout_secs: u64,
    /// Log overlay open state (Phase 7, §17.3 item 3).
    log_overlay_open: bool,
    /// Log overlay viewport for scrolling.
    log_overlay_vp: Viewport,
    /// Log buffer: in-memory ring of tracing events (§17.2).
    log_buffer: Option<LogBufferHandle>,
    /// Active SSH auth popup, if any. Takes highest key-routing priority after Ctrl+C.
    auth_popup: Option<AuthPopup>,
    /// Sender side of the auth bridge channel; cloned into each execute operation.
    auth_bridge_tx: Option<SshAuthSender>,
    /// Scroll offset for the Applicable Entries panel (0 = top).
    entries_scroll: usize,
    /// User-controlled scroll offset for the progress popup (None = auto-scroll to bottom).
    progress_popup_scroll: Option<usize>,
}

impl App {
    pub fn from_context(ctx: &Context, log_buffer: Option<LogBufferHandle>) -> Self {
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
            checkout_snapshots: snapshots.clone(),
            checkout_all_snapshots: snapshots,
            checkout_columns: columns,
            config: ctx.config.clone(),
            config_path: ctx.config_path.clone(),
            config_tab,
            needs_editor_open: false,
            state_file_path,
            target_filter: persisted.target_filter,
            filter_popup: None,
            operate_focus: OperateFocus::Execute,
            operate_operation: persisted.operate.operation,
            run_command: InputField::new(""),
            exec_script: InputField::new(""),
            run_sudo: persisted.operate.run_sudo,
            run_yes: persisted.operate.run_yes,
            exec_sudo: persisted.operate.exec_sudo,
            exec_keep: persisted.operate.exec_keep,
            sync_mode: persisted.operate.sync_mode,
            sync_dry_run: persisted.operate.sync_dry_run,
            sync_adhoc_files: Vec::new(),
            sync_adhoc_input: InputField::new(""),
            sync_source_input: InputField::new(""),
            param_field: ParamPanelField::CommandOrScript,
            running_op: None,
            event_tx,
            event_rx: Some(event_rx),
            completed_report: None,
            db_stale: false,
            last_timeout_secs: timeout,
            log_overlay_open: false,
            log_overlay_vp: Viewport::new(),
            log_buffer,
            auth_popup: None,
            auth_bridge_tx: None,
            entries_scroll: 0,
            progress_popup_scroll: None,
        }
    }

    /// Persist current state to disk. Errors are logged but never propagated.
    /// Returns true if the current operation has an applicable entries panel
    /// (check op, or sync in ConfigEntries mode).
    fn has_entries_panel(&self) -> bool {
        self.operate_operation == OperationKind::Check
            || (self.operate_operation == OperationKind::Sync
                && self.sync_mode == SyncMode::ConfigEntries)
    }

    /// Returns the total number of entries in the currently-applicable panel.
    fn entries_panel_count(&self) -> usize {
        match self.operate_operation {
            OperationKind::Check => self.config.check.len(),
            OperationKind::Sync => self.config.sync.len(),
            _ => 0,
        }
    }

    fn save_state(&self) {
        let state = TuiPersistedState {
            tui_state: super::state::persist::TuiSection {
                active_tab: ActiveTab::from_tab_id(self.active_tab),
            },
            target_filter: self.target_filter.clone(),
            operate: super::state::persist::OperateState {
                operation: self.operate_operation,
                run_sudo: self.run_sudo,
                run_yes: self.run_yes,
                exec_sudo: self.exec_sudo,
                exec_keep: self.exec_keep,
                sync_mode: self.sync_mode,
                sync_dry_run: self.sync_dry_run,
            },
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

        // Bridge: convert SshAuthRequest events to TuiEvent::SshAuthRequired.
        let (auth_tx, mut auth_rx) = tokio::sync::mpsc::unbounded_channel::<SshAuthRequest>();
        self.auth_bridge_tx = Some(auth_tx);
        let event_tx_for_bridge = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some(req) = auth_rx.recv().await {
                let _ = event_tx_for_bridge.send(TuiEvent::SshAuthRequired(req));
            }
        });

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

            if self.config_tab.pending_open_editor {
                self.config_tab.pending_open_editor = false;
                self.needs_editor_open = true;
                dirty = true;
            }

            // Open external editor if requested (§7.4 4-stage flow).
            if self.needs_editor_open {
                self.needs_editor_open = false;
                self.do_open_editor(&mut terminal)?;
                dirty = true;
            }

            // Expire the "Config reloaded" banner so it disappears after 2s.
            if let Some(until) = self.config_tab.reload_banner_until {
                if Instant::now() >= until {
                    self.config_tab.reload_banner_until = None;
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
            TuiEvent::SshAuthRequired(req) => {
                self.auth_popup = Some(AuthPopup::new(req));
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
        let auth_sender = self.auth_bridge_tx.clone();
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
                        cfg,
                        cfg_path,
                        target_mode,
                        serial,
                        timeout,
                        verbose,
                        auth_sender,
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
                        Ok(report) => {
                            let _ = event_tx.send(TuiEvent::OperationFinished(report));
                        }
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                        }
                    }
                });
            });

        self.progress_popup_scroll = None;
        self.running_op = Some(RunningOp {
            cancel,
            started_at: std::time::Instant::now(),
            targets,
            host_outcomes: Vec::new(),
        });
        true
    }

    /// Execute a `run` command against the current target filter.
    fn execute_run(&mut self) -> bool {
        if self.running_op.is_some() {
            self.error = Some("Operation already running".to_string());
            return true;
        }
        let command = self.run_command.value.trim().to_string();
        if command.is_empty() {
            self.error = Some("Command field is empty.".to_string());
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
        let cfg = self.config.clone();
        let cfg_path = self.config_path.clone();
        let event_tx = self.event_tx.clone();
        let auth_sender = self.auth_bridge_tx.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let sudo = self.run_sudo;
        let yes = self.run_yes;

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
                        cfg, cfg_path, target_mode, serial, timeout, false, auth_sender,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                            return;
                        }
                    };
                    let sink = EventSender::new(event_tx.clone());
                    let outcome = tokio::select! {
                        res = crate::commands::run::run_core(&ctx, &command, sudo, yes, Some(&sink)) => res,
                        _ = cancel_for_task.cancelled() => {
                            let _ = event_tx.send(TuiEvent::OperationCancelled);
                            return;
                        }
                    };
                    match outcome {
                        Ok(report) => {
                            let _ = event_tx.send(TuiEvent::OperationFinished(report));
                        }
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                        }
                    }
                });
            });

        self.progress_popup_scroll = None;
        self.running_op = Some(RunningOp {
            cancel,
            started_at: std::time::Instant::now(),
            targets,
            host_outcomes: Vec::new(),
        });
        true
    }

    /// Execute an `exec` (script upload + run) against the current target filter.
    fn execute_exec(&mut self) -> bool {
        if self.running_op.is_some() {
            self.error = Some("Operation already running".to_string());
            return true;
        }
        let script = self.exec_script.value.trim().to_string();
        if script.is_empty() {
            self.error = Some("Script path field is empty.".to_string());
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
        let cfg = self.config.clone();
        let cfg_path = self.config_path.clone();
        let event_tx = self.event_tx.clone();
        let auth_sender = self.auth_bridge_tx.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let sudo = self.exec_sudo;
        let keep = self.exec_keep;

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
                        cfg, cfg_path, target_mode, serial, timeout, false, auth_sender,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                            return;
                        }
                    };
                    let sink = EventSender::new(event_tx.clone());
                    let outcome = tokio::select! {
                        res = crate::commands::exec::exec_core(&ctx, &script, sudo, false, keep, Some(&sink)) => res,
                        _ = cancel_for_task.cancelled() => {
                            let _ = event_tx.send(TuiEvent::OperationCancelled);
                            return;
                        }
                    };
                    match outcome {
                        Ok(report) => {
                            let _ = event_tx.send(TuiEvent::OperationFinished(report));
                        }
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                        }
                    }
                });
            });

        self.progress_popup_scroll = None;
        self.running_op = Some(RunningOp {
            cancel,
            started_at: std::time::Instant::now(),
            targets,
            host_outcomes: Vec::new(),
        });
        true
    }

    /// Execute a `sync` operation in a background thread, following the same
    /// pattern as `execute_check`/`execute_run`/`execute_exec`.
    fn execute_sync(&mut self) -> bool {
        if self.running_op.is_some() {
            self.error = Some("An operation is already running.".to_string());
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
        let cfg = self.config.clone();
        let cfg_path = self.config_path.clone();
        let event_tx = self.event_tx.clone();
        let auth_sender = self.auth_bridge_tx.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let dry_run = self.sync_dry_run;
        let adhoc_files = match self.sync_mode {
            SyncMode::AdHoc => self.sync_adhoc_files.clone(),
            SyncMode::ConfigEntries => Vec::new(),
        };

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
                        cfg, cfg_path, target_mode, serial, timeout, false, auth_sender,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                            return;
                        }
                    };
                    let sink = EventSender::new(event_tx.clone());
                    let outcome = tokio::select! {
                        res = crate::commands::sync::sync_core(&ctx, &adhoc_files, dry_run, None, Some(&sink)) => res,
                        _ = cancel_for_task.cancelled() => {
                            let _ = event_tx.send(TuiEvent::OperationCancelled);
                            return;
                        }
                    };
                    match outcome {
                        Ok(report) => {
                            let _ = event_tx.send(TuiEvent::OperationFinished(report));
                        }
                        Err(e) => {
                            let _ = event_tx.send(TuiEvent::OperationError(e.to_string()));
                        }
                    }
                });
            });

        self.progress_popup_scroll = None;
        self.running_op = Some(RunningOp {
            cancel,
            started_at: std::time::Instant::now(),
            targets,
            host_outcomes: Vec::new(),
        });
        true
    }
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
                    tui_auth_sender: None,
                };
                if let Ok(snaps) = fetch_latest_snapshots(&tmp_ctx, &host_names) {
                    self.checkout_all_snapshots = snaps;
                    self.apply_checkout_filter();
                }
                self.db_stale = false;
            }
            Err(e) => {
                tracing::warn!("Checkout DB reload failed: {e}");
                // Leave db_stale true so the next OperationFinished retries.
            }
        }
    }

    /// Filter `checkout_all_snapshots` by the current `target_filter` host/group
    /// selection and write the result into `checkout_snapshots`.
    fn apply_checkout_filter(&mut self) {
        let target_mode = build_target_mode(&self.target_filter, &self.config);
        let visible: std::collections::HashSet<String> =
            resolve_target_names(&target_mode, &self.config)
                .unwrap_or_default()
                .into_iter()
                .collect();
        if visible.is_empty() {
            // No filter active (or filter matches nothing): show all.
            self.checkout_snapshots = self.checkout_all_snapshots.clone();
        } else {
            self.checkout_snapshots = self
                .checkout_all_snapshots
                .iter()
                .filter(|s| visible.contains(&s.host))
                .cloned()
                .collect();
        }
        self.checkout_viewport
            .set_dims(self.checkout_snapshots.len(), 0);
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

        // Auth popup takes highest priority after Ctrl+C.
        if let Some(popup) = self.auth_popup.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    popup.submit();
                    self.auth_popup = None;
                }
                KeyCode::Esc => {
                    popup.cancel();
                    self.auth_popup = None;
                }
                _ => {
                    popup.input.handle_key(key);
                }
            }
            return Ok(true);
        }

        // §14.3: while an input field is active, suspend ALL other routing.
        if self.active_tab == TabId::Operate {
            let active_field: Option<&mut InputField> =
                match (self.operate_operation, self.operate_focus, self.param_field) {
                    (OperationKind::Run, OperateFocus::ParamPanel, _) => {
                        if self.run_command.mode == InputMode::Active {
                            Some(&mut self.run_command)
                        } else {
                            None
                        }
                    }
                    (OperationKind::Exec, OperateFocus::ParamPanel, _) => {
                        if self.exec_script.mode == InputMode::Active {
                            Some(&mut self.exec_script)
                        } else {
                            None
                        }
                    }
                    (
                        OperationKind::Sync,
                        OperateFocus::ParamPanel,
                        ParamPanelField::SyncAdHocInput,
                    ) => {
                        if self.sync_adhoc_input.mode == InputMode::Active {
                            Some(&mut self.sync_adhoc_input)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
            if let Some(field) = active_field {
                let changed = field.handle_key(key);
                // If sync adhoc input just committed (Enter → mode Normal), add path to list.
                if self.operate_operation == OperationKind::Sync
                    && self.sync_adhoc_input.mode == InputMode::Normal
                    && !self.sync_adhoc_input.value.is_empty()
                {
                    let path = std::mem::take(&mut self.sync_adhoc_input.value);
                    self.sync_adhoc_files.push(path);
                }
                return Ok(changed);
            }
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
                    // Re-filter checkout snapshots from the unfiltered cache.
                    self.apply_checkout_filter();
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

        // Log overlay intercepts: Esc/L close it, scroll inside.
        if self.log_overlay_open {
            return self.handle_log_overlay_key(key);
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
            // While running, ignore most keys except 1/2/3 tab switches, Ctrl+C,
            // and Up/Down which scroll the progress popup.
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
                KeyCode::Up | KeyCode::Char('k') if self.active_tab == TabId::Operate => {
                    // Enable manual scroll: lock to current position if auto-scrolling.
                    let outcomes_len = self
                        .running_op
                        .as_ref()
                        .map_or(0, |o| o.host_outcomes.len());
                    let current = self
                        .progress_popup_scroll
                        .unwrap_or(outcomes_len.saturating_sub(12));
                    self.progress_popup_scroll = Some(current.saturating_sub(1));
                    return Ok(true);
                }
                KeyCode::Down | KeyCode::Char('j') if self.active_tab == TabId::Operate => {
                    let outcomes_len = self
                        .running_op
                        .as_ref()
                        .map_or(0, |o| o.host_outcomes.len());
                    let current = self
                        .progress_popup_scroll
                        .unwrap_or(outcomes_len.saturating_sub(12));
                    let max_start = outcomes_len.saturating_sub(12);
                    let next = (current + 1).min(max_start);
                    // If scrolled to the auto-scroll position, clear manual scroll.
                    if next >= max_start {
                        self.progress_popup_scroll = None;
                    } else {
                        self.progress_popup_scroll = Some(next);
                    }
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }

        // §edit-guard: while config tab has an active text input, suspend all
        // global shortcuts and route directly to the config tab.
        if self.active_tab == TabId::Config && self.config_tab.is_editing_active() {
            let handled = self.config_tab.handle_key(key, &mut self.config);
            if let Some((kind, index)) = self.config_tab.pending_delete.take() {
                self.config_tab.execute_delete(&mut self.config, kind, index);
            }
            return Ok(handled);
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
            KeyCode::Char('L') => {
                self.log_overlay_open = !self.log_overlay_open;
                if self.log_overlay_open {
                    self.log_overlay_vp = Viewport::new();
                    let len = self.log_buffer.as_ref().map_or(0, |b| b.len());
                    self.log_overlay_vp.set_dims(len, 0);
                }
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
                if self.active_tab == TabId::Config && self.config_tab.config_dirty {
                    self.error =
                        Some("Unsaved config changes — press S to save or fix first.".to_string());
                    return Ok(true);
                }
                self.active_tab = TabId::Config;
                Ok(true)
            }
            KeyCode::Char('2') => {
                if self.active_tab == TabId::Config && self.config_tab.config_dirty {
                    self.error =
                        Some("Unsaved config changes — press S to save or fix first.".to_string());
                    return Ok(true);
                }
                self.active_tab = TabId::Operate;
                Ok(true)
            }
            KeyCode::Char('3') => {
                if self.active_tab == TabId::Config && self.config_tab.config_dirty {
                    self.error =
                        Some("Unsaved config changes — press S to save or fix first.".to_string());
                    return Ok(true);
                }
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

            // ── Config tab (§8.6, §12.2, Phase 4+7) ───────────────────────────
            // E opens external editor (§7.4 4-stage flow).
            KeyCode::Char('E') if self.active_tab == TabId::Config => {
                if self.running_op.is_some() {
                    self.error =
                        Some("Cannot edit config while an operation is running.".to_string());
                } else if self.config_path.is_none() {
                    self.error = Some("No config path set — cannot open editor.".to_string());
                } else if self.config_tab.config_dirty {
                    use crate::tui::tabs::config_tab::{ConfirmAction, ConfirmState};
                    self.config_tab.confirm = Some(ConfirmState {
                        prompt: "Unsaved changes will be lost.".to_string(),
                        action: ConfirmAction::OpenEditorDirty,
                        hints: "  [y/Enter] Open editor   [Esc] Cancel",
                    });
                } else {
                    self.needs_editor_open = true;
                }
                Ok(true)
            }
            // S saves config with toml_edit round-trip (Phase 7).
            KeyCode::Char('S') if self.active_tab == TabId::Config => {
                self.save_config();
                Ok(true)
            }
            // 'a' adds a new entry (Phase 7 Case B).
            KeyCode::Char('a') if self.active_tab == TabId::Config => {
                let kind = self.config_add_kind();
                if let Some(kind) = kind {
                    self.config_tab.start_add_entry(kind);
                }
                Ok(true)
            }
            // 'd' deletes focused entry (Phase 7).
            KeyCode::Char('d') if self.active_tab == TabId::Config => {
                if self.config_tab.confirm.is_some() {
                    // Confirm dialog handles 'y'/'n'; 'd' is not consumed here.
                    return Ok(false);
                }
                self.config_tab.request_delete();
                Ok(true)
            }
            // All other Config tab keys routed to ConfigTabState (including 'e'/Enter for inline edit).
            _ if self.active_tab == TabId::Config => {
                let handled = self.config_tab.handle_key(key, &mut self.config);
                if let Some((kind, index)) = self.config_tab.pending_delete.take() {
                    self.config_tab
                        .execute_delete(&mut self.config, kind, index);
                }
                Ok(handled)
            }

            // ── Operate tab ────────────────────────────────────────────────
            // Vertical navigation between zones (OpRadio → ParamPanel → TargetRow → Execute).
            KeyCode::Up | KeyCode::Char('k') if self.active_tab == TabId::Operate => {
                self.operate_focus = match self.operate_focus {
                    OperateFocus::Execute => {
                        // Only go to ApplicableEntries if there's an entries panel visible.
                        if self.has_entries_panel() {
                            let count = self.entries_panel_count();
                            self.entries_scroll = count.saturating_sub(6);
                            OperateFocus::ApplicableEntries
                        } else {
                            OperateFocus::TargetRow
                        }
                    }
                    OperateFocus::ApplicableEntries => {
                        if self.entries_scroll == 0 {
                            OperateFocus::TargetRow
                        } else {
                            self.entries_scroll -= 1;
                            OperateFocus::ApplicableEntries
                        }
                    }
                    OperateFocus::TargetRow => {
                        if self.operate_operation == OperationKind::Check {
                            OperateFocus::OpRadio
                        } else {
                            self.param_field = match self.operate_operation {
                                OperationKind::Sync => ParamPanelField::SyncDryRun,
                                _ => ParamPanelField::SecondFlag,
                            };
                            OperateFocus::ParamPanel
                        }
                    }
                    OperateFocus::ParamPanel => match self.operate_operation {
                        OperationKind::Sync => match self.param_field {
                            ParamPanelField::SyncDryRun => {
                                self.param_field = ParamPanelField::SyncAdHocInput;
                                OperateFocus::ParamPanel
                            }
                            ParamPanelField::SyncAdHocInput => {
                                self.param_field = ParamPanelField::SyncModeToggle;
                                OperateFocus::ParamPanel
                            }
                            _ => OperateFocus::OpRadio,
                        },
                        _ => match self.param_field {
                            ParamPanelField::SecondFlag => {
                                self.param_field = ParamPanelField::Sudo;
                                OperateFocus::ParamPanel
                            }
                            ParamPanelField::Sudo => {
                                self.param_field = ParamPanelField::CommandOrScript;
                                OperateFocus::ParamPanel
                            }
                            ParamPanelField::CommandOrScript => OperateFocus::OpRadio,
                            _ => OperateFocus::ParamPanel,
                        },
                    },
                    OperateFocus::OpRadio => OperateFocus::OpRadio,
                };
                Ok(true)
            }
            KeyCode::Down | KeyCode::Char('j') if self.active_tab == TabId::Operate => {
                self.operate_focus = match self.operate_focus {
                    OperateFocus::OpRadio => {
                        if self.operate_operation == OperationKind::Check {
                            OperateFocus::TargetRow
                        } else {
                            self.param_field = match self.operate_operation {
                                OperationKind::Sync => ParamPanelField::SyncModeToggle,
                                _ => ParamPanelField::CommandOrScript,
                            };
                            OperateFocus::ParamPanel
                        }
                    }
                    OperateFocus::ParamPanel => match self.operate_operation {
                        OperationKind::Sync => match self.param_field {
                            ParamPanelField::SyncModeToggle => {
                                self.param_field = ParamPanelField::SyncAdHocInput;
                                OperateFocus::ParamPanel
                            }
                            ParamPanelField::SyncAdHocInput => {
                                self.param_field = ParamPanelField::SyncDryRun;
                                OperateFocus::ParamPanel
                            }
                            _ => OperateFocus::TargetRow,
                        },
                        _ => match self.param_field {
                            ParamPanelField::CommandOrScript => {
                                self.param_field = ParamPanelField::Sudo;
                                OperateFocus::ParamPanel
                            }
                            ParamPanelField::Sudo => {
                                self.param_field = ParamPanelField::SecondFlag;
                                OperateFocus::ParamPanel
                            }
                            ParamPanelField::SecondFlag => OperateFocus::TargetRow,
                            _ => OperateFocus::TargetRow,
                        },
                    },
                    OperateFocus::TargetRow => {
                        if self.has_entries_panel() {
                            self.entries_scroll = 0;
                            OperateFocus::ApplicableEntries
                        } else {
                            OperateFocus::Execute
                        }
                    }
                    OperateFocus::ApplicableEntries => {
                        let entry_count = self.entries_panel_count();
                        let max_scroll = entry_count.saturating_sub(6);
                        if self.entries_scroll < max_scroll {
                            self.entries_scroll += 1;
                            OperateFocus::ApplicableEntries
                        } else {
                            OperateFocus::Execute
                        }
                    }
                    OperateFocus::Execute => OperateFocus::Execute,
                };
                Ok(true)
            }
            // Left/Right on OpRadio cycles the selected operation.
            KeyCode::Left
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::OpRadio =>
            {
                self.operate_operation = match self.operate_operation {
                    OperationKind::Check => OperationKind::Sync,
                    OperationKind::Run => OperationKind::Check,
                    OperationKind::Exec => OperationKind::Run,
                    OperationKind::Sync => OperationKind::Exec,
                };
                self.save_state();
                Ok(true)
            }
            KeyCode::Right
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::OpRadio =>
            {
                self.operate_operation = match self.operate_operation {
                    OperationKind::Check => OperationKind::Run,
                    OperationKind::Run => OperationKind::Exec,
                    OperationKind::Exec => OperationKind::Sync,
                    OperationKind::Sync => OperationKind::Check,
                };
                self.save_state();
                Ok(true)
            }
            // Enter on ParamPanel command/script field activates it.
            KeyCode::Enter
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::ParamPanel
                    && self.param_field == ParamPanelField::CommandOrScript =>
            {
                match self.operate_operation {
                    OperationKind::Run => self.run_command.activate(),
                    OperationKind::Exec => self.exec_script.activate(),
                    _ => {}
                }
                Ok(true)
            }
            // Space on ParamPanel checkbox fields toggles them.
            KeyCode::Char(' ')
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::ParamPanel =>
            {
                match (self.operate_operation, self.param_field) {
                    (OperationKind::Run, ParamPanelField::Sudo) => {
                        self.run_sudo = !self.run_sudo;
                        self.save_state();
                    }
                    (OperationKind::Run, ParamPanelField::SecondFlag) => {
                        self.run_yes = !self.run_yes;
                        self.save_state();
                    }
                    (OperationKind::Exec, ParamPanelField::Sudo) => {
                        self.exec_sudo = !self.exec_sudo;
                        self.save_state();
                    }
                    (OperationKind::Exec, ParamPanelField::SecondFlag) => {
                        self.exec_keep = !self.exec_keep;
                        self.save_state();
                    }
                    (OperationKind::Sync, ParamPanelField::SyncModeToggle) => {
                        self.sync_mode = match self.sync_mode {
                            SyncMode::ConfigEntries => SyncMode::AdHoc,
                            SyncMode::AdHoc => SyncMode::ConfigEntries,
                        };
                        self.save_state();
                    }
                    (OperationKind::Sync, ParamPanelField::SyncDryRun) => {
                        self.sync_dry_run = !self.sync_dry_run;
                        self.save_state();
                    }
                    _ => {}
                }
                Ok(true)
            }
            // Enter on sync ad-hoc input activates it.
            KeyCode::Enter
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::ParamPanel
                    && self.operate_operation == OperationKind::Sync
                    && self.param_field == ParamPanelField::SyncAdHocInput =>
            {
                self.sync_adhoc_input.activate();
                Ok(true)
            }
            // Delete removes the last item from the sync adhoc file list.
            KeyCode::Delete
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::ParamPanel
                    && self.operate_operation == OperationKind::Sync
                    && self.param_field == ParamPanelField::SyncAdHocInput =>
            {
                self.sync_adhoc_files.pop();
                Ok(true)
            }
            KeyCode::Enter
                if self.active_tab == TabId::Operate
                    && self.operate_focus == OperateFocus::Execute =>
            {
                Ok(match self.operate_operation {
                    OperationKind::Check => self.execute_check(),
                    OperationKind::Run => self.execute_run(),
                    OperationKind::Exec => self.execute_exec(),
                    OperationKind::Sync => self.execute_sync(),
                })
            }
            KeyCode::Char('f') if self.active_tab == TabId::Operate => {
                let popup = FilterPopup::new(self.target_filter.clone(), true, &self.config);
                self.filter_popup = Some(popup);
                Ok(true)
            }

            // ── Checkout tab ───────────────────────────────────────────────
            KeyCode::Char('f') if self.active_tab == TabId::Checkout => {
                let popup = FilterPopup::new(self.target_filter.clone(), false, &self.config);
                self.filter_popup = Some(popup);
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
        if self.log_overlay_open {
            self.render_log_overlay(area, frame);
        }
        if self.auth_popup.is_some() {
            self.render_auth_popup(area, frame);
        }
    }

    fn render_tab_bar(&self, area: Rect, frame: &mut ratatui::Frame) {
        let titles: Vec<&str> = TabId::ALL.iter().map(|t| t.label()).collect();
        let selected = TabId::ALL
            .iter()
            .position(|t| *t == self.active_tab)
            .unwrap_or(0);
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
        self.config_tab.render(
            area,
            frame,
            &self.theme,
            &self.config,
            self.config_path.as_deref(),
        );
    }

    fn save_config(&mut self) {
        if let Some(path) = &self.config_path {
            match crate::config::app::save(&self.config, Some(path)) {
                Ok(()) => {
                    self.config_tab.config_dirty = false;
                    self.config_tab.reload_banner_until =
                        Some(Instant::now() + Duration::from_secs(2));
                    self.config_tab.reload(&self.config, Some(path));
                }
                Err(e) => {
                    self.error = Some(format!("Config save failed: {e}"));
                }
            }
        } else {
            self.error = Some("No config path set — cannot save.".to_string());
        }
    }

    fn config_add_kind(&self) -> Option<super::tabs::config_tab::EntryFormKind> {
        use super::tabs::config_tab::EntryFormKind;
        let item = self
            .config_tab
            .items
            .get(self.config_tab.sidebar_vp.selected);
        match item {
            Some(super::tabs::config_tab::SidebarItem::SectionHosts)
            | Some(super::tabs::config_tab::SidebarItem::Host(_)) => Some(EntryFormKind::Host),
            Some(super::tabs::config_tab::SidebarItem::SectionChecks)
            | Some(super::tabs::config_tab::SidebarItem::Check(_)) => Some(EntryFormKind::Check),
            Some(super::tabs::config_tab::SidebarItem::SectionSyncs)
            | Some(super::tabs::config_tab::SidebarItem::Sync(_)) => Some(EntryFormKind::Sync),
            _ => None,
        }
    }

    fn handle_log_overlay_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('L') => {
                self.log_overlay_open = false;
                Ok(true)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.log_overlay_vp.move_up();
                Ok(true)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.log_overlay_vp.move_down();
                Ok(true)
            }
            KeyCode::PageUp => {
                self.log_overlay_vp.page_up();
                Ok(true)
            }
            KeyCode::PageDown => {
                self.log_overlay_vp.page_down();
                Ok(true)
            }
            KeyCode::Home => {
                self.log_overlay_vp.home();
                Ok(true)
            }
            KeyCode::End => {
                self.log_overlay_vp.end();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn render_log_overlay(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        let popup_area = centered_rect(80, 60, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.warning))
            .title(" Log (L to close) ");
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let buf = match &self.log_buffer {
            Some(b) => b,
            None => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "(log capture not available)",
                        Style::default().fg(self.theme.inactive),
                    )),
                    inner,
                );
                return;
            }
        };

        let entries = buf.snapshot();
        let visible_h = inner.height as usize;
        self.log_overlay_vp.set_dims(entries.len(), visible_h);

        if entries.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "(no log entries yet)",
                    Style::default().fg(self.theme.inactive),
                )),
                inner,
            );
            return;
        }

        let (start, end) = self.log_overlay_vp.visible_range();
        let lines: Vec<Line> = entries[start..end.min(entries.len())]
            .iter()
            .enumerate()
            .map(|(rel, entry)| {
                let abs = start + rel;
                let is_sel = abs == self.log_overlay_vp.selected;
                let level_color = match entry.level.as_str() {
                    "ERROR" => self.theme.error,
                    "WARN" => self.theme.warning,
                    "INFO" => self.theme.accent_checkout,
                    _ => self.theme.inactive,
                };
                let style = if is_sel {
                    Style::default()
                        .fg(level_color)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default().fg(level_color)
                };
                let prefix = if is_sel { "▶ " } else { "  " };
                let text = trunc(
                    &format!(
                        "{}{:5} {} {}",
                        prefix, entry.level, entry.target, entry.text
                    ),
                    inner.width as usize,
                );
                Line::from(Span::styled(text, style))
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_auth_popup(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        use ratatui::style::Color;
        let popup_area = centered_rect(60, 30, area);
        frame.render_widget(Clear, popup_area);

        let popup = match &self.auth_popup {
            Some(p) => p,
            None => return,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" SSH Authentication ");
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let chunks = ratatui::layout::Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(inner);

        let prompt_text = Paragraph::new(popup.prompt.as_str())
            .style(Style::default().fg(self.theme.inactive))
            .wrap(Wrap { trim: true });
        frame.render_widget(prompt_text, chunks[0]);

        // Render masked input (show '*' for each character).
        let masked: String = "*".repeat(popup.input.value.chars().count());
        let masked_field = InputField::new(&masked);
        masked_field.render(frame, chunks[1], "Credential", true);

        let hint = Paragraph::new(Span::styled(
            "Enter to confirm · Esc to cancel",
            Style::default().fg(self.theme.inactive),
        ));
        frame.render_widget(hint, chunks[2]);
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
        let new_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

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
                    self.config_tab.reload_banner_until =
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
        let target_count = match resolve_target_names(
            &build_target_mode(&self.target_filter, &self.config),
            &self.config,
        ) {
            Ok(t) => t.len(),
            Err(_) => 0,
        };
        let data = OperateRenderData {
            focus: self.operate_focus,
            operation: self.operate_operation,
            sync_mode: self.sync_mode,
            sync_dry_run: self.sync_dry_run,
            sync_adhoc_files: &self.sync_adhoc_files,
            sync_adhoc_input: &self.sync_adhoc_input,
            run_command: &self.run_command,
            exec_script: &self.exec_script,
            run_sudo: self.run_sudo,
            run_yes: self.run_yes,
            exec_sudo: self.exec_sudo,
            exec_keep: self.exec_keep,
            param_field: self.param_field,
            entries_scroll: self.entries_scroll,
            config: &self.config,
            theme: &self.theme,
            is_running: self.running_op.is_some(),
            target_filter: &self.target_filter,
            target_count,
        };
        operate_tab::render_operate(&data, area, frame);
    }

    fn render_progress_popup(&self, area: Rect, frame: &mut ratatui::Frame) {
        let Some(op) = &self.running_op else {
            return;
        };
        let op_name = match self.operate_operation {
            OperationKind::Check => "check",
            OperationKind::Run => "run",
            OperationKind::Exec => "exec",
            OperationKind::Sync => "sync",
        };
        operate_tab::render_progress_popup(
            &self.theme,
            op_name,
            &op.host_outcomes,
            &op.targets,
            op.started_at.elapsed().as_secs(),
            op.completed_count(),
            self.progress_popup_scroll,
            area,
            frame,
        );
    }

    fn render_results_popup(&self, area: Rect, frame: &mut ratatui::Frame, report: &CommandReport) {
        let popup_area = centered_rect(75, 75, area);
        frame.render_widget(Clear, popup_area);

        // Extract common fields for any variant.
        let (host_count, executed_at, header_detail): (usize, &str, String) = match report {
            CommandReport::Check(r) => (r.hosts.len(), r.executed_at.as_str(), String::new()),
            CommandReport::Run(r) => (
                r.hosts.len(),
                r.executed_at.as_str(),
                format!("  cmd: {}", truncate(&r.command, 50)),
            ),
            CommandReport::Exec(r) => (
                r.hosts.len(),
                r.executed_at.as_str(),
                format!("  script: {}", truncate(&r.script, 50)),
            ),
            CommandReport::Sync(r) => (
                r.hosts.len(),
                r.executed_at.as_str(),
                format!(
                    "  mode:{} dry-run:{} total_synced:{}",
                    r.mode, r.dry_run, r.total_files_synced
                ),
            ),
        };

        let title = format!(" Results — {host_count} hosts  (Enter / Esc to dismiss) ");
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(title);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let mut lines: Vec<Line> = Vec::new();

        // Helper closure to render a per-host row.
        let render_row = |host: &str, status: HostStatus, detail: &str, ms: Option<u64>| {
            let glyph = match status {
                HostStatus::Online => "✓",
                HostStatus::Partial => "⚠",
                HostStatus::Offline | HostStatus::Error => "✗",
                HostStatus::Unreachable => "⊘",
                HostStatus::TimedOut => "⏱",
                HostStatus::Skipped => "⊘",
            };
            let color = match status {
                HostStatus::Online => self.theme.accent_checkout,
                HostStatus::Partial => self.theme.warning,
                HostStatus::Skipped => self.theme.inactive,
                _ => self.theme.error,
            };
            let ms_val = ms.unwrap_or(0);
            let line = format!(
                "  {} {:<16} ({:>4}ms) — {}",
                glyph,
                truncate(host, 16),
                ms_val,
                truncate(detail, 80),
            );
            Line::from(Span::styled(line, Style::default().fg(color)))
        };

        match report {
            CommandReport::Check(r) => {
                let ok = r
                    .hosts
                    .iter()
                    .filter(|h| matches!(h.status, HostStatus::Online | HostStatus::Partial))
                    .count();
                let fail = r.hosts.len() - ok;
                lines.push(Line::from(format!(
                    "Summary: {ok} ok / {fail} fail    Executed: {executed_at}"
                )));
                lines.push(Line::from(""));
                for h in &r.hosts {
                    lines.push(render_row(&h.host, h.status, &h.detail, h.duration_ms));
                }
            }
            CommandReport::Run(r) => {
                let ok = r
                    .hosts
                    .iter()
                    .filter(|h| h.status == HostStatus::Online)
                    .count();
                let fail = r.hosts.len() - ok;
                lines.push(Line::from(format!(
                    "Summary: {ok} ok / {fail} fail    Executed: {executed_at}"
                )));
                lines.push(Line::from(header_detail));
                lines.push(Line::from(""));
                for h in &r.hosts {
                    lines.push(render_row(&h.host, h.status, &h.detail, h.duration_ms));
                    // Show first line of stdout for context.
                    if !h.stdout.is_empty() {
                        let first = h.stdout.lines().next().unwrap_or("").trim();
                        if !first.is_empty() {
                            lines.push(Line::from(format!("     ↳ {}", truncate(first, 70))));
                        }
                    }
                }
            }
            CommandReport::Exec(r) => {
                let ok = r
                    .hosts
                    .iter()
                    .filter(|h| h.status == HostStatus::Online)
                    .count();
                let skipped = r
                    .hosts
                    .iter()
                    .filter(|h| h.status == HostStatus::Skipped)
                    .count();
                let fail = r.hosts.len() - ok - skipped;
                lines.push(Line::from(format!(
                    "Summary: {ok} ok / {fail} fail / {skipped} skipped    Executed: {executed_at}"
                )));
                lines.push(Line::from(header_detail));
                lines.push(Line::from(""));
                for h in &r.hosts {
                    lines.push(render_row(&h.host, h.status, &h.detail, h.duration_ms));
                    if !h.stdout.is_empty() {
                        let first = h.stdout.lines().next().unwrap_or("").trim();
                        if !first.is_empty() {
                            lines.push(Line::from(format!("     ↳ {}", truncate(first, 70))));
                        }
                    }
                }
            }
            CommandReport::Sync(r) => {
                let ok = r
                    .hosts
                    .iter()
                    .filter(|h| matches!(h.status, HostStatus::Online))
                    .count();
                let fail = r
                    .hosts
                    .iter()
                    .filter(|h| !matches!(h.status, HostStatus::Online))
                    .count();
                lines.push(Line::from(format!(
                    "Summary: {ok} ok / {fail} fail    synced:{} skipped:{}    Executed: {executed_at}",
                    r.total_files_synced, r.total_files_skipped
                )));
                lines.push(Line::from(header_detail));
                lines.push(Line::from(""));
                for h in &r.hosts {
                    let detail = if h.files_synced > 0 || h.files_skipped > 0 {
                        format!("{} synced, {} skipped", h.files_synced, h.files_skipped)
                    } else {
                        h.detail.clone()
                    };
                    lines.push(render_row(&h.host, h.status, &detail, h.duration_ms));
                    for err in h.errors.iter().take(2) {
                        lines.push(Line::from(format!("     ↳ {}", truncate(err, 70))));
                    }
                }
            }
        }

        let p = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(p, inner);
    }

    fn render_info_popup(&self, area: Rect, frame: &mut ratatui::Frame) {
        let popup_area = centered_rect(60, 50, area);
        frame.render_widget(Clear, popup_area);
        let body = match self.active_tab {
            TabId::Operate => format!(
                "Operate tab\n\nSelect an operation with ← → on the Operation row.\n\ncheck — collect host metrics and write to DB.\nrun   — execute a shell command on all targets.\nexec  — upload and run a local script on targets.\nsync  — sync files between hosts (Phase 6).\n\nUse `f` to change the target filter; press Enter on [Execute] to run.\nEsc cancels a running operation (may take up to {}s per host).\n\nResults appear in a popup when the operation completes.",
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

        let mut header_cells: Vec<Cell> = vec![Cell::from("Host"), Cell::from("Status")];
        let mut constraints: Vec<Constraint> = vec![Constraint::Length(16), Constraint::Length(12)];
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
            let status_text = if snap.online {
                "✓ online"
            } else {
                "✗ offline"
            };
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

        let table = Table::new(rows, &constraints)
            .header(Row::new(header_cells).style(Style::default().add_modifier(Modifier::BOLD)));
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
            TabId::Config => {
                "↑↓:Rows ←→:Zones e:Edit E:Editor S:Save a:Add d:Del L:Log i:Info ?:Help q:Quit"
            }
            TabId::Operate => "↑↓:Zones ←→:OpType f:Filter L:Log i:Info ?:Help q:Quit",
            TabId::Checkout => "↑↓/jk:Rows PgUp/PgDn Home/End f:Filter L:Log i:Info ?:Help q:Quit",
        };
        let p = Paragraph::new(Line::from(vec![Span::styled(
            hints,
            Style::default().fg(self.theme.inactive),
        )]));
        frame.render_widget(p, chunks[1]);
    }

    fn render_help_popup(&self, area: Rect, frame: &mut ratatui::Frame) {
        let popup_area = centered_rect(60, 70, area);
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
  L           Toggle log overlay
  i           Toggle contextual info popup

Operate tab
  ↑↓ / j k   Navigate zones: OpRadio → ParamPanel → TargetRow → Execute
  ← →         (OpRadio) cycle check / run / exec / sync
  f           Open Target Filter popup
  Enter       (ParamPanel text field) activate input; (Execute) run operation
  Space       (checkbox) toggle sudo / yes / keep / dry-run / sync-mode
  Del         (SyncAdHocInput focused) remove last ad-hoc path
  Esc         Dismiss results popup / cancel running operation
  (while typing) Enter to confirm, Esc to revert

Sync operation (ParamPanel)
  Space on Mode toggle   Switch between Config-entries ↔ Ad-hoc
  Enter on Ad-hoc input  Add typed path to the list
  Del on Ad-hoc input    Remove last path from the list
  Space on Dry-run       Toggle dry-run flag

Checkout tab
  ↑↓ / j k    Move row selection
  PgUp/PgDn   Page navigation
  Home/End    Jump to top / bottom
  f           Open Filter popup (filter by group / host)

Filter popup
  ↑↓ / Tab    Move between fields
  Space/Enter Toggle / select
  Enter on [Apply]   Commit + persist filter
  Esc                Cancel

Config tab
  ↑↓ / j k    Move sidebar / field rows
  ← / →       Switch zones (Sidebar ↔ FieldTable)
  Tab         Switch zone (Sidebar → FieldTable)
  PgUp/PgDn   Page navigation
  Home/End    Jump to top / bottom
  e / Enter   Edit focused field inline (scalar / form)
  a           Add new entry (host / check / sync)
  d           Delete focused entry
  S           Save config (format-preserving via toml_edit)
  E           Open config in $VISUAL/$EDITOR (TUI suspends, reloads on change)

Log overlay
  L           Open / close
  ↑↓ / j k    Scroll
  PgUp/PgDn   Page navigation
  Home/End    Jump to top / bottom
";
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_active))
            .title(" Keybindings (?) ");
        let p = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
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
