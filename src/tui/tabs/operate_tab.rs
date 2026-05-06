//! Operate tab — operation selection, parameter panels, target filter, execution.
//!
//! Phase 5 scope: check/run/exec/sync operations with param panels.
//! Phase 3 adds: applicable entries panel with scroll, ad-hoc banner, conflict detection.

use std::collections::HashMap;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::commands::report::HostStatus;
use crate::config::schema::{AppConfig, SyncEntry};

use super::super::components::input_field::InputField;
use super::super::state::persist::{OperationKind, SyncMode, TargetFilterMode, TargetFilterState};
use super::super::theme::Theme;

/// Operate-tab focused element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperateFocus {
    OpRadio,
    ParamPanel,
    TargetRow,
    /// Read-only applicable entries panel (check/sync config-entries mode).
    ApplicableEntries,
    Execute,
}

/// Which field in the param panel has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamPanelField {
    #[default]
    CommandOrScript,
    Sudo,
    SecondFlag,
    SyncModeToggle,
    SyncAdHocInput,
    SyncDryRun,
}

/// Rendering data for the Operate tab, passed from App.
pub struct OperateRenderData<'a> {
    pub focus: OperateFocus,
    pub operation: OperationKind,
    pub sync_mode: SyncMode,
    pub sync_dry_run: bool,
    pub sync_adhoc_files: &'a [String],
    pub sync_adhoc_input: &'a InputField,
    pub run_command: &'a InputField,
    pub exec_script: &'a InputField,
    pub run_sudo: bool,
    pub run_yes: bool,
    pub exec_sudo: bool,
    pub exec_keep: bool,
    pub param_field: ParamPanelField,
    pub entries_scroll: usize,
    pub config: &'a AppConfig,
    pub theme: &'a Theme,
    pub is_running: bool,
    pub target_filter: &'a TargetFilterState,
    pub target_count: usize,
}

/// Render the entire Operate tab.
pub fn render_operate(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(data.theme.border_active))
        .title(" Operate ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Ad-hoc mode banner (B3 acceptance test).
    let show_adhoc_banner = data.sync_mode == SyncMode::AdHoc;
    let banner_rows = if show_adhoc_banner { 1u16 } else { 0 };
    if show_adhoc_banner {
        let banner_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " ⚡ Ad-hoc mode active",
                    Style::default()
                        .fg(data.theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " — sync will use ad-hoc file list, not [[sync]] config entries",
                    Style::default().fg(data.theme.inactive),
                ),
            ])),
            banner_area,
        );
    }
    let inner_after_banner = Rect {
        x: inner.x,
        y: inner.y + banner_rows,
        width: inner.width,
        height: inner.height.saturating_sub(banner_rows),
    };

    let has_params = matches!(
        data.operation,
        OperationKind::Run | OperationKind::Exec | OperationKind::Sync
    );
    let param_rows = match data.operation {
        OperationKind::Run | OperationKind::Exec => 7u16,
        OperationKind::Sync => {
            let adhoc_list_rows = data.sync_adhoc_files.len().min(5) as u16;
            8 + adhoc_list_rows
        }
        _ => 0u16,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),          // OpRadio row
            Constraint::Length(param_rows), // param panel (0 for check)
            Constraint::Length(4),          // target row
            Constraint::Min(0),             // applicable entries / spacer
            Constraint::Length(3),          // execute button
        ])
        .split(inner_after_banner);

    render_op_radio(data, chunks[0], frame);

    if has_params {
        match data.operation {
            OperationKind::Run | OperationKind::Exec => {
                render_run_exec_params(data, chunks[1], frame);
            }
            OperationKind::Sync => {
                render_sync_params(data, chunks[1], frame);
            }
            _ => {}
        }
    }

    // Target row.
    render_target_row(data, chunks[2], frame);

    // Applicable entries panel.
    render_applicable_entries(data, chunks[3], frame);

    // Execute button.
    render_execute_button(data, chunks[4], frame);
}

