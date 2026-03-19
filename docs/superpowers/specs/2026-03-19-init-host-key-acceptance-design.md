# Init Host Key Acceptance

## Problem

When `ssync init` encounters SSH hosts not yet in `~/.ssh/known_hosts`, the ControlMaster pre-check fails with "Host key verification failed" because `BatchMode=yes` disables interactive prompts. Users must manually SSH to each host to accept the key, then re-run `init`. This is tedious when onboarding many new hosts.

## Solution

After the pre-check phase in `init`, detect host key verification failures, prompt the user to accept the unknown keys via `ssh-keyscan`, and automatically retry the connection.

## Scope

- **In scope:** `init` command only. Other commands (check, sync, run, exec) assume hosts are already trusted.
- **Out of scope:** Per-host confirmation, fingerprint display, key type selection.

## Design

### Flow Change in `init.rs`

After `conn_mgr.pre_check()` returns, the existing code iterates `failed_hosts()` to report errors. The new logic inserts a step before that:

1. **Partition failures.** Split `conn_mgr.failed_hosts()` into:
   - `host_key_failures`: entries where the error string contains `"Host key verification failed"`
   - `other_failures`: everything else

2. **Report other failures.** Print `other_failures` as errors using `printer::print_host_line` (existing behavior, unchanged).

3. **Handle host key failures.** If `host_key_failures` is non-empty and not `--dry-run`:
   - Print a summary: `"N host(s) have unknown SSH host keys:"`
   - List each host name
   - Prompt: `"Add to known_hosts and retry? [y/N]: "`
   - If the user declines, report these as errors and continue.

4. **Keyscan.** If the user accepts:
   - For each host, run `ssh-keyscan -H <ssh_host>` to fetch the host's public keys.
   - Append results to `~/.ssh/known_hosts`.
   - Keyscan operations run in parallel, bounded by the existing concurrency semaphore.
   - Hosts where keyscan fails are reported as errors.

5. **Retry connection.** After keyscan:
   - Keep the original `ConnectionManager` alive (it holds ControlMaster sockets for already-reachable hosts).
   - Create a second `ConnectionManager` for retrying only the host-key-failure hosts.
   - Run `pre_check` on the second CM only for hosts where keyscan succeeded.
   - Merge results: newly connected hosts join the reachable set for shell detection (using the second CM's sockets); still-failing hosts are reported as errors.
   - Both CMs are shut down at the end.

6. **Dry-run behavior.** When `--dry-run` is active, host key failures are reported with a skip message: `"unknown host key (dry-run, skipped)"`. No keyscan or prompt.

### ssh-keyscan Details

- Command: `ssh-keyscan -H <ssh_host>` (hashes the hostname in known_hosts for privacy).
- The `-p` flag is used if the host's SSH config specifies a non-standard port (parsed from the SSH host alias or extracted from ssh config). However, since ssync uses SSH host aliases from `~/.ssh/config`, the keyscan should use the resolved hostname/port. We'll use `ssh -G <alias>` to extract the actual `Hostname` and `Port`, then pass those to `ssh-keyscan -H -p <port> <hostname>`.
- Output is collected in memory, then written to `~/.ssh/known_hosts` in a single append operation (avoids filesystem races from parallel writes).
- Each keyscan has a timeout matching the configured SSH timeout.
- Empty or comment-only output from keyscan is treated as a failure.

### Error Handling

- If `ssh-keyscan` fails or returns empty output for a host, that host is reported as an error and excluded from the retry.
- If the retry pre-check fails for a host, it is reported as a normal connection error.
- The feature does not modify `StrictHostKeyChecking` or any other SSH option.

### User Prompt Style

Follows the same pattern as the existing stale-host removal prompt in `init.rs`:
```
3 host(s) have unknown SSH host keys:
  - realmed
  - server2
  - server3
Add to known_hosts and retry? [y/N]: 
```

## Testing

- Unit tests for the partitioning logic (parsing error strings for "Host key verification failed").
- The keyscan and retry logic involves real SSH operations and cannot be unit tested meaningfully; manual integration testing is appropriate.

## Files Modified

- `src/commands/init.rs` — Main logic changes (partition, prompt, keyscan, retry)
- No changes to `ConnectionManager`, `SshPool`, or other modules
