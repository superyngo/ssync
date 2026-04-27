# russh Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace external `ssh`/`scp` binary calls with the embedded `russh` library to gain Windows multiplexing, consistent cross-platform behaviour, ProxyJump support, and SFTP-based file transfers.

**Architecture:** Four incremental phases keep the build green at every commit. Phase 1 replaces the SSH config parser (multi-alias fix + ProxyJump extraction). Phase 2 introduces `RusshSessionPool` for exec operations alongside the existing `ConnectionManager` (kept temporarily for sync file transfers). Phase 3 adds SFTP and removes `ConnectionManager`. Phase 4 deletes all legacy process-spawn code.

**Tech Stack:** `russh = "0.44"`, `russh-sftp = "2.1"`, `russh-keys = "0.44"`, `ssh2-config = "0.7"`, `rpassword = "7"`

---

## File Map

### Created
| File | Purpose |
|------|---------|
| `src/host/auth.rs` | Auth chain: IdentityFile → passphrase prompt → password prompt |
| `src/host/session_pool.rs` | `RusshSessionPool`: connect, ProxyJump, exec, SFTP (Phases 2–3) |
| `src/host/sftp.rs` | SFTP helpers: upload, download, mkdir_p, home_dir cache (Phase 3) |

### Modified
| File | Change |
|------|--------|
| `Cargo.toml` | Add new crate deps per phase |
| `src/config/ssh_config.rs` | Multi-alias fix, `ResolvedHostConfig`, `load_ssh_config()`, `resolve_host()` |
| `src/config/schema.rs` | Add `proxy_jump: Option<String>` to `HostEntry` |
| `src/commands/init.rs` | Replace `ssh -G` call with `resolve_host()` |
| `src/main.rs` | Tracing filter to suppress VirtualLock warnings |
| `src/host/mod.rs` | Add `pub mod auth`, `pub mod session_pool`, `pub mod sftp` |
| `src/host/pool.rs` | Add `session_pool: Arc<RusshSessionPool>` field; add `pool.exec()` |
| `src/metrics/collector.rs` | Accept `Arc<RusshSessionPool>` instead of socket path |
| `src/commands/run.rs` | Use `pool.session_pool.exec()` |
| `src/commands/exec.rs` | Use `pool.session_pool.exec()` |
| `src/commands/check.rs` | Pass `pool.session_pool.clone()` to `collect_pooled()` |
| `src/commands/sync.rs` | Phase 3: use `pool.session_pool` for SFTP; remove `conn_mgr` usage |

### Deleted (Phase 4)
| File | Reason |
|------|--------|
| `src/host/connection.rs` | Replaced by `RusshSessionPool` |
| `src/host/executor.rs` | Replaced by russh channels + SFTP |
| `src/host/process_transport.rs` | `SshTransport` impl for old process path |
| `src/host/transport.rs` | Superseded by `RusshSessionPool` methods |

---

## Phase 1 — Config Layer & Diagnostics

### Task 1: Add Phase 1 dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add deps to Cargo.toml**

In `[dependencies]` add:
```toml
ssh2-config = "0.7"
rpassword = "7"
```

- [ ] **Step 2: Verify build**

```bash
cargo check
```
Expected: compiles with 0 errors (ssh2-config and rpassword are not yet used, just declared).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add ssh2-config and rpassword dependencies"
```

---

### Task 2: Fix multi-alias parsing + add ResolvedHostConfig

**Files:**
- Modify: `src/config/ssh_config.rs`
- Test file: same file (`#[cfg(test)]` module)

The current parser stores `Host bastion.bss-qa slb225` as a single name `"bastion.bss-qa slb225"`. We fix this by splitting each `Host` line on whitespace and emitting one `SshHostEntry` per alias. We also:
1. Add `proxy_jump: Option<String>` to `SshHostEntry`  
2. Add `ResolvedHostConfig` struct (for Phase 2 auth use)
3. Add `load_ssh_config() -> Result<ssh2_config::SshConfig>` using the `ssh2-config` crate
4. Add `resolve_host(alias) -> Result<ResolvedHostConfig>` for Phase 2 use

- [ ] **Step 1: Write failing tests for multi-alias and proxy_jump**

Add inside the `#[cfg(test)]` module in `src/config/ssh_config.rs`:

```rust
#[test]
fn test_multi_alias_parsed_as_separate_hosts() {
    let content = r#"
Host bastion.bss-qa slb225
    HostName 10.0.0.1
    User admin
    Port 2222

Host web1
    HostName 192.168.1.10
"#;
    let hosts = parse_ssh_config_content(content).unwrap();
    assert_eq!(hosts.len(), 3);
    // Both aliases get the same settings
    let bss = hosts.iter().find(|h| h.name == "bastion.bss-qa").unwrap();
    let slb = hosts.iter().find(|h| h.name == "slb225").unwrap();
    let web = hosts.iter().find(|h| h.name == "web1").unwrap();
    assert_eq!(bss.hostname.as_deref(), Some("10.0.0.1"));
    assert_eq!(bss.port, Some(2222));
    assert_eq!(slb.hostname.as_deref(), Some("10.0.0.1"));
    assert_eq!(slb.port, Some(2222));
    assert_eq!(web.hostname.as_deref(), Some("192.168.1.10"));
}

#[test]
fn test_proxy_jump_parsed() {
    let content = r#"
Host internal
    HostName 10.10.0.5
    ProxyJump bastion
"#;
    let hosts = parse_ssh_config_content(content).unwrap();
    assert_eq!(hosts[0].proxy_jump.as_deref(), Some("bastion"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test config::ssh_config::tests
```
Expected: 2 test failures about multi-alias and proxy_jump.

- [ ] **Step 3: Add proxy_jump field to SshHostEntry**

Replace the existing `SshHostEntry` struct:

```rust
#[derive(Debug, Clone)]
pub struct SshHostEntry {
    pub name: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
}
```

- [ ] **Step 4: Rewrite parse_ssh_config_content to handle multi-alias**

Replace the entire `parse_ssh_config_content` function:

```rust
fn parse_ssh_config_content(content: &str) -> Result<Vec<SshHostEntry>> {
    let mut hosts = Vec::new();
    // current block's pending names and settings
    let mut pending_names: Vec<String> = Vec::new();
    let mut hostname: Option<String> = None;
    let mut user: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut identity_file: Option<String> = None;
    let mut proxy_jump: Option<String> = None;

    let flush = |hosts: &mut Vec<SshHostEntry>,
                 names: &[String],
                 hostname: &Option<String>,
                 user: &Option<String>,
                 port: Option<u16>,
                 identity_file: &Option<String>,
                 proxy_jump: &Option<String>| {
        for name in names.iter().filter(|n| !is_wildcard(n)) {
            hosts.push(SshHostEntry {
                name: name.clone(),
                hostname: hostname.clone(),
                user: user.clone(),
                port,
                identity_file: identity_file.clone(),
                proxy_jump: proxy_jump.clone(),
            });
        }
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = if let Some(eq_pos) = line.find('=') {
            (line[..eq_pos].trim(), line[eq_pos + 1..].trim())
        } else if let Some(space_pos) = line.find(char::is_whitespace) {
            (line[..space_pos].trim(), line[space_pos..].trim())
        } else {
            continue
        };

        match key.to_lowercase().as_str() {
            "host" => {
                flush(&mut hosts, &pending_names, &hostname, &user, port, &identity_file, &proxy_jump);
                pending_names = value.split_whitespace().map(|s| s.to_string()).collect();
                hostname = None;
                user = None;
                port = None;
                identity_file = None;
                proxy_jump = None;
            }
            "hostname" => hostname = Some(value.to_string()),
            "user" => user = Some(value.to_string()),
            "port" => port = value.parse().ok(),
            "identityfile" => identity_file = Some(value.to_string()),
            "proxyjump" => {
                // Take only the first hop; multiple hops are comma-separated
                let first_hop = value.split(',').next().unwrap_or(value).trim().to_string();
                proxy_jump = Some(first_hop);
            }
            _ => {}
        }
    }

    flush(&mut hosts, &pending_names, &hostname, &user, port, &identity_file, &proxy_jump);
    Ok(hosts)
}
```

- [ ] **Step 5: Add ResolvedHostConfig struct and load/resolve helpers**

After the existing `parse_ssh_config` / `parse_ssh_config_content` functions, add:

