---
name: foreman-kanban
description: Use when running inside foreman (the FOREMAN env var is 1) and coordinating work through the project's kanban board — picking up a card, dispatching a worker onto a card, orchestrating or running a plan (dispatching its cards wave by wave), creating cards, editing title/body, ordering cards into plans and waves, closing out with done/block, or waiting on workers.
---

# The foreman project kanban board

**This skill is complete. Do NOT read foreman source or docs to learn kanban
mechanics — every fact you need is below.** Researching your task's subject
matter is separate and fine. Precondition: `$env:FOREMAN` is `1`.

Address the CLI the same way as dispatch/chat:

    & $env:FOREMAN_EXE kanban <verb> ...     # PowerShell

    "$FOREMAN_EXE" kanban <verb> ...         # bash

`foreman kanban --help` is ground truth for flags — treat this skill as the
map, not the last word on syntax. Per-verb `--help` is not accepted.

## Verbs

    & $env:FOREMAN_EXE kanban add "fix caret flicker" --body "repros on resize; see wm.rs"
    & $env:FOREMAN_EXE kanban edit a3f8k2 --title "new title" --body "new body"
    & $env:FOREMAN_EXE kanban edit a3f8k2 --plan "Terminal work" --wave 2
    & $env:FOREMAN_EXE kanban list --state backlog
    & $env:FOREMAN_EXE kanban start a3f8k2
    & $env:FOREMAN_EXE kanban dispatch a3f8k2 --agent codex --worktree
    & $env:FOREMAN_EXE kanban done a3f8k2
    & $env:FOREMAN_EXE kanban block a3f8k2 --reason "needs a design decision"
    & $env:FOREMAN_EXE kanban rm a3f8k2
    & $env:FOREMAN_EXE kanban wait a3f8k2 --timeout 300
    & $env:FOREMAN_EXE kanban wait --any --timeout 300
    & $env:FOREMAN_EXE kanban list --shipped v0.5.0 --json
    & $env:FOREMAN_EXE kanban worktrees --stray --json
    & $env:FOREMAN_EXE kanban integrate a3f8k2
    & $env:FOREMAN_EXE kanban cut v0.5.0
    & $env:FOREMAN_EXE kanban uncut v0.5.0

- `add` — positional words join into the title; `--body` attaches a longer
  description. Reply carries the new card's id.
- `edit` — replace title, body, and/or plan membership after add. At least
  one of `--title`/`--body`/`--plan`/`--wave` is required; title is trimmed
  and must be non-empty; body replaces (does not append). Allowed in any
  state; does not claim or change state.
- `edit --plan NAME --wave N` — ordering, for the Plan view (leader `L`).
  A plan is derived from the cards, not stored: tag each card as you create
  it and the view groups them by plan, then by wave, lowest first. Names
  fold case and outer whitespace like Version names, so `Terminal work` and
  `terminal-work` are two different plans — copy the name from an existing
  card rather than retyping it. `--plan` alone starts at wave 1, `--wave`
  alone renumbers a card that already has a plan, and `--plan ""` clears it.
  Read plans back with `list --json` (the `planned` field).
