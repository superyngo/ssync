# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Phase 3 (MVP complete) — Operate tab + `check` execution: `tui::async_bridge` module with `TuiEvent` enum, bounded ring of host outcomes, `EventSender` (impls `ProgressSink`), and `RunningOp` (`CancellationToken` + targets + accumulated outcomes). Operate tab UI shows the operation row, target summary (with live host count + filter info), the first six applicable `[[check]]` entries, and an `[Execute]` button. Pressing Enter on Execute spawns the operation on a dedicated OS thread with its own current-thread tokio runtime — sidesteps the `!Send` constraint that `rusqlite::Connection` imposes on a multi-thread `tokio::spawn`. While running: progress popup shows the last 12 host outcomes with elapsed counter; Esc cancels via `CancellationToken`; tab switches still work; concurrency-guard surfaces "Operation already running" if Execute is pressed twice. After completion: results popup with per-host status / detail / duration; the Checkout tab is marked `db_stale` and lazily reopens its DB connection on next render so freshly-written snapshots appear. README and AGENTS.md updated with the two-binary model, TUI keybindings, and contributor rules.
- Phase 2 popups + persistence wiring: `tui::components::popup::centered_rect` helper; `tui::components::target_filter::FilterPopup` with All / Groups / Hosts / Shell modes (Shell hidden when `allow_shell = false`), serial toggle, timeout display, Apply/Cancel buttons. `f` key on the Operate tab opens the popup; Apply commits the new filter to `App.target_filter`, validates against the current config, and atomically writes `tui_state-{config_hash}.toml` to the resolved state dir (per AD-16). Persistence: `state_file_path()` (blake3-hashed config path), `persist::load()` (silent fallback to defaults on missing/malformed file with `tracing::warn!`), `persist::save()` (atomic via `tempfile::persist`), `persist::validate_filter()` (drops unknown groups/hosts; falls back to All if Groups/Hosts mode ends up empty). Active tab and filter state are restored on the next `ssync-tui` launch. `AppConfig` and `Settings` now `Clone`. Eight new tests cover load/save round-trip, malformed-file fallback, validation, and `config_hash` determinism (116 total).
- Phase 1b TUI navigation core: `tui::focus` module — `Direction`, `Axis`, `AxisFreedom`, `FocusZone`, `FocusPath`, `Focusable` trait with default `handle_arrow` (returns `ArrowResult::Consumed` or `Escaped(Direction)`), and `escape_to_parent` implementing the §8.6 zone-neighbour table for the Checkout tab. Twelve focus state-machine unit tests cover Y/X/None axis-freedom branches, boundary detection, empty-list safety, breadcrumb update on zone change, and zone-transition outcomes. `tui::state::persist::TuiPersistedState` schema struct lands with `#[serde(default)]` on every field; load/save wiring deferred to Phase 2 (AD-8). All persistence enums (`ActiveTab`, `TargetFilterMode`, `ShellMode`, `OperationKind`) round-trip via TOML.
- Phase 1a TUI scaffolding: new `ssync-tui` binary launches an interactive interface when invoked on a TTY without a subcommand. Tab bar (`1`/`2`/`3`/Tab/Shift+Tab) cycles between Config/Operate/Checkout. The Checkout tab shows the latest snapshot per host with `↑↓`/`jk`/PgUp/PgDn/Home/End scrolling. `?` toggles a keybinding help popup; `q`/Ctrl+C quit; `Esc` clears errors. Terminal-size guard renders a "too small" message below 80×24 and resumes when the terminal is enlarged. Panic hook + `TerminalGuard` Drop guarantee terminal restoration on any exit path. SIGHUP/SIGTERM/SIGINT (Unix) and Ctrl+C (Windows) trigger graceful shutdown. The `ssync` (non-tui) binary always falls through to a "TUI not available" error (exit 1); piped or `TERM=dumb` `ssync-tui` invocations print clap help and exit 2.
- Phase 0.7 command-core extraction: new `commands::report` module with `ProgressSink` trait, `HostStatus` enum, `CheckHostResult`, `CheckReport`, and `CommandReport`; `commands::check` split into `check_core` (DB-writing pure core) and a thin CLI `run` wrapper with a `PrinterSink`. `Context::from_tui_parts` constructor added (TUI-feature-gated). `commands::checkout` helpers (`fetch_latest_snapshots`, `DisplayColumns`, `format_relative_time`, `HostSnapshot`) promoted to `pub(crate)` for TUI reuse.
- TUI groundwork (Phase 0 + 0.5 of `docs/tui_reconstruct_plan.md`): added `ssync-tui` bin target gated by `tui` feature; `toml_edit`, `unicode-width`, optional `tokio-util` deps; `name`/`id` optional fields on `[[check]]`/`[[sync]]` entries (legacy configs continue to load); BOM stripping in config load; `~` expansion in `resolve_path`; `state::db::resolved_state_dir()` helper; format-preserving config save via `toml_edit` with round-trip validation
- Removed unused `tracing-appender` dependency

