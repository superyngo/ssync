# SSYNC Codebase Analysis - Quick Start Guide

This directory contains three comprehensive analysis documents of the SSYNC codebase.

## 📚 Documents Overview

### 1. [CODEBASE_ANALYSIS.md](./CODEBASE_ANALYSIS.md) - **Main Reference** ⭐
The comprehensive architectural analysis document (652 lines).

**What's inside:**
- Complete overview of the SSYNC system
- Detailed breakdown of all 16 code components
- Config schema structures (AppConfig, HostEntry, SyncFile, etc.)
- CLI argument definitions with all subcommands
- Command implementation details (sync, check, checkout, init, run, exec, log, config)
- Main entry point and command dispatch routing
- Metrics collector logic
- Key architectural patterns (target selection, file sync scoping, concurrency model)
- Database schema overview
- Code statistics table

**Best for:**
- Understanding overall system architecture
- Planning new features
- Understanding component interactions
- Learning how target selection works (--group/--host/--all)
- Understanding the 3-stage sync pipeline

**Time to read:** 30-45 minutes

---

### 2. [FILE_CONTENTS_REFERENCE.md](./FILE_CONTENTS_REFERENCE.md) - **Code Reference**
Complete source code of critical files (593 lines).

**What's inside:**
- Full Rust source code for:
  - src/config/schema.rs (141 lines) - TOML data structures
  - src/config/app.rs (120 lines) - Config file I/O
  - src/cli.rs (204 lines) - Clap CLI definitions
  - src/host/filter.rs (89 lines) - Host filtering logic
- Summary references to other files with line counts
- Inline code comments from original files

**Best for:**
- Quick code reference when editing
- Understanding exact implementations
- Copying patterns and boilerplate
- Learning data structure definitions
- Understanding CLI argument parsing

**Time to read:** Quick lookups as needed

---

### 3. [EXPLORATION_COMPLETE.md](./EXPLORATION_COMPLETE.md) - **Navigation Guide**
Navigation and quick reference document (294 lines).

**What's inside:**
- Quick reference to file locations and purposes
- Organized by subsystem (Configuration, CLI, Commands, Infrastructure, etc.)
- File size and complexity ratings
- Key concepts explained (target selection, file sync scoping, config structure)
- Concurrency model overview
- Database tables explanation
- Platform support details
- Common code patterns with examples
- Testing entry points
- Next steps for development

**Best for:**
- Getting oriented in the codebase
- Finding where specific functionality lives
- Understanding key concepts
- Learning common patterns
- Planning development work

**Time to read:** 15-20 minutes for full guide, quick lookups after

---

## 🚀 Quick Start by Goal

### I want to understand the system design
1. Read EXPLORATION_COMPLETE.md (Quick Reference section) - 10 min
2. Read CODEBASE_ANALYSIS.md sections 1-6 - 20 min
3. Look at specific files in FILE_CONTENTS_REFERENCE.md as needed

### I want to add a new metric
1. Check EXPLORATION_COMPLETE.md "Metrics System" location
2. Look at src/metrics/probes/ (mentioned in CODEBASE_ANALYSIS.md section 7)
3. Study similar existing metrics
4. Reference CODEBASE_ANALYSIS.md section 7 for collector architecture

### I want to add a new config option
1. Read CODEBASE_ANALYSIS.md section 1 (schema.rs)
2. Review FILE_CONTENTS_REFERENCE.md schema.rs code
3. Find where config is used in commands/ (see CODEBASE_ANALYSIS.md)
4. Add to schema, update users, add to comments in app.rs

### I want to create a new subcommand
1. Read CODEBASE_ANALYSIS.md sections 3 and 5 (CLI and Commands)
2. Review FILE_CONTENTS_REFERENCE.md cli.rs code
3. Copy pattern from similar command (see CODEBASE_ANALYSIS.md section 5)
4. Implement in commands/{name}.rs

### I want to understand file sync (complex feature)
1. Read EXPLORATION_COMPLETE.md "Understanding Sync Pipeline" section
2. Read CODEBASE_ANALYSIS.md section 5b (sync.rs detailed)
3. Review actual code from FILE_CONTENTS_REFERENCE.md if needed
4. Study the 3-stage pattern: collect → decide → distribute

---

## 🗺️ System Map

```
ssync/
├── src/config/           ← Configuration system
│   ├── schema.rs         (141 lines) - TOML structures
│   ├── app.rs            (120 lines) - Config I/O
│   └── ssh_config.rs     - ~/.ssh/config parsing
│
├── src/cli.rs            (204 lines) - CLI argument parsing
│
├── src/commands/         ← Command implementations
│   ├── mod.rs            (178 lines) - Context + target resolution
│   ├── sync.rs           (589 lines) ⭐ 3-stage file sync
│   ├── check.rs          (150 lines) - Metric collection
│   ├── checkout.rs       (510 lines) - Report generation
│   ├── init.rs           (125 lines) - Host discovery
│   ├── run.rs            (90 lines)  - Command execution
│   ├── exec.rs           (218 lines) - Script execution
│   ├── config.rs         (35 lines)  - Config editor
│   └── log.rs            (114 lines) - Operation logs
│
├── src/host/             ← Remote execution
│   ├── filter.rs         (89 lines)  - Host filtering
│   ├── executor.rs       - SSH execution
│   └── shell.rs          - Shell type detection
│
├── src/metrics/          ← System metrics
│   ├── collector.rs      (100 lines) - Metric coordination
│   ├── parser.rs         - Parse metric outputs
│   └── probes/           - Shell-specific commands
│
├── src/state/            ← Database
│   ├── db.rs             - SQLite initialization
│   └── retention.rs      - Data cleanup
│
├── src/output/           ← Output formatting
│   ├── printer.rs        - Host-prefixed output
│   └── summary.rs        (43 lines)  - Summary stats
│
└── src/main.rs           (96 lines)  - Entry point
```

