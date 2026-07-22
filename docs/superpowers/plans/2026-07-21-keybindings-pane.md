# Keybindings editor → Keybindings pane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fold the keybindings editor (`SettingsView`) into the settings window's Keybindings pane, inline — removing the stacked modal — with chord capture that briefly grabs all input.

**Architecture:** `SettingsView` loses its `egui::Window`/backdrop and draws into the pane rect (mirror the menu conversion). It is embedded in `SettingsMenu`; the Keybindings pane delegates to it. Because the keymap lives in the WM but `Content::show` can't reach it, the keymap flows through a **ctx seam** (`keymap::seed_live`/`live`) identical to `config::seed_live` for settings: the WM seeds `self.keymap` each frame, the Keybindings pane edits a clone and re-seeds on change, the WM reads it back after the render loop and saves. Capture-gating: a `settings_capturing()` query read at frame-start by `pump_commands` suppresses the leader while a chord is being captured. The modal path (`keymap_editor`, `MenuOutcome::OpenKeybindings`, the `show_modals` editor block) is deleted.

**Tech Stack:** Rust, egui 0.34.3, eframe. Windows / PowerShell / GNU toolchain.

## Global Constraints

- **Build inside foreman:** this dev session runs INSIDE foreman. **NEVER `Stop-Process foreman`.** Every cargo command uses `--target-dir target/agent`. **Never** `--lib` (bin-only crate). Long Bash timeout (up to 600000 ms) for cargo.
- Expected warning baseline ~22. Test census ~719 `#[test]` attributes (some `#[ignore]`d); the running-count is ~705 passing.
- **Pure model is untouched:** `SettingsView::apply`, `render_rows`, `capture_chord`, the `Mode`/`Row` state machine, and `Keymap`'s `resolve`/`rebind`/`set_leader`/`save`/`load` — logic unchanged. Only derives, `show`'s shell, input routing, and the WM wiring change.
- **The keymap seam mirrors `config::seed_live` exactly** (per-frame `Arc<Keymap>` in `ctx` data). This is the load-bearing cycle — the review target.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Commit only the exact files (`git add <paths>`, never `-A`); no `--no-verify`. Commits are authorized as part of executing this plan.

---

### Task 1: Infrastructure — derives + the keymap ctx seam

Adds the `Clone`/`Debug`/`PartialEq` derives the embedding needs and the `keymap::seed_live`/`live` seam. Nothing uses the seam yet; the modal editor still works unchanged after this task.

**Files:**
- Modify: `src/settings.rs` (`Row` ~27, `Mode` ~34, `SettingsView` ~50)
- Modify: `src/keymap.rs` (`Keymap` struct ~301; add seam fns)

**Interfaces:**
- Produces: `SettingsView: Clone + Debug`; `Keymap: Clone + PartialEq`; `keymap::seed_live(ctx, &Keymap)`, `keymap::live(ctx) -> std::sync::Arc<Keymap>`.

- [ ] **Step 1: Derives on the editor types (so `SettingsView` can embed in `SettingsMenu`, which derives `Clone, Debug`).**

`src/settings.rs`:
- `Row` (~27): `#[derive(Clone, Copy, PartialEq)]` → `#[derive(Clone, Copy, PartialEq, Debug)]`.
- `Mode` (~34): it has no derives — add `#[derive(Clone, Debug)]` on the line above `enum Mode {`. (`Conflict` holds `Chord`+`Command`, both `Clone+Debug`; `Row` is now `Debug`.)
- `SettingsView` (~50): add `#[derive(Clone, Debug)]` on the line above `pub struct SettingsView {`.

- [ ] **Step 2: `Clone` + `PartialEq` on `Keymap` (for the seam clone + read-back compare).**

`src/keymap.rs`, the `Keymap` struct (~301) currently has no derives. Add:
```rust
#[derive(Clone, PartialEq)]
pub struct Keymap {
    pub leader: Chord,
    table: HashMap<Chord, Command>,
}
```
(`Chord` is `Clone+PartialEq+Eq+Hash`; `Command` is `Clone+PartialEq+Eq+Hash`; `HashMap<K,V>: Clone+PartialEq` when both are.)

- [ ] **Step 3: Add the keymap ctx seam (mirror `config::seed_live`).**

