use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::config::schema::HostEntry;

/// Result of a remote command execution.
#[derive(Debug)]
pub struct RemoteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Execute a command on a remote host via SSH.
pub async fn run_remote(
    host: &HostEntry,
    command: &str,
    timeout_secs: u64,
) -> Result<RemoteOutput> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(&host.ssh_host)
            .arg("--")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SSH connection timeout")?
    .context("Failed to execute ssh")?;

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        success: output.status.success(),
    })
}

/// Upload a file to a remote host via scp.
pub async fn upload(
    host: &HostEntry,
    local_path: &Path,
    remote_path: &str,
    timeout_secs: u64,
) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("scp")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(local_path.as_os_str())
            .arg(format!("{}:{}", host.ssh_host, remote_path))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SCP timeout")?
    .context("Failed to execute scp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("scp upload failed: {}", stderr.trim());
    }
    Ok(())
}

/// Download a file from a remote host via scp.
pub async fn download(
    host: &HostEntry,
    remote_path: &str,
    local_path: &Path,
    timeout_secs: u64,
) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("scp")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", timeout_secs))
            .arg(format!("{}:{}", host.ssh_host, remote_path))
            .arg(local_path.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SCP timeout")?
    .context("Failed to execute scp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("scp download failed: {}", stderr.trim());
    }
    Ok(())
}

/// Build common SSH args including optional ControlPath.
fn ssh_base_args(host: &HostEntry, timeout_secs: u64, socket: Option<&Path>) -> Vec<String> {
    let _ = host; // host reserved for future per-host arg customisation
    let mut args = vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={}", timeout_secs),
    ];
    if let Some(sock) = socket {
        args.push("-o".to_string());
        args.push(format!("ControlPath={}", sock.display()));
    }
    args
}

/// Execute a command on a remote host, optionally reusing a ControlMaster socket.
pub async fn run_remote_pooled(
    host: &HostEntry,
    command: &str,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<RemoteOutput> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("ssh")
            .args(&args)
            .arg(&host.ssh_host)
            .arg("--")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SSH connection timeout")?
    .context("Failed to execute ssh")?;

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        success: output.status.success(),
    })
}

/// Upload a file via scp, optionally reusing a ControlMaster socket.
pub async fn upload_pooled(
    host: &HostEntry,
    local_path: &Path,
    remote_path: &str,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<()> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("scp")
            .args(&args)
            .arg(local_path.as_os_str())
            .arg(format!("{}:{}", host.ssh_host, remote_path))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SCP timeout")?
    .context("Failed to execute scp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("scp upload failed: {}", stderr.trim());
    }
    Ok(())
}

/// Probe whether scp works to a remote host by uploading a 1-byte temp file.
/// Uses shell-appropriate remote paths and cleanup commands.
pub async fn scp_probe(host: &HostEntry, timeout_secs: u64, socket: Option<&Path>) -> Result<()> {
    use crate::config::schema::ShellType;

    let temp_dir = tempfile::tempdir().context("Failed to create temp dir for scp probe")?;
    let local_probe = temp_dir.path().join("ssync_probe");
    std::fs::write(&local_probe, b"0").context("Failed to write probe file")?;

    // Tilde (~) is expanded by SCP/SFTP natively; env vars ($env:TEMP, %TEMP%) are NOT.
    let probe_paths: Vec<&str> = match host.shell {
        ShellType::Sh => vec!["/tmp/.ssync_probe", "~/.ssync_probe"],
        ShellType::PowerShell | ShellType::Cmd => vec!["~/.ssync_probe"],
    };

    let mut last_err = None;
    for remote_path in &probe_paths {
        match upload_pooled(host, &local_probe, remote_path, timeout_secs, socket).await {
            Ok(()) => {
                // Shell-aware cleanup (best-effort)
                let rm_cmd = match host.shell {
                    ShellType::Sh => {
                        if remote_path.starts_with("~/") {
                            format!(
                                "rm -f \"$HOME/{}\" 2>/dev/null; exit 0",
                                &remote_path[2..]
                            )
                        } else {
                            format!("rm -f '{}' 2>/dev/null; exit 0", remote_path)
                        }
                    }
                    ShellType::PowerShell => {
                        format!(
                            "Remove-Item -Force '{}' -ErrorAction SilentlyContinue",
                            remote_path
                        )
                    }
                    ShellType::Cmd => {
                        format!("del /f /q \"{}\" 2>nul", remote_path)
                    }
                };
                let _ = run_remote_pooled(host, &rm_cmd, timeout_secs, socket).await;
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("scp probe failed")))
}

/// Download a file via scp, optionally reusing a ControlMaster socket.
pub async fn download_pooled(
    host: &HostEntry,
    remote_path: &str,
    local_path: &Path,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<()> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("scp")
            .args(&args)
            .arg(format!("{}:{}", host.ssh_host, remote_path))
            .arg(local_path.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("SCP timeout")?
    .context("Failed to execute scp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("scp download failed: {}", stderr.trim());
    }
    Ok(())
}
