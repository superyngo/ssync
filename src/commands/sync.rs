use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;

use crate::config::schema::{ConflictStrategy, HostEntry, ShellType};
use crate::host::executor;
use crate::output::printer;
use crate::output::summary::Summary;

use super::Context;

/// File metadata collected from a host.
#[derive(Debug, Clone)]
struct FileInfo {
    host: String,
    #[allow(dead_code)]
    path: String,
    mtime: i64,
    #[allow(dead_code)]
    size: u64,
    hash: String,
}

/// Result of metadata collection with missing-host tracking.
struct CollectResult {
    found: Vec<FileInfo>,
    missing: Vec<String>, // hosts where SSH succeeded but file doesn't exist
}

/// Sync decision for a file.
#[derive(Debug)]
struct SyncDecision {
    path: String,
    source_host: String,
    target_hosts: Vec<String>,
    /// Hosts that are already up-to-date (hash matches source); shown as ✓ already in sync.
    synced_hosts: Vec<String>,
    reason: String,
}

/// Info about a skipped path: `(source_host, path)`.
type SkipInfo = Option<(String, String)>;

pub async fn run(
    ctx: &Context,
    dry_run: bool,
    files: &[String],
    no_push_missing: bool,
    cli_source: Option<&str>,
) -> Result<()> {
    let push_missing = !no_push_missing;
    let hosts = ctx.resolve_hosts()?;

    // Ad-hoc file mode: --files/-f
    if !files.is_empty() {
        let mut total_summary = Summary::default();
        println!("\n── Sync: ad-hoc files ──");
        for path in files {
            // If the shell expanded ~/path to an absolute path under $HOME, convert it back
            // so the same tilde-relative path is used consistently on all remote hosts.
            let tilde_path = to_tilde_path(path);
            sync_path_across(
                ctx,
                &hosts,
                &tilde_path,
                "ad-hoc",
                dry_run,
                push_missing,
                cli_source,
                &mut total_summary,
            )
            .await?;
        }
        total_summary.print();
        return Ok(());
    }

    // Config-based sync
    let sync_entries = ctx.resolve_syncs();
    if sync_entries.is_empty() {
        println!("No sync entries matched the current filter. Add [[sync]] to config.toml or use --files/-f.");
        return Ok(());
    }

    let mut total_summary = Summary::default();

    if hosts.len() < 2 {
        println!("Need at least 2 hosts for sync (found {})", hosts.len());
        return Ok(());
    }

    let label = match &ctx.mode {
        super::TargetMode::All => "global".to_string(),
        super::TargetMode::Groups(g) => g.join(", "),
        super::TargetMode::Hosts(h) => h.join(", "),
    };
    println!("\n── Sync: {} ──", label);
    for sync_entry in sync_entries {
        // CLI --source takes priority, then config source
        let effective_source = cli_source.or(sync_entry.source.as_deref());
        for path in &sync_entry.paths {
            sync_path_across(
                ctx,
                &hosts,
                path,
                &label,
                dry_run,
                push_missing,
                effective_source,
                &mut total_summary,
            )
            .await?;
        }
    }

    total_summary.print();
    Ok(())
}

