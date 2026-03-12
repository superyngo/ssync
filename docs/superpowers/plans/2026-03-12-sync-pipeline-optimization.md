# Sync Pipeline Optimization Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Optimize the sync command with SSH connection pooling, batched metadata collection, parallel file distribution, live progress, and per-file failure isolation.

**Architecture:** Hybrid approach — Phase 0 pre-checks host connectivity via ControlMaster, Phase 1 batch-collects metadata with one SSH call per host, Phase 2 decides sources locally, Phase 3 distributes all files in parallel with dual-level concurrency (global + per-host). Failures are isolated per-file so one missing file doesn't abort the entire sync.

**Tech Stack:** Rust, tokio (async), SSH ControlMaster (connection pooling), indicatif (progress bars), rusqlite (state persistence)

**Spec:** `docs/superpowers/specs/2026-03-12-sync-pipeline-optimization-design.md`

**Deferred:** `max_per_host_concurrency = "auto"` (auto-tuning) — the config field is `usize` for now. Auto-tuning can be added later as a string-or-int enum if needed.

---

## Chunk 1: Foundation — Concurrency Limiter + Executor Pooling

### Task 1: Add `max_per_host_concurrency` to config schema

**Files:**
- Modify: `src/config/schema.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/config/schema.rs` a test that deserializes a config with `max_per_host_concurrency`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_per_host_concurrency_default() {
        let settings = Settings::default();
        assert_eq!(settings.max_per_host_concurrency, 4);
    }

    #[test]
    fn test_per_host_concurrency_from_toml() {
        let toml_str = r#"
            max_per_host_concurrency = 8
        "#;
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert_eq!(settings.max_per_host_concurrency, 8);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test config::schema::tests -v`
Expected: FAIL — `max_per_host_concurrency` field doesn't exist yet

- [ ] **Step 3: Implement the config field**

In `src/config/schema.rs`, add to `Settings`:

```rust
#[serde(default = "default_per_host_concurrency")]
pub max_per_host_concurrency: usize,
```

Add the default function:

```rust
fn default_per_host_concurrency() -> usize {
    4
}
```

Update `Settings::default()` to include:

```rust
max_per_host_concurrency: default_per_host_concurrency(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test config::schema::tests -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add max_per_host_concurrency setting

Default: 4 concurrent operations per host. Prevents overloading
SSH servers when multiple files are synced in parallel.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 2: Create ConcurrencyLimiter module

**Files:**
- Create: `src/host/concurrency.rs`
- Modify: `src/host/mod.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/host/concurrency.rs` with tests only (implementation stubs that fail):

```rust
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;

/// Dual-level concurrency limiter: global cap + per-host cap.
/// Acquire both permits before any SSH/SCP operation.
pub struct ConcurrencyLimiter {
    global: Arc<Semaphore>,
    per_host: HashMap<String, Arc<Semaphore>>,
}

impl ConcurrencyLimiter {
    /// Create a new limiter with global and per-host caps.
    /// `hosts` is the list of host names that will be used.
    pub fn new(global_limit: usize, per_host_limit: usize, hosts: &[String]) -> Self {
        todo!()
    }

    /// Acquire both global and per-host permits.
    /// Order: global first, then per-host (deterministic to prevent deadlock).
    /// Returns a guard that releases both permits on drop.
    pub async fn acquire(&self, host: &str) -> ConcurrencyPermit {
        todo!()
    }
}

/// RAII guard that holds both global and per-host semaphore permits.
pub struct ConcurrencyPermit {
    _global: tokio::sync::OwnedSemaphorePermit,
    _per_host: tokio::sync::OwnedSemaphorePermit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_global_limit_respected() {
        let hosts = vec!["a".into(), "b".into(), "c".into()];
        let limiter = ConcurrencyLimiter::new(2, 10, &hosts);

        // Acquire 2 permits (filling global limit)
        let _p1 = limiter.acquire("a").await;
        let _p2 = limiter.acquire("b").await;

        // Third acquire should not complete immediately (global limit = 2)
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            limiter.acquire("c"),
        ).await;
        assert!(result.is_err(), "Third acquire should block when global limit is 2");
    }

    #[tokio::test]
    async fn test_per_host_limit_respected() {
        let hosts = vec!["a".into()];
        let limiter = ConcurrencyLimiter::new(10, 2, &hosts);

        let _p1 = limiter.acquire("a").await;
        let _p2 = limiter.acquire("a").await;

        // Third acquire on same host should block (per-host limit = 2)
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            limiter.acquire("a"),
        ).await;
        assert!(result.is_err(), "Third acquire on same host should block when per-host limit is 2");
    }

    #[tokio::test]
    async fn test_permits_released_on_drop() {
        let hosts = vec!["a".into()];
        let limiter = ConcurrencyLimiter::new(1, 1, &hosts);

        {
            let _p = limiter.acquire("a").await;
            // permit held
        }
        // permit dropped — should be acquirable again
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            limiter.acquire("a"),
        ).await;
        assert!(result.is_ok(), "Should acquire after previous permit dropped");
    }
}
```

- [ ] **Step 2: Register module in mod.rs**

Add to `src/host/mod.rs`:

```rust
pub mod concurrency;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test host::concurrency::tests -- --nocapture`
Expected: FAIL — `todo!()` panics

- [ ] **Step 4: Implement ConcurrencyLimiter**

Replace the `todo!()` in `new` and `acquire`:

```rust
impl ConcurrencyLimiter {
    pub fn new(global_limit: usize, per_host_limit: usize, hosts: &[String]) -> Self {
        let mut per_host = HashMap::new();
        for host in hosts {
            per_host.insert(host.clone(), Arc::new(Semaphore::new(per_host_limit)));
        }
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            per_host,
        }
    }

    pub async fn acquire(&self, host: &str) -> ConcurrencyPermit {
        // Global first, then per-host (deterministic order prevents deadlock)
        let global_permit = self.global.clone().acquire_owned().await.unwrap();
        let per_host_sem = self.per_host.get(host).expect("host not registered in limiter");
        let per_host_permit = per_host_sem.clone().acquire_owned().await.unwrap();
        ConcurrencyPermit {
            _global: global_permit,
            _per_host: per_host_permit,
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test host::concurrency::tests -- --nocapture`
Expected: ALL PASS

- [ ] **Step 6: Run full test suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 7: Commit**

```bash
git add src/host/concurrency.rs src/host/mod.rs
git commit -m "feat(host): add dual-level ConcurrencyLimiter

Global semaphore + per-host semaphore to prevent overloading
individual SSH servers during parallel multi-file sync.
Deterministic acquisition order (global then per-host) prevents deadlock.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 3: Add pooled executor functions (ControlPath-aware)

**Files:**
- Modify: `src/host/executor.rs`

- [ ] **Step 1: Add pooled variants to executor**

Add these functions to `src/host/executor.rs` (after existing functions):

```rust
use std::path::PathBuf;

/// Build common SSH args including optional ControlPath.
fn ssh_base_args(host: &HostEntry, timeout_secs: u64, socket: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(), "BatchMode=yes".to_string(),
        "-o".to_string(), format!("ConnectTimeout={}", timeout_secs),
    ];
    if let Some(sock) = socket {
        args.push("-o".to_string());
        args.push(format!("ControlPath={}", sock.display()));
    }
    args
}

/// Execute a command on a remote host, optionally reusing a ControlMaster socket.
pub async fn run_remote_pooled(
    host: &HostEntry,
    command: &str,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<RemoteOutput> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .args(&args)
            .arg(&host.ssh_host)
            .arg("--")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SSH connection timeout")?
    .context("Failed to execute ssh")?;

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        success: output.status.success(),
    })
}

