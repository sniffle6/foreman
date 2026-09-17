# Kanban board

Per-project work board: Backlog / In Progress / Blocked / Done. Cards are
plain JSON files in the project's `.foreman/tasks/` directory; the app is the
single writer and validates every transition. Agents drive cards over the
`foreman kanban` CLI; humans use the board window. Decision history (why
file-per-card, why derived orphans, rejected alternatives) lives in
`docs/superpowers/specs/2026-08-28-kanban-board-design.md` and its brainstorm
sibling — read those for *why*, this doc for *how*.

## What it does

- **Cards** are one unit of work-in-flight each: title, optional body, state,
  and (while claimed) a claim linking the card to the Session working it.
- **Dispatch from a card**: Start on a Backlog or Blocked card offers an agent
  picker. Picking one spawns a new Session in the
  project cwd whose prompt embeds the card body and the exact close-out
  commands, then claims the card and moves it to In Progress in the same
  action — a card-spawned agent never runs `start` itself.
  Failed board actions show an error toast. A failed spawn leaves the card
  unchanged; if claiming fails after spawning, Foreman closes the new Session.
  Command shims that cannot accept the multiline card prompt report that
  limitation and suggest installing a native executable.
- **Per-card worktrees**: the choice is made per dispatch. The inline agent
  picker carries a `wt on/off` chip and the detail page a checkbox, both
  seeded from `dispatch_worktrees` (default on; Agents pane) and reset per
  card, so one card's override never leaks onto the next. A card that already
  has a worktree hides the toggle and always restarts in it. There is no CLI
  flag: the choice lives on the board. With the toggle on, Start creates
  `<repo>/.foreman/worktrees/<id>` on branch `card/<id>`
  and spawns the worker there, so no two workers share a checkout. The card
  records `worktree` (path, branch, and `base` — the branch the main checkout
  had at dispatch). The prompt's Workspace section tells the worker to
  `git rebase <base>` and fast-forward `<base>` from the worktree before
  `done`. `done`, board Release, and `rm` queue a non-forcing teardown
  (`worktree remove`, `branch -d`, `prune`) that runs on a background thread
  once the worker's terminal is gone; a dirty tree or an unmerged branch is
  kept and the card says so. `rm` refuses outright while the tree is dirty or
  ahead of base, and also when git cannot answer (fail closed: an
  uninspectable worktree is never deleted). Re-dispatching a released card
  cancels its still-queued teardown and refuses while one is mid-removal.
  `block` and orphaned cards keep the tree so Restart resumes in it. Outside a git repository dispatch runs in place silently; on a
  detached HEAD it runs in place with a warning toast. **Discard worktree** on
  a Done, Blocked, or orphaned card's detail page is the only forcing path and
  is human-only (no wire verb), behind the standard confirm. Why this shape
  and what was rejected: `docs/superpowers/specs/2026-09-15-dispatch-worktrees-design.md`.
- **Worktree status is derived**: while a board is shown, every worktree card
  is probed every few seconds on a background thread (dirty, ahead, behind,
  missing) and the result is shown on the card face and by
  `foreman kanban list` (`[wt card/<id> +A -B dirty]`; `--json` adds
  `worktree` and `worktree_status`). Nothing about status is written to a
  card file; a hidden board polls nothing.
- **Cut and Versions**: Done is the live pile until you Cut it. Cut (a
  button on the Done header, or `foreman kanban cut <name>`) stamps every
  ungrouped Done card with `shipped` (`{name, at, commits}`), which moves
  them out of Current into a named Version; `state` stays `done`. The Done
  header's dropdown switches between Current and any Version, newest Cut
  first; a Version shows an `Archived · <name>` banner with Uncut, hides
  Cut, and leaves the other three columns live. A Done card whose kept
  worktree is still ahead of base is not in the tip, so Cut leaves it in
  Current and says so; merge or Discard it and it goes into the next Cut.
  Duplicate names (case-insensitive) and `Current` are refused. The
  selection is view state and resets to Current on restart. Why this shape
  and what was rejected: `docs/superpowers/specs/2026-09-16-kanban-cut-design.md`.