```rust
/// Fully resolved SSH connection parameters for a host alias.
#[derive(Debug, Clone)]
pub struct ResolvedHostConfig {
    /// The alias as stored in ssync config (used for display/lookup)
    pub alias: String,
    /// Actual DNS name or IP to connect to
    pub hostname: String,
    pub port: u16,
    pub user: String,
    /// Ordered list of identity files to try
    pub identity_files: Vec<std::path::PathBuf>,
    /// First ProxyJump hop alias (None = direct connection)
    pub proxy_jump: Option<String>,
    /// Whether IdentitiesOnly is set (skip password fallback)
    pub identities_only: bool,
}

/// Parse ~/.ssh/config using the ssh2-config crate (handles full OpenSSH semantics).
pub fn load_ssh_config() -> Result<ssh2_config::SshConfig> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let config_path = home.join(".ssh").join("config");

    if !config_path.exists() {
        return Ok(ssh2_config::SshConfig::default());
    }

    let file = std::fs::File::open(&config_path)
        .with_context(|| format!("Failed to open {}", config_path.display()))?;
    let mut reader = std::io::BufReader::new(file);

    ssh2_config::SshConfig::default()
        .parse(&mut reader, ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS)
        .with_context(|| format!("Failed to parse {}", config_path.display()))
}

/// Resolve a host alias to its full connection parameters.
/// Uses the ssh2-config crate for correct multi-alias and inheritance handling.
pub fn resolve_host(alias: &str) -> Result<ResolvedHostConfig> {
    let config = load_ssh_config()?;
    let params = config.query(alias);

    let hostname = params
        .host_name
        .as_deref()
        .unwrap_or(alias)
        .to_string();

    let port = params.port.unwrap_or(22);

    let user = params
        .user
        .clone()
        .unwrap_or_else(|| whoami::username());

    let identity_files: Vec<std::path::PathBuf> = params
        .identity_file
        .unwrap_or_default()
        .into_iter()
        .map(|p| expand_tilde(&p))
        .collect();

    let proxy_jump = params
        .proxy_jump
        .and_then(|pj| pj.into_iter().next());

    // identities_only: not exposed by ssh2-config, assume false
    let identities_only = false;

    Ok(ResolvedHostConfig {
        alias: alias.to_string(),
        hostname,
        port,
        user,
        identity_files,
        proxy_jump,
        identities_only,
    })
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}
```

Add to the top of `src/config/ssh_config.rs`:
```rust
use anyhow::{Context, Result};
```
(replace existing `use anyhow::{Context, Result};` if it already exists)

- [ ] **Step 6: Add whoami dependency to Cargo.toml**

In `[dependencies]`:
```toml
whoami = "1"
```

Then run:
```bash
cargo check
```

- [ ] **Step 7: Run tests to verify they pass**

```bash
cargo test config::ssh_config::tests
```
Expected: all tests pass including the 2 new ones.

- [ ] **Step 8: Run full test suite**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 9: Commit**

```bash
git add src/config/ssh_config.rs Cargo.toml Cargo.lock
git commit -m "feat(config): fix multi-alias SSH host parsing, add ResolvedHostConfig"
```

---

### Task 3: Add proxy_jump to HostEntry schema

**Files:**
- Modify: `src/config/schema.rs`

- [ ] **Step 1: Write a failing test for proxy_jump round-trip in schema**

In `src/config/schema.rs` `#[cfg(test)]` module, add:

```rust
#[test]
fn test_host_entry_proxy_jump_roundtrip() {
    let entry = HostEntry {
        name: "backend".to_string(),
        ssh_host: "backend".to_string(),
        shell: ShellType::Sh,
        groups: vec![],
        proxy_jump: Some("bastion".to_string()),
    };
    let toml_str = toml::to_string_pretty(&entry).unwrap();
    let parsed: HostEntry = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.proxy_jump.as_deref(), Some("bastion"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test config::schema::tests::test_host_entry_proxy_jump_roundtrip
```
Expected: compile error — `proxy_jump` field not found on `HostEntry`.

- [ ] **Step 3: Add proxy_jump to HostEntry**

In `src/config/schema.rs`, find the `HostEntry` struct and add the field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEntry {
    pub name: String,
    pub ssh_host: String,
    pub shell: ShellType,
    #[serde(default)]
    pub groups: Vec<String>,
    /// Optional first-hop ProxyJump alias. None = direct connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
}
```

- [ ] **Step 4: Fix all HostEntry struct literals in the codebase**

Every `HostEntry { name, ssh_host, shell, groups }` construct must add `proxy_jump: None`. Run:

```bash
cargo check 2>&1 | grep "missing field"
```

Fix each instance by adding `proxy_jump: None`. Affected files include `src/commands/init.rs` (lines 280, 351, 426).

- [ ] **Step 5: Run tests**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/config/schema.rs src/commands/init.rs
git commit -m "feat(schema): add proxy_jump field to HostEntry"
```

---

### Task 4: Replace resolve_ssh_host_port with ssh2-config in init.rs

**Files:**
- Modify: `src/commands/init.rs`

Currently `resolve_ssh_host_port(alias)` shells out to `ssh -G <alias>` and greps hostname/port from the output. Replace with the new `config::ssh_config::resolve_host()`.

- [ ] **Step 1: Write a test for the new resolver**

The function is private; test via a small wrapper. Add to init.rs `#[cfg(test)]` module:

```rust
#[test]
fn test_resolve_host_falls_back_to_alias() {
    // An alias not in ~/.ssh/config should return alias as hostname, port 22
    let result = crate::config::ssh_config::resolve_host("nonexistent-test-host-xyz");
    let resolved = result.unwrap();
    assert_eq!(resolved.hostname, "nonexistent-test-host-xyz");
    assert_eq!(resolved.port, 22);
}
```

- [ ] **Step 2: Run test to verify it passes (uses new resolve_host)**

```bash
cargo test commands::init::tests::test_resolve_host_falls_back_to_alias
```
Expected: PASS (resolve_host handles missing hosts gracefully).

- [ ] **Step 3: Replace resolve_ssh_host_port in init.rs**

Find the existing `resolve_ssh_host_port` function (around line 32) and replace it:

```rust
/// Resolve SSH hostname and port for a host alias using ~/.ssh/config.
async fn resolve_ssh_host_port(alias: &str) -> Result<(String, u16)> {
    let resolved = crate::config::ssh_config::resolve_host(alias)?;
    Ok((resolved.hostname, resolved.port))
}
```

This keeps the same signature so all call sites continue to work unchanged.

