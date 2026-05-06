use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::config::schema::ShellType;
use crate::host::pool::SshPool;
use crate::host::shell;
use crate::output::printer;
use crate::output::report::{FilterInfo, HostResult, OperationReport, ReportSummary};
use crate::output::summary::Summary;

use super::report::{CommandReport, ExecHostResult, ExecReport, HostStatus, ProgressSink};
use super::Context;

/// Pure command core: uploads and executes a script on each host, writes to
/// the operation log, and returns a typed `ExecReport`.
///
/// `dry_run` is handled by the CLI wrapper before calling this.
pub async fn exec_core(
    ctx: &Context,
    script: &str,
    sudo: bool,
    _yes: bool,
    keep: bool,
    progress: Option<&dyn ProgressSink>,
) -> Result<CommandReport> {
    let script_path = Path::new(script);
    if !script_path.exists() {
        bail!("Script not found: {}", script);
    }

    let extension = script_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let compatible_shell = match extension.as_str() {
        "sh" => Some(ShellType::Sh),
        "ps1" => Some(ShellType::PowerShell),
        "bat" | "cmd" => Some(ShellType::Cmd),
        _ => None,
    };

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

    let mut host_results: Vec<ExecHostResult> = Vec::new();

    // Report unreachable hosts.
    for (name, err) in pool.failed_hosts() {
        let detail = format!("unreachable — {}", err);
        if let Some(p) = progress {
            p.host_completed(&name, HostStatus::Unreachable, &detail, 0);
        }
        host_results.push(ExecHostResult {
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
        // Check shell compatibility — skipped hosts are reported immediately.
        if let Some(required) = compatible_shell {
            if required != host.shell {
                let detail = format!(
                    "skipped (shell mismatch: need {:?}, have {:?})",
                    required, host.shell
                );
                if let Some(p) = progress {
                    p.host_completed(&host.name, HostStatus::Skipped, &detail, 0);
                }
                host_results.push(ExecHostResult {
                    host: host.name.clone(),
                    status: HostStatus::Skipped,
                    duration_ms: None,
                    detail,
                    stdout: String::new(),
                    stderr: String::new(),
                });
                continue;
            }
        }

        let host = (*host).clone();
        let script_path = script_path.to_path_buf();
        let timeout = ctx.timeout;
        let sessions = pool.session_pool.clone();
        let global_sem = pool.limiter.global_semaphore();
        if let Some(p) = progress {
            p.host_started(&host.name);
        }

        handles.push(tokio::spawn(async move {
            let _permit = global_sem.acquire_owned().await.unwrap();
            let start = Instant::now();
            let result =
                exec_on_host_pooled(&host, &script_path, timeout, keep, sudo, sessions).await;
            let elapsed = start.elapsed();
            (host, result, elapsed)
        }));
    }

    for handle in handles {
        let (host, result, elapsed) = handle.await?;
        let ms = elapsed.as_millis() as u64;
        let now = chrono::Utc::now().timestamp();

        match result {
            Ok(exec_stdout) => {
                let first_line = exec_stdout.lines().next().unwrap_or("").to_string();
                let detail = if first_line.is_empty() {
                    format!("ok ({:.1}s)", elapsed.as_secs_f64())
                } else {
                    format!("ok ({:.1}s) — {}", elapsed.as_secs_f64(), first_line)
                };
                if let Some(p) = progress {
                    p.host_completed(&host.name, HostStatus::Online, &detail, ms);
                }
                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms) \
                     VALUES (?1, 'exec', ?2, ?3, 'ok', ?4)",
                    rusqlite::params![now, host.name, script, elapsed.as_millis() as i64],
                )?;
                host_results.push(ExecHostResult {
                    host: host.name.clone(),
                    status: HostStatus::Online,
                    duration_ms: Some(ms),
                    detail,
                    stdout: exec_stdout,
                    stderr: String::new(),
                });
            }
            Err(e) => {
                let detail = format!("error ({:.1}s) — {}", elapsed.as_secs_f64(), e);
                if let Some(p) = progress {
                    p.host_completed(&host.name, HostStatus::Error, &detail, ms);
                }
                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'exec', ?2, ?3, 'error', ?4, ?5)",
                    rusqlite::params![now, host.name, script, elapsed.as_millis() as i64, e.to_string()],
                )?;
                host_results.push(ExecHostResult {
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

    Ok(CommandReport::Exec(ExecReport {
        executed_at,
        script: script.to_string(),
        targets,
        hosts: host_results,
    }))
}

/// Thin CLI wrapper: handles dry-run, calls `exec_core`, prints summary,
/// writes `--out` reports.
pub async fn run(
    ctx: &Context,
    script: &str,
    sudo: bool,
    yes: bool,
    keep: bool,
    dry_run: bool,
    output: &crate::cli::OutputArgs,
) -> Result<()> {
    let script_path = Path::new(script);

    if dry_run {
        if !script_path.exists() {
            bail!("Script not found: {}", script);
        }
        let extension = script_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let compatible_shell: Option<ShellType> = match extension.as_str() {
            "sh" => Some(ShellType::Sh),
            "ps1" => Some(ShellType::PowerShell),
            "bat" | "cmd" => Some(ShellType::Cmd),
            _ => None,
        };
        println!("[dry-run] Script: {}", script);
        println!("[dry-run] Compatible shell: {:?}", compatible_shell);
        let hosts = ctx.resolve_hosts()?;
        for host in &hosts {
            let compat = compatible_shell.is_none_or(|s| s == host.shell);
            if compat {
                printer::print_host_line(&host.name, "ok", "would execute");
            } else {
                printer::print_host_line(&host.name, "skip", "shell mismatch");
            }
        }
        return Ok(());
    }

    let sink = PrinterSink;
    let CommandReport::Exec(report) = exec_core(ctx, script, sudo, yes, keep, Some(&sink)).await?
    else {
        unreachable!("exec_core always returns CommandReport::Exec")
    };

    let mut summary = Summary::default();
    let mut report_results: Vec<HostResult> = Vec::new();
    for h in &report.hosts {
        match h.status {
            HostStatus::Online => summary.add_success(),
            HostStatus::Skipped => summary.add_skip(),
            HostStatus::Unreachable | HostStatus::Error => {
                summary.add_failure(&h.host, &h.detail);
            }
            _ => {}
        }
        let status_str = match h.status {
            HostStatus::Online => "success",
            HostStatus::Skipped => "skipped",
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
            skipped: report_results
                .iter()
                .filter(|r| r.status == "skipped")
                .count(),
        };
        let targets = report.targets.clone();
        let rep = OperationReport {
            executed_at: report.executed_at,
            command: "exec".to_string(),
            filter: FilterInfo::from_mode(&ctx.mode),
            task: serde_json::json!({ "script": script }),
            targets,
            results: report_results,
            summary: rep_summary,
        };
        crate::output::report::write_report(
            &rep,
            out,
            "exec",
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
            HostStatus::Skipped => "skip",
            _ => "error",
        };
        printer::print_host_line(host, kind, detail);
    }
}

async fn exec_on_host_pooled(
    host: &crate::config::schema::HostEntry,
    script_path: &Path,
    timeout: u64,
    keep: bool,
    sudo: bool,
    sessions: std::sync::Arc<crate::host::session_pool::RusshSessionPool>,
) -> Result<String> {
    let temp_dir = get_expanded_temp_dir_pooled(host, timeout, sessions.clone()).await?;
    let script_name = script_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ssync_script");

    let suffix = format!("ssync_{}_{}", std::process::id(), script_name);

    // Upload — for Sh shells, try /tmp first then fall back to ~/ (like scp_probe)
    let remote_path = if host.shell == ShellType::Sh {
        let primary = format!("{}/{}", temp_dir, suffix);
        match sessions.upload(host, script_path, &primary, timeout).await {
            Ok(()) => primary,
            Err(_) => {
                let fallback = format!("~/{}", suffix);
                sessions
                    .upload(host, script_path, &fallback, timeout)
                    .await?;
                fallback
            }
        }
    } else {
        let path = format!("{}/{}", temp_dir, suffix);
        sessions.upload(host, script_path, &path, timeout).await?;
        path
    };

    let remote_path_quoted = if host.shell == ShellType::PowerShell {
        format!("'{}'", remote_path)
    } else {
        remote_path.clone()
    };

    // Make executable (sh only)
    if host.shell == ShellType::Sh {
        sessions
            .exec(
                &host.ssh_host,
                &format!("chmod +x {}", remote_path),
                timeout,
            )
            .await?;
    }

    // Execute
    let exec_cmd = match host.shell {
        ShellType::Sh => remote_path.clone(),
        ShellType::PowerShell => format!("powershell -File {}", remote_path_quoted),
        ShellType::Cmd => remote_path.clone(),
    };

    let exec_cmd = if sudo {
        shell::sudo_wrap(host.shell, &exec_cmd)
    } else {
        exec_cmd
    };

    let output = sessions.exec(&host.ssh_host, &exec_cmd, timeout).await?;

    // Cleanup (unless --keep)
    if !keep {
        let rm_cmd = match host.shell {
            ShellType::Sh => format!("rm -f {}", remote_path),
            ShellType::PowerShell => format!("Remove-Item -Force {}", remote_path_quoted),
            ShellType::Cmd => format!("del /f \"{}\"", remote_path),
        };
        let _ = sessions.exec(&host.ssh_host, &rm_cmd, timeout).await;
    }

    if output.success {
        Ok(output.stdout)
    } else {
        bail!(
            "Script failed (exit {}): {}",
            output.exit_code.unwrap_or(-1),
            output.stderr.trim()
        );
    }
}

async fn get_expanded_temp_dir_pooled(
    host: &crate::config::schema::HostEntry,
    timeout: u64,
    sessions: std::sync::Arc<crate::host::session_pool::RusshSessionPool>,
) -> Result<String> {
    let temp_dir = shell::temp_dir(host.shell);

    // For sh, /tmp is already a literal path
    if host.shell == ShellType::Sh {
        return Ok(temp_dir.to_string());
    }

    // For PowerShell and Cmd, need to expand the variable
    let echo_cmd = match host.shell {
        ShellType::PowerShell => "echo $env:TEMP".to_string(),
        ShellType::Cmd => "echo %TEMP%".to_string(),
        ShellType::Sh => unreachable!(),
    };

    let output = sessions.exec(&host.ssh_host, &echo_cmd, timeout).await?;

    if !output.success {
        bail!("Failed to get temp directory: {}", output.stderr.trim());
    }

    Ok(output.stdout.trim().to_string())
}
