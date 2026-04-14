# SshTransport Trait Abstraction — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract an `SshTransport` trait and `ProcessTransport` implementation that unify SSH operations behind a single abstraction, alongside existing code (no command migrations).

**Architecture:** Two new files — `src/host/transport.rs` (trait + `RemoteOutput`) and `src/host/process_transport.rs` (`ProcessTransport` wrapping `ConnectionManager` + `executor`). Existing modules (`executor.rs`, `connection.rs`, `pool.rs`) remain unchanged. The `async_trait` crate is added for `dyn`-compatible async traits.

**Tech Stack:** Rust, `async_trait`, `tokio`, `anyhow`, existing `ConnectionManager` and `executor` modules.

**Spec:** `docs/superpowers/specs/2026-04-14-ssh-transport-trait-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `Cargo.toml` | Modify (add `async_trait`) | Dependency |
| `src/host/transport.rs` | Create | `SshTransport` trait + `RemoteOutput` struct |
| `src/host/process_transport.rs` | Create | `ProcessTransport` impl wrapping `ConnectionManager` + `executor` |
| `src/host/mod.rs` | Modify | Add `pub mod transport;` and `pub mod process_transport;` |
No files deleted. No command modules touched. `executor.rs` is unchanged (both `RemoteOutput` types coexist until Phase 2).

---

### Task 1: Add `async_trait` dependency

**Files:**
- Modify: `Cargo.toml:28` (after `thiserror` in error handling section)

- [ ] **Step 1: Add `async_trait` to Cargo.toml**

In `Cargo.toml`, add `async_trait` in the error handling section (it's used alongside `anyhow` for trait definitions):

```toml
# Error handling
anyhow = "1"
thiserror = "2"

# Async trait (for dyn-compatible async traits)
async-trait = "0.1"
```

- [ ] **Step 2: Verify dependency resolves**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles successfully (warning about unused dep is fine at this stage)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add async-trait dependency for SshTransport trait"
```

---

### Task 2: Create `transport.rs` — trait definition and RemoteOutput

**Files:**
- Create: `src/host/transport.rs`

- [ ] **Step 1: Create `src/host/transport.rs` with the trait and RemoteOutput**

```rust
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::schema::HostEntry;

/// Result of a remote command execution.
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
    async fn exec(
        &self,
        host: &HostEntry,
        cmd: &str,
        timeout: Duration,
    ) -> Result<RemoteOutput>;

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
```

- [ ] **Step 2: Register the module in `src/host/mod.rs`**

Add `pub mod transport;` to `src/host/mod.rs`. The file should become:

```rust
pub mod concurrency;
pub mod connection;
pub mod executor;
pub mod filter;
pub mod pool;
pub mod shell;
pub mod transport;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles successfully

- [ ] **Step 4: Commit**

```bash
git add src/host/transport.rs src/host/mod.rs
git commit -m "feat: add SshTransport trait definition and RemoteOutput

Defines the unified async trait interface for SSH operations:
connect, exec, upload, download, scp_probe, shutdown.
RemoteOutput is the canonical return type for remote command execution."
```

---

### Task 3: Create `process_transport.rs` — ProcessTransport struct and trait impl

**Files:**
- Create: `src/host/process_transport.rs`

- [ ] **Step 1: Create `src/host/process_transport.rs` with full implementation**

```rust
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::RwLock;

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

    async fn exec(
        &self,
        host: &HostEntry,
        cmd: &str,
        timeout: Duration,
    ) -> Result<RemoteOutput> {
        let timeout_secs = timeout.as_secs();
        let socket_path = {
            let mgr = self.inner.read().await;
            mgr.socket_for(&host.name).map(|p| p.to_path_buf())
        };
        let out =
            executor::run_remote_pooled(host, cmd, timeout_secs, socket_path.as_deref()).await?;
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
        self.inner
            .try_read()
            .map(|mgr| mgr.failed_hosts())
            .unwrap_or_default()
    }

    fn scp_failed_hosts(&self) -> Vec<(String, String)> {
        self.inner
            .try_read()
            .map(|mgr| mgr.scp_failed_hosts())
            .unwrap_or_default()
    }

    fn reachable_hosts(&self) -> Vec<String> {
        self.inner
            .try_read()
            .map(|mgr| mgr.reachable_hosts())
            .unwrap_or_default()
    }

    async fn shutdown(&self) -> Result<()> {
        let mut mgr = self.inner.write().await;
        mgr.shutdown().await;
        Ok(())
    }
}
```

- [ ] **Step 2: Register the module in `src/host/mod.rs`**

Add `pub mod process_transport;` to `src/host/mod.rs`. The file should become:

```rust
pub mod concurrency;
pub mod connection;
pub mod executor;
pub mod filter;
pub mod pool;
pub mod process_transport;
pub mod shell;
pub mod transport;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: compiles successfully. There may be a warning about `RemoteOutput` being defined in both `executor.rs` and `transport.rs` — that's expected and addressed in Task 4.

