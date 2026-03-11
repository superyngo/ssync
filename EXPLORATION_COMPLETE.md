# SSYNC Codebase Exploration - Complete Report

## Documents Generated

This exploration has generated three comprehensive documents:

### 1. **CODEBASE_ANALYSIS.md** (652 lines)
**Comprehensive architectural analysis with all key insights**

Contents:
- Overview of the entire system
- Detailed breakdown of each config component (schema.rs, app.rs)
- Complete CLI definitions (cli.rs) with all subcommands
- Host filtering logic (filter.rs)
- All command implementations with full descriptions
- Main entry point routing
- Metrics collector logic
- Output summary formatting
- Key architectural patterns
- Database schema
- Code statistics

**Use this for**: Understanding overall architecture, planning new features, understanding how components interact

---

### 2. **FILE_CONTENTS_REFERENCE.md** (593 lines)
**Complete source code of critical files with inline documentation**

Contents:
- Full Rust source code for:
  - src/config/schema.rs (141 lines)
  - src/config/app.rs (120 lines)
  - src/cli.rs (204 lines)
  - src/host/filter.rs (89 lines)
- Summary references to other files with line counts

**Use this for**: Quick reference when editing files, understanding exact implementations, copying boilerplate patterns

---

### 3. **EXPLORATION_COMPLETE.md** (this file)
**Navigation guide and quick reference**

---

## Quick Reference: File Locations and Purposes

### Configuration System
- **src/config/schema.rs** - TOML data structures (AppConfig, HostEntry, SyncFile, etc.)
- **src/config/app.rs** - Config file I/O (load/save/path resolution)
- **src/config/ssh_config.rs** - ~/.ssh/config parsing

### CLI Layer
- **src/cli.rs** - Clap argument parsing (TargetArgs, all subcommands)

### Command Implementations
- **src/commands/mod.rs** - CommandContext, TargetMode, host resolution (178 lines)
- **src/commands/sync.rs** - 3-stage file sync pipeline (589 lines) **← Most complex**
- **src/commands/check.rs** - Metric collection coordination (150 lines)
- **src/commands/checkout.rs** - Report generation (510 lines)
- **src/commands/init.rs** - Host discovery from ~/.ssh/config (125 lines)
- **src/commands/run.rs** - Execute remote commands (90 lines)
- **src/commands/exec.rs** - Upload and execute scripts (218 lines)
- **src/commands/config.rs** - Edit config in $EDITOR (35 lines)
- **src/commands/log.rs** - View operation logs (114 lines)

### Core Infrastructure
- **src/main.rs** - Application entry point and command dispatch (96 lines)
- **src/host/filter.rs** - Host filtering logic (89 lines)
- **src/host/executor.rs** - SSH execution and file transfer
- **src/host/shell.rs** - Shell type detection and commands

