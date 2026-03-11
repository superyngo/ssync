# SSYNC Codebase Analysis Report

## Overview
SSYNC is an SSH-config-based cross-platform remote management tool built in Rust. It synchronizes files, runs commands, and collects metrics across multiple remote hosts using a collect-decide-distribute model.

---

## 1. CONFIG SCHEMA (`src/config/schema.rs`) - FULL FILE

The schema defines the TOML configuration structure with support for hosts, groups, file sync rules, and health checks.

### Key Types:

**`AppConfig`** (root config)
- `settings: Settings` - Global configuration
- `host: Vec<HostEntry>` - List of remote hosts
- `check: CheckConfig` - Health check configuration
- `sync: SyncConfig` - File sync configuration

**`Settings`**
- `default_timeout: u64` (default: 30s)
- `data_retention_days: u64` (default: 90)
- `conflict_strategy: ConflictStrategy` (Newest | Skip)
- `propagate_deletes: bool` (default: false)
- `max_concurrency: usize` (default: 10)
- `skipped_hosts: Vec<String>` - Hosts persisted from init to skip
- `state_dir: Option<PathBuf>` - Override state database location

**`HostEntry`**
- `name: String` - Host identifier
- `ssh_host: String` - SSH config host name
- `shell: ShellType` (Sh | PowerShell | Cmd)
- `groups: Vec<String>` - Group membership (e.g., ["webservers", "prod"])

**`ShellType`** Enum
- `Sh` - Unix/Linux/macOS shell
- `PowerShell` - Windows PowerShell
- `Cmd` - Windows CMD

**`CheckConfig`**
- `enabled: Vec<String>` - Metric types: "online", "system_info", "cpu_arch", "memory", "swap", "disk", "cpu_load", "network", "battery"
- `path: Vec<CheckPath>` - Custom paths to monitor

**`CheckPath`**
- `path: String` - Directory/file path
- `label: String` - Display label

**`SyncConfig`**
- `file: Vec<SyncFile>` - File sync rules

**`SyncFile`** - KEY FOR UNDERSTANDING SYNC SCOPING
- `paths: Vec<String>` - Files to sync (supports ~/ expansion)
- `groups: Vec<String>` - **Empty = applies to --all/--host scope. Non-empty = applies only to matching groups**
- `recursive: bool` - Recursive sync flag (default: false)
- `mode: Option<String>` - File permissions (e.g., "0644")
- `propagate_deletes: Option<bool>` - Sync deletions (overrides global setting)

---

## 2. CONFIG APP (`src/config/app.rs`) - FULL FILE

Handles config file I/O and path resolution.

### Functions:

**`config_dir() -> Result<PathBuf>`**
- Returns platform-specific config directory:
  - Linux/macOS: `~/.config/ssync`
  - Windows: `%APPDATA%\ssync`

**`config_path() -> Result<PathBuf>`**
- Returns `~/.config/ssync/config.toml`

**`resolve_path(custom_path: Option<&Path>) -> Result<PathBuf>`**
- Uses custom path if provided, else `config_path()`

**`load(custom_path) -> Result<Option<AppConfig>>`**
- Loads and parses TOML config
- Returns None if file doesn't exist
- Uses `toml::from_str()` for parsing

**`save(config: &AppConfig, custom_path) -> Result<()>`**
- Saves config to disk
- Creates parent directories if needed
- Uses `toml::to_string_pretty()`
- Injects helpful comments via `inject_config_comments()`

**`inject_config_comments(toml_str: &str) -> String`**
- Adds Chinese comments for `[check]` and `[sync]` sections
- Documents available metrics and sync examples
- Shows group-based vs global sync patterns

---

## 3. CLI DEFINITIONS (`src/cli.rs`) - FULL FILE

Clap-based CLI argument parsing.

### Main `Cli` struct:
- `--verbose, -v` - Enable debug logging
- `--config, -c <PATH>` - Custom config file path (global flag)

