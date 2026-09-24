# Git History diff window — design

Status: spec, awaiting review (card `l8th0t`). Not yet planned or built.

Clicking a file in the Git History commit details pane opens that file's change
in a side-by-side Diff window: a slim take on the JetBrains "Repository Diff"
window. Today (`src/git_history/details.rs`, commit 67cdbc8) that click does
nothing.

## Intent

- **Outcome:** read a whole file's change at a commit, old and new side by
  side, and hop between the differences, without leaving foreman.
- **Shape:** one reusable Diff window per Project. It is an ordinary `Content`
  variant, so it tiles, tabs, zooms and restores like every other viewer.
  Clicking another file retargets it; it never multiplies.
- **Constraints:** fast and native. The GUI thread never waits on git. No new
  dependency. Read-only: no edit, stage, blame, or repository writes.
- **Done means:** whole file (not just hunks), old and new line numbers, change
  bands, prev/next difference with an "N differences" count, change markers
  along the right edge, connector gutter, restore across restart, and bounded
  work for huge files.

## Decisions (from brainstorm)

| Decision | Chosen | Rejected and why |
|---|---|---|
| Syntax highlighting | **None.** Plain monospace on change bands | `syntect`-class crates cost several MB of binary and build time; a hand-rolled tinter is wrong on multi-line strings/comments. The bands carry the meaning that matters |
| Word-level highlight | **Prefix/suffix trim** of each paired line; the differing middle gets a stronger tint | Token LCS is a second diff algorithm to own. Known limit: two separate edits on one line tint everything between them |
| Row geometry | **Padded rows**: a block of r removed and a added lines takes `max(r, a)` rows on both sides, blanks padding the shorter side | The unpadded JetBrains model (curved connectors) needs two virtualized panes plus a scroll-sync map through blocks; that is most of the complexity for a cosmetic gain |
| Connectors | **Straight band gutter** between the panes, per block | Curves only mean something with unpadded geometry (see above) |
| Viewer / whitespace / highlight dropdowns | **Cut** | Each opens another mode with its own read or render path |
| Where the alignment comes from | **Git computes the diff**; we parse one whole-file hunk | `similar` crate: new dependency and can disagree with what `git diff` shows. Hand-rolled Myers: an algorithm to own for no user-visible gain |
| Long lines | **One shared horizontal offset** for both sides; line numbers and gutter stay pinned | Independent offsets break the visual pairing; clipping hides content; wrapping breaks fixed row height |
| Restart | **Restore with target**: window and its file come back, re-read from git | An empty restored window or no persistence both lose the user's tiled History+Diff layout content |
| Merges | Compare against **first parent**, same as the details pane | — |

## Findings that shaped the design

Probed against git 2.39.1 (the version on the dev machine) before writing this:

1. **`-U<INT_MAX>` corrupts the output.** `git diff-tree -p -U2147483647` on
   67cdbc8's `src/git_history.rs` (864 → 945 lines) emitted **nine overlapping
   hunks** all starting at line 1, 8068 lines total. `-U99999`, `-U1000000`
   and `-U100000000` each gave one correct hunk (953 lines). So the context
   value must be a bounded constant, and the parser must reject anything that
   is not one whole-file hunk rather than render it.
2. **Renames and copies need the blob form.** `git diff <parent>:<old>
   <commit>:<new>` diffs a renamed or copied pair exactly, without relying on
   rename/copy detection under a pathspec (copy detection would additionally
   need `--find-copies-harder`). It cannot express a missing side, so adds and
   deletes keep a pathspec-limited `diff-tree`.
3. **The click-to-sibling-window seam already exists.** `PlanView` records
   `acts` during draw; `WindowManager::drain_plan_acts` applies them after the
   window loop. The details pane reaches the Diff window the same way.
4. **The two existing git runners differ in a way the helper must keep.**
   `stream_history` streams stdout on demand with no timeout (it idles waiting
   for the viewport). `details::git_output` is a one-shot capped read with a
   30 s timeout. One shared function with a mode flag would blur that; a
   spawn primitive plus a one-shot wrapper keeps both honest.

## Units

### 1. `src/git_history/git.rs` — shared subprocess helper (first task)

Extracted from `stream_history` (`src/git_history.rs`) and `git_output`
(`src/git_history/details.rs`). Removes their duplicated command setup, drain,
and kill/reap code.

- `spawn(cwd, args, cancel) -> Result<Running, GitError>`: builds the command
  (`--no-pager`, `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`, null stdin,
  `CREATE_NO_WINDOW` on Windows), drains stderr on its own thread with a 16 KiB
  cap, and starts a watchdog thread that kills and reaps the child when
  `cancel` is set. `Running` exposes the stdout pipe and a `finish()` returning
  exit status plus stderr text.
- `output(cwd, args, cancel, cap, timeout) -> Result<Vec<u8>, GitError>`:
  built on `spawn`; drains stdout concurrently, keeps at most `cap` bytes,
  kills on `cancel` or `timeout`.
