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
  npm-installed Codex launches through Node and its package entry point, preserving
  multiline prompts and quotes without passing them through a command shell.
  Other command shims that cannot accept the multiline card prompt report that
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
  had at dispatch). The prompt's close-out hands the finished branch to the
  repository's **integration queue** (`foreman kanban integrate`), which
  rebases, checks, fast-forwards, and marks the card Done — the worker
  never merges into the main checkout and never runs `done` on a worktree
  card; see `docs/integration-queue.md`. `done`, board Release, and `rm` queue a non-forcing teardown
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
- **Worktrees page**: every worktree foreman made for the project, in one
  place. While at least one exists, the board shows a one-line strip along
  its bottom (`N worktrees · D dirty · S no card`, zero segments omitted,
  danger colour when D or S is non-zero); a project without any sees no
  change. Clicking the strip swaps the Worktrees page in for the columns,
  like the card detail page, with Back to board. Source of truth is
  `git worktree list` filtered to `<root>/.foreman/worktrees/`, matched by
  path to each card's stored worktree. A tree no card points at is a
  **stray** (`no card`): the card was removed while its teardown could not
  run, its file was deleted by hand, or the current branch does not carry
  it (cards travel with the clone). Hand-made trees elsewhere are not
  listed. Rows are card-owned first (Backlog, In Progress, Blocked, Done,
  store order within a state), then strays by path; each shows the branch
  (path on hover), the owner (card id, state, Version, title, or `no card`
  plus the directory name), and the same counts, flags, and colour rules as
  the card face. Card rows navigate only — Open card, and Open terminal
  when the claim is live; Back from that detail page returns to the
  Worktrees page, and Discard stays on the card. Stray rows clean up:
  **Remove** queues the non-forcing teardown (git refuses a dirty or
  unmerged tree and the toast says so), **Discard** the forcing one behind
  the standard confirm. Stray status is measured against whatever branch
  the main checkout has at poll time (`HEAD` when detached), since no card
  remembers a base. Strays are found by the same status poll as card
  status, so a hidden board lists nothing and the poll now runs on a shown
  board even when no card has a worktree.
- **Cut and Versions**: Done is the live pile until you Cut it. Cut (a
  button on the Done header, or `foreman kanban cut <name>`) stamps every
  ungrouped Done card with `shipped` (`{name, at, commits}`), which moves
  them out of Current into a named Version; `state` stays `done`. The Done
  header's dropdown switches between Current and any Version, newest Cut
  first; a Version shows an `Archived · <name>` banner with Uncut, hides
  Cut, and leaves the other three columns live. A Done card whose kept
  worktree is dirty, is ahead of base, or cannot be probed at all is not
  provably in the tip, so Cut leaves it in Current and says so; commit and
  merge it, or Discard it, and it goes into the next Cut.
  Duplicate names (case-insensitive) and `Current` are refused. The
  selection is view state and resets to Current on restart. Why this shape
  and what was rejected: `docs/superpowers/specs/2026-09-16-kanban-cut-design.md`.
- **Cut is the release.** The board's Cut (and `foreman kanban cut vX.Y.Z
  --release`) stamps the cards as above, then runs the documented release
  procedure on a background thread (`src/release.rs`): checks, commit the
  card files as `chore(kanban): cut vX.Y.Z`, bump `Cargo.toml` (and the
  package's `Cargo.lock` entry) as `chore(release): bump version to X.Y.Z`,
  push the branch, `git tag vX.Y.Z`, push the tag — which fires
  `.github/workflows/release.yml`. The checks all run before any write:
  on origin's default branch, nothing dirty outside `.foreman/tasks/`, not
  behind origin after a fetch, the tag free locally and on origin, and the
  name strictly `vX.Y.Z` and newer than `Cargo.toml`'s version (else the
  newest `v*` tag). A `Cargo.toml` already bumped by hand to the requested
  version (untagged, newer than the newest `v*` tag) skips the bump instead
  of refusing. No `Cargo.toml` at the repo root means no bump step: the
  release is tag-only. The Cut field prefills `Cargo.toml`'s version when it
  is not yet tagged, else the next patch past it and the newest tag. While
  it runs, the Done column shows one line per step in the Cut field's row
  and Cut is disabled; success shows the GitHub Actions link, failure shows
  git's error. A failure before any commit returns the cards to Current (an
  automatic `uncut`); after a commit everything stays, and the list shows
  the one command that finishes by hand (e.g. `git push origin v0.5.1`).
  Nothing is ever undone in git. ✕ dismisses a finished list. Plain
  `foreman kanban cut <name>` stays record-only.
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
- **Plans order cards without a second store**: a card can carry a plan name
  and a wave number (`kanban edit --plan/--wave`). A "plan" is then *derived*
  from the cards the same way a Version is — group by folded name, then by
  wave. The Plan view window reads it; nothing schedules or dispatches from
  it. Design and the rejected alternatives:
  `docs/superpowers/specs/2026-09-22-plan-view-design.md`.
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

**Plan view**: leader then `L` (`Command::OpenPlan`) opens the project's plan
view — one per project, same singleton rule as the board, and persisted as a
unit `ContentSnap::Plan` (there is nothing to snapshot but the fact it was
open). It is read-only: plans stack newest-activity-first, each showing its
waves ascending, and only the **current wave** — the lowest wave still
holding a non-Done card — is expanded by default. Click a wave header to
open or shut it; click a card to jump to that card's detail page on the
board, opening the board window if it is not already up. A plan holding a
single card is drawn dimmed, labelled `1 card`, with no wave header at all
— see the gotcha about plan-name typos below. Expanded/collapsed waves and
scroll are view state, not persisted.

**CLI** (inside a foreman terminal, address it as `& $env:FOREMAN_EXE`; the
installed exe is also on PATH as `foreman`):

```
foreman kanban add "fix caret flicker" --body "repros on resize; see wm.rs"
foreman kanban edit <id> [--title T] [--body B] [--plan NAME] [--wave N]
                                          # at least one of the four; --plan "" clears
