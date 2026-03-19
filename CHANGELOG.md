# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [v0.4.0] - 2026-03-19

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
