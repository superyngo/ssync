//! Config tab — read-only 3-level browser (section → entry → field) + external editor.
//! Phase 4 scope: display only; inline editing lands in Phase 7.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::config::schema::{AppConfig, CheckEntry, HostEntry, Settings, SyncEntry};

use super::super::components::viewport::Viewport;
use super::super::theme::Theme;

/// Which pane has keyboard focus on the Config tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigZone {
    Sidebar,
    FieldTable,
}

/// A single selectable row in the flattened sidebar list.
#[derive(Debug, Clone)]
enum SidebarItem {
    SectionSettings,
    SectionHosts,
    Host(usize),
    SectionChecks,
    Check(usize),
    SectionSyncs,
    Sync(usize),
}

pub struct ConfigTabState {
    pub zone: ConfigZone,
    items: Vec<SidebarItem>,
    sidebar_vp: Viewport,
    field_vp: Viewport,
    /// Flash banner expiry. Set to `Instant::now() + 2s` after a config reload.
    pub reload_banner_until: Option<Instant>,
    /// mtime of the config file at the last load, used to detect external changes.
    pub config_mtime: Option<std::time::SystemTime>,
}

impl ConfigTabState {
    pub fn new(config: &AppConfig, config_path: Option<&std::path::Path>) -> Self {
        let items = build_sidebar_items(config);
        let mut sidebar_vp = Viewport::new();
        sidebar_vp.set_dims(items.len(), 0);

        let config_mtime = config_path
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());

