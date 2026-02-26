pub mod check;
pub mod checkout;
pub mod exec;
pub mod init;
pub mod log;
pub mod run;
pub mod sync;

use anyhow::{bail, Result};
use rusqlite::Connection;

use crate::cli::Cli;
use crate::config::schema::{AppConfig, HostEntry};

/// Shared context available to all commands.
pub struct Context {
    pub config: AppConfig,
    pub db: Connection,
    pub timeout: u64,
    pub groups: Vec<String>,
    pub hosts: Vec<String>,
    pub all: bool,
    pub serial: bool,
    pub verbose: bool,
}

impl Context {
    pub async fn new(cli: &Cli) -> Result<Self> {
        let config = crate::config::app::load()?.unwrap_or_default();
        let db = crate::state::db::open()?;
        let timeout = cli.timeout.unwrap_or(config.settings.default_timeout);

        Ok(Self {
            config,
            db,
            timeout,
            groups: cli.group.clone(),
            hosts: cli.host.clone(),
            all: cli.all,
            serial: cli.serial,
            verbose: cli.verbose,
        })
    }

    /// Get filtered hosts based on CLI parameters.
    pub fn filtered_hosts(&self) -> Vec<&HostEntry> {
        crate::host::filter::filter_hosts(
            &self.config.host,
            &self.groups,
            &self.hosts,
            self.all,
        )
    }

    /// Validate that at least one host target is specified.
    pub fn require_targets(&self) -> Result<Vec<&HostEntry>> {
        let hosts = self.filtered_hosts();
        if hosts.is_empty() {
            bail!(
                "No hosts matched. Use --group, --host, or --all to specify targets.\n\
                 Hint: Run 'ssync init' first to import hosts from ~/.ssh/config."
            );
        }
        Ok(hosts)
    }

    /// Get the concurrency limit.
    pub fn concurrency(&self) -> usize {
        if self.serial {
            1
        } else {
            self.config.settings.max_concurrency
        }
    }
}
