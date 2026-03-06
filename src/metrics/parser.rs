use crate::config::schema::ShellType;
use serde_json::Value;

/// Parse command output for a given metric and shell type into JSON.
pub fn parse(shell: ShellType, metric: &str, stdout: &str) -> Value {
    match (shell, metric) {
        (ShellType::Sh, "system_info") => parse_sh_system_info(stdout),
        (ShellType::Sh, "cpu_arch") => Value::String(stdout.trim().to_string()),
        (ShellType::Sh, "memory") => parse_sh_memory(stdout),
        (ShellType::Sh, "swap") => parse_sh_swap(stdout),
        (ShellType::Sh, "disk") => parse_sh_disk(stdout),
        (ShellType::Sh, "cpu_load") => parse_sh_cpu_load(stdout),
        (ShellType::Sh, "network") => parse_sh_network(stdout),
        (ShellType::Sh, "battery") => parse_sh_battery(stdout),
        (ShellType::PowerShell, "system_info") => parse_ps_system_info(stdout),
        (ShellType::PowerShell, "cpu_arch") => Value::String(stdout.trim().to_string()),
        (ShellType::PowerShell, "memory") => parse_ps_memory(stdout),
        _ => Value::String(stdout.trim().to_string()),
    }
}

/// Parse path size output.
pub fn parse_path_size(_shell: ShellType, stdout: &str) -> u64 {
    let trimmed = stdout.trim();
    // `du -sb` outputs "SIZE\tPATH"
    trimmed
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// --- sh parsers ---

fn parse_sh_system_info(stdout: &str) -> Value {
    let lines: Vec<&str> = stdout.lines().collect();
    let mut map = serde_json::Map::new();
    if let Some(uname) = lines.first() {
        map.insert("uname".to_string(), Value::String(uname.trim().to_string()));
        // Try to extract OS from uname -a
        let parts: Vec<&str> = uname.split_whitespace().collect();
        if parts.len() >= 2 {
            map.insert("hostname".to_string(), Value::String(parts[1].to_string()));
        }
        if parts.len() >= 3 {
            map.insert("kernel".to_string(), Value::String(parts[2].to_string()));
        }
    }
    Value::Object(map)
}

fn parse_sh_memory(stdout: &str) -> Value {
    let mut map = serde_json::Map::new();
    // Parse `free -b` output
    for line in stdout.lines() {
        if line.starts_with("Mem:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(total) = parts[1].parse::<u64>() {
                    map.insert("total_bytes".to_string(), Value::Number(total.into()));
                }
                if let Ok(used) = parts[2].parse::<u64>() {
                    map.insert("used_bytes".to_string(), Value::Number(used.into()));
                }
            }
        }
    }
    Value::Object(map)
}

fn parse_sh_swap(stdout: &str) -> Value {
    let mut map = serde_json::Map::new();
    for line in stdout.lines() {
        if line.starts_with("Swap:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(total) = parts[1].parse::<u64>() {
                    map.insert("total_bytes".to_string(), Value::Number(total.into()));
                }
                if let Ok(used) = parts[2].parse::<u64>() {
                    map.insert("used_bytes".to_string(), Value::Number(used.into()));
                }
            }
        }
    }
    Value::Object(map)
}

fn parse_sh_disk(stdout: &str) -> Value {
    let mut disks = Vec::new();
    // Parse `df -B1` output (skip header)
    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 6 {
            let mut entry = serde_json::Map::new();
            if let Ok(total) = parts[1].parse::<u64>() {
                entry.insert("total_bytes".to_string(), Value::Number(total.into()));
            }
            if let Ok(used) = parts[2].parse::<u64>() {
                entry.insert("used_bytes".to_string(), Value::Number(used.into()));
            }
            entry.insert(
                "mount".to_string(),
                Value::String(parts.last().unwrap_or(&"").to_string()),
            );
            disks.push(Value::Object(entry));
        }
    }
    Value::Array(disks)
}

fn parse_sh_cpu_load(stdout: &str) -> Value {
    let mut map = serde_json::Map::new();
    // /proc/loadavg: "0.52 0.38 0.21 1/234 5678"
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() >= 3 {
        if let Ok(v) = parts[0].parse::<f64>() {
            map.insert(
                "load1".to_string(),
                Value::Number(serde_json::Number::from_f64(v).unwrap_or(0.into())),
            );
        }
        if let Ok(v) = parts[1].parse::<f64>() {
            map.insert(
                "load5".to_string(),
                Value::Number(serde_json::Number::from_f64(v).unwrap_or(0.into())),
            );
        }
        if let Ok(v) = parts[2].parse::<f64>() {
            map.insert(
                "load15".to_string(),
                Value::Number(serde_json::Number::from_f64(v).unwrap_or(0.into())),
            );
        }
    }
    Value::Object(map)
}

fn parse_sh_network(stdout: &str) -> Value {
    // Simple: just store raw output as string for now
    Value::String(stdout.trim().to_string())
}

fn parse_sh_battery(stdout: &str) -> Value {
    let mut map = serde_json::Map::new();
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed.contains("No such file") {
        map.insert("present".to_string(), Value::Bool(false));
    } else {
        map.insert("present".to_string(), Value::Bool(true));
        // Try to extract percentage from various formats
        for line in stdout.lines() {
            if let Some(pct) = line.trim().strip_suffix('%') {
                if let Ok(v) = pct.trim().parse::<u64>() {
                    map.insert("percent".to_string(), Value::Number(v.into()));
                    break;
                }
            }
        }
    }
    Value::Object(map)
}

// --- PowerShell parsers ---

fn parse_ps_system_info(stdout: &str) -> Value {
    Value::String(stdout.trim().to_string())
}

fn parse_ps_memory(stdout: &str) -> Value {
    Value::String(stdout.trim().to_string())
}
