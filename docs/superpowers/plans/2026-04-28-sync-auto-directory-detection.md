# Sync Auto-Directory Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two bugs so `sync` automatically detects and expands directory paths regardless of trailing slash or symlink type.

**Architecture:** Two surgical changes in `src/commands/sync.rs` only: (1) add `-L` to the `find` command so it follows symlinks; (2) add a Step 3.6 block that expands directory paths that have no fixed source (`-s` flag absent), probing all hosts in parallel and unioning results.

**Tech Stack:** Rust, tokio, rusqlite (no new deps)

---

## Background

Two bugs cause the trailing-slash discrepancy:

**Bug 1 – `find` does not follow symlinks.**
In `build_dir_expand_cmd` (Sh branch, ~line 1327):
```
find "$p" -maxdepth 1 -type f
```
When `$p` is itself a symlink directory (e.g. `~/.local/bin → /mnt/d/...`), POSIX `find` without `-L` does not follow the symlink for the starting path on Linux/WSL/DrvFS. Result: zero files found, "empty directory" shown.
Trailing `/` works because the OS resolves the final symlink component before `find` receives the path.

**Bug 2 – Step 3.5 only expands directories when `-s` is set.**
Step 3.5 (lines 137–232) expands directories only for paths with a fixed source. When no `-s` flag is given, all paths have `None` source and the entire expansion block is skipped.

---

## File Map

| File | Change |
|------|--------|
| `src/commands/sync.rs:1327` | Add `-L` flag to `find` in Sh branch |
| `src/commands/sync.rs:232` | Insert new helper `union_dir_expansions` before `run()` (or in same file), insert Step 3.6 block after existing Step 3.5 |
| `src/commands/sync.rs` (test section ~1884) | Update existing test + add two new tests |

No other files need changes.

---

## Task 1: Fix symlink-following in `find` (Bug 1)

**Files:**
- Modify: `src/commands/sync.rs:1327`
- Test: `src/commands/sync.rs` (test `test_build_dir_expand_cmd_sh_shallow`, ~line 1791)

### Step 1.1: Update the failing test to assert `-L`

Open `src/commands/sync.rs` and find `test_build_dir_expand_cmd_sh_shallow` (~line 1791). Add an assertion that the command contains `find -L`:

```rust
#[test]
fn test_build_dir_expand_cmd_sh_shallow() {
    let paths = vec!["~/mydir".to_string(), "~/single.conf".to_string()];
    let cmd = build_dir_expand_cmd(&paths, false, ShellType::Sh);
    assert!(cmd.contains("---PATH:"));
    assert!(cmd.contains("[ -d"));
    assert!(cmd.contains("-maxdepth 1"));
    assert!(cmd.contains("find"));
    assert!(cmd.contains("find -L"), "find must use -L to follow symlinks");
    assert!(cmd.contains("mydir"));
    assert!(cmd.contains("single.conf"));
}
```

- [ ] Replace the existing `test_build_dir_expand_cmd_sh_shallow` body with the above.

### Step 1.2: Run the test to verify it fails

```powershell
cargo test test_build_dir_expand_cmd_sh_shallow -- --nocapture
```

Expected: **FAIL** with assertion `find must use -L to follow symlinks`.

- [ ] Confirm the test fails.

### Step 1.3: Add `-L` to `find` in the Sh branch

In `src/commands/sync.rs`, around line 1327, the current line is:
```rust
               find \"$p\"{depth} -type f 2>/dev/null | sed \"s|^$HOME/|~/|\" | sort; \
```

Change it to:
```rust
               find -L \"$p\"{depth} -type f 2>/dev/null | sed \"s|^$HOME/|~/|\" | sort; \
```

Full surrounding context for the edit (lines 1320–1336):
```rust
            let depth_flag = if recursive { "" } else { " -maxdepth 1" };
            // Detect type, list files, normalize $HOME → ~
            format!(
                "for p in {files}; do \
                   orig=$(echo \"$p\" | sed \"s|^$HOME/|~/|;s|^$HOME$|~|\"); \
                   echo \"---PATH:$orig\"; \
                   if [ -d \"$p\" ]; then \
                     echo \"DIR\"; \
                     find -L \"$p\"{depth} -type f 2>/dev/null | sed \"s|^$HOME/|~/|\" | sort; \
                   elif [ -e \"$p\" ]; then \
                     echo \"FILE\"; \
                   else \
                     echo \"MISSING\"; \
                   fi; \
                 done",
                files = expanded.join(" "),
                depth = depth_flag
            )
```

- [ ] Make the edit.

### Step 1.4: Run the test to verify it passes

```powershell
cargo test test_build_dir_expand_cmd_sh_shallow -- --nocapture
```

Expected: **PASS**.

- [ ] Confirm the test passes.

### Step 1.5: Also update the recursive test to check `-L`

In `test_build_dir_expand_cmd_sh_recursive` (~line 1803):

