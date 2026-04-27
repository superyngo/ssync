use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::config::schema::ShellType;
use crate::host::pool::SshPool;
use crate::host::{executor, shell};
use crate::output::printer;
use crate::output::summary::Summary;

use super::Context;

pub async fn run(
    ctx: &Context,
    script: &str,
    sudo: bool,
    _yes: bool,
    keep: bool,
    dry_run: bool,
) -> Result<()> {
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

    if dry_run {
        println!("[dry-run] Script: {}", script);
        println!("[dry-run] Compatible shell: {:?}", compatible_shell);
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

    // Set up SSH connection pool
    let (pool, _connected) = SshPool::setup(
        &hosts,
        ctx.timeout,
        ctx.concurrency(),
        ctx.per_host_concurrency(),
    )
    .await?;

    let mut summary = Summary::default();

    // Report unreachable hosts
    for (name, err) in pool.failed_hosts() {
        printer::print_host_line(&name, "error", &format!("unreachable — {}", err));
        summary.add_failure(&name, &err);
    }

    let reachable = pool.filter_reachable(&hosts);

    let mut handles = Vec::new();
    for host in &reachable {
        // Check shell compatibility
        if let Some(required) = compatible_shell {
            if required != host.shell {
                printer::print_host_line(
                    &host.name,
                    "skip",
                    &format!(
                        "skipped (shell mismatch: need {}, have {})",
                        required, host.shell
                    ),
                );
                summary.add_skip();
                continue;
            }
        }

        let host = (*host).clone();
        let script_path = script_path.to_path_buf();
        let timeout = ctx.timeout;
        let sessions = pool.session_pool.clone();
        let socket = pool.socket_for(&host.name).map(|p| p.to_path_buf());
        let global_sem = pool.limiter.global_semaphore();

        handles.push(tokio::spawn(async move {
            let _permit = global_sem.acquire_owned().await.unwrap();
            let start = Instant::now();
            let result =
                exec_on_host_pooled(&host, &script_path, timeout, keep, sudo, socket.as_deref(), sessions)
                    .await;
            let elapsed = start.elapsed();
            (host, result, elapsed)
        }));
    }

    for handle in handles {
        let (host, result, elapsed) = handle.await?;
        let now = chrono::Utc::now().timestamp();

        match result {
            Ok(output) => {
                for line in output.lines() {
                    printer::print_host_line(&host.name, "ok", line);
                }
                summary.add_success();

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms) \
                     VALUES (?1, 'exec', ?2, ?3, 'ok', ?4)",
                    rusqlite::params![now, host.name, script, elapsed.as_millis() as i64],
                )?;
            }
            Err(e) => {
                printer::print_host_line(&host.name, "error", &e.to_string());
                summary.add_failure(&host.name, &e.to_string());

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'exec', ?2, ?3, 'error', ?4, ?5)",
                    rusqlite::params![now, host.name, script, elapsed.as_millis() as i64, e.to_string()],
                )?;
            }
        }
    }

    pool.shutdown().await;
    summary.print();
    Ok(())
}

async fn exec_on_host_pooled(
    host: &crate::config::schema::HostEntry,
    script_path: &Path,
    timeout: u64,
    keep: bool,
    sudo: bool,
    socket: Option<&Path>,
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
        match executor::upload_pooled(host, script_path, &primary, timeout, socket).await {
            Ok(()) => primary,
            Err(_) => {
                let fallback = format!("~/{}", suffix);
                executor::upload_pooled(host, script_path, &fallback, timeout, socket).await?;
                fallback
            }
        }
    } else {
        let path = format!("{}/{}", temp_dir, suffix);
        executor::upload_pooled(host, script_path, &path, timeout, socket).await?;
        path
    };

    let remote_path_quoted = if host.shell == ShellType::PowerShell {
        format!("'{}'", remote_path)
    } else {
        remote_path.clone()
    };

    // Make executable (sh only)
    if host.shell == ShellType::Sh {
        sessions.exec(&host.ssh_host, &format!("chmod +x {}", remote_path), timeout).await?;
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
