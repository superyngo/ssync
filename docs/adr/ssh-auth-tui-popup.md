# ADR: SSH Auth TUI Popup

**Status:** Accepted  
**Date:** 2026-05-06  
**Relates to:** §18.1, §19 Phase 7 item 8 of `docs/tui_reconstruct_plan.md`

---

## Context

`ssync` authenticates to remote hosts via `src/host/auth.rs::authenticate()`, which currently
calls `rpassword::prompt_password()` (or equivalent) directly — a blocking terminal prompt that
is incompatible with the ratatui TUI event loop.  Auth scenarios that require interactive input:

1. **Encrypted private key** — passphrase prompt (per identity file, cached in `PassphraseCache`)
2. **Password fallback** — when `identities_only = false`
3. **Keyboard-interactive** — not yet wired but covered by this design

The russh `ClientHandler` trait (`impl client::Handler for SshHandler`,
`src/host/session_pool.rs:21-76`) currently only implements `check_server_key`. No auth callbacks
(`keyboard_interactive`, `auth_banner`) are hooked.  The `SshAuthRequired` `TuiEvent` variant
(`src/tui/async_bridge.rs`) is reserved but not yet added to the enum.

---

## Decision

### (a) Russh callback hooks used

| Auth type | Hook point | Notes |
|-----------|-----------|-------|
| Passphrase (encrypted key) | `authenticate()` in `src/host/auth.rs` before calling `russh_keys::load_secret_key` | russh doesn't own this; we intercept before passing key to russh |
| Password fallback | `authenticate()` before `handle.authenticate_password()` | Same pattern |
| Keyboard-interactive | Add `keyboard_interactive_request()` to `impl client::Handler for SshHandler` | russh calls this when server sends SSH_MSG_USERAUTH_INFO_REQUEST |

All three prompt sources converge on the **same oneshot handshake** described below.

### (b) Tokio oneshot channel handshake

```
auth task (russh thread)                    TUI main loop
─────────────────────────────               ──────────────────────────────
let (tx, rx) = oneshot::channel();
tui_sender.send(TuiEvent::SshAuthRequired {
    host, prompt, responder: tx,            ← mpsc to TUI
}).await?;
let credential = rx.await?;                 block on oneshot reply

                                            handle_tui_event(SshAuthRequired)
                                              → set app.auth_popup = Some(AuthPopup { prompt, tx })
                                              → redraw (masked InputField)
                                            handle_key (popup active)
                                              → on Enter: tx.send(credential_string)
                                              → clear app.auth_popup
                                              → zeroize local buffer
```

`TuiEvent::SshAuthRequired` carries a `tokio::sync::oneshot::Sender<String>` (`responder`).
The TUI stores it in `AppState::auth_popup: Option<AuthPopup>`. On Enter the sender is consumed
and the value forwarded; on Esc (cancel) the sender is dropped, causing `rx.await` to return
`Err(RecvError)`, which `authenticate()` maps to `anyhow::Error` → auth failure.

### (c) Timeout and cancellation

- The auth task already receives a `CancellationToken` from the pool (`src/host/session_pool.rs`).
- Wrap the `rx.await` with `tokio::select!`:

```rust
tokio::select! {
    result = rx => result.map_err(|_| anyhow!("auth cancelled")),
    _ = cancel_token.cancelled() => Err(anyhow!("operation cancelled")),
    _ = tokio::time::sleep(AUTH_POPUP_TIMEOUT) => {
        Err(anyhow!("auth prompt timed out"))
    }
}
```

- Default `AUTH_POPUP_TIMEOUT`: **120 seconds** (configurable via const in `src/host/auth.rs`).
- On timeout or cancellation: the sender is dropped (or the token fires), `rx` resolves to `Err`,
  and the host is marked failed in the progress report. The TUI clears the popup and logs the
  timeout reason to the log overlay.

### (d) Credential lifetime in memory

- The `String` credential lives only on the stack inside `authenticate()` and the `oneshot`
  channel buffer. It is **never cloned** beyond what russh requires internally.
- After `handle.authenticate_password(user, &credential)` or `load_secret_key(..., Some(&credential))`
  returns, immediately call `zeroize::Zeroize::zeroize(&mut credential)` (add `zeroize` crate,
  implement via `ZeroizeOnDrop` newtype wrapper).
- The `AuthPopup` input buffer in the TUI (`InputField`) is zeroized when the popup is dismissed
  (both on Enter and on Esc).
- Credentials are **never written to logs**, never stored in `PassphraseCache` beyond the session,
  and never persisted to disk.
- `PassphraseCache` (`HashMap<PathBuf, String>`) — existing type in `src/host/auth.rs` — should
  wrap its values in a `ZeroizeOnDrop` newtype so all cached passphrases are wiped when the cache
  is dropped at session end.

---

## Consequences

- `TuiEvent` gains a `SshAuthRequired` variant with an embedded `oneshot::Sender<String>`; this
  makes the enum non-`Clone` (acceptable — it is already consumed via `mpsc`).
- `AppState` gains `auth_popup: Option<AuthPopup>` which takes highest key-routing priority in
  `handle_key()` (above filter popup, below Ctrl+C quit).
- `impl client::Handler for SshHandler` gains `keyboard_interactive_request()` for SSH KI auth.
- New dependency: `zeroize` (already common in the Rust crypto ecosystem).
- The non-TUI (CLI) code path in `auth.rs` is unchanged; the oneshot path is only activated when
  a `tui_sender` is provided.

---

## Files to change

| File | Change |
|------|--------|
| `src/tui/async_bridge.rs` | Add `SshAuthRequired { host, prompt, responder }` to `TuiEvent` |
| `src/tui/app.rs` | Handle `SshAuthRequired` in `handle_tui_event`; add popup intercept in `handle_key` |
| `src/host/auth.rs` | Replace `rpassword` calls with oneshot sender; add zeroize; `AUTH_POPUP_TIMEOUT` |
| `src/host/session_pool.rs` | Add `keyboard_interactive_request` to `impl client::Handler for SshHandler` |
| `Cargo.toml` | Add `zeroize` dependency |
