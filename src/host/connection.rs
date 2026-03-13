use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::config::schema::HostEntry;
use crate::host::executor;

/// State of an SSH connection to a host.
#[derive(Debug, Clone)]
pub enum ConnectionState {
    Connected { socket_path: PathBuf },
    Failed { error: String },
}

/// Manages SSH ControlMaster connections for connection reuse.
/// Establishes persistent master connections during pre-check,
/// then provides socket paths for subsequent operations.
pub struct ConnectionManager {
    socket_dir: tempfile::TempDir,
    hosts: HashMap<String, ConnectionState>,
    /// Maps host name → ssh_host for shutdown/drop (ssh -O exit needs the ssh_host, not name).
    host_map: HashMap<String, String>,
    /// Hosts that passed SSH but failed SCP probe.
    scp_failed: HashMap<String, String>,
}

impl ConnectionManager {
    /// Create a new ConnectionManager with a temporary socket directory.
    /// Uses /tmp/ssync-XXXX/ to keep socket paths short (macOS ~104 byte limit).
    pub fn new() -> Result<Self> {
        let socket_dir = tempfile::Builder::new()
            .prefix("ssync-")
            .tempdir_in("/tmp")
            .context("Failed to create socket directory")?;
        Ok(Self {
            socket_dir,
            hosts: HashMap::new(),
            host_map: HashMap::new(),
            scp_failed: HashMap::new(),
        })
    }

    /// Establish ControlMaster connections to all hosts in parallel.
    /// Returns the number of successfully connected hosts.
    pub async fn pre_check(
        &mut self,
        hosts: &[&HostEntry],
        timeout_secs: u64,
        concurrency: usize,
    ) -> usize {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut handles = Vec::new();

        for host in hosts {
            let sem = semaphore.clone();
            let host = (*host).clone();
            let socket_path = self.socket_path_for(&host.name);

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = establish_master(&host, &socket_path, timeout_secs).await;
                (
                    host.name.clone(),
                    host.ssh_host.clone(),
                    socket_path,
                    result,
                )
            }));
        }

        let mut connected = 0;
        for handle in handles {
            match handle.await {
                Ok((name, ssh_host, socket_path, Ok(()))) => {
                    self.hosts
                        .insert(name.clone(), ConnectionState::Connected { socket_path });
                    self.host_map.insert(name, ssh_host);
                    connected += 1;
                }
                Ok((name, ssh_host, _, Err(e))) => {
                    self.hosts.insert(
                        name.clone(),
                        ConnectionState::Failed {
                            error: e.to_string(),
                        },
                    );
                    self.host_map.insert(name, ssh_host);
                }
                Err(e) => {
                    tracing::warn!("pre-check task panicked: {}", e);
                }
            }
        }

        connected
    }

    /// Get the socket path for a connected host, or None if not connected.
    pub fn socket_for(&self, host_name: &str) -> Option<&Path> {
        match self.hosts.get(host_name) {
            Some(ConnectionState::Connected { socket_path }) => Some(socket_path),
            _ => None,
        }
    }

    /// Get the connection state for a host.
    #[allow(dead_code)]
    pub fn state(&self, host_name: &str) -> Option<&ConnectionState> {
        self.hosts.get(host_name)
    }

    /// Get all host connection states.
    #[allow(dead_code)]
    pub fn all_states(&self) -> &HashMap<String, ConnectionState> {
        &self.hosts
    }

    /// Return names of hosts that connected successfully.
    pub fn reachable_hosts(&self) -> Vec<String> {
        self.hosts
            .iter()
            .filter_map(|(name, state)| match state {
                ConnectionState::Connected { .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Return names of hosts that failed to connect with error messages.
    pub fn failed_hosts(&self) -> Vec<(String, String)> {
        self.hosts
            .iter()
            .filter_map(|(name, state)| match state {
                ConnectionState::Failed { error } => Some((name.clone(), error.clone())),
                _ => None,
            })
            .collect()
    }

    /// Probe SCP capability on reachable hosts in parallel.
    /// Hosts that fail the probe are tracked internally and excluded from `scp_capable_hosts()`.
    /// Returns the number of hosts that passed the scp probe.
    pub async fn scp_probe(
        &mut self,
        hosts: &[&HostEntry],
        timeout_secs: u64,
        concurrency: usize,
    ) -> usize {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut handles = Vec::new();

        let reachable = self.reachable_hosts();
        for host in hosts {
            if !reachable.contains(&host.name) {
                continue;
            }
            let sem = semaphore.clone();
            let host = (*host).clone();
            let socket = self.socket_for(&host.name).map(|p| p.to_path_buf());

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = executor::scp_probe(&host, timeout_secs, socket.as_deref()).await;
                (host.name.clone(), result)
            }));
        }

        let mut passed = 0;
        for handle in handles {
            match handle.await {
                Ok((_name, Ok(()))) => {
                    passed += 1;
                }
                Ok((name, Err(e))) => {
                    self.scp_failed.insert(name, e.to_string());
                }
                Err(e) => {
                    tracing::warn!("scp probe task panicked: {}", e);
                }
            }
        }

        passed
    }

    /// Return names of hosts that failed the SCP probe with error messages.
    pub fn scp_failed_hosts(&self) -> Vec<(String, String)> {
        self.scp_failed
            .iter()
            .map(|(name, err)| (name.clone(), err.clone()))
            .collect()
    }

    /// Return names of hosts that are both SSH-reachable and SCP-capable.
    pub fn scp_capable_hosts(&self) -> Vec<String> {
        self.reachable_hosts()
            .into_iter()
            .filter(|name| !self.scp_failed.contains_key(name))
            .collect()
    }

    /// Async shutdown: gracefully close all ControlMaster connections.
    /// Preferred over Drop (which uses blocking I/O as a safety net).
    pub async fn shutdown(&mut self) {
        for (name, state) in &self.hosts {
            if let ConnectionState::Connected { socket_path } = state {
                let ssh_host = self.host_map.get(name).map(|s| s.as_str()).unwrap_or(name);
                let result = Command::new("ssh")
                    .arg("-o")
                    .arg(format!("ControlPath={}", socket_path.display()))
                    .arg("-O")
                    .arg("exit")
                    .arg(ssh_host)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .await;
                if let Err(e) = result {
                    tracing::debug!("Failed to close master for {}: {}", name, e);
                }
            }
        }
        self.hosts.clear();
    }

    /// Compute the socket path for a given host name.
    /// Uses a short hash to keep path length under macOS 104-byte limit.
    fn socket_path_for(&self, host_name: &str) -> PathBuf {
        let hash = blake3::hash(host_name.as_bytes());
        let short_hash = &hash.to_hex()[..12];
        self.socket_dir.path().join(short_hash)
    }
}