/// Sync a single path across a set of hosts.
#[allow(clippy::too_many_arguments)]
async fn sync_path_across(
    ctx: &Context,
    hosts: &[&HostEntry],
    path: &str,
    group_name: &str,
    dry_run: bool,
    push_missing: bool,
    source_override: Option<&str>,
    summary: &mut Summary,
) -> Result<()> {
    // Stage 1: Collect metadata from all hosts
    let collect_result = collect_file_metadata(hosts, path, ctx.timeout, ctx.concurrency()).await?;

    if collect_result.found.is_empty() {
        if !collect_result.missing.is_empty() {
            println!("  {}: file not found on any reachable host", path);
        } else {
            println!("  {}: no data collected", path);
        }
        return Ok(());
    }

    // Stage 2: Decide which version to use
    let decisions = if let Some(src) = source_override {
        // Fixed source: bypass automatic selection
        let (decs, skip_info) = make_decisions_fixed_source(
            &collect_result.found,
            path,
            push_missing,
            &collect_result.missing,
            src,
        )?;
        if let Some((source, skipped_path)) = skip_info {
            printer::print_host_line("skip", &source, &format!("does not have '{}'", skipped_path));
            summary.add_skip_with_reason(&skipped_path, &source, &format!("source '{}' does not have '{}'", source, skipped_path));
            return Ok(());
        }
        decs
    } else {
        make_decisions(
            &collect_result.found,
            &ctx.config.settings.conflict_strategy,
            path,
            push_missing,
            &collect_result.missing,
        )
    };

    if decisions.is_empty() {
        let hosts_list: Vec<&str> = collect_result
            .found
            .iter()
            .map(|f| f.host.as_str())
            .collect();
        println!("  {} (all in sync)", path);
        printer::print_host_line("passed", "ok", &hosts_list.join(", "));
        summary.add_success();
        return Ok(());
    }

    for decision in &decisions {
        // Header: show all non-source hosts (synced + targets) in the targets list
        let mut all_targets: Vec<&str> = decision.synced_hosts.iter().map(|s| s.as_str()).collect();
        all_targets.extend(decision.target_hosts.iter().map(|s| s.as_str()));
        println!(
            "  {} → source: {} → targets: [{}] ({})",
            decision.path,
            decision.source_host,
            all_targets.join(", "),
            decision.reason
        );

        // Passed: hosts already in sync (no upload needed)
        if !decision.synced_hosts.is_empty() {
            printer::print_host_line("passed", "ok", &decision.synced_hosts.join(", "));
        }

        if dry_run {
            continue;
        }

        // Stage 3: Distribute via local relay
        match distribute(hosts, decision, ctx.timeout, ctx.concurrency()).await {
            Ok((succeeded, failed_uploads)) => {
                if !succeeded.is_empty() {
                    printer::print_host_line("synced", "ok", &succeeded.join(", "));
                    summary.add_success();

                    // Update sync_state in DB
                    let now = chrono::Utc::now().timestamp();
                    for target in &succeeded {
                        let _ = ctx.db.execute(
                            "INSERT INTO sync_state (sync_group, host, path, mtime, size_bytes, blake3, synced_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                             ON CONFLICT(sync_group, host, path) DO UPDATE SET mtime=?4, size_bytes=?5, blake3=?6, synced_at=?7",
                            rusqlite::params![
                                group_name, target, decision.path,
                                0i64, 0i64, "", now
                            ],
                        );
                    }

                    // Log operation
                    let _ = ctx.db.execute(
                        "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms) \
                         VALUES (?1, 'sync', ?2, ?3, 'ok', 0)",
                        rusqlite::params![now, decision.source_host, format!("sync {}", decision.path)],
                    );
                }

                if !failed_uploads.is_empty() {
                    let failed_names: Vec<&str> =
                        failed_uploads.iter().map(|(n, _)| n.as_str()).collect();
                    printer::print_host_line("failed", "error", &failed_names.join(", "));
                    for (target, err) in &failed_uploads {
                        println!("    {}: {}", target, err);
                        summary.add_failure(target, err);
                    }
                }
            }
            Err(e) => {
                // Download from source failed — all targets implicitly fail
                printer::print_host_line("failed", "error", &decision.source_host);
                println!("    {}: download failed: {}", decision.source_host, e);
                summary.add_failure(&decision.source_host, &e.to_string());
            }
        }
    }

    if dry_run {
        println!("  [dry-run] No changes applied.");
    }

    Ok(())
}

