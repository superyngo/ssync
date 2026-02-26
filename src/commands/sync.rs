use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;

use crate::config::schema::{ConflictStrategy, HostEntry};
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

/// Sync decision for a file.
#[derive(Debug)]
struct SyncDecision {
    path: String,
    source_host: String,
    target_hosts: Vec<String>,
    reason: String,
}

pub async fn run(ctx: &Context, dry_run: bool) -> Result<()> {
    if ctx.config.sync.group.is_empty() {
        println!("No sync groups configured. Add [[sync.group]] to config.toml.");
        return Ok(());
    }

    let all_hosts = ctx.require_targets()?;
    let mut total_summary = Summary::default();

    for sync_group in &ctx.config.sync.group {
        println!("\n── Sync group: {} ──", sync_group.name);

        // Filter to hosts in this sync group
        let group_hosts: Vec<&HostEntry> = all_hosts
            .iter()
            .filter(|h| sync_group.hosts.contains(&h.name))
            .copied()
            .collect();

        if group_hosts.len() < 2 {
            println!("  Skipping: need at least 2 hosts (found {})", group_hosts.len());
            continue;
        }

        for sync_file in &sync_group.file {
            // Stage 1: Collect metadata from all hosts
            let file_infos = collect_file_metadata(
                &group_hosts,
                &sync_file.path,
                ctx.timeout,
                ctx.concurrency(),
            )
            .await?;

            if file_infos.is_empty() {
                println!("  {}: no data collected", sync_file.path);
                continue;
            }

            // Stage 2: Decide which version to use
            let decisions = make_decisions(
                &file_infos,
                &ctx.config.settings.conflict_strategy,
                &sync_file.path,
            );

            if decisions.is_empty() {
                println!("  {}: all hosts in sync ✓", sync_file.path);
                total_summary.add_success();
                continue;
            }

            // Display decisions
            for decision in &decisions {
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
                continue;
            }

            // Stage 3: Distribute via local relay
            for decision in &decisions {
                match distribute(
                    &group_hosts,
                    decision,
                    ctx.timeout,
                    ctx.concurrency(),
                )
                .await
                {
                    Ok(()) => {
                        printer::print_host_line(
                            &decision.source_host,
                            "ok",
                            &format!("synced {} to {} host(s)", decision.path, decision.target_hosts.len()),
                        );
                        total_summary.add_success();

                        // Update sync_state in DB
                        let now = chrono::Utc::now().timestamp();
                        for target in &decision.target_hosts {
                            let _ = ctx.db.execute(
                                "INSERT INTO sync_state (sync_group, host, path, mtime, size_bytes, blake3, synced_at) \
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                                 ON CONFLICT(sync_group, host, path) DO UPDATE SET mtime=?4, size_bytes=?5, blake3=?6, synced_at=?7",
                                rusqlite::params![
                                    sync_group.name, target, decision.path,
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
                    Err(e) => {
                        printer::print_host_line(
                            &decision.source_host,
                            "error",
                            &format!("failed to sync {}: {}", decision.path, e),
                        );
                        total_summary.add_failure(&decision.source_host, &e.to_string());
                    }
                }
            }
        }
    }

    total_summary.print();
    Ok(())
}

/// Stage 1: Collect file metadata (mtime, hash) from all hosts in parallel.
async fn collect_file_metadata(
    hosts: &[&HostEntry],
    path: &str,
    timeout: u64,
    concurrency: usize,
) -> Result<Vec<FileInfo>> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for host in hosts {
        let sem = semaphore.clone();
        let host = (*host).clone();
        let path = path.to_string();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            // Get mtime and hash via single SSH command
            let cmd = format!(
                "stat -c '%Y %s' {path} 2>/dev/null && blake3sum {path} 2>/dev/null || \
                 stat -f '%m %z' {path} 2>/dev/null && shasum -a 256 {path} 2>/dev/null",
                path = path
            );

            match executor::run_remote(&host, &cmd, timeout).await {
                Ok(output) if output.success => {
                    let lines: Vec<&str> = output.stdout.lines().collect();
                    let (mtime, size) = if let Some(first) = lines.first() {
                        let parts: Vec<&str> = first.split_whitespace().collect();
                        (
                            parts.first().and_then(|s| s.parse().ok()).unwrap_or(0i64),
                            parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0u64),
                        )
                    } else {
                        (0, 0)
                    };

                    let hash = lines
                        .get(1)
                        .and_then(|l| l.split_whitespace().next())
                        .unwrap_or("")
                        .to_string();

                    Some(FileInfo {
                        host: host.name.clone(),
                        path,
                        mtime,
                        size,
                        hash,
                    })
                }
                _ => None,
            }
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        if let Some(info) = handle.await? {
            results.push(info);
        }
    }

    Ok(results)
}

/// Stage 2: Make sync decisions based on conflict strategy.
fn make_decisions(
    file_infos: &[FileInfo],
    strategy: &ConflictStrategy,
    path: &str,
) -> Vec<SyncDecision> {
    if file_infos.is_empty() {
        return Vec::new();
    }

    // Check if all hosts are in sync (same hash)
    let first_hash = &file_infos[0].hash;
    if !first_hash.is_empty() && file_infos.iter().all(|f| f.hash == *first_hash) {
        return Vec::new(); // All in sync
    }

    match strategy {
        ConflictStrategy::Newest => {
            // Pick the file with the newest mtime as source
            let source = file_infos.iter().max_by_key(|f| f.mtime).unwrap();
            let targets: Vec<String> = file_infos
                .iter()
                .filter(|f| f.host != source.host)
                .filter(|f| f.hash != source.hash)
                .map(|f| f.host.clone())
                .collect();

            if targets.is_empty() {
                return Vec::new();
            }

            vec![SyncDecision {
                path: path.to_string(),
                source_host: source.host.clone(),
                target_hosts: targets,
                reason: format!("newest mtime: {}", source.mtime),
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
            }
            Vec::new()
        }
    }
}

/// Stage 3: Distribute file from source to targets via local relay.
/// source_host → download to local temp → upload to each target
async fn distribute(
    hosts: &[&HostEntry],
    decision: &SyncDecision,
    timeout: u64,
    concurrency: usize,
) -> Result<()> {
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

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            executor::upload(&target, &local_temp, &remote_path, timeout).await
        }));
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}
