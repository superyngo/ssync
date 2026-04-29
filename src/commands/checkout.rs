use anyhow::{bail, Result};
use rusqlite::params;

use super::Context;

/// Snapshot row from the database.
#[allow(dead_code)]
struct HostSnapshot {
    host: String,
    collected_at: i64,
    online: bool,
    data: serde_json::Value,
    /// When the host was last confirmed online (from host_last_seen table).
    last_online: i64,
}

/// Columns to display, derived from enabled metrics in applicable check entries.
struct DisplayColumns {
    metrics: Vec<String>,
}

impl DisplayColumns {
    fn from_context(ctx: &Context) -> Self {
        let checks = ctx.resolve_checks();
        let mut metrics: Vec<String> = Vec::new();
        for entry in &checks {
            for m in &entry.enabled {
                if !metrics.contains(m) && m != "online" {
                    metrics.push(m.clone());
                }
            }
        }
        Self { metrics }
    }
}

pub async fn run(
    ctx: &Context,
    _history: bool,
    _since: Option<String>,
    output: &crate::cli::OutputArgs,
) -> Result<()> {
    let _ = output; // wired in Task 10
    let hosts = ctx.resolve_hosts()?;
    let host_names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();
    let columns = DisplayColumns::from_context(ctx);

    let snapshots = fetch_latest_snapshots(ctx, &host_names)?;
    print_table_report(&snapshots, &columns);

    Ok(())
}

/// Fetch the latest snapshot for each host.
fn fetch_latest_snapshots(ctx: &Context, host_names: &[&str]) -> Result<Vec<HostSnapshot>> {
    let mut snapshots = Vec::new();
    for host in host_names {
        let mut stmt = ctx.db.prepare(
            "SELECT collected_at, online, raw_json FROM check_snapshots \
             WHERE host = ?1 ORDER BY collected_at DESC LIMIT 1",
        )?;
        let entry = stmt.query_row(params![host], |row| {
            let ts: i64 = row.get(0)?;
            let online: bool = row.get(1)?;
            let json_str: String = row.get(2)?;
            Ok((ts, online, json_str))
        });

        // Query host_last_seen for the actual last_online timestamp.
        let last_online: i64 = ctx
            .db
            .query_row(
                "SELECT last_online FROM host_last_seen WHERE host = ?1",
                params![host],
                |row| row.get(0),
            )
            .unwrap_or(0);

        match entry {
            Ok((ts, online, json_str)) => {
                let data = serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
                snapshots.push(HostSnapshot {
                    host: host.to_string(),
                    collected_at: ts,
                    online,
                    data,
                    last_online,
                });
            }
            Err(_) => {
                snapshots.push(HostSnapshot {
                    host: host.to_string(),
                    collected_at: 0,
                    online: false,
                    data: serde_json::Value::Null,
                    last_online,
                });
            }
        }
    }
    Ok(snapshots)
}

fn extract_cpu_load(data: &serde_json::Value) -> String {
    let v = match data.get("cpu_load") {
        Some(v) => v,
        None => return "-".to_string(),
    };

    // Sh format: {load1: f, load5: f, load15: f}
    if let Some(obj) = v.as_object() {
        for key in &["load1", "load5", "load15"] {
            if let Some(f) = obj.get(*key).and_then(|n| n.as_f64()) {
                return format!("{:.2}", f);
            }
        }
    }
    // PowerShell / raw string format: "21.18..."
    if let Some(s) = v.as_str() {
        if let Ok(f) = s.trim().parse::<f64>() {
            return format!("{:.2}", f);
        }
    }
    if let Some(f) = v.as_f64() {
        return format!("{:.2}", f);
    }
    "-".to_string()
}

fn extract_memory(data: &serde_json::Value) -> (String, bool) {
    let v = match data.get("memory") {
        Some(v) => v,
        None => return ("-".to_string(), false),
    };

    // Sh format: {total_bytes, used_bytes}
    if let (Some(total), Some(used)) = (
        v.get("total_bytes").and_then(|n| n.as_u64()),
        v.get("used_bytes").and_then(|n| n.as_u64()),
    ) {
        if total > 0 {
            let pct = used as f64 / total as f64 * 100.0;
            return (format!("{:.0}%", pct), pct > 90.0);
        }
    }
    // PowerShell format: JSON string {TotalVisibleMemorySize, FreePhysicalMemory} (KB)
    if let Some(s) = v.as_str() {
        if let Ok(p) = serde_json::from_str::<serde_json::Value>(s) {
            if let (Some(total), Some(free)) = (
                p.get("TotalVisibleMemorySize").and_then(|n| n.as_u64()),
                p.get("FreePhysicalMemory").and_then(|n| n.as_u64()),
            ) {
                if total > 0 {
                    let pct = (total - free) as f64 / total as f64 * 100.0;
                    return (format!("{:.0}%", pct), pct > 90.0);
                }
            }
        }
    }
    ("-".to_string(), false)
}

