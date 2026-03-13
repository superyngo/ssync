# SSYNC CLI Analysis Report

**Analysis Date**: 2025  
**Scope**: All 9 SSYNC commands and their CLI argument structures  
**Source Files Analyzed**: 
- `src/cli.rs` (lines 1-215)
- `src/commands/mod.rs` (lines 1-251)
- `src/main.rs` (lines 1-101)
- Individual command implementations

## Executive Summary

This report documents the complete CLI argument structure across all SSYNC commands and identifies 8 significant inconsistencies, including 3 critical issues that will affect users.

### Critical Issues Found

1. **Help Flag Inconsistency** (-H vs -h)
   - TargetArgs commands use custom `-H` for help
   - INIT/LOG/CONFIG use standard `-h` for help
   - Creates user confusion when switching between commands
   
2. **-s Flag Overloading**
   - SYNC: `-s, --source` (takes value)
   - RUN/EXEC: `-s, --sudo` (boolean flag)
   - High risk of command misuse

3. **--timeout Not Available for INIT**
   - INIT performs SSH shell detection but cannot override timeout
   - No CLI workaround; requires config file editing
   - Clunky user experience for slow networks

## Quick Reference

### Commands Overview

| Command | Type | TargetArgs | --timeout | --dry-run | Notes |
|---------|------|-----------|-----------|-----------|-------|
| init | Setup | NO | ✗⚠️ | ✓ | Cannot override timeout |
| check | Metrics | YES | ✓ | ✗ | Basic operation |
| checkout | History | YES | ✓ | ✗ | Format options |
| sync | File Sync | YES | ✓ | ✓ | -s conflict |
| run | Execute | YES | ✓ | ✗ | -s conflict |
| exec | Script | YES | ✓ | ✓ | -s conflict |
| list | List | YES | ✓ | ✗ | Like check |
| log | Logs | NO | ✗ | ✗ | -h=filter |
| config | Edit | NO | ✗ | ✗ | Editor only |

### TargetArgs (6 Commands)

Commands using TargetArgs: **CHECK, CHECKOUT, SYNC, RUN, EXEC, LIST**

Common arguments (all shared):
```
-a, --all                  Target all hosts
-h, --host <HOST1,HOST2>   Target specific hosts
-g, --group <GROUP1,GROUP2> Target groups
--serial                   Execute sequentially
--timeout <SECONDS>        Override timeout
-H, --help                 Help (custom flag, not -h)
```

**Validation**: Requires exactly ONE of `--all`, `--host`, or `--group`

### INIT Arguments

```
--update           Re-detect shell types for existing hosts
--dry-run         Preview imports without writing
--skip <list>     Skip specific hosts (comma-separated)
```

**Issue**: No `--timeout` support despite SSH operations ⚠️

### Other Commands

**CHECKOUT** adds:
- `--format` (tui|table|html|json)
- `--history`, `--since`, `-o/--out`

**SYNC** adds:
- `--dry-run`, `-f/--files`, `--no-push-missing`
- `-s/--source` (host name) ⚠️ **CONFLICTS with RUN/EXEC**

**RUN** adds:
- `<COMMAND>` (positional)
- `-s/--sudo` ⚠️ **CONFLICTS with SYNC**
- `-y/--yes`

**EXEC** adds:
- `<SCRIPT>` (positional)
- `-s/--sudo` ⚠️ **CONFLICTS with SYNC**
- `-y/--yes`, `--keep`, `--dry-run`

**LOG** (no TargetArgs):
- `--last <N>`, `--since <date>`
- `-h/--host` (filter) ⚠️ **Different meaning**
- `--action`, `--errors`

**CONFIG**: No arguments (editor only)

## Detailed Findings

### 1. Help Flag Inconsistency (CRITICAL)

**Definition**: src/cli.rs:48-49
```rust
#[arg(short = 'H', long, action = clap::ArgAction::HelpLong)]
pub help: Option<bool>,
```

**Applied to**: CHECK, CHECKOUT, SYNC, RUN, EXEC, LIST via `#[command(disable_help_flag = true)]`

**Not applied to**: INIT, LOG, CONFIG (use standard `-h`)

