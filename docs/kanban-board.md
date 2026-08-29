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
- **Dispatch from a card**: hovering a Backlog or Blocked card offers an agent
  picker (claude / codex / grok). Picking one spawns a new Session in the
  project cwd whose prompt embeds the card body and the exact close-out
  commands, then claims the card and moves it to In Progress in the same
  action — a card-spawned agent never runs `start` itself.
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
top of Backlog creates title-only cards. Hover a card for its actions:
dispatch/re-dispatch, mark Done, send back to Backlog (orphaned cards), and
delete. Click a claimed card's terminal tag to jump to that Session. There is
deliberately no block button — blocking demands a typed reason, so it is the
CLI's move.

**CLI** (inside a foreman terminal, address it as `& $env:FOREMAN_EXE`; the
installed exe is also on PATH as `foreman`):

```
foreman kanban add "fix caret flicker" --body "repros on resize; see wm.rs"
foreman kanban list [--state backlog|in_progress|blocked|done] [--json]
foreman kanban start <id>                 # claim a card yourself
foreman kanban done <id>                  # close out: In Progress -> Done
foreman kanban block <id> --reason "..."  # close out: needs a human
foreman kanban rm <id>                    # delete the card file, any state
foreman kanban wait <id> | --any [--timeout SECS]
```

`wait` exit codes: `0` Done, `1` Blocked / orphaned / removed (needs a
human), `2` timeout or foreman unreachable. `foreman kanban --help` is ground
truth for flags. Agents not spawned from a card learn all of this from the
embedded **foreman-kanban** skill.

**Transitions** are enforced identically for CLI and board: claims move
Backlog/Blocked cards to In Progress; `start` on a card with a live claim is
rejected (the two-agents-one-card guard) but seizes a dead one; `done`/`block`
only from In Progress; release (board-only) returns In Progress or Blocked to
Backlog; Done is terminal — delete or promote to a GitHub issue.

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

## Key files

- `src/kanban.rs` — the pure domain: `Card`/`Claim`/`CardState`, `CardStore`
  (file-per-card load/save, transition verbs, staleness poll), `claim_is_dead`
  / `is_orphaned` (derived orphan rule), `run_nonce`, `dispatch_prompt` +
  `CloseoutStyle`, `CardLine`, `wait_verdict`.
- `src/board.rs` — `BoardView` (the window content) and `BoardAct` (the
  intents it records for the manager to drain).
- `src/wm.rs` — the seams: `kanban_tick` (per-frame orphan recompute + gated
  reload), `kanban_dispatch` (the control-pipe verb table), `drain_board_acts`
  (applies board intents: store writes, jump-to-terminal, dispatch-from-card),
  `open_board_window` (per-project singleton), `term_states`.
- `src/control.rs` — `KanbanRequest` (the wire shape), `parse_kanban_args`,
  `kanban_main`, `kanban_wait` (client-side poll loop), `HELP_KANBAN`.
- `src/workspace.rs` — `ContentSnap::Board` persistence variant.
- `src/keymap.rs` — `Command::OpenBoard` and its default binding.
- `src/skills_install.rs` — embeds the foreman-kanban skill (Claude + Codex
  twins).
