# Git History

## What it does

Each Project can open a native, read-only Git History window. Use Leader then
H (default Leader: Ctrl+B), or find **Open project Git history** in the bindings
help or keybindings editor. Opening again surfaces the existing window, including
a minimized window or an inactive tab. It floats initially and supports the same
tiling, tabbing, zoom, and workspace restore behavior as other viewers.

The timeline shows commits with colored branch/merge lanes, commit subjects,
branch/tag decorations, authors, and local commit times (`8/18/2026 10:13 PM`).

Order is `git log --date-order`: a parent never shows above its children, but
otherwise newest first, so parallel branches interleave by time like other Git
graph clients. The old `--topo-order` kept each branch in one block, which put
a 10 PM mainline commit below an 8 PM branch commit. The time shown is the
**committer** time, which is what `--date-order` sorts by. The author time can
differ after a rebase or cherry-pick and would make rows look out of order.
Which commits it walks is the **scope**, picked from the dropdown at the left
of the header:

- **Current** (the default, shown as the branch name): `HEAD` plus its
  upstream. `Detached HEAD` when detached.
- **Local branches**: `HEAD` and every local branch, `card/*` included.
- **All**: `HEAD`, local and remote branches, and tags. Unlike `git log
  --all` it skips `refs/stash` (stash commits draw as fake merges) and other
  tools' private refs.
- **One branch**: any branch from the list below the scopes, grouped Local /
  Cards / Remote, with a filter once there are more than eight.

The branch list is read fresh each time the dropdown opens. Picking restarts
the read and keeps the selected commit in the details pane. The scope
survives Refresh but not closing the window: a new History window starts on
Current. If a picked branch is deleted (a finished card), the next read falls
back to Current. Resolution lives in `src/git_history/scope.rs` (`resolve`,
`revisions`).

Gotcha: the dropdown menu is a fixed height (`MENU_H`), even for two
branches. The branch list arrives a frame or more after the popup opens, and
egui's combo scroll area never grows past the size it had on the spinner
frame, so a content-sized menu clipped every branch row. Don't nest a second
`ScrollArea` inside the combo either; it collapses to a sliver.

The subject sits beside the graph and ref chips; the
date stays pinned to the right edge of the visible timeline pane, even while
scrolled sideways, with the author name right-aligned just before it at its own
width (capped). The subject wins: author and date only use room the subject's
full text leaves free on that row. They never move; as the subject's end comes
within 32px of one, it fades out, the name first and then the date. The fade
follows pane width, not time, so dragging the divider fades smoothly both ways.
Once both are gone the subject is elided at the pane's edge. So on a narrow
pane, rows with long subjects show no metadata. Hover a row for its full hash and untruncated metadata.

Subjects start after the widest lane graph **on screen**, not the widest in
the whole history, so a busy stretch only pushes text right while it is
visible. Growth is instant (lanes never paint under text); when the wide
rows scroll away the column glides back over 150 ms (`ease_lanes`).

### Long edges are stubs

A card branch that forked from `main` long ago used to hold a lane open for
every row down to its fork point. With twenty open cards that is twenty lanes,
and the subjects get shoved off the pane. Now an edge longer than **30 rows**
is drawn as two colored stubs instead:

- a **▼** under the child, one half-row stem ending at the row bottom;
- a **▲** capping the parent's lane one row above the parent.

Both are the parked edge's color, so you can match them by eye. Between them
the lane is free for other branches. Exactly 30 rows is still a normal lane.
A parent that never shows up (shallow clone, or outside the scope) gets only
its ▼.

If a *near* child (within 30 rows) reaches the parent first, the parent goes
live in that child's ordinary edge, keeps the parked color, and gets no ▲.
On a linear `main` this is the common case: the main commit above a fork point
is its child, so the fork point just takes the card's color, the same color
change the old full lanes showed. You mostly see a ▲ under a long merged side
branch, where the merge's first parent lands 30+ rows down. Many far children
of one parent share one parked color and one ▲.

The ▼ sits right of every lane that continues past that row, so it can angle
sideways from its node. It never lands on another lane's end point.