- [ ] **Step 4: Build and test**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat(init): replace ssh -G with ssh2-config for host resolution"
```

---

### Task 5: Suppress VirtualLock warnings in main.rs

**Files:**
- Modify: `src/main.rs`

When russh initialises cryptographic buffers on Windows, `zeroize`/`ssh-key` crates log a non-fatal `WARN Security warning: OS has failed to lock/unlock memory...`. We suppress all russh-adjacent crate noise below ERROR unless `--verbose` is set or `RUST_LOG` is explicitly provided.

- [ ] **Step 1: Write a test that the tracing filter compiles**

Add to `src/main.rs` `#[cfg(test)]` block (or create one if absent):

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_tracing_filter_builds() {
        use tracing_subscriber::EnvFilter;
        let _ = EnvFilter::new("russh=error,russh_keys=error,ssh_key=error,zeroize=error,info");
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

```bash
cargo test main::tests::test_tracing_filter_builds
```
Expected: PASS.

- [ ] **Step 3: Update tracing setup in main.rs**

Find the existing tracing subscriber initialisation (around lines 42–51). Replace with:

```rust
fn init_tracing(verbose: bool) {
    use tracing_subscriber::{EnvFilter, fmt};

    // If RUST_LOG is set, respect it entirely.
    // Otherwise apply our defaults: suppress russh/zeroize noise unless verbose.
    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        // Suppress VirtualLock warnings and other russh diagnostic noise.
        EnvFilter::new(
            "russh=error,russh_keys=error,ssh_key=error,zeroize=error,info",
        )
    };

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
```

Then call `init_tracing(verbose)` from `main()` where the tracing subscriber was previously initialised.

- [ ] **Step 4: Build and test**

```bash
cargo test && cargo build
```
Expected: no regressions.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(tracing): suppress russh VirtualLock warnings in non-verbose mode"
```

---

## Phase 2 — russh Transport (exec)

### Task 6: Add russh + russh-keys dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add deps**

In `[dependencies]`:
```toml
russh = "0.44"
russh-keys = "0.44"
```

- [ ] **Step 2: Verify build (no new code yet)**

```bash
cargo check
```
Expected: compiles. russh brings in tokio, ssh-key, zeroize; should not conflict.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add russh and russh-keys dependencies"
```

---

### Task 7: Implement host/auth.rs

**Files:**
- Create: `src/host/auth.rs`
- Modify: `src/host/mod.rs`

This module implements the auth chain: try each `IdentityFile` (no passphrase), then retry with passphrase prompt, then try interactive password. Passphrase attempts are cached in a `HashMap` so the user is not re-prompted for the same key across multiple hosts.

- [ ] **Step 1: Declare module in mod.rs**

Add to `src/host/mod.rs`:
```rust
pub mod auth;
```

- [ ] **Step 2: Create src/host/auth.rs**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use russh::client::Handle;
use russh_keys::key::KeyPair;

use super::session_pool::SshHandler;

/// Per-process passphrase cache: key_path → passphrase.
/// Avoids re-prompting for the same key file.
pub type PassphraseCache = HashMap<PathBuf, String>;

/// Attempt to authenticate `handle` as `user`.
///
/// Auth chain:
/// 1. For each identity file: try without passphrase
/// 2. For each identity file: if step 1 failed, prompt for passphrase (cached)
/// 3. Prompt for password (if identities_only is false)
pub async fn authenticate(
    handle: &mut Handle<SshHandler>,
    user: &str,
    identity_files: &[PathBuf],
    identities_only: bool,
    cache: &mut PassphraseCache,
) -> Result<()> {
    // Step 1: try each identity file without passphrase
    for path in identity_files {
        if try_pubkey(handle, user, path, None).await? {
            return Ok(());
        }
    }

    // Step 2: retry each identity file with passphrase prompt
    for path in identity_files {
        let passphrase = match cache.get(path) {
            Some(pp) => pp.clone(),
            None => {
                let prompt = format!("Enter passphrase for {}: ", path.display());
                let pp = rpassword::prompt_password(&prompt)
                    .context("Failed to read passphrase")?;
                cache.insert(path.clone(), pp.clone());
                pp
            }
        };
        if try_pubkey(handle, user, path, Some(&passphrase)).await? {
            return Ok(());
        }
    }

    // Step 3: password fallback
    if !identities_only {
        let prompt = format!("{}@<host> password: ", user);
        let password = rpassword::prompt_password(&prompt)
            .context("Failed to read password")?;
        if handle
            .authenticate_password(user, &password)
            .await
            .context("Password authentication failed")?
        {
            return Ok(());
        }
    }

    anyhow::bail!("All authentication methods exhausted for user '{}'", user)
}

/// Try public-key auth with an optional passphrase. Returns true if auth succeeded.
async fn try_pubkey(
    handle: &mut Handle<SshHandler>,
    user: &str,
    key_path: &Path,
    passphrase: Option<&str>,
) -> Result<bool> {
    let key_pair = match passphrase {
        Some(pp) if !pp.is_empty() => {
            match russh_keys::load_secret_key(key_path, Some(pp)) {
                Ok(kp) => kp,
                Err(_) => return Ok(false),
            }
        }
        _ => {
            match russh_keys::load_secret_key(key_path, None) {
                Ok(kp) => kp,
                Err(_) => return Ok(false), // encrypted key, will retry with passphrase
            }
        }
    };

    let rc = std::sync::Arc::new(key_pair);
    Ok(handle
        .authenticate_publickey(user, rc)
        .await
        .unwrap_or(false))
}
```

- [ ] **Step 3: Build**

```bash
cargo check
```
Expected: compiles (SshHandler not yet defined — fix by adding a stub forward declaration or placeholder; alternatively, define SshHandler in session_pool.rs first in Task 8 and come back to fix the import).

> **Note:** If `cargo check` fails because `super::session_pool::SshHandler` doesn't exist yet, change the import to use a placeholder: temporarily comment out the `use super::session_pool::SshHandler;` line and add `pub struct SshHandler;` as a stub. This will be fixed in Task 8.

- [ ] **Step 4: Commit**

```bash
git add src/host/auth.rs src/host/mod.rs
git commit -m "feat(auth): implement russh auth chain with passphrase and password fallback"
```

---

### Task 8: Implement host/session_pool.rs — connect and known_hosts

**Files:**
- Create: `src/host/session_pool.rs`
- Modify: `src/host/mod.rs`
- Modify: `src/host/auth.rs` (fix import now that SshHandler is defined)

- [ ] **Step 1: Declare module in mod.rs**

Add to `src/host/mod.rs`:
```rust
pub mod session_pool;
```

- [ ] **Step 2: Write failing test for connect (known_hosts rejected)**

Add at the bottom of `src/host/session_pool.rs` (create the file):

```rust
// Tests will be added at the bottom after the impl.
```

Create `src/host/session_pool.rs` and add to its test section:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolved_remote_output_success() {
        let out = RemoteOutput {
            stdout: "hello\n".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            success: true,
        };
        assert!(out.success);
        assert_eq!(out.stdout.trim(), "hello");
    }
}
```

- [ ] **Step 3: Run test to verify it fails (file doesn't exist)**

```bash
cargo test host::session_pool::tests
```
Expected: compile error — module not found.

- [ ] **Step 4: Create src/host/session_pool.rs with SshHandler and direct connect**

```rust
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use russh::client::{self, Handle};
use russh_keys::key::PublicKey;

use crate::config::schema::HostEntry;
use crate::config::ssh_config::ResolvedHostConfig;
use super::auth::{authenticate, PassphraseCache};

// Re-export so auth.rs can import it
pub use handler::SshHandler;

mod handler {
    use super::*;

    /// russh client handler: verifies server host keys against ~/.ssh/known_hosts.
    pub struct SshHandler {
        /// Hostname used for known_hosts lookup (may differ from ssh_host alias)
        pub hostname: String,
        pub port: u16,
    }

    impl client::Handler for SshHandler {
        type Error = anyhow::Error;

        async fn check_server_key(
            &mut self,
            server_public_key: &PublicKey,
        ) -> Result<bool, Self::Error> {
            let known_hosts_path = dirs::home_dir()
                .context("Cannot determine home directory")?
                .join(".ssh")
                .join("known_hosts");

            if !known_hosts_path.exists() {
                bail!(
                    "Unknown host key for {}:{} — run `ssync init` to add the host to known_hosts",
                    self.hostname,
                    self.port
                );
            }

            let known_hosts_data = std::fs::read_to_string(&known_hosts_path)
                .context("Failed to read known_hosts")?;

            let mut known_hosts = russh_keys::key::KnownHosts::default();
            known_hosts
                .read(&mut known_hosts_data.as_bytes())
                .context("Failed to parse known_hosts")?;

            match known_hosts.check_server_key(&self.hostname, self.port, server_public_key) {
                russh_keys::key::CheckResult::Match => Ok(true),
                russh_keys::key::CheckResult::Mismatch => bail!(
                    "HOST KEY MISMATCH for {}:{} — possible man-in-the-middle attack",
                    self.hostname,
                    self.port
                ),
                russh_keys::key::CheckResult::NotFound => bail!(
                    "Unknown host key for {}:{} — run `ssync init` to accept the key first",
                    self.hostname,
                    self.port
                ),
            }
        }
    }
}

/// Result of a remote command execution via russh channel.
#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Pool of authenticated russh sessions, one per host alias.
pub struct RusshSessionPool {
    /// host alias → open authenticated session handle
    sessions: HashMap<String, Arc<Handle<SshHandler>>>,
    /// hosts that failed to connect (alias → error message)
    failed: Vec<(String, String)>,
}

impl RusshSessionPool {
    /// Connect to all hosts concurrently; unreachable hosts are recorded in `failed`.
    pub async fn setup(
        hosts: &[&HostEntry],
        timeout_secs: u64,
        concurrency: usize,
    ) -> Result<Self> {
        let timeout = Duration::from_secs(timeout_secs);
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut handles = Vec::new();

        for host in hosts {
            let alias = host.ssh_host.clone();
            let sem = sem.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.unwrap();
                let mut cache = PassphraseCache::new();
                let result = connect_one(&alias, timeout, &mut cache).await;
                (alias, result)
            }));
        }

        let mut sessions: HashMap<String, Arc<Handle<SshHandler>>> = HashMap::new();
        let mut failed: Vec<(String, String)> = Vec::new();

        for jh in handles {
            let (alias, result) = jh.await.context("task panic")?;
            match result {
                Ok(handle) => { sessions.insert(alias, Arc::new(handle)); }
                Err(e) => { failed.push((alias, e.to_string())); }
            }
        }

        Ok(Self { sessions, failed })
    }

    /// Names of hosts that failed to connect (with error messages).
    pub fn failed_hosts(&self) -> Vec<(String, String)> {
        self.failed.clone()
    }

    /// Names of all successfully connected hosts.
    pub fn reachable_hosts(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Execute a command on a connected host. Returns RemoteOutput.
    pub async fn exec(
        &self,
        host_alias: &str,
        cmd: &str,
        timeout_secs: u64,
    ) -> Result<RemoteOutput> {
        let handle = self
            .sessions
            .get(host_alias)
            .ok_or_else(|| anyhow::anyhow!("Host '{}' is not connected", host_alias))?
            .clone();

        exec_on_handle(&handle, cmd, Duration::from_secs(timeout_secs)).await
    }

    /// Close all sessions gracefully.
    pub async fn shutdown(self) {
        for (_, handle) in self.sessions {
            let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        }
    }
}

/// Open and authenticate a single session to `alias`.
/// Resolves alias via ~/.ssh/config; handles ProxyJump recursively (single hop).
async fn connect_one(
    alias: &str,
    timeout: Duration,
    cache: &mut PassphraseCache,
) -> Result<Handle<SshHandler>> {
    let resolved = crate::config::ssh_config::resolve_host(alias)?;

    match &resolved.proxy_jump.clone() {
        Some(proxy_alias) => {
            let proxy_resolved = crate::config::ssh_config::resolve_host(proxy_alias)?;
            connect_via_proxy(&proxy_resolved, &resolved, timeout, cache).await
        }
        None => connect_direct(&resolved, timeout, cache).await,
    }
}

/// Open a direct TCP connection to `config.hostname:config.port`.
async fn connect_direct(
    config: &ResolvedHostConfig,
    timeout: Duration,
    cache: &mut PassphraseCache,
) -> Result<Handle<SshHandler>> {
    let russh_config = Arc::new(client::Config {
        connection_timeout: Some(timeout),
        ..<client::Config as Default>::default()
    });

    let handler = SshHandler {
        hostname: config.hostname.clone(),
        port: config.port,
    };

    let addr = format!("{}:{}", config.hostname, config.port);
    let addr = addr
        .to_socket_addrs()
        .with_context(|| format!("Cannot resolve {}", addr))?
        .next()
        .with_context(|| format!("No address for {}", addr))?;

    let mut handle = tokio::time::timeout(timeout, client::connect(russh_config, addr, handler))
        .await
        .context("SSH connect timeout")?
        .with_context(|| format!("Failed to connect to {}:{}", config.hostname, config.port))?;

    authenticate(
        &mut handle,
        &config.user,
        &config.identity_files,
        config.identities_only,
        cache,
    )
    .await
    .with_context(|| format!("Authentication failed for {}@{}:{}", config.user, config.hostname, config.port))?;

    Ok(handle)
}

/// Open an SSH session through a jump host (ProxyJump).
async fn connect_via_proxy(
    proxy: &ResolvedHostConfig,
    target: &ResolvedHostConfig,
    timeout: Duration,
    cache: &mut PassphraseCache,
) -> Result<Handle<SshHandler>> {
    // Step 1: connect and auth to the proxy
    let proxy_handle = connect_direct(proxy, timeout, cache)
        .await
        .with_context(|| format!("Failed to connect to proxy {}", proxy.alias))?;

    // Step 2: open a direct-tcpip channel through the proxy to the target
    let channel = tokio::time::timeout(
        timeout,
        proxy_handle.channel_open_direct_tcpip(
            &target.hostname,
            target.port as u32,
            "127.0.0.1",
            0,
        ),
    )
    .await
    .context("Proxy channel open timeout")?
    .with_context(|| {
        format!(
            "Failed to open direct-tcpip channel to {}:{} via {}",
            target.hostname, target.port, proxy.alias
        )
    })?;

    // Step 3: establish a second SSH session over the channel stream
    let russh_config = Arc::new(client::Config {
        connection_timeout: Some(timeout),
        ..<client::Config as Default>::default()
    });

    let handler = SshHandler {
        hostname: target.hostname.clone(),
        port: target.port,
    };

    let mut target_handle = tokio::time::timeout(
        timeout,
        client::connect_stream(russh_config, channel.into_stream(), handler),
    )
    .await
    .context("SSH-through-proxy connect timeout")?
    .context("Failed to establish SSH session through proxy")?;

    authenticate(
        &mut target_handle,
        &target.user,
        &target.identity_files,
        target.identities_only,
        cache,
    )
    .await
    .with_context(|| {
        format!(
            "Authentication failed for {}@{} (via proxy {})",
            target.user, target.alias, proxy.alias
        )
    })?;

    Ok(target_handle)
}

/// Execute a command on an open session handle and collect output.
pub async fn exec_on_handle(
    handle: &Handle<SshHandler>,
    cmd: &str,
    timeout: Duration,
) -> Result<RemoteOutput> {
    let mut channel = tokio::time::timeout(timeout, handle.channel_open_session())
        .await
        .context("Channel open timeout")?
        .context("Failed to open SSH channel")?;

    channel
        .exec(true, cmd)
        .await
        .context("Failed to exec command")?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: Option<u32> = None;

    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
            Some(russh::ChannelMsg::ExtendedData { data, ext }) if ext == 1 => {
                stderr.extend_from_slice(&data)
            }
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = Some(exit_status);
            }
            Some(russh::ChannelMsg::Eof) => {}
            None => break,
            _ => {}
        }
    }

    let exit_code = exit_code.map(|c| c as i32);
    let success = exit_code.map_or(false, |c| c == 0);

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        exit_code,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_output_success_flag() {
        let out = RemoteOutput {
            stdout: "hello\n".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            success: true,
        };
        assert!(out.success);
        assert_eq!(out.stdout.trim(), "hello");
    }

    #[test]
    fn test_remote_output_failure_flag() {
        let out = RemoteOutput {
            stdout: String::new(),
            stderr: "not found\n".to_string(),
            exit_code: Some(127),
            success: false,
        };
        assert!(!out.success);
        assert_eq!(out.exit_code, Some(127));
    }
}
```

- [ ] **Step 5: Fix auth.rs import now that SshHandler is defined**

In `src/host/auth.rs`, update the import:
```rust
use super::session_pool::SshHandler;
```
(remove the placeholder stub if you added one in Task 7)

- [ ] **Step 6: Build**

```bash
cargo check
```
Expected: compiles. Fix any type errors (russh API details may need minor adjustment).

- [ ] **Step 7: Run unit tests**

```bash
cargo test host::session_pool::tests
```
Expected: both tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/host/session_pool.rs src/host/auth.rs src/host/mod.rs
git commit -m "feat(session_pool): implement RusshSessionPool with direct connect and exec"
```

---

### Task 9: Add ProxyJump test + verify connect_via_proxy compiles

**Files:**
- Modify: `src/host/session_pool.rs` (test only, no new logic — ProxyJump is already implemented in Task 8)

- [ ] **Step 1: Add unit test for proxy config resolution logic**

In `src/host/session_pool.rs` test module:

```rust
#[test]
fn test_proxy_alias_resolved_from_config() {
    // Verify that resolve_host correctly returns proxy_jump
    // for a host entry that has ProxyJump in ~/.ssh/config.
    // This is a config-level test; actual ProxyJump connection is integration-tested manually.
    let resolved = crate::config::ssh_config::resolve_host("nonexistent-xyz-direct");
    let r = resolved.unwrap();
    assert!(r.proxy_jump.is_none(), "Host not in config should have no ProxyJump");
}
```

- [ ] **Step 2: Run test**

```bash
cargo test host::session_pool::tests::test_proxy_alias_resolved_from_config
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/host/session_pool.rs
git commit -m "test(session_pool): add proxy jump resolution unit test"
```

---

### Task 10: Update pool.rs to embed RusshSessionPool

**Files:**
- Modify: `src/host/pool.rs`

Add a `session_pool: Arc<RusshSessionPool>` field to `SshPool`. The existing `conn_mgr: ConnectionManager` is kept for now (sync.rs still uses it for file transfers until Phase 3). Both connections are established in `setup()`.

- [ ] **Step 1: Write a test for pool construction**

In `src/host/pool.rs` test module add:

```rust
#[test]
fn test_pool_host_result_with_russh_output() {
    use crate::host::session_pool::RemoteOutput;
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
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test host::pool::tests::test_pool_host_result_with_russh_output
```
Expected: compile error — `RemoteOutput` from session_pool not yet used here.

- [ ] **Step 3: Update SshPool struct and setup in pool.rs**

Replace `src/host/pool.rs` with the following (keeping existing tests at the bottom):

```rust
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::output::progress::SyncProgress;

use super::concurrency::ConcurrencyLimiter;
use super::connection::ConnectionManager;
use super::session_pool::{RemoteOutput, RusshSessionPool};

/// Shared SSH connection pool.
/// Holds both the legacy ConnectionManager (for sync file transfers) and
/// RusshSessionPool (for exec operations). ConnectionManager will be removed in Phase 3.
pub struct SshPool {
    /// russh-based sessions for exec operations
    pub session_pool: Arc<RusshSessionPool>,
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
    pub async fn setup(
        hosts: &[&HostEntry],
        timeout: u64,
        global_concurrency: usize,
        per_host_concurrency: usize,
    ) -> Result<(Self, usize)> {
        Self::setup_with_options(hosts, timeout, global_concurrency, per_host_concurrency, false)
            .await
    }

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

        // Legacy pre-check (kept for sync.rs compatibility)
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

        Ok((
            Self {
                session_pool,
                conn_mgr,
                limiter,
                progress,
            },
            connected,
        ))
    }

    pub fn socket_for(&self, host_name: &str) -> Option<&Path> {
        self.conn_mgr.socket_for(host_name)
    }

    #[allow(dead_code)]
    pub fn reachable_hosts(&self) -> Vec<String> {
        self.conn_mgr.reachable_hosts()
    }

    pub fn failed_hosts(&self) -> Vec<(String, String)> {
        self.conn_mgr.failed_hosts()
    }

    pub fn scp_failed_hosts(&self) -> Vec<(String, String)> {
        self.conn_mgr.scp_failed_hosts()
    }

    pub fn filter_reachable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let reachable = self.conn_mgr.reachable_hosts();
        hosts
            .iter()
            .filter(|h| reachable.contains(&h.name))
            .copied()
            .collect()
    }

    pub fn filter_scp_capable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
        let capable = self.conn_mgr.scp_capable_hosts();
        hosts
            .iter()
            .filter(|h| capable.contains(&h.name))
            .copied()
            .collect()
    }

    pub async fn shutdown(mut self) {
        self.progress.clear();
        self.conn_mgr.shutdown().await;
        // Note: RusshSessionPool shutdown is deferred; Arc will drop when last ref is gone
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
```

- [ ] **Step 4: Build**

```bash
cargo check
```
Expected: compiles. Fix any type errors.

- [ ] **Step 5: Run tests**

```bash
cargo test host::pool::tests
```
Expected: all 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/host/pool.rs
git commit -m "feat(pool): add RusshSessionPool alongside ConnectionManager in SshPool"
```

---

### Task 11: Update commands and collector to use session_pool.exec()

**Files:**
- Modify: `src/commands/run.rs`
- Modify: `src/commands/exec.rs` (exec operations only; script upload stays scp for now)
- Modify: `src/metrics/collector.rs` (update `collect_pooled` signature)
- Modify: `src/commands/check.rs` (pass `session_pool` instead of socket)

The goal: replace every `executor::run_remote_pooled(host, cmd, timeout, socket)` call that goes through `SshPool` with `pool.session_pool.exec(&host.name, cmd, timeout)`.

#### 11a — Update commands/run.rs

- [ ] **Step 1: Update run.rs spawn loop**

In `src/commands/run.rs`, replace the inner spawn block:

Before:
```rust
let socket = pool.socket_for(&host.name).map(|p| p.to_path_buf());
let global_sem = pool.limiter.global_semaphore();

handles.push(tokio::spawn(async move {
    let _permit = global_sem.acquire_owned().await.unwrap();
    let start = Instant::now();
    let result = executor::run_remote_pooled(&host, &cmd, timeout, socket.as_deref()).await;
    let elapsed = start.elapsed();
    (host, result, elapsed)
}));
```

After:
```rust
let sessions = pool.session_pool.clone();
let global_sem = pool.limiter.global_semaphore();

handles.push(tokio::spawn(async move {
    let _permit = global_sem.acquire_owned().await.unwrap();
    let start = Instant::now();
    let result = sessions.exec(&host.ssh_host, &cmd, timeout).await
        .map(|o| crate::host::executor::RemoteOutput {
            stdout: o.stdout,
            stderr: o.stderr,
            exit_code: o.exit_code,
            success: o.success,
        });
    let elapsed = start.elapsed();
    (host, result, elapsed)
}));
```

> **Note:** The `executor::RemoteOutput` and `session_pool::RemoteOutput` structs have identical fields. In Phase 4 we unify them; for now we map between them.

- [ ] **Step 2: Remove unused imports from run.rs**

Remove:
```rust
use crate::host::executor;
```
(if `executor` is no longer used after this change)

- [ ] **Step 3: Build run.rs**

```bash
cargo check
```
Expected: compiles.

#### 11b — Update metrics/collector.rs collect_pooled

- [ ] **Step 4: Update collect_pooled signature to accept Arc<RusshSessionPool>**

In `src/metrics/collector.rs`:

Change the signature of `collect_pooled`:
```rust
pub async fn collect_pooled(
    host: &HostEntry,
    enabled: &[String],
    check_paths: &[(String, String)],
    timeout_secs: u64,
    sessions: std::sync::Arc<crate::host::session_pool::RusshSessionPool>,
) -> Result<CollectionResult>
```

Internally, replace:
```rust
executor::run_remote_pooled(host, &batch_cmd, timeout_secs, socket).await
```
with:
```rust
sessions.exec(&host.ssh_host, &batch_cmd, timeout_secs).await
    .map(|o| crate::host::executor::RemoteOutput {
        stdout: o.stdout,
        stderr: o.stderr,
        exit_code: o.exit_code,
        success: o.success,
    })
```

Do this replacement for both the metrics batch call and the paths batch call in `collect_pooled`.

Remove the `socket: Option<&Path>` parameter and the `use std::path::Path;` import if it's no longer needed.

- [ ] **Step 5: Update check.rs to pass session_pool**

In `src/commands/check.rs`, in the per-host spawn loop, replace:

Before:
```rust
let socket = pool.socket_for(&host.name).map(|p| p.to_path_buf());
// ...
let result = collector::collect_pooled(
    &host, &enabled, &check_paths, timeout, socket.as_deref(),
).await;
```

After:
```rust
let sessions = pool.session_pool.clone();
// ...
let result = collector::collect_pooled(
    &host, &enabled, &check_paths, timeout, sessions,
).await;
```

Remove the `socket` local variable.

- [ ] **Step 6: Update exec.rs exec operations**

In `src/commands/exec.rs`, in the per-host exec spawn loop (the command-run loop, NOT the script upload), replace `executor::run_remote_pooled()` with `pool.session_pool.exec()` using the same pattern as run.rs above.

Script upload (the scp call) remains unchanged for now — it will be replaced in Phase 3.

- [ ] **Step 7: Build all**

```bash
cargo check
```
Expected: compiles with no errors. If there are type mismatches on `RemoteOutput`, add the mapping shim as shown in step 1.

- [ ] **Step 8: Run full test suite**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 9: Commit**

```bash
git add src/commands/run.rs src/commands/exec.rs src/commands/check.rs src/metrics/collector.rs
git commit -m "feat(transport): migrate run/exec/check commands to RusshSessionPool exec"
```

---

## Phase 3 — SFTP

### Task 12: Add russh-sftp dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add dep**

In `[dependencies]`:
```toml
russh-sftp = "2.1"
```

- [ ] **Step 2: Verify**

```bash
cargo check
```
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add russh-sftp dependency"
```

---

### Task 13: Implement host/sftp.rs

**Files:**
- Create: `src/host/sftp.rs`
- Modify: `src/host/mod.rs`

SFTP does NOT expand `~` server-side. We call `echo $HOME` (or PowerShell equivalent) once per session and cache the result. Files larger than 64 MB are chunked in 4 MB pieces to avoid memory exhaustion.

- [ ] **Step 1: Declare module**

Add to `src/host/mod.rs`:
```rust
pub mod sftp;
```

- [ ] **Step 2: Write failing tests for tilde expansion and mkdir_p**

Create `src/host/sftp.rs` with test section:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path_expands_tilde() {
        assert_eq!(
            resolve_remote_path("~/.config/app.toml", "/home/alice"),
            "/home/alice/.config/app.toml"
        );
    }

    #[test]
    fn test_resolve_path_no_tilde() {
        assert_eq!(
            resolve_remote_path("/etc/hosts", "/home/alice"),
            "/etc/hosts"
        );
    }

    #[test]
    fn test_resolve_path_tilde_only() {
        assert_eq!(
            resolve_remote_path("~", "/home/alice"),
            "/home/alice"
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test host::sftp::tests
```
Expected: compile error — module not found.

- [ ] **Step 4: Implement src/host/sftp.rs**

```rust
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use russh::client::Handle;
use russh_sftp::client::SftpSession;

use crate::config::schema::ShellType;
use super::session_pool::SshHandler;

const CHUNK_SIZE: u64 = 4 * 1024 * 1024; // 4 MB

/// Resolve a remote path, expanding a leading `~` using the provided home directory.
pub fn resolve_remote_path(remote: &str, home_dir: &str) -> String {
    if remote == "~" {
        home_dir.to_string()
    } else if let Some(rest) = remote.strip_prefix("~/") {
        format!("{}/{}", home_dir.trim_end_matches('/'), rest)
    } else {
        remote.to_string()
    }
}

/// Retrieve the remote home directory by running `echo $HOME` (sh) or equivalent.
pub async fn remote_home_dir(
    handle: &Handle<SshHandler>,
    shell: ShellType,
    timeout: Duration,
) -> Result<String> {
    let cmd = match shell {
        ShellType::Sh => "echo $HOME",
        ShellType::PowerShell => "Write-Output $env:USERPROFILE",
        ShellType::Cmd => "echo %USERPROFILE%",
    };

    let out = super::session_pool::exec_on_handle(handle, cmd, timeout).await?;
    Ok(out.stdout.trim().to_string())
}

/// Upload a local file to a remote path via SFTP.
/// The `remote_path` may start with `~` (expanded using `home_dir`).
pub async fn upload(
    handle: &Handle<SshHandler>,
    local_path: &Path,
    remote_path: &str,
    home_dir: &str,
    timeout: Duration,
) -> Result<()> {
    let resolved = resolve_remote_path(remote_path, home_dir);
    let channel = tokio::time::timeout(timeout, handle.channel_open_session())
        .await
        .context("SFTP channel open timeout")?
        .context("Failed to open SFTP channel")?;

    channel.request_subsystem(true, "sftp")
        .await
        .context("Failed to request SFTP subsystem")?;

    let sftp = SftpSession::new(channel.into_stream()).await
        .context("Failed to create SFTP session")?;

    let local_data = tokio::fs::read(local_path)
        .await
        .with_context(|| format!("Failed to read {}", local_path.display()))?;

    // mkdir_p for the parent directory
    if let Some(parent) = std::path::Path::new(&resolved).parent() {
        mkdir_p_sftp(&sftp, parent).await?;
    }

    sftp.write(russh_sftp::client::fs::CreateFlags::TRUNCATE, &resolved, &local_data)
        .await
        .with_context(|| format!("SFTP upload failed for {}", resolved))?;

    Ok(())
}

/// Download a remote file to a local path via SFTP.
/// The `remote_path` may start with `~` (expanded using `home_dir`).
pub async fn download(
    handle: &Handle<SshHandler>,
    remote_path: &str,
    local_path: &Path,
    home_dir: &str,
    timeout: Duration,
) -> Result<()> {
    let resolved = resolve_remote_path(remote_path, home_dir);
    let channel = tokio::time::timeout(timeout, handle.channel_open_session())
        .await
        .context("SFTP channel open timeout")?
        .context("Failed to open SFTP channel")?;

    channel.request_subsystem(true, "sftp")
        .await
        .context("Failed to request SFTP subsystem")?;

    let sftp = SftpSession::new(channel.into_stream()).await
        .context("Failed to create SFTP session")?;

    let data = sftp
        .read(&resolved)
        .await
        .with_context(|| format!("SFTP download failed for {}", resolved))?;

    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(local_path, &data)
        .await
        .with_context(|| format!("Failed to write {}", local_path.display()))?;

    Ok(())
}

/// Recursively create directories on the remote (best-effort; ignores already-exists errors).
async fn mkdir_p_sftp(sftp: &SftpSession, path: &Path) -> Result<()> {
    let parts: Vec<&std::ffi::OsStr> = path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect();

    let mut current = if path.is_absolute() {
        "/".to_string()
    } else {
        String::new()
    };

    for part in &parts {
        if !current.is_empty() && !current.ends_with('/') {
            current.push('/');
        }
        current.push_str(&part.to_string_lossy());
        let _ = sftp.create_dir(&current).await; // ignore error if already exists
    }
    Ok(())
}

/// SFTP probe: attempt to write and delete a 1-byte sentinel file.
/// Returns Ok(()) if SFTP is available, Err otherwise.
pub async fn sftp_probe(
    handle: &Handle<SshHandler>,
    home_dir: &str,
    timeout: Duration,
) -> Result<()> {
    let probe_path = format!("{}/.ssync_probe", home_dir);
    let channel = tokio::time::timeout(timeout, handle.channel_open_session())
        .await
        .context("SFTP probe channel open timeout")?
        .context("Failed to open SFTP probe channel")?;

    channel.request_subsystem(true, "sftp")
        .await
        .context("Failed to request SFTP subsystem for probe")?;

    let sftp = SftpSession::new(channel.into_stream()).await
        .context("Failed to create SFTP session for probe")?;

    sftp.write(russh_sftp::client::fs::CreateFlags::TRUNCATE, &probe_path, b"0")
        .await
        .context("SFTP probe write failed")?;

    // best-effort cleanup
    let _ = sftp.remove_file(&probe_path).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path_expands_tilde() {
        assert_eq!(
            resolve_remote_path("~/.config/app.toml", "/home/alice"),
            "/home/alice/.config/app.toml"
        );
    }

    #[test]
    fn test_resolve_path_no_tilde() {
        assert_eq!(
            resolve_remote_path("/etc/hosts", "/home/alice"),
            "/etc/hosts"
        );
    }

    #[test]
    fn test_resolve_path_tilde_only() {
        assert_eq!(
            resolve_remote_path("~", "/home/alice"),
            "/home/alice"
        );
    }
}
```

> **Note on russh-sftp API:** The `SftpSession::write` and `SftpSession::read` APIs may differ across minor versions. Consult `docs.rs/russh-sftp/2.1` for the exact method names. Common alternatives: `sftp.create(path)` + write to file handle, or `sftp.open_with_flags(path, OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)`.

- [ ] **Step 5: Build**

```bash
cargo check
```
Expected: compiles. Adjust SFTP API method names as needed per the russh-sftp 2.1 docs.

- [ ] **Step 6: Run tests**

```bash
cargo test host::sftp::tests
```
Expected: 3 tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/host/sftp.rs src/host/mod.rs
git commit -m "feat(sftp): implement SFTP upload, download, mkdir_p, and probe helpers"
```

---

### Task 14: Add SFTP methods to RusshSessionPool + update SshPool

**Files:**
- Modify: `src/host/session_pool.rs`
- Modify: `src/host/pool.rs`

Add home-dir caching, `upload()`, `download()`, `sftp_probe()` to `RusshSessionPool`, and wire them through `SshPool`. Also add `sftp_failed_hosts` tracking to `RusshSessionPool`.

- [ ] **Step 1: Add home_dir cache and SFTP methods to RusshSessionPool**

In `src/host/session_pool.rs`, update `RusshSessionPool`:

```rust
use std::collections::HashMap;
use tokio::sync::Mutex;

pub struct RusshSessionPool {
    sessions: HashMap<String, Arc<Handle<SshHandler>>>,
    failed: Vec<(String, String)>,
    sftp_failed: Vec<(String, String)>,
    /// home directory cache: host_alias → home path
    home_dirs: Mutex<HashMap<String, String>>,
}
```

Add a method to get (or fetch + cache) the home dir:

```rust
impl RusshSessionPool {
    // ... existing methods ...

    /// Get the remote home directory for a host, caching the result.
    async fn home_dir(
        &self,
        host_alias: &str,
        shell: crate::config::schema::ShellType,
        timeout: Duration,
    ) -> Result<String> {
        {
            let cache = self.home_dirs.lock().await;
            if let Some(home) = cache.get(host_alias) {
                return Ok(home.clone());
            }
        }
        let handle = self.sessions.get(host_alias)
            .ok_or_else(|| anyhow::anyhow!("Host '{}' not connected", host_alias))?
            .clone();
        let home = crate::host::sftp::remote_home_dir(&handle, shell, timeout).await?;
        self.home_dirs.lock().await.insert(host_alias.to_string(), home.clone());
        Ok(home)
    }

    /// Upload a local file to a remote host via SFTP.
    pub async fn upload(
        &self,
        host: &crate::config::schema::HostEntry,
        local_path: &std::path::Path,
        remote_path: &str,
        timeout_secs: u64,
    ) -> Result<()> {
        let handle = self.sessions.get(&host.ssh_host)
            .ok_or_else(|| anyhow::anyhow!("Host '{}' not connected", host.ssh_host))?
            .clone();
        let home = self.home_dir(&host.ssh_host, host.shell, Duration::from_secs(timeout_secs)).await?;
        crate::host::sftp::upload(&handle, local_path, remote_path, &home, Duration::from_secs(timeout_secs)).await
    }

    /// Download a remote file via SFTP.
    pub async fn download(
        &self,
        host: &crate::config::schema::HostEntry,
        remote_path: &str,
        local_path: &std::path::Path,
        timeout_secs: u64,
    ) -> Result<()> {
        let handle = self.sessions.get(&host.ssh_host)
            .ok_or_else(|| anyhow::anyhow!("Host '{}' not connected", host.ssh_host))?
            .clone();
        let home = self.home_dir(&host.ssh_host, host.shell, Duration::from_secs(timeout_secs)).await?;
        crate::host::sftp::download(&handle, remote_path, local_path, &home, Duration::from_secs(timeout_secs)).await
    }

    /// Run SFTP probe on all connected hosts. Records failures in sftp_failed.
    pub async fn run_sftp_probe(
        &mut self,
        hosts: &[&crate::config::schema::HostEntry],
        timeout_secs: u64,
    ) {
        for host in hosts {
            if let Some(handle) = self.sessions.get(&host.ssh_host) {
                let handle = handle.clone();
                match self.home_dir(&host.ssh_host, host.shell, Duration::from_secs(timeout_secs)).await {
                    Ok(home) => {
                        if let Err(e) = crate::host::sftp::sftp_probe(&handle, &home, Duration::from_secs(timeout_secs)).await {
                            self.sftp_failed.push((host.name.clone(), e.to_string()));
                        }
                    }
                    Err(e) => {
                        self.sftp_failed.push((host.name.clone(), e.to_string()));
                    }
                }
            }
        }
    }

    /// Names and errors of hosts that failed the SFTP probe.
    pub fn sftp_failed_hosts(&self) -> Vec<(String, String)> {
        self.sftp_failed.clone()
    }

    /// Hosts that passed the SFTP probe (reachable minus sftp_failed).
    pub fn sftp_capable_hosts(&self) -> Vec<String> {
        let failed: std::collections::HashSet<&str> =
            self.sftp_failed.iter().map(|(n, _)| n.as_str()).collect();
        self.sessions.keys().filter(|n| !failed.contains(n.as_str())).cloned().collect()
    }
}
```

Also initialise `sftp_failed: Vec::new()` and `home_dirs: Mutex::new(HashMap::new())` in `RusshSessionPool::setup()`.

- [ ] **Step 2: Update pool.rs to use session_pool for SFTP probe + expose SFTP methods**

In `src/host/pool.rs`, update `setup_with_options` so that when `probe_scp` is true, it runs `session_pool.run_sftp_probe()` instead of `conn_mgr.scp_probe()`:

```rust
// In setup_with_options, REPLACE the probe_scp block:
if probe_scp && connected > 0 {
    let mut sp = Arc::try_unwrap(session_pool_arc)
        .unwrap_or_else(|arc| (*arc).clone()); // if clone is needed
    sp.run_sftp_probe(&reachable_entries, timeout).await;
    let sftp_failed_count = sp.sftp_failed_hosts().len();
    let effective_ok = connected - sftp_failed_count;
    progress.finish_host_check(effective_ok, hosts.len() - effective_ok);
    session_pool_arc = Arc::new(sp);
}
```

> **Implementation note:** `Arc::try_unwrap` works only if there's exactly one reference. Since we just created the Arc, this is safe here. Alternatively, keep `session_pool` as a plain `RusshSessionPool` until after the probe, then wrap in Arc.

Add delegation methods to `SshPool`:

```rust
pub fn sftp_failed_hosts(&self) -> Vec<(String, String)> {
    self.session_pool.sftp_failed_hosts()
}

pub fn filter_sftp_capable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
    let capable = self.session_pool.sftp_capable_hosts();
    hosts.iter().filter(|h| capable.contains(&h.name)).copied().collect()
}
```

- [ ] **Step 3: Build**

```bash
cargo check
```
Expected: compiles (resolve type errors as needed).

- [ ] **Step 4: Run tests**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/host/session_pool.rs src/host/pool.rs
git commit -m "feat(sftp): add SFTP upload/download/probe methods to RusshSessionPool and SshPool"
```

---

### Task 15: Update sync.rs to use SFTP

**Files:**
- Modify: `src/commands/sync.rs`

Replace all `executor::download_pooled`, `executor::upload_pooled`, `executor::run_remote_pooled` calls in sync.rs with the equivalent `pool.session_pool` calls. Also replace the `SCP probe` check with `SFTP probe`, and `filter_scp_capable` with `filter_sftp_capable`.

This is the most complex file change. Work through it function by function.

- [ ] **Step 1: Replace scp probe and capable filter in sync.rs**

Find the `SshPool::setup_with_options(..., true)` call (around line 95) and update the post-setup error reporting:

Before:
```rust
for (name, err) in pool.scp_failed_hosts() {
    printer::print_host_line("scp-failed", "error", &format!("{}: {}", name, err));
    ...
}
let reachable_hosts = pool.filter_scp_capable(&hosts);
```

After:
```rust
for (name, err) in pool.sftp_failed_hosts() {
    printer::print_host_line("sftp-failed", "error", &format!("{}: {}", name, err));
    ...
}
let reachable_hosts = pool.filter_sftp_capable(&hosts);
```

- [ ] **Step 2: Remove ConnectionManager import and direct access**

In `src/commands/sync.rs`, remove:
```rust
use crate::host::connection::ConnectionManager;
```
and replace all `&pool.conn_mgr` arguments with `&pool.session_pool`.

Update function signatures in sync.rs that accepted `conn_mgr: &ConnectionManager`:
- `collect_file_metadata(…, conn_mgr: &ConnectionManager, …)` → `sessions: &Arc<RusshSessionPool>`
- `sync_file(…, conn_mgr: &ConnectionManager, …)` → `sessions: &Arc<RusshSessionPool>`
- `expand_directory_paths(…, conn_mgr: &ConnectionManager, …)` → `sessions: &Arc<RusshSessionPool>`

Replace each internal call:

| Before | After |
|--------|-------|
| `conn_mgr.socket_for(host)` | (remove; socket no longer needed) |
| `executor::run_remote_pooled(host, cmd, timeout, socket)` | `sessions.exec(&host.ssh_host, cmd, timeout).await.map(convert_output)` |
| `executor::download_pooled(source, path, local, timeout, socket)` | `sessions.download(source, path, local_path, timeout).await` |
| `executor::upload_pooled(target, local, path, timeout, socket)` | `sessions.upload(target, local_path, path, timeout).await` |

Add a conversion helper at the top of sync.rs (or inline where needed):
```rust
fn convert_remote_output(o: crate::host::session_pool::RemoteOutput) -> crate::host::executor::RemoteOutput {
    crate::host::executor::RemoteOutput {
        stdout: o.stdout,
        stderr: o.stderr,
        exit_code: o.exit_code,
        success: o.success,
    }
}
```

Also remove `use crate::host::executor;` and `use crate::host::pool::SshPool;` socket-related usage.

- [ ] **Step 3: Remove executor imports from sync.rs**

After completing the replacements, remove:
```rust
use crate::host::executor;
```
if it's no longer referenced in sync.rs.

- [ ] **Step 4: Build**

```bash
cargo check
```
Expected: compiles. Fix any type mismatches.

- [ ] **Step 5: Run tests**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/commands/sync.rs
git commit -m "feat(sync): replace scp with SFTP for file transfers in sync command"
```

---

### Task 16: Remove conn_mgr from SshPool

**Files:**
- Modify: `src/host/pool.rs`
- Modify: `src/commands/sync.rs` (remove `pool.conn_mgr` usage — should be clean after Task 15)

Now that sync.rs uses `pool.session_pool` exclusively, we can remove `conn_mgr: ConnectionManager` from `SshPool`.

- [ ] **Step 1: Verify conn_mgr is no longer accessed externally**

```bash
grep -rn "pool\.conn_mgr\|\.conn_mgr" src/
```
Expected: no output (all usages were removed in Task 15).

- [ ] **Step 2: Remove conn_mgr from SshPool struct**

In `src/host/pool.rs`:
1. Remove `conn_mgr: ConnectionManager` field from the struct
2. Remove `use super::connection::ConnectionManager;` import
3. Update `setup_with_options` to NOT create a `ConnectionManager`; instead use `session_pool.failed_hosts()` and `session_pool.reachable_hosts()` as the source of truth
4. Update `failed_hosts()`, `reachable_hosts()`, `filter_reachable()` to delegate to `session_pool`
5. Remove `socket_for()` (returns None now — or keep as stub returning None for Phase 4 cleanup)
6. Remove `scp_probe` path from `setup_with_options`; replace with `run_sftp_probe`
7. Update `shutdown()` to not call `conn_mgr.shutdown()`

New `setup_with_options`:

```rust
pub async fn setup_with_options(
    hosts: &[&HostEntry],
    timeout: u64,
    global_concurrency: usize,
    per_host_concurrency: usize,
    probe_sftp: bool,
) -> Result<(Self, usize)> {
    let host_names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
    let limiter = ConcurrencyLimiter::new(global_concurrency, per_host_concurrency, &host_names);
    let mut progress = SyncProgress::new();

    progress.start_host_check(hosts.len());
    let mut session_pool = RusshSessionPool::setup(hosts, timeout, global_concurrency).await?;
    let connected = session_pool.reachable_hosts().len();

    if probe_sftp && connected > 0 {
        session_pool.run_sftp_probe(hosts, timeout).await;
        let sftp_failed = session_pool.sftp_failed_hosts().len();
        progress.finish_host_check(connected - sftp_failed, hosts.len() - (connected - sftp_failed));
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
```

Updated delegation:
```rust
pub fn failed_hosts(&self) -> Vec<(String, String)> {
    self.session_pool.failed_hosts()
}

pub fn filter_reachable<'a>(&self, hosts: &[&'a HostEntry]) -> Vec<&'a HostEntry> {
    let reachable: std::collections::HashSet<String> =
        self.session_pool.reachable_hosts().into_iter().collect();
    hosts.iter().filter(|h| reachable.contains(&h.name)).copied().collect()
}
```

- [ ] **Step 3: Build**

```bash
cargo check
```
Expected: compiles with no errors.

- [ ] **Step 4: Run tests**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/host/pool.rs src/commands/sync.rs
git commit -m "refactor(pool): remove ConnectionManager from SshPool; SFTP is now sole transport"
```

