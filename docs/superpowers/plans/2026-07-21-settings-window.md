# Settings modal → window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert foreman's settings menu from a desktop-level modal overlay into a first-class `Content::Settings` window (floating, closable, tabbable) that lives in the desktop `WindowManager`, mirroring `Content::Chat`.

**Architecture:** Add a `Content::Settings(SettingsMenu)` variant. The settings view (`SettingsMenu::show`) drops its dim backdrop + `egui::Window` wrapper and draws into a rect via a scoped child UI, gating key input on `active`. `open_settings` becomes an open-or-focus singleton (copied from `open_chat_window`). Outcomes propagate through the `drain_chat_clicks` pattern: the `Content::show` arm applies `Changed` in-place (`config::seed_live`) and stashes `Close`/`OpenKeybindings`/`CheckUpdatesNow` on the menu; a new `drain_settings()` acts on the WM after the render loop. The seed→apply→read-back→save cycle is preserved because the arm draws inside `desktop.show`, between `main.rs`'s seed and read-back.

**Tech Stack:** Rust, egui 0.34.3, eframe. Windows / PowerShell / GNU toolchain.

## Global Constraints

- **Build inside foreman:** this dev session runs INSIDE foreman (`$env:FOREMAN=1`). **NEVER `Stop-Process foreman`** — it kills the host. Build/test with `--target-dir target/agent` on EVERY cargo command so you don't touch the running exe's lock.
- **Bin-only crate:** test filters are `cargo test <filter> --target-dir target/agent`. **Never `--lib`** (errors "no library targets found").
- Toolchain: GNU (`stable-gnu`), w64devkit linker. Expected warning baseline ~22 (a transient "variant never constructed" warning is expected between Task 1 and Task 3).
- `Content::Settings` is **unboxed** (`Content::Settings(SettingsMenu)`), matching `Chat`/`TaskManager` — NOT `Box<...>`. `SettingsMenu` is tiny; `Session` dominates the enum size.
- **Settings is a normal non-panel window:** closable, tabbable, floatable, AND minimizable — zero capability special-casing (mirrors Chat). A minimized/buried Settings window is resurfaced by the same `Ctrl+B Ctrl+,` (open-or-focus un-minimizes it). *(This supersedes the spec's "non-minimizable" decision — see the deviation note in the spec's §Locked-decisions.)*
- **Commit policy:** the project commits only when the user asks. Commit steps below are the intended commit points; at each, pause for the user's go-ahead per subagent-driven-development review gates. Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` (or the executing session's own trailer).
- **Do NOT touch the pure model half** of `settings_menu.rs`: `Pane`, `Field`, `Kind`, `rows`, `adjust`, `display`, `bump`/`rank`, and their unit tests. Only `MenuOutcome` derives, the `SettingsMenu` struct field, `SettingsMenu::show`, `handle_keys`, and a new `size()` helper change.

---

### Task 1: Add `Content::Settings` variant + all match arms + snapshot exclusion

Adds the enum variant and every exhaustive-match arm it forces, so the code compiles with the variant present but not yet constructed. The old modal (`self.menu`, `show_modals` block, old `SettingsMenu::show`) is left **completely untouched** — the app still uses the modal after this task. Also adds the `MenuOutcome` derives and the `SettingsMenu.pending` field that Task 3 needs.

**Files:**
- Modify: `src/wm.rs` (Content enum ~117; arms at ~173, ~187, ~529/536, ~1777, ~3287, ~3310, ~5468; new test near the other `capture_*` tests ~5666)
- Modify: `src/settings_menu.rs` (MenuOutcome derives ~424; SettingsMenu struct ~362 + `new()` ~373)

**Interfaces:**
- Produces: `Content::Settings(SettingsMenu)` variant; `SettingsMenu.pending: Option<MenuOutcome>` field (init `None`); `MenuOutcome: Clone + Debug`.

- [ ] **Step 1: Add derives to `MenuOutcome`**

In `src/settings_menu.rs`, the enum at line ~425 currently has no derives. Add them on the line directly above `pub enum MenuOutcome {`:

