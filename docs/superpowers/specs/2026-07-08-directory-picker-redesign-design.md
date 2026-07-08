# Spec: directory picker redesign — path field with completion

Date: 2026-07-08. Status: approved (design), not yet built.
Related: `2026-07-08-foreman-landing-design.md` (the landing's hero renders this
picker inline). Same feature branch.

## Problem

The current `DirPicker` (`src/dirpicker.rs`) is a dimmed, centered modal
window driven only by the keyboard, and its text buffer is a **substring
filter**, not a path. You cannot type or edit a path; you can only Up/Down a
list, Right/Tab to drill, Left to climb, Enter to open `cwd`. Editing "where am
I" means walking the tree one directory at a time.

We want an address-bar model: an **editable path field is the source of truth**,
with completion shown two ways at once — **inline ghost text** in the field
(the remainder of the top match, accepted with Tab or →, exactly like PSReadLine
predictive text) and a **clickable dropdown** that slides out beneath the field
(`../` at the top plus matching child directories). Editing the field text —
e.g. shrinking `/code/foreman/` to `/code/`, or typing `for` — re-derives the
list live.

## Design

The path buffer drives everything. Each frame the buffer is split into a
**base** directory and a **partial** last segment; the base's children whose
names prefix-match the partial are the completions; the highlighted completion
supplies both the ghost text and the drop-down selection.

```
 ┌─────────────────────────────────────────┐
 │ /code/for▏eman/                          │  field + ghost (eman/ is gray)
 └─────────────────────────────────────────┘
   ../
 ▸ foreman/                                    dropdown: ../ + prefix matches
   formats/
```

- Type → edits the buffer, re-derives base/partial, resets highlight to the top
  match. Ghost = top match's remainder.
- ↑/↓ → move the highlight; the ghost follows the highlighted row.
- **Tab or → (at end of text)** → accept: rewrite the buffer to the highlighted
  directory + separator (drill in), then re-derive. `→` mid-text is a normal
  cursor move; only `→` at the caret's end accepts the ghost.
- Click a row → same as accept for that row.
- **Enter** → open the buffer path as the project directory, iff it exists.
- Esc → cancel.

### Module and interface

The module stays `src/dirpicker.rs` — *"turn a directory into a chosen project
location"* — but its state and render are rewritten. The `Outcome` contract is
unchanged so callers stay simple:

```rust
pub enum Outcome { Pending, Cancelled, Accepted(PathBuf) }

pub struct DirPicker { path: String, selected: usize, root: PathBuf }

impl DirPicker {
    /// Start with `start` as the buffer (with a trailing separator).
    pub fn new(start: PathBuf) -> Self;

    /// Render the field + completion dropdown into the current `ui` (no scrim,
    /// no window). Placement-agnostic: the landing calls this inline.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome;

    /// Wrap `show` in a light top-center floating Area for the leader-invoked
    /// modal (replaces today's dimmed centered Window).
    pub fn show_modal(&mut self, ui: &mut egui::Ui) -> Outcome;
}
```

Making `show` placement-agnostic (it no longer owns an `egui::Window`) is the
key seam: the same field+dropdown renders inline on the landing and inside a
floating Area from the leader. The dropdown is drawn in its own popup `Area`
anchored under the field so it overlays following content instead of being
clipped by layout.

### Internal seam (pure, unit-tested)

The navigation logic is pure functions of the buffer string and a directory
lister, so tests never need a GUI (mirrors the existing `dirpicker` test suite):

```rust
/// Split a path buffer into (existing-or-implied base dir, partial last segment).
/// A trailing '/' or '\\' means partial is empty and the whole buffer is base.
fn split(path: &str, root: &Path) -> (PathBuf, String);

/// Child dirs of `base` whose names case-insensitively prefix `partial`;
/// sorted, dotfiles excluded (unchanged policy). `lister` is injected.
fn completions(base: &Path, partial: &str, lister: &dyn Fn(&Path) -> Vec<PathBuf>)
    -> Vec<PathBuf>;

/// Remainder of the highlighted match after `partial`, plus a separator
/// (the gray ghost). None when partial is empty or `../` is highlighted.
fn ghost(partial: &str, highlighted: Option<&Path>) -> Option<String>;
```

`show` calls these, paints, and maps input to state transitions
(`set_path`, `move_up/down`, `complete` = accept-highlighted-and-drill,
`accept` = open-if-exists). Relative buffers resolve against `root` (the start
dir); on Windows both `/` and `\\` split and drive matching.

### Rendering specifics (egui 0.34)

