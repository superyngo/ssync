use anyhow::{Context as _, Result};

use crate::config::schema::HostEntry;
use crate::config::ssh_config;
use crate::host::session_pool::RusshSessionPool;
use crate::host::shell;
use crate::output::printer;
use crate::output::progress::SyncProgress;
use crate::output::summary::Summary;

use super::Context;

/// Partition connection failures into host-key verification errors and other errors.
#[allow(clippy::type_complexity)]
fn partition_host_key_failures(
    failures: Vec<(String, String)>,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut host_key_failures = Vec::new();
    let mut other_failures = Vec::new();
    for (name, err) in failures {
        if err.contains("Host key verification failed") {
            host_key_failures.push((name, err));
        } else {
            other_failures.push((name, err));
        }
    }
    (host_key_failures, other_failures)
}

/// Resolve the actual hostname and port for an SSH alias using `ssh -G`.
/// Returns (hostname, port). Falls back to (alias, "22") on failure.
/// Resolve SSH hostname and port for a host alias using ~/.ssh/config.
async fn resolve_ssh_host_port(alias: &str) -> Result<(String, u16)> {
    let resolved = crate::config::ssh_config::resolve_host(alias)?;
    Ok((resolved.hostname, resolved.port))
}

/// Run ssh-keyscan for a single host and return the output lines (key entries).
/// Returns Ok(output) on success, Err on failure or empty output.
async fn keyscan_host(alias: &str, timeout_secs: u64) -> Result<String> {
    let (hostname, port) = resolve_ssh_host_port(alias).await?;

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new("ssh-keyscan")
            .arg("-H")
            .arg("-p")
            .arg(port.to_string())
            .arg(&hostname)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .context("ssh-keyscan timeout")?
    .context("Failed to run ssh-keyscan")?;

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();

    // Filter out empty lines and comments
    let key_lines: String = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    if key_lines.is_empty() {
        anyhow::bail!("ssh-keyscan returned no keys for {}", alias);
    }

    Ok(key_lines)
}

/// Run ssh-keyscan for multiple hosts in parallel and append results to ~/.ssh/known_hosts.
/// Returns the list of host names that were successfully keyscanned.
async fn batch_keyscan_and_accept(
    hosts: &[(String, String)],
    timeout_secs: u64,
    concurrency: usize,
) -> Vec<String> {
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for (name, _err) in hosts {
        let sem = semaphore.clone();
        let alias = name.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = keyscan_host(&alias, timeout_secs).await;
            (alias, result)
        }));
    }

    let mut all_keys = String::new();
    let mut succeeded = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((alias, Ok(keys))) => {
                if !all_keys.is_empty() {
                    all_keys.push('\n');
                }
                all_keys.push_str(&keys);
                succeeded.push(alias.clone());
                printer::print_host_line(&alias, "ok", "host key accepted");
            }
            Ok((alias, Err(e))) => {
                printer::print_host_line(&alias, "error", &format!("keyscan failed: {}", e));
            }
            Err(e) => {
                tracing::warn!("keyscan task panicked: {}", e);
            }
        }
    }

    // Append all keys to ~/.ssh/known_hosts in one operation
    if !all_keys.is_empty() {
        let known_hosts_path = dirs::home_dir()
            .map(|h| h.join(".ssh").join("known_hosts"))
            .expect("Could not determine home directory");

        // Ensure the .ssh directory exists
        if let Some(parent) = known_hosts_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Ensure trailing newline before appending
        let mut content = String::new();
        if known_hosts_path.exists() {
            if let Ok(existing) = std::fs::read_to_string(&known_hosts_path) {
                if !existing.ends_with('\n') && !existing.is_empty() {
                    content.push('\n');
                }
            }
        }
        content.push_str(&all_keys);
        if !content.ends_with('\n') {
            content.push('\n');
        }

        use std::io::Write;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&known_hosts_path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(content.as_bytes()) {
                    eprintln!("Warning: failed to write known_hosts: {}", e);
                }
            }
            Err(e) => {
                eprintln!("Warning: failed to open known_hosts: {}", e);
            }
        }
    }

    succeeded
}