/// Stage 1: Collect file metadata (mtime, hash) from all hosts in parallel.
/// Also tracks hosts where the file doesn't exist (SSH OK but stat failed).
async fn collect_file_metadata(
    hosts: &[&HostEntry],
    path: &str,
    timeout: u64,
    concurrency: usize,
) -> Result<CollectResult> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for host in hosts {
        let sem = semaphore.clone();
        let host = (*host).clone();
        let path = path.to_string();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            // Build metadata command based on host shell type.
            // Output format (2 lines): "<mtime> <size>\n<hash>  <filename>"
            // hash line may be absent if hash tool unavailable — that's fine.
            let cmd = match host.shell {
                ShellType::PowerShell => {
                    // Expand ~/path to $HOME\path for PowerShell (use double-quotes for $HOME expansion)
                    let ps_path = if let Some(stripped) = path.strip_prefix("~/") {
                        format!("$HOME\\{}", stripped.replace('/', "\\"))
                    } else {
                        path.clone()
                    };
                    format!(
                        "$f=\"{p}\"; \
                         $i=Get-Item $f -ErrorAction SilentlyContinue; \
                         if ($i) {{ \
                           [int64](($i.LastWriteTimeUtc-[datetime]\"1970-01-01\").TotalSeconds), $i.Length -join \" \"; \
                           (Get-FileHash $f -Algorithm SHA256).Hash.ToLower() \
                         }}",
                        p = ps_path
                    )
                }
                ShellType::Sh | ShellType::Cmd => {
                    // Handle ~ expansion: $HOME must stay unquoted for shell expansion
                    let escaped = if let Some(stripped) = path.strip_prefix("~/") {
                        format!("$HOME/'{}'", stripped.replace('\'', "'\\''"))
                    } else {
                        format!("'{}'", path.replace('\'', "'\\''"))
                    };
                    // stat line 1: Linux `stat -c '%Y %s'` or macOS `stat -f '%m %z'`
                    // hash line 2: sha256sum (Linux) or shasum -a 256 (macOS/BSD) — unified SHA-256
                    // hash failure must NOT affect exit code
                    format!(
                        "stat -c '%Y %s' {p} 2>/dev/null || stat -f '%m %z' {p} 2>/dev/null; \
                         (sha256sum {p} 2>/dev/null || shasum -a 256 {p} 2>/dev/null) || true",
                        p = escaped
                    )
                }
            };

            match executor::run_remote(&host, &cmd, timeout).await {
                Ok(output) => {
                    // Command always exits 0 (hash uses || true).
                    // Parse first line for mtime+size; empty/unparseable means file missing.
                    let lines: Vec<&str> = output.stdout.lines().collect();
                    let stat_parts: Vec<&str> = lines
                        .first()
                        .map(|l| l.split_whitespace().collect())
                        .unwrap_or_default();
                    let mtime: Option<i64> = stat_parts.first().and_then(|s| s.parse().ok());
                    let size: u64 = stat_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

                    if let Some(mtime) = mtime {
                        let hash = lines
                            .get(1)
                            .and_then(|l| l.split_whitespace().next())
                            .unwrap_or("")
                            .to_string();
                        // "found" — file exists
                        (host.name.clone(), Some(FileInfo {
                            host: host.name.clone(),
                            path,
                            mtime,
                            size,
                            hash,
                        }), false)
                    } else {
                        // SSH succeeded but stat failed — file missing
                        (host.name.clone(), None, true)
                    }
                }
                Err(_) => {
                    // SSH itself failed — host unreachable, exclude entirely
                    (host.name.clone(), None, false)
                }
            }
        }));
    }

    let mut found = Vec::new();
    let mut missing = Vec::new();
    for handle in handles {
        let (host_name, info, is_missing) = handle.await?;
        if let Some(fi) = info {
            found.push(fi);
        } else if is_missing {
            missing.push(host_name);
        }
    }

    Ok(CollectResult { found, missing })
}

