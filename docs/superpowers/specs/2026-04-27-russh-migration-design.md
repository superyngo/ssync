# russh Migration Design

> Date: 2026-04-27
> Status: Approved
> Replaces: docs/russh-migration-evaluation.md (evaluation phase)

## Problem Statement

ssync currently shells out to system `ssh`/`scp` binaries for all remote operations. This creates four concrete problems:

1. **Multi-alias SSH config parsing is broken**: `Host bastion.bss-qa slb225` creates one entry named `"bastion.bss-qa slb225"` instead of two separate aliases, so hosts registered under multiple aliases are unreachable.
2. **No connection multiplexing on Windows**: Windows lacks Unix domain sockets, so ControlMaster is unavailable. Every SSH operation spawns an independent process, causing high per-operation latency with many hosts.
3. **No ProxyJump support**: The current transport blindly passes `host.ssh_host` to the ssh binary, so ProxyJump defined in `~/.ssh/config` only works by accident (the system ssh binary resolves it). ssync has no explicit awareness of the jump topology.
4. **Cross-platform SCP inconsistency**: SCP availability and behavior varies by platform; Windows OpenSSH's SCP implementation has edge cases.
5. **VirtualLock security warnings on Windows**: When russh is added, its crypto subsystem tries to lock private key memory via `VirtualLock`. Under standard Windows user accounts this fails and emits a noisy warning on stderr.

## Proposed Approach: Full russh Migration

Replace all `Command::new("ssh")`/`"scp"` calls with russh native sessions. Use `ssh2-config` crate for full SSH config parsing. The existing `SshTransport` trait (`host/transport.rs`) serves as the abstraction boundary for SSH exec operations — command modules invoking `exec` do not change. File transfer calls in `sync.rs` and `exec.rs` currently bypass the trait (calling `executor` directly); these are migrated to use `RusshTransport::upload`/`download` through the trait in Phase 3.

---

## Architecture Overview

### Module Changes

| Module | Status | Change |
|--------|--------|--------|
| `config/ssh_config.rs` | Rewrite | Replace hand-rolled parser with `ssh2-config` |
| `config/schema.rs` | Extend | Add `proxy_jump: Option<String>` to `HostEntry` |
| `host/session_pool.rs` | New (Ph.2) | Replaces `connection.rs` — russh `Handle`-based session pool |
| `host/auth.rs` | New (Ph.2) | Authentication chain: IdentityFile → passphrase prompt → password prompt |
| `host/sftp.rs` | New (Ph.3) | SFTP upload/download/probe wrapping `russh-sftp` |
| `host/russh_transport.rs` | New (Ph.2) | `SshTransport` trait implementation using the above modules |
| `host/pool.rs` | Update (Ph.2) | `SshPool` updated to hold `RusshSessionPool` instead of `ConnectionManager` |
| `host/process_transport.rs` | Remove (Ph.4) | Superseded by `russh_transport.rs` |
| `host/connection.rs` | Remove (Ph.4) | Superseded by `session_pool.rs` |
| `host/executor.rs` | Remove (Ph.4) | All operations moved into `russh_transport.rs`/`sftp.rs` |
| `commands/sync.rs` | Update (Ph.3) | SCP upload/download calls replaced with SFTP via `SshTransport` trait |
| `commands/exec.rs` | Update (Ph.3) | `upload_pooled` call replaced with SFTP |
| `commands/init.rs` | Update (Ph.1) | `ssh -G` replaced with `ssh2-config` query; `ssh-keyscan` kept for known_hosts seeding |
| `main.rs` | Update (Ph.1) | tracing subscriber filter updated for VirtualLock suppression |

### Dependency Changes

**Add:**
```toml
russh = "0.46"
russh-keys = "0.46"
russh-sftp = "2.0"
ssh2-config = "0.2"
rpassword = "7"
```

**Remove (Phase 4):** `tokio` process feature can be dropped once `ssh-keyscan` is replaced by host key collection via russh's `check_server_key` callback in `init.rs`.

