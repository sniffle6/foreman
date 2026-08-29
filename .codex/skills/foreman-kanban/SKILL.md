---
name: foreman-kanban
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex needs to coordinate work through the project's kanban board — picking up a card, creating cards, closing out with done/block, or waiting on workers.
---

# The Foreman Project Kanban Board

Precondition: `$env:FOREMAN` is `1`. If not, tell the user this needs to run
inside a Foreman terminal.

Address the CLI the same way as dispatch/chat:

```powershell
& $env:FOREMAN_EXE kanban <verb> ...
```

```bash
"$FOREMAN_EXE" kanban <verb> ...
```

`--help` on any verb is ground truth for flags — treat this skill as the map,
not the last word on syntax.

## Verbs

```powershell
& $env:FOREMAN_EXE kanban add "fix caret flicker" --body "repros on resize; see wm.rs"
& $env:FOREMAN_EXE kanban list --state backlog
& $env:FOREMAN_EXE kanban start a3f8k2
& $env:FOREMAN_EXE kanban done a3f8k2
& $env:FOREMAN_EXE kanban block a3f8k2 --reason "needs a design decision"
& $env:FOREMAN_EXE kanban rm a3f8k2
& $env:FOREMAN_EXE kanban wait a3f8k2 --timeout 300
& $env:FOREMAN_EXE kanban wait --any --timeout 300
```

- `add` — positional words join into the title; `--body` attaches a longer
  description. Reply carries the new card's id.
- `list` — one line per card by default (id, state, title, context tail);
  `--json` emits full card objects, one per line, including a derived
  `orphaned` flag (claim points at a Session that's gone).
- `start` — self-service claim of a backlog card. Requires Codex to be
  inside a Foreman terminal (`FOREMAN_TERMINAL_ID` set).
- `done` — closes a card you hold: in-progress -> done.
- `block --reason R` — in-progress -> blocked; the reason is mandatory.
- `rm` — deletes the card's file outright, from any state.
- `wait` — polls until the card (or, with `--any`, any in-progress card)
  reaches done, blocked, orphaned, or removed. Exit codes: `0` done, `1`
  blocked/orphaned/removed (needs a human), `2` timeout or Foreman
  unreachable.

## Fast path: one command, trust the exit code

Creating a card is a single `add` call. Do not research the repo to compose
a body — write the pointer you already have, or go title-only and move on.
Exit code `0` means the card exists; no follow-up `list` to verify. The
ok-reply JSON on stdout carries the new card's `id` — capture it if you
will `wait` on the card later; if your harness ate stdout, `list --json`
recovers it.

Sandbox note: Foreman's control channel is a named pipe, which the Codex
sandbox blocks — the connection fails with "Access is denied (os error 5)".
Run kanban commands with escalated/approved execution from the start
instead of discovering this per card.

## Close-out discipline

A card you claimed with `start` ends with `done` or `block --reason "..."` —
never end a session holding a claimed card with neither. If Codex was
spawned to work a specific card, the dispatch prompt already contains the
exact close-out command; use it as given rather than reconstructing it.

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
