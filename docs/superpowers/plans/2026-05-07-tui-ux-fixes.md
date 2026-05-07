# TUI UX Fixes & Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 4 bugs in Config tab editing and implement 4 UX improvements (NavBar highlight, cross-panel navigation, Esc shortcut, context Tab cycling).

**Architecture:** All changes are in `src/tui/app.rs` and `src/tui/tabs/config_tab.rs`. The key event routing in `app.rs::handle_key` is a sequential match chain — order of arms matters. Config tab editing logic lives entirely in `config_tab.rs`. No new files needed.

**Tech Stack:** Rust, Ratatui 0.29, Crossterm 0.28

---

## Root Cause Summary (read before touching any code)

- **BUG A (inline edit Enter):** `handle_inline_edit_key` calls `input.handle_key(key)` which internally calls `confirm()` setting `saved = value`. The subsequent check `value != saved` is then always `false`, so `commit_inline_edit` never fires. Same bug in entry form `active_input` block.
- **BUG B (entry form Esc):** The global `KeyCode::Esc` arm at `app.rs:1104` runs *before* the `_ if Config` arm and returns `Ok(false)`, so the key never reaches `config_tab.handle_entry_form_key`.
- **BUG C (entry form stray keys):** `KeyCode::Char('a')` (line 1175) and `KeyCode::Char('d')` (line 1183) have no `entry_form.is_none()` guard, so they fire while the popup is open. Global `'q'` also fires.

---

## Files Modified

| File | Changes |
|------|---------|
| `src/tui/tabs/config_tab.rs` | Fix inline edit Enter (BUG A); fix entry form active_input Enter (BUG A); expose `field_vp_at_top()` helper |
| `src/tui/app.rs` | Fix global Esc routing (BUG B); guard 'a'/'d' arms (BUG C); Operate initial focus; NavBar border highlight; FieldTable Up→NavBar; global Esc→NavBar; Tab context cycling |

---

## Task 1: Fix Inline Edit Enter Save (BUG A — `handle_inline_edit_key`)

**Files:**
- Modify: `src/tui/tabs/config_tab.rs:623-629`

### Background

`InputField::confirm()` sets `self.saved = self.value.clone()` then switches mode to `Normal`. After `input.handle_key(key)` processes Enter, `saved == value`, so the old guard `value != saved` always returns false and commit never happens.

- [ ] **Step 1: Replace the guard condition**

In `src/tui/tabs/config_tab.rs`, replace lines 623–629:

```rust
        if input.mode == InputMode::Active {
            input.handle_key(key);
            if input.mode == InputMode::Normal && input.value != input.saved {
                self.commit_inline_edit(&input.value, config);
                self.config_dirty = true;
            }
            return true;
        }
```

with:

```rust
        if input.mode == InputMode::Active {
            input.handle_key(key);
            if input.mode == InputMode::Normal {
                // confirm() already set saved=value, so compare against original raw value.
                // Always commit on Enter/Esc-cancel — the value is whatever survived handle_key.
                self.commit_inline_edit(&input.value, config);
                self.config_dirty = true;
            }
            return true;
        }
```

- [ ] **Step 2: Build to verify no compile errors**

```bash
cd /Volumes/Home/Users/wen/repos/ssync && cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/tui/tabs/config_tab.rs
git commit -m "fix: inline edit Enter now commits value to config"
```

---

## Task 2: Fix Entry Form Active-Input Enter Save (BUG A — `handle_entry_form_key`)

**Files:**
- Modify: `src/tui/tabs/config_tab.rs:726-736`

### Background

Same root cause as Task 1 but inside the entry form. After `form.input.handle_key(key)` calls `confirm()`, `value == saved` so the display_value is never updated.

- [ ] **Step 1: Remove the `!= saved` guard**

In `src/tui/tabs/config_tab.rs`, replace lines 726–736:

```rust
        if form.active_input.is_some() {
            form.input.handle_key(key);
            if form.input.mode == InputMode::Normal {
                let idx = form.active_input.unwrap();
                if form.input.value != form.input.saved {
                    form.fields[idx].display_value = form.input.value.clone();
                    form.dirty = true;
                }
                form.active_input = None;
            }
            return true;
        }
```

with:

```rust
        if form.active_input.is_some() {
            form.input.handle_key(key);
            if form.input.mode == InputMode::Normal {
                let idx = form.active_input.unwrap();
                form.fields[idx].display_value = form.input.value.clone();
                form.dirty = true;
                form.active_input = None;
            }
            return true;
        }
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/tui/tabs/config_tab.rs
git commit -m "fix: entry form inline edit Enter now persists field value"
```

---

## Task 3: Fix Entry Form Esc Close (BUG B)

**Files:**
- Modify: `src/tui/app.rs:1104-1110`

### Background

The global `KeyCode::Esc` arm returns `Ok(false)` before execution reaches the `_ if Config` arm. We need to intercept when an entry form is open and delegate to `config_tab.handle_key`.

- [ ] **Step 1: Add entry-form delegation inside the Esc arm**

In `src/tui/app.rs`, replace lines 1104–1110:

```rust
            KeyCode::Esc => {
                if self.error.is_some() {
                    self.error = None;
                    return Ok(true);
                }
                Ok(false)
            }
```

with:

```rust
            KeyCode::Esc => {
                if self.error.is_some() {
                    self.error = None;
                    return Ok(true);
                }
                // Entry form must handle Esc before the global NavBar escape below.
                if self.active_tab == TabId::Config
                    && (self.config_tab.entry_form.is_some()
                        || self.config_tab.confirm.is_some())
                {
                    let handled = self.config_tab.handle_key(key, &mut self.config);
                    return Ok(handled);
                }
                // Any other position: Esc jumps focus to NavBar.
                if !self.navbar_focused {
                    self.navbar_focused = true;
                    return Ok(true);
                }
                Ok(false)
            }
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "fix: entry form Esc now closes popup; global Esc jumps to NavBar"
```

---

## Task 4: Block Stray Keys While Entry Form Is Open (BUG C)

**Files:**
- Modify: `src/tui/app.rs:1175-1190`

### Background

`'a'` (add entry), `'d'` (delete entry), and `'q'` (quit) are matched before the `_ if Config` arm. When the entry form popup is open, these keys should be swallowed. Add `entry_form.is_none()` guards.

- [ ] **Step 1: Guard the 'a' arm**

In `src/tui/app.rs`, replace lines 1175–1181:

```rust
            // 'a' adds a new entry (Phase 7 Case B).
            KeyCode::Char('a') if self.active_tab == TabId::Config => {
                let kind = self.config_add_kind();
                if let Some(kind) = kind {
                    self.config_tab.start_add_entry(kind);
                }
                Ok(true)
            }
```

with:

```rust
            // 'a' adds a new entry (Phase 7 Case B).
            KeyCode::Char('a')
                if self.active_tab == TabId::Config
                    && self.config_tab.entry_form.is_none()
                    && self.config_tab.confirm.is_none() =>
            {
                let kind = self.config_add_kind();
                if let Some(kind) = kind {
                    self.config_tab.start_add_entry(kind);
                }
                Ok(true)
            }
```

- [ ] **Step 2: Guard the 'd' arm**

In `src/tui/app.rs`, replace lines 1182–1190:

```rust
            // 'd' deletes focused entry (Phase 7).
            KeyCode::Char('d') if self.active_tab == TabId::Config => {
                if self.config_tab.confirm.is_some() {
                    // Confirm dialog handles 'y'/'n'; 'd' is not consumed here.
                    return Ok(false);
                }
                self.config_tab.request_delete();
                Ok(true)
            }
```

with:

```rust
            // 'd' deletes focused entry (Phase 7).
            KeyCode::Char('d')
                if self.active_tab == TabId::Config
                    && self.config_tab.entry_form.is_none()
                    && self.config_tab.confirm.is_none() =>
            {
                self.config_tab.request_delete();
                Ok(true)
            }
```

- [ ] **Step 3: Absorb unhandled keys when entry form is open**

The `_ if Config` catch-all at line 1201 already routes to `config_tab.handle_key`, which returns `false` for unknown keys. To prevent those from leaking to global handlers (including 'q'), add an absorb arm *before* the global 'q' handler. Find the existing `_ if self.active_tab == TabId::Config` arm (around line 1201) and **also** add an explicit early-absorb arm that fires when the entry form is open:

Insert immediately before line 1201 (`_ if self.active_tab == TabId::Config =>`):