`src/keymap.rs`, at module level (near the top-level fns, e.g. after `Keymap`'s impl). Copy the shape of `config::seed_live`/`live` (`src/config.rs:235-244`):
```rust
/// Per-frame publish of the live keymap into egui ctx data, so the settings
/// window's Keybindings pane (drawn deep in the render loop, with no access to
/// the WM) can read + edit it. Same seam as `config::seed_live`.
pub fn seed_live(ctx: &eframe::egui::Context, k: &Keymap) {
    let arc = std::sync::Arc::new(k.clone());
    ctx.data_mut(|d| d.insert_temp(eframe::egui::Id::new("foreman::keymap"), arc));
}

/// The keymap seeded this frame (defaults before the first seed).
pub fn live(ctx: &eframe::egui::Context) -> std::sync::Arc<Keymap> {
    ctx.data_mut(|d| d.get_temp(eframe::egui::Id::new("foreman::keymap")))
        .unwrap_or_else(|| std::sync::Arc::new(Keymap::default()))
}
```

- [ ] **Step 4: Write the failing seam round-trip test.**

`src/keymap.rs` test module (~662), add:
```rust
    #[test]
    fn seed_live_round_trips_the_keymap() {
        let ctx = egui::Context::default();
        let mut km = Keymap::default();
        km.set_leader(Chord::new(egui::Key::Y, false, false, false));
        seed_live(&ctx, &km);
        assert_eq!(*live(&ctx), km);
    }
```

- [ ] **Step 5: Build + run the new test + full suite.**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20`
Then: `cargo test seed_live_round_trips_the_keymap --target-dir target/agent` (expect PASS) and `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 5` (expect all green).

- [ ] **Step 6: Commit** *(pause for review per SDD)*

```bash
git add src/settings.rs src/keymap.rs
git commit -m "feat(keybindings): derives + keymap ctx seam for inline editing

Clone/Debug on SettingsView (+ Row/Mode), Clone/PartialEq on Keymap, and
keymap::seed_live/live mirroring config::seed_live — the plumbing an inline
Keybindings pane needs. Nothing uses the seam yet."
```

---

### Task 2: `SettingsView` draws inline into a rect (shell swap)

Converts `SettingsView::show` from a modal (`egui::Window` + dim backdrop) to drawing into a given rect, mirroring `SettingsMenu::show`. The `show_modals` caller is updated to pass a centered rect so the **modal keeps working** this task (deleted in Task 3). Adds `is_capturing()`.

**Files:**
- Modify: `src/settings.rs` (`show` ~159; add `is_capturing`)
- Modify: `src/wm.rs` (`show_modals` editor block ~4863 — temporary caller update)

**Interfaces:**
- Produces: `SettingsView::show(&mut self, ui, rect: egui::Rect, km: &mut Keymap) -> Outcome`; `SettingsView::is_capturing(&self) -> bool`.

- [ ] **Step 1: Rewrite `SettingsView::show` to draw into `rect`.**

Keep the entire input-handling `match self.mode { ... }` block (lines ~166-220) **unchanged**. Replace the modal wrapper (the `let screen = ui.ctx().content_rect();` backdrop + `egui::Window::new("keybindings")...show(ui.ctx(), |ui| {...})`, ~222-283) with a scoped child UI bounded to `rect` (same idiom as `SettingsMenu::show`), preserving the inner body verbatim:
```rust
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, km: &mut Keymap) -> Outcome {
        let mut changed = false;
        let mut close = false;

        // --- input --- (UNCHANGED: the `match self.mode { Capturing / Conflict / Idle }`
        //                block from the current show, verbatim — capture_chord, apply,
        //                move_sel, start_capture.)

        // --- draw into the pane rect (was a dim backdrop + centered egui::Window) ---
        ui.painter_at(rect).rect_filled(rect, 0.0, WIN_BG);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let ui = &mut child;
        ui.set_clip_rect(rect);
        ui.visuals_mut().override_text_color = Some(TEXT);
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Keyboard bindings").color(BORDER_FOCUS).size(16.0).strong(),
        );
        ui.label(
            egui::RichText::new("j/k or ↑/↓ select · Enter rebind · Esc back / cancel capture")
                .color(DIM).size(11.5),
        );
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_salt("keybindings_pane")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.render_rows(ui, km, &mut changed);
            });
        ui.add_space(6.0);
        ui.separator();
        if let Some(msg) = &self.message {
            ui.label(egui::RichText::new(msg).color(DIM).size(11.5));
        }
        ui.horizontal(|ui| {
            if ui.button("Reset all to defaults").clicked() {
                km.reset_all();
                self.mode = Mode::Idle;
                self.message = Some("All bindings reset to defaults.".into());
                changed = true;
            }
        });

        if close { Outcome::Close } else if changed { Outcome::Changed } else { Outcome::Pending }
    }
