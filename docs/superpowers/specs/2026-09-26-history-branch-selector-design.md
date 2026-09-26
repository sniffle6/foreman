# Git History: branch selector, per-screen text column, collapsed long edges — design spec

2026-09-26. Extends the Git History timeline (`docs/git-history.md`). The
decision to stream with a bounded lookahead instead of loading the whole
graph first is ADR 0004 (`docs/adr/0004-history-graph-streams-with-bounded-lookahead.md`).
This file is the *why*; the how-to goes into `docs/git-history.md` when each
phase ships.

## Problem

In a repo with many live branches the timeline gets too wide to read:

1. **Everything is loaded.** `stream_history` runs `git log --all`, which pulls
   in every local and remote branch, every tag, `refs/stash`, and any other
   tool's private refs (`epic-manager` carries dozens of `refs/em-loop/*`). The
   header just says "All branches" and there is no way to narrow it.
2. **The graph width is the global max.** `HistoryView` keeps the widest lane
   count of any loaded row and reserves that much graph room on *every* row, so
   one busy stretch pushes every subject in the history to the right.
3. **Long edges hold a lane the whole way.** `Graph::push` keeps a frontier
   slot open until the parent arrives, so a branch forked 300 commits ago
   holds a column across all 300 rows.

The goal: the timeline stays readable in agent-heavy repos without giving up
the hard "fast" requirement — streaming stays demand-driven in `BATCH` pages
and painting stays viewport-only.

The look must stay simple, modern and light, and match the foreman theme: no
new widget kinds, no checkbox grids, theme colors only.

## Phases

- **Phase 1** — branch selector (§1) and per-screen text column (§2). Ships
  on its own.
- **Phase 2** — collapsed long edges (§3). Designed here, built after phase 1
  lands. It changes the layout seam, so it gets its own plan.

## §1 Branch selector

### Look

The static "All branches" header label becomes a compact themed
`egui::ComboBox`, built the way the kanban board's version dropdown is
(`ViewScale::popup_style()` for the popup, `selectable_label` rows). The theme
already styles combo popups, so it inherits foreman's look.

The closed combo shows only the current choice: `main ▾`,
`Local branches ▾`, `All ▾`, `fix/foo ▾` (one picked branch), or
`Detached HEAD ▾`.

The popup:

```
  main → origin/main        ✓
  Local branches
  All
  ───────────────
  [ filter…      ]          ← only when there are more than 8 branches
  LOCAL
    main
    fix/foo
  CARDS
    card/a1
  REMOTE
    origin/main
```

- Scope rows and branch rows are plain `selectable_label`s. The current
  choice shows a trailing ✓.
- Group headings (`LOCAL`, `CARDS`, `REMOTE`) are small, dim, uppercase theme
  text — not widgets.
- Every row is a single pick: clicking it selects that scope or branch and
  closes the popup, exactly like the board's version dropdown.
- The filter is a plain single-line text field; it narrows branch rows by
  substring, case-insensitive. Group headings with no matching rows hide.

### Scopes

| Scope | `git log` revisions |
|---|---|
| **Current** (default) | `HEAD` plus its upstream when one exists |
| **Local branches** | `HEAD --branches` |
| **All** | `HEAD --branches --remotes --tags` |

`HEAD` is in every scope except One branch so a detached checkout's commits
never vanish (`--all` walked `HEAD` too). An unborn `HEAD` (an empty repo) is
left out, since `git log` fails on it; with nothing left to walk, the timeline
shows "No commits yet."
| **One branch** | the picked ref, by full refname (`refs/heads/…` or `refs/remotes/…`) |

- **Current** resolves the upstream with `git rev-parse --symbolic-full-name
  @{upstream}` before the log starts (a full refname, so it cannot collide
  with a same-named local branch); failure means "no upstream" and the scope
  is `HEAD` alone. The label is the short branch name,
  or `Detached HEAD` when `HEAD` is detached.
