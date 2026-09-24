# Git History

## What it does

Each Project can open a native, read-only Git History window. Use Leader then
H (default Leader: Ctrl+B), or find **Open project Git history** in the bindings
help or keybindings editor. Opening again surfaces the existing window, including
a minimized window or an inactive tab. It floats initially and supports the same
tiling, tabbing, zoom, and workspace restore behavior as other viewers.

The timeline shows commits reachable from all refs and HEAD in topological
order, with colored branch/merge lanes, commit subjects, branch/tag decorations,
authors, and author dates. Hover a row for its full hash and untruncated metadata.
Scroll vertically through history; horizontal scrolling keeps wide graphs and
metadata reachable in a narrow window. Refresh starts a new read from the current
repository state. An empty repository and a failed Git read have distinct states.
Git must be available on PATH. Linked worktrees and detached HEAD work through
Git's own repository discovery.

Text and row geometry follow the theme font size (Appearance → Font size, or
Ctrl+Scroll zoom), scaled from the 13px default the same way the board does.
Zooming keeps the same rows in view by rescaling the scroll offset.

Click a timeline row to select it. The right-hand details pane shows the full
commit message, copyable object id, author name/email, author timestamp with
timezone, and local/remote branches containing that commit. The branch list is
an ancestry query, not just the decorations attached to the selected row.

The changed-file tree groups paths into collapsible directories. File labels
and status letters share JetBrains' Darcula file-status palette, independently of
graph lane colors. Copies use the added-file color and type changes use the
modified-file color; status letters also identify each change. The mapping is
in `src/git_history/details.rs` (`status_color`), based on JetBrains'
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

Branch selection and checkout/switching belong to the next task. Diffs, search,
context menus, and other Git operations are outside this viewer's scope.

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
parser, batch, repository, virtualization, and workspace tests. The repository
fixture exercises an annotated tag, merge, linked worktree, detached commit,
empty repository, and non-repository error. The graph cases include linear,
diamond, octopus, and disconnected history.

On 2026-09-23, the synthetic 100,000-commit test built the graph in 148 ms and
rendered twelve headless debug frames in 39 ms. After a large scroll it painted
only the visible rows around commit 1,700. This is a bounded-work regression
check, not a release-build end-to-end frame-time benchmark.

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

## Key files

- `src/git_history.rs`: `HistoryView`, `Stream`, `stream_history`, `Graph`, and
  module-local tests.
- `src/git_history/details.rs`: `DetailsView`, cancellable commit queries,
  changed-file parsing, and the virtualized tree.
- `src/wm.rs`: `Content::GitHistory`, `open_git_history_window`, snapshot capture
  and restore, and the Project-scoped command dispatch.
- `src/keymap.rs`: `Command::OpenGitHistory` and its default binding.
- `src/workspace.rs`: `ContentSnap::GitHistory` (window identity only; cached
  commits and scroll position are not persisted).
