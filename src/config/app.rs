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
        return Ok(base.join("ssync"));
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
/// Adds helpful comments to [check] and [sync] sections.
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

/// Inject helpful comments into TOML config for [check] and [sync] sections.
fn inject_config_comments(toml_str: &str) -> String {
    let check_comment = "\
# [check] 可設定的 enabled 值：
#   \"online\"      - 檢查主機是否在線
#   \"system_info\" - 系統資訊 (uname / systeminfo)
#   \"cpu_arch\"    - CPU 架構
#   \"memory\"      - 記憶體使用量
#   \"swap\"        - Swap 使用量
#   \"disk\"        - 磁碟使用量
#   \"cpu_load\"    - CPU 負載
#   \"network\"     - 網路介面資訊
#   \"battery\"     - 電池狀態
#
# [[check.path]] 可設定自訂路徑監控：
#   path  = \"/var/log\"    # 要監控的路徑
#   label = \"Logs\"        # 顯示用的標籤
#
# 範例：
#   enabled = [\"online\", \"memory\", \"disk\", \"cpu_load\"]
#   [[check.path]]
#   path = \"/home\"
#   label = \"Home\"
";

    let sync_comment = "\
# [sync] 同步設定：
#
# ── 全域同步 (搭配 --all/-a 使用) ──
# [[sync.file]]
# paths = [\"/etc/timezone\"]          # 要同步的檔案路徑 (可多個)
# recursive = false                 # 是否遞迴同步 (預設: false)
# mode = \"0644\"                     # 檔案權限 (選填)
# propagate_deletes = false         # 是否同步刪除 (選填, 預設: false)
#
# ── 群組同步 (搭配 --group/-g 使用) ──
# [[sync.file]]
# paths = [\"/etc/nginx/nginx.conf\", \"/etc/nginx/conf.d\"]
# groups = [\"webservers\"]           # 套用的 group (對應 host[].groups)
#
# [[sync.file]]
# paths = [\"/etc/my.cnf\"]
# groups = [\"databases\"]
";

    let mut result = String::new();
    for line in toml_str.lines() {
        if line.trim() == "[check]" {
            result.push_str(check_comment);
        } else if line.trim() == "[sync]" {
            result.push_str(sync_comment);
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}
