use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;

use crate::config::schema::{ConflictStrategy, HostEntry, ShellType, SyncEntry};
use crate::host::concurrency::ConcurrencyLimiter;
use crate::host::connection::ConnectionManager;
use crate::host::executor;
use crate::host::pool::SshPool;
use crate::output::printer;
use crate::output::summary::SyncSummary;

use super::{Context, TargetMode};

/// Recursive sync entry with its applicable host set and effective source override.
type RecursiveEntry<'a> = (&'a SyncEntry, HashSet<String>, Option<&'a str>);

/// Per-host applicable path sets for group-scoped sync filtering.
type HostPathMap = HashMap<String, HashSet<String>>;

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
    let mut summary = SyncSummary::default();

    // Step 1: Collect all file paths, separating recursive from non-recursive.
    // For --groups: also build per-host applicable path sets for scoping.
    let (mut all_paths, recursive_entries, mut host_applicable_paths, mut path_source_map) =
        if !files.is_empty() {
            let paths: Vec<String> = files.iter().map(|p| to_tilde_path(p)).collect();
            (paths, Vec::new(), None, HashMap::new())
        } else {
            let (paths, recursive, applicable, path_src) =
                collect_sync_paths_scoped(ctx, &hosts, cli_source);
            if paths.is_empty() && recursive.is_empty() {
                println!("No sync entries matched the current filter. Add [[sync]] to config.toml or use --files/-f.");
                return Ok(());
            }
            (paths, recursive, applicable, path_src)
        };

    if hosts.len() < 2 && (!all_paths.is_empty() || !recursive_entries.is_empty()) {
        println!("Need at least 2 hosts for sync (found {})", hosts.len());
        return Ok(());
    }

    let label = match &ctx.mode {
        super::TargetMode::All => "global".to_string(),
        super::TargetMode::Groups(g) => g.join(", "),
        super::TargetMode::Hosts(h) => h.join(", "),
    };
    println!("\n── Sync: {} ──", label);

    // Step 2: Pre-check SSH connections via SshPool (with SCP probe)
    let (mut pool, _connected) = SshPool::setup_with_options(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
        true, // probe scp capability
    )
    .await?;

    for (name, err) in pool.failed_hosts() {
        printer::print_host_line("unreachable", "error", &format!("{}: {}", name, err));
        summary.add_host_failure(&name, &err);
    }

    for (name, err) in pool.scp_failed_hosts() {
        printer::print_host_line("scp-failed", "error", &format!("{}: {}", name, err));
        summary.add_host_failure(&name, &format!("scp probe failed: {}", err));
    }

    // Step 3: Filter to scp-capable hosts (excludes both unreachable and scp-failed)
    if let Some(src) = cli_source {
        if pool.socket_for(src).is_none() {
            pool.shutdown().await;
            anyhow::bail!("Source host '{}' is unreachable", src);
        }
    }
    let reachable_hosts = pool.filter_scp_capable(&hosts);

    if reachable_hosts.len() < 2 && !all_paths.is_empty() {
        println!(
            "Need at least 2 reachable hosts for sync (found {})",
            reachable_hosts.len()
        );
        pool.shutdown().await;
        return Ok(());
    }

    // Step 3.5: Expand directory paths for entries with a fixed source.
    // Non-recursive paths use shallow listing (maxdepth 1).
    {
        // Collect paths that have a fixed source
        let source_paths: Vec<(String, &str)> = all_paths
            .iter()
            .filter_map(|p| {
                path_source_map
                    .get(p.as_str())
                    .and_then(|s| s.as_ref())
                    .map(|src| (p.clone(), *src))
            })
            .collect();

        if !source_paths.is_empty() {
            // Group by source host
            let mut by_source: HashMap<&str, Vec<String>> = HashMap::new();
            for (path, src) in &source_paths {
                by_source.entry(src).or_default().push(path.clone());
            }

            let mut dirs_expanded: HashMap<String, Vec<String>> = HashMap::new();
            let mut dirs_missing: Vec<String> = Vec::new();

            for (src_name, paths_for_src) in &by_source {
                if let Some(source_host) = reachable_hosts.iter().find(|h| h.name == *src_name) {
                    match expand_directory_paths(
                        source_host,
                        paths_for_src,
                        false, // shallow for non-recursive entries
                        ctx.timeout,
                        &pool.conn_mgr,
                    )
                    .await
                    {
                        Ok(expansions) => {
                            for (path, result) in expansions {
                                match result {
                                    DirExpandResult::Directory(files) => {
                                        dirs_expanded.insert(path, files);
                                    }
                                    DirExpandResult::Missing => {
                                        dirs_missing.push(path);
                                    }
                                    DirExpandResult::File => {} // keep as-is
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                source = %src_name,
                                error = %e,
                                "Failed to expand directories from source"
                            );
                        }
                    }
                }
            }

            // Replace directory paths with expanded file paths
            if !dirs_expanded.is_empty() || !dirs_missing.is_empty() {
                let mut new_paths = Vec::new();
                for path in &all_paths {
                    if let Some(expanded_files) = dirs_expanded.get(path) {
                        let src = path_source_map.get(path.as_str()).copied().flatten();
                        // Add each expanded file to the path source map
                        for file_path in expanded_files {
                            if !new_paths.contains(file_path) {
                                new_paths.push(file_path.clone());
                                path_source_map.entry(file_path.clone()).or_insert(src);
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
                            println!("  {} (empty directory on source, skipping)", path);
                        }
                    } else if dirs_missing.contains(path) {
                        // Skip missing paths — already handled by normal flow
                        new_paths.push(path.clone());
                    } else {
                        new_paths.push(path.clone());
                    }
                }
                all_paths = new_paths;
            }
        }
    }

    // Step 4: Batch collect metadata for non-recursive files
    if !all_paths.is_empty() {
        pool.progress.start_collect(reachable_hosts.len());
        let batch_result = batch_collect_all_metadata(
            &reachable_hosts,
            &all_paths,
            ctx.timeout,
            ctx.concurrency(),
            &pool.conn_mgr,
        )
        .await?;
        pool.progress.finish_collect();

        // Step 5: Make decisions per file
        let mut all_decisions: Vec<SyncDecision> = Vec::new();
        for path in &all_paths {
            if let Some(collect) = batch_result.per_file.get(path) {
                // For --groups mode: filter to only hosts whose applicable path set includes this path
                let (scoped_found, scoped_missing) =
                    scope_collect_result(collect, path, &host_applicable_paths);

                if scoped_found.is_empty() {
                    if !scoped_missing.is_empty() {
                        println!("  {}: file not found on any reachable host", path);
                    } else {
                        println!("  {}: no data collected", path);
                    }
                    continue;
                }

                let effective_source =
                    cli_source.or_else(|| path_source_map.get(path.as_str()).copied().flatten());
                let decisions = if let Some(src) = effective_source {
                    let (decs, skip_info) = make_decisions_fixed_source(
                        &scoped_found,
                        path,
                        push_missing,
                        &scoped_missing,
                        src,
                    )?;
                    if let Some((source, skipped_path)) = skip_info {
                        printer::print_host_line(
                            "skip",
                            &source,
                            &format!("does not have '{}'", skipped_path),
                        );
                        summary.add_skip_with_reason(
                            &skipped_path,
                            &source,
                            &format!("source '{}' does not have '{}'", source, skipped_path),
                        );
                        continue;
                    }
                    decs
                } else {
                    make_decisions(
                        &scoped_found,
                        &ctx.config.settings.conflict_strategy,
                        path,
                        push_missing,
                        &scoped_missing,
                    )
                };

                if decisions.is_empty() {
                    let hosts_list: Vec<&str> =
                        scoped_found.iter().map(|f| f.host.as_str()).collect();
                    println!("  {} (all in sync)", path);
                    printer::print_host_line("passed", "ok", &hosts_list.join(", "));
                    summary.file_in_sync(&hosts_list);
                } else {
                    all_decisions.extend(decisions);
                }
            }
        }

        // Step 6: Distribute all files
        if dry_run {
            for d in &all_decisions {
                let mut all_targets: Vec<&str> =
                    d.synced_hosts.iter().map(|s| s.as_str()).collect();
                all_targets.extend(d.target_hosts.iter().map(|s| s.as_str()));
                println!(
                    "  {} → source: {} → targets: [{}] ({})",
                    d.path,
                    d.source_host,
                    all_targets.join(", "),
                    d.reason
                );
                if !d.synced_hosts.is_empty() {
                    printer::print_host_line("passed", "ok", &d.synced_hosts.join(", "));
                }
            }
            if !all_decisions.is_empty() {
                println!("  [dry-run] No changes applied.");
            }
        } else {
            for decision in &all_decisions {
                let mut all_targets: Vec<&str> =
                    decision.synced_hosts.iter().map(|s| s.as_str()).collect();
                all_targets.extend(decision.target_hosts.iter().map(|s| s.as_str()));
                println!(
                    "  {} → source: {} → targets: [{}] ({})",
                    decision.path,
                    decision.source_host,
                    all_targets.join(", "),
                    decision.reason
                );
                if !decision.synced_hosts.is_empty() {
                    printer::print_host_line("passed", "ok", &decision.synced_hosts.join(", "));
                }

                match distribute_pooled(
                    &reachable_hosts,
                    decision,
                    ctx.timeout,
                    &pool.limiter,
                    &pool.conn_mgr,
                )
                .await
                {
                    Ok((succeeded, failed_uploads)) => {
                        if !succeeded.is_empty() {
                            printer::print_host_line("synced", "ok", &succeeded.join(", "));

                            let now = chrono::Utc::now().timestamp();
                            for target in &succeeded {
                                let _ = ctx.db.execute(
                                    "INSERT INTO sync_state (sync_group, host, path, mtime, size_bytes, blake3, synced_at) \
                                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                                     ON CONFLICT(sync_group, host, path) DO UPDATE SET mtime=?4, size_bytes=?5, blake3=?6, synced_at=?7",
                                    rusqlite::params![
                                        label, target, decision.path,
                                        0i64, 0i64, "", now
                                    ],
                                );
                            }

                            let _ = ctx.db.execute(
                                "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms) \
                                 VALUES (?1, 'sync', ?2, ?3, 'ok', 0)",
                                rusqlite::params![
                                    now,
                                    decision.source_host,
                                    format!("sync {}", decision.path)
                                ],
                            );
                        }

                        if !failed_uploads.is_empty() {
                            let failed_names: Vec<&str> =
                                failed_uploads.iter().map(|(n, _)| n.as_str()).collect();
                            printer::print_host_line("failed", "error", &failed_names.join(", "));
                        }

                        summary.complete_file(
                            &decision.path,
                            &decision.synced_hosts,
                            &succeeded,
                            &failed_uploads,
                        );
                    }
                    Err(e) => {
                        printer::print_host_line("failed", "error", &decision.source_host);
                        // Source download failed — all targets implicitly fail
                        let all_failed: Vec<(String, String)> =
                            std::iter::once((decision.source_host.clone(), e.to_string()))
                                .chain(
                                    decision.target_hosts.iter().map(|t| {
                                        (t.clone(), format!("source download failed: {}", e))
                                    }),
                                )
                                .collect();
                        summary.complete_file(
                            &decision.path,
                            &decision.synced_hosts,
                            &[],
                            &all_failed,
                        );
                    }
                }
            }
        }
    }

    // Handle recursive entries with per-file flow.
    // For --groups: scope hosts per recursive entry based on group membership.
    // For entries with a fixed source: expand directory paths (deep recursive) before syncing.
    for (entry, hosts_for_entry, effective_source) in &recursive_entries {
        let scoped_hosts: Vec<&HostEntry> = reachable_hosts
            .iter()
            .filter(|h| hosts_for_entry.contains(&h.name))
            .copied()
            .collect();
        if scoped_hosts.len() < 2 {
            continue;
        }

        // Expand directory paths from source when a fixed source is set
        let expanded_paths: Vec<String> = if let Some(src_name) = effective_source {
            if let Some(source_host) = scoped_hosts.iter().find(|h| h.name == *src_name) {
                match expand_directory_paths(
                    source_host,
                    &entry.paths,
                    true, // deep recursive for recursive entries
                    ctx.timeout,
                    &pool.conn_mgr,
                )
                .await
                {
                    Ok(expansions) => {
                        let mut paths = Vec::new();
                        for p in &entry.paths {
                            match expansions.get(p) {
                                Some(DirExpandResult::Directory(files)) => {
                                    if files.is_empty() {
                                        println!("  {} (empty directory on source, skipping)", p);
                                    } else {
                                        paths.extend(files.iter().cloned());
                                    }
                                }
                                Some(DirExpandResult::File) | None => {
                                    paths.push(p.clone());
                                }
                                Some(DirExpandResult::Missing) => {
                                    // Will be handled by sync_path_across as missing on source
                                    paths.push(p.clone());
                                }
                            }
                        }
                        paths
                    }
                    Err(e) => {
                        tracing::warn!(
                            source = %src_name,
                            error = %e,
                            "Failed to expand directories from source, falling back to original paths"
                        );
                        entry.paths.clone()
                    }
                }
            } else {
                entry.paths.clone()
            }
        } else {
            entry.paths.clone()
        };

        for path in &expanded_paths {
            sync_path_across(
                ctx,
                &scoped_hosts,
                path,
                &label,
                dry_run,
                push_missing,
                *effective_source,
                &mut summary,
            )
            .await?;
        }
    }

    // Cleanup
    pool.shutdown().await;

    summary.print();
    Ok(())
}

/// Collect sync paths with per-host scoping for --groups mode.
/// Returns: (all_paths for batch collection, recursive entries with host sets, optional per-host path map).
/// Per-path source override map: path → source host name.
type PathSourceMap<'a> = HashMap<String, Option<&'a str>>;

fn collect_sync_paths_scoped<'a>(
    ctx: &'a Context,
    hosts: &[&HostEntry],
    cli_source: Option<&'a str>,
) -> (
    Vec<String>,
    Vec<RecursiveEntry<'a>>,
    Option<HostPathMap>,
    PathSourceMap<'a>,
) {
    match &ctx.mode {
        TargetMode::Groups(groups) => {
            let mut all_paths_set: Vec<String> = Vec::new();
            let mut host_paths: HashMap<String, HashSet<String>> = HashMap::new();
            let mut recursive: Vec<(&SyncEntry, HashSet<String>, Option<&str>)> = Vec::new();
            let mut seen_recursive: HashSet<usize> = HashSet::new();
            let mut path_sources: PathSourceMap<'a> = HashMap::new();

            for host in hosts {
                let mut seen_entries = HashSet::new();
                for group in &host.groups {
                    if !groups.contains(group) {
                        continue;
                    }
                    for entry in ctx.resolve_syncs_for_group(group) {
                        let ptr = std::ptr::from_ref(entry) as usize;
                        if !seen_entries.insert(ptr) {
                            continue;
                        }
                        let effective_source = cli_source.or(entry.source.as_deref());
                        if entry.recursive {
                            // Track which hosts apply to this recursive entry
                            if let Some(existing) = recursive
                                .iter_mut()
                                .find(|(e, _, _)| std::ptr::from_ref(*e) as usize == ptr)
                            {
                                existing.1.insert(host.name.clone());
                            } else if seen_recursive.insert(ptr) {
                                let mut host_set = HashSet::new();
                                host_set.insert(host.name.clone());
                                recursive.push((entry, host_set, effective_source));
                            }
                        } else {
                            for p in &entry.paths {
                                host_paths
                                    .entry(host.name.clone())
                                    .or_default()
                                    .insert(p.clone());
                                if !all_paths_set.contains(p) {
                                    all_paths_set.push(p.clone());
                                }
                                path_sources.entry(p.clone()).or_insert(effective_source);
                            }
                        }
                    }
                }
            }

            (all_paths_set, recursive, Some(host_paths), path_sources)
        }
        _ => {
            // Flat merge for --hosts and --all
            let sync_entries = ctx.resolve_syncs();
            let mut paths = Vec::new();
            let mut recursive = Vec::new();
            let mut path_sources: PathSourceMap<'a> = HashMap::new();
            for entry in sync_entries {
                let effective_source = cli_source.or(entry.source.as_deref());
                if entry.recursive {
                    // For flat merge, all hosts are applicable
                    let all_host_names: HashSet<String> =
                        hosts.iter().map(|h| h.name.clone()).collect();
                    recursive.push((entry, all_host_names, effective_source));
                } else {
                    for p in &entry.paths {
                        if !paths.contains(p) {
                            paths.push(p.clone());
                        }
                        path_sources.entry(p.clone()).or_insert(effective_source);
                    }
                }
            }
            (paths, recursive, None, path_sources)
        }
    }
}