foreman kanban list [--state ...] [--shipped NAME] [--all] [--json]   # bare = live board
foreman kanban start <id>                 # claim a card yourself
foreman kanban done <id>                  # close out: In Progress -> Done
foreman kanban block <id> --reason "..."  # close out: needs a human
foreman kanban rm <id>                    # delete the card file, any state
foreman kanban cut <name>                 # record only: Done -> Version <name>
foreman kanban cut vX.Y.Z --release       # the board's Cut: record + release (replies at once)
foreman kanban uncut <name>               # Version <name> -> Current Done
foreman kanban wait <id> | --any [--timeout SECS]
foreman kanban worktrees [--stray] [--json]   # every foreman worktree, probed live
foreman kanban integrate <id> [--cancel] [--json]   # submit a worktree card to the queue
```

`wait` exit codes: `0` Done, `1` Blocked / orphaned / removed (needs a
human), `2` timeout or foreman unreachable, `3` (`wait <id>` only) its
integration needs resolution. `foreman kanban --help` is ground
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
cards without changing it. `edit` replaces title, body, and/or plan
membership in any state and does not claim or move the card; body is a full
replace, not an append.
`--plan` alone starts the card at wave 1, `--wave` alone renumbers a card
that already has a plan (and errors on one that does not), and `--plan ""`
clears the plan. Plans read back through `list --json` as the card's
`planned` object; there is no read verb and no new `cmd` on the wire.
A worktree card's `done` is additionally refused while its integration
request is queued or integrating, and while its branch has commits not on
its base (`docs/integration-queue.md`).

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
- **`git worktree remove` is not atomic, so teardown judges what is left.**
  When the cwd hold above bites mid-removal, git has already deleted the
  tree's contents and its registration and only the empty top directory
  survives — retrying git on it fails forever ("not a working tree").
  Teardown therefore checks registration and the directory separately: a
  registered tree goes through git; an unregistered leftover is removed only
  if it is empty and sits under `<root>/.foreman/worktrees/`; a tree or
  branch that is already gone counts as that step done, so repeating a
  teardown is harmless. A nonempty unregistered directory is never deleted
  (git no longer tracks it, so nothing in it is provably ours) — the error
  toast names it; inspect and delete it by hand.
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
- **`kanban list` never shows a stray.** It knows cards. `kanban worktrees`
  is the whole picture: it lists git's foreman trees plus any tree a card
  still points at, probing each on the request (synchronous git, like `rm`
  and `cut`), so its counts are live where the board's are the last poll
  round. `--stray` keeps the cardless ones; `--json` is one object per line
  with `card` and `status` absent when there is none. Outside a git
  repository the verb errors. Cleanup of a stray stays board-only.
- **A stray's Remove or Discard clicked twice is one teardown.** The queue
  keeps one entry per directory name; a second click while the first runs
  waits its turn and then finds nothing to remove.
- **Ignore is per-clone.** Bring-up appends `.foreman/worktrees/` to
  `.git/info/exclude`, never to `.gitignore`.
- **Cut on the branch you ship from.** A Cut rewrites every ungrouped Done
  card file at once; two branches each cutting overlapping cards conflict
  per file on merge, like any two transitions on one card would.
- **A release refuses a dirty tree, but not your card files.** The stamp
  itself rewrites `.foreman/tasks/`, so changes there are expected and go
  into the cut commit; anything else uncommitted refuses the release (and
  the cards return to Current). Pushes never prompt for credentials — a
  push that needs a prompt fails with git's message instead of hanging.
- **A record-only Cut with no git, or before any trailer commit, still
  ships.** It just records no commits. The trailer is a convention the prompt teaches, not a
  gate `done` enforces.
- **Two spellings of a plan name are two plans, silently.** `same_name`
  folds case and outer whitespace and nothing else, so `Terminal work` and
  `terminal-work` are different plans and the second one renders as a
  perfectly convincing plan of its own. There is deliberately no validation
  and no name registry — that would be the second store the design exists to
  avoid. The plan view's defence is visual: a one-card plan is dimmed and
  labelled `1 card`. If you see one you did not mean, fix the card's
  `--plan`, copying the name off a card already in the plan.
- **A card is in at most one plan, by construction.** `planned` is one
  optional field, not a list. Re-running `--plan` moves the card; it never
  adds a second membership.
- **Plans are derived per frame, never stored.** Nothing lives in
  `.foreman/plans/`; deleting a card removes it from its plan, and the last
  card leaving a plan makes the plan cease to exist. Waves need not be
  contiguous — a gap is just a gap.
- **Bare `list` hides shipped cards.** Scripts that dumped every card need
  `--all`; `--state done` is Current Done only.

## Key files

- `src/integrate.rs` — the repository integration queue (own doc:
  `docs/integration-queue.md`).
- `src/kanban.rs` — the pure domain: `Card`/`Claim`/`CardState`, `CardStore`
  (file-per-card load/save, transition verbs including `CardStore::edit`,
  staleness poll), `claim_is_dead`
  / `is_orphaned` (derived orphan rule), `run_nonce`, `dispatch_prompt` +
  `CloseoutStyle`, `CardLine`, `wait_verdict`; the worktree half:
  `Worktree` / `WorktreeStatus`, `worktree_layout`, `worktree_summary`,
  `bring_up_worktree`, `worktree_status_now`, `teardown_worktree` (+
  `remove_tree` / `delete_branch`, the retry-safe steps) + `teardown_verdict`,
  `CardStore::take_status_poll`; the overview half:
  `parse_worktree_list`, `foreman_worktrees_now`, `strays_among`,
  `StrayWorktree`, `worktree_rows` / `WorktreeRow` / `RowOwner`,
  `worktree_strip`, `CardStore::strays`, `live_worktree_rows`,
  `WorktreeLine` (the `kanban worktrees` line); the Cut half: `Shipped`,
  `same_name`, `versions`, `CardStore::cut` / `uncut` (batch write, revert),
  `parse_trailer_log` + `trailer_commits`, `latest_v_tag`; the plan half:
  `Planned` (the card field), `plans` / `Plan` / `Wave` / `PlanCard` (the
  derivation and `Plan::current`).
- `src/release.rs` — Cut's release: `run` (checks, commit, bump, push, tag,
  push tag; sync so tests drive it) and `spawn` (the thread), the
  `ReleaseEvent` stream and the board's folded `Progress`, plus the pure
  helpers: `parse_tag` / `parse_version`, `bump_cargo_toml` /
  `bump_cargo_lock`, `prefill` (untagged Cargo version, else next patch), `actions_url`.
- `src/plan_view.rs` — `PlanView` (the plan window's content) and `PlanAct`
  (its one intent, `OpenCard`).
- `src/board.rs` — `BoardView` (the window content) and `BoardAct` (the
  intents it records for the manager to drain), including the card-face
  worktree line and the detail page's Discard action, the Done header's
  version dropdown and Cut field, the release step list (`show_release`),
  the archive banner, the worktree strip
  (`show_strip`) and the Worktrees page (`show_worktrees`,
  `show_worktree_row`, the `RemoveStray` / `DiscardStray` acts).
- `src/wm.rs` — the seams: `kanban_tick` (per-frame orphan recompute + gated
  reload), `kanban_dispatch` (the control-pipe verb table), `drain_board_acts`
  (applies board intents: store writes, jump-to-terminal, dispatch-from-card
  with bring-up), `drain_worktree_msgs` (queued teardowns, thread results,
  status poll kick including the stray listing), `CloseTarget::DiscardStray`,
  `kanban_rm` (the `rm` pre-check), `open_board_window` /
  `open_plan_window` (per-project singletons), `drain_plan_acts` (opens or
  focuses the board and points it at the clicked card), `term_states`, `kanban_cut` (the hold-back probe
  and trailer walk injected into the store), `cut_and_release` (stamp, then
  spawn the release; one at a time) and `drain_release` (events → progress
  → board views, uncut on an uncommitted failure, the outcome toast).
- `src/config.rs` — `Settings::dispatch_worktrees`.
- `src/control.rs` — `KanbanRequest` (the wire shape), `parse_kanban_args`,
  `parse_kanban_edit`, `kanban_main`, `kanban_wait` (client-side poll loop),
  `HELP_KANBAN`.
- `src/workspace.rs` — `ContentSnap::Board` and `ContentSnap::Plan`
  persistence variants.
- `src/keymap.rs` — `Command::OpenBoard` / `Command::OpenPlan` and their
  default bindings (`K` and `L`).
- `src/skills_install.rs` — embeds the foreman-kanban skill (Claude + Codex
  twins).
