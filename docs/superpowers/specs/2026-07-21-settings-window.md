# Settings modal → window — design

Date: 2026-07-21
Status: draft, awaiting user review
Branch: `feat/settings-menu` @ `0bfd7cd`
Supersedes the "UI shell" of `2026-07-17-settings-menu-design.md` (modal → window);
everything else in that phase-1 spec still holds.

## What and why

Settings ships today as a desktop-level **modal overlay** (`SettingsMenu`, drawn
over a dim backdrop, swallowing all input). Foreman's identity is "everything is
a pane you can tile" — task manager and chat are already `Content` windows; the
settings modal is the one surface that refuses foreman's own window manager. A
window also makes live-apply **visible**: a modal covers the terminals, so you
can't watch bell-pulse-speed / dim-unfocused change as you drag a slider.

This is a **shell swap, not a re-plan.** The pure model, every `adjust`/`display`,
all consumer wiring, and persistence stay exactly as-is. Only presentation,
input-routing, and lifecycle change. The precedents to mirror are
`Content::Chat` + `open_chat_window`, but at **desktop** level (settings is
opened on `self.desktop`, so it tabs with projects, not terminals — same-manager
rule).

## Locked decisions

1. **Keep-alive:** a Settings window keeps the app alive. It is a non-panel
   `Win`, so `deserted()`'s existing `windows.iter().all(is_panel)` clause
   already returns `false` while it is open — no new keep-alive logic, only
   removal of the dead `menu.is_none()` guards.
2. **Placement:** **floating** by default (mirrors `open_chat_window`'s
   `next_slot`). User can tile/tab it afterward.
3. **Esc:** **Esc closes** the Settings window, but only when it is focused and
   not mid-text-edit. A window-specific convention (no other foreman window
   closes on Esc); settings users expect it.
4. **Input routing:** focus-gated through the normal cascade. Settings reads
   keys only when its window is focused — never as a background window, and
   background terminals never eat settings keys. (Technical given, not a fork.)
5. **Keybindings editor stays modal.** Chord capture needs total input theft
   (`Ctrl+W` mid-capture must not close the window). Only its *trigger* moves.
6. **Minimizable (revised).** Originally locked as *non*-minimizable, but
   anchor extraction showed `Win` has no capability flags — closable/tabbable/
   minimizable are all derived from `is_panel()`, so "non-minimizable" would
   mean special-casing `minimize()` AND handling the edge where a Settings tab
   rides on a minimized project. That fights the "mirror Chat exactly, zero
   special-casing" goal. So Settings is a **normal non-panel window**
   (closable + tabbable + floatable + minimizable), exactly like Chat. The
   invisible-window-blocks-quit worry is mild and self-correcting: the same
   `Ctrl+B Ctrl+,` toggle resurfaces a minimized/buried window (open-or-focus
   un-minimizes it).
7. **Not persisted.** A transient config surface; excluded from `workspace.json`
   so it is not resurrected on relaunch.

## Data model

New variant `Content::Settings(Box<SettingsMenu>)` (`wm.rs:117`). Arms to fill
(grep every `Content::Chat(_)` / `Content::TaskManager(_)` site; the compiler
enumerates them):

- `Content::show` (`~144`) — draw into the given `rect`, gated by `active`
  (see Input). Returns whether it was interacted with (for focus-raise).
- `keepalive` (`~173`) → `{}` — no PTY; pure model, nothing to pump.
- `icon_kind` (`~187`) → a gear icon if `icons.rs` has one, else `None`.
- Workspace snapshot (`~529`) — **add `Content::Settings(_)` to the existing
  `TaskManager` skip.** No new `ContentSnap` variant.
- `panel_model` (`~1764`) — no arm: it only walks `Content::Project`, so a
  desktop-level Settings window is skipped and never appears in the panel.

## Open / lifecycle

Rewrite `open_settings` (`~2451`) from `self.menu = Some(SettingsMenu::new())` to
the **open-or-focus singleton** copied from `open_chat_window` (`~1565`):

```rust
if let Some((win, tab)) = /* find a Content::Settings tab */ {
    self.surface_target(/* raise + focus + un-minimize */);
    return;
}
let (id, rect) = self.next_slot(/* menu's natural size */);
self.push_win(id, Tab::fixed("Settings", Content::Settings(Box::new(SettingsMenu::new()))), rect);
```

The keybind→`open_settings` path (the `OpenSettings` command) is untouched.
Window is **floating, closable, tabbable, not minimizable**.

`deserted()` / `should_show_landing()` (`~2581`, `~2599`): drop the
`self.menu.is_none()` clause. The input-swallow guards that referenced `menu`
(`~2710`, `~3423`, `~4610`, `~4680`) drop their `menu` term too — a Settings
window no longer swallows input globally.

