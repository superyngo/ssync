# TUI Design Plan: Config / Operate / Checkout

**Version:** v6.0

| Version | Changes |
|---------|---------|
| v5.8 | AD-16 config_hash collision risk note; AD-17 two-binary tradeoff note; AD-18 index-shift limitation; §2.1 defer config_state.rs + viewport/scrollable_list unification note; §4 accessibility note; §6.4 Context field audit requirement; §7.7 external config hot-reload expectation; §7.9 saving-flag race guard + Windows signal deferred to post-MVP; §8.3 Focusable simplification note; §8.6 ← → L1 vs L2 clarification; §14.3 Ctrl+D display spec; §15.4 TOML round-trip validation + Phase 0.5 scope tightening; §16.2 step 5 MVP skip guard; §16.3 drop tab-switch debounce; §18.1 bounded channel (10K) replaces unbounded; §18.3 db_stale lazy reopen; §19 Phase 0 merge tracing steps; §19 Phase 0.5 CLI regression test; §19 Phase 0.7 Context audit gate; §19 Phase 1a Windows signal deferred; §19 Phase 3 concurrency + Checkout reload tests; §19 Phase 7 SSH auth hard gate |
| v6.0 | AD-18 add `id` field recommendation; AD-20 viewport/scrollable_list unification promoted from note to AD; §2.1 reference AD-20; §7.9 remove unnecessary `saving: bool` race guard; §14.2 add `r` key to Checkout; §15.4 add toml_edit parse-fail fallback path + write-back scope matrix table; §17.2 `LogEntry.when` changed from `Instant` to `SystemTime`; §18.1 channel capacity 10K→1K; §18.2 define `OperationCancelled` partial-results UI; §18.4 slice-based rendering recommendation; §21.A add tmux resize test; §22 add yank + single-binary evaluation backlog items; Phase 0 add rollback strategy + `feat/tui` CI pipeline; Phase 0.5 add BOM stripping + `dirs` dep verification + `id` field; Phase 0.7 reduce hard gate from committed document to PR description; Phase 0.8 renamed to Phase 8; Phase 3 add sleep/wake manual validation |
**Status:** Implementation-ready

This document is the single source of truth. Earlier amendment chains
(v2.1 / v2.2 / v2.3 / v3 / v4 with adversarial-review subsections) have
been folded into the body or dropped where superseded. If you find this
document contradicting itself, the contradiction is a bug — file an
issue rather than picking a side.

---

## 0. Architecture Decisions (Canonical)

These are the cross-cutting decisions that earlier drafts contradicted.
All later sections defer to this table.

