# Spec: landing recent-projects list

Date: 2026-07-08. Status: approved (design), not yet built.

## Problem

The landing (`src/landing.rs`, gated behind `FOREMAN_LANDING`) has no memory:
every session starts with typing a path into the picker, even for the same two
or three projects opened every day. The landing spec
(`2026-07-08-foreman-landing-design.md`) explicitly deferred this — "recent-
projects-grid (deferred; slots under the icon row later)".

We want a **recent projects list** under the icon row: up to five entries, each
remembering *how* it was opened (Claude / Codex / Terminal), so one click — or
Tab + arrows + Enter — fully reopens a project with its agent.

## Design

```
              ███████  FOREMAN  ███████
                  tmux for AI agents

             ┌──────────────────────────────┐
             │ /code/for▏                   │    inline picker (unchanged)
             └──────────────────────────────┘

               [Claude]   [Codex]   [Terminal]

             Recent
             > ✦ foreman      H:\claude code\
               ✦ dotfiles     C:\Users\sniff\
               >_ blog        H:\sites\
```

A vertical list, most-recent-first: kind icon (`icons::texture`, same art as
the tabs), directory name in `TEXT`, parent path in `DIM`, a `>` marker plus a
highlight on the selected row. A `Recent` header in `DIM` labels the band. The
section is hidden entirely when there are no entries (first run, or every
entry's directory is currently missing).

**Mouse:** clicking a row opens it. **Keyboard:** when the picker popup is
*closed*, Tab moves focus into the list (the picker only eats Tab while its
popup is open, so this claims a dead key). In the list: ↑/↓ move the
selection, Enter opens the selected entry, Tab or Esc — or ↑ past the top row —
returns focus to the field. Any text input also returns to the field (typing
always means "edit the path"). Opening uses the entry's remembered kind; a
missing agent surfaces the landing's existing "isn't installed" toast.

### Module and interface — `src/recents.rs` (new)

A deep module: one call records, everything else (dedup, cap, ordering,
persistence) is implementation.

```rust
/// One remembered open. `kind` is a plain string ("claude" | "codex" |
/// "terminal") — deliberately NOT the landing's provisional `SessionKind`
/// enum, so the disk format survives phase-2 renaming it, the file can never
/// fail to parse on an unknown kind, and this module doesn't depend on a UI
/// module. Unknown strings degrade to Terminal at the landing edge.
pub struct RecentEntry { pub path: PathBuf, pub kind: String }

pub struct Recents { entries: Vec<RecentEntry> }

impl Recents {
    /// Load `%APPDATA%\foreman\recents.json` (via config.rs's generic
    /// loader): missing/corrupt file → empty list. Called once at startup.
    pub fn load() -> Self;
    /// Record an open: dedup by path, push to front, cap at 5, persist
    /// atomically. Best-effort — a failed save is logged (eprintln) and never
    /// blocks the open. Re-recording an existing path moves it to the front
    /// and adopts the new kind.
    pub fn record(&mut self, path: PathBuf, kind: &str);
    /// Most-recent-first. The caller filters (e.g. missing dirs) — this
    /// module never touches the filesystem beyond its own JSON file.
    pub fn entries(&self) -> &[RecentEntry];
}
```

Constants: `MAX_RECENTS = 5` (store 5, show 5 — one number, per review), file
name `recents.json`. **Dedup key is the case-folded path string**
(`to_string_lossy().to_lowercase()`) because Windows paths are
case-insensitive and `PathBuf` equality is not — `H:\Foo` and `h:\foo` are the
same project. No canonicalization beyond that (no filesystem calls in the
model; `foo\..\bar` duplicates are accepted as a non-problem).

Internal seam: the MRU mutation is pure `Vec` logic, unit-tested without a
disk; persistence rides `config.rs`'s existing `load_json`/`save_json`
(atomic tmp+rename, corruption-tolerant), which already take a file name — no
changes to `config.rs`.

Storage is a **separate file**, not a `Settings` field: settings.json is user
*preferences* written on a zoom debounce; recents is app *state* written on
opens. Keeping them apart avoids interleaved writes and keeps each file's
meaning single.

### Recording seam — the `WindowManager` open drain

Project opens happen in two places: the landing routing (`src/main.rs:407`)
and the leader `NewProject` picker accept, which is **inside `wm.rs`**
(`show_modals`, `src/wm.rs:3699`). Recording at call sites would therefore put
recents knowledge in the engine. Instead, the engine reports and the app
records:

```rust
// wm.rs — pushed by add_project (command = None) and
// add_project_with_command (command = Some(cmd)); drained by the app.
opened: Vec<(PathBuf, Option<String>)>,
pub fn take_opened(&mut self) -> Vec<(PathBuf, Option<String>)>;
```

`App` (owner of the one `Recents`) drains once per frame after the desktop
runs and maps each drained open to a kind string: `None` → `"terminal"`,
`Some(cmd)` → match the command's program stem against the known agent stems
(the logic `SessionKind::stem` already encodes) → `"claude"` / `"codex"`,
anything else → `"terminal"`. One choke point catches every current and future
project-open path; `wm.rs` never learns what a "recent" is.

Exclusions:

- **Flag-off startup auto-project** (`src/main.rs:379`) is implicit, not a
  choice — `App` discards the drain once right after the startup block so it
  is never recorded.
- **CLI `foreman open`** does **not** create projects — `open_dispatch`
  (`src/wm.rs`) resolves an *existing* project and spawns a terminal inside
  it. Terminals-within-projects are not "project opens" and are out of scope;
  they never reach `add_project`, so the drain naturally excludes them.

### Landing changes — `src/landing.rs`

- `Landing` gains a focus zone (`enum Zone { Field, Recents }`, default
  `Field`) and a selected index. `reopen()` resets to `Field`.
- `show` takes the visible entries: `show(&mut self, ui, area, recents:
  &[RecentEntry]) -> Option<LandingAction>`. `App` passes
  `recents.entries()`; the landing filters `!path.is_dir()` rows at render
  (display-only — disk entries are kept, an unplugged drive's project comes
  back when the drive does).
- The pure `layout()` seam grows a `recents: Vec<egui::Rect>` band (header +
  n rows) below the icons, still centered; existing containment/centering
  tests extend to cover it.
- Key handling order per frame: picker popup open → picker owns keys as
  today. Popup closed and visible recents non-empty → Tab toggles the zone;
  in `Recents`, ↑/↓/Enter/Esc are consumed as described above; a text event
  flips back to `Field` and falls through to the field.
- Enter/click on a row maps the entry's kind string back to `SessionKind`
  (unknown → `Terminal`) and returns `LandingAction { path, kind }` through
  the existing routing in `main.rs` — no new launch code, and the existing
  installed-check/toast applies unchanged.
- The zone/selection stepping is a small pure function (state + key →
  state/action) so keyboard behavior is unit-testable without a GUI.

### What does not change

- **Flag-off behavior:** byte-for-byte — the drain is populated but discarded
  after startup and never rendered (the landing doesn't show).
- **Picker (`dirpicker.rs`):** untouched. Tab is only claimed by the landing
  when the popup is closed, which the picker already ignores.
- **`config.rs`:** reused as-is (its generic helpers already take a file
  name).
- **Launch semantics:** `LandingAction` routing, installed-check, and toast in
  `main.rs` are reused unchanged.
- **No new dependency.**

## Decision history (settled with the user)

- **Click reopens with remembered kind** — accepted over open-as-plain-
  terminal and fill-the-field-only. One interaction restores the project.
- **All deliberate opens record** — accepted over landing-only recording.
  Amended by code reality: the set of deliberate project opens is {landing,
  leader picker}; CLI `foreman open` spawns terminals in existing projects
  and is out of scope. Startup auto-project excluded (implicit, would pollute
  the list with the launch cwd).
- **Vertical list under the icon row** — accepted over a card grid (truncates
  paths, fights ↑/↓-only navigation) and recents-inside-the-picker-dropdown
  (invisible until focused, mixes two navigation models).
- **Tab focus zone with `>` marker** — the user's model: Tab switches to the
  history area, a marker/highlight points at the selection, Enter opens.
  Chosen over ArrowDown-into-the-list and number shortcuts.
- **Reviewed by grug-review + codebase-design; amendments adopted:**
  - store 5 / show 5 (was store 10 / show 5) — one number, no ghost-entry
    interactions;
  - `Recents::record` persists internally (was a load/push/save protocol) —
    the ordering constraint was interface complexity;
  - kind persisted as a plain string, mapped at the landing edge (was
    serde on `SessionKind`) — fixes the model→UI dependency direction and
    makes unknown kinds degrade per-entry instead of wiping the file;
  - case-insensitive path dedup (Windows);
  - missing-dir filtering at render, not in the model (model stays pure).
- **Open drain on `WindowManager` (`take_opened`)** — accepted over recording
  at call sites (the leader-picker site is inside `wm.rs`; direct recording
  would point the engine at app state) and over passing `&mut Recents` into
  `show` (threads app state through the recursive compositor).
- **Separate `recents.json`** — accepted over a `Settings` field (state vs
  preferences; debounced-write interleaving) and over an append-only open log
  (event-sourcing a five-row list).

## Testing

Pure, no GUI (matching the `layout.rs` / `dirpicker.rs` / `chat.rs` pattern):

1. `recents.rs`: record dedups case-insensitively, moves re-opens to the
   front, adopts the new kind, caps at 5; entries are most-recent-first;
   round-trips through serde; an entry with an unrecognized kind string
   survives load (kind is a `String` — nothing to fail).
2. `landing.rs` layout: the recents band is inside `area`, below the icons,
   non-overlapping, horizontally centered; rows are equal height; zero
   entries → no band and the stack matches today's layout.
3. `landing.rs` stepping: Tab (popup closed) enters the list at row 0; ↑/↓
   clamp/step; ↑ at row 0, Esc, and Tab return to `Field`; Enter yields the
   selected entry; text input returns to `Field`.
4. Kind mapping: `"claude"`/`"codex"`/`"terminal"`/`"anything-else"` →
   `SessionKind::{Claude, Codex, Terminal, Terminal}`.

Evidence loop (per `CLAUDE.md`): build; run with `FOREMAN_LANDING=1`; open a
project via the picker, close it back to the landing; screenshot — the list
shows the entry with the right icon; Tab + Enter reopens it; open via a
[Claude] icon, land again, confirm the entry shows the Claude icon and
reopens running Claude. Confirm the default build (flag off) writes nothing
new to `%APPDATA%\foreman` behavior-wise (recents.json may exist but the
startup auto-project never appears in it).

## Key files

- `src/recents.rs` — **new.** `Recents` / `RecentEntry`, MRU model, load/
  record, `recents.json` persistence via `config.rs` helpers.
- `src/landing.rs` — focus zone + selection, recents band in `layout()`,
  row rendering, key handling, kind-string → `SessionKind` mapping.
- `src/wm.rs` — `opened` drain pushed by `add_project` /
  `add_project_with_command`; `take_opened()`. Nothing else.
- `src/main.rs` — `App` owns `Recents`; drains + records each frame; discards
  the startup auto-project drain; passes entries into `landing.show`.
- `src/config.rs` — reused, unchanged.
- `src/icons.rs`, `src/theme.rs` — reused, unchanged.
