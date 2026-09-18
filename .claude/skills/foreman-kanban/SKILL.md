---
name: foreman-kanban
description: Use when running inside foreman (the FOREMAN env var is 1) and coordinating work through the project's kanban board — picking up a card, creating cards, editing title/body, closing out with done/block, or waiting on workers.
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
    & $env:FOREMAN_EXE kanban list --state backlog
    & $env:FOREMAN_EXE kanban start a3f8k2
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
- `edit` — replace title and/or body after add. At least one of `--title`/
  `--body` is required; title is trimmed and must be non-empty; body
  replaces (does not append). Allowed in any state; does not claim or
  change state.
- `list` — one line per card by default (id, state, title, context tail);
  `--json` emits full card objects, one per line, including a derived
  `orphaned` flag (claim points at a Session that's gone). Bare `list` is
  the live board — cards Cut into a Version are hidden; `--shipped NAME`
  lists one Version, `--all` everything.
- `start` — self-service claim of a backlog card. Requires you to be inside a
  foreman terminal (`FOREMAN_TERMINAL_ID` set).
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

## Worktrees: where a dispatched card runs

A card started from the board may run in its own git worktree —
`<repo>/.foreman/worktrees/<id>` on branch `card/<id>` — so no two workers
share a checkout. The choice is made on the board at dispatch time (a
`wt on/off` chip beside the agent picker, defaulting to the app setting);
there is no CLI flag, and a card that already has a worktree always restarts
in it. If you were dispatched into one, your prompt has a `# Workspace`
section saying so; if it does not, you are in the project cwd.

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
…`) until the human fixes it; nothing of yours is touched. `integrate <id>
--cancel` withdraws your request. The board shows the same substates on
the card (queued, integrating, needs resolution) and offers Submit /
Resubmit / Cancel on the detail page.
