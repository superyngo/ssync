# CLI Filter & Output Refactor Design

**Date:** 2026-04-29  
**Status:** Approved

## Problem Statement

Three improvements to ssync CLI targeting usability and output flexibility:

1. **Shell-based host filtering** — add a 4th exclusive target mode (`--shell`) so users can operate on all `sh` or `powershell` hosts in one command, without creating explicit groups.
2. **Remove `--format` from `checkout`** — the flag causes confusion by coupling output destination with rendering format; TUI will be fully redesigned separately.
3. **Unified structured output (`--out`)** — expand file output to all remote operation commands, with a consistent report envelope and format auto-detected from file extension.

## Approach

Plan A: minimal-invasive, following existing patterns.

- Add `TargetMode::Shell(Vec<ShellType>)` as a 4th variant alongside existing `All`, `Hosts`, `Groups`.
- New `OutputArgs` struct flattened into each applicable command — keeps output concerns separate from target selection.
- Remove `OutputFormat` enum and `--format` flag entirely; `checkout` hardcodes table rendering.
- TUI feature disabled by default (Cargo feature flag, not deleted).

---

## § 1 — Shell Filter (`--shell / -S`)

### CLI Change (`src/cli.rs`)

Add to `TargetArgs`:

```rust
/// Filter by remote shell type (comma-separated: sh, powershell, cmd)
#[arg(short = 'S', long, value_delimiter = ',')]
pub shell: Vec<ShellType>,
```

`ShellType` gains `#[derive(clap::ValueEnum)]` (values: `sh`, `powershell`, `cmd`).

### TargetMode (`src/commands/mod.rs`)

```rust
pub enum TargetMode {
    All,
    Hosts(Vec<String>),
    Groups(Vec<String>),
    Shell(Vec<ShellType>),   // new
}
```

`resolve_target_mode()` updated: count includes `has_shell`; exactly one of 4 must be set.

```rust
let has_shell = !target.shell.is_empty();
let count = has_all as u8 + has_hosts as u8 + has_groups as u8 + has_shell as u8;
// error on count == 0 or count > 1
```

`resolve_hosts()` new branch:

```rust
TargetMode::Shell(shells) => self.config.host.iter()
    .filter(|h| shells.contains(&h.shell))
    .collect(),
```

**Config entry scoping for `--shell`:** treated identically to `--host` mode — entries where `enable_hosts: true` apply. This is consistent because both select a specific subset of hosts.

**Error hint when no hosts match:**

```
No hosts matched shell type: cmd
Available shells: sh (host1, host3), powershell (host2)
```

### Filter module (`src/host/filter.rs`)

`filter_hosts()` gains `shells: &[ShellType]` parameter and a retain clause. Existing callers pass `&[]`.

---

## § 2 — Remove `--format`, Disable TUI

### `src/cli.rs`

- Remove `OutputFormat` enum entirely.
- Remove `format: OutputFormat` and `out: Option<String>` from `Checkout` struct (both replaced by `OutputArgs` flatten in §3).

### `src/commands/checkout.rs`

- `run()` signature drops `format` parameter.
- Screen output: always `print_table_report()`.
- File output: delegated to `OutputArgs` handler (§3).
- Existing `build_json_report()` and `render_html()` functions are kept; called from `output::report` when `--out` is provided.

### `Cargo.toml`

```toml
[features]
tui = ["ratatui", "crossterm"]
default = []    # was: default = ["tui"]
```

TUI Rust code (`#[cfg(feature = "tui")]` blocks) is left in place — no deletion — to be redesigned separately.

---

## § 3 — Unified `--out` Flag

### 3a. `OutputArgs` (`src/cli.rs`)

```rust
#[derive(Args, Clone, Debug, Default)]
pub struct OutputArgs {
    /// Write structured report to file (.json or .html); omit path for auto-generated JSON.
    /// Examples:  --out            → ssync-{cmd}-{YYYYMMDD-HHmmss}.json
    ///             --out out.json   → explicit JSON file
    ///             --out out.html   → HTML report
    #[arg(short = 'o', long, num_args = 0..=1, default_missing_value = "")]
    pub out: Option<String>,
}
```

Semantics:

| Value | Meaning |
|-------|---------|
| `None` | `--out` not provided; no file output |
| `Some("")` | flag only; auto-generate `ssync-{command}-{YYYYMMDD-HHmmss}.json` in CWD |
| `Some("path.json")` | write JSON to path |
| `Some("path.html")` | write HTML to path |
| Other extension | error: unsupported format; suggest `.json` or `.html` |

Commands gaining `#[command(flatten)] output: OutputArgs`:  
`Checkout`, `Check`, `Sync`, `Run`, `Exec`

### 3b. Report Module (`src/output/report.rs`)

New module providing the shared report structures and write logic.

**Rust types:**

