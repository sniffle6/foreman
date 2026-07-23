# Theme system + Appearance pane + user themes (settings phase 3) — design

Date: 2026-07-22
Status: draft, awaiting user review
Branch: `feat/settings-appearance-theme` (to be created off `main` @ `0939487`)
Follows: `2026-07-21-keybindings-pane.md` (phase 2). This is settings **phase 3**
from `2026-07-17-settings-menu-design.md` (Appearance layout "B — split preview"
was chosen there).

## What and why

`theme.rs` is intentionally static consts, glob-imported (`use crate::theme::*`)
by every view module. Its own module doc says: *"When a second real theme lands,
these consts become fields on a struct."* Phase 3 is that moment — user themes
force a runtime theme system. This phase converts the static consts into a
runtime `Theme` struct behind a ctx seam (the third instance of the
`config::seed_live`/`keymap::seed_live` pattern), adds the split-preview
**Appearance** pane that edits the live theme, and persists **user themes** to
`%APPDATA%\foreman\themes\`.

Scope is **colors-first** (user decision): the theme struct, the seam migration,
color + ANSI-palette editing, preset/duplicate, user themes. Font *family*, line
spacing, and cursor shape/blink are a separate subsystem (font-face loading +
caret rendering) and are **deferred to phase 3b** — bundling them would multiply
the risk of the load-bearing static→runtime migration.

## Locked decisions

1. **All ~40 color tokens become `Theme` fields** (34 scalar colors +
   `chat_colors: [Color32; 6]` + `palette: [Color32; 16]`). One uniform
   `theme::live(ctx).x` access path everywhere; a user-theme JSON fully describes
   the look. `BELL_PERIOD` stays a const (it is already a `bell_period`
   *setting*, not a color); `unmultiplied` stays a const helper used to build the
   default; `bell_pulse` takes the bell color as a parameter; `APP_BORDER`
   becomes an `app_border()` method returning `self.chrome_bg` (preserves the
   "frame matches the revealed OS bar" invariant).
2. **The existing consts become the definition of the built-in default.**
   `Theme::foreman_warm()` is constructed *from* the current const values, so the
   default render is byte-identical **by construction** — no duplicated literals,
   no eyeballed re-entry of colors.
3. **Theme is WM-owned**, mirroring `keymap`: `WindowManager.theme: Theme`,
   seeded each frame via `theme::seed_live(ctx, &self.theme)`, edited as a clone
   in the `Content::Settings` arm, read back + persisted after the render loop.
4. **Colors-first scope** — see above.
5. **Live preview is a self-contained sample renderer, no PTY** (judgment call).
6. **Built-in "Foreman Warm" is read-only; editing requires Duplicate**
   (judgment call) — the color controls are disabled while the built-in is
   active; Duplicate forks it into an editable user theme.
7. **Font size stepper appears in Appearance** (judgment call) — the existing
   `font_size` setting, zero new subsystem; matches the mockup.
8. **User-theme colors serialize as hex strings** (`"#rrggbb"` / `"#rrggbbaa"`),
   human-editable and import-friendly.

## The seam (the crux)

`theme.rs` gains:

```rust
pub struct Theme { /* bg, desk_bg, ... , chat_colors: [Color32;6], palette: [Color32;16] */ }
impl Theme { pub fn foreman_warm() -> Self { /* built from the current consts */ } }
impl Default for Theme { fn default() -> Self { Self::foreman_warm() } }
pub fn seed_live(ctx: &egui::Context, t: &Theme);         // stores Arc<Theme> in ctx data
pub fn live(ctx: &egui::Context) -> std::sync::Arc<Theme>; // reads it back
```

byte-for-byte the shape of `keymap::seed_live`/`live`. Consumers replace
glob-const reads with **one fetch per render fn** — `let th = theme::live(ctx);`
then `th.bg`, `th.text`, … — so it is one `Arc` clone per fn per frame, not one
per token.

Migration insight that makes it safe: after every consumer is migrated, the old
`pub const BG` etc. are referenced **only** by `foreman_warm()`. They stay as the
default's single source of truth; nothing else reads them.

## Staging (each stage keeps the suite green; A and B render identically)

- **A — `Theme` struct + seam, zero behavior change.** Add the struct,
  `foreman_warm()` (from consts), `Default`, `seed_live`/`live`, the WM-owned
  `theme` field, and the per-frame seed. No consumer migrated; no persistence.
  The app renders identically (consts still in use). Test: seam round-trip;
  `foreman_warm()` fields equal the old const values.
- **B — migrate the 7 consumers** (`wm.rs`, `terminal.rs`, `settings_menu.rs`,
  `settings.rs`, `panel.rs`, `chat_view.rs`, `confirm.rs`) from the glob consts
  to `theme::live(ctx)`. `bell_pulse` call sites pass `th.bell`. After this the
  consts are referenced only by `foreman_warm()`. **Prove byte-identical** with
  before/after screenshots of the desktop, a project with terminals, chat, and
  the settings window.
- **C — Appearance pane (colors).** New `Pane::Appearance` (custom-body, mirrors
  the Keybindings pane: `rows()` returns `&[]`, `draw_pane` special-cases it,
  input gated on `active && !in_rail`, `Pane::ALL` grows to 7 so `min_size()`
  adapts). Split-preview layout "B":
  - **Left column** — compact control rows: preset `select` + **Duplicate**;
    core color pickers via `color_edit_button_srgba` (background, foreground,
    selection, focus-border, cursor); ANSI-16 swatch grid (each a picker); the
    existing font-size stepper; a **Revert to saved** action shown while dirty.
  - **Right column** — a **sticky live preview**: the self-contained sample
    renderer (prompt line, a command, colorized output, a selected span, the
    caret) painted with the *working* theme, plus the ANSI palette grid.
  - Edits write the working theme → re-seed → **the preview and every real
    terminal repaint live** (they read the seam). No OK button.
- **D — user themes + persistence.** `theme` field on `Settings` names the active
  theme. User themes live in `%APPDATA%\foreman\themes\<name>.json`, full token
  set as hex strings, `#[serde(default)]` per token falling back to the built-in
  value. Load the active theme on startup (`Theme::load()` → read the name from
  settings, load the JSON, sanitize). Save is **debounced** (the
  `FONT_SAVE_DEBOUNCE` pattern). Duplicate writes a new file and switches to it.
  Built-in is read-only (never written). `sanitize()` replaces an unparseable
  hex token with its built-in default so a hand-edited file can't brick the UI.

