# Keybindings editor → Keybindings pane (settings phase 2) — design

Date: 2026-07-21
Status: draft, awaiting user review
Branch: `feat/settings-keybindings-pane` (off `main` @ `925c8ff`)
Follows: `2026-07-21-settings-window.md` (the modal→window conversion). This is
settings **phase 2** from `2026-07-17-settings-menu-design.md`.

## What and why

The settings menu is now a `Content` window, but the **keybindings editor**
(`settings.rs` `SettingsView`) is still a separate modal stacked on top of it,
reached from the Keybindings pane's single "Edit keybindings…" action. That's a
two-layer UX inside a window that is otherwise non-modal. Phase 2 folds the
editor into the **Keybindings pane** so selecting that pane shows the editor
inline — no trigger, no stacked modal.

The editor is a modal today for exactly one reason: **chord capture** must grab
all keyboard input so `Ctrl+B` (leader), `Ctrl+W`, etc. get captured as bindings
instead of triggering commands or closing a window. The design keeps that
guarantee without a modal, by grabbing input only for the brief moment a row is
listening (user-chosen approach: **fully inline + brief input-grab**).

Scope is a **shell swap + a capture-gating signal**, mirroring the menu
conversion. `SettingsView`'s pure model is untouched.

## Locked decisions

1. **Fully inline + brief input-grab.** The bindings list lives in the
   Keybindings pane; activating capture on a row makes the settings window grab
   all keyboard input (leader suppressed) until a chord is pressed or Esc
   cancels. No modal ever.
2. **Embed `SettingsView` inside `SettingsMenu`.** The pane owns the editor;
   cleanest place for the state.
3. **`Esc` is context-dependent:** cancels capture while `Mode::Capturing`,
   closes the window otherwise. The editor consumes Esc while capturing, so the
   window's Esc-closes rule only applies when the editor is idle.

## Structure (reuse — mirror the menu shell swap)

- `SettingsMenu` gains a `keybindings: SettingsView` field.
- `draw_pane` **special-cases `Pane::Keybindings`**: instead of the standard row
  list, it delegates to `SettingsView`, drawn into the pane rect. `SettingsView`
  loses its `egui::Window` wrapper + dim backdrop (`ui.ctx().content_rect()` +
  `from_black_alpha`) and draws directly into the given rect, exactly like
  `SettingsMenu::show` now does. The bindings list scrolls in the pane's
  existing `ScrollArea`.
- **Untouched:** `SettingsView`'s `Row`, `Mode` (`Idle`/`Capturing`/`Conflict`),
  `apply`, conflict handling, and keymap save — the pure model. Only its shell
  (draw + input routing) changes.

## Capture-gating (the crux)

`SettingsView` already holds capture state in `Mode::Capturing{row}`, which
**persists across frames** (until a chord is captured or cancelled).

- Expose `WindowManager::settings_capturing() -> bool` — true iff a
  `Content::Settings` window's embedded `SettingsView` is in `Mode::Capturing`.
- `pump_commands` (the leader pump, runs at the **top of the frame**) today gates
  on `keymap_editor.is_none()`. Replace that clause with
  **`!self.settings_capturing()`**. While capturing, the leader pump is
  suppressed, so `Ctrl+B` and any chord flow to the focused settings window's
  editor instead of triggering commands.
- Terminals are unfocused, so they don't read the keys; the editor's
  `Capturing` branch grabs the chord. Optionally swallow the captured chord's
  events after the editor reads them so nothing else can act on them that frame
  (same discipline the old modal used).
- **No ordering hazard:** capture is armed the frame the user presses Enter on a
  row; `pump_commands` reads that state at the *next* frame's start, so the
  leader is already suppressed by the time the next chord arrives.

## Removals (the modal path goes away)

- `keymap_editor: Option<SettingsView>` field (wm) and its `show_modals` draw
  block.
- `MenuOutcome::OpenKeybindings` and `drain_settings`'s arm for it.
- `Field::OpenKeybindings` (the "Edit keybindings…" action row) — the Keybindings
  pane no longer has an action; it *is* the editor.
- The `keymap_editor` term in the modal guards. `pump_commands` keeps a gate but
  swaps it for `!self.settings_capturing()` (suppress the leader during capture).
  `overlay_blocks_close` / `no_modal` / `apply_acts` simply **drop** the
  `keymap_editor` term — a pane is not a modal, so it no longer blocks
  close-confirms, focus, or background mouse acts. (Clicking a background window
  mid-capture is allowed and cancels the capture, rather than being swallowed.)

## Navigation

Same two-level model as the other panes: rail ↑↓ switches panes; diving into the
Keybindings pane (Enter/→) hands ↑↓/Enter to the editor's own key handling
(navigate bindings / start capture); Tab returns to the rail. When the pane is
Keybindings, `handle_keys` delegates row-level keys to the `SettingsView` instead
of the standard row nav.

## Testing

- `SettingsView`'s model tests stay green (model untouched).
- Add: `settings_capturing()` returns true iff the embedded editor is
  `Mode::Capturing`; the leader pump is suppressed while capturing (assert
  `pump_commands` does not dispatch a leader command when a settings window is
  capturing — wm-level unit test).
- All existing ~705 tests stay green.

## Visual verification

Second-instance build, open settings (`Ctrl+B Ctrl+,`), select Keybindings:
the editor renders inline (no separate modal, no dim backdrop). Start a capture
on a row, press `Ctrl+B` — confirm it's captured as the binding (leader did not
fire) and the window did not close; press `Esc` mid-capture — confirm it cancels
the capture and the window stays open. NEVER `Stop-Process foreman` (session runs
inside foreman); build with `--target-dir target/agent`.

## Docs

- `docs/settings-menu.md`: the Keybindings pane is the editor inline, not a modal
  trigger.
- `docs/epics/keyboard-control-epic.md`: the keybindings editor is a pane, not a
  desktop modal.

## Out of scope

Search/filter across bindings (phase-1 deferred). Any change to the keymap data
model, the chord format, or the conflict-resolution UX (reused as-is). Phase 3
(theme system + split-preview Appearance).
