# Settings menu — design

Date: 2026-07-17
Status: draft, awaiting user review
Mockup: https://claude.ai/code/artifact/1be93b27-134e-4798-b34b-176820362e75
(interactive; Appearance layout "B — split preview" chosen)

## What and why

Foreman today has three user-configurable things: font size (Ctrl+Scroll only,
no UI), keybindings (the `settings.rs` modal editor), and `bell: bool`
(file-only, its comment promises a UI). Everything else a terminal user expects
to customize — colors, font, cursor, shell, scrollback — is a compile-time
const. This design turns the keybindings modal into a full settings menu and
makes the terminal genuinely customizable, without exposing correctness knobs.

## UI shell

A desktop-level **modal overlay** (the existing `settings.rs` / `dirpicker.rs`
pattern), opened by the existing `OpenSettings` command (`Ctrl+B ,`). Not a
`Content` window: settings are global, and one modal pattern already exists.

Layout, top to bottom:

- **Title band** — "Settings — <category>".
- **Search field** — `/` focuses it; filters rows by label across all
  categories (flat result list while filtering). Phase 2.
- **Body** — left category rail (7 categories below), right scrolling pane.
- **Footer** — keyboard hints: `↑↓` navigate, `Tab` rail⇄pane, `Enter`
  edit/capture, `/` search, `Esc` close.

Fully keyboard-drivable like the current keybindings editor; mouse works
everywhere. All styling through `theme.rs` tokens.

## Categories and settings

Legend: **[P]** persisted today · **[N]** new setting · **[F]** future, not in
scope.

### Appearance — split-preview layout (chosen "B")

Two columns: compact control rows on the left; a **sticky live preview**
(mini terminal rendering sample output with the edited theme) plus the ANSI
palette grid on the right. Edits apply live — the preview and the real
terminals repaint immediately; no OK button. While dirty, a "Revert to saved"
action restores the last persisted theme (and closing via Esc keeps changes).

- Theme preset select + Duplicate [N] — built-in "Foreman Warm" + user themes
- ANSI 16 palette swatches [N]
- Background / foreground / selection / focus-border / cursor colors [N]
- Font family [N] — built-in Hack default, or a system-installed family
- Font size [P — `font_size`] — same value Ctrl+Scroll zooms
- Line spacing [N] — multiplier on cell height
- Cursor shape (block/bar/underline) + blink [N]
- Import scheme (Windows Terminal / iTerm2 / base16) [F]
- Direct-edit mode on the preview (click a region to jump to its token) [F]

### Terminal

- Default shell: PowerShell / CMD / SH / Custom + custom command line [N]
  (per-pane chips still override per-Session)
- Scrollback lines [N] — default 10 000 (alacritty default today)
- Scroll speed (lines per wheel notch) [N]
- Zoom step (points per Ctrl+Scroll notch) [N]
- Copy on select [N]
- Warn on multi-line paste [N]

### Bell & Alerts

- Bell attention master switch [P — `bell`, gets its promised UI]
- Pulse speed (`BELL_PERIOD`) [N]
- Toast duration (`notify.rs TTL`) [N]
- Bell sound [F]

### Window Manager

- New terminals open tiled / floating [N]
- Focus follows mouse [N]
- Dim unfocused panes [N]
- Minimum tile size (`MIN_RATIO`) [N]
- Border width [N]

### Keybindings

The existing `settings.rs` editor absorbed as a pane, behavior unchanged
(Enter captures a chord, conflict prompts replace/cancel, merge-over-defaults
persistence in `keybindings.json`). Leader row on top.

### Agents

- Install agent skills on launch [N] — gates `skills_install.rs`
- Crew stale threshold (`STALE_AFTER`) [N]
- Chat history default (`DEFAULT_HISTORY`) [N]
- Send settle default (`DEFAULT_SETTLE_MS`) [N] — clamped so it can never
  reach `MAX_SETTLE_MS` (preserving the `MAX_SETTLE_MS < REPLY_TIMEOUT`
  invariant)