/// Upload a file via scp, optionally reusing a ControlMaster socket.
pub async fn upload_pooled(
    host: &HostEntry,
    local_path: &Path,
    remote_path: &str,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<()> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("scp")
            .args(&args)
            .arg(local_path.as_os_str())
            .arg(format!("{}:{}", host.ssh_host, remote_path))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SCP timeout")?
    .context("Failed to execute scp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("scp upload failed: {}", stderr.trim());
    }
    Ok(())
}

/// Download a file via scp, optionally reusing a ControlMaster socket.
pub async fn download_pooled(
    host: &HostEntry,
    remote_path: &str,
    local_path: &Path,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<()> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("scp")
            .args(&args)
            .arg(format!("{}:{}", host.ssh_host, remote_path))
            .arg(local_path.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SCP timeout")?
    .context("Failed to execute scp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("scp download failed: {}", stderr.trim());
    }
    Ok(())
}
```

- [ ] **Step 2: Run existing tests + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass (new functions aren't called by old code yet)

- [ ] **Step 3: Commit**

```bash
git add src/host/executor.rs
git commit -m "feat(executor): add pooled SSH/SCP functions with ControlPath support

New run_remote_pooled, upload_pooled, download_pooled accept an optional
ControlMaster socket path. Extracted ssh_base_args helper to DRY argument building.
Existing functions unchanged for backward compatibility.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 4: Create SSH ConnectionManager

**Files:**
- Create: `src/host/connection.rs`
- Modify: `src/host/mod.rs`

- [ ] **Step 1: Write the module with tests**

Create `src/host/connection.rs`:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::config::schema::HostEntry;

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
                (host.name.clone(), host.ssh_host.clone(), socket_path, result)
            }));
        }

        let mut connected = 0;
        for handle in handles {
            match handle.await {
                Ok((name, ssh_host, socket_path, Ok(()))) => {
                    self.hosts.insert(name.clone(), ConnectionState::Connected { socket_path });
                    self.host_map.insert(name, ssh_host);
                    connected += 1;
                }
                Ok((name, ssh_host, _, Err(e))) => {
                    self.hosts.insert(name.clone(), ConnectionState::Failed { error: e.to_string() });
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
    pub fn state(&self, host_name: &str) -> Option<&ConnectionState> {
        self.hosts.get(host_name)
    }

    /// Get all host connection states.
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
        // Use first 12 chars of blake3 hash to avoid collisions while keeping path short
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
async fn establish_master(
    host: &HostEntry,
    socket_path: &Path,
    timeout_secs: u64,
) -> Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs))
            .arg("-o").arg("ControlMaster=yes")
            .arg("-o").arg(format!("ControlPath={}", socket_path.display()))
            .arg("-o").arg("ControlPersist=300")
            .arg("-N")  // no remote command
            .arg("-f")  // go to background after auth
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
        assert!(path_str.len() < 104, "Socket path too long: {} ({} bytes)", path_str, path_str.len());
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
```

- [ ] **Step 2: Register module in mod.rs**

Add to `src/host/mod.rs`:

```rust
pub mod connection;
```

- [ ] **Step 3: Run tests**

Run: `cargo test host::connection::tests -- --nocapture`
Expected: ALL PASS

- [ ] **Step 4: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 5: Commit**

```bash
git add src/host/connection.rs src/host/mod.rs
git commit -m "feat(host): add SSH ConnectionManager with ControlMaster pooling

Manages persistent SSH master connections for connection reuse.
Pre-check establishes masters in parallel, subsequent operations
reuse them via ControlPath. Uses blake3-hashed socket paths to
stay under macOS 104-byte limit. Async shutdown with blocking Drop safety net.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Chunk 2: Summary Enhancement + Per-File Failure Isolation

### Task 5: Enhance Summary with skip_reasons

**Files:**
- Modify: `src/output/summary.rs`

- [ ] **Step 1: Write the failing test**

Add tests to `src/output/summary.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skip_reason_recorded() {
        let mut s = Summary::default();
        s.add_skip_with_reason("~/.bashrc", "host-a", "source 'host-a' does not have '~/.bashrc'");
        assert_eq!(s.skipped, 1);
        assert_eq!(s.skip_reasons.len(), 1);
        assert_eq!(s.skip_reasons[0].path, "~/.bashrc");
        assert_eq!(s.skip_reasons[0].host, "host-a");
    }

    #[test]
    fn test_summary_counts() {
        let mut s = Summary::default();
        s.add_success();
        s.add_success();
        s.add_failure("host-x", "timeout");
        s.add_skip_with_reason("~/f", "h", "missing");
        assert_eq!(s.succeeded, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.skipped, 1);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test output::summary::tests -- --nocapture`
Expected: FAIL — `add_skip_with_reason` and `skip_reasons` don't exist

- [ ] **Step 3: Implement**

Update `src/output/summary.rs`:

```rust
/// Reason for skipping a file during sync.
#[derive(Debug)]
pub struct SkipReason {
    pub path: String,
    pub host: String,
    pub reason: String,
}

/// Execution summary for a batch operation.
#[derive(Default)]
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>,
    pub skip_reasons: Vec<SkipReason>,
}

impl Summary {
    pub fn add_success(&mut self) {
        self.succeeded += 1;
    }

    pub fn add_failure(&mut self, host: &str, message: &str) {
        self.failed += 1;
        self.errors.push((host.to_string(), message.to_string()));
    }

    pub fn add_skip(&mut self) {
        self.skipped += 1;
    }

    pub fn add_skip_with_reason(&mut self, path: &str, host: &str, reason: &str) {
        self.skipped += 1;
        self.skip_reasons.push(SkipReason {
            path: path.to_string(),
            host: host.to_string(),
            reason: reason.to_string(),
        });
    }

    pub fn print(&self) {
        println!();
        println!("── Summary ──────────────────────────────");
        print!("  {} succeeded", self.succeeded);
        if self.failed > 0 {
            print!("  {} failed", self.failed);
        }
        if self.skipped > 0 {
            print!("  {} skipped", self.skipped);
        }
        println!();

        if !self.errors.is_empty() {
            println!("  Errors:");
            for (host, msg) in &self.errors {
                println!("    {}: {}", host, msg);
            }
        }

        if !self.skip_reasons.is_empty() {
            println!("  Skipped:");
            for sr in &self.skip_reasons {
                println!("    {} ({}): {}", sr.path, sr.host, sr.reason);
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test output::summary::tests -- --nocapture`
Expected: ALL PASS

- [ ] **Step 5: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 6: Commit**

```bash
git add src/output/summary.rs
git commit -m "feat(summary): add SkipReason tracking for per-file skip reporting

New add_skip_with_reason() records path, host, and reason for skipped files.
Print method shows skip details alongside errors.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 6: Fix make_decisions_fixed_source to skip instead of abort

**Files:**
- Modify: `src/commands/sync.rs`

- [ ] **Step 1: Write the failing test**

Add test module to `src/commands/sync.rs` (before the closing of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_file_info(host: &str, hash: &str, mtime: i64) -> FileInfo {
        FileInfo {
            host: host.to_string(),
            path: "~/.bashrc".to_string(),
            mtime,
            size: 100,
            hash: hash.to_string(),
        }
    }

    #[test]
    fn test_fixed_source_missing_returns_empty_not_error() {
        let infos = vec![
            make_file_info("host-b", "abc123", 1000),
            make_file_info("host-c", "abc123", 1000),
        ];
        // Source "host-a" is NOT in infos — should return Ok(empty), not Err
        let result = make_decisions_fixed_source(
            &infos,
            "~/.bashrc",
            false,
            &[],
            "host-a",
        );
        assert!(result.is_ok());
        let (decisions, skip_info) = result.unwrap();
        assert!(decisions.is_empty());
        assert!(skip_info.is_some(), "Should return skip info when source missing");
        let (skip_source, skip_path) = skip_info.unwrap();
        assert_eq!(skip_source, "host-a");
        assert_eq!(skip_path, "~/.bashrc");
    }

    #[test]
    fn test_fixed_source_present_works_normally() {
        let infos = vec![
            make_file_info("host-a", "abc123", 2000),
            make_file_info("host-b", "def456", 1000),
        ];
        let result = make_decisions_fixed_source(
            &infos,
            "~/.bashrc",
            false,
            &[],
            "host-a",
        );
        assert!(result.is_ok());
        let (decisions, skip_info) = result.unwrap();
        assert!(skip_info.is_none());
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].source_host, "host-a");
        assert_eq!(decisions[0].target_hosts, vec!["host-b"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test commands::sync::tests -- --nocapture`
Expected: FAIL — return type doesn't match (current returns `Result<Vec<SyncDecision>>`, test expects tuple)

- [ ] **Step 3: Change make_decisions_fixed_source return type**

Update the function signature and body in `src/commands/sync.rs`:

```rust
/// Make sync decisions using a fixed source host (bypasses conflict strategy).
/// Returns (decisions, optional_skip_info).
/// When source host lacks the file, returns Ok((empty, Some((source_name, path))))
/// instead of Err — allowing the caller to skip gracefully.
fn make_decisions_fixed_source(
    file_infos: &[FileInfo],
    path: &str,
    push_missing: bool,
    missing_hosts: &[String],
    source_name: &str,
) -> Result<(Vec<SyncDecision>, Option<(String, String)>)> {
    let source = match file_infos.iter().find(|f| f.host == source_name) {
        Some(s) => s,
        None => {
            // Source host doesn't have this file — skip, don't abort
            return Ok((Vec::new(), Some((source_name.to_string(), path.to_string()))));
        }
    };

    let mut targets: Vec<String> = file_infos
        .iter()
        .filter(|f| f.host != source.host)
        .filter(|f| f.hash != source.hash)
        .map(|f| f.host.clone())
        .collect();

    if push_missing {
        for h in missing_hosts {
            if !targets.contains(h) {
                targets.push(h.clone());
            }
        }
    }

    if targets.is_empty() {
        return Ok((Vec::new(), None));
    }

    let synced_hosts: Vec<String> = file_infos
        .iter()
        .filter(|f| f.host != source.host && f.hash == source.hash)
        .map(|f| f.host.clone())
        .collect();

    let mut reason = format!("fixed source: {}", source_name);
    if push_missing && !missing_hosts.is_empty() {
        reason.push_str(&format!(", +{} missing", missing_hosts.len()));
    }

    Ok((vec![SyncDecision {
        path: path.to_string(),
        source_host: source.host.clone(),
        target_hosts: targets,
        synced_hosts,
        reason,
    }], None))
}
```

- [ ] **Step 4: Update the caller in sync_path_across**

In `sync_path_across`, update the call site from:

```rust
    let decisions = if let Some(src) = source_override {
        make_decisions_fixed_source(
            &collect_result.found,
            path,
            push_missing,
            &collect_result.missing,
            src,
        )?
    } else {
```

To:

```rust
    let decisions = if let Some(src) = source_override {
        let (decs, skip_info) = make_decisions_fixed_source(
            &collect_result.found,
            path,
            push_missing,
            &collect_result.missing,
            src,
        )?;
        if let Some((skip_source, skip_path)) = skip_info {
            let available: Vec<&str> = collect_result.found.iter().map(|f| f.host.as_str()).collect();
            let msg = format!(
                "source '{}' does not have '{}'. Available: [{}]",
                skip_source, skip_path, available.join(", ")
            );
            printer::print_host_line("skipped", "skip", &format!("{}: {}", skip_source, msg));
            summary.add_skip_with_reason(&skip_path, &skip_source, &msg);
            return Ok(());
        }
        decs
    } else {
```

- [ ] **Step 5: Run tests**

Run: `cargo test commands::sync::tests -- --nocapture`
Expected: ALL PASS

- [ ] **Step 6: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 7: Commit**

```bash
git add src/commands/sync.rs
git commit -m "fix(sync): skip file when fixed source host lacks it instead of aborting

make_decisions_fixed_source now returns Ok((empty, skip_info)) instead of
Err when the source host doesn't have the file. The caller prints a skip
message and continues to the next file. Previously this aborted the
entire sync operation.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Chunk 3: Batched Metadata Collection

### Task 7: Build batch metadata command generator

**Files:**
- Modify: `src/commands/sync.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/commands/sync.rs`:

```rust
    #[test]
    fn test_build_batch_metadata_cmd_sh() {
        let paths = vec!["~/.bashrc".to_string(), "~/.vimrc".to_string()];
        let cmd = build_batch_metadata_cmd(&paths, ShellType::Sh);
        assert!(cmd.contains("---FILE:"));
        assert!(cmd.contains("stat"));
        assert!(cmd.contains("sha256sum"));
        // Verify all paths are included
        assert!(cmd.contains(".bashrc"));
        assert!(cmd.contains(".vimrc"));
    }

    #[test]
    fn test_build_batch_metadata_cmd_powershell() {
        let paths = vec!["~/.bashrc".to_string()];
        let cmd = build_batch_metadata_cmd(&paths, ShellType::PowerShell);
        assert!(cmd.contains("---FILE:"));
        assert!(cmd.contains("Get-Item"));
        assert!(cmd.contains("Get-FileHash"));
    }

    #[test]
    fn test_build_batch_metadata_cmd_escapes_special_chars() {
        let paths = vec!["~/file with spaces.txt".to_string(), "~/it's a file.txt".to_string()];
        let cmd = build_batch_metadata_cmd(&paths, ShellType::Sh);
        // Single quotes with escaped apostrophes
        assert!(cmd.contains("'\\''"));
    }

    #[test]
    fn test_parse_batch_metadata_output() {
        let output = "\
---FILE:$HOME/.bashrc
1700000000 1234
abc123def456  /home/user/.bashrc
---FILE:$HOME/.vimrc
MISSING
";
        let paths = vec!["~/.bashrc".to_string(), "~/.vimrc".to_string()];
        let result = parse_batch_metadata_output(output, &paths, "host-a");
        assert_eq!(result.len(), 2);

        let bashrc = &result["~/.bashrc"];
        assert!(bashrc.found.is_some());
        let fi = bashrc.found.as_ref().unwrap();
        assert_eq!(fi.mtime, 1700000000);
        assert_eq!(fi.hash, "abc123def456");

        let vimrc = &result["~/.vimrc"];
        assert!(vimrc.found.is_none());
        assert!(vimrc.is_missing);
    }

    #[test]
    fn test_parse_batch_metadata_nohash() {
        let output = "\
---FILE:$HOME/.bashrc
1700000000 1234
NOHASH
";
        let paths = vec!["~/.bashrc".to_string()];
        let result = parse_batch_metadata_output(output, &paths, "host-a");
        let bashrc = &result["~/.bashrc"];
        assert!(bashrc.found.is_some());
        let fi = bashrc.found.as_ref().unwrap();
        assert_eq!(fi.mtime, 1700000000);
        assert!(fi.hash.is_empty(), "NOHASH sentinel should produce empty hash");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test commands::sync::tests -- --nocapture`
Expected: FAIL — functions don't exist

- [ ] **Step 3: Implement build_batch_metadata_cmd**

Add to `src/commands/sync.rs`:

```rust
/// Build a single shell command that collects metadata for all files at once.
/// Output uses ---FILE:<path> markers to delimit per-file results.
fn build_batch_metadata_cmd(paths: &[String], shell: ShellType) -> String {
    match shell {
        ShellType::PowerShell => {
            let items: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("\"$HOME\\{}\"", stripped.replace('/', "\\"))
                    } else {
                        format!("\"{}\"", p)
                    }
                })
                .collect();
            format!(
                "foreach ($f in @({items})) {{ \
                    \"---FILE:$f\"; \
                    $i=Get-Item $f -ErrorAction SilentlyContinue; \
                    if ($i) {{ \
                        [int64](($i.LastWriteTimeUtc-[datetime]\"1970-01-01\").TotalSeconds), $i.Length -join \" \"; \
                        (Get-FileHash $f -Algorithm SHA256).Hash.ToLower() \
                    }} else {{ \"MISSING\" }} \
                }}",
                items = items.join(",")
            )
        }
        ShellType::Sh | ShellType::Cmd => {
            let items: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("$HOME/'{}'", stripped.replace('\'', "'\\''"))
                    } else {
                        format!("'{}'", p.replace('\'', "'\\''"))
                    }
                })
                .collect();
            format!(
                "for f in {items}; do \
                    echo \"---FILE:$f\"; \
                    stat -c '%Y %s' \"$f\" 2>/dev/null || stat -f '%m %z' \"$f\" 2>/dev/null || echo \"MISSING\"; \
                    (sha256sum \"$f\" 2>/dev/null || shasum -a 256 \"$f\" 2>/dev/null) || echo \"NOHASH\"; \
                done",
                items = items.join(" ")
            )
        }
    }
}
```

- [ ] **Step 4: Implement parse_batch_metadata_output**

Add helper structs and the parsing function:

```rust
/// Per-file result from batch metadata parsing.
struct SingleFileResult {
    found: Option<FileInfo>,
    is_missing: bool,
}