```rust
#[derive(Clone, Debug)]
pub enum MenuOutcome {
```

(Required because `SettingsMenu` derives `Clone, Debug` and will gain a `MenuOutcome` field in Step 2.)

- [ ] **Step 2: Add the `pending` field to `SettingsMenu` + init it**

In `src/settings_menu.rs`, the struct at ~362 and `new()` at ~373. Add the field as the last struct member:

```rust
pub struct SettingsMenu {
    pub pane: Pane,
    pub row: usize,
    pub in_rail: bool,
    /// When `Some`, an inline text field is open for the selected `Text` row,
    /// holding the in-progress edit buffer. `None` = browsing.
    pub editing: Option<String>,
    /// A window-lifecycle outcome (Close / OpenKeybindings / CheckUpdatesNow)
    /// produced this frame, stashed for the WM's `drain_settings` to act on
    /// after the render loop (content cannot mutate the WM mid-loop).
    pub pending: Option<MenuOutcome>,
}
```

And in `new()` (~373), add `pending: None,` as the last initializer:

```rust
    pub fn new() -> Self {
        Self {
            pane: Pane::Terminal,
            row: 0,
            in_rail: true,
            editing: None,
            pending: None,
        }
    }
```

- [ ] **Step 3: Add a `size()` helper to `SettingsMenu`**

In `src/settings_menu.rs`, inside `impl SettingsMenu` (near `new()`), add:

```rust
    /// The menu's intrinsic content size (title + body + footer bands), used to
    /// size the floating window when it is first opened.
    pub fn size() -> egui::Vec2 {
        egui::vec2(WIN_W, TITLE_H + BODY_H + FOOTER_H)
    }
```

(`WIN_W`, `TITLE_H`, `BODY_H`, `FOOTER_H` are the existing module consts at ~418-422.)

- [ ] **Step 4: Add the `Content::Settings` variant**

In `src/wm.rs`, the `Content` enum at ~117. Add after the `Chat` variant:

```rust
    /// The settings menu as a desktop-level window (open-or-focus singleton).
    /// Floating/closable/tabbable like Chat; not persisted (see capture_manager).
    Settings(crate::settings_menu::SettingsMenu),
```

- [ ] **Step 5: Add the no-op/skip arms the compiler demands**

Add a `Content::Settings` arm at each exhaustive match. Follow the `Chat`/`TaskManager` precedent (no-op or skip):

`keepalive` (~173): add to the empty group —
```rust
            Content::Chat(_) | Content::TaskManager(_) | Content::Settings(_) => {}
```

`icon_kind` (~187): 
```rust
            Content::Settings(_) => None,
```

`panel_model` RowKind match (~1785): 
```rust
                            Content::TaskManager(_) | Content::Settings(_) => continue,
```

`refresh_exit_titles` (~3287) and `refresh_auto_titles` (~3310): add `Content::Settings(_)` to each `Content::Chat(_) | Content::TaskManager(_) => {}` arm.

`terminal_groups` (~5468): add `Content::Settings(_)` to the `Content::Chat(_) | Content::TaskManager(_) => Vec::new()` arm.

`Content::show` (~144): add a **temporary** arm (real logic lands in Task 3) so it compiles — draw nothing yet:
```rust
            Content::Settings(_) => false,
```

- [ ] **Step 6: Exclude Settings from the workspace snapshot**

In `src/wm.rs`, `capture_manager` (~512). The per-tab skip at ~529 currently skips only `TaskManager`; extend it:

```rust
                if matches!(t.content, Content::TaskManager(_) | Content::Settings(_)) {
                    continue;
                }
```

And the content match at ~536-545 has `Content::TaskManager(_) => unreachable!("filtered above")`; add Settings to it:

```rust
                    Content::TaskManager(_) | Content::Settings(_) => {
                        unreachable!("filtered above")
                    }
```

