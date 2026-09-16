---
name: foreman-kanban
description: Use when running inside foreman (the FOREMAN env var is 1) and coordinating work through the project's kanban board — picking up a card, creating cards, closing out with done/block, or waiting on workers.
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
    & $env:FOREMAN_EXE kanban list --state backlog
    & $env:FOREMAN_EXE kanban start a3f8k2
    & $env:FOREMAN_EXE kanban done a3f8k2
    & $env:FOREMAN_EXE kanban block a3f8k2 --reason "needs a design decision"
    & $env:FOREMAN_EXE kanban rm a3f8k2
    & $env:FOREMAN_EXE kanban wait a3f8k2 --timeout 300
    & $env:FOREMAN_EXE kanban wait --any --timeout 300

- `add` — positional words join into the title; `--body` attaches a longer
  description. Reply carries the new card's id.
- `list` — one line per card by default (id, state, title, context tail);
  `--json` emits full card objects, one per line, including a derived
  `orphaned` flag (claim points at a Session that's gone).
- `start` — self-service claim of a backlog card. Requires you to be inside a
  foreman terminal (`FOREMAN_TERMINAL_ID` set).
- `done` — closes a card you hold: in-progress -> done.
- `block --reason R` — in-progress -> blocked; the reason is mandatory.
- `rm` — deletes the card's file outright, from any state.
- `wait` — polls until the card (or, with `--any`, any in-progress card)
  reaches done, blocked, orphaned, or removed. Exit codes: `0` done, `1`
  blocked/orphaned/removed (needs a human), `2` timeout or foreman
  unreachable.

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

`start` on a card another live Session already holds is **rejected by
design** — that is the guard doing its job, not a transient error to retry.
If you hit it, the card is already someone's; go pick a different one or
check `list` for what's actually free.

## Routing: kanban vs chat vs elsewhere

If it changes a card's column, it is a kanban verb. If it needs a reply from
someone, it is chat — see the foreman-chat skill. Durable content (specs,
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

- **Integrate before `done`.** From inside your worktree: `git rebase <base>`
  then `git -C "<main checkout>" merge --ff-only card/<id>` (both lines are
  in your prompt). If the fast-forward is refused, rebase and retry; if the
  main checkout has uncommitted edits in files you touched, `block` instead
  of forcing.
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
