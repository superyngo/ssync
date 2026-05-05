use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{value, DocumentMut, Item, Table, Value};

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

/// Expand a leading `~` or `~/` in `p` to the user's home directory.
/// Returns the path unchanged if no tilde prefix is present or home cannot be resolved.
fn expand_tilde(p: &Path) -> PathBuf {
    let s = match p.to_str() {
        Some(s) => s,
        None => return p.to_path_buf(),
    };
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| p.to_path_buf());
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

/// Resolve the effective config file path (AD-16): expands `~` so explicit and
/// default paths hash identically.
pub fn resolve_path(custom_path: Option<&Path>) -> Result<PathBuf> {
    match custom_path {
        Some(p) => Ok(expand_tilde(p)),
        None => config_path(),
    }
}

/// Strip a leading UTF-8 BOM from the input if present.
fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Load config from disk. Returns None if file doesn't exist.
pub fn load(custom_path: Option<&Path>) -> Result<Option<AppConfig>> {
    let path = resolve_path(custom_path)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let content = strip_bom(&raw);
    let config: AppConfig =
        toml::from_str(content).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(config))
}

/// Save config to disk, creating parent directories if needed.
///
/// First-create path: serialize fresh and inject `inject_config_comments` guidance.
/// Edit path: parse existing file with `toml_edit::DocumentMut`, mutate scalars
/// in place, and write back — preserving user comments and key order.
/// Atomic write via `tempfile::persist()` (cross-platform safe).
pub fn save(config: &AppConfig, custom_path: Option<&Path>) -> Result<()> {
    let path = resolve_path(custom_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let new_content = match std::fs::read_to_string(&path) {
        Ok(original) => {
            let trimmed = strip_bom(&original);
            match trimmed.parse::<DocumentMut>() {
                Ok(mut doc) => {
                    apply_config_to_doc(&mut doc, config);
                    let candidate = doc.to_string();
                    // Round-trip validate: catch apply_config_to_doc bugs before writing.
                    toml::from_str::<AppConfig>(&candidate).context(
                        "apply_config_to_doc produced invalid TOML; aborting write",
                    )?;
                    candidate
                }
                Err(e) => {
                    tracing::warn!(
                        "Config comments lost — file contained non-standard formatting \
                         that toml_edit could not parse: {e}"
                    );
                    toml::to_string_pretty(config).context("Failed to serialize config")?
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let serialized = toml::to_string_pretty(config).context("Failed to serialize config")?;
            inject_config_comments(&serialized)
        }
        Err(e) => return Err(e).context("Failed to read existing config"),
    };

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".ssync-config-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("Failed to create temp file in {}", parent.display()))?;
    tmp.as_file_mut()
        .write_all(new_content.as_bytes())
        .context("Failed to write temp config file")?;
    tmp.as_file_mut().flush().context("Failed to flush temp config file")?;
    tmp.persist(&path)
        .map_err(|e| e.error)
        .with_context(|| format!("Failed to persist {}", path.display()))?;
    Ok(())
}

/// Mutate a parsed TOML document in place to reflect the in-memory `AppConfig`.
///
/// **Phase 0.5 scope:** scalar fields under `[settings]` are written through;
/// per-entry scalars inside `[[host]]` / `[[check]]` / `[[sync]]` and full
/// array-of-tables add/remove/reorder are deferred to Phase 7.
///
/// Unknown top-level keys are preserved automatically by `toml_edit`.
/// Set a scalar key in `table`, preserving the existing item's decor
/// (whitespace and inline comments) if the key already exists.
fn set_scalar<V: Into<Value>>(table: &mut Table, key: &str, v: V) {
    let new_val: Value = v.into();
    match table.get_mut(key) {
        Some(Item::Value(existing)) => {
            let decor = existing.decor().clone();
            let mut replacement = new_val;
            *replacement.decor_mut() = decor;
            *existing = replacement;
        }
        Some(slot) => {
            *slot = Item::Value(new_val);
        }
        None => {
            table.insert(key, Item::Value(new_val));
        }
    }
}

fn apply_config_to_doc(doc: &mut DocumentMut, config: &AppConfig) {
    // Ensure [settings] exists as a table.
    if !doc.contains_key("settings") {
        doc.insert("settings", Item::Table(Table::new()));
    }
    let settings = doc["settings"]
        .as_table_mut()
        .expect("settings must be a table");

    set_scalar(settings, "default_timeout", config.settings.default_timeout as i64);
    set_scalar(
        settings,
        "data_retention_days",
        config.settings.data_retention_days as i64,
    );
    set_scalar(
        settings,
        "conflict_strategy",
        match config.settings.conflict_strategy {
            super::schema::ConflictStrategy::Newest => "newest",
            super::schema::ConflictStrategy::Skip => "skip",
        },
    );
    set_scalar(settings, "propagate_deletes", config.settings.propagate_deletes);
    set_scalar(settings, "max_concurrency", config.settings.max_concurrency as i64);
    set_scalar(
        settings,
        "max_per_host_concurrency",
        config.settings.max_per_host_concurrency as i64,
    );

    // skipped_hosts: write as inline array; remove if empty.
    if config.settings.skipped_hosts.is_empty() {
        settings.remove("skipped_hosts");
    } else {
        let mut arr = toml_edit::Array::new();
        for h in &config.settings.skipped_hosts {
            arr.push(h.as_str());
        }
        settings["skipped_hosts"] = value(Value::Array(arr));
    }

    match &config.settings.state_dir {
        Some(dir) => {
            set_scalar(settings, "state_dir", dir.to_string_lossy().into_owned());
        }
        None => {
            settings.remove("state_dir");
        }
    }

    match &config.settings.default_output_format {
        Some(fmt) => {
            set_scalar(settings, "default_output_format", fmt.as_str());
        }
        None => {
            settings.remove("default_output_format");
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .prefix("ssync-cfg-test-")
            .suffix(".toml")
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn bom_is_stripped_on_load() {
        let with_bom = format!("\u{feff}[settings]\ndefault_timeout = 7\n");
        let f = write_tmp(&with_bom);
        let cfg = load(Some(f.path())).unwrap().unwrap();
        assert_eq!(cfg.settings.default_timeout, 7);
    }

    #[test]
    fn tilde_resolution() {
        let p = std::path::Path::new("~/foo/bar.toml");
        let resolved = resolve_path(Some(p)).unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolved, home.join("foo/bar.toml"));
    }

    /// T-WB-2: unknown top-level key inside [settings] survives a save round-trip.
    #[test]
    fn t_wb_2_preserve_unknown_key() {
        let original = "\
[settings]
default_timeout = 30
unknown_future_option = true
";
        let f = write_tmp(original);
        let cfg = load(Some(f.path())).unwrap().unwrap();
        save(&cfg, Some(f.path())).unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            after.contains("unknown_future_option = true"),
            "unknown key dropped:\n{after}"
        );
    }

    /// T-WB-3: an inline comment on a scalar key survives an edit to that scalar.
    #[test]
    fn t_wb_3_inline_comment_survives_scalar_edit() {
        let original = "\
[settings]
max_concurrency = 10  # max 50
";
        let f = write_tmp(original);
        let mut cfg = load(Some(f.path())).unwrap().unwrap();
        cfg.settings.max_concurrency = 20;
        save(&cfg, Some(f.path())).unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            after.contains("max_concurrency = 20"),
            "value not updated:\n{after}"
        );
        assert!(
            after.contains("# max 50"),
            "inline comment dropped:\n{after}"
        );
    }
}
