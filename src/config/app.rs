use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::schema::AppConfig;

/// Returns the platform-appropriate config directory for ssync.
pub fn config_dir() -> Result<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        Ok(home.join(".config").join("ssync"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = dirs::config_dir().context("Cannot determine config directory")?;
        Ok(base.join("ssync"))
    }
}

/// Returns the path to config.toml.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Resolve the effective config file path.
pub fn resolve_path(custom_path: Option<&Path>) -> Result<PathBuf> {
    match custom_path {
        Some(p) => Ok(p.to_path_buf()),
        None => config_path(),
    }
}

/// Load config from disk. Returns None if file doesn't exist.
pub fn load(custom_path: Option<&Path>) -> Result<Option<AppConfig>> {
    let path = resolve_path(custom_path)?;
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
/// Adds helpful comments to [settings], [[check]] and [[sync]] sections.
pub fn save(config: &AppConfig, custom_path: Option<&Path>) -> Result<()> {
    let path = resolve_path(custom_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(config).context("Failed to serialize config")?;
    let content = inject_config_comments(&content);
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Inject helpful comments into TOML config for [settings], [[check]] and [[sync]] sections.
fn inject_config_comments(toml_str: &str) -> String {
    let settings_comment = "\
# [settings] Global settings:
#   state_dir = \"/custom/path/to/state\"  # Custom DB storage location
#                                          # Default: ~/.local/state/ssync (Linux/macOS)
#                                          #          %LOCALAPPDATA%/ssync (Windows)
";

    let check_comment = "\
# [[check]] Available enabled values:
# enabled = [
#     \"online\",        # Check if host is online
#     \"system_info\",   # System info (uname / systeminfo)
#     \"cpu_arch\",      # CPU architecture
#     \"memory\",        # Memory usage
#     \"swap\",          # Swap usage
#     \"disk\",          # Disk usage
#     \"cpu_load\",      # CPU load
#     \"network\",       # Network interface info
#     \"battery\",       # Battery status
#     \"ip_address\",    # IP address
# ]
#
# ── Scoping ──
# groups = [\"web\"]       # Apply only to specified groups (used with --group/-g)
#
# ── Visibility controls (default: true) ──
# enable_hosts = true     # Include this entry when using --host/-h mode
#                         # Set to false to exclude from per-host checks
# enable_all   = true     # Include this entry when using --all/-a mode
#                         # Set to false to exclude from whole-fleet checks
#
# Note: When groups is non-empty, the entry is scoped to those groups
#       and only selected with --group/-g (enable_hosts/enable_all are ignored).
#       When groups is empty, the entry is unscoped and filtered by
#       enable_hosts (--host/-h) or enable_all (--all/-a).
#
# [[check.path]] Custom path monitoring:
#   path  = \"/var/log\"    # Path to monitor
#   label = \"Logs\"        # Display label
#
# Examples:
# [[check]]
# enabled = [\"online\", \"memory\", \"disk\", \"cpu_load\", \"ip_address\"]
#
# [[check]]
# enabled = [\"online\", \"memory\", \"disk\", \"cpu_load\"]
# groups = [\"webservers\"]
# [[check.path]]
# path = \"/var/log/nginx\"
# label = \"Nginx Logs\"
#
# [[check]]
# enabled = [\"online\", \"disk\"]
# enable_hosts = false    # Only run with --all or --group, not --host
";

    let sync_comment = "\
# [[sync]] Sync settings:
#
# ── Unscoped sync (used with --all/-a when groups is empty) ──
# [[sync]]
# paths = [\"/etc/timezone\"]            # File paths to sync (multiple allowed)
# recursive = false                    # Recursive sync (default: false)
# mode = \"0644\"                        # File permissions (optional)
# propagate_deletes = false            # Sync deletions (optional, default: false)
# source = \"myhost\"                    # Fixed source host (optional, skips auto-selection)
#
# ── Group sync (used with --group/-g) ──
# [[sync]]
# paths = [\"/etc/nginx/nginx.conf\", \"/etc/nginx/conf.d\"]
# groups = [\"webservers\"]              # Target groups (matches host[].groups)
#
# ── Per-host sync (used with --host/-h) ──
# [[sync]]
# paths = [\"/etc/special.conf\"]
# hosts = [\"special-host\"]             # Target hosts (matches host[].name)
#
# ── Visibility controls (default: true) ──
# enable_hosts = true     # Include this entry when using --host/-h mode
#                         # Set to false to exclude from per-host syncs
# enable_all   = true     # Include this entry when using --all/-a mode
#                         # Set to false to exclude from whole-fleet syncs
#
# Note: When groups is non-empty, the entry is scoped to those groups
#       and only selected with --group/-g (enable_hosts/enable_all are ignored).
#       When groups is empty, the entry is unscoped and filtered by
#       enable_hosts (--host/-h) or enable_all (--all/-a).
#
# Example: group-only sync that won't run with --all or --host:
# [[sync]]
# paths = [\"/etc/nginx/nginx.conf\"]
# groups = [\"webservers\"]
# enable_hosts = false
# enable_all = false
";

    let mut result = String::new();
    let mut has_check = false;
    let mut has_sync = false;
    for line in toml_str.lines() {
        if line.trim() == "[settings]" {
            result.push_str(settings_comment);
        } else if line.trim() == "[[check]]" && !has_check {
            result.push_str(check_comment);
            has_check = true;
        } else if line.trim() == "[[sync]]" && !has_sync {
            result.push_str(sync_comment);
            has_sync = true;
        }
        result.push_str(line);
        result.push('\n');
    }

    // Append comment blocks for sections that are empty / absent in the TOML
    if !has_check {
        result.push('\n');
        result.push_str(check_comment);
    }
    if !has_sync {
        result.push('\n');
        result.push_str(sync_comment);
    }

    result
}
