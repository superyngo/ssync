# SshTransport Trait Abstraction

## Problem

The ssync codebase has SSH operations scattered across three modules (`executor.rs`, `connection.rs`, `pool.rs`) with no unified interface. Command modules directly import `executor::run_remote_pooled`, `executor::upload_pooled`, and `ConnectionManager`, creating tight coupling that prevents unit testing and makes future backend changes invasive.

**Current pain points:**
- 0 tests on executor.rs (269 lines), 4 tests on connection.rs (only socket-path helpers)
- Commands manually thread `Option<&Path>` socket parameters through every SSH call
- Duplicate function pairs: `run_remote` / `run_remote_pooled`, `upload` / `upload_pooled`, `download` / `download_pooled`
- Swapping the SSH backend (e.g., to `russh`) would require touching every command module

## Approach

Extract an `SshTransport` trait that becomes the sole SSH abstraction for command modules. The existing spawn-process logic is encapsulated in `ProcessTransport`. Connection pooling (ControlMaster sockets) becomes an internal implementation detail — callers never see socket paths.

**Phase 1 (this spec):** Extract trait + `ProcessTransport` as new files. Existing code unchanged — `SshPool`, `executor`, and command modules remain as-is. Proves the abstraction compiles.

**Phase 2 (future):** Migrate command modules to receive `&dyn SshTransport`.

**Phase 3 (future):** Add `MockTransport` and write command-level unit tests.

## Scope

### In scope
- `SshTransport` trait definition in new `transport.rs`
- `ProcessTransport` struct implementing the trait in new `process_transport.rs`
- `RemoteOutput` re-exported from `transport.rs` (canonical location; `executor.rs` re-exports for backward compatibility)
- Both new modules compile and pass `cargo check` / `cargo test`

### Deferred to Phase 2 (command migration)
- Migrate command modules to use `&dyn SshTransport` instead of `executor::*` / `SshPool`
- Remove `SshPool` (its responsibilities absorbed by `ProcessTransport`)
- Downgrade `executor` module visibility to `pub(crate)`

### Out of scope
- Command module migration (future PR)
- `MockTransport` (future PR)
- `shell::detect` / `shell::detect_pooled` — stays as standalone functions
- `init.rs` raw SSH usage (`ssh -G`, `ssh-keyscan`) — setup utilities, not operational SSH
- `ConcurrencyLimiter` — stays external, commands own their semaphores
- `SyncProgress` — UI concern, doesn't belong in transport

## Trait Interface

```rust
// src/host/transport.rs

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::schema::HostEntry;

/// Result of a remote command execution.
#[derive(Debug)]
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

### Design decisions

- **`Duration` over `u64`**: More idiomatic Rust. Callers convert once at the call site.
- **`&self` (not `&mut self`)**: Internal mutation uses `RwLock` so the trait stays `Send + Sync` and supports concurrent operations.
- **`scp_probe` is separate from `connect`**: Only the `sync` command needs SCP probing; other commands skip it.
- **Query methods are sync**: `failed_hosts()`, `reachable_hosts()`, `scp_failed_hosts()` only read internal state.
- **`RemoteOutput` moves here**: It's the trait's return type and belongs with the trait definition.

### Intentional exclusions from the trait

| Excluded | Reason |
|----------|--------|
| `shell::detect` | Init-only concern; stays standalone |
| `ssh -G` / `ssh-keyscan` | Setup utilities, not operational SSH |
| `ConcurrencyLimiter` | Commands own their own semaphores |
| `SyncProgress` | UI concern; doesn't belong in transport |

## ProcessTransport Implementation

```rust
// src/host/process_transport.rs

use std::sync::RwLock;
use anyhow::Result;
use crate::host::connection::ConnectionManager;

pub struct ProcessTransport {
    inner: RwLock<ConnectionManager>,
}

