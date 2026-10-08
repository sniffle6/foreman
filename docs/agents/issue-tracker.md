# Issue tracker: routed by artifact

This repo has three homes for tracked work. Every skill that says "issue tracker"
lands in one of them:

| Artifact | Home | Written by |
|---|---|---|
| **Spec** | `docs/superpowers/specs/<YYYY-MM-DD>-<slug>-design.md` | `/to-spec` |
| **Ticket** | a card on the foreman kanban board, grouped into a plan and ordered by wave | `/to-tickets` |
| **Issue** (bug report, request, wayfinder map) | GitHub Issues on `sniffle6/foreman` | `/triage`, `/wayfinder`, humans |

Why the split: specs are kept forever and `src/` headers cite them by path, so
they live in the repo; tickets are executed by `kanban dispatch`, so they live on
the board; GitHub stays the inbox for work that arrives from outside a session.

## GitHub conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --comments`, filtering comments by `jq` and also fetching labels.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

Infer the repo from `git remote -v` — `gh` does this automatically when run inside a clone.

Repo remote: `https://github.com/sniffle6/foreman.git`.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number> --comments` and `gh pr diff <number>` for the diff.
- **List external PRs for triage**: `gh pr list --state open --json number,title,body,labels,author,authorAssociation,comments` then keep only `authorAssociation` of `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, or `NONE` (drop `OWNER`/`MEMBER`/`COLLABORATOR`).
- **Comment / label / close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either — resolve with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Publish by artifact, per the table at the top:

- **A spec** (`/to-spec`): write `docs/superpowers/specs/<YYYY-MM-DD>-<slug>-design.md`
  from the skill's template, then add a `Status:` line under the title and a
  `## Rejected alternatives` section, each with its why — this repo's specs are
  the permanent record of *why* (**foreman-docs-and-writing**). The triage label
  step does not apply to a file.
- **Tickets** (`/to-tickets`): kanban cards — see **Kanban tickets** below.
- **Anything else**: a GitHub issue (`gh issue create`).

## Kanban tickets

Run from a foreman terminal (`$env:FOREMAN` = `1`) so cards land on this
project's board — outside one, `kanban` falls back to whichever project is
focused, which may be the wrong board. Verbs and flags: the **foreman-kanban**
skill.

- **Plan** = the spec's slug. One plan per spec; copy the name exactly on every
  card (names fold case, so `Diff window` and `diff-window` are two plans).
- **Wave** = 1 + the highest wave among the ticket's blockers; a ticket with no
  blockers is wave 1. Waves are coarser than blocking edges (a card waits for
  its whole previous wave), so record the real edges in the body too.
- **Publish blockers first**, two calls per ticket, keeping the `id` from the
  `add` reply:

      & $env:FOREMAN_EXE kanban add "<ticket title>" --body "<body>"
      & $env:FOREMAN_EXE kanban edit <id> --plan "<plan>" --wave <N>

- **Body** — a dispatched worker sees only the card's title, body, and close-out
  lines, so the body starts a cold worker:

      <What to build: the end-to-end behaviour, 1–3 lines>
      Spec: docs/superpowers/specs/<file>.md
      Blocked by: <ticket titles> | None
      Done when:
      - <acceptance criterion>
      Gate: cargo test --target-dir target/agent <filter>

  The spec path and gate command are pointers, so they belong here even though
  `/to-tickets` keeps file paths out of the ticket text itself.
- **Ready** = Backlog. Cards carry no labels; a planned Backlog card is
  agent-ready. Leave every card in Backlog.
- **Execution** is **foreman-kanban**'s "Orchestrating a plan": dispatch wave by
  wave, each worker on its own worktree, landing through the integration queue.

## When a skill says "fetch the relevant ticket"

- A card id (six characters, e.g. `a3f8k2`): `& $env:FOREMAN_EXE kanban list --all --json`
  and pick the line whose `id` matches.
- An issue number (`#42`): `gh issue view <number> --comments`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`gh api` on the sub-issues endpoint). Where sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub's **native issue dependencies** — the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only — the live gate). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me` — the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.

## Note on `ISSUES.md`

`ISSUES.md` at the repo root is a pre-existing lightweight log (e.g. the egui-wgpu device-lost crash). Skills that say "issue tracker" route per the table at the top of this file, never to `ISSUES.md`. You may keep `ISSUES.md` as human scratch notes or migrate entries into GitHub Issues over time.
