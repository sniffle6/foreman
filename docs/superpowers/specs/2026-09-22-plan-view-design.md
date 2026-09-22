# Plan view — design spec

Settled with the user 2026-09-18, carried forward 2026-09-22. This replaces
the abandoned Plan window (card `ul4qs9`, GitHub issue #7, branch archived as
tag `archive/plan-window-ul4qs9`). That build was 1,970 production lines with
a plan store, a scheduler engine, dispatch, pause/resume/stop, attempts, and
integration witnesses. All of it is cut. The target here is roughly 500 lines.

The user's constraint, in his words: he does not want foreman to get slow or
bloated. That is the acceptance criterion for every choice below. When in
doubt, choose the smaller thing.

## Problem

The board shows cards in columns but not in order. There is no way to say "do
these three, then those two", and no way to see at a glance which group of
work is current.

## Why the first design died

A plan authored up front needs its cards to exist up front. This repo's cards
are discovered as work proceeds: the `plds8v` worktrees card was in flight for
44 hours before the three cards depending on it were created, so a plan
authored at its start would have held exactly one card. Card timestamps in
`.foreman/tasks/*.json` are the evidence.

Putting ordering on the Card sidesteps this. You tag each card as you create
it. No foresight is needed, and there is no second store to keep in sync with
the board.

The live-agent control probes that killed the pause/stop half of that design
are recorded separately: `docs/2026-09-18-agent-control-probes.md`, and
entry 10 of **foreman-failure-archaeology**.

## Decisions — do not reopen

- **Ordering lives on the Card**, not in `.foreman/plans/*.json`. A "plan" is
  derived from card fields, exactly as a Version already is.
- **No Start button in v1.** Pure view; the board already dispatches. Deferred
  to a later change, not rejected forever.
- **No engine, no automatic wave advancing, no attempts, no recovery.**
- **No new control-plane verb.** Extend the existing `kanban edit`.
- The separate plan store, pause/interrupt controls, and integration witnesses
  are dead. Do not resurrect them.
- `dispatch_card` (the branch's extraction of the Board's inline dispatch) is
  dropped with the rest. With no Start button there is no second caller, so
  its `require_isolation` flag would always be false and `Dispatched` would
  carry unread fields. `src/wm.rs` stays as `main` has it.

## Data shape — mirror `Shipped` exactly

`src/kanban.rs` already proves this pattern. Read `Shipped`, `versions()`, and
`same_name()` before writing anything; the new code should read as their
sibling, not as a new invention.

- One optional field on `Card`, holding a plan name and a wave number.
- `#[serde(default, skip_serializing_if = "Option::is_none")]` so v1 card files
  stay byte-identical. Wire-compat requirement, not a preference — see
  **foreman-change-control**.
- Derive the plan list by scanning cards and folding names with `same_name()`,
  the way `versions()` does.
- A card is in at most one plan by construction. Do not validate what the type
  already guarantees.

## Authoring — no new verb

Extend `kanban edit`, which already takes `--title`/`--body`, is allowed in any
state, and touches no claim:

```
foreman kanban edit <id> --plan "Terminal work" --wave 2
foreman kanban edit <id> --plan ""      # clears it
```

Reading is free: `kanban list --json` already emits the whole card. No new read
verb, no new `cmd` on the wire, no new compat surface. Add the flags to
`KanbanRequest` as optional fields, and a compat test beside the existing ones
in `src/control.rs`.

## The window

A new `Content` variant, opened per project like the board, persisted as a unit
`ContentSnap`. Read-only:

- Group cards by plan, then by wave.
- Show each card's board state.
- A click opens that card's detail on the board.
- "Current wave" is the lowest wave number holding a non-Done card.
- Sort within a wave by `created`. Cards in a wave are a set, not a list; this
  costs no extra data and matches how the board already sorts.

`src/plan_view.rs` on `archive/plan-window-ul4qs9` is a reasonable *visual*
reference for layout, elision, and theming — it was screenshot-verified at full
and narrow widths. Read it with `git show archive/plan-window-ul4qs9:src/plan_view.rs`.
Do not copy its data plumbing; it reads a store that will not exist.

## Things the last build got wrong — do not repeat

- **Do per-frame work only when there is something to do.** The old `plan_tick`
  cloned whole plans and rebuilt card maps every frame even with zero plans.
- **Never index in draw code.** `progress.waves[i]` was a latent panic, and an
  index panic in a draw path aborts the app and kills every terminal.
- Use `th.danger` for attention states, not `th.bell`. The board's convention.
- Scale every dimension by the live font size; take colours from theme tokens
  only. Both were verified by screenshot and both work.

## Verification

Do not claim the window works without pixels. The user runs `/build-screenshot`
himself — the model cannot invoke it. Ask him, and give him a seeded fixture.

Recipe that worked: build to `target/agent`; create a throwaway project under
`target/agent/` with card JSON files; copy the user's real `settings.json` and
`themes/` into a sandbox `APPDATA` so the theme matches his; seed a
`workspace.json` pointing at it. A window whose rect you want respected must be
floating — a tiled window takes its rect from the layout tree.

## Gates

Full `cargo test` green, warnings at or below the 26-warning baseline, and
`cargo fmt --check` clean except the pre-existing `src/panel.rs` diff already
on `main`. Never stage `.foreman/`.

## Key files

- `src/kanban.rs` — `Card`, `Shipped`, `versions()`, `same_name()`.
- `src/control.rs` — `KanbanRequest`, the edit parse path, compat tests.
- `src/wm.rs` / `src/workspace.rs` — the `Content` variant and its `ContentSnap`.
- `src/board.rs` — card detail navigation target.
- New: the plan view module.