```
Notes: dropped the `set_min_width(540)`/`set_max_width(540)` clamp (the pane sizes it) and the "Close" button (inline, Esc/Tab back out — see Task 3). `WIN_BG`, `BORDER_FOCUS`, `TEXT`, `DIM` are already glob-imported (`use crate::theme::*`).

- [ ] **Step 2: Add `is_capturing`.**

`src/settings.rs`, `impl SettingsView`:
```rust
    /// True while waiting for the user to press a chord to bind. The WM reads
    /// this to suppress the leader so any chord (incl. the leader itself) is
    /// captured instead of dispatched.
    pub fn is_capturing(&self) -> bool {
        matches!(self.mode, Mode::Capturing { .. })
    }
```

- [ ] **Step 3: Update the `show_modals` caller (keep the modal alive this task).**

`src/wm.rs` (~4864): `editor.show(ui, &mut self.keymap)` → compute a centered rect and pass it:
```rust
            let sz = egui::vec2(540.0, 460.0);
            let rect = egui::Rect::from_center_size(area.center(), sz);
            let outcome = editor.show(ui, rect, &mut self.keymap);
```
(`area` is `show_modals`'s content-rect param. Keep the dim backdrop the modal block already paints, if any, so it still reads as a modal this task — if the backdrop was only inside `SettingsView`, add `ui.painter().rect_filled(area, 0.0, egui::Color32::from_black_alpha(170));` before the call so parity holds until Task 3 deletes this block.)

- [ ] **Step 4: Build + full suite.**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` then `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 5`. Expect green (the `SettingsView::apply` model tests are untouched; `show` has no unit test).

- [ ] **Step 5: Commit** *(pause for review)*

```bash
git add src/settings.rs src/wm.rs
git commit -m "refactor(keybindings): editor draws into a rect, not an egui::Window

Adds a rect param + is_capturing; the modal caller passes a centered rect.
Pure view refactor; the state machine and render_rows are unchanged."
```

---

### Task 3: The switch — inline Keybindings pane, keymap seam, capture-gating, remove the modal

The atomic modal→inline switch. Embed `SettingsView` in `SettingsMenu`; the Keybindings pane renders it; the keymap flows through the seam (WM seeds → arm edits a clone → WM reads back + saves); `settings_capturing()` suppresses the leader during capture; the modal path is deleted. This is the review target.

**Files:**
- Modify: `src/settings_menu.rs` — `SettingsMenu` struct/new (~362), `show` (~500), `draw_pane` (~718), `handle_keys` (~581), `rows()` Keybindings arm (~174), `Field`/`do_action`/`MenuOutcome`
- Modify: `src/wm.rs` — `Content::Settings` arm (~169), `WindowManager::show` (seed + read-back), `pump_commands` (~4699), `drain_settings` (~1664), delete `keymap_editor` (~421) + `show_modals` block (~4863), guards (~2790/~3502/~4771), add `settings_capturing()`

**Interfaces:**
- Consumes: `keymap::seed_live`/`live` (T1), `SettingsView::show(ui, rect, km)` + `is_capturing` (T2).
- Produces: `SettingsMenu::show(..., km: &mut Keymap)`, `SettingsMenu::is_capturing()`, `WindowManager::settings_capturing()`.

- [ ] **Step 1: Write the failing capture-gating test** (leader suppressed while a settings window is capturing).

