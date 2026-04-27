use std::sync::Arc;

use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::output::progress::SyncProgress;

use super::concurrency::ConcurrencyLimiter;
use super::session_pool::RusshSessionPool;

/// Shared SSH connection pool: wraps RusshSessionPool + ConcurrencyLimiter + SyncProgress.
/// Used by all SSH-using subcommands for consistent connection pooling, concurrency control,
/// and progress display.
pub struct SshPool {
    pub(crate) session_pool: Arc<RusshSessionPool>,
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

    /// Set up the pool with optional SFTP probe.
    /// When `probe_sftp` is true, reachable hosts are also tested for SFTP capability.
    /// The progress bar reflects both SSH + SFTP checks.
    pub async fn setup_with_options(
        hosts: &[&HostEntry],
        timeout: u64,
        global_concurrency: usize,
        per_host_concurrency: usize,
        probe_sftp: bool,
    ) -> Result<(Self, usize)> {
        let host_names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let limiter =
            ConcurrencyLimiter::new(global_concurrency, per_host_concurrency, &host_names);
        let mut progress = SyncProgress::new();

        progress.start_host_check(hosts.len());
        let mut session_pool = RusshSessionPool::setup(hosts, timeout, global_concurrency).await?;
        let connected = session_pool.reachable_hosts().len();

        if probe_sftp && connected > 0 {
            session_pool.run_sftp_probe(hosts, timeout).await;
            let sftp_failed = session_pool.sftp_failed_hosts().len();
            let effective_ok = connected.saturating_sub(sftp_failed);
            progress.finish_host_check(effective_ok, hosts.len() - effective_ok);
        } else {
            let failed = hosts.len() - connected;
            progress.finish_host_check(connected, failed);
        }

        Ok((
            Self {
                session_pool: Arc::new(session_pool),
                limiter,
                progress,
            },
            connected,
        ))
    }

    /// ControlMaster sockets are no longer used; always returns None.
    pub fn socket_for(&self, _host_name: &str) -> Option<&std::path::Path> {
        None
    }

    /// Get names of all reachable hosts.
    #[allow(dead_code)]
    pub fn reachable_hosts(&self) -> Vec<String> {
        self.session_pool.reachable_hosts()
    }

    /// Get names and errors of all failed hosts.
    pub fn failed_hosts(&self) -> Vec<(String, String)> {
        self.session_pool.failed_hosts()
    }

    /// Filter a host list to only reachable hosts.
    pub fn filter_reachable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let reachable: std::collections::HashSet<String> =
            self.session_pool.reachable_hosts().into_iter().collect();
        hosts
            .iter()
            .filter(|h| reachable.contains(&h.name))
            .copied()
            .collect()
    }

    /// Get names and errors of hosts that failed the SFTP probe.
    pub fn sftp_failed_hosts(&self) -> Vec<(String, String)> {
        self.session_pool.sftp_failed_hosts()
    }

    /// Filter a host list to only hosts that passed the SFTP probe.
    pub fn filter_sftp_capable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let capable = self.session_pool.sftp_capable_hosts();
        hosts
            .iter()
            .filter(|h| capable.contains(&h.ssh_host))
            .copied()
            .collect()
    }

    /// Gracefully shut down all sessions and clear progress bars.
    pub async fn shutdown(self) {
        self.progress.clear();
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
    use super::super::session_pool::RemoteOutput;
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