---

## Section 1: SSH Config Layer (`config/ssh_config.rs`)

### Problem: Multi-Alias Parsing

OpenSSH allows multiple space-separated aliases on a single `Host` line:
```
Host bastion.bss-qa slb225
    HostName 10.0.1.5
    User deploy
    IdentityFile ~/.ssh/bastion_key
```

The current parser stores the entire string `"bastion.bss-qa slb225"` as the host name. The `ssh2-config` crate correctly splits aliases: querying either `"bastion.bss-qa"` or `"slb225"` returns the same resolved config.

### New API

```rust
pub struct ResolvedHostConfig {
    pub alias: String,           // the name used to query (e.g. "slb225")
    pub hostname: String,        // actual IP/FQDN to connect to
    pub user: String,
    pub port: u16,
    pub identity_files: Vec<PathBuf>,
    pub proxy_jump: Option<String>, // raw ProxyJump value, e.g. "bastion" or "user@bastion:22"
    pub identities_only: bool,
}

/// Resolve all SSH config settings for a given host alias.
pub fn resolve_host(alias: &str) -> Result<ResolvedHostConfig>;

/// List all non-wildcard host aliases from ~/.ssh/config (for init discovery).
pub fn list_aliases() -> Result<Vec<String>>;
```

`resolve_host` uses `SshConfig::parse_default_file(ParseRule::ALLOW_UNKNOWN_FIELDS)` and `config.query(alias)` for full OpenSSH-compatible resolution including `Include`, `Match` blocks, and `*`/`?` wildcard inheritance.

### `HostEntry` Schema Change

```rust
// config/schema.rs
pub struct HostEntry {
    pub name: String,
    pub ssh_host: String,
    pub shell: ShellType,
    #[serde(default)]
    pub groups: Vec<String>,
    /// ProxyJump alias or user@host:port. Populated from SSH config at init time,
    /// can also be set manually in ssync config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
}
```

`init.rs` populates `proxy_jump` from `ResolvedHostConfig.proxy_jump` when building `HostEntry` records. Existing TOML configs without `proxy_jump` continue to work (defaults to `None`).

---

## Section 2: Session Pool (`host/session_pool.rs`)

### Design

Replaces `ConnectionManager`. A session is a live `Arc<russh::client::Handle<SshHandler>>`. Multiple tokio tasks can share a single `Handle` (it is `Send + Sync` and multiplexes channels internally via mpsc). No Unix sockets, no temporary directories — connection state lives entirely in-process.

```rust
pub struct RusshSessionPool {
    // Primary sessions: one per target host (name → Handle)
    sessions: HashMap<String, Arc<Handle<SshHandler>>>,
    // Proxy sessions: kept alive as long as any dependent target is connected
    proxy_sessions: HashMap<String, Arc<Handle<SshHandler>>>,
    failed: HashMap<String, String>,
    scp_failed: HashMap<String, String>,  // hosts where SFTP probe failed
}
```

### Direct Connection Flow

```
tokio::net::TcpStream::connect(hostname:port)
  → russh::client::connect_stream(config, stream, handler)
  → auth::authenticate(handle, user, host_config)
  → store Arc<Handle> in sessions
```

### ProxyJump Flow

For a `HostEntry` with `proxy_jump = Some("bastion")`:

```
1. Recursively resolve "bastion" → its HostEntry
2. Establish (or reuse) bastion session → proxy_sessions["bastion"]
3. bastion_handle.channel_open_direct_tcpip(target_hostname, target_port, "127.0.0.1", 0)
4. russh::client::connect_stream(config, channel.into_stream(), target_handler)
5. auth::authenticate(target_handle, target_user, target_config)
6. Store Arc<Handle> in sessions[target_name]
```

Multi-hop (`ProxyJump = "jump1,jump2"`) is handled by parsing the comma-separated list and applying this process recursively from outermost to innermost.