- `GitError`: `Spawn(String)`, `Cancelled`, `TimedOut`, `TooLarge`,
  `Failed(stderr)`. Typed so the diff can show "too large" as a notice rather
  than an error. `details.rs` maps each variant to its current message, so its
  visible behavior does not change.

`stream_history` uses `spawn` directly; `details::load` and the diff use
`output`.

### 2. `src/git_history/diff.rs` — pure parse and row model

No egui, no I/O. Everything the painter needs is computed here, once, on the
worker.

```rust
pub enum Diff { Doc(Doc), Notice(Notice) }
pub enum Notice { Binary, TooLarge, Unchanged, Submodule }
pub struct Doc {
    rows: Vec<Row>,          // padded, side-by-side
    blocks: Vec<Range<usize>>, // row range of each difference
    max_cols: usize,         // longest display line, for horizontal extent
    max_line_no: u32,        // for line-number column width
}
struct Row { kind: Kind, old: Option<Cell>, new: Option<Cell> }
struct Cell { line_no: u32, text: String, hot: Option<Range<usize>>, no_eol: bool }
enum Kind { Same, Removed, Added, Modified }
```

(Field names are indicative; the plan settles exact types, for example
whether `text` is a range into one shared buffer.)

**Parsing rules:**

- Skip header lines up to the first `@@`. A `Binary files … differ` line gives
  `Notice::Binary`; a `Subproject commit` line gives `Notice::Submodule`; no
  `@@` at all gives `Notice::Unchanged` (pure rename, mode-only change, empty
  add/delete).
- **Strict single hunk.** Exactly one `@@`; old and new starts are 0 or 1; the
  counted `-`/` ` and `+`/` ` lines equal the header counts. A second `@@` or a
  count mismatch gives `Notice::TooLarge`. This covers files over the line cap
  and any future git quirk like finding 1, so a wrong diff is never painted.
- Lines decode as lossy UTF-8, strip one trailing `\r`, and expand tabs to
  4-column stops.
- `\ No newline at end of file` sets `no_eol` on the preceding cell, painted as
  a dim marker, so a commit whose only change is the final newline still
  shows something.

**Pairing:** a run of `-` lines immediately followed by a run of `+` lines is
one block of `max(r, a)` rows. Row *i* pairs removed *i* with added *i* as
`Modified`; the remainder are `Removed` or `Added` with the opposite side
`None`. A lone `-` run or `+` run is its own block. Context lines are `Same`.

**Word trim:** for each `Modified` row, the common char-boundary prefix and
suffix of the two texts are excluded; the remaining middle on each side is
`hot`. If one side's middle is empty (pure insertion or deletion within the
line), only the other side gets a `hot` span.

"N differences" is `blocks.len()`.

### 3. `src/git_history/diff_view.rs` — `DiffView`, the window content

**Target:**

```rust
pub struct DiffTarget {
    commit: String,          // 40/64-hex, validated before any git call
    parent: Option<String>,  // first parent; None for a root commit
    status: char,            // A M D R C T
    old_path: Option<String>,// R/C source
    path: String,
}
```

**Git reads, by status** (all with `--no-ext-diff --no-textconv --no-color
-U200000`, `output` cap 16 MiB, timeout 30 s):

| Status | Command |
|---|---|
| `A`, `D` | `git diff-tree -p <parent> <commit> -- <path>`; root commit: `git diff-tree -p --root <commit> -- <path>` |
| `M`, `T` | `git diff <parent>:<path> <commit>:<path>` |
| `R`, `C` | `git diff <parent>:<old_path> <commit>:<path>` |

200,000 lines is the line cap: anything longer produces more than one hunk and
lands on `Notice::TooLarge` by the strict-hunk rule. `GitError::TooLarge` maps
to the same notice. Paths that were lossily decoded from non-UTF-8 bytes will
fail the object lookup and show git's error; acceptable for v1.

**Request lifecycle:** same pattern as `DetailsView`: one `Request { target,
cancel, receiver }`, a worker thread running `git::output` then
`diff::parse`, `try_recv` polling on the GUI thread, and `Drop` setting
`cancel`. `retarget` to the shown target is a no-op; otherwise it cancels and
restarts. A superseded or closed view's `Doc` is dropped on a background
thread, as History already does with its pages.

**Layout:**

- **Header row:** file path (`old → new` for renames/copies); `abc1234 vs
  def5678`, or "vs first parent" on merges, "new file" / "deleted file" when a
  side is absent; **"N differences"**; ▲/▼ prev/next buttons.
- **Body, left to right:** old line numbers | old text | connector gutter
  (≈14 px × scale) | new line numbers | new text | marker strip (≈6 px).
- One `ScrollArea::vertical().show_rows` drives every column (rows are
  aligned). Row height and the monospace font scale by
  `terminal::font_size / config::DEFAULT_FONT_SIZE`, as in `HistoryView`,
  including rescaling the scroll offset on zoom.