fn render_op_radio(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let radio_focused = data.focus == OperateFocus::OpRadio;
    let ops = [
        (OperationKind::Check, "check"),
        (OperationKind::Run, "run"),
        (OperationKind::Exec, "exec"),
        (OperationKind::Sync, "sync"),
    ];
    let mut spans = vec![Span::raw(" Operation: ")];
    for (kind, label) in &ops {
        let selected = *kind == data.operation;
        let (prefix, _suffix) = if selected { ("◉ ", "") } else { ("○ ", "") };
        let style = if selected && radio_focused {
            Style::default()
                .fg(data.theme.accent_operate)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if selected {
            Style::default()
                .fg(data.theme.accent_operate)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(data.theme.inactive)
        };
        spans.push(Span::styled(format!("[{prefix}{label}]"), style));
        spans.push(Span::raw("  "));
    }
    if radio_focused {
        spans.push(Span::styled(
            " ← → to change",
            Style::default().fg(data.theme.inactive),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_run_exec_params(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let param_focused = data.focus == OperateFocus::ParamPanel;
    let param_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // text input
            Constraint::Length(1), // sudo
            Constraint::Length(1), // yes / keep
            Constraint::Min(0),
        ])
        .split(area);

    let field_focused = param_focused && data.param_field == ParamPanelField::CommandOrScript;
    let (field_label, field) = match data.operation {
        OperationKind::Run => ("Command", data.run_command),
        OperationKind::Exec => ("Script path", data.exec_script),
        _ => return,
    };
    field.render(frame, param_chunks[0], field_label, field_focused);

    let sudo_focused = param_focused && data.param_field == ParamPanelField::Sudo;
    let sudo_val = match data.operation {
        OperationKind::Run => data.run_sudo,
        OperationKind::Exec => data.exec_sudo,
        _ => false,
    };
    let sudo_style = if sudo_focused {
        Style::default()
            .fg(data.theme.accent_operate)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(if sudo_val { "[✓] sudo" } else { "[ ] sudo" }, sudo_style),
            Span::styled(
                "  (Space to toggle)",
                Style::default().fg(data.theme.inactive),
            ),
        ])),
        param_chunks[1],
    );

    let flag2_focused = param_focused && data.param_field == ParamPanelField::SecondFlag;
    let (flag2_label, flag2_val) = match data.operation {
        OperationKind::Run => ("--yes (skip confirmation)", data.run_yes),
        OperationKind::Exec => ("--keep (keep script after exec)", data.exec_keep),
        _ => ("", false),
    };
    let flag2_style = if flag2_focused {
        Style::default()
            .fg(data.theme.accent_operate)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                if flag2_val {
                    format!("[✓] {flag2_label}")
                } else {
                    format!("[ ] {flag2_label}")
                },
                flag2_style,
            ),
        ])),
        param_chunks[2],
    );
}

fn render_sync_params(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let param_focused = data.focus == OperateFocus::ParamPanel;

    let mode_focused = param_focused && data.param_field == ParamPanelField::SyncModeToggle;
    let mode_label = match data.sync_mode {
        SyncMode::ConfigEntries => "◉ Config entries  ○ Ad-hoc files",
        SyncMode::AdHoc => "○ Config entries  ◉ Ad-hoc files",
    };
    let mode_style = if mode_focused {
        Style::default()
            .fg(data.theme.accent_operate)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    let adhoc_input_focused = param_focused && data.param_field == ParamPanelField::SyncAdHocInput;
    let dry_run_focused = param_focused && data.param_field == ParamPanelField::SyncDryRun;

    let adhoc_list_rows = data.sync_adhoc_files.len().min(5) as u16;
    let sync_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(adhoc_list_rows.max(1)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  Mode: "),
            Span::styled(mode_label, mode_style),
            Span::styled(
                "  (Space to toggle)",
                Style::default().fg(data.theme.inactive),
            ),
        ])),
        sync_chunks[0],
    );

    if data.sync_mode == SyncMode::AdHoc {
        data.sync_adhoc_input.render(
            frame,
            sync_chunks[2],
            "Add path (Enter=add, Del=remove last)",
            adhoc_input_focused,
        );
        let list_lines: Vec<Line> = if data.sync_adhoc_files.is_empty() {
            vec![Line::from(Span::styled(
                "  (no paths — type above and press Enter)",
                Style::default().fg(data.theme.inactive),
            ))]
        } else {
            data.sync_adhoc_files
                .iter()
                .rev()
                .take(5)
                .map(|p| Line::from(format!("  · {p}")))
                .collect()
        };
        frame.render_widget(Paragraph::new(list_lines), sync_chunks[3]);
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  (using [[sync]] config entries)",
                Style::default().fg(data.theme.inactive),
            )),
            sync_chunks[2],
        );
    }

    let dry_style = if dry_run_focused {
        Style::default()
            .fg(data.theme.accent_operate)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                if data.sync_dry_run {
                    "[✓] dry-run"
                } else {
                    "[ ] dry-run"
                },
                dry_style,
            ),
            Span::styled(
                "  (Space to toggle)",
                Style::default().fg(data.theme.inactive),
            ),
        ])),
        sync_chunks[4],
    );
}