### Windows Multiplexing

`Arc<Handle>` is shared across tasks on all platforms. There are no ControlMaster sockets, no platform-specific code paths. Windows gets the same session multiplexing as Linux/macOS.

### Shutdown

```rust
for handle in sessions.values() {
    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
}
// proxy sessions disconnected after all dependent sessions are closed
```

No blocking `Drop` fallback needed — dropping an `Arc<Handle>` closes the underlying TCP connection automatically when the ref count reaches zero.

### known_hosts Verification

Implemented via the `SshHandler::check_server_key` callback:

```rust
async fn check_server_key(&mut self, server_public_key: &ssh_key::PublicKey) -> Result<bool> {
    let known_hosts = dirs::home_dir()
        .map(|h| h.join(".ssh/known_hosts"))
        .context("cannot determine home directory")?;

    // Also check UserKnownHostsFile from SSH config if set
    match russh::client::check_known_hosts_path(&self.host, self.port, server_public_key, &known_hosts) {
        Ok(true) => Ok(true),
        Ok(false) => bail!("HOST KEY VERIFICATION FAILED for {}:{} — possible MITM", self.host, self.port),
        Err(_) => bail!("Unknown host key for {}:{} — run `ssync init` to accept", self.host, self.port),
    }
}
```

`init.rs` continues to use `ssh-keyscan` to seed `~/.ssh/known_hosts` before the first connection. Alternatively, `init` can connect via russh with a `StrictHostKeyChecking=no`-equivalent handler and capture the key from `check_server_key` to write it manually — this removes the `ssh-keyscan` dependency entirely (optional, Phase 4 cleanup).

---

## Section 3: Authentication Chain (`host/auth.rs`)

Authentication is attempted in order until one succeeds. All branches use `rpassword` for terminal prompts so that ssync works in non-interactive pipelines (passphrase cache means users are only prompted once per unique key path per process invocation).

```
1. For each key in identity_files (or defaults if empty and !identities_only):
   a. Try load_secret_key(path, passphrase=None)
      → if Ok: authenticate_publickey → return if accepted
   b. Try load_secret_key(path, passphrase=None) failed (encrypted key):
      → check passphrase cache
      → if not cached: prompt with rpassword::prompt_password
      → try load_secret_key(path, Some(passphrase))
      → if Ok: authenticate_publickey → return if accepted; cache passphrase

2. All key attempts exhausted (or identity_files empty with identities_only=true):
   → prompt for password: rpassword::prompt_password("Password for user@host: ")
   → authenticate_password → return if accepted

3. All methods failed:
   → bail!("Authentication failed for {name}: no valid credentials")
```

**Passphrase cache**: `HashMap<PathBuf, String>` stored in the auth context for the lifetime of a single ssync invocation. Not persisted to disk.

**Default key paths** (used when `identity_files` is empty and `identities_only` is false):
- `~/.ssh/id_ed25519`
- `~/.ssh/id_ecdsa`
- `~/.ssh/id_rsa`

Each path is checked for existence before attempting to load.

---

## Section 4: SFTP Operations (`host/sftp.rs`)

Replaces all `executor::upload_pooled` / `download_pooled` / `scp_probe` calls.

### Upload

```rust
pub async fn upload(
    session: &Handle<SshHandler>,
    local: &Path,
    remote: &str,
    timeout: Duration,
) -> Result<()>
```

1. Open SFTP channel from session
2. Resolve `~` in remote path if present (exec `echo $HOME` once and cache)
3. `mkdir_p(sftp, parent_of_remote)` — create intermediate directories
4. For files ≤ 64 MB: `sftp.create(remote)` → `write_all(data)` → `shutdown()`
5. For files > 64 MB: stream in 4 MB chunks with `AsyncWriteExt::write_all` in a loop

### Download

```rust
pub async fn download(
    session: &Handle<SshHandler>,
    remote: &str,
    local: &Path,
    timeout: Duration,
) -> Result<()>
```

