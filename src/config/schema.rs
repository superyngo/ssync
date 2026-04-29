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

    #[serde(default = "default_per_host_concurrency")]
    pub max_per_host_concurrency: usize,

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
            max_per_host_concurrency: default_per_host_concurrency(),
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
fn default_per_host_concurrency() -> usize {
    4
}
fn default_true() -> bool {
    true
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
    /// Optional first-hop ProxyJump alias. None = direct connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum ShellType {
    Sh,
    #[serde(rename = "powershell")]
    #[clap(name = "powershell")]
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

    /// Groups this check applies to. Empty = unscoped.
    #[serde(default)]
    pub groups: Vec<String>,

    /// Whether this entry applies when using --hosts.
    #[serde(default = "default_true")]
    pub enable_hosts: bool,

    /// Whether this entry applies when using --all.
    #[serde(default = "default_true")]
    pub enable_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckPath {
    pub path: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    pub paths: Vec<String>,
    /// Groups this sync applies to. Empty = unscoped.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Whether this entry applies when using --hosts.
    #[serde(default = "default_true")]
    pub enable_hosts: bool,
    /// Whether this entry applies when using --all.
    #[serde(default = "default_true")]
    pub enable_all: bool,
    #[serde(default)]
    pub recursive: bool,
    pub mode: Option<String>,
    pub propagate_deletes: Option<bool>,
    /// Fixed source host — bypass automatic source selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_per_host_concurrency_default() {
        let settings = Settings::default();
        assert_eq!(settings.max_per_host_concurrency, 4);
    }

    #[test]
    fn test_per_host_concurrency_from_toml() {
        let toml_str = "max_per_host_concurrency = 8";
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert_eq!(settings.max_per_host_concurrency, 8);
    }

    #[test]
    fn test_host_entry_proxy_jump_roundtrip() {
        let entry = HostEntry {
            name: "backend".to_string(),
            ssh_host: "backend".to_string(),
            shell: ShellType::Sh,
            groups: vec![],
            proxy_jump: Some("bastion".to_string()),
        };
        let toml_str = toml::to_string_pretty(&entry).unwrap();
        let parsed: HostEntry = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.proxy_jump.as_deref(), Some("bastion"));
    }
}