pub async fn run(ctx: &Context, update: bool, dry_run: bool, skip: Vec<String>) -> Result<()> {
    println!("Scanning ~/.ssh/config...");
    let ssh_hosts = ssh_config::parse_ssh_config()?;

    if ssh_hosts.is_empty() {
        println!("No hosts found in ~/.ssh/config");
        return Ok(());
    }

    // When config already exists, default to update behavior
    let config_exists = crate::config::app::resolve_path(ctx.config_path.as_deref())?.exists();
    let effective_update = update || config_exists;

    let mut stale_hosts_removed = false;
    let mut stale_host_names: Vec<String> = Vec::new();

    if config_exists {
        // Detect hosts in ssync config that no longer exist in ~/.ssh/config
        let ssh_host_names: std::collections::HashSet<&str> =
            ssh_hosts.iter().map(|h| h.name.as_str()).collect();
        stale_host_names = ctx
            .config
            .host
            .iter()
            .filter(|h| !ssh_host_names.contains(h.ssh_host.as_str()))
            .map(|h| h.ssh_host.clone())
            .collect();

        if !stale_host_names.is_empty() {
            println!(
                "\nFound {} host(s) no longer in ~/.ssh/config:",
                stale_host_names.len()
            );
            for name in &stale_host_names {
                println!("  - {}", name);
            }

            if dry_run {
                println!(
                    "[dry-run] Would remove {} stale host(s).",
                    stale_host_names.len()
                );
            } else {
                print!(
                    "Remove these {} host(s) from ssync config? [y/N]: ",
                    stale_host_names.len()
                );
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("y") {
                    stale_hosts_removed = true;
                    println!("Removed {} stale host(s).", stale_host_names.len());
                }
            }
        }
    }

    // Merge CLI --skip with persisted skipped_hosts
    let persisted_skips = &ctx.config.settings.skipped_hosts;
    let all_skips: Vec<String> = persisted_skips
        .iter()
        .cloned()
        .chain(skip.iter().cloned())
        .collect();

    // Filter to hosts that need shell detection
    let mut detect_hosts: Vec<String> = Vec::new();
    let mut summary = Summary::default();

    for ssh_host in &ssh_hosts {
        if all_skips.iter().any(|s| s == &ssh_host.name) {
            printer::print_host_line(&ssh_host.name, "skip", "skipped");
            summary.add_skip();
            continue;
        }
        let already_exists = ctx.config.host.iter().any(|h| h.ssh_host == ssh_host.name);
        if already_exists && !effective_update {
            continue;
        }
        detect_hosts.push(ssh_host.name.clone());
    }

    if detect_hosts.is_empty() {
        if skip.is_empty() {
            println!("No new hosts to detect.");
        }
        // Still may need to persist skip changes
    } else {
        println!(
            "Found {} host(s). Detecting shell types...",
            detect_hosts.len()
        );

        // Build temp HostEntry list for session pool setup
        let temp_entries: Vec<HostEntry> = detect_hosts
            .iter()
            .map(|name| HostEntry {
                name: name.clone(),
                ssh_host: name.clone(),
                shell: crate::config::schema::ShellType::Sh,
                groups: Vec::new(),
                proxy_jump: None,
            })
            .collect();
        let entry_refs: Vec<&HostEntry> = temp_entries.iter().collect();

        let mut progress = SyncProgress::new();
        progress.start_host_check(entry_refs.len());
        let session_pool =
            RusshSessionPool::setup(&entry_refs, ctx.timeout, ctx.concurrency()).await?;
        let connected = session_pool.reachable_hosts().len();
        let failed_count = entry_refs.len() - connected;
        progress.finish_host_check(connected, failed_count);

        // Partition failures: host key errors vs other errors
        let (host_key_failures, other_failures) =
            partition_host_key_failures(session_pool.failed_hosts());

        // Report non-host-key errors immediately
        for (name, err) in &other_failures {
            printer::print_host_line(name, "error", err);
            summary.add_failure(name, err);
        }

        // Handle host key verification failures
        let mut retry_pool: Option<RusshSessionPool> = None;
        if !host_key_failures.is_empty() {
            if dry_run {
                for (name, _err) in &host_key_failures {
                    printer::print_host_line(name, "skip", "unknown host key (dry-run, skipped)");
                    summary.add_skip();
                }
            } else {
                println!(
                    "\n{} host(s) have unknown SSH host keys:",
                    host_key_failures.len()
                );
                for (name, _err) in &host_key_failures {
                    println!("  - {}", name);
                }
                print!("Add to known_hosts and retry? [y/N]: ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;

                if answer.trim().eq_ignore_ascii_case("y") {
                    let accepted = batch_keyscan_and_accept(
                        &host_key_failures,
                        ctx.timeout,
                        ctx.concurrency(),
                    )
                    .await;

                    // Report hosts where keyscan failed
                    for (name, _err) in &host_key_failures {
                        if !accepted.contains(name) {
                            printer::print_host_line(name, "error", "keyscan failed");
                            summary.add_failure(name, "keyscan failed");
                        }
                    }

                    // Retry connection for accepted hosts
                    if !accepted.is_empty() {
                        let retry_entries: Vec<HostEntry> = accepted
                            .iter()
                            .map(|name| HostEntry {
                                name: name.clone(),
                                ssh_host: name.clone(),
                                shell: crate::config::schema::ShellType::Sh,
                                groups: Vec::new(),
                                proxy_jump: None,
                            })
                            .collect();
                        let retry_refs: Vec<&HostEntry> = retry_entries.iter().collect();

                        println!("\nRetrying {} host(s)...", accepted.len());
                        progress.start_host_check(retry_refs.len());
                        let rp =
                            RusshSessionPool::setup(&retry_refs, ctx.timeout, ctx.concurrency())
                                .await?;
                        let retry_connected = rp.reachable_hosts().len();
                        let retry_failed = retry_refs.len() - retry_connected;
                        progress.finish_host_check(retry_connected, retry_failed);

                        // Report retry failures
                        for (name, err) in rp.failed_hosts() {
                            printer::print_host_line(&name, "error", &err);
                            summary.add_failure(&name, &err);
                        }

                        retry_pool = Some(rp);
                    }
                } else {
                    // User declined — report as errors
                    for (name, err) in &host_key_failures {
                        printer::print_host_line(name, "error", err);
                        summary.add_failure(name, err);
                    }
                }
            }
        }

        // Detect shell type on reachable hosts using russh sessions
        let mut reachable = session_pool.reachable_hosts();
        if let Some(ref rp) = retry_pool {
            reachable.extend(rp.reachable_hosts());
        }

        let mut new_hosts: Vec<HostEntry> = Vec::new();
        for host_name in &reachable {
            let host_entry = HostEntry {
                name: host_name.clone(),
                ssh_host: host_name.clone(),
                shell: crate::config::schema::ShellType::Sh,
                groups: Vec::new(),
                proxy_jump: None,
            };
            let pool_ref = if session_pool.reachable_hosts().contains(host_name) {
                &session_pool
            } else if let Some(ref rp) = retry_pool {
                rp
            } else {
                continue;
            };
            match shell::detect_russh(&host_entry, pool_ref, ctx.timeout).await {
                Ok(shell_type) => {
                    printer::print_host_line(host_name, "ok", &format!("detected: {}", shell_type));
                    new_hosts.push(HostEntry {
                        name: host_name.clone(),
                        ssh_host: host_name.clone(),
                        shell: shell_type,
                        groups: Vec::new(),
                        proxy_jump: None,
                    });
                    summary.add_success();
                }
                Err(e) => {
                    printer::print_host_line(host_name, "error", &format!("{}", e));
                    summary.add_failure(host_name, &e.to_string());
                }
            }
        }

        session_pool.shutdown().await;
        if let Some(rp) = retry_pool {
            rp.shutdown().await;
        }
        progress.clear();
        summary.print();

        if dry_run {
            println!("\n[dry-run] No changes written.");
            return Ok(());
        }

        if new_hosts.is_empty() && skip.is_empty() && !stale_hosts_removed {
            println!("No new hosts to add.");
            return Ok(());
        }

        // Merge with existing config
        let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();
        if stale_hosts_removed {
            config
                .host
                .retain(|h| !stale_host_names.contains(&h.ssh_host));
        }
        for host in new_hosts {
            if let Some(existing) = config.host.iter_mut().find(|h| h.ssh_host == host.ssh_host) {
                existing.shell = host.shell;
            } else {
                config.host.push(host);
            }
        }

        // Persist newly skipped hosts (deduplicated)
        for s in &skip {
            if !config.settings.skipped_hosts.contains(s) {
                config.settings.skipped_hosts.push(s.clone());
            }
        }

        crate::config::app::save(&config, ctx.config_path.as_deref())?;
        let saved_path = crate::config::app::resolve_path(ctx.config_path.as_deref())?;
        println!("\nConfig saved to {}", saved_path.display());
        return Ok(());
    }

    // If we only had skip changes and no hosts to detect
    if !skip.is_empty() {
        summary.print();

        if dry_run {
            println!("\n[dry-run] No changes written.");
            return Ok(());
        }

        let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();
        if stale_hosts_removed {
            config
                .host
                .retain(|h| !stale_host_names.contains(&h.ssh_host));
            stale_hosts_removed = false;
        }
        for s in &skip {
            if !config.settings.skipped_hosts.contains(s) {
                config.settings.skipped_hosts.push(s.clone());
            }
        }
        crate::config::app::save(&config, ctx.config_path.as_deref())?;
        let saved_path = crate::config::app::resolve_path(ctx.config_path.as_deref())?;
        println!("\nConfig saved to {}", saved_path.display());
    }

    // Save if only stale hosts were removed (no other changes triggered a save)
    if stale_hosts_removed {
        let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();
        config
            .host
            .retain(|h| !stale_host_names.contains(&h.ssh_host));
        crate::config::app::save(&config, ctx.config_path.as_deref())?;
        let saved_path = crate::config::app::resolve_path(ctx.config_path.as_deref())?;
        println!("\nConfig saved to {}", saved_path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_host_falls_back_to_alias() {
        // An alias not in ~/.ssh/config should return alias as hostname, port 22
        let result = crate::config::ssh_config::resolve_host("nonexistent-test-host-xyz");
        let resolved = result.unwrap();
        assert_eq!(resolved.hostname, "nonexistent-test-host-xyz");
        assert_eq!(resolved.port, 22);
    }

    #[test]
    fn test_partition_host_key_failures_mixed() {
        let failures = vec![
            (
                "host-a".to_string(),
                "ControlMaster failed: Host key verification failed.".to_string(),
            ),
            ("host-b".to_string(), "Connection refused".to_string()),
            (
                "host-c".to_string(),
                "ControlMaster failed: Host key verification failed.".to_string(),
            ),
        ];
        let (hk, other) = partition_host_key_failures(failures);
        assert_eq!(hk.len(), 2);
        assert_eq!(hk[0].0, "host-a");
        assert_eq!(hk[1].0, "host-c");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].0, "host-b");
    }

    #[test]
    fn test_partition_host_key_failures_none() {
        let failures = vec![("host-a".to_string(), "Connection timeout".to_string())];
        let (hk, other) = partition_host_key_failures(failures);
        assert!(hk.is_empty());
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn test_partition_host_key_failures_all() {
        let failures = vec![(
            "host-a".to_string(),
            "Host key verification failed.".to_string(),
        )];
        let (hk, other) = partition_host_key_failures(failures);
        assert_eq!(hk.len(), 1);
        assert!(other.is_empty());
    }
}
