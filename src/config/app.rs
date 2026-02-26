use std::path::PathBuf;

use anyhow::{Context, Result};

use super::schema::AppConfig;

/// Returns the platform-appropriate config directory for ssync.
pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("Cannot determine config directory")?;
    Ok(base.join("ssync"))
}

/// Returns the path to config.toml.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Load config from disk. Returns None if file doesn't exist.
pub fn load() -> Result<Option<AppConfig>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config: AppConfig =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(config))
}

/// Save config to disk, creating parent directories if needed.
pub fn save(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let content =
        toml::to_string_pretty(config).context("Failed to serialize config")?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}