```rust
#[test]
fn test_build_dir_expand_cmd_sh_recursive() {
    let paths = vec!["~/mydir".to_string()];
    let cmd = build_dir_expand_cmd(&paths, true, ShellType::Sh);
    assert!(cmd.contains("---PATH:"));
    assert!(cmd.contains("find"));
    assert!(cmd.contains("find -L"), "find must use -L to follow symlinks");
    assert!(
        !cmd.contains("-maxdepth"),
        "recursive should not have maxdepth"
    );
}
```

- [ ] Make the edit.

### Step 1.6: Run all dir-expand tests

```powershell
cargo test test_build_dir_expand_cmd -- --nocapture
```

Expected: all pass.

- [ ] Confirm all pass.

### Step 1.7: Run full test suite + clippy

```powershell
cargo test
cargo clippy --all-targets
```

Expected: all tests pass, no clippy errors.

- [ ] Confirm.

### Step 1.8: Commit

```powershell
git add src/commands/sync.rs
git commit -m "fix: follow symlinks in find when expanding directory paths

Use `find -L` in the Sh branch of build_dir_expand_cmd so that when a
path is itself a symlink to a directory (e.g. ~/.local/bin -> /mnt/d/...
on WSL/DrvFS), find correctly traverses its contents instead of returning
empty results.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

- [ ] Commit.

---

## Task 2: Add Step 3.6 – no-source directory auto-detection (Bug 2)

**Files:**
- Modify: `src/commands/sync.rs` (~line 232, after Step 3.5 closing brace, and just before `run()` or in the free-function section near `expand_directory_paths`)
- Test: `src/commands/sync.rs` (test section, ~line 1884)

### Step 2.1: Write a failing unit test for `union_dir_expansions`

Add this test to the `#[cfg(test)]` module at the end of `sync.rs` (after the last `}` of `test_parse_dir_expand_output_nested`, before the module's closing `}`):

```rust
#[test]
fn test_union_dir_expansions_merges_files() {
    let host_a = HashMap::from([
        (
            "~/bin".to_string(),
            DirExpandResult::Directory(vec!["~/bin/f1".to_string(), "~/bin/f2".to_string()]),
        ),
        ("~/.vimrc".to_string(), DirExpandResult::File),
    ]);
    let host_b = HashMap::from([
        (
            "~/bin".to_string(),
            DirExpandResult::Directory(vec!["~/bin/f2".to_string(), "~/bin/f3".to_string()]),
        ),
        ("~/.vimrc".to_string(), DirExpandResult::File),
    ]);
    let result = union_dir_expansions(vec![host_a, host_b]);
    // Only directory paths end up in the result; files are excluded
    assert_eq!(result.len(), 1, "only ~/bin should be in result");
    let files = result.get("~/bin").expect("~/bin must be present");
    assert_eq!(files.len(), 3, "union must deduplicate f2");
    assert!(files.contains(&"~/bin/f1".to_string()));
    assert!(files.contains(&"~/bin/f2".to_string()));
    assert!(files.contains(&"~/bin/f3".to_string()));
}

#[test]
fn test_union_dir_expansions_empty_on_one_host() {
    // If one host sees an empty directory and another sees files, union has those files
    let host_a = HashMap::from([(
        "~/bin".to_string(),
        DirExpandResult::Directory(vec![]),
    )]);
    let host_b = HashMap::from([(
        "~/bin".to_string(),
        DirExpandResult::Directory(vec!["~/bin/tool".to_string()]),
    )]);
    let result = union_dir_expansions(vec![host_a, host_b]);
    let files = result.get("~/bin").expect("~/bin must be present");
    assert_eq!(files.len(), 1);
    assert!(files.contains(&"~/bin/tool".to_string()));
}
```

- [ ] Add the two tests.

### Step 2.2: Run the tests to verify they fail

```powershell
cargo test test_union_dir_expansions -- --nocapture
```

Expected: **compile error** — `union_dir_expansions` does not exist yet.

- [ ] Confirm the compile error.

### Step 2.3: Add the `union_dir_expansions` free function

Add the following function to `sync.rs`, just before `expand_directory_paths` (which is around line 1413 before your edits). A good anchor is the line containing `async fn expand_directory_paths`:

```rust
/// Merges per-host `DirExpandResult` maps into a single map of path → union of file lists.
/// Only paths classified as Directory on at least one host appear in the result.
/// File and Missing results are excluded (callers keep those paths unchanged).
fn union_dir_expansions(
    host_results: Vec<HashMap<String, DirExpandResult>>,
) -> HashMap<String, Vec<String>> {
    let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
    for expansions in host_results {
        for (path, result) in expansions {
            if let DirExpandResult::Directory(files) = result {
                let entry = dirs.entry(path).or_default();
                for f in files {
                    if !entry.contains(&f) {
                        entry.push(f);
                    }
                }
            }
        }
    }
    dirs
}
```

- [ ] Insert the function.

### Step 2.4: Run the tests to verify they pass

```powershell
cargo test test_union_dir_expansions -- --nocapture
```

Expected: **PASS** (both tests).

- [ ] Confirm.

### Step 2.5: Add Step 3.6 block in `run()`

