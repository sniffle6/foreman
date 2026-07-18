# Settings menu

One modal, six panes, every phase-1 setting. Opened with the leader chord
`Ctrl+B ,` (the `OpenSettings` command). Everything it edits lives in
`%APPDATA%\foreman\settings.json` and applies live — no OK button, no restart
(two exceptions below).

## What it does

A desktop-level modal overlay (same pattern as the dir picker and the old
keybindings editor): a left rail of categories, a right pane of rows, a footer
with the key hints. Fully keyboard-driven; mouse works everywhere.

Keys: `↑↓` navigate · `Tab` rail⇄pane · `←→` adjust a value · `Enter`
toggle / run action / edit text · `Esc` close (or cancel a text edit).

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
| Keybindings | "Edit keybindings…" — opens the existing editor on top of the menu |
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

## Key files

- `src/settings_menu.rs` — the menu: pure model (panes, row specs, `adjust`,
  `display` — unit-tested) + egui view (modal, rail, rows, text edit).
- `src/config.rs` — the `Settings` struct, defaults, `sanitize()` clamps,
  `seed_live`/`live` (per-frame settings access for deep consumers).
- `src/settings.rs` — the keybindings editor the menu opens for chords.
- Consumers: `src/wm.rs` (shell default, float default, focus-follows-mouse,
  settle), `src/terminal.rs` (scrollback, zoom/scroll, copy-on-select, paste
  gate, dim), `src/theme.rs` (`bell_pulse` period, `DIM_UNFOCUSED`),
  `src/notify.rs` (toast TTL), `src/chat.rs`/`src/chat_view.rs` (crew
  staleness), `src/main.rs` (startup gates, save loop).
