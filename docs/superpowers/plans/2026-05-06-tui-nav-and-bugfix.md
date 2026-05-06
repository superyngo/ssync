# ssync TUI Navigation & Bug-Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify ssync into a single TUI binary, implement full arrow-key navigation (including NavBar escape), and fix five config/operate bugs.

**Architecture:**
- Remove `ssync-tui` binary; `ssync` always launches TUI when invoked with no subcommand.
- Add `navbar_focused: bool` to `App` so the tab-bar can receive arrow focus; each tab's top boundary emits an escape signal handled in `app.rs`.
- All config editing-state checks are hoisted to `app.rs::handle_key` so global hotkeys are suspended while inline edits are active.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, tokio

---

## File Map

| File | Changes |
|------|---------|
| `Cargo.toml` | Remove `[[bin]] ssync-tui`, make `tui` the default feature |
| `src/main.rs` | Remove `binary_is_ssync_tui`, always launch TUI on no-subcommand |
| `src/tui/app.rs` | `navbar_focused` field; NavBar key handling; editing-state guard; `ApplicableEntries` trap fix; NavBar render highlight |
| `src/tui/tabs/config_tab.rs` | `is_editing_active()` helper; Vec-field edit routes to entry form |

---

## Task 1: Unify into single binary

**Files:**
- Modify: `Cargo.toml:7-14` (bin targets), `Cargo.toml:80-81` (features)
- Modify: `src/main.rs:17-24` (binary detection), `src/main.rs:113-137` (main dispatch)

- [ ] **Step 1: Edit Cargo.toml — remove ssync-tui, set default tui feature**

Replace the `[[bin]]` block and `[features]` section:

```toml
# In Cargo.toml — replace lines 7-14:
[[bin]]
name = "ssync"
path = "src/main.rs"

# In Cargo.toml — replace lines 79-81:
[features]
default = ["tui"]
tui = ["ratatui", "crossterm", "tokio-util"]
```

- [ ] **Step 2: Edit src/main.rs — remove binary_is_ssync_tui, update dispatch**

Remove the function `binary_is_ssync_tui` (lines 17-24) and update the no-subcommand branch (lines 113-137):

```rust
// Remove entirely:
// #[cfg(feature = "tui")]
// fn binary_is_ssync_tui() -> bool { ... }

// Replace the no-subcommand branch in main():
//   OLD (lines 113-137):
//     #[cfg(feature = "tui")]
//     let tui_silent = cli.command.is_none() && binary_is_ssync_tui();
//     ...
//     if binary_is_ssync_tui() { ... }
//     eprintln!("Interactive TUI not available. Use the `ssync-tui` binary.");
//
//   NEW:
#[cfg(feature = "tui")]
let tui_silent = cli.command.is_none();
#[cfg(not(feature = "tui"))]
let tui_silent = false;

#[cfg(feature = "tui")]
let log_buffer = init_tracing(cli.verbose, tui_silent);
#[cfg(not(feature = "tui"))]
init_tracing(cli.verbose, tui_silent);

let cfg = cli.config.as_deref();

let command = match cli.command {
    Some(c) => c,
    None => {
        #[cfg(feature = "tui")]
        {
            return tui::entry::run_or_fallback(cli.verbose, cfg, log_buffer).await;
        }
        #[cfg(not(feature = "tui"))]
        {
            eprintln!("TUI not compiled in. Rebuild with --features tui.");
            std::process::exit(1);
        }
    }
};
```

- [ ] **Step 3: Build and verify**

```bash
cd /Volumes/Home/Users/wen/repos3/ssync
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors. Verify only one binary exists:

```bash
ls target/debug/ssync* 2>/dev/null | grep -v '\.d'
```

Expected output: only `target/debug/ssync` (no `ssync-tui`).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat: unify into single ssync binary, tui is default feature"
```

---

## Task 2: Add `is_editing_active` helper to ConfigTabState

**Files:**
- Modify: `src/tui/tabs/config_tab.rs` (add public method after `breadcrumb`)

This helper is used by `app.rs` to suspend global hotkeys while the config tab has an active inline edit or entry-form field active.

- [ ] **Step 1: Add method after `breadcrumb` (~line 436)**

Insert after the closing `}` of `fn breadcrumb` at `config_tab.rs:436`:

```rust
/// Returns true when a text input is currently active in the config tab
/// (inline scalar edit, entry form field input, or vec editor input).
/// Used by app.rs to suspend global hotkeys.
pub fn is_editing_active(&self) -> bool {
    if let Some(ref input) = self.editing_field {
        if input.mode == crate::tui::components::input_field::InputMode::Active {
            return true;
        }
    }
    if let Some(ref form) = self.entry_form {
        if form.active_input.is_some() {
            return true;
        }
        if let Some(ref ve) = form.vec_editor {
            if ve.input.mode == crate::tui::components::input_field::InputMode::Active {
                return true;
            }
        }
    }
    false
}
```

- [ ] **Step 2: Build to verify no compile errors**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/tui/tabs/config_tab.rs
git commit -m "fix(config): add is_editing_active helper for app-level edit-state guard"
```

---

## Task 3: Fix global hotkey suspension during config editing (bugs 4a & 4b)

**Root cause:** `app.rs::handle_key` matches `KeyCode::Char('q')` and `KeyCode::Esc` globally (lines 1028, 1049) BEFORE reaching the config tab handler at line 1137. When an inline edit is active, `q` quits the app and `Esc` does nothing useful.

**Files:**
- Modify: `src/tui/app.rs` — insert editing-state guard just before the global `match key.code` block at line 1026

- [ ] **Step 1: Insert editing guard block before `match key.code` at line 1026**

Find the comment line `// ── Global keys` at `app.rs:1027` and insert BEFORE the `match key.code {` opening:

```rust
// §edit-guard: while config tab has an active text input, suspend all
// global shortcuts and route directly to the config tab.
if self.active_tab == TabId::Config && self.config_tab.is_editing_active() {
    let handled = self.config_tab.handle_key(key, &mut self.config);
    if let Some((kind, index)) = self.config_tab.pending_delete.take() {
        self.config_tab.execute_delete(&mut self.config, kind, index);
    }
    return Ok(handled);
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Manual smoke test (launch TUI)**

```bash
cargo run -- 2>/dev/null &
sleep 1; kill %1 2>/dev/null; true
```

(Just verifying it compiles and launches; actual key-press testing is manual.)

- [ ] **Step 4: Commit**

```bash
git add src/tui/app.rs
git commit -m "fix(config): suspend global hotkeys (q, Esc, etc.) while inline edit is active"
```

---

## Task 4: Fix Vec-field editing in FieldTable (bugs 4c & 4d)

**Root cause:** `activate_inline_edit` at `config_tab.rs:609` explicitly returns `false` for `VecString | VecCheckPath | TriBool`. Pressing `e`/Enter on `paths`, `groups`, `enabled` fields does nothing. The entry-form (`start_edit_entry`) already has a full Vec editor — just need to route Vec fields to it and pre-select the right field.

**Files:**
- Modify: `src/tui/tabs/config_tab.rs` — FieldTable `e`/Enter handler (~line 557)

- [ ] **Step 1: In FieldTable `e`/Enter handler, route Vec fields to entry form**

Replace the handler block at `config_tab.rs:557-570`:

```rust
// OLD:
KeyCode::Char('e') | KeyCode::Enter => {
    {
        let fields = self.current_descriptors(config);
        if let Some(f) = fields.get(self.field_vp.selected) {
            if matches!(f.kind, FieldKind::TriBool) {
                let new_val = tribool_cycle_fwd(&f.display_value);
                self.commit_inline_edit(new_val, config);
                self.config_dirty = true;
                return true;
            }
        }
    }
    self.activate_inline_edit(config)
}

