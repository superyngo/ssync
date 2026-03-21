# Windows Client Compatibility — Design Specification

**Date**: 2026-03-21
**Status**: Draft
**Scope**: Windows as ssync client (primary), Cmd remote shell fixes (secondary)

---

## Problem Statement

ssync currently assumes a Unix client environment. Running on Windows fails immediately:

```
PS> ssync init
Error: Failed to create socket directory
Caused by: 系統找不到指定的路徑。 (os error 3)
at path "F:/tmp\\ssync-GfIODq"
```

Root causes:
1. `ConnectionManager::new()` hardcodes `tempdir_in("/tmp")`
2. ControlMaster/ControlPath/ControlPersist are Unix-only SSH features
3. Various Unix assumptions scattered throughout the codebase

## Approach

- **ConnectionManager dual-mode**: `Pooled` (Unix, ControlMaster) vs `Direct` (Windows, per-call SSH)
- **Platform detection**: Runtime `cfg!(target_os = "windows")` checks, not separate platform modules
- **Graceful fallback**: Windows clients work without ControlMaster; slightly slower but fully functional
- **Cmd remote fixes**: Split `ShellType::Sh | ShellType::Cmd` branches where Cmd was incorrectly grouped with Sh

---

## Design

### 1. ConnectionManager Dual-Mode Architecture

**File**: `src/host/connection.rs`

#### New Types

```rust
enum ConnectionMode {
    /// Unix: ControlMaster connection pooling with socket directory
    Pooled { socket_dir: tempfile::TempDir },
    /// Windows/fallback: no connection pooling, each SSH call is independent
    Direct,
}
```

#### Modified `ConnectionManager`

```rust
pub struct ConnectionManager {
    mode: ConnectionMode,              // was: socket_dir: tempfile::TempDir
    hosts: HashMap<String, ConnectionState>,
    host_map: HashMap<String, String>,
    scp_failed: HashMap<String, String>,
}
```

#### Constructor

