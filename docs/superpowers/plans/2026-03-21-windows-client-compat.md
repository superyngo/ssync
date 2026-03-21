# Windows Client Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ssync fully functional as a Windows client by introducing dual-mode ConnectionManager (Pooled/Direct), fixing Unix-hardcoded paths, and splitting Cmd remote shell branches from Sh.

**Architecture:** ConnectionManager gets a `ConnectionMode` enum (`Pooled` with socket dir for Unix, `Direct` with no connection pooling for Windows). Platform detection uses runtime `cfg!(target_os = "windows")`. All `_pooled` executor functions already accept `Option<&Path>` socket, so Direct mode (returning `None`) requires zero downstream API changes.

**Tech Stack:** Rust, tokio, tempfile, crossterm (optional TUI feature)

**Spec:** `docs/superpowers/specs/2026-03-21-windows-client-compat-design.md`

---

### Task 1: ConnectionManager Dual-Mode

**Files:**
- Modify: `src/host/connection.rs:11-44` (struct + constructor)
- Modify: `src/host/connection.rs:48-99` (pre_check)
- Modify: `src/host/connection.rs:205-256` (shutdown + Drop)
- Modify: `src/host/connection.rs:229-235` (socket_path_for)
- Modify: `src/host/connection.rs:292-332` (tests)

- [ ] **Step 1: Add ConnectionMode enum and refactor struct**

In `src/host/connection.rs`, add above `ConnectionManager`:

```rust
/// Connection pooling strategy.
enum ConnectionMode {
    /// Unix: ControlMaster connection pooling with socket directory.
    Pooled { socket_dir: tempfile::TempDir },
    /// Windows/fallback: no persistent connections, each SSH call is independent.
    Direct,
}
```

/// State of an SSH connection to a host.
#[derive(Debug, Clone)]
pub enum ConnectionState {
    Connected { socket_path: PathBuf },
    /// Host reachable but no persistent socket (Direct mode on Windows).
    DirectConnected,
    Failed { error: String },
}
```

Change the `ConnectionManager` struct (line 21-28):

```rust
pub struct ConnectionManager {
    mode: ConnectionMode,
    hosts: HashMap<String, ConnectionState>,
    host_map: HashMap<String, String>,
    scp_failed: HashMap<String, String>,
}
```

- [ ] **Step 2: Refactor constructor for dual-mode**

Replace `ConnectionManager::new()` (lines 30-44):

```rust
impl ConnectionManager {
    /// Create a new ConnectionManager.
    /// On Unix: creates a temporary socket directory for ControlMaster.
    /// On Windows: uses Direct mode (no connection pooling).
    pub fn new() -> Result<Self> {
        let mode = if cfg!(target_os = "windows") {
            ConnectionMode::Direct
        } else {
            let socket_dir = tempfile::Builder::new()
                .prefix("ssync-")
                .tempdir_in("/tmp")
                .context("Failed to create socket directory")?;
            ConnectionMode::Pooled { socket_dir }
        };
        Ok(Self {
            mode,
            hosts: HashMap::new(),
            host_map: HashMap::new(),
            scp_failed: HashMap::new(),
        })
    }
```

- [ ] **Step 3: Add Direct-mode connectivity check**

Add a new function below `establish_master`:

```rust
/// Lightweight connectivity check for Direct mode (no ControlMaster).
/// Runs `ssh host exit 0` to verify the host is reachable.
async fn check_connectivity(host: &HostEntry, timeout_secs: u64) -> Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(&host.ssh_host)
            .arg("exit")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SSH connectivity check timeout")?
    .context("Failed to check SSH connectivity")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("SSH connection failed: {}", stderr.trim());
    }
    Ok(())
}
```

- [ ] **Step 4: Refactor pre_check to support both modes**

Modify `pre_check()` (lines 48-99). The key change is inside the spawn: use `establish_master` for Pooled mode and `check_connectivity` for Direct mode. Also `socket_path_for` is only called in Pooled mode.

Replace the spawned task logic:

```rust
// In pre_check, before the for-loop, determine if pooled:
let is_pooled = matches!(self.mode, ConnectionMode::Pooled { .. });

