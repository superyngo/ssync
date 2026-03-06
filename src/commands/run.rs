use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::Semaphore;

use crate::host::executor;
use crate::host::shell;
use crate::output::printer;
use crate::output::summary::Summary;

use super::Context;

pub async fn run(ctx: &Context, command: &str, sudo: bool, _yes: bool) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;
    let semaphore = Arc::new(Semaphore::new(ctx.concurrency()));
    let mut summary = Summary::default();

    let mut handles = Vec::new();
    for host in &hosts {
        let sem = semaphore.clone();
        let host = (*host).clone();
        let cmd = if sudo {
            shell::sudo_wrap(host.shell, command)
        } else {
            command.to_string()
        };
        let timeout = ctx.timeout;

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let start = Instant::now();
            let result = executor::run_remote(&host, &cmd, timeout).await;
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
                    // Print stdout with host prefix
                    for line in output.stdout.lines() {
                        printer::print_host_line(&host.name, "ok", line);
                    }
                    summary.add_success();
                } else {
                    let msg = output.stderr.trim().to_string();
                    printer::print_host_line(&host.name, "error", &msg);
                    summary.add_failure(&host.name, &msg);
                }

                // Log operation
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

    summary.print();
    Ok(())
}