1. Open SFTP channel
2. `sftp.open(remote)` → `tokio::io::copy` → local file

### SFTP Probe (replaces `scp_probe`)

```rust
pub async fn sftp_probe(session: &Handle<SshHandler>, timeout: Duration) -> Result<()>
```

Attempts `sftp.stat(".")` (current directory) or `sftp.create("~/.ssync_probe")` + `sftp.remove(...)`. Success means SFTP subsystem is available. Failure marks the host as SFTP-incapable.

### Remote Path Handling

- SFTP protocol always uses `/` as the path separator, even for Windows remote hosts.
- `~` is NOT expanded by the SFTP protocol. A one-time `channel.exec("echo $HOME")` caches the remote home directory per session. All `~`-prefixed paths are substituted before SFTP calls.
- `mkdir_p` issues sequential `sftp.create_dir()` calls for each path component, ignoring "already exists" errors.

---

## Section 5: VirtualLock Warning Suppression

### Source

The warning originates from the `zeroize` or `ssh-key` crate attempting `VirtualLock()` on Windows to prevent private key material from being swapped to disk. Under standard (non-privileged) Windows user accounts, this syscall fails with error `0x5AD` (ERROR_WORKING_SET_QUOTA). The failure is non-fatal and logged via the `tracing` framework at `WARN` level.

### Fix

Configure the tracing subscriber filter in `main.rs` to suppress `WARN`-level events from russh-related crates unless `--verbose` is active:

```rust
// main.rs：tracing filter — respects RUST_LOG env var when set
let filter = if std::env::var("RUST_LOG").is_ok() {
    EnvFilter::from_default_env()
} else if args.verbose {
    // Show russh warnings (including VirtualLock) in verbose mode
    EnvFilter::new("ssync=debug,russh=debug,russh_keys=debug,info")
} else {
    // Suppress russh noise; show only errors from crypto crates
    EnvFilter::new("russh=error,russh_keys=error,ssh_key=error,zeroize=error,info")
};
tracing_subscriber::fmt().with_env_filter(filter).init();
```

---

## Section 6: Migration Phases

### Phase 1 — SSH Config Fix (isolated, low risk)

**Files changed**: `config/ssh_config.rs`, `config/schema.rs`, `main.rs` (tracing filter), `Cargo.toml`

**Goal**: Fix multi-alias bug, add `proxy_jump` field to `HostEntry`, prepare tracing filter for VirtualLock suppression.

1. Add `ssh2-config`, `rpassword` to `Cargo.toml`
2. Rewrite `config/ssh_config.rs` using `ssh2-config`; expose `resolve_host()` and `list_aliases()`
3. Add `proxy_jump: Option<String>` to `HostEntry` with `#[serde(default)]`
4. Update `commands/init.rs` to call `resolve_host()` instead of `ssh -G`
5. Update tracing subscriber in `main.rs`
6. All existing tests must pass; add tests for multi-alias and ProxyJump parsing

### Phase 2 — RusshTransport (exec + auth + ProxyJump)

**Files changed**: New `host/session_pool.rs`, `host/auth.rs`, `host/russh_transport.rs`; update `host/mod.rs`

**Goal**: Implement `SshTransport` trait backed by russh. SFTP is stubbed — upload/download fall back to the existing `ProcessTransport` behavior until Phase 3.

1. Add `russh`, `russh-keys` to `Cargo.toml`
2. Implement `session_pool.rs`: `RusshSessionPool` with `connect`, `connect_via_proxy`, `shutdown`
3. Implement `auth.rs`: authentication chain as designed
4. Implement `russh_transport.rs`: `exec`, `connect`, `shutdown`, `failed_hosts`, `reachable_hosts`; temporarily delegate `upload`/`download` to spawning `scp` for continuity
5. Switch `SshPool` to use `RusshTransport` behind a feature flag `russh-transport`
6. Integration tests: connect to a real host, run a command, verify output

