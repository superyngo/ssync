use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::schema::HostEntry;

/// Result of a remote command execution.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Unified interface for SSH operations.
///
/// Abstracts over the transport mechanism (process spawning, embedded library, etc.)
/// so that command modules depend only on this trait and can be tested with mocks.
///
/// # Lifecycle
///
/// 1. Create the transport (`ProcessTransport::new()`)
/// 2. Call `connect()` to establish connections to hosts
/// 3. Use `exec()`, `upload()`, `download()` for operations
/// 4. Optionally call `scp_probe()` to verify SCP capability
/// 5. Call `shutdown()` when done
#[async_trait]
#[allow(dead_code)]
pub trait SshTransport: Send + Sync {
    /// Establish connections to a set of hosts.
    /// Returns names of successfully connected hosts.
    async fn connect(
        &self,
        hosts: &[&HostEntry],
        timeout: Duration,
        concurrency: usize,
    ) -> Result<Vec<String>>;

    /// Execute a command on a remote host.
    async fn exec(&self, host: &HostEntry, cmd: &str, timeout: Duration) -> Result<RemoteOutput>;

    /// Upload a local file to a remote path.
    async fn upload(
        &self,
        host: &HostEntry,
        local: &Path,
        remote: &str,
        timeout: Duration,
    ) -> Result<()>;

    /// Download a remote file to a local path.
    async fn download(
        &self,
        host: &HostEntry,
        remote: &str,
        local: &Path,
        timeout: Duration,
    ) -> Result<()>;

    /// Probe SCP capability on connected hosts.
    /// Returns names of hosts that passed.
    async fn scp_probe(
        &self,
        hosts: &[&HostEntry],
        timeout: Duration,
        concurrency: usize,
    ) -> Result<Vec<String>>;

    /// Get names of hosts that failed to connect (with error messages).
    fn failed_hosts(&self) -> Vec<(String, String)>;

    /// Get names of hosts that failed the SCP probe.
    fn scp_failed_hosts(&self) -> Vec<(String, String)>;

    /// Get names of all successfully connected hosts.
    fn reachable_hosts(&self) -> Vec<String>;

    /// Gracefully close all connections.
    async fn shutdown(&self) -> Result<()>;
}
