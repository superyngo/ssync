use std::time::Instant;

use anyhow::Result;

use crate::host::pool::SshPool;
use crate::host::shell;
use crate::output::printer;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
use crate::output::summary::Summary;

use super::Context;

pub async fn run(
    ctx: &Context,
    command: &str,
    sudo: bool,
    _yes: bool,
    output: &crate::cli::OutputArgs,
) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;

    // Set up SSH connection pool
    let (pool, _connected) = SshPool::setup(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
    )
    .await?;

    let mut summary = Summary::default();
    let mut report_results: Vec<HostResult> = Vec::new();

    // Report unreachable hosts
    for (name, err) in pool.failed_hosts() {
        printer::print_host_line(&name, "error", &format!("unreachable — {}", err));
        summary.add_failure(&name, &err);
        report_results.push(HostResult {
            host: name.clone(),
            status: "error".to_string(),
            duration_ms: None,
            output: serde_json::json!({ "stdout": "", "stderr": format!("unreachable: {}", err) }),
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
        let duration_ms = elapsed.as_millis() as u64;
        let now = chrono::Utc::now().timestamp();

        match result {
            Ok(exec_output) => {
                if exec_output.success {
                    for line in exec_output.stdout.lines() {
                        printer::print_host_line(&host.name, "ok", line);
                    }
                    summary.add_success();
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "success".to_string(),
                        duration_ms: Some(duration_ms),
                        output: serde_json::json!({
                            "stdout": exec_output.stdout,
                            "stderr": exec_output.stderr,
                        }),
                    });
                } else {
                    let msg = exec_output.stderr.trim().to_string();
                    printer::print_host_line(&host.name, "error", &msg);
                    summary.add_failure(&host.name, &msg);
                    report_results.push(HostResult {
                        host: host.name.clone(),
                        status: "error".to_string(),
                        duration_ms: Some(duration_ms),
                        output: serde_json::json!({
                            "stdout": exec_output.stdout,
                            "stderr": exec_output.stderr,
                        }),
                    });
                }

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        now,
                        host.name,
                        command,
                        if exec_output.success { "ok" } else { "error" },
                        elapsed.as_millis() as i64,
                        if exec_output.success { None::<String> } else { Some(exec_output.stderr.trim().to_string()) },
                    ],
                )?;
            }
            Err(e) => {
                printer::print_host_line(&host.name, "error", &e.to_string());
                summary.add_failure(&host.name, &e.to_string());
                report_results.push(HostResult {
                    host: host.name.clone(),
                    status: "error".to_string(),
                    duration_ms: Some(duration_ms),
                    output: serde_json::json!({ "stdout": "", "stderr": e.to_string() }),
                });

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, 'error', ?4, ?5)",
                    rusqlite::params![
                        now, host.name, command,
                        elapsed.as_millis() as i64,
                        e.to_string(),
                    ],
                )?;
            }
        }
    }

    pool.shutdown().await;
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
        let targets: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let report = OperationReport {
            executed_at: chrono::Utc::now().to_rfc3339(),
            command: "run".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({ "command": command }),
            targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(
            &report,
            out,
            "run",
            ctx.config.settings.default_output_format.as_deref(),
        )?;
    }

    Ok(())
}
