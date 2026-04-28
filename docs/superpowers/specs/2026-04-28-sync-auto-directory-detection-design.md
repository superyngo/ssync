# Spec: Sync Auto-Directory Detection

**Date:** 2026-04-28  
**Status:** Approved

## Problem

The `sync` command treats every path as a plain file unless the caller appends a trailing `/`. When a path like `~/.local/bin` is a directory (or a symlink to a directory), two separate issues prevent auto-detection from working:

1. **Symlink bug** — In the Sh shell probe, `find "$p" -maxdepth 1 -type f` does not follow the starting symlink on WSL/DrvFS, so it returns an empty list. Adding `-L` fixes this.
2. **No-source gap** — Step 3.5 (directory expansion) only runs for paths that have a fixed source host (`-s <host>`). Without `-s`, directory paths bypass expansion entirely, causing them to be stat'd as opaque objects rather than listed as files.

## Proposed Changes

### Change 1 — `find -L` in the Sh probe

**File:** `src/commands/sync.rs`, function `build_dir_expand_cmd`, Sh branch

**Before:**
```
find "$p"{depth} -type f 2>/dev/null | sed "s|^$HOME/|~/|" | sort
```

**After:**
```
find -L "$p"{depth} -type f 2>/dev/null | sed "s|^$HOME/|~/|" | sort
```

The `-L` flag tells `find` to follow symbolic links for the starting path. This is safe: GNU find has built-in cycle detection for `-L`. No behavioral change for non-symlink paths.

### Change 2 — Step 3.6: no-source directory auto-detection

**File:** `src/commands/sync.rs`, inserted after Step 3.5 (after the closing brace of the Step 3.5 block, around line 232)

**Logic:**

1. Collect all paths from `all_paths` whose `path_source_map` entry is `None` (no fixed source).
2. If any exist, spawn `expand_directory_paths` against **all reachable hosts in parallel** (respecting `ctx.concurrency()` semaphore).
3. Union the file lists: for each path, if any host returns `DirExpandResult::Directory(files)`, accumulate unique files from all hosts.
4. Replace directory paths in `all_paths` with the unioned file set. Non-directory paths (`File`, `Missing` from all hosts) are left unchanged.
5. Update `path_source_map` for each newly discovered file path (source = `None`).
6. Update `host_applicable_paths` for `--groups` mode (same pattern as Step 3.5).
7. Print `"<path> (empty directory on all hosts, skipping)"` when the union is empty.

### Edge cases

| Situation | Behaviour |
|-----------|-----------|
| Path is a regular file on all hosts | All hosts return `File` → no expansion → unchanged |
| Directory on host A, missing on host B | Only A's files in union; B shows MISSING in Step 4 → conflict strategy resolves normally |
| Directory on host A, regular file on host B | Treated as directory (A wins); B's file will appear as metadata mismatch in Step 4 |
| Empty directory on all hosts | Skipped with message |
| WSL symlink (`~/.local/bin -> /mnt/d/...`) | Fixed by Change 1 (`find -L`) |

## Data Flow

```
CLI paths / config entries
        │
  [Step 3.5]  expand fixed-source directories
        │
  [Step 3.6]  expand no-source directories (new)
        │
  [Step 4]    batch collect metadata (mtime + hash)
        │
  [Step 5]    make sync decisions
        │
  [Step 6]    distribute (download + upload)
```

## Files Changed

- `src/commands/sync.rs` — `build_dir_expand_cmd` (1-line change) + Step 3.6 block (~40 lines)

## Testing

- Unit test for `build_dir_expand_cmd` Sh output: verify `-L` appears in the generated command.
- Integration scenario: run sync with a symlinked directory path without trailing `/` — should expand correctly.
- Run existing test suite (`cargo test`) to verify no regressions.