`src/wm.rs` test module, mirroring `leader_stays_dormant_while_a_widget_holds_focus` (~7437):
```rust
    #[test]
    fn leader_is_suppressed_while_the_keybindings_pane_captures() {
        let mut wm = WindowManager::new();
        wm.desktop = true; // not as_desktop(): avoids loading the user's keymap file
        // A focused settings window whose Keybindings editor is capturing.
        wm.open_settings();
        let sid = wm.windows.last().unwrap().id;
        wm.focused = Some(sid);
        // Put the embedded editor into capture on the Keybindings pane.
        {
            let w = wm.windows.iter_mut().find(|w| w.id == sid).unwrap();
            let Content::Settings(menu) = &mut w.tabs[w.active].content else { panic!() };
            menu.pane = crate::settings_menu::Pane::Keybindings;
            menu.in_rail = false;
            menu.begin_capture_for_test(); // helper added in Step 5
        }
        assert!(wm.settings_capturing(), "settings_capturing reflects Mode::Capturing");
        let leader = wm.keymap.leader;
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: leader.key, physical_key: None, pressed: true, repeat: false,
            modifiers: egui::Modifiers { ctrl: leader.ctrl, shift: leader.shift, alt: leader.alt, ..Default::default() },
        });
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { wm.pump_commands(ui, true); });
        });
        assert!(!wm.armed, "the leader must stay dormant while the pane is capturing");
    }
```

- [ ] **Step 2: Embed `SettingsView` in `SettingsMenu`.**

