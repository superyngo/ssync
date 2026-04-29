use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::host::pool::SshPool;
use crate::metrics::collector;
use crate::output::printer;
use crate::output::summary::Summary;
use crate::state::retention;

use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};

use super::{Context, TargetMode};

/// Per-host check configuration: (enabled_metrics, check_paths).
type HostCheckConfig = (Vec<String>, Vec<(String, String)>);

pub async fn run(ctx: &Context, output: &crate::cli::OutputArgs) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;

    // Build per-host check config: each host gets its own (enabled, check_paths).
    // For --groups: scoped by group membership with entry dedup.
    // For --hosts/--all: flat merge of all matching entries.
    let host_configs = build_host_check_configs(ctx, &hosts);

    if host_configs.is_empty() {
        println!("No check entries matched the current filter. Add [[check]] to config.toml.");
        return Ok(());
    }

    // Set up SSH connection pool (ControlMaster pre-check + concurrency limiter)
    let (mut pool, _connected) = SshPool::setup(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
    )
    .await?;

    // Report unreachable hosts immediately
    let now_ts = chrono::Utc::now().timestamp();
    let mut summary = Summary::default();
    let mut report_results: Vec<HostResult> = Vec::new();
    let executed_at = chrono::Utc::now().to_rfc3339();
    for (name, err) in pool.failed_hosts() {
        // Write offline status to DB
        ctx.db.execute(
            "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 0, '{}')",
            rusqlite::params![name, now_ts],
        )?;
        ctx.db.execute(
            "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, 0) \
             ON CONFLICT(host) DO UPDATE SET last_seen = ?2",
            rusqlite::params![name, now_ts],
        )?;
        printer::print_host_line(&name, "error", &format!("unreachable — {}", err));
        report_results.push(HostResult {
            host: name.clone(),
            status: "error".to_string(),
            duration_ms: None,
            output: serde_json::json!({
                "metrics": {},
                "probe_outputs": {},
                "error": format!("unreachable: {}", err),
            }),
        });
    }

    for (name, err) in pool.failed_hosts() {
        summary.add_failure(&name, &err);
    }
    let reachable = pool.filter_reachable(&hosts);
    pool.progress.start_collect(reachable.len());

    let mut handles = Vec::new();
    for host in &reachable {
        let (enabled, check_paths) = match host_configs.get(&host.name) {
            Some(config) => config.clone(),
            None => continue,
        };
        let host = (*host).clone();
        let timeout = ctx.timeout;
        let sessions = pool.session_pool.clone();
        let global_sem = pool.limiter.global_semaphore();

        handles.push(tokio::spawn(async move {
            let _permit = global_sem.acquire_owned().await.unwrap();
            let start = Instant::now();
            let result =
                collector::collect_pooled(&host, &enabled, &check_paths, timeout, sessions).await;
            let elapsed = start.elapsed();
            (host, result, elapsed)
        }));
    }

    for handle in handles {
        let (host, result, elapsed) = handle.await?;
        let now = chrono::Utc::now().timestamp();
        pool.progress.host_collected();

        match result {
            Ok(cr) => {
                let json_str = serde_json::to_string(&cr.data)?;

                if cr.succeeded == 0 {
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
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "error".to_string(),
                        duration_ms: Some(elapsed.as_millis() as u64),
                        output: serde_json::json!({
                            "metrics": cr.data,
                            "probe_outputs": {
                                "metrics_batch": { "stdout": cr.metrics_raw_stdout, "stderr": cr.metrics_raw_stderr }
                            },
                        }),
                    });
                } else if cr.failed > 0 {
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
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "success".to_string(),
                        duration_ms: Some(elapsed.as_millis() as u64),
                        output: serde_json::json!({
                            "metrics": cr.data,
                            "probe_outputs": {
                                "metrics_batch": { "stdout": cr.metrics_raw_stdout, "stderr": cr.metrics_raw_stderr }
                            },
                        }),
                    });
                } else {
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
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "success".to_string(),
                        duration_ms: Some(elapsed.as_millis() as u64),
                        output: serde_json::json!({
                            "metrics": cr.data,
                            "probe_outputs": {
                                "metrics_batch": { "stdout": cr.metrics_raw_stdout, "stderr": cr.metrics_raw_stderr }
                            },
                        }),
                    });
                }
            }
            Err(e) => {
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
                report_results.push(HostResult {
                    host: host.name.clone(),
                    status: "error".to_string(),
                    duration_ms: Some(elapsed.as_millis() as u64),
                    output: serde_json::json!({
                        "metrics": {},
                        "probe_outputs": {},
                        "error": e.to_string(),
                    }),
                });
            }
        }
    }

    pool.progress.finish_collect();
    pool.shutdown().await;

    summary.print();
    retention::cleanup(&ctx.db, ctx.config.settings.data_retention_days)?;

    if let Some(out) = &output.out {
        let enabled_metrics: Vec<String> = host_configs
            .values()
            .flat_map(|(enabled, _)| enabled.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let rep_summary = ReportSummary {
            total: report_results.len(),
            success: report_results.iter().filter(|r| r.status == "success").count(),
            failed: report_results.iter().filter(|r| r.status == "error").count(),
            skipped: 0,
        };
        let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let report = OperationReport {
            executed_at,
            command: "check".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({ "metrics": enabled_metrics }),
            targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(&report, out, "check")?;
    }

    Ok(())
}

/// Build per-host check configuration based on target mode.
/// For --groups: each host gets metrics only from entries matching its groups (with entry dedup).
/// For --hosts/--all: all hosts get the same merged metrics (flat merge).
fn build_host_check_configs(
    ctx: &Context,
    hosts: &[&HostEntry],
) -> HashMap<String, HostCheckConfig> {
    match &ctx.mode {
        TargetMode::Groups(groups) => {
            let mut configs = HashMap::new();
            for host in hosts {
                let mut enabled: Vec<String> = Vec::new();
                let mut check_paths: Vec<(String, String)> = Vec::new();
                let mut seen_entries = HashSet::new();

                for group in &host.groups {
                    if !groups.contains(group) {
                        continue;
                    }
                    for entry in ctx.resolve_checks_for_group(group) {
                        let ptr = std::ptr::from_ref(entry) as usize;
                        if !seen_entries.insert(ptr) {
                            continue;
                        }
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
                }

                if !enabled.is_empty() || !check_paths.is_empty() {
                    configs.insert(host.name.clone(), (enabled, check_paths));
                }
            }
            configs
        }
        _ => {
            // Flat merge for --hosts, --shell, and --all
            let checks = ctx.resolve_checks();
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
            let mut configs = HashMap::new();
            if !enabled.is_empty() || !check_paths.is_empty() {
                for host in hosts {
                    configs.insert(host.name.clone(), (enabled.clone(), check_paths.clone()));
                }
            }
            configs
        }
    }
}