### `TargetArgs` - Common target selection (used by check, sync, run, exec, checkout):
- `--group, -g <GROUPS>` - Comma-separated group names
- `--host, -h <HOSTS>` - Comma-separated host names
- `--all, -a` - Select all hosts
- `--serial` - Execute sequentially (default: parallel)
- `--timeout <SECS>` - Override config timeout
- `-H, --help` - Show help

**Constraint**: Only one of `--group`, `--host`, `--all` can be used at a time.

### Subcommands:

**`init`**
- `--update` - Re-detect shell types for existing hosts
- `--dry-run` - Show what would import without writing
- `--skip <HOSTS>` - Skip hosts (comma-separated, persisted)

**`check`**
- Uses `TargetArgs`
- Collects system metrics and stores in DB

**`checkout`**
- Uses `TargetArgs`
- `--format <FORMAT>` - tui|table|html|json (default: tui)
- `--history` - Show trend history
- `--since <VALUE>` - Start point (e.g., "2025-01-01" or "7d")
- `--out <PATH>` - Output file for html/json

**`sync`**
- Uses `TargetArgs`
- `--dry-run` - Preview without changing
- `--files, -f <PATHS>` - Ad-hoc files to sync (comma-separated)
- `--no-push-missing` - Don't push to hosts lacking the file

**`run`**
- Uses `TargetArgs`
- `<COMMAND>` - Command string to execute
- `--sudo, -s` - Run with sudo
- `--yes, -y` - Auto-respond yes (serial only)

**`exec`**
- Uses `TargetArgs`
- `<SCRIPT>` - Local script path
- `--sudo, -s` - Run with sudo
- `--yes, -y` - Auto-respond yes
- `--keep` - Keep remote temp script after execution
- `--dry-run` - Preview without executing

**`config`**
- No args - Opens config in $EDITOR

**`log`**
- `--last <N>` - Show last N entries (default: 20)
- `--since <VALUE>` - Filter by datetime
- `--host <NAME>` - Filter by host
- `--action <ACTION>` - Filter by action (sync|run|exec|check)
- `--errors` - Show only errors

---

## 4. HOST FILTER (`src/host/filter.rs`) - FULL FILE

Single exported function with unit tests.

**`filter_hosts<'a>(hosts: &'a [HostEntry], groups: &[String], host_names: &[String], all: bool) -> Vec<&'a HostEntry>`**

Logic:
1. If `all=true`, return all hosts
2. If `groups` not empty, filter to hosts whose `groups` vector contains any of the specified groups
3. If `host_names` not empty, filter to hosts whose `name` matches
4. Both group and host filters can be applied together (intersection)

Tests cover:
- `--all` returns all hosts
- `--group web` returns hosts with "web" in their groups
- `--host b` returns specific host
- `--group web --host c` returns intersection (c only if in web group)

---

## 5. COMMANDS DIRECTORY

### Files:
- `mod.rs` - CommandContext and TargetMode
- `sync.rs` - File synchronization (20.9 KB)
- `check.rs` - Health metric collection
- `checkout.rs` - View metrics/reports
- `init.rs` - Host discovery
- `run.rs` - Execute remote commands
- `exec.rs` - Execute remote scripts
- `config.rs` - Edit config file
- `log.rs` - View operation logs

### 5a. COMMANDS MOD (`src/commands/mod.rs`) - FULL FILE

**`TargetMode`** Enum:
- `All` - All configured hosts
- `Hosts(Vec<String>)` - Specific hosts by name
- `Groups(Vec<String>)` - Hosts in named groups

**`Context`** - Shared command context:
- `config: AppConfig` - Loaded configuration
- `config_path: Option<PathBuf>` - Custom config path
- `db: Connection` - SQLite database connection
- `timeout: u64` - SSH timeout
- `mode: TargetMode` - Target selection mode
- `serial: bool` - Execute sequentially
- `verbose: bool` - Debug logging

**Key Methods**:

`Context::new(verbose, target: &TargetArgs, config_path) -> Result<Self>`
- Loads config or creates default
- Opens state DB
- Resolves target mode (validates --group/--host/--all constraints)
- Sets timeout from args or config

