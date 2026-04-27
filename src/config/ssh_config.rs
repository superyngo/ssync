use anyhow::{Context, Result};

/// A parsed SSH host entry from ~/.ssh/config.
#[derive(Debug, Clone)]
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

/// Parse ~/.ssh/config and return a list of host entries.
/// Only parses basic Host blocks; skips wildcards (* and ?).
pub fn parse_ssh_config() -> Result<Vec<SshHostEntry>> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let config_path = home.join(".ssh").join("config");

    if !config_path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    parse_ssh_config_content(&content)
}

fn flush_block(
    hosts: &mut Vec<SshHostEntry>,
    names: &[String],
    hostname: &Option<String>,
    user: &Option<String>,
    port: Option<u16>,
    identity_file: &Option<String>,
    proxy_jump: &Option<String>,
) {
    for name in names.iter().filter(|n| !is_wildcard(n)) {
        hosts.push(SshHostEntry {
            name: name.clone(),
            hostname: hostname.clone(),
            user: user.clone(),
            port,
            identity_file: identity_file.clone(),
            proxy_jump: proxy_jump.clone(),
        });
    }
}

fn parse_ssh_config_content(content: &str) -> Result<Vec<SshHostEntry>> {
    let mut hosts = Vec::new();
    let mut pending_names: Vec<String> = Vec::new();
    let mut hostname: Option<String> = None;
    let mut user: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut identity_file: Option<String> = None;
    let mut proxy_jump: Option<String> = None;

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
                flush_block(
                    &mut hosts,
                    &pending_names,
                    &hostname,
                    &user,
                    port,
                    &identity_file,
                    &proxy_jump,
                );
                pending_names = value.split_whitespace().map(|s| s.to_string()).collect();
                hostname = None;
                user = None;
                port = None;
                identity_file = None;
                proxy_jump = None;
            }
            "hostname" => hostname = Some(value.to_string()),
            "user" => user = Some(value.to_string()),
            "port" => port = value.parse().ok(),
            "identityfile" => identity_file = Some(value.to_string()),
            "proxyjump" => {
                // Take only the first hop; multiple hops are comma-separated
                let first_hop = value.split(',').next().unwrap_or(value).trim().to_string();
                proxy_jump = Some(first_hop);
            }
            _ => {}
        }
    }

    flush_block(
        &mut hosts,
        &pending_names,
        &hostname,
        &user,
        port,
        &identity_file,
        &proxy_jump,
    );
    Ok(hosts)
}

fn is_wildcard(name: &str) -> bool {
    name.contains('*') || name.contains('?')
}

/// Parse ~/.ssh/config using the ssh2-config crate (handles full OpenSSH semantics).
pub fn load_ssh_config() -> Result<ssh2_config::SshConfig> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    let config_path = home.join(".ssh").join("config");

    if !config_path.exists() {
        return Ok(ssh2_config::SshConfig::default());
    }

    let file = std::fs::File::open(&config_path)
        .with_context(|| format!("Failed to open {}", config_path.display()))?;
    let mut reader = std::io::BufReader::new(file);

    ssh2_config::SshConfig::default()
        .parse(&mut reader, ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS)
        .with_context(|| format!("Failed to parse {}", config_path.display()))
}

/// Resolve a host alias using a pre-loaded SshConfig (avoids re-parsing for bulk use).
pub fn resolve_host_with_config(
    alias: &str,
    config: &ssh2_config::SshConfig,
) -> Result<ResolvedHostConfig> {
    let params = config.query(alias);

    let hostname = params.host_name.as_deref().unwrap_or(alias).to_string();

    let port = params.port.unwrap_or(22);

    let user = params.user.clone().unwrap_or_else(whoami::username);

    let identity_files: Vec<std::path::PathBuf> = params
        .identity_file
        .unwrap_or_default()
        .into_iter()
        .map(|p| expand_tilde(&p))
        .collect();

    let proxy_jump = params.proxy_jump.and_then(|pj| pj.into_iter().next());

    // TODO: ssh2-config does not expose IdentitiesOnly; revisit in Phase 2 auth chain
    let identities_only = false;

    Ok(ResolvedHostConfig {
        alias: alias.to_string(),
        hostname,
        port,
        user,
        identity_files,
        proxy_jump,
        identities_only,
    })
}

/// Resolve a host alias to its full connection parameters.
/// Uses the ssh2-config crate for correct multi-alias and inheritance handling.
/// For bulk use (multiple hosts), prefer loading config once with load_ssh_config()
/// and calling resolve_host_with_config() in a loop.
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
        let hosts = parse_ssh_config_content(content).unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].name, "home-linux");
        assert_eq!(hosts[0].hostname.as_deref(), Some("192.168.1.10"));
        assert_eq!(hosts[0].user.as_deref(), Some("alice"));
        assert_eq!(hosts[0].port, Some(22));
        assert_eq!(hosts[1].name, "work-windows");
        assert_eq!(hosts[1].hostname.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn test_skip_wildcards() {
        let content = "Host *\n    ServerAliveInterval 60\n\nHost ?\n    Foo bar\n";
        let hosts = parse_ssh_config_content(content).unwrap();
        assert!(hosts.is_empty());
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
        let hosts = parse_ssh_config_content(content).unwrap();
        assert_eq!(hosts.len(), 3);
        let bss = hosts.iter().find(|h| h.name == "bastion.bss-qa").unwrap();
        let slb = hosts.iter().find(|h| h.name == "slb225").unwrap();
        let web = hosts.iter().find(|h| h.name == "web1").unwrap();
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
        let hosts = parse_ssh_config_content(content).unwrap();
        assert_eq!(hosts[0].proxy_jump.as_deref(), Some("bastion"));
    }
}
