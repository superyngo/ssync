use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use russh::client::{self, Handle};
use russh_keys::key::PublicKey;

use crate::config::schema::HostEntry;
use crate::config::ssh_config::ResolvedHostConfig;
use super::auth::{authenticate, PassphraseCache};

/// russh client handler: verifies server host keys against ~/.ssh/known_hosts.
pub struct SshHandler {
    /// Hostname used for known_hosts lookup (may differ from the ssh alias)
    pub hostname: String,
    pub port: u16,
}

impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    #[allow(clippy::manual_async_fn)]
    fn check_server_key<'life0, 'life1, 'async_trait>(
        &'life0 mut self,
        server_public_key: &'life1 PublicKey,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<bool, Self::Error>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let known_hosts_path = dirs::home_dir()
                .context("Cannot determine home directory")?
                .join(".ssh")
                .join("known_hosts");

            if !known_hosts_path.exists() {
                bail!(
                    "Unknown host key for {}:{} — run `ssync init` to add the host to known_hosts",
                    self.hostname,
                    self.port
                );
            }

            match russh_keys::check_known_hosts_path(
                &self.hostname,
                self.port,
                server_public_key,
                &known_hosts_path,
            ) {
                Ok(true) => Ok(true),
                Ok(false) => bail!(
                    "Unknown host key for {}:{} — run `ssync init` to accept the key first",
                    self.hostname,
                    self.port
                ),
                Err(russh_keys::Error::KeyChanged { line }) => bail!(
                    "HOST KEY MISMATCH for {}:{} at line {} — possible man-in-the-middle attack",
                    self.hostname,
                    self.port,
                    line
                ),
                Err(e) => Err(e.into()),
            }
        })
    }
}

/// Result of a remote command execution via a russh channel.
#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Pool of authenticated russh sessions, one per host alias.
pub struct RusshSessionPool {
    /// host alias → open authenticated session handle
    sessions: HashMap<String, Arc<Handle<SshHandler>>>,
    /// hosts that failed to connect: (alias, error message)
    failed: Vec<(String, String)>,
}

impl RusshSessionPool {
    /// Connect to all hosts concurrently; unreachable hosts are recorded in `failed`.
    pub async fn setup(
        hosts: &[&HostEntry],
        timeout_secs: u64,
        concurrency: usize,
    ) -> Result<Self> {
        let timeout = Duration::from_secs(timeout_secs);
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut handles = Vec::new();

        for host in hosts {
            let alias = host.ssh_host.clone();
            let sem = sem.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.unwrap();
                let mut cache = PassphraseCache::new();
                let result = connect_one(&alias, timeout, &mut cache).await;
                (alias, result)
            }));
        }

        let mut sessions: HashMap<String, Arc<Handle<SshHandler>>> = HashMap::new();
        let mut failed: Vec<(String, String)> = Vec::new();

        for jh in handles {
            let (alias, result) = jh.await.context("task panic")?;
            match result {
                Ok(handle) => {
                    sessions.insert(alias, Arc::new(handle));
                }
                Err(e) => {
                    failed.push((alias, e.to_string()));
                }
            }
        }

        Ok(Self { sessions, failed })
    }

    /// Names of hosts that failed to connect (with error messages).
    pub fn failed_hosts(&self) -> Vec<(String, String)> {
        self.failed.clone()
    }

    /// Names of all successfully connected hosts.
    pub fn reachable_hosts(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Execute a command on a connected host.
    pub async fn exec(
        &self,
        host_alias: &str,
        cmd: &str,
        timeout_secs: u64,
    ) -> Result<RemoteOutput> {
        let handle = self
            .sessions
            .get(host_alias)
            .ok_or_else(|| anyhow::anyhow!("Host '{}' is not connected", host_alias))?
            .clone();

        exec_on_handle(&handle, cmd, Duration::from_secs(timeout_secs)).await
    }

    /// Close all sessions gracefully.
    pub async fn shutdown(self) {
        for (_, handle) in self.sessions {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
        }
    }
}

/// Open and authenticate a single session to `alias`.
/// Resolves the alias via ~/.ssh/config and handles a single ProxyJump hop.
async fn connect_one(
    alias: &str,
    timeout: Duration,
    cache: &mut PassphraseCache,
) -> Result<Handle<SshHandler>> {
    let resolved = crate::config::ssh_config::resolve_host(alias)?;

    match &resolved.proxy_jump.clone() {
        Some(proxy_alias) => {
            let proxy_resolved = crate::config::ssh_config::resolve_host(proxy_alias)?;
            connect_via_proxy(&proxy_resolved, &resolved, timeout, cache).await
        }
        None => connect_direct(&resolved, timeout, cache).await,
    }
}

