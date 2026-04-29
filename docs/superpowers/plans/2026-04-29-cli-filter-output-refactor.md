# CLI Filter & Output Refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--shell` as a 4th exclusive host-filter mode, remove `--format` from `checkout` (always table, TUI disabled by default), and introduce a unified `--out` flag on all remote-operation commands that writes structured JSON/HTML reports.

**Architecture:** `TargetMode::Shell` is added alongside existing variants in `commands/mod.rs`; a new `OutputArgs` struct is flattened into each applicable CLI command; a new `src/output/report.rs` module provides the canonical `OperationReport` type and file-write logic used by all five command handlers.

**Tech Stack:** Rust / clap 4 (derive), serde_json, chrono, anyhow — all already in `Cargo.toml`.

**Spec:** `docs/superpowers/specs/2026-04-29-cli-filter-output-refactor-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/config/schema.rs` | Modify | Add `clap::ValueEnum` to `ShellType` |
| `src/cli.rs` | Modify | Add `OutputArgs`; add `--shell/-S` to `TargetArgs`; update `Checkout`; add `OutputArgs` flatten to `Check`/`Sync`/`Run`/`Exec`; remove `OutputFormat` |
| `src/commands/mod.rs` | Modify | Add `TargetMode::Shell`; update `resolve_target_mode`, `resolve_hosts`, `filter_entries_by_mode`, error hints |
| `src/host/filter.rs` | Modify | Add `shells` param to `filter_hosts`; update tests |
| `src/commands/sync.rs` | Modify | Fix non-exhaustive `TargetMode` match; add per-host file tracking; wire `--out` |
| `Cargo.toml` | Modify | `default = []` (disable TUI by default) |
| `src/output/report.rs` | **Create** | `OperationReport`, `FilterInfo`, `HostResult`, `ReportSummary`, `write_report`, `render_html_report` |
| `src/output/mod.rs` | Modify | `pub mod report;` |
| `src/commands/checkout.rs` | Modify | Remove `format` param; always table; wire `--out`; remove old `build_json_report`/`render_html` |
| `src/commands/check.rs` | Modify | Add `output` param; accumulate results; wire `--out` |
| `src/commands/run.rs` | Modify | Add `output` param; accumulate results; wire `--out` |
| `src/commands/exec.rs` | Modify | Add `output` param; accumulate results (incl. skipped); wire `--out` |
| `src/metrics/collector.rs` | Modify | Add `raw_stdout`/`raw_stderr` to `CollectionResult` |
| `src/main.rs` | Modify | Destructure `output` from each command; pass to handlers |

---

## Task 1 — `ShellType` gains `clap::ValueEnum`

**Files:**
- Modify: `src/config/schema.rs`

- [ ] **Step 1: Add the derive**

