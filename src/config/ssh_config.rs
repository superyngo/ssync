use anyhow::{Context, Result};

/// A parsed SSH host entry from ~/.ssh/config.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct SshHostEntry {
    pub name: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
}

/// Fully resolved SSH connection parameters for a host alias.
#[derive(Debug, Clone)]
pub struct ResolvedHostConfig {
    /// The alias as stored in ssync config (used for display/lookup)
    pub alias: String,
    /// Actual DNS name or IP to connect to
    pub hostname: String,
    pub port: u16,
    pub user: String,
    /// Ordered list of identity files to try
    pub identity_files: Vec<std::path::PathBuf>,
    /// First ProxyJump hop alias (None = direct connection)
    pub proxy_jump: Option<String>,
    /// Whether IdentitiesOnly is set (skip password fallback)
    pub identities_only: bool,
}

/// A parsed representation of ~/.ssh/config, supporting host-specific blocks
/// and wildcard (`Host *`) inheritance.  Replaces the ssh2-config crate to
/// avoid a transitive openssl-sys C dependency (via git2 → libgit2-sys).
#[derive(Debug, Clone, Default)]
pub struct ParsedSshConfig {
    /// Specific (non-wildcard) host entries.
    hosts: Vec<SshHostEntry>,
    /// Wildcard defaults merged into every host that lacks the field.
    wildcard_defaults: SshHostEntry,
}

impl ParsedSshConfig {
    /// Resolve `alias` to its full connection parameters, applying wildcard
    /// inheritance for any field not set in the host-specific block.
    pub fn query(&self, alias: &str) -> ResolvedHostConfig {
        // Find the first matching specific host block.
        let specific = self.hosts.iter().find(|h| h.name == alias);

        let d = &self.wildcard_defaults;

        let hostname = specific
            .and_then(|h| h.hostname.clone())
            .or_else(|| d.hostname.clone())
            .unwrap_or_else(|| alias.to_string());

        let user = specific
            .and_then(|h| h.user.clone())
            .or_else(|| d.user.clone())
            .unwrap_or_else(whoami::username);

        let port = specific.and_then(|h| h.port).or(d.port).unwrap_or(22);

        let identity_file = specific
            .and_then(|h| h.identity_file.clone())
            .or_else(|| d.identity_file.clone());

        let identity_files = identity_file
            .map(|f| vec![expand_tilde(std::path::Path::new(&f))])
            .unwrap_or_default();

        let proxy_jump = specific
            .and_then(|h| h.proxy_jump.clone())
            .or_else(|| d.proxy_jump.clone());

        ResolvedHostConfig {
            alias: alias.to_string(),
            hostname,
            port,
            user,
            identity_files,
            proxy_jump,
            identities_only: false,
        }
    }
}

/// Parse ~/.ssh/config and return a list of named host entries.
/// Skips wildcard patterns (`*`, `?`).
pub fn parse_ssh_config() -> Result<Vec<SshHostEntry>> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let config_path = home.join(".ssh").join("config");

    if !config_path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let parsed = parse_ssh_config_content(&content)?;
    Ok(parsed.hosts)
}

/// Parse raw SSH config text into a `ParsedSshConfig` that supports
/// `Host *` wildcard inheritance.
fn parse_ssh_config_content(content: &str) -> Result<ParsedSshConfig> {
    // Accumulate all blocks (wildcard and specific) then post-process.
    struct Block {
        names: Vec<String>,
        hostname: Option<String>,
        user: Option<String>,
        port: Option<u16>,
        identity_file: Option<String>,
        proxy_jump: Option<String>,
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut current: Option<Block> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = if let Some(eq_pos) = line.find('=') {
            (line[..eq_pos].trim(), line[eq_pos + 1..].trim())
        } else if let Some(space_pos) = line.find(char::is_whitespace) {
            (line[..space_pos].trim(), line[space_pos..].trim())
        } else {
            continue;
        };

        match key.to_lowercase().as_str() {
            "host" => {
                if let Some(b) = current.take() {
                    blocks.push(b);
                }
                current = Some(Block {
                    names: value.split_whitespace().map(|s| s.to_string()).collect(),
                    hostname: None,
                    user: None,
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                });
            }
            "hostname" => {
                if let Some(b) = current.as_mut() {
                    b.hostname = Some(value.to_string());
                }
            }
            "user" => {
                if let Some(b) = current.as_mut() {
                    b.user = Some(value.to_string());
                }
            }
            "port" => {
                if let Some(b) = current.as_mut() {
                    b.port = value.parse().ok();
                }
            }
            "identityfile" => {
                if let Some(b) = current.as_mut() {
                    b.identity_file = Some(value.to_string());
                }
            }
            "proxyjump" => {
                if let Some(b) = current.as_mut() {
                    // Take only the first hop; multiple hops are comma-separated.
                    let first_hop = value.split(',').next().unwrap_or(value).trim().to_string();
                    b.proxy_jump = Some(first_hop);
                }
            }
            _ => {}
        }
    }
    if let Some(b) = current.take() {
        blocks.push(b);
    }

    // Separate wildcard blocks (Host *) from specific host blocks.
    let mut config = ParsedSshConfig::default();
    for block in blocks {
        let all_wildcard = block.names.iter().all(|n| is_wildcard(n));
        if all_wildcard {
            // Merge this wildcard block into the defaults (first-wins for each field).
            let d = &mut config.wildcard_defaults;
            if d.hostname.is_none() {
                d.hostname = block.hostname;
            }
            if d.user.is_none() {
                d.user = block.user;
            }
            if d.port.is_none() {
                d.port = block.port;
            }
            if d.identity_file.is_none() {
                d.identity_file = block.identity_file;
            }
            if d.proxy_jump.is_none() {
                d.proxy_jump = block.proxy_jump;
            }
        } else {
            // Expand multi-alias blocks into individual SshHostEntry values.
            for name in block.names.iter().filter(|n| !is_wildcard(n)) {
                config.hosts.push(SshHostEntry {
                    name: name.clone(),
                    hostname: block.hostname.clone(),
                    user: block.user.clone(),
                    port: block.port,
                    identity_file: block.identity_file.clone(),
                    proxy_jump: block.proxy_jump.clone(),
                });
            }
        }
    }

    Ok(config)
}

