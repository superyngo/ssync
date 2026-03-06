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
    path: String,
    mtime: i64,
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

pub async fn run(ctx: &Context, dry_run: bool, files: &[String], no_push_missing: bool) -> Result<()> {
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
            sync_path_across(ctx, &hosts, &tilde_path, "ad-hoc", dry_run, push_missing, &mut total_summary).await?;
        }
        total_summary.print();
        return Ok(());
    }

    // Config-based sync
    if ctx.config.sync.file.is_empty() {
        println!("No sync configured. Add [[sync.file]] to config.toml or use --files/-f.");
        return Ok(());
    }

    let mut total_summary = Summary::default();

    match &ctx.mode {
        // --all or --host: process [[sync.file]] entries without groups
        super::TargetMode::All | super::TargetMode::Hosts(_) => {
            let files: Vec<_> = ctx.config.sync.file.iter()
                .filter(|f| f.groups.is_empty())
                .collect();
            if files.is_empty() {
                println!("No [[sync.file]] without groups configured for --all/--host sync.");
                return Ok(());
            }
            if hosts.len() < 2 {
                println!("Need at least 2 hosts for sync (found {})", hosts.len());
                return Ok(());
            }
            println!("\n── Sync: global ──");
            for sync_file in files {
                for path in &sync_file.paths {
                    sync_path_across(ctx, &hosts, path, "global", dry_run, push_missing, &mut total_summary).await?;
                }
            }
        }

        // --group: process [[sync.file]] entries whose groups intersect
        super::TargetMode::Groups(groups) => {
            let files: Vec<_> = ctx.config.sync.file.iter()
                .filter(|f| !f.groups.is_empty() && f.groups.iter().any(|g| groups.contains(g)))
                .collect();
            if files.is_empty() {
                println!("No [[sync.file]] configured for group(s): {}", groups.join(", "));
                return Ok(());
            }
            if hosts.len() < 2 {
                println!("Need at least 2 hosts for sync (found {})", hosts.len());
                return Ok(());
            }
            let label = groups.join(", ");
            println!("\n── Sync group: {} ──", label);
            for sync_file in files {
                for path in &sync_file.paths {
                    sync_path_across(ctx, &hosts, path, &label, dry_run, push_missing, &mut total_summary).await?;
                }
            }
        }
    }

    total_summary.print();
    Ok(())
}

/// Sync a single path across a set of hosts.
async fn sync_path_across(
    ctx: &Context,
    hosts: &[&HostEntry],
    path: &str,
    group_name: &str,
    dry_run: bool,
    push_missing: bool,
    summary: &mut Summary,
) -> Result<()> {
    // Stage 1: Collect metadata from all hosts
    let collect_result = collect_file_metadata(
        hosts,
        path,
        ctx.timeout,
        ctx.concurrency(),
    )
    .await?;

    if collect_result.found.is_empty() {
        if !collect_result.missing.is_empty() {
            println!("  {}: file not found on any reachable host", path);
        } else {
            println!("  {}: no data collected", path);
        }
        return Ok(());
    }

    // Stage 2: Decide which version to use
    let decisions = make_decisions(
        &collect_result.found,
        &ctx.config.settings.conflict_strategy,
        path,
        push_missing,
        &collect_result.missing,
    );

    if decisions.is_empty() {
        for fi in &collect_result.found {
            printer::print_host_line(&fi.host, "ok", "already in sync");
        }
        summary.add_success();
        return Ok(());
    }

    // Display decisions
    for decision in &decisions {
        // Show hosts already in sync before listing what needs to be synced
        for host in &decision.synced_hosts {
            printer::print_host_line(host, "ok", "already in sync");
        }
        println!(
            "  {} → source: {} → targets: [{}] ({})",
            decision.path,
            decision.source_host,
            decision.target_hosts.join(", "),
            decision.reason
        );
    }

    if dry_run {
        println!("  [dry-run] No changes applied.");
        return Ok(());
    }

    // Stage 3: Distribute via local relay
    for decision in &decisions {
        match distribute(
            hosts,
            decision,
            ctx.timeout,
            ctx.concurrency(),
        )
        .await
        {
            Ok((succeeded, failed)) => {
                if !succeeded.is_empty() {
                    printer::print_host_line(
                        &decision.source_host,
                        "ok",
                        &format!("synced {} to {} host(s)", decision.path, succeeded.len()),
                    );
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

                for (target, err) in &failed {
                    printer::print_host_line(
                        target,
                        "error",
                        &format!("upload failed: {}", err),
                    );
                    summary.add_failure(target, err);
                }

                if succeeded.is_empty() && !failed.is_empty() {
                    // All targets failed
                }
            }
            Err(e) => {
                // download itself failed
                printer::print_host_line(
                    &decision.source_host,
                    "error",
                    &format!("failed to download {}: {}", decision.path, e),
                );
                summary.add_failure(&decision.source_host, &e.to_string());
            }
        }
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
                    let ps_path = if path.starts_with("~/") {
                        format!("$HOME\\{}", &path[2..].replace('/', "\\"))
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
                    let escaped = if path.starts_with("~/") {
                        format!("$HOME/'{}'", path[2..].replace('\'', "'\\''"))
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
            let reason = if conflict_targets.is_empty() && push_missing && !missing_hosts.is_empty() {
                // All reachable hosts are in sync; only pushing to hosts that lack the file
                format!("in sync on reachable hosts, pushing to {} missing", missing_hosts.len())
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
            let hashes: std::collections::HashSet<_> =
                file_infos.iter().map(|f| &f.hash).collect();
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
                let synced_hosts: Vec<String> = file_infos
                    .iter()
                    .skip(1)
                    .map(|f| f.host.clone())
                    .collect();
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