## Live-apply / dirty / revert

The Appearance pane holds a `working: Theme` and knows the `saved: Theme` (the
last persisted state of the active theme). Dirty = `working != saved`. Each frame
the pane writes `working` back through the seam so real terminals track edits.
**Revert to saved** sets `working = saved.clone()`. On edit (when the active theme
is a user theme) the debounced save persists `working` and advances `saved`.
Selecting a different preset loads it as the new `saved`/`working`.

## Persistence details

- New `Settings.theme: String` (default `"Foreman Warm"`), in the existing
  `settings.json` layer (`load_json`/`save_json`, atomic, corruption-tolerant).
- `config::themes_dir()` helper (sibling of `config_dir()`), created on first
  save.
- Color (de)serialization: a `#rrggbb`/`#rrggbbaa` hex codec with round-trip and
  clamp-on-parse tests. `Theme` derives serde with `#[serde(default)]` on each
  field so a partial or forward/backward-skewed file still loads.

## Testing

- Seam round-trip (`seed_live` then `live` returns an equal `Theme`).
- `foreman_warm()` fields equal the old const values (guards byte-identity — this
  is the regression net for stage B).
- Hex codec: parse/serialize round-trip; invalid hex → default token; short/long
  strings rejected cleanly.
- User theme load/save round-trip; missing token falls back via `serde(default)`;
  `sanitize()` repairs an invalid token.
- Appearance pane: input is inert unless `active && !in_rail` (the phase-2
  CRITICAL was an unfocused pane reading input); Revert restores `saved`;
  Duplicate produces a new editable theme and the built-in stays read-only.
- All existing ~708 tests stay green.
- **`foreman-reviewer` pass** on the load-bearing diff: the seam + the
  input-reading Appearance pane.

## Visual verification

Second-instance build (`target/agent/debug/foreman.exe`; **never**
`Stop-Process foreman` — this session runs inside foreman; build with
`--target-dir target/agent`). Open settings with `Ctrl+B` then `Ctrl+,`.

- **Stage B:** before/after screenshots of desktop + a project with terminals,
  chat open, and the settings window must be pixel-identical.
- **Stage C/D:** the Appearance pane renders split-preview; editing background
  repaints the preview *and* the real terminals live; Duplicate off the built-in
  yields an editable theme; Revert restores; the built-in's controls are
  disabled; a restart reloads the saved active theme.

## Docs

- `docs/settings-menu.md`: add the Appearance pane (split-preview, live-apply,
  Duplicate/Revert).
- `docs/theme.rs`-adjacent (module doc): note the seam and that consts now define
  the built-in default; drop the "static by design" caveat.
- A short feature doc `docs/theme-system.md` (grug-simple): what it does, the
  seam, user-theme file format, gotchas.
- Update the phase-1 spec status and the auto-memory when phase 3 lands.

## Out of scope (phase 3b / phase 4)

Font family (bold/italic face loading + synthesis), line spacing, cursor
shape/blink — a separate subsystem (phase 3b). Scheme import (Windows
Terminal / iTerm2 / base16), direct-edit preview mode, bell sound — phase 4. No
change to the settings-window shell, the keymap, or the chat/terminal data
models.
