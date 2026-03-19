# Init Host Key Acceptance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When `ssync init` encounters hosts with unknown SSH keys, prompt the user to accept them via `ssh-keyscan` and retry automatically.

**Architecture:** After `pre_check`, partition failures by error type. For "Host key verification failed" errors, use `ssh -G` to resolve host/port, run `ssh-keyscan` in parallel, batch-append to `~/.ssh/known_hosts`, then retry with a second `ConnectionManager`. All changes are in `src/commands/init.rs`.

**Tech Stack:** Rust, tokio (async/process), ssh-keyscan (system command), ssh -G (config resolution)

**Spec:** `docs/superpowers/specs/2026-03-19-init-host-key-acceptance-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/commands/init.rs` | Modify (lines 119-134) | Add host key failure partitioning, user prompt, keyscan, retry logic |

No new files are created. All logic is added to the existing `init.rs` `run()` function.

---

### Task 1: Add helper — partition failures by host key error

Extract a pure function to partition `failed_hosts()` output into host-key failures vs other failures. This is unit-testable.

**Files:**
- Modify: `src/commands/init.rs` (add function + test at bottom)

- [ ] **Step 1: Write the failing test**

Add at bottom of `src/commands/init.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_host_key_failures_mixed() {
        let failures = vec![
            ("host-a".to_string(), "ControlMaster failed: Host key verification failed.".to_string()),
            ("host-b".to_string(), "Connection refused".to_string()),
            ("host-c".to_string(), "ControlMaster failed: Host key verification failed.".to_string()),
        ];
        let (hk, other) = partition_host_key_failures(failures);
        assert_eq!(hk.len(), 2);
        assert_eq!(hk[0].0, "host-a");
        assert_eq!(hk[1].0, "host-c");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].0, "host-b");
    }

    #[test]
    fn test_partition_host_key_failures_none() {
        let failures = vec![
            ("host-a".to_string(), "Connection timeout".to_string()),
        ];
        let (hk, other) = partition_host_key_failures(failures);
        assert!(hk.is_empty());
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn test_partition_host_key_failures_all() {
        let failures = vec![
            ("host-a".to_string(), "Host key verification failed.".to_string()),
        ];
        let (hk, other) = partition_host_key_failures(failures);
        assert_eq!(hk.len(), 1);
        assert!(other.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test commands::init::tests -- --nocapture`
Expected: FAIL — `partition_host_key_failures` not found.

- [ ] **Step 3: Write the partition function**

Add above the `run()` function in `src/commands/init.rs`:

```rust
/// Partition connection failures into host-key verification errors and other errors.
fn partition_host_key_failures(
    failures: Vec<(String, String)>,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut host_key_failures = Vec::new();
    let mut other_failures = Vec::new();
    for (name, err) in failures {
        if err.contains("Host key verification failed") {
            host_key_failures.push((name, err));
        } else {
            other_failures.push((name, err));
        }
    }
    (host_key_failures, other_failures)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test commands::init::tests -- --nocapture`
