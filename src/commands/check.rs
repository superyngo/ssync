use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

use anyhow::Result;

use crate::config::schema::HostEntry;
use crate::host::pool::SshPool;
use crate::metrics::collector;
use crate::output::printer;
use crate::output::summary::Summary;
use crate::state::retention;

use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};

use super::report::{CheckHostResult, CheckReport, CommandReport, HostStatus, ProgressSink};
use super::{Context, TargetMode};

/// Per-host check configuration: (enabled_metrics, check_paths).
type HostCheckConfig = (Vec<String>, Vec<(String, String)>);

/// Pure command core: resolves targets, dispatches per-host metric collection,
/// writes snapshot rows to the DB, and returns a typed `CheckReport`.
///
/// No `println!`, no `output::printer` calls — those belong to the CLI
/// wrapper. Per-host events are surfaced via the optional `ProgressSink`
/// callbacks (called for both unreachable hosts from `SshPool::setup`
/// and reachable hosts after their `collect_pooled` future resolves).
///
/// DB writes (snapshot inserts, host_last_seen upserts) and retention
/// cleanup stay here because the TUI also needs them.
pub async fn check_core(
    ctx: &Context,
    progress: Option<&dyn ProgressSink>,
) -> Result<CommandReport> {
    let hosts = ctx.resolve_hosts()?;
    let host_configs = build_host_check_configs(ctx, &hosts);

    let run_start = chrono::Utc::now();
    let now_ts = run_start.timestamp();
    let executed_at = run_start.to_rfc3339();
    let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
    let enabled_metrics: Vec<String> = host_configs
        .values()
        .flat_map(|(enabled, _)| enabled.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if host_configs.is_empty() {
        return Ok(CommandReport::Check(CheckReport {
            executed_at,
            enabled_metrics,
            targets,
            hosts: Vec::new(),
        }));
    }

    let (pool, _connected) = SshPool::setup(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
    )
    .await?;

    let mut results: Vec<CheckHostResult> = Vec::new();

    // Unreachable hosts (from pool setup): report immediately, write offline rows.
    for (name, err) in pool.failed_hosts() {
        ctx.db.execute(
            "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, 0, '{}')",
            rusqlite::params![name, now_ts],
        )?;
        ctx.db.execute(
            "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, 0) \
             ON CONFLICT(host) DO UPDATE SET last_seen = ?2",
            rusqlite::params![name, now_ts],
        )?;
        let detail = format!("unreachable — {}", err);
        if let Some(p) = progress {
            p.host_completed(&name, HostStatus::Unreachable, &detail, 0);
        }
        results.push(CheckHostResult {
            host: name.clone(),
            status: HostStatus::Unreachable,
            duration_ms: None,
            detail,
            metrics_succeeded: 0,
            metrics_failed: 0,
            data: serde_json::json!({}),
            raw_stdout: String::new(),
            raw_stderr: String::new(),
        });
    }

    let reachable = pool.filter_reachable(&hosts);

    let mut handles = Vec::new();
    for host in &reachable {
        let (enabled, check_paths) = match host_configs.get(&host.name) {
            Some(config) => config.clone(),
            None => continue,
        };
        if let Some(p) = progress {
            p.host_started(&host.name);
        }
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
        let ms = elapsed.as_millis() as u64;

        match result {
            Ok(cr) => {
                let json_str = serde_json::to_string(&cr.data)?;
                let total = cr.succeeded + cr.failed;

                let (status, online_int, detail) = if cr.succeeded == 0 {
                    let err_detail = cr
                        .errors
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    (
                        HostStatus::Offline,
                        0_i64,
                        format!("failed ({:.1}s) — {}", elapsed.as_secs_f64(), err_detail),
                    )
                } else if cr.failed > 0 {
                    let warn_detail = cr
                        .errors
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    (
                        HostStatus::Partial,
                        1,
                        format!(
                            "partial ({}/{} metrics, {:.1}s) — warn: {}",
                            cr.succeeded,
                            total,
                            elapsed.as_secs_f64(),
                            warn_detail
                        ),
                    )
                } else {
                    (
                        HostStatus::Online,
                        1,
                        format!(
                            "collected ({} metrics, {:.1}s)",
                            cr.succeeded,
                            elapsed.as_secs_f64()
                        ),
                    )
                };

                ctx.db.execute(
                    "INSERT INTO check_snapshots (host, collected_at, online, raw_json) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![host.name, now, online_int, json_str],
                )?;
                if online_int == 1 {
                    ctx.db.execute(
                        "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, ?2) \
                         ON CONFLICT(host) DO UPDATE SET last_seen = ?2, last_online = ?2",
                        rusqlite::params![host.name, now],
                    )?;
                } else {
                    ctx.db.execute(
                        "INSERT INTO host_last_seen (host, last_seen, last_online) VALUES (?1, ?2, 0) \
                         ON CONFLICT(host) DO UPDATE SET last_seen = ?2",
                        rusqlite::params![host.name, now],
                    )?;
                }

                if let Some(p) = progress {
                    p.host_completed(&host.name, status, &detail, ms);
                }

                results.push(CheckHostResult {
                    host: host.name.clone(),
                    status,
                    duration_ms: Some(ms),
                    detail,
                    metrics_succeeded: cr.succeeded,
                    metrics_failed: cr.failed,
                    data: cr.data,
                    raw_stdout: cr.metrics_raw_stdout,
                    raw_stderr: cr.metrics_raw_stderr,
                });
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
                let detail = e.to_string();
                if let Some(p) = progress {
                    p.host_completed(&host.name, HostStatus::Error, &detail, ms);
                }
                results.push(CheckHostResult {
                    host: host.name.clone(),
                    status: HostStatus::Error,
                    duration_ms: Some(ms),
                    detail,
                    metrics_succeeded: 0,
                    metrics_failed: 0,
                    data: serde_json::json!({}),
                    raw_stdout: String::new(),
                    raw_stderr: String::new(),
                });
            }
        }
    }

    pool.shutdown().await;
    retention::cleanup(&ctx.db, ctx.config.settings.data_retention_days)?;

    Ok(CommandReport::Check(CheckReport {
        executed_at,
        enabled_metrics,
        targets,
        hosts: results,
    }))
}

/// Thin CLI wrapper: invokes `check_core` with a printer-driven
/// `ProgressSink`, prints the run summary, and writes `--out` reports.
pub async fn run(ctx: &Context, output: &crate::cli::OutputArgs) -> Result<()> {
    let host_configs_empty_hint = {
        // We need to detect the "no entries" case before invoking core to
        // print the same hint as before. Cheap: re-derive host_configs.
        let hosts = ctx.resolve_hosts()?;
        build_host_check_configs(ctx, &hosts).is_empty()
    };
    if host_configs_empty_hint {
        println!("No check entries matched the current filter. Add [[check]] to config.toml.");
        return Ok(());
    }

    let sink = PrinterSink;
    let CommandReport::Check(report) = check_core(ctx, Some(&sink)).await?;

    // Build legacy Summary + OperationReport from the typed CheckReport.
    let mut summary = Summary::default();
    let mut report_results: Vec<HostResult> = Vec::new();
    for h in &report.hosts {
        match h.status {
            HostStatus::Online | HostStatus::Partial => summary.add_success(),
            HostStatus::Offline | HostStatus::Error | HostStatus::TimedOut => {
                summary.add_failure(&h.host, &h.detail);
            }
            HostStatus::Unreachable => {
                let err = h
                    .detail
                    .strip_prefix("unreachable — ")
                    .unwrap_or(&h.detail);
                summary.add_failure(&h.host, err);
            }
        }
        let status = match h.status {
            HostStatus::Online | HostStatus::Partial => "success",
            _ => "error",
        };
        let output_json = if matches!(h.status, HostStatus::Unreachable) {
            serde_json::json!({
                "metrics": {},
                "probe_outputs": {},
                "error": format!(
                    "unreachable: {}",
                    h.detail.strip_prefix("unreachable — ").unwrap_or(&h.detail)
                ),
            })
        } else if matches!(h.status, HostStatus::Error) {
            serde_json::json!({
                "metrics": {},
                "probe_outputs": {},
                "error": h.detail,
            })
        } else {
            serde_json::json!({
                "metrics": h.data,
                "probe_outputs": {
                    "metrics_batch": { "stdout": h.raw_stdout, "stderr": h.raw_stderr }
                },
            })
        };
        report_results.push(HostResult {
            host: h.host.clone(),
            status: status.to_string(),
            duration_ms: h.duration_ms,
            output: output_json,
        });
    }

    summary.print();

    if let Some(out) = &output.out {
        let rep_summary = ReportSummary {
            total: report_results.len(),
            success: report_results
                .iter()
                .filter(|r| r.status == "success")
                .count(),
            failed: report_results
                .iter()
                .filter(|r| r.status == "error")
                .count(),
            skipped: 0,
        };
        let op_report = OperationReport {
            executed_at: report.executed_at,
            command: "check".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({ "metrics": report.enabled_metrics }),
            targets: report.targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(
            &op_report,
            out,
            "check",
            ctx.config.settings.default_output_format.as_deref(),
        )?;
    }

    Ok(())
}

/// `ProgressSink` impl that prints to stdout via the existing `output::printer`.
/// Maintains byte-for-byte compatibility with the pre-refactor CLI output.
struct PrinterSink;

impl ProgressSink for PrinterSink {
    fn host_started(&self, _host: &str) {
        // The pre-refactor CLI did not print a "started" line; leave silent
        // to keep stdout byte-identical.
    }

    fn host_completed(&self, host: &str, status: HostStatus, detail: &str, _ms: u64) {
        let kind = match status {
            HostStatus::Online => "ok",
            HostStatus::Partial => "skip",
            _ => "error",
        };
        printer::print_host_line(host, kind, detail);
    }
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
