# Settings menu

One window, six panes, every phase-1 setting. Opened with the leader chord
`Ctrl+B Ctrl+,` (the `OpenSettings` command; bare `,` is Rename). Everything it
edits lives in `%APPDATA%\foreman\settings.json` and applies live — no OK
button, no restart (two exceptions below).

## What it does

A desktop-level **`Content` window** (same family as chat and the task-manager
panel), not a modal: a left rail of categories, a right pane of rows, a footer
with the key hints. It **floats** by default, tiles/tabs like any window, and
doesn't block the terminals behind it — so live-apply is visible while you drag
a value. Fully keyboard-driven when focused; mouse works everywhere.

The layout is **reactive to the window size**: the bands span the window's
width, the footer pins to the bottom, and the pane **scrolls** — vertically
when the window is too short to show every row (keyboard nav auto-scrolls the
selected row into view), and horizontally when it's too narrow to fit a row's
label and control (the rows hold a comfortable minimum width, `PANE_MIN_W`).
A floating settings window also has a **larger resize floor** than other
windows (`SettingsMenu::min_size()`, built on `PANE_MIN_W`) so dragging its edge
can't cramp it; the horizontal scroll is mainly for the **tiled** case, where
the layout tree can force a narrow column. The window opens at its natural size.

`OpenSettings` is **open-or-focus**: it raises the existing settings window (and
un-minimizes it) instead of opening a second one — the same singleton pattern as
the chat window (`open_chat_window`).

Keys (only when the window is focused): `↑↓` navigate · `Tab` rail⇄pane · `←→`
adjust a value · `Enter` toggle / run action / edit text · `Esc` close (or
cancel a text edit).

## Why it exists

Before this, foreman had exactly three user-configurable things (font size,
keybindings, and a file-only `bell` flag). Everything else was a compile-time
const. The menu turns the obvious preferences into settings without exposing
correctness knobs (pipe timeouts, `SUBMIT_DELAY`, repaint cadence stay in code
on purpose — a user slider on those creates support tickets, not value).

## The panes and what they hold

| Pane | Settings (default) |
|---|---|
| Terminal | Default shell (PowerShell); scrollback lines (10 000); scroll speed (3 lines/notch); zoom step (1.0 pt); copy on select (off); warn on multi-line paste (on) |
| Bell & Alerts | Bell master switch (on); pulse speed (1.2 s); toast duration (6 s) |
| Window Manager | New terminals float (off); focus follows mouse (off); dim unfocused panes (off) |
| Keybindings | The keybindings editor, inline — leader, per-command chords, conflicts, reset-one / reset-all |
| Agents | Install skills on launch (on); crew stale after (5 min); send settle default (120 ms) |
| Startup & Updates | Restore workspace (on); default project directory (blank = old behavior); update check on launch (on); Check now; open settings folder; version |

## Gotchas

- **Scrollback lines applies to new terminals only.** Live panes keep the
  history size they spawned with.
- **"Install skills on launch" takes effect next launch** — the install runs
  once at startup, before any frame.
- **Send settle is clamped to 2000 ms** (UI and on load). The pipe server's
  reply timeout is 5 s and `MAX_SETTLE_MS` is 4 s; the clamp keeps a
  hand-edited settings.json from wedging `foreman send`.
- **Hand-edited files are sanitized on load** — out-of-range numbers are
  clamped, unknown fields ignored, corrupt JSON falls back to defaults
  (nothing crashes the app).
- **Default shell doesn't touch the per-project chips.** The `PS`/`CMD`/`SH`
  chips in a project's `+` menu are explicit choices and always spawn what
  they say.
- **Explicit splits and project-open always tile**, even with "new terminals
  float" on — the float default only applies to plain new-terminal commands.
- Saves are debounced (~400 ms after the last change) and atomic; the file is
  written once per burst of changes, not per keystroke.
- **The window keeps the app alive** like a project: if you close your last
  project while settings is open, the app stays up (it's a non-panel window).
- **Not persisted across restarts.** The settings window is excluded from
  `workspace.json`, so it never reopens on relaunch (open it when you want it).
- **Esc / close on a tab-stacked settings window closes only the settings
  tab**, not a co-tabbed project — it routes through the normal per-tab close
  (`request_close_active_tab`), same as clicking the header close button.
- **The keybindings editor IS the Keybindings pane** (no longer a modal). While
  binding a key, chord capture briefly grabs all keyboard input — the leader is
  suppressed (`settings_capturing()`) so any chord, including the leader itself,
  is captured instead of dispatched. `Esc` cancels a capture / backs out to the
  rail (it never closes the window while the editor is focused). Clicking a
  window away *mid-capture* **freezes** the capture — it resumes when the
  settings window is refocused — rather than cancelling it.
- **The keybindings editor only reads input when the settings window is
  focused** (like every other pane). An unfocused Keybindings pane is inert, so
  typing in a terminal beside it can't drive the hidden editor.

## Key files

- `src/settings_menu.rs` — the menu: pure model (panes, row specs, `adjust`,
  `display` — unit-tested) + egui view (`show(ui, rect, active, s)` draws into a
  window rect via a scoped child UI, rail, rows, text edit).
- `src/wm.rs` — the window integration: `Content::Settings` variant,
  `open_settings` (open-or-focus singleton), the `Content::show` arm (reads
  `config::live` + `keymap::live`, edits clones, re-seeds on change),
  `drain_settings` (per-frame: close the tab / check updates), the keymap
  read-back + save in `show`, and `settings_capturing()` gating the leader.
- `src/config.rs` — the `Settings` struct, defaults, `sanitize()` clamps,
  `seed_live`/`live` (per-frame settings access for deep consumers).
- `src/keymap.rs` — the keymap + `seed_live`/`live` (the same ctx seam as
  settings, carrying keymap edits from the inline editor back to the wm to save).
- `src/settings.rs` — the keybindings editor, embedded in the Keybindings pane
  (`SettingsMenu.keybindings`), drawn inline into the pane rect.
- Consumers: `src/wm.rs` (shell default, float default, focus-follows-mouse,
  settle), `src/terminal.rs` (scrollback, zoom/scroll, copy-on-select, paste
  gate, dim), `src/theme.rs` (`bell_pulse` period, `DIM_UNFOCUSED`),
  `src/notify.rs` (toast TTL), `src/chat.rs`/`src/chat_view.rs` (crew
  staleness), `src/main.rs` (startup gates, save loop).