| # | Decision | Value |
|---|----------|-------|
| AD-1 | `tui` cargo feature default | **NOT in `[features].default`** for source builds. `cargo build` is headless. |
| AD-2 | Release binaries | Two: `ssync` (headless, no `tui`) and `ssync-tui` (with `--features tui`). End-users installing from GitHub Releases get TUI by default via `ssync-tui`. |
| AD-3 | Entry point when run without subcommand | Three exit paths: (a) `tui` feature missing → stderr error + **exit 1**; (b) feature present but stdin/stdout not a TTY (or `TERM=dumb`) → print help + **exit 2** (clap convention); (c) feature present + TTY → launch TUI. See §4. |
| AD-4 | New runtime dependencies | **`toml_edit = "0.22"`** (format-preserving config write-back; always-on). **`tempfile`** (cross-platform atomic config / state write; **already declared as a direct dependency in `Cargo.toml`** — no new entry needed). **`tokio-util` with `features = ["sync"]`** (provides `CancellationToken` used in §18.2; tui-feature-gated). **`tracing-subscriber` must declare `features = ["env-filter", "reload"]`** (the `reload` feature is required for AD-15 fmt-layer writer swap — the current `Cargo.toml` only has `"env-filter"`). **No subprocess transport, no `tracing-appender`** — see AD-13/AD-15. (`tracing-appender` appears in `Cargo.toml` but has **no references in `src/`**; Phase 0 removes it — see §19 Phase 0 step 5.) (Earlier text claiming "no new dependencies needed" is wrong and is corrected here.) |
| AD-5 | DB ownership | `rusqlite::Connection` lives **only on the main thread** inside `App.db`. Spawned operation tasks **open their own connection** via `state::db::open(state_dir_override)?` and write through it; SQLite WAL mode handles concurrency. |
| AD-6 | Config ownership | `App.config: AppConfig` is the canonical mutable copy. Spawned tasks receive a **`Clone`** of the relevant subset (`AppConfig` or `Vec<HostEntry>`) snapshot at spawn time; mid-flight Config tab edits never affect a running op. |
| AD-7 | Persisted cursor positions | **NOT persisted.** Cursors reset to top on startup. Only user *intent* (active tab, filter, op flags) is persisted. |
| AD-8 | Persistence wiring phase | Schema struct lands in **Phase 1b**; full save-on-quit + load-on-startup wired in **Phase 2**. (Earlier "moved to Phase 5" guidance is dropped.) |
| AD-9 | Terminal restoration on panic | **Mandatory `TerminalGuard` Drop impl + `panic::set_hook`** installed before first render. Non-negotiable: a panicked TUI must leave a usable terminal. |
| AD-10 | stderr / per-step output capture | **Operations emit structured `TuiEvent`s from inside the russh-driven collector** (see AD-13). The TUI does NOT spawn subprocesses, does NOT do fd-level `dup2`, and does NOT redirect process stderr at all. Internal Rust logs are captured via an in-memory `tracing` layer (AD-15). |
| AD-11 | MVP scope | Phases 0–3 = Checkout tab (latest snapshots + filter only) + Persistence + `check` operation. **Adaptive arrow navigation IS in MVP** (Phase 1b) because every downstream zone depends on it; Config write-back, log overlay, SSH auth popup, history/since/export panels are explicitly **post-MVP**. Config read-only browsing + external editor land in **Phase 4** (first post-MVP phase) to validate terminal suspend/resume before form-editing complexity arrives. |
| AD-12 | `persist_commands` config flag | **Dropped from v5.** `run_command` / `exec_script` / `sync_source` are never persisted. Privacy concern outweighs convenience. |
| AD-13 | Operation runtime | **Reuse the existing russh-based `host::pool::SshPool` and per-op collector building blocks.** The TUI is a UI on top of the same async runtime that CLI commands use; no second transport. AsyncBridge converts collector progress callbacks (or a new lightweight event channel embedded in the collector) into `TuiEvent`s on a `tokio::mpsc::channel`. (Resolves the prior `Stdio::piped` / `kill_on_drop` confusion.) |
| AD-14 | Command-core extraction precedes TUI | **MVP only extracts `check_core`** in Phase 0.7. `checkout` read helpers are made `pub(crate)` in the same phase. `run_core`, `exec_core`, `sync_core`, and `checkout_core` are extracted in their own phases (Phase 5, 5, 6, and Phase 4 respectively) so the CLI regression surface stays small. Without `check_core`, Phase 3 cannot land. |
| AD-15 | Tracing initialization | The existing `init_tracing()` global subscriber **stays**. The TUI does NOT re-init or redirect the global subscriber. Instead, when the `tui` feature is enabled and TUI is launched, the tracing setup is changed to a `Registry` with two layers: (1) the existing fmt/env-filter writing to stderr (active only outside TUI), and (2) an in-memory ring-buffer layer that pushes into a shared `Arc<Mutex<VecDeque<LogEntry>>>` the App reads. While the TUI is running, the fmt layer is dropped to a no-op writer (`std::io::sink`) via a `reload::Layer` handle so that nothing prints to the raw terminal. |
| AD-16 | TUI state file path | The state file lives at `{resolved_state_dir}/tui_state-{config_hash}.toml`, where `resolved_state_dir` is the **same root as the SQLite DB** (honors `config.settings.state_dir` override via a new `state::db::resolved_state_dir(override)` helper — see §16) and `config_hash` is the first 8 hex chars of `blake3(effective_path_str)`. **blake3 is used here because it is already a direct dependency of ssync (via the `sync` command's file-hashing path); no new hasher is introduced.** **`effective_path_str` is derived by calling `config::app::resolve_path(custom_path)` first**, then canonicalizing the result if the file exists. See §16 for the full fallback chain. **Note:** `resolve_path` currently does NOT expand `~` or normalize relative paths — Phase 0.5 must enhance it with tilde expansion (`dirs::home_dir()` substitution) so that `None`, `~/.config/ssync/config.toml`, and an explicit equivalent path all land on the same hash. `canonicalize()` is **never** a hard precondition for startup. Atomic write uses `tempfile::NamedTempFile::persist()` in the same directory — see §15.4 for the cross-platform rationale. **Collision risk:** the 8-hex-char (32-bit) hash gives a collision probability of ~2⁻¹⁶ at 65K distinct config paths. For a single-user tool this is an accepted risk; a collision causes two config paths to share the same state file (last-write-wins), not data corruption. |
| AD-17 | `ssync-tui` binary realization | `Cargo.toml` declares **two `[[bin]]` targets**: `ssync` (always built, no TUI) and `ssync-tui` (`required-features = ["tui"]`). Both share `src/main.rs`; the binary name is read at runtime to decide whether the no-subcommand fallback is even reachable. Release CI runs `cargo build --release --bin ssync` and `cargo build --release --bin ssync-tui --features tui` and uploads both artifacts. (Resolves the prior "AD-2 was a slogan" gap.) **Tradeoff note:** the two-binary model doubles the release artifact count and CI matrix. An alternative is a single binary with runtime feature detection — the linking cost of `ratatui`/`crossterm` is only ~1–2 MB. However, the two-binary approach is kept for principle-of-least-surprise: the binary name declares intent, and symlink aliasing cannot accidentally expose TUI to headless users. |
| AD-18 | Config entry stable identity | Phase 0.5 adds **two** new fields to `CheckEntry` and `SyncEntry` only (`HostEntry.name` is already a required `String` and is unchanged): (1) **`name: Option<String>`** — pure display label for the TUI sidebar and Applicable Entries panel, never used as a lookup key; and (2) **`id: String`** — a short random 8-hex-char identifier (e.g. `"a3f7c2d1"`) generated once on entry creation. Both are `#[serde(default)]`, so existing configs load fine (missing `id` → empty string). **Persistence uses `id` when non-empty, falls back to vec index when empty.** Implications: (a) edit/delete operations in the TUI reference entries by `id` when available, index otherwise; (b) `name` need not be unique and may be empty; (c) persistence stores the active entry by `id`; (d) if `id` is empty (legacy config) and the persisted index is out-of-range, the value is silently dropped to `None`. **Index-shift limitation (legacy configs only):** if a legacy entry (empty `id`) at position N is removed externally, all persisted indices > N shift down by 1 and may point to wrong entries. Once an entry gains an `id` (on first TUI save), this limitation no longer applies to that entry. **`id` generation:** `blake3(name_bytes + current_unix_timestamp_nanos)[..4].to_hex()` — BLAKE3 is already a direct dependency; this avoids introducing a UUID library. Collisions are negligible in a single-user config. **`name` display rule:** truncated to fit the available sidebar column width using `unicode_width::UnicodeWidthStr::width()` (not `str::len()`) to correctly handle CJK and other wide characters; the maximum is the sidebar column width minus padding, with an absolute cap of 40 display columns. When `None` or empty string, falls back to `"{Type} #{index}"` (e.g. `"Check #1"`, `"Sync #2"`). Control characters and newlines are stripped before display. |
| AD-19 | Concurrent instance handling | Running two `ssync-tui` processes against the same config file simultaneously is **a known limitation in MVP**: both write to `tui_state-{config_hash}.toml` with last-write-wins semantics, risking state loss. No file lock or pidfile is implemented. This is acceptable for MVP because the TUI is a single-user interactive tool and simultaneous instances are uncommon. **Document** the limitation in `README.md` ("Running multiple ssync-tui instances with the same config simultaneously is not supported"). Any future feature that requires exclusive state access must add a pidfile or advisory `flock(2)` lock under a separate ADR. |
| AD-20 | `viewport.rs` / `scrollable_list.rs` unification | Phase 1a implements a **single `viewport.rs`** that encapsulates `scroll_y`, `selected`, `visible_height`, and the scroll invariant (`scroll_y ≤ selected ≤ scroll_y + visible_height − 1`). A separate `scrollable_list.rs` is introduced only if a genuinely distinct use case emerges that cannot share the same struct. **`visible_range()`** is a required method on `viewport.rs` that returns `(scroll_y, scroll_y + visible_height)` — rendering code iterates only this slice, giving O(visible) render cost regardless of list length. If both files are still present at Phase 1b review, consolidate before Phase 2. This is an enforceable architectural decision, not an advisory note. |

---

## 1. Context & Goals

ssync is a CLI-based SSH remote management tool. Today users interact
exclusively through subcommand flags. A TUI provides an interactive,
visual workflow for the three core activities: configuring the tool
(Config), executing operations (Operate), and reviewing checkout data
(Checkout).

The codebase already carries `ratatui` and `crossterm` as optional
dependencies. A dormant `run_tui()` function exists in
`commands/checkout.rs` (single-loop PoC, no state, no tabs) and is
**deleted** as part of Phase 1a — it is not the basis for the new TUI.
The current `Cli.command` is a required field; Phase 1a flips it to
`Option<Commands>` and adds the no-subcommand dispatch (§5). All
existing subcommands (`ssync check`, `ssync run`, etc.) continue to
work unchanged.

---

## 2. Architecture Overview

### 2.1 New files

```
src/tui/
  mod.rs                  -- crate module root; re-exports `entry::run_or_fallback`
  entry.rs                -- pub async fn run_or_fallback(cfg, cfg_path) — performs §4 TTY / TERM detection, launches TUI or prints help + exits 2
  terminal.rs             -- TerminalGuard, panic hook, suspend/resume helpers
  app.rs                  -- App struct, main event loop, render dispatch
  event.rs                -- Crossterm event polling + async event channel
  async_bridge.rs         -- tokio::mpsc channel + CancellationToken for op tasks
  tabs/
    mod.rs                -- Tab trait, TabId enum
    config_tab.rs         -- Config browsing + editing
    operate_tab.rs        -- Operation execution (check/run/exec/sync)
    checkout_tab.rs       -- Snapshot data viewer
  components/
    mod.rs
    tab_bar.rs
    popup.rs              -- Modal overlay system (centered + Clear)
    input_field.rs        -- Text input with cursor + InputMode isolation
    target_filter.rs      -- Shared target filter popup
    status_bar.rs         -- Bottom keybinding hints + breadcrumb trail
    scrollable_list.rs    -- Generic selectable list with cursor/scroll decoupling
    viewport.rs           -- scroll_y + cursor + PgUp/Dn/Home/End
    breadcrumb.rs         -- Hierarchy path indicator widget
  state/
    mod.rs
    operate_state.rs
    checkout_state.rs
    config_state.rs       -- NOTE: file is not created until Phase 7 (no content until then)
    persist.rs            -- TuiPersistedState: serialize/deserialize
  theme.rs                -- Canonical color palette (see §10)

src/commands/
  report.rs               -- ProgressSink trait, HostStatus, CommandReport enum (Phase 0.7)
```

**`scrollable_list.rs` / `viewport.rs` unification:** See AD-20. Implement a single `viewport.rs` with `visible_range()`. Only introduce `scrollable_list.rs` if a genuinely distinct use case emerges.

| File | Change |
|------|--------|
| `src/cli.rs` | `command: Commands` → `command: Option<Commands>`; add `subcommand_required = false` |
| `src/main.rs` | Add `#[cfg(feature = "tui")] mod tui;` and dispatch (§5); detect binary name (`argv[0]`) for AD-17 fallback gating; rewire `init_tracing` to insert the ring-buffer layer (AD-15) |
| `src/commands/mod.rs` | Add `Context::from_tui_parts(...)` constructor (§6.4); state-dir resolution helper exposed (AD-16) |
| `src/commands/{check,run,exec,sync,checkout}.rs` | **Phase 0.7 refactor (MVP scope)**: extract printer-free `check_core` returning `CommandReport::Check(...)`; make `checkout` read helpers `pub(crate)`. `run_core`, `exec_core`, `sync_core` are extracted in Phases 5/5/6 respectively; `checkout_core` in Phase 4. Existing `run()` wrappers are untouched until each core is extracted. |
| `src/commands/checkout.rs` | **Phase 0.7:** make `fetch_latest_snapshots`, `DisplayColumns`, `extract_*` `pub(crate)` (already covered by the combined row above — listed here for clarity). **Phase 1a:** delete the dead `run_tui()` PoC. |
| `src/config/schema.rs` | Add `name: Option<String>` (pure display label; see AD-18) and `id: String` (stable entry identity; see AD-18) to `CheckEntry` and `SyncEntry` (`#[serde(default)]`) |
| `src/config/app.rs` | Update `save()` to use `toml_edit::DocumentMut` for the **edit path** while preserving the existing `inject_config_comments` behavior on first-create (§15.4) |
| `Cargo.toml` | Add `toml_edit = "0.22"` (always-on, not feature-gated; cheap dep). Add second `[[bin]]` for `ssync-tui` with `required-features = ["tui"]` (AD-17). |

### 2.3 Cargo.toml feature & bin configuration (AD-1, AD-17)

```toml
[features]
default = []                         # AD-1: source build is headless
tui     = ["ratatui", "crossterm"]

[[bin]]
name = "ssync"
path = "src/main.rs"

[[bin]]
name = "ssync-tui"
path = "src/main.rs"
required-features = ["tui"]
```

Build matrix:

| Command | Produces | Behavior of no-subcommand invocation |
|---------|----------|--------------------------------------|
| `cargo build --bin ssync` | `target/debug/ssync` | stderr: `"Interactive TUI not available. Use the ssync-tui binary."` + **exit 1** (AD-17 binary-name gate; TUI is unreachable from this binary regardless of compiled feature flag). |
| `cargo build --bin ssync-tui --features tui` | `target/debug/ssync-tui` | If TTY: launches TUI (exit 0 on clean quit). If non-TTY (pipe / `TERM=dumb`): clap help + **exit 2**. |
| `cargo build` (no `--bin`) | Both `ssync` and `ssync-tui` (the latter only if `--features tui` is also passed). | Behavior per the binary actually invoked. |

Why a runtime binary-name check? Without it, a user who built
`ssync-tui` and then symlinked it as `ssync` would get an
unexpected TUI on what is supposed to be the headless name. The
binary-name gate makes the contract explicit: only `ssync-tui` is
ever allowed to enter alternate screen.

Release CI runs both `cargo build --release --bin ssync` and
`cargo build --release --bin ssync-tui --features tui` and uploads
both artifacts as named.

---

## 3. Feature Flag & Build Strategy

The `tui` feature gates *all* TUI source code via
`#[cfg(feature = "tui")]` so a headless build links no TUI symbols.
Specifically:

- `mod tui;` in `main.rs` is feature-gated.
- `ratatui` and `crossterm` are `optional = true` in `Cargo.toml` and
  pulled in only by the `tui` feature.
- `toml_edit` is **always on** (small dep, used by `config::app::save`).

---

## 4. Non-Interactive / TTY Behavior

When the binary is invoked **with no subcommand**, the entry-point
logic is (AD-3, AD-17):

```text
binary_name = argv[0].file_stem()                   # "ssync" or "ssync-tui"

if binary_name != "ssync-tui" OR not built with `tui` feature:
    eprintln!("Interactive TUI not available. Use the `ssync-tui` binary.")
    exit code 1

elif not stdout.is_terminal() OR not stdin.is_terminal():
    # Pipe / CI / cron / redirected
    print short help (clap auto)
    exit code 2

elif TERM env var is unset OR equals "dumb" (Unix only):
    eprintln!("Terminal does not support TUI (TERM=dumb or unset).")
    print short help
    exit code 2

else:
    launch TUI
```

**Windows note:** The `TERM` env-var check is Unix-only (Windows has no `TERM` convention and ConPTY does not set it). On Windows the detection reduces to: binary-name gate → `IsTerminal` check → launch TUI. No `TERM` check is performed on Windows.

Exit codes are stable contract:
- **0** — clean TUI quit (`q` / Ctrl+C from inside TUI).
- **1** — TUI requested but not available (wrong binary, missing feature, init failure).
- **2** — non-TTY environment, help printed (clap convention).

Implementation: use `std::io::IsTerminal` (stable since 1.70) on stdin
and stdout. Do NOT check stderr — stderr being redirected is fine and
common during interactive use.

Detection happens **before** any terminal-mode changes (raw mode,
alternate screen). This keeps non-TTY invocations from corrupting the
calling shell.

`ssync -c custom.toml` (no subcommand, `-c` only) follows the same
rules. All explicit subcommands (`ssync check --all`, etc.) bypass
this detection entirely and run unchanged.

**Accessibility note:** The TUI renders in alternate-screen ANSI mode,
which screen-reader software cannot interpret. Users relying on
screen readers should use the `ssync` CLI subcommands directly; all
TUI operations are equally available via the command line. The `ssync`
binary always works in non-TTY and pipe mode (exit 1 for no-subcommand,
normal subcommand behavior otherwise).

---

## 5. CLI Integration

`src/cli.rs`:

```rust
#[derive(Parser)]
#[command(
    name = "ssync",
    version,
    about = "SSH-config-based cross-platform remote management tool",
    subcommand_required = false,
)]
pub struct Cli {
    #[arg(short = 'v', long)]
    pub verbose: bool,

    #[arg(short = 'c', long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}
```

`src/main.rs` (AD-3, AD-17):

```rust
#[cfg(feature = "tui")]
mod tui;

fn binary_is_ssync_tui() -> bool {
    std::env::args_os()
        .next()
        .and_then(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_owned()))
        .map(|s| s == "ssync-tui")
        .unwrap_or(false)
}

match cli.command {
    None => {
        // AD-17 binary-name gate: only the ssync-tui binary is allowed
        // to enter alternate screen, regardless of compiled features.
        #[cfg(feature = "tui")]
        {
            if binary_is_ssync_tui() {
                return crate::tui::entry::run_or_fallback(cfg, cfg_path).await;
            }
        }
        eprintln!("Interactive TUI not available. Use the `ssync-tui` binary.");
        std::process::exit(1);
    }
    Some(Commands::Init { .. }) => { /* unchanged */ }
    // ... existing arms unchanged
}
```

`run_or_fallback` performs the §4 TTY / `TERM` detection and either
launches the TUI (exit 0 on clean quit) or falls back to printing
clap help with **exit 2**. The `ssync` binary never reaches
`run_or_fallback` — it always falls through to the eprintln + exit 1
path above, even when the binary was compiled with the `tui` feature.

---

## 6. State Machine & Ownership

### 6.1 App

```text
App
 ├── active_tab: TabId          { Config, Operate, Checkout }
 ├── focus: FocusPath            (App-owned; tabs receive focus_zone as render param)
 ├── popup: Option<PopupState>
 ├── config: AppConfig           (canonical, mutable)
 ├── config_path: Option<PathBuf>
 ├── config_dirty: bool
 ├── db: rusqlite::Connection    (main thread only; AD-5)
 ├── db_healthy: bool            (false when DB open or query failed; disables DB-dependent features)
 ├── running_op: Option<RunningOp>  (CancellationToken + JoinHandle)
 ├── error: Option<String>       (status-bar red message; Esc clears)
 ├── log_buffer: VecDeque<LogEntry>  (max 500; §17)
 ├── persisted: TuiPersistedState
 └── async_bridge: AsyncBridge
```

When `db_healthy = false` the TUI shows a persistent red banner: `"⚠ Database unavailable — Checkout data disabled"`. Tabs that require a DB (Checkout) display a placeholder; Operate tab still allows check execution (writes will fail and surface via status-bar error). `db_healthy` is set to `false` on the first DB error and rechecked on each `OperationFinished` reload attempt.

### 6.2 FocusPath

```text
FocusPath
 ├── zone: FocusZone            (current zone within active tab)
 └── breadcrumb: Vec<String>    (updated only on zone change, not per ↑↓)
```

### 6.3 PopupState (modal, one at a time)

```text
TargetFilter(TargetFilterState)
OperationParams(OperationParamsState)
OperationProgress(OperationProgressState)
OperationResults(OperationResultsState)
Info { content: String }
ConfigEdit { field, input }
EntryForm { kind: Host|Check|Sync, draft: ... }     // Case B in §15
Confirm { prompt, on_confirm }
```

Input priority: **popup > sub-editor stack > active tab**. Only one
popup at a time. Popups are **focus roots**: the adaptive-arrow escape
in §8 never crosses a popup boundary; only `Esc` dismisses a popup.

**Popup lifetime during tab switches:** `popup` is an `App`-level field,
not per-tab. If the user switches tabs (via `1`/`2`/`3`) while an
`OperationProgress` or `OperationResults` popup is open, the popup
remains visible over the newly active tab — the operation is not tied
to any single tab. Only `Esc` or the operation completing closes it.

### 6.4 Context::from_tui_parts (canonical signature)

The existing `Context` (in `src/commands/mod.rs`) owns its config and
its own `rusqlite::Connection` and is consumed by command functions.
After Phase 0.7 (AD-14) extracts `*_core` functions that take
`&Context`, the TUI builds a fresh `Context` per operation:

```rust
// In src/commands/mod.rs
impl Context {
    /// Build a Context for a single TUI-driven operation.
    ///
    /// `config` is cloned from App.config at spawn time (AD-6).
    /// `state_dir_override` is read from `config.settings.state_dir`
    /// and threaded through `state::db::open` so the DB path matches
    /// CLI behavior exactly (AD-16).
    /// A new rusqlite::Connection is opened per call; App.db is
    /// untouched (AD-5).
    pub fn from_tui_parts(
        config: AppConfig,
        config_path: Option<PathBuf>,
        target_mode: TargetMode,
        serial: bool,
        timeout: u64,
        verbose: bool,
    ) -> Result<Self> {
        let db = crate::state::db::open(config.settings.state_dir.as_deref())?;
        Ok(Context { config, config_path, db, mode: target_mode, serial, timeout, verbose, /* ... */ })
    }
}
```

**Ownership rules (AD-5, AD-6, AD-13):**

- `App.db` (main thread) is never moved or shared with spawned tasks.
- Each TUI-driven operation owns its own `Context` (and therefore its
  own `Connection`); SQLite WAL handles concurrent reads cleanly.
- `AppConfig` is `Clone`; spawn snapshots it.
- The russh `SshPool` lives inside the spawned task's `Context::run`
  scope (built via `SshPool::setup`, same as CLI); `Arc<SshPool>` is
  passed through callers as needed but is **not** held by `App`.
- The TUI never reuses `App.db` for op-driven inserts (e.g. snapshot
  writes from check). Those go through the per-op connection. The
  Checkout tab's `fetch_latest_snapshots` reads happen on `App.db`.

`Context::from_tui_parts` is a thin assembly helper. The actual
operation lives in `commands::check::check_core(ctx) -> CommandReport`
(Phase 0.7), which the TUI calls and translates to `TuiEvent`s.

**Context fields audit (Phase 0.7 precondition):** Before coding
`from_tui_parts`, audit every field of the real `Context` struct in
`src/commands/mod.rs` and confirm each has a valid source in
`from_tui_parts`. The stub `/* ... */` above is intentionally
incomplete — the actual constructor must enumerate all fields
explicitly. Missing fields will surface as compile errors but are
better caught at design time. Additionally verify that `check_core`
does not depend on any `Context` fields that are only meaningful for
other commands (e.g. `run`-specific flags); if it does, those fields
must receive sensible defaults in `from_tui_parts`.

**`target_mode` construction note:** The CLI's `resolve_target_mode`
function takes `&TargetArgs` (CLI flags) and `&AppConfig`. The TUI
bypasses this function and builds `TargetMode` directly from the filter
popup result (`TargetFilterState` → `TargetMode` conversion is 1-to-1:
`All` / `Groups(vec)` / `Hosts(vec)` / `Shell(kind)`). No CLI args
structure is needed because the TUI drives all target selection through
the filter popup UI.

---

## 7. Terminal Lifecycle & Panic Safety (AD-9, AD-10)

This section is the robustness backbone. None of it is optional.

### 7.1 TerminalGuard

```rust
// src/tui/terminal.rs
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn install() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::DisableMouseCapture,  // explicit disable
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(),
            crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
```

A `TerminalGuard` value lives on the stack for the entire `tui::run()`
function. Normal exit, `?` early return, and panic unwind all run its
`Drop`, restoring the terminal.

### 7.2 Panic hook

Installed once, before `TerminalGuard::install()`:

```rust
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore terminal before panic backtrace prints
        let _ = crossterm::execute!(io::stdout(),
            crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
        default_hook(info);
    }));
}
```

The hook + Drop guard are redundant on purpose: if `panic = "abort"`
is ever set, `Drop` won't run and the hook covers it; if the hook fails
for some reason, the guard covers it. Belt and braces.

### 7.3 stderr / log policy (AD-10, AD-13, AD-15)

The TUI does **not** redirect process stderr at the fd level, does
**not** spawn subprocesses, and does **not** re-init the global
tracing subscriber. Instead:

- Operations run through the existing russh `SshPool` (AD-13). Per-host
  command output is captured by the collector (`metrics::collector`,
  `host::session_pool`) and surfaced as structured `TuiEvent`s on the
  AsyncBridge channel — never as raw stdout/stderr.
- `init_tracing()` in `main.rs` is extended (AD-15) to register a
  ring-buffer `tracing_subscriber::Layer` alongside the existing fmt
  layer. While the TUI is running, the fmt layer's writer is swapped
  to `std::io::sink()` via a `tracing_subscriber::reload::Handle` so
  no Rust log line ever paints the raw terminal. On TUI exit the
  writer is restored.
- Rust panics flow through the panic hook (§7.2), which restores the
  terminal first; the panic message reaches the original stderr after
  `LeaveAlternateScreen`.
- The L-key log overlay (Phase 7) reads from the ring buffer.

**Direct-stdio prohibition policy.** AD-10 and AD-15 together cover
russh events and `tracing`-instrumented code, but do NOT automatically
capture `eprintln!`, `println!`, or third-party crates that write
directly to stderr/stdout (e.g., a transitive dep calling `libc::write`
on fd 2). The following rules are **non-negotiable**:

| Rule | Where enforced |
|------|----------------|
| No `eprintln!` / `println!` / `print!` / `eprint!` anywhere in `src/tui/` or in any code path reachable while the TUI is running. All diagnostic output **must** go through `tracing` macros (`error!`, `warn!`, `debug!`). | Code review |
| `src/commands/*_core` functions must never call the output printer (`output::printer`). They receive a `ProgressSink` or return `CommandReport`; printing is the CLI wrapper's responsibility. | Phase 0.7 split enforces this structurally |
| If a new dependency introduces direct stderr output that cannot be disabled, it must be wrapped or replaced before landing in a TUI-reachable path. | CI job: `cargo build --features tui` with `RUST_LOG=error` piped through a TUI smoke test that asserts the raw terminal buffer contains no non-ANSI stray lines |
| Third-party crates that are tui-path-only (e.g., `ratatui`, `crossterm`) do not write to stderr in normal operation and need no wrapping. | N/A |

These rules must be documented in `AGENTS.md` (the existing contributor
policy file in this repo) and enforced
during review for every PR that touches `src/tui/` or adds a
transitive dependency.

### 7.4 External editor 4-stage flow (A-V from v4)

```text
fn open_in_editor(app: &mut App, path: &Path) -> Result<()>:

  Stage 0 — DIRTY CHECK
    if app.config_dirty:
        prompt: "Unsaved changes. [S]ave first / [D]iscard / [C]ancel?"
        S → app.save_config()? (then continue)
        D → reload from disk (then continue)
        C → return Ok(()) without invoking editor

  Stage 1 — PAUSE
    drop the inner TerminalGuard scope        (LeaveAlternateScreen + disable_raw_mode)
    flush stdout

  Stage 2 — EXECUTE
    Command::new(editor_from_env_or_default()).arg(path).status()?
    capture exit_status

  Stage 3 — RESTORE
    re-create TerminalGuard
    if exit_status.success() OR mtime changed:
        app.config = config::app::load(path)?
        reset config_tab cursors to (0, 0)
        app.config_dirty = false

  Stage 4 — REDRAW
    terminal.clear()?
    next render frame paints from scratch
```

Editor resolution: `$VISUAL` → `$EDITOR` → platform default (`vi` on
Unix, `notepad` on Windows). If none of those is available, status bar
shows red "No editor found ($VISUAL/$EDITOR unset)" and stage 1 is
skipped.

Windows mtime granularity is 2s; we treat `exit_status.success()` as
sufficient signal to reload regardless of mtime, which avoids the
"saved fast, no reload" trap.

### 7.5 Operation runtime decision (AD-13)

The TUI is a UI on top of the **existing russh-based execution
runtime**. It does not introduce a second transport (no
`std::process::Command`, no `tokio::process::Command`, no
`Stdio::piped()`). All operations go through:

- `host::pool::SshPool::setup` — same setup CLI uses.
- `metrics::collector::collect_pooled` — per-host metric collection.
- `host::session_pool::*` — channel exec, file transfer.

**Wiring per-host streaming events into the TUI** is done by calling
`ProgressSink` callbacks from `check_core`'s host loop — **not** by
adding a parameter to `collect_pooled`. The actual `collect_pooled`
function is per-host and its signature stays unchanged:

```rust
// metrics::collector — signature UNCHANGED; stays per-host.
pub async fn collect_pooled(
    host: &HostEntry,
    enabled: &[String],
    check_paths: &[(String, String)],
    timeout_secs: u64,
    sessions: Arc<RusshSessionPool>,
) -> Result<CollectionResult>

// ProgressSink is a parameter of check_core, NOT collect_pooled.
pub trait ProgressSink: Send + Sync {
    fn host_started(&self, host: &str);
    fn host_completed(&self, host: &str, status: HostStatus, detail: &str, ms: u64);
}

// Sketch of check_core's host loop:
for host in &reachable_hosts {
    if let Some(p) = progress { p.host_started(&host.name); }
    let result = collect_pooled(host, &enabled[&host.name], ...).await;
    // write result to DB (stays in check_core — TUI needs these writes too)
    if let Some(p) = progress { p.host_completed(&host.name, status, detail, ms); }
}
// Unreachable hosts (reported by SshPool::setup) are also surfaced:
for host in &unreachable_hosts {
    if let Some(p) = progress { p.host_completed(&host.name, HostStatus::Unreachable, "", 0); }
}
```

The TUI's `AsyncBridge` provides a `ProgressSink` impl that pushes
into the `tokio::mpsc` channel; the CLI's `run()` wrapper provides a
`PrinterSink` that calls `output::printer`. No code is duplicated.

**Why ProgressSink must NOT go into `collect_pooled`:** (a) `collect_pooled`
is per-host — there is no batch loop inside it to call `host_started`/
`host_completed` across multiple hosts; (b) unreachable-host events are
emitted by `SshPool::setup`, which runs before any `collect_pooled`
call, so they would be invisible to a sink attached only to
`collect_pooled`; (c) the batch loop, semaphore, and DB writes all live
in `check_core` — the correct integration layer.

**Cancellation under russh** is cooperative at the await point of
each per-host future. Aborting the JoinHandle drops the russh
channel, which sends `CHANNEL_CLOSE` and returns the slot to the
pool. There is no `kill_on_drop` because there is no child process.
Stuck-but-not-yielding russh state machines are bounded by the
existing `ctx.timeout` per-host timeout — the same one the CLI uses.

This decision **enables Phase 3 to ship without an architecture
debate** and is the load-bearing assumption for AD-5/AD-6/AD-10.

### 7.6 Command-core extraction (AD-14, Phase 0.7)

Today `commands::check::run` does target-resolve + pool setup + DB
writes + stdout printing + summary aggregation in one function. The
TUI cannot call it without breaking the screen. Phase 0.7 splits each
of `check`, `run`, `exec`, `sync`, `checkout` into:

```rust
// Pure: no println!, no exit codes, no progress bars.
pub async fn check_core(
    ctx: &Context,
    progress: Option<&dyn ProgressSink>,
) -> Result<CommandReport>;   // returns CommandReport::Check(...)

// Today's run() becomes:
pub async fn run(ctx: &Context, output: &OutputArgs) -> Result<()> {
    let report = check_core(ctx, Some(&PrinterSink::new())).await?;
    output::report::write(&report, output)?;
    Ok(())
}
```

`CommandReport` is the typed per-command result enum defined in Phase
0.7. The TUI obtains the appropriate variant and renders it in
`OperationResults` popup. CLI tests are unchanged because `run()` keeps
the same signature and observable behavior. New core functions get unit
tests in §20.

This refactor **must land before Phase 1a**. Without it, Phase 3's
"thin adapter" is a re-implementation of every command and AD-13
collapses.

### 7.7 Config mutation during active operations

When a `check_core` (or other `*_core`) operation is in flight, the
user may open the config in an external editor (`E` key, Phase 4) or
switch to the Config tab. The following invariants hold:

- **The running operation uses its snapshot.** Spawned tasks receive a
  `Clone` of `AppConfig` at spawn time (AD-6); any mid-flight edits to
  `App.config` never affect the running task.
- **The UI reflects the current `App.config`.** The Applicable Entries
  panel and all Config tab displays show the *current* `App.config`, not
  the snapshot held by the running task. A user who adds or removes a host
  during a running `check` will see the updated sidebar immediately, while
  the progress popup still lists the original host set.
- **Deleted hosts still appear in the progress popup.** If a host is
  removed from config (e.g. via external editor + reload) while a check
  is running on it, the progress popup continues to show that host
  completing with its `HostCompleted` event. This is correct and
  expected: the popup reflects the operation's snapshot, not the
  current config. No special handling is required.

These rules are enforced structurally by AD-6 (snapshot at spawn time)
and require no additional synchronisation primitives.

**External config changes:** If the user edits `config.toml` in a
separate terminal while the TUI is open, the TUI does not automatically
detect the change. For MVP, the user must press `E` (opens external
editor flow) and then Esc to reload the config, or restart `ssync-tui`.
No `inotify`/`FSEvents` file-watcher is implemented — this avoids
platform-specific complexity. This expectation is documented in
`README.md`.

### 7.8 Terminal resize handling

Ratatui requires explicit handling of `crossterm::event::Event::Resize`.
The main loop must match this variant and:

1. Call `terminal.autoresize()` (or re-query terminal size) so ratatui
   knows the new dimensions.
2. Re-evaluate the 80×24 minimum guard: if the new size falls below the
   threshold, immediately render the "Terminal too small" message and
   stop processing other events until the terminal is enlarged again.
   **While in the too-small state, drain and discard all non-Resize
   events from the crossterm queue** (using `event::poll(Duration::ZERO)`
   in a loop) to prevent the event queue from accumulating stale keypresses
   during the resize.
   When the terminal is enlarged above the threshold, resume normal
   rendering.
3. Re-calculate any viewport / scroll offsets that depend on
   `visible_height` (see §11). Clamp `scroll_y` so the invariant
   `scroll_y <= selected <= scroll_y + visible_height - 1` holds with
   the new height.
4. Mark the frame dirty and redraw immediately (do not wait for the next
   50ms tick).

There is no persistent storage of terminal size; all calculations are
done at render time using the live area passed to each widget.

### 7.9 Signal handling (SIGHUP / SIGTERM)

The `q` and Ctrl+C paths in §16.3 trigger state save on clean shutdown.
However:

- **SIGHUP**: sent when the SSH session carrying the TUI is disconnected.
  Default OS behavior is process termination, bypassing `Drop`.
- **SIGTERM**: sent by external process managers (systemd, Docker). With
  `panic = "abort"` the `Drop` impl is also skipped.

To avoid silent state loss, Phase 1a registers a `tokio::signal` handler
(Unix) that intercepts `SIGHUP` and `SIGTERM` and performs a graceful
shutdown identical to `q` (save state, restore terminal, exit 0). On
Windows, `CTRL_CLOSE_EVENT` is the closest equivalent and should be
handled via `SetConsoleCtrlHandler`.

```rust
// In tui/app.rs — inside App::run(), on Unix
#[cfg(unix)]
{
    use tokio::signal::unix::{signal, SignalKind};
    let mut sighup  = signal(SignalKind::hangup())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    // select! branch: sighup.recv() | sigterm.recv() → break 'main
}
```

Signal receipt sets an `App::shutdown_requested` flag (or breaks the
main loop directly); the cleanup path is identical to `q` — state is
saved, `TerminalGuard` is dropped, and the process exits 0.

**No `saving: bool` guard needed:** `tempfile::persist()` is already
atomic on POSIX (rename) and Windows (MoveFileExW). Two concurrent saves
each produce their own temp file and the last persist wins — correct
last-write-wins behavior. The main loop is single-threaded; signal handlers
set a flag and the save runs on the next loop iteration, so there is no
true concurrent struct mutation.

**Windows:** On Windows, `CTRL_CLOSE_EVENT` is the closest equivalent to
SIGHUP/SIGTERM and is handled via `SetConsoleCtrlHandler`. However,
`SetConsoleCtrlHandler` requires the `windows-sys` crate (a Win32 API
wrapper) which is not currently a dependency. For MVP, **defer
`CTRL_CLOSE_EVENT` handling to post-MVP** and rely on
`tokio::signal::ctrl_c()` (which catches Ctrl+C and `CTRL_BREAK_EVENT`
on Windows, stable and dependency-free). Add a note in the code:
`// TODO(post-MVP): CTRL_CLOSE_EVENT via windows-sys for close-button
// shutdown on Windows`. This means Windows users who close the terminal
window without Ctrl+C may not get a clean state save; this is acceptable
for MVP.

**Windows timing constraint:** On Windows, `CTRL_CLOSE_EVENT` handlers
run in a separate thread with a hard OS deadline (~5 seconds) before
the process is forcibly terminated. State save + terminal restore must
complete within this window. `tempfile::persist()` is typically fast
(< 10ms), but if the state directory is on a network drive or slow
disk it could approach the limit. If state save takes > 1s on Windows,
emit `tracing::warn!("State save is taking longer than expected on
CTRL_CLOSE_EVENT")`. The Unix `SIGHUP`/`SIGTERM` handlers do not have
this hard time constraint.

---

## 8. Navigation Model

### 8.1 Levels

| Level | Description |
|-------|-------------|
| L0 | Tab Bar — always reachable via ←→ or `1`/`2`/`3` shortcuts. **X-only with wrap.** |
| L1 | Tab Root — top-level zones within a tab (sidebar, panels, control rows) |
| L2 | Component — lists, tables, forms, radio groups within a zone |
| L3 | Item — individual field, list item, checkbox, radio button |
| L4 | Popup — modal overlay; **focus root**; same level model applies internally as PL1/PL2/... |

### 8.2 Keys

| Key | Behavior |
|-----|----------|
| ↑ / ↓ | PRIMARY vertical. Adaptive escape (§8.3) at boundary. |
| ← / → | PRIMARY horizontal. Adaptive escape at boundary. |
| Tab | Cycle focusable targets in **current level only**, with wrap. **Never escapes level boundaries.** The canonical navigation rule is: arrow keys drive cross-level transitions (via `escape_to_parent`); Tab only cycles peers within the same level. |
| Shift+Tab | Reverse cycle, same level, same wrap rule. |
| Enter | Confirm / execute the focused element. **Never** used for level entry. |
| Space | Toggle checkbox / multi-select / radio. |
| Esc | Close popup; cancel edit; clear `app.error`. |
| 1 / 2 / 3 | Jump directly to Config / Operate / Checkout. |
| `?` | Open keybinding help popup (minimal in Phase 1a; full table in Phase 6). |
| `i` | Toggle contextual info popup (open ↔ close). |
| `q` | Quit (when no popup, no edit, no running op). |
| Ctrl+C | Always quits / cancels running op (overrides `q` gating). |

### 8.3 Adaptive arrow navigation

Every focusable component implements:

```rust
trait Focusable {
    fn axis_freedom(&self) -> AxisFreedom;     // Y, X, XY, or None
    fn at_boundary(&self, dir: Direction) -> bool;
}

enum AxisFreedom { Y, X, XY, None }
```

Decision per keypress:

```text
[Key pressed] → look at focused element's axis_freedom

  None (single button / lone checkbox):
    any arrow                   → escape_to_parent(axis=key, dir=key)

  Y-only:
    ← or →                      → escape_to_parent(axis=X, dir=key)
    ↑ or ↓ if !at_boundary      → move within element
    ↑ or ↓ if  at_boundary      → escape_to_parent(axis=Y, dir=key)

  X-only:
    ↑ or ↓                      → escape_to_parent(axis=Y, dir=key)
    ← or → if !at_boundary      → move within element
    ← or → if  at_boundary      → escape_to_parent(axis=X, dir=key)

  XY:
    if !at_boundary(dir)        → move within element
    if  at_boundary(dir)        → escape_to_parent(axis=key_axis, dir=key)
```

`escape_to_parent` walks up the focus stack until a container can
absorb the move; if nothing absorbs and the root is reached:

- L0 Tab Bar wraps (← from leftmost → rightmost; → from rightmost → leftmost).
- L4 popup root **does not wrap and does not escape** (modal is sealed).
- Other roots stop silently at boundary.

**`Focusable` simplification consideration (Phase 1b):** The two-method
API (`axis_freedom()` + `at_boundary(dir)`) requires every component to
implement both. An alternative: a single
`handle_arrow(dir) -> ArrowResult { Consumed, Escaped(Direction) }`
method that encapsulates the boundary check internally and reduces the
public surface area to one method. Consider this design before finalising
Phase 1b — if both methods are easy to implement in every component the
current design is fine; if components repeatedly duplicate the same
boundary logic, refactor to the single-method form.

### 8.4 Radio/toggle ←→ priority (was A10, simplified)

When an X-only radio (`propagate_deletes`, ad-hoc `[◉ Use entries]`) is
focused, ←→ cycles values **first**; only after the radio reports
`at_boundary(left)` for ← (i.e. already at first option) does the
escape rule fire. This makes the radio feel native without inventing
`h`/`l` aliases.

### 8.5 Breadcrumb update rule

Breadcrumb updates **only on zone change** (which includes any
`escape_to_parent` that crosses a level boundary). Plain ↑↓ within a
list does NOT redraw the breadcrumb. The rightmost segment shows the
current item name.

Examples:
- `[1:Config] Hosts > debian > address`
- `[2:Operate] Filter Popup > Mode > Groups > web`

### 8.6 Per-tab zone neighbour tables

These tables make `escape_to_parent` deterministic for multi-candidate
situations. Each row defines: when focus escapes from a given zone in a
given direction, which zone it enters. A cell marked `—` means the
boundary is sealed (focus stops silently). Zone IDs map to the render
layout (§12).

**Config tab (§12.2) — zones: `Sidebar`, `FieldTable`**

| Focused zone | ← (left escape) | → (right escape) | ↑ (top escape) | ↓ (bottom escape) |
|---|---|---|---|---|
| Sidebar | — | FieldTable | — | — |
| FieldTable | Sidebar | — | — | — |

Within Sidebar: ↑↓ cycle section rows; Enter expands/collapses. Within
FieldTable: ↑↓ cycle fields; Enter or `e` activates inline edit.

**Operate tab (§12.3) — zones: `OpRadio`, `TargetRow`, `ParamPanel`, `ApplicableEntries`, `ExecuteBtn`**

| Focused zone | ← | → | ↑ (at top) | ↓ (at bottom) |
|---|---|---|---|---|
| OpRadio | — | — | — | TargetRow |
| TargetRow | — | — | OpRadio | ParamPanel |
| ParamPanel | — | — | TargetRow | ApplicableEntries |
| ApplicableEntries | — | — | ParamPanel | ExecuteBtn |
| ExecuteBtn | — | — | ApplicableEntries | — |

Tab cycles within: `OpRadio` → `TargetRow` → `ParamPanel` → `ApplicableEntries` → `ExecuteBtn` → (wrap to `OpRadio`). All at L1/L2 — Tab never jumps to L0 Tab Bar.

**← → in the Operate tab:** At L1 (zone level) all ← → entries are `—` (sealed) — arrow keys do not move between zones horizontally. At L2 within `OpRadio`, ← → **do** cycle the radio button value (§8.4). This is intentional: zone-level horizontal navigation is not needed because the tab has a vertical stack layout; ← → at zone level would have no meaningful destination.

**Checkout tab (§12.4) — zones: `Controls`, `HostTable`**

| Focused zone | ← | → | ↑ (at top) | ↓ (at bottom) |
|---|---|---|---|---|
| Controls | — | — | — | HostTable |
| HostTable | — | — | Controls | — |

Tab cycles: `Controls` ↔ `HostTable`.

**All popups (L4):** focus is fully contained inside the popup. No
direction escapes through the popup boundary. Esc closes the popup and
restores focus to the zone that opened it.

---

## 9. Focus Visual States

Three states per element type:

| State | When |
|-------|------|
| Inactive layer | Element is in a level NOT currently active |
| Active non-focus | Element is in the active level but is not the focus target |
| Current focus | This element receives keyboard input |

| Element | Inactive | Active non-focus | Current focus |
|---------|----------|------------------|---------------|
| List item | DarkGray text | Normal | Reversed + Bold + `▶` prefix |
| Input field | DarkGray border, dim text | Normal | Yellow/Cyan border, bold, blinking cursor |
| Panel border | DarkGray (`│`) | Normal (`│`) | Accent color, thick (`┃`) |
| Button | DarkGray, dim brackets | Normal `[X]` | Reversed bg, bold |
| Checkbox | DarkGray `[☐]/[☑]` | Normal | Yellow `[☐]/[☑]`, bold |
| Radio | DarkGray `○/◉` | Normal | Yellow `◉`, bold |

**Non-color affordances** (for color-blind / monochrome terms):
- Focused list items always carry a `▶` prefix.
- Active-layer panel uses a thick border character (`┃` `━` `┏` `┓`),
  inactive uses thin (`│` `─` `┌` `┐`). Border weight conveys state
  even when colors collapse.

Implementation: a `FocusLayer { Active, Inactive }` enum is passed as
a render parameter to every component. Components select their
border/text style accordingly.

---

## 10. Color Scheme

A single canonical palette lives in `src/tui/theme.rs`. Per-tab accent
colors are pulled from this module; no hard-coded ratatui Color values
elsewhere in the TUI.

```rust
// src/tui/theme.rs
pub struct Theme {
    pub accent_config:   Color,  // Yellow
    pub accent_operate:  Color,  // Cyan
    pub accent_checkout: Color,  // Green
    pub error:           Color,  // Red
    pub warning:         Color,  // Yellow (same hue, different role)
    pub stdout:          Color,  // White / default
    pub stderr:          Color,  // Red
    pub internal:        Color,  // DarkGray
    pub inactive:        Color,  // DarkGray
    pub editing:         Color,  // accent_config when editing scalars
    pub border_active:   Color,  // tab-accent
    pub border_inactive: Color,  // DarkGray
}

impl Theme {
    pub fn default() -> Self { /* ... */ }
    pub fn from_config(cfg: &AppConfig) -> Self { /* future: user theme */ }
}
```

Future user-customizable theming is left as a Post-MVP item; for v1 the
`default()` palette ships and `Theme::from_config` is a stub that
returns default. Centralization now means swapping later is mechanical.

**16-color compatibility rule:** All `Theme` colors must be distinguishable
on 16-color ANSI terminals (e.g. `TERM=screen`, `tmux` with limited color
support). Accent colors must not rely exclusively on 256-color or true-color
values. All `Theme` fields must use `ratatui::style::Color` **named variants**
(`Color::Yellow`, `Color::Cyan`, `Color::Red`, `Color::DarkGray`, etc.) — no
RGB (`Color::Rgb(r, g, b)`) or indexed (`Color::Indexed(n)`) values in the
default palette. Phase 1b's theme validation step must include testing under
`TERM=screen` and a 16-color terminal to confirm state (focus, inactive,
error) remains visually distinct without true-color rendering.

### 10.1 Glyph fallback

The TUI uses Unicode box-drawing and symbol characters: `▶ ┃ ━ ◉ ☑ ✓ ✗ ⊘ ⚠`
and box corners `┏ ┓ ┗ ┛`. **Terminals that cannot render these characters
(e.g. Windows cmd.exe without ConPTY, legacy serial terminals) are
out of scope for MVP.** The supported baseline is any modern terminal
emulator on Unix/macOS/Windows Terminal (ConPTY) that sets a UTF-8 locale
or code page.

If runtime Unicode glyph fallback is needed in a future release, add a
`unicode_glyphs: bool` field to `Theme` and substitute ASCII equivalents:
`>` for `▶`, `|` for `┃`, `-` for `━`, `#` for `☑`, `!` for `⚠`. This
is a Post-MVP item; do not add the fallback detection logic to MVP code.

---

## 11. Scroll & Cursor Model

Selection cursor and viewport offset are decoupled.

```text
viewport_offset: scroll_y       (first visible line index)
cursor:          selected       (currently highlighted item index)

Invariant: scroll_y <= selected <= scroll_y + visible_height - 1
```

| Key | Effect |
|-----|--------|
| ↑ / ↓ (or `j`/`k`) | Move `selected` ± 1. Adjust `scroll_y` minimally to keep cursor visible. |
| PgUp / PgDn | Shift `scroll_y` by `visible_height`. Cursor clamps to nearest visible edge if it would fall outside. |
| Home / End | `selected` jumps to first/last; viewport adjusts. |

Y-overflow handled in: Config field table, Checkout host table, Filter
popup sub-lists, Info popup, Operation progress popup, Applicable
Entries panel.

**Unicode display width:** All truncation and visible-height calculations
must use `unicode_width::UnicodeWidthStr::width()` rather than
`str::len()` or `.chars().count()`. CJK characters occupy 2 terminal
columns; truncating by byte or char count produces misaligned columns
and visual corruption. This applies to host names, entry labels, command
output in the progress popup, and any string rendered in a fixed-width
column. Add `unicode-width` to `Cargo.toml` (always-on, tiny dep) in
Phase 0.5 alongside `toml_edit`.

---

## 12. Tab Layouts

### 12.1 Tab bar (always visible)

```
[1:Config]  [2:Operate]  [3:Checkout]              ssync v0.9 | 12 hosts
```

### 12.2 Config tab

```
┌──────────────┬──────────────────────────────────────┐
│[>Settings  ] │ default_timeout .............. 30   │
│  Hosts (12)  │ data_retention_days .......... 90   │
│  Checks (3)  │[>conflict_strategy ...... Newest]   │ ← focused field
│  Syncs (2)   │ propagate_deletes ......... false   │
│              │ max_concurrency ............. 10    │
│              │ ...                                  │
├──────────────┴──────────────────────────────────────┤
│ Config > Settings > conflict_strategy               │ ← breadcrumb
│ ←→:Sidebar↔Fields  e/Enter:Edit  a:Add d:Del S:Save │
└─────────────────────────────────────────────────────┘
```

Three explicit levels: **section → entry → field**. Sidebar shows
sections; `Hosts/Checks/Syncs` expand to per-entry rows. `Enter` on a
sub-entry opens the field table for that entry.

Keys (Config tab):
- ↑↓: vertical move within current pane (sidebar or field table)
- ←→ / Tab: switch between sidebar and field table
- `e` / `Enter` on scalar field: inline edit (§15 Case A)
- `a` on `Hosts/Checks/Syncs`: add new entry via template form (§15 Case B)
- `d`: delete focused entry (confirmation)
- `S`: save (`config::app::save`, format-preserving via `toml_edit`)
- `E`: open `config_path` in `$VISUAL`/`$EDITOR` (§7.4)

### 12.3 Operate tab

```
┌─────────────────────────────────────────────────────┐
│ Operation: [◉ check] [○ run] [○ exec] [○ sync]     │ ← L1 radio
├─────────────────────────────────────────────────────┤
│[Target: groups:web,db (8 hosts)        [f] Filter] │ ← L1 row
│ Mode: parallel  Timeout: [30s     ]                 │
├─────────────────────────────────────────────────────┤
│ Parameters:                                         │ ← L2
│   For run:  [command: __________] [sudo:☐] [yes:☐] │
│   For exec: [script:  __________] [sudo:☐] [keep:☐]│
│   For sync: [Mode: ◉entries  ○adhoc] ...            │
├─────────────────────────────────────────────────────┤
│ ─ Applicable Entries (2 of 3 match) ──────────────  │
│ ▶ Check #1  groups:[web,db]  metrics:cpu,mem,disk   │
│   ✓ matches  [Press Enter to expand]                │
│ ⊘ Check #3  enable_all=false (excluded)             │
├─────────────────────────────────────────────────────┤
│ [Execute]                                           │ ← explicit action button
├─────────────────────────────────────────────────────┤
│ Operate > Parameters > run_command                  │
│ ↑↓:Fields  Enter:Edit/Execute  f:Filter  i:Info     │
└─────────────────────────────────────────────────────┘
```

Notes:
- The `[Execute]` button is its own focusable element. `Enter` on a
  param edits; `Enter` on `[Execute]` runs. This eliminates the v4
  ambiguity about what `Enter` does at different focus locations.
- Applicable Entries panel: max 6 rows + own scroll; collapses to
  one-line header below 90-col width.
- Sync mode banner: when `sync_mode = adhoc` is restored from
  persisted state, a yellow banner appears at the top of the params
  block: `⚠ Ad-hoc mode active — config entries bypassed [M to switch]`.

### 12.4 Checkout tab — MVP layout (Phase 1a + 2)

```
┌─────────────────────────────────────────────────────┐
│[[f] Filter]                                         │ ← L1 controls (MVP: filter only)
├─────────────────────────────────────────────────────┤
│ Host       Status    CPU Load Memory Disk  Last Seen│
│ ─────────────────────────────────────────────────── │
│[> debian   ✓ online  0.15     45%    32%   5m ago] │ ← focused row
│  macmini   ✓ online  1.23     67%    54%   12s ago  │
│  win-box   ✗ offline -        -      -     3d ago   │
├─────────────────────────────────────────────────────┤
│ Checkout > Rows > debian                            │
│ ↑↓:Rows  Tab:Controls↔Table  f:Filter               │
└─────────────────────────────────────────────────────┘
```

MVP key bindings only: `↑↓` `Tab` `f`. **No** `h`, `s`, `o`, or
`Enter→detail` in MVP — those controls are not rendered, the keys
are not bound, and the persistence schema does not store them
(see §16.1). They appear in the Phase 6 expansion below.

Reuses `fetch_latest_snapshots`, `DisplayColumns::from_context`, and
all `extract_*`/`format_relative_time` helpers from `checkout.rs`
(made `pub(crate)` in Phase 1a).

### 12.4.1 Checkout tab — Post-MVP additions (Phase 6)

When `commands::checkout` gains `--history` and `--since` capability
(or as a TUI-only feature), the controls bar grows to:

```
[[f] Filter]  [[h] History: off]  [[s] Since: -]
```

with `h` toggling history mode, `s` opening a date input popup,
`o` exporting the report, and `Enter` opening a per-host detail
popup. Keybindings, persistence schema entries, and the post-MVP
acceptance list (§21.B) all gain the matching rows at that time.

---

## 13. Filter Popup

Shared by Operate and Checkout (`f` key).

```
┌── Target Filter ────────────────────────┐
│  Filter > Mode:Groups > web ☑           │
│                                         │
│  Mode:                                  │
│   ○ All hosts                           │
│  [◉ Groups            ]  ← focused PL1  │
│     ┌──────────────────────────┐        │
│     │[☑ web              ]     │ ← PL2  │
│     │ ☐ db                     │        │
│     │ ☐ prod                   │        │
│     └──────────────────────────┘        │
│   ○ Hosts                               │
│   ○ Shell                               │
│                                         │
│  [☐ Serial execution]   [Timeout: 30 ]  │
│                                         │
│  [[Apply]]   [[Cancel]]                 │
└─────────────────────────────────────────┘
```

Keys inside popup follow §8 (popup is its own focus root):
- ↑↓: vertical across PL1 → PL2 → options → buttons
- → on a mode radio: enters its sub-list (PL2)
- ← inside sub-list: exits to PL1
- Space: toggle ☑/☐ or select ◉
- Tab: cycle within current PL level only
- Enter on `[Apply]`: persist filter + close
- Esc: close without applying

Data sources:
- Groups: `collect_available_groups()`
- Hosts: `host[].name` entries
- Shells: `[Sh, PowerShell, Cmd]` — **dynamically filtered** to only
  show shell types actually present among configured hosts (e.g. if no
  Windows hosts exist, `PowerShell` and `Cmd` are omitted from the list).
  **Note:** the Shell filter mode is only meaningful in the Operate tab
  (where operations target hosts by detected shell type). The Checkout
  tab's filter popup should **omit the Shell option entirely** — Checkout
  data is indexed by host name, not shell type, and showing Shell there
  would produce confusing or empty results.

Cold-start fallback: if no known groups/hosts exist (`config` empty),
show "(No known hosts — run `ssync check` first)" with a manual-input
fallback row.

---

## 14. Key Bindings (consolidated)

### 14.1 Global

| Key | Action |
|-----|--------|
| 1 / 2 / 3 | Jump to Config / Operate / Checkout |
| Tab / Shift+Tab | Cycle within current level (with wrap) |
| q | Quit (gated: no popup, no edit, no running op) |
| Ctrl+C | Quit immediately (cancels running op) |
| Esc | Close popup / cancel edit / clear error |
| `i` | Toggle contextual info popup |
| `?` | Keybinding help popup |

### 14.2 Per-tab

Config: ↑↓ (move), ←→ (sidebar↔fields), `e`/`Enter` (edit field),
`a` (add entry), `d` (delete), `S` (save), `E` (open in $EDITOR),
Home/End, PgUp/PgDn.

Operate: ↑↓ (between selector / target / params / execute),
←→ (op type cycle, radio cycle), Enter (edit field OR execute, see
§12.3), Space (toggle checkbox), `f` (filter), `i` (info),
`r` (refresh applicable entries preview).

Checkout (MVP): ↑↓ / `j`/`k` (rows), Tab (controls↔table), `f` (filter),
`r` (manual DB reconnect when `db_healthy = false`),
Home/End, PgUp/PgDn.

Checkout (Post-MVP additions, Phase 6): `Enter` (detail popup),
`h` (toggle history), `s` (since input), `o` (export report).

### 14.3 Inside any text input field (InputMode::Active)

ALL global single-letter shortcuts are suspended. Only:
- printable chars: typed into field
- Backspace / Delete / arrow keys (cursor movement within field)
- Ctrl+A / Ctrl+E (line start / end)
- Ctrl+D: clear `Option<T>` field to `None` (where applicable); after clearing, the field displays as `—` (em dash) with dim styling to indicate "no value set"
- Enter: confirm
- Esc: cancel and **restore previous value** (does not clear)

The InputMode flag is checked **first** in `App::handle_key()` before
any global routing.

---

## 15. Config Edit Model

### 15.1 Case A — Single-value scalar

Applies to: strings, numbers, booleans, `Option<T>` scalars
(`default_timeout`, `max_concurrency`, `propagate_deletes`,
`source`, etc.).

Flow:
1. Press `Enter` or `e` on a focused scalar field row.
2. Value cell becomes an inline `InputField` (renders full row width
   if needed; long inputs scroll horizontally inside the field).
3. Border becomes Yellow; breadcrumb shows `[EDIT]` suffix.
4. Esc → restore original; Enter → validate type → if invalid show
   inline red hint, keep edit mode; if valid → mutate `App.config`,
   set `config_dirty = true`.
   Ctrl+D → clear an `Option<T>` field to `None` (e.g. clear a custom
   source path); the field displays as `—` (em dash) with dim styling after clearing.
5. Save deferred until `S` (which calls `config::app::save` →
   `toml_edit` write-back preserves comments / formatting).

### 15.2 Case B — Composite entry (table / array of tables)

Applies to: `[[host]]`, `[[check]]`, `[[sync]]` entries; `Vec<CheckPath>`
sub-tables.

Add flow:
1. Press `a` while sidebar focuses a composite section.
2. Pre-filled template form popup opens, all fields shown with
   defaults (`enabled=true`, `groups=[]`, etc.).
3. Form layout: one field per row; ↑↓ between rows; `Enter`/`e` opens
   per-field inline edit (Case A applied within the popup).
4. `[Save Entry]` validates required fields → appends to
   `App.config` → `config_dirty = true` → close.
5. `[Cancel]` discards and closes.

Edit existing flow:
1. `Enter` or `e` on an existing composite entry row in sidebar.
2. Same form opens, fields pre-populated.
3. The form binds to a **mutable clone** of the entry; `App.config`
   is unchanged until `[Save Entry]` confirms.

### 15.3 Sub-editor lifecycle (Vec fields)

Vec fields (`paths`, `enabled`, `groups`) open as scoped sub-editors.
Inside a sub-editor:
- ↑↓: move rows
- `a`: add row   `d`: delete row
- `Enter`: edit cell (inline)
- `Esc`: exit sub-editor (changes committed to in-memory entry)

While a sub-editor is open, **global single-letter shortcuts
(`a`, `d`, `S`, `q`, `i`, `f`, `1`/`2`/`3`) are suspended.** Same
discipline as InputMode.

### 15.4 TOML write-back

`config::app::save()` is updated to keep its current first-create
behavior (which calls `inject_config_comments` to seed the file with
guidance comments) while gaining a format-preserving edit path:

```rust
pub fn save(config: &AppConfig, custom_path: Option<&Path>) -> Result<()> {
    let path = resolve_path(custom_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let new_content = match std::fs::read_to_string(&path) {
        // Existing file: parse with toml_edit, mutate in place,
        // serialize. This preserves user comments and key order.
        Ok(original) => {
            match original.parse::<DocumentMut>() {
                Ok(mut doc) => {
                    apply_config_to_doc(&mut doc, config)?;
                    let candidate = doc.to_string();
                    // Round-trip validation: re-parse the output to catch bugs
                    // in apply_config_to_doc before writing to disk. A buggy
                    // helper producing invalid TOML is caught here, not by the
                    // user on next reload.
                    toml::from_str::<AppConfig>(&candidate)
                        .context("apply_config_to_doc produced invalid TOML; aborting write")?;
                    candidate
                }
                Err(e) => {
                    // toml_edit failed to parse the existing file (e.g. non-standard
                    // whitespace, multiline edge cases). Fall back to a full
                    // toml::to_string_pretty serialize — comments are lost but data
                    // is preserved. Surface a Tier 3 warning to the user.
                    tracing::warn!(
                        "Config comments lost — file contained non-standard formatting \
                         that toml_edit could not parse: {e}"
                    );
                    toml::to_string_pretty(config)?
                }
            }
        // First-create: serialize fresh and inject the guidance
        // comments via the existing inject_config_comments() helper.
        // toml_edit cannot generate these from nothing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let serialized = toml::to_string_pretty(config)?;
            inject_config_comments(&serialized)
        }
        Err(e) => return Err(e.into()),
    };

    let tmp_file = tempfile::Builder::new()
        .prefix(".ssync-config-")
        .suffix(".tmp")
        .tempfile_in(path.parent().unwrap_or(Path::new(".")))?;
    tmp_file.as_file().write_all(new_content.as_bytes())?;
    tmp_file.as_file().flush()?;
    // persist() does an atomic rename on POSIX and a replacement
    // strategy on Windows (uses MoveFileExW with MOVEFILE_REPLACE_EXISTING
    // internally), making this safe on both platforms.
    tmp_file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
```

Notes:
- `inject_config_comments` stays in `config::app` and is reused only
  on first-create. Once the file exists, `toml_edit` round-trips
  preserve whatever comments are in it (including the seeded ones).
- `apply_config_to_doc` is a new helper that walks `AppConfig` fields
  and writes them into the `DocumentMut`, preserving table/key
  ordering for keys that already exist; new entries get appended.
  **Implementation note:** this function is non-trivial — the TOML
  structure includes three distinct `[[array-of-tables]]` sections
  (`[[host]]`, `[[check]]`, `[[sync]]`) each with different fields
  and nested sub-tables (e.g. `CheckPath` within `[[check]]`). A
  field mapping table or pseudocode design should be written before
  implementation. See the write-back policy and test cases below.
  **Phase split:** Phase 0.5 implements **scalar-only** round-trip
  (all `[settings]` scalar fields; `name` label on `CheckEntry` /
  `SyncEntry`). **Explicitly excluded from Phase 0.5:** per-entry
  scalars such as `address`, `port`, `user` inside `[[host]]`
  entries, and any field inside `[[check]]`/`[[sync]]` entries
  beyond the `name` label — these belong to the full
  `[[array-of-tables]]` mutation scope deferred to Phase 7.
  This covers ~80% of use cases with minimal CLI regression risk.
  Full `[[array-of-tables]]` add/remove/reorder support lands in
  **Phase 7** alongside the Config tab case B editor.
  Phase 0.5 tests (T-WB-1 through T-WB-4) still land in Phase 0.5
  as design validation even if array-of-tables mutation is stubbed.

**`apply_config_to_doc` write-back scope matrix:**

| Scope | Phase | What changes |
|-------|-------|-------------|
| `[settings]` scalars + `name`/`id` label on `CheckEntry`/`SyncEntry` | 0.5 | All `AppConfig.settings` fields + display labels |
| Per-host scalars (`address`, `port`, `user`, etc. within `[[host]]`) | 7 | Individual `[[host]]` field mutation |
| Full `[[array-of-tables]]` mutation (add/remove/reorder `[[host]]`, `[[check]]`, `[[sync]]`) | 7 | Alongside Config tab Case B editor |
- `tempfile::NamedTempFile::persist()` is cross-platform safe: on
  POSIX it issues an atomic `rename(2)`; on Windows it uses
  `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`. The temp file is created
  in the **same directory** as the target to guarantee same-filesystem
  rename semantics.

**`apply_config_to_doc` write-back policy (explicit rules):**

1. **Unknown keys are preserved.** Keys present in the TOML file that
   do not map to any `AppConfig` field are left in the `DocumentMut`
   unchanged.  `toml_edit` retains them automatically; `apply_config_to_doc`
   must not call `.remove()` on unrecognised keys.
2. **Deleted `[[host]]`, `[[check]]`, or `[[sync]]` entries lose their attached
   comments.** When an `[[array-of-tables]]` entry is removed from
   `AppConfig`, the entire `toml_edit` array item (including any
   inline comments immediately above it) is dropped.  This is an
   accepted limitation of the `toml_edit` model — it cannot detach
   comments from their owning item.  **Document this clearly in the
   UI:** a warning banner in the Config edit form will note
   "Saving removes comments attached to deleted entries."
   This rule applies equally to `[[host]]`, `[[check]]`, and `[[sync]]`
   entries — all three are TOML arrays-of-tables and receive identical
   treatment by `apply_config_to_doc`.
3. **Entry order follows `AppConfig` order, not file order.**
   When the in-memory `Vec` order differs from the file order
   (e.g. user reordered entries in the TUI), `apply_config_to_doc`
   rebuilds the array from scratch using the in-memory order.
   Scalar-only edits where the entry count and identity are unchanged
   **do** preserve original file order because `apply_config_to_doc`
   iterates by existing item index and mutates in place.

**Required test cases for `apply_config_to_doc` (land in §20.1):**

- **T-WB-1 (delete `[[check]]`):** Load a config with two `[[check]]`
  entries (the first has a `# comment` line above it); call
  `apply_config_to_doc` with only the second entry retained;
  assert the saved file: contains exactly one `[[check]]`; the
  comment is absent; all other top-level keys are byte-identical.
- **T-WB-2 (preserve unknown key):** Load a config that contains an
  unrecognised key `[settings]\nunknown_future_option = true`; save
  with an unchanged `AppConfig`; assert the key survives in the
  round-tripped file.
- **T-WB-3 (inline comment survives scalar edit):** Load a config
  whose `[settings]` table has `max_concurrency = 10  # max 50`;
  mutate only `max_concurrency` to `20`; assert the saved file
  contains `max_concurrency = 20  # max 50` (comment preserved).
- **T-WB-4 (delete `[[host]]`):** Load a config with two `[[host]]`
  entries (the second has a `# my jump-box` comment above it); call
  `apply_config_to_doc` with only the first entry retained; assert
  the saved file: contains exactly one `[[host]]`; the comment is
  absent; all `[[check]]` / `[[sync]]` blocks and top-level keys
  are byte-identical to the original. This confirms the comment-loss
  rule (rule 2) applies uniformly to all array-of-tables sections.

---

## 16. State Persistence

File: `{resolved_state_dir}/tui_state-{config_hash}.toml` (AD-16).

- `resolved_state_dir` is the **same path the DB uses**: call
  `state::db::resolved_state_dir(config.settings.state_dir.as_deref())?`
  — a new `pub fn` to be added to `src/state/db.rs` (Phase 0.5) that
  returns the OS default (`~/.local/state/ssync` on Unix,
  `%LOCALAPPDATA%/ssync` on Windows) when the override is `None`, or
  the override path directly. `state::db::open` is refactored to call
  this helper internally so both paths stay in sync. **Note:** the
  existing `state::db::state_dir()` (no-arg variant) does not accept
  an override; using it directly would silently ignore
  `config.settings.state_dir`.
- `config_hash` is the first 8 hex chars of `blake3(effective_path_str)` where
  `effective_path_str` is derived with this exact sequence:
  1. **`let resolved = config::app::resolve_path(custom_path)?`** — applies
     the same default path (`~/.config/ssync/config.toml`) that the runtime
     uses when no custom path is given. **Note:** the current implementation
     does **not** expand `~` or normalize relative paths. Phase 0.5 must
     enhance `resolve_path` with tilde expansion (`dirs::home_dir()`
     substitution) so that `None`, `~/.config/ssync/config.toml`, and an
     explicit equivalent path all produce the **same hash**.
     Without this fix, users passing `~/.config/ssync/config.toml` explicitly
     would get a different state file than users relying on the default.
  2. `canonicalize(&resolved).to_str().to_owned()` — succeeds when the file
     exists; eliminates symlinks and double-slashes.
  3. `resolved.to_string_lossy()` — fallback for first-launch before the
     config file is written (canonicalize fails on non-existent paths).
  `canonicalize()` is **never** a hard precondition for startup; the
  hash is stable for the typical case (file exists) and degrades
  gracefully when it does not. Most users see exactly one state file.
  Users who run ssync against multiple distinct configs get one state
  file per config without manual setup.
- File missing or unreadable → start with `TuiPersistedState::default()`,
  log a warning to the ring buffer, do not crash.

### 16.1 Schema

```toml
[tui_state]
active_tab = "Operate"          # Config | Operate | Checkout

[target_filter]
mode    = "Groups"              # All | Groups | Hosts | Shell
groups  = ["web", "db"]
hosts   = []
shell   = "Sh"
serial  = false
timeout = 30

# MVP fields below — present in Phase 2 wiring.
[operate]
operation       = "check"       # MVP: check only. run|exec|sync land in Phases 5–6.
# NOTE: The Operate tab Phase 1a UI shows all four radio options (check/run/exec/sync)
# but only "check" is persisted in MVP.  Selecting run/exec/sync in Phase 1a is allowed
# but the selection is NOT stored and resets to "check" on restart.  The full
# operate.operation value set ("run"|"exec"|"sync") is persisted starting in Phase 5.

# ── Phase 4 fields (NOT serialized in MVP) ──
# run_sudo        = false
# run_yes         = false
# exec_sudo       = false
# exec_keep       = false

# ── Phase 5 fields (NOT serialized in MVP) ──
# sync_dry_run    = false
# sync_mode       = "entries"     # entries | adhoc — banner if adhoc
# adhoc_files     = []
# config_mtime    = 0             # for sync_source invalidation
# sync_source     = ""

# ── Phase 6 fields (NOT serialized in MVP) ──
# [checkout]
# history = false
# since   = ""

# Intentionally NEVER persisted (per AD-7, AD-12):
# - cursor positions (config_cursor, operate_cursor, checkout_cursor)
# - run_command, exec_script (privacy: may contain tokens)
```

### 16.2 Load-time validation

After deserializing, before `App::run()`:

1. If `active_tab` is unknown → default to `Config`.
2. `target_filter.groups`: filter out groups not in current
   `config.collect_available_groups()`. If result is empty and
   mode was `Groups`, fall back to `mode = "All"`.
3. `target_filter.hosts`: filter against current `host[].name` list.
4. `target_filter.shell`: validate against the 3-value enum.
5. `operate.config_mtime` vs current `config.toml` mtime: if
   different → reset `sync_source = ""`, update `config_mtime`,
   surface a status-bar info message. **(Phase 5 only; `config_mtime`
   is not present in `TuiPersistedState` until Phase 5. For MVP skip
   this step entirely — the field will deserialise to its `Default`
   and the check is a no-op.)**

All `TuiPersistedState` fields carry `#[serde(default)]` — missing
keys load as defaults rather than parse-erroring.

**Schema version note:** `#[serde(default)]` handles additive changes
(new optional fields) gracefully but cannot handle enum variant renames
or semantic meaning changes without silent data loss.  If a future
change renames a variant (e.g. `"Sh"` → `"Unix"` in `shell`) or
changes the meaning of a stored value, add an explicit
`schema_version: u32` field (default 0) to `TuiPersistedState` and
gate a migration block on load:
```rust
if state.schema_version < CURRENT_SCHEMA_VERSION {
    migrate_state(&mut state);
    state.schema_version = CURRENT_SCHEMA_VERSION;
}
```
This is not required until the first breaking schema change occurs,
but the migration hook should be wired at that point, not earlier.

### 16.3 Save points

- On `q` / Ctrl+C graceful shutdown.
- On `SIGHUP` / `SIGTERM` (Unix) and Ctrl+C / `CTRL_BREAK_EVENT` (Windows) —
  see §7.9. These signals bypass the `q` path but must also trigger a
  state save before the process exits.
- On Filter popup `Apply` (preserves filter mid-session).

*Tab-switch saves are not implemented.* The earlier debounce design
(save after 500ms of inactivity on tab switch) added implementation
complexity for marginal benefit. The quit-path save and the Apply save
cover all practical mid-session-recovery scenarios. If the process is
killed without a signal (e.g. `kill -9` or OOM), the most recent
Apply-save or startup-load state is used on next restart — this is
acceptable.

Atomic write (matching AD-16): given the resolved filename
`tui_state-{config_hash}.toml`, serialize to a
`tempfile::NamedTempFile` created in the **same** directory, then call
`persist()` to replace the target. `persist()` is cross-platform safe
(atomic rename on POSIX, `MoveFileExW` on Windows — see §15.4).
Failures emit a `tracing::warn!` (which the AD-15 ring buffer captures
and the L-key log overlay shows) but never crash the TUI. There is no
`tui.log` file; logging is in-memory only.

### 16.4 Phase mapping (resolves prior contradiction; AD-8)

- **Phase 1b**: define `TuiPersistedState` struct with `#[serde(default)]`.
- **Phase 2**: full load-on-startup + save-on-quit + save-on-Apply
  wiring; Phase 2 verification explicitly tests round-trip restart.
- No further deferral. Earlier "MC3 — wiring moved to Phase 5" is
  abandoned.

---

## 17. stdout / stderr / Logging

### 17.1 Capture model (AD-10, AD-13, AD-15)

- Operations run through russh `SshPool` (no subprocess). Per-host
  command output is captured by the existing collector and surfaced
  to the TUI via the `ProgressSink` trait (§7.5) → `TuiEvent`s on
  the AsyncBridge channel.
- Tracing: `init_tracing()` registers a `Registry` with two layers —
  the existing `fmt` layer (writer wrapped in a `reload::Handle`)
  and an in-memory `RingBufferLayer` that pushes structured events
  into `Arc<Mutex<VecDeque<LogEntry>>>`. On TUI launch the fmt
  layer's writer is swapped to `std::io::sink()`; on TUI exit it is
  restored. No file logging, no subscriber re-init.

### 17.2 LogEntry & buffer

```rust
struct LogEntry {
    when:   std::time::SystemTime,   // wall-clock; use SystemTime::now() at capture
    level:  tracing::Level,      // ERROR / WARN / INFO / DEBUG / TRACE
    target: String,              // tracing target (module path)
    text:   String,
}

App.log_buffer: Arc<Mutex<VecDeque<LogEntry>>>   // capped at 500
```

**Overflow behavior:** When the 500-entry cap is reached, the oldest entry
is silently dropped (`VecDeque::pop_front` before each `push_back`). There
is no user-visible notification — the ring buffer is a diagnostic aid, not
an audit log. This is standard `VecDeque` ring-buffer semantics.

### 17.3 Display channels

Three places where users see operation/log information. None of
these consume per-line stdout/stderr — those are not events the
russh runtime currently emits (see Phase 8 for streaming).

1. **Progress popup** — populated from per-host `TuiEvent::HostStarted`
   / `HostCompleted` events. Failed hosts render the `detail` string
   (from the per-host failure path in `*_core`) in `theme.error`
   color. (Phase 3.)
2. **Status bar transient** — most recent `tracing::ERROR` or
   `tracing::WARN` line from the ring buffer shown for 5s in
   `theme.error`, then auto-clears. (Phase 3.)
3. **Log overlay** — `L` key opens scrollable overlay of the full
   ring buffer with level filtering. (**Post-MVP**, Phase 7. AD-11.)

---

## 18. Async Operations

### 18.1 AsyncBridge

```rust
enum TuiEvent {
    HostStarted(String),
    HostCompleted { host: String, status: HostStatus, duration_ms: u64, detail: String },
    // NOTE: StdoutLine / StderrLine intentionally absent in MVP.
    // The russh layer returns full stdout/stderr per command (not streamed line-by-line).
    // Per-line streaming requires the Phase 8 streaming-events refactor; until that
    // lands, the only per-host signal is HostCompleted with full detail attached.
    ProgressUpdate{ completed: usize, total: usize },
    OperationFinished(OpSummary),
    OperationCancelled,
    OperationError(String),
    SshAuthRequired { host: String, prompt: String }, // Phase 7
}
```

**`HostCompleted.detail` truncation:** The `detail` field carries a
per-host error or warning string from `*_core`. Before storing it in the
`TuiEvent`, callers must truncate to **3 display lines** (first 3 lines of
the string, joined with ` ↵ `, followed by `…` if more lines existed).
`unicode_width` must be used for line-length capping within each line.
This prevents a host that returns multi-line stderr from blowing out the
progress popup layout. The full detail is still available in the
`CommandReport` for the Results popup (which is larger and scrollable).

Channel: **`tokio::sync::mpsc::channel(1_024)`** (bounded channel).
1 024 capacity covers ~500 hosts with room for intermediate
`ProgressUpdate` events, prevents unbounded memory growth, and
has a nice power-of-2 alignment. In practice, typical deployments
have tens to low hundreds of hosts (each generates 2 events:
`HostStarted` + `HostCompleted`). If `send` returns a full-channel error, log
a `tracing::warn!` and drop the event — this is better than
unbounded memory growth. Main loop uses an **event-driven render
strategy** with a 50ms poll timeout:

```rust
// App::run() inner loop — pseudo-code
loop {
    // Drain async event channel first (non-blocking)
    while let Ok(ev) = tui_rx.try_recv() {
        dirty |= handle_tui_event(ev);
    }
    // Poll crossterm events with 50ms timeout
    if crossterm::event::poll(Duration::from_millis(50))? {
        let ev = crossterm::event::read()?;
        dirty |= handle_crossterm_event(ev);
    }
    // Only redraw if something changed
    if dirty {
        terminal.draw(|f| render(f, &app))?;
        dirty = false;
    }
}
```

This avoids the CPU waste of re-rendering every 50ms when nothing has
changed. `dirty` is set to `true` by any event handler that mutates
state. The 50ms timeout is also the maximum latency before
`TuiEvent::HostCompleted` or `OperationFinished` causes a repaint.
**Batched render on rapid events:** the `while let Ok(ev) = tui_rx.try_recv()`
loop drains *all* pending TuiEvents before the render step, so 100
`HostCompleted` events arriving simultaneously trigger exactly one
repaint — not one per event. This is the intended behavior.

**`OperationError` UI:** On receipt of `TuiEvent::OperationError(msg)`,
the progress popup transitions to an error variant: red header banner
`"⚠ Operation failed"`, the error message body, and a single
`[Dismiss]` button. This reuses the `OperationResults` popup component
with an error display mode — no separate popup type is needed.

### 18.2 Cancellation (AD-5 + AD-11)

```rust
struct RunningOp {
    handle: JoinHandle<()>,
    cancel: tokio_util::sync::CancellationToken,
    started_at: Instant,
}
```

- Esc on the progress popup calls `cancel.cancel()`.
- Ctrl+C triggers cancel + initiates graceful shutdown.
- Cancellation is **cooperative** (russh is fully async). It fires
  at the next `.await` point in the per-host future. Aborting the
  JoinHandle drops the russh channel, which sends `CHANNEL_CLOSE`
  and returns the slot to the pool — no hung child process to kill
  because there is no child process (AD-13).
- Hosts wedged in russh state (e.g. hung TCP read) are bounded by
  the existing `ctx.timeout` per-host deadline that the CLI also
  uses. Timeout firing is observable as `TuiEvent::HostCompleted`
  with `status = TimedOut`.
- The cooperative limitation is mentioned in the `?` help popup so
  users understand "Esc may take a moment".

**`OperationCancelled` UI behavior:** On receipt of
`TuiEvent::OperationCancelled`, the progress popup transitions to a
summary view: completed hosts show their results (✓ / ✗), incomplete
hosts are marked `"cancelled"` in grey. A `[Dismiss]` button is shown
(same as the error variant). This reuses the `OperationResults` popup in
a cancelled-summary mode — no separate popup type is needed.
**Contract requirement for `check_core`:** `check_core` must return a
`CommandReport::Check(...)` containing all completed host results up to
the cancellation point (not an empty or error report). The TUI derives
the cancelled-host list from the difference between targeted hosts and
the completed hosts in the report.

### 18.3 DB access pattern (AD-5, AD-16)

- Spawned op tasks build their own `Context` via
  `Context::from_tui_parts`, which calls
  `state::db::open(config.settings.state_dir.as_deref())?` — same
  resolution path as CLI. They never share `App.db`.
- The TUI render thread reads `App.db` synchronously between frames
  (`fetch_latest_snapshots`).
- Writes (snapshot inserts) happen from op tasks on their own
  connection; SQLite WAL mode handles concurrent reads.
- **WAL visibility and Checkout tab reload:** On receipt of
  `TuiEvent::OperationFinished`, the main loop must trigger a
  Checkout tab data reload. SQLite WAL makes writes from op tasks
  visible to the reader connection once the writer commits, but a
  stale page cache on `App.db` can delay visibility. To guarantee
  fresh data, set a `db_stale: bool` flag when `OperationFinished`
  arrives and **lazily** close and reopen `App.db` (drop the existing
  `rusqlite::Connection` and call `state::db::open(...)` again)
  only when the Checkout tab is actually rendered next. This avoids
  the latency of a connection setup on every `OperationFinished`
  event and eliminates the window where `App.db` is `None` (between
  drop and reopen) when the user is on a different tab.
  The actual data reload (`fetch_latest_snapshots`) is then called
  immediately after the reopen, before the first Checkout render.
  **Explicit Phase 3 requirement:** Phase 3 implementation must
  call `fetch_latest_snapshots` after the lazy reopen triggered by
  `OperationFinished` — not just on tab entry from startup — to
  ensure fresh snapshot data appears after each `check` run.
  `PRAGMA wal_checkpoint(PASSIVE)` is **not** used here: `PASSIVE`
  only attempts to checkpoint and gives no durability guarantee for
  the reader — it does not ensure the page cache is invalidated.
  **Reopen failure handling:** If `state::db::open(...)` fails (e.g.
  file deleted, permissions changed, disk full), retry once after a
  100ms async sleep. If the second attempt also fails, set
  `db_healthy = false` (Tier 2 degraded mode, §24.2). Additionally,
  bind the `r` key in the Checkout tab to a manual DB-reopen attempt:
  when `db_healthy = false` and `r` is pressed in the Checkout tab,
  attempt `state::db::open(...)` immediately; on success clear
  `db_healthy = false`, reload snapshot data, and surface a green
  status-bar message "Database reconnected".

### 18.4 Performance budget

These are the targets that guide implementation choices; they are not
enforced by automated benchmarks but should be checked manually:

| Metric | Target |
|--------|--------|
| First frame render after startup | < 100 ms |
| Single frame render time (steady state) | < 16 ms (≥ 60 fps feel) |
| Checkout tab table render with 1000 hosts | scrolling without perceptible lag |
| Filter popup groups list render | instant (< 5 ms) |
| Ring-buffer push contention (`Arc<Mutex<VecDeque<LogEntry>>>`) | negligible — tracing events are infrequent relative to frame rate |

If the Checkout host table grows past ~500 rows, consider switching
`HostTable` to a virtual/lazy renderer that only processes visible rows
rather than iterating the full list on every frame. **The `visible_range()`
method on `viewport.rs` (AD-20) already provides the slice bounds
`(scroll_y, scroll_y + visible_height)` — rendering code should iterate
only this slice from the start, giving O(visible) cost at any list size.**
This is slice-based rendering (not true virtual scroll with async data
loading), costs nothing to maintain once `viewport.rs` is implemented in
Phase 1a, and prevents the per-frame full-list iteration that would
otherwise degrade with 500+ hosts.

**Lazy tab rendering:** Only the active tab is rendered each frame.
Inactive tabs are skipped entirely. Each tab carries a `dirty` flag that
triggers a full redraw when the tab becomes active via a switch. This
avoids the CPU cost of rendering the Checkout table (potentially hundreds
of rows) every 50ms while the user is on the Operate tab. Implement by
gating each tab's render call behind `if active_tab == tab_id { ... }`.

---

## 19. Implementation Phases

**MVP scope (AD-11): Phases 0 through 3.** After Phase 3, the TUI is
launchable, browses checkout data, runs `check`, and persists user
intent. Phases 4–7 are independently demo-able and mergeable.

**Phase dependency graph (critical path):**

```
Phase 0 ──► Phase 0.5 ──► Phase 0.7 ──► Phase 1a ──► Phase 1b ──► Phase 2 ──► Phase 3  (MVP)
                                                                                   │
                                                                                   ├──► Phase 4
                                                                                   │
                                                                                   └──► Phase 5 ──► Phase 6 ──► Phase 7
```

Phases 4, 5, 6, 7 are post-MVP and can be developed in parallel after
Phase 3 merges. Phase 8 (optional streaming) is independent and can
be inserted anywhere after Phase 3.

**Documentation rule (applies to every phase):** Any phase that changes
user-visible behavior (new binary, new key binding, new subcommand
output, changed defaults) **must** include a documentation-update item
that updates `README.md` and/or `AGENTS.md` before the phase is
considered done.  Each phase checklist below marks the specific docs
files that need updating.  A phase PR that changes user-visible behavior
without updating docs is **not mergeable**.

### Phase 0 — Branch, deps, bin layout

1. Branch `feat/tui` from `main`.
2. Add `toml_edit = "0.22"` to `Cargo.toml` (always-on).
3. Confirm `[features].default = []` (AD-1).
4. Add the second `[[bin]] ssync-tui` with `required-features = ["tui"]`
   and the `argv[0]` runtime gate in `main.rs` (AD-17).
5. **Remove `tracing-appender` and update `tracing-subscriber` features**
   in a single `Cargo.toml` change to avoid a transient CI break:
   remove `tracing-appender` (AD-4 says "no `tracing-appender`"; **confirmed
   safe** — `grep -r tracing_appender src/` returns no results, meaning no
   `src/` code references it); simultaneously update `tracing-subscriber` to
   declare `features = ["env-filter", "reload"]` (the `reload` feature is
   required for AD-15 fmt-layer writer swap — see AD-4). If another feature
   requires `tracing-appender` in the future, update AD-4 with justification
   before landing.
6. CI matrix: `cargo build --bin ssync` (headless) **and**
   `cargo build --bin ssync-tui --features tui`; release CI uploads
   both artifacts under their built names.
7. **Add `feat/tui` CI pipeline configuration:** update the CI workflow
   to trigger on every push to `feat/tui` and run the full matrix:
   `cargo test`, `cargo test --features tui`,
   `cargo clippy --all-targets`, `cargo clippy --all-targets --features tui`,
   and `cargo fmt --check`. This prevents regressions from accumulating
   across the long development window.
8. **Rollback strategy:** each phase is developed on a short-lived branch
   (e.g. `feat/tui-phase-0.5`) merged into `feat/tui` only after its
   validation checklist passes (golden-diff for CLI regressions, all
   new tests green). Keep `feat/tui` rebased on `main` weekly. If a
   phase introduces a regression on `feat/tui`, revert the merge commit
   on `feat/tui` — the branch history is the rollback. Document this
   workflow in `AGENTS.md`.
9. **Docs:** Update `README.md` to document the two-binary release
   model (`ssync` vs `ssync-tui`) and `AGENTS.md` build commands
   to include the `--bin ssync-tui --features tui` variant.

### Phase 0.5 — Schema & config save  *(pre-TUI refactor milestone, part 1)*

1. Add `name: Option<String>` (pure display label per AD-18; added to
   `CheckEntry` and `SyncEntry` only — `HostEntry.name` is unchanged)
   in `src/config/schema.rs` (`#[serde(default)]`).
2. Add `id: String` (stable entry identity per AD-18; added to
   `CheckEntry` and `SyncEntry` only) in `src/config/schema.rs`
   (`#[serde(default)]`). Default is empty string (legacy config
   fallback to index-based matching). Generation: `blake3(name_bytes +
   timestamp_nanos)[..4].to_hex()` — called once at entry creation.
3. **Strip BOM in `config::app::load()`:** before parsing, strip any
   leading UTF-8 BOM (`\xEF\xBB\xBF`) from the file content. This is
   a one-line fix that prevents confusing parse errors for Windows users
   who save config with a BOM-emitting editor.
4. Update `config::app::save` per §15.4: keep `inject_config_comments`
   on first-create, use `toml_edit::DocumentMut` round-trip on edit.
   **Scope for this phase:** implement `apply_config_to_doc` for
   **scalar fields only** (`[settings]` block + `name`/`id` display labels).
   Array-of-tables add/remove/reorder (`[[host]]`, `[[check]]`,
   `[[sync]]`) is deferred to Phase 7. In this phase, only tests
   **T-WB-2** (preserve unknown key) and **T-WB-3** (inline comment
   survives scalar edit) are implemented — these are meaningful
   validations of the scalar-only scope. Tests **T-WB-1** (delete
   `[[check]]`) and **T-WB-4** (delete `[[host]]`) are explicitly
   deferred to Phase 7 and **must NOT be added** as pending/ignored
   tests in Phase 0.5 (passing vacuously provides no signal and may
   mask regressions when array mutation is added).
5. Add `pub fn resolved_state_dir(override_dir: Option<&std::path::Path>) -> Result<PathBuf>`
   to `src/state/db.rs`. Refactor the existing `open()` to call it
   internally. This is the single source of truth for state-dir
   resolution (AD-16).
6. **Enhance `config::app::resolve_path`** to expand `~` via
   `dirs::home_dir()` substitution so that explicit and default paths
   hash identically (AD-16 config_hash stability — see §16).
   **Verify `dirs` is a direct dependency in `Cargo.toml`** (not merely
   transitive) before this step — add it explicitly if missing.
   **CLI regression test:** before changing `resolve_path`, add a
   test that invokes `ssync check -c ~/custom.toml` (or the equivalent
   path resolution) with a known `~`-prefixed path and asserts the
   resolved absolute path is correct. This guards against accidentally
   changing resolution behavior for existing CLI users who pass `~/`
   paths today.
7. Update `Cargo.toml`: add `tokio-util = { version = "...", features = ["sync"] }`
   under the `tui` feature dependencies; add `unicode-width = "0.1"`
   (always-on, needed by §11 and AD-18 display-width truncation — see
   §11 Unicode display width note). (`tracing-subscriber` features
   update was already done in Phase 0 step 5.)
8. Validate: existing `cargo test` passes; round-trip test added
   (load → save → diff is no-op on an unchanged config); BOM-stripped
   config parses identically to a BOM-free equivalent.

### Phase 0.7 — check_core extraction (AD-14)  *(pre-TUI refactor milestone, part 2)*

**Required precondition for Phase 3. MVP scope: `check_core` only.**
`run_core`, `exec_core`, `sync_core` are extracted in Phases 5, 5, 6 respectively.
`checkout_core` is extracted in Phase 4. No TUI code yet.

> **Design-first requirement (hard gate — no coding until complete):**
> The current `commands::check::run`
> handles at least 9 concerns in sequence: `resolve_hosts`, per-host
> config mapping, `SshPool::setup`, unreachable-host detection,
> `tokio::spawn` per-host concurrency loop, DB writes, `Summary`
> aggregation, `output::printer` calls, and `write_report`. Before
> writing any code, produce a pseudocode-level design that maps each
> step to either (a) stays in `check_core` with `ProgressSink`
> callbacks, or (b) moves to the thin `run()` CLI wrapper. Key
> constraint: **DB writes stay in `check_core`** because the TUI also
> needs them. `output::printer` calls move exclusively to `run()`.
> **This design must include a complete line-level attribution table**
> listing every distinct code block / concern in the current
> `check.rs::run` and its assigned destination (`check_core` vs
> `run()`), including where `serde_json::Value` construction for
> `HostResult.output` belongs — this resolves the `CheckHostResult`
> field design before the split is implemented.
> **Gate:** the attribution table must appear in the **PR description**
> for the Phase 0.7 PR. It does not need to be committed as a separate
> document in the repository, but it must be present in the PR before
> any review or merge. A PR that implements the split without this table
> in the description is not mergeable.

1. Define `trait ProgressSink`, `HostStatus`, and a **`CommandReport`
   enum with the `Check` variant only** in **`src/commands/report.rs`**
   (not `output/report.rs` — placing these types in the output layer
   would create a reverse dependency where `*_core` functions depend on
   the output module, violating separation of concerns; `output/report.rs`
   is a thin consumer that imports from `commands::report` and provides
   the CLI `write()` formatting function).
   Other variants (`Run`, `Exec`, `Sync`, `Checkout`) are added in
   their respective extraction phases (5, 5, 6, 4) to keep the
   serialisation contract surface small and CLI regression risk low:
   ```rust
   pub trait ProgressSink: Send + Sync {
       fn host_started(&self, host: &str);
       fn host_completed(&self, host: &str, status: HostStatus, detail: &str, ms: u64);
   }

   /// Typed result returned by *_core functions.
   /// The TUI pattern-matches on this; the CLI wrapper calls
   /// output::report::write(&report, output) as before.
   /// NOTE: Only the Check variant is defined here.  Run/Exec/Sync/Checkout
   /// variants land alongside their _core extraction phases.
   pub enum CommandReport {
       Check(CheckReport),
       // Run(RunReport),        ← Phase 5
       // Exec(ExecReport),      ← Phase 5
       // Sync(SyncReport),      ← Phase 6
       // Checkout(CheckoutReport), ← Phase 4
   }

   /// Per-command payload struct.  Carries typed host results (NOT
   /// serde_json::Value).  CLI serialisation uses serde on CommandReport;
   /// the existing --out JSON path is preserved by deriving Serialize.
   pub struct CheckReport  { pub hosts: Vec<CheckHostResult> }
   ```
   The existing `OperationReport` / `HostResult` types **stay** for the
   current CLI `--out` serialisation path but are treated as a legacy
   serialisation adapter.  New TUI code never touches `HostResult.output:
   serde_json::Value` directly — it receives a `CommandReport` variant.
2. Split `commands::check::run` into:
   - `check_core(ctx: &Context, progress: Option<&dyn ProgressSink>) -> Result<CommandReport>`
     (returns `CommandReport::Check(...)`)
   - the existing `run(...)` becomes a thin wrapper that constructs
     a `PrinterSink` and forwards the report to `output::report`.
3. In `commands::checkout`, make `fetch_latest_snapshots`,
   `DisplayColumns`, and `extract_*` helpers `pub(crate)`.
   **Do not extract `checkout_core` yet** — that happens in Phase 4.
4. Wire `ProgressSink` callbacks into `check_core`'s host loop
   (before/after each per-host future and for unreachable hosts from
   `SshPool::setup`). **Do not add `ProgressSink` to `collect_pooled`**
   — `collect_pooled` is a per-host function; the integration layer is
   `check_core` (see §7.5 for the rationale). CLI `run()` passes a
   `PrinterSink`; TUI `AsyncBridge` passes its own `ProgressSink` impl.
5. Validate: `cargo test` + manual `ssync check --all` against the
   existing test fixtures produces identical stdout to pre-refactor.
6. **Validate `Context::from_tui_parts` fields:** before committing
   the Phase 0.7 code, do a line-by-line audit of every field in the
   real `Context` struct (`src/commands/mod.rs`) against the
   `from_tui_parts` constructor (see §6.4). Include this field
   attribution in the Phase 0.7 PR description (alongside the
   `check_core` split table). A PR implementing `from_tui_parts`
   without this audit in the description is not mergeable.
7. Validate: a unit test invokes `check_core` with a stub
   `ProgressSink` and asserts the recorded events match a known
   per-host outcome list. **Prerequisite for this test:** introduce
   `trait SshPoolProvider` (or an equivalent injection point) so that
   `check_core` accepts a mock `SshPool` that returns pre-recorded
   `CollectionResult` fixtures without network I/O. Without this trait
   the stub test cannot be written — `SshPool::setup` requires a live
   SSH connection. See §20.1.1 for the mock boundary design.

### Phase 8 — (Optional, post-MVP) Streaming session events

**Not required for MVP.** Documented here so Phase 3's progress popup
sets correct expectations.

The current russh execution path
(`src/host/session_pool.rs::ChannelExec::exec_command` and friends)
returns full stdout/stderr only after the remote command completes.
There is no per-line callback. As a consequence:

- MVP progress popup updates **per host**, not per line.
- `TuiEvent::StdoutLine` / `TuiEvent::StderrLine` are intentionally
  absent from the AsyncBridge enum (see §18.1).

If finer-grained streaming is needed later, this phase adds:

1. A line-streaming variant of `exec_command` in `session_pool.rs`
   that takes a per-line callback (`FnMut(StreamKind, &str)`).
2. A new `ProgressSink::output_line(host, kind, line)` method.
3. AsyncBridge gains `TuiEvent::OutputLine { host, kind, line }`.
4. Progress popup renders incrementally.

This is post-MVP because (a) the existing CLI doesn't need it, and
(b) russh stream rechunking + UTF-8 boundary handling is non-trivial
and shouldn't gate the first usable TUI release.

### Phase 1a — Skeleton + non-interactive guard + minimal Checkout tab

**MVP-only Checkout scope:** latest-snapshots host table + Filter
popup integration (Phase 2). History toggle, since-input, export, and
detail popup are deferred to Phase 6 / Post-MVP because the CLI side
of `commands::checkout` does not yet support them either.

1. `cli.rs`: `command: Option<Commands>`, `subcommand_required = false`.
2. `main.rs`: `#[cfg(feature = "tui")] mod tui;` and
   `run_or_fallback` dispatch with binary-name + `IsTerminal` +
   `TERM` checks (§4, AD-3, AD-17). Exit codes: 1 / 2 / 0 per AD-3.
3. `init_tracing` rewired to install the ring-buffer layer + reload
   handle (AD-15). When `ssync` (non-tui) binary runs, behavior is
   unchanged (fmt layer stays attached to stderr).
4. `src/tui/terminal.rs`: `TerminalGuard` + `install_panic_hook()`.
5. `src/tui/entry.rs`: `pub async fn run_or_fallback()` — performs
   §4 TTY / TERM detection and exits 1/2 or proceeds to launch TUI.
   `src/tui/mod.rs` re-exports it as the public surface.
   Inner `run()` (private to `tui::app`): install hook,
   install guard, swap fmt writer to `sink`, run app loop, restore
   fmt writer on exit.
6. `App` struct with crossterm init, render loop, tab switching
   (`1`/`2`/`3`, `q`, Esc).
7. Tab bar + empty Config/Operate placeholders + status bar with red
   `app.error`.
8. Terminal-size guard: < 80×24 → render "Terminal too small
   (need 80×24+)".
9. `Context::from_tui_parts` added to `commands/mod.rs` (AD-16
   resolution).
10. Build Checkout tab fresh:
    - Delete the dead `run_tui()` PoC in `commands/checkout.rs`
      (the `pub(crate)` changes were already done in Phase 0.7).
    - **`src/tui/components/scrollable_list.rs` + `viewport.rs`**:
      implement cursor / scroll decoupling (§11) here, not Phase 1b —
      the Checkout host table in this phase already needs scrolling.
      Phase 1b reuses these components for zone navigation.
    - Render host table (latest snapshots only) with cursor / scroll
      decoupling via the new `scrollable_list` component.
11. Register signal handlers (§7.9): on Unix, install `tokio::signal`
    handlers for `SIGHUP` and `SIGTERM` that trigger graceful shutdown
    (save state + restore terminal); on Windows, use
    `tokio::signal::ctrl_c()` for Ctrl+C / `CTRL_BREAK_EVENT` (which
    `tokio` handles natively without extra dependencies). Windows
    `CTRL_CLOSE_EVENT` (terminal close button) handling via
    `SetConsoleCtrlHandler` requires the `windows-sys` crate and is
    deferred to post-MVP — add a `// TODO(post-MVP windows): CTRL_CLOSE_EVENT`
    comment. This is required in Phase 1a so state saving added in
    Phase 2 is automatically covered.
12. **Minimal `?` help popup:** add a simple centered popup (~20 lines)
    activated by `?` that lists the global keys already bound in this
    phase: `1`/`2`/`3` (tabs), `Tab` (cycle zones), `q` (quit),
    `Esc` (close popup / cancel), `i` (info), `?` (this help).
    Keybinding rows are added to this popup as each phase introduces new
    bindings. The full Phase 6 help popup replaces this; until then,
    this popup prevents "what does this app do?" confusion from Phase 1a
    onward. (~20 lines of code; `PopupState::Help { content: String }`).
13. Validate: `ssync-tui` on TTY launches TUI; `ssync-tui | cat`
    prints help and exits 2 (no ANSI escapes leak into the pipe);
    `ssync` (the non-tui binary) prints "TUI not available" and
    exits 1; `ssync check --all` and other CLI subcommands behave
    identically to pre-refactor.

**Out of scope for this phase:** Config tab content (placeholder only),
Operate tab content (placeholder only), filter popup, persistence,
operation execution, adaptive arrow navigation machinery.

### Phase 1b — Navigation core

1. App-owned `FocusPath { zone, breadcrumb }`. Tabs are passive
   renderers receiving `focus_zone` as a render parameter.
2. ↑↓ within-zone navigation; **Tab/Shift+Tab same-level cycle** (never
   crosses zone or level boundaries — matches §8.2 canonical rule).
3. Adaptive arrow navigation (§8.3) implemented via `Focusable`
   trait. **NOTE: this is full machinery; bundled in MVP because all
   downstream phases depend on it.**
4. Breadcrumb update on zone change only.
5. `scrollable_list.rs` + `viewport.rs` cursor / scroll decoupling
   components are **already landed in Phase 1a**; Phase 1b wires them
   into the navigation model for Config and Operate tab zones.
6. `theme.rs` palette landed; verify 16-color compatibility per §10.
7. `TuiPersistedState` struct defined (no wiring yet).

Validation: full keyboard navigation works in Checkout tab with zone
transitions; focus visual states correct on inactive/active/focus
elements; **unit tests for `Focusable::axis_freedom` /
`at_boundary` / `escape_to_parent` (§20.2).**

**Out of scope for this phase:** popup system, filter popup, persistence
wiring, Operate/Config tab content.

### Phase 2 — Popup system + Filter + Persistence wiring (AD-8)

1. `components/popup.rs` (centered overlay + Clear).
2. `components/target_filter.rs` with full nav. **Note on Shell option:**
   the filter popup component renders Shell mode only when invoked from
   the Operate tab; when invoked from Checkout tab it omits the Shell
   option (§13).
3. `components/input_field.rs` with `InputMode` isolation.
4. Wire filter popup into **Operate tab only**. Checkout tab filter
   wiring is **deferred to Phase 6** (alongside history/since features
   where filtering has more practical value — MVP Checkout only shows
   latest snapshots which rarely need filtering).
5. **Full persistence wiring**: load on startup (with §16.2
   validation), save on quit, save on filter Apply, atomic write.
6. Validate: filter applied → persisted → restart restores. Edit
   `config.toml` externally to delete a group → restart → invalid
   group dropped silently from filter.

**Out of scope for this phase:** Checkout tab filter wiring (Phase 6), log overlay, Config
tab content.

### Phase 3 — Operate tab (`check` only) [MVP COMPLETE]

Depends on Phase 0.7's `check_core` already existing.

1. `tabs/operate_tab.rs`: L1 op selector + L1 target row + L2 params
   + L1 `[Execute]` button.
2. `async_bridge.rs`: `TuiEvent` channel + `RunningOp` with
   `CancellationToken`. AsyncBridge implements `ProgressSink` and
   forwards events to the channel.
3. Wire `[Execute]` for `check`: build `Context::from_tui_parts`,
   spawn `check_core(&ctx, Some(&bridge))` on a tokio task, render
   progress popup from inbound events, present `OperationResults`
   popup from the returned `CommandReport::Check(...)`.
4. Applicable Entries panel (read-only, max 6 rows + scroll).
5. Progress popup with per-host status updates from `host_completed`
   events. Failed hosts show their failure detail (from `CheckReport`)
   inline with red text — no stderr stream concept needed.
   **Scrollable host list:** the popup embeds a `scrollable_list`
   component (Phase 1a) with a fixed visible height of 8 rows. When
   more than 8 hosts are targeted the list scrolls; the list
   auto-scrolls to the last completed host as events arrive, then
   allows manual ↑↓ navigation once the operation ends.
   `OperationError` events transition the popup header to a red error
   banner with a `[Dismiss]` button (reuses `OperationResults` error
   variant — see §18.1).
6. `i` key toggles contextual info popup.
7. Confirm filter popup is wired to Operate tab (landed in Phase 2):
   - On Apply: update `operate_state.target_mode` and immediately
     refresh the `target_host_count` shown in the "Target: … (N
     hosts)" L1 row by re-evaluating the filtered host set against
     `App.config.hosts`.
   - The L1 target row must re-render within the same frame as the
     Apply event to avoid a stale count flash.
   - Persisted state: `target_filter.*` fields are shared by
     Checkout and Operate (same struct, already persisted in Phase
     2); no additional persistence fields needed.