```rust
#[derive(Serialize)]
pub struct OperationReport {
    pub executed_at: String,          // ISO 8601 UTC
    pub command: String,
    pub filter: FilterInfo,
    pub task: serde_json::Value,      // command-specific (see below)
    pub targets: Vec<String>,         // resolved host names
    pub results: Vec<HostResult>,
    pub summary: ReportSummary,
}

#[derive(Serialize)]
pub struct FilterInfo {
    pub mode: String,                 // "all" | "groups" | "hosts" | "shell"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,  // omitted when mode == "all"
}

#[derive(Serialize)]
pub struct HostResult {
    pub host: String,
    pub status: String,               // "success" | "error" | "skipped"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub output: serde_json::Value,    // command-specific fields
}

#[derive(Serialize, Default)]
pub struct ReportSummary {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub skipped: usize,
}
```

**`task` field by command:**

| Command | `task` JSON |
|---------|-------------|
| `run`     | `{ "command": "<cmd string>" }` |
| `exec`    | `{ "script": "<path>" }` |
| `check`   | `{ "metrics": ["cpu","mem","disk",...] }` |
| `sync`    | `{ "paths": ["/etc/app.conf",...] }` |
| `checkout`| `{ "history": bool, "since": string|null }` |

**`results[].output` field by command:**

`run` / `exec`:
```jsonc
{ "stdout": "...", "stderr": "" }
```

`check` (parsed metrics + raw probe outputs):
```jsonc
{
  "metrics": { "cpu_pct": 12.3, "mem_pct": 45.6, "disk": { "/": 80.1 } },
  "probe_outputs": {
    "cpu": { "stdout": "cpu  12.3 ...", "stderr": "" },
    "mem": { "stdout": "...", "stderr": "" }
  }
}
```

`sync`:
```jsonc
{
  "files_synced": ["/etc/app.conf", "/etc/app.d/custom.conf"],
  "files_skipped": ["/etc/app.bak"],
  "stderr": ""
}
```

`checkout`:
```jsonc
{ "collected_at": "2026-04-28T12:00:00Z", "online": true, "snapshot": { ...metrics_json... } }
```

**`FilterInfo.values` for each mode:**

| Mode | `values` |
|------|---------|
| `all` | `null` (omitted) |
| `groups` | `["web","api"]` |
| `hosts` | `["host1","host2"]` |
| `shell` | `["sh"]` |

**Write logic (`write_report()`):**

```rust
pub fn write_report(report: &OperationReport, out: &str, command: &str) -> Result<()>
```

- `out == ""` → auto-generate filename `ssync-{command}-{YYYYMMDD-HHmmss}.json`, write to CWD
- Extension `.json` → `serde_json::to_string_pretty`
- Extension `.html` → `render_html_report()`
- Other → `bail!("Unsupported output format. Use .json or .html")`
- After write: print `Report written to <path>`

### 3c. HTML Report (`render_html_report()`)

Standalone HTML (no external CSS/JS), inline styles only. Structure:

- Page title: `ssync {command} report — {executed_at}`
- Header section: executed_at, command, filter mode/values, summary counts
- Per-host results table: host | status | duration | output (stdout/stderr or metrics)
- For `check`: two sub-sections per host — Metrics (key/value table) + Probe Outputs (collapsible `<details>` per probe)

---

## Files Changed

| File | Change |
|------|--------|
| `src/cli.rs` | Remove `OutputFormat`; add `OutputArgs`; add `--shell/-S` to `TargetArgs`; update `Checkout`; add `OutputArgs` flatten to `Check`, `Sync`, `Run`, `Exec` |
| `src/commands/mod.rs` | Add `TargetMode::Shell`; update `resolve_target_mode`, `resolve_hosts`, `filter_entries_by_mode`, hints |
| `src/host/filter.rs` | Add `shells` param to `filter_hosts`; update tests |
| `src/output/report.rs` | New: `OperationReport`, `HostResult`, `ReportSummary`, `FilterInfo`, `write_report`, `render_html_report` |
| `src/output/mod.rs` | Export `report` module |
| `src/commands/checkout.rs` | Remove `format` param; always table; wire `OutputArgs` |
| `src/commands/check.rs` | Add `OutputArgs` param; build and write `OperationReport` |
| `src/commands/sync.rs` | Add `OutputArgs` param; build and write `OperationReport` |
| `src/commands/run.rs` | Add `OutputArgs` param; build and write `OperationReport` |
| `src/commands/exec.rs` | Add `OutputArgs` param; build and write `OperationReport` |
| `src/main.rs` | Pass `OutputArgs` to each command dispatcher |
| `Cargo.toml` | Change `default = ["tui"]` → `default = []` |

## Non-Goals (this refactor)

- TUI redesign (future)
- Excel output (future, if needed)
- `log` command `--out` (log already has its own display; out-of-scope)
- `init` / `list` / `config` commands (no remote operation output to report)
