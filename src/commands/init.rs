use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::config::ssh_config;
use crate::host::connection::ConnectionManager;
use crate::host::shell;
use crate::output::printer;
use crate::output::progress::SyncProgress;
use crate::output::summary::Summary;

use super::Context;

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

        // Build temp HostEntry list for ConnectionManager pre-check
        let temp_entries: Vec<HostEntry> = detect_hosts
            .iter()
            .map(|name| HostEntry {
                name: name.clone(),
                ssh_host: name.clone(),
                shell: crate::config::schema::ShellType::Sh,
                groups: Vec::new(),
            })
            .collect();
        let entry_refs: Vec<&HostEntry> = temp_entries.iter().collect();

        // Pre-check connectivity via ControlMaster
        let mut conn_mgr = ConnectionManager::new()?;
        let mut progress = SyncProgress::new();

        progress.start_host_check(entry_refs.len());
        let connected = conn_mgr
            .pre_check(&entry_refs, ctx.timeout, ctx.concurrency())
            .await;
        let failed_count = entry_refs.len() - connected;
        progress.finish_host_check(connected, failed_count);

        // Report unreachable hosts
        for (name, err) in conn_mgr.failed_hosts() {
            printer::print_host_line(&name, "error", &err.to_string());
            summary.add_failure(&name, &err);
        }

        // Detect shell type on reachable hosts using pooled connections
        let reachable = conn_mgr.reachable_hosts();
        let global_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(ctx.concurrency()));

        let mut handles = Vec::new();
        for host_name in &reachable {
            let sem = global_sem.clone();
            let host_name = host_name.clone();
            let timeout = ctx.timeout;
            let socket = conn_mgr.socket_for(&host_name).map(|p| p.to_path_buf());

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let shell_result =
                    shell::detect_pooled(&host_name, timeout, socket.as_deref()).await;
                (host_name, shell_result)
            }));
        }

        let mut new_hosts: Vec<HostEntry> = Vec::new();
        for handle in handles {
            let (host_name, shell_result) = handle.await?;
            match shell_result {
                Ok(shell_type) => {
                    printer::print_host_line(
                        &host_name,
                        "ok",
                        &format!("detected: {}", shell_type),
                    );
                    new_hosts.push(HostEntry {
                        name: host_name.clone(),
                        ssh_host: host_name,
                        shell: shell_type,
                        groups: Vec::new(),
                    });
                    summary.add_success();
                }
                Err(e) => {
                    printer::print_host_line(&host_name, "error", &format!("{}", e));
                    summary.add_failure(&host_name, &e.to_string());
                }
            }
        }

        conn_mgr.shutdown().await;
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
            config.host.retain(|h| !stale_host_names.contains(&h.ssh_host));
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
            config.host.retain(|h| !stale_host_names.contains(&h.ssh_host));
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
        config.host.retain(|h| !stale_host_names.contains(&h.ssh_host));
        crate::config::app::save(&config, ctx.config_path.as_deref())?;
        let saved_path = crate::config::app::resolve_path(ctx.config_path.as_deref())?;
        println!("\nConfig saved to {}", saved_path.display());
    }

    Ok(())
}
