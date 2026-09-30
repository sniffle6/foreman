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

- a **▼** one row under the child: the child's edge runs into a one-row
  "tail" lane right of the continuing lanes, and the arrowhead ends it at the
  bottom of the next row, the way JetBrains draws it. The tail is a real lane
  for that row, so nothing is laid across it. If the next commit would open a
  new lane, that lane opens a row early, left of the tail, so the commit is not
  pushed sideways. The last row of history has no next row, so a tail there
  gets no arrowhead;
- a **▲** capping the parent's lane one row above the parent.

Both are the parked edge's color, so you can match them by eye. Between them
the lane is free for other branches. Exactly 30 rows is still a normal lane.
A parent that never shows up (shallow clone, or outside the scope) gets only
its ▼.

If a *near* child (within 30 rows) reaches the parent first, the parent goes
live in that child's ordinary edge and keeps the parked color. It still gets
a ▲, as JetBrains draws it: the row just above the parent carries the ▲ right
of every lane, with a short stroke angling down into the parent's lane. On a
linear `main` this is the common case: the main commit above a fork point is
its child, so the fork point takes the card's color and the ▲ angles into the
main lane. Many far children of one parent share one parked color and one ▲.

The ▼'s tail sits right of every lane that continues past the child's row,
so the child's edge can angle sideways into it. The ▼ never lands on another
lane's end point.

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
repository state (see **Live updates** below for what happens when refs move
on their own). An empty repository and a failed Git read have distinct states.
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
icon; files have a folded-page icon. Selection works as in the next section;
directory expansion and scrolling preserve it. Status meanings and
rename paths are available on hover, without a permanent legend. Compact
metadata includes containing-branch badges and a short hash that copies the
full object id when clicked.
Selecting another row cancels the previous read. Refresh clears the selection.

Double-click a file (or press Enter on it) to open it in the project's Diff
window. The right-click menu has **Open Diff** (one file selected) and **Copy
Path** (newline-separated). Checkout, search, and other Git operations are
outside this viewer's scope.

### Selecting files and the context menu

Both file trees (commit details, Git Changes) share one selection model in
`file_tree.rs`:

- **Click** selects just that row. **Ctrl+click** toggles it. **Shift+click**
  selects from the anchor (the last plain or Ctrl click) to here, replacing
  the selection; **Ctrl+Shift+click** adds the range instead.
- **A folder or section row stands for every file under it**: clicking it
  selects them all, Ctrl+click toggles them all. A folder row shows as
  selected when all its files are. A Shift range takes the file rows it
  covers plus the hidden files of collapsed folders in it; an *open*
  heading inside the range does not drag in the rest of its section.
- **The disclosure arrow** (the strip left of the folder icon) collapses and
  expands without touching the selection. Double-clicking a folder row
  toggles it too.
- **Double-click opens** a file's diff; a single click only selects. Decided
  so that Ctrl/Shift clicks never fire an open, and so a click doesn't hand
  focus to the Diff window and strand the keyboard there.
- **Keyboard**, while the tree's window is active and no text box has focus:
  Up/Down move the cursor row and select it, Shift+Up/Down extend from the
  anchor, Ctrl+A selects all, Enter opens the file (or toggles a folder),
  Left/Right collapse/expand the folder under the cursor, Shift+F10 opens the
  context menu at the cursor. The cursor row gets a thin outline.
- **Right-click** on a row that isn't selected selects just it first; on a
  selected row it keeps the whole group. The menu then acts on the group.

**The menu seam.** The tree doesn't know any actions. Its owner passes
`menu(files) -> Vec<MenuItem>`; the tree calls it with the selection when the
menu opens and draws the items, and reports the choice back as
`TreeEvent::Act { item, files }`. Each item is always listed and enabled only
when it applies to something selected, so the menu doesn't shuffle. There
are no hover buttons on rows; every action is in the menu. New actions go in
the owner's `menu`, not in `file_tree.rs`.

**Checkboxes** (Git Changes only: `FileTree::grouped` with a section's
check flag). A box sits between the disclosure arrow and the icon. Clicking
it ticks or unticks that row's files (a folder or section: all of them) and
never touches the selection. A folder's box is ticked, empty, or a dash
when only some of its files are. Space ticks the selected files, or unticks
them if all are already ticked. The tree keeps its own checks for painting
and reports each change as `TreeEvent::Check { files, on }`; the owner holds
the real set.