---

## Phase 4 — Cleanup

### Task 17: Remove legacy process-spawn SSH modules

**Files:**
- Delete: `src/host/connection.rs`
- Delete: `src/host/executor.rs`
- Delete: `src/host/process_transport.rs`
- Delete: `src/host/transport.rs`
- Modify: `src/host/mod.rs`
- Modify: `src/commands/init.rs` (still uses ConnectionManager — update to use RusshSessionPool)
- Modify: `src/host/shell.rs` (detect_pooled uses executor — update to use session_pool)

> **Important:** After removing modules, fix ALL compiler errors caused by remaining usages.

- [ ] **Step 1: Find all remaining usages of deleted modules**

```bash
grep -rn "use crate::host::executor\|use crate::host::connection\|use crate::host::process_transport\|use crate::host::transport" src/
```
Expected: references remain in `src/commands/init.rs` and `src/host/shell.rs`.

- [ ] **Step 2: Update init.rs to use RusshSessionPool for pre-check**

In `src/commands/init.rs`:
1. Remove `use crate::host::connection::ConnectionManager;`
2. Add `use crate::host::session_pool::RusshSessionPool;`
3. Replace `ConnectionManager::new()` + `pre_check()` with `RusshSessionPool::setup()`
4. Replace `conn_mgr.reachable_hosts()` with `session_pool.reachable_hosts()`
5. Replace `conn_mgr.failed_hosts()` with `session_pool.failed_hosts()`
6. Replace socket-based `shell::detect_pooled()` calls: since there's no socket anymore, call `session_pool.exec(&host, cmd, timeout)` directly for shell detection, or call `shell::detect_russh(&host, &session_pool, timeout)` (a new function added to shell.rs)
7. Remove the keyscan/host-key-failure flow for now (russh handles known_hosts checking at connect time) — if a host fails to connect due to unknown key, it appears in `session_pool.failed_hosts()` with an informative message.