`Context::new_without_targets(verbose, config_path) -> Result<Self>`
- For commands like init, config, log that don't operate on hosts
- Sets mode to `All` (unused)

`Context::resolve_hosts(&self) -> Result<Vec<&HostEntry>>`
- Returns filtered hosts based on mode
- Shows helpful error with available groups/hosts if empty

`Context::concurrency(&self) -> usize`
- Returns 1 if serial, else `config.settings.max_concurrency`

**Helper Functions**:

`resolve_target_mode(target: &TargetArgs, config: &AppConfig) -> Result<TargetMode>`
- Validates exactly one of --group/--host/--all is specified
- Shows available groups/hosts on error
- Returns appropriate TargetMode

`append_available_hints(config: &AppConfig, hint: &mut String)`
- Collects all unique group names from hosts
- Appends formatted list to error message

`collect_available_groups(config: &AppConfig) -> BTreeSet<String>`
- Iterates all hosts and collects unique groups

---

### 5b. COMMANDS SYNC (`src/commands/sync.rs`) - FULL FILE (589 lines)

**Synchronizes files across hosts using a 3-stage collect-decide-distribute model.**

**Main Entry**:
`pub async fn run(ctx: &Context, dry_run: bool, files: &[String], no_push_missing: bool) -> Result<()>`

**Modes**:

1. **Ad-hoc mode** (`--files/-f`)
   - Syncs specific file paths provided on CLI
   - Converts absolute paths back to `~/` form for consistency across hosts
   - Label: "ad-hoc"

2. **Config-based global** (`--all` or `--host`)
   - Processes `[[sync.file]]` entries where `groups` is empty
   - Requires ≥2 hosts
   - Label: "global"

3. **Config-based group** (`--group`)
   - Processes `[[sync.file]]` entries whose `groups` intersect with specified groups
   - Requires ≥2 hosts
   - Label: group name(s)

**Stage 1: Collect Metadata** (`collect_file_metadata()`)
- Spawns parallel tasks with semaphore for concurrency control
- For each host, runs shell-specific metadata command:
  - **Sh**: `stat -c '%Y %s' <path>` (mtime, size) + `sha256sum`
  - **PowerShell**: `Get-Item` + `Get-FileHash SHA256`
  - **Cmd**: Similar to Sh
- Returns `CollectResult`:
  - `found: Vec<FileInfo>` - (host, mtime, hash, size)
  - `missing: Vec<String>` - Hosts where SSH OK but file missing

**Stage 2: Make Decisions** (`make_decisions()`)
- Conflict strategies:
  - **Newest**: Pick file with latest mtime as source
  - **Skip**: Skip if any conflict detected
- Returns `SyncDecision`:
  - `source_host: String` - Which host provides the canonical version
  - `target_hosts: Vec<String>` - Where to push (different hash + optionally missing)
  - `synced_hosts: Vec<String>` - Already have same content (shown as ✓)
  - `reason: String` - Why this decision (mtime, conflict, etc.)

**Stage 3: Distribute** (`distribute()`)
1. Downloads file from source to local temp via SCP
2. Uploads to all targets in parallel with:
   - Parent directory creation (mkdir -p)
   - Shell-specific path expansion (~/ → $HOME)
3. Returns (succeeded_hosts, failed_hosts_with_errors)
4. Logs sync state to DB for each succeeded target

**Key Features**:
- Handles `~/` path expansion with `to_tilde_path()`
- Tilde paths stay tilde on all remotes (home dir is local)
- Parent directory creation handles sh/PowerShell/cmd syntax
- DB logging: `sync_state` table tracks mtime, hash, timestamp
- Operation logging: `operation_log` table records sync attempts

---

### 5c. COMMANDS CHECK (`src/commands/check.rs`) - FULL FILE (150 lines)

**Collects system metrics from remote hosts and stores snapshots in DB.**

`pub async fn run(ctx: &Context) -> Result<()>`