## [v0.8.0] - 2026-04-29

### Added
- `--shell/-s` target filter to select hosts by detected shell type (sh, powershell, cmd)
- `--out` report output for `run`, `exec`, `check`, `sync`, and `checkout` commands (JSON and HTML)
- `default_output_format` config setting to set default report format when `--out` path has no extension
- Per-host raw output JSON in HTML reports via collapsible details
- Auto-generated report filenames now respect `default_output_format`

### Changed
- CLI short flags reassigned for consistency: `--shell/-s`, `run|exec --sudo/-S`, `sync --source/-S`
- Removed `--format` from checkout; `--out` now handles both JSON and HTML output
- Removed "Collecting" progress bar from check and sync commands for cleaner output
- Deleted temporary test scripts (test.ps1, test.sh)

### Fixed
- Raw probe output strings now use move instead of clone for efficiency
- Unified `Utc::now()` timestamp handling in check command

## [v0.7.3] - 2026-04-28

### Fixed
- musl cross-compilation builds now succeed: removed `ssh2-config` dependency (which
  unconditionally pulled in `git2 → libgit2-sys → libssh2-sys → openssl-sys`) and replaced
  it with an enhanced pure-Rust SSH config parser that supports `Host *` wildcard inheritance
- Added `Cross.toml` with `pre-build` commands as a belt-and-suspenders guard to install
  `libssl-dev` in the cross Docker containers for all four musl targets
- Reverted the incorrect v0.7.2 vendored-OpenSSL workaround (target-conditional dependencies
  do not affect build-dependency compilation inside cross containers)

### Changed
- `ssh2-config` crate removed; SSH config parsing is now handled entirely by the built-in
  pure-Rust parser in `src/config/ssh_config.rs`. `ParsedSshConfig` replaces
  `ssh2_config::SshConfig` as the shared config handle in `session_pool`

## [v0.7.2] - 2026-04-28

### Fixed
- Vendor OpenSSL for musl targets to fix CI build failures (`ssh2-config` 0.7.1 transitively requires `openssl-sys` via `git2`; musl cross-compilation containers lack system OpenSSL headers)

## [v0.7.1] - 2026-04-28

### Fixed
- SFTP probe and upload now use `sftp.create()` instead of `sftp.write()` to correctly create non-existent files (russh-sftp `write()` opens with `WRITE` flag only, no `CREATE`)
- Removed `inactivity_timeout` from russh client config; the timeout was killing idle sessions between `setup()` and subsequent `exec`/shell-detection calls, causing all shell detections to fail

## [v0.7.0] - 2026-04-27

### Added
- Embedded russh SSH transport: all SSH/SCP subprocess calls replaced with pure-Rust russh library
- Multi-alias SSH host parsing: correctly handles `Host bastion alias1 alias2` entries in `~/.ssh/config`
- SFTP-based file transfers in `sync` command (replaces external `scp`)
- ProxyJump support via russh (single-hop; config-driven)
- Windows SSH connection multiplexing via connection pool (no ControlMaster required)
- `detect_russh` shell detection using established russh sessions
- SFTP upload/download helpers with 64 MB size guard and early stat check
- Parallel SFTP probe with JoinSet and home-dir caching in session pool