- [ ] **Step 4: Commit**

```bash
git add src/host/process_transport.rs src/host/mod.rs
git commit -m "feat: add ProcessTransport implementing SshTransport

Wraps ConnectionManager + executor functions behind the SshTransport trait.
Uses tokio::sync::RwLock for interior mutability: read locks for
exec/upload/download, write locks for connect/scp_probe/shutdown.
tokio RwLock avoids blocking the async runtime when write locks are
held across .await points in connect/scp_probe."
```

---

### Task 4: Add backward-compatible `RemoteOutput` re-export in `executor.rs`

The spec says `RemoteOutput` moves to `transport.rs` as the canonical location. But executor.rs currently defines and exports `RemoteOutput`, and all command modules import it from there. To avoid breaking existing imports, keep the `RemoteOutput` definition in `executor.rs` unchanged for now. The `transport.rs` version has an identical struct (with an added `Clone` derive) that `ProcessTransport` uses.

In Phase 2 (command migration), commands will switch to importing from `transport.rs` and the executor copy will be removed.

**Files:**
- No file changes needed for Phase 1.

- [ ] **Step 1: Verify no duplicate-type issues**

The two `RemoteOutput` structs are separate types in different modules. `ProcessTransport::exec()` converts from `executor::RemoteOutput` to `transport::RemoteOutput` field-by-field (see Task 3, Step 1). This is intentional — no type aliasing or re-export needed yet.

Run: `cargo check 2>&1 | tail -5`
Expected: compiles with no errors

- [ ] **Step 2: Verify all existing tests still pass**

Run: `cargo test 2>&1 | tail -15`
Expected: all 63 tests pass, 0 failures

- [ ] **Step 3: Commit (only if any changes were needed)**

Skip if no changes — this task is a verification checkpoint.

---

### Task 5: Add unit tests for ProcessTransport construction

**Files:**
- Modify: `src/host/process_transport.rs` (append test module)

- [ ] **Step 1: Add test module to `process_transport.rs`**

Append to the end of `src/host/process_transport.rs`:

```rust
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
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test process_transport::tests 2>&1 | tail -15`
Expected: 6 tests pass

- [ ] **Step 3: Run full test suite to verify no regressions**

Run: `cargo test 2>&1 | tail -15`
Expected: 69 tests pass (63 existing + 6 new), 0 failures

- [ ] **Step 4: Commit**

```bash
git add src/host/process_transport.rs
git commit -m "test: add ProcessTransport unit tests

Tests construction, initial state, Send+Sync bounds, and dyn compatibility."
```

---

### Task 6: Run lints and final validation

**Files:**
- No changes expected (fix any clippy issues if found)

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -20`
Expected: no errors. Fix any warnings in the new files only.

- [ ] **Step 2: Run fmt check**

Run: `cargo fmt --check 2>&1`
Expected: no formatting issues. If any, run `cargo fmt` and commit.

- [ ] **Step 3: Build with default features**

Run: `cargo build 2>&1 | tail -5`
Expected: build succeeds

- [ ] **Step 4: Build without TUI feature**

Run: `cargo build --no-default-features 2>&1 | tail -5`
Expected: build succeeds

- [ ] **Step 5: Run full test suite one final time**

Run: `cargo test 2>&1 | tail -15`
Expected: all tests pass

- [ ] **Step 6: Commit any fixes and push**

```bash
# Only if fixes were needed:
git add -A
git commit -m "style: fix clippy/fmt issues in transport modules"
```
