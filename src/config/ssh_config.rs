
use anyhow::{Context, Result};

/// A parsed SSH host entry from ~/.ssh/config.
#[derive(Debug, Clone)]
pub struct SshHostEntry {
    pub name: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
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

fn parse_ssh_config_content(content: &str) -> Result<Vec<SshHostEntry>> {
    let mut hosts = Vec::new();
    let mut current: Option<SshHostEntry> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split key/value (supports both "Key Value" and "Key=Value")
        let (key, value) = if let Some(eq_pos) = line.find('=') {
            let k = line[..eq_pos].trim();
            let v = line[eq_pos + 1..].trim();
            (k, v)
        } else if let Some(space_pos) = line.find(char::is_whitespace) {
            let k = line[..space_pos].trim();
            let v = line[space_pos..].trim();
            (k, v)
        } else {
            continue;
        };

        match key.to_lowercase().as_str() {
            "host" => {
                // Save previous host
                if let Some(h) = current.take() {
                    if !is_wildcard(&h.name) {
                        hosts.push(h);
                    }
                }
                // Start new host (skip wildcard patterns)
                let name = value.to_string();
                current = Some(SshHostEntry {
                    name,
                    hostname: None,
                    user: None,
                    port: None,
                    identity_file: None,
                });
            }
            "hostname" => {
                if let Some(ref mut h) = current {
                    h.hostname = Some(value.to_string());
                }
            }
            "user" => {
                if let Some(ref mut h) = current {
                    h.user = Some(value.to_string());
                }
            }
            "port" => {
                if let Some(ref mut h) = current {
                    h.port = value.parse().ok();
                }
            }
            "identityfile" => {
                if let Some(ref mut h) = current {
                    h.identity_file = Some(value.to_string());
                }
            }
            // Skip Match, Include, and other complex directives
            _ => {}
        }
    }

    // Don't forget the last host
    if let Some(h) = current {
        if !is_wildcard(&h.name) {
            hosts.push(h);
        }
    }

    Ok(hosts)
}

fn is_wildcard(name: &str) -> bool {
    name.contains('*') || name.contains('?')
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
}
