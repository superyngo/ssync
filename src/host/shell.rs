use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::schema::ShellType;
use crate::host::executor;

/// Detect the shell type of a remote host by trying common commands.
#[allow(dead_code)]
pub async fn detect(host_ssh: &str, timeout_secs: u64) -> Result<ShellType> {
    use std::process::Stdio;
    use tokio::process::Command;

    let timeout = Duration::from_secs(timeout_secs);

    // Track whether SSH could connect at all
    let mut ssh_connected = false;
    let mut last_error = String::new();

    // Try sh first (most common)
    let output = tokio::time::timeout(
        timeout,
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(host_ssh)
            .arg("--")
            .arg("uname -s 2>/dev/null || echo __NOT_SH__")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("shell detection timeout")?;

    if let Ok(out) = output {
        if out.status.success() {
            ssh_connected = true;
            let stdout = String::from_utf8_lossy(&out.stdout);
            if !stdout.contains("__NOT_SH__") {
                let trimmed = stdout.trim().to_lowercase();
                if trimmed.contains("linux")
                    || trimmed.contains("darwin")
                    || trimmed.contains("freebsd")
                    || trimmed.contains("openbsd")
                {
                    return Ok(ShellType::Sh);
                }
            }
        } else {
            last_error = String::from_utf8_lossy(&out.stderr).trim().to_string();
        }
    }

    // Try PowerShell
    let output = tokio::time::timeout(
        timeout,
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(host_ssh)
            .arg("--")
            .arg("powershell -Command \"echo POWERSHELL_OK\"")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("shell detection timeout")?;

    if let Ok(out) = output {
        if out.status.success() {
            ssh_connected = true;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains("POWERSHELL_OK") {
            return Ok(ShellType::PowerShell);
        }
        if !ssh_connected {
            last_error = String::from_utf8_lossy(&out.stderr).trim().to_string();
        }
    }

    // Fallback: try CMD
    let output = tokio::time::timeout(
        timeout,
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(host_ssh)
            .arg("--")
            .arg("ver")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("shell detection timeout")?;

    if let Ok(out) = output {
        if out.status.success() {
            ssh_connected = true;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.to_lowercase().contains("windows") {
            return Ok(ShellType::Cmd);
        }
        if !ssh_connected {
            last_error = String::from_utf8_lossy(&out.stderr).trim().to_string();
        }
    }

    // Only default to sh if SSH actually connected (host is reachable)
    if ssh_connected {
        return Ok(ShellType::Sh);
    }

    // Host unreachable — return error so init skips it
    anyhow::bail!(
        "host unreachable: {}",
        if last_error.is_empty() {
            "SSH connection failed"
        } else {
            &last_error
        }
    )
}

/// Detect the shell type of a remote host using a ControlMaster socket.
/// Same logic as `detect()` but uses `executor::run_remote_pooled()`.
pub async fn detect_pooled(
    host_ssh: &str,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<ShellType> {
    use crate::config::schema::HostEntry;

    // Create a minimal HostEntry for the executor calls
    let host = HostEntry {
        name: host_ssh.to_string(),
        ssh_host: host_ssh.to_string(),
        shell: ShellType::Sh, // doesn't matter for detection
        groups: Vec::new(),
    };

    let mut ssh_connected = false;
    let mut last_error = String::new();

    // Try sh first
    match executor::run_remote_pooled(
        &host,
        "uname -s 2>/dev/null || echo __NOT_SH__",
        timeout_secs,
        socket,
    )
    .await
    {
        Ok(output) if output.success => {
            ssh_connected = true;
            if !output.stdout.contains("__NOT_SH__") {
                let trimmed = output.stdout.trim().to_lowercase();
                if trimmed.contains("linux")
                    || trimmed.contains("darwin")
                    || trimmed.contains("freebsd")
                    || trimmed.contains("openbsd")
                {
                    return Ok(ShellType::Sh);
                }
            }
        }
        Ok(output) => {
            last_error = output.stderr.trim().to_string();
        }
        Err(e) => {
            last_error = e.to_string();
        }
    }

    // Try PowerShell
    match executor::run_remote_pooled(
        &host,
        "powershell -Command \"echo POWERSHELL_OK\"",
        timeout_secs,
        socket,
    )
    .await
    {
        Ok(output) => {
            if output.success {
                ssh_connected = true;
            }
            if output.stdout.contains("POWERSHELL_OK") {
                return Ok(ShellType::PowerShell);
            }
            if !ssh_connected {
                last_error = output.stderr.trim().to_string();
            }
        }
        Err(e) => {
            if !ssh_connected {
                last_error = e.to_string();
            }
        }
    }

    // Try CMD
    match executor::run_remote_pooled(&host, "ver", timeout_secs, socket).await {
        Ok(output) => {
            if output.success {
                ssh_connected = true;
            }
            if output.stdout.to_lowercase().contains("windows") {
                return Ok(ShellType::Cmd);
            }
            if !ssh_connected {
                last_error = output.stderr.trim().to_string();
            }
        }
        Err(e) => {
            if !ssh_connected {
                last_error = e.to_string();
            }
        }
    }

    if ssh_connected {
        return Ok(ShellType::Sh);
    }

    anyhow::bail!(
        "host unreachable: {}",
        if last_error.is_empty() {
            "SSH connection failed"
        } else {
            &last_error
        }
    )
}

/// Get the temporary directory path for a given shell type.
pub fn temp_dir(shell: ShellType) -> &'static str {
    match shell {
        ShellType::Sh => "/tmp",
        ShellType::PowerShell => "$env:TEMP",
        ShellType::Cmd => "%TEMP%",
    }
}

/// Wrap a command for sudo execution based on shell type.
pub fn sudo_wrap(shell: ShellType, command: &str) -> String {
    match shell {
        ShellType::Sh => format!("sudo {}", command),
        ShellType::PowerShell => {
            format!(
                "Start-Process powershell -ArgumentList '-Command {}' -Verb RunAs",
                command
            )
        }
        ShellType::Cmd => format!("runas /user:Administrator \"{}\"", command),
    }
}
