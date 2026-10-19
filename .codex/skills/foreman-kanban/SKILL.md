---
name: foreman-kanban
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex needs to coordinate work through the project's kanban board — picking up a card, dispatching a worker onto a card, orchestrating or running a plan (dispatching its cards wave by wave), creating cards, editing title/body, ordering cards into plans and waves, closing out with done/block, or waiting on workers.
---

# The Foreman Project Kanban Board

**This skill is complete. Do NOT read foreman source or docs to learn kanban
mechanics — every fact you need is here or in the two reference files
beside this file.** Researching your task's subject matter is separate and
fine.
Precondition: `$env:FOREMAN` is `1`. If not, tell the user this needs to run
inside a Foreman terminal.

Two reference files sit beside this one. Read each only when you need it:

- `orchestrating-plans.md` — you run or orchestrate a plan: dispatching its
  cards wave by wave, reviewing finished workers, closing their terminals.
- `worktrees-and-integration.md` — your prompt has a `# Workspace` section,
  `wait` exited 3, the queue is held, or you are inspecting worktrees,
  strays, or a Cut. Orchestrating a plan does not need it.

Address the CLI the same way as dispatch/chat:

```powershell
& $env:FOREMAN_EXE kanban <verb> ...
```

```bash
"$FOREMAN_EXE" kanban <verb> ...
```

`foreman kanban --help` is ground truth for flags. Per-verb `--help` is not
accepted.

## Verbs

```powershell
& $env:FOREMAN_EXE kanban add "fix caret flicker" --body "repros on resize; see wm.rs"
& $env:FOREMAN_EXE kanban edit a3f8k2 --title "new title" --body "new body"
& $env:FOREMAN_EXE kanban edit a3f8k2 --plan "Terminal work" --wave 2
& $env:FOREMAN_EXE kanban list --state backlog
& $env:FOREMAN_EXE kanban list a3f8k2 --json
& $env:FOREMAN_EXE kanban start a3f8k2
& $env:FOREMAN_EXE kanban dispatch a3f8k2 --agent codex --worktree
& $env:FOREMAN_EXE kanban done a3f8k2
& $env:FOREMAN_EXE kanban block a3f8k2 --reason "needs a design decision"
& $env:FOREMAN_EXE kanban rm a3f8k2
& $env:FOREMAN_EXE kanban wait a3f8k2 --timeout 300
& $env:FOREMAN_EXE kanban wait --any a3f8k2 b7c1d9 --timeout 300
```

`worktrees`, `integrate`, `cut`, and `uncut` are in the reference file.

- `add` — positional words join into the title; `--body` attaches a longer
  description. Reply carries the new card's id.
- `edit` — replace title, body, and/or plan membership. At least one of
  `--title`/`--body`/`--plan`/`--wave` is required; the body replaces, never
  appends. Allowed in any state; does not claim or change state.
- `edit --plan NAME --wave N` — ordering for the Plan view. A plan is derived
  from its cards, grouped by plan then wave, lowest first. Names fold case and
  outer whitespace, so `Terminal work` and `terminal-work` are different
  plans — copy the name from an existing card. `--plan` alone starts at wave
  1, `--wave` alone renumbers, `--plan ""` clears. Plans read back from
  `list --json` (the `planned` field).
- `list` — one line per card: id, state, title, context tail. Bare `list` is
  the live board (cards Cut into a Version are hidden); `--shipped NAME` lists
  one Version, `--all` everything. `list <id>` prints only that card, in any
  state or Version. `--json` emits full card objects, one per line, with a
  derived `orphaned` flag (the claim points at a Session that's gone). Use
  `list <id> --json` whenever you want one card: the whole board costs every
  card's body.
- `start` — self-service claim of a backlog card. Requires
  `FOREMAN_TERMINAL_ID` (be inside a foreman terminal).
- `dispatch <id> --agent claude|codex|grok [--worktree|--branch|--no-worktree]`
  — what the board's "Start with" button does: spawn the agent in a new
  terminal with the generated dispatch prompt and claim the card for it.
  Backlog cards only. Reply is `{"ok":true,"terminal":"tN","project":"pN"}`.
  Never hand-roll `git worktree add` for a card.
- `done` — closes a card you hold: in-progress -> done.
- `block --reason R` — in-progress -> blocked; the reason is mandatory.
- `rm` — deletes the card's file outright, from any state.
- `wait` — polls until the card (or, with `--any`, any in-progress card;
  with `--any <id>...`, any of those cards, even one already finished)
  reaches done, blocked, orphaned, or removed, then prints that card's `list`
  line (`<id>  removed` if it vanished). Exit codes: `0` done, `1`
  blocked/orphaned/removed (needs a human), `2` timeout or foreman
  unreachable, `3` (only `wait <id>`) the card's integration was handed back.
  On `3` it also prints an `integration: {...}` line carrying `reason`,
  `detail`, and `next`. Queued and integrating are still pending.

## Fast path: one command, trust the exit code

Creating a card is a single `add` call. Do not research the repo to compose
a body — write the pointer you already have, or go title-only. Exit code `0`
means the card exists; no follow-up `list`. The reply JSON carries the new
card's `id` — capture it if you will `wait` on the card.

Sandbox note: Foreman's control channel is a named pipe, which the Codex
sandbox blocks — the connection fails with "Access is denied (os error 5)".
Run kanban commands with escalated/approved execution from the start
instead of discovering this per card.

## Close-out discipline

A card you claimed with `start` ends with `done` or `block --reason "..."` —
never end a session holding a claimed card with neither. A dispatched
worker's prompt already contains the exact close-out commands; use them as
given.

End every commit message with the trailer line `Card: <id>`. That line is
how the board attaches your commits to the card at Cut.

A dispatched worker's prompt has a `# Final report` section: end on its
four labeled lines. An orchestrator reads them from your pane, then closes
it.

`start` on a card another live Session holds is **rejected by design**, not
a transient error. Pick a different card.

## Routing: kanban vs chat vs elsewhere

If it changes a card's column, title, or body, it is a kanban verb. If it
needs a reply from someone, it is chat (foreman-chat skill). Durable content
belongs in GitHub Issues or `docs/`; the card body points at it.

A dispatched worker's prompt is ONLY the card's title, body, and close-out
lines — it sees nothing you know. A card you will dispatch needs a body that
starts a cold worker: what to do, the doc/issue/file pointers, acceptance
criteria, and gate commands.

Card workers are in the project chat room but hear only posts that target
them (`chat --to tN` or a leading `@tN`); broadcasts never wake them. Steer
one with a targeted post.

## Orchestrating a plan

"Run / orchestrate the plan" means every worker is a card started with
`kanban dispatch`. Never `open`, never `git worktree add` by hand — those
skip the claim, the `Card:` trailer, the integration queue, and the board.

**Read `orchestrating-plans.md` before dispatching the first card.** It
carries the whole loop: dispatch a wave, wait, review each finished
worker's report, fix the board from what it says, close its terminal, then
dispatch more.
