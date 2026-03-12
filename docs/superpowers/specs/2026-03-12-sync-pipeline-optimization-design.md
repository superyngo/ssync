# Sync Pipeline Optimization Design

**Date**: 2026-03-12  
**Status**: Approved  
**Scope**: Optimize multi-file sync flow with connection pooling, parallel pipeline, live progress, and per-file failure isolation

## Problem Statement

The current sync command has several performance and reliability limitations:

1. **No SSH connection reuse** — every SSH/SCP operation spawns a fresh process with full handshake overhead
2. **Sequential file processing** — files are synced one at a time through all 3 stages; no overlap
3. **Per-file SSH calls for metadata** — collecting metadata requires `hosts × files` SSH calls
4. **Abort-on-error** — when a fixed source host lacks a file, the entire sync aborts
5. **No live progress** — output is line-by-line after each operation completes
6. **No connection pre-check** — unreachable hosts cause repeated timeout waits per file
7. **No per-host concurrency limits** — a single host could be overwhelmed with concurrent operations

## Out of Scope

- **Recursive directory sync** (`recursive: true`) and **propagate_deletes**: These require runtime file discovery on remote hosts, which is fundamentally different from the known-file-list approach. They continue to work with the existing per-file flow (not batched). A future design may optimize these separately.
- **SHA-256 vs BLAKE3**: The DB column is named `blake3` but the implementation uses SHA-256 (`sha256sum`/`shasum`). This naming discrepancy predates this design and is not addressed here.

## Approach: Hybrid Batched-Collect + Parallel-Distribute

Three approaches were evaluated:

- **A: Full Pipeline (Producer-Consumer)** — maximum throughput but most complex
- **B: Batched Parallel** — simplest but no pipeline overlap between phases
- **C: Hybrid** — batch metadata collection (1 SSH/host), parallel distribution with concurrency limits

**Selected: Approach C** — captures ~90% of performance gains with significantly less complexity. The key insight is that metadata collection benefits most from batching (eliminates N-1 SSH round trips per host), while distribution benefits most from parallelism (SCP transfers are the bottleneck).

## Design

### 1. SSH Connection Manager (ControlMaster)

**New module**: `src/host/connection.rs`

```rust
pub struct ConnectionManager {
    socket_dir: tempfile::TempDir,
    hosts: HashMap<String, ConnectionState>,
}

pub enum ConnectionState {
    Connected { socket_path: PathBuf },
    Failed { error: String },
}
```

**Behavior**:

- `pre_check(hosts, timeout, concurrency)` establishes ControlMaster connections in parallel:
  ```
  ssh -o ControlMaster=auto -o ControlPath=<socket> -o ControlPersist=300s -N -f <host>
  ```
- All subsequent ssh/scp calls receive `-o ControlPath=<socket>` automatically
- `reachable_hosts()` returns only hosts with `ConnectionState::Connected`
- `cleanup()` on Drop: uses blocking `std::process::Command` to send `ssh -O exit` per master, then removes socket dir. An explicit `async fn shutdown()` is provided as the preferred cleanup path — callers should call it before drop when possible. Drop serves as a safety net.
- ControlPersist=300s keeps idle connections alive for 5 minutes
- **Socket path length**: macOS limits Unix domain socket paths to ~104 bytes. Socket dir uses `/tmp/ssync-XXXX/` with hashed hostnames (`%C` or truncated SHA) to stay under the limit.

**Integration with executor.rs**: New function signatures accept an optional `&Path` for the control socket:

```rust
pub async fn run_remote_pooled(host, command, timeout, socket: Option<&Path>) -> Result<RemoteOutput>
pub async fn upload_pooled(host, local_path, remote_path, timeout, socket: Option<&Path>) -> Result<()>
pub async fn download_pooled(host, remote_path, local_path, timeout, socket: Option<&Path>) -> Result<()>
```

### 2. Concurrency Model

**New module**: `src/host/concurrency.rs`

```rust
pub struct ConcurrencyLimiter {
    global: Arc<Semaphore>,
    per_host: HashMap<String, Arc<Semaphore>>,
}

impl ConcurrencyLimiter {
    pub async fn acquire(&self, host: &str) -> (OwnedSemaphorePermit, OwnedSemaphorePermit)
}
```

**Configuration** (in `[settings]`):