8. Validate: execute `check` from TUI on real hosts; results update
   per-host; Esc on progress popup cancels (cooperative — may take
   up to `ctx.timeout` for wedged hosts); Ctrl+C aborts cleanly with
   terminal restored.
   **Sleep/wake test (manual):** start a `check` with a 30s timeout,
   suspend the laptop for 60s (lid close), resume — the operation either
   completes with per-host `TimedOut` results or finishes normally; the
   TUI remains responsive throughout and does not hang. (Per-host timeout
   `ctx.timeout` covers this implicitly, but verify empirically.)
9. Validate: open `f` filter from Operate tab, change groups, Apply →
   host count in L1 target row updates immediately; close and reopen →
   selection is retained; quit and restart → filter is restored from
   persisted state (Checkout and Operate share the same stored filter).
10. **Validate concurrency guards:**
    - Press `[Execute]` while a `check` operation is already in progress
      → button press must be a no-op with a Tier 3 status-bar message
      "Operation already running" (not silently ignored).
    - Press Esc (cancel) and then immediately Enter (execute) in rapid
      succession → `Enter` on `[Execute]` while `RunningOp` is non-None
      (cancellation may be cooperative/pending) must be a no-op with
      the same Tier 3 feedback, regardless of cancellation state.
11. **Validate Checkout tab reload after `check`:** after `check`
    completes and `OperationFinished` is received, switch to the
    Checkout tab and verify the snapshot table shows updated data from
    the just-completed check (the `db_stale` → lazy-reopen →
    `fetch_latest_snapshots` chain must fire).