### Metrics System
- **src/metrics/collector.rs** - Coordinate metric collection (100 lines)
- **src/metrics/probes/** - Shell-specific metric commands
- **src/metrics/parser.rs** - Parse metric outputs

### State Management
- **src/state/db.rs** - SQLite database initialization
- **src/state/retention.rs** - Data cleanup

### Output
- **src/output/printer.rs** - Host-prefixed output formatting
- **src/output/summary.rs** - Summary statistics and formatting (43 lines)

---

## Key Concepts

### 1. Target Selection (Critical for Planning)
```
CLI arguments: --group/-g, --host/-h, --all/-a
↓ (only ONE allowed per command)
TargetMode enum: All | Hosts(Vec<String>) | Groups(Vec<String>)
↓
Context::resolve_hosts() → Vec<&HostEntry>
↓ (filters via host.groups membership)
Selected hosts for operation
```

### 2. File Sync Scoping
- **SyncFile with empty `groups` field**: Applies to `--all` or `--host` mode only
- **SyncFile with `groups: ["web", "db"]`**: Applies when using `--group web` or `--group db`
- Selection is intersection: (file's groups) AND (CLI-specified groups)

### 3. Configuration Structure
```toml
[settings]
default_timeout = 30
max_concurrency = 10
conflict_strategy = "newest"

[[host]]
name = "web1"
ssh_host = "web1.example.com"
shell = "sh"
groups = ["webservers", "prod"]

[[check.path]]
path = "/var/log"
label = "Logs"

[[sync.file]]
paths = ["/etc/nginx/nginx.conf"]
groups = ["webservers"]  # Only with --group webservers
recursive = false
```

### 4. Concurrency Model
- Default: Parallel execution with `Semaphore(max_concurrency)`
- `--serial` flag forces sequential execution (`Semaphore(1)`)
- All commands support parallel execution except explicitly serial features

### 5. Database Tables
- **check_snapshots**: System metrics history
- **host_last_seen**: Host reachability tracking
- **sync_state**: File sync tracking (group, host, path, mtime, hash, timestamp)
- **operation_log**: All operations (sync, run, exec, check) with status and duration

### 6. Platform Support
- **Sh** (Linux/macOS): Uses stat, sha256sum/shasum, chmod, mkdir -p
- **PowerShell** (Windows): Uses Get-Item, Get-FileHash, mkdir, Remove-Item
- **Cmd** (Windows): Limited support, primarily for .bat/.cmd scripts

---

## File Size and Complexity

| File | Size | Complexity | Key Insight |
|------|------|-----------|------------|
| sync.rs | 589 lines | ⭐⭐⭐⭐⭐ | 3-stage pipeline, shell-specific commands, conflict strategies |
| checkout.rs | 510 lines | ⭐⭐⭐⭐ | Multiple output formats, metric extraction, TUI/HTML/JSON |
| exec.rs | 218 lines | ⭐⭐⭐ | Script upload, compatibility checking, cleanup |
| cli.rs | 204 lines | ⭐⭐ | Clap argument definitions |
| commands/mod.rs | 178 lines | ⭐⭐⭐ | Context creation, target resolution, error hints |
| init.rs | 125 lines | ⭐⭐ | Host discovery, shell detection |
| schema.rs | 141 lines | ⭐ | Data structures |
| app.rs | 120 lines | ⭐ | Config I/O |
| log.rs | 114 lines | ⭐⭐ | DB queries with filtering |
| collector.rs | 100 lines | ⭐⭐ | Metric coordination |
| main.rs | 96 lines | ⭐ | Command dispatch |
| run.rs | 90 lines | ⭐ | Command execution |
| filter.rs | 89 lines | ⭐ | Filtering logic |
| config.rs | 35 lines | ⭐ | Editor invocation |
| summary.rs | 43 lines | ⭐ | Summary formatting |

---

## Understanding the Sync Pipeline (Most Complex Feature)

The sync command uses a **collect-decide-distribute** pattern:

### Stage 1: Collect Metadata
```
For each target host in parallel:
  - Execute shell-specific command to get: mtime, size, SHA256 hash
  - Track if file exists or is missing
  - Handle SSH failures separately from file-not-found
```

### Stage 2: Make Decisions
```
Based on collected metadata:
  - If all hosts have same hash → already in sync
  - If conflicts detected → apply conflict strategy (Newest/Skip)
  - Newest strategy: Pick file with latest mtime as source
  - Skip strategy: Abort if any conflicts, or push to missing hosts if all in sync
  - Output: source_host, target_hosts, synced_hosts, reason
```

### Stage 3: Distribute
```
For selected source/target pair:
  - Download file from source to local temp via SCP
  - For each target in parallel:
    - Create parent directories (mkdir -p with platform-specific syntax)
    - Upload file from temp to target
    - Track success/failure
  - Log results to sync_state table
```

---

## Common Patterns in Codebase

### 1. Semaphore-Based Concurrency
```rust
let semaphore = Arc::new(Semaphore::new(ctx.concurrency()));
for host in hosts {
    let sem = semaphore.clone();
    tokio::spawn(async move {
        let _permit = sem.acquire().await.unwrap();
        // Do work
    })
}
```

### 2. Shell-Specific Command Generation
```rust
let cmd = match host.shell {
    ShellType::Sh => "stat -c '%Y %s' /path",
    ShellType::PowerShell => "$i=Get-Item /path; [int64](($i.LastWriteTimeUtc-[datetime]\"1970-01-01\").TotalSeconds)",
    ShellType::Cmd => "for %i in (/path) do @echo %~ti",
};
```

### 3. Host Line Printing with Status
```rust
printer::print_host_line(&host.name, "ok", "message");
printer::print_host_line(&host.name, "error", "message");
printer::print_host_line(&host.name, "skip", "message");
// Outputs with color codes and emoji
```

### 4. Error Handling with Helpful Hints
```rust
if hosts.is_empty() {
    let mut hint = String::from("No hosts matched.");
    append_available_hints(&config, &mut hint);
    bail!("{}", hint);
}
```

### 5. Database Logging Pattern
```rust
ctx.db.execute(
    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    rusqlite::params![now, "sync", host_name, "sync_file", "ok", elapsed_ms],
)?;
```

---

## Testing Entry Points

To understand the codebase by reading tests:

1. **Host filtering**: `src/host/filter.rs` has 4 unit tests showing:
   - `--all` behavior
   - `--group` filtering
   - `--host` filtering
   - Combined filters

2. **Run other commands with `--dry-run`**:
   - `sync --dry-run` shows decisions without applying
   - `exec --dry-run` shows shell compatibility check

3. **Look at `--help` output**:
   - Shows all available arguments
   - Gives usage examples

---

## Next Steps for Development

1. **Understand target selection**: Read `commands/mod.rs` resolve_target_mode()
2. **Understand sync decisions**: Read `commands/sync.rs` make_decisions()
3. **Add a new metric**: Look at probes/{sh,powershell,cmd}.rs and parser.rs
4. **Add a new config option**: Update schema.rs, then check all places that read it
5. **Add a new subcommand**: Add to cli.rs Commands enum, implement in commands/*.rs

---

## Document Index

- **CODEBASE_ANALYSIS.md** ← Comprehensive architecture
- **FILE_CONTENTS_REFERENCE.md** ← Source code reference
- **EXPLORATION_COMPLETE.md** ← This navigation guide

Generated on: 2025 (Full codebase exploration)
Total analysis: 1,245+ lines across three documents
Code covered: 3,500+ lines of Rust source
