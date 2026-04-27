use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use russh::client::Handle;
use russh_sftp::client::SftpSession;

use super::session_pool::SshHandler;
use crate::config::schema::ShellType;

/// Resolve a remote path, expanding a leading `~` using the provided home directory.
pub fn resolve_remote_path(remote: &str, home_dir: &str) -> String {
    if remote == "~" {
        home_dir.to_string()
    } else if let Some(rest) = remote.strip_prefix("~/") {
        format!("{}/{}", home_dir.trim_end_matches('/'), rest)
    } else {
        remote.to_string()
    }
}

/// Retrieve the remote home directory by running `echo $HOME` (sh) or equivalent.
pub async fn remote_home_dir(
    handle: &Handle<SshHandler>,
    shell: ShellType,
    timeout: Duration,
) -> Result<String> {
    let cmd = match shell {
        ShellType::Sh => "echo $HOME",
        ShellType::PowerShell => "Write-Output $env:USERPROFILE",
        ShellType::Cmd => "echo %USERPROFILE%",
    };

    let out = super::session_pool::exec_on_handle(handle, cmd, timeout).await?;
    Ok(out.stdout.trim().to_string())
}

/// Maximum file size for in-memory SFTP transfer (64 MB).
const MAX_SFTP_FILE_SIZE: u64 = 64 * 1024 * 1024;

/// Open an SFTP session on the given SSH handle.
/// Callers are responsible for wrapping this in a timeout.
async fn open_sftp(handle: &Handle<SshHandler>) -> Result<SftpSession> {
    let channel = handle
        .channel_open_session()
        .await
        .context("Failed to open SFTP channel")?;

    channel
        .request_subsystem(true, "sftp")
        .await
        .context("Failed to request SFTP subsystem")?;

    SftpSession::new(channel.into_stream())
        .await
        .context("Failed to create SFTP session")
}

/// Upload a local file to a remote path via SFTP.
/// The `remote_path` may start with `~` (expanded using `home_dir`).
pub async fn upload(
    handle: &Handle<SshHandler>,
    local_path: &Path,
    remote_path: &str,
    home_dir: &str,
    timeout: Duration,
) -> Result<()> {
    tokio::time::timeout(timeout, async {
        let resolved = resolve_remote_path(remote_path, home_dir);
        let metadata = tokio::fs::metadata(local_path)
            .await
            .with_context(|| format!("Cannot stat {}", local_path.display()))?;
        if metadata.len() > MAX_SFTP_FILE_SIZE {
            anyhow::bail!(
                "File {} is too large for SFTP transfer ({} > {} bytes). Chunked transfer not yet implemented.",
                local_path.display(),
                metadata.len(),
                MAX_SFTP_FILE_SIZE
            );
        }
        let sftp = open_sftp(handle).await?;
        let local_data = tokio::fs::read(local_path)
            .await
            .with_context(|| format!("Failed to read {}", local_path.display()))?;
        if let Some(parent) = std::path::Path::new(&resolved).parent() {
            if parent != std::path::Path::new("") {
                mkdir_p_sftp(&sftp, parent).await?;
            }
        }
        sftp.write(&resolved, &local_data)
            .await
            .with_context(|| format!("SFTP upload failed for {}", resolved))?;
        Ok(())
    })
    .await
    .context("SFTP upload timed out")?
}

/// Download a remote file to a local path via SFTP.
/// The `remote_path` may start with `~` (expanded using `home_dir`).
pub async fn download(
    handle: &Handle<SshHandler>,
    remote_path: &str,
    local_path: &Path,
    home_dir: &str,
    timeout: Duration,
) -> Result<()> {
    tokio::time::timeout(timeout, async {
        let resolved = resolve_remote_path(remote_path, home_dir);
        let sftp = open_sftp(handle).await?;
        let data = sftp
            .read(&resolved)
            .await
            .with_context(|| format!("SFTP download failed for {}", resolved))?;
        if data.len() as u64 > MAX_SFTP_FILE_SIZE {
            anyhow::bail!(
                "Remote file {} is too large ({} > {} bytes). Chunked transfer not yet implemented.",
                resolved,
                data.len(),
                MAX_SFTP_FILE_SIZE
            );
        }
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(local_path, &data)
            .await
            .with_context(|| format!("Failed to write {}", local_path.display()))?;
        Ok(())
    })
    .await
    .context("SFTP download timed out")?
}

/// Recursively create directories on the remote (best-effort; ignores already-exists errors).
async fn mkdir_p_sftp(sftp: &SftpSession, path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();
    let parts: Vec<&str> = path_str.split('/').filter(|s| !s.is_empty()).collect();

    let mut current = if path_str.starts_with('/') {
        "/".to_string()
    } else {
        String::new()
    };

    for part in &parts {
        if !current.is_empty() && !current.ends_with('/') {
            current.push('/');
        }
        current.push_str(part);
        let _ = sftp.create_dir(&current).await; // ignore error if already exists
    }
    Ok(())
}

/// SFTP probe: attempt to write and delete a sentinel file.
/// Returns Ok(()) if SFTP is available, Err otherwise.
pub async fn sftp_probe(
    handle: &Handle<SshHandler>,
    home_dir: &str,
    timeout: Duration,
) -> Result<()> {
    tokio::time::timeout(timeout, async {
        let probe_path = format!("{}/.ssync_probe", home_dir);
        let sftp = open_sftp(handle).await?;
        sftp.write(&probe_path, b"0")
            .await
            .context("SFTP probe write failed")?;
        if let Err(e) = sftp.remove_file(&probe_path).await {
            tracing::debug!("SFTP probe cleanup failed for {}: {}", probe_path, e);
        }
        Ok(())
    })
    .await
    .context("SFTP probe timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path_expands_tilde() {
        assert_eq!(
            resolve_remote_path("~/.config/app.toml", "/home/alice"),
            "/home/alice/.config/app.toml"
        );
    }

    #[test]
    fn test_resolve_path_no_tilde() {
        assert_eq!(
            resolve_remote_path("/etc/hosts", "/home/alice"),
            "/etc/hosts"
        );
    }

    #[test]
    fn test_resolve_path_tilde_only() {
        assert_eq!(resolve_remote_path("~", "/home/alice"), "/home/alice");
    }
}
