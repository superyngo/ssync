# SSYNC Complete File Contents Reference

This document contains the complete content of all key files mentioned in the analysis.

## 1. src/config/schema.rs

```rust
use std::path::PathBuf;

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

fn default_timeout() -> u64 { 30 }
fn default_retention() -> u64 { 90 }
fn default_conflict_strategy() -> ConflictStrategy { ConflictStrategy::Newest }
fn default_concurrency() -> usize { 10 }

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
    pub file: Vec<SyncFile>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncFile {
    pub paths: Vec<String>,
    /// Groups this file applies to. Empty = applies to --all/--host scope.
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub recursive: bool,
    pub mode: Option<String>,
    pub propagate_deletes: Option<bool>,
}
```

---

## 2. src/config/app.rs

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::schema::AppConfig;

/// Returns the platform-appropriate config directory for ssync.
pub fn config_dir() -> Result<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        Ok(home.join(".config").join("ssync"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = dirs::config_dir().context("Cannot determine config directory")?;
        return Ok(base.join("ssync"));
    }
}

/// Returns the path to config.toml.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Resolve the effective config file path.
pub fn resolve_path(custom_path: Option<&Path>) -> Result<PathBuf> {
    match custom_path {
        Some(p) => Ok(p.to_path_buf()),
        None => config_path(),
    }
}

/// Load config from disk. Returns None if file doesn't exist.
pub fn load(custom_path: Option<&Path>) -> Result<Option<AppConfig>> {
    let path = resolve_path(custom_path)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config: AppConfig =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(config))
}

