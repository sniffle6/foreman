# Settings persistence (`config.rs`)

The one place to store small app settings on disk. Use it for any new persisted
setting instead of hand-rolling file I/O again.

## What it does

`src/config.rs` gives you three reusable functions and one settings struct:

- `config_dir()` → `%APPDATA%\foreman` (created if missing). `None` only if
  `APPDATA` is unset.
- `load_json::<T>(file)` → reads a JSON file from that dir. Missing file, bad
  file, or invalid JSON all fall back to `T::default()`. Invalid files, including
  wrong-typed fields and invalid UTF-8, are copied byte-for-byte to a unique
  `<file>.corrupt-<timestamp>-<pid>-<sequence>` sibling before defaults can be
  saved. A warning toast names the backup. If reading or backup creation fails,
  saves to that path are refused until a subsequent load succeeds or preserves
  the original. **Never panics** — a corrupt config can't crash the app.
- `save_json(file, &value)` → writes JSON **atomically** (write a `.tmp`, then
  rename over the real file). A crash mid-write leaves the old good file intact.
- `Settings` → the actual app-settings struct, saved to
  `%APPDATA%\foreman\settings.json`. Holds `font_size`, the panel prefs
  (`panel_collapsed`/`panel_width`/`panel_dock`), `bell`, and the whole
  settings-menu phase-1 set (shell, scrollback, zoom/scroll, clipboard, bell
  timing, WM behaviors, agent knobs (including the opt-in naming provider and
  exact model id), startup gates — see
  `docs/settings-menu.md` for the full list). `Settings::sanitize()` clamps
  every numeric field on load so hand-edited files can't violate invariants.
  `seed_live(ctx, &settings)` / `live(ctx)` park a per-frame `Arc<Settings>`
  in egui context data so deep consumers (terminal, wm, chat) read settings
  without parameter threading — same pattern as `terminal::font_size`.
  Agent naming adds `auto_name_agent_sessions` (off), `title_provider` (Codex),
  and `title_model` (`gpt-5.6-luna`); model ids are trimmed and capped at 128
  characters on load.
- `Settings::window` → the OS main window's last placement, so foreman reopens
  where it was closed. File-only (no settings-menu row — the window is the
  editor). See "OS window placement" below.

## Why it exists

Before this, the only persisted thing (`keybindings.json`) hand-rolled the whole
"resolve APPDATA → create dir → serde → fall back on error" dance inline in
`keymap.rs`, and the chat-log plan was about to do it a third time. This is the
shared, **production** version of that pattern (atomic write added), so the next
setting is a one-line field, not a new copy of the plumbing.

## How to use it

Add a persisted setting = add a field to `Settings`:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]              // <- the important part
pub struct Settings {
    pub font_size: f32,
    pub my_new_thing: bool,    // add a field...
}
impl Default for Settings {    // ...and its default
    fn default() -> Self { Self { font_size: 13.0, my_new_thing: false } }
}
```

Read with `Settings::load()`, write with `settings.save()`. That's it.

## Gotchas

- **`#[serde(default)]` on the struct is load-bearing.** It means an old file
  missing your new field, OR a file written by a *newer* foreman with extra
  fields, both still load. Drop it and adding a field breaks every existing file.
- **Don't save on a hot path.** `save_json` touches disk. The font-zoom caller
  debounces (writes once ~400ms after the last change), not once per scroll
  notch. Do the same for anything that changes rapidly.
  Clean quit and update restart flush pending settings and user-theme edits
  immediately, then save the workspace. The built-in theme is never written.
  Explicit quit paths read back this frame's live edits before flushing.
- **This is for settings, not logs.** A growing append-only log (e.g. the chat
  history in `docs/chat-persistence.md`) is a different problem — JSONL, one line
  per event — and intentionally does not go through here.
- **Layout is not settings.** Desktop layout (projects, tiling, tabs, floats)
  lives in `%APPDATA%\foreman\workspace.json` — see
  `docs/workspace-persistence.md`. Do not add layout fields to `Settings`; the
  panel prefs above stay here so wipe-layout and wipe-prefs stay independent.
- `keybindings.json` uses the same protected load and atomic save helpers.
  Its parsed overrides still merge over defaults so new commands retain their
  default bindings when loading an older file.

## OS window placement (`Settings::window`)

`window` is an `Option<WindowGeometry>` (`src/window_placement.rs`):

```json
"window": { "x": 312, "y": 148, "w": 1280.0, "h": 800.0, "ppp": 1.25, "maximized": false }
```

- `x`/`y`: outer top-left in **physical px** (screen coordinates; negative on a
  monitor left of or above the primary).
