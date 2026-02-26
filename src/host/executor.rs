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
