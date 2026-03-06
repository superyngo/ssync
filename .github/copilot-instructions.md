# Copilot Instructions for ssync

## Build & Test

```bash
# Build
cargo build

# Build without TUI feature
cargo build --no-default-features

# Run tests
cargo test

# Run a single test
cargo test test_name

# Run tests in a specific module
cargo test config::ssh_config::tests

# Check without building
cargo check

# Lint
cargo clippy
```

## Architecture

ssync is a single-binary CLI tool that manages remote hosts over SSH by shelling out to the system `ssh`/`scp` commands (no embedded SSH library). It reads `~/.ssh/config` for host definitions and stores its own config in `~/.config/ssync/config.toml`. State is persisted in a SQLite database at `~/.local/state/ssync/ssync.db`.

### Module structure

- **`cli.rs`** — Clap derive-based CLI definitions for all subcommands
- **`commands/`** — One file per subcommand (`init`, `check`, `checkout`, `sync`, `run`, `exec`, `log`, `config`). Each receives a shared `CommandContext` (from `commands/mod.rs`) holding config, DB handle, timeout, and concurrency settings.
- **`config/`** — TOML config schema (`schema.rs`), config file I/O (`app.rs`), and `~/.ssh/config` parser (`ssh_config.rs`)
- **`host/`** — SSH execution abstraction (`executor.rs`), shell type detection and command wrapping (`shell.rs`), host/group filtering (`filter.rs`)
- **`metrics/`** — System metric collection orchestrator (`collector.rs`), output parsing (`parser.rs`), and per-shell probe command definitions (`probes/sh.rs`, `probes/powershell.rs`, `probes/cmd.rs`)
- **`state/`** — SQLite connection with WAL mode and migration via `PRAGMA user_version` (`db.rs`), data retention cleanup (`retention.rs`)
- **`output/`** — Host-prefixed colored terminal output (`printer.rs`), execution summary (`summary.rs`)

### Key data flow

1. CLI args parsed via clap → `main.rs` dispatches to the appropriate `commands/*.rs` handler
2. Hosts are filtered by `--group`/`--host`/`--all` flags using `host::filter`
3. Remote operations run in parallel via Tokio with a semaphore (default 10 concurrent, `--serial` sets to 1)
4. All operations are logged to the `operation_log` SQLite table

### Sync strategy

The `sync` command uses a 3-stage approach: (1) collect file metadata (mtime + BLAKE3 hash) from all hosts, (2) decide the source using the configured conflict strategy (`newest` or `skip`), (3) distribute files through a local relay (download from source, upload to targets).

## Conventions

- **Error handling**: `anyhow::Result<T>` throughout commands; `thiserror` for typed errors where needed. Use `?` with `.context()` for propagation.
- **Async**: All command handlers are async. Use `tokio::time::timeout` for SSH operations. Concurrency is controlled via `tokio::sync::Semaphore`.
- **Shell compatibility**: Remote commands must account for three shell types (`Sh`, `PowerShell`, `Cmd`). Use `host::shell` for command wrapping, temp directory paths, and sudo handling.
- **TUI feature**: The `tui` feature (enabled by default) gates `ratatui`/`crossterm` dependencies. Guard TUI-specific code with `#[cfg(feature = "tui")]`.
- **SSH transport**: All remote operations shell out to system `ssh`/`scp` via `tokio::process::Command`. Never use an embedded SSH library — this ensures full `~/.ssh/config` compatibility (ProxyJump, ssh-agent, etc.).
- **Database migrations**: Embedded in the binary. Version tracked via `PRAGMA user_version`. New migrations increment the version number.
- **Config paths**: Use the `dirs` crate for cross-platform path resolution. Config: `dirs::config_dir()/ssync/`, state: `dirs::state_dir()/ssync/` (falling back to `dirs::data_local_dir()`).
- **Output formatting**: Use `output::printer` for host-prefixed colored output (✓ green success, ✗ red error, ⊘ yellow skip). Use `output::summary` for execution summaries.