- `w`/`h`: inner size in **logical points** — what egui lays out in and what
  `with_inner_size` takes.
- `ppp`: pixels-per-point at capture, so `x / ppp` is the point position the
  builder wants.
- `maximized`: the window was maximized; the rect is the **normal** rect
  un-maximize returns to, not the maximized extent.

**Capture** runs every frame in `App::update` from `ctx.input(|i| i.viewport())`
(`window_placement::capture`) and feeds the same `FONT_SAVE_DEBOUNCE` write as
zoom and panel prefs: a drag or resize writes once after it goes quiet, plus
the `flush_all` on quit / update-restart. `changed` ignores sub-point size
jitter so an idle window never dirties the file.

**Restore** happens in `main` through eframe's `window_builder` hook:
`window_placement::placement` keeps the saved rect only if it is sane and its
top band lands on a current monitor, then sets `with_inner_size` +
`with_position` + `with_maximized`. The App then runs a bounded first-frame
correction (`window_placement::Restore`) that re-asserts the exact physical
rect with `ViewportCommand::OuterPosition` / `InnerSize`.

Gotchas, each one a bug that was designed around:

- **Minimized sentinel.** winit's `outer_position` is a raw `GetWindowRect`, so
  a minimized window reports Windows' iconic placement at -32000,-32000, and
  `is_maximized` reads false while iconic. Geometry is never sampled while
  `viewport().minimized == Some(true)`; while maximized only the flag changes
  and the last normal rect is kept.
- **`with_position` is scaled by the wrong DPI on mixed-DPI desktops.** winit
  creates the window at `CW_USEDEFAULT`, then applies the builder's LOGICAL
  position scaled by *that* monitor's DPI. If the saved rect lives on a
  monitor with a different scale, the builder lands near, not on, it. The
  first-frame `Restore` fixes that: `OuterPosition` takes points × the
  window's *current* ppp, which egui-winit hands to winit as physical px. It
  nudges for up to 30 frames and then stops (a size larger than its monitor
  will never match). A **maximized** restore is checked instead of nudged:
  the builder's `with_maximized` maximizes on whatever monitor the scaled
  position landed on, so `Restore` waits for the flag, verifies the maximized
  extent contains the saved normal rect's origin, and if not moves the
  window there (`set_outer_position` clears winit's MAXIMIZED flag, so this
  un-maximizes) and then sends `Maximized(true)` — a one-frame flash, only on
  a mixed-DPI mismatch. Capture stays suspended until the flag is confirmed,
  so a lagging maximize can never overwrite the normal rect with the
  full-screen extent.
- **Monitor rects must be read after winit is DPI-aware.** foreman has no DPI
  manifest; winit's `EventLoop::new` makes the process per-monitor aware.
  Before that, `EnumDisplayMonitors` returns 96-DPI-virtualized rects that
  never match the saved physical position. That is why the decision lives in
  the `window_builder` hook, not at the top of `main`.
- **Off-screen clamp.** Neither winit nor Windows clamps a restored position,
  so a monitor unplugged since the last run would leave the window invisible.
  `placement` requires ≥160×64 px of the window's **top band** (where the drag
  bar is) on some monitor's work area, else the default placement is used and
  the stale value is overwritten by the next capture.
- **`sanitize` drops garbage.** Non-finite, below 200 pt, above 16384 pt, a
  `ppp` outside 0.5–8, or a coordinate beyond ±2^20 px → `None`. A hand-edited
  file cannot produce a 0×0 or 1e9 window.
- **Why not eframe's `persistence` feature.** It pulls in `ron`, writes
  `app.ron` under the *real* RoamingAppData via `SHGetKnownFolderPath`
  (ignoring the `APPDATA` override `scripts/run-dev.ps1` relies on, so a dev
  build would clobber the user's position), autosaves from a 30 s thread
  without `config.rs`'s atomic write + corrupt-file backup, and its Windows
  clamp assumes a 32 px title bar this undecorated window does not have.

## Key files

- `src/config.rs` — the whole thing: `config_dir`, `load_json`, `save_json`,
  `Settings`, and the font-size constants (`DEFAULT/MIN/MAX_FONT_SIZE`,
  `FONT_ZOOM_STEP`).
- `src/main.rs` — `App` owns a `Settings`, loads it at startup, and saves it
  (debounced) when the live font size changes. Also the `window_builder` hook
  that applies the saved OS-window placement and the first-frame `Restore`.
- `src/window_placement.rs` — `WindowGeometry`, the pure `placement` /
  `capture` / `Restore::step` decisions and their table tests, and the Win32
  `monitors()` enumeration.
- `src/keymap.rs` — merges the protected config load over default bindings.
