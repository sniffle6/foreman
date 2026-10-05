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
- The top strip is a row of **preset chips** (the built-in first, then your
  themes) plus **Duplicate**, **Reload**, **Delete** and **Folder**. The status
  line under it names the file and says whether it auto-saves; a user theme's
  name is editable inline there, and **Revert** appears while there are unsaved
  edits.
- The left column lists **every colour token**, grouped (Terminal, Windows, Text
  & accents, App bar, Chat, Search, then the ANSI palette and chat member
  colours). Each row shows the label, what it paints, and the **JSON key** in
  dim text beside the swatch; hover for key · hex · description. Click a swatch
  to pick. Edits apply live and auto-save.
- Editing the built-in **Foreman Warm** transparently **forks an editable copy**
  (the built-in stays a pristine preset you can switch back to); the active chip
  flips to the new copy. **Duplicate** makes an explicit copy.
- **Revert** undoes edits back to the baseline (the theme as it was when you
  opened/selected it, or as last reloaded from disk).
- The preview on the right mocks a window — title bar, tab chips, focus border —
  around a sample terminal, so the Windows tokens visibly do something.
- **Theme expert** is the chat under the preview. Describe a palette or ask for
  a refinement; starter chips fill the box when it is empty. Enter sends,
  Shift+Enter inserts a newline. Each answer that produced a theme renders a
  **proposal card** under that reply with a swatch strip and two chips:
  **Preview** toggles the hero to the proposal (the caption says so, with
  Discard), **Apply** saves it through the user theme flow. When Foreman Warm
  is active, Apply creates a user copy first.
- The chat offers Codex, Claude, and Grok via the provider chip. Codex is
  selected initially; a blank model uses that CLI's default (Codex ignores its
  user `config.toml`, so that is Codex's built-in default). Click the model text
  to override it. Changing provider clears the override.

## Hand-editing the file

The file under `%APPDATA%\foreman\themes\` is a first-class way to edit a
theme. The rules, all in `src/main.rs` (`App`) and `src/theme.rs`:

- **Edits apply live.** While a user theme is active the App stats its file
  once a second (`THEME_POLL_EVERY`) and, when the mtime moved, re-reads it
  with the strict `Theme::try_load`. A valid file is adopted and seeded; the
  open Appearance pane resyncs to it (`AppearanceView::needs_resync`) and a
  toast says it reloaded.
- **An invalid file is left alone.** `try_load` never backs up, renames or
  rewrites. A bad value (a 3-digit hex, a missing `#`, truncated JSON from an
  editor mid-write) toasts the serde error once and keeps the current theme;
  fix the file and it applies on the next tick. The destructive
  `.corrupt-*` backup path belongs to the tolerant startup loader only.
- **The app never clobbers a clean theme.** The App is the only writer of
  theme files. On a preset switch it flushes an edit still inside the save
  debounce to the outgoing file, and only then (`flush_outgoing` in
  `src/main.rs`; a renamed or deleted file is never resurrected). The one
  window where the app's write wins is the ~400 ms save debounce after an
  in-app edit; the poll is paused while a save is pending and the mtime is
  re-recorded after it lands. A transient read failure (an editor holding the
  file) is retried silently on the next tick.
- **Alpha is premultiplied.** `#rrggbbaa` stores egui's premultiplied bytes, so
  a hand-written `#ffffff80` is not "50 % white". Prefer the in-app picker for
  translucent tokens (it edits straight alpha). Fixing the format needs a
  version field and a migration; it is deliberately not done yet.
- **Reload** in the pane forces the same strict re-read on demand.

## Theme expert boundary

The provider runs on a background thread in a dedicated configuration directory,
through the shared one-shot launcher (`docs/ai-oneshot.md`): Claude and Grok have
their tools disabled, Codex runs read-only with its shell tools disabled. No provider response is treated as a command or file edit. The
response must be one JSON object containing a short `message` and a complete
`Theme` value with exactly the supported color keys and valid color values. Bad
or incomplete output is shown as an error; the current proposal and saved theme
remain intact. Conversation and previews live only as long as the Settings window.

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
- **Every token is in the pane, from one table.** `theme::TOKENS` is the single
  list of scalar colour tokens (key, label, description, group, alpha flag,
  accessors); the pane is generated from it and a test asserts the table keys
  equal the serialized `Theme` keys. Adding a `Theme` field without a row fails
  that test. The hover-revealed OS bar keeps its own neutral `chrome_*` tokens
  (the App bar group); the in-window chrome is warm by default and deliberately
  separate.
- **A hand edit while previewing a proposal** shows the proposal in the hero and
  the file in every real terminal. The caption says "Proposal preview"; Discard
  the preview to see the file.
- **Colors-first scope:** font family, line spacing, and cursor shape/blink are
  deliberately NOT here (they are separate subsystems — a later phase).

## Key files

- `src/theme.rs` — the `Theme` struct, `foreman_warm()` (built from the legacy
  consts), the `seed_live`/`live` seam, `visuals()` (the egui `Visuals` bridge),
  hex serde (`color_hex`), the `TOKENS` table (`TokenSpec`, `TokenGroup`,
  `PALETTE_NAMES`), the strict `try_load`/`file_mtime`, and
  `load`/`save`/`slug`/`is_builtin`/`user_theme_names`.
- `src/appearance.rs` — the Appearance pane (`AppearanceView`): the pure model
  (working/saved/dirty/revert/presets, `needs_resync`, `persist_outgoing`), the
  house-style view generated from `TOKENS`, the preview, and the theme chat.
- `src/theme_expert.rs` — the bounded conversation, prompt, strict proposal
  parser, and proposal history (each reply links to its proposal). The CLI call
  itself is `src/ai_oneshot.rs`.
- `src/settings_menu.rs` — the custom-body `Pane::Appearance`, and the
  Duplicate / preset-switch / Reload / resync coordination in `draw_pane`.
- `src/main.rs` — `App` owns/seeds/reads-back `active_theme`, installs the egui
  `Visuals` bridge (`ctx.set_visuals`) each frame + pins dark, debounce-saves,
  and runs the disk poll (`THEME_POLL_EVERY`, `theme_disk_action`).
- `src/config.rs` — `parse_json_from`, the strict non-destructive reader the
  poll uses (versus the tolerant, backing-up `load_json_from`).
- `src/config.rs` — `themes_dir()`, the dir-parameterized JSON helpers
  (`load_json_from`/`save_json_in`), and the `Settings.theme` name field.
- `src/terminal.rs` / `src/frame.rs` — `GridColors` parameterizes the color
  pipeline; `MonoPaintKey` includes it for live-apply.