---

## 🔑 Key Architectural Patterns

### Target Selection (Critical to understand)
```
--group web        →  TargetMode::Groups(["web"])
--host web1        →  TargetMode::Hosts(["web1"])
--all              →  TargetMode::All
(only ONE allowed per command)
    ↓
Context::resolve_hosts()  →  Filter via host.groups membership
    ↓
Vec<&HostEntry>  →  Selected hosts for operation
```

### File Sync Scoping
```
[[sync.file]] with groups=[]           →  --all or --host mode only
[[sync.file]] with groups=["web"]      →  --group web only
Selection is intersection of file's groups AND CLI --group
```

### 3-Stage Sync Pipeline
```
Stage 1: Collect metadata (mtime, hash) from all hosts in parallel
         ↓
Stage 2: Decide which host is source (conflict strategy: Newest/Skip)
         ↓
Stage 3: Distribute file from source to targets via local relay
```

### Concurrency Model
```
Default:   Semaphore(max_concurrency)     ← parallel
--serial:  Semaphore(1)                   ← sequential
```

---

## 📊 Document Statistics

| Document | Lines | Focus | Read Time |
|----------|-------|-------|-----------|
| CODEBASE_ANALYSIS.md | 652 | Architecture + Implementation | 30-45 min |
| FILE_CONTENTS_REFERENCE.md | 593 | Source Code | As needed |
| EXPLORATION_COMPLETE.md | 294 | Navigation | 15-20 min |
| **Total** | **1,539** | **Complete Analysis** | **45-65 min** |

Source code coverage: 3,500+ lines of Rust analyzed

---

## 💡 Common Patterns in Codebase

1. **Semaphore-based concurrency** - Used in all parallel operations
2. **Shell-specific command generation** - Match on ShellType enum
3. **Host-prefixed output** - Use `printer::print_host_line()`
4. **Error handling with hints** - Show available groups/hosts on failure
5. **Database logging** - Log all significant operations
6. **Result-based error handling** - Use `anyhow::Result<T>`

See EXPLORATION_COMPLETE.md "Common Patterns in Codebase" for examples.

---

## 🎯 Development Workflow

1. **Understand requirements** → Read relevant sections of CODEBASE_ANALYSIS.md
2. **Find similar code** → Use EXPLORATION_COMPLETE.md file locations
3. **Review implementation** → Check FILE_CONTENTS_REFERENCE.md for patterns
4. **Implement changes** → Follow patterns from similar features
5. **Test** → Use `--dry-run` flags where available
6. **Review** → Compare with documented patterns

---

## 📖 Citation Conventions

When referring to code:
- "See CODEBASE_ANALYSIS.md section 5b (sync.rs)" for architecture
- "See FILE_CONTENTS_REFERENCE.md schema.rs" for exact implementation
- "See EXPLORATION_COMPLETE.md file locations" for finding files

---

## 🏗️ Understanding Key Features

### Config System
- Start: CODEBASE_ANALYSIS.md sections 1-2
- Code: FILE_CONTENTS_REFERENCE.md (schema.rs, app.rs)
- Concepts: EXPLORATION_COMPLETE.md Configuration Structure

### Target Selection
- Start: EXPLORATION_COMPLETE.md Target Selection section
- Architecture: CODEBASE_ANALYSIS.md section 5a (commands/mod.rs)
- Code: Implementation in Context::resolve_hosts()

### File Sync
- Overview: EXPLORATION_COMPLETE.md Understanding Sync Pipeline
- Architecture: CODEBASE_ANALYSIS.md section 5b (sync.rs)
- Code: src/commands/sync.rs (see analysis for function breakdown)

### Metrics Collection
- Architecture: CODEBASE_ANALYSIS.md sections 5c and 7
- Implementation: src/metrics/collector.rs
- Data flow: Check → Collector → Parser → Storage

### Report Generation
- Architecture: CODEBASE_ANALYSIS.md section 5d (checkout.rs)
- Formats: JSON, HTML, Table, TUI (interactive)
- Extraction: See metric extraction functions

---

## ❓ FAQ

**Q: Where is the file sync pipeline?**
A: src/commands/sync.rs (589 lines). See CODEBASE_ANALYSIS.md section 5b and EXPLORATION_COMPLETE.md "Understanding Sync Pipeline".

**Q: How are hosts selected?**
A: Via TargetMode enum. See CODEBASE_ANALYSIS.md section 5a (commands/mod.rs) for resolve_target_mode().

**Q: How do I add a new metric?**
A: Update src/metrics/probes/{sh,powershell,cmd}.rs and parser.rs. See CODEBASE_ANALYSIS.md section 7.

**Q: Where are database tables defined?**
A: src/state/db.rs (not included in analysis but schema is in CODEBASE_ANALYSIS.md).

**Q: How does concurrency work?**
A: Tokio Semaphore with max_concurrency setting. See patterns in any command file.

**Q: Where are config options documented?**
A: In src/config/app.rs inject_config_comments(). Also in schema.rs field comments.

---

Generated: March 2025
Codebase: SSYNC (SSH-config-based remote management tool)
Status: Complete exploration with full documentation

For questions, see the relevant analysis document sections listed above.