Process:
1. Resolves target hosts
2. Collects configured check paths (custom path monitoring)
3. Spawns parallel tasks with semaphore for concurrency
4. For each host, calls `collector::collect()` which:
   - Runs enabled metrics probes (online, memory, disk, cpu_load, etc.)
   - Collects custom path sizes
   - Returns `CollectionResult` with success/failure counts and errors

**Result handling**:
- **All metrics failed** (0 succeeded):
  - Marked as offline (online=0)
  - Status: "error", shows elapsed time + first error
  - Added to summary as failure

- **Partial success** (some failed):
  - Marked as online (online=1)
  - Status: "skip" (not an error, warnings)
  - Shows X/Y metrics succeeded + error detail
  - Added to summary as success (site is reachable)

- **Full success** (all metrics succeeded):
  - Marked as online (online=1)
  - Status: "ok"
  - Shows count of metrics + elapsed
  - Added to summary as success

**Database updates**:
- `check_snapshots` table: Host, timestamp, online flag, raw JSON
- `host_last_seen` table: Track last_seen and last_online timestamps
- Retention cleanup: Deletes snapshots older than `data_retention_days`

---

### 5d. COMMANDS CHECKOUT (`src/commands/checkout.rs`) - FULL FILE (510 lines)

**Views historical system metrics and generates reports.**

`pub async fn run(ctx: &Context, format: OutputFormat, history: bool, since: Option<String>, out: Option<String>) -> Result<()>`

**Output Formats**:

1. **JSON** (`--format json`)
   - Latest snapshot or history (array if `--history`)
   - Requires `--out` to write to file (or `-` for stdout)

2. **HTML** (`--format html`)
   - Generates interactive HTML report
   - Requires `--out <PATH>`
   - Includes embedded JSON and styles

3. **Table** (`--format table`)
   - Plain text table to stdout
   - Shows: Host, Status (online/offline), CPU Load, Memory%, Disk%, Battery, Last Seen

4. **TUI** (`--format tui`, default)
   - Interactive terminal UI using ratatui
   - Shows same info as table but with colors and styling
   - Press 'q' or Esc to exit
   - Requires `tui` feature at compile time

**Time Parsing** (`parse_since()`):
- Relative: "7d" (7 days ago), "24h" (24 hours ago)
- ISO date: "2025-01-01" (midnight UTC)

**Metric Extraction**:
- `extract_cpu_load()` - Handles sh (JSON object) and PowerShell (string/float)
- `extract_memory()` - Calculates percent, highlights if >90%
- `extract_disk()` - Root mount priority, highlights if >90%
- `extract_battery()` - Shows percent or N/A if no battery

**Relative time formatting** (`format_relative_time()`):
- Shows: "30s ago", "15m ago", "3h ago", "2d ago"

---

### 5e. COMMANDS INIT (`src/commands/init.rs`) - FULL FILE (125 lines)

**Discovers hosts from ~/.ssh/config and detects remote shell types.**

`pub async fn run(ctx: &Context, update: bool, dry_run: bool, skip: Vec<String>) -> Result<()>`

Process:
1. Parses `~/.ssh/config` to extract host entries
2. Merges CLI `--skip` with persisted `settings.skipped_hosts`
3. Spawns parallel shell detection tasks:
   - Skips hosts already imported (unless `--update`)
   - Runs probe command to detect shell (sh/PowerShell/cmd)
   - Shows status: "detected: sh", "error: <reason>", or "skipped"
4. Merges detected hosts into existing config
5. Persists newly skipped hosts to prevent re-prompting
6. Saves config and prints summary