// NEW:
KeyCode::Char('e') | KeyCode::Enter => {
    let field_idx = self.field_vp.selected;
    let fields = self.current_descriptors(config);
    if let Some(f) = fields.get(field_idx) {
        match &f.kind {
            FieldKind::TriBool => {
                let new_val = tribool_cycle_fwd(&f.display_value);
                self.commit_inline_edit(new_val, config);
                self.config_dirty = true;
                return true;
            }
            FieldKind::VecString | FieldKind::VecCheckPath => {
                // Open full entry form and jump to this field so the user
                // can use the vec editor.
                self.start_edit_entry(config);
                if let Some(ref mut form) = self.entry_form {
                    // Navigate the form viewport to the matching field.
                    let target = form.fields.iter().position(|fd| fd.key == f.key);
                    if let Some(pos) = target {
                        form.field_vp = Viewport::new();
                        form.field_vp.set_dims(form.fields.len(), 8);
                        for _ in 0..pos {
                            form.field_vp.move_down();
                        }
                    }
                }
                return true;
            }
            _ => {}
        }
    }
    self.activate_inline_edit(config)
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/tui/tabs/config_tab.rs
git commit -m "fix(config): pressing e on Vec fields (paths/groups/enabled) opens entry form vec editor"
```

---

## Task 5: Fix Operate tab ApplicableEntries navigation trap (bug 4e)

**Root cause:** `app.rs:1158-1161` — pressing Up while at `ApplicableEntries` always stays in `ApplicableEntries` (just scrolls). When `entries_scroll == 0` it should escape to `TargetRow`. This means once you scroll from Execute into ApplicableEntries you can never navigate back up to OpRadio/ParamPanel.

**Files:**
- Modify: `src/tui/app.rs` — Up-arrow handler at line ~1158

- [ ] **Step 1: Fix ApplicableEntries Up escape**

Replace lines 1158-1161 in `app.rs`:

```rust
// OLD:
OperateFocus::ApplicableEntries => {
    self.entries_scroll = self.entries_scroll.saturating_sub(1);
    OperateFocus::ApplicableEntries
}

// NEW:
OperateFocus::ApplicableEntries => {
    if self.entries_scroll == 0 {
        OperateFocus::TargetRow
    } else {
        self.entries_scroll -= 1;
        OperateFocus::ApplicableEntries
    }
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "fix(operate): Up from ApplicableEntries at top escapes to TargetRow instead of trapping"
```

---

## Task 6: NavBar focus — full arrow-key navigation (requirements 2 & 3)

**Goal:** Pressing Up at the topmost widget in any tab moves focus to the NavBar (tab bar). The NavBar shows a selection highlight. Left/Right moves between tab labels. Down/Enter returns to the tab's content. This enables keyboard-only full-range navigation.

**Files:**
- Modify: `src/tui/app.rs` — add `navbar_focused` field, update handle_key, update render_tab_bar

### Sub-task 6a: Add `navbar_focused` to App state

- [ ] **Step 1: Add field to App struct**

Find the App struct definition in `app.rs`. Add the field `navbar_focused: bool` near other focus/tab fields. In `App::new` (or `from_context`), initialize it to `false`.

Search for where `active_tab: TabId` is declared in the struct, and add after it:

```rust
navbar_focused: bool,
```

In the initializer block (where `active_tab: TabId::Config` or similar appears), add:

```rust
navbar_focused: false,
```

- [ ] **Step 2: Build to verify**

```bash
cargo build 2>&1 | tail -5
```

### Sub-task 6b: Update handle_key for NavBar routing

- [ ] **Step 3: Add NavBar key handling block**

In `app.rs::handle_key`, after the log-overlay block (around line 949) and BEFORE the `completed_report` block, insert:

```rust
// NavBar focus: intercept keys when navbar_focused is true.
if self.navbar_focused {
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => {
            self.active_tab = self.active_tab.prev();
            return Ok(true);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            self.active_tab = self.active_tab.next();
            return Ok(true);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Enter => {
            self.navbar_focused = false;
            return Ok(true);
        }
        KeyCode::Esc => {
            self.navbar_focused = false;
            return Ok(true);
        }
        // 1/2/3 still work from NavBar.
        KeyCode::Char('1') => {
            self.active_tab = TabId::Config;
            self.navbar_focused = false;
            return Ok(true);
        }
        KeyCode::Char('2') => {
            self.active_tab = TabId::Operate;
            self.navbar_focused = false;
            return Ok(true);
        }
        KeyCode::Char('3') => {
            self.active_tab = TabId::Checkout;
            self.navbar_focused = false;
            return Ok(true);
        }
        _ => return Ok(false),
    }
}
```

- [ ] **Step 4: Wire Up-at-top-boundary to set navbar_focused = true**

**Config tab** — in the Config Sidebar zone, `Up` at top boundary currently just calls `sidebar_vp.move_up()`. Change the routing in `app.rs` at the `_ if self.active_tab == TabId::Config` branch: before forwarding to `config_tab.handle_key`, check if Up is pressed while at the top of Sidebar:

Add this block BEFORE the existing `_ if self.active_tab == TabId::Config` arm (around line 1137), as a new match arm:

```rust
KeyCode::Up | KeyCode::Char('k')
    if self.active_tab == TabId::Config
        && self.config_tab.zone == ConfigZone::Sidebar
        && self.config_tab.sidebar_vp.selected == 0 =>
{
    self.navbar_focused = true;
    Ok(true)
}
```

**Operate tab** — change `OperateFocus::OpRadio` Up handler at line 1198:

```rust
// OLD:
OperateFocus::OpRadio => OperateFocus::OpRadio,

// NEW:
OperateFocus::OpRadio => {
    self.navbar_focused = true;
    OperateFocus::OpRadio
}
```

**Checkout tab** — add a new arm before the existing checkout Up handler at line 1382:

```rust
KeyCode::Up | KeyCode::Char('k')
    if self.active_tab == TabId::Checkout
        && self.checkout_viewport.selected == 0 =>
{
    self.navbar_focused = true;
    Ok(true)
}
```

Note: the existing `checkout_viewport.move_up()` arm must remain below this new arm (match arms are evaluated in order).

- [ ] **Step 5: Build to verify**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

### Sub-task 6c: Render NavBar highlight when focused

- [ ] **Step 6: Find NavBar render function and add highlight**

Search for the tab-bar render function:

```bash
grep -n "render_tab_bar\|TabId::Config.*label\|1:Config\|tab.*title" /Volumes/Home/Users/wen/repos3/ssync/src/tui/app.rs | head -20
```

In the tab-bar render, the currently active tab is highlighted (e.g. `Modifier::BOLD | REVERSED`). When `self.navbar_focused` is true, add an additional visual indicator: render a `▶` prefix or a distinct border/color around the entire tab row, AND highlight the `active_tab` label differently to show it is "selected" in the NavBar.

The exact change depends on the render code. The principle:
- If `navbar_focused`: render the active tab title with `Modifier::REVERSED | UNDERLINED | BOLD` and a distinct foreground (e.g. `theme.accent_operate`).
- If not `navbar_focused`: render as before (active tab with `BOLD`, inactive with `inactive` color).

- [ ] **Step 7: Build final**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 8: Commit**

```bash
git add src/tui/app.rs src/tui/tabs/config_tab.rs
git commit -m "feat(nav): full arrow-key navigation — Up from top escapes to NavBar, Left/Right to switch tabs"
```

---

## Task 7: Update CHANGELOG and README

**Files:**
- Modify: `CHANGELOG.md` — prepend Unreleased entry
- Modify: `README.md` — if it describes ssync-tui binary, update to ssync

- [ ] **Step 1: Prepend Unreleased entry to CHANGELOG.md**

Add at the top of the `## [Unreleased]` section (or create it):

```markdown
## [Unreleased] — 2026-05-06

### Changed
- **Binary unified**: `ssync` now launches TUI directly when invoked with no subcommand; `ssync-tui` binary removed.

### Added
- **Full arrow-key navigation**: pressing ↑ at the top of any tab escapes to the tab NavBar; ←/→ switches tabs; ↓/Enter returns to content.

### Fixed
- Config: pressing `Esc` while editing a scalar field now correctly exits edit mode.
- Config: global hotkeys (`q`, etc.) are suspended while a config field is being edited.
- Config: pressing `e` on Vec-type fields (`paths`, `groups`, `enabled`) now opens the entry form vec editor.
- Operate: pressing ↑ from the Applicable Entries panel at scroll position 0 now escapes to Target Row (was trapped).
```

- [ ] **Step 2: Update README if needed**

```bash
grep -n "ssync-tui" /Volumes/Home/Users/wen/repos3/ssync/README.md | head -10
```

Replace any mention of `ssync-tui` with `ssync`.

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md README.md
git commit -m "docs: update CHANGELOG and README for binary unification and nav improvements"
```

---

## Self-Review

### Spec coverage
| Requirement | Task |
|-------------|------|
| 1. Single binary (ssync = TUI) | Task 1 |
| 2. ↑ from Config content → NavBar | Task 6 |
| 3. ↑ from Operate/Checkout → NavBar | Task 6 |
| 4a. Config Esc exits edit mode | Task 3 |
| 4b. Global hotkeys suspended during edit | Task 3 |
| 4c. Array field editing (paths) | Task 4 |
| 4d. (unscoped) check/sync e key | Task 4 (same root: VecString) |
| 4e. Operate ParamPanel fields selectable | Task 5 (ApplicableEntries trap fix) |

### Notes
- Task 6, Step 4 requires `ConfigZone` to be accessible from `app.rs`. `ConfigZone` is pub (`pub enum ConfigZone`) and `config_tab.zone` is pub, so this compiles.
- Task 6, Step 4 requires `sidebar_vp.selected` to be accessible. `sidebar_vp` is `pub` in `ConfigTabState`. `Viewport::selected` must be pub — verify with `grep -n "pub selected" src/tui/components/viewport.rs`.
- Task 6, Step 4 requires `checkout_viewport.selected` — verify accessibility same way.
- Tasks are ordered by dependency: Task 1 (binary) → Task 2 (helper) → Tasks 3-5 (independent bugfixes using helper) → Task 6 (nav feature) → Task 7 (docs).
- Tasks 3, 4, 5 are independent and can be run in parallel.