In `src/config/schema.rs`, update the `ShellType` derives:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum ShellType {
    Sh,
    #[serde(rename = "powershell")]
    #[clap(name = "powershell")]
    PowerShell,
    Cmd,
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check 2>&1 | head -20
```
Expected: no errors (clap re-exports ValueEnum via the `derive` feature, which is already enabled).

- [ ] **Step 3: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat: add clap::ValueEnum to ShellType

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 2 — `--shell/-S` in `TargetArgs` + `TargetMode::Shell` + all match sites

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/commands/sync.rs` (label match)

- [ ] **Step 1: Add `--shell` to `TargetArgs` in `src/cli.rs`**

Add after the `all` field:

```rust
/// Filter by remote shell type (comma-separated: sh, powershell, cmd)
#[arg(short = 'S', long, value_delimiter = ',')]
pub shell: Vec<crate::config::schema::ShellType>,
```

The full `TargetArgs` struct becomes:

```rust
#[derive(Args, Clone, Debug)]
pub struct TargetArgs {
    /// Specify groups (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub group: Vec<String>,

    /// Specify hosts (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub host: Vec<String>,

    /// Target all hosts
    #[arg(short, long)]
    pub all: bool,

    /// Filter by remote shell type (comma-separated: sh, powershell, cmd)
    #[arg(short = 'S', long, value_delimiter = ',')]
    pub shell: Vec<crate::config::schema::ShellType>,

    /// Execute sequentially instead of in parallel
    #[arg(long)]
    pub serial: bool,

    /// Connection timeout in seconds (overrides config)
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Print help
    #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
    pub help: Option<bool>,
}
```

- [ ] **Step 2: Add `TargetMode::Shell` variant in `src/commands/mod.rs`**

```rust
/// Target mode derived from CLI flags.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetMode {
    /// --all: all configured hosts
    All,
    /// --host: specific hosts by name
    Hosts(Vec<String>),
    /// --group: hosts belonging to named groups
    Groups(Vec<String>),
    /// --shell: hosts with the specified shell type(s)
    Shell(Vec<crate::config::schema::ShellType>),
}
```

- [ ] **Step 3: Update `resolve_target_mode()` in `src/commands/mod.rs`**

Replace the existing `resolve_target_mode` function:

```rust
fn resolve_target_mode(target: &TargetArgs, config: &AppConfig) -> Result<TargetMode> {
    let has_all = target.all;
    let has_hosts = !target.host.is_empty();
    let has_groups = !target.group.is_empty();
    let has_shell = !target.shell.is_empty();

    let count = has_all as u8 + has_hosts as u8 + has_groups as u8 + has_shell as u8;

    if count == 0 {
        let mut hint = String::from(
            "Target required. Use --group/-g, --host/-h, --shell/-S, or --all/-a to specify targets.",
        );
        if config.host.is_empty() {
            hint.push_str("\nHint: Run 'ssync init' first to import hosts from ~/.ssh/config.");
        } else {
            append_available_hints(config, &mut hint);
        }
        bail!("{}", hint);
    }

    if count > 1 {
        bail!("Only one of --all/-a, --host/-h, --group/-g, or --shell/-S can be used at a time.");
    }

    if has_all {
        Ok(TargetMode::All)
    } else if has_hosts {
        Ok(TargetMode::Hosts(target.host.clone()))
    } else if has_groups {
        Ok(TargetMode::Groups(target.group.clone()))
    } else {
        Ok(TargetMode::Shell(target.shell.clone()))
    }
}
```

- [ ] **Step 4: Update `resolve_hosts()` in `src/commands/mod.rs`**

Add the `Shell` arm:

```rust
pub fn resolve_hosts(&self) -> Result<Vec<&HostEntry>> {
    let hosts: Vec<&HostEntry> = match &self.mode {
        TargetMode::All => self.config.host.iter().collect(),
        TargetMode::Hosts(names) => self
            .config
            .host
            .iter()
            .filter(|h| names.contains(&h.name))
            .collect(),
        TargetMode::Groups(groups) => self
            .config
            .host
            .iter()
            .filter(|h| h.groups.iter().any(|g| groups.contains(g)))
            .collect(),
        TargetMode::Shell(shells) => self
            .config
            .host
            .iter()
            .filter(|h| shells.contains(&h.shell))
            .collect(),
    };

    if hosts.is_empty() {
        let mut hint = String::from("No hosts matched the specified filter.");
        if let TargetMode::Shell(shells) = &self.mode {
            hint = format!(
                "No hosts matched shell type: {}",
                shells.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
            );
            // Show available shells
            let mut shell_map: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for h in &self.config.host {
                shell_map
                    .entry(h.shell.to_string())
                    .or_default()
                    .push(h.name.clone());
            }
            if !shell_map.is_empty() {
                let parts: Vec<String> = shell_map
                    .iter()
                    .map(|(shell, hosts)| format!("{} ({})", shell, hosts.join(", ")))
                    .collect();
                hint.push_str(&format!("\nAvailable shells: {}", parts.join(", ")));
            }
        } else {
            append_available_hints(&self.config, &mut hint);
        }
        bail!("{}", hint);
    }

    Ok(hosts)
}
```

- [ ] **Step 5: Update `filter_entries_by_mode()` in `src/commands/mod.rs`**

Add `Shell` arm — treat like `Hosts` (uses `enable_hosts` flag):

```rust
fn filter_entries_by_mode<'a, T>(
    entries: &'a [T],
    get_groups: impl Fn(&T) -> &Vec<String>,
    get_enable_hosts: impl Fn(&T) -> bool,
    get_enable_all: impl Fn(&T) -> bool,
    mode: &TargetMode,
) -> Vec<&'a T> {
    entries
        .iter()
        .filter(|e| match mode {
            TargetMode::All => get_enable_all(e),
            TargetMode::Groups(g) => get_groups(e).iter().any(|eg| g.contains(eg)),
            TargetMode::Hosts(_) => get_enable_hosts(e),
            TargetMode::Shell(_) => get_enable_hosts(e),
        })
        .collect()
}
```

- [ ] **Step 6: Fix non-exhaustive `TargetMode` match in `src/commands/sync.rs`**

Find the `label` computation around line 92 and add the `Shell` arm:

```rust
let label = match &ctx.mode {
    super::TargetMode::All => "global".to_string(),
    super::TargetMode::Groups(g) => g.join(", "),
    super::TargetMode::Hosts(h) => h.join(", "),
    super::TargetMode::Shell(s) => {
        s.iter().map(|sh| sh.to_string()).collect::<Vec<_>>().join(", ")
    }
};
```

- [ ] **Step 7: Write failing tests for shell filter in `src/host/filter.rs`**

The `filter_hosts` function in `host/filter.rs` is used independently. Add `shells` parameter and tests:

```rust
use crate::config::schema::ShellType;

#[allow(dead_code)]
pub fn filter_hosts<'a>(
    hosts: &'a [HostEntry],
    groups: &[String],
    host_names: &[String],
    all: bool,
    shells: &[ShellType],
) -> Vec<&'a HostEntry> {
    if all {
        return hosts.iter().collect();
    }

    let mut result: Vec<&HostEntry> = hosts.iter().collect();

    if !groups.is_empty() {
        result.retain(|h| h.groups.iter().any(|g| groups.contains(g)));
    }

    if !host_names.is_empty() {
        result.retain(|h| host_names.contains(&h.name));
    }

    if !shells.is_empty() {
        result.retain(|h| shells.contains(&h.shell));
    }

    result
}
```

Add these tests to the `#[cfg(test)]` block in `src/host/filter.rs`:

```rust
#[test]
fn test_filter_by_shell() {
    let hosts = make_hosts();
    let result = filter_hosts(&hosts, &[], &[], false, &[ShellType::Sh]);
    assert_eq!(result.len(), 2);
    assert!(result.iter().all(|h| h.shell == ShellType::Sh));
}

#[test]
fn test_filter_by_shell_powershell() {
    let hosts = make_hosts();
    let result = filter_hosts(&hosts, &[], &[], false, &[ShellType::PowerShell]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "b");
}

#[test]
fn test_filter_by_shell_multiple() {
    let hosts = make_hosts();
    let result = filter_hosts(
        &hosts,
        &[],
        &[],
        false,
        &[ShellType::Sh, ShellType::PowerShell],
    );
    assert_eq!(result.len(), 3);
}

#[test]
fn test_filter_no_shell_filter_returns_all_when_no_other_filter() {
    let hosts = make_hosts();
    let result = filter_hosts(&hosts, &[], &[], false, &[]);
    assert_eq!(result.len(), 3);
}
```

Also update the existing 4 tests that call `filter_hosts` to add `&[]` as the new last argument:
- `test_filter_all`: `filter_hosts(&hosts, &[], &[], true, &[])`
- `test_filter_by_group`: `filter_hosts(&hosts, &["web".into()], &[], false, &[])`
- `test_filter_by_host_name`: `filter_hosts(&hosts, &[], &["b".into()], false, &[])`
- `test_filter_intersection`: `filter_hosts(&hosts, &["web".into()], &["c".into()], false, &[])`

- [ ] **Step 8: Run tests**

```bash
cargo test host::filter::tests 2>&1
```
Expected: all 8 tests pass.

- [ ] **Step 9: Full compile check**

```bash
cargo check 2>&1 | head -30
```
Expected: 0 errors.

- [ ] **Step 10: Commit**

```bash
git add src/cli.rs src/commands/mod.rs src/host/filter.rs src/commands/sync.rs
git commit -m "feat: add --shell/-S target filter (TargetMode::Shell)

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 3 — Remove `--format`, disable TUI by default

**Files:**
- Modify: `src/cli.rs`
- Modify: `Cargo.toml`
- Modify: `src/commands/checkout.rs` (signature only — full wiring in Task 10)
- Modify: `src/main.rs` (dispatch only)

- [ ] **Step 1: Remove `OutputFormat` and update `Checkout` in `src/cli.rs`**

Remove the entire `OutputFormat` enum:

```rust
// DELETE this entire block:
// #[derive(Clone, clap::ValueEnum)]
// pub enum OutputFormat {
//     Tui,
//     Table,
//     Html,
//     Json,
// }
```

Update the `Checkout` variant to remove `format` and old `out`. (The new `out` comes via `OutputArgs` in Task 4 — for now add a placeholder `output: OutputArgs`; we'll also add it in Task 4 so do this step as part of Task 4 to avoid double-editing. Skip for now — only remove format here):

Change the `Checkout` variant in `Commands`:

```rust
/// View historical data and generate reports from state DB
#[command(disable_help_flag = true)]
Checkout {
    #[command(flatten)]
    target: TargetArgs,

    /// Show trend history
    #[arg(long)]
    history: bool,

    /// History start point (e.g. "2025-01-01" or "7d")
    #[arg(long)]
    since: Option<String>,

    /// Print help
    #[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
    help: Option<bool>,
},
```

(Note: `out` is removed here; `OutputArgs` flatten is added in Task 4.)

- [ ] **Step 2: Disable TUI default in `Cargo.toml`**

Change:
```toml
[features]
default = ["tui"]
tui = ["ratatui", "crossterm"]
```
to:
```toml
[features]
default = []
tui = ["ratatui", "crossterm"]
```

- [ ] **Step 3: Update `checkout::run()` signature in `src/commands/checkout.rs`**

Remove the `format` and `out` parameters (full `--out` wiring is in Task 10):

```rust
pub async fn run(
    ctx: &Context,
    history: bool,
    since: Option<String>,
) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;
    let host_names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();
    let columns = DisplayColumns::from_context(ctx);
    let snapshots = fetch_latest_snapshots(ctx, &host_names)?;
    print_table_report(&snapshots, &columns);
    Ok(())
}
```

- [ ] **Step 4: Update `main.rs` checkout dispatch**

```rust
Commands::Checkout {
    target,
    history,
    since,
    ..
} => {
    let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
    commands::checkout::run(&ctx, history, since).await
}
```

- [ ] **Step 5: Compile check**

```bash
cargo check 2>&1 | head -30
```
Expected: 0 errors.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs Cargo.toml src/commands/checkout.rs src/main.rs
git commit -m "refactor: remove --format from checkout, disable TUI by default

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 4 — `OutputArgs` struct + wire into CLI + `main.rs`

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add `OutputArgs` to `src/cli.rs`**

After the `TargetArgs` struct definition, add:

```rust
/// Output arguments for writing structured reports to file.
#[derive(Args, Clone, Debug, Default)]
pub struct OutputArgs {
    /// Write structured report to file (.json or .html).
    /// Omit path for auto-named file: ssync-{command}-{YYYYMMDD-HHmmss}.json
    /// Examples: --out  |  --out report.json  |  --out report.html
    #[arg(short = 'o', long, num_args = 0..=1, default_missing_value = "")]
    pub out: Option<String>,
}
```

- [ ] **Step 2: Add `#[command(flatten)] output: OutputArgs` to each applicable command**

