# Theme system

## What it does

Foreman's colors live in a runtime `Theme` struct — every surface, text, border,
selection, caret, chat color, and the 16-color ANSI palette. One theme is active
at a time. The **Appearance** settings pane edits it live: change a color and
every terminal (and all app chrome) repaints instantly. Themes are named; you
duplicate the built-in into your own editable theme, saved to disk.

## Why it exists

`theme.rs` used to be static `const` colors ("no runtime theme system until a
second theme exists"). User themes are that second theme, so the consts became a
`Theme` struct published each frame through a ctx seam — exactly like settings
and the keymap. The consts still exist, but only to *define the built-in default*
(`Theme::foreman_warm()`), so the default renders byte-identically to the old
static palette.

## How it works (the seam)

- `theme::seed_live(ctx, &Theme)` publishes the active theme into egui ctx data
  each frame; `theme::live(ctx) -> Arc<Theme>` reads it back. Byte-for-byte the
  `config::seed_live` / `keymap::seed_live` pattern.
- `App` (main.rs) owns `active_theme`, resolved from `settings.theme` (the name)
  at startup. It seeds it each frame, reads back any edit the Appearance pane
  published (live-apply), and debounce-saves to the user theme file.
- Every view reads `theme::live(ctx).<field>`. The terminal color-resolution
  pipeline (`resolve`/`glyph_style`/`indexed_rgb`/`query_color`) runs partly off
  the egui thread, so it can't read a ctx — it takes a plain `GridColors { fg,
  bg, palette }` value instead. Render paths build it from the live theme; the
  grid galley cache (`MonoPaintKey`) **includes** it, so a palette edit busts the
  cache and repaints already-drawn terminal text.

## egui widget colors (the Visuals bridge)

Almost everything in Foreman is **hand-painted** straight from `theme::live` —
the terminal grid, window chrome, chat, panel, landing — so those already match
whatever theme is active. But a few surfaces use **egui-native controls** that
read their colors from egui's own `Visuals`, not the theme: the Appearance/
settings widgets (`ComboBox`, `TextEdit`, `Button`, the `color_edit` swatches +
their popups, the form scrollbar) and the close-confirm modal. Without a bridge
these fall back to egui's stock cool-grey dark theme and clash with the warm app.

`Theme::visuals(&self) -> egui::Visuals` is that bridge: it starts from
`Visuals::dark()` and remaps the load-bearing slots — window/panel fills, the
`TextEdit` well (`extreme_bg_color`), the five `widgets` states (idle/hover/
active/open), selection, focus ring, caret, and semantic accents — onto the
theme's tokens. `App` installs it **once per frame** via `ctx.set_visuals(...)`
right after `theme::seed_live` (main.rs), sourced from `active_theme`. It's the
same cost class as the other per-frame seeds and does not request a repaint.
A startup `ctx.set_theme(ThemePreference::Dark)` pins dark so a system light-mode
preference can't swap in unstyled light visuals. Editing a color recolors these
controls too, one frame behind — the same lag every terminal repaint already has.

## How to use it

- Open settings (`Ctrl+B` then `Ctrl+,`), select **Appearance** (top of the rail).
- Just edit the colors — background / foreground / selection / focus-border /
  cursor and the 16 ANSI swatches. Edits apply live and auto-save.
- Editing the built-in **Foreman Warm** transparently **forks an editable copy**
  (the built-in stays a pristine preset you can switch back to via the dropdown);
  the preset name flips to the new copy. **Duplicate** makes an explicit copy.
- **Revert to saved** undoes edits back to the baseline (the theme as it was when
  you opened/selected it). The preset dropdown switches themes.

## User theme files

- Live in `%APPDATA%\foreman\themes\<name>.json`. A user theme's name *is* its
  file stem (a slug: lowercased, non-alphanumerics → `-`). The built-in
  "Foreman Warm" is code-only — it never has a file.
- Each color is a hex string: `#rrggbb` (opaque) or `#rrggbbaa` (translucent —
  the stored *premultiplied* bytes, so every token round-trips exactly, including
  odd ones like the snap overlay).
- `#[serde(default)]` per field: a file missing a token gets the built-in value
  (forward-compatible when tokens are added later). A corrupt file (bad hex,
  truncated JSON) tolerantly falls back to the built-in — it never bricks the UI.
- The active theme's name is stored in `settings.json`'s `theme` field.

## Gotchas

- **Two paths still report the DEFAULT palette, not the live theme** (a phase-3b
  follow-up): the OSC color-query answers (`query_color`, on the PTY reader
  thread — no egui ctx exists there) and the headless `foreman snapshot --attrs`
  inspector. The *visible* terminal grid DOES reflect the live theme; only these
  self-report paths lag.
- **The built-in is never written to disk.** `Theme::save` refuses the built-in
  name; editing the built-in forks a copy (and that copy is what's saved), so the
  shipped colors are always recoverable by selecting "Foreman Warm" again.
- **Font size** in the Appearance pane rides the `Ctrl+Scroll` zoom seam (a
  `Settings` field, not a theme token) — it persists in `settings.json`, not the
  theme file. Changing it resizes the grid (cols/rows change), with the same
  ConPTY reflow caveat as zoom (`Ctrl+L` heals residuals).
- **Colors-first scope:** font family, line spacing, and cursor shape/blink are
  deliberately NOT here (they are separate subsystems — a later phase).

## Key files

- `src/theme.rs` — the `Theme` struct, `foreman_warm()` (built from the legacy
  consts), the `seed_live`/`live` seam, `visuals()` (the egui `Visuals` bridge),
  hex serde (`color_hex`), and `load`/`save`/`slug`/`is_builtin`/`user_theme_names`.
- `src/appearance.rs` — the Appearance pane (`AppearanceView`): the pure model
  (working/saved/dirty/revert/presets), the split-preview view + live sample, and
  the color pickers.
- `src/settings_menu.rs` — the custom-body `Pane::Appearance`, and the
  Duplicate / preset-switch / resync coordination in `draw_pane`.
- `src/main.rs` — `App` owns/seeds/reads-back `active_theme`, installs the egui
  `Visuals` bridge (`ctx.set_visuals`) each frame + pins dark, and debounce-saves.
- `src/config.rs` — `themes_dir()`, the dir-parameterized JSON helpers
  (`load_json_from`/`save_json_in`), and the `Settings.theme` name field.
- `src/terminal.rs` / `src/frame.rs` — `GridColors` parameterizes the color
  pipeline; `MonoPaintKey` includes it for live-apply.
