use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub settings: Settings,

    #[serde(default)]
    pub host: Vec<HostEntry>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check: Vec<CheckEntry>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync: Vec<SyncEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_timeout")]
    pub default_timeout: u64,

    #[serde(default = "default_retention")]
    pub data_retention_days: u64,

    #[serde(default = "default_conflict_strategy")]
    pub conflict_strategy: ConflictStrategy,

    #[serde(default)]
    pub propagate_deletes: bool,

    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,

    /// Hosts to skip during init (persisted across re-init)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_hosts: Vec<String>,

    /// Override the state directory (where ssync.db is stored).
    /// Default: ~/.local/state/ssync (Linux/macOS) or %LOCALAPPDATA%/ssync (Windows)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_timeout: default_timeout(),
            data_retention_days: default_retention(),
            conflict_strategy: default_conflict_strategy(),
            propagate_deletes: false,
            max_concurrency: default_concurrency(),
            skipped_hosts: Vec::new(),
            state_dir: None,
        }
    }
}

fn default_timeout() -> u64 {
    30
}
fn default_retention() -> u64 {
    90
}
fn default_conflict_strategy() -> ConflictStrategy {
    ConflictStrategy::Newest
}
fn default_concurrency() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConflictStrategy {
    Newest,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEntry {
    pub name: String,
    pub ssh_host: String,
    pub shell: ShellType,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellType {
    Sh,
    #[serde(rename = "powershell")]
    PowerShell,
    Cmd,
}

impl std::fmt::Display for ShellType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellType::Sh => write!(f, "sh"),
            ShellType::PowerShell => write!(f, "powershell"),
            ShellType::Cmd => write!(f, "cmd"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckEntry {
    #[serde(default)]
    pub enabled: Vec<String>,

    #[serde(default)]
    pub path: Vec<CheckPath>,

    /// Groups this check applies to. Empty = global (applies with --all).
    #[serde(default)]
    pub groups: Vec<String>,

    /// Hosts this check applies to. Empty = not host-scoped.
    #[serde(default)]
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckPath {
    pub path: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    pub paths: Vec<String>,
    /// Groups this sync applies to. Empty = global (applies with --all).
    #[serde(default)]
    pub groups: Vec<String>,
    /// Hosts this sync applies to. Empty = not host-scoped.
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub recursive: bool,
    pub mode: Option<String>,
    pub propagate_deletes: Option<bool>,
    /// Fixed source host — bypass automatic source selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