Expected: All 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat(init): add partition_host_key_failures helper with tests"
```

---

### Task 2: Add helper — resolve SSH host/port via `ssh -G`

`ssh-keyscan` needs the actual hostname and port, not the SSH alias. Use `ssh -G <alias>` to resolve them.

**Files:**
- Modify: `src/commands/init.rs` (add function)

- [ ] **Step 1: Write the resolve function**

Add below `partition_host_key_failures` in `src/commands/init.rs`:

```rust
/// Resolve the actual hostname and port for an SSH alias using `ssh -G`.
/// Returns (hostname, port). Falls back to (alias, "22") on failure.
async fn resolve_ssh_host_port(alias: &str) -> (String, String) {
    let output = tokio::process::Command::new("ssh")
        .arg("-G")
        .arg(alias)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    let mut hostname = alias.to_string();
    let mut port = "22".to_string();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("hostname ") {
                hostname = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("port ") {
                port = val.trim().to_string();
            }
        }
    }

    (hostname, port)
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 3: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat(init): add resolve_ssh_host_port helper using ssh -G"
```

---

### Task 3: Add helper — run ssh-keyscan for a single host

Run `ssh-keyscan -H -p <port> <hostname>` with a timeout. Return the keyscan output or an error.

**Files:**
- Modify: `src/commands/init.rs` (add function + test)

- [ ] **Step 1: Write the keyscan function**

Add below `resolve_ssh_host_port`:

```rust
/// Run ssh-keyscan for a single host and return the output lines (key entries).
/// Returns Ok(output) on success, Err on failure or empty output.
async fn keyscan_host(alias: &str, timeout_secs: u64) -> Result<String> {
    let (hostname, port) = resolve_ssh_host_port(alias).await;

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new("ssh-keyscan")
            .arg("-H")
            .arg("-p")
            .arg(&port)
            .arg(&hostname)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .context("ssh-keyscan timeout")?
    .context("Failed to run ssh-keyscan")?;

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();

    // Filter out empty lines and comments
    let key_lines: String = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    if key_lines.is_empty() {
        anyhow::bail!("ssh-keyscan returned no keys for {}", alias);
    }

    Ok(key_lines)
}
```

- [ ] **Step 2: Add the `use anyhow::Context;` import if not already present**

Check the imports at the top of `init.rs`. The file uses `anyhow::Result` but may not import `Context`. Add it if missing:

```rust
use anyhow::{Context, Result};
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 4: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat(init): add keyscan_host helper for ssh-keyscan execution"
```

---

### Task 4: Add helper — batch keyscan and write to known_hosts

Run keyscan for multiple hosts in parallel, collect results, append to `~/.ssh/known_hosts` in one write.

**Files:**
- Modify: `src/commands/init.rs` (add function)

- [ ] **Step 1: Write the batch keyscan function**

Add below `keyscan_host`:

```rust
/// Run ssh-keyscan for multiple hosts in parallel and append results to ~/.ssh/known_hosts.
/// Returns the list of host names that were successfully keyscanned.
async fn batch_keyscan_and_accept(
    hosts: &[(String, String)],
    timeout_secs: u64,
    concurrency: usize,
) -> Vec<String> {
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for (name, _err) in hosts {
        let sem = semaphore.clone();
        let alias = name.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = keyscan_host(&alias, timeout_secs).await;
            (alias, result)
        }));
    }

    let mut all_keys = String::new();
    let mut succeeded = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((alias, Ok(keys))) => {
                if !all_keys.is_empty() {
                    all_keys.push('\n');
                }
                all_keys.push_str(&keys);
                succeeded.push(alias.clone());
                printer::print_host_line(&alias, "ok", "host key accepted");
            }
            Ok((alias, Err(e))) => {
                printer::print_host_line(&alias, "error", &format!("keyscan failed: {}", e));
            }
            Err(e) => {
                tracing::warn!("keyscan task panicked: {}", e);
            }
        }
    }

    // Append all keys to ~/.ssh/known_hosts in one operation
    if !all_keys.is_empty() {
        let known_hosts_path = dirs::home_dir()
            .map(|h| h.join(".ssh").join("known_hosts"))
            .expect("Could not determine home directory");

        // Ensure the .ssh directory exists
        if let Some(parent) = known_hosts_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Ensure trailing newline before appending
        let mut content = String::new();
        if known_hosts_path.exists() {
            if let Ok(existing) = std::fs::read_to_string(&known_hosts_path) {
                if !existing.ends_with('\n') && !existing.is_empty() {
                    content.push('\n');
                }
            }
        }
        content.push_str(&all_keys);
        if !content.ends_with('\n') {
            content.push('\n');
        }

        use std::io::Write;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&known_hosts_path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(content.as_bytes()) {
                    eprintln!("Warning: failed to write known_hosts: {}", e);
                }
            }
            Err(e) => {
                eprintln!("Warning: failed to open known_hosts: {}", e);
            }
        }
    }

    succeeded
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 3: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat(init): add batch_keyscan_and_accept helper"
```

---

### Task 5: Integrate host key handling into the init flow

Replace the existing "Report unreachable hosts" block (lines 130-134) with the new partitioning, prompting, keyscan, and retry logic.

**Files:**
- Modify: `src/commands/init.rs` (lines 119-153, the pre-check + shell detection section)

- [ ] **Step 1: Replace the failure reporting block with the new flow**

In `src/commands/init.rs`, replace lines 130-134 (the `// Report unreachable hosts` block) with the new logic. The full replacement for the section from `// Pre-check connectivity via ControlMaster` through to `// Detect shell type` should become:

```rust
        // Pre-check connectivity via ControlMaster
        let mut conn_mgr = ConnectionManager::new()?;
        let mut progress = SyncProgress::new();

        progress.start_host_check(entry_refs.len());
        let connected = conn_mgr
            .pre_check(&entry_refs, ctx.timeout, ctx.concurrency())
            .await;
        let failed_count = entry_refs.len() - connected;
        progress.finish_host_check(connected, failed_count);

        // Partition failures: host key errors vs other errors
        let (host_key_failures, other_failures) =
            partition_host_key_failures(conn_mgr.failed_hosts());

        // Report non-host-key errors immediately
        for (name, err) in &other_failures {
            printer::print_host_line(name, "error", err);
            summary.add_failure(name, err);
        }

        // Handle host key verification failures
        let mut retry_conn_mgr: Option<ConnectionManager> = None;
        if !host_key_failures.is_empty() {
            if dry_run {
                for (name, _err) in &host_key_failures {
                    printer::print_host_line(name, "skip", "unknown host key (dry-run, skipped)");
                    summary.add_skip();
                }
            } else {
                println!(
                    "\n{} host(s) have unknown SSH host keys:",
                    host_key_failures.len()
                );
                for (name, _err) in &host_key_failures {
                    println!("  - {}", name);
                }
                print!("Add to known_hosts and retry? [y/N]: ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;

                if answer.trim().eq_ignore_ascii_case("y") {
                    let accepted = batch_keyscan_and_accept(
                        &host_key_failures,
                        ctx.timeout,
                        ctx.concurrency(),
                    )
                    .await;

                    // Report hosts where keyscan failed
                    for (name, err) in &host_key_failures {
                        if !accepted.contains(name) {
                            printer::print_host_line(
                                name,
                                "error",
                                &format!("keyscan failed: {}", err),
                            );
                            summary.add_failure(name, "keyscan failed");
                        }
                    }

                    // Retry connection for accepted hosts
                    if !accepted.is_empty() {
                        let retry_entries: Vec<HostEntry> = accepted
                            .iter()
                            .map(|name| HostEntry {
                                name: name.clone(),
                                ssh_host: name.clone(),
                                shell: crate::config::schema::ShellType::Sh,
                                groups: Vec::new(),
                            })
                            .collect();
                        let retry_refs: Vec<&HostEntry> = retry_entries.iter().collect();

                        let mut retry_cm = ConnectionManager::new()?;
                        println!("\nRetrying {} host(s)...", accepted.len());
                        progress.start_host_check(retry_refs.len());
                        let retry_connected = retry_cm
                            .pre_check(&retry_refs, ctx.timeout, ctx.concurrency())
                            .await;
                        let retry_failed = retry_refs.len() - retry_connected;
                        progress.finish_host_check(retry_connected, retry_failed);

                        // Report retry failures
                        for (name, err) in retry_cm.failed_hosts() {
                            printer::print_host_line(&name, "error", &err);
                            summary.add_failure(&name, &err);
                        }

                        retry_conn_mgr = Some(retry_cm);
                    }
                } else {
                    // User declined — report as errors
                    for (name, err) in &host_key_failures {
                        printer::print_host_line(name, "error", err);
                        summary.add_failure(name, err);
                    }
                }
            }
        }

        // Detect shell type on reachable hosts using pooled connections
        // Merge reachable hosts from both connection managers
        let mut reachable = conn_mgr.reachable_hosts();
        if let Some(ref rcm) = retry_conn_mgr {
            reachable.extend(rcm.reachable_hosts());
        }
        let global_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(ctx.concurrency()));
```

Then update the shell detection loop to pick the right socket from either CM:

```rust
        let mut handles = Vec::new();
        for host_name in &reachable {
            let sem = global_sem.clone();
            let host_name = host_name.clone();
            let timeout = ctx.timeout;
            // Check both CMs for a socket path
            let socket = conn_mgr
                .socket_for(&host_name)
                .or_else(|| retry_conn_mgr.as_ref().and_then(|rcm| rcm.socket_for(&host_name)))
                .map(|p| p.to_path_buf());

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let shell_result =
                    shell::detect_pooled(&host_name, timeout, socket.as_deref()).await;
                (host_name, shell_result)
            }));
        }
```

And update the shutdown section to shut down both CMs:

```rust
        conn_mgr.shutdown().await;
        if let Some(mut rcm) = retry_conn_mgr {
            rcm.shutdown().await;
        }
        progress.clear();
        summary.print();
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass, including the new partition tests.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings.

- [ ] **Step 5: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat(init): prompt to accept unknown SSH host keys and retry

When init encounters 'Host key verification failed' errors, partition
them from other failures, prompt the user to add keys via ssh-keyscan,
and retry the connection automatically."
```

---

### Task 6: Final validation

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 2: Build release**

Run: `cargo build`
Expected: Build succeeds.

- [ ] **Step 3: Build without TUI**

Run: `cargo build --no-default-features`
Expected: Build succeeds.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings.

- [ ] **Step 5: Verify formatting**

Run: `cargo fmt --check`
Expected: No formatting issues.