For `Checkout` (add after `since`):
```rust
#[command(flatten)]
output: OutputArgs,
/// Print help
#[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
help: Option<bool>,
```

For `Check`:
```rust
/// Collect system snapshots from hosts and store in state DB
#[command(disable_help_flag = true)]
Check {
    #[command(flatten)]
    target: TargetArgs,
    #[command(flatten)]
    output: OutputArgs,
},
```

For `Sync` (add after `source`):
```rust
#[command(flatten)]
output: OutputArgs,
```

For `Run` (add after `yes`):
```rust
#[command(flatten)]
output: OutputArgs,
```

For `Exec` (add after `dry_run`):
```rust
#[command(flatten)]
output: OutputArgs,
```

- [ ] **Step 3: Update `main.rs` to destructure `output` from each command**

For `Check`:
```rust
Commands::Check { target, output } => {
    let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
    commands::check::run(&ctx, &output).await
}
```

For `Checkout`:
```rust
Commands::Checkout {
    target,
    history,
    since,
    output,
    ..
} => {
    let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
    commands::checkout::run(&ctx, history, since, &output).await
}
```

For `Sync`:
```rust
Commands::Sync {
    target,
    dry_run,
    files,
    no_push_missing,
    source,
    output,
} => {
    let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
    commands::sync::run(&ctx, dry_run, &files, no_push_missing, source.as_deref(), &output).await
}
```

For `Run`:
```rust
Commands::Run {
    target,
    command,
    sudo,
    yes,
    output,
} => {
    let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
    commands::run::run(&ctx, &command, sudo, yes, &output).await
}
```

For `Exec`:
```rust
Commands::Exec {
    target,
    script,
    sudo,
    yes,
    keep,
    dry_run,
    output,
} => {
    let ctx = commands::Context::new(cli.verbose, &target, cfg).await?;
    commands::exec::run(&ctx, &script, sudo, yes, keep, dry_run, &output).await
}
```

- [ ] **Step 4: Update command handler signatures (stubs — full impl in later tasks)**

In each command file, update the `run()` function signature to accept `output: &crate::cli::OutputArgs` and add `let _ = output;` as a stub:

`src/commands/check.rs`:
```rust
pub async fn run(ctx: &Context, output: &crate::cli::OutputArgs) -> Result<()> {
    let _ = output; // wired in Task 8
    // ... existing body unchanged
```

`src/commands/checkout.rs`:
```rust
pub async fn run(ctx: &Context, history: bool, since: Option<String>, output: &crate::cli::OutputArgs) -> Result<()> {
    let _ = output; // wired in Task 10
    // ... existing table body
```

`src/commands/sync.rs`:
```rust
pub async fn run(ctx: &Context, dry_run: bool, files: &[String], no_push_missing: bool, cli_source: Option<&str>, output: &crate::cli::OutputArgs) -> Result<()> {
    let _ = output; // wired in Task 9
    // ... existing body unchanged
```

`src/commands/run.rs`:
```rust
pub async fn run(ctx: &Context, command: &str, sudo: bool, _yes: bool, output: &crate::cli::OutputArgs) -> Result<()> {
    let _ = output; // wired in Task 6
    // ... existing body unchanged
```

`src/commands/exec.rs`:
```rust
pub async fn run(ctx: &Context, script: &str, sudo: bool, _yes: bool, keep: bool, dry_run: bool, output: &crate::cli::OutputArgs) -> Result<()> {
    let _ = output; // wired in Task 7
    // ... existing body unchanged
```

- [ ] **Step 5: Compile check**

```bash
cargo check 2>&1 | head -30
```
Expected: 0 errors (stubs compile through `let _ = output`).

- [ ] **Step 6: Run all tests**

```bash
cargo test 2>&1 | tail -10
```
Expected: all existing tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/main.rs src/commands/check.rs src/commands/checkout.rs src/commands/sync.rs src/commands/run.rs src/commands/exec.rs
git commit -m "feat: add OutputArgs to all remote-operation commands (stubs)

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 5 — Create `src/output/report.rs`

