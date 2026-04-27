# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
