use crate::config::schema::ShellType;

/// Detect shell type using an established russh session pool.
pub async fn detect_russh(
    host: &crate::config::schema::HostEntry,
    sessions: &super::session_pool::RusshSessionPool,
    timeout: u64,
) -> anyhow::Result<crate::config::schema::ShellType> {
    use crate::config::schema::ShellType;
    // Try PowerShell first
    if let Ok(o) = sessions
        .exec(&host.ssh_host, "$PSVersionTable.PSVersion.Major", timeout)
        .await
    {
        if o.success && !o.stdout.trim().is_empty() {
            return Ok(ShellType::PowerShell);
        }
    }
    // Check for Windows CMD via 'ver'
    if let Ok(o) = sessions.exec(&host.ssh_host, "ver", timeout).await {
        if o.success && o.stdout.contains("Windows") {
            return Ok(ShellType::Cmd);
        }
    }
    // Default: POSIX shell
    Ok(ShellType::Sh)
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
