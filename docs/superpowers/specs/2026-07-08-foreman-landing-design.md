# Spec: FOREMAN empty-state landing

Date: 2026-07-08. Status: approved (design), not yet built.

## Problem

Foreman never shows an empty state, so there is nowhere to *land*. At startup
(`src/main.rs:343`) it auto-creates a project rooted at the current working
directory and tiles a terminal into it; closing the last project makes
`WindowManager::deserted` true and the app **quits** (`src/main.rs:398`, like
tmux exiting with its last session). The result: no first-run identity, no
"pick where to start," no home base.

We want a graphically pleasing **empty-state landing**: a terminal-drawn
`FOREMAN` wordmark over a primary "open a project" action and a row of session
launcher icons. This is the first-run and last-close screen — not a persistent
desktop backdrop (deferred) and not a closable subwindow (rejected; a "desktop"
that is itself a window is a muddy metaphor).

Scope of this spec is a **mock**: the visual screen wired to real navigation
via the **redesigned directory picker rendered inline** (its own spec,
`2026-07-08-directory-picker-redesign-design.md`), gated so default behavior is
untouched while we iterate the look. Spawning a *specific* agent from an icon is
a named phase-2 gap, not part of this spec.

## Design

When the desktop has no windows, render a centered landing instead of quitting;
the hero **is** the redesigned picker rendered inline, and Enter (or an icon)
opens the field's path as a new project.

Layout is **A — centered stack**: wordmark → tagline → inline path field → icon
row, vertically centered in the desktop area.

```
              ███████  ██████  ██████  ███████  …   (FOREMAN, mono block art)
                        tmux for AI agents

                 ┌──────────────────────────────┐
                 │ /code/for▏eman/               │   inline picker field + ghost
                 └──────────────────────────────┘
                   ../  foreman/  formats/ …          completion dropdown

                    [Claude]   [Codex]   [Terminal]    icon row (icons.rs art)
```

Enter opens the field path as a plain-terminal project; an icon opens that same
path running that kind. The dropdown/ghost behavior is the picker spec's.

### Module and interface

New module **`src/landing.rs`** — the empty-desktop landing screen. It owns
*how the landing looks and what a click means*, nothing else. Interface:

```rust
/// Open `path` as a new project running `kind`. Fires on Enter in the field
/// (kind = Terminal) or an icon click (that kind), only when `path` exists.
pub struct LandingAction { pub path: PathBuf, pub kind: SessionKind }

pub enum SessionKind { Claude, Codex, Terminal }

/// Holds the inline path field's state across frames.
pub struct Landing { picker: crate::dirpicker::DirPicker }

impl Landing {
    pub fn new(start: PathBuf) -> Self;   // picker starts at `start`
    /// Paint wordmark + inline picker + icon row into `area`; return an action.
    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> Option<LandingAction>;
}
```

The inline path field is stateful, so `Landing` **owns a `DirPicker`** — the
redesigned picker, rendered inline via `picker.show(ui)`. This is a *separate*
instance from the desktop's own `picker` field (used by the leader `NewProject`
command), which the landing never touches; the desktop picker therefore stays
`None` while the landing is up, so `deserted()` remains true and the render
condition needs no special-casing. Icon textures are fetched internally via the
existing `icons::texture(ui.ctx(), kind, px)` (the cache lives in egui context
data). `App` owns one `Landing`.

Icon mapping (the row is Claude / Codex / Terminal):
`SessionKind::Claude → IconKind::Claude`, `Codex → IconKind::Codex`,
`Terminal → IconKind::PowerShell` (the shared shell-prompt glyph — there is no
distinct `Terminal` variant; `wm.rs` already draws plain shells with it).

### Internal seam

The layout is a pure function, private to the module, so the arithmetic is
unit-tested without a GUI (mirrors `layout.rs` / `dirpicker.rs` tests):

```rust
struct LandingLayout {
    wordmark: egui::Rect,   // block-art galley
    tagline:  egui::Rect,
    field:    egui::Rect,   // inline picker (field + dropdown anchor)
    icons:    Vec<egui::Rect>, // one per SessionKind, evenly spaced
}

/// Pure: place every element inside `area`, centered, non-overlapping.
fn layout(area: egui::Rect, n_icons: usize) -> LandingLayout;
```

`show` calls `layout`, paints the wordmark (monospace galley) and tagline,
renders `self.picker.show(ui)` inside `field`, and draws the icon row as
textured labelled buttons. The picker's `Outcome::Accepted(path)` becomes
`LandingAction { path, kind: Terminal }`; an icon click opens the picker's
current path (same existence check) with that kind. The layout arithmetic is
pure and unit-testable.

The wordmark is an embedded constant — real terminal art, not painter strokes:

```rust
/// FOREMAN in a mono block font (figlet "ANSI Regular"), one glyph run.
const FOREMAN_ART: &str = "…";
```

Colors come from `theme.rs` tokens (surface for the ground, the focus/border
ladder for the wordmark, muted text for the tagline) — no ad-hoc RGB, no
gradient-hero cliché.

### Wiring in `main.rs`

Gated behind `FOREMAN_LANDING=1` (read once at startup) so the default build is
byte-for-byte the current behavior:

- **Startup** (`src/main.rs:343`): when the flag is set, *skip* the auto-project
  so first run is deserted and lands. Flag off → auto-project as today.
- **Render** (top of the desktop draw): `if flag && desktop.deserted() {
  self.landing.show(ui, area) } else { desktop.show(...) }`. `deserted()` stays
  true on the landing because the landing's picker is `App`-owned — the
  desktop's own `picker` remains `None` — so no special-casing, and there is no
  separate modal drawn over a blank desktop.
- **Quit guard** (`src/main.rs:398`): when the flag is set, deserted no longer
  quits — the landing owns that state. Flag off → quit-on-deserted as today.
- **Routing:** a returned `LandingAction { path, kind }` calls
  `desktop.add_project(shell, path, ctx)` + `tile_new` (the same pair startup
  uses, `src/main.rs:355`). For the mock every `kind` maps to a plain shell;
  phase-2 maps `Claude`/`Codex` to spawning that agent.

### What does not change

- **Default behavior (flag off):** startup auto-project and quit-on-deserted
  are exactly as today. Zero risk to the shipped app while we iterate.
- **`WindowManager`, layout tree, focus cascade, control plane, chat:**
  untouched. The landing is an `App`-level render, not a `Content` variant, not
  a `Win`, not a tree leaf — it never enters the recursive compositor.
  `add_project` / `tile_new` / `deserted` are reused as-is; the desktop's own
  leader `picker` is independent of the landing's.
- **`icons.rs`:** the `Claude` / `Codex` / `PowerShell` (shell-prompt) textures
  already exist and are reused via `icons::texture`; no new art, no rasterizer
  change, no new `IconKind` variant.
- **No new dependency.**

## Decision history (settled with the user)

- **Empty-state landing** — **accepted.** Shows on first run and last-close.
- **Persistent desktop backdrop** (wordmark/icons behind all windows) —
  **deferred.** A larger change to the root canvas; the empty-state screen is
  the first, self-contained step and can be promoted to a backdrop later.
- **Dedicated `Content::Launcher` subwindow** — **rejected.** A desktop that is
  itself a closable/tabbable window competing for space is a muddy metaphor and
  widens the `Content` enum for no gain.
- **Hybrid body** (primary hero + secondary icon row) — **accepted** over
  project-first-only, subwindow-types (two of three dead at empty state), and
  recent-projects-grid (deferred; slots under the icon row later).
- **Layout A, centered stack** — **accepted** over B (wordmark-top desktop
  grid); A reads as a focused first-run picker, B as a busier desktop.
- **Wordmark as embedded ASCII figlet art** — **accepted** over painter strokes
  (overkill) and large app-font text (not "terminal drawn"). It is literally
  terminal art in the mono font.
- **Mock gated behind `FOREMAN_LANDING=1`** — **accepted** over making it the
  default now. Reversible; lets us screenshot and refine before committing the
  behavior change. **Promotion path:** once the look is approved, drop the flag
  so deserted always lands (and delete the startup auto-project).
- **Inline picker as the hero** — **accepted.** The redesigned picker renders
  inline in the field rect (not a button that opens a separate modal), so the
  landing hosts its own `DirPicker`. This removes any blank-behind-modal seam.
- **Icon-row semantics for the mock:** Enter opens the field path as a plain
  terminal; an icon opens that same path with its `kind`. **Named gap (phase
  2):** map `Claude`/`Codex` to actually spawning that agent in the new project;
  the mock spawns a plain shell for every kind.

## Testing

Pure layout/action tests (no GUI, like the `layout.rs` suite):

1. Every rect from `layout` is inside `area`; wordmark, field, and icon row do
   not overlap.
2. The block is horizontally centered; the stack is vertically centered.
3. `n_icons` rects are evenly spaced and equal width.
4. Small-area degradation: at a minimum desktop size nothing has negative size
   and rects stay within `area`.
5. Action mapping: an icon `i` click → `LandingAction { kind: kind[i] }` with the
   picker's current path; picker `Accepted` → `kind: Terminal`; no interaction →
   `None`. (Picker internals are tested in the picker spec.)

Evidence loop (per the build/verify loop in `CLAUDE.md`): build, run with
`FOREMAN_LANDING=1`, screenshot the window; confirm the wordmark and tagline
render, the inline field is focused and shows a gray ghost as you type, Enter
opens the field's directory as a project, and the three labelled icons render.
Confirm the default build (flag off) still auto-opens a project and quits on
last close.

## Key files

- `src/landing.rs` — **new.** `Landing` (owns a `DirPicker`), `LandingAction` /
  `SessionKind`, the pure `layout` seam, `FOREMAN_ART`, and `show`.
- `src/main.rs` — flag read (startup), `App` gains a `Landing`, `deserted() →
  landing` render branch, startup auto-project skip, quit-guard skip, routing a
  `LandingAction` to `add_project` + `tile_new`.
- `src/dirpicker.rs` — the redesigned picker (its own spec), rendered inline via
  `show`. The desktop's leader picker is a separate instance, unchanged here.
- `src/wm.rs` — `add_project` / `tile_new` / `deserted` reused as-is; **no
  change** for the landing.
- `src/icons.rs` — `IconKind` textures for Claude/Codex/PowerShell, via
  `icons::texture` (reused).
- `src/theme.rs` — color tokens (reused).