**User impact**:
```bash
ssync check -H              # Works: shows help
ssync check -h host1        # Works: targets host1
ssync init -h               # Works: shows help
ssync log -h filter         # Works: filters by hostname
```

The same flag has different meanings in different contexts.

### 2. -s Flag Overloading (CRITICAL)

**SYNC** (cli.rs:118):
```rust
#[arg(short = 's', long)]
source: Option<String>,  // Takes a host name as value
```

**RUN** (cli.rs:132):
```rust
#[arg(short, long)]
sudo: bool,  // Boolean flag, no value
```

**EXEC** (cli.rs:150):
```rust
#[arg(short, long)]
sudo: bool,  // Boolean flag, no value
```

**Examples**:
```bash
ssync sync --all -s web1           # ✓ Works: source=web1
ssync run --all -s "whoami"        # ✗ Fails: -s is bool, not source
ssync run --all --sudo "whoami"    # ✓ Works: explicit form
```

### 3. --timeout Not Available for INIT (MAJOR)

**Definition**: cli.rs:54-67
```rust
pub enum Commands {
    Init {
        #[arg(long)]
        update: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_delimiter = ',')]
        skip: Vec<String>,
        // NO timeout field!
    },
    ...
}
```

**Context creation** (mod.rs:65-79):
```rust
pub async fn new_without_targets(verbose: bool, config_path: Option<&Path>) -> Result<Self> {
    let config = crate::config::app::load(config_path)?.unwrap_or_default();
    let db = crate::state::db::open(config.settings.state_dir.as_deref())?;
    let timeout = config.settings.default_timeout;  // NO CLI OVERRIDE
    // ...
}
```

**Examples**:
```bash
ssync init --timeout 60            # ✗ Error: unknown argument
ssync check --all --timeout 60     # ✓ Works
ssync sync --all --timeout 60      # ✓ Works
```

**Impact**: If default timeout is too short, user must:
1. Edit config file
2. Change `default_timeout`
3. Run init
4. Edit config file again
5. Change `default_timeout` back

### 4. Inconsistent --dry-run Support (MODERATE)

**Supported**: INIT (line 61), SYNC (line 106), EXEC (line 162)
**Not supported**: CHECK, CHECKOUT, RUN, LIST, LOG, CONFIG

**Impact**: Users don't know which commands are "safe to test"

### 5. Context Creation Pattern Mismatch (MODERATE)

**Pattern A** (mod.rs:42-62) - TargetArgs:
```rust
pub async fn new(verbose: bool, target: &TargetArgs, config_path: Option<&Path>) -> Result<Self> {
    let timeout = target.timeout.unwrap_or(config.settings.default_timeout);
    // Validates target selection
}
```

**Pattern B** (mod.rs:65-79) - Non-TargetArgs:
```rust
pub async fn new_without_targets(verbose: bool, config_path: Option<&Path>) -> Result<Self> {
    let timeout = config.settings.default_timeout;  // NO CLI OVERRIDE
    // No target validation
}
```

**Impact**: Inconsistent timeout handling between command types

### 6. -h Conflict in LOG (MINOR)

**Definition** (cli.rs:187-188):
```rust
#[arg(short, long)]
host: Option<String>,  // -h to filter by hostname
```

**Meanings**:
- LOG: Filter by hostname
- TargetArgs: Target specific hosts
- Standard: Help flag

**Examples**:
```bash
ssync log -h web1           # Filter logs for host web1
ssync log --help            # Show help
ssync init -h               # Show help (standard)
ssync check -h web1         # Target web1 (TargetArgs)
```

### 7. Other Minor Issues

**Positional Argument Placement**:
- RUN and EXEC require positional args after options
- `ssync run --all "cmd"` works
- `ssync run "cmd" --all` fails

**--serial on Read-Only Commands**:
- LIST, CHECKOUT have `--serial` but don't benefit from it
- Harmless but confusing

## Timeout Resolution Details

### TargetArgs Commands (CHECK, CHECKOUT, SYNC, RUN, EXEC, LIST)

Flow:
```
User input: ssync check --all --timeout 60
  ↓
clap parses: target.timeout = Some(60)
  ↓
Context::new() at mod.rs:50:
  timeout = target.timeout.unwrap_or(config.settings.default_timeout)
  ↓
Result: context.timeout = 60 (CLI wins)
```

