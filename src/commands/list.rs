use anyhow::Result;

use super::Context;

pub async fn run(ctx: &Context) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;
    let checks = ctx.resolve_checks();
    let syncs = ctx.resolve_syncs();

    // ── Hosts ──
    println!("── Hosts ({}) ──", hosts.len());
    println!("  {:<16} {:<20} {:<12} Groups", "Name", "SSH Host", "Shell");
    println!("  {}", "-".repeat(64));
    for h in &hosts {
        let groups = if h.groups.is_empty() {
            "-".to_string()
        } else {
            h.groups.join(", ")
        };
        println!(
            "  {:<16} {:<20} {:<12} {}",
            h.name, h.ssh_host, h.shell, groups
        );
    }

    // ── Checks ──
    println!("\n── Applicable Checks ({}) ──", checks.len());
    if checks.is_empty() {
        println!("  (none)");
    } else {
        for (i, entry) in checks.iter().enumerate() {
            let scope = format_scope(&entry.groups, &entry.hosts);
            println!("  [{}] scope: {}", i + 1, scope);
            if !entry.enabled.is_empty() {
                println!("      enabled: {}", entry.enabled.join(", "));
            }
            for p in &entry.path {
                println!("      path: {} ({})", p.path, p.label);
            }
        }
    }

    // ── Sync ──
    println!("\n── Applicable Sync Entries ({}) ──", syncs.len());
    if syncs.is_empty() {
        println!("  (none)");
    } else {
        for (i, entry) in syncs.iter().enumerate() {
            let scope = format_scope(&entry.groups, &entry.hosts);
            println!(
                "  [{}] scope: {}  paths: {}",
                i + 1,
                scope,
                entry.paths.join(", ")
            );
        }
    }

    Ok(())
}

fn format_scope(groups: &[String], hosts: &[String]) -> String {
    if !groups.is_empty() {
        format!("groups=[{}]", groups.join(", "))
    } else if !hosts.is_empty() {
        format!("hosts=[{}]", hosts.join(", "))
    } else {
        "global".to_string()
    }
}
