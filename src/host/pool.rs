use std::path::Path;

use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::output::progress::SyncProgress;

use super::concurrency::ConcurrencyLimiter;
use super::connection::ConnectionManager;

/// Shared SSH connection pool: wraps ConnectionManager + ConcurrencyLimiter + SyncProgress.
/// Used by all SSH-using subcommands for consistent connection pooling, concurrency control,
/// and progress display.
pub struct SshPool {
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
        let host_names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let limiter =
            ConcurrencyLimiter::new(global_concurrency, per_host_concurrency, &host_names);
        let mut conn_mgr = ConnectionManager::new()?;
        let mut progress = SyncProgress::new();

        progress.start_host_check(hosts.len());
        let connected = conn_mgr.pre_check(hosts, timeout, global_concurrency).await;
        let failed = hosts.len() - connected;
        progress.finish_host_check(connected, failed);

        Ok((
            Self {
                conn_mgr,
                limiter,
                progress,
            },
            connected,
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

    /// Filter a host list to only reachable hosts.
    pub fn filter_reachable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let reachable = self.conn_mgr.reachable_hosts();
        hosts
            .iter()
            .filter(|h| reachable.contains(&h.name))
            .copied()
            .collect()
    }

    /// Gracefully close all ControlMaster connections and clear progress bars.
    pub async fn shutdown(mut self) {
        self.progress.clear();
        self.conn_mgr.shutdown().await;
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
}