**Files:**
- Create: `src/output/report.rs`
- Modify: `src/output/mod.rs`

- [ ] **Step 1: Write tests first (TDD)**

At the bottom of the new `src/output/report.rs` file, write the tests before the implementation:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_report(command: &str) -> OperationReport {
        OperationReport {
            executed_at: "2026-04-29T10:00:00Z".to_string(),
            command: command.to_string(),
            filter: FilterInfo {
                mode: "hosts".to_string(),
                values: Some(vec!["host1".to_string()]),
            },
            task: serde_json::json!({ "command": "echo hi" }),
            targets: vec!["host1".to_string()],
            results: vec![HostResult {
                host: "host1".to_string(),
                status: "success".to_string(),
                duration_ms: Some(42),
                output: serde_json::json!({ "stdout": "hi\n", "stderr": "" }),
            }],
            summary: ReportSummary {
                total: 1,
                success: 1,
                failed: 0,
                skipped: 0,
            },
        }
    }

    #[test]
    fn test_write_report_json_explicit_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.json").to_str().unwrap().to_string();
        let report = sample_report("run");
        write_report(&report, &path, "run").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["command"], "run");
        assert_eq!(v["summary"]["total"], 1);
        assert_eq!(v["filter"]["mode"], "hosts");
    }

    #[test]
    fn test_write_report_html_explicit_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.html").to_str().unwrap().to_string();
        let report = sample_report("run");
        write_report(&report, &path, "run").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
        assert!(content.contains("host1"));
        assert!(content.contains("success"));
    }

    #[test]
    fn test_write_report_auto_filename() {
        let dir = TempDir::new().unwrap();
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let report = sample_report("check");
        write_report(&report, "", "check").unwrap();
        // A file matching ssync-check-*.json should exist
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let name = entries[0].file_name().to_string_lossy().to_string();
        assert!(name.starts_with("ssync-check-"));
        assert!(name.ends_with(".json"));
        std::env::set_current_dir(orig).unwrap();
    }

    #[test]
    fn test_write_report_unsupported_extension() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv").to_str().unwrap().to_string();
        let report = sample_report("run");
        let result = write_report(&report, &path, "run");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unsupported"));
    }

    #[test]
    fn test_filter_info_all_has_no_values() {
        let fi = FilterInfo { mode: "all".to_string(), values: None };
        let json = serde_json::to_string(&fi).unwrap();
        assert!(!json.contains("values"));
    }
}
```

- [ ] **Step 2: Run tests — verify they fail**

```bash
# File doesn't exist yet, so this just shows compile error
cargo test output::report 2>&1 | head -20
```
Expected: compile error `module 'report' not found` — confirms TDD starting point.

- [ ] **Step 3: Create `src/output/report.rs` with full implementation**

```rust
use anyhow::{bail, Result};
use serde::Serialize;

use crate::commands::TargetMode;

#[derive(Serialize)]
pub struct OperationReport {
    pub executed_at: String,
    pub command: String,
    pub filter: FilterInfo,
    pub task: serde_json::Value,
    pub targets: Vec<String>,
    pub results: Vec<HostResult>,
    pub summary: ReportSummary,
}

#[derive(Serialize)]
pub struct FilterInfo {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

impl FilterInfo {
    pub fn from_mode(mode: &TargetMode) -> Self {
        match mode {
            TargetMode::All => FilterInfo {
                mode: "all".to_string(),
                values: None,
            },
            TargetMode::Groups(g) => FilterInfo {
                mode: "groups".to_string(),
                values: Some(g.clone()),
            },
            TargetMode::Hosts(h) => FilterInfo {
                mode: "hosts".to_string(),
                values: Some(h.clone()),
            },
            TargetMode::Shell(s) => FilterInfo {
                mode: "shell".to_string(),
                values: Some(s.iter().map(|sh| sh.to_string()).collect()),
            },
        }
    }
}

#[derive(Serialize)]
pub struct HostResult {
    pub host: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub output: serde_json::Value,
}

#[derive(Serialize, Default)]
pub struct ReportSummary {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Write `report` to a file. Path semantics:
/// - `""` → auto-generate `ssync-{command}-{YYYYMMDD-HHmmss}.json` in CWD
/// - `*.json` → JSON
/// - `*.html` → HTML
/// - other extension → error
pub fn write_report(report: &OperationReport, out: &str, command: &str) -> Result<()> {
    use std::path::Path;

    let path = if out.is_empty() {
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
        format!("ssync-{}-{}.json", command, ts)
    } else {
        out.to_string()
    };

    let ext = Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("json")
        .to_lowercase();

    match ext.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(report)?;
            std::fs::write(&path, json)?;
        }
        "html" => {
            let html = render_html_report(report);
            std::fs::write(&path, html)?;
        }
        other => {
            bail!(
                "Unsupported output format '.{}'. Use .json or .html",
                other
            );
        }
    }

    println!("Report written to {}", path);
    Ok(())
}

