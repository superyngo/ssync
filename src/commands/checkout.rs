use anyhow::{bail, Result};
use rusqlite::params;

use crate::cli::OutputFormat;

use super::Context;

/// Snapshot row from the database.
struct HostSnapshot {
    host: String,
    collected_at: i64,
    online: bool,
    data: serde_json::Value,
}

pub async fn run(
    ctx: &Context,
    format: OutputFormat,
    history: bool,
    since: Option<String>,
    out: Option<String>,
) -> Result<()> {
    let hosts = ctx.resolve_hosts()?;
    let host_names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();

    match format {
        OutputFormat::Json => {
            let path = out.as_deref().unwrap_or("-");
            let data = build_json_report(ctx, &host_names, history, since.as_deref())?;
            if path == "-" {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                std::fs::write(path, serde_json::to_string_pretty(&data)?)?;
                println!("JSON report written to {}", path);
            }
        }
        OutputFormat::Html => {
            let path = out
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--out is required for HTML format"))?;
            let data = build_json_report(ctx, &host_names, history, since.as_deref())?;
            let html = render_html(&data)?;
            std::fs::write(path, html)?;
            println!("HTML report written to {}", path);
        }
        OutputFormat::Table => {
            let snapshots = fetch_latest_snapshots(ctx, &host_names)?;
            print_table_report(&snapshots);
        }
        OutputFormat::Tui => {
            #[cfg(feature = "tui")]
            {
                let snapshots = fetch_latest_snapshots(ctx, &host_names)?;
                run_tui(&snapshots)?;
            }
            #[cfg(not(feature = "tui"))]
            {
                bail!("TUI support not compiled. Use --format json or --format html.");
            }
        }
    }

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

        match entry {
            Ok((ts, online, json_str)) => {
                let data = serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
                snapshots.push(HostSnapshot {
                    host: host.to_string(),
                    collected_at: ts,
                    online,
                    data,
                });
            }
            Err(_) => {
                snapshots.push(HostSnapshot {
                    host: host.to_string(),
                    collected_at: 0,
                    online: false,
                    data: serde_json::Value::Null,
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

/// Print a plain-text table report to stdout.
fn print_table_report(snapshots: &[HostSnapshot]) {
    // Header
    println!(
        "{:<16} {:<12} {:<10} {:<8} {:<8} {:<8} Last Seen",
        "Host", "Status", "CPU Load", "Memory", "Disk", "Battery"
    );
    println!("{}", "-".repeat(78));

    for snap in snapshots {
        let status = if snap.online {
            "\x1b[32m✓ online\x1b[0m"
        } else {
            "\x1b[31m✗ offline\x1b[0m"
        };
        let cpu = extract_cpu_load(&snap.data);
        let (mem, mem_crit) = extract_memory(&snap.data);
        let (disk, disk_crit) = extract_disk(&snap.data);
        let bat = extract_battery(&snap.data);
        let last_seen = format_relative_time(snap.collected_at);

        let mem_str = if mem_crit {
            format!("\x1b[31m{}\x1b[0m", mem)
        } else {
            mem
        };
        let disk_str = if disk_crit {
            format!("\x1b[31m{}\x1b[0m", disk)
        } else {
            disk
        };

        println!(
            "{:<16} {:<20} {:<10} {:<16} {:<16} {:<8} {}",
            snap.host, status, cpu, mem_str, disk_str, bat, last_seen
        );
    }
}

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
fn run_tui(snapshots: &[HostSnapshot]) -> Result<()> {
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
                let cpu = extract_cpu_load(&snap.data);
                let (mem, mem_crit) = extract_memory(&snap.data);
                let (disk, disk_crit) = extract_disk(&snap.data);
                let bat = extract_battery(&snap.data);
                let last_seen = format_relative_time(snap.collected_at);

                let status_style = if snap.online {
                    Style::new().fg(Color::Green)
                } else {
                    Style::new().fg(Color::Red)
                };
                let mem_style = if mem_crit {
                    Style::new().fg(Color::Red)
                } else {
                    Style::default()
                };
                let disk_style = if disk_crit {
                    Style::new().fg(Color::Red)
                } else {
                    Style::default()
                };

                rows.push(Row::new(vec![
                    Cell::from(snap.host.clone()),
                    Cell::from(status.to_string()).style(status_style),
                    Cell::from(cpu),
                    Cell::from(mem).style(mem_style),
                    Cell::from(disk).style(disk_style),
                    Cell::from(bat),
                    Cell::from(last_seen),
                ]));
            }

            let table = Table::new(
                rows,
                [
                    Constraint::Length(16),
                    Constraint::Length(12),
                    Constraint::Length(10),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Min(10),
                ],
            )
            .header(
                Row::new(vec![
                    "Host",
                    "Status",
                    "CPU Load",
                    "Memory",
                    "Disk",
                    "Battery",
                    "Last Seen",
                ])
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