### Changed
- `ssync init` migrated to `RusshSessionPool`; unknown-host-key flow matches russh error format
- `shell.rs` detect functions replaced with `detect_russh`
- `pool.rs` is now russh-only; `ConnectionManager` removed entirely
- `filter_reachable` now consistently keyed on `ssh_host` (matching `filter_sftp_capable`)
- `exec.rs` upload path uses SFTP instead of `scp` subprocess

### Fixed
- VirtualLock security warnings suppressed on Windows in non-verbose mode
- `partition_host_key_failures` matches both `"Unknown host key"` (russh) and legacy ControlMaster error strings
- SFTP download guards file size via `metadata()` before `read()` to prevent OOM

### Removed
- `connection.rs`, `executor.rs`, `process_transport.rs`, `transport.rs` legacy modules (~1,600 lines)
- `async-trait` dependency (no longer needed after transport abstraction removal)
- `socket_for` stub in `pool.rs`

## [v0.6.0] - 2026-04-14

### Added
- SshTransport trait definition with unified interface for SSH operations
- RemoteOutput struct for structured command execution results
- ProcessTransport implementation wrapping ConnectionManager with RwLock
- async-trait dependency for async trait support

### Changed
- SSH abstraction layer (Phase 1) enabling future transport backends

### Docs
- SshTransport trait abstraction design spec
- OpenSSH library migration evaluation
- russh library migration evaluation
- SSH transport trait implementation plan

### Tests
- ProcessTransport unit tests (send/sync, creation, initial state)

## [v0.5.0] - 2026-03-21

### Added
- Dual-mode ConnectionManager (Pooled/Direct) for Windows client support
- ANSI escape code support on Windows terminals via `SetConsoleMode`

### Fixed
- Shell-aware SCP probe paths for PowerShell and Cmd remote hosts
- Windows Cmd remote shell support in sync commands (metadata, batch, dir-expand)
- Defensive escaping and clippy/fmt fixes throughout

## [v0.4.0]- 2026-03-19

### Added
- SSH host key acceptance during init with interactive prompt
- SSH host resolution and keyscan helpers for batch operations
- Partition helper for host key failure handling
- Stale host detection and removal prompt during init
- Hostname display in sync summary transfer lines

### Changed
- Enhanced init workflow with host key management
- Improved sync summary with clearer host identification

### Docs
- Implementation plan for init host key acceptance
- Design spec for init host key acceptance feature
- Implementation plan for init stale hosts, summary hostnames, version verification
- Design spec for init stale host detection, summary hostnames, version verification

### CI
- Version verification step in release workflow

## [v0.3.0] - 2026-03-13

### Added
- SSH connection pooling for improved performance
- Batch metadata collection with parallel operations
- Per-host concurrency configuration
- Skip reasons tracking for sync operations
- Progress display enhancements
- ConcurrencyLimiter and pooled SSH executor functions
- SSH ConnectionManager with per-file skip on missing source
- Batched metadata command builder and parser

### Changed
- Complete sync pipeline optimization with batched collection and parallel distribution
- Rewrote check, exec, run, and init commands to use pooled executor
- Replaced inline ConnectionManager with SshPool

### Docs
- Sync pipeline optimization implementation plan
- Sync pipeline optimization design spec

## [v0.2.0] - 2026-03-11

### Added
- List command to display configured hosts and groups
- Enhanced CLI capabilities with improved command structure

### Changed
- Improved checkout command with better HTML export
- Enhanced sync command with collect-decide-distribute model refinement
- Refined app config schema for better flexibility
- Improved shell-specific probe implementations

## [v0.1.1] - 2026-03-08

### Fixed
- Corrected Windows state directory variable binding in `state_dir()` for proper platform-specific resolution.

## [v0.1.0] - 2026-03-06

### Added
- SSH-config-based host discovery and import
- Automatic shell type detection (sh, bash, zsh, PowerShell, cmd.exe)
- System snapshots (CPU, memory, disk, battery metrics)
- File synchronization with collect-decide-distribute model
- Remote command execution (`run`, `exec`)
- TUI for viewing historical data and trends
- Operation logging with SQLite state database
- Group-based host targeting
- Cross-platform GitHub release workflow

### Documentation
- Complete README with usage examples for all commands
- Configuration file examples
- Target selection reference