impl Drop for ConnectionManager {
    fn drop(&mut self) {
        // Safety net: try to close masters with blocking I/O
        for (name, state) in &self.hosts {
            if let ConnectionState::Connected { socket_path } = state {
                let ssh_host = self.host_map.get(name).map(|s| s.as_str()).unwrap_or(name);
                let _ = std::process::Command::new("ssh")
                    .arg("-o")
                    .arg(format!("ControlPath={}", socket_path.display()))
                    .arg("-O")
                    .arg("exit")
                    .arg(ssh_host)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .output();
            }
        }
    }
}

/// Establish a ControlMaster connection to a host.
async fn establish_master(host: &HostEntry, socket_path: &Path, timeout_secs: u64) -> Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg("-o")
            .arg("ControlMaster=yes")
            .arg("-o")
            .arg(format!("ControlPath={}", socket_path.display()))
            .arg("-o")
            .arg("ControlPersist=300")
            .arg("-N") // no remote command
            .arg("-f") // go to background after auth
            .arg(&host.ssh_host)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SSH ControlMaster timeout")?
    .context("Failed to establish ControlMaster")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ControlMaster failed: {}", stderr.trim());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_short_enough() {
        let mgr = ConnectionManager::new().unwrap();
        let path = mgr.socket_path_for("very-long-hostname.example.com");
        // /tmp/ssync-XXXXXX/123456789012 should be well under 104 bytes
        let path_str = path.to_string_lossy();
        assert!(
            path_str.len() < 104,
            "Socket path too long: {} ({} bytes)",
            path_str,
            path_str.len()
        );
    }

    #[test]
    fn test_socket_paths_unique() {
        let mgr = ConnectionManager::new().unwrap();
        let p1 = mgr.socket_path_for("host-a");
        let p2 = mgr.socket_path_for("host-b");
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_socket_paths_deterministic() {
        let mgr = ConnectionManager::new().unwrap();
        let p1 = mgr.socket_path_for("host-a");
        let p2 = mgr.socket_path_for("host-a");
        assert_eq!(p1, p2);
    }

    #[test]
    fn test_reachable_hosts_empty_initially() {
        let mgr = ConnectionManager::new().unwrap();
        assert!(mgr.reachable_hosts().is_empty());
        assert!(mgr.failed_hosts().is_empty());
    }
}