Without CLI timeout:
```
User input: ssync check --all
  ↓
clap parses: target.timeout = None
  ↓
Context::new() at mod.rs:50:
  timeout = None.unwrap_or(config.settings.default_timeout)
  ↓
Result: context.timeout = config.settings.default_timeout
```

### Non-TargetArgs Commands (INIT, LOG)

Flow:
```
User input: ssync init
  ↓
clap parses: (no --timeout field)
  ↓
Context::new_without_targets() at mod.rs:68:
  timeout = config.settings.default_timeout
  ↓
Result: context.timeout = config.settings.default_timeout (always)
```

**No override possible for INIT** ⚠️

## Recommendations

### High Priority (Critical Issues)

1. **Add --timeout to INIT**
   - File: `src/cli.rs`
   - Line: ~55
   - Change: Add `timeout: Option<u64>` to Init variant
   - Effort: LOW (< 1 hour)
   - Impact: Solves INIT timeout override limitation

2. **Rename -s in RUN/EXEC**
   - File: `src/cli.rs`
   - Lines: 132, 150
   - Change: Use `-S` (capital) for sudo instead of `-s`
   - Effort: LOW (< 30 min)
   - Impact: Eliminates -s conflict with SYNC

3. **Document -H for Help**
   - Type: Documentation
   - Change: README and help text
   - Effort: LOW (< 15 min)
   - Impact: Reduces user confusion

### Medium Priority (Moderate Issues)

4. **Add --dry-run to CHECK and RUN**
   - Effort: MEDIUM (1-2 hours)
   - Impact: Users can safely test before applying

5. **Standardize Help Flag Mechanism**
   - Effort: MEDIUM (2-3 hours)
   - Impact: Consistent experience across commands

### Low Priority (Nice to Have)

6. Document why LOG uses -h for filtering
7. Document why LIST has --serial but doesn't use it effectively
8. Create visual cheat sheet of all argument combinations

## Source Code References

### Core Files

**src/cli.rs** (215 lines):
- Line 5-22: Cli struct with global args
- Line 25-50: TargetArgs struct definition
- Line 52-198: Commands enum with all subcommands
- Line 48-49: Custom -H help flag definition

**src/commands/mod.rs** (251 lines):
- Line 20-28: TargetMode enum
- Line 31-40: Context struct
- Line 42-62: Context::new() for TargetArgs
- Line 65-79: Context::new_without_targets()
- Line 192-221: resolve_target_mode() validation

**src/main.rs** (101 lines):
- Line 31-99: Command dispatch matching
- Shows how args flow to context creation

### Command Implementations

Each command file contains specific implementation:
- `src/commands/init.rs`: INIT command (lines 13-193)
- `src/commands/check.rs`: CHECK command
- `src/commands/checkout.rs`: CHECKOUT command
- `src/commands/sync.rs`: SYNC command
- `src/commands/run.rs`: RUN command
- `src/commands/exec.rs`: EXEC command
- `src/commands/list.rs`: LIST command
- `src/commands/log.rs`: LOG command
- `src/commands/config.rs`: CONFIG command

## Statistics

- **Total Commands**: 9
- **Commands with TargetArgs**: 6
- **Commands without TargetArgs**: 3
- **Total Unique Arguments**: ~40
- **Global Arguments**: 2 (-v, -c)
- **TargetArgs**: 6 (-a, -h, -g, --serial, --timeout, -H)
- **Command-specific Arguments**: ~32

## Issues Summary

| Severity | Count | Issues |
|----------|-------|--------|
| Critical | 3 | Help flag, -s conflict, --timeout for init |
| Moderate | 3 | --dry-run inconsistency, context mismatch, -h in log |
| Minor | 2 | Positional arg placement, --serial on read-only |

## Conclusion

The SSYNC CLI has a well-structured argument system based on TargetArgs for remote commands and specific arguments for each command. However, three critical inconsistencies should be addressed:

1. The help flag mechanism should be standardized
2. The -s flag conflict between commands should be resolved
3. INIT should support --timeout override

These fixes are low-effort and would significantly improve user experience and reduce command misuse.

---

**Generated**: 2025  
**Analysis Method**: Static code analysis with focus on argument definitions, context creation, and dispatch flow