Arrows are paint only: no click, hover, tooltip, or jump. Clicking the row
still selects the commit.

How it works: `Graph::feed` buffers 30 commits of lookahead so each row knows
whether a parent arrives soon. It returns at most one finished row per call.
`Graph::finish` lays out the rest at EOF, exactly once. A long parent is
*parked*: a color and no lane. It goes live one row before it arrives.
`stream_history` only packs those rows into 512-row pages, so the first page
reads up to 542 commits and page cuts never change a row. `Row::width` counts
live lanes and visible stubs, never parked parents.

Scroll vertically through history; the timeline only scrolls sideways when the
lane graph itself is too wide for the pane. Column geometry is the pure
`columns` function in `src/git_history.rs`. Refresh starts a new read from the current
repository state. An empty repository and a failed Git read have distinct states.
Git must be available on PATH. Linked worktrees and detached HEAD work through
Git's own repository discovery.

Text and row geometry follow the theme font size (Appearance → Font size, or
Ctrl+Scroll zoom), scaled from the 13px default the same way the board does.
Zooming keeps the same rows in view by rescaling the scroll offset.
Widget fonts and spacing use the shared view scale; painted graph geometry
uses explicit scaled coordinates.

Click a timeline row to select it. The right-hand details pane shows the full
commit message, copyable object id, author name/email, author timestamp with
timezone, and local/remote branches containing that commit. The branch list is
an ancestry query, not just the decorations attached to the selected row.

Drag the vertical divider between the timeline and the details pane to resize
them. The pane keeps its width in px (scaled by zoom), so growing the window
widens the timeline, and shrinking then regrowing it restores your width. The
width survives commit selection and Refresh but is not saved across restart.