New skeleton for `run()` in init.rs:
```rust
// Replace ConnectionManager block with:
let session_pool = RusshSessionPool::setup(&entry_refs, ctx.timeout, ctx.concurrency()).await?;
let connected = session_pool.reachable_hosts().len();
let failed_count = entry_refs.len() - connected;
progress.finish_host_check(connected, failed_count);

for (name, err) in session_pool.failed_hosts() {
    printer::print_host_line(&name, "error", &err);
    summary.add_failure(&name, &err);
}
```

For shell detection, add to `src/host/shell.rs`:
```rust
/// Detect shell type using an established russh session.
pub async fn detect_russh(
    host: &crate::config::schema::HostEntry,
    sessions: &super::session_pool::RusshSessionPool,
    timeout: u64,
) -> Result<crate::config::schema::ShellType> {
    // Try PowerShell: if it responds, it's PowerShell
    let ps_result = sessions.exec(&host.ssh_host, "$PSVersionTable.PSVersion.Major", timeout).await;
    if let Ok(o) = ps_result {
        if o.success && !o.stdout.trim().is_empty() {
            return Ok(crate::config::schema::ShellType::PowerShell);
        }
    }
    // Try CMD: if 'ver' works but 'echo $SHELL' doesn't, it's CMD
    let cmd_result = sessions.exec(&host.ssh_host, "ver", timeout).await;
    if let Ok(o) = cmd_result {
        if o.success && o.stdout.contains("Windows") {
            // Distinguish CMD vs PowerShell by checking if '&' operator works
            let cmd2 = sessions.exec(&host.ssh_host, "echo ok 2>nul", timeout).await;
            if cmd2.map(|o| o.success).unwrap_or(false) {
                return Ok(crate::config::schema::ShellType::Cmd);
            }
        }
    }
    // Default: sh
    Ok(crate::config::schema::ShellType::Sh)
}
```