/// Save config to disk, creating parent directories if needed.
/// Adds helpful comments to [check] and [sync] sections.
pub fn save(config: &AppConfig, custom_path: Option<&Path>) -> Result<()> {
    let path = resolve_path(custom_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(config).context("Failed to serialize config")?;
    let content = inject_config_comments(&content);
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Inject helpful comments into TOML config for [check] and [sync] sections.
fn inject_config_comments(toml_str: &str) -> String {
    let check_comment = "\
# [check] 可設定的 enabled 值：
# enabled = [
#     \"online\",        # 檢查主機是否在線
#     \"system_info\",   # 系統資訊 (uname / systeminfo)
#     \"cpu_arch\",      # CPU 架構
#     \"memory\",        # 記憶體使用量
#     \"swap\",          # Swap 使用量
#     \"disk\",          # 磁碟使用量
#     \"cpu_load\",      # CPU 負載
#     \"network\",       # 網路介面資訊
#     \"battery\",       # 電池狀態
# ]
# [[check.path]] 可設定自訂路徑監控：
#   path  = \"/var/log\"    # 要監控的路徑
#   label = \"Logs\"        # 顯示用的標籤
#
# 範例：
#   enabled = [\"online\", \"memory\", \"disk\", \"cpu_load\"]
#   [[check.path]]
#   path = \"/home\"
#   label = \"Home\"
";

    let sync_comment = "\
# [sync] 同步設定：
#
# ── 全域同步 (搭配 --all/-a 使用) ──
# [[sync.file]]
# paths = [\"/etc/timezone\"]          # 要同步的檔案路徑 (可多個)
# recursive = false                 # 是否遞迴同步 (預設: false)
# mode = \"0644\"                     # 檔案權限 (選填)
# propagate_deletes = false         # 是否同步刪除 (選填, 預設: false)
#
# ── 群組同步 (搭配 --group/-g 使用) ──
# [[sync.file]]
# paths = [\"/etc/nginx/nginx.conf\", \"/etc/nginx/conf.d\"]
# groups = [\"webservers\"]           # 套用的 group (對應 host[].groups)
#
# [[sync.file]]
# paths = [\"/etc/my.cnf\"]
# groups = [\"databases\"]
";

    let mut result = String::new();
    for line in toml_str.lines() {
        if line.trim() == "[check]" {
            result.push_str(check_comment);
        } else if line.trim() == "[sync]" {
            result.push_str(sync_comment);
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}
```

---

## 3. src/cli.rs

```rust
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ssync",
    version,
    about = "SSH-config-based cross-platform remote management tool"
)]
pub struct Cli {
    /// Enable verbose output
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Path to config file (default: ~/.config/ssync/config.toml)
    #[arg(short = 'c', long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Common target selection arguments for commands that operate on remote hosts.
#[derive(Args, Clone, Debug)]
pub struct TargetArgs {
    /// Specify groups (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub group: Vec<String>,

    /// Specify hosts (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub host: Vec<String>,

    /// Target all hosts
    #[arg(short, long)]
    pub all: bool,

    /// Execute sequentially instead of in parallel
    #[arg(long)]
    pub serial: bool,

    /// Connection timeout in seconds (overrides config)
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Print help
    #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
    pub help: Option<bool>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Import hosts from ~/.ssh/config and detect remote shell types
    Init {
        /// Re-detect shell type for existing hosts
        #[arg(long)]
        update: bool,

        /// Show what would be imported without writing
        #[arg(long)]
        dry_run: bool,

        /// Skip specific hosts (comma-separated)
        #[arg(long, value_delimiter = ',')]
        skip: Vec<String>,
    },

    /// Collect system snapshots from hosts and store in state DB
    #[command(disable_help_flag = true)]
    Check {
        #[command(flatten)]
        target: TargetArgs,
    },

    /// View historical data and generate reports from state DB
    #[command(disable_help_flag = true)]
    Checkout {
        #[command(flatten)]
        target: TargetArgs,

        /// Output format
        #[arg(long, default_value = "tui")]
        format: OutputFormat,

        /// Show trend history
        #[arg(long)]
        history: bool,

        /// History start point (e.g. "2025-01-01" or "7d")
        #[arg(long)]
        since: Option<String>,

        /// Output file path (required for html/json)
        #[arg(short, long)]
        out: Option<String>,
    },

    /// Synchronize files across hosts using collect-decide-distribute model
    #[command(disable_help_flag = true)]
    Sync {
        #[command(flatten)]
        target: TargetArgs,

        /// Preview sync decisions without making changes
        #[arg(long)]
        dry_run: bool,

        /// Ad-hoc file paths to sync (comma-separated)
        #[arg(short = 'f', long, value_delimiter = ',')]
        files: Vec<String>,

        /// Don't push files to hosts that are missing them
        #[arg(long)]
        no_push_missing: bool,
    },

    /// Execute a command string on remote hosts
    #[command(disable_help_flag = true)]
    Run {
        #[command(flatten)]
        target: TargetArgs,

        /// Command to execute
        command: String,

        /// Run with sudo
        #[arg(short, long)]
        sudo: bool,

        /// Auto-respond yes to interactive prompts (serial mode only)
        #[arg(short, long)]
        yes: bool,
    },

    /// Upload and execute a local script on remote hosts
    #[command(disable_help_flag = true)]
    Exec {
        #[command(flatten)]
        target: TargetArgs,

        /// Local script path
        script: String,

        /// Run with sudo
        #[arg(short, long)]
        sudo: bool,

        /// Auto-respond yes to interactive prompts (serial mode only)
        #[arg(short, long)]
        yes: bool,

        /// Keep remote temp script after execution
        #[arg(long)]
        keep: bool,

        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },

    /// Open config file in $EDITOR
    Config,

    /// View operation logs
    Log {
        /// Show last N entries (default: 20)
        #[arg(long, default_value = "20")]
        last: usize,

        /// Show entries since datetime
        #[arg(long)]
        since: Option<String>,

        /// Filter by host name
        #[arg(short, long)]
        host: Option<String>,

        /// Filter by action type
        #[arg(long)]
        action: Option<ActionFilter>,

        /// Show only error entries
        #[arg(long)]
        errors: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum OutputFormat {
    Tui,
    Table,
    Html,
    Json,
}

#[derive(Clone, clap::ValueEnum)]
pub enum ActionFilter {
    Sync,
    Run,
    Exec,
    Check,
}
```

---

## 4. src/host/filter.rs

```rust
use crate::config::schema::HostEntry;

/// Filter hosts based on CLI parameters.
/// Matches groups from host[].groups tags.
#[allow(dead_code)]
pub fn filter_hosts<'a>(
    hosts: &'a [HostEntry],
    groups: &[String],
    host_names: &[String],
    all: bool,
) -> Vec<&'a HostEntry> {
    if all {
        return hosts.iter().collect();
    }

    let mut result: Vec<&HostEntry> = hosts.iter().collect();

    if !groups.is_empty() {
        result.retain(|h| h.groups.iter().any(|g| groups.contains(g)));
    }

    if !host_names.is_empty() {
        result.retain(|h| host_names.contains(&h.name));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ShellType;

    fn make_hosts() -> Vec<HostEntry> {
        vec![
            HostEntry {
                name: "a".into(),
                ssh_host: "a".into(),
                shell: ShellType::Sh,
                groups: vec!["web".into()],
            },
            HostEntry {
                name: "b".into(),
                ssh_host: "b".into(),
                shell: ShellType::PowerShell,
                groups: vec!["db".into()],
            },
            HostEntry {
                name: "c".into(),
                ssh_host: "c".into(),
                shell: ShellType::Sh,
                groups: vec!["web".into()],
            },
        ]
    }

    #[test]
    fn test_filter_all() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &[], true);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_by_group() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &["web".into()], &[], false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "a");
        assert_eq!(result[1].name, "c");
    }

    #[test]
    fn test_filter_by_host_name() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &["b".into()], false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "b");
    }

    #[test]
    fn test_filter_intersection() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &["web".into()], &["c".into()], false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "c");
    }
}
```

---

## FILES SUMMARY

All complete file contents have been included above. Additional files referenced:

- **src/commands/mod.rs**: CommandContext and TargetMode (178 lines)
- **src/commands/sync.rs**: File synchronization pipeline (589 lines)
- **src/commands/check.rs**: Metric collection (150 lines)
- **src/commands/checkout.rs**: Report generation (510 lines)
- **src/commands/init.rs**: Host discovery (125 lines)
- **src/commands/run.rs**: Command execution (90 lines)
- **src/commands/exec.rs**: Script execution (218 lines)
- **src/commands/config.rs**: Config editor (35 lines)
- **src/commands/log.rs**: Operation logs (114 lines)
- **src/main.rs**: Application entry point (96 lines)
- **src/metrics/collector.rs**: Metric collection logic (100 lines)
- **src/output/summary.rs**: Summary formatting (43 lines)

See CODEBASE_ANALYSIS.md for the complete detailed analysis of all these files.

