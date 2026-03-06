# AGENTS.md - Shared AI Agent Prompt for ssync

## Build, Test, and Quality Commands

```bash
# Build
cargo build
cargo build --no-default-features  # Build without TUI feature
cargo build --release               # Release build

# Check without building (faster)
cargo check

# Run tests
cargo test
cargo test test_name                # Run single test
cargo test config::ssh_config::tests # Run module tests
cargo test -- --nocapture           # Show print! output

# Linting
cargo clippy
cargo clippy --all-targets          # Lint tests too

# Formatting
cargo fmt
cargo fmt --check                   # Check formatting
```

## Code Style Guidelines

### Error Handling
- Use `anyhow::Result<T>` throughout command handlers
- Use `thiserror` for typed errors in library modules
- Propagate with `?` and add context: `.context("description")`
- Example: `fs::read_to_string(path).context("Failed to read config")?`

### Imports
- Group imports: std extern crates -> third-party -> local modules
- Use `use crate::module::item` for local imports
- Keep imports at file top, sorted alphabetically within groups
- Example:
  ```rust
  use std::path::PathBuf;
  use anyhow::{Context, Result};
  use tokio::process::Command;
  use crate::config::schema::HostEntry;
  ```

### Types and Naming
- Use `snake_case` for functions, modules, variables
- Use `PascalCase` for structs, enums, types
- Use `SCREAMING_SNAKE_CASE` for constants
- Prefer explicit types over `impl Trait` in public APIs
- Use `&str` for borrowable data, `String` for owned data

### Async Concurrency
- All command handlers are `async fn` returning `Result<()>`
- Use `tokio::time::timeout` for SSH operations with timeout
- Control concurrency with `tokio::sync::Semaphore` (default: 10 permits)
- Use `tokio::process::Command` for spawning ssh/scp subprocesses
- Parallelize host operations with `futures::future::join_all` or stream

### Testing
- Place tests in `#[cfg(test)]` modules at file bottom
- Write helper functions for test data setup
- Use in-memory SQLite for DB tests: `Connection::open_in_memory()`
- Test public APIs, not implementation details
- Prefix test functions with `test_`

### Shell Compatibility
- Support three shells: `Sh`, `PowerShell`, `Cmd` (from `host::shell` module)
- Use `host::shell::ShellType` enum for shell detection
- Commands must account for shell-specific syntax (paths, quoting, operators)
- Use `host::shell` module for command wrapping and temp paths

### Feature Flags
- TUI features guarded with `#[cfg(feature = "tui")]`
- Default feature set includes `tui` (ratatui, crossterm)
- Test builds with `--no-default-features` for TUI-less configs

### SSH Transport
- NEVER use embedded SSH libraries - shell out to system ssh/scp
- This ensures `~/.ssh/config` compatibility (ProxyJump, ssh-agent, etc.)
- Use `tokio::process::Command` to spawn ssh/scp processes

### Database
- Use SQLite with `rusqlite` and `bundled` feature
- Enable WAL mode: `PRAGMA journal_mode=WAL;`
- Migrations are embedded via `include_str!("migrations/NXX_name.sql")`
- Track version with `PRAGMA user_version`

### Paths
- Use `dirs` crate for cross-platform paths
- Config: `dirs::config_dir()/ssync/`
- State: `dirs::state_dir()/ssync/` (fallback: `dirs::data_local_dir()/ssync/`)
- SSH config: `~/.ssh/config`

### Output Formatting
- Use `output::printer` for host-prefixed colored terminal output
- Symbols: ✓ (green success), ✗ (red error), ⊘ (yellow skip)
- Use `output::summary` for execution summaries
- Use `indicatif` for progress bars

### Logging
- Use `tracing` for structured logging
- Levels: `DEBUG` (verbose mode), `INFO` (default)
- Set filter via `tracing_subscriber::EnvFilter::from_default_env()`

### CLI Arguments
- Use `clap` with derive macros
- Use `-v` as `--version` short option
- Common args: `--group`, `--host`, `--all`, `--serial`, `--timeout`
- Flatten shared args with `#[command(flatten)]`

### Comments and Documentation
- Document public APIs with `///` doc comments
- Keep comments concise and purpose-focused
- Avoid obvious comments, add for "why" not "what"

## Architecture Overview

ssync is a CLI tool managing remote hosts over SSH. Single binary, no embedded SSH.

**Module Structure:**
- `cli.rs` - Clap CLI definitions
- `commands/` - One file per subcommand (init, check, checkout, sync, run, exec, log, config)
- `config/` - Config schema, file I/O, SSH config parser
- `host/` - SSH execution, shell detection, host/group filtering
- `metrics/` - System metrics collection, parsing, shell-specific probes
- `state/` - SQLite DB, migrations, retention cleanup
- `output/` - Terminal printer, execution summary

**Key Data Flow:**
1. CLI args parsed → main.rs dispatches to command handler
2. Hosts filtered by --group/--host/--all via `host::filter`
3. Remote operations parallelized via Tokio (semaphore-limited)
4. All operations logged to `operation_log` table

**Sync Strategy:**
3-stage: (1) collect metadata (mtime + BLAKE3), (2) decide source (newest/skip), (3) distribute via local relay
