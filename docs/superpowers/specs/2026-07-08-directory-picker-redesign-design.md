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
(the remainder of the top match, accepted with **Tab**, like PSReadLine
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

- Type → edits the buffer, re-derives base/partial, resets the highlight to the
  first completion. Ghost = highlighted match's remainder.
- ↑/↓ → move the highlight; the ghost follows the highlighted row.
- **Tab** → accept: rewrite the buffer to the highlighted directory + separator
  (drill in), then re-derive. (Right-arrow stays a normal cursor move — see the
  decision history for why `→`-to-accept was dropped.)
- Click a row → same as accept for that row.
- **Enter** → open the buffer path as the project, iff it is an existing
  **directory** (`is_dir`, not `exists`: a file path is rejected). On a
  non-directory, re-request field focus and flag it invalid — never a silent
  dead field (see Rendering).
- Esc → cancel.

**Row / selection model (load-bearing for the tests).** The dropdown rows are
`[Parent] ++ completions` when `base` has a parent, else just `completions`;
`selected` indexes that row list, and `Parent` (`../`) climbs to `base`'s
parent. Re-deriving after an edit seeds `selected` to the first completion row
(index 1 when `Parent` leads and there is ≥1 completion, else 0), so the default
ghost is the top match. With **zero completions** `selected` pins to `Parent`
(index 0) if present, else the row list is empty and every nav key is a no-op.
`selected` clamps whenever completions shrink.

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

    /// Wrap `show` in a top-center floating Area with a subtle scrim (the
    /// modality signal) for the leader-invoked modal, replacing today's dimmed
    /// centered Window.
    pub fn show_modal(&mut self, ui: &mut egui::Ui) -> Outcome;

    /// The buffer resolved to an existing directory (`is_dir`), else `None`.
    /// Lets a caller open the field's current path without an Enter — the
    /// landing icon row needs this while the picker is still `Pending`.
    pub fn current_dir(&self) -> Option<PathBuf>;
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

/// Child dirs of `base` whose names case-insensitively **prefix** `partial`.
/// A pure prefix filter over the injected `lister`'s output — the sort +
/// dotfile-exclusion policy lives in `list_dirs` (the real lister), so a test
/// lister must apply the same policy to match prod.
fn completions(base: &Path, partial: &str, lister: &dyn Fn(&Path) -> Vec<PathBuf>)
    -> Vec<PathBuf>;

/// Remainder of the highlighted dir's REAL name after `partial.len()` bytes,
/// plus a separator (the gray ghost) — case-preserving, since prefix matching
/// is case-insensitive (partial `FOR`, dir `foreman` → ghost `eman`). None when
/// partial is empty or `../` is highlighted. A thin helper, kept separate only
/// for its own test.
fn ghost(partial: &str, highlighted: Option<&Path>) -> Option<String>;
```

`show` calls these, paints, and maps input to state transitions
(`set_path`, `move_up/down`, `complete` = accept-highlighted-and-drill,
`accept` = open-if-`is_dir`). A buffer that is not absolute is joined onto
`root` (the start dir) before `split`; on Windows both `/` and `\\` separate
segments and drive matching.

### Rendering specifics (egui 0.34)

- The field is `egui::TextEdit::singleline(&mut self.path).show(ui)` so we get
  real editing — mid-string cursor, selection, paste — which the "edit the path
  text" requirement needs. `TextEditOutput` exposes the galley, the text draw
  position, and `cursor_range`; the ghost is painted just past the text in a
  weak theme color via `ui.painter()`, so it never enters the buffer and cannot
  desync the cursor. (Confirm the exact field name during impl — it may be
  `galley_pos` rather than `text_draw_pos` in egui 0.34.)
- **Enter never leaves a dead field.** A singleline `TextEdit` fires
  `lost_focus()` on Enter (egui semantics — the chat input re-focuses for
  exactly this reason, `wm.rs:438-440`, as does rename, `wm.rs:2917-2924`). On
  an Enter that does NOT open (path is not a dir) we `request_focus()` again and
  paint a weak invalid cue from the theme border ladder; only a valid dir ends
  the picker.
- **Key capture** stays in the safe, standard tier: intercept Tab, ↑/↓, Enter,
  Esc via `input_mut().consume_key(...)` before showing the field — the codebase
  already runs focused `TextEdit`s this way (chat input, rename). Accept is
  **Tab only**; right-arrow is left to the `TextEdit` as an ordinary cursor
  move, which removes the one genuinely fiddly interception (caret-at-end
  gating) entirely. Follow the `egui-immediate-mode-reference` skill. (The
  current picker avoids a focusable field to keep these keys free,
  `dirpicker.rs:137-139`; the rewrite takes the conflict on, but only for the
  no-conflict keys.)
- The dropdown "slide-out" is a plain popup for v1; a real slide animation is
  optional polish. `show_modal` keeps a **subtle scrim** (lighter than today's
  150-alpha dim) as the modality signal; `show` (inline) draws no scrim.
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
- **Tab completes, Enter opens** — **accepted** over Enter-drills-then-opens
  and Enter-opens-highlighted; only this lets you open the folder you are
  *inside* (`/code/foreman/`) without it being a row.
- **Prefix matching** — **accepted** over substring (current) and fuzzy;
  predictable, address-bar/shell-like.
- **Inline on the landing + light floating popup from the leader** — **accepted**
  over always-a-bar and keep-the-centered-modal. Drove the placement-agnostic
  `show` / `show_modal` split.
- **Tab accepts the ghost; `→` does not** — **settled Tab-only.** The user first
  asked for "Tab or right arrow," a reviewer flagged `→`-at-end-of-text as the
  one genuinely risky bit of key interception (needs a frame-lagged caret-at-end
  read), and on that tradeoff the user chose Tab-only. `→` stays a plain cursor
  move; right-arrow accept is not planned.
- **Enter opens only an existing directory** (`is_dir`) — never a file or a
  missing path; a no-op Enter re-focuses the field with an invalid cue rather
  than dying. The current picker deliberately avoided a focusable field for this
  exact key-conflict reason (`dirpicker.rs:137-139`); the rewrite takes it on.
- **This is a near-total rewrite, not an extension** — none of the 10 current
  tests survive the new API (they key off `cwd`/`query`/`items`/`drill_in`);
  only the tempdir-tree fixture is reused.
- **Accepted tradeoff:** ghost/key handling leans on egui event interception,
  the one real implementation risk; isolated to `show`, covered by a skill.

## Testing

Pure core (no GUI). **A near-total rewrite** — only the current suite's
tempdir-tree fixture (`dirpicker.rs:267`) survives; the 10 existing tests key
off the old `cwd`/`query`/`items` API and are replaced.

1. `split` (highest-risk pure fn; this is a Windows-first app — cover Windows):
   - POSIX: `/a/b/c` → (`/a/b`, `c`); `/a/b/` → (`/a/b`, ``); empty → (`root`, ``).
   - Windows: trailing `\\`; drive root `C:\` → (`C:\`, ``) and `C:\Us` →
     (`C:\`, `Us`); trailing `C:\Users\`; bare drive `C:` (drive-relative — pick
     and pin one rule); UNC `\\server\share`. `Path::parent()` is surprising at
     drive/UNC roots — assert the chosen behavior explicitly.
   - Relative buffer resolves against `root` before splitting.
2. `completions` prefix over {`foreman`,`formats`,`platform`}, partial `for` →
   {`foreman`,`formats`} (not `platform`); case-insensitive; policy (sort,
   dotfiles) inherited from the injected lister.
3. `ghost`: highlighted `foreman`, partial `for` → `eman` + sep; **casing** —
   partial `FOR`, dir `foreman` → `eman` (dir's real name); partial empty → None;
   `../` highlighted → None.
4. `complete` (Tab/click): highlighted `foreman` → buffer becomes `…/foreman`
   + sep, partial resets, rows = foreman's children, `selected` re-seeds to the
   first completion.
5. `../` drill rewrites the buffer to base's parent + sep.
6. `accept`: existing dir → `Accepted`; nonexistent path → stays `Pending`; a
   **file** path → not accepted (`is_dir`, not `exists`).
7. Re-derive on shrink: buffer `/code/foreman/` edited to `/code/` re-lists
   `/code`'s children with `../` restored; `selected` clamps.
8. **Empty-completions safety** (ported from `empty_filter_clamps_and_drill_is_safe`,
   `dirpicker.rs:361`, and now more load-bearing): a partial matching nothing →
   `move_up/down`, `complete`, and `accept` are all panic-free no-ops with
   `selected` pinned in range.
9. `current_dir()`: buffer at an existing dir → `Some`; at a partial/missing
   path → `None`.

Evidence loop: build, invoke the picker (leader `NewProject` and the landing
hero), type a partial and confirm the gray ghost, Tab fills it, ↑/↓ moves the
ghost, a click drills, Enter opens an existing dir (and a bad Enter keeps focus
with an invalid cue), Esc cancels — screenshot.

## Key files

- `src/dirpicker.rs` — rewritten: `path`/`selected`/`root` state, pure `split`
  / `completions` / `ghost` seam, `show` (inline) + `show_modal` (floating),
  ghost painting via `TextEditOutput`. `list_dirs` reused.
- `src/wm.rs` — `NewProject` picker construction (`1779`) unchanged; the
  `show_modals` call site (`3661`) switches `picker.show(ui)` →
  `picker.show_modal(ui)`; accept path (`3667`) unchanged.
- `src/theme.rs` — field/ghost/highlight colors (reused).
- `src/landing.rs` — calls `picker.show(ui)` inline for the hero and
  `picker.current_dir()` for the icon row (see the landing spec).
