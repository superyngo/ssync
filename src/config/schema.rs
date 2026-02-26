use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub settings: Settings,

    #[serde(default)]
    pub host: Vec<HostEntry>,

    #[serde(default)]
    pub check: CheckConfig,

    #[serde(default)]
    pub sync: SyncConfig,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_timeout: default_timeout(),
            data_retention_days: default_retention(),
            conflict_strategy: default_conflict_strategy(),
            propagate_deletes: false,
            max_concurrency: default_concurrency(),
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

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CheckConfig {
    #[serde(default)]
    pub enabled: Vec<String>,

    #[serde(default)]
    pub path: Vec<CheckPath>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckPath {
    pub path: String,
    pub label: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    #[serde(default)]
    pub group: Vec<SyncGroup>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncGroup {
    pub name: String,
    pub hosts: Vec<String>,
    #[serde(default)]
    pub file: Vec<SyncFile>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncFile {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    pub mode: Option<String>,
    pub propagate_deletes: Option<bool>,
}
