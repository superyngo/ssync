use anyhow::{bail, Result};
use rusqlite::params;

use crate::cli::OutputFormat;

use super::Context;

pub async fn run(
    ctx: &Context,
    format: OutputFormat,
    history: bool,
    since: Option<String>,
    out: Option<String>,
) -> Result<()> {
    let hosts = ctx.require_targets()?;
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
        OutputFormat::Tui => {
            #[cfg(feature = "tui")]
            {
                run_tui(ctx, &host_names, history, since.as_deref())?;
            }
            #[cfg(not(feature = "tui"))]
            {
                bail!("TUI support not compiled. Use --format json or --format html.");
            }
        }
    }

    Ok(())
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
                    entry.insert("collected_at".to_string(), serde_json::Value::Number(ts.into()));
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
                    map.insert("collected_at".to_string(), serde_json::Value::Number(ts.into()));
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
            bail!("Invalid --since value: {}. Use '7d', '24h', or 'YYYY-MM-DD'", s);
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
fn run_tui(
    ctx: &Context,
    host_names: &[&str],
    history: bool,
    since: Option<&str>,
) -> Result<()> {
    use crossterm::{
        event::{self, Event, KeyCode},
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        ExecutableCommand,
    };
    use ratatui::{prelude::*, widgets::*};
    use std::io::stdout;

    let data = build_json_report(ctx, host_names, history, since)?;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            // Build table rows from data
            let mut rows = Vec::new();
            if let Some(obj) = data.as_object() {
                for (host, val) in obj {
                    let online = val
                        .get("online")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let status = if online { "✓ online" } else { "✗ offline" };
                    rows.push(Row::new(vec![host.clone(), status.to_string()]));
                }
            }

            let table = Table::new(
                rows,
                [Constraint::Length(20), Constraint::Min(30)],
            )
            .header(Row::new(vec!["Host", "Status"]).style(Style::new().bold()))
            .block(Block::bordered().title(" ssync checkout "));

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
