use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::Connection;

const CURRENT_VERSION: u32 = 1;

/// Returns the platform-appropriate state directory for ssync.
pub fn state_dir() -> Result<PathBuf> {
    // On macOS/Linux: ~/.local/state/ssync
    // On Windows: %LOCALAPPDATA%/ssync
    let _base = dirs::data_local_dir().context("Cannot determine local data directory")?;
    // Use state subdirectory on Linux/macOS for XDG compliance
    #[cfg(not(target_os = "windows"))]
    let base = {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        home.join(".local").join("state")
    };
    Ok(base.join("ssync"))
}

/// Returns the path to ssync.db.
pub fn db_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("ssync.db"))
}

/// Open or create the SQLite database with migrations applied.
pub fn open() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open database {}", path.display()))?;

    // Enable WAL mode for better concurrent reads
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

    if version < 1 {
        conn.execute_batch(include_str!("migrations/001_init.sql"))?;
        conn.pragma_update(None, "user_version", CURRENT_VERSION)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_in_memory() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        migrate(&conn).unwrap();

        let version: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_VERSION);
    }
}
