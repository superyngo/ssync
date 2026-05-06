use std::time::Instant;

use anyhow::Result;

use crate::host::pool::SshPool;
use crate::host::shell;
use crate::output::printer;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
use crate::output::summary::Summary;

use super::report::{CommandReport, HostStatus, ProgressSink, RunHostResult, RunReport};
use super::Context;

/// Pure command core: resolves targets, spawns per-host `run` tasks, writes
/// to the operation log, and returns a typed `RunReport`.
///
/// No `println!`, no `output::printer` calls — those belong to the CLI
/// wrapper. Per-host events are surfaced via the optional `ProgressSink`.
pub async fn run_core(
    ctx: &Context,
    command: &str,
    sudo: bool,
    _yes: bool,
    progress: Option<&dyn ProgressSink>,
) -> Result<CommandReport> {
    let hosts = ctx.resolve_hosts()?;
    let executed_at = chrono::Utc::now().to_rfc3339();
    let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();

    let (pool, _connected) = SshPool::setup(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
    )
    .await?;

    let mut host_results: Vec<RunHostResult> = Vec::new();

    // Report unreachable hosts.
    for (name, err) in pool.failed_hosts() {
        let detail = format!("unreachable — {}", err);
        if let Some(p) = progress {
            p.host_completed(&name, HostStatus::Unreachable, &detail, 0);
        }
        host_results.push(RunHostResult {
            host: name.clone(),
            status: HostStatus::Unreachable,
            duration_ms: None,
            detail,
            stdout: String::new(),
            stderr: err.clone(),
        });
    }

    let reachable = pool.filter_reachable(&hosts);

    let mut handles = Vec::new();
    for host in &reachable {
        let host = (*host).clone();
        let cmd = if sudo {
            shell::sudo_wrap(host.shell, command)
        } else {
            command.to_string()
        };
        let timeout = ctx.timeout;
        let sessions = pool.session_pool.clone();
        let global_sem = pool.limiter.global_semaphore();
        if let Some(p) = progress {
            p.host_started(&host.name);
        }

        handles.push(tokio::spawn(async move {
            let _permit = global_sem.acquire_owned().await.unwrap();
            let start = Instant::now();
            let result = sessions.exec(&host.ssh_host, &cmd, timeout).await;
            let elapsed = start.elapsed();
            (host, result, elapsed)
        }));
    }

    for handle in handles {
        let (host, result, elapsed) = handle.await?;
        let ms = elapsed.as_millis() as u64;
        let now = chrono::Utc::now().timestamp();

        match result {
            Ok(exec_output) => {
                let (status, detail) = if exec_output.success {
                    let first_line = exec_output.stdout.lines().next().unwrap_or("").to_string();
                    let detail = if first_line.is_empty() {
                        format!("ok ({:.1}s)", elapsed.as_secs_f64())
                    } else {
                        format!("ok ({:.1}s) — {}", elapsed.as_secs_f64(), first_line)
                    };
                    (HostStatus::Online, detail)
                } else {
                    let msg = exec_output.stderr.trim().to_string();
                    (
                        HostStatus::Error,
                        format!("error ({:.1}s) — {}", elapsed.as_secs_f64(), msg),
                    )
                };

                if let Some(p) = progress {
                    p.host_completed(&host.name, status, &detail, ms);
                }

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        now,
                        host.name,
                        command,
                        if matches!(status, HostStatus::Online) { "ok" } else { "error" },
                        elapsed.as_millis() as i64,
                        if matches!(status, HostStatus::Error) { Some(exec_output.stderr.trim().to_string()) } else { None::<String> },
                    ],
                )?;

                host_results.push(RunHostResult {
                    host: host.name.clone(),
                    status,
                    duration_ms: Some(ms),
                    detail,
                    stdout: exec_output.stdout,
                    stderr: exec_output.stderr,
                });
            }
            Err(e) => {
                let detail = format!("error ({:.1}s) — {}", elapsed.as_secs_f64(), e);
                if let Some(p) = progress {
                    p.host_completed(&host.name, HostStatus::Error, &detail, ms);
                }
                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, 'error', ?4, ?5)",
                    rusqlite::params![
                        now, host.name, command,
                        elapsed.as_millis() as i64,
                        e.to_string(),
                    ],
                )?;
                host_results.push(RunHostResult {
                    host: host.name.clone(),
                    status: HostStatus::Error,
                    duration_ms: Some(ms),
                    detail,
                    stdout: String::new(),
                    stderr: e.to_string(),
                });
            }
        }
    }

    pool.shutdown().await;

    Ok(CommandReport::Run(RunReport {
        executed_at,
        command: command.to_string(),
        targets,
        hosts: host_results,
    }))
}

/// Thin CLI wrapper: invokes `run_core` with a printer-driven `ProgressSink`,
/// prints the run summary, and writes `--out` reports.
pub async fn run(
    ctx: &Context,
    command: &str,
    sudo: bool,
    yes: bool,
    output: &crate::cli::OutputArgs,
) -> Result<()> {
    let sink = PrinterSink;
    let CommandReport::Run(report) = run_core(ctx, command, sudo, yes, Some(&sink)).await? else {
        unreachable!("run_core always returns CommandReport::Run")
    };

    let mut summary = Summary::default();
    let mut report_results: Vec<HostResult> = Vec::new();
    for h in &report.hosts {
        match h.status {
            HostStatus::Online => summary.add_success(),
            HostStatus::Unreachable | HostStatus::Error => {
                summary.add_failure(&h.host, &h.detail);
            }
            _ => {}
        }
        let status_str = match h.status {
            HostStatus::Online => "success",
            _ => "error",
        };
        report_results.push(HostResult {
            host: h.host.clone(),
            status: status_str.to_string(),
            duration_ms: h.duration_ms,
            output: serde_json::json!({
                "stdout": h.stdout,
                "stderr": h.stderr,
            }),
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
        let targets: Vec<String> = ctx
            .resolve_hosts()?
            .iter()
            .map(|h| h.name.clone())
            .collect();
        let rep = OperationReport {
            executed_at: report.executed_at,
            command: "run".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({ "command": command }),
            targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(
            &rep,
            out,
            "run",
            ctx.config.settings.default_output_format.as_deref(),
        )?;
    }

    Ok(())
}

/// `ProgressSink` impl that prints to stdout via the existing `output::printer`.
struct PrinterSink;

impl ProgressSink for PrinterSink {
    fn host_started(&self, _host: &str) {}

    fn host_completed(&self, host: &str, status: HostStatus, detail: &str, _ms: u64) {
        let kind = match status {
            HostStatus::Online => "ok",
            _ => "error",
        };
        printer::print_host_line(host, kind, detail);
    }
}