/// Parse the output of a batch metadata command.
/// Splits by ---FILE: markers and parses each block.
fn parse_batch_metadata_output(
    output: &str,
    paths: &[String],
    host_name: &str,
) -> HashMap<String, SingleFileResult> {
    let mut results = HashMap::new();

    // Split output by ---FILE: markers
    let blocks: Vec<&str> = output.split("---FILE:").collect();

    // First element is empty or pre-marker output — skip
    for (block_idx, block) in blocks.iter().skip(1).enumerate() {
        let lines: Vec<&str> = block.lines().collect();
        if lines.is_empty() {
            continue;
        }

        // First line after marker contains the expanded path
        let _expanded_path = lines[0].trim();

        if block_idx >= paths.len() {
            continue;
        }
        let canonical_path = &paths[block_idx];

        // Remaining lines: stat output + hash
        let data_lines: Vec<&str> = lines[1..].iter().copied().collect();

        if data_lines.first().map(|l| l.trim()) == Some("MISSING") {
            results.insert(
                canonical_path.clone(),
                SingleFileResult { found: None, is_missing: true },
            );
            continue;
        }

        // Parse stat line
        let stat_parts: Vec<&str> = data_lines
            .first()
            .map(|l| l.split_whitespace().collect())
            .unwrap_or_default();
        let mtime: Option<i64> = stat_parts.first().and_then(|s| s.parse().ok());
        let size: u64 = stat_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        if let Some(mtime) = mtime {
            let hash = data_lines
                .get(1)
                .and_then(|l| l.split_whitespace().next())
                .unwrap_or("")
                .to_string();

            // Skip "NOHASH" sentinel
            let hash = if hash == "NOHASH" { String::new() } else { hash };

            results.insert(
                canonical_path.clone(),
                SingleFileResult {
                    found: Some(FileInfo {
                        host: host_name.to_string(),
                        path: canonical_path.clone(),
                        mtime,
                        size,
                        hash,
                    }),
                    is_missing: false,
                },
            );
        } else {
            results.insert(
                canonical_path.clone(),
                SingleFileResult { found: None, is_missing: true },
            );
        }
    }

    // Fill in any paths that had no output block (host unreachable scenario handled elsewhere)
    for path in paths {
        results.entry(path.clone()).or_insert(SingleFileResult {
            found: None,
            is_missing: false,
        });
    }

    results
}
```

- [ ] **Step 5: Add HashMap import if needed**

Ensure `use std::collections::HashMap;` is at the top of `sync.rs`.

- [ ] **Step 6: Run tests**

Run: `cargo test commands::sync::tests -- --nocapture`
Expected: ALL PASS

- [ ] **Step 7: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 8: Commit**

```bash
git add src/commands/sync.rs
git commit -m "feat(sync): add batched metadata command builder and parser