- The field is `egui::TextEdit::singleline(&mut self.path).show(ui)` so we get
  real editing — mid-string cursor, selection, paste — which the "edit the path
  text" requirement needs. `TextEditOutput` gives the `galley` and
  `text_draw_pos`; the ghost is painted at the end-of-text position in a weak
  theme color via `ui.painter()`, so it never enters the buffer and cannot
  desync the cursor.
- **Key capture is the known risk.** A focused `TextEdit` wants Tab (focus
  change), arrows (cursor), and Enter (submit) for itself; we intercept Tab,
  ↑/↓, Enter, Esc, and end-of-text →, and act instead. This is exactly the
  event-ordering hazard documented in the `egui-immediate-mode-reference` skill
  (keys not arriving, focus stealing) — follow it during implementation.
- The dropdown "slide-out" is a plain popup for v1; an actual slide animation is
  optional polish, not required.
- The field **requests focus on first frame** (both inline and modal) so the
  user can type immediately; the ghost/dropdown appear as soon as there is a
  partial to complete.

### What does not change

- **`Outcome` and the accept path:** `Accepted(PathBuf)` still flows to
  `WindowManager::add_project` (`src/wm.rs:3667`); the leader `NewProject`
  entry (`src/wm.rs:1779`) still constructs `DirPicker::new`.
- **Directory listing policy:** dirs only, sorted case-insensitively, dotfiles
  excluded — `list_dirs` is reused as the injected `lister`.
- **Modal capture discipline:** while the picker is up, no terminal is active
  (`src/wm.rs:2582`, `3602`); `deserted()` stays false (`src/wm.rs:1902`).
- No new dependency.

## Decision history (settled with the user)

- **Editable path field as source of truth** — **accepted** over the current
  substring filter. Enables direct path editing and re-derive-on-edit.
- **Ghost text in the field + clickable dropdown, both synced to the highlight**
  — **accepted** (user: "auto complete should be on the text field too; Tab or
  → to fill it in"). One highlight drives both surfaces.
- **Tab/→ completes, Enter opens** — **accepted** over Enter-drills-then-opens
  and Enter-opens-highlighted; only this lets you open the folder you are
  *inside* (`/code/foreman/`) without it being a row.
- **Prefix matching** — **accepted** over substring (current) and fuzzy;
  predictable, address-bar/shell-like.
- **Inline on the landing + light floating popup from the leader** — **accepted**
  over always-a-bar and keep-the-centered-modal. Drove the placement-agnostic
  `show` / `show_modal` split.
- **`→` accepts the ghost only at end-of-text** — **accepted;** mid-text `→`
  stays a cursor move so the field is still normally editable.
- **Accepted tradeoff:** ghost/key handling leans on egui event interception,
  the one real implementation risk; isolated to `show`, covered by a skill.

## Testing

Pure core (no GUI), extending the current `dirpicker` tests:

1. `split`: `/a/b/c` → (`/a/b`, `c`); `/a/b/` → (`/a/b`, ``); trailing `\\`
   same; empty buffer → (`root`, ``).
2. `completions` prefix: partial `for` over {`foreman`,`formats`,`platform`} →
   {`foreman`,`formats`} sorted; case-insensitive; dotfiles excluded.
3. `ghost`: highlighted `foreman`, partial `for` → `eman` + sep; partial empty
   → None; `../` highlighted → None.
4. `complete` (Tab/→/click): highlighted `foreman` → buffer becomes
   `…/foreman` + sep, partial resets, rows = foreman's children.
5. `../` drill rewrites the buffer to base's parent + sep.
6. `accept`: existing dir → `Accepted`; nonexistent path → stays `Pending`
   (Enter is a no-op); a file path → not accepted.
7. Re-derive on shrink: buffer `/code/foreman/` edited to `/code/` re-lists
   `/code`'s children with `../` restored.

Evidence loop: build, invoke the picker (leader `NewProject` and the landing
hero), type a partial and confirm the gray ghost, Tab/→ fills it, ↑/↓ moves the
ghost, a click drills, Enter opens an existing dir, Esc cancels — screenshot.

## Key files

- `src/dirpicker.rs` — rewritten: `path`/`selected`/`root` state, pure `split`
  / `completions` / `ghost` seam, `show` (inline) + `show_modal` (floating),
  ghost painting via `TextEditOutput`. `list_dirs` reused.
- `src/wm.rs` — `NewProject` picker construction (`1779`) unchanged; the
  `show_modals` call site (`3661`) switches `picker.show(ui)` →
  `picker.show_modal(ui)`; accept path (`3667`) unchanged.
- `src/theme.rs` — field/ghost/highlight colors (reused).
- `src/landing.rs` — calls `picker.show(ui)` inline for the hero (see the
  landing spec).
