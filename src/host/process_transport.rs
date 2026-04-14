use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::debug;

use crate::config::schema::HostEntry;
use crate::host::connection::ConnectionManager;
use crate::host::executor;
use crate::host::transport::{RemoteOutput, SshTransport};

/// SSH transport that shells out to system `ssh`/`scp` commands.
///
/// Wraps `ConnectionManager` for connection pooling (ControlMaster on Unix,
/// direct on Windows) and delegates operations to `executor` functions.
/// Internal state is protected by `tokio::sync::RwLock` so that concurrent
/// `exec`/`upload`/`download` calls only take read locks while
/// `connect`/`shutdown` take write locks without blocking the async runtime.
pub struct ProcessTransport {
    inner: RwLock<ConnectionManager>,
}

impl ProcessTransport {
    /// Create a new ProcessTransport.
    /// On Unix, allocates a temporary socket directory for ControlMaster pooling.
    /// On Windows, uses direct (non-pooled) SSH connections.
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: RwLock::new(ConnectionManager::new()?),
        })
    }
}

#[async_trait]
impl SshTransport for ProcessTransport {
    async fn connect(
        &self,
        hosts: &[&HostEntry],
        timeout: Duration,
        concurrency: usize,
    ) -> Result<Vec<String>> {
        let timeout_secs = timeout.as_secs();
        let mut mgr = self.inner.write().await;
        mgr.pre_check(hosts, timeout_secs, concurrency).await;
        Ok(mgr.reachable_hosts())
    }

    async fn exec(&self, host: &HostEntry, cmd: &str, timeout: Duration) -> Result<RemoteOutput> {
        let timeout_secs = timeout.as_secs();
        let socket_path = {
            let mgr = self.inner.read().await;
            mgr.socket_for(&host.name).map(|p| p.to_path_buf())
        };
        let out = executor::run_remote_pooled(host, cmd, timeout_secs, socket_path.as_deref())
            .await
            .context("exec via ProcessTransport failed")?;
        Ok(RemoteOutput {
            stdout: out.stdout,
            stderr: out.stderr,
            exit_code: out.exit_code,
            success: out.success,
        })
    }

    async fn upload(
        &self,
        host: &HostEntry,
        local: &Path,
        remote: &str,
        timeout: Duration,
    ) -> Result<()> {
        let timeout_secs = timeout.as_secs();
        let socket_path = {
            let mgr = self.inner.read().await;
            mgr.socket_for(&host.name).map(|p| p.to_path_buf())
        };
        executor::upload_pooled(host, local, remote, timeout_secs, socket_path.as_deref())
            .await
            .context("upload via ProcessTransport failed")
    }

    async fn download(
        &self,
        host: &HostEntry,
        remote: &str,
        local: &Path,
        timeout: Duration,
    ) -> Result<()> {
        let timeout_secs = timeout.as_secs();
        let socket_path = {
            let mgr = self.inner.read().await;
            mgr.socket_for(&host.name).map(|p| p.to_path_buf())
        };
        executor::download_pooled(host, remote, local, timeout_secs, socket_path.as_deref())
            .await
            .context("download via ProcessTransport failed")
    }

    async fn scp_probe(
        &self,
        hosts: &[&HostEntry],
        timeout: Duration,
        concurrency: usize,
    ) -> Result<Vec<String>> {
        let timeout_secs = timeout.as_secs();
        let mut mgr = self.inner.write().await;
        mgr.scp_probe(hosts, timeout_secs, concurrency).await;
        Ok(mgr.scp_capable_hosts())
    }

    fn failed_hosts(&self) -> Vec<(String, String)> {
        match self.inner.try_read() {
            Ok(mgr) => mgr.failed_hosts(),
            Err(_) => {
                debug!("failed_hosts: could not acquire read lock, returning empty");
                Vec::new()
            }
        }
    }

    fn scp_failed_hosts(&self) -> Vec<(String, String)> {
        match self.inner.try_read() {
            Ok(mgr) => mgr.scp_failed_hosts(),
            Err(_) => {
                debug!("scp_failed_hosts: could not acquire read lock, returning empty");
                Vec::new()
            }
        }
    }

    fn reachable_hosts(&self) -> Vec<String> {
        match self.inner.try_read() {
            Ok(mgr) => mgr.reachable_hosts(),
            Err(_) => {
                debug!("reachable_hosts: could not acquire read lock, returning empty");
                Vec::new()
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let mut mgr = self.inner.write().await;
        mgr.shutdown().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_transport() {
        let transport = ProcessTransport::new();
        assert!(transport.is_ok(), "ProcessTransport::new() should succeed");
    }

    #[test]
    fn test_reachable_hosts_empty_initially() {
        let transport = ProcessTransport::new().unwrap();
        assert!(
            transport.reachable_hosts().is_empty(),
            "No hosts should be reachable before connect()"
        );
    }

    #[test]
    fn test_failed_hosts_empty_initially() {
        let transport = ProcessTransport::new().unwrap();
        assert!(
            transport.failed_hosts().is_empty(),
            "No hosts should be failed before connect()"
        );
    }

    #[test]
    fn test_scp_failed_hosts_empty_initially() {
        let transport = ProcessTransport::new().unwrap();
        assert!(
            transport.scp_failed_hosts().is_empty(),
            "No hosts should have failed SCP before scp_probe()"
        );
    }

    #[test]
    fn test_transport_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ProcessTransport>();
    }

    #[test]
    fn test_trait_object_is_dyn_compatible() {
        fn assert_dyn_compatible(_: &dyn SshTransport) {}
        let transport = ProcessTransport::new().unwrap();
        assert_dyn_compatible(&transport);
    }
}
