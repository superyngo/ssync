use anyhow::Result;
use rusqlite::Connection;

/// Delete check_snapshots and operation_log entries older than retention_days.
/// If retention_days is 0, no cleanup is performed (keep forever).
pub fn cleanup(conn: &Connection, retention_days: u64) -> Result<()> {
    if retention_days == 0 {
        return Ok(());
    }

    let cutoff_secs = retention_days * 86400;

    conn.execute(
        "DELETE FROM check_snapshots WHERE collected_at < (strftime('%s', 'now') - ?1)",
        [cutoff_secs],
    )?;

    conn.execute(
        "DELETE FROM operation_log WHERE timestamp < (strftime('%s', 'now') - ?1)",
        [cutoff_secs],
    )?;

    Ok(())
}