- **Bands:** `Removed` red on the left, `Added` green on the right, `Modified`
  amber both sides with a stronger tint over `hot`, padding cells a faint
  neutral fill. Colors come from `details.rs`'s `status_color` family at
  reduced alpha; no new theme fields in v1.
- **Connector gutter:** for each visible row inside a block, a straight band in
  the block's color spans the gap.
- **Marker strip:** pinned at the right edge, outside the scroll area. Each
  block is painted at its proportional position in the whole file, with a thin
  outline for the current viewport. Clicking the strip scrolls there. egui's
  own scrollbar is left alone.
- **Horizontal:** one shared offset `hx`. Text paints at `x - hx`, clipped to
  its column; line numbers and gutter stay put. Horizontal wheel delta
  (Shift+wheel) is read while the body is hovered; a thin bar under the text
  columns drags both. Extent is `max_cols × monospace advance`, so nothing is
  measured per frame.

**Navigation:**

- After each load, scroll to the first difference.
- Prev/next (buttons, or F7 / Shift+F7 while the window is active) put the
  adjacent block's first row a third of the way down the viewport. The
  "current" block is derived from the scroll offset each frame, never stored,
  so free scrolling cannot leave it stale.
- Up/Down/PgUp/PgDn/Home/End scroll while active. Keys are read locally, like
  `imageview.rs`'s Ctrl+0; they do not go through the Leader keymap.

**States:** no target yet ("Select a file in Git History"), loading (spinner),
notice (one dim line), error (git's stderr, one line), document.

### 4. Wiring in `src/wm.rs`, `src/workspace.rs`, `src/git_history*.rs`

- `Content::GitDiff(DiffView)`, added to every exhaustive `Content` match
  alongside `GitHistory` (no PTY, no icon, `claims_click` on show).
- `ContentSnap::GitDiff { commit, parent, status, old_path, path }`. Old
  workspace files simply lack the variant. Restore rebuilds the target and
  reads again; a gc'd commit shows git's error line.
- `WindowManager::open_git_diff_window(target)`: surface an existing
  `Content::GitDiff` in this manager and retarget it, else create one via
  `next_slot` + `push_win`. Sets `Tab.title` to `Diff: <file name>` each time.
  Marks the workspace dirty.
- `DetailsView`: a single click on a file row records the `DiffTarget` (built
  from the loaded `Details` and the selected commit) and keeps a "last opened"
  highlight on that row. Folder rows keep their collapse toggle.
- `HistoryView.acts: Vec<HistoryAct>` with `HistoryAct::OpenDiff(DiffTarget)`,
  drained by a new `WindowManager::drain_history_acts` next to
  `drain_plan_acts`. History refresh does not touch the Diff window; its
  target is self-contained.

## Out of scope for v1

Syntax highlighting, curved connectors, the viewer/whitespace/highlight
dropdowns, text selection and copy (painted, virtualized rows need their own
selection model), search, unified view, diffs of the working tree or of two
arbitrary commits, and opening a diff from anywhere other than the details
pane.

## Testing

Module-local tests, the same style as `git_history.rs` today.

- **`git.rs`:** cancel kills and reaps a blocked child; cap returns
  `TooLarge`; timeout fires; non-zero exit carries stderr. The existing
  `stream_history` and `details` tests pass unchanged, and they are the gate
  for the extraction.
- **`diff.rs`, pure and table-driven:** pairing (3−/7+, 7−/3+, lone `+` run,
  lone `-` run, two blocks separated by one context line); strict rejection (a
  two-hunk fixture shaped like finding 1, and a header count mismatch);
  binary, submodule, and unchanged notices; CRLF, tab expansion, the
  no-newline marker; word trim across multibyte chars and the one-sided `hot`
  case.
- **Real repositories** (tempdir, as `details.rs` does): A, D, M, R, C, T,
  root commit, merge against first parent; a file over the line cap giving
  `TooLarge`; a regression test that the chosen `-U` yields exactly one hunk;
  a linked worktree's `status --porcelain` unchanged afterward (no writes).
- **View:** a 100k-row `Doc` paints only viewport rows (same shape as
  `large_history_paints_only_viewport_rows_and_scrolls_to_old_commits`);
  F7/Shift+F7 land on the right block; rows follow the theme font size;
  retarget cancels the previous request.
- **wm:** two opens reuse one Diff window and retarget it; snapshot
  round-trip restores the target.
- **Evidence:** native screenshot through **build-screenshot** (user-run) of a
  modified file, a rename, and a notice state.

## Task order

1. Extract `git.rs`; convert `stream_history` and `details::git_output`.
   Behavior-preserving, gated by existing tests.
2. `diff.rs` parser and row model with its tests.
3. `DiffView`: request lifecycle, painting, navigation.
4. Wiring: `Content`/`ContentSnap`, opener, `HistoryView.acts`, details click.
5. Update `docs/git-history.md` (Diff section and key files; remove the
   "Diffs … outside this viewer's scope" line), then the screenshot.