build_batch_metadata_cmd generates a single shell command to stat+hash
all files on a host (Sh and PowerShell variants). parse_batch_metadata_output
splits output by ---FILE: markers. Reduces SSH calls from hosts*files to just hosts.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 8: Implement batch_collect_all_metadata function

**Files:**
- Modify: `src/commands/sync.rs`

- [ ] **Step 1: Implement the batch collection function**

Add to `src/commands/sync.rs`:

```rust
use std::collections::HashMap;

/// Result of batch metadata collection across all files and hosts.
struct BatchCollectResult {
    /// Per-file results: path → CollectResult (found + missing hosts)
    per_file: HashMap<String, CollectResult>,
    /// Hosts where SSH itself failed (excluded from all file results)
    unreachable_hosts: Vec<String>,
}

/// Stage 1 (Batched): Collect metadata for ALL files from all hosts with one SSH call per host.
async fn batch_collect_all_metadata(
    hosts: &[&HostEntry],
    paths: &[String],
    timeout: u64,
    concurrency: usize,
    conn_mgr: &ConnectionManager,
) -> Result<BatchCollectResult> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for host in hosts {
        // Skip hosts that failed pre-check
        let socket = match conn_mgr.socket_for(&host.name) {
            Some(s) => Some(s.to_path_buf()),
            None => continue,  // unreachable host
        };

        let sem = semaphore.clone();
        let host = (*host).clone();
        let paths = paths.to_vec();
        let cmd = build_batch_metadata_cmd(&paths, host.shell);

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = executor::run_remote_pooled(
                &host,
                &cmd,
                timeout,
                socket.as_deref(),
            ).await;

            match result {
                Ok(output) => {
                    let parsed = parse_batch_metadata_output(&output.stdout, &paths, &host.name);
                    (host.name.clone(), Some(parsed), false)
                }
                Err(_) => {
                    (host.name.clone(), None, true)
                }
            }
        }));
    }

    // Aggregate results per file
    let mut per_file: HashMap<String, CollectResult> = HashMap::new();
    for path in paths {
        per_file.insert(path.clone(), CollectResult {
            found: Vec::new(),
            missing: Vec::new(),
        });
    }
    let mut unreachable_hosts = Vec::new();

    for handle in handles {
        let (host_name, parsed_opt, is_unreachable) = handle.await?;
        if is_unreachable {
            unreachable_hosts.push(host_name);
            continue;
        }
        if let Some(parsed) = parsed_opt {
            for (path, single) in parsed {
                if let Some(collect) = per_file.get_mut(&path) {
                    if let Some(fi) = single.found {
                        collect.found.push(fi);
                    } else if single.is_missing {
                        collect.missing.push(host_name.clone());
                    }
                }
            }
        }
    }

    Ok(BatchCollectResult {
        per_file,
        unreachable_hosts,
    })
}
```