- [ ] **Step 7: Build to verify it compiles (variant unused is expected)**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20`
Expected: builds; a `variant Settings is never constructed` warning is OK (Task 3 constructs it). If the compiler names another exhaustive match missing an arm, add a no-op/skip `Content::Settings(_)` arm there too.

- [ ] **Step 8: Write the failing snapshot-exclusion test**

In `src/wm.rs`'s test module, near `capture_records_chat_tab` (~5666), add:

```rust
    #[test]
    fn capture_workspace_excludes_settings() {
        let mut m = WindowManager::new();
        let id = m.next;
        m.next += 1;
        m.z += 1;
        m.windows.push(Win {
            id,
            tabs: vec![Tab::fixed(
                "Settings",
                Content::Settings(crate::settings_menu::SettingsMenu::new()),
            )],
            active: 0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
            z: m.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
        let snap = crate::workspace::capture_manager(&m);
        assert!(
            snap.windows.iter().all(|w| w.id != id),
            "a settings window must be omitted from the workspace snapshot"
        );
    }
```

- [ ] **Step 9: Run the test — expect PASS**

Run: `cargo test capture_workspace_excludes_settings --target-dir target/agent`
Expected: PASS (the Step 6 filter drops the Settings-only window, so its tabs are empty and the window is omitted).

- [ ] **Step 10: Run the full suite to confirm no regressions**

Run: `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 15`
Expected: all pass (~700 + 1 new).

- [ ] **Step 11: Commit** *(await user go-ahead)*

```bash
git add src/wm.rs src/settings_menu.rs
git commit -m "feat(settings): add Content::Settings variant + snapshot exclusion

Adds the enum variant, every exhaustive-match arm it forces, the
MenuOutcome derives and SettingsMenu.pending field that the window
lifecycle needs. Modal is still the live path; the variant is dormant."
```

---

### Task 2: Rect-based `SettingsMenu::show` (shell-swap) — keep modal working

Converts `SettingsMenu::show` from a screen-dimming centered `egui::Window` into a method that draws into a passed `rect`, gated by `active`. The existing `show_modals` caller is updated to compute a centered rect and pass `active = true`, so **the modal keeps working with visual parity** — this is a pure view refactor with no lifecycle change. (Task 3 deletes that caller.)

**Files:**
- Modify: `src/settings_menu.rs` (`show` ~462; `handle_keys` gating via `show`)
- Modify: `src/wm.rs` (`show_modals` settings block ~4776-4797 — temporary caller update only)

**Interfaces:**
- Consumes: `SettingsMenu::size()` (Task 1).
- Produces: `SettingsMenu::show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool, s: &mut Settings) -> MenuOutcome`.

- [ ] **Step 1: Rewrite `SettingsMenu::show` to draw into `rect` via a scoped child UI**

In `src/settings_menu.rs`, replace the whole `show` body (~462-542) with:

```rust
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        s: &mut Settings,
    ) -> MenuOutcome {
        // Keyboard drives the menu only when this window is focused and no
        // inline text edit owns input.
        let mut outcome = if active && self.editing.is_none() {
            self.handle_keys(ui, s)
        } else {
            MenuOutcome::Pending
        };

        // Fill the window body (the WM draws the header/border chrome around it).
        ui.painter_at(rect).rect_filled(rect, 0.0, WIN_BG);

        // A scoped child UI bounded to `rect` so `override_text_color`/spacing
        // mutations stay local and never leak to sibling windows drawn later in
        // the same parent `ui` (idiom: landing.rs `ui.new_child`).
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let ui = &mut child;
        ui.set_min_width(WIN_W);
        ui.set_max_width(WIN_W);
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        ui.visuals_mut().override_text_color = Some(TEXT);
        let w = WIN_W;

        // --- title band ---
        let (title, _) = ui.allocate_exact_size(egui::vec2(w, TITLE_H), egui::Sense::hover());
        ui.painter().rect_filled(
            title,
            egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 },
            TITLE_BG_FOCUS,
        );
        ui.painter().text(
            egui::pos2(title.min.x + 18.0, title.center().y),
            egui::Align2::LEFT_CENTER,
            format!("Settings — {}", self.pane.label()),
            egui::FontId::proportional(15.0),
            TEXT,
        );

        // --- body: rail | pane ---
        let (body, _) = ui.allocate_exact_size(egui::vec2(w, BODY_H), egui::Sense::hover());
        let rail = egui::Rect::from_min_size(body.min, egui::vec2(RAIL_W, BODY_H));
        let pane =
            egui::Rect::from_min_max(egui::pos2(body.min.x + RAIL_W, body.min.y), body.max);
        self.draw_rail(ui, rail);
        self.draw_pane(ui, pane, s, &mut outcome);

        // --- footer ---
        let (footer, _) = ui.allocate_exact_size(egui::vec2(w, FOOTER_H), egui::Sense::hover());
        ui.painter().line_segment(
            [footer.left_top(), footer.right_top()],
            egui::Stroke::new(1.0, BORDER),
        );
        ui.painter().text(
            egui::pos2(footer.min.x + 18.0, footer.center().y),
            egui::Align2::LEFT_CENTER,
            "↑↓ navigate · Tab rail⇄pane · Enter edit · ←→ adjust · Esc close",
            egui::FontId::proportional(11.5),
            DIM,
        );

        outcome
    }
