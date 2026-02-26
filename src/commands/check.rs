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
    let hosts = ctx.require_targets()?;

    let check_paths: Vec<(String, String)> = ctx
        .config
        .check
        .path
        .iter()
        .map(|p| (p.path.clone(), p.label.clone()))
        .collect();

    let enabled = &ctx.config.check.enabled;
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
            Ok(json_value) => {
                let metric_count = json_value.as_object().map(|m| m.len()).unwrap_or(0);
                let json_str = serde_json::to_string(&json_value)?;

                // Store snapshot
                ctx.db.execute(
                    "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 1, ?3)",
                    rusqlite::params![host.name, now, json_str],
                )?;

                // Update last seen
                ctx.db.execute(
                    "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, ?2) \
                     ON CONFLICT(host) DO UPDATE SET last_seen = ?2, last_online = ?2",
                    rusqlite::params![host.name, now],
                )?;

                printer::print_host_line(
                    &host.name,
                    "ok",
                    &format!("collected ({} metrics, {:.1}s)", metric_count, elapsed.as_secs_f64()),
                );
                summary.add_success();
            }
            Err(e) => {
                // Record offline
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