for host in hosts {
    let sem = semaphore.clone();
    let host = (*host).clone();
    let socket_path = if is_pooled {
        Some(self.socket_path_for(&host.name))
    } else {
        None
    };

    handles.push(tokio::spawn(async move {
        let _permit = sem.acquire().await.unwrap();
        let result = if let Some(ref sp) = socket_path {
            establish_master(&host, sp, timeout_secs).await
        } else {
            check_connectivity(&host, timeout_secs).await
        };
        (host.name.clone(), host.ssh_host.clone(), socket_path, result)
    }));
}
```

And in the result-gathering loop, handle `socket_path: Option<PathBuf>`:

```rust
Ok((name, ssh_host, socket_path, Ok(()))) => {
    if let Some(sp) = socket_path {
        self.hosts.insert(name.clone(), ConnectionState::Connected { socket_path: sp });
    } else {
        self.hosts.insert(name.clone(), ConnectionState::DirectConnected);
    }
    self.host_map.insert(name, ssh_host);
    connected += 1;
}
```

- [ ] **Step 5: Update socket_for to respect Direct mode**

Modify `socket_for()` (line 102-107):

```rust
pub fn socket_for(&self, host_name: &str) -> Option<&Path> {
    if matches!(self.mode, ConnectionMode::Direct) {
        return None;
    }
    match self.hosts.get(host_name) {
        Some(ConnectionState::Connected { socket_path }) => Some(socket_path),
        _ => None,
    }
}
```

    pub fn reachable_hosts(&self) -> Vec<String> {
        self.hosts
            .iter()
            .filter_map(|(name, state)| match state {
                ConnectionState::Connected { .. } | ConnectionState::DirectConnected => Some(name.clone()),
                _ => None,
            })
            .collect()
    }
```

- [ ] **Step 6: Update socket_path_for to require Pooled mode**

Modify `socket_path_for()` (lines 229-235):

```rust
fn socket_path_for(&self, host_name: &str) -> PathBuf {
    match &self.mode {
        ConnectionMode::Pooled { socket_dir } => {
            let hash = blake3::hash(host_name.as_bytes());
            let short_hash = &hash.to_hex()[..12];
            socket_dir.path().join(short_hash)
        }
        ConnectionMode::Direct => {
            unreachable!("socket_path_for called in Direct mode")
        }
    }
}
```

- [ ] **Step 7: Update shutdown and Drop for dual-mode**

Modify `shutdown()` (lines 205-227) — skip ControlMaster teardown in Direct mode:

```rust
pub async fn shutdown(&mut self) {
    if matches!(self.mode, ConnectionMode::Direct) {
        self.hosts.clear();
        return;
    }
    // existing ControlMaster shutdown logic...
}
```

Modify `Drop` (lines 238-256) — skip in Direct mode:

```rust
impl Drop for ConnectionManager {
    fn drop(&mut self) {
        if matches!(self.mode, ConnectionMode::Direct) {
            return;
        }
        // existing blocking shutdown logic...
    }
}
```

- [ ] **Step 8: Update tests**

The existing tests call `ConnectionManager::new().unwrap()` which will now work on Windows (Direct mode). Update `test_socket_path_short_enough` and friends to only run on Unix:

```rust
#[test]
#[cfg(not(target_os = "windows"))]
fn test_socket_path_short_enough() { ... }

#[test]
#[cfg(not(target_os = "windows"))]
fn test_socket_paths_unique() { ... }

#[test]
#[cfg(not(target_os = "windows"))]
fn test_socket_paths_deterministic() { ... }

#[test]
fn test_reachable_hosts_empty_initially() {
    let mgr = ConnectionManager::new().unwrap();
    assert!(mgr.reachable_hosts().is_empty());
    assert!(mgr.failed_hosts().is_empty());
}
```