/// Stage 2: Make sync decisions based on conflict strategy.
fn make_decisions(
    file_infos: &[FileInfo],
    strategy: &ConflictStrategy,
    path: &str,
    push_missing: bool,
    missing_hosts: &[String],
) -> Vec<SyncDecision> {
    if file_infos.is_empty() {
        return Vec::new();
    }

    // Check if all found hosts are in sync (same hash)
    let first_hash = &file_infos[0].hash;
    let all_in_sync = !first_hash.is_empty() && file_infos.iter().all(|f| f.hash == *first_hash);

    // If all in sync and no missing hosts to push to, nothing to do
    if all_in_sync && (!push_missing || missing_hosts.is_empty()) {
        return Vec::new();
    }

    match strategy {
        ConflictStrategy::Newest => {
            // Pick the file with the newest mtime as source
            let source = file_infos.iter().max_by_key(|f| f.mtime).unwrap();

            let mut targets: Vec<String> = file_infos
                .iter()
                .filter(|f| f.host != source.host)
                .filter(|f| f.hash != source.hash)
                .map(|f| f.host.clone())
                .collect();

            // Add missing hosts if push_missing is enabled
            if push_missing {
                for h in missing_hosts {
                    if !targets.contains(h) {
                        targets.push(h.clone());
                    }
                }
            }

            if targets.is_empty() {
                return Vec::new();
            }

            let conflict_targets: Vec<_> = file_infos
                .iter()
                .filter(|f| f.host != source.host && f.hash != source.hash)
                .collect();
            let reason = if conflict_targets.is_empty() && push_missing && !missing_hosts.is_empty()
            {
                // All reachable hosts are in sync; only pushing to hosts that lack the file
                format!(
                    "in sync on reachable hosts, pushing to {} missing",
                    missing_hosts.len()
                )
            } else {
                let mut r = format!("newest mtime: {}", source.mtime);
                if push_missing && !missing_hosts.is_empty() {
                    r.push_str(&format!(", +{} missing", missing_hosts.len()));
                }
                r
            };

            // Hosts that already have the same content as source (no upload needed)
            let synced_hosts: Vec<String> = file_infos
                .iter()
                .filter(|f| f.host != source.host && f.hash == source.hash)
                .map(|f| f.host.clone())
                .collect();

            vec![SyncDecision {
                path: path.to_string(),
                source_host: source.host.clone(),
                target_hosts: targets,
                synced_hosts,
                reason,
            }]
        }
        ConflictStrategy::Skip => {
            // Check if there's a conflict (different hashes)
            let hashes: std::collections::HashSet<_> = file_infos.iter().map(|f| &f.hash).collect();
            if hashes.len() > 1 {
                tracing::warn!(
                    path = %path,
                    "Conflict detected, skipping (strategy: skip)"
                );
                return Vec::new();
            }

            // Even with skip strategy, push to missing hosts if all found are in sync
            if push_missing && !missing_hosts.is_empty() && all_in_sync {
                let source = &file_infos[0];
                let synced_hosts: Vec<String> =
                    file_infos.iter().skip(1).map(|f| f.host.clone()).collect();
                return vec![SyncDecision {
                    path: path.to_string(),
                    source_host: source.host.clone(),
                    target_hosts: missing_hosts.to_vec(),
                    synced_hosts,
                    reason: format!("push to {} missing host(s)", missing_hosts.len()),
                }];
            }

            Vec::new()
        }
    }
}

/// Make sync decisions using a fixed source host (bypasses conflict strategy).
///
/// Returns `(decisions, skip_info)`. When the source host is not found in `file_infos`,
/// the function returns empty decisions and `Some((source_name, path))` so the caller
/// can report a skip instead of aborting the entire sync run.
fn make_decisions_fixed_source(
    file_infos: &[FileInfo],
    path: &str,
    push_missing: bool,
    missing_hosts: &[String],
    source_name: &str,
) -> Result<(Vec<SyncDecision>, SkipInfo)> {
    let source = match file_infos.iter().find(|f| f.host == source_name) {
        Some(s) => s,
        None => {
            return Ok((
                Vec::new(),
                Some((source_name.to_string(), path.to_string())),
            ));
        }
    };

    let mut targets: Vec<String> = file_infos
        .iter()
        .filter(|f| f.host != source.host)
        .filter(|f| f.hash != source.hash)
        .map(|f| f.host.clone())
        .collect();

    if push_missing {
        for h in missing_hosts {
            if !targets.contains(h) {
                targets.push(h.clone());
            }
        }
    }

    if targets.is_empty() {
        return Ok((Vec::new(), None));
    }

    let synced_hosts: Vec<String> = file_infos
        .iter()
        .filter(|f| f.host != source.host && f.hash == source.hash)
        .map(|f| f.host.clone())
        .collect();

    let mut reason = format!("fixed source: {}", source_name);
    if push_missing && !missing_hosts.is_empty() {
        reason.push_str(&format!(", +{} missing", missing_hosts.len()));
    }

    Ok((
        vec![SyncDecision {
            path: path.to_string(),
            source_host: source.host.clone(),
            target_hosts: targets,
            synced_hosts,
            reason,
        }],
        None,
    ))
}