- `list` — one line per card by default (id, state, title, context tail);
  `--json` emits full card objects, one per line, including a derived
  `orphaned` flag (claim points at a Session that's gone). Bare `list` is
  the live board — cards Cut into a Version are hidden; `--shipped NAME`
  lists one Version, `--all` everything.
- `start` — self-service claim of a backlog card. Requires you to be inside a
  foreman terminal (`FOREMAN_TERMINAL_ID` set).
- `dispatch <id> --agent claude|codex|grok [--worktree|--branch|--no-worktree]` —
  what the board's "Start with" button does, for an orchestrator: spawn the
  agent in a new terminal with the generated dispatch prompt (`# Workspace`
  section, close-out lines, `Card:` trailer) and claim the card for it.
  Backlog cards only; refused while a live Session holds the card. Reply is
  `open`'s shape (`{"ok":true,"terminal":"tN","project":"pN"}`); follow the
  worker with `wait <id>`. Never hand-roll `git worktree add` for a card —
  that skips the integration queue, teardown, and orphan tracking.
- `done` — closes a card you hold: in-progress -> done.
- `block --reason R` — in-progress -> blocked; the reason is mandatory.
- `rm` — deletes the card's file outright, from any state.
- `worktrees` — every worktree foreman made for the project, probed live:
  `<name> <state> <title> [wt …]` for a card's tree, `<name> no card [wt …]`
  for a stray (a tree no card owns). `--stray` keeps only strays; `--json`
  emits one object per line (name, path, branch, base, card, status).
- `wait` — polls until the card (or, with `--any`, any in-progress card)
  reaches done, blocked, orphaned, or removed. Exit codes: `0` done, `1`
  blocked/orphaned/removed (needs a human), `2` timeout or foreman
  unreachable, `3` (only `wait <id>`) the card's integration needs
  resolution — resolve in the worktree, then `integrate` and `wait` again.
  Queued and integrating are still pending.
- `integrate` — submit a worktree card's committed work to the repository's
  integration queue, or `--cancel` to withdraw it; `--json` prints the
  integration object. See **Integration queue** below.
- `cut NAME` / `uncut NAME` — the ship ritual, run by a human or a release
  script after tagging: `cut` moves every ungrouped Done card into Version
  NAME; `uncut` puts them back. Workers are not expected to Cut.

## Fast path: one command, trust the exit code

Creating a card is a single `add` call. Do not research the repo to compose
a body — write the pointer you already have, or go title-only and move on.
Exit code `0` means the card exists; no follow-up `list` to verify. The
ok-reply JSON on stdout carries the new card's `id` — capture it if you
will `wait` on the card later; if your harness ate stdout, `list --json`
recovers it.

## Close-out discipline

A card you claimed with `start` ends with `done` or `block --reason "..."` —
never end a session holding a claimed card with neither. If you were spawned
to work a specific card, your dispatch prompt already contains the exact
close-out command; use it as given rather than reconstructing it.

End every commit message with the trailer line `Card: <id>` (your dispatch
prompt shows it). That line is how the board attaches your commits to the
card when the release is Cut; a commit without it is simply not attached.

`start` on a card another live Session already holds is **rejected by
design** — that is the guard doing its job, not a transient error to retry.
If you hit it, the card is already someone's; go pick a different one or
check `list` for what's actually free.

## Routing: kanban vs chat vs elsewhere

If it changes a card's column, title, or body, it is a kanban verb. If it
needs a reply from someone, it is chat — see the foreman-chat skill. Durable content (specs,
decisions, long writeups) belongs in GitHub Issues or `docs/`, not in the
card body; the card body is a pointer to where the real detail lives, not
the detail itself.

Body convention: a few lines of task statement plus the paths or issue
numbers a worker needs to start — not a full brief crammed into the card.
But a dispatched worker's prompt is ONLY the card's title, body, and the
close-out lines — it sees nothing you know. So a card you will dispatch needs a
body that starts a cold worker: what to do, the doc/issue/file pointers,
the acceptance criteria, and the gate commands. The detail still lives in the
doc; the body is what makes the pointer findable.

## Orchestrating a plan

"Run / orchestrate the plan" means every worker is a card started with
`kanban dispatch`. Never `open`, never `git worktree add` by hand — those skip
the card claim, the `Card:` trailer, the integration queue, and the board.

1. **Find the plan.** `list --json`: a card's plan is `planned.name` and
   `planned.wave` (absent = unplanned). Copy the name from a card.
2. **Current wave** = the lowest `planned.wave` among the plan's cards that
   are not `done`. Cards in a wave are a set: any order, may run together.
   Dispatch only that wave's `backlog` cards; never start a later wave early.
3. **Dispatch one at a time**, keeping each reply's `terminal`:

       & $env:FOREMAN_EXE kanban dispatch <id> --agent claude

   `--agent` is required and cards have no agent field: follow the user's
   instruction, else `claude` (workers implement; the chat skill's
   Codex-research/Claude-implement split is for chat rooms, not cards).
4. **Wait**: `kanban wait --any --timeout 600`, then `list --json` — `--any`
   does not say which card moved. The wave is finished when every card in it
   is `done`; then start the next wave. Do not run `integrate` or `done` for a
   worker — its prompt already does.
5. **Stop and tell the human** when a wave card is `blocked` (read
   `blocked_reason`) or `orphaned` (its terminal died). `dispatch` only starts
   backlog cards and restart is a board action — do not route around it with
   `open`.

Dispatch errors:

- `foreman did not respond` or a bring-up error: worktree bring-up must finish
  inside foreman's 5 s reply window. A slow one spawns nothing and keeps the
  tree; a reply lost after a spawn is undone and the card returns to Backlog.
  Check `list`, then retry the same dispatch once.
- `held by live Session`, `integration is …`, not Backlog, cmd-shim: a real
  refusal. Report it. Do not retry blindly and do not fall back to `open`.

Expect: workers are interactive agents, and one stuck on a permission prompt
looks exactly like a working one — `wait` only times out (exit 2). On a
timeout, read the pane:
`& $env:FOREMAN_EXE snapshot --terminal tN --tail 30`. Each worktree card
cold-builds its own `target/` (minutes) and lands through one integration
queue a card at a time, so a wave fans out and lands slowly.

## Worktrees: where a dispatched card runs

A card started from the board (or `kanban dispatch`) may run in its own
git worktree —
`<repo>/.foreman/worktrees/<id>` on branch `card/<id>` — so no two workers
share a checkout. The choice is made at dispatch time: the board's
`wt` / `branch` / `here` chip beside the agent picker, or `dispatch
--worktree` / `--branch` / `--no-worktree`; both default to the app setting
(`wt` or `here`). A card that already has a worktree or a branch always
restarts in it (a conflicting flag is refused). If you were dispatched into
one, your prompt has a `# Workspace` section saying so; if it does not, you
are in the project cwd.

**Branch mode** (`branch`) puts `card/<id>` in the project checkout itself,
no worktree. Your `# Workspace` says "There is no worktree". The checkout may
hold the human's uncommitted changes, so: stage only your own files by name
(never `git add -A` / `git add .`), and never stash, reset, restore, clean,
or switch branches. Integration is the same `integrate` + `wait`, but the
queue never rebases the shared checkout: if the base moved, `wait` exits 3
with reason `base moved` and you rebase yourself (block if the human's
changes stop the rebase). On success the checkout is back on the base with
the human's changes untouched. Only one branch-mode card can hold the
checkout at a time; while it does, worktree cards' integrations hold.

What that changes for you:

- **Never merge into the main checkout yourself, and never run `done` on
  a worktree card.** Commit everything, then hand the card to the
  integration queue (`integrate` + `wait`, both lines are in your prompt);
  Foreman rebases, checks, fast-forwards, and marks the card Done. `done`
  on a branch with commits not yet on its base is refused and points at
  `integrate`. **Queued is not Done.** Details: **Integration queue** below.
- **Never stage `.foreman/`.** The worktree carries a stale copy of the
  board's card files; `git add -A` would commit them.
- **`done` queues a non-forcing teardown** that waits until your terminal
  pane is closed (a directory in use cannot be deleted). A Done card still
  showing its branch until then is normal. A dirty tree or an unmerged branch
  is kept, never deleted — the board shows it, and only a human can discard.
- **`rm` refuses** a card whose tree is dirty or ahead of base.
- **`list`** appends `[wt card/<id> +ahead -behind dirty|missing]` to a
  worktree card's line; `--json` adds `worktree` (path, branch, base) and a
  derived `worktree_status`, both absent for cards without one.
- **`worktrees`** is the whole picture, cards or not: a tree whose card was
  removed, deleted by hand, or left on another branch is a stray only this
  verb (and the board's Worktrees page) can see. Cleanup of a stray is
  human-only, on the board.

## Integration queue: how a worktree card lands

Foreman owns one integration turn per repository at a time, so two workers
can never race each other onto the shared checkout. The whole close-out
for a worktree card is two commands, run from anywhere inside your
Session:

    & $env:FOREMAN_EXE kanban integrate <id>
    & $env:FOREMAN_EXE kanban wait <id> --timeout 1800

`integrate` needs a clean worktree on `card/<id>` with no rebase or merge
in progress; it replies with the request's state (`queued #N`) and returns
at once. **Stop touching the worktree while it is queued or integrating**:
Foreman rebases it onto the card's base, runs the project's checks
(`.foreman/integrate.json`: `{"check": ["cargo","test"], "timeout_secs": 1800}`;
no file means no checks, and the card says so), fast-forwards the base,
and marks the card Done — teardown then waits for your pane as before.

Then branch on `wait`'s exit code:

- `0` — integrated and Done. You are finished; do not run `done`.
- `3` — handed back: the rebase conflicted or a check failed. Read the
  reason with `list --json` (the `integration` object: `reason`, `detail`,
  `next`), fix it in your worktree — a conflict is left mid-rebase for you:
  resolve, `git add`, `git rebase --continue` — commit, then run
  `integrate` and `wait` again. A resubmission joins the tail of the queue.
- `2` — timed out (still queued, or a long check): run `wait` again.
- `1` — blocked, orphaned, or removed: a human is involved; stop.

Resubmitting the same commit is harmless (you get the existing request
back). Committing more after submitting makes the submission stale: it
comes back as `needs resolution · source changed`; resubmit. A dirty main
checkout or one on the wrong branch holds the whole queue (`queued · held:
…`) until the human fixes it; nothing of yours is touched. **Held is not
blocked — do not `block` the card.** The queue retries by itself and lands
your commit once the destination is clean; keep running `wait`. A card you
block while its request is queued still lands, but you have handed the
human a stale Blocked reason to clear. `integrate <id>
--cancel` withdraws your request. The board shows the same substates on
the card (queued, integrating, needs resolution) and offers Submit /
Resubmit / Cancel on the detail page.