/// Filter collect results to only include hosts whose applicable path set includes the path.
/// When host_applicable_paths is None (flat merge mode), returns unfiltered data.
fn scope_collect_result(
    collect: &CollectResult,
    path: &str,
    host_applicable_paths: &Option<HostPathMap>,
) -> (Vec<FileInfo>, Vec<String>) {
    match host_applicable_paths {
        Some(map) => {
            let found: Vec<FileInfo> = collect
                .found
                .iter()
                .filter(|fi| map.get(&fi.host).is_some_and(|paths| paths.contains(path)))
                .cloned()
                .collect();
            let missing: Vec<String> = collect
                .missing
                .iter()
                .filter(|host| map.get(*host).is_some_and(|paths| paths.contains(path)))
                .cloned()
                .collect();
            (found, missing)
        }
        None => (collect.found.clone(), collect.missing.clone()),
    }
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
    summary: &mut SyncSummary,
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
            printer::print_host_line(
                "skip",
                &source,
                &format!("does not have '{}'", skipped_path),
            );
            summary.add_skip_with_reason(
                &skipped_path,
                &source,
                &format!("source '{}' does not have '{}'", source, skipped_path),
            );
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
        summary.file_in_sync(&hosts_list);
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
                }

                summary.complete_file(
                    &decision.path,
                    &decision.synced_hosts,
                    &succeeded,
                    &failed_uploads,
                );
            }
            Err(e) => {
                // Download from source failed — all targets implicitly fail
                printer::print_host_line("failed", "error", &decision.source_host);
                let all_failed: Vec<(String, String)> =
                    std::iter::once((decision.source_host.clone(), e.to_string()))
                        .chain(
                            decision
                                .target_hosts
                                .iter()
                                .map(|t| (t.clone(), format!("source download failed: {}", e))),
                        )
                        .collect();
                summary.complete_file(&decision.path, &decision.synced_hosts, &[], &all_failed);
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

/// Stage 3 (Pooled): Distribute file from source to targets via local relay,
/// using ControlMaster sockets and dual-level concurrency limiter.
async fn distribute_pooled(
    hosts: &[&HostEntry],
    decision: &SyncDecision,
    timeout: u64,
    limiter: &ConcurrencyLimiter,
    conn_mgr: &ConnectionManager,
) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let source = hosts
        .iter()
        .find(|h| h.name == decision.source_host)
        .ok_or_else(|| anyhow::anyhow!("Source host not found: {}", decision.source_host))?;

    // Download from source using pooled connection
    let temp_dir = tempfile::tempdir()?;
    let local_temp = temp_dir.path().join("ssync_relay");
    let source_socket = conn_mgr.socket_for(&source.name);
    {
        let _permit = limiter.acquire(&source.name).await;
        executor::download_pooled(source, &decision.path, &local_temp, timeout, source_socket)
            .await?;
    }

    // Upload to all targets in parallel with concurrency limiter
    let mut handles = Vec::new();

    for target_name in &decision.target_hosts {
        let target = hosts
            .iter()
            .find(|h| h.name == *target_name)
            .ok_or_else(|| anyhow::anyhow!("Target host not found: {}", target_name))?;

        let target = (*target).clone();
        let local_temp = local_temp.clone();
        let remote_path = decision.path.clone();
        let target_name = target_name.clone();
        let socket = conn_mgr.socket_for(&target.name).map(|p| p.to_path_buf());

        // We need references to limiter, but can't move them into spawn.
        // Use Arc for the limiter's semaphores (they're already Arc internally).
        // Instead, acquire permit inside the task.
        let limiter_global = limiter.global_semaphore();
        let limiter_per_host = limiter
            .per_host_semaphore(&target.name)
            .expect("target not registered");

        handles.push(tokio::spawn(async move {
            // Acquire permits: global first, then per-host
            let _global_permit = limiter_global.acquire().await.unwrap();
            let _per_host_permit = limiter_per_host.acquire().await.unwrap();

            // Ensure parent directory exists on target
            let parent = std::path::Path::new(&remote_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if !parent.is_empty() && parent != "/" && !parent.starts_with('~') {
                let mkdir_cmd = format!("mkdir -p '{}'", parent.replace('\'', "'\\''"));
                let _ =
                    executor::run_remote_pooled(&target, &mkdir_cmd, timeout, socket.as_deref())
                        .await;
            } else if parent.starts_with("~/") || parent == "~" {
                let sub = if parent == "~" { "" } else { &parent[2..] };
                if !sub.is_empty() {
                    let mkdir_cmd = format!("mkdir -p \"$HOME/{}\"", sub.replace('"', "\\\""));
                    let _ = executor::run_remote_pooled(
                        &target,
                        &mkdir_cmd,
                        timeout,
                        socket.as_deref(),
                    )
                    .await;
                }
            }

            let result = executor::upload_pooled(
                &target,
                &local_temp,
                &remote_path,
                timeout,
                socket.as_deref(),
            )
            .await;
            (target_name, result)
        }));
    }

    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for handle in handles {
        let (target_name, result) = handle.await?;
        match result {
            Ok(()) => {
                succeeded.push(target_name.clone());
            }
            Err(e) => failed.push((target_name, e.to_string())),
        }
    }

    Ok((succeeded, failed))
}
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

/// Result of directory expansion for a single path.
#[derive(Debug, Clone, PartialEq)]
enum DirExpandResult {
    /// Path is a regular file — pass through unchanged.
    File,
    /// Path is a directory — contains the expanded list of file paths within it.
    Directory(Vec<String>),
    /// Path does not exist on the source host.
    Missing,
}

/// Build a remote shell command that detects whether each path is a directory or file,
/// and lists directory contents when applicable.
///
/// Output format per path:
/// ```text
/// ---PATH:<original_path>
/// DIR
/// <file1>
/// <file2>
/// ```
/// or `FILE` / `MISSING` instead of `DIR`.
fn build_dir_expand_cmd(paths: &[String], recursive: bool, shell: ShellType) -> String {
    match shell {
        ShellType::PowerShell => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("\"$HOME\\{}\"", stripped.replace('/', "\\"))
                    } else {
                        format!("\"{}\"", p)
                    }
                })
                .collect();
            let recurse_flag = if recursive { " -Recurse" } else { "" };
            // For each path: detect type, list files if directory
            // Normalize paths back to ~/... by replacing $HOME with ~
            format!(
                "$h=$HOME; \
                 foreach ($p in @({files})) {{ \
                   $orig=$p -replace [regex]::Escape($h),'~'; \
                   \"---PATH:$orig\"; \
                   if (Test-Path $p -PathType Container) {{ \
                     \"DIR\"; \
                     Get-ChildItem $p -File{recurse} | ForEach-Object {{ \
                       $_.FullName -replace [regex]::Escape($h),'~' \
                     }} \
                   }} elseif (Test-Path $p) {{ \"FILE\" }} \
                   else {{ \"MISSING\" }} \
                 }}",
                files = expanded.join(","),
                recurse = recurse_flag
            )
        }
        ShellType::Sh | ShellType::Cmd => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("$HOME/'{}'", stripped.replace('\'', "'\\''"))
                    } else {
                        format!("'{}'", p.replace('\'', "'\\''"))
                    }
                })
                .collect();
            let depth_flag = if recursive { "" } else { " -maxdepth 1" };
            // Detect type, list files, normalize $HOME → ~
            format!(
                "for p in {files}; do \
                   orig=$(echo \"$p\" | sed \"s|^$HOME/|~/|;s|^$HOME$|~|\"); \
                   echo \"---PATH:$orig\"; \
                   if [ -d \"$p\" ]; then \
                     echo \"DIR\"; \
                     find \"$p\"{depth} -type f 2>/dev/null | sed \"s|^$HOME/|~/|\" | sort; \
                   elif [ -e \"$p\" ]; then \
                     echo \"FILE\"; \
                   else \
                     echo \"MISSING\"; \
                   fi; \
                 done",
                files = expanded.join(" "),
                depth = depth_flag
            )
        }
    }
}