- [ ] **Step 9: Verify build**

Run: `cargo check`
Expected: no errors

- [ ] **Step 10: Commit**

```
git add src/host/connection.rs
git commit -m "feat: dual-mode ConnectionManager (Pooled/Direct) for Windows support"
```

---

### Task 2: SCP Probe Shell-Aware Paths

**Files:**
- Modify: `src/host/executor.rs:190-224` (scp_probe function)

- [ ] **Step 1: Replace scp_probe with shell-aware version**

Replace the `scp_probe` function (lines 190-224):

```rust
/// Probe whether scp works to a remote host by uploading a 1-byte temp file.
/// Uses shell-appropriate remote paths and cleanup commands.
pub async fn scp_probe(host: &HostEntry, timeout_secs: u64, socket: Option<&Path>) -> Result<()> {
    use crate::config::schema::ShellType;

    let temp_dir = tempfile::tempdir().context("Failed to create temp dir for scp probe")?;
    let local_probe = temp_dir.path().join("ssync_probe");
    std::fs::write(&local_probe, b"0").context("Failed to write probe file")?;

    let probe_paths: Vec<&str> = match host.shell {
        ShellType::Sh => vec!["/tmp/.ssync_probe", "~/.ssync_probe"],
        ShellType::PowerShell => vec!["$env:TEMP\\.ssync_probe", "~/.ssync_probe"],
        ShellType::Cmd => vec!["%TEMP%\\.ssync_probe"],
    };

    let mut last_err = None;
    for remote_path in &probe_paths {
        match upload_pooled(host, &local_probe, remote_path, timeout_secs, socket).await {
            Ok(()) => {
                // Shell-aware cleanup (best-effort)
                let rm_cmd = match host.shell {
                    ShellType::Sh => {
                        if remote_path.starts_with("~/") {
                            format!(
                                "rm -f \"$HOME/{}\" 2>/dev/null; exit 0",
                                &remote_path[2..]
                            )
                        } else {
                            format!("rm -f '{}' 2>/dev/null; exit 0", remote_path)
                        }
                    }
                    ShellType::PowerShell => {
                        format!(
                            "Remove-Item -Force '{}' -ErrorAction SilentlyContinue",
                            remote_path
                        )
                    }
                    ShellType::Cmd => {
                        format!("del /f /q \"{}\" 2>nul", remote_path)
                    }
                };
                let _ = run_remote_pooled(host, &rm_cmd, timeout_secs, socket).await;
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("scp probe failed")))
}
```

- [ ] **Step 2: Verify build**

Run: `cargo check`
Expected: no errors

- [ ] **Step 3: Commit**

```
git add src/host/executor.rs
git commit -m "fix: shell-aware SCP probe paths for PowerShell/Cmd remotes"
```

---

### Task 3: Fix to_tilde_path HOME Variable

**Files:**
- Modify: `src/commands/sync.rs:1236-1251` (to_tilde_path function)

- [ ] **Step 1: Replace to_tilde_path**

Replace the function (lines 1236-1251):

```rust
/// This handles the case where the shell expands `~/foo` → `/home/user/foo` before
/// ssync receives it — remotes need the tilde form so it resolves to *their* home dir.
fn to_tilde_path(path: &str) -> String {
    // Check both HOME (Unix) and USERPROFILE (Windows)
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    if let Some(ref home) = home {
        if !home.is_empty() {
            if path == home.as_str() {
                return "~".to_string();
            }
            // Check both forward-slash and backslash separators
            for sep in &["/", "\\"] {
                let prefix = format!("{}{}", home, sep);
                if let Some(rest) = path.strip_prefix(&prefix) {
                    return format!("~/{}", rest);
                }
            }
        }
    }
    path.to_string()
}
```

- [ ] **Step 2: Add unit test for to_tilde_path**