- **Commits attach at Cut through the card trailer.** Every dispatch prompt
  tells the worker to end each commit message with `Card: <id>`. Cut walks
  `git log` once (bounded by the oldest card's creation date) and stores
  each card's trailer commits in `shipped.commits`, shown on the detail
  page and in `list --json`. A card whose commits lack the trailer ships
  with none; nothing is ever refreshed after Cut.
- **Derived orphan detection**: a card is orphaned when it is In Progress but
  its claim no longer checks out — wrong app run, or the claimed terminal is
  gone or exited. Orphan state is recomputed every frame and exists nowhere in
  the card files. Restart, crash, and branch-switch reconciliation all fall
  out of this rule with zero file writes.
- **`wait` gives orchestrators a synchronous primitive**: block until a card
  (or any watched card) leaves In Progress, with exit codes scripts can
  branch on.

## How to use it

**Board window**: leader then `K` (`Command::OpenBoard`) opens the project's
board — one per project; reopening surfaces the existing one. Quick-add at the
top of Backlog creates title-only cards. Click a column header to collapse it
into a rail with its name and count; click the rail to expand it. Expanded
columns share the remaining width. Collapse and scroll positions belong to
the open board view and are not persisted across app restarts.

Cards have a stable action footer: Start or Restart opens the agent picker;
Open terminal surfaces a live claimed Session. Click the card body or its
ellipsis to read the full stored task in a scrollable detail page inside the
board pane. The detail page preserves multiline text and offers web links,
claim information, and timestamps, with task actions below. Back to board
preserves column collapse and scroll positions. A card removed while its
details are open returns the view to the board. Description and blocker text
are existing card fields, not a separate notes or attachments store.

Card surfaces, text, attention indicators, and controls use the shared theme
tokens and Visuals bridge; there is no board-specific palette. There is
no separate board font-size setting: text follows the shared terminal font
size, with proportional card heights, spacing, and button hit areas. Changes
apply to the open board and its detail page on the next frame. There is
deliberately no block button — blocking demands a typed reason, so it is the
CLI's move.

**CLI** (inside a foreman terminal, address it as `& $env:FOREMAN_EXE`; the
installed exe is also on PATH as `foreman`):

```
foreman kanban add "fix caret flicker" --body "repros on resize; see wm.rs"
foreman kanban list [--state ...] [--shipped NAME] [--all] [--json]   # bare = live board
foreman kanban start <id>                 # claim a card yourself
foreman kanban done <id>                  # close out: In Progress -> Done
foreman kanban block <id> --reason "..."  # close out: needs a human
foreman kanban rm <id>                    # delete the card file, any state
foreman kanban cut <name>                 # ship ritual: Done -> Version <name>
foreman kanban uncut <name>               # Version <name> -> Current Done
foreman kanban wait <id> | --any [--timeout SECS]
```

`wait` exit codes: `0` Done, `1` Blocked / orphaned / removed (needs a
human), `2` timeout or foreman unreachable. `foreman kanban --help` is ground
truth for flags. Agents not spawned from a card learn all of this from the
embedded **foreman-kanban** skill.

Host errors also exit `2`. Busy and no-response replies retry until the wait
deadline; other host errors exit immediately. Only card-state verdicts produce
exit `1`, so temporary host load does not masquerade as a blocked Card.

**Transitions** are enforced identically for CLI and board: claims move
Backlog/Blocked cards to In Progress; `start` on a card with a live claim is
rejected (the two-agents-one-card guard) but seizes a dead one; `done`/`block`
only from In Progress; release (board-only) returns In Progress or Blocked to
Backlog; Done is terminal for state; Cut and Uncut group and ungroup Done
cards without changing it.

## Gotchas

- **Orphaned-ness is invisible in the JSON files.** It is derived at render
  time; only the board and `foreman kanban list` show it. Reading
  `.foreman/tasks/` by hand tells you the last written claim, not whether it
  is alive.
- **Close-out on a missing card errors, never creates.** A deleted card is not
  resurrected by its worker's `done`; the worker sees the error, the board
  simply lacks the card.
- **`.foreman/tasks/` travels with the clone.** Cards are repo files by
  design (they merge branch-to-branch); add the directory to a repo's
  `.gitignore` to opt out per-project.
- **The dispatch prompt's close-out commands vary by install.** An installed
  foreman renders plain `foreman kanban done <id>` (the installer puts the exe
  on PATH); a dev/debug build renders the `$env:FOREMAN_EXE` form, because
  `foreman` is not on PATH inside a dev fleet. See `closeout_style` in
  `src/kanban.rs`.
- **Editing the foreman-kanban skill requires a rebuild to propagate** — it is
  embedded via `src/skills_install.rs` like dispatch/chat/icat.
- **The staleness poll only runs while a board is visible.** External file
  changes (branch switch, pull) appear within seconds on a shown board; a
  hidden board catches up when next rendered. The app's own writes repaint
  immediately.
- **Teardown waits for the worker's pane to close.** On Windows a directory
  that is some process's cwd cannot be deleted, and the worker's shell sits
  in the worktree when it runs `done`. The teardown is queued and starts
  once the claiming terminal is no longer running — close the pane and the
  tree goes away. A Done card still showing its branch means the pane is
  still open (or the tree was kept: see the toast).
- **Each worktree cold-builds.** It has its own `target/`; the first build
  costs minutes (`docs/dev-launcher.md` forbids sharing a target dir). Minutes
  of compile beat corrupted commits.
- **The worktree carries a stale `.foreman/tasks/`.** Close-out reaches the
  project's board through the pipe, so the board is unaffected — but a worker
  that runs `git add -A` commits stale card files and the fast-forward
  carries them into base. The prompt says never to stage `.foreman/`; review
  for it anyway.
- **`kanban list` worktree status comes from the last poll round.** A board
  that has not been shown since the card was dispatched prints the branch
  with no counts.
- **Ignore is per-clone.** Bring-up appends `.foreman/worktrees/` to
  `.git/info/exclude`, never to `.gitignore`.
- **Cut on the branch you ship from.** A Cut rewrites every ungrouped Done
  card file at once; two branches each cutting overlapping cards conflict
  per file on merge, like any two transitions on one card would.
- **A Cut with no git, or before any trailer commit, still ships.** It just
  records no commits. The trailer is a convention the prompt teaches, not a
  gate `done` enforces.
- **Bare `list` hides shipped cards.** Scripts that dumped every card need
  `--all`; `--state done` is Current Done only.

## Key files

- `src/kanban.rs` — the pure domain: `Card`/`Claim`/`CardState`, `CardStore`
  (file-per-card load/save, transition verbs, staleness poll), `claim_is_dead`
  / `is_orphaned` (derived orphan rule), `run_nonce`, `dispatch_prompt` +
  `CloseoutStyle`, `CardLine`, `wait_verdict`; the worktree half:
  `Worktree` / `WorktreeStatus`, `worktree_layout`, `worktree_summary`,
  `bring_up_worktree`, `worktree_status_now`, `teardown_worktree` +
  `teardown_verdict`, `CardStore::take_status_poll`; the Cut half: `Shipped`,
  `same_name`, `versions`, `CardStore::cut` / `uncut` (batch write, revert),
  `parse_trailer_log` + `trailer_commits`, `latest_v_tag`.
- `src/board.rs` — `BoardView` (the window content) and `BoardAct` (the
  intents it records for the manager to drain), including the card-face
  worktree line and the detail page's Discard action, the Done header's
  version dropdown and Cut field, the archive banner.
- `src/wm.rs` — the seams: `kanban_tick` (per-frame orphan recompute + gated
  reload), `kanban_dispatch` (the control-pipe verb table), `drain_board_acts`
  (applies board intents: store writes, jump-to-terminal, dispatch-from-card
  with bring-up), `drain_worktree_msgs` (queued teardowns, thread results,
  status poll kick), `kanban_rm` (the `rm` pre-check), `open_board_window`
  (per-project singleton), `term_states`, `kanban_cut` (the hold-back probe
  and trailer walk injected into the store).
- `src/config.rs` — `Settings::dispatch_worktrees`.
- `src/control.rs` — `KanbanRequest` (the wire shape), `parse_kanban_args`,
  `kanban_main`, `kanban_wait` (client-side poll loop), `HELP_KANBAN`.
- `src/workspace.rs` — `ContentSnap::Board` persistence variant.
- `src/keymap.rs` — `Command::OpenBoard` and its default binding.
- `src/skills_install.rs` — embeds the foreman-kanban skill (Claude + Codex
  twins).
