use std::collections::HashMap;

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

/// Parse batched metric output (split by `---METRIC:name` markers).
/// Returns a map of metric_name → parsed Value.
pub fn parse_batch(shell: ShellType, metrics: &[String], stdout: &str) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    let blocks = split_by_marker(stdout, "---METRIC:");
    for (name, content) in &blocks {
        if metrics.contains(&name.to_string()) {
            results.insert(name.to_string(), parse(shell, name, content));
        }
    }
    results
}

/// Parse batched path size output (split by `---PATH:label` markers).
/// Returns a map of label → Option<size_bytes> (None if MISSING).
pub fn parse_batch_paths(
    shell: ShellType,
    paths: &[(String, String)],
    stdout: &str,
) -> HashMap<String, Option<u64>> {
    let mut results = HashMap::new();
    let blocks = split_by_marker(stdout, "---PATH:");
    let label_map: HashMap<&str, &str> = paths
        .iter()
        .map(|(p, l)| (l.as_str(), p.as_str()))
        .collect();
    for (label, content) in &blocks {
        if label_map.contains_key(label.as_str()) {
            let trimmed = content.trim();
            if trimmed == "MISSING" || trimmed.is_empty() {
                results.insert(label.to_string(), None);
            } else {
                results.insert(label.to_string(), Some(parse_path_size(shell, content)));
            }
        }
    }
    results
}

/// Split output by `PREFIX<name>` markers into (name, content) pairs.
fn split_by_marker(output: &str, prefix: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            // Save previous block
            if let Some(name) = current_name.take() {
                results.push((name, current_lines.join("\n")));
            }
            current_name = Some(rest.trim().to_string());
            current_lines.clear();
        } else if current_name.is_some() {
            current_lines.push(line);
        }
    }
    // Save last block
    if let Some(name) = current_name {
        results.push((name, current_lines.join("\n")));
    }
    results
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_batch_splits_metrics() {
        let output = "---METRIC:online\nok\n---METRIC:cpu_arch\nx86_64\n";
        let metrics = vec!["online".into(), "cpu_arch".into()];
        let result = parse_batch(ShellType::Sh, &metrics, output);
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("online"));
        assert!(result.contains_key("cpu_arch"));
        assert_eq!(result["cpu_arch"], Value::String("x86_64".into()));
    }

    #[test]
    fn test_parse_batch_empty_output() {
        let result = parse_batch(ShellType::Sh, &["online".into()], "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_batch_ignores_unknown_metrics() {
        let output = "---METRIC:online\nok\n---METRIC:unknown_thing\nfoo\n";
        let metrics = vec!["online".into()];
        let result = parse_batch(ShellType::Sh, &metrics, output);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("online"));
    }

    #[test]
    fn test_parse_batch_paths_ok() {
        let output = "---PATH:home\n12345\t/home\n---PATH:logs\nMISSING\n";
        let paths = vec![("~/".into(), "home".into()), ("/var".into(), "logs".into())];
        let result = parse_batch_paths(ShellType::Sh, &paths, output);
        assert_eq!(result.len(), 2);
        assert_eq!(*result.get("home").unwrap(), Some(12345u64));
        assert_eq!(*result.get("logs").unwrap(), None);
    }

    #[test]
    fn test_parse_batch_paths_empty() {
        let result = parse_batch_paths(ShellType::Sh, &[], "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_split_by_marker() {
        let output = "---METRIC:a\nline1\nline2\n---METRIC:b\nline3\n";
        let result = split_by_marker(output, "---METRIC:");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "a");
        assert!(result[0].1.contains("line1"));
        assert_eq!(result[1].0, "b");
        assert!(result[1].1.contains("line3"));
    }
}
