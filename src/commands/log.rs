use anyhow::Result;

use crate::cli::ActionFilter;

use super::Context;

pub async fn run(
    ctx: &Context,
    last: usize,
    since: Option<String>,
    host: Option<String>,
    action: Option<ActionFilter>,
    errors: bool,
) -> Result<()> {
    let mut query = String::from(
        "SELECT timestamp, command, host, action, status, duration_ms, note FROM operation_log WHERE 1=1",
    );
    let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref h) = host {
        query.push_str(&format!(" AND host = ?{}", bind_values.len() + 1));
        bind_values.push(Box::new(h.clone()));
    }

    if let Some(ref a) = action {
        let action_str = match a {
            ActionFilter::Sync => "sync",
            ActionFilter::Run => "run",
            ActionFilter::Exec => "exec",
            ActionFilter::Check => "check",
        };
        query.push_str(&format!(" AND command = ?{}", bind_values.len() + 1));
        bind_values.push(Box::new(action_str.to_string()));
    }

    if errors {
        query.push_str(" AND status = 'error'");
    }

    if let Some(ref s) = since {
        let since_ts = parse_since(s)?;
        query.push_str(&format!(" AND timestamp >= ?{}", bind_values.len() + 1));
        bind_values.push(Box::new(since_ts));
    }

    query.push_str(" ORDER BY timestamp DESC");
    query.push_str(&format!(" LIMIT {}", last));

    let mut stmt = ctx.db.prepare(&query)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        bind_values.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        let ts: i64 = row.get(0)?;
        let command: String = row.get(1)?;
        let host: String = row.get(2)?;
        let action: String = row.get(3)?;
        let status: String = row.get(4)?;
        let duration_ms: Option<i64> = row.get(5)?;
        let note: Option<String> = row.get(6)?;
        Ok((ts, command, host, action, status, duration_ms, note))
    })?;

    let mut count = 0;
    for row in rows {
        let (ts, command, host, action, status, duration_ms, note) = row?;
        let time = chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| ts.to_string());

        let duration = duration_ms
            .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
            .unwrap_or_default();

        let note_str = note.map(|n| format!(" — {}", n)).unwrap_or_default();

        let status_icon = match status.as_str() {
            "ok" => "\x1b[32m✓\x1b[0m",
            "error" => "\x1b[31m✗\x1b[0m",
            "skipped" => "\x1b[33m⊘\x1b[0m",
            _ => "·",
        };

        println!(
            "{} {} [{}] {} {}{}{}",
            time, status_icon, host, command, action, duration, note_str
        );
        count += 1;
    }

    if count == 0 {
        println!("No log entries found.");
    }

    Ok(())
}

fn parse_since(s: &str) -> Result<i64> {
    let now = chrono::Utc::now().timestamp();
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
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(dt.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp());
    }
    anyhow::bail!("Invalid --since value: {}", s);
}
