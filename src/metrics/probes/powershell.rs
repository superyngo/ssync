/// Return the PowerShell probe command for a given metric.
pub fn command_for(metric: &str) -> String {
    match metric {
        "online" => "echo ok".to_string(),
        "system_info" => "Get-ComputerInfo | Select-Object OsName, CsName, OsVersion | ConvertTo-Json".to_string(),
        "cpu_arch" => "$env:PROCESSOR_ARCHITECTURE".to_string(),
        "memory" => {
            "Get-CimInstance Win32_OperatingSystem | Select-Object TotalVisibleMemorySize, FreePhysicalMemory | ConvertTo-Json".to_string()
        }
        "swap" => {
            "Get-CimInstance Win32_PageFileUsage | Select-Object AllocatedBaseSize, CurrentUsage | ConvertTo-Json".to_string()
        }
        "disk" => "Get-PSDrive -PSProvider FileSystem | Select-Object Name, Used, Free | ConvertTo-Json".to_string(),
        "cpu_load" => {
            "(Get-Counter '\\Processor(_Total)\\% Processor Time').CounterSamples[0].CookedValue".to_string()
        }
        "network" => "Get-NetIPAddress | Select-Object InterfaceAlias, IPAddress | ConvertTo-Json".to_string(),
        "battery" => "Get-WmiObject Win32_Battery | Select-Object EstimatedChargeRemaining, BatteryStatus | ConvertTo-Json".to_string(),
        "ip_address" => {
            "(Get-NetIPAddress -AddressFamily IPv4 | Where-Object {$_.IPAddress -ne '127.0.0.1'} | Select-Object -ExpandProperty IPAddress) -join ' '".to_string()
        }
        _ => String::new(),
    }
}

/// Build a single PowerShell command that collects all metrics with `---METRIC:` markers.
pub fn batch_command(metrics: &[String]) -> String {
    if metrics.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for metric in metrics {
        let cmd = command_for(metric);
        if cmd.is_empty() {
            continue;
        }
        parts.push(format!("\"---METRIC:{}\"; {}", metric, cmd));
    }
    parts.join("; ")
}

/// Build a single PowerShell command that measures all path sizes with `---PATH:` markers.
pub fn batch_path_command(paths: &[(String, String)]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for (path, label) in paths {
        parts.push(format!(
            "\"---PATH:{}\"; try {{ (Get-ChildItem -Recurse -File '{}' | Measure-Object -Property Length -Sum).Sum }} catch {{ \"MISSING\" }}",
            label, path
        ));
    }
    parts.join("; ")
}
