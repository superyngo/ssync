use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::output::progress::SyncProgress;

use super::concurrency::ConcurrencyLimiter;
use super::connection::ConnectionManager;
use super::session_pool::{RemoteOutput, RusshSessionPool};

/// Shared SSH connection pool: wraps ConnectionManager + ConcurrencyLimiter + SyncProgress.
/// Used by all SSH-using subcommands for consistent connection pooling, concurrency control,
/// and progress display.
pub struct SshPool {
    /// russh-based sessions for exec operations
    pub(crate) session_pool: Arc<RusshSessionPool>,
    /// legacy ControlMaster pool — kept for sync.rs file transfers until Phase 3
    pub conn_mgr: ConnectionManager,
    pub limiter: ConcurrencyLimiter,
    pub progress: SyncProgress,
}

/// Result of a per-host operation executed through the pool.
#[allow(dead_code)]
pub struct PoolHostResult<T> {
    pub host_name: String,
    pub result: Result<T>,
    pub elapsed: std::time::Duration,
}

impl SshPool {
    /// Set up the pool: create ControlMaster connections, build ConcurrencyLimiter,
    /// initialize progress bars. Returns (pool, connected_count).
    pub async fn setup(
        hosts: &[&HostEntry],
        timeout: u64,
        global_concurrency: usize,
        per_host_concurrency: usize,
    ) -> Result<(Self, usize)> {
        Self::setup_with_options(
            hosts,
            timeout,
            global_concurrency,
            per_host_concurrency,
            false,
        )
        .await
    }

    /// Set up the pool with optional SCP probe.
    /// When `probe_scp` is true, reachable hosts are also tested for SCP capability.
    /// The progress bar reflects both SSH + SCP checks.
    pub async fn setup_with_options(
        hosts: &[&HostEntry],
        timeout: u64,
        global_concurrency: usize,
        per_host_concurrency: usize,
        probe_scp: bool,
    ) -> Result<(Self, usize)> {
        let host_names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let limiter =
            ConcurrencyLimiter::new(global_concurrency, per_host_concurrency, &host_names);
        let mut conn_mgr = ConnectionManager::new()?;
        let mut progress = SyncProgress::new();

        progress.start_host_check(hosts.len());
        let connected = conn_mgr.pre_check(hosts, timeout, global_concurrency).await;

        if probe_scp && connected > 0 {
            let _scp_passed = conn_mgr.scp_probe(hosts, timeout, global_concurrency).await;
            let effective_ok = connected - conn_mgr.scp_failed_hosts().len();
            progress.finish_host_check(effective_ok, hosts.len() - effective_ok);
        } else {
            let failed = hosts.len() - connected;
            progress.finish_host_check(connected, failed);
        }

        // Establish russh sessions to all reachable hosts
        let reachable_names: std::collections::HashSet<String> =
            conn_mgr.reachable_hosts().into_iter().collect();
        let reachable_entries: Vec<&HostEntry> =
            hosts.iter().copied().filter(|h| reachable_names.contains(&h.name)).collect();

        let session_pool = Arc::new(
            RusshSessionPool::setup(&reachable_entries, timeout, global_concurrency).await?,
        );

        let russh_connected = session_pool.reachable_hosts().len();
        Ok((
            Self {
                session_pool,
                conn_mgr,
                limiter,
                progress,
            },
            russh_connected,
        ))
    }

    /// Get the ControlMaster socket path for a connected host.
    pub fn socket_for(&self, host_name: &str) -> Option<&Path> {
        self.conn_mgr.socket_for(host_name)
    }

    /// Get names of all reachable hosts.
    #[allow(dead_code)]
    pub fn reachable_hosts(&self) -> Vec<String> {
        self.conn_mgr.reachable_hosts()
    }

    /// Get names and errors of all failed hosts.
    pub fn failed_hosts(&self) -> Vec<(String, String)> {
        self.conn_mgr.failed_hosts()
    }

    /// Get names and errors of hosts that failed the SCP probe.
    pub fn scp_failed_hosts(&self) -> Vec<(String, String)> {
        self.conn_mgr.scp_failed_hosts()
    }

    /// Filter a host list to only reachable hosts.
    pub fn filter_reachable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let reachable = self.conn_mgr.reachable_hosts();
        hosts
            .iter()
            .filter(|h| reachable.contains(&h.name))
            .copied()
            .collect()
    }

    /// Filter a host list to only hosts that are both SSH-reachable and SCP-capable.
    pub fn filter_scp_capable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let capable = self.conn_mgr.scp_capable_hosts();
        hosts
            .iter()
            .filter(|h| capable.contains(&h.name))
            .copied()
            .collect()
    }

    /// Gracefully close all ControlMaster connections and clear progress bars.
    pub async fn shutdown(mut self) {
        self.progress.clear();
        self.conn_mgr.shutdown().await;
        match Arc::try_unwrap(self.session_pool) {
            Ok(pool) => pool.shutdown().await,
            Err(arc) => {
                tracing::warn!(
                    "session_pool has {} strong references at shutdown; \
                     sessions may not be cleanly closed",
                    Arc::strong_count(&arc)
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_host_result_struct() {
        let r: PoolHostResult<String> = PoolHostResult {
            host_name: "h1".into(),
            result: Ok("ok".into()),
            elapsed: std::time::Duration::from_millis(100),
        };
        assert_eq!(r.host_name, "h1");
        assert!(r.result.is_ok());
    }

    #[test]
    fn test_pool_host_result_error() {
        let r: PoolHostResult<String> = PoolHostResult {
            host_name: "h2".into(),
            result: Err(anyhow::anyhow!("connection refused")),
            elapsed: std::time::Duration::from_millis(50),
        };
        assert!(r.result.is_err());
        assert_eq!(r.host_name, "h2");
    }

    #[test]
    fn test_pool_host_result_with_russh_output() {
        let r: PoolHostResult<RemoteOutput> = PoolHostResult {
            host_name: "h1".into(),
            result: Ok(RemoteOutput {
                stdout: "ok".into(),
                stderr: String::new(),
                exit_code: Some(0),
                success: true,
            }),
            elapsed: std::time::Duration::from_millis(50),
        };
        assert!(r.result.is_ok());
    }
}
