use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use russh::client::Handle;
use russh_keys::key::KeyPair;

use super::session_pool::SshHandler;

/// Per-process passphrase cache: key_path → passphrase.
/// Avoids re-prompting for the same key file within a single run.
pub type PassphraseCache = HashMap<PathBuf, String>;

/// Attempt to authenticate `handle` as `user`.
///
/// Auth chain:
/// 1. For each identity file: try without passphrase (unencrypted keys)
/// 2. For each identity file: if step 1 failed, prompt for passphrase (cached per path)
/// 3. Prompt for password if `identities_only` is false
pub async fn authenticate(
    handle: &mut Handle<SshHandler>,
    user: &str,
    identity_files: &[PathBuf],
    identities_only: bool,
    cache: &mut PassphraseCache,
) -> Result<()> {
    // Step 1: try each identity file without passphrase (handles unencrypted keys)
    for path in identity_files {
        if try_pubkey(handle, user, path, None).await? {
            return Ok(());
        }
    }

    // Step 2: retry each identity file with passphrase prompt (handles encrypted keys)
    for path in identity_files {
        let passphrase = match cache.get(path) {
            Some(pp) => pp.clone(),
            None => {
                let prompt = format!("Enter passphrase for {}: ", path.display());
                let pp = rpassword::prompt_password(&prompt)
                    .context("Failed to read passphrase")?;
                cache.insert(path.clone(), pp.clone());
                pp
            }
        };
        if try_pubkey(handle, user, path, Some(&passphrase)).await? {
            return Ok(());
        }
    }

    // Step 3: password fallback (only if IdentitiesOnly is not set)
    if !identities_only {
        let prompt = format!("{}@<host> password: ", user);
        let password = rpassword::prompt_password(&prompt)
            .context("Failed to read password")?;
        if handle
            .authenticate_password(user, &password)
            .await
            .context("Password authentication failed")?
        {
            return Ok(());
        }
    }

    anyhow::bail!("All authentication methods exhausted for user '{}'", user)
}

/// Try public-key auth with an optional passphrase. Returns true if auth succeeded.
async fn try_pubkey(
    handle: &mut Handle<SshHandler>,
    user: &str,
    key_path: &Path,
    passphrase: Option<&str>,
) -> Result<bool> {
    let key_pair: KeyPair = match passphrase {
        Some(pp) if !pp.is_empty() => {
            match russh_keys::load_secret_key(key_path, Some(pp)) {
                Ok(kp) => kp,
                Err(_) => return Ok(false),
            }
        }
        _ => {
            match russh_keys::load_secret_key(key_path, None) {
                Ok(kp) => kp,
                Err(_) => return Ok(false), // encrypted key; will retry with passphrase in step 2
            }
        }
    };

    let authed = handle
        .authenticate_publickey(user, Arc::new(key_pair))
        .await
        .context("Public key authentication error")?;
    Ok(authed)
}
