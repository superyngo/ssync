use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::Semaphore;

use crate::metrics::collector;
use crate::output::printer;
use crate::output::summary::Summary;
use crate::state::retention;

use super::Context;

pub async fn run(ctx: &Context) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;
    let checks = ctx.resolve_checks();

    // Merge enabled metrics and paths from all applicable check entries
    let mut enabled: Vec<String> = Vec::new();
    let mut check_paths: Vec<(String, String)> = Vec::new();
    for entry in &checks {
        for m in &entry.enabled {
            if !enabled.contains(m) {
                enabled.push(m.clone());
            }
        }
        for p in &entry.path {
            let key = (p.path.clone(), p.label.clone());
            if !check_paths.contains(&key) {
                check_paths.push(key);
            }
        }
    }

    if enabled.is_empty() && check_paths.is_empty() {
        println!("No check entries matched the current filter. Add [[check]] to config.toml.");
        return Ok(());
    }
    let semaphore = Arc::new(Semaphore::new(ctx.concurrency()));
    let mut summary = Summary::default();

    let mut handles = Vec::new();
    for host in &hosts {
        let sem = semaphore.clone();
        let host = (*host).clone();
        let enabled = enabled.clone();
        let check_paths = check_paths.clone();
        let timeout = ctx.timeout;

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let start = Instant::now();
            let result = collector::collect(&host, &enabled, &check_paths, timeout).await;
            let elapsed = start.elapsed();
            (host, result, elapsed)
        }));
    }

    for handle in handles {
        let (host, result, elapsed) = handle.await?;
        let now = chrono::Utc::now().timestamp();

        match result {
            Ok(cr) => {
                let json_str = serde_json::to_string(&cr.data)?;

                if cr.succeeded == 0 {
                    // All metrics failed — treat as offline
                    ctx.db.execute(
                        "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 0, ?3)",
                        rusqlite::params![host.name, now, json_str],
                    )?;

                    ctx.db.execute(
                        "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, 0) \
                         ON CONFLICT(host) DO UPDATE SET last_seen = ?2",
                        rusqlite::params![host.name, now],
                    )?;

                    let err_detail = cr.errors.first().map(|s| s.as_str()).unwrap_or("unknown");
                    printer::print_host_line(
                        &host.name,
                        "error",
                        &format!("failed ({:.1}s) — {}", elapsed.as_secs_f64(), err_detail),
                    );
                    summary.add_failure(&host.name, err_detail);
                } else if cr.failed > 0 {
                    // Partial success — online but with warnings
                    ctx.db.execute(
                        "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 1, ?3)",
                        rusqlite::params![host.name, now, json_str],
                    )?;

                    ctx.db.execute(
                        "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, ?2) \
                         ON CONFLICT(host) DO UPDATE SET last_seen = ?2, last_online = ?2",
                        rusqlite::params![host.name, now],
                    )?;

                    let warn_detail = cr.errors.first().map(|s| s.as_str()).unwrap_or("unknown");
                    printer::print_host_line(
                        &host.name,
                        "skip",
                        &format!(
                            "partial ({}/{} metrics, {:.1}s) — warn: {}",
                            cr.succeeded,
                            cr.succeeded + cr.failed,
                            elapsed.as_secs_f64(),
                            warn_detail,
                        ),
                    );
                    summary.add_success();
                } else {
                    // Full success
                    ctx.db.execute(
                        "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 1, ?3)",
                        rusqlite::params![host.name, now, json_str],
                    )?;

                    ctx.db.execute(
                        "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, ?2) \
                         ON CONFLICT(host) DO UPDATE SET last_seen = ?2, last_online = ?2",
                        rusqlite::params![host.name, now],
                    )?;

                    printer::print_host_line(
                        &host.name,
                        "ok",
                        &format!(
                            "collected ({} metrics, {:.1}s)",
                            cr.succeeded,
                            elapsed.as_secs_f64()
                        ),
                    );
                    summary.add_success();
                }
            }
            Err(e) => {
                // SSH connection itself failed
                ctx.db.execute(
                    "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 0, '{}')",
                    rusqlite::params![host.name, now],
                )?;

                ctx.db.execute(
                    "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, 0) \
                     ON CONFLICT(host) DO UPDATE SET last_seen = ?2",
                    rusqlite::params![host.name, now],
                )?;

                printer::print_host_line(&host.name, "error", &e.to_string());
                summary.add_failure(&host.name, &e.to_string());
            }
        }
    }

    summary.print();

    // Retention cleanup
    retention::cleanup(&ctx.db, ctx.config.settings.data_retention_days)?;

    Ok(())
}