Gotchas:
- **The Menu key doesn't work, only Shift+F10.** egui-winit 0.34 has no
  `egui::Key` for it and drops the press before egui sees it.
- **Keys go to the tree via the window's `active` flag, not egui focus.**
  A focused egui widget suppresses the leader key (`pump_commands`), so the
  tree deliberately never takes focus. It yields to any focused widget (the
  commit message box), and consumes its keys so nothing behind sees them.
- **Selection survives a re-read by row key** (section + path), so a file
  that moved from Unversioned Files to Changes is not selected any more: it's
  a different row. Cursor and anchor carry the same way (`keep_selection`).
  Grouping by directory vs flat doesn't change file keys, so the selection
  and checks survive that switch too (`set_flat`).
- egui reports a quick third click as another double-click, whatever row it
  lands on. Tests that double-click twice need a fresh `Context` between.

### Live updates: the "↻ N new commits" pill

While the History window is shown, the repository watch (below) tells it
when a ref or `HEAD` moved. It then re-reads the scope's **fingerprint** on
a worker: the tips of the refs the scope walks, plus `HEAD`, plus (Current
only) which branch is checked out. If that changed:

- scrolled down: a pill appears in the header, `↻ 1 new commit` /
  `↻ N new commits` (`git rev-list --count <scope> --not <old tips>`), or
  `↻ History changed` when nothing new is reachable (reset, rebase, a
  deleted branch, a tag). Clicking it re-reads and keeps the selected
  commit.
- at the top of the timeline: it just re-reads, no pill. Nothing is lost
  there, and JetBrains follows the top the same way.

Refs the scope doesn't walk never count: a card commit leaves Current alone
but shows up in Local and All; `refs/stash` counts nowhere. The stream takes
its fingerprint *before* `git log` starts, so a ref that moves during the
read is caught by the next event instead of being missed.

Without a live watch (see below), the window becoming active stands in for
the event: it re-checks the fingerprint the same way, so the pill or the
silent re-read still happen, only on focus instead of at once.

A re-read (pill, silent, Refresh, or a scope change) keeps the old rows
and count painted until the new stream's first page lands, then swaps them
in one frame and frees the old rows on a worker thread, so agents
committing often never blink the timeline. Gotcha: the scroll area is
keyed by the read's generation, so a re-read still jumps to the top at
once, over the old rows; clicking a held row selects that (still valid)
commit.

## Repository watch