12. **Docs:** Update `README.md` with TUI launch instructions (`ssync-tui`),
    keybinding quick-reference (tabs, `f`, `q`, `i`), and a note on the
    two-binary release model if not already added in Phase 0.  Update
    `AGENTS.md` with the `ProgressSink` / `AsyncBridge` patterns for
    future contributors.

**Out of scope for this phase:** run/exec/sync operations, Config tab
content, log overlay, per-line streaming.

**MVP cutline.** Stop here for the initial PR. Subsequent phases ship
incrementally.

### Phase 4 — Config tab (read-only) + external editor

> **Rationale for moving Config read-only earlier:** The external editor
> 4-stage flow (terminal suspend → spawn → wait → restore) exercises the
> most critical terminal-lifecycle path outside of initial launch and panic
> handling. Discovering suspend/resume bugs here (Phase 4) is far safer
> than discovering them in Phase 7 alongside form-focus complexity. The
> `toml_edit` dirty-state machinery is also validated early.

1. `tabs/config_tab.rs`: 3-level model (section → entry → field), **all
   read-only display** (no inline editing yet).
2. Zone neighbour table (§8.6 Config): Sidebar ↔ FieldTable via ←→.
3. `E` key: full §7.4 external editor 4-stage flow including dirty check
   (nothing is dirty in read-only mode, so Stage 0 always passes through;
   validate the no-op path here so it's ready for Phase 7).
4. `config_mtime` baseline stored on Config tab load; after editor
   return, if mtime changed the config is reloaded and a yellow banner
   "Config reloaded" flashes for 2s. This is the **global** config-change
   detection mechanism — later phases (Phase 7 inline edit, Phase 5 sync
   config_mtime) reuse the same codepath.
5. Validate: navigate all 3 levels with ↑↓ ←→; breadcrumb updates on
   zone change only; `E` suspends TUI, editor opens, TUI restores cleanly
   on exit (test with both `$VISUAL` set and unset); terminal is in correct
   state after editor returns even if editor exits non-zero.
6. **Docs:** Update `README.md` to document the Config tab navigation
   (read-only browsing, `E` to edit externally).

**Out of scope for this phase:** inline config editing, log overlay, SSH auth
popup, `?` help popup (keybindings still changing in Phases 5–6).

### Phase 5 — Operate: run / exec

1. Text input with `InputMode` isolation (already built in Phase 2).
2. `run` / `exec` parameter panels (command, script, sudo, keep, yes).
3. Op wrappers extending the Phase 3 pattern.
4. Validate: execute run/exec; per-host output displayed in results
   popup after each host completes (per-host granularity, matching
   Phase 3 `check` pattern — no line-by-line streaming until the
   optional Phase 8 refactor).
5. **Docs:** Update `README.md` keybinding table to include `run` / `exec`
   parameter fields and `CommandReport::Run` / `::Exec` variant notes.

**Out of scope for this phase:** per-line streaming events
(`TuiEvent::OutputLine`); those require Phase 8.

### Phase 6 — Operate: sync (two-mode) + `?` help popup

1. Two-mode params panel (Config entries vs Ad-hoc files **list**
   editor — never comma-separated).
2. Applicable Entries panel for sync (bounded + scroll).
3. Source conflict detection per the formal rule below.
4. Ad-hoc mode banner when restoring `sync_mode = adhoc`.
5. `config_mtime` invalidation on Operate tab when config changes
   externally (reuse the Phase 4 global detection mechanism).
6. `?` keybinding help popup (lands here because the keybinding set is
   now stable after Phases 0–6).

Source conflict rule:

```
⚠ warn iff:
  2+ matching entries have DIFFERENT explicit (non-None) sources
  AND their target host sets OVERLAP (group overlap or shared host)
  AND the global source override is empty.

Additional advisory warnings (independent):
  ⚠ source not in filter   — configured source ∉ matched hosts
  ⚠ auto-detect ambiguous  — 2+ entries with source=None and overlapping targets

Preview labelled: "Approximate — runtime resolution may differ"
```

**Out of scope for this phase:** Config inline edit, log overlay.

6. **Docs:** Update `README.md` with `sync` two-mode usage, ad-hoc banner
   behaviour, and the `?` help popup keybinding reference.

### Phase 7 — Config tab (editable) + log overlay + auth popup

> Inline scalar edit (Case A) is the first item here; it arrives no
> later than this phase. Complex template forms (Case B) and Vec
> sub-editors (Case C) follow in the same phase.

1. Case A inline scalar edit (§15.1): `e`/`Enter` on a scalar field
   activates `InputMode`; `Enter` commits, `Esc` cancels; dirty flag
   `*` appears in status bar; `config_dirty` blocks navigation away
   with a "Save or discard?" prompt.
2. Case B composite entry template form (§15.2).
3. Sub-editor lifecycle for Vec fields (§15.3).
4. 3-way radio for `propagate_deletes` (inherit / yes / no).
5. Group / host pickers (reuse Filter popup widgets where possible).
6. `S` save with `toml_edit` round-trip; dirty flag cleared.
7. **Log overlay** (`L` key) showing full `log_buffer`.
8. SSH auth-required popup (M2): ⚠ **Design required before
   implementation — this is a hard gate equivalent to the Phase 0.7
   attribution-table gate. No coding may begin until a separate ADR
   is written, reviewed, and committed.** The russh-based transport
   does not expose a subprocess stdin; interactive auth
   (keyboard-interactive, passphrase prompts) surfaces through
   russh's `ClientHandler` auth callbacks, not through
   `std::process::Child::stdin`. The `SshAuthRequired` `TuiEvent`
   variant (§18.1) is reserved. **The ADR must specify:** (a) which
   russh callback hooks are used; (b) the `tokio::oneshot::channel`
   handshake for forwarding credentials from the TUI popup back to
   the russh callback; (c) the timeout and cancellation behavior if
   the user does not respond; (d) security constraints (credential
   lifetime in memory). A PR that implements any part of the SSH auth
   popup without the committed ADR is not mergeable.
   Post-design expected flow: auth callback sends event → TUI shows
   masked-input popup → user submits → value forwarded via oneshot →
   russh callback resolves; cancellation via `CancellationToken` still
   works.
9. Validate: full config editing round-trip; comments preserved;
   external editor flow (Stage 0 dirty check fires when applicable).
10. **Docs:** Update `README.md` with inline Config editing instructions
    (`e`/`S`, `a`/`d`, `L` log overlay) and note the `toml_edit`
    comment-preservation guarantee and its known limitation (deleted
    entries lose attached comments).

**Out of scope for this phase:** per-line streaming (Phase 8), theme
override, undo/redo. SSH auth popup (item 8) is blocked until its ADR
is written.

---

## 20. Automated Tests

Manual verification (§21) is not enough. The following test categories
are **required** to land alongside the phases that introduce them.

### 20.1 Parser & schema (Phase 0.5)

- `serde::Deserialize` round-trip for `CheckEntry` / `SyncEntry`
  with and without `name` and `id` fields (legacy config without
  either field must load with `name = None`, `id = ""`).
- New entry creation generates a non-empty `id` (8 hex chars).
- `toml_edit` write-back: load existing config with comments → save
  with no changes → file is byte-identical (proves comments survive).
- `toml_edit` write-back: load → modify one scalar → save → only the
  changed key differs; comment block preserved.
- First-create path: `save()` to a non-existent path produces a file
  that includes the `inject_config_comments` guidance block.
- **T-WB-1** (delete `[[check]]`): load config with two `[[check]]`
  entries (first has an inline `# comment`); remove first entry from
  `AppConfig`; assert saved file has exactly one `[[check]]`, the
  comment is absent, all other keys are byte-identical to original.
- **T-WB-2** (preserve unknown key): load config with
  `[settings]\nunknown_future_option = true`; save with unchanged
  `AppConfig`; assert `unknown_future_option` survives in output.
- **T-WB-3** (inline comment survives scalar edit): load config with
  `max_concurrency = 10  # max 50`; mutate only `max_concurrency`
  to `20`; assert saved file contains `max_concurrency = 20  # max 50`.
- **T-WB-4** (delete `[[host]]`): load a config with two `[[host]]`
  entries (second has an inline `# comment`); call
  `apply_config_to_doc` with only the first host retained; assert
  the saved file: contains exactly one `[[host]]`; the comment is
  absent; all other blocks are byte-identical.  Confirms rule 2
  applies uniformly to all array-of-tables types.

### 20.1.1 Command-core refactor (Phase 0.7)

- `check_core` invoked with stub `ProgressSink` and a fixture
  `Context` returns a `CommandReport::Check(...)` whose host list and
  statuses match a recorded golden output.
- The `CheckReport` host entries carry typed fields (not
  `serde_json::Value`); assert specific fields directly.
- `commands::check::run` (the thin wrapper) called against the same
  fixture produces stdout byte-identical to pre-refactor (golden
  file diff in `tests/cli/check_golden.txt`).
- `run_core` / `exec_core` tests (and the corresponding
  `CommandReport::Run` / `::Exec` variants) are added in Phase 5.
- `sync_core` tests (and `CommandReport::Sync`) are added in Phase 6.
- `checkout_core` tests (and `CommandReport::Checkout`) are added in
  Phase 4.

**Mock SSH boundary:** `check_core` calls `SshPool::setup` internally,
which requires a real SSH connection. Unit tests for `check_core` must
therefore mock at the `SshPool` boundary. The recommended approach is
to introduce a `trait SshPoolProvider` (or equivalent) that
`check_core` accepts as a parameter, allowing tests to inject a
`MockSshPool` that returns pre-recorded `CollectionResult` fixtures
without any network I/O. Integration tests that require real SSH
connectivity should be gated with `#[ignore]` and documented as
requiring a local test SSH server (e.g. a Docker container running
`openssh-server`). CI runs only unit tests by default.

### 20.2 Focus state machine (Phase 1b)

- `axis_freedom()` returns expected enum for each component type.
- `at_boundary(dir)` correctness for: empty list, single-item list,
  cursor at start, cursor at end, cursor in middle.
- `escape_to_parent` table-driven tests:
  - Y-only at boundary + ↑ → escapes
  - X-only at boundary + → → escapes
  - XY in middle + any arrow → does not escape
  - `None` axis-freedom + any arrow → escapes immediately
  - Popup root absorbs all escape attempts (no leak through L4)
  - Tab Bar wraps on ←→ at extremes
- Coverage target: ≥ 80% of `src/tui/components/{focus,viewport}.rs`.

### 20.3 Persistence round-trip (Phase 2)

- Save default `TuiPersistedState` → load → equal.
- Save with filter set → load → filter restored.
- Load with `groups` containing a name not in current config →
  validation drops it without panic.
- Load malformed TOML → returns defaults + logged warning, never panics.
- Atomic write: simulate `persist()` failure (read-only dir) → caller
  gets warning, prior file untouched.

### 20.4 Target applicability (Phase 3) — Formal rule

The applicability rule (formerly "§A2") is a unit-testable pure
function. **Canonical definition:**

```text
entry matches the current operation iff:

  match(entry, target_mode) where:
    target_mode = All        → entry.enable_all == true
    target_mode = Hosts(H)   → entry.enable_hosts == true
    target_mode = Groups(G)  → entry.groups is non-empty
                                AND entry.groups ∩ G is non-empty
    target_mode = Shell(S)   → entry applies to all hosts whose
                                detected shell is in S, gated by
                                enable_hosts == true (Shell mode is
                                a host filter, not a group filter)
```

Display labels in the Applicable Entries preview:
- `groups: []` → `(unscoped — applies only via --all / --hosts modes)`
- `enable_all = false` → `(excluded from --all mode)`
- `enable_hosts = false` → `(excluded from --hosts mode)`

Implementation must be a pure function:

```rust
fn entry_matches(entry: &impl ApplicableEntry, mode: &TargetMode) -> bool { ... }
```

Cases:
- `groups=[]` + mode=Groups → false
- `groups=["web"]` + mode=Groups(["web","db"]) → true
- `enable_all=true` + mode=All → true
- `enable_all=false` + mode=All → false
- mode=Hosts requires `enable_hosts=true`
- All combinations table-tested.

### 20.5 Entry-point gating (Phase 1a)

- `cargo build --bin ssync` + run with no subcommand → stderr
  contains "Interactive TUI not available", **exit 1**.
- `cargo build --bin ssync-tui --features tui` + run with no
  subcommand and stdout piped to `cat` → clap help, **exit 2**, no
  ANSI escape sequences in captured output (assert absent).
- Same `ssync-tui` binary with `TERM=dumb` env override → "Terminal
  does not support TUI", **exit 2**.
- `argv[0]` rename test: copy `ssync-tui` to `ssync` and invoke →
  must take the "TUI not available" path (exit 1) regardless of the
  `tui` feature being compiled in.

### 20.6 Terminal guard (Phase 1a)

- Test that `TerminalGuard::Drop` is invoked on:
  - normal return
  - `?` early return
  - panic (use `std::panic::catch_unwind` in test)
- After drop, `crossterm::terminal::is_raw_mode_enabled()` returns
  false. (Skipped on platforms where this introspection is
  unavailable; covered by manual test.)
- Panic hook: trigger a panic inside the closure → assert raw mode
  disabled before backtrace prints (verified via captured stderr).

### 20.7 State path & tracing layer

- `state_dir` resolution: with `config.settings.state_dir =
  "/tmp/X"`, `tui_state-{hash}.toml` lands under `/tmp/X/`, **not**
  under `dirs::state_dir()`. Same path used by `state::db::open`.
- Two distinct config paths produce two distinct `tui_state-*.toml`
  files in the same state dir; loading one never sees the other.
- Tracing ring buffer: a `tracing::info!()` call after `tui::run()`
  starts appears in `App.log_buffer` and does **not** appear on
  stderr (writer was swapped to `sink`). After TUI exit the writer
  is restored and a subsequent `tracing::info!()` reaches stderr
  again.

### 20.8 CI

`cargo test --features tui` runs all of the above. A separate
`cargo test` (no features) verifies the headless build still passes
its existing tests. Both runs are required to be green before merge.

Additionally, the CI pipeline must include:
- `cargo clippy --all-targets --features tui -- -D warnings`
- `cargo clippy --all-targets -- -D warnings` (headless build)
- `cargo fmt --check`

These checks ensure no lint regressions are introduced by TUI code
and that the codebase remains consistently formatted across both
feature configurations.

### 20.9 Windows-specific tests

**Windows CI scope for MVP:** The primary CI target is Unix (Linux/macOS).
Windows is **best-effort for MVP** — the headless `ssync` binary and all
existing CLI tests run on Windows CI, but the TUI (`ssync-tui`) is not
smoke-tested in automated CI on Windows. Confirm this is clearly
documented in `AGENTS.md`.

The following Windows-specific items require manual validation when
Windows testing resources are available (post-MVP or when a Windows
contributor sets up Windows CI):

- `CTRL_CLOSE_EVENT` handler: register a handler and verify state save +
  terminal restore complete before Windows forcibly kills the process
  (§7.9 timing constraint). Test by simulating close on Windows Terminal.
- `tempfile::persist()` atomic write: verify `MoveFileExW` succeeds on
  NTFS and does not leave stale `.tmp` files on rename failure.
- `notepad` as default editor fallback (§7.4): `$VISUAL` / `$EDITOR`
  unset on Windows → `notepad` opens; on exit, mtime is updated; config
  reloads correctly (Windows 2s mtime granularity is handled by
  `exit_status.success()` check per §7.4).
- Windows Terminal (ConPTY): Unicode glyphs `▶ ┃ ━ ◉ ☑ ✓ ✗ ⊘ ⚠`
  render correctly; cmd.exe and PowerShell sessions without ConPTY are
  out of scope (§10.1).

Until Windows automated CI is set up, any PR that touches signal
handling, file I/O, or terminal detection code should note in its PR
description whether Windows manual validation was performed.

---

## 21. Manual Verification

Two acceptance lists. The MVP list (§21.A) is the cutline for the
first merged PR. The Post-MVP list (§21.B) only applies to releases
that ship the corresponding phases.

### 21.A — MVP acceptance (Phases 0 → 3)

Run on a real terminal (not CI) before tagging the first TUI release.

1. `cargo build --bin ssync-tui --features tui` compiles cleanly.
2. `cargo build --bin ssync` (headless) compiles cleanly;
   `ssync` (no subcommand) prints "Interactive TUI not available",
   **exit 1**.
3. `cargo clippy --all-targets --features tui` warns nothing.
4. `cargo test --features tui` passes; `cargo test` (no features)
   passes.
5. `ssync-tui` on a TTY launches TUI; `1`/`2`/`3` and Tab switch
   tabs; `q` quits with **exit 0**.
6. `ssync-tui | cat` prints clap help and **exits 2** — does not
   corrupt the pipe with ANSI escapes.
7. `ssync-tui -c custom.toml` launches TUI with custom config; the
   matching `tui_state-{hash}.toml` is created under
   `state::db::state_dir()` (or override).
8. `ssync check --all` (existing CLI) is byte-for-byte identical
   to pre-Phase-0.7 stdout (golden-diff).
9. `i` key opens info popup; `i` again closes; Esc also closes.
10. Quit and relaunch → last active tab, filter, and `[operate].operation`
    restored; cursors reset to top.
11. Config tab placeholder (Phase 1a): pressing `1`/`2`/`3` or Tab
    cycles to the Config tab; the tab renders a placeholder message
    (e.g. "Config — available in Phase 4"); **no crash, no blank
    screen**. Full read-only Config tab is **post-MVP** (Phase 4,
    see §21.B item B5.5).
12. Operate tab: arrow keys cross selector / target / params /
    [Execute]; Space toggles checkboxes; **`Enter` on `[Execute]`
    runs `check`** (only `check` is wired in MVP).
13. Checkout tab (MVP scope): ↑↓ scrolls rows; Tab cycles controls;
    `f` opens filter popup. **No** detail popup, history, since,
    or export controls in MVP.
14. Filter popup: ↑↓ crosses radios → sub-list → options → buttons;
    → enters sub-list; ← exits; breadcrumb updates only on level
    change.
15. Run a `check` from Operate tab including an unreachable host →
    red failure detail appears in progress popup for that host;
    **no terminal tearing**; Esc cancels (cooperatively, may take
    up to `ctx.timeout`).
16. Force a panic (debug-build only: hidden Ctrl+Alt+P keybind, or a
    test build) → terminal restored, panic backtrace printed
    cleanly, shell prompt usable.
17. External `vim config.toml` deletes a group used by persisted
    filter → restart → invalid group silently dropped from filter,
    info message in status bar.
18. Adaptive arrow nav: in a vertical list, ← escapes to sidebar;
    in a radio at first option, ← still escapes (boundary
    condition).
19. Color-blind mode (mono terminal): focused items still
    distinguishable via `▶` prefix and thick border characters.
20. **tmux resize:** run `ssync-tui` inside a tmux session; resize the
    tmux window — TUI layout reflows correctly with no rendering
    artifacts. (Tests that `crossterm` resize events propagate through
    tmux correctly.)

### 21.B — Post-MVP acceptance (Phases 4 → 7)

Each row applies to releases that ship the corresponding phase.
Mark items N/A on releases that have not yet picked up the phase.

| # | Phase | Item |
|---|-------|------|
| B1 | 5 | `run` and `exec` operations executable from Operate tab; param fields editable inline; results popup shows per-host stdout. |
| B2 | 6 | `sync` operation: two-mode panel (entries / adhoc); ad-hoc files use **list editor**, not comma string; conflict warning fires only on overlapping host sets. |
| B3 | 6 | `[operate].sync_mode` / `adhoc_files` / `config_mtime` / `sync_source` persisted; ad-hoc banner appears on restart when `sync_mode = adhoc`. |
| B4 | 6 | Checkout: `Enter` opens detail popup; `h` toggles history; `s` opens since input; `o` exports report. `[checkout]` schema fields persisted. |
| B5 | 6 | `?` keybinding help popup renders the full table. |
| B5.5 | 4 | Config tab (read-only): arrow keys cross sidebar ↔ fields; ALL fields reachable; `E` opens `$VISUAL`/`$EDITOR` (§7.4); no inline edit yet. |
| B6 | 7 | Config tab: `e`/`Enter` opens inline edit (Case A); `a` opens template form (Case B); `S` saves with `toml_edit` round-trip; comments preserved on round-trip. |
| B7 | 7 | `E` opens `$VISUAL`/`$EDITOR`; dirty prompt fires when config has unsaved edits before invoking editor; on return, full repaint and reload. |
| B8 | 7 | `L` opens log overlay scrollable view; level filtering works. |
| B9 | 7 | SSH auth-required popup masks input; credential forwarded to russh auth callback via `oneshot` channel (not subprocess stdin — see AD-13); cancellation still works. Requires Phase 7 ADR to be completed first. |
| B10 | 8 | (If shipped) Per-line streaming: `TuiEvent::OutputLine` events appear in progress popup as remote command runs. |

---

## 22. Post-MVP Backlog

These are deferred until after Phase 7 ships. Each is independently
demo-able and mergeable.

1. **User theme override** — `[tui].theme = "default" | "monochrome" |
   custom palette table` in `config.toml`; `Theme::from_config` reads it.
2. **Inline search / quick-filter** — `/` in any list enters
   type-to-filter; matching items highlighted.
3. **Undo / Redo for config edits** — Ctrl+Z / Ctrl+Y (single-level).
4. **Toast notifications** — non-blocking 3s auto-dismiss overlays
   for "Config saved", etc.
5. **Auto-refresh for Checkout** — optional background polling,
   `R` toggles.
6. **Operation history view** — 4th tab or sub-section showing
   `operation_log` entries.
7. **SSH host quick-connect status indicator** — background reachability probe.
8. **Confirm before broad operations** — `--all` run/exec with > N
   hosts requires confirmation.
9. **Multi-select in Checkout** — Space selects rows; `o` exports
   only selected.
10. **Partial-entry execution** — Space toggles per entry in
    Applicable Entries; only toggled entries run.
11. **Mouse support** — opt-in via `[tui].mouse = true`; click on
    list items, scroll wheel.
12. **Yank (copy to clipboard)** — `y` in any list yanks the focused
    item (host name, error message, etc.) to the system clipboard via
    `arboard` or equivalent. In MVP, terminal-native selection
    (mouse or Ctrl+Shift+C) works in most emulators running alternate
    screen — no code needed until this post-MVP item lands.
13. **Evaluate single-binary with runtime detection** — the two-binary
    model (AD-17) doubles CI artifact count for the lifetime of the
    project. Once the TUI is stable and the `ssync-tui` name is
    understood by users, evaluate merging to a single binary with
    a `--tui` flag or environment-variable detection. The ~1–2 MB
    linking cost of `ratatui`/`crossterm` is the primary tradeoff.

---

## 23. Removed / Rejected Decisions

For posterity (so we don't relitigate):

- `tui` in `[features].default` for source builds — **rejected** (AD-1).
  Source builds default to headless; release CI ships `ssync-tui`.
- Subprocess transport (`std::process::Command` + `Stdio::piped()` +
  `kill_on_drop`) — **rejected** (AD-13). The TUI reuses the existing
  russh `SshPool`; ProgressSink trait surfaces per-host events. No
  second runtime.
- Re-initializing the global `tracing` subscriber inside `tui::run()`
  — **rejected** (AD-15). `init_tracing` is changed once in `main.rs`
  to install a `reload`-capable layered subscriber; TUI swaps the
  writer, never the subscriber.
- Logging to a `tui.log` file via `tracing-appender` — **rejected**.
  In-memory ring buffer only; no extra dep, no disk writes during
  TUI session.
- Storing `tui_state.toml` at a hardcoded `dirs::state_dir()` path
  — **rejected** (AD-16). State file follows the same
  `state_dir` override the DB honors, with a per-config-path hash.
- Persisted cursor positions — **rejected** (AD-7). External edits
  invalidate them; complexity > benefit.
- fd-level `dup2` stderr redirect — **rejected** (AD-10). Swallows
  panic backtraces.
- `persist_commands = true` config flag — **rejected** (AD-12).
  Privacy concern. `run_command` / `exec_script` are never persisted.
- Comma-separated ad-hoc files string — **rejected**. Replaced by
  list editor (paths can contain commas, spaces, shell escapes).
- `Esc` to clear `Option<T>` to `None` — **rejected**. Esc means
  cancel-and-restore. `Ctrl+D` clears.
- Mouse support default-on — **rejected**. Disabled explicitly via
  `DisableMouseCapture` in `TerminalGuard::install`. Re-enabling is
  a Post-MVP item.
- Implicit `Enter` overload (edit-or-execute based on focus heuristics)
  — **rejected**. Replaced by explicit `[Execute]` button (§12.3).

---

## 24. Error Taxonomy

The plan references error handling in multiple scattered places
(`db_healthy`, `OperationError` popup, status bar red text). This
section provides the canonical three-tier classification.

### 24.1 Three error tiers

| Tier | Name | Condition | TUI Response |
|------|------|-----------|-------------|
| 1 | **Fatal** | TUI cannot start or cannot safely continue | Restore terminal + `eprintln!` + **exit 1**. Examples: `TerminalGuard::install()` fails, `panic::set_hook` fails, config completely unparseable on startup. |
| 2 | **Degraded** | Core functionality impaired but TUI can run | Persistent red banner at top of screen (above tab bar). Affected tab(s) show placeholder. Examples: DB open fails (`db_healthy = false`), state file is corrupt. |
| 3 | **Transient** | Single operation or action failed; recoverable | Status bar red message, auto-clears after 5s or on next `Esc`. Examples: single `check` operation fails, config save fails (disk full), atomic write fails. |

### 24.2 Degraded mode behaviour (Tier 2)

When `db_healthy = false`:
- Persistent banner: `"⚠ Database unavailable — Checkout data disabled"`
- Checkout tab shows: `"(Database unavailable — restart to retry)"`
- Operate tab: `check` execution still allowed; snapshot DB writes will
  fail and surface as Tier 3 status-bar errors. The user is warned
  before executing via a yellow line in the Applicable Entries panel:
  `"⚠ DB unavailable — results will not be stored"`.
- `db_healthy` is re-checked on each `OperationFinished` reload
  attempt; recovery without restart is possible if the DB file
  becomes accessible again.

### 24.3 Boundary between Tier 2 and Tier 3

The decision rule:
- **Tier 2** when the failure affects all future actions in a session
  (DB unavailable → every subsequent snapshot write will fail).
- **Tier 3** when the failure is isolated to one action and the next
  action might succeed (e.g. one SSH command timeout).

Config parse failure at startup is Tier 1 if the file is completely
unreadable (TUI cannot show anything meaningful); Tier 2 if partially
parseable (TUI can render what it has but marks affected fields with
`⚠`).

### 24.4 Implementation guidance

- Tier 1 errors are handled in `tui::entry::run_or_fallback` before
  `TerminalGuard::install()`.
- Tier 2 errors set a flag on `App` (e.g. `db_healthy: bool`) that
  persists for the session. The banner renders unconditionally in
  `app.rs::render_frame()` above the tab bar when the flag is set.
- Tier 3 errors set `App.error: Option<String>` (already defined in
  §6.1) and are cleared by `Esc` or after 5s. **Timer implementation:**
  add `App.error_since: Option<Instant>` alongside `App.error`. At the
  start of each render cycle (before calling `terminal.draw()`), check:
  ```rust
  if self.error_since.map_or(false, |t| t.elapsed() > Duration::from_secs(5)) {
      self.error = None;
      self.error_since = None;
  }
  ```
  Set `error_since = Some(Instant::now())` whenever `App.error` is set.
  This integrates naturally with the existing 50ms render loop and
  requires no additional timer task or tokio interval.
