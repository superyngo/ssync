use crate::config::schema::ShellType;

/// Detect shell type using an established russh session pool.
pub async fn detect_russh(
    host: &crate::config::schema::HostEntry,
    sessions: &super::session_pool::RusshSessionPool,
    timeout: u64,
) -> anyhow::Result<crate::config::schema::ShellType> {
    use crate::config::schema::ShellType;

    let mut any_exec_ok = false;

    // Try PowerShell
    if let Ok(o) = sessions
        .exec(&host.ssh_host, "$PSVersionTable.PSVersion.Major", timeout)
        .await
    {
        any_exec_ok = true;
        if o.success && !o.stdout.trim().is_empty() {
            return Ok(ShellType::PowerShell);
        }
    }

    // Try CMD (Windows)
    if let Ok(o) = sessions.exec(&host.ssh_host, "ver", timeout).await {
        any_exec_ok = true;
        if o.success && o.stdout.contains("Windows") {
            return Ok(ShellType::Cmd);
        }
    }

    // Confirm POSIX shell is reachable before defaulting
    if let Ok(o) = sessions.exec(&host.ssh_host, "echo ok", timeout).await {
        any_exec_ok = true;
        if o.success {
            return Ok(ShellType::Sh);
        }
    }

    if !any_exec_ok {
        anyhow::bail!(
            "shell detection failed for {}: all exec attempts returned errors (session may be dropped)",
            host.ssh_host
        );
    }

    // exec succeeded but none of the markers matched — default to Sh
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