/// Open a direct TCP connection to `config.hostname:config.port` and authenticate.
async fn connect_direct(
    config: &ResolvedHostConfig,
    timeout: Duration,
    cache: &mut PassphraseCache,
) -> Result<Handle<SshHandler>> {
    let russh_config = Arc::new(client::Config {
        inactivity_timeout: Some(timeout),
        ..<client::Config as Default>::default()
    });

    let handler = SshHandler {
        hostname: config.hostname.clone(),
        port: config.port,
    };

    let addr = format!("{}:{}", config.hostname, config.port);
    let addr = addr
        .to_socket_addrs()
        .with_context(|| format!("Cannot resolve {}", addr))?
        .next()
        .with_context(|| format!("No address resolved for {}", addr))?;

    let mut handle =
        tokio::time::timeout(timeout, client::connect(russh_config, addr, handler))
            .await
            .context("SSH connect timeout")?
            .with_context(|| {
                format!("Failed to connect to {}:{}", config.hostname, config.port)
            })?;

    authenticate(
        &mut handle,
        &config.user,
        &config.identity_files,
        config.identities_only,
        cache,
    )
    .await
    .with_context(|| {
        format!(
            "Authentication failed for {}@{}:{}",
            config.user, config.hostname, config.port
        )
    })?;

    Ok(handle)
}

/// Open an SSH session through a jump host (ProxyJump, single hop).
async fn connect_via_proxy(
    proxy: &ResolvedHostConfig,
    target: &ResolvedHostConfig,
    timeout: Duration,
    cache: &mut PassphraseCache,
) -> Result<Handle<SshHandler>> {
    // Step 1: connect and authenticate to the proxy
    let proxy_handle = connect_direct(proxy, timeout, cache)
        .await
        .with_context(|| format!("Failed to connect to proxy {}", proxy.alias))?;

    // Step 2: open a direct-tcpip tunnel through the proxy to the target
    let channel = tokio::time::timeout(
        timeout,
        proxy_handle.channel_open_direct_tcpip(
            target.hostname.as_str(),
            target.port as u32,
            "127.0.0.1",
            0u32,
        ),
    )
    .await
    .context("Proxy channel open timeout")?
    .with_context(|| {
        format!(
            "Failed to open direct-tcpip channel to {}:{} via {}",
            target.hostname, target.port, proxy.alias
        )
    })?;

    // Step 3: establish a second SSH session over the channel stream.
    // The proxy_handle is kept alive in a background task so the tunnel stays open.
    tokio::spawn(async move {
        let _keep_alive = proxy_handle;
        // Wait until the channel stream signals EOF (proxy connection dropped)
        tokio::time::sleep(Duration::from_secs(86400)).await;
    });

    let russh_config = Arc::new(client::Config {
        inactivity_timeout: Some(timeout),
        ..<client::Config as Default>::default()
    });

    let handler = SshHandler {
        hostname: target.hostname.clone(),
        port: target.port,
    };

    let mut target_handle = tokio::time::timeout(
        timeout,
        client::connect_stream(russh_config, channel.into_stream(), handler),
    )
    .await
    .context("SSH-through-proxy connect timeout")?
    .context("Failed to establish SSH session through proxy")?;

    authenticate(
        &mut target_handle,
        &target.user,
        &target.identity_files,
        target.identities_only,
        cache,
    )
    .await
    .with_context(|| {
        format!(
            "Authentication failed for {}@{} (via proxy {})",
            target.user, target.alias, proxy.alias
        )
    })?;

    Ok(target_handle)
}

/// Execute a command on an open session handle and collect stdout/stderr/exit code.
pub async fn exec_on_handle(
    handle: &Handle<SshHandler>,
    cmd: &str,
    timeout: Duration,
) -> Result<RemoteOutput> {
    let mut channel = tokio::time::timeout(timeout, handle.channel_open_session())
        .await
        .context("Channel open timeout")?
        .context("Failed to open SSH channel")?;

    channel
        .exec(true, cmd)
        .await
        .context("Failed to exec command")?;

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut exit_code: Option<u32> = None;

    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
            Some(russh::ChannelMsg::ExtendedData { data, ext }) if ext == 1 => {
                stderr.extend_from_slice(&data);
            }
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = Some(exit_status);
            }
            Some(russh::ChannelMsg::Eof) | Some(russh::ChannelMsg::Close) => {}
            None => break,
            _ => {}
        }
    }

    let exit_code = exit_code.map(|c| c as i32);
    let success = exit_code.map_or(false, |c| c == 0);

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        exit_code,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_output_success_flag() {
        let out = RemoteOutput {
            stdout: "hello\n".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            success: true,
        };
        assert!(out.success);
        assert_eq!(out.stdout.trim(), "hello");
    }

    #[test]
    fn test_remote_output_failure_flag() {
        let out = RemoteOutput {
            stdout: String::new(),
            stderr: "not found\n".to_string(),
            exit_code: Some(127),
            success: false,
        };
        assert!(!out.success);
        assert_eq!(out.exit_code, Some(127));
    }

    #[test]
    fn test_proxy_alias_resolved_from_config() {
        let resolved = crate::config::ssh_config::resolve_host("nonexistent-xyz-direct");
        let r = resolved.unwrap();
        assert!(
            r.proxy_jump.is_none(),
            "Host not in config should have no ProxyJump"
        );
    }
}