- [ ] **Step 2: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 3: Commit**

```bash
git add src/commands/sync.rs
git commit -m "feat(sync): implement batch_collect_all_metadata

Collects file metadata for all paths from all hosts using one SSH call
per host. Uses ControlMaster socket for connection reuse. Aggregates
results into per-file CollectResult maps.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Chunk 4: Progress Display + Pipeline Integration

### Task 9: Create indicatif progress display module

**Files:**
- Create: `src/output/progress.rs`
- Modify: `src/output/mod.rs`

- [ ] **Step 1: Create the progress module**

Create `src/output/progress.rs`:

```rust
use std::collections::HashMap;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Live progress display for the sync pipeline.
/// Falls back to no-op when stderr is not a TTY.
pub struct SyncProgress {
    multi: MultiProgress,
    host_bar: Option<ProgressBar>,
    collect_bar: Option<ProgressBar>,
    transfer_bars: HashMap<String, ProgressBar>,
    is_tty: bool,
}

impl SyncProgress {
    pub fn new() -> Self {
        let is_tty = atty_check();
        Self {
            multi: MultiProgress::new(),
            host_bar: None,
            collect_bar: None,
            transfer_bars: HashMap::new(),
            is_tty,
        }
    }

    /// Start the host pre-check progress bar.
    pub fn start_host_check(&mut self, total: u64) {
        if !self.is_tty {
            return;
        }
        let style = ProgressStyle::default_bar()
            .template(" {prefix:>12} {bar:30.cyan/dim} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("██░");
        let bar = self.multi.add(ProgressBar::new(total));
        bar.set_style(style);
        bar.set_prefix("Hosts");
        self.host_bar = Some(bar);
    }

    /// Update host check progress.
    pub fn host_checked(&self, connected: usize, failed: usize) {
        if let Some(bar) = &self.host_bar {
            bar.inc(1);
            bar.set_message(format!("{} connected, {} failed", connected, failed));
        }
    }

    /// Finish host check bar.
    pub fn finish_host_check(&self, connected: usize, failed: usize) {
        if let Some(bar) = &self.host_bar {
            bar.set_message(format!("{} connected, {} failed", connected, failed));
            bar.finish();
        }
    }

    /// Start the metadata collection progress bar.
    pub fn start_collect(&mut self, total: u64) {
        if !self.is_tty {
            return;
        }
        let style = ProgressStyle::default_bar()
            .template(" {prefix:>12} {bar:30.green/dim} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("██░");
        let bar = self.multi.add(ProgressBar::new(total));
        bar.set_style(style);
        bar.set_prefix("Collecting");
        bar.set_message("metadata...");
        self.collect_bar = Some(bar);
    }

    /// Update collection progress (one host responded).
    pub fn host_collected(&self) {
        if let Some(bar) = &self.collect_bar {
            bar.inc(1);
        }
    }

    /// Finish collection bar.
    pub fn finish_collect(&self) {
        if let Some(bar) = &self.collect_bar {
            bar.finish_with_message("done");
        }
    }

    /// Start a transfer progress bar for a file.
    pub fn start_transfer(&mut self, path: &str, total_targets: u64) {
        if !self.is_tty {
            return;
        }
        let style = ProgressStyle::default_bar()
            .template(" {prefix:>12} {bar:30.yellow/dim} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("██░");
        let bar = self.multi.add(ProgressBar::new(total_targets));
        bar.set_style(style);
        bar.set_prefix("Transfer");
        bar.set_message(path.to_string());
        self.transfer_bars.insert(path.to_string(), bar);
    }

    /// Update transfer progress for a file.
    pub fn target_transferred(&self, path: &str) {
        if let Some(bar) = self.transfer_bars.get(path) {
            bar.inc(1);
        }
    }

    /// Finish transfer bar for a file.
    pub fn finish_transfer(&self, path: &str) {
        if let Some(bar) = self.transfer_bars.get(path) {
            bar.finish_and_clear();
        }
    }

    /// Clear all bars (call before printing final summary).
    pub fn clear(&self) {
        self.multi.clear().ok();
    }
}

/// Check if stderr is a TTY (used to decide whether to show progress bars).
fn atty_check() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_non_tty_no_panic() {
        // In test environment, stderr is typically not a TTY
        let mut p = SyncProgress::new();
        p.start_host_check(5);
        p.host_checked(1, 0);
        p.finish_host_check(5, 0);
        p.start_collect(3);
        p.host_collected();
        p.finish_collect();
        p.start_transfer("~/.bashrc", 4);
        p.target_transferred("~/.bashrc");
        p.finish_transfer("~/.bashrc");
        p.clear();
        // Should not panic even in non-TTY environment
    }
}
```

- [ ] **Step 2: Register module**

Add to `src/output/mod.rs`:

```rust
pub mod progress;
```

- [ ] **Step 3: Run tests**

Run: `cargo test output::progress::tests -- --nocapture`
Expected: PASS

- [ ] **Step 4: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass

- [ ] **Step 5: Commit**

```bash
git add src/output/progress.rs src/output/mod.rs
git commit -m "feat(output): add indicatif-based SyncProgress display

Multi-progress bars for host pre-check, metadata collection, and
per-file transfer. Falls back to no-op on non-TTY stderr.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 10: Rewrite sync::run() to use the new pipeline

**Files:**
- Modify: `src/commands/sync.rs`

This is the largest task — it rewires the main `run()` and `sync_path_across()` to use:
- ConnectionManager pre-check
- Batched metadata collection
- ConcurrencyLimiter
- SyncProgress bars
- Per-file failure isolation

- [ ] **Step 1: Add new imports at top of sync.rs**

```rust
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;

use crate::config::schema::{ConflictStrategy, HostEntry, ShellType};
use crate::host::concurrency::ConcurrencyLimiter;
use crate::host::connection::ConnectionManager;
use crate::host::executor;
use crate::output::printer;
use crate::output::progress::SyncProgress;
use crate::output::summary::Summary;

use super::Context;
```

- [ ] **Step 2: Rewrite run() function**

The new `run()` implements the 10-step pipeline from the spec:

**Both ad-hoc (`--files/-f`) and config-based paths use the new pipeline.** The unified flow:

```rust
pub async fn run(ctx: &Context, dry_run: bool, files: &[String], no_push_missing: bool, cli_source: Option<&str>) -> Result<()> {
    let push_missing = !no_push_missing;
    let hosts = ctx.resolve_hosts()?;
    let mut summary = Summary::default();
    let mut progress = SyncProgress::new();

    // Step 1: Collect all file paths
    let (all_paths, recursive_entries) = if !files.is_empty() {
        // Ad-hoc mode: convert paths to tilde form
        let paths: Vec<String> = files.iter().map(|p| to_tilde_path(p)).collect();
        (paths, Vec::new())
    } else {
        // Config mode: separate recursive vs non-recursive
        let sync_entries = ctx.resolve_syncs();
        let mut paths = Vec::new();
        let mut recursive = Vec::new();
        for entry in &sync_entries {
            let effective_source = cli_source.or(entry.source.as_deref());
            if entry.recursive {
                recursive.push((entry, effective_source));
            } else {
                paths.extend(entry.paths.iter().cloned());
            }
        }
        (paths, recursive)
    };

    // Step 2: Pre-check connections
    let mut conn_mgr = ConnectionManager::new()?;
    progress.start_host_check(hosts.len() as u64);
    let connected = conn_mgr.pre_check(&hosts, ctx.timeout, ctx.concurrency()).await;
    let failed = hosts.len() - connected;
    progress.finish_host_check(connected, failed);

    // Report unreachable hosts
    for (name, err) in conn_mgr.failed_hosts() {
        printer::print_host_line("unreachable", "error", &format!("{}: {}", name, err));
        summary.add_failure(&name, &err);
    }

    // Step 3: Filter to reachable hosts
    if let Some(src) = cli_source {
        if conn_mgr.socket_for(src).is_none() {
            anyhow::bail!("Source host '{}' is unreachable", src);
        }
    }
    let reachable_names = conn_mgr.reachable_hosts();
    let reachable_hosts: Vec<&HostEntry> = hosts.iter()
        .filter(|h| reachable_names.contains(&h.name))
        .copied()
        .collect();

    if reachable_hosts.len() < 2 && all_paths.len() > 0 {
        println!("Need at least 2 reachable hosts for sync (found {})", reachable_hosts.len());
        conn_mgr.shutdown().await;
        return Ok(());
    }

    // Step 4: Batch collect metadata (non-recursive files)
    if !all_paths.is_empty() {
        progress.start_collect(reachable_hosts.len() as u64);
        let batch_result = batch_collect_all_metadata(
            &reachable_hosts, &all_paths, ctx.timeout, ctx.concurrency(), &conn_mgr,
        ).await?;
        progress.finish_collect();

        // Step 5: Make decisions per file
        let mut all_decisions: Vec<(String, Vec<SyncDecision>)> = Vec::new();
        for path in &all_paths {
            if let Some(collect) = batch_result.per_file.get(path) {
                let effective_source = cli_source;
                let decisions = if let Some(src) = effective_source {
                    let (decs, skip_info) = make_decisions_fixed_source(
                        &collect.found, path, push_missing, &collect.missing, src,
                    )?;
                    if let Some((skip_source, skip_path)) = skip_info {
                        let available: Vec<&str> = collect.found.iter().map(|f| f.host.as_str()).collect();
                        let msg = format!("source '{}' does not have '{}'. Available: [{}]",
                            skip_source, skip_path, available.join(", "));
                        printer::print_host_line("skipped", "skip", &format!("{}: {}", skip_source, msg));
                        summary.add_skip_with_reason(&skip_path, &skip_source, &msg);
                        continue;
                    }
                    decs
                } else {
                    make_decisions(&collect.found, &ctx.config.settings.conflict_strategy,
                        path, push_missing, &collect.missing)
                };
                if !decisions.is_empty() {
                    all_decisions.push((path.clone(), decisions));
                }
            }
        }

        // Step 6: Dry-run check
        if dry_run {
            for (path, decisions) in &all_decisions {
                for d in decisions {
                    println!("  {} → source: {} → targets: [{}] ({})",
                        d.path, d.source_host,
                        d.target_hosts.join(", "), d.reason);
                }
            }
            println!("  [dry-run] No changes applied.");
        } else {
            // Step 7: Distribute all files in parallel
            let host_names: Vec<String> = reachable_hosts.iter().map(|h| h.name.clone()).collect();
            let limiter = ConcurrencyLimiter::new(
                ctx.concurrency(), ctx.config.settings.max_per_host_concurrency, &host_names,
            );

            for (_path, decisions) in &all_decisions {
                for decision in decisions {
                    // ... distribute_pooled with limiter, conn_mgr, progress
                    // ... update summary on success/failure
                    // Step 8: Update DB state on success
                }
            }
        }
    }

    // Handle recursive entries with existing per-file flow (unchanged)
    for (entry, effective_source) in &recursive_entries {
        for path in &entry.paths {
            sync_path_across(ctx, &reachable_hosts, path, "recursive",
                dry_run, push_missing, *effective_source, &mut summary).await?;
        }
    }

    // Step 9: Cleanup
    conn_mgr.shutdown().await;

    // Step 10: Print summary
    summary.print();
    Ok(())
}
```

Key implementation details:
- Ad-hoc (`--files/-f`) and config paths both enter the same batched pipeline
- Recursive entries fall back to existing per-file `collect_file_metadata` + `sync_path_across`
- The `distribute` function is updated to accept `ConcurrencyLimiter` and `ConnectionManager`
- Progress bars are updated at each phase

- [ ] **Step 3: Update distribute() to use pooled executor and ConcurrencyLimiter**

Update the `distribute` function signature to accept socket paths and the limiter:

```rust
async fn distribute_pooled(
    hosts: &[&HostEntry],
    decision: &SyncDecision,
    timeout: u64,
    limiter: &ConcurrencyLimiter,
    conn_mgr: &ConnectionManager,
    progress: &SyncProgress,
) -> Result<(Vec<String>, Vec<(String, String)>)> {
    // Similar to existing distribute() but:
    // 1. Uses executor::download_pooled with socket
    // 2. Uses executor::upload_pooled with socket
    // 3. Uses limiter.acquire() instead of raw Semaphore
    // 4. Updates progress bars
    ...
}
```

- [ ] **Step 4: Run full suite + clippy**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: No warnings, all tests pass. Existing tests for sync decision logic still pass.

- [ ] **Step 5: Manual testing notes**

Test scenarios to verify manually (not automated — requires SSH hosts):
- `ssync sync -a` with multiple files: should see progress bars, batch collection
- `ssync sync -a --source host-a` with file missing on host-a: should skip, not abort
- `ssync sync -a --dry-run`: should show decisions without transferring
- `ssync sync -a --serial`: should work with concurrency=1

- [ ] **Step 6: Commit**

```bash
git add src/commands/sync.rs
git commit -m "feat(sync): rewrite pipeline with batched collection and parallel distribution

Complete pipeline rewrite implementing the 10-step optimized flow:
- SSH ControlMaster connection pooling via ConnectionManager
- Batched metadata collection (1 SSH call per host for all files)
- Parallel file distribution with dual-level ConcurrencyLimiter
- Live progress bars via indicatif SyncProgress
- Per-file failure isolation (source missing file → skip, not abort)
- Recursive entries fall back to existing per-file collection
- DB state updates preserved (sync_state + operation_log)
- --dry-run support preserved

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Chunk 5: Final Verification + Cleanup

### Task 11: Final integration test and cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run complete verification**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
cargo build --no-default-features
```

Expected: All pass.

- [ ] **Step 2: Review for dead code**

Check if the original `collect_file_metadata` function is still needed (yes — used by recursive entries). Check if original `run_remote`, `upload`, `download` are still called (yes — by other commands like exec, check, etc.). No dead code to remove.

- [ ] **Step 3: Final commit if any cleanup needed**

```bash
git add -A
git commit -m "chore: cleanup and final verification

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```