**Behavior Notes**:
- Failed hosts are NOT registered in config (don't know their shell)
- `--update` re-detects shell for existing hosts
- When config exists, defaults to merge behavior (not pure overwrite)
- Dry-run shows what would import without writing

---

### 5f. COMMANDS RUN (`src/commands/run.rs`) - FULL FILE (90 lines)

**Executes a command string on remote hosts.**

`pub async fn run(ctx: &Context, command: &str, sudo: bool, _yes: bool) -> Result<()>`

Process:
1. Resolves target hosts
2. Wraps command with sudo if `--sudo` flag
3. Spawns parallel execution tasks with concurrency control
4. For each host:
   - Executes command via SSH
   - Prints stdout with host prefix via `print_host_line()`
   - Logs success/failure to `operation_log` table with elapsed time
   - Status: "ok" if exit code 0, else "error" with stderr

---

### 5g. COMMANDS EXEC (`src/commands/exec.rs`) - FULL FILE (218 lines)

**Uploads and executes a local script on remote hosts.**

`pub async fn run(ctx: &Context, script: &str, sudo: bool, _yes: bool, keep: bool, dry_run: bool) -> Result<()>`

Process:
1. Validates script exists and determines shell type from extension:
   - `.sh` → Sh
   - `.ps1` → PowerShell
   - `.bat|.cmd` → Cmd
2. Dry-run mode:
   - Shows script path and compatible shell
   - Lists which hosts would execute vs skip due to shell mismatch
3. For each compatible host:
   - Uploads script to temp directory with unique name (mktemp-style)
   - Makes executable (Sh only)
   - Executes:
     - Sh: `./remote_path`
     - PowerShell: `powershell -File remote_path`
     - Cmd: `remote_path` (native .cmd execution)
   - Wraps with sudo if requested
   - Cleans up temp file (unless `--keep`)
   - Logs to operation_log

**Shell compatibility**: Skips hosts with mismatched shell types

---

### 5h. COMMANDS CONFIG (`src/commands/config.rs`) - FULL FILE (35 lines)

**Opens config file in $EDITOR.**

`pub async fn run(config_path: Option<&Path>) -> Result<()>`

- Validates config exists (fails if not initialized)
- Resolves editor: $EDITOR → $VISUAL → "vi" (Unix) or "notepad" (Windows)
- Spawns editor process

---

### 5i. COMMANDS LOG (`src/commands/log.rs`) - FULL FILE (114 lines)

**Displays operation logs from the DB.**

`pub async fn run(ctx: &Context, last: usize, since: Option<String>, host: Option<String>, action: Option<ActionFilter>, errors: bool) -> Result<()>`

Filters from `operation_log` table:
- `--last N` - Most recent N entries (default: 20)
- `--since <VALUE>` - Relative ("7d", "24h") or ISO date ("2025-01-01")
- `--host <NAME>` - Filter by host
- `--action <ACTION>` - sync|run|exec|check
- `--errors` - Only error status

Output format per line:
- Timestamp (YYYY-MM-DD HH:MM:SS)
- Status icon (✓ ok, ✗ error, ⊘ skipped)
- Host in [brackets]
- Command + action
- Duration (if available)
- Note (stderr or reason)

---

## 6. MAIN ENTRY (`src/main.rs`) - FULL FILE (96 lines)

**Entry point for the application.**

Structure:
1. Parses CLI args via `Cli::parse()`
2. Initializes tracing with debug/info level based on `--verbose`
3. Resolves config path from `--config` or uses default
4. Matches on command and creates appropriate Context:
   - Commands with targets (check, sync, run, exec, checkout): `Context::new(verbose, &target, cfg)`
   - Commands without targets (init, config, log): `Context::new_without_targets(verbose, cfg)`
5. Calls command handler and returns result

**Command dispatch**:
```rust
match cli.command {
    Init { ... } → commands::init::run(ctx, update, dry_run, skip)
    Config → commands::config::run(cfg)
    Check { target } → commands::check::run(ctx)
    Checkout { target, format, ... } → commands::checkout::run(ctx, format, history, since, out)
    Sync { target, dry_run, files, ... } → commands::sync::run(ctx, dry_run, files, no_push_missing)
    Run { target, command, ... } → commands::run::run(ctx, command, sudo, yes)
    Exec { target, script, ... } → commands::exec::run(ctx, script, sudo, yes, keep, dry_run)
    Log { last, since, host, action, errors } → commands::log::run(ctx, last, since, host, action, errors)
}
```

---

## 7. METRICS COLLECTOR (`src/metrics/collector.rs`) - FULL FILE (100 lines)

**Collects system metrics from a single remote host.**

`pub async fn collect(host: &HostEntry, enabled: &[String], check_paths: &[(String, String)], timeout_secs: u64) -> Result<CollectionResult>`

**`CollectionResult`**:
- `data: serde_json::Value` - JSON object with all metrics + metadata
- `succeeded: usize` - Count of successful metrics
- `failed: usize` - Count of failed metrics
- `errors: Vec<String>` - Error messages per metric

**Process**:

1. For each enabled metric:
   - Gets shell-specific command via `probes::command_for(shell, metric)`
   - Executes via `executor::run_remote()`
   - On success: Parses output via `parser::parse()` and stores in JSON
   - On failure: Records error message, increments failed count

2. For each custom check path:
   - Gets size via `probes::path_size_command()`
   - Parses output via `parser::parse_path_size()`
   - Creates object: `{label, path, size_bytes}`
   - Stores in `paths` array in JSON

3. Returns CollectionResult with:
   - `schema_version: 1` metadata
   - All metric results
   - Success/failure/error tracking

---

## 8. OUTPUT SUMMARY (`src/output/summary.rs`) - FULL FILE (43 lines)

**Tracks and displays operation summary statistics.**

**`Summary` struct**:
```rust
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>,  // (host, message)
}
```

**Methods**:
- `add_success()` - Increment succeeded counter
- `add_failure(host, message)` - Track failed host with error
- `add_skip()` - Increment skipped counter
- `print()` - Output formatted summary to stdout

**Output format**:
```
── Summary ──────────────────────────────
  X succeeded  Y failed  Z skipped
  Errors:
    host1: error message
    host2: error message
```

---

## Key Architectural Patterns

### 1. Target Selection Hierarchy
```
--all (All hosts)
--host host1,host2 (Specific hosts)
--group web,db (Hosts in groups)
(Only one can be used; resolved to TargetMode)
```

### 2. File Sync Scoping
```
SyncFile with empty groups → applies to --all/--host mode only
SyncFile with groups=["web"] → applies to --group web only
Matching is intersection: file's groups AND selected groups
```

### 3. Concurrency Model
- Default: Parallel with `max_concurrency` semaphore
- `--serial` flag: Set concurrency to 1
- Context: `ctx.concurrency()` returns 1 or max_concurrency

### 4. Database Schema (inferred)
```
check_snapshots: (host, collected_at, online, raw_json)
host_last_seen: (host, last_seen, last_online)
sync_state: (sync_group, host, path, mtime, size_bytes, blake3, synced_at)
operation_log: (timestamp, command, host, action, status, duration_ms, note)
```

### 5. Platform Support
- **Sh** (Linux/macOS): stat, sha256sum/shasum, chmod, mkdir -p
- **PowerShell** (Windows): Get-Item, Get-FileHash, Remove-Item, mkdir
- **Cmd** (Windows): del, mkdir (limited use)

### 6. Error Handling
- Check partial success: Some metrics fail but host is reachable → Status "skip" (online)
- Check total failure: All metrics fail → Status "error" (offline)
- Sync missing hosts: Tracked separately, can be pushed to if `push_missing=true`

---

## Summary Statistics

| Component | Lines | Purpose |
|-----------|-------|---------|
| schema.rs | 141 | Config types |
| app.rs | 120 | Config I/O |
| cli.rs | 204 | CLI argument parsing |
| filter.rs | 89 | Host filtering |
| mod.rs (commands) | 178 | Command context & target resolution |
| sync.rs | 589 | File synchronization |
| check.rs | 150 | Metric collection |
| checkout.rs | 510 | Report generation |
| init.rs | 125 | Host discovery |
| run.rs | 90 | Remote command execution |
| exec.rs | 218 | Remote script execution |
| config.rs | 35 | Config editor |
| log.rs | 114 | Operation logs |
| main.rs | 96 | Application entry point |
| collector.rs | 100 | Metric collection logic |
| summary.rs | 43 | Summary output |

