use std::time::Instant;

use anyhow::Result;

use crate::host::pool::SshPool;
use crate::host::shell;
use crate::output::printer;
use crate::output::summary::Summary;

use super::Context;

pub async fn run(ctx: &Context, command: &str, sudo: bool, _yes: bool) -> Result<()> {
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

    // Report unreachable hosts
    for (name, err) in pool.failed_hosts() {
        printer::print_host_line(&name, "error", &format!("unreachable — {}", err));
        summary.add_failure(&name, &err);
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
        let now = chrono::Utc::now().timestamp();

        match result {
            Ok(output) => {
                if output.success {
                    for line in output.stdout.lines() {
                        printer::print_host_line(&host.name, "ok", line);
                    }
                    summary.add_success();
                } else {
                    let msg = output.stderr.trim().to_string();
                    printer::print_host_line(&host.name, "error", &msg);
                    summary.add_failure(&host.name, &msg);
                }

                ctx.db.execute(
                    "INSERT INTO operation_log (timestamp, command, host, action, status, duration_ms, note) \
                     VALUES (?1, 'run', ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        now,
                        host.name,
                        command,
                        if output.success { "ok" } else { "error" },
                        elapsed.as_millis() as i64,
                        if output.success { None::<String> } else { Some(output.stderr.trim().to_string()) },
                    ],
                )?;
            }
            Err(e) => {
                printer::print_host_line(&host.name, "error", &e.to_string());
                summary.add_failure(&host.name, &e.to_string());

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
    Ok(())
}