```rust
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

#### Behavioral Differences

| Method | Pooled (Unix) | Direct (Windows) |
|--------|--------------|-----------------|
| `pre_check()` | Establish ControlMaster, record socket path | Lightweight connectivity test (`ssh host exit 0`) |
| `socket_for()` | Returns `Some(&Path)` for connected hosts | Always returns `None` |
| `socket_path_for()` | Computes blake3-based socket path | Not called |
| `establish_master()` | Full ControlMaster setup | Not called |
| `shutdown()` | `ssh -O exit` for each master | No-op |
| `Drop` | Blocking `ssh -O exit` fallback | No-op |

#### Direct Mode Connectivity Test

```rust
async fn check_connectivity(host: &HostEntry, timeout_secs: u64) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(&host.ssh_host)
            .arg("exit").arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    ).await
    .context("SSH connectivity check timeout")?
    .context("Failed to check SSH connectivity")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("SSH connection failed: {}", stderr.trim());
    }
    Ok(())
}
```

#### Downstream Impact

Minimal. All `_pooled` functions in `executor.rs` already accept `socket: Option<&Path>`:
- `run_remote_pooled()` — `None` → no ControlPath arg → direct SSH
- `upload_pooled()` — same
- `download_pooled()` — same

The `ssh_base_args()` function already handles `None`:
```rust
if let Some(sock) = socket {
    args.push("-o".to_string());
    args.push(format!("ControlPath={}", sock.display()));
}
// When socket is None, no ControlPath added — works as direct connection
```

---

### 2. SCP Probe Shell-Aware Paths

**File**: `src/host/executor.rs` — `scp_probe()`

#### Current Problem

Probe paths (`/tmp/.ssync_probe`, `~/.ssync_probe`) and cleanup command (`rm -f`) are Sh-only — both fail on Cmd/PowerShell remotes.

#### Fix

```rust
pub async fn scp_probe(host: &HostEntry, timeout_secs: u64, socket: Option<&Path>) -> Result<()> {
    let temp_dir = tempfile::tempdir().context("Failed to create temp dir for scp probe")?;
    let local_probe = temp_dir.path().join("ssync_probe");
    std::fs::write(&local_probe, b"0")?;

    // Shell-aware remote probe paths
    let probe_paths: Vec<&str> = match host.shell {
        ShellType::Sh => vec!["/tmp/.ssync_probe", "~/.ssync_probe"],
        ShellType::PowerShell => vec!["$env:TEMP\\.ssync_probe"],
        ShellType::Cmd => vec!["%TEMP%\\.ssync_probe"],
    };

    for remote_path in &probe_paths {
        match upload_pooled(host, &local_probe, remote_path, timeout_secs, socket).await {
            Ok(()) => {
                // Shell-aware cleanup
                let rm_cmd = match host.shell {
                    ShellType::Sh => {
                        if remote_path.starts_with("~/") {
                            format!("rm -f \"$HOME/{}\" 2>/dev/null; exit 0",
                                &remote_path[2..])
                        } else {
                            format!("rm -f '{}' 2>/dev/null; exit 0", remote_path)
                        }
                    }
                    ShellType::PowerShell => {
                        format!("Remove-Item -Force '{}' -ErrorAction SilentlyContinue",
                            remote_path)
                    }
                    ShellType::Cmd => {
                        format!("del /f /q \"{}\" 2>nul", remote_path)
                    }
                };
                let _ = run_remote_pooled(host, &rm_cmd, timeout_secs, socket).await;
                return Ok(());
            }
            Err(_) => continue,
        }
    }

    anyhow::bail!("SCP probe failed for all paths on {}", host.name);
}
```

> **Implementation note**: SCP with env-var paths (`%TEMP%`, `$env:TEMP`) relies on the remote shell expanding the variable. Verify early with a manual `scp test.txt host:%TEMP%\test.txt` on a Cmd remote.

---

### 3. Sync Command Cmd Remote Fixes

**File**: `src/commands/sync.rs`

#### 3a. File Metadata Collection (`collect_file_metadata` / `build_metadata_cmd`)

Split `ShellType::Sh | ShellType::Cmd` into separate branches.

**Cmd approach**: Use `forfiles` for file timestamp/size and `certutil` for SHA256 hash:

```rust
ShellType::Cmd => {
    // forfiles outputs date/size; certutil computes SHA256
    // Expected output format (2 lines per file):
    //   Line 1: "MM/DD/YYYY HH:MM AM <size_bytes>" (from 'for' loop)
    //   Line 2: "<hex_hash>" (from certutil, filtered by findstr)
    format!(
        "for %f in ({p}) do @echo %~tf %~zf & \
         certutil -hashfile {p} SHA256 2>nul | findstr /v \"hash\\|certutil\\|CertUtil\"",
        p = escaped
    )
}
```

**Parser update**: `parse_file_metadata()` must handle Cmd output format alongside existing Sh and PowerShell formats. Add Cmd-specific parsing branch that extracts mtime and hash from the Windows output.

#### 3b. Directory Expansion (`build_dir_expand_cmd`)

Separate Cmd from Sh for directory listing.

```rust
ShellType::Cmd => {
    let dir_flag = if recursive { "/s /b" } else { "/b" };
    // dir /b lists filenames; /s adds recursion
    format!("dir {} \"{}\"", dir_flag, path)
}
```

**Parser update**: `parse_dir_expand_output()` must handle `dir /b` output format (full paths on Windows vs relative on Sh).

#### 3c. Remote mkdir (`distribute` / `distribute_pooled`)

Make mkdir command shell-aware:

```rust
fn build_mkdir_cmd(shell: ShellType, parent: &str) -> String {
    match shell {
        ShellType::Sh => format!("mkdir -p '{}'", parent.replace('\'', "'\\''")),
        ShellType::PowerShell => {
            format!("New-Item -ItemType Directory -Force -Path '{}'", parent)
        }
        ShellType::Cmd => {
            format!("if not exist \"{}\" mkdir \"{}\"", parent, parent)
        }
    }
}
```

#### 3d. Batch Metadata Collection (`build_batch_metadata_cmd`)

Same split as 3a, applied to the batch variant that wraps metadata commands in a loop. Cmd gets its own loop using `for %f in (file1 file2 ...) do ...` syntax.

---

### 4. HOME Variable Fix

**File**: `src/commands/sync.rs` — `to_tilde_path()`

```rust
fn to_tilde_path(path: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    if let Some(ref home) = home {
        if !home.is_empty() {
            if path == home.as_str() {
                return "~".to_string();
            }
            // Check both / and \ separators for cross-platform paths
            let prefix_fwd = format!("{}/", home);
            let prefix_bk = format!("{}\\", home);
            if let Some(rest) = path.strip_prefix(&prefix_fwd)
                .or_else(|| path.strip_prefix(&prefix_bk))
            {
                return format!("~/{}", rest);
            }
        }
    }
    path.to_string()
}
```

---

### 5. ANSI Terminal Support

**File**: `src/main.rs` (or `src/output/printer.rs`)

Enable Virtual Terminal Processing on Windows for ANSI escape code support.

```rust
#[cfg(target_os = "windows")]
fn enable_ansi_support() {
    // crossterm is already a dependency when TUI feature is enabled
    #[cfg(feature = "tui")]
    {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnableAnsiSupport);
    }
    // When TUI is disabled, use windows-sys directly or accept degraded output
    #[cfg(not(feature = "tui"))]
    {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            let handle = std::io::stdout().as_raw_handle();
            let mut mode: u32 = 0;
            windows_sys::Win32::System::Console::GetConsoleMode(
                handle as _, &mut mode);
            windows_sys::Win32::System::Console::SetConsoleMode(
                handle as _,
                mode | windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}
```

Call `enable_ansi_support()` early in `main()`, before any colored output.

---

### 6. exec.rs Script Execution Fixes

**File**: `src/commands/exec.rs`

The `chmod +x` call is already guarded to `ShellType::Sh` — this is correct. No change needed.

The `get_expanded_temp_dir_pooled()` function already handles all three shells correctly. No change needed.

---

## Files Changed

| File | Change Summary |
|------|---------------|
| `src/host/connection.rs` | Dual-mode ConnectionManager (Pooled/Direct) |
| `src/host/executor.rs` | Shell-aware SCP probe paths and cleanup |
| `src/commands/sync.rs` | Split Cmd from Sh in metadata/dir_expand/mkdir/batch_metadata; fix HOME var |
| `src/output/printer.rs` or `src/main.rs` | Enable ANSI support on Windows |

## Files Not Changed (Already Correct)

| File | Reason |
|------|--------|
| `src/config/app.rs` | Already has `#[cfg(target_os)]` for config dir |
| `src/state/db.rs` | Already has `#[cfg(target_os)]` for state dir |
| `src/config/ssh_config.rs` | `~/.ssh/config` is correct on Windows OpenSSH too |
| `src/host/shell.rs` | `temp_dir()` already returns shell-appropriate paths |
| `src/commands/exec.rs` | `chmod` already guarded to Sh; temp expansion already shell-aware |
| `src/host/executor.rs` (ssh_base_args) | Already handles `socket: None` correctly |

## Testing Strategy

1. **Unit tests**: Add `#[cfg(target_os = "windows")]` test for `ConnectionManager::new()` succeeding
2. **Unit tests**: Test `to_tilde_path()` with both `HOME` and `USERPROFILE`
3. **Existing tests**: Run full `cargo test` to ensure no regressions
4. **Manual test**: `ssync init` on Windows should complete without socket directory error
5. **Build matrix**: Verify both `cargo build` and `cargo build --no-default-features` on Windows

## Out of Scope

- WSL SSH detection/integration
- Embedded SSH library (against project principles)
- Full Cmd remote shell overhaul beyond known broken paths
- Platform abstraction module (decided against in favor of inline `cfg!()`)