```rust
            // Absorb all keys while entry form or confirm dialog is open.
            _ if self.active_tab == TabId::Config
                && (self.config_tab.entry_form.is_some()
                    || self.config_tab.confirm.is_some()) =>
            {
                let handled = self.config_tab.handle_key(key, &mut self.config);
                if let Some((kind, index)) = self.config_tab.pending_delete.take() {
                    self.config_tab.execute_delete(&mut self.config, kind, index);
                }
                Ok(true) // always consume — don't leak to global 'q'/etc.
            }
```

- [ ] **Step 4: Build**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "fix: block a/d/q and other global keys while entry form popup is open"
```

---

## Task 5: Operate Tab Initial Focus → OpRadio

**Files:**
- Modify: `src/tui/app.rs:219`

- [ ] **Step 1: Change the initial value**

In `src/tui/app.rs`, replace line 219:

```rust
            operate_focus: OperateFocus::Execute,
```

with:

```rust
            operate_focus: OperateFocus::OpRadio,
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "fix: Operate tab now starts with focus on Operation radio, not Execute"
```

---

## Task 6: NavBar Border Highlight When Focused

**Files:**
- Modify: `src/tui/app.rs:1568-1573` (`render_tab_bar`)

### Background

Currently the tab bar's `Block` has no border styling change when `navbar_focused`. Only the selected tab's highlight style changes. We want the entire block border to use the accent color when navbar is focused, and `border_inactive` otherwise.

- [ ] **Step 1: Add block border styling**

In `src/tui/app.rs`, inside `render_tab_bar`, replace the `Tabs` construction (around lines 1568–1573):

```rust
        let tabs = Tabs::new(titles)
            .block(Block::default().borders(Borders::ALL).title(" ssync "))
            .select(selected)
            .style(Style::default().fg(self.theme.inactive))
            .highlight_style(highlight);
```

with:

```rust
        let block_border_style = if self.navbar_focused {
            Style::default().fg(accent)
        } else {
            Style::default().fg(self.theme.border_inactive)
        };
        let tabs = Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" ssync ")
                    .border_style(block_border_style),
            )
            .select(selected)
            .style(Style::default().fg(self.theme.inactive))
            .highlight_style(highlight);
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: NavBar block border highlights with accent color when focused"
```

---

## Task 7: Config FieldTable Top-Edge Up → NavBar

**Files:**
- Modify: `src/tui/tabs/config_tab.rs` (add `pub fn field_vp_at_top`)
- Modify: `src/tui/app.rs` (add new key arm)

### Background

`field_vp` is private. We need a public helper on `ConfigTabState` so `app.rs` can check whether the cursor is at the top of the field table.

- [ ] **Step 1: Add `field_vp_at_top` helper to `ConfigTabState`**

In `src/tui/tabs/config_tab.rs`, after the `is_editing_active` method (around line 461), add:

```rust
    /// Returns true when the FieldTable zone cursor is at the first row.
    pub fn field_vp_at_top(&self) -> bool {
        self.field_vp.selected == 0
    }
```

- [ ] **Step 2: Add the new key arm in `app.rs`**

In `src/tui/app.rs`, after the existing "Up at top of Config Sidebar escapes to NavBar" arm (lines 1191–1199) and before the `_ if Config` catch-all, insert:

```rust
            // Up at top of Config FieldTable also escapes to NavBar.
            KeyCode::Up | KeyCode::Char('k')
                if self.active_tab == TabId::Config
                    && self.config_tab.zone == ConfigZone::FieldTable
                    && self.config_tab.field_vp_at_top() =>
            {
                self.navbar_focused = true;
                Ok(true)
            }
