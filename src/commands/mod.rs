pub mod check;
pub mod checkout;
pub mod config;
pub mod exec;
pub mod init;
pub mod list;
pub mod log;
pub mod run;
pub mod sync;

use anyhow::{bail, Result};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::cli::TargetArgs;
use crate::config::schema::{AppConfig, CheckEntry, HostEntry, SyncEntry};

/// Target mode derived from CLI flags.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetMode {
    /// --all: all configured hosts
    All,
    /// --host: specific hosts by name
    Hosts(Vec<String>),
    /// --group: hosts belonging to named groups
    Groups(Vec<String>),
    /// --shell: hosts with the specified shell type(s)
    Shell(Vec<crate::config::schema::ShellType>),
}

/// Shared context available to all commands.
pub struct Context {
    pub config: AppConfig,
    pub config_path: Option<PathBuf>,
    pub db: Connection,
    pub timeout: u64,
    pub mode: TargetMode,
    pub serial: bool,
    #[allow(dead_code)]
    pub verbose: bool,
}

impl Context {
    pub async fn new(
        verbose: bool,
        target: &TargetArgs,
        config_path: Option<&Path>,
    ) -> Result<Self> {
        let config = crate::config::app::load(config_path)?.unwrap_or_default();
        let db = crate::state::db::open(config.settings.state_dir.as_deref())?;
        let timeout = target.timeout.unwrap_or(config.settings.default_timeout);
        let mode = resolve_target_mode(target, &config)?;

        Ok(Self {
            config,
            config_path: config_path.map(|p| p.to_path_buf()),
            db,
            timeout,
            mode,
            serial: target.serial,
            verbose,
        })
    }

    /// Create a context without target args (for commands like init, config, log).
    pub async fn new_without_targets(
        verbose: bool,
        config_path: Option<&Path>,
        timeout_override: Option<u64>,
    ) -> Result<Self> {
        let config = crate::config::app::load(config_path)?.unwrap_or_default();
        let db = crate::state::db::open(config.settings.state_dir.as_deref())?;
        let timeout = timeout_override.unwrap_or(config.settings.default_timeout);

        Ok(Self {
            config,
            config_path: config_path.map(|p| p.to_path_buf()),
            db,
            timeout,
            mode: TargetMode::All,
            serial: false,
            verbose,
        })
    }

