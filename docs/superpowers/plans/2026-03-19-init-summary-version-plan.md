# Init Stale Host Detection, Summary Hostnames, Version Verification — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add stale host cleanup to init, show hostnames in sync summary, and add version verification to the release workflow.

**Architecture:** Three independent changes: (1) init.rs gains a stale-host comparison + prompt before the detection loop, (2) SyncSummary in summary.rs tracks hostname vectors alongside counts and formats them in parentheses, (3) release.yml gets a version-match guard step.

**Tech Stack:** Rust (tokio, clap, anyhow), GitHub Actions YAML

**Spec:** `docs/superpowers/specs/2026-03-19-init-summary-version-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/output/summary.rs` | Modify | Add hostname tracking vectors to `SyncSummary`, update `complete_file`/`file_in_sync`/`add_host_failure`, update `print()` format |
| `src/commands/init.rs` | Modify | Add stale-host detection logic, prompt, and removal before detection loop |
| `.github/workflows/release.yml` | Modify | Add version verification step to build job |

---

### Task 1: Add hostname tracking to SyncSummary

**Files:**
- Modify: `src/output/summary.rs:142-157` (struct fields)
- Modify: `src/output/summary.rs:159-217` (methods)
- Modify: `src/output/summary.rs:219-254` (print method)
- Test: `src/output/summary.rs` (existing test module)

- [ ] **Step 1: Write failing tests for hostname tracking**

Add tests at the end of the `mod tests` block in `src/output/summary.rs`:

```rust
#[test]
fn test_sync_summary_tracks_passed_hosts() {
    let mut s = SyncSummary::default();
    s.file_in_sync(&["host-a", "host-b", "host-c"]);
    assert_eq!(s.transfers_passed_hosts, vec!["host-a", "host-b", "host-c"]);
}

#[test]
fn test_sync_summary_tracks_synced_hosts() {
    let mut s = SyncSummary::default();
    s.complete_file(
        "~/.bashrc",
        &["host-a".to_string()],
        &["host-b".to_string(), "host-c".to_string()],
        &[],
    );
    assert_eq!(s.transfers_passed_hosts, vec!["host-a"]);
    assert_eq!(s.transfers_synced_hosts, vec!["host-b", "host-c"]);
}

#[test]
fn test_sync_summary_tracks_failed_hosts() {
    let mut s = SyncSummary::default();
    s.complete_file(
        "~/config",
        &[],
        &[],
        &[
            ("host-a".to_string(), "download failed".to_string()),
            ("host-b".to_string(), "download failed".to_string()),
        ],
    );
    assert_eq!(s.transfers_failed_hosts, vec!["host-a", "host-b"]);
}

#[test]
fn test_sync_summary_tracks_host_failure_hosts() {
    let mut s = SyncSummary::default();
    s.add_host_failure("host-x", "unreachable");
    assert_eq!(s.transfers_failed_hosts, vec!["host-x"]);
}

#[test]
fn test_sync_summary_deduplicates_hosts_in_format() {
    let mut s = SyncSummary::default();
    // Same host appears in two files
    s.complete_file("~/.bashrc", &[], &["host-a".to_string()], &[]);
    s.complete_file("~/.zshrc", &[], &["host-a".to_string()], &[]);
    assert_eq!(s.transfers_synced, 2);
    // Host list has duplicates (expected), but format_hosts deduplicates
    assert_eq!(SyncSummary::format_hosts(&s.transfers_synced_hosts), "host-a");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test output::summary::tests -- --nocapture 2>&1 | tail -20`
Expected: compilation errors — `transfers_passed_hosts`, `transfers_synced_hosts`, `transfers_failed_hosts`, `format_hosts` don't exist yet.

- [ ] **Step 3: Add hostname vector fields to SyncSummary struct**

In `src/output/summary.rs`, add three new fields to the `SyncSummary` struct after the existing counter fields (after line 152):

```rust
// Transfer-level hostname tracking
pub transfers_passed_hosts: Vec<String>,
pub transfers_synced_hosts: Vec<String>,
pub transfers_failed_hosts: Vec<String>,
```

> **Note:** The spec mentions `transfers_skipped_hosts` but this is deliberately omitted. Skips are tracked at the file level (`files_skipped`), not the transfer level. The "Skipped:" section in the summary already shows per-host details. Adding a transfer-level skip counter and hostname vec would be redundant.

- [ ] **Step 4: Add `format_hosts` helper method**

Add a public associated function to the `impl SyncSummary` block (before the `complete_file` method):