```toml
max_concurrency = 10              # global cap (unchanged default)
max_per_host_concurrency = 4      # per-host cap (new setting)
# or: max_per_host_concurrency = "auto"  → min(4, max_concurrency / host_count)
```

**Rules**:

- Every SSH/SCP operation acquires **both** a global permit and a per-host permit
- Acquisition order: global first, then per-host (deterministic to prevent deadlock)
- `--serial` sets both to 1
- Auto-tune: `min(4, max_concurrency / host_count)` provides even distribution

### 3. Batched Metadata Collection

**Current**: `hosts × files` SSH calls (1 per host per file).  
**New**: `hosts` SSH calls (1 per host for all files).

**Path escaping**: Follows the same strategy as the current code — `$HOME` left unquoted for shell expansion, rest single-quoted with `'\''` escaping:

**Shell command** (Sh / Cmd fallback):
```sh
# Paths are escaped per-item: $HOME/'path with spaces'/'file.txt'
for f in $HOME/'.bashrc' $HOME/'.vimrc' $HOME/'.gitconfig'; do
  echo "---FILE:$f"
  stat -c '%Y %s' "$f" 2>/dev/null || stat -f '%m %z' "$f" 2>/dev/null || echo "MISSING"
  (sha256sum "$f" 2>/dev/null || shasum -a 256 "$f" 2>/dev/null) || echo "NOHASH"
done
```

**PowerShell equivalent**:
```powershell
foreach ($f in @("$HOME\.bashrc","$HOME\.vimrc","$HOME\.gitconfig")) {
  "---FILE:$f"
  $i=Get-Item $f -ErrorAction SilentlyContinue
  if ($i) {
    [int64](($i.LastWriteTimeUtc-[datetime]"1970-01-01").TotalSeconds), $i.Length -join " "
    (Get-FileHash $f -Algorithm SHA256).Hash.ToLower()
  } else { "MISSING" }
}
```

**Cmd shell**: Treated as `Sh` fallback (same as current code: `ShellType::Sh | ShellType::Cmd`). The `for` loop uses POSIX syntax which works on Cmd hosts that have a POSIX-compatible shell available (the same assumption the current code makes).

**Output parsing**: Split by `---FILE:` markers, parse each block with current single-file logic.

**Result**:
```rust
struct BatchCollectResult {
    per_file: HashMap<String, CollectResult>,
    unreachable_hosts: Vec<String>,
}
```

**Note**: Batching only applies to non-recursive sync entries with a known file list. Recursive entries (`recursive: true`) continue to use the existing per-file collection path.

### 4. Per-File Failure Isolation

**Change in `make_decisions_fixed_source()`**:

```rust
// BEFORE: aborts entire sync
let source = file_infos.iter().find(|f| f.host == source_name)
    .ok_or_else(|| anyhow::anyhow!("Source host '{}' has no data...", ...))?;

// AFTER: skip this file, continue others
let source = match file_infos.iter().find(|f| f.host == source_name) {
    Some(s) => s,
    None => {
        // Return empty decisions — caller records the skip
        return Ok(Vec::new());
    }
};
```

The caller (`sync_path_across`) handles the skip:
```rust
if decisions.is_empty() && source_override.is_some() {
    // Check if source was missing from collect results
    if !collect_result.found.iter().any(|f| f.host == source_name) {
        summary.add_skip_with_reason(path, &format!(
            "source '{}' does not have '{}'", source_name, path
        ));
        continue;  // next file
    }
}
```

**Enhanced Summary**:
```rust
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>,        // (host, message)
    pub skip_reasons: Vec<SkipReason>,        // NEW
}

pub struct SkipReason {
    pub path: String,
    pub host: String,   // source host that lacked the file
    pub reason: String,
}
```

**Error semantics**:

