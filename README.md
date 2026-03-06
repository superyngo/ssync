# ssync

SSH-config-based cross-platform remote management tool.

## Features

- **Host Discovery**: Import hosts from `~/.ssh/config` with automatic shell type detection
- **System Snapshots**: Collect and store system information for historical tracking
- **File Synchronization**: Sync files across multiple hosts using collect-decide-distribute model
- **Remote Execution**: Run commands or scripts on multiple hosts in parallel
- **TUI Interface**: Interactive terminal UI for viewing historical data and trends

## Installation

### Windows

```powershell
$env:APP_NAME="ssync"; $env:REPO="superyngo/ssync"; irm https://gist.githubusercontent.com/superyngo/a6b786af38b8b4c2ce15a70ae5387bd7/raw/gpinstall.ps1 | iex
```

### macOS / Linux

```bash
cargo install ssync
```

Or build from source:

```bash
git clone https://github.com/superyngo/ssync.git
cd ssync
cargo install --path .
```

## Usage

### Initialize

Import hosts from `~/.ssh/config`:

```bash
ssync init
```

Re-detect shell types for existing hosts:

```bash
ssync init --update
```

### Check

Collect system snapshots from hosts:

```bash
# All hosts
ssync check --all

# Specific group
ssync check -g servers

# Specific hosts
ssync check -h host1,host2

# Sequential execution
ssync check --all --serial
```

### Sync

Synchronize files across hosts:

```bash
# Sync configured files
ssync sync --all

# Preview without changes
ssync sync --all --dry-run

# Sync specific files
ssync sync --all -f /etc/hosts,/etc/resolv.conf

# Don't push to hosts missing files
ssync sync --all --no-push-missing
```

### Run

Execute commands on remote hosts:

```bash
# Run command on all hosts
ssync run --all "uptime"

# Run with sudo
ssync run --all "apt update" --sudo

# Auto-confirm prompts (serial mode)
ssync run --all "systemctl restart nginx" --yes
```

### Exec

Upload and execute local scripts:

```bash
# Execute script
ssync exec --all ./deploy.sh

# Execute with sudo
ssync exec --all ./install.sh --sudo

# Keep remote script after execution
ssync exec --all ./script.sh --keep

# Preview without executing
ssync exec --all ./deploy.sh --dry-run
```

### Checkout

View historical data and generate reports:

```bash
# Interactive TUI
ssync checkout --all

# Table format
ssync checkout --all --format table

# HTML report
ssync checkout --all --format html --out report.html

# Show trend history
ssync checkout --all --history

# History from specific date
ssync checkout --all --history --since "2025-01-01"
```

### Log

View operation logs:

```bash
# Show last 20 entries
ssync log

# Show last 50 entries
ssync log --last 50

# Filter by host
ssync log --host server1

# Filter by action type
ssync log --action sync

# Show only errors
ssync log --errors
```

### Config

Open configuration file in `$EDITOR`:

```bash
ssync config
```

## Target Selection

All commands that operate on remote hosts support the following target options:

| Flag | Description |
|------|-------------|
| `-a, --all` | Target all configured hosts |
| `-g, --group` | Target hosts by group (comma-separated) |
| `-h, --host` | Target specific hosts (comma-separated) |
| `--serial` | Execute sequentially instead of in parallel |
| `--timeout` | Connection timeout in seconds |

## Configuration

The default config location is `~/.config/ssync/config.toml`.

Example configuration:

```toml
[settings]
default_timeout = 30
max_concurrency = 10
state_dir = "~/.local/share/ssync"

[[host]]
name = "server1"
hostname = "192.168.1.10"
user = "admin"
port = 22
groups = ["production", "web"]

[[host]]
name = "server2"
hostname = "192.168.1.11"
user = "admin"
groups = ["production", "db"]

[[host.file]]
path = "/etc/hosts"
description = "Hosts file"

[[host.file]]
path = "/etc/resolv.conf"
description = "DNS configuration"
```

## License

MIT