- [ ] **Step 3: Remove modules from mod.rs**

In `src/host/mod.rs`:
```rust
pub mod auth;
pub mod concurrency;
// connection — DELETED
// executor — DELETED
pub mod filter;
pub mod mod;
// process_transport — DELETED
pub mod pool;
pub mod session_pool;
pub mod sftp;
pub mod shell;
// transport — DELETED
```

- [ ] **Step 4: Delete the four files**

```bash
rm src/host/connection.rs
rm src/host/executor.rs
rm src/host/process_transport.rs
rm src/host/transport.rs
```

- [ ] **Step 5: Build and fix all remaining errors**

```bash
cargo check 2>&1 | head -60
```
Fix each compile error in turn. Common issues:
- `executor::RemoteOutput` references in `run.rs`/`check.rs` — replace with `session_pool::RemoteOutput`
- `async_trait` dep may become unused — remove from Cargo.toml if so
- Any `#[allow(dead_code)]` on now-removed items

- [ ] **Step 6: Run full test suite**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(cleanup): remove connection, executor, process_transport, transport modules"
```

---

### Task 18: Final cleanup and validation

**Files:**
- Modify: `Cargo.toml` (remove unused deps)
- Verify: `RemoteOutput` unified across codebase

- [ ] **Step 1: Remove unused Cargo deps**

Check for crates that were only used by the deleted modules:

```bash
cargo check 2>&1 | grep "unused"
```

Likely candidates to remove from `Cargo.toml`:
- `async-trait` (if no longer used after removing `transport.rs`)
- `tempfile` (if only used by `executor::scp_probe`)

Remove them and run:
```bash
cargo check
```

- [ ] **Step 2: Unify RemoteOutput**

After removing `executor.rs`, `session_pool::RemoteOutput` is the only `RemoteOutput`. Find and remove any temporary mapping shims added during Phase 2/3:

```bash
grep -rn "convert_remote_output\|executor::RemoteOutput" src/
```
Each match should be replaced with direct `session_pool::RemoteOutput` usage.

- [ ] **Step 3: Run full test suite and lints**

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
```
Expected: zero errors, zero warnings (address any clippy warnings).