Insert the following block immediately after the closing `}` of Step 3.5 (line 232), before the blank line and the `// Step 4:` comment:

```rust
    // Step 3.6: Expand directory paths for entries with no fixed source.
    // Probes all reachable hosts in parallel and unions the file lists so that
    // files present on any host are included in the sync set.
    {
        let no_source_paths: Vec<String> = all_paths
            .iter()
            .filter(|p| {
                path_source_map
                    .get(p.as_str())
                    .map_or(true, |s| s.is_none())
            })
            .cloned()
            .collect();

        if !no_source_paths.is_empty() {
            let semaphore = Arc::new(Semaphore::new(ctx.concurrency()));
            let mut handles = Vec::new();

            for host in &reachable_hosts {
                let host = (*host).clone();
                let paths = no_source_paths.clone();
                let sessions = Arc::clone(&pool.session_pool);
                let timeout = ctx.timeout;
                let sem = semaphore.clone();

                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    expand_directory_paths(&host, &paths, false, timeout, &sessions).await
                }));
            }

            let mut host_results: Vec<HashMap<String, DirExpandResult>> = Vec::new();
            for handle in handles {
                match handle.await {
                    Ok(Ok(expansions)) => host_results.push(expansions),
                    Ok(Err(e)) => tracing::warn!(error = %e, "Failed to expand directories"),
                    Err(e) => tracing::warn!(error = %e, "Directory expand task panicked"),
                }
            }

            let dirs_expanded = union_dir_expansions(host_results);

            if !dirs_expanded.is_empty() {
                let mut new_paths = Vec::new();
                for path in &all_paths {
                    if let Some(expanded_files) = dirs_expanded.get(path) {
                        for file_path in expanded_files {
                            if !new_paths.contains(file_path) {
                                new_paths.push(file_path.clone());
                                path_source_map.entry(file_path.clone()).or_insert(None);
                            }
                        }
                        // Update host_applicable_paths: replace dir with expanded files
                        if let Some(ref mut host_map) = host_applicable_paths {
                            for (_host, path_set) in host_map.iter_mut() {
                                if path_set.remove(path) {
                                    for file_path in expanded_files {
                                        path_set.insert(file_path.clone());
                                    }
                                }
                            }
                        }
                        if expanded_files.is_empty() {
                            println!("  {} (empty directory on all hosts, skipping)", path);
                        }
                    } else {
                        new_paths.push(path.clone());
                    }
                }
                all_paths = new_paths;
            }
        }
    }
```

The anchor for insertion: find the exact text `}` followed by a blank line then `    // Step 4: Batch collect metadata` in the file.

- [ ] Insert the Step 3.6 block.

### Step 2.6: Run `cargo check` to verify it compiles

```powershell
cargo check
```

Expected: no errors.

- [ ] Confirm.

### Step 2.7: Run the full test suite

```powershell
cargo test
```

Expected: all tests pass (same count as baseline + the 2 new tests).

- [ ] Confirm.

### Step 2.8: Run clippy

```powershell
cargo clippy --all-targets
```

Expected: no warnings or errors.

- [ ] Confirm.

### Step 2.9: Commit

```powershell
git add src/commands/sync.rs
git commit -m "feat: auto-detect directory paths without fixed source in sync

Step 3.5 only expanded directory paths when a fixed source (-s) was
provided. Without -s, directory paths were never expanded and showed
'empty directory on source, skipping'.

Add Step 3.6 which probes all reachable hosts in parallel (respecting
--serial / max_concurrency), unions the file lists with the new
union_dir_expansions helper, and replaces directory paths with their
expanded file lists. Paths that are not directories on any host are
passed through unchanged.

Fixes: sync -f ~/.local/bin (without trailing slash, without -s)

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

- [ ] Commit.

---

## Self-Review Checklist

- [x] Bug 1 (find -L) → Task 1, Steps 1.3
- [x] Bug 2 (no-source dirs) → Task 2, Step 2.5
- [x] Symlink on source with -s → also covered by Task 1 (find -L applies in Step 3.5 expansion too)
- [x] `host_applicable_paths` updated in Step 3.6 → Step 2.5 code
- [x] `path_source_map` updated for new file paths → Step 2.5 code (`or_insert(None)`)
- [x] Empty directory message → Step 2.5 code (`empty directory on all hosts, skipping`)
- [x] Tests written before implementation (TDD) → Steps 1.1, 2.1 come before implementation steps
- [x] `union_dir_expansions` tested in isolation → Steps 2.1–2.4
- [x] No placeholder code

---

## Notes

- `find -L` with GNU find is safe — it has built-in cycle detection (ignores symlink loops via `ELOOP`).
- PowerShell and Cmd branches of `build_dir_expand_cmd` use `Get-ChildItem`/`dir` which already follow symlinks natively, so no change needed there.
- Step 3.6 always uses `false` (shallow/non-recursive) for the directory expansion. Recursive entries go through a different code path (the `recursive_entries` Vec) and are not affected.
- Performance: Step 3.6 issues one SSH call per host (one call for all paths). With 10 hosts and default concurrency=10, all calls run in parallel.
