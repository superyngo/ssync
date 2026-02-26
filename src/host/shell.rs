use anyhow::Result;

use crate::config::schema::ShellType;

/// Detect the shell type of a remote host by trying common commands.
pub async fn detect(host_ssh: &str, timeout_secs: u64) -> Result<ShellType> {
    use std::process::Stdio;
    use tokio::process::Command;

    // Try sh first (most common)
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs))
        .arg(host_ssh)
        .arg("--")
        .arg("uname -s 2>/dev/null || echo __NOT_SH__")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if out.status.success() && !stdout.contains("__NOT_SH__") {
            let trimmed = stdout.trim().to_lowercase();
            if trimmed.contains("linux")
                || trimmed.contains("darwin")
                || trimmed.contains("freebsd")
                || trimmed.contains("openbsd")
            {
                return Ok(ShellType::Sh);
            }
        }
    }

    // Try PowerShell
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs))
        .arg(host_ssh)
        .arg("--")
        .arg("powershell -Command \"echo POWERSHELL_OK\"")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains("POWERSHELL_OK") {
            return Ok(ShellType::PowerShell);
        }
    }

    // Fallback: try CMD
    let output = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs))
        .arg(host_ssh)
        .arg("--")
        .arg("ver")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.to_lowercase().contains("windows") {
            return Ok(ShellType::Cmd);
        }
    }

    // Default to sh
    Ok(ShellType::Sh)
}

/// Get the temporary directory path for a given shell type.
pub fn temp_dir(shell: ShellType) -> &'static str {
    match shell {
        ShellType::Sh => "/tmp",
        ShellType::PowerShell | ShellType::Cmd => "%TEMP%",
    }
}

/// Wrap a command for sudo execution based on shell type.
pub fn sudo_wrap(shell: ShellType, command: &str) -> String {
    match shell {
        ShellType::Sh => format!("sudo {}", command),
        ShellType::PowerShell => {
            format!("Start-Process powershell -ArgumentList '-Command {}' -Verb RunAs", command)
        }
        ShellType::Cmd => format!("runas /user:Administrator \"{}\"", command),
    }
}