`src/settings_menu.rs`: add `use crate::settings::{Outcome as EditorOutcome, SettingsView};` near the top. Add field to the struct (~372, after `pending`) and init in `new()`:
```rust
    /// The keybindings editor, rendered inline when the Keybindings pane is
    /// active (replaces the old stacked modal).
    pub keybindings: SettingsView,
```
`new()`: add `keybindings: SettingsView::new(),`. (`SettingsView` is `Clone + Debug` after T1, so `SettingsMenu`'s derives still hold.)

- [ ] **Step 3: Thread `&mut Keymap` through `SettingsMenu::show` → `draw_pane`; delegate the Keybindings pane to the editor.**

`SettingsMenu::show` (~500): add `km: &mut Keymap` param (after `s: &mut Settings`). Pass it to `draw_pane` (change `draw_pane`'s signature to take `km: &mut Keymap`).

In `draw_pane` (~718), branch before the standard row loop:
```rust
        if self.pane == Pane::Keybindings {
            match self.keybindings.show(ui, rect, km) {
                EditorOutcome::Changed => bump(outcome, MenuOutcome::Changed),
                EditorOutcome::Close => self.in_rail = true, // Esc/back-out returns to the rail
                EditorOutcome::Pending => {}
            }
            return;
        }
        // ... existing ScrollArea row loop for the other panes ...
```
(`bump` is the existing outcome-merge fn. `MenuOutcome::Changed` here means "keymap changed" — the arm re-seeds the keymap; see Step 6.)

- [ ] **Step 4: Delegate keyboard to the editor on the Keybindings pane in `handle_keys`.**

`handle_keys` (~581): after reading the 7 keys and handling `esc`/`tab`, and before the `in_rail`/row branches, add:
```rust
        // The Keybindings pane's editor reads its own input in draw_pane; don't
        // double-handle its keys here. Tab still backs out to the rail.
        if self.pane == Pane::Keybindings && !self.in_rail {
            if tab {
                self.in_rail = true;
            }
            return MenuOutcome::Pending;
        }
```
**Important:** this must run BEFORE the `if esc { return Close }` line, so Esc inside the editor is left for `SettingsView::show` (cancel-capture / back-to-rail) instead of closing the window. Move the `if esc { return MenuOutcome::Close; }` to AFTER this delegation block. Diving in from the rail already works (the `in_rail` branch's `Enter || right` clears `in_rail`, entering the editor).

- [ ] **Step 5: Add `is_capturing` accessors + the test helper.**

`SettingsMenu` (`impl`): 
```rust
    pub fn is_capturing(&self) -> bool {
        self.pane == Pane::Keybindings && self.keybindings.is_capturing()
    }
    #[cfg(test)]
    pub fn begin_capture_for_test(&mut self) {
        self.keybindings.begin_capture_for_test();
    }
```
`SettingsView` (`impl`, `src/settings.rs`):
```rust
    #[cfg(test)]
    pub fn begin_capture_for_test(&mut self) {
        self.start_capture();
    }
```

- [ ] **Step 6: Wire the keymap seam in the `Content::Settings` arm.**

`src/wm.rs`, the arm (~169). It currently reads only live settings. Add the live keymap, pass both to `menu.show`, and re-seed the keymap on a `Changed` outcome:
```rust
            Content::Settings(menu) => {
                let mut live = (*crate::config::live(ui.ctx())).clone();
                let mut km = (*crate::keymap::live(ui.ctx())).clone();
                match menu.show(ui, rect, active, &mut live, &mut km) {
                    MenuOutcome::Changed => {
                        crate::config::seed_live(ui.ctx(), &live);
                        crate::keymap::seed_live(ui.ctx(), &km);
                    }
                    MenuOutcome::Pending => {}
                    other => menu.pending = Some(other),
                }
                ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_click())
            }
```
(Re-seeding both on `Changed` is safe: the WM/main read-backs compare by value, so an unchanged clone is a no-op.)

- [ ] **Step 7: Seed the keymap + read it back in `WindowManager::show`.**

At the **top** of `WindowManager::show` (before the render loop / `pump_commands`), seed:
```rust
        if self.desktop {
            crate::keymap::seed_live(&ctx, &self.keymap);
        }
```
After the render loop and `drain_settings` (alongside where settings would be adopted — but settings is read back in `main.rs`; the keymap is WM-owned so read it back here), add:
```rust
        if self.desktop {
            let live_km = crate::keymap::live(&ctx);
            if *live_km != self.keymap {
                self.keymap = (*live_km).clone();
                if let Err(e) = self.keymap.save() {
                    self.set_keybindings_save_error(e);
                }
            }
        }
```
Add the helper (find the focused settings window's embedded editor, surface the error):
```rust
    fn set_keybindings_save_error(&mut self, msg: String) {
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Settings(menu) = &mut t.content {
                    menu.keybindings.set_save_error(msg.clone());
                }
            }
        }
    }
```

- [ ] **Step 8: Add `settings_capturing()` + gate `pump_commands`.**

`src/wm.rs`, mirror `drain_settings`'s scan but read-only + focus-checked:
```rust
    /// True iff the focused settings window's Keybindings pane is mid chord-
    /// capture — the WM suppresses the leader so the chord is captured, not
    /// dispatched.
    fn settings_capturing(&self) -> bool {
        let Some(fid) = self.focused else { return false };
        self.windows.iter().any(|w| {
            w.id == fid
                && matches!(&w.tabs[w.active].content, Content::Settings(menu) if menu.is_capturing())
        })
    }
```
`pump_commands` (~4699): remove `&& self.keymap_editor.is_none()` and add `&& !self.settings_capturing()`.

- [ ] **Step 9: Delete the modal path.**

- `keymap_editor: Option<SettingsView>` field (~421) + its `None` init (~493).
- The whole `if let Some(mut editor) = self.keymap_editor.take() { ... }` block in `show_modals` (~4863-4876) and its comment.
- `drain_settings`'s `MenuOutcome::OpenKeybindings => { self.keymap_editor = Some(...); true }` arm (~1683) — delete it (fall through to `_ => false`).
- `overlay_blocks_close` (~2790), `no_modal` (~3502), `apply_acts` guard (~4771): drop the `self.keymap_editor.is_some()`/`.is_none()` term.
- `src/settings_menu.rs`: `MenuOutcome::OpenKeybindings` variant (~469) + its `bump` rank arm (~487); `Field::OpenKeybindings` (~65); `do_action`'s `Field::OpenKeybindings => ...` arm (~665); the `rows()` `Pane::Keybindings` arm (~174) — replace with an empty slice `Pane::Keybindings => &[],` (the pane is now the editor; `draw_pane`/`handle_keys` special-case it before touching `rows()`).
- Fix the now-unused `SettingsView`/`SettingsOutcome` import in wm.rs (~3): `SettingsView` is still used by the embedded field's type indirectly, but the wm no longer names it — remove `SettingsView` from the import if the compiler flags it unused; keep whatever remains needed.
- Grep to confirm: `grep -n "keymap_editor\|OpenKeybindings" src/wm.rs src/settings_menu.rs` prints nothing.

- [ ] **Step 10: Build; fix the exhaustiveness/borrow fallout the compiler names.**

Run: `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 30`. Expect it to flag: the `rows()`/`adjust()`/`display()` match arms that referenced `Field::OpenKeybindings` (remove them — an empty Keybindings `rows()` means `handle_keys`'s `rows(self.pane)[self.row]` must never run for that pane, which Step 4's early return guarantees); any remaining `MenuOutcome::OpenKeybindings` match arm. Resolve each; re-run until clean at the ~22 warning baseline.

- [ ] **Step 11: Run the capture-gating test + full suite.**

Run: `cargo test leader_is_suppressed_while_the_keybindings_pane_captures --target-dir target/agent` (expect PASS) and `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 8` (expect all green; investigate any focus/leader/`drain_settings_opens_keybindings_editor` failures — that last test asserted the deleted OpenKeybindings path and must be **removed or rewritten** to assert the editor is now inline).

- [ ] **Step 12: Commit** *(pause for review — this is the foreman-reviewer target)*

```bash
git add src/settings_menu.rs src/wm.rs src/settings.rs
git commit -m "feat(keybindings): editor is the Keybindings pane, not a modal

Embeds SettingsView in SettingsMenu; the pane renders it inline. The keymap
flows through keymap::seed_live/live (WM seeds, the pane edits a clone, the WM
reads back + saves) like the settings cycle. settings_capturing() suppresses
the leader during chord capture. Removes the keymap_editor modal, the
OpenKeybindings outcome/field, and the show_modals editor block."
```

---

### Task 4: Docs + visual verification

**Files:** `docs/settings-menu.md`, `docs/epics/keyboard-control-epic.md`, spec + plan (this file).

- [ ] **Step 1: Update `docs/settings-menu.md`** — the Keybindings pane *is* the editor inline (leader, chords, conflicts); no stacked modal. Add a note on capture: while binding a key the window briefly grabs all input (leader suppressed); Esc cancels the capture / backs out to the rail.

- [ ] **Step 2: Update `docs/epics/keyboard-control-epic.md`** — the keybindings editor is a settings pane, not a desktop modal; capture-gating via `settings_capturing()`.

- [ ] **Step 3: Build + launch a second instance** (`--target-dir target/agent`, `Start-Process ...target\agent\debug\foreman.exe` — never kill the host). Open settings (`Ctrl+B` then `Ctrl+,`), select **Keybindings**.

- [ ] **Step 4: Verify + screenshot** (Read the PNG): the editor renders **inline** in the pane (no separate modal, no dim backdrop). Start a rebind on a row; press **`Ctrl+B`** → confirm it's captured as the binding (the leader did **not** fire, no window closed). Press **`Esc`** mid-capture → the capture cancels, the window stays open. Rebind a real command, confirm `%APPDATA%\foreman\keybindings.json` updated.

- [ ] **Step 5: Commit docs** *(pause for review)*

```bash
git add docs/settings-menu.md docs/epics/keyboard-control-epic.md docs/superpowers/specs/2026-07-21-keybindings-pane.md docs/superpowers/plans/2026-07-21-keybindings-pane.md
git commit -m "docs(keybindings): editor is a settings pane, not a modal"
```

---

## Self-Review

**Spec coverage:** Fully-inline + input-grab → Task 3 (seam + delegation) + capture-gating. Embed in `SettingsMenu` → T3 Step 2. Esc context-dependent → T3 Step 4 (delegation moves Esc past the window-close) + `SettingsView` Idle-Esc→`Close`→`in_rail=true`. Removals (keymap_editor, OpenKeybindings, show_modals block, guards) → T3 Step 9. Capture-gating (`settings_capturing` read by `pump_commands`) → T3 Step 8. Model untouched → only derives/shell/wiring change. Tests (seam, capturing-suppresses-leader) → T1/T3. Docs → T4. ✓

**Placeholder scan:** No TBD/TODO. The T2 caller update and empty-`rows()` Keybindings arm are explicit temporaries/consequences, not placeholders. The "verbatim, unchanged" `match self.mode` block in T2 Step 1 references the exact current lines rather than re-pasting — the engineer keeps them as-is.

**Type consistency:** `SettingsView::show(ui, rect, km) -> Outcome` (T2) consumed by `draw_pane` (T3). `keymap::seed_live/live` (T1) used by the arm + WM (T3). `SettingsMenu::show` gains `km: &mut Keymap` (T3 Step 3), supplied by the arm (T3 Step 6). `settings_capturing()`/`is_capturing()` chain (T3 Steps 5/8). `Keymap: Clone+PartialEq` (T1) used by the seam clone + WM read-back compare (T3 Step 7). `EditorOutcome`/`MenuOutcome` mapping in `draw_pane` (T3 Step 3).

**Risk flagged for the reviewer:** the keymap seam cycle (WM seed → arm clone/edit/re-seed → WM read-back + save) is the load-bearing part — same class as the settings cycle. The `pump_commands` frame-ordering (reads last-frame capture state at frame-start) and the Esc-delegation ordering in `handle_keys` are the two subtle correctness points.