fn render_applicable_entries(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let entries_focused = data.focus == OperateFocus::ApplicableEntries;
    let page_size = 6usize;

    if data.operation == OperationKind::Check {
        let mut lines: Vec<Line> = Vec::new();
        let total = data.config.check.len();
        let scroll = data.entries_scroll.min(total.saturating_sub(page_size));
        let scroll_hint = if total > page_size {
            format!(
                " [{}-{}/{}] ↑↓",
                scroll + 1,
                (scroll + page_size).min(total),
                total
            )
        } else {
            String::new()
        };
        let header_style = if entries_focused {
            Style::default()
                .fg(data.theme.accent_operate)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(data.theme.inactive)
        };
        lines.push(Line::from(Span::styled(
            format!("─ Applicable [[check]] entries ─{scroll_hint}"),
            header_style,
        )));
        let conflicts = detect_sync_source_conflicts(&data.config.sync);
        if !conflicts.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ⚠ Source conflict: {} [[sync]] entries share the same source",
                    conflicts.len()
                ),
                Style::default().fg(data.theme.warning),
            )));
        }
        if data.config.check.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no [[check]] entries — add one to config.toml)",
                Style::default().fg(data.theme.inactive),
            )));
        } else {
            for (i, entry) in data
                .config
                .check
                .iter()
                .enumerate()
                .skip(scroll)
                .take(page_size)
            {
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
        }
        let panel = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(panel, area);
    } else if data.operation == OperationKind::Sync && data.sync_mode == SyncMode::ConfigEntries {
        let mut lines: Vec<Line> = Vec::new();
        let total = data.config.sync.len();
        let scroll = data.entries_scroll.min(total.saturating_sub(page_size));
        let scroll_hint = if total > page_size {
            format!(
                " [{}-{}/{}] ↑↓",
                scroll + 1,
                (scroll + page_size).min(total),
                total
            )
        } else {
            String::new()
        };
        let header_style = if entries_focused {
            Style::default()
                .fg(data.theme.accent_operate)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(data.theme.inactive)
        };
        lines.push(Line::from(Span::styled(
            format!("─ Applicable [[sync]] entries ─{scroll_hint}"),
            header_style,
        )));
        let conflicts = detect_sync_source_conflicts(&data.config.sync);
        if !conflicts.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ⚠ Source conflict: {} entries share the same source path",
                    conflicts.len()
                ),
                Style::default().fg(data.theme.warning),
            )));
        }
        if data.config.sync.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no [[sync]] entries — add one to config.toml)",
                Style::default().fg(data.theme.inactive),
            )));
        } else {
            for entry in data.config.sync.iter().skip(scroll).take(page_size) {
                let paths = entry
                    .paths
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let groups = if entry.groups.is_empty() {
                    "unscoped".to_string()
                } else {
                    format!("groups:[{}]", entry.groups.join(","))
                };
                let src = entry
                    .source
                    .as_deref()
                    .map(|s| format!("  src:{s}"))
                    .unwrap_or_default();
                lines.push(Line::from(format!("  ▸ {}  {}{}", paths, groups, src)));
            }
        }
        let panel = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(panel, area);
    }
}