### Phase 3 — SFTP Integration

**Files changed**: New `host/sftp.rs`; update `host/russh_transport.rs`, `commands/sync.rs`, `commands/exec.rs`

**Goal**: Replace all `scp`-based file transfers with SFTP.

1. Add `russh-sftp` to `Cargo.toml`
2. Implement `sftp.rs`: `upload`, `download`, `sftp_probe`, `mkdir_p`, `resolve_home`
3. Wire `RusshTransport::upload`/`download`/`scp_probe` to `sftp.rs`
4. Update `sync.rs` Stage 3 upload/download calls
5. Update `exec.rs` file upload call
6. End-to-end test: sync a directory with files > 64 MB; verify integrity

### Phase 4 — Cleanup

**Files changed**: Remove `host/executor.rs`, `host/connection.rs`, `host/process_transport.rs`; remove `tokio/process` feature; remove feature flag

**Goal**: Remove all dead code, make `RusshTransport` the unconditional default.

1. Remove `ProcessTransport`, `ConnectionManager`, `executor.rs`
2. Remove the `russh-transport` feature flag — russh is now the only transport
3. Drop `tokio` `process` feature from `Cargo.toml` (verify `init.rs` no longer needs it, or keep for `ssh-keyscan`)
4. Update `cargo clippy --all-targets` to pass with no warnings
5. Full test suite must pass

---

## Edge Cases

### Authentication
- SSH Agent not used (out of scope for this design)
- FIDO/U2F keys (ed25519-sk, ecdsa-sk) not supported by russh — if encountered, fail with clear message: `"FIDO/hardware keys are not supported by ssync's embedded SSH client. Use the system ssh binary instead."`
- Certificate-based auth: not in scope
- Encrypted keys with wrong passphrase: retry up to 3 times before bailing

### Connection
- IPv6 literal addresses: pass directly to `TcpStream::connect`, format as `[::1]:22`
- DNS resolution failure: propagate as connection error with host name in context
- Server with banner longer than 64 KB: russh handles internally; not an ssync concern
- Server disconnects mid-operation: return an error from the channel read loop; ssync marks host as failed for that operation

### SFTP
- Remote disk full: `write_all` returns an error; propagate as upload failure
- Remote path contains spaces: passed as-is to SFTP (protocol handles it natively)
- Symlinks: followed by default; no explicit symlink handling added
- Files > 1 GB: chunked streaming write; memory usage bounded by chunk size (4 MB)

### ProxyJump
- `ProxyJump` alias not found in SSH config: fail at connection time with clear error
- Circular ProxyJump (A → B → A): detect by tracking the current resolution chain; bail on cycle
- Proxy host itself requires ProxyJump: recursive resolution handles this transparently (depth limit: 8 hops)

### Multi-hop ProxyJump (`ProxyJump = "jump1,jump2"`)
OpenSSH's comma-separated ProxyJump means: connect via jump1 first, then via jump2 to reach the target. ssync resolves this left-to-right, each hop wrapped in a `channel_open_direct_tcpip`.

---

## Testing Strategy

- **Unit tests**: `ssh_config.rs` — multi-alias parsing, `proxy_jump` extraction, wildcard inheritance
- **Unit tests**: `auth.rs` — key loading logic, passphrase cache, fallback ordering
- **Unit tests**: `session_pool.rs` — ProxyJump chain detection, cycle detection
- **Integration tests** (in `tests/`): require a real SSH target defined in a test env var; skipped if not set
  - `test_exec_remote_command`: connects, runs `echo hello`, checks output
  - `test_upload_download_roundtrip`: uploads a 1 MB file, downloads it back, compares hash
  - `test_proxy_jump_connection`: connects to a target via a bastion, runs a command
  - `test_sftp_large_file`: uploads a 128 MB file, verifies integrity
- **Windows CI**: run all unit tests; integration tests skipped if no SSH target available
