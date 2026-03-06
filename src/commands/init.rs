use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::config::schema::HostEntry;
use crate::config::ssh_config;
use crate::host::shell;
use crate::output::printer;
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

    // Merge CLI --skip with persisted skipped_hosts
    let persisted_skips = &ctx.config.settings.skipped_hosts;
    let all_skips: Vec<String> = persisted_skips
        .iter()
        .cloned()
        .chain(skip.iter().cloned())
        .collect();

    println!(
        "Found {} host(s). Detecting shell types...",
        ssh_hosts.len()
    );

    let mut new_hosts: Vec<HostEntry> = Vec::new();
    let mut summary = Summary::default();
    let semaphore = Arc::new(Semaphore::new(ctx.concurrency()));

    let mut handles = Vec::new();
    for ssh_host in &ssh_hosts {
        // Skip hosts from --skip and persisted skipped_hosts
        if all_skips.iter().any(|s| s == &ssh_host.name) {
            printer::print_host_line(&ssh_host.name, "skip", "skipped");
            summary.add_skip();
            continue;
        }

        // Skip if already exists and not updating
        let already_exists = ctx.config.host.iter().any(|h| h.ssh_host == ssh_host.name);
        if already_exists && !effective_update {
            continue;
        }

        let sem = semaphore.clone();
        let host_name = ssh_host.name.clone();

        let timeout = ctx.timeout;
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let shell_result = shell::detect(&host_name, timeout).await;
            (host_name, shell_result)
        }));
    }

    for handle in handles {
        let (host_name, shell_result) = handle.await?;
        match shell_result {
            Ok(shell_type) => {
                printer::print_host_line(&host_name, "ok", &format!("detected: {}", shell_type));
                new_hosts.push(HostEntry {
                    name: host_name.clone(),
                    ssh_host: host_name,
                    shell: shell_type,
                    groups: Vec::new(),
                });
                summary.add_success();
            }
            Err(e) => {
                // Failed hosts are NOT registered into config
                printer::print_host_line(&host_name, "error", &format!("{}", e));
                summary.add_failure(&host_name, &e.to_string());
            }
        }
    }

    summary.print();

    if dry_run {
        println!("\n[dry-run] No changes written.");
        return Ok(());
    }

    if new_hosts.is_empty() && skip.is_empty() {
        println!("No new hosts to add.");
        return Ok(());
    }

    // Merge with existing config
    let mut config = crate::config::app::load(ctx.config_path.as_deref())?.unwrap_or_default();
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

    Ok(())
}