```

- [ ] **Step 3: Build**

```bash
cargo build 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add src/tui/tabs/config_tab.rs src/tui/app.rs
git commit -m "feat: pressing Up at top of Config detail panel jumps to NavBar"
```

---

## Task 8: Context-Aware Tab Key Cycling

**Files:**
- Modify: `src/tui/app.rs:1138-1147` (Tab/BackTab arms)

### Background

Current behaviour: Tab cycles tabs on non-Config tabs; on Config Tab falls through to `config_tab.handle_key` which ignores it. New behaviour:

| Current focus | Tab action |
|---|---|
| NavBar (`navbar_focused`) | Cycle Config→Operate→Checkout (already mostly works via `active_tab.next()`), clear `navbar_focused` |
| Config Sidebar | Move cursor down in sidebar, wrap at bottom |
| Config FieldTable | Move cursor down in field list, wrap at bottom |
| Operate OpRadio | Cycle operation (Check→Run→Exec→Sync→Check), same as Right arrow |
| Operate (any other zone) | Cycle through operate zones: OpRadio→ParamPanel→TargetRow→ApplicableEntries→Execute→OpRadio |
| Checkout | Move cursor down in hosts list, wrap at bottom |

BackTab reverses each of the above.

- [ ] **Step 1: Add `tab_cycle_operate` helper to `app.rs`**

In `src/tui/app.rs`, add a new private method after `handle_key` (search for `fn save_state` to find a good insertion point):

```rust
    fn tab_cycle_operate(&mut self, forward: bool) {
        use super::tabs::operate_tab::OperateFocus;
        let zones: &[OperateFocus] = if self.has_entries_panel() {
            &[
                OperateFocus::OpRadio,
                OperateFocus::ParamPanel,
                OperateFocus::TargetRow,
                OperateFocus::ApplicableEntries,
                OperateFocus::Execute,
            ]
        } else {
            &[
                OperateFocus::OpRadio,
                OperateFocus::ParamPanel,
                OperateFocus::TargetRow,
                OperateFocus::Execute,
            ]
        };
        // Skip ParamPanel when operation is Check (no param panel).
        let effective: Vec<OperateFocus> = zones
            .iter()
            .copied()
            .filter(|z| {
                !(*z == OperateFocus::ParamPanel
                    && self.operate_operation == crate::config::OperationKind::Check)
            })
            .collect();
        let pos = effective
            .iter()
            .position(|z| *z == self.operate_focus)
            .unwrap_or(0);
        let next_pos = if forward {
            (pos + 1) % effective.len()
        } else {
            (pos + effective.len() - 1) % effective.len()
        };
        self.operate_focus = effective[next_pos];
        // When landing on OpRadio via Tab, also cycle the operation.
        // (OpRadio stays as trap — Tab from OpRadio moves to next zone, not cycles op.)
    }
```

- [ ] **Step 2: Replace the Tab/BackTab arms**

In `src/tui/app.rs`, replace lines 1138–1147:

```rust
            // Tab/BackTab on Config tab cycle zones (§8.2); on other tabs
            // cycle the tab bar.
            KeyCode::Tab if self.active_tab != TabId::Config => {
                self.active_tab = self.active_tab.next();
                Ok(true)
            }
            KeyCode::BackTab if self.active_tab != TabId::Config => {
                self.active_tab = self.active_tab.prev();
                Ok(true)
            }