```

Changed from the original: removed the `ui.ctx().content_rect()` dim backdrop and the `egui::Window::new("settings_menu")...show(ui.ctx(), |ui| {...})` wrapper; the band layout that was inside the Window closure now runs inside `ui.new_child(...)`; `handle_keys` is gated on `active &&`.

- [ ] **Step 2: Update the temporary `show_modals` caller (keeps the modal alive this task)**

In `src/wm.rs`, the block at ~4776-4797 calls `menu.show(ui, &mut live_settings)`. Update **only that call** to compute a centered rect and pass `active = true`:

```rust
                let mut live_settings = (*crate::config::live(ui.ctx())).clone();
                let sz = SettingsMenu::size();
                let rect = egui::Rect::from_center_size(area.center(), sz);
                match menu.show(ui, rect, true, &mut live_settings) {
```

(`area` is the `show_modals` param — the desktop content rect. Everything else in the block is unchanged for now.)

- [ ] **Step 3: Build**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20`
Expected: compiles clean.

- [ ] **Step 4: Run the full suite**

Run: `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 15`
Expected: all pass (no new tests; the pure-model tests are untouched).

- [ ] **Step 5: Visual parity check (second instance — do NOT kill the host)**

Launch a second instance and screenshot it (see Task 4 Step 5 for the exact script), open settings with `Ctrl+B` then `Ctrl+,`. Confirm the menu still renders centered with rail/pane/footer, keyboard nav works, and edits apply live. It's still a modal (dim backdrop) at this stage — that's expected; Task 3 removes the backdrop. `Read` the PNG to confirm.

- [ ] **Step 6: Commit** *(await user go-ahead)*

```bash
git add src/settings_menu.rs src/wm.rs
git commit -m "refactor(settings): draw SettingsMenu into a rect, not an egui::Window

Adds rect+active params and a scoped child UI; the modal caller now
passes a centered rect. Pure view refactor, visual parity preserved."
```

---

### Task 3: Lifecycle switch — open-or-focus window, Content arm, drain, remove modal

Turns Settings into a real window: `open_settings` becomes an open-or-focus singleton, the `Content::show` arm drives the menu and propagates outcomes, a new `drain_settings()` acts on the WM, the `show_modals` settings block is deleted, and the `menu` field + all its guards are removed. This is the atomic modal→window switch and the primary review target (the seed→read-back cycle).

**Files:**
- Modify: `src/wm.rs` — `Content::show` arm (~144), `open_settings` (~2451), new `drain_settings` (near `drain_chat_clicks` ~1597), drain call site (~4494), delete `show_modals` block (~4769-4797), remove `menu` field (decl ~402, init ~475) and every `self.menu` reference (guards at ~2585, ~2603, ~2711, ~3424, ~4611, ~4681), new tests (~7071, ~9419)

**Interfaces:**
- Consumes: `SettingsMenu::show(ui, rect, active, s)` (Task 2), `SettingsMenu::size()` (Task 1), `SettingsMenu.pending` (Task 1), `next_slot`, `push_win`, `surface_target`, `close`, `Tab::fixed`, `TargetPath`.
- Produces: `open_settings` (open-or-focus), `drain_settings`.

- [ ] **Step 1: Write the failing singleton test**

In `src/wm.rs`'s test module, near `open_chat_window_is_a_singleton` (~7071), add:

```rust
    #[test]
    fn open_settings_is_a_singleton() {
        let mut wm = WindowManager::new();
        wm.open_settings();
        let count = |wm: &WindowManager| {
            wm.windows
                .iter()
                .filter(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Settings(_))))
                .count()
        };
        assert_eq!(count(&wm), 1);
        let first = wm.windows.last().unwrap().id;
        // focus something else, then reopen: focuses, does not duplicate
        wm.focused = None;
        wm.open_settings();
        assert_eq!(count(&wm), 1);
        assert_eq!(wm.focused, Some(first));
    }
```

- [ ] **Step 2: Write the failing deserted test**

Near `deserted_ignores_the_panel` (~9419), add:

```rust
    #[test]
    fn deserted_counts_a_settings_window() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        assert!(desk.deserted(), "a lone panel must not hold the app alive");
        desk.open_settings();
        assert!(
            !desk.deserted(),
            "an open settings window must hold the app alive"
        );
    }
```

- [ ] **Step 3: Run both tests — expect FAIL**

Run: `cargo test open_settings_is_a_singleton --target-dir target/agent` and `cargo test deserted_counts_a_settings_window --target-dir target/agent`
Expected: FAIL — `open_settings` still sets `self.menu` (creates no window), so `count == 0`; `deserted` still returns true.

- [ ] **Step 4: Rewrite `open_settings` as an open-or-focus singleton**

In `src/wm.rs` (~2451), replace the body:

```rust
    /// Open (or focus) the settings window — desktop-level singleton, mirroring
    /// `open_chat_window`. Closes the read-only help overlay if it was up.
    fn open_settings(&mut self) {
        self.show_help = false;
        if let Some((win, tab)) = self.windows.iter().find_map(|w| {
            w.tabs
                .iter()
                .position(|t| matches!(t.content, Content::Settings(_)))
                .map(|i| (w.id, i))
        }) {
            self.surface_target(crate::panel::TargetPath {
                project: win,
                ptab: None,
                window: None,
                tab: Some(tab),
            });
            return;
        }
        let (id, rect) = self.next_slot(SettingsMenu::size());
        self.push_win(
            id,
            Tab::fixed("Settings", Content::Settings(SettingsMenu::new())),
            rect,
        );
        self.mark_workspace_dirty();
    }
```

- [ ] **Step 5: Implement the real `Content::show` Settings arm**

In `src/wm.rs`, replace the temporary arm from Task 1 Step 5 (`Content::Settings(_) => false,`) at ~144 with:

```rust
            Content::Settings(menu) => {
                // Draw inside desktop.show, between main.rs's config::seed_live
                // and its read-back: read live settings, edit a clone, and on a
                // change republish via seed_live (same channel as font-zoom).
                // Window-lifecycle outcomes are stashed for drain_settings.
                let mut live = (*crate::config::live(ui.ctx())).clone();
                match menu.show(ui, rect, active, &mut live) {
                    MenuOutcome::Changed => crate::config::seed_live(ui.ctx(), &live),
                    MenuOutcome::Pending => {}
                    other => menu.pending = Some(other),
                }
                false
            }
```

- [ ] **Step 6: Add `drain_settings` next to `drain_chat_clicks`**

In `src/wm.rs`, after `drain_chat_clicks` (~1634), add:

```rust
    /// Apply a settings-window lifecycle outcome recorded during the draw
    /// (content cannot mutate the WM mid-loop — same discipline as
    /// `drain_chat_clicks`). Close removes the window; OpenKeybindings stacks
    /// the modal editor; CheckUpdatesNow latches the update fetch.
    fn drain_settings(&mut self) {
        let mut found = None;
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Settings(menu) = &mut t.content {
                    if let Some(o) = menu.pending.take() {
                        found = Some((w.id, o));
                    }
                }
            }
        }
        let Some((id, outcome)) = found else { return };
        match outcome {
            MenuOutcome::Close => self.close(id),
            MenuOutcome::OpenKeybindings => self.keymap_editor = Some(SettingsView::new()),
            MenuOutcome::CheckUpdatesNow => self.check_updates_requested = true,
            _ => {}
        }
    }
```

- [ ] **Step 7: Call `drain_settings` in the per-frame sequence**

In `src/wm.rs`, the tail of `WindowManager::show` (~4493-4494) calls `drain_chat_clicks` then `drain_chat_posts`. Add `drain_settings` right after, before the panel sync / `show_modals`:

```rust
        self.drain_chat_clicks();
        self.drain_chat_posts();
        self.drain_settings();
```

(Placing it before `show_modals` at ~4500 means an `OpenKeybindings` outcome sets `keymap_editor` in time for `show_modals` to draw the editor this same frame.)

- [ ] **Step 8: Delete the `show_modals` settings block**

In `src/wm.rs`, delete the comment (~4769-4775) and the entire `if self.keymap_editor.is_none() { if let Some(mut menu) = self.menu.take() {...} }` block (~4776-4797). **Do NOT touch** the keybindings-editor block immediately after it (`if let Some(mut editor) = self.keymap_editor.take() {...}`, ~4799+) — that stays.

- [ ] **Step 9: Remove the `menu` field and every reference**

Delete the field declaration (`menu: Option<SettingsMenu>`, ~402) and its `new()` initializer (`menu: None,`, ~475). Then remove the `menu` term from each guard (leave the `keymap_editor` term intact everywhere):
- `deserted` (~2585): delete `&& self.menu.is_none()`
- `should_show_landing` (~2603): delete `&& self.menu.is_none()`
- `overlay_blocks_close` (~2711): delete `|| self.menu.is_some()`
- `no_modal` (~3424): delete `&& self.menu.is_none()`
- `pump_commands` (~4611): delete `&& self.menu.is_none()`
- `apply_acts` mouse-drop (~4681): delete `|| self.menu.is_some()`

Then confirm nothing remains: `grep -n "self.menu" src/wm.rs` (should print nothing). The `use crate::settings_menu::{MenuOutcome, SettingsMenu};` import at wm.rs:4 stays (still used by `open_settings`, the arm, and `drain_settings`).

- [ ] **Step 10: Build**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20`
Expected: compiles clean; the Task 1 "variant never constructed" warning is now gone (open_settings constructs it).

- [ ] **Step 11: Run the new tests — expect PASS**

Run: `cargo test open_settings_is_a_singleton --target-dir target/agent` and `cargo test deserted_counts_a_settings_window --target-dir target/agent`
Expected: both PASS.

- [ ] **Step 12: Run the full suite**

Run: `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 15`
Expected: all pass (~700 + 3 new). Investigate any failure in `should_show_landing`/focus/leader tests — those are the guards you edited.

- [ ] **Step 13: Commit** *(await user go-ahead)*

```bash
git add src/wm.rs
git commit -m "feat(settings): settings menu is now a Content window

open_settings is an open-or-focus singleton; the Content::Settings arm
drives the menu and republishes edits via config::seed_live; drain_settings
handles close/open-keybindings/check-updates. Removes the modal field and
its input-swallow guards. Live-apply + save cycle preserved."
```

---

### Task 4: Docs, memory, and visual verification

**Files:**
- Modify: `docs/settings-menu.md`
- Modify: `C:\Users\sniff\.claude\projects\H--claude-code-foreman\memory\settings-menu-design.md` + `MEMORY.md`

- [ ] **Step 1: Update `docs/settings-menu.md`**

Change "modal overlay" phrasing to "window". Document: opens via `Ctrl+B Ctrl+,` as an open-or-focus floating `Content::Settings` window; closable (Esc when focused, or the header close button); tabbable/floatable; keeps the app alive like a project; not persisted across restarts; the keybindings editor still opens as a modal on top. Add `src/settings_menu.rs`, the `Content::Settings` arm, `open_settings`, and `drain_settings` to the Key files section.

- [ ] **Step 2: Update auto-memory**

In `settings-menu-design.md`, mark the modal→window conversion done and note the roadmap (phase 2 = keybindings editor as a pane; phase 3 = theme system + split-preview Appearance). Update the `MEMORY.md` one-line pointer.

- [ ] **Step 3: Build a release-ish second instance for the visual pass**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 5`
Then launch a SECOND instance (safe — separate process; do NOT kill the host):
`Start-Process "H:\claude code\foreman\target\agent\debug\foreman.exe"`

- [ ] **Step 4: Open settings and screenshot**

In the new instance, open a project (so there's a terminal beside settings), then press `Ctrl+B` then `Ctrl+,`. Use the Win32 screenshot script from `docs/HANDOFF.md` §3 to capture the foreman window to a PNG in the scratchpad dir.

- [ ] **Step 5: Read the PNG and verify**

`Read` the screenshot. Confirm: **no dim backdrop**; real window chrome (header + close button); the window floats and can be dragged/tiled/tabbed; the terminal beside it is still live; toggling a setting (e.g. Dim-unfocused) visibly repaints the terminal while settings stays open. Screenshot again after toggling to prove live-apply is visible.

- [ ] **Step 6: Confirm persistence**

Toggle a setting, wait ~1s, then check `%APPDATA%\foreman\settings.json` reflects the change (the ~400ms debounce fired). This proves the seed→read-back→save cycle survived the refactor.

- [ ] **Step 7: Commit docs** *(await user go-ahead)*

```bash
git add docs/settings-menu.md
git commit -m "docs(settings): settings menu is a window, not a modal"
```

---

## Self-Review

**Spec coverage:**
- Data model (`Content::Settings` + arms, snapshot exclusion, icon None) → Task 1. ✓
- Open-or-focus singleton, floating → Task 3 Step 4. ✓
- Keep-alive (`deserted` counts Settings) → Task 3 Steps 2/9 + test. ✓
- Focus-gated input (`active` gate) → Task 2 Step 1 + Task 3 (guards removed). ✓
- Esc-close (when focused/not editing) → `handle_keys` returns Close (unchanged) + Task 3 arm/drain. ✓
- Load-bearing cycle (seed→apply→re-seed→read-back→save) → Task 3 Step 5 (arm) preserves it. ✓
- View shell-swap (drop backdrop/Window, draw into rect) → Task 2 Step 1. ✓
- Keybindings editor stays modal → untouched (Task 3 Step 8 preserves the editor block; OpenKeybindings routes via drain). ✓
- Not persisted → Task 1 Step 6 + test. ✓
- Tests (singleton, deserted, snapshot) → Tasks 1/3. ✓
- Docs/memory → Task 4. ✓
- **Deviation from spec:** minimizable (not non-minimizable) — noted in Global Constraints; update the spec's decision 6.

**Placeholder scan:** No TBD/TODO; every code step shows full code. The Task 1 `Content::show` arm is an explicit temporary (`=> false`) replaced in Task 3 Step 5, not a placeholder.

**Type consistency:** `SettingsMenu::show(ui, rect, active, s) -> MenuOutcome` defined in Task 2, consumed in Task 3 arm. `SettingsMenu::size() -> egui::Vec2` defined Task 1, used in Task 2 (show_modals) and Task 3 (open_settings). `SettingsMenu.pending: Option<MenuOutcome>` set in Task 3 arm, taken in `drain_settings`. `MenuOutcome: Clone + Debug` (Task 1) satisfies the `SettingsMenu` derive. `close(id)`, `surface_target(TargetPath{...})`, `next_slot(size) -> (WinId, Rect)`, `push_win(id, tab, rect)`, `Tab::fixed(title, content)` all match the extracted signatures.