`src/git_history/watch.rs`. One `RepoWatch` per worktree root, shared by the
Changes and History windows and the board's card status through a registry of
weak refs; the last holder to drop stops its thread. A thread waits on `ReadDirectoryChangesW`
(recursive) on the worktree root, plus the common git dir when that is
outside it (a linked worktree like a card's). Each changed path is sorted:

| Path | Means |
|---|---|
| this worktree's git dir: `HEAD` | both |
| … `index` | working tree changed |
| … `MERGE_HEAD`, `rebase-merge/`, `rebase-apply/` | refs moved |
| common dir: `refs/heads/**`, `refs/remotes/**`, `refs/tags/**`, `packed-refs`, `reftable/**` | refs moved |
| common dir: `config`, `info/exclude`; any `.gitignore` | re-list ignored paths |
| `objects/`, `logs/`, `FETCH_HEAD`, `ORIG_HEAD`, `refs/stash`, `*.lock`, other worktrees' git dirs | nothing |
| anything else in the worktree, unless ignored | working tree changed |

Bursts are debounced (trailing 300 ms, at most 2 s late), then a generation
counter per kind is bumped and egui repaints. Views compare generations, so
two windows on one watch never eat each other's signal. If the kernel buffer
overflows, it waits 500 ms and bumps everything.

Gotchas:
- **Ignored paths are load-bearing.** Card worktrees live inside the main
  checkout (`.foreman/worktrees/<id>`, excluded via `.git/info/exclude`), so
  without the `git ls-files --others --ignored --exclude-standard
  --directory` list every agent write would wake the main project's Changes
  window. `target/` likewise. The list is re-read on ignore-file changes, and
  at most every 10 s while the working tree churns (so a `target/` created
  after opening stops waking it).
- `*.lock` is ignored only inside git dirs (git writes `index.lock` and
  renames it onto `index`; the rename is the event). `Cargo.lock` in the
  worktree counts.
- Events can arrive as 8.3 short names (`PACKED~1`). The thread expands
  them; one it can't expand (already deleted) counts as everything.
- The handles share read/write/delete, so a watch never blocks a card
  teardown. When the watched root is deleted the read fails and the thread
  exits for good; the thread never retries on its handle (that would hold
  the tree teardown wants gone). The *view* (`Follow`) opens a fresh watch
  every 10 s while it is shown and down; a live watch never blocks
  teardown, so this is safe. A retry whose folder is gone costs one stat,
  not a `git` spawn, so a window left on a torn-down card worktree stays
  cheap, and it reattaches if the folder comes back. Until one is live,
  windows fall back to the old behavior.
- No watch on network paths (UNC, mapped network drives, `\\wsl$`): the
  windows keep the refresh-on-activate fallback.
- **Refresh only shows when it's needed.** With a live watch both windows
  are always current, so their Refresh button is hidden. It appears when
  the watch is down (`Follow::down`: an open finished without a live watch,
  including while a retry is in flight, so it doesn't flicker) or when the
  read failed. What you lose with a live watch: History's Refresh also
  cleared the selection; click a row or pick the scope again instead.
- Our own reads don't wake it: they run with `GIT_OPTIONAL_LOCKS=0` and
  write nothing. `our_own_reads_never_wake_the_watch` pins that.
- The watch thread runs while a view or card holds it, even when minimized,
  but only waits in the kernel; hidden views and the board run no Git.

## Git Changes window

Each Project can open a Git Changes window: Leader then U, or
**Open project Git changes** in the bindings help. It's the JetBrains Commit
tool window, non-modal, with the staging area turned off (JetBrains'
changelists mode). JetBrains is the spec: where this doc is silent, do what
JetBrains does. Opening again surfaces the existing window. It is the only
Git window that writes; History and Diff stay read-only.

The header shows the branch ("On main", or "Detached HEAD"), the change
count, and **Merging** while a merge is in progress. On its right: **Add to
VCS (N)** (only when unversioned files are checked), **Push…**,
**Directories** (group by directory, or a flat list sorted by file name with
the folder dimmed after it), **Expand All**, **Collapse All**.

Files sit under collapsible sections, in the same status colors as the
commit details:

- **Conflicts**: unmerged files (`U`), only while a merge has them. Their
  diff is working copy vs HEAD, because a conflicted index has no single
  version to compare against. No checkboxes.
- **Changes**: every versioned file that differs from HEAD, once, whether
  its change is staged, unstaged, or both. The letter is the whole change
  since HEAD (`combined`): a file added and then edited is `A`, a staged
  rename is `R`, anything gone from disk is `D`. Its diff is HEAD vs the
  working copy (`Stage::Local`); a new file is read from disk.
- **Unversioned Files**: untracked files (`?`). Every file inside a new
  directory is listed, not just the directory. Ignored files are not shown.

There is no Staged section. What an agent staged is still in the index;
this window just doesn't show the split, the same as JetBrains. Empty
sections are hidden. A clean tree says so.

### Checkboxes

The checkboxes are this window's own selection, **not Git's index**, as in
JetBrains: ticking a box never runs Git. They are kept in memory by path,
so they survive every live re-read; a path that leaves the list (committed,
rolled back, deleted) drops out.

- **Changes**: the boxes pick what Commit takes. **Nothing ticked means
  everything** in Changes.
- **Unversioned Files**: the boxes pick what **Add to VCS (N)** in the
  header puts under version control. A ticked unversioned file is never
  committed.

Files that change section because of a write (Add to VCS, rolling back a new
file) come back unticked.

Double-click a file (or Enter) to open it in the Diff window; selecting and
the menu work as in "Selecting files and the context menu" above. Opening the
same working-tree file again re-reads it; a commit diff with an unchanged
target does not.

**Refresh model:** one `git status --porcelain=v2 -z --branch
--untracked-files=all` read per refresh, on a worker. It runs when the
window first shows, when you press Refresh, and whenever the repository
watch says the working tree changed while the window is shown, so agents'
edits appear without touching anything. A watch-triggered read never cancels
one in flight (steady churn would starve it); it marks "again" and reads
once more when the current one lands. Those reads show no spinner, and when
the status bytes hash the same as last time, the shown tree is kept as is.
The Refresh button shows only without a live watch or after a failed read.
The previous list stays up while a re-read runs, and collapsed folders and
the selection carry over.

Without a watch (a network share, or the watched root was deleted) it falls
back to the old rule: re-read when the window becomes active, at most once
per second. A finished write also re-reads then; with a live watch the
index change does it. The status read also stats `MERGE_HEAD` in the git
dir (found once per window with `rev-parse --absolute-git-dir`), which is
how the window knows a merge is in progress.

### The context menu

Right-click the selection (see "Selecting files and the context menu").
JetBrains' names; each write takes only the selected files it applies to:

- **Show Diff**: one file. Same as double-click.
- **Add to VCS**: unversioned files. `git add --intent-to-add`: the file is
  versioned and shows in Changes as new (`A`), unticked, with nothing staged.
- **Mark Resolved**: conflicts. `git add`.
- **Rollback…**: Changes files, after a dialog listing them. A modified or
  deleted file goes back to HEAD, index and working copy (`git restore
  --source=HEAD --staged --worktree`). A new file (`A`, including Add to
  VCS ones) leaves the index and becomes unversioned again, **kept on disk**
  (`git reset`, which also works before the first commit). A rename does
  both: the old path comes back, the new file stays as unversioned.
  JetBrains deletes the new-name file; we keep it, since nothing else here
  can bring it back.
- **Delete…**: unversioned files and Changes files still on disk, after a
  dialog listing them. To the Recycle Bin (`SHFileOperationW` with
  `FOF_ALLOWUNDO`). A versioned file then shows as deleted (`D`). On a drive
  with no Recycle Bin (a network share) Windows deletes outright.
- **Add to .gitignore**: unversioned files. Appends one pattern per file to
  the `.gitignore` in the project folder, anchored and escaped so it matches
  only that file (`[a].txt` → `/\[a].txt`). JetBrains asks which ignore file;
  this always uses that one.
- **Copy Path**: newline-separated.
- **Show in Explorer**: one file, selected in its folder (a deleted file
  opens the folder).

Mark Resolved stays in the menu; the old hover Stage / Unstage / Mark
Resolved row buttons are gone. Paths go through `--literal-pathspecs` and
are fed on stdin (`--pathspec-from-file=- --pathspec-file-nul`), so a
file named `[a].txt` is only that file and a thousand-file commit never hits
Windows' command-line limit.

### Committing

- **Message box** pinned at the bottom, with **Amend** under it. Ctrl+Enter
  in the box commits.
- **Commit** takes the ticked Changes files, or all of Changes when none is
  ticked: `git commit --only -m <message> -- <paths>` (a rename brings both
  of its paths). `--only` commits those paths' **working-copy** content,
  staged or not, and leaves everything else in the index where it was:
  anything an agent staged for other files stays staged and uncommitted.
  Paths are always the ones the window shows, never `git add -A`.
  Unversioned files are never committed.
- **Disabled** while there are conflicts, with nothing to commit, or with a
  blank message; the hover says which.
- **During a merge** Git refuses a partial commit ("cannot do a partial
  commit during a merge"), so Commit adds every Changes path and commits the
  whole index, ticks or not (JetBrains does the same). The header says
  Merging.
- **Amend**: `--amend`. Ticking it over a blank message fills in the last
  commit's message (read on a worker; never over text typed meanwhile). With
  nothing to commit, Amend rewrites just the message. Off before the first
  commit and during a merge.
- Success clears the box, unticks Amend, and shows Git's
  `[main 1a2b3c4] subject` line; failure keeps the box and shows everything
  Git and the hooks printed (a failing `pre-commit` shows its output).
- **Commit and Push…**: commit, then the push dialog. Push is a separate
  step, as in JetBrains.
- **AI message**: one click, no chat. It reads the diff against HEAD of
  what Commit would take (the ticked files, or everything; before the first
  commit, the staged and intent-to-add files), capped at 32 KB; over that it
  sends `--stat` instead, plus `git log --oneline -10` for style, to the
  session-title provider and model from Settings through the shared one-shot
  launcher (`docs/ai-oneshot.md`). The reply replaces the message box. It
  must be non-empty plain text: a code fence or control characters are
  rejected with an error instead. Spinner and Cancel while it runs; Cancel
  drops the result, but the CLI process itself runs to its 90 s deadline in
  the background (the launcher has no cancel).

### The push dialog

Opened by Commit and Push… once the commit lands, or by **Push…** in the
header. It shows `branch → target` and the outgoing commits (hash and
subject, newest first, 200 listed at most), then **Push** / **Cancel**
(Enter / Esc). The target is the branch's upstream when it's on a remote,
even under another name (`topic` tracking `origin/main` goes to
`origin/main`), or with no upstream `origin/<branch>`, tagged "sets
upstream". It's marked **New** when that remote branch doesn't exist yet
(then the commits on no branch of that remote are listed). Push is disabled
when nothing would leave. Detached HEAD, "no upstream and no `origin`", and
an upstream that is a local branch show as errors in the dialog.

**One target, decided once.** `push::target` works it out (one
`for-each-ref` for the upstream's remote and branch) and returns a `Target`.
The dialog lists commits from it, and Push hands that same `Target` to the
write (`Write::Push(Target)`), which names both refs:
`git push [-u] <remote> refs/heads/<branch>:refs/heads/<remote branch>`. So
the push goes exactly where the dialog said. `push.default` can't redirect
it (a bare `git push` refuses the differently named upstream above), and
neither can a checkout between opening the dialog and clicking Push. A
rejected push (the remote moved) says REJECTED with Git's output; the commit
already stands either way.

Gotchas:
- **Writes go through `git::write`, never `git::output`.** The read helper
  sets `GIT_OPTIONAL_LOCKS=0` so our reads never wake the watch; writes
  take real locks and are supposed to wake it.
- **Commits and `add` are never killed.** A commit killed mid-write leaves
  `index.lock` behind and every later Git command fails until someone deletes
  it. So no timeout and no cancel; closing the window leaves the write
  running to completion. Only a push has a timeout (5 min), because killing
  a push leaves nothing locked locally.
- **One write at a time.** While one runs, the menu's writes and the commit
  and push buttons disable, so two writes never race for `index.lock`.
- **The dialogs own the keyboard.** While Rollback / Delete / Push is up,
  the tree reads no keys, so Enter confirms instead of opening a diff.
- `GIT_TERMINAL_PROMPT=0`: a push needing a password fails with Git's error
  instead of hanging on a terminal nobody can see. A GUI credential helper
  (Git Credential Manager) can still pop up its own window.

It is a whole-tree `git status`, not JetBrains' dirty-path-scoped one: about
76 ms on this repo, almost all Windows process spawn. Revisit only if a big
repository proves it slow.

Gotchas:
- Untracked files are read from disk and turned into an all-added diff, not run
  through `git diff --no-index`: that command exits 1 whenever the files
  differ, which the shared git helper reports as failure. The binary check
  copies Git's (a NUL in the first 8000 bytes), and the size cap is 16 MiB. A
  nested repository (`dir/` in status) shows the submodule notice. Paths from a
  restored workspace are rejected if they're absolute or contain `..`.
- Changes diffs (`Stage::Local`) are `git diff -M HEAD -- <new> <old>`, so a
  rename pairs up by rename detection. If the working copy has drifted too far
  for Git to still call it a rename, that is two file diffs and the window
  shows a malformed-diff error. A type change (file ↔ symlink) splits the
  same way and shows the too-large notice.
- `Stage::Staged` and `Stage::Unstaged` (index splits) are no longer produced
  by the Changes window except for conflicts, but still load for Diff
  windows restored from an older workspace. Staged T/R/C diffs use the blob
  form (`HEAD:old` vs `:new`, resolved to ids), for the same mode-split
  reason as commit diffs.
- Reads never write: the shared read helper sets `GIT_OPTIONAL_LOCKS=0`, so
  `git status` doesn't refresh the index as it normally would. Only the
  explicit menu, commit and push actions above write.

## Diff window

One Diff window per Project, reused: clicking another file retargets it. It
tiles, tabs, zooms and restores (with its file) like any viewer. It shows the
whole file side by side, old left and new right, with both line numbers.
Changed regions are banded: red removed, green added, amber modified, with the
changed middle of a modified line tinted stronger. A gutter band links each
change across the panes; a strip on the right marks every change in the file
and outlines the viewport (click it to jump). The header shows the path
(`old → new` for renames), the commits compared (or, for a Git Changes file,
"Working copy vs HEAD", "New file", and so on), and "N differences".

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
  Directory clicks only toggle expansion (superseded: a directory click now
  selects its files and the arrow toggles; see "Selecting files and the
  context menu"). Selecting another commit or refreshing clears file
  selection.

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
The status count is the loaded count until the stream reaches EOF. A stream
never changes under you: when refs move, the pill offers a re-read (or, at
the top, it re-reads), so one stream's ordering and decorations stay stable
while browsing.

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

On 2026-09-27 the live updates were pinned by tests, not screenshots:
`classify_sorts_paths_into_worktree_refs_and_ignore` (main checkout and
linked worktree layouts, `*.lock`, `logs/`, ignored prefixes, case, short
names), `debounce_trails_by_quiet_time_and_caps_at_max_wait`,
`bursts_in_ignored_dirs_are_silent_and_source_bursts_report_once` (1000
`target/` writes: no bump; 50 `src/` writes: exactly one),
`our_own_reads_never_wake_the_watch`, `deleting_the_watch_root_stops_the_thread`,
`a_watched_main_checkout_or_card_worktree_never_blocks_teardown` (both
`TeardownOutcome::Removed`), `watched_edits_appear_while_shown_without_a_focus_change`,
`moved_refs_offer_a_pill_when_scrolled_and_reread_silently_at_the_top`,
`unwatched_history_rechecks_refs_on_refocus_and_offers_refresh`, and
`a_dead_watch_reads_down_and_reopens_after_the_retry_interval`.
No native screenshot of the pill yet.

## Key files

- `src/git_history.rs`: `HistoryView` (including `restart`, the one path for
  Refresh and a scope change, and the scope dropdown), `Stream`,
  `stream_history`, `Graph` (`Graph::feed` / `Graph::finish`: lookahead,
  parked long edges and `Arrow` stubs), `HistoryView::show` (paints the
  ▲/▼), `ease_lanes` (the per-screen text column), and module-local tests.
- `src/git_history/scope.rs`: `Scope`, `resolve` (scope → `git log`
  revisions and header label, with the deleted-branch fallback),
  `branches` (the dropdown's grouped branch list), and `fingerprint` /
  `new_commits` for the pill.
- `src/git_history/details.rs`: `DetailsView`, cancellable commit queries, and
  changed-file parsing.
- `src/git_history/file_tree.rs`: `FileTree`, the virtualized status-colored
  tree (with sections) shared by the details pane and Git Changes: the
  multi-select model, checkboxes (`grouped`, `toggle_checks`), flat vs
  directory grouping (`set_flat`), keyboard, the `MenuItem` / `TreeEvent`
  menu seam, and `status_color`.
- `src/git_history/changes.rs`: `ChangesView`, the `git status` porcelain v2
  parser (`combined` for the one-letter status), the path-keyed checks and
  `scope` (what Commit takes), the tree's context menu (`menu`, and `write`
  for its items), the Rollback / Delete `Confirm` dialog, the header
  toolbar, and the watch-driven refresh (refresh-on-activate as fallback).
- `src/git_history/commit.rs`: `CommitPanel` (message box, Amend, Commit /
  Commit and Push…, AI message), the `Write` ops and their worker `run`
  (`--only` commits, the merge case, Recycle Bin, `.gitignore` patterns),
  and the AI prompt / `clean_message`.
- `src/git_history/push.rs`: `Target` and `target` (where a push goes),
  `push` (the write, with the rejection text), `PushDialog` and `outgoing`
  (target and
  outgoing commits).
- `src/git_history/watch.rs`: `RepoWatch`, the registry and `open`, the
  pure `classify` / `Debounce` core, the `ReadDirectoryChangesW` thread, and
  `Follow` (a view's lazily opened handle on the watch).
- `src/git_history/git.rs`: the shared Git subprocess helper: reads (spawn,
  capped drains, cancel/timeout watchdog) used by the history stream,
  details, diff, and status, and `write` for the Changes window's writes.
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
