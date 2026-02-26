pub mod sh;
pub mod powershell;
pub mod cmd;

use crate::config::schema::ShellType;

/// Get the probe command for a given shell and metric.
pub fn command_for(shell: ShellType, metric: &str) -> String {
    match shell {
        ShellType::Sh => sh::command_for(metric),
        ShellType::PowerShell => powershell::command_for(metric),
        ShellType::Cmd => cmd::command_for(metric),
    }
}

/// Get the command to measure path size.
pub fn path_size_command(shell: ShellType, path: &str) -> String {
    match shell {
        ShellType::Sh => format!("du -sb {} 2>/dev/null", path),
        ShellType::PowerShell => {
            format!("(Get-ChildItem -Recurse -File '{}' | Measure-Object -Property Length -Sum).Sum", path)
        }
        ShellType::Cmd => format!("dir /s /a \"{}\" 2>nul", path),
    }
}