Add to the test module in sync.rs (or create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_tilde_path_unix_style() {
        std::env::set_var("HOME", "/home/user");
        assert_eq!(to_tilde_path("/home/user"), "~");
        assert_eq!(to_tilde_path("/home/user/docs/file.txt"), "~/docs/file.txt");
        assert_eq!(to_tilde_path("/other/path"), "/other/path");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn test_to_tilde_path_windows_style() {
        std::env::set_var("USERPROFILE", "C:\\Users\\test");
        std::env::remove_var("HOME");
        assert_eq!(to_tilde_path("C:\\Users\\test"), "~");
        assert_eq!(to_tilde_path("C:\\Users\\test\\docs\\file.txt"), "~/docs\\file.txt");
        assert_eq!(to_tilde_path("D:\\other"), "D:\\other");
    }
}
```

- [ ] **Step 3: Verify build and test**

Run: `cargo test to_tilde`
Expected: PASS

- [ ] **Step 4: Commit**

```
git add src/commands/sync.rs
git commit -m "fix: to_tilde_path supports USERPROFILE and backslash paths"
```

---

### Task 4: Split Cmd from Sh in collect_file_metadata

**Files:**
- Modify: `src/commands/sync.rs:813-828` (metadata cmd in collect_file_metadata)

- [ ] **Step 1: Split the ShellType::Sh | ShellType::Cmd branch**

Replace lines 813-828 with separate branches:

```rust
                ShellType::Sh => {
                    let escaped = if let Some(stripped) = path.strip_prefix("~/") {
                        format!("$HOME/'{}'", stripped.replace('\'', "'\\''"))
                    } else {
                        format!("'{}'", path.replace('\'', "'\\''"))
                    };
                    format!(
                        "stat -c '%Y %s' {p} 2>/dev/null || stat -f '%m %z' {p} 2>/dev/null; \
                         (sha256sum {p} 2>/dev/null || shasum -a 256 {p} 2>/dev/null) || true",
                        p = escaped
                    )
                }
                ShellType::Cmd => {
                    // Windows Cmd: use 'forfiles' for timestamp and 'certutil' for hash.
                    // Output line 1: epoch_seconds size
                    // Output line 2: hex_hash
                    // PowerShell is available on Cmd hosts; use it inline for epoch conversion.
                    let escaped = path.replace('"', "\"\"");
                    format!(
                        "powershell -NoProfile -Command \"\
                         $i=Get-Item '{p}' -ErrorAction SilentlyContinue; \
                         if ($i) {{ \
                           [int64](($i.LastWriteTimeUtc-[datetime]'1970-01-01').TotalSeconds), $i.Length -join ' '; \
                           (Get-FileHash '{p}' -Algorithm SHA256).Hash.ToLower() \
                         }}\"",
                        p = escaped
                    )
                }
```

Note: For Cmd hosts, we invoke `powershell -NoProfile` inline since pure Cmd lacks epoch-time and hashing. This is a pragmatic approach — PowerShell is available on all modern Windows.

- [ ] **Step 2: Verify build**

Run: `cargo check`
Expected: no errors

- [ ] **Step 3: Commit**

```
git add src/commands/sync.rs
git commit -m "fix: separate Cmd metadata collection from Sh (use PowerShell inline)"
```

---

### Task 5: Split Cmd from Sh in build_batch_metadata_cmd

**Files:**
- Modify: `src/commands/sync.rs:1455-1475` (batch metadata)

- [ ] **Step 1: Split the ShellType::Sh | ShellType::Cmd branch**

Replace lines 1455-1475:

```rust
        ShellType::Sh => {
            let expanded: Vec<String> = paths
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
                "for f in {files}; do \
                 echo \"---FILE:$f\"; \
                 stat -c '%Y %s' \"$f\" 2>/dev/null || stat -f '%m %z' \"$f\" 2>/dev/null || echo \"MISSING\"; \
                 (sha256sum \"$f\" 2>/dev/null || shasum -a 256 \"$f\" 2>/dev/null) || echo \"NOHASH\"; \
                 done",
                files = expanded.join(" ")
            )
        }
        ShellType::Cmd => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    let p = p.replace('/', "\\");
                    format!("\"{}\"", p)
                })
                .collect();
            format!(
                "powershell -NoProfile -Command \"\
                 foreach ($f in @({files})) {{ \
                   '---FILE:' + $f; \
                   $i=Get-Item $f -ErrorAction SilentlyContinue; \
                   if ($i) {{ \
                     [int64](($i.LastWriteTimeUtc-[datetime]'1970-01-01').TotalSeconds), $i.Length -join ' '; \
                     (Get-FileHash $f -Algorithm SHA256).Hash.ToLower() \
                   }} else {{ 'MISSING' }} \
                 }}\"",
                files = expanded.join(",")
            )
        }