The changed-file tree groups paths into collapsible directories; a chain of
single-child directories folds into one row (`.foreman/tasks`). File labels
and status letters share JetBrains' Darcula file-status palette, independently of
graph lane colors. Copies use the added-file color and type changes use the
modified-file color; status letters also identify each change. The mapping is
in `src/git_history/file_tree.rs` (`status_color`), based on JetBrains'
[default color schemes](https://github.com/JetBrains/intellij-community/blob/master/platform/platform-resources/src/DefaultColorSchemesManager.xml).
Hover a rename for both paths. Roots compare against the empty tree; merges compare
against their first parent, as labeled in the pane. Empty changes have an
explicit placeholder. The tree sits above the subject-led commit details, with a
draggable divider and independent scrolling. Folders reuse the Sessions panel
icon; files have a folded-page icon. Clicking a file selects it;
directory expansion and scrolling preserve that selection. Status meanings and
rename paths are available on hover, without a permanent legend. Compact
metadata includes containing-branch badges and a short hash that copies the
full object id when clicked.
Selecting another row cancels the previous read. Refresh clears the selection.

Click a file in the changed-file tree to open it in the project's Diff window.
Checkout, search, context menus, and other Git operations are outside this
viewer's scope.

## Git Changes window

Each Project can open a read-only Git Changes window: Leader then U, or
**Open project Git changes** in the bindings help. It's the JetBrains Commit
tool window's change list without anything that writes: no checkboxes, commit
message, stage, rollback, or push. Opening again surfaces the existing window.

The header shows the branch ("On main", or "Detached HEAD") and the change
count. Files sit under collapsible sections, each a directory tree in the same
status colors as the commit details:

- **Conflicts**: unmerged files (`U`). Their diff is working copy vs HEAD,
  because a conflicted index has no single version to compare against.
- **Staged**: what the next commit would contain (HEAD vs index), including
  staged renames.
- **Changes**: edits not yet staged (index vs working copy).
- **Unversioned Files**: untracked files (`?`). Every file inside a new
  directory is listed, not just the directory. Ignored files are not shown.

A file that is staged and then edited again shows up twice, once in Staged and
once in Changes, and each opens its own diff. That split is the reason for the
Staged section: JetBrains merges them, which hides what a commit would contain.
Empty sections are hidden. A clean tree says so.

Click a file to open it in the Diff window. Clicking the same working-tree file
again re-reads it; a commit diff with an unchanged target does not.

**Refresh model:** one `git status --porcelain=v2 -z --branch
--untracked-files=all` read per refresh, on a worker. Refresh runs when the
window first shows, when you press Refresh, and when the window becomes active
(focused, inside the active Project), at most once per second. There is no
file watcher and no polling, so nothing runs while you aren't looking at the
window. The flip side: while the window stays focused, agents' edits don't
appear until you press Refresh or focus away and back. The previous list stays
up while a re-read runs, and collapsed folders and the selected file carry over.

Gotchas:
- Untracked files are read from disk and turned into an all-added diff, not run
  through `git diff --no-index`: that command exits 1 whenever the files
  differ, which the shared git helper reports as failure. The binary check
  copies Git's (a NUL in the first 8000 bytes), and the size cap is 16 MiB. A
  nested repository (`dir/` in status) shows the submodule notice. Paths from a
  restored workspace are rejected if they're absolute or contain `..`.
- Staged T/R/C diffs use the blob form (`HEAD:old` vs `:new`, resolved to ids),
  for the same mode-split reason as commit diffs. An unstaged type change
  (file ↔ symlink) still splits and shows the too-large notice.
- Reads are repository-read-only: the shared helper sets
  `GIT_OPTIONAL_LOCKS=0`, so `git status` doesn't refresh the index as it
  normally would.

## Diff window

One Diff window per Project, reused: clicking another file retargets it. It
tiles, tabs, zooms and restores (with its file) like any viewer. It shows the
whole file side by side, old left and new right, with both line numbers.
Changed regions are banded: red removed, green added, amber modified, with the
changed middle of a modified line tinted stronger. A gutter band links each
change across the panes; a strip on the right marks every change in the file
and outlines the viewport (click it to jump). The header shows the path
(`old → new` for renames), the commits compared (or, for a Git Changes file,
"Staged vs HEAD", "Working copy vs index", and so on), and "N differences".

Keys while the window is focused: F7 / Shift+F7 next/previous difference;
Up/Down/PgUp/PgDn/Home/End scroll. Shift+wheel scrolls both sides
horizontally together; so does the thin bar under the text.

Drag the connector gutter between the panes to give one side more room;
double-click it to even them out. The split is a fraction of the text width,
so it holds when you open another file in the same Diff window or resize the
window. Each side stops at a 12-char minimum. With unequal sides the shared
horizontal scroll range is sized to the narrower side, so both can reach line
ends. The divider's hit area is registered after the rows so it wins over the
ScrollArea's own drag; move it earlier and dragging the gutter scrolls
instead. The split is not saved across restarts.

Binary files, submodules, diffs over 16 MiB or that git cannot return as one
whole-file hunk (a change more than 200,000 lines from the file start or from
the next change), and changes with no content difference (pure renames, mode
changes, empty adds) show a one-line notice instead.

Gotchas:
- Git computes the diff with `-U200000`: `diff-tree -p` for A/D/M, the
  `<rev>:<path>` form for R/C, and bare blob ids (`rev-parse` first) for T.
  T can't use `<rev>:<path>`: when the file mode changes, git 2.39 splits it
  into a delete plus an add. Never raise `-U` toward `i32::MAX`: git 2.39
  emits overlapping repeated hunks there. The parser accepts exactly one
  whole-file hunk and turns anything else into the too-large notice.
- CRLF is shown as LF, so a pure line-ending change shows Modified rows with
  nothing highlighted inside them. Tabs are expanded to 4 columns.
- The monospace grid assumes one column per char; wide CJK glyphs render wider
  than their column and can misalign highlights on that line.
- Painting lays out only the visible slice of each line, so a 1 MB minified
  line stays cheap. Slices always land on char boundaries; a bad slice would
  panic the frame and take the whole app down.
- No syntax highlighting, text selection, or copy (v1).

## Design and implementation plan

### Details-pane polish specification

The polish scope is presentation and single-file selection; opening file diffs
is outside this scope. Preserve Foreman's warm dark palette, compact density,
and restrained separators while using JetBrains' information hierarchy.

- Place the changed-file tree above commit details with a draggable divider
  and independent scrolling for each region.
- Give the tree folder/file icons, consistent indentation and disclosure-arrow
  spacing, and muted file counts beside directories.
- Keep status-colored filenames and status letters using the shared palette in
  `status_color`. Remove the permanent legend and explain statuses on hover.
- Lead commit details with the subject, then the message, then compact author,
  date, and hash metadata. Show a short hash; its copy action copies the full
  hash. Show containing branches as compact badges alongside the metadata.
- Clicking a file selects only that file with a subtle full-row background.
  Keep its status color and letter readable. Hover has a lighter treatment
  distinct from selection. File clicks only select; they do not open a diff.
- Preserve file selection through scrolling and directory collapse/expansion.
  Directory clicks only toggle expansion. Selecting another commit or
  refreshing clears file selection.

Validate the layout, divider, scrolling, hover/selection distinction, and
status-color readability with native screenshots. Verify selection persistence
and clearing behavior, and run the repository's build/test checks.

### Existing viewer architecture

The implementation follows the existing Project viewer seam instead of adding
another window system:

1. Add a Git History Content variant, singleton opener, command, and cold
   workspace snapshot. Restore derives its directory from the owning Project.
2. Read one Git log stream on background threads. Send a bounded batch only
   when the viewer requests it. Keep the graph frontier across batches so
   scrolling cannot introduce false roots or change lane colors.
3. Precompute row edges on the worker. Paint only the viewport's rows with egui
   virtualization, without scanning the loaded history on each frame.
4. Validate split/join behavior, stream framing, real repository variants,
   restore, large-history rendering, and the actual native window.

`Graph` tracks pending parent commits as a frontier. Each pending hash occupies
one lane. Visiting a commit replaces its lane with unseen parents, reuses lanes
for shared parents, and compacts completed lanes. Colors travel with pending
commits, rather than with column positions. Edges connect the before and after
frontiers across each row; disconnected tips and roots do not invent edges.

Git subprocess creation, pipe reads, parsing, graph layout, cancellation, and
child reaping happen off the GUI thread. A process supervisor can kill Git even
when its output reader is blocked. Closing or refreshing disconnects the old
stream and discards its replies; there is no shared receiver for stale data.
Loaded pages are also freed on a background thread. No repository writes,
network fetches, or terminal Sessions are created by the viewer.

Commit details use their own cancellable request and receiver so a late result
cannot replace a newer selection. Git output is NUL-framed for changed paths,
including rename pairs; control characters are escaped for display. Tree rows
are built off-thread and only visible rows are painted. Detail queries time out
and reject oversized output with an error instead of displaying partial data.

The viewport requests more rows near the loaded end. There is no fixed history
cutoff. Memory grows with the history actually visited, not the full repository
at open; Git may itself traverse a large graph before returning the first row.
The status count is the loaded count until the stream reaches EOF. Refresh is
manual, keeping a single stream's ordering and decorations stable while browsing.

## Validation

Run `cargo test --target-dir target/agent git_history -- --nocapture` for graph,
parser, batch, repository, virtualization, workspace, `diff` (unified-diff
parser), and `diff_view` (git reads, request lifecycle, painting) tests.
`cargo test --target-dir target/agent git_diff_window` covers the wm wiring. The repository
fixture exercises an annotated tag, merge, linked worktree, detached commit,
empty repository, and non-repository error. The graph cases include linear,
diamond, octopus, and disconnected history.

On 2026-09-23, the synthetic 100,000-commit test built the graph in 148 ms and
rendered twelve headless debug frames in 39 ms. After a large scroll it painted
only the visible rows around commit 1,700. This is a bounded-work regression
check, not a release-build end-to-end frame-time benchmark.

On 2026-09-23, the diff view parsed a synthetic 100,000-line file in 82 ms and
rendered twelve headless debug frames in 52 ms. Same caveat: a bounded-work
check, not a release frame-time benchmark.

Native screenshots use a seeded workspace and fixture repository under
`target/history-evidence`, with isolated APPDATA and global skill installation
disabled. The build-screenshot script captures via PrintWindow without driving
the user's mouse or keyboard.

On 2026-09-23, details-pane polish was checked in native 1280×800 captures
under `target/history-evidence`: default layout, distinct hover and selection,
scrolled tree, resized divider, collapsed directories, and scrolled metadata.
The captures used temporary fixture-selection/input instrumentation, removed
before committing; no user mouse or keyboard input was used. Pointer-event tests
cover single-file selection, directory toggling, and divider dragging. Selection
is owned by the loaded commit details, so retiring those details clears it.

On 2026-09-26 the scope dropdown and per-screen text column were checked in a
native build against `epic-manager`. The closed dropdown reads `main ▾` and fits
at default and 2× zoom. The popup shows the three scope rows, dim LOCAL /
CARDS / REMOTE headings, the filter field and a ✓ that renders as a check, and
the fixed-height empty space is acceptable. On All, subjects move right
through busy regions and glide back afterwards. `scope_dropdown_*`,
`scopes_walk_only_their_refs`, `text_column_grows_at_once_and_eases_back` and
`subjects_start_after_the_widest_graph_on_screen` pin the behavior.

On 2026-09-26 the long-edge stubs were checked two ways. First, the pure
width proof `staggered_card_branches_have_bounded_visible_width`: a generated
history of N card branches, each forking from its own distant point on
`main`, half of them merged. Collapsed max width was 2 lanes at N = 5, 20 and
50, where the old full-lane layout reached 6, 21 and 51. All 73 `git_history`
tests passed. Second, native captures of a fast-import fixture (`main` 0–59
plus merged cards A, B, C and a 40-commit card D) under
`target/history-evidence/phase2`, font size 7 and detached HEAD to frame each
stretch without input. At `main 3`, the teal ▼ and the teal ▲ on `card D 1`
match, with lane 1 free between them. At `main 8`, card A's ▼ is orange and
card C's is blue. `card B 1` rejoins the parked `main 45` in card A's orange
with no ▲.

## Key files

- `src/git_history.rs`: `HistoryView` (including `restart`, the one path for
  Refresh and a scope change, and the scope dropdown), `Stream`,
  `stream_history`, `Graph` (`Graph::feed` / `Graph::finish`: lookahead,
  parked long edges and `Arrow` stubs), `HistoryView::show` (paints the
  ▲/▼), `ease_lanes` (the per-screen text column), and module-local tests.
- `src/git_history/scope.rs`: `Scope`, `resolve` (scope → `git log`
  revisions and header label, with the deleted-branch fallback), and
  `branches` (the dropdown's grouped branch list).
- `src/git_history/details.rs`: `DetailsView`, cancellable commit queries, and
  changed-file parsing.
- `src/git_history/file_tree.rs`: `FileTree`, the virtualized status-colored
  tree (with sections) shared by the details pane and Git Changes, and
  `status_color`.
- `src/git_history/changes.rs`: `ChangesView`, the `git status` porcelain v2
  parser, and the refresh-on-activate model.
- `src/git_history/git.rs`: the shared Git subprocess helper (spawn, capped
  drains, cancel/timeout watchdog) used by the history stream, details, and diff.
- `src/git_history/diff.rs`: pure unified-diff parser into aligned rows and blocks.
- `src/git_history/diff_view.rs`: `DiffTarget` and its `Stage`, the diff reads
  (commit and working tree, plus the untracked-file synthesis), and `DiffView`.
- `src/wm.rs`: `Content::GitHistory`, `Content::GitDiff`, `Content::GitChanges`,
  `open_git_history_window`, `open_git_diff_window`, `open_git_changes_window`,
  `drain_history_acts`, snapshot capture and restore,
  and the Project-scoped command dispatch.
- `src/keymap.rs`: `Command::OpenGitHistory` / `OpenGitChanges` and their
  default bindings (H / U).
- `src/workspace.rs`: `ContentSnap::GitHistory` (window identity only; cached
  commits and scroll position are not persisted), `ContentSnap::GitChanges`
  (window identity only), and `ContentSnap::GitDiff` (the target, including its
  `stage`, is persisted; restore re-reads it from git; old files without
  `stage` restore as commit diffs).