impl ProcessTransport {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: RwLock::new(ConnectionManager::new()?),
        })
    }
}
```

### Method delegation map

| Trait method | Delegates to | Lock type |
|---|---|---|
| `connect()` | `ConnectionManager::pre_check()` → returns `reachable_hosts()` | Write |
| `exec()` | `executor::run_remote_pooled()` with `self.inner.read().socket_for()` | Read |
| `upload()` | `executor::upload_pooled()` with internal socket lookup | Read |
| `download()` | `executor::download_pooled()` with internal socket lookup | Read |
| `scp_probe()` | `ConnectionManager::scp_probe()` | Write |
| `failed_hosts()` | `ConnectionManager::failed_hosts()` | Read |
| `scp_failed_hosts()` | `ConnectionManager::scp_failed_hosts()` | Read |
| `reachable_hosts()` | `ConnectionManager::reachable_hosts()` | Read |
| `shutdown()` | `ConnectionManager::shutdown()` | Write |

### Why RwLock

`exec`/`upload`/`download` only read the socket map (concurrent OK). Only `connect`, `scp_probe`, and `shutdown` need write access (infrequent, sequential). This preserves the current concurrency model.

### Non-pooled function removal

The non-pooled functions (`run_remote`, `upload`, `download`) are superseded. Under the trait, all operations use the pooled path. On Windows (Direct mode), `socket_for()` returns `None`, so pooled functions behave identically to non-pooled ones.

## File Layout

### Phase 1 (this spec)

```
src/host/
├── transport.rs           # NEW — SshTransport trait + RemoteOutput
├── process_transport.rs   # NEW — ProcessTransport implementation
├── connection.rs          # Unchanged
├── executor.rs            # Unchanged (still pub)
├── concurrency.rs         # Unchanged
├── filter.rs              # Unchanged
├── pool.rs                # Unchanged (still pub)
├── shell.rs               # Unchanged
└── mod.rs                 # Adds two new module declarations
```

### After Phase 2 (command migration)

```
src/host/
├── transport.rs           # SshTransport trait + RemoteOutput
├── process_transport.rs   # ProcessTransport implementation
├── connection.rs          # Unchanged internally
├── executor.rs            # Visibility → pub(crate)
├── concurrency.rs         # Unchanged
├── filter.rs              # Unchanged
├── shell.rs               # Unchanged
└── mod.rs                 # pool.rs removed
```

### Module visibility changes (after Phase 2)

| Module | Before | After Phase 2 |
|--------|--------|-------|
| `transport` | — | `pub` (new public API) |
| `process_transport` | — | `pub` (new public API) |
| `executor` | `pub` | `pub(crate)` (internal impl detail) |
| `pool` | `pub` | Removed |
| `connection` | `pub` | `pub` (init.rs still uses it directly) |

In Phase 1, all existing visibility remains unchanged. The two new modules are added as `pub`.

## Command Migration Pattern (Future Phase)

### Current pattern

```rust
let (pool, _) = SshPool::setup(&hosts, ctx.timeout, ctx.concurrency(), ...).await?;
let socket = pool.socket_for(&host.name).map(|p| p.to_path_buf());
let result = executor::run_remote_pooled(&host, &cmd, timeout, socket.as_deref()).await;
pool.shutdown().await;
```

### Future pattern

```rust
let transport = ProcessTransport::new()?;
transport.connect(&hosts, Duration::from_secs(ctx.timeout), ctx.concurrency()).await?;
let result = transport.exec(&host, &cmd, Duration::from_secs(ctx.timeout)).await;
transport.shutdown().await?;
```

### Per-command migration scope

| Command | Current SSH deps | Migration complexity |
|---|---|---|
| `run.rs` | `SshPool`, `executor::run_remote_pooled` | Low — 1 exec call |
| `exec.rs` | `SshPool`, `executor::{run_remote_pooled, upload_pooled}` | Low — 5 executor calls |
| `check.rs` | `SshPool`, `collector::collect_pooled` | Medium — collector also needs updating |
| `sync.rs` | `SshPool`, `pool.conn_mgr`, 6 executor functions | High — largest migration |
| `init.rs` | `ConnectionManager` directly + raw `Command::new("ssh")` | Partial — keeps CM for keyscan/detect |

### CommandContext extension (future)

```rust
pub struct Context {
    // ... existing fields ...
    pub transport: Box<dyn SshTransport>,  // injected at construction
}
```

## Testability (Future Phase)

```rust
#[cfg(test)]
pub struct MockTransport {
    responses: RwLock<HashMap<(String, String), RemoteOutput>>,
    call_log: RwLock<Vec<MockCall>>,
    connected_hosts: Vec<String>,
    failed: Vec<(String, String)>,
}
```

### Coverage improvement potential

| Module | Current testable | With MockTransport |
|---|---|---|
| sync.rs business logic | Parsing only (12 tests) | Full decide/distribute/expand coverage |
| exec.rs flow | 0 tests | Upload→chmod→execute→cleanup flow |
| run.rs | 0 tests | Sudo wrapping, output handling |
| check.rs orchestration | 0 tests | Metrics collection flow |
| executor/connection | 0% | ~70%+ command-level logic |

## Risk Analysis

### Risks

**RwLock contention (Low):** `connect()` and `shutdown()` take write locks once each. All concurrent `exec`/`upload`/`download` take read locks simultaneously.

**sync.rs legacy non-pooled path (Medium):** sync.rs has calls to non-pooled `executor::run_remote()` and `executor::upload()`. Under the trait, these map to pooled functions with `socket=None` in Direct mode. Verify Windows-targeted hosts don't regress.

**init.rs divergence (Low):** init.rs continues using `ConnectionManager` directly. Risk: CM API changes need reflection in both `ProcessTransport` and `init.rs`. Mitigated by keeping CM stable.

**shell::detect_pooled uses executor directly (Low):** After making executor `pub(crate)`, `shell.rs` can still access it (same crate). No change needed.

**Async trait lifetime (Low):** `async_trait` boxes futures. `RwLock` guards are short-lived within each method — no locks held across await points.

### Edge cases

- **Windows Direct mode**: `socket_for()` returns `None` → pooled functions work as non-pooled. ✓
- **Host not connected**: `exec()` on unconnected host → SSH runs without ControlMaster → may timeout. Matches current behavior.
- **Double connect**: `pre_check()` adds to existing state, doesn't reset. Acceptable.
- **Shutdown then exec**: SSH runs without socket → works but slower. No crash.