        Self {
            zone: ConfigZone::Sidebar,
            items,
            sidebar_vp,
            field_vp: Viewport::new(),
            reload_banner_until: None,
            config_mtime,
        }
    }

    /// Rebuild sidebar from an updated config and reset field cursor.
    pub fn reload(&mut self, config: &AppConfig, config_path: Option<&std::path::Path>) {
        self.items = build_sidebar_items(config);
        // Clamp sidebar cursor to new length.
        let new_len = self.items.len();
        let old_sel = self.sidebar_vp.selected;
        self.sidebar_vp = Viewport::new();
        self.sidebar_vp.set_dims(new_len, 0);
        if old_sel < new_len {
            // Restore cursor position if still valid.
            for _ in 0..old_sel {
                self.sidebar_vp.move_down();
            }
        }
        self.field_vp = Viewport::new();
        self.config_mtime = config_path
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    }

    /// Build the breadcrumb string for the current focus position.
    pub fn breadcrumb(&self, config: &AppConfig) -> String {
        match self.items.get(self.sidebar_vp.selected) {
            None => "Config".to_string(),
            Some(SidebarItem::SectionSettings) => {
                if self.zone == ConfigZone::FieldTable {
                    let fields = settings_fields(&config.settings);
                    let name = fields
                        .get(self.field_vp.selected)
                        .map(|(k, _)| k.as_str())
                        .unwrap_or("?");
                    format!("Config > Settings > {name}")
                } else {
                    "Config > Settings".to_string()
                }
            }
            Some(SidebarItem::SectionHosts) => "Config > Hosts".to_string(),
            Some(SidebarItem::Host(i)) => {
                let name = config.host.get(*i).map(|h| h.name.as_str()).unwrap_or("?");
                if self.zone == ConfigZone::FieldTable {
                    let fields = host_fields(&config.host[*i]);
                    let fname = fields
                        .get(self.field_vp.selected)
                        .map(|(k, _)| k.as_str())
                        .unwrap_or("?");
                    format!("Config > Hosts > {name} > {fname}")
                } else {
                    format!("Config > Hosts > {name}")
                }
            }
            Some(SidebarItem::SectionChecks) => "Config > Checks".to_string(),
            Some(SidebarItem::Check(i)) => {
                let label = entry_label_check(config, *i);
                if self.zone == ConfigZone::FieldTable {
                    let fields = check_fields(&config.check[*i]);
                    let fname = fields
                        .get(self.field_vp.selected)
                        .map(|(k, _)| k.as_str())
                        .unwrap_or("?");
                    format!("Config > Checks > {label} > {fname}")
                } else {
                    format!("Config > Checks > {label}")
                }
            }
            Some(SidebarItem::SectionSyncs) => "Config > Syncs".to_string(),
            Some(SidebarItem::Sync(i)) => {
                let label = entry_label_sync(config, *i);
                if self.zone == ConfigZone::FieldTable {
                    let fields = sync_fields(&config.sync[*i]);
                    let fname = fields
                        .get(self.field_vp.selected)
                        .map(|(k, _)| k.as_str())
                        .unwrap_or("?");
                    format!("Config > Syncs > {label} > {fname}")
                } else {
                    format!("Config > Syncs > {label}")
                }
            }
        }
    }

    /// Handle a keypress. Returns `true` if state changed and a redraw is needed.
    /// Returns `None` if the key was not consumed (caller handles it).
    pub fn handle_key(&mut self, key: KeyEvent, config: &AppConfig) -> bool {
        match self.zone {
            ConfigZone::Sidebar => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.sidebar_vp.move_up();
                    self.reset_field_vp(config);
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.sidebar_vp.move_down();
                    self.reset_field_vp(config);
                    true
                }
                KeyCode::PageUp => {
                    self.sidebar_vp.page_up();
                    self.reset_field_vp(config);
                    true
                }
                KeyCode::PageDown => {
                    self.sidebar_vp.page_down();
                    self.reset_field_vp(config);
                    true
                }
                KeyCode::Home => {
                    self.sidebar_vp.home();
                    self.reset_field_vp(config);
                    true
                }
                KeyCode::End => {
                    self.sidebar_vp.end();
                    self.reset_field_vp(config);
                    true
                }
                KeyCode::Right | KeyCode::Tab => {
                    self.zone = ConfigZone::FieldTable;
                    true
                }
                _ => false,
            },
            ConfigZone::FieldTable => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.field_vp.move_up();
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.field_vp.move_down();
                    true
                }
                KeyCode::PageUp => {
                    self.field_vp.page_up();
                    true
                }
                KeyCode::PageDown => {
                    self.field_vp.page_down();
                    true
                }
                KeyCode::Home => {
                    self.field_vp.home();
                    true
                }
                KeyCode::End => {
                    self.field_vp.end();
                    true
                }
                KeyCode::Left | KeyCode::BackTab => {
                    self.zone = ConfigZone::Sidebar;
                    true
                }
                _ => false,
            },
        }
    }

    /// Render the full Config tab into `area`. Called by `App::render_config`.
    pub fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame,
        theme: &Theme,
        config: &AppConfig,
        config_path: Option<&std::path::Path>,
    ) {
        let banner_active = self
            .reload_banner_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false);
        let banner_h: u16 = if banner_active { 1 } else { 0 };

        // Vertical layout: [banner?] [main panes] [breadcrumb]
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(banner_h),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);

        if banner_active {
            let p = Paragraph::new(Span::styled(
                "  ✓ Config reloaded",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ));
            frame.render_widget(p, vert[0]);
        }

        // Horizontal split: sidebar (20 cols) | field table
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(22), Constraint::Min(0)])
            .split(vert[1]);

        self.render_sidebar(horiz[0], frame, theme, config);
        self.render_field_table(horiz[1], frame, theme, config);

        // Breadcrumb line at bottom of tab area.
        let crumb = self.breadcrumb(config);
        let path_hint = config_path
            .map(|p| format!("  [{}]", p.display()))
            .unwrap_or_default();
        let crumb_line = Line::from(vec![
            Span::styled(crumb, Style::default().fg(theme.inactive)),
            Span::styled(path_hint, Style::default().fg(theme.border_inactive)),
        ]);
        frame.render_widget(Paragraph::new(crumb_line), vert[2]);
    }

    // ── private helpers ──────────────────────────────────────────────────────

    /// Reset field viewport when sidebar selection changes.
    fn reset_field_vp(&mut self, config: &AppConfig) {
        let count = self.current_fields(config).len();
        self.field_vp = Viewport::new();
        self.field_vp.set_dims(count, 0);
    }

    fn current_fields(&self, config: &AppConfig) -> Vec<(String, String)> {
        match self.items.get(self.sidebar_vp.selected) {
            None => vec![],
            Some(SidebarItem::SectionSettings) => settings_fields(&config.settings),
            Some(SidebarItem::SectionHosts) => {
                if config.host.is_empty() {
                    vec![]
                } else {
                    vec![("hosts".to_string(), format!("{} configured", config.host.len()))]
                }
            }
            Some(SidebarItem::Host(i)) => {
                config.host.get(*i).map(|h| host_fields(h)).unwrap_or_default()
            }
            Some(SidebarItem::SectionChecks) => {
                if config.check.is_empty() {
                    vec![]
                } else {
                    vec![("checks".to_string(), format!("{} configured", config.check.len()))]
                }
            }
            Some(SidebarItem::Check(i)) => {
                config.check.get(*i).map(|c| check_fields(c)).unwrap_or_default()
            }
            Some(SidebarItem::SectionSyncs) => {
                if config.sync.is_empty() {
                    vec![]
                } else {
                    vec![("syncs".to_string(), format!("{} configured", config.sync.len()))]
                }
            }
            Some(SidebarItem::Sync(i)) => {
                config.sync.get(*i).map(|s| sync_fields(s)).unwrap_or_default()
            }
        }
    }

    fn render_sidebar(
        &mut self,
        area: Rect,
        frame: &mut Frame,
        theme: &Theme,
        config: &AppConfig,
    ) {
        let focused = self.zone == ConfigZone::Sidebar;
        let border_style = Style::default().fg(if focused {
            theme.accent_config
        } else {
            theme.border_inactive
        });
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(" Config ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let visible_h = inner.height as usize;
        self.sidebar_vp.set_dims(self.items.len(), visible_h);

        let max_w = inner.width.saturating_sub(1) as usize;
        let (start, end) = self.sidebar_vp.visible_range();

        let lines: Vec<Line> = self.items[start..end]
            .iter()
            .enumerate()
            .map(|(rel, item)| {
                let abs = start + rel;
                let is_sel = abs == self.sidebar_vp.selected;

                let (prefix, text, is_header) = sidebar_item_display(item, config);
                let glyph = if is_sel && focused { "▶" } else if is_sel { ">" } else { " " };
                let label = trunc(&format!("{glyph}{prefix}{text}"), max_w);

                let style = if is_sel && focused {
                    Style::default()
                        .fg(theme.accent_config)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else if is_sel {
                    Style::default().add_modifier(Modifier::BOLD)
                } else if is_header {
                    Style::default().fg(theme.accent_config)
                } else {
                    Style::default()
                };

                Line::from(Span::styled(label, style))
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_field_table(
        &mut self,
        area: Rect,
        frame: &mut Frame,
        theme: &Theme,
        config: &AppConfig,
    ) {
        let focused = self.zone == ConfigZone::FieldTable;
        let border_style = Style::default().fg(if focused {
            theme.accent_config
        } else {
            theme.border_inactive
        });
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let fields = self.current_fields(config);
        let visible_h = inner.height as usize;
        self.field_vp.set_dims(fields.len(), visible_h);

        if fields.is_empty() {
            let msg = match self.items.get(self.sidebar_vp.selected) {
                Some(SidebarItem::SectionHosts) if config.host.is_empty() => {
                    "(no hosts configured)"
                }
                Some(SidebarItem::SectionHosts) => "(select a host entry in the sidebar  ↑↓)",
                Some(SidebarItem::SectionChecks) if config.check.is_empty() => {
                    "(no [[check]] entries configured)"
                }
                Some(SidebarItem::SectionChecks) => {
                    "(select a check entry in the sidebar  ↑↓)"
                }
                Some(SidebarItem::SectionSyncs) if config.sync.is_empty() => {
                    "(no [[sync]] entries configured)"
                }
                Some(SidebarItem::SectionSyncs) => {
                    "(select a sync entry in the sidebar  ↑↓)"
                }
                _ => "(nothing to show)",
            };
            frame.render_widget(
                Paragraph::new(Span::styled(msg, Style::default().fg(theme.inactive))),
                inner,
            );
            return;
        }

        let key_w = fields
            .iter()
            .map(|(k, _)| k.width())
            .max()
            .unwrap_or(10)
            .min(30) as u16;

        let (start, end) = self.field_vp.visible_range();
        let rows: Vec<Row> = fields[start..end]
            .iter()
            .enumerate()
            .map(|(rel, (k, v))| {
                let abs = start + rel;
                let is_sel = abs == self.field_vp.selected && focused;
                let key_style = if is_sel {
                    Style::default()
                        .fg(theme.accent_config)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default().fg(theme.inactive)
                };
                let val_style = if is_sel {
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Row::new(vec![
                    Cell::from(k.as_str()).style(key_style),
                    Cell::from(" = ").style(Style::default().fg(theme.inactive)),
                    Cell::from(v.as_str()).style(val_style),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [Constraint::Length(key_w), Constraint::Length(3), Constraint::Min(0)],
        );
        frame.render_widget(table, inner);
    }
}

// ── Sidebar construction ─────────────────────────────────────────────────────

fn build_sidebar_items(config: &AppConfig) -> Vec<SidebarItem> {
    let mut items = vec![SidebarItem::SectionSettings, SidebarItem::SectionHosts];
    for i in 0..config.host.len() {
        items.push(SidebarItem::Host(i));
    }
    items.push(SidebarItem::SectionChecks);
    for i in 0..config.check.len() {
        items.push(SidebarItem::Check(i));
    }
    items.push(SidebarItem::SectionSyncs);
    for i in 0..config.sync.len() {
        items.push(SidebarItem::Sync(i));
    }
    items
}

/// Returns `(indent_prefix, display_text, is_section_header)` for a sidebar item.
fn sidebar_item_display<'a>(
    item: &SidebarItem,
    config: &'a AppConfig,
) -> (&'static str, String, bool) {
    match item {
        SidebarItem::SectionSettings => ("", "Settings".to_string(), true),
        SidebarItem::SectionHosts => ("", format!("Hosts ({})", config.host.len()), true),
        SidebarItem::Host(i) => {
            let name = config.host.get(*i).map(|h| h.name.as_str()).unwrap_or("?");
            ("  ", name.to_string(), false)
        }
        SidebarItem::SectionChecks => ("", format!("Checks ({})", config.check.len()), true),
        SidebarItem::Check(i) => ("  ", entry_label_check(config, *i), false),
        SidebarItem::SectionSyncs => ("", format!("Syncs ({})", config.sync.len()), true),
        SidebarItem::Sync(i) => ("  ", entry_label_sync(config, *i), false),
    }
}

// ── Field row builders ───────────────────────────────────────────────────────

fn settings_fields(s: &Settings) -> Vec<(String, String)> {
    let mut f = vec![
        ("default_timeout".into(), format!("{}s", s.default_timeout)),
        (
            "data_retention_days".into(),
            format!("{}d", s.data_retention_days),
        ),
        (
            "conflict_strategy".into(),
            format!("{:?}", s.conflict_strategy).to_lowercase(),
        ),
        ("propagate_deletes".into(), s.propagate_deletes.to_string()),
        ("max_concurrency".into(), s.max_concurrency.to_string()),
        (
            "max_per_host_concurrency".into(),
            s.max_per_host_concurrency.to_string(),
        ),
    ];
    if let Some(d) = &s.state_dir {
        f.push(("state_dir".into(), d.display().to_string()));
    }
    if let Some(fmt) = &s.default_output_format {
        f.push(("default_output_format".into(), fmt.clone()));
    }
    if !s.skipped_hosts.is_empty() {
        f.push((
            "skipped_hosts".into(),
            format!("[{}]", s.skipped_hosts.join(", ")),
        ));
    }
    f
}

fn host_fields(h: &HostEntry) -> Vec<(String, String)> {
    let mut f = vec![
        ("name".into(), h.name.clone()),
        ("ssh_host".into(), h.ssh_host.clone()),
        ("shell".into(), h.shell.to_string()),
        (
            "groups".into(),
            if h.groups.is_empty() {
                "(none)".into()
            } else {
                format!("[{}]", h.groups.join(", "))
            },
        ),
    ];
    if let Some(pj) = &h.proxy_jump {
        f.push(("proxy_jump".into(), pj.clone()));
    }
    f
}

fn check_fields(c: &CheckEntry) -> Vec<(String, String)> {
    let mut f = vec![
        (
            "enabled".into(),
            if c.enabled.is_empty() {
                "(none)".into()
            } else {
                format!("[{}]", c.enabled.join(", "))
            },
        ),
        (
            "groups".into(),
            if c.groups.is_empty() {
                "(unscoped)".into()
            } else {
                format!("[{}]", c.groups.join(", "))
            },
        ),
        ("enable_hosts".into(), c.enable_hosts.to_string()),
        ("enable_all".into(), c.enable_all.to_string()),
    ];
    for (i, p) in c.path.iter().enumerate() {
        f.push((
            format!("path[{i}]"),
            format!("{} → {}", p.label, p.path),
        ));
    }
    f
}

fn sync_fields(s: &SyncEntry) -> Vec<(String, String)> {
    let mut f = vec![
        (
            "paths".into(),
            if s.paths.is_empty() {
                "(none)".into()
            } else {
                format!("[{}]", s.paths.join(", "))
            },
        ),
        (
            "groups".into(),
            if s.groups.is_empty() {
                "(unscoped)".into()
            } else {
                format!("[{}]", s.groups.join(", "))
            },
        ),
        ("enable_hosts".into(), s.enable_hosts.to_string()),
        ("enable_all".into(), s.enable_all.to_string()),
        ("recursive".into(), s.recursive.to_string()),
    ];
    if let Some(m) = &s.mode {
        f.push(("mode".into(), m.clone()));
    }
    if let Some(pd) = s.propagate_deletes {
        f.push(("propagate_deletes".into(), pd.to_string()));
    }
    if let Some(src) = &s.source {
        f.push(("source".into(), src.clone()));
    }
    f
}

// ── Label helpers ────────────────────────────────────────────────────────────

fn entry_label_check(config: &AppConfig, i: usize) -> String {
    config
        .check
        .get(i)
        .map(|c| {
            if c.groups.is_empty() {
                format!("Check #{}", i + 1)
            } else {
                format!("Check #{} [{}]", i + 1, c.groups.join(","))
            }
        })
        .unwrap_or_else(|| format!("Check #{}", i + 1))
}

fn entry_label_sync(config: &AppConfig, i: usize) -> String {
    config
        .sync
        .get(i)
        .map(|s| {
            let path_hint = s.paths.first().map(|p| trunc(p, 10)).unwrap_or_default();
            if path_hint.is_empty() {
                format!("Sync #{}", i + 1)
            } else {
                format!("Sync #{}: {}", i + 1, path_hint)
            }
        })
        .unwrap_or_else(|| format!("Sync #{}", i + 1))
}

// ── Utilities ────────────────────────────────────────────────────────────────

fn trunc(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut w = 0usize;
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