fn extract_disk(data: &serde_json::Value) -> (String, bool) {
    let v = match data.get("disk") {
        Some(v) => v,
        None => return ("-".to_string(), false),
    };

    // Sh format: [{total_bytes, used_bytes, mount}, ...]
    if let Some(arr) = v.as_array() {
        let entry = arr
            .iter()
            .find(|e| e.get("mount").and_then(|m| m.as_str()) == Some("/"))
            .or_else(|| arr.iter().find(|e| e.get("total_bytes").is_some()));
        if let Some(e) = entry {
            if let (Some(total), Some(used)) = (
                e.get("total_bytes").and_then(|n| n.as_u64()),
                e.get("used_bytes").and_then(|n| n.as_u64()),
            ) {
                if total > 0 {
                    let pct = used as f64 / total as f64 * 100.0;
                    return (format!("{:.0}%", pct), pct > 90.0);
                }
            }
        }
    }
    // PowerShell format: JSON string [{Name, Used, Free}, ...] (bytes)
    if let Some(s) = v.as_str() {
        if let Ok(p) = serde_json::from_str::<serde_json::Value>(s) {
            if let Some(e) = p.as_array().and_then(|a| a.first()) {
                if let (Some(used), Some(free)) = (
                    e.get("Used").and_then(|n| n.as_u64()),
                    e.get("Free").and_then(|n| n.as_u64()),
                ) {
                    let total = used + free;
                    if total > 0 {
                        let pct = used as f64 / total as f64 * 100.0;
                        return (format!("{:.0}%", pct), pct > 90.0);
                    }
                }
            }
        }
    }
    ("-".to_string(), false)
}

fn extract_battery(data: &serde_json::Value) -> String {
    if let Some(bat) = data.get("battery") {
        if bat.get("present").and_then(|v| v.as_bool()) == Some(false) {
            return "N/A".to_string();
        }
        if let Some(pct) = bat.get("percent").and_then(|v| v.as_u64()) {
            return format!("{}%", pct);
        }
        // present but no percent (desktop without battery info)
        return "-".to_string();
    }
    "-".to_string()
}