```

with:

```rust
            // Tab/BackTab: context-aware cycling within the focused layer.
            KeyCode::Tab | KeyCode::BackTab => {
                let forward = key.code == KeyCode::Tab;
                if self.navbar_focused {
                    // NavBar: cycle tabs, enter the new tab.
                    self.active_tab = if forward {
                        self.active_tab.next()
                    } else {
                        self.active_tab.prev()
                    };
                    self.navbar_focused = false;
                } else {
                    match self.active_tab {
                        TabId::Config => {
                            match self.config_tab.zone {
                                ConfigZone::Sidebar => {
                                    let count = self.config_tab.items.len();
                                    if count > 0 {
                                        if forward {
                                            let sel = self.config_tab.sidebar_vp.selected;
                                            if sel + 1 >= count {
                                                self.config_tab.sidebar_vp.home();
                                            } else {
                                                self.config_tab.sidebar_vp.move_down();
                                            }
                                        } else {
                                            let sel = self.config_tab.sidebar_vp.selected;
                                            if sel == 0 {
                                                self.config_tab.sidebar_vp.end();
                                            } else {
                                                self.config_tab.sidebar_vp.move_up();
                                            }
                                        }
                                        self.config_tab.reset_field_vp(&self.config);
                                    }
                                }
                                ConfigZone::FieldTable => {
                                    let fields =
                                        self.config_tab.current_descriptors(&self.config);
                                    let count = fields.len();
                                    if count > 0 {
                                        if forward {
                                            let sel = self.config_tab.field_vp.selected;
                                            if sel + 1 >= count {
                                                self.config_tab.field_vp.home();
                                            } else {
                                                self.config_tab.field_vp.move_down();
                                            }
                                        } else {
                                            let sel = self.config_tab.field_vp.selected;
                                            if sel == 0 {
                                                self.config_tab.field_vp.end();
                                                let new_count = self
                                                    .config_tab
                                                    .current_descriptors(&self.config)
                                                    .len();
                                                self.config_tab
                                                    .field_vp
                                                    .set_dims(new_count, self.config_tab.field_vp.visible_height);
                                                self.config_tab.field_vp.end();
                                            } else {
                                                self.config_tab.field_vp.move_up();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        TabId::Operate => {
                            if self.operate_focus == OperateFocus::OpRadio {
                                // On OpRadio: Tab cycles the operation selection.
                                self.operate_operation = if forward {
                                    match self.operate_operation {
                                        OperationKind::Check => OperationKind::Run,
                                        OperationKind::Run => OperationKind::Exec,
                                        OperationKind::Exec => OperationKind::Sync,
                                        OperationKind::Sync => OperationKind::Check,
                                    }
                                } else {
                                    match self.operate_operation {
                                        OperationKind::Check => OperationKind::Sync,
                                        OperationKind::Run => OperationKind::Check,
                                        OperationKind::Exec => OperationKind::Run,
                                        OperationKind::Sync => OperationKind::Exec,
                                    }
                                };
                                self.save_state();
                            } else {
                                self.tab_cycle_operate(forward);
                            }
                        }
                        TabId::Checkout => {
                            let count = self.checkout_viewport.item_count;
                            if count > 0 {
                                if forward {
                                    let sel = self.checkout_viewport.selected;
                                    if sel + 1 >= count {
                                        self.checkout_viewport.home();
                                    } else {
                                        self.checkout_viewport.move_down();
                                    }
                                } else {
                                    let sel = self.checkout_viewport.selected;
                                    if sel == 0 {
                                        self.checkout_viewport.end();
                                    } else {
                                        self.checkout_viewport.move_up();
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(true)
            }
```

- [ ] **Step 3: Make `field_vp` and `current_descriptors` accessible from `app.rs`**

The Tab cycling code above accesses `config_tab.field_vp` directly and calls `current_descriptors`. `field_vp` is currently private (line 291 of config_tab.rs). Make it public:

In `src/tui/tabs/config_tab.rs`, change line 291:

```rust
    field_vp: Viewport,
```

to:

```rust
    pub field_vp: Viewport,
```

Also make `current_descriptors` pub (it may already be, but confirm):

```bash
grep -n "fn current_descriptors" /Volumes/Home/Users/wen/repos/ssync/src/tui/tabs/config_tab.rs
```

If the line reads `fn current_descriptors` (no `pub`), change it to `pub fn current_descriptors`.

Also make `reset_field_vp` pub:

```bash
grep -n "fn reset_field_vp" /Volumes/Home/Users/wen/repos/ssync/src/tui/tabs/config_tab.rs
```

If private, change to `pub fn reset_field_vp`.

- [ ] **Step 4: Build**

```bash
cargo build 2>&1 | tail -20
```

Fix any compile errors (likely missing `pub`, wrong method name, or import needed). Common fixes:
- `OperationKind` may need import: check existing `use` statements in `app.rs` for `OperationKind`.
- `OperateFocus` is already imported via `use super::tabs::operate_tab::{..., OperateFocus, ...}` (line 50).

- [ ] **Step 5: Commit**

```bash
git add src/tui/app.rs src/tui/tabs/config_tab.rs
git commit -m "feat: Tab key cycles within current focus layer (nav/config/operate/checkout)"
```

---

## Self-Review Checklist

- [x] **Task 1** covers BUG A `handle_inline_edit_key` inline edit Enter
- [x] **Task 2** covers BUG A entry form `active_input` Enter
- [x] **Task 3** covers BUG B entry form Esc; also rolls in global Esc→NavBar (requirement 5)
- [x] **Task 4** covers BUG C stray keys in entry form
- [x] **Task 5** covers requirement 4 (Operate initial focus)
- [x] **Task 6** covers requirement 1 (NavBar border highlight)
- [x] **Task 7** covers requirement 2 (FieldTable top-edge Up→NavBar)
- [x] **Task 8** covers requirement 6 (Tab cycling) — note: requirement 5 (global Esc→NavBar) is folded into Task 3

All 6 user requirements and 4 bugs are covered. No TBD placeholders remain.