fn render_html_report(report: &OperationReport) -> String {
    let filter_str = match &report.filter.values {
        Some(vals) => format!("{}: {}", report.filter.mode, vals.join(", ")),
        None => report.filter.mode.clone(),
    };

    let rows = report
        .results
        .iter()
        .map(|r| {
            let duration = r
                .duration_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "—".to_string());
            let status_class = match r.status.as_str() {
                "success" => "status-ok",
                "error" => "status-err",
                _ => "status-skip",
            };
            let output_html = render_output_html(&r.output);
            format!(
                r#"<tr>
  <td>{host}</td>
  <td><span class="{cls}">{status}</span></td>
  <td>{duration}</td>
  <td class="output-cell">{output}</td>
</tr>"#,
                host = html_escape(&r.host),
                cls = status_class,
                status = html_escape(&r.status),
                duration = duration,
                output = output_html,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ssync {command} report</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 2rem; background: #f8f9fa; color: #212529; }}
  h1 {{ font-size: 1.4rem; margin-bottom: 0.25rem; }}
  .meta {{ color: #6c757d; font-size: 0.9rem; margin-bottom: 1.5rem; }}
  .summary {{ display: flex; gap: 1rem; margin-bottom: 1.5rem; }}
  .badge {{ padding: 0.35rem 0.75rem; border-radius: 4px; font-weight: 600; font-size: 0.85rem; }}
  .badge-total {{ background: #e9ecef; }}
  .badge-ok {{ background: #d4edda; color: #155724; }}
  .badge-err {{ background: #f8d7da; color: #721c24; }}
  .badge-skip {{ background: #fff3cd; color: #856404; }}
  table {{ width: 100%; border-collapse: collapse; background: white; border-radius: 8px; overflow: hidden; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
  th {{ background: #343a40; color: white; text-align: left; padding: 0.6rem 1rem; font-size: 0.85rem; }}
  td {{ padding: 0.6rem 1rem; border-bottom: 1px solid #dee2e6; vertical-align: top; font-size: 0.85rem; }}
  tr:last-child td {{ border-bottom: none; }}
  .status-ok {{ color: #28a745; font-weight: 600; }}
  .status-err {{ color: #dc3545; font-weight: 600; }}
  .status-skip {{ color: #ffc107; font-weight: 600; }}
  .output-cell {{ font-family: monospace; white-space: pre-wrap; word-break: break-all; max-width: 600px; }}
  details summary {{ cursor: pointer; color: #0066cc; }}
</style>
</head>
<body>
<h1>ssync {command} report</h1>
<div class="meta">
  <strong>Executed:</strong> {executed_at} &nbsp;|&nbsp;
  <strong>Filter:</strong> {filter}
</div>
<div class="summary">
  <span class="badge badge-total">Total: {total}</span>
  <span class="badge badge-ok">Success: {success}</span>
  <span class="badge badge-err">Failed: {failed}</span>
  <span class="badge badge-skip">Skipped: {skipped}</span>
</div>
<table>
<thead><tr><th>Host</th><th>Status</th><th>Duration</th><th>Output</th></tr></thead>
<tbody>
{rows}
</tbody>
</table>
</body>
</html>"#,
        command = html_escape(&report.command),
        executed_at = html_escape(&report.executed_at),
        filter = html_escape(&filter_str),
        total = report.summary.total,
        success = report.summary.success,
        failed = report.summary.failed,
        skipped = report.summary.skipped,
        rows = rows,
    )
}

fn render_output_html(output: &serde_json::Value) -> String {
    // check command: has "metrics" and "probe_outputs"
    if let Some(metrics) = output.get("metrics") {
        let mut html = String::from("<strong>Metrics:</strong><br>");
        if let Some(obj) = metrics.as_object() {
            for (k, v) in obj {
                html.push_str(&format!(
                    "{}: {}<br>",
                    html_escape(k),
                    html_escape(&v.to_string())
                ));
            }
        }
        if let Some(probes) = output.get("probe_outputs") {
            html.push_str("<details><summary>Raw probe output</summary><pre>");
            html.push_str(&html_escape(&serde_json::to_string_pretty(probes).unwrap_or_default()));
            html.push_str("</pre></details>");
        }
        return html;
    }

    // sync command: has "files_synced" and "files_skipped"
    if let Some(synced) = output.get("files_synced") {
        let mut html = String::new();
        if let Some(arr) = synced.as_array() {
            if !arr.is_empty() {
                html.push_str("<strong>Synced:</strong><br>");
                for f in arr {
                    html.push_str(&format!("  {}<br>", html_escape(&f.to_string())));
                }
            }
        }
        if let Some(skipped) = output.get("files_skipped") {
            if let Some(arr) = skipped.as_array() {
                if !arr.is_empty() {
                    html.push_str("<strong>Skipped (in-sync):</strong><br>");
                    for f in arr {
                        html.push_str(&format!("  {}<br>", html_escape(&f.to_string())));
                    }
                }
            }
        }
        if let Some(stderr) = output.get("stderr") {
            let s = stderr.as_str().unwrap_or("");
            if !s.is_empty() {
                html.push_str(&format!("<strong>stderr:</strong><pre>{}</pre>", html_escape(s)));
            }
        }
        return html;
    }

    // checkout: has "snapshot"
    if let Some(snap) = output.get("snapshot") {
        let online = output.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
        let collected_at = output
            .get("collected_at")
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        return format!(
            "Online: {} | Collected: {}<details><summary>Snapshot</summary><pre>{}</pre></details>",
            if online { "✓" } else { "✗" },
            html_escape(collected_at),
            html_escape(&serde_json::to_string_pretty(snap).unwrap_or_default()),
        );
    }

    // run/exec: stdout/stderr
    let stdout = output
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stderr = output
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut html = String::new();
    if !stdout.is_empty() {
        html.push_str(&format!("<strong>stdout:</strong><pre>{}</pre>", html_escape(stdout)));
    }
    if !stderr.is_empty() {
        html.push_str(&format!("<strong>stderr:</strong><pre>{}</pre>", html_escape(stderr)));
    }
    html
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 go here)
}
```

- [ ] **Step 4: Export from `src/output/mod.rs`**

```rust
pub mod printer;
pub mod progress;
pub mod report;
pub mod summary;
```

- [ ] **Step 5: Run the tests**

```bash
cargo test output::report::tests 2>&1
```
Expected: all 5 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/output/report.rs src/output/mod.rs
git commit -m "feat: add output::report module with OperationReport, write_report, HTML rendering

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 6 — Wire `--out` into `run` command

**Files:**
- Modify: `src/commands/run.rs`

- [ ] **Step 1: Add per-host result accumulation to `run.rs`**

Replace `let _ = output;` stub and add report accumulation. The full updated `run()` function:

```rust
use std::time::Instant;

use anyhow::Result;

use crate::cli::OutputArgs;
use crate::host::pool::SshPool;
use crate::host::shell;
use crate::output::printer;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
use crate::output::summary::Summary;

use super::Context;

pub async fn run(
    ctx: &Context,
    command: &str,
    sudo: bool,
    _yes: bool,
    output: &OutputArgs,
) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;

    let (pool, _connected) = SshPool::setup(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
    )
    .await?;

    let mut summary = Summary::default();
    let mut report_results: Vec<HostResult> = Vec::new();

    for (name, err) in pool.failed_hosts() {
        printer::print_host_line(&name, "error", &format!("unreachable — {}", err));
        summary.add_failure(&name, &err);
        report_results.push(HostResult {
            host: name.clone(),
            status: "error".to_string(),
            duration_ms: None,
            output: serde_json::json!({ "stdout": "", "stderr": format!("unreachable: {}", err) }),
        });
    }

    let reachable = pool.filter_reachable(&hosts);

    let mut handles = Vec::new();
    for host in &reachable {
        let host = (*host).clone();
        let cmd = if sudo {
            shell::sudo_wrap(host.shell, command)
        } else {
            command.to_string()
        };
        let timeout = ctx.timeout;
        let sessions = pool.session_pool.clone();
        let global_sem = pool.limiter.global_semaphore();

        handles.push(tokio::spawn(async move {
            let _permit = global_sem.acquire_owned().await.unwrap();
            let start = Instant::now();
            let result = sessions.exec(&host.ssh_host, &cmd, timeout).await;
            let elapsed = start.elapsed();
            (host, result, elapsed)
        }));
    }

    for handle in handles {
        let (host, result, elapsed) = handle.await?;
        let now = chrono::Utc::now().timestamp();
        let duration_ms = elapsed.as_millis() as u64;

        match result {
            Ok(exec_output) => {
                if exec_output.success {
                    for line in exec_output.stdout.lines() {
                        printer::print_host_line(&host.name, "ok", line);
                    }
                    summary.add_success();
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "success".to_string(),
                        duration_ms: Some(duration_ms),
                        output: serde_json::json!({
                            "stdout": exec_output.stdout,
                            "stderr": exec_output.stderr,
                        }),
                    });
                } else {
                    let msg = exec_output.stderr.trim().to_string();
                    printer::print_host_line(&host.name, "error", &msg);
                    summary.add_failure(&host.name, &msg);
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "error".to_string(),
                        duration_ms: Some(duration_ms),
                        output: serde_json::json!({
                            "stdout": exec_output.stdout,
                            "stderr": exec_output.stderr,
                        }),
                    });
                }

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        now,
                        host.name,
                        command,
                        if exec_output.success { "ok" } else { "error" },
                        elapsed.as_millis() as i64,
                        if exec_output.success { None::<String> } else { Some(exec_output.stderr.trim().to_string()) },
                    ],
                )?;
            }
            Err(e) => {
                printer::print_host_line(&host.name, "error", &e.to_string());
                summary.add_failure(&host.name, &e.to_string());
                report_results.push(HostResult {
                    host: host.name.clone(),
                    status: "error".to_string(),
                    duration_ms: Some(duration_ms),
                    output: serde_json::json!({ "stdout": "", "stderr": e.to_string() }),
                });

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, 'error', ?4, ?5)",
                    rusqlite::params![
                        now, host.name, command,
                        elapsed.as_millis() as i64,
                        e.to_string(),
                    ],
                )?;
            }
        }
    }

    pool.shutdown().await;
    summary.print();

    if let Some(out) = &output.out {
        let rep_summary = ReportSummary {
            total: report_results.len(),
            success: report_results.iter().filter(|r| r.status == "success").count(),
            failed: report_results.iter().filter(|r| r.status == "error").count(),
            skipped: 0,
        };
        let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let report = OperationReport {
            executed_at: chrono::Utc::now().to_rfc3339(),
            command: "run".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({ "command": command }),
            targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(&report, out, "run")?;
    }

    Ok(())
}
```

- [ ] **Step 2: Compile check**

```bash
cargo check 2>&1 | head -20
```
Expected: 0 errors.

- [ ] **Step 3: Commit**

```bash
git add src/commands/run.rs
git commit -m "feat: wire --out report output into run command

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 7 — Wire `--out` into `exec` command

**Files:**
- Modify: `src/commands/exec.rs`

- [ ] **Step 1: Add accumulation and wire report**

The exec command has an additional "skipped" status when shell is incompatible. Update `run()` in `src/commands/exec.rs`:

Add these imports:
```rust
use crate::cli::OutputArgs;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
```

Change signature:
```rust
pub async fn run(
    ctx: &Context,
    script: &str,
    sudo: bool,
    _yes: bool,
    keep: bool,
    dry_run: bool,
    output: &OutputArgs,
) -> Result<()> {
```

After `let mut summary = Summary::default();`, add:
```rust
let mut report_results: Vec<HostResult> = Vec::new();
```

In the unreachable hosts loop, add:
```rust
report_results.push(HostResult {
    host: name.clone(),
    status: "error".to_string(),
    duration_ms: None,
    output: serde_json::json!({ "stdout": "", "stderr": format!("unreachable: {}", err) }),
});
```

In the shell mismatch skip section (before `continue`):
```rust
report_results.push(HostResult {
    host: host.name.clone(),
    status: "skipped".to_string(),
    duration_ms: None,
    output: serde_json::json!({
        "stdout": "",
        "stderr": format!("shell mismatch: need {}, have {}", required, host.shell),
    }),
});
```

After `let (host, result, elapsed) = handle.await?;`, update the `Ok(output)` arm:
```rust
Ok(exec_stdout) => {
    for line in exec_stdout.lines() {
        printer::print_host_line(&host.name, "ok", line);
    }
    summary.add_success();
    report_results.push(HostResult {
        host: host.name.clone(),
        status: "success".to_string(),
        duration_ms: Some(elapsed.as_millis() as u64),
        output: serde_json::json!({ "stdout": exec_stdout, "stderr": "" }),
    });
    // existing DB insert unchanged ...
}
Err(e) => {
    printer::print_host_line(&host.name, "error", &e.to_string());
    summary.add_failure(&host.name, &e.to_string());
    report_results.push(HostResult {
        host: host.name.clone(),
        status: "error".to_string(),
        duration_ms: Some(elapsed.as_millis() as u64),
        output: serde_json::json!({ "stdout": "", "stderr": e.to_string() }),
    });
    // existing DB insert unchanged ...
}
```

After `summary.print();`, add the report write block:
```rust
if let Some(out) = &output.out {
    let rep_summary = ReportSummary {
        total: report_results.len(),
        success: report_results.iter().filter(|r| r.status == "success").count(),
        failed: report_results.iter().filter(|r| r.status == "error").count(),
        skipped: report_results.iter().filter(|r| r.status == "skipped").count(),
    };
    let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
    let report = OperationReport {
        executed_at: chrono::Utc::now().to_rfc3339(),
        command: "exec".to_string(),
        filter: FilterInfo::from_mode(&ctx.mode),
        task: serde_json::json!({ "script": script }),
        targets,
        results: report_results,
        summary: rep_summary,
    };
    crate::output::report::write_report(&report, out, "exec")?;
}
```

Note: The `dry_run` path returns early — no report is written for dry-run (consistent with no actual operations being performed).

- [ ] **Step 2: Compile check**

```bash
cargo check 2>&1 | head -20
```
Expected: 0 errors.

- [ ] **Step 3: Commit**

```bash
git add src/commands/exec.rs
git commit -m "feat: wire --out report output into exec command

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 8 — Extend `CollectionResult` + wire `--out` into `check`

**Files:**
- Modify: `src/metrics/collector.rs`
- Modify: `src/commands/check.rs`

- [ ] **Step 1: Add raw batch fields to `CollectionResult`**

In `src/metrics/collector.rs`, update the struct:

```rust
pub struct CollectionResult {
    pub data: Value,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    /// Raw stdout from the metrics batch SSH call (empty if no metrics collected).
    pub metrics_raw_stdout: String,
    /// Raw stderr from the metrics batch SSH call.
    pub metrics_raw_stderr: String,
}
```

Update the `Ok(CollectionResult { ... })` return at the bottom of `collect_pooled` to include the new fields. Add `metrics_stdout` and `metrics_stderr` accumulators:

After `let mut errors: Vec<String> = Vec::new();`, add:
```rust
let mut metrics_raw_stdout = String::new();
let mut metrics_raw_stderr = String::new();
```

In the `Ok(output) if output.success` arm:
```rust
metrics_raw_stdout = output.stdout.clone();
metrics_raw_stderr = output.stderr.clone();
```
In the `Ok(output)` partial arm:
```rust
metrics_raw_stdout = output.stdout.clone();
metrics_raw_stderr = output.stderr.clone();
```

Return:
```rust
Ok(CollectionResult {
    data: Value::Object(result),
    succeeded,
    failed,
    errors,
    metrics_raw_stdout,
    metrics_raw_stderr,
})
```

Also update the test `CollectionResult` construction in `collector.rs` tests to include the new fields:
```rust
let cr = CollectionResult {
    data: serde_json::json!({"schema_version": 1}),
    succeeded: 0,
    failed: 0,
    errors: vec![],
    metrics_raw_stdout: String::new(),
    metrics_raw_stderr: String::new(),
};
```

- [ ] **Step 2: Run existing collector tests**

```bash
cargo test metrics::collector 2>&1
```
Expected: all pass.

- [ ] **Step 3: Wire report into `check.rs`**

Add imports in `src/commands/check.rs`:
```rust
use crate::cli::OutputArgs;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
```

Change signature:
```rust
pub async fn run(ctx: &Context, output: &OutputArgs) -> Result<()> {
```

After `let mut summary = Summary::default();`, add:
```rust
let mut report_results: Vec<HostResult> = Vec::new();
let executed_at = chrono::Utc::now().to_rfc3339();
```

In the failed-hosts loop (before the `for handle in handles` loop), add:
```rust
for (name, err) in pool.failed_hosts() {
    // existing printer and summary calls ...
    report_results.push(HostResult {
        host: name.clone(),
        status: "error".to_string(),
        duration_ms: None,
        output: serde_json::json!({
            "metrics": {},
            "probe_outputs": {},
            "error": format!("unreachable: {}", err),
        }),
    });
}
```

In the `Ok(cr)` arm, after existing DB/printer code, add:
```rust
let probe_outputs = serde_json::json!({
    "metrics_batch": {
        "stdout": cr.metrics_raw_stdout,
        "stderr": cr.metrics_raw_stderr,
    }
});
let status = if cr.succeeded == 0 { "error" } else { "success" };
report_results.push(HostResult {
    host: host.name.clone(),
    status: status.to_string(),
    duration_ms: Some(elapsed.as_millis() as u64),
    output: serde_json::json!({
        "metrics": cr.data,
        "probe_outputs": probe_outputs,
    }),
});
```

In the `Err(e)` arm, add:
```rust
report_results.push(HostResult {
    host: host.name.clone(),
    status: "error".to_string(),
    duration_ms: Some(elapsed.as_millis() as u64),
    output: serde_json::json!({
        "metrics": {},
        "probe_outputs": {},
        "error": e.to_string(),
    }),
});
```

After `retention::cleanup(...)`, add report write:
```rust
if let Some(out) = &output.out {
    let enabled_metrics: Vec<String> = host_configs
        .values()
        .flat_map(|(enabled, _)| enabled.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let rep_summary = ReportSummary {
        total: report_results.len(),
        success: report_results.iter().filter(|r| r.status == "success").count(),
        failed: report_results.iter().filter(|r| r.status == "error").count(),
        skipped: 0,
    };
    let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
    let report = OperationReport {
        executed_at,
        command: "check".to_string(),
        filter: FilterInfo::from_mode(&ctx.mode),
        task: serde_json::json!({ "metrics": enabled_metrics }),
        targets,
        results: report_results,
        summary: rep_summary,
    };
    crate::output::report::write_report(&report, out, "check")?;
}
```

- [ ] **Step 4: Compile and test**

```bash
cargo check 2>&1 | head -20
cargo test 2>&1 | tail -10
```
Expected: 0 errors, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/metrics/collector.rs src/commands/check.rs
git commit -m "feat: wire --out report into check command; add raw probe output to CollectionResult

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 9 — Wire `--out` into `sync` command

**Files:**
- Modify: `src/commands/sync.rs`

- [ ] **Step 1: Add per-host file tracking accumulators**

Add imports:
```rust
use crate::cli::OutputArgs;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
```

The `run()` function already has `let _ = output;` stub. Remove it and add:

After `let mut summary = SyncSummary::default();`, add:
```rust
let executed_at = chrono::Utc::now().to_rfc3339();
// Per-host (files_synced, files_skipped) accumulator
let mut host_file_map: std::collections::HashMap<String, (Vec<String>, Vec<String>)> =
    std::collections::HashMap::new();
// Initialize map entry for all hosts (so hosts with zero files still appear)
```

Initialize after `hosts` is resolved:
```rust
for host in &hosts {
    host_file_map.entry(host.name.clone()).or_insert((Vec::new(), Vec::new()));
}
```

- [ ] **Step 2: Accumulate per-host synced/skipped lists**

Inside the non-recursive Step 6 distribution loop, after `Ok((succeeded, failed_uploads)) =>`, before the existing DB insert, add:
```rust
// Track synced files per host
for target in &succeeded {
    host_file_map
        .entry(target.clone())
        .or_default()
        .0
        .push(decision.path.clone());
}
// Track skipped (already-in-sync) files per host
for h in &decision.synced_hosts {
    host_file_map
        .entry(h.clone())
        .or_default()
        .1
        .push(decision.path.clone());
}
```

After the `if !decision.synced_hosts.is_empty()` printer call (dry_run path), add:
```rust
// Track synced_hosts as skipped for report
for h in &decision.synced_hosts {
    host_file_map
        .entry(h.clone())
        .or_default()
        .1
        .push(decision.path.clone());
}
```

- [ ] **Step 3: Write report after all sync is done**

After `pool.shutdown().await;` and `summary.print();` (near the end of `run()`), add:

```rust
if let Some(out) = &output.out {
    let sync_paths: Vec<String> = {
        let mut all: Vec<String> = hosts
            .iter()
            .flat_map(|_| {
                host_file_map
                    .values()
                    .flat_map(|(s, k)| s.iter().chain(k.iter()).cloned())
                    .collect::<Vec<_>>()
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        all
    };

    let report_results: Vec<HostResult> = hosts
        .iter()
        .map(|h| {
            let (synced, skipped) = host_file_map
                .get(&h.name)
                .cloned()
                .unwrap_or_default();
            HostResult {
                host: h.name.clone(),
                status: "success".to_string(),
                duration_ms: None,
                output: serde_json::json!({
                    "files_synced": synced,
                    "files_skipped": skipped,
                    "stderr": "",
                }),
            }
        })
        .collect();

    let rep_summary = ReportSummary {
        total: report_results.len(),
        success: report_results.len(),
        failed: 0,
        skipped: 0,
    };
    let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();

    // Collect configured sync paths for the task field
    let configured_paths: Vec<String> = ctx
        .resolve_syncs()
        .iter()
        .flat_map(|e| e.paths.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let report = OperationReport {
        executed_at,
        command: "sync".to_string(),
        filter: FilterInfo::from_mode(&ctx.mode),
        task: serde_json::json!({ "paths": configured_paths }),
        targets,
        results: report_results,
        summary: rep_summary,
    };
    crate::output::report::write_report(&report, out, "sync")?;
}
```

- [ ] **Step 4: Compile and test**

```bash
cargo check 2>&1 | head -20
cargo test 2>&1 | tail -10
```
Expected: 0 errors, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/commands/sync.rs
git commit -m "feat: wire --out report into sync command with per-host file lists

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 10 — Wire `--out` into `checkout` command + remove old JSON/HTML functions

**Files:**
- Modify: `src/commands/checkout.rs`

- [ ] **Step 1: Remove `render_html` and `build_json_report` old functions**

Delete `render_html()` (lines ~513–543 in checkout.rs) and `build_json_report()` (lines ~412–483) entirely. Also remove the `parse_since` function if it's only used by `build_json_report`. (Keep `parse_since` if it's also used by history-mode — yes, it's needed for the `--history/--since` feature, keep it.)

Actually `parse_since` is used by `build_json_report` which is being deleted. If `--history` + `--since` for the table view also needs `parse_since`, we must keep it. Looking at `fetch_latest_snapshots` — it only fetches the latest, no `since`. The `--history` feature in the original code path used `build_json_report`. Since we're removing `build_json_report`, history/since for the table view still needs to work. 

Keep `parse_since` and use it in `fetch_snapshots_for_report` below.

Remove only `build_json_report` and `render_html`.

- [ ] **Step 2: Add the `--out` implementation to `run()`**

The full `run()` function for checkout:

```rust
use crate::cli::OutputArgs;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};

pub async fn run(
    ctx: &Context,
    history: bool,
    since: Option<String>,
    output: &OutputArgs,
) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;
    let host_names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();
    let columns = DisplayColumns::from_context(ctx);

    let snapshots = fetch_latest_snapshots(ctx, &host_names)?;
    print_table_report(&snapshots, &columns);

    if let Some(out) = &output.out {
        let executed_at = chrono::Utc::now().to_rfc3339();

        let report_results: Vec<HostResult> = snapshots
            .iter()
            .map(|snap| {
                let collected_at_str = if snap.collected_at > 0 {
                    chrono::DateTime::from_timestamp(snap.collected_at, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_else(|| snap.collected_at.to_string())
                } else {
                    "never".to_string()
                };
                HostResult {
                    host: snap.host.clone(),
                    status: if snap.online { "success" } else { "error" }.to_string(),
                    duration_ms: None,
                    output: serde_json::json!({
                        "collected_at": collected_at_str,
                        "online": snap.online,
                        "snapshot": snap.data,
                    }),
                }
            })
            .collect();

        let rep_summary = ReportSummary {
            total: report_results.len(),
            success: report_results.iter().filter(|r| r.status == "success").count(),
            failed: report_results.iter().filter(|r| r.status == "error").count(),
            skipped: 0,
        };
        let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();

        let report = OperationReport {
            executed_at,
            command: "checkout".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({
                "history": history,
                "since": since,
            }),
            targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(&report, out, "checkout")?;
    }

    Ok(())
}
```

Remove the `use crate::cli::OutputFormat;` import (no longer needed).

- [ ] **Step 3: Compile and test**

```bash
cargo check 2>&1 | head -20
cargo test 2>&1 | tail -15
```
Expected: 0 errors, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkout.rs
git commit -m "feat: wire --out report into checkout command; remove old JSON/HTML format functions

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Task 11 — Final validation

- [ ] **Step 1: Run full test suite**

```bash
cargo test 2>&1
```
Expected: all tests pass (no regressions).

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets 2>&1 | grep -E "^error" | head -20
```
Expected: 0 errors (warnings acceptable).

- [ ] **Step 3: Check fmt**

```bash
cargo fmt --check 2>&1
```
Expected: no diff output (or run `cargo fmt` to fix).

- [ ] **Step 4: Build release**

```bash
cargo build --release 2>&1 | tail -5
```
Expected: `Compiling ssync ... Finished release [optimized]`

- [ ] **Step 5: Smoke test `--help`**

```bash
./target/release/ssync --help
./target/release/ssync run --help
./target/release/ssync checkout --help
```
Verify `--shell/-S` appears in global target options, `--out/-o` appears in run/checkout, no `--format` visible.

- [ ] **Step 6: Final commit**

```bash
git add -A
git commit -m "chore: final validation — CLI filter & output refactor complete

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

## Self-Review Notes

**Spec coverage check:**
- §1 Shell filter: ✅ Tasks 1–2 (ShellType ValueEnum, TargetMode::Shell, 4-way exclusive, error hints, filter_hosts)
- §2 Remove --format / TUI disable: ✅ Task 3
- §3a OutputArgs: ✅ Task 4
- §3b OperationReport struct + write_report: ✅ Task 5
- §3b run/exec output: ✅ Tasks 6–7
- §3b check output (metrics + probe_outputs): ✅ Task 8
- §3b sync output (files_synced/skipped as lists): ✅ Task 9
- §3b checkout output: ✅ Task 10
- §3c HTML report: ✅ Task 5 (`render_html_report`)
- Filter field in report envelope: ✅ `FilterInfo::from_mode()` in Task 5

**Type consistency check:**
- `OperationReport`, `FilterInfo`, `HostResult`, `ReportSummary` — defined in Task 5, used consistently in Tasks 6–10
- `FilterInfo::from_mode(&ctx.mode)` — used identically in Tasks 6–10
- `write_report(&report, out, "command_name")` — signature consistent across all use sites
- `CollectionResult.metrics_raw_stdout/stderr` — defined Task 8 step 1, used Task 8 step 3

**No placeholders confirmed:** all steps contain exact code.