/// Stage 3: Distribute file from source to targets via local relay.
/// Returns (succeeded_hosts, failed_hosts_with_errors).
async fn distribute(
    hosts: &[&HostEntry],
    decision: &SyncDecision,
    timeout: u64,
    concurrency: usize,
) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let source = hosts
        .iter()
        .find(|h| h.name == decision.source_host)
        .ok_or_else(|| anyhow::anyhow!("Source host not found: {}", decision.source_host))?;

    // Download to local temp file
    let temp_dir = tempfile::tempdir()?;
    let local_temp = temp_dir.path().join("ssync_relay");
    executor::download(source, &decision.path, &local_temp, timeout).await?;

    // Upload to all targets in parallel
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for target_name in &decision.target_hosts {
        let target = hosts
            .iter()
            .find(|h| h.name == *target_name)
            .ok_or_else(|| anyhow::anyhow!("Target host not found: {}", target_name))?;

        let sem = semaphore.clone();
        let target = (*target).clone();
        let local_temp = local_temp.clone();
        let remote_path = decision.path.clone();
        let target_name = target_name.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            // Ensure parent directory exists on target
            let parent = std::path::Path::new(&remote_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if !parent.is_empty() && parent != "/" && !parent.starts_with('~') {
                let mkdir_cmd = format!("mkdir -p '{}'", parent.replace('\'', "'\\''"));
                let _ = executor::run_remote(&target, &mkdir_cmd, timeout).await;
            } else if parent.starts_with("~/") || parent == "~" {
                // Use $HOME expansion instead of literal '~'
                let sub = if parent == "~" { "" } else { &parent[2..] };
                let mkdir_cmd = if sub.is_empty() {
                    String::new()
                } else {
                    format!("mkdir -p \"$HOME/{}\"", sub.replace('"', "\\\""))
                };
                if !mkdir_cmd.is_empty() {
                    let _ = executor::run_remote(&target, &mkdir_cmd, timeout).await;
                }
            }

            let result = executor::upload(&target, &local_temp, &remote_path, timeout).await;
            (target_name, result)
        }));
    }

    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for handle in handles {
        let (target_name, result) = handle.await?;
        match result {
            Ok(()) => succeeded.push(target_name),
            Err(e) => failed.push((target_name, e.to_string())),
        }
    }

    Ok((succeeded, failed))
}

/// Convert an absolute path back to a tilde-relative path if it falls under $HOME.
/// This handles the case where the shell expands `~/foo` → `/home/user/foo` before
/// ssync receives it — remotes need the tilde form so it resolves to *their* home dir.
fn to_tilde_path(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if path == home {
                return "~".to_string();
            }
            let prefix = format!("{}/", home);
            if let Some(rest) = path.strip_prefix(&prefix) {
                return format!("~/{}", rest);
            }
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file_info(host: &str, path: &str, hash: &str) -> FileInfo {
        FileInfo {
            host: host.to_string(),
            path: path.to_string(),
            mtime: 1000,
            size: 100,
            hash: hash.to_string(),
        }
    }

    #[test]
    fn test_fixed_source_missing_returns_empty_not_error() {
        let infos = vec![
            make_file_info("host-b", "~/.bashrc", "abc123"),
            make_file_info("host-c", "~/.bashrc", "def456"),
        ];
        let missing: Vec<String> = vec![];
        let result =
            make_decisions_fixed_source(&infos, "~/.bashrc", false, &missing, "host-a");
        assert!(result.is_ok(), "should not return an error");
        let (decisions, skip_info) = result.unwrap();
        assert!(decisions.is_empty(), "decisions should be empty");
        assert!(skip_info.is_some(), "skip_info should be Some");
        let (source, path) = skip_info.unwrap();
        assert_eq!(source, "host-a");
        assert_eq!(path, "~/.bashrc");
    }

    #[test]
    fn test_fixed_source_present_works_normally() {
        let infos = vec![
            make_file_info("host-a", "~/.bashrc", "abc123"),
            make_file_info("host-b", "~/.bashrc", "def456"),
            make_file_info("host-c", "~/.bashrc", "abc123"),
        ];
        let missing: Vec<String> = vec![];
        let result =
            make_decisions_fixed_source(&infos, "~/.bashrc", false, &missing, "host-a");
        assert!(result.is_ok());
        let (decisions, skip_info) = result.unwrap();
        assert!(skip_info.is_none(), "skip_info should be None when source is found");
        assert_eq!(decisions.len(), 1);
        let d = &decisions[0];
        assert_eq!(d.source_host, "host-a");
        assert_eq!(d.target_hosts, vec!["host-b".to_string()]);
        assert_eq!(d.synced_hosts, vec!["host-c".to_string()]);
        assert!(d.reason.contains("fixed source: host-a"));
    }
}