fn render_execute_button(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let execute_focused = data.focus == OperateFocus::Execute;
    let op_name = match data.operation {
        OperationKind::Check => "check",
        OperationKind::Run => "run",
        OperationKind::Exec => "exec",
        OperationKind::Sync => "sync",
    };
    let exec_label = if data.is_running {
        " [ running... — Esc to cancel ] ".to_string()
    } else {
        format!(" [ Execute {op_name} (Enter) ] ")
    };
    let exec_style = if execute_focused && !data.is_running {
        Style::default()
            .fg(data.theme.accent_operate)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else if data.is_running {
        Style::default().fg(data.theme.warning)
    } else {
        Style::default().fg(data.theme.inactive)
    };
    let exec = Paragraph::new(Line::from(Span::styled(exec_label, exec_style)))
        .block(Block::default().borders(Borders::TOP));
    frame.render_widget(exec, area);
}

fn render_target_row(data: &OperateRenderData, area: Rect, frame: &mut Frame) {
    let target_focused = data.focus == OperateFocus::TargetRow;
    let mode_summary = match data.target_filter.mode {
        TargetFilterMode::All => "all hosts".to_string(),
        TargetFilterMode::Groups => format!("groups:{}", data.target_filter.groups.join(",")),
        TargetFilterMode::Hosts => format!("hosts:{}", data.target_filter.hosts.join(",")),
        TargetFilterMode::Shell => format!("shell:{:?}", data.target_filter.shell),
    };
    let target_text = format!(
        " Target: {}  ({} hosts)    [f] Filter   serial={}   timeout={}s",
        mode_summary, data.target_count, data.target_filter.serial, data.target_filter.timeout,
    );
    let target_p = Paragraph::new(target_text).style(if target_focused {
        Style::default()
            .fg(data.theme.accent_operate)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    });
    frame.render_widget(target_p, area);
}

/// Render the progress popup showing running operation status.
#[allow(clippy::too_many_arguments)]
pub fn render_progress_popup(
    theme: &Theme,
    op_name: &str,
    host_outcomes: &[(String, HostStatus, String, u64)],
    targets: &[String],
    elapsed_secs: u64,
    completed_count: usize,
    progress_scroll: Option<usize>,
    area: Rect,
    frame: &mut Frame,
) {
    use super::super::components::popup::centered_rect;

    let popup_area = centered_rect(70, 70, area);
    frame.render_widget(ratatui::widgets::Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(format!(" Running {op_name} — Esc to cancel "));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines: Vec<Line> = Vec::new();
    let total_outcomes = host_outcomes.len();
    let take = 12usize;
    let auto_start = total_outcomes.saturating_sub(take);
    let start = progress_scroll.unwrap_or(auto_start).min(auto_start);
    let scroll_hint = if total_outcomes > take {
        format!("  [{}/{}] ↑↓ scroll", start + 1, total_outcomes)
    } else {
        String::new()
    };
    lines.push(Line::from(format!(
        "Targets: {}    Completed: {}    Elapsed: {}s{}",
        targets.len(),
        completed_count,
        elapsed_secs,
        scroll_hint,
    )));
    lines.push(Line::from(""));

    for (host, status, detail, ms) in &host_outcomes[start..(start + take).min(total_outcomes)] {
        let glyph = match status {
            HostStatus::Online => "✓",
            HostStatus::Partial => "⚠",
            HostStatus::Offline => "✗",
            HostStatus::Unreachable => "⊘",
            HostStatus::TimedOut => "⏱",
            HostStatus::Error => "✗",
            HostStatus::Skipped => "⊘",
        };
        let color = match status {
            HostStatus::Online => theme.accent_checkout,
            HostStatus::Partial => theme.warning,
            HostStatus::Skipped => theme.inactive,
            _ => theme.error,
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

/// Detect sync entries that share the same (non-empty) source path.
pub fn detect_sync_source_conflicts(sync_entries: &[SyncEntry]) -> Vec<String> {
    let mut source_counts: HashMap<&str, usize> = HashMap::new();
    for entry in sync_entries {
        if let Some(src) = entry.source.as_deref() {
            if !src.is_empty() {
                *source_counts.entry(src).or_insert(0) += 1;
            }
        }
    }
    source_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(src, _)| src.to_string())
        .collect()
}

pub fn truncate(s: &str, max: usize) -> String {
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