```

- [ ] **Step 2: Verify build**

Run: `cargo check`
Expected: no errors

- [ ] **Step 3: Commit**

```
git add src/commands/sync.rs
git commit -m "fix: separate Cmd batch metadata from Sh"
```

---

### Task 6: Split Cmd from Sh in build_dir_expand_cmd

**Files:**
- Modify: `src/commands/sync.rs:1308-1337` (dir expand cmd)

- [ ] **Step 1: Split the ShellType::Sh | ShellType::Cmd branch**

Replace lines 1308-1337:

```rust
        ShellType::Sh => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("$HOME/'{}'", stripped.replace('\'', "'\\''"))
                    } else {
                        format!("'{}'", p.replace('\'', "'\\''"))
                    }
                })
                .collect();
            let depth_flag = if recursive { "" } else { " -maxdepth 1" };
            format!(
                "for p in {files}; do \
                   orig=$(echo \"$p\" | sed \"s|^$HOME/|~/|;s|^$HOME$|~|\"); \
                   echo \"---PATH:$orig\"; \
                   if [ -d \"$p\" ]; then \
                     echo \"DIR\"; \
                     find \"$p\"{depth} -type f 2>/dev/null | sed \"s|^$HOME/|~/|\" | sort; \
                   elif [ -e \"$p\" ]; then \
                     echo \"FILE\"; \
                   else \
                     echo \"MISSING\"; \
                   fi; \
                 done",
                files = expanded.join(" "),
                depth = depth_flag
            )
        }
        ShellType::Cmd => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    let p = p.replace('/', "\\");
                    format!("\"{}\"", p)
                })
                .collect();
            let recurse_flag = if recursive { " -Recurse" } else { "" };
            // Use PowerShell inline for directory expansion on Cmd hosts
            format!(
                "powershell -NoProfile -Command \"\
                 foreach ($p in @({files})) {{ \
                   '---PATH:' + $p; \
                   if (Test-Path $p -PathType Container) {{ \
                     'DIR'; \
                     Get-ChildItem $p -File{recurse} | ForEach-Object {{ $_.FullName }} \
                   }} elseif (Test-Path $p) {{ 'FILE' }} \
                   else {{ 'MISSING' }} \
                 }}\"",
                files = expanded.join(","),
                recurse = recurse_flag
            )
        }