- **All** deliberately differs from `--all`: it drops `refs/stash` (stash
  commits are 2–3-parent merges that draw as fake branches) and non-standard
  namespaces such as `refs/em-loop/*` (another tool's bookkeeping). Tags stay:
  a tag on a commit no branch reaches (an old release) is still history the
  human asked for.
- `card/*` branches are ordinary local branches. They are in Local and All,
  and have their own `CARDS` heading only so the `LOCAL` list stays short.
  They are never hidden by default: in-flight agent work is what foreman
  exists to show, and card teardown deletes the branch anyway, so they do not
  pile up. "My branch plus every in-flight card" is the Local scope.
- Tags are not in the picker. They still draw as ref chips on their commits.

### Branch list

- `git for-each-ref --format=%(refname) refs/heads refs/remotes` runs on a
  worker thread each time the popup opens. It takes milliseconds, so the list
  is always fresh and there is no watcher. `refs/remotes/*/HEAD` symrefs are
  skipped.
- Until the worker answers, the popup shows the three scope rows and a
  spinner. A `git` failure shows one dim line in place of the groups; the
  scope rows still work.

### Changing scope

- A new scope restarts the stream through the existing Refresh path, which
  retires the old `HistoryView` state off the GUI thread. The restart keeps
  the scope, the details pane width, and the selected commit.
- The details pane keeps showing the selected commit even when the new scope
  does not contain it: it is still a valid commit. Its row is highlighted if
  it streams in.
- The list scrolls to the top.
- If the picked branch is deleted while shown (a torn-down card), the next
  Refresh finds nothing to log. `stream_history` checks the ref with
  `git rev-parse --verify` before starting; if it is gone, the scope falls
  back to Current, so a dead refname never reaches `git log`.

### Rejected

- **Multi-pick** (toggle rows with the popup open, a `CARDS` "all" button, a
  Picked scope with fallback rules). Its main use, "my branch plus the
  cards", is already the Local scope; the rest was most of phase 1's state
  and fought `ComboBox`, which closes on every click. Add it if someone asks.

### Persistence

None. The scope survives Refresh but not closing the History window or
restarting foreman; every new History window opens on Current. Current is
designed to be the right answer most of the time. If re-picking turns out
to be annoying, persisting the choice per project is an add-on.

### Seam

A pure function maps a scope (plus the resolved upstream or picked ref) to
the revision arguments for `git log`. It has table tests. `stream_history`
takes those arguments instead of a hardcoded `--all`; the worker loop,
paging, and `Graph` are otherwise untouched in phase 1.

## §2 Per-screen text column

### Behavior

Subjects start right after the widest graph **currently on screen**, not the
widest graph ever loaded. The global width field on `HistoryView` goes away.

- Inside the `show_rows` closure the visible `range` is known. Before
  painting, `target` = the max `Row::width` over that range.
- `HistoryView` keeps the displayed width as a fractional lane count. Each
  frame a pure `ease_lanes(current, target, dt) -> f32` updates it:
  - `target > current`: jump to `target` at once. Growth is instant so new
    lanes never paint under text.
  - `target < current`: move down at a rate that covers the whole gap in
    150 ms, never below `target`. While it is moving, request a repaint; once
    it settles, no extra frames.
- The graph room is `(lanes + 1) * lane_w`. The ref chip position, the
  subject's left edge, the `columns()` clip, and the horizontal-scroll width
  (`total_w`) all follow from it. The row-allocation and `total_w` math move
  inside the `show_rows` closure because they now depend on the range.
- Scope change and Refresh reset the displayed width to 1 lane. Zoom scales
  `lane_w` as today; the lane count is unaffected.

**Invariant:** the displayed width is always ≥ every visible row's width, so
graph lines never overlap text, even mid-ease.

### Rejected

- **Per-row offset** (each subject starts after its own row's graph). It loses
  the aligned left edge that makes a column of subjects scannable, and in a
  busy stretch every row sits at a different x.
- **Snap both ways.** Scrolling through a busy region visibly jitters.
- **Never shrink until scrolling stops.** After passing a wide stretch the
  whole screen keeps a gap until the wheel stops.

## §3 Collapsed long edges (phase 2)

### Behavior

This matches JetBrains' Git log (`PrintElementGeneratorImpl` in
`intellij-community`: `LONG_EDGE_SIZE = 30`, `LONG_EDGE_PART_SIZE = 1`).

- An edge from a child at row *i* to its parent at row *j* is **long** when
  *j − i* > `LONG` (30), or when the parent never arrives (for example a
  shallow-clone boundary). Short edges draw exactly as today.
- A long edge draws as two stubs in its own color; the lane between them is
  free for other branches:
  - **Down-stub:** leaves the child, runs `STUB` (1) row, ends in a ▼.
  - **Up-stub:** starts with a ▲ `STUB` rows above the parent and runs into
    it.
- **Parent already live:** if a nearer child joins the parent with a short
  edge, the parent has a live lane anyway; the long edge resolves into it and
  there is no up-stub.
- **Several long children, one parent:** one up-stub. Its ▲ jumps to the
  nearest of those children.
- Parent never arrives: down-stub only. The parked slot is dropped at end of
  stream, so it does not leak.

### How it streams

The worker's layout runs `LONG` rows behind its reader. Commits wait in a
`VecDeque` with a hash→row map covering only that window. When row *i* is
laid out, rows up to *i + LONG* are already read:

- Parent inside the window → short edge, handled as today.
- Parent outside the window → long edge: emit the down-stub and **park** the
  parent's frontier slot. A parked slot keeps its color and its nearest long
  child's row, but holds no lane.
- When a parked parent comes within `STUB` rows, its slot gets a lane again,
  marked with a ▲.
- A later child reaching a parked parent with a short edge un-parks it at
  that child's row, with no arrow.
- At end of stream the window drains; pages are still `BATCH` rows each.

There is no up-front `rev-list`, no delay before the first row, and rows
already drawn are never laid out again. The whole cost is buffering `LONG`
commits.

### Seam

- `Graph` gains the lookahead front end and the parked state. `LONG` and
  `STUB` become `Graph` fields (30 and 1 in production) so tests can use tiny
  values.
- `Graph` numbers rows globally (across pages) so a ▲ can name its target row.
- `Row` gains `arrows: Vec<Arrow>`; an `Arrow` has a lane, a direction, and a
  jump target:
  - ▲ → the child's row index (already loaded).
  - ▼ → the parent's hash. Its row is unknown when the stub is laid out,
    because the parent is past the window by definition.
- `Row::width` counts live lanes and stubs only, so §2 narrows further with
  no change of its own.

### Clicking an arrow

- Hovering an arrow shows a pointing-hand cursor and a tooltip:
  "Jump to parent `abc1234`" or "Jump to child `def5678` — subject".
- ▲ click: scroll that row into view and select it (the details pane opens
  it).
- ▼ click: open the parent's hash in the details pane. The list does not
  scroll: the parent may be pages away and not loaded yet. The details pane
  already shows commits that are not in the list, so this adds no state.
- The arrow hit rect is the stub's cell; clicks elsewhere on the row still
  select the row.

### Rejected

- **Load all parents up front** (`git rev-list --parents`, then lay out the
  whole graph, as JetBrains does). It makes ▼ jumps trivial, but delays the
  first row by a full walk of the repo and holds every hash in memory. See
  ADR 0004.
- **Re-lay-out rows when a parent lands.** Rows already painted would move
  under the reader, and pages stop being immutable.
- **▼ seeks to the parent** (keep loading pages until the parent arrives,
  then scroll). A new state machine with four exits (found, end of stream,
  user scroll, Refresh / scope change), each a race to get wrong, for a jump
  the details pane already answers.
- **A "show long edges" toggle** (JetBrains has one). Nobody has asked; add it
  if someone does.
- **Collapsing linear runs, dimming off-branch commits.** Out of scope.

### Phase 2 shipped lean (2026-09-26)

Phase 2 shipped without arrow jumps. What shipped: long edges (over 30 rows)
as colored ▼/▲ stubs, parked parents that keep one color and no lane, one ▲
per parent however many long children it has, no ▲ when a near child revives
the parent, and the 30-commit lookahead inside `Graph`. Deferred: row-index
or hash jump targets on arrows, arrow tooltips and clicks, and arrow-click UI
tests. The design above stays as the record of what a jump would take.
`docs/git-history.md` explains how the shipped version works.

## Testing

Phase 1:

- Table test: scope → `git log` revision arguments, including detached HEAD
  and no-upstream.
- Real-repo worker test: Current excludes a sibling branch; All excludes
  `refs/stash` and a custom `refs/x/*` ref; One branch shows only that
  branch's history; a deleted picked branch falls back to Current.
- Headless UI test: open the combo, click a row, assert the stream restarted
  with the new scope and the selected commit survived. Record row rects the
  way the board's dropdown test does rather than guessing popup placement.
- Table test for `ease_lanes`: instant growth; monotonic shrink that never
  undershoots; reaches the target after 150 ms of accumulated `dt`; steady
  state is a fixed point.
- Headless UI test: history narrow at the top with one wide stretch deeper
  down — subjects start at the narrow x at the top, further right at the wide
  stretch, and back at the narrow x once the ease settles.
- `large_history_paints_only_viewport_rows_and_scrolls_to_old_commits` keeps
  passing (painting is still viewport-only).
- Screenshot of `epic-manager` on Current and on All.

Phase 2 (table tests on `Graph` with small `LONG` / `STUB`):

- Every existing graph test passes unchanged: short edges draw as today.
- A long edge frees its lane between the stubs: the width in between equals
  the baseline, ▼ and ▲ sit on the right rows, colors match.
- Long child plus short child to one parent: no ▲; the edge merges into the
  live lane.
- Two long children: one ▲, jumping to the nearer child.
- Missing parent: down-stub only; no parked slot survives end of stream.
- A merge whose second parent is long.
- **Page-split independence:** the same history fed with a page boundary at
  every offset produces identical rows. This is the property that proves the
  lookahead did not break streaming.
- `demand_batches_keep_graph_continuity_and_stop_when_view_closes` keeps
  passing.
- Headless UI test: click a ▼ whose parent is two pages away → the details
  pane shows the parent; no extra pages were requested. Click a ▲ → its
  child row is selected and scrolled into view.
- Screenshot of `epic-manager` on All, before and after.

## Out of scope

- Persisting the scope across restarts.
- A "show long edges" toggle.
- Collapsing linear runs; dimming commits off the selected branch.
- Watching refs for changes while the History window is open (Refresh and
  reopening the popup are the refresh paths).