## Input + outcome propagation

`SettingsMenu::show` gains `rect: egui::Rect` and `active: bool`:

```rust
pub fn show(&mut self, ui, rect: egui::Rect, active: bool, s: &mut Settings) -> MenuOutcome
```

- `handle_keys` runs only `if active && self.editing.is_none()`. A focused
  Settings window reads `↑↓←→ / Enter / Esc` exactly as a focused terminal reads
  its keys; a background one reads nothing. No global `swallow_input`.
- **Outcomes propagate via the `drain_chat_clicks` pattern** (content cannot
  mutate the WM mid-loop). The `Content::show` arm:
  - applies `Changed` **immediately in-place** — `config::seed_live(ui.ctx(),
    &live)` needs only ctx, no WM borrow, keeps live-apply visibly instant;
  - stashes `Close` / `OpenKeybindings` / `CheckUpdatesNow` on the `SettingsMenu`
    (e.g. `pending: Option<MenuOutcome>`).
  A new post-loop `drain_settings(&mut self)` — sibling of `drain_chat_clicks`
  (`~1597`) — reads the stash and acts on the WM: close the Settings window,
  set `keymap_editor = Some(SettingsView::new())`, or set
  `check_updates_requested = true`.

## The load-bearing invariant (review target)

The seed → apply → read-back → save cycle is preserved **by construction**:

1. `main.rs:552` — `config::seed_live(&ctx, &self.settings)` before `desktop.show`.
2. The `Content::Settings` arm draws *inside* `desktop.show`: reads
   `config::live(ctx)` → clone → `SettingsMenu::show(&mut live)` → on `Changed`,
   `config::seed_live(ctx, &live)`.
3. `main.rs:647` — reads back `config::live(&ctx)`, adopts if changed, arms the
   ~400ms save debounce.

This is the same channel font-zoom publishes through. The `show_modals` settings
block (`~4776-4797`) is deleted; its clone/re-seed logic moves verbatim into the
arm. **`foreman-reviewer` must verify this cycle end-to-end — do not regress it.**

## View shell-swap

`SettingsMenu::show`:

- **drop** the dim backdrop (`rect_filled(screen, from_black_alpha(170))`, `~472`);
- **drop** the `egui::Window`/`CENTER_CENTER` wrapper (`~475-479`);
- draw rail / pane / footer directly into `rect`, mirroring `ChatView::show`.
  The WM supplies the header + close button (quiet chrome).
- The internal "Settings — {pane}" title band's redundancy with the WM header is
  resolved in the visual pass (likely: band shows the pane name only). Deferred.

**Untouched:** `Pane`, `Field`, `Kind`, `rows`, `adjust`, `display`, `bump`, and
every one of their unit tests. This is the pure model half — no changes.

## Keybindings editor — unchanged

Stays `keymap_editor: Option<SettingsView>`, still drawn in `show_modals`
(`~4803`), still steals all input for chord capture. Only its trigger moves:
from a direct `MenuOutcome::OpenKeybindings` match to the `drain_settings`
handler.

## Testing

All ~700 existing tests stay green (pure model + `bump`/`rank` tests untouched;
`SettingsMenu::show` has no unit test to break). Add:

- **open-or-focus singleton:** call `open_settings` twice → exactly one
  `Content::Settings` window; the second call focuses/raises it.
- **`deserted` counts Settings:** a desktop with only a Settings window (no
  project) → `deserted() == false`. Keep `deserted_ignores_the_panel` valid.
- **not persisted:** a snapshot of a desktop containing a Settings window
  excludes it (mirror the existing TaskManager-exclusion coverage).

## Visual verification

Second-instance build (`--target-dir target/agent`), launch
`target/agent/debug/foreman.exe`, open with `Ctrl+B` then `Ctrl+,`, screenshot,
`Read` the PNG. Confirm: no dim backdrop, real window chrome + close button,
tiles/floats/tabs, live-apply visible while a terminal is beside it. NEVER
`Stop-Process foreman` (this dev session runs inside foreman).

## Docs / memory

- `docs/settings-menu.md`: "modal overlay" → "window"; note keep-alive,
  floating default, Esc-close, non-minimizable, non-persisted.
- Auto-memory `settings-menu-design.md`: mark the window conversion done,
  update the roadmap (phase 2 = absorb keybindings editor as a pane; phase 3 =
  theme system + split-preview Appearance pane).

## Out of scope

Phase 2 (keybindings-editor-as-pane) and phase 3 (theme system, split-preview
Appearance, user themes). The non-blocking follow-ups already triaged in
memory/ledger. The title-band redundancy polish beyond "show pane name only".