### Startup & Updates

- Restore workspace on launch [N] — gates the `workspace.rs` snapshot restore
- Default project directory (picker start) [N]
- Check for updates on launch [N] — gates `update.rs`
- Version display + "Check now" + "Open settings folder" buttons

### Deliberately not exposed

`SUBMIT_DELAY`, repaint cadence, pipe name/timeouts, `MAX_INFLIGHT` — protocol
and correctness knobs, not preferences. A user slider on any of them creates
support surface for self-inflicted breakage.

## Persistence

Everything lands in the existing `config.rs` layer (`settings.json`,
`#[serde(default)]`, atomic save, corruption-tolerant load):

- New flat fields on `Settings` per the lists above (enums for shell/cursor
  shape/new-window mode serialize as strings).
- Rapidly-changing controls (sliders, steppers) reuse the
  `FONT_SAVE_DEBOUNCE` pattern; discrete toggles save on change.
- Keybindings stay in `keybindings.json` with their merge semantics.
  Opportunistic (per `docs/settings-persistence.md`): move its save onto
  `config::save_json` for atomicity while touching it.
- **Themes**: `theme` field in `settings.json` names the active theme.
  Built-in "Foreman Warm" is in code; user themes are JSON files in
  `%APPDATA%\foreman\themes\<name>.json` (full token set, `serde(default)`
  falling back to built-in values per token).

## Theme system (the big lift)

Exactly what `theme.rs`'s module doc planned: the consts become fields on a
`Theme` struct; the current values become the built-in default. Consumers
already go through `theme.rs`, so the migration is mechanical: each
`theme::TOKEN` use becomes a lookup on the active theme. Active theme lives
where `font_size` does today — seeded into egui context data each frame by the
app, so there is one read path and no locking surprises.

The Appearance pane edits the active `Theme` directly (live apply, matching
the section above); "Revert to saved" reloads the persisted one. Saves are
debounced like font size.

## Behavior notes / gotchas

- **Line spacing and font family changes resize the grid** (cell metrics
  change → PTY cols/rows change). Reuse the font-zoom resize path; the known
  ConPTY reflow limitation applies (`Ctrl+L` heals residuals) — same as zoom
  today, not a new problem.
- **Scrollback lines** applies to new Sessions; live-applying to an existing
  alacritty `Term` needs investigation (planning task, not a blocker — the
  setting says "applies to new terminals" if live-apply is impractical).
- **Font family selection** needs bold/italic faces. Rule: use real faces when
  the family has them; otherwise synthesize (bold = regular face at same
  weight, italic = egui mesh shear) and say so in the UI hint.
- **Send settle default** must clamp below `MAX_SETTLE_MS` on load, not just
  in the UI, so a hand-edited settings.json can't violate the invariant.
- `settings.rs` grows from one editor into a shell + panes; keybindings logic
  moves into a pane module rather than being rewritten.

## Phasing

1. **Shell + plain settings** — modal with rail/panes/footer; every
   toggle/number/enum setting above that needs no new subsystem (bell UI,
   shell, scrollback, clipboard, WM, agents, startup). Each new field wired to
   its consumer.
2. **Keybindings pane** — absorb the existing editor; atomic-save migration.
3. **Theme system + Appearance pane** — `Theme` struct, split-preview layout,
   palette/color/cursor/font editing, user theme files.
4. **Later** — search across settings (if not in 1), scheme import, bell
   sound, direct-edit preview mode.

Phases 1–3 are separately shippable; each gets its own plan.

## Key files

- `src/settings.rs` — becomes the settings shell (rail + panes); keybindings
  editor becomes a pane.
- `src/config.rs` — `Settings` fields, theme name, themes-dir helpers.
- `src/theme.rs` — `Theme` struct + built-in default.
- `src/terminal.rs`, `src/wm.rs`, `src/notify.rs`, `src/chat.rs`,
  `src/skills_install.rs`, `src/workspace.rs`, `src/update.rs`,
  `src/layout.rs` — consume their respective settings instead of consts.