fn format_relative_time(ts: i64) -> String {
    if ts == 0 {
        return "never".to_string();
    }
    let now = chrono::Utc::now().timestamp();
    let diff = now - ts;
    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

fn extract_ip_address(data: &serde_json::Value) -> String {
    if let Some(v) = data.get("ip_address") {
        if let Some(s) = v.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "-".to_string()
}

/// Extract a generic metric value as a display string.
fn extract_metric_value(data: &serde_json::Value, metric: &str) -> (String, bool) {
    match metric {
        "cpu_load" => (extract_cpu_load(data), false),
        "memory" => extract_memory(data),
        "disk" => extract_disk(data),
        "battery" => (extract_battery(data), false),
        "ip_address" => (extract_ip_address(data), false),
        "swap" => {
            if let Some(v) = data.get("swap") {
                if let (Some(total), Some(used)) = (
                    v.get("total_bytes").and_then(|n| n.as_u64()),
                    v.get("used_bytes").and_then(|n| n.as_u64()),
                ) {
                    if total > 0 {
                        let pct = used as f64 / total as f64 * 100.0;
                        return (format!("{:.0}%", pct), pct > 90.0);
                    }
                }
            }
            ("-".to_string(), false)
        }
        "system_info" => {
            if let Some(v) = data.get("system_info") {
                if let Some(obj) = v.as_object() {
                    if let Some(uname) = obj.get("uname").and_then(|u| u.as_str()) {
                        let short: String = uname
                            .split_whitespace()
                            .take(3)
                            .collect::<Vec<_>>()
                            .join(" ");
                        return (short, false);
                    }
                }
                if let Some(s) = v.as_str() {
                    let short: String = s.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
                    return (short, false);
                }
            }
            ("-".to_string(), false)
        }
        "cpu_arch" => {
            if let Some(v) = data.get("cpu_arch") {
                if let Some(s) = v.as_str() {
                    return (s.trim().to_string(), false);
                }
            }
            ("-".to_string(), false)
        }
        "network" => ("…".to_string(), false),
        _ => {
            if let Some(v) = data.get(metric) {
                if let Some(s) = v.as_str() {
                    return (s.trim().to_string(), false);
                }
                return (v.to_string(), false);
            }
            ("-".to_string(), false)
        }
    }
}

/// Map metric name to a human-readable column header.
fn metric_header(metric: &str) -> &str {
    match metric {
        "cpu_load" => "CPU Load",
        "memory" => "Memory",
        "disk" => "Disk",
        "battery" => "Battery",
        "ip_address" => "IP Address",
        "swap" => "Swap",
        "system_info" => "System",
        "cpu_arch" => "Arch",
        "network" => "Network",
        _ => metric,
    }
}

/// Column width for a metric.
fn metric_width(metric: &str) -> usize {
    match metric {
        "ip_address" => 18,
        "system_info" => 20,
        "cpu_load" => 10,
        "memory" | "disk" | "swap" | "battery" => 8,
        "cpu_arch" => 8,
        _ => 12,
    }
}

/// Print a plain-text table report to stdout with dynamic columns.
fn print_table_report(snapshots: &[HostSnapshot], columns: &DisplayColumns) {
    // Build header
    let mut header = format!("{:<16} {:<12}", "Host", "Status");
    for metric in &columns.metrics {
        let w = metric_width(metric);
        header.push_str(&format!(" {:<width$}", metric_header(metric), width = w));
    }
    header.push_str(" Last Seen");
    println!("{}", header);
    println!("{}", "-".repeat(header.len().max(78)));

    for snap in snapshots {
        let status = if snap.online {
            "\x1b[32m✓ online\x1b[0m"
        } else {
            "\x1b[31m✗ offline\x1b[0m"
        };
        let last_seen = format_relative_time(snap.last_online);

        // Status has ANSI codes (8 extra chars), so pad wider
        let mut line = format!("{:<16} {:<20}", snap.host, status);
        for metric in &columns.metrics {
            let w = metric_width(metric);
            let (val, critical) = extract_metric_value(&snap.data, metric);
            if critical {
                line.push_str(&format!(" \x1b[31m{:<width$}\x1b[0m", val, width = w));
            } else {
                line.push_str(&format!(" {:<width$}", val, width = w));
            }
        }
        line.push_str(&format!(" {}", last_seen));
        println!("{}", line);
    }
}

#[allow(dead_code)]
fn build_json_report(
    ctx: &Context,
    host_names: &[&str],
    history: bool,
    since: Option<&str>,
) -> Result<serde_json::Value> {
    let mut result = serde_json::Map::new();

    for host in host_names {
        let rows = if history {
            let since_ts = parse_since(since)?;
            let mut stmt = ctx.db.prepare(
                "SELECT collected_at, online, raw_json FROM check_snapshots \
                 WHERE host = ?1 AND collected_at >= ?2 ORDER BY collected_at DESC",
            )?;
            let entries: Vec<serde_json::Value> = stmt
                .query_map(params![host, since_ts], |row| {
                    let ts: i64 = row.get(0)?;
                    let online: bool = row.get(1)?;
                    let json_str: String = row.get(2)?;
                    Ok((ts, online, json_str))
                })?
                .filter_map(|r| r.ok())
                .map(|(ts, online, json_str)| {
                    let mut entry = serde_json::Map::new();
                    entry.insert(
                        "collected_at".to_string(),
                        serde_json::Value::Number(ts.into()),
                    );
                    entry.insert("online".to_string(), serde_json::Value::Bool(online));
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        entry.insert("data".to_string(), v);
                    }
                    serde_json::Value::Object(entry)
                })
                .collect();
            serde_json::Value::Array(entries)
        } else {
            // Latest snapshot only
            let mut stmt = ctx.db.prepare(
                "SELECT collected_at, online, raw_json FROM check_snapshots \
                 WHERE host = ?1 ORDER BY collected_at DESC LIMIT 1",
            )?;
            let entry = stmt.query_row(params![host], |row| {
                let ts: i64 = row.get(0)?;
                let online: bool = row.get(1)?;
                let json_str: String = row.get(2)?;
                Ok((ts, online, json_str))
            });

            match entry {
                Ok((ts, online, json_str)) => {
                    let mut map = serde_json::Map::new();
                    map.insert(
                        "collected_at".to_string(),
                        serde_json::Value::Number(ts.into()),
                    );
                    map.insert("online".to_string(), serde_json::Value::Bool(online));
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        map.insert("data".to_string(), v);
                    }
                    serde_json::Value::Object(map)
                }
                Err(_) => serde_json::Value::Null,
            }
        };

        result.insert(host.to_string(), rows);
    }

    Ok(serde_json::Value::Object(result))
}

