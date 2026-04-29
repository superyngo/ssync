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

    /// Filter by remote shell type (comma-separated: sh, powershell, cmd)
    #[arg(short = 'S', long, value_delimiter = ',')]
    pub shell: Vec<crate::config::schema::ShellType>,

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
    #[command(disable_help_flag = true)]
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

        /// Connection timeout in seconds (overrides config)
        #[arg(long)]
        timeout: Option<u64>,

        /// Print help
        #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
        help: Option<bool>,
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

        /// Show trend history
        #[arg(long)]
        history: bool,

        /// History start point (e.g. "2025-01-01" or "7d")
        #[arg(long)]
        since: Option<String>,

        /// Print help
        #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
        help: Option<bool>,
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

        /// Use a specific host as file source (bypasses auto-detection)
        #[arg(short = 's', long)]
        source: Option<String>,
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
    #[command(disable_help_flag = true)]
    Config {
        /// Print help
        #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
        help: Option<bool>,
    },

    /// List hosts, applicable checks, and sync paths
    #[command(disable_help_flag = true)]
    List {
        #[command(flatten)]
        target: TargetArgs,
    },

    /// View operation logs
    #[command(disable_help_flag = true)]
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

        /// Print help
        #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
        help: Option<bool>,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum ActionFilter {
    Sync,
    Run,
    Exec,
    Check,
}