/// Parse the output of `build_dir_expand_cmd` into per-path results.
fn parse_dir_expand_output(output: &str, paths: &[String]) -> HashMap<String, DirExpandResult> {
    let mut result = HashMap::new();

    let blocks: Vec<&str> = output.split("---PATH:").collect();
    let file_blocks = if blocks.len() > 1 {
        &blocks[1..]
    } else {
        return result;
    };

    for (i, block) in file_blocks.iter().enumerate() {
        if i >= paths.len() {
            break;
        }
        let original_path = &paths[i];
        let lines: Vec<&str> = block.lines().collect();

        // First line is the path echo, second line is the type marker
        if lines.len() < 2 {
            result.insert(original_path.clone(), DirExpandResult::Missing);
            continue;
        }

        let type_marker = lines[1].trim();
        match type_marker {
            "DIR" => {
                let files: Vec<String> = lines[2..]
                    .iter()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect();
                result.insert(original_path.clone(), DirExpandResult::Directory(files));
            }
            "FILE" => {
                result.insert(original_path.clone(), DirExpandResult::File);
            }
            _ => {
                result.insert(original_path.clone(), DirExpandResult::Missing);
            }
        }
    }

    result
}

/// SSH to source host, detect directories, and list their contents.
/// Returns a mapping from each original path to its expansion result.
async fn expand_directory_paths(
    source_host: &HostEntry,
    paths: &[String],
    recursive: bool,
    timeout: u64,
    conn_mgr: &ConnectionManager,
) -> Result<HashMap<String, DirExpandResult>> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }

    let cmd = build_dir_expand_cmd(paths, recursive, source_host.shell);
    let socket = conn_mgr.socket_for(&source_host.name);
    let output = executor::run_remote_pooled(source_host, &cmd, timeout, socket).await?;

    if !output.success {
        tracing::warn!(
            host = %source_host.name,
            stderr = %output.stderr,
            "Directory expansion command failed"
        );
        // Treat all as missing if command fails
        let mut result = HashMap::new();
        for p in paths {
            result.insert(p.clone(), DirExpandResult::Missing);
        }
        return Ok(result);
    }

    Ok(parse_dir_expand_output(&output.stdout, paths))
}