#[allow(dead_code)]
fn parse_since(since: Option<&str>) -> Result<i64> {
    let now = chrono::Utc::now().timestamp();
    match since {
        None => Ok(0),
        Some(s) => {
            // Try relative duration (e.g. "7d", "24h")
            if let Some(days) = s.strip_suffix('d') {
                if let Ok(n) = days.parse::<i64>() {
                    return Ok(now - n * 86400);
                }
            }
            if let Some(hours) = s.strip_suffix('h') {
                if let Ok(n) = hours.parse::<i64>() {
                    return Ok(now - n * 3600);
                }
            }
            // Try ISO 8601 date
            if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                return Ok(dt.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp());
            }
            bail!(
                "Invalid --since value: {}. Use '7d', '24h', or 'YYYY-MM-DD'",
                s
            );
        }
    }
}

#[allow(dead_code)]
fn render_html(data: &serde_json::Value) -> Result<String> {
    let json_str = serde_json::to_string_pretty(data)?;
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ssync - System Report</title>
<style>
  body {{ font-family: -apple-system, sans-serif; margin: 2rem; background: #f5f5f5; }}
  h1 {{ color: #333; }}
  .host {{ background: white; border-radius: 8px; padding: 1.5rem; margin: 1rem 0; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
  .host h2 {{ margin-top: 0; color: #0066cc; }}
  .metric {{ display: flex; justify-content: space-between; padding: 0.25rem 0; border-bottom: 1px solid #eee; }}
  .online {{ color: #28a745; }}
  .offline {{ color: #dc3545; }}
  pre {{ background: #f8f8f8; padding: 1rem; border-radius: 4px; overflow-x: auto; font-size: 0.85rem; }}
</style>
</head>
<body>
<h1>ssync System Report</h1>
<p>Generated: {time}</p>
<pre>{json}</pre>
<script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
</body>
</html>"#,
        time = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        json = json_str,
    );
    Ok(html)
}

#[cfg(feature = "tui")]
fn run_tui(snapshots: &[HostSnapshot], columns: &DisplayColumns) -> Result<()> {
    use crossterm::{
        event::{self, Event, KeyCode},
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        ExecutableCommand,
    };
    use ratatui::{prelude::*, widgets::*};
    use std::io::stdout;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    // Build header and constraints dynamically
    let mut header_cells: Vec<&str> = vec!["Host", "Status"];
    let mut constraints: Vec<Constraint> = vec![Constraint::Length(16), Constraint::Length(12)];
    for metric in &columns.metrics {
        header_cells.push(metric_header(metric));
        constraints.push(Constraint::Length(metric_width(metric) as u16));
    }
    header_cells.push("Last Seen");
    constraints.push(Constraint::Min(10));

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let mut rows = Vec::new();
            for snap in snapshots {
                let status = if snap.online {
                    "✓ online"
                } else {
                    "✗ offline"
                };
                let last_seen = format_relative_time(snap.last_online);

                let status_style = if snap.online {
                    Style::new().fg(Color::Green)
                } else {
                    Style::new().fg(Color::Red)
                };

                let mut cells = vec![
                    Cell::from(snap.host.clone()),
                    Cell::from(status.to_string()).style(status_style),
                ];

                for metric in &columns.metrics {
                    let (val, critical) = extract_metric_value(&snap.data, metric);
                    let style = if critical {
                        Style::new().fg(Color::Red)
                    } else {
                        Style::default()
                    };
                    cells.push(Cell::from(val).style(style));
                }

                cells.push(Cell::from(last_seen));
                rows.push(Row::new(cells));
            }

            let table = Table::new(rows, &constraints)
                .header(
                    Row::new(
                        header_cells
                            .clone()
                            .into_iter()
                            .map(|h| h.to_string())
                            .collect::<Vec<_>>(),
                    )
                    .style(Style::new().bold()),
                )
                .block(Block::bordered().title(" ssync checkout — press q to quit "));

            frame.render_widget(table, area);
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