```

- [ ] **Step 2: Verify build**

Run: `cargo check`
Expected: no errors

- [ ] **Step 3: Commit**

```
git add src/commands/sync.rs
git commit -m "fix: separate Cmd directory expansion from Sh"
```

---

### Task 7: Shell-Aware mkdir in distribute/distribute_pooled

**Files:**
- Modify: `src/commands/sync.rs:1095-1114` (distribute mkdir section)
- Modify: `src/commands/sync.rs:1186-1208` (distribute_pooled mkdir section)

- [ ] **Step 1: Add helper function for shell-aware mkdir**

Add above `distribute()`:

```rust
/// Build a shell-appropriate mkdir command for creating parent directories.
fn build_mkdir_cmd(shell: ShellType, parent: &str) -> Option<String> {
    if parent.is_empty() {
        return None;
    }

    match shell {
        ShellType::Sh => {
            if parent.starts_with("~/") || parent == "~" {
                let sub = if parent == "~" { "" } else { &parent[2..] };
                if sub.is_empty() {
                    None
                } else {
                    Some(format!("mkdir -p \"$HOME/{}\"", sub.replace('"', "\\\"")))
                }
            } else if parent != "/" {
                Some(format!("mkdir -p '{}'", parent.replace('\'', "'\\''")))
            } else {
                None
            }
        }
        ShellType::PowerShell => {
            if parent != "/" && parent != "\\" {
                Some(format!(
                    "New-Item -ItemType Directory -Force -Path '{}' | Out-Null",
                    parent
                ))
            } else {
                None
            }
        }
        ShellType::Cmd => {
            let win_parent = parent.replace('/', "\\");
            Some(format!(
                "if not exist \"{}\" mkdir \"{}\"",
                win_parent, win_parent
            ))
        }
    }
}
```

- [ ] **Step 2: Replace distribute mkdir section**

Replace lines 1095-1114 in `distribute()`:

```rust
            // Ensure parent directory exists on target
            let parent = std::path::Path::new(&remote_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Some(mkdir_cmd) = build_mkdir_cmd(target.shell, &parent) {
                let _ = executor::run_remote(&target, &mkdir_cmd, timeout).await;
            }
```

- [ ] **Step 3: Replace distribute_pooled mkdir section**

Replace lines 1186-1208 in `distribute_pooled()`:

```rust
            // Ensure parent directory exists on target
            let parent = std::path::Path::new(&remote_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Some(mkdir_cmd) = build_mkdir_cmd(target.shell, &parent) {
                let _ =
                    executor::run_remote_pooled(&target, &mkdir_cmd, timeout, socket.as_deref())
                        .await;
            }
```

- [ ] **Step 4: Verify build**

Run: `cargo check`
Expected: no errors

- [ ] **Step 5: Commit**

```
git add src/commands/sync.rs
git commit -m "fix: shell-aware mkdir for distribute (Sh/PowerShell/Cmd)"
```

---

### Task 8: Enable ANSI Support on Windows

**Files:**
- Modify: `src/main.rs:14` (add ANSI setup before CLI output)

- [ ] **Step 1: Add enable_ansi_support function**

Add before `fn main()` in `src/main.rs`:

```rust
/// Enable ANSI escape code support on Windows terminals.
/// Modern Windows 10+ supports ANSI via Virtual Terminal Processing,
/// but it must be explicitly enabled.
#[cfg(target_os = "windows")]
fn enable_ansi_support() {
    #[cfg(feature = "tui")]
    {
        // crossterm (already a dependency via TUI feature) handles this
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnableAnsiSupport
        );
    }
    #[cfg(not(feature = "tui"))]
    {
        // Without TUI/crossterm, use raw Win32 API via FFI.
        use std::os::windows::io::AsRawHandle;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
        extern "system" {
            fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
        }
        unsafe {
            let handle = std::io::stdout().as_raw_handle() as *mut std::ffi::c_void;
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn enable_ansi_support() {}
```

No new crate dependency needed. TUI feature uses crossterm; non-TUI uses inline FFI.

- [ ] **Step 2: Call enable_ansi_support in main**

Add as first line in `async fn main()`:

```rust
enable_ansi_support();
```

- [ ] **Step 3: Verify build on both feature flags**

Run: `cargo check` and `cargo check --no-default-features`
Expected: no errors on both

- [ ] **Step 4: Commit**

```
git add src/main.rs
git commit -m "feat: enable ANSI terminal support on Windows"
```

---

### Task 9: Final Verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Check formatting**

Run: `cargo fmt --check`
Expected: no formatting issues

- [ ] **Step 4: Build with default features**

Run: `cargo build`
Expected: build succeeds

- [ ] **Step 5: Build without TUI**

Run: `cargo build --no-default-features`
Expected: build succeeds

- [ ] **Step 6: Final commit (if any fixups needed)**

```
git add -A
git commit -m "chore: final cleanup for Windows client compatibility"
```