/// Result of batch metadata collection for a single file.
#[derive(Debug, Clone)]
struct SingleFileResult {
    found: Option<FileInfo>,
    is_missing: bool,
}

/// Generate a single shell command that collects stat + hash for ALL files at once.
fn build_batch_metadata_cmd(paths: &[String], shell: ShellType) -> String {
    match shell {
        ShellType::PowerShell => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("\"$HOME\\{}\"", stripped.replace('/', "\\"))
                    } else {
                        format!("\"{}\"", p)
                    }
                })
                .collect();
            format!(
                "foreach ($f in @({files})) {{ \
                 \"---FILE:$f\"; \
                 $i=Get-Item $f -ErrorAction SilentlyContinue; \
                 if ($i) {{ \
                   [int64](($i.LastWriteTimeUtc-[datetime]\"1970-01-01\").TotalSeconds), $i.Length -join \" \"; \
                   (Get-FileHash $f -Algorithm SHA256).Hash.ToLower() \
                 }} else {{ \"MISSING\" }} \
                 }}",
                files = expanded.join(",")
            )
        }
        ShellType::Sh | ShellType::Cmd => {
            let expanded: Vec<String> = paths
                .iter()
                .map(|p| {
                    if let Some(stripped) = p.strip_prefix("~/") {
                        format!("$HOME/'{}'", stripped.replace('\'', "'\\''"))
                    } else {
                        format!("'{}'", p.replace('\'', "'\\''"))
                    }
                })
                .collect();
            format!(
                "for f in {files}; do \
                 echo \"---FILE:$f\"; \
                 stat -c '%Y %s' \"$f\" 2>/dev/null || stat -f '%m %z' \"$f\" 2>/dev/null || echo \"MISSING\"; \
                 (sha256sum \"$f\" 2>/dev/null || shasum -a 256 \"$f\" 2>/dev/null) || echo \"NOHASH\"; \
                 done",
                files = expanded.join(" ")
            )
        }
    }
}