```rust
/// Deduplicate a hostname list (preserving first-seen order) and join with ", ".
pub fn format_hosts(hosts: &[String]) -> String {
    let mut seen = Vec::new();
    for h in hosts {
        if !seen.contains(&h) {
            seen.push(h);
        }
    }
    seen.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
}
```

- [ ] **Step 5: Update `complete_file` to track hostnames**

In `complete_file()`, after the counter increments (lines 172-174), add two `extend` calls for passed/synced:

```rust
self.transfers_passed_hosts.extend_from_slice(passed);
self.transfers_synced_hosts.extend_from_slice(synced);
```

Then replace the existing failed loop (lines 176-182) with a merged version that tracks both hostnames and errors:

```rust
for (host, msg) in failed {
    self.transfers_failed_hosts.push(host.clone());
    self.errors.push(ErrorEntry {
        host: host.clone(),
        message: msg.clone(),
        path: Some(path.to_string()),
    });
}
```

- [ ] **Step 6: Update `file_in_sync` to track hostnames**

In `file_in_sync()`, add after `self.transfers_passed += passed_hosts.len();`:

```rust
self.transfers_passed_hosts.extend(passed_hosts.iter().map(|s| s.to_string()));
```

- [ ] **Step 7: Update `add_host_failure` to track hostnames**

In `add_host_failure()`, add after `self.transfers_failed += 1;`:

```rust
self.transfers_failed_hosts.push(host.to_string());
```

- [ ] **Step 8: Update `print()` to show hostnames in parentheses**

Replace the transfer-level line section in `print()` (lines 241-254) with:

```rust
// Transfer-level line
let mut xfer_parts = Vec::new();
if self.transfers_passed > 0 {
    let hosts = Self::format_hosts(&self.transfers_passed_hosts);
    xfer_parts.push(format!("{} passed({})", self.transfers_passed, hosts));
}
if self.transfers_synced > 0 {
    let hosts = Self::format_hosts(&self.transfers_synced_hosts);
    xfer_parts.push(format!("{} synced({})", self.transfers_synced, hosts));
}
if self.transfers_failed > 0 {
    let hosts = Self::format_hosts(&self.transfers_failed_hosts);
    xfer_parts.push(format!("{} failed({})", self.transfers_failed, hosts));
}
if !xfer_parts.is_empty() {
    println!("  Transfers:  {}", xfer_parts.join("  "));
}
```

- [ ] **Step 9: Run all tests to verify they pass**

Run: `cargo test output::summary::tests -- --nocapture 2>&1`
Expected: All 19 tests pass (14 existing + 5 new).

- [ ] **Step 10: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 60 tests pass, no clippy warnings.

- [ ] **Step 11: Commit**

```bash
git add src/output/summary.rs
git commit -m "feat: show hostnames in sync summary transfer line

Transfers line now shows which hosts are in each category:
  Transfers:  3 passed(macmini, debian, smbnfs)  1 synced(tonarchy)  1 failed(iphone12)

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 2: Add stale host detection to init command

**Files:**
- Modify: `src/commands/init.rs:13-49` (insert stale-host logic before detection loop)

- [ ] **Step 1: Add stale host detection and prompt logic**

In `src/commands/init.rs`, after line 24 (`let effective_update = update || config_exists;`) and before line 26 (`// Merge CLI --skip with persisted skipped_hosts`), insert stale-host detection:

```rust
// Detect hosts in ssync config that no longer exist in ~/.ssh/config
let ssh_host_names: std::collections::HashSet<&str> =
    ssh_hosts.iter().map(|h| h.name.as_str()).collect();
let stale_hosts: Vec<String> = ctx
    .config
    .host
    .iter()
    .filter(|h| !ssh_host_names.contains(h.ssh_host.as_str()))
    .map(|h| h.ssh_host.clone())
    .collect();

if !stale_hosts.is_empty() {
    println!(
        "\nFound {} host(s) no longer in ~/.ssh/config:",
        stale_hosts.len()
    );
    for name in &stale_hosts {
        println!("  - {}", name);
    }

    if dry_run {
        println!(
            "[dry-run] Would remove {} stale host(s).",
            stale_hosts.len()
        );
    } else {
        print!(
            "Remove these {} host(s) from ssync config? [y/N]: ",
            stale_hosts.len()
        );
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            stale_hosts_removed = true;
            println!("Removed {} stale host(s).", stale_hosts.len());
        }
    }
}
```