- [ ] **Step 4: Build release binary**

```bash
cargo build --release
cargo build --release --no-default-features
```
Expected: both build successfully.

- [ ] **Step 5: Final commit**

```bash
git add -A
git commit -m "chore: final cleanup after russh migration — unify RemoteOutput, remove unused deps

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Self-Review Checklist

| Spec Requirement | Covered By |
|-----------------|-----------|
| Multi-alias `Host a b c` parsing | Task 2 (parse_ssh_config_content rewrite) |
| Windows connection multiplexing (no ControlMaster) | Task 8/10 (RusshSessionPool replaces ConnectionManager) |
| ProxyJump support (direct-tcpip) | Task 8 (connect_via_proxy) |
| SFTP for sync command | Tasks 12–15 |
| VirtualLock suppression | Task 5 (tracing EnvFilter) |
| Auth: IdentityFile → passphrase → password | Task 7 (host/auth.rs) |
| Passphrase cache across hosts | Task 7 (PassphraseCache HashMap) |
| Known-hosts check | Task 8 (SshHandler::check_server_key) |
| Phase 4 legacy code removal | Tasks 17–18 |

## Notes

- **russh API drift:** The exact method names for `SftpSession` operations may differ in `russh-sftp 2.1` from what is shown in this plan. Always consult `docs.rs/russh-sftp/2.1` when writing the SFTP code in Task 13.
- **Windows testing:** VirtualLock suppression (Task 5) can only be verified on Windows. On macOS/Linux the tracing filter is harmless.
- **ProxyJump multi-hop:** The design supports single-hop only. Multi-hop (comma-separated ProxyJump) is noted in `connect_one` — `proxy_jump` takes only the first hop. Full multi-hop recursive support can be added as a follow-up.
- **init.rs keyscan flow:** In Phase 4, the keyscan-and-retry flow is simplified because russh reports unknown host keys as connection failures with a clear error message. If users need to batch-accept keys, the `ssh-keyscan` call can be kept as a standalone utility.