/// Parse the output of a batch metadata command into per-file results.
///
/// Splits output by `---FILE:` markers, matching each block positionally
/// to the corresponding path in `paths`.
#[allow(dead_code)]
fn parse_batch_metadata_output(
    output: &str,
    paths: &[String],
    host_name: &str,
) -> HashMap<String, SingleFileResult> {
    let mut result = HashMap::new();

    // Split by "---FILE:" marker; first element is before any marker (empty/junk).
    let blocks: Vec<&str> = output.split("---FILE:").collect();
    let file_blocks = if blocks.len() > 1 {
        &blocks[1..]
    } else {
        return result;
    };

    for (i, block) in file_blocks.iter().enumerate() {
        if i >= paths.len() {
            break;
        }
        let original_path = &paths[i];
        // Lines: first is the expanded path, rest is data
        let lines: Vec<&str> = block.lines().collect();

        if lines.len() < 2 {
            result.insert(
                original_path.clone(),
                SingleFileResult {
                    found: None,
                    is_missing: true,
                },
            );
            continue;
        }

        let data_line = lines[1].trim();
        if data_line == "MISSING" {
            result.insert(
                original_path.clone(),
                SingleFileResult {
                    found: None,
                    is_missing: true,
                },
            );
            continue;
        }

        // Parse stat line: "mtime size"
        let stat_parts: Vec<&str> = data_line.split_whitespace().collect();
        let mtime: i64 = stat_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let size: u64 = stat_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        // Parse hash line (third line in block)
        let hash = if lines.len() > 2 {
            let hash_line = lines[2].trim();
            if hash_line == "NOHASH" {
                String::new()
            } else {
                // Format: "hash  filename" or just "hash"
                hash_line
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string()
            }
        } else {
            String::new()
        };

        result.insert(
            original_path.clone(),
            SingleFileResult {
                found: Some(FileInfo {
                    host: host_name.to_string(),
                    path: original_path.clone(),
                    mtime,
                    size,
                    hash,
                }),
                is_missing: false,
            },
        );
    }

    result
}