| Scenario | Behavior |
|----------|----------|
| Host unreachable in pre-check | Excluded from everything, reported in summary |
| Source host unreachable | Abort sync (can't proceed without source) |
| Source host lacks specific file | Skip that file, continue others |
| Download failure for one file | That file fails, others continue |
| Upload failure to one target | Other targets for same file continue |

### 5. Live Progress Display (indicatif)

**New module**: `src/output/progress.rs`

```rust
pub struct SyncProgress {
    multi: MultiProgress,
    host_bar: ProgressBar,
    collect_bar: ProgressBar,
    transfer_bars: HashMap<String, ProgressBar>,
}
```

**Display layout**:
```
── Sync: my-group ──
 Hosts:    8 connected, 2 failed                   [██████████░░] 10/10
 Files:    collecting metadata...                   [████░░░░░░░░]  3/12
 Transfer: ~/.bashrc → 6 hosts                     [██████████░░]  5/6
 Transfer: ~/.vimrc → 4 hosts                      [████░░░░░░░]   2/4
```

**Behavior per phase**:

- **Step 2** (pre-check): host bar fills as connections establish
- **Step 4** (collect): collect bar fills as hosts respond with metadata
- **Step 7** (distribute): one transfer bar per in-flight file, shows `filename → N/M hosts`
- Bars auto-remove when complete
- Final summary prints after all bars finish

**Fallback**: When stderr is not a TTY (piped), use current line-based output.

### 6. Revised Sync Pipeline (Complete Flow)

```
1. Resolve hosts and files from config/CLI args
   → Thread push_missing (from --no-push-missing flag) through the pipeline
   → Separate recursive entries (use existing per-file flow) from non-recursive (use batched flow)

2. ConnectionManager::pre_check(hosts)
   → Establish ControlMaster connections in parallel
   → Record reachable vs failed hosts
   → Progress: "Hosts: 8 connected, 2 failed [██████████░░]"

3. Filter to reachable_hosts only
   → If source_override host is unreachable → fail entire sync with clear error

4. batch_collect_metadata(reachable_hosts, all_non_recursive_files)
   → One SSH call per host, parse all file metadata
   → Progress: "Files: collecting... [████░░░░]"
   → Result: HashMap<path, CollectResult>

5. For each file: make_decisions()
   → If fixed source missing file → skip (not abort), record in summary
   → Thread push_missing flag into decision functions
   → Produce Vec<SyncDecision>

6. If --dry-run: print decisions and skip to step 10

7. distribute_all(decisions, concurrency_limiter)
   → Spawn all file distributions in parallel
   → Each: download(source) → parallel upload(targets)
   → Per-host concurrency respected via ConcurrencyLimiter
   → Progress: per-file transfer bars
   → Failures isolated per-file

8. Update database state
   → Write sync_state for succeeded transfers (INSERT ... ON CONFLICT DO UPDATE)
   → Write operation_log entries for audit trail

9. Cleanup: ConnectionManager::shutdown() (async), then drop as safety net

10. Print final Summary (succeeded/failed/skipped with details)
```

**Ad-hoc file mode (`--files/-f`)**: Uses the same optimized pipeline. Even for a single file, the connection pre-check and ControlMaster pooling provide value by verifying connectivity upfront. Batching provides no benefit for a single file but the code path is unified — a 1-file batch works correctly.

**Default concurrency**: `max_concurrency` default remains at 10 (unchanged from current behavior). Users can increase it in config. New `max_per_host_concurrency` defaults to 4.

## Files to Create/Modify

### New Files
- `src/host/connection.rs` — SSH ConnectionManager with ControlMaster pooling
- `src/host/concurrency.rs` — Dual-level concurrency limiter
- `src/output/progress.rs` — indicatif-based live progress display

### Modified Files
- `src/host/executor.rs` — Add `_pooled` variants accepting socket path
- `src/host/mod.rs` — Export new modules
- `src/commands/sync.rs` — Rewrite pipeline flow (largest change)
- `src/output/summary.rs` — Add `skip_reasons` field and display
- `src/config/schema.rs` — Add `max_per_host_concurrency` setting
- `Cargo.toml` — Add `indicatif` dependency

### Unchanged
- `src/cli.rs` — No new CLI flags needed (all config-based)
- `src/host/shell.rs` — Shell type logic unchanged
- `src/host/filter.rs` — Host filtering unchanged
- `src/state/` — Database schema unchanged

## Testing Strategy

- Unit tests for batch metadata command generation (per shell type)
- Unit tests for batch output parsing (including missing files, mixed results)
- Unit tests for `make_decisions_fixed_source` skip behavior
- Unit tests for ConcurrencyLimiter (deadlock-free, correct limiting)
- Integration test: ConnectionManager with mock SSH (or skip in CI)
- Existing tests must continue to pass
