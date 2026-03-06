use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::app;

pub async fn run(config_path: Option<&Path>) -> Result<()> {
    let path = app::resolve_path(config_path)?;

    if !path.exists() {
        bail!(
            "Config file not found at {}\nRun 'ssync init' first to create it.",
            path.display()
        );
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            if cfg!(target_os = "windows") {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });

    Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("Failed to open editor '{}'", editor))?;

    Ok(())
}