struct BatchCollectResult {
    per_file: HashMap<String, CollectResult>,
    #[allow(dead_code)]
    unreachable_hosts: Vec<String>,
}

/// Stage 1 (Batched): Collect metadata for ALL files from all hosts with one SSH call per host.
async fn batch_collect_all_metadata(
    hosts: &[&HostEntry],
    paths: &[String],
    timeout: u64,
    concurrency: usize,
    conn_mgr: &ConnectionManager,
) -> Result<BatchCollectResult> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    let mut skipped_unreachable: Vec<String> = Vec::new();

    for host in hosts {
        let socket = match conn_mgr.socket_for(&host.name) {
            Some(s) => Some(s.to_path_buf()),
            None => {
                skipped_unreachable.push(host.name.clone());
                continue;
            }
        };

        let sem = semaphore.clone();
        let host = (*host).clone();
        let paths = paths.to_vec();
        let cmd = build_batch_metadata_cmd(&paths, host.shell);

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = executor::run_remote_pooled(&host, &cmd, timeout, socket.as_deref()).await;

            match result {
                Ok(output) if output.success => {
                    let parsed = parse_batch_metadata_output(&output.stdout, &paths, &host.name);
                    (host.name.clone(), Some(parsed), false)
                }
                _ => (host.name.clone(), None, true),
            }
        }));
    }

    let mut per_file: HashMap<String, CollectResult> = HashMap::new();
    for path in paths {
        per_file.insert(
            path.clone(),
            CollectResult {
                found: Vec::new(),
                missing: Vec::new(),
            },
        );
    }
    let mut unreachable_hosts = skipped_unreachable;

    for handle in handles {
        let (host_name, parsed_opt, is_unreachable) = handle.await?;
        if is_unreachable {
            unreachable_hosts.push(host_name);
            continue;
        }
        if let Some(parsed) = parsed_opt {
            for (path, single) in parsed {
                if let Some(collect) = per_file.get_mut(&path) {
                    if let Some(fi) = single.found {
                        collect.found.push(fi);
                    } else if single.is_missing {
                        collect.missing.push(host_name.clone());
                    }
                }
            }
        }
    }

    Ok(BatchCollectResult {
        per_file,
        unreachable_hosts,
    })
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
        let result = make_decisions_fixed_source(&infos, "~/.bashrc", false, &missing, "host-a");
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
        let result = make_decisions_fixed_source(&infos, "~/.bashrc", false, &missing, "host-a");
        assert!(result.is_ok());
        let (decisions, skip_info) = result.unwrap();
        assert!(
            skip_info.is_none(),
            "skip_info should be None when source is found"
        );
        assert_eq!(decisions.len(), 1);
        let d = &decisions[0];
        assert_eq!(d.source_host, "host-a");
        assert_eq!(d.target_hosts, vec!["host-b".to_string()]);
        assert_eq!(d.synced_hosts, vec!["host-c".to_string()]);
        assert!(d.reason.contains("fixed source: host-a"));
    }

    #[test]
    fn test_build_batch_metadata_cmd_sh() {
        let paths = vec!["~/.bashrc".to_string(), "~/.vimrc".to_string()];
        let cmd = build_batch_metadata_cmd(&paths, ShellType::Sh);
        assert!(cmd.contains("---FILE:"));
        assert!(cmd.contains("stat"));
        assert!(cmd.contains("sha256sum") || cmd.contains("shasum"));
        assert!(cmd.contains(".bashrc"));
        assert!(cmd.contains(".vimrc"));
    }

    #[test]
    fn test_build_batch_metadata_cmd_powershell() {
        let paths = vec!["~/.bashrc".to_string()];
        let cmd = build_batch_metadata_cmd(&paths, ShellType::PowerShell);
        assert!(cmd.contains("---FILE:"));
        assert!(cmd.contains("Get-Item"));
        assert!(cmd.contains("Get-FileHash"));
    }

    #[test]
    fn test_parse_batch_metadata_output() {
        let output = "---FILE:$HOME/.bashrc\n1700000000 1234\nabc123def456  /home/user/.bashrc\n---FILE:$HOME/.vimrc\nMISSING\n";
        let paths = vec!["~/.bashrc".to_string(), "~/.vimrc".to_string()];
        let result = parse_batch_metadata_output(output, &paths, "host-a");
        assert_eq!(result.len(), 2);
        let bashrc = &result["~/.bashrc"];
        assert!(bashrc.found.is_some());
        let fi = bashrc.found.as_ref().unwrap();
        assert_eq!(fi.mtime, 1700000000);
        assert_eq!(fi.hash, "abc123def456");
        let vimrc = &result["~/.vimrc"];
        assert!(vimrc.found.is_none());
        assert!(vimrc.is_missing);
    }

    #[test]
    fn test_parse_batch_metadata_nohash() {
        let output = "---FILE:$HOME/.bashrc\n1700000000 1234\nNOHASH\n";
        let paths = vec!["~/.bashrc".to_string()];
        let result = parse_batch_metadata_output(output, &paths, "host-a");
        let bashrc = &result["~/.bashrc"];
        assert!(bashrc.found.is_some());
        let fi = bashrc.found.as_ref().unwrap();
        assert!(
            fi.hash.is_empty(),
            "NOHASH sentinel should produce empty hash"
        );
    }

    #[test]
    fn test_build_dir_expand_cmd_sh_shallow() {
        let paths = vec!["~/mydir".to_string(), "~/single.conf".to_string()];
        let cmd = build_dir_expand_cmd(&paths, false, ShellType::Sh);
        assert!(cmd.contains("---PATH:"));
        assert!(cmd.contains("[ -d"));
        assert!(cmd.contains("-maxdepth 1"));
        assert!(cmd.contains("find"));
        assert!(cmd.contains("mydir"));
        assert!(cmd.contains("single.conf"));
    }

    #[test]
    fn test_build_dir_expand_cmd_sh_recursive() {
        let paths = vec!["~/mydir".to_string()];
        let cmd = build_dir_expand_cmd(&paths, true, ShellType::Sh);
        assert!(cmd.contains("---PATH:"));
        assert!(cmd.contains("find"));
        assert!(
            !cmd.contains("-maxdepth"),
            "recursive should not have maxdepth"
        );
    }

    #[test]
    fn test_build_dir_expand_cmd_powershell() {
        let paths = vec!["~/mydir".to_string()];
        let cmd = build_dir_expand_cmd(&paths, false, ShellType::PowerShell);
        assert!(cmd.contains("---PATH:"));
        assert!(cmd.contains("Test-Path"));
        assert!(cmd.contains("Get-ChildItem"));
        assert!(
            !cmd.contains("-Recurse"),
            "shallow should not have -Recurse"
        );

        let cmd_recursive = build_dir_expand_cmd(&paths, true, ShellType::PowerShell);
        assert!(cmd_recursive.contains("-Recurse"));
    }

    #[test]
    fn test_parse_dir_expand_output_mixed() {
        let output = "---PATH:~/mydir\nDIR\n~/mydir/file1.txt\n~/mydir/file2.txt\n---PATH:~/single.conf\nFILE\n---PATH:~/gone\nMISSING\n";
        let paths = vec![
            "~/mydir".to_string(),
            "~/single.conf".to_string(),
            "~/gone".to_string(),
        ];
        let result = parse_dir_expand_output(output, &paths);
        assert_eq!(result.len(), 3);

        match &result["~/mydir"] {
            DirExpandResult::Directory(files) => {
                assert_eq!(files.len(), 2);
                assert_eq!(files[0], "~/mydir/file1.txt");
                assert_eq!(files[1], "~/mydir/file2.txt");
            }
            other => panic!("Expected Directory, got {:?}", other),
        }

        assert_eq!(result["~/single.conf"], DirExpandResult::File);
        assert_eq!(result["~/gone"], DirExpandResult::Missing);
    }

    #[test]
    fn test_parse_dir_expand_output_empty_dir() {
        let output = "---PATH:~/emptydir\nDIR\n";
        let paths = vec!["~/emptydir".to_string()];
        let result = parse_dir_expand_output(output, &paths);

        match &result["~/emptydir"] {
            DirExpandResult::Directory(files) => {
                assert!(files.is_empty(), "empty directory should have no files");
            }
            other => panic!("Expected Directory, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_dir_expand_output_nested() {
        let output = "---PATH:~/project\nDIR\n~/project/src/main.rs\n~/project/src/lib.rs\n~/project/Cargo.toml\n";
        let paths = vec!["~/project".to_string()];
        let result = parse_dir_expand_output(output, &paths);

        match &result["~/project"] {
            DirExpandResult::Directory(files) => {
                assert_eq!(files.len(), 3);
                assert!(files.contains(&"~/project/src/main.rs".to_string()));
                assert!(files.contains(&"~/project/src/lib.rs".to_string()));
                assert!(files.contains(&"~/project/Cargo.toml".to_string()));
            }
            other => panic!("Expected Directory, got {:?}", other),
        }
    }
}
