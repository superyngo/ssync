use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ssync", version, about = "SSH-config-based cross-platform remote management tool")]
pub struct Cli {
    /// Enable verbose output
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Specify groups (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub group: Vec<String>,

    /// Specify hosts (comma-separated)
    #[arg(short = 'H', long, value_delimiter = ',')]
    pub host: Vec<String>,

    /// Target all hosts
    #[arg(long)]
    pub all: bool,

    /// Execute sequentially instead of in parallel
    #[arg(long)]
    pub serial: bool,

    /// Connection timeout in seconds (overrides config)
    #[arg(long)]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Commands,
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
    },

    /// Collect system snapshots from hosts and store in state DB
    Check,

    /// View historical data and generate reports from state DB
    Checkout {
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
    Sync {
        /// Preview sync decisions without making changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Execute a command string on remote hosts
    Run {
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
    Exec {
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

    /// View operation logs
    Log {
        /// Show last N entries (default: 20)
        #[arg(long, default_value = "20")]
        last: usize,

        /// Show entries since datetime
        #[arg(long)]
        since: Option<String>,

        /// Filter by host name
        #[arg(short = 'H', long)]
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