    /// Resolve targeted hosts based on mode.
    /// For --all: all hosts. For --host: named hosts. For --group: hosts in group.
    pub fn resolve_hosts(&self) -> Result<Vec<&HostEntry>> {
        let hosts: Vec<&HostEntry> = match &self.mode {
            TargetMode::All => self.config.host.iter().collect(),
            TargetMode::Hosts(names) => self
                .config
                .host
                .iter()
                .filter(|h| names.contains(&h.name))
                .collect(),
            TargetMode::Groups(groups) => self
                .config
                .host
                .iter()
                .filter(|h| h.groups.iter().any(|g| groups.contains(g)))
                .collect(),
            TargetMode::Shell(shells) => self
                .config
                .host
                .iter()
                .filter(|h| shells.contains(&h.shell))
                .collect(),
        };

        if hosts.is_empty() {
            let mut hint = String::from("No hosts matched the specified filter.");
            if let TargetMode::Shell(shells) = &self.mode {
                hint = format!(
                    "No hosts matched shell type: {}",
                    shells
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let mut shell_map: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for h in &self.config.host {
                    shell_map
                        .entry(h.shell.to_string())
                        .or_default()
                        .push(h.name.clone());
                }
                if !shell_map.is_empty() {
                    let parts: Vec<String> = shell_map
                        .iter()
                        .map(|(shell, hosts)| format!("{} ({})", shell, hosts.join(", ")))
                        .collect();
                    hint.push_str(&format!("\nAvailable shells: {}", parts.join(", ")));
                }
            } else {
                append_available_hints(&self.config, &mut hint);
            }
            bail!("{}", hint);
        }

        Ok(hosts)
    }

    /// Get the global concurrency limit.
    pub fn concurrency(&self) -> usize {
        if self.serial {
            1
        } else {
            self.config.settings.max_concurrency
        }
    }

    /// Get the per-host concurrency limit.
    pub fn per_host_concurrency(&self) -> usize {
        if self.serial {
            1
        } else {
            self.config.settings.max_per_host_concurrency
        }
    }

    /// Resolve check entries based on target mode.
    /// --groups: entries whose groups intersect. --hosts: entries with enable_hosts.
    /// --all: entries with enable_all.
    pub fn resolve_checks(&self) -> Vec<&CheckEntry> {
        filter_entries_by_mode(
            &self.config.check,
            |e| &e.groups,
            |e| e.enable_hosts,
            |e| e.enable_all,
            &self.mode,
        )
    }

    /// Resolve sync entries based on target mode.
    pub fn resolve_syncs(&self) -> Vec<&SyncEntry> {
        filter_entries_by_mode(
            &self.config.sync,
            |e| &e.groups,
            |e| e.enable_hosts,
            |e| e.enable_all,
            &self.mode,
        )
    }

    /// Resolve check entries for a single group (used in per-group execution).
    pub fn resolve_checks_for_group(&self, group: &str) -> Vec<&CheckEntry> {
        self.config
            .check
            .iter()
            .filter(|e| e.groups.iter().any(|g| g == group))
            .collect()
    }

    /// Resolve sync entries for a single group (used in per-group execution).
    pub fn resolve_syncs_for_group(&self, group: &str) -> Vec<&SyncEntry> {
        self.config
            .sync
            .iter()
            .filter(|e| e.groups.iter().any(|g| g == group))
            .collect()
    }
}

/// Generic filter for config entries (check/sync) by target mode.
/// --groups: entries whose groups intersect the specified groups.
/// --hosts: entries where enable_hosts is true.
/// --all: entries where enable_all is true.
fn filter_entries_by_mode<'a, T>(
    entries: &'a [T],
    get_groups: impl Fn(&T) -> &Vec<String>,
    get_enable_hosts: impl Fn(&T) -> bool,
    get_enable_all: impl Fn(&T) -> bool,
    mode: &TargetMode,
) -> Vec<&'a T> {
    entries
        .iter()
        .filter(|e| match mode {
            TargetMode::All => get_enable_all(e),
            TargetMode::Groups(g) => get_groups(e).iter().any(|eg| g.contains(eg)),
            TargetMode::Hosts(_) => get_enable_hosts(e),
            TargetMode::Shell(_) => get_enable_hosts(e),
        })
        .collect()
}

/// Resolve which target mode the user intended, or show helpful error.
fn resolve_target_mode(target: &TargetArgs, config: &AppConfig) -> Result<TargetMode> {
    let has_all = target.all;
    let has_hosts = !target.host.is_empty();
    let has_groups = !target.group.is_empty();
    let has_shell = !target.shell.is_empty();

    let count = has_all as u8 + has_hosts as u8 + has_groups as u8 + has_shell as u8;

    if count == 0 {
        let mut hint = String::from(
            "Target required. Use --group/-g, --host/-h, --shell/-S, or --all/-a to specify targets.",
        );
        if config.host.is_empty() {
            hint.push_str("\nHint: Run 'ssync init' first to import hosts from ~/.ssh/config.");
        } else {
            append_available_hints(config, &mut hint);
        }
        bail!("{}", hint);
    }

    if count > 1 {
        bail!("Only one of --all/-a, --host/-h, --group/-g, or --shell/-S can be used at a time.");
    }

    if has_all {
        Ok(TargetMode::All)
    } else if has_hosts {
        Ok(TargetMode::Hosts(target.host.clone()))
    } else if has_groups {
        Ok(TargetMode::Groups(target.group.clone()))
    } else {
        Ok(TargetMode::Shell(target.shell.clone()))
    }
}

/// Append available groups and hosts to hint message.
fn append_available_hints(config: &AppConfig, hint: &mut String) {
    let groups = collect_available_groups(config);
    if !groups.is_empty() {
        hint.push_str(&format!(
            "\n\nAvailable groups: {}",
            groups.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !config.host.is_empty() {
        let names: Vec<&str> = config.host.iter().map(|h| h.name.as_str()).collect();
        hint.push_str(&format!("\nAvailable hosts: {}", names.join(", ")));
    }
}

/// Collect available group names from host[].groups tags.
fn collect_available_groups(config: &AppConfig) -> BTreeSet<String> {
    let mut groups = BTreeSet::new();
    for h in &config.host {
        for g in &h.groups {
            if !g.is_empty() {
                groups.insert(g.clone());
            }
        }
    }
    groups
}