fn is_wildcard(name: &str) -> bool {
    name.contains('*') || name.contains('?')
}

/// Load and parse `~/.ssh/config`.
/// For bulk use (multiple hosts), prefer `load_ssh_config()` once and call
/// `config.query(alias)` in a loop.
pub fn load_ssh_config() -> Result<ParsedSshConfig> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let config_path = home.join(".ssh").join("config");

    if !config_path.exists() {
        return Ok(ParsedSshConfig::default());
    }

    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    parse_ssh_config_content(&content)
}

/// Resolve a host alias using a pre-loaded `ParsedSshConfig`.
pub fn resolve_host_with_config(
    alias: &str,
    config: &ParsedSshConfig,
) -> Result<ResolvedHostConfig> {
    Ok(config.query(alias))
}

/// Resolve a host alias to its full connection parameters.
/// For bulk use, prefer `load_ssh_config()` + `resolve_host_with_config()`.
pub fn resolve_host(alias: &str) -> Result<ResolvedHostConfig> {
    let config = load_ssh_config()?;
    resolve_host_with_config(alias, &config)
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_config() {
        let content = r#"
Host home-linux
    HostName 192.168.1.10
    User alice
    Port 22
    IdentityFile ~/.ssh/id_rsa

Host work-windows
    HostName 10.0.0.5
    User bob

Host *
    ServerAliveInterval 60
"#;
        let config = parse_ssh_config_content(content).unwrap();
        assert_eq!(config.hosts.len(), 2);
        assert_eq!(config.hosts[0].name, "home-linux");
        assert_eq!(config.hosts[0].hostname.as_deref(), Some("192.168.1.10"));
        assert_eq!(config.hosts[0].user.as_deref(), Some("alice"));
        assert_eq!(config.hosts[0].port, Some(22));
        assert_eq!(config.hosts[1].name, "work-windows");
        assert_eq!(config.hosts[1].hostname.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn test_skip_wildcards_in_hosts_list() {
        let content = "Host *\n    ServerAliveInterval 60\n\nHost ?\n    Foo bar\n";
        let config = parse_ssh_config_content(content).unwrap();
        assert!(config.hosts.is_empty());
    }

    #[test]
    fn test_multi_alias_parsed_as_separate_hosts() {
        let content = r#"
Host bastion.bss-qa slb225
    HostName 10.0.0.1
    User admin
    Port 2222

Host web1
    HostName 192.168.1.10
"#;
        let config = parse_ssh_config_content(content).unwrap();
        assert_eq!(config.hosts.len(), 3);
        let bss = config
            .hosts
            .iter()
            .find(|h| h.name == "bastion.bss-qa")
            .unwrap();
        let slb = config.hosts.iter().find(|h| h.name == "slb225").unwrap();
        let web = config.hosts.iter().find(|h| h.name == "web1").unwrap();
        assert_eq!(bss.hostname.as_deref(), Some("10.0.0.1"));
        assert_eq!(bss.port, Some(2222));
        assert_eq!(slb.hostname.as_deref(), Some("10.0.0.1"));
        assert_eq!(slb.port, Some(2222));
        assert_eq!(web.hostname.as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn test_proxy_jump_parsed() {
        let content = r#"
Host internal
    HostName 10.10.0.5
    ProxyJump bastion
"#;
        let config = parse_ssh_config_content(content).unwrap();
        assert_eq!(config.hosts[0].proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn test_wildcard_inheritance_fills_missing_fields() {
        let content = r#"
Host specific
    HostName 10.0.0.1

Host *
    User defaultuser
    IdentityFile ~/.ssh/id_rsa
    Port 2222
"#;
        let config = parse_ssh_config_content(content).unwrap();
        let resolved = config.query("specific");
        assert_eq!(resolved.hostname, "10.0.0.1");
        // user and identity_file come from wildcard block
        assert_eq!(resolved.user, "defaultuser");
        assert_eq!(resolved.port, 2222);
        assert!(!resolved.identity_files.is_empty());
    }

    #[test]
    fn test_specific_host_overrides_wildcard() {
        let content = r#"
Host myhost
    HostName 10.0.0.5
    User specialuser
    Port 443

Host *
    User defaultuser
    Port 22
"#;
        let config = parse_ssh_config_content(content).unwrap();
        let resolved = config.query("myhost");
        // Specific block takes precedence over wildcard.
        assert_eq!(resolved.user, "specialuser");
        assert_eq!(resolved.port, 443);
    }

    #[test]
    fn test_unknown_host_falls_back_to_alias_as_hostname() {
        let content = r#"
Host known
    HostName 192.168.1.1
"#;
        let config = parse_ssh_config_content(content).unwrap();
        let resolved = config.query("unknown-host");
        assert_eq!(resolved.hostname, "unknown-host");
        assert_eq!(resolved.port, 22);
    }
}
