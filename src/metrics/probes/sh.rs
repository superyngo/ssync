/// Return the sh/bash probe command for a given metric.
pub fn command_for(metric: &str) -> String {
    match metric {
        "online" => "echo ok".to_string(),
        "system_info" => "uname -a && hostname".to_string(),
        "cpu_arch" => "uname -m".to_string(),
        "memory" => "free -b 2>/dev/null || vm_stat 2>/dev/null".to_string(),
        "swap" => "free -b 2>/dev/null".to_string(),
        "disk" => "df -B1 2>/dev/null || df -k".to_string(),
        "cpu_load" => {
            "cat /proc/loadavg 2>/dev/null || sysctl -n vm.loadavg 2>/dev/null".to_string()
        }
        "network" => "ip -j addr 2>/dev/null || ifconfig 2>/dev/null".to_string(),
        "battery" => "cat /sys/class/power_supply/BAT0/capacity 2>/dev/null || \
             pmset -g batt 2>/dev/null || echo"
            .to_string(),
        "ip_address" => {
            "hostname -I 2>/dev/null || \
             (ifconfig 2>/dev/null | grep 'inet ' | awk '{print $2}' | grep -v '127.0.0.1' | tr '\\n' ' ')"
                .to_string()
        }
        _ => String::new(),
    }
}

/// Build a single sh command that collects all metrics with `---METRIC:` markers.
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
        parts.push(format!("echo '---METRIC:{}'; {{ {}; }} 2>&1", metric, cmd));
    }
    parts.join("; ")
}

/// Build a single sh command that measures all path sizes with `---PATH:` markers.
pub fn batch_path_command(paths: &[(String, String)]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for (path, label) in paths {
        parts.push(format!(
            "echo '---PATH:{}'; du -sb {} 2>/dev/null || echo MISSING",
            label, path
        ));
    }
    parts.join("; ")
}
