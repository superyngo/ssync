use anyhow::Result;
use tokio::sync::Semaphore;
use std::sync::Arc;

use crate::config::schema::HostEntry;
use crate::config::ssh_config;
use crate::host::shell;
use crate::output::printer;
use crate::output::summary::Summary;

use super::Context;

pub async fn run(ctx: &Context, update: bool, dry_run: bool) -> Result<()> {
    println!("Scanning ~/.ssh/config...");
    let ssh_hosts = ssh_config::parse_ssh_config()?;

    if ssh_hosts.is_empty() {
        println!("No hosts found in ~/.ssh/config");
        return Ok(());
    }

    println!("Found {} host(s). Detecting shell types...", ssh_hosts.len());

    let mut new_hosts: Vec<HostEntry> = Vec::new();
    let mut summary = Summary::default();
    let semaphore = Arc::new(Semaphore::new(ctx.concurrency()));

    let mut handles = Vec::new();
    for ssh_host in &ssh_hosts {
        // Skip if already exists and not updating
        let already_exists = ctx.config.host.iter().any(|h| h.ssh_host == ssh_host.name);
        if already_exists && !update {
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
                printer::print_host_line(
                    &host_name,
                    "ok",
                    &format!("detected: {}", shell_type),
                );
                new_hosts.push(HostEntry {
                    name: host_name.clone(),
                    ssh_host: host_name,
                    shell: shell_type,
                    groups: vec!["all".to_string()],
                });
                summary.add_success();
            }
            Err(e) => {
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

    if new_hosts.is_empty() {
        println!("No new hosts to add.");
        return Ok(());
    }

    // Merge with existing config
    let mut config = crate::config::app::load()?.unwrap_or_default();
    for host in new_hosts {
        if let Some(existing) = config.host.iter_mut().find(|h| h.ssh_host == host.ssh_host) {
            existing.shell = host.shell;
        } else {
            config.host.push(host);
        }
    }

    crate::config::app::save(&config)?;
    println!("\nConfig saved to {}", crate::config::app::config_path()?.display());

    Ok(())
}
