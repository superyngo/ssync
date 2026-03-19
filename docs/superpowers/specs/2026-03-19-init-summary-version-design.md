# Design: Init Stale Host Detection, Summary Hostnames, Version Verification

**Date:** 2026-03-19  
**Status:** Draft

---

## Problem Statement

Three improvements to ssync:

1. **Init stale host detection:** When running `ssync init`, hosts that exist in ssync's config but have been removed from `~/.ssh/config` are silently kept. Users have no way to know or clean up stale entries.

2. **Sync summary hostnames:** The sync summary shows transfer counts (`1 synced  1 failed`) but doesn't indicate *which* hosts succeeded or failed. Users must scan verbose output to identify problematic hosts.

3. **Release version mismatch:** The release workflow can produce binaries with incorrect version numbers if a git tag is created before `Cargo.toml` is updated. The `v0.3.0` release shipped with version `0.2.0` due to this.

---

## Task 1: Init Stale Host Detection

### Approach

Add stale-host detection directly in the `init` command flow. After loading the SSH config and existing ssync config, compare host lists. If stale hosts are found, print a summary and prompt for confirmation before removing.

### Location

`src/commands/init.rs` — insert after SSH config is parsed (line 15) and before the detection loop (line 38).

### Logic

1. Collect all `ssh_host` values from `ctx.config.host`
2. Build a set of SSH config host names from `ssh_hosts`
3. Any ssync host whose `ssh_host` is NOT in the SSH config set → stale
4. If stale hosts found:
   - Print summary listing each stale host
   - Prompt: `Remove these N host(s) from ssync config? [y/N]:`
   - On `y`/`Y`: filter stale hosts out of the config's `host` vector
5. In dry-run mode: print the stale list with `[dry-run]` note, skip prompt
6. Only remove from `[[host]]` section — leave `[[check]]` and `[[sync]]` entries untouched

### Prompt Implementation

Use `std::io::stdin().read_line()` — no new dependency needed. Simple `y/N` prompt with `N` as default (safe).

### Example Output

```
Scanning ~/.ssh/config...
Found 2 host(s) no longer in ~/.ssh/config:
  - oldserver
  - retired-pi
Remove these 2 host(s) from ssync config? [y/N]: y
Removed 2 stale host(s).
```

### Dry-run Behavior

```
Scanning ~/.ssh/config...
Found 2 host(s) no longer in ~/.ssh/config:
  - oldserver
  - retired-pi
[dry-run] Would remove 2 stale host(s).
```

### Edge Cases

- No stale hosts → no output, no prompt
- All hosts are stale → still prompt (not auto-remove)
- User answers `N` or empty → skip removal, continue with init normally
- Hosts in `skipped_hosts` that are also stale → stale check is based on `config.host` entries, not skip list

---

## Task 2: Sync Summary with Hostnames

### Approach

Extend `SyncSummary` to track per-category hostname vectors alongside the existing counts. Display hostnames in parentheses after each count.

### Location

`src/output/summary.rs` — modify `SyncSummary` struct and its methods.

### Data Structure Changes

Add four new fields to `SyncSummary`:

```rust
pub transfers_passed_hosts: Vec<String>,
pub transfers_synced_hosts: Vec<String>,
pub transfers_failed_hosts: Vec<String>,
pub transfers_skipped_hosts: Vec<String>,
```

### Method Changes

- **`complete_file()`**: Extend `transfers_passed_hosts` from `passed`, `transfers_synced_hosts` from `synced`, `transfers_failed_hosts` from failed host names.
- **`file_in_sync()`**: Extend `transfers_passed_hosts` from `passed_hosts`.
- **`add_host_failure()`**: Push to `transfers_failed_hosts`.
- **`add_skip_with_reason()`**: Push to `transfers_skipped_hosts`.

### Display Format

In `print()`, for each transfer category with count > 0:

1. Deduplicate the hostname vector (preserving order)
2. Join with `, ` and wrap in parentheses
3. Append directly after the count+label

```
── Summary ──────────────────────────────
  Files:      1 synced
  Transfers:  1 synced(tonarchy)  1 failed(iphone12)
```

Multiple hosts example:

```
  Transfers:  3 passed(macmini, debian, smbnfs)  2 synced(tonarchy, realme)  1 failed(iphone12)
```

### Helper Function

Add a private helper to deduplicate and format hostname list:

```rust
fn format_hosts(hosts: &[String]) -> String {
    let mut seen = Vec::new();
    for h in hosts {
        if !seen.contains(h) {
            seen.push(h.clone());
        }
    }
    seen.join(", ")
}
```

### Test Updates

Update existing `SyncSummary` tests to verify hostname tracking. Add tests for deduplication.

---

## Task 3: Release Workflow Version Verification

### Root Cause

The `v0.3.0` git tag was created on commit `fdbd46d` where `Cargo.toml` still had `version = "0.2.0"`. The version bump to `0.3.0` happened in a later commit `f5c10da`. Since the release workflow builds from the tagged commit, it produced a binary with version `0.2.0`.

### Solution

Add a version verification step at the start of the build job in `.github/workflows/release.yml`.

### Implementation

Insert a new step after "Checkout code" and before "Setup Rust":

```yaml
- name: Verify version matches tag
  if: startsWith(github.ref, 'refs/tags/v')
  run: |
    TAG_VERSION="${GITHUB_REF#refs/tags/v}"
    CARGO_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    if [ "$TAG_VERSION" != "$CARGO_VERSION" ]; then
      echo "::error::Version mismatch! Tag: v$TAG_VERSION, Cargo.toml: $CARGO_VERSION"
      echo "Please ensure Cargo.toml version is updated before tagging."
      exit 1
    fi
    echo "Version verified: $TAG_VERSION"
```

### Behavior

- **Tag-triggered builds:** Extracts version from tag, compares to `Cargo.toml`, fails if mismatched
- **Manual workflow_dispatch:** Skips check (no tag to compare against)
- **Failure message:** Clear error explaining the mismatch and what to do

---

## Out of Scope

- Re-tagging existing `v0.3.0` release (user preference: fix workflow only)
- Cleaning up `[[check]]`/`[[sync]]` references when removing stale hosts
- Truncating long hostname lists in summary output