Also, declare a mutable binding before this block and a `stale_hosts` clone for later use:

At the top of the function (after `let effective_update = ...`), add:
```rust
let mut stale_hosts_removed = false;
let mut stale_host_names: Vec<String> = Vec::new();
```

Use `stale_host_names` directly in the detection block (no intermediate variable):
```rust
stale_host_names = ctx
    .config
    .host
    .iter()
    .filter(|h| !ssh_host_names.contains(h.ssh_host.as_str()))
    .map(|h| h.ssh_host.clone())
    .collect();
```

Then reference `stale_host_names` throughout (in the println, the prompt, and the retain calls).

- [ ] **Step 2: Apply stale host removal when merging config**

In the config merge section (around line 150), after loading the config but before saving, add the removal if confirmed:

After `let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();` and before the `for host in new_hosts` loop, add:

```rust
if stale_hosts_removed {
    config.host.retain(|h| !stale_host_names.contains(&h.ssh_host));
}
```

Also add the same removal in the second config-save path (the "skip-only" branch around line 181):

After `let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();`, add:
```rust
if stale_hosts_removed {
    config.host.retain(|h| !stale_host_names.contains(&h.ssh_host));
}
```

**Additionally**, handle the case where stale hosts were confirmed for removal but no other changes trigger a save (i.e. `detect_hosts.is_empty()` AND `skip.is_empty()`). At the end of the function (before the final `Ok(())`), add a third save path:

```rust
// Save if only stale hosts were removed (no other changes triggered a save)
if stale_hosts_removed {
    let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();
    config.host.retain(|h| !stale_host_names.contains(&h.ssh_host));
    crate::config::app::save(&config, ctx.config_path.as_deref())?;
    let saved_path = crate::config::app::resolve_path(ctx.config_path.as_deref())?;
    println!("\nConfig saved to {}", saved_path.display());
}
```

This covers the scenario where a user's SSH config has shrunk (host removed) but ssync config is otherwise stable — the most common real-world case.

- [ ] **Step 3: Handle edge case — config doesn't exist yet**

The stale-host check should only run when a config already exists. Wrap the stale-host block with:

```rust
if config_exists {
    // ... stale host detection block ...
}
```

This prevents false positives when running init for the first time (ctx.config.host would be empty anyway, but it's clearer to guard it).

- [ ] **Step 4: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: All tests pass, no clippy warnings. (Init has no unit tests — it's an integration-heavy function that relies on SSH connectivity.)

- [ ] **Step 5: Manual smoke test description**

To manually test:
1. Add a fake host to `~/.config/ssync/config.toml` that doesn't exist in `~/.ssh/config`
2. Run `ssync init` — should prompt to remove it
3. Run `ssync init --dry-run` — should show the stale list but not prompt

- [ ] **Step 6: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat: detect and prompt to remove stale hosts during init

When running 'ssync init', hosts in ssync config that no longer exist
in ~/.ssh/config are detected. User is shown a summary and prompted
for confirmation before removal. Dry-run mode shows the list without
prompting.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 3: Add version verification to release workflow

**Files:**
- Modify: `.github/workflows/release.yml` (add step after checkout)

- [ ] **Step 1: Add version verification step**

In `.github/workflows/release.yml`, in the `build` job, after the "Checkout code" step and before the "Setup Rust" step, add:

```yaml
      - name: Verify version matches tag
        if: startsWith(github.ref, 'refs/tags/v')
        shell: bash
        run: |
          TAG_VERSION="${GITHUB_REF#refs/tags/v}"
          CARGO_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
          if [ "$TAG_VERSION" != "$CARGO_VERSION" ]; then
            echo "::error::Version mismatch! Tag: v$TAG_VERSION, Cargo.toml: $CARGO_VERSION"
            echo "Please ensure Cargo.toml version is updated before tagging."
            exit 1
          fi
          echo "Version verified: v$TAG_VERSION matches Cargo.toml"
```

- [ ] **Step 2: Verify YAML syntax**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))" && echo "YAML valid"`
Expected: "YAML valid"

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add version verification step to release workflow

Fails the build early if the git tag version doesn't match the
version in Cargo.toml. Prevents releasing binaries with incorrect
version numbers.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Execution Order

Tasks 1, 2, and 3 are independent and can be done in any order. Recommended order:
1. **Task 1** (summary hostnames) — has testable unit changes, good TDD candidate
2. **Task 2** (init stale hosts) — integration-heavy, manual test
3. **Task 3** (release workflow) — simple YAML change
