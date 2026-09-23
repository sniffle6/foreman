# Kanban Cut — design spec

Settled with the user 2026-09-16. Amended the same day after a negative-space
review, before approval: `shipped` became an object that also carries the
card's commits, Cut validates before it writes and reverts on failure, a
card with an unmerged worktree stays in Current instead of being cut,
`Current` is reserved, and bare `list` hides shipped cards. Extends the
kanban board (`2026-08-28-kanban-board-design.md`): Done cards can be
grouped into a named Version by an explicit Cut, and the Done column can
switch between live ungrouped work and a past Version. Nothing in the
terminal, chat, or sessions panel changes.

## Problem

Done is a pile. After a ship you cannot answer "what was in this version"
without reading git log or deleting the evidence. The board was built as
work-in-flight (`2026-08-28-kanban-board-brainstorm.md`); Done was terminal
(`rm` or promote to GitHub) and had no grouping. The human want is to see
completed cards per ship, on the board, for any project Foreman is attached
to — not only this repo — and to get from a shipped card to its commits
without a manual `git log` search.

## Decisions (from brainstorm)

- **Cut is a button (and a CLI verb), not a git/GitHub detector.** The human
  (or a release script) scoops the ungrouped Done cards on the board at
  click time. *Rejected:* watching `v*` tags and stamping by `updated` vs
  taggerdate (Foreman-was-closed, lightweight tags, and a late `done` all
  assign the wrong Version; empty-Done at tag time needs marker files so a
  later catch-up does not lie). *Rejected:* following the project's GitHub
  Releases (network, remote, ~12 min after the tag, and the existing update
  poll is Foreman's own releases, not the open project's).
- **A release is everything up to the tip.** Cut does not try to match
  cards to commits one-to-one. Work that never had a card is fine; the
  Version is the set of cards whose work is in the tip, nothing more. The
  only card Cut declines to take is one whose kept worktree branch is still
  ahead of base — that work is provably not in the tip, so the card stays
  in Current and the Cut says so. *Rejected:* refusing the whole Cut over
  one stranded card (blocks the ship ritual). *Rejected:* cutting it anyway
  with a red worktree line (the Version lies at a glance).
- **Per-project, any board.** Cards live in that project's `.foreman/tasks/`.
  Cut stamps that store. Foreman's repo is not special. A project with no
  git, no `v*` tags, or a different naming scheme still Cuts; the name is
  typed and no commits are attached.
- **Commits attach at Cut through a commit trailer.** Every card dispatch
  prompt (and the foreman-kanban skill, for `start`ed workers) tells the
  worker to end each commit message with a `Card: <id>` trailer. Cut walks
  the log once and records, per card, the trailer commits reachable from
  `HEAD`. The trailer survives rebase, squash, and cherry-pick, so nothing
  has to be captured at `done` time, where the worker's fast-forward has
  already made "which commits were mine" unanswerable from refs alone.
  *Rejected:* `rev-list base..card/<id>` at `done` (empty after the
  integrate step). *Rejected:* recording a base sha at dispatch and
  diffing (includes everyone else's commits landed on base meanwhile).
  *Rejected:* the worktree's private reflog (clever, unreadable, gone
  after teardown).
- **Live Done is ungrouped only.** `state` stays `done`. An optional
  `shipped` object names the Version. The Done column's Current view is
  `state=done` and no `shipped`. That keeps the board work-in-flight; shipped
  cards are history you switch into, not a graveyard in the live column.
  *Rejected:* stacked groups inside Done (the column becomes a changelog).
  *Rejected:* a separate archive page (you asked to stay on the board).
- **The version dropdown switches Done only.** Backlog / In Progress /
  Blocked stay live so you can still dispatch while browsing a cut.
  *Rejected:* hiding the live columns in archive mode. *Rejected:* dimming
  them. Archive obviousness is chrome on the Done column itself.
- **Duplicate names are refused, case-insensitively.** A second Cut cannot
  silently append into last ship's Version (the hotfix-into-v1.2.0 case),
  and `v1` / `V1` are one name, not two. Uncut and recut, or pick a new
  name. *Rejected:* merge-on-duplicate.
- **`Current` is a reserved name** (any case). It is the pinned dropdown
  literal; a Version with that name would render as a second identical row.
- **Cut validates everything, then writes; a failed write reverts.** N card
  files with no cross-file transaction is the storage model; the spec's job
  is to make a half-Cut impossible to observe. See §Cut.
- **No ship marker files.** Cut is disabled/errors when ungrouped Done is
  empty, so every Version has at least one card at creation. The dropdown is
  the distinct `shipped.name` values. `rm` the last card in a Version and
  that name leaves the dropdown. *Rejected:* `.foreman/ships/<name>.json`
  (needed for the tag-watcher empty-ship case, not for a button).
- **CLI `cut` / `uncut` exist** so a release ritual can script it. GUI
  prefills the latest local `v*` tag when that name is unused; CLI never
  guesses — the name is always an argument.
- **Bare `list` is the live board.** It hides shipped cards, exactly as the
  Current view does; `--all` dumps history too. Agents are taught to run
  `list` to see the board, and that output must not grow with every ship.
  *Rejected:* unfiltered `list` dumping every card forever (the first draft
  of this spec; reversed on review).

## Vocabulary

**Cut** is the action (board button and `foreman kanban cut`). **Version** is
the named group it creates. Cards in a Version are **shipped**. The live
Done column is **Current**. The **card trailer** is the `Card: <id>` line a
worker puts at the end of a commit message.

**Release** stays the existing board action: In Progress or Blocked →
Backlog, clearing the claim. Do not reuse that word on the button, the verb,
the field, or the dropdown.

_Avoid:_ milestone, sprint, cycle, changelog, archive (the *state* — the
Done column shows an archived *view*).

## Goals and non-goals

Goals: after a Cut, Current Done is empty of that work; picking a Version in
the dropdown shows those cards; it is obvious that Done is not Current; any
project's board can do this; Cut/Uncut are scriptable; a Cut either fully
happens or leaves no trace; a shipped card shows the commits that carried
its work.

Non-goals (v1): tag or GitHub auto-detect; stacked groups; hiding or dimming
other columns; merging into an existing Version name; generating GitHub
release notes from cards; persisting the dropdown selection across app
restarts; a fifth `CardState`; reconstructing a Version from git history;
targeting a card at a Version before it is Done; showing commits on a card
before it is shipped; enforcing the trailer (a commit without it is simply
not attached).

> **Superseded 2026-09-23 (tagging):** this spec kept Cut record-only and
> left the tag to a separate manual step. The board's Cut now performs the
> release — commit the cards, bump `Cargo.toml`, push, tag, push the tag
> (`src/release.rs`, `docs/kanban-board.md` "Cut is the release"). Reason:
> the user's intent for Cut is "ship it", and a record-only Cut paired with
> a manual tag was two rituals for one act. Tags are still created, never
> watched; the auto-detect rejection above stands. The CLI keeps a
> record-only `cut`; `cut --release` is the board's behavior.

## Card schema

`Card` gains one optional field. Absent in the file and in `list --json`
when `None`, so v1 card files and unshipped json lines stay byte-identical.

```json
"shipped": {
  "name": "v0.4.9",
  "at": "2026-09-16T23:10:00Z",
  "commits": ["0ea479a", "a52089e"]
}
```

- `name` is free-form, stored as typed (after trim). The GUI prefill is a
  convenience, not a format. `v0.4.9`, `1.2.0`, `2026-09` are all valid.
  Names compare case-insensitively everywhere (duplicate check, `--shipped`,
  `uncut`, the dropdown); the stored case is the one typed at Cut.
- `at` is the Cut timestamp, one value shared by every card in the Cut. It
  is the Version's sort key in the dropdown and is the only ordering
  source. Same shape as `claim.at`.
- `commits` is the list of abbreviated shas, oldest first, of commits
  reachable from `HEAD` at Cut time whose trailer names this card. Omitted
  when empty (no git, no trailer commits, a card done by hand). Never
  refreshed after Cut: it is a record of what the Cut saw, and a later
  history rewrite does not edit history on the board.
- Set only by Cut; cleared only by Uncut. No other transition writes it.
- `state` remains `done`. Cut is not a state change.
- `updated` is bumped by Cut and Uncut like any other write, and keeps
  bumping on later writes (worktree clear after a teardown, Discard). It is
  *not* the Version's order key — a Discard on one shipped card must not
  reorder or split its Version. *Rejected:* a string `shipped` ordered by
  `updated` (the first draft; broken by exactly that Discard case, and it
  overwrote the closest thing to a done-time the card had).
- On load, a `shipped` whose `name` trims to empty is treated as absent.

Same additive-field pattern as `worktree`. Old parsers that ignore unknown
keys still see a Done card; an old exe that rewrites a card without the
field would drop it (the same mixed-version cost `worktree` already has).
One Foreman writes the files.

## The card trailer

The dispatch prompt template gains one line under its existing close-out
instructions, in both `CloseoutStyle` renderings:

    End every commit message with the trailer line `Card: <id>`.

The foreman-kanban skill says the same for workers who `start` a card
themselves. It is a convention, not a gate: `done` does not check for it,
and a card whose commits lack it ships with no `commits` list. It is a
standard git trailer, so `git log --grep` and `git interpret-trailers` both
find it, and `git log --format=%(trailers:key=Card,valueonly)` reads it
without parsing message bodies.

## Cut

Operates on the focused project's store (the same project rule as every
other kanban verb). All validation happens before the first write.

1. `reload()` first, like `add` — the candidate set and the duplicate check
   are judged against the files, not the in-memory mirror.
2. Name is required, trimmed, non-empty, and not `Current` (any case).
3. Ungrouped Done (`state=done`, `shipped` is `None`) must be non-empty.
4. No existing card may already have a `shipped.name` equal to the name,
   case-insensitively.
5. Every candidate that still carries a `worktree` is probed synchronously
   (the `rm` pre-check verdict, ~100 ms per card). Dirty, ahead of base, or
   unprobeable means that card's work is not in the tip: it is dropped from
   the candidate set and stays in Current. If that empties the set, Cut is
   an error (`no card in Done is merged; nothing to cut`) and writes
   nothing. Cards without a worktree were worked in the main checkout and
   are always candidates.
6. Commits are gathered in one walk, bounded by the oldest candidate's
   `created` (a card's commits cannot predate the card):
   `git log HEAD --since=<created> --format=%h%x00%(trailers:key=Card,valueonly)`
   bucketed by trailer value. Git missing, not a repo, or the command
   failing gives every card an empty list — fail-open, like the prefill.
7. One `at` stamp is taken. Every remaining candidate gets
   `shipped = {name, at, commits}` and `updated = at`, written in one batch
   with one fingerprint refresh (not one per card). Other states,
   already-shipped cards, and the cards held back in step 5 are untouched.
8. If any write fails, the cards already stamped in this Cut are rewritten
   without `shipped` (best effort, same batch path) and the error is
   `cut <name> failed on <id>: <e>; nothing shipped`. A revert that itself
   fails reports both ids; the human's recovery is `uncut <name>`, which
   works on a partial Version because it clears by name.
9. The reply names any card held back in step 5. Board: a toast
   `cut <name>: N cards; <id> stayed in Current (unmerged card/<id>)`. CLI:
   the same line on stdout, exit `0` — the Cut happened.

Board: Cut lives on the Done header, **only in the Current view**, disabled
when ungrouped Done is empty. Click opens a name field — an inline
single-line `TextEdit` in the Done column, the same shape as Backlog's
quick-add, not a modal (the board is pane-local by design). Prefill is the
latest `v*` tag in *that* project's repo (`git tag -l "v*" --sort=-v:refname`,
first result) **only if that string is not already a Version**. Git missing,
not a repo, no `v*` tags, or the command fails: the field starts empty.
Prefill is fail-open and runs synchronously at click time, the same
precedent as worktree bring-up. Confirm runs Cut; cancel does nothing.
Duplicate, reserved, or empty name is a toast, not a write.

CLI: `foreman kanban cut <name>` — name is positional, required, never
defaulted from git. Empty ungrouped Done, no mergeable candidate, and a
duplicate, reserved, or blank name are errors (same table as other kanban
verbs: the host's error line, exit `1` for a rejected transition).

## Uncut

Clears `shipped` (name, stamp, and commits) on every card whose
`shipped.name` matches (case-insensitively). They reappear in Current Done.
`updated` bumps. Unknown name is an error / no-op with an error line. Uncut
has no confirm: it is undone by recutting, and the board's confirm bar is
irreversible loss.

Board: Uncut lives on the Done column's archive banner, only while that
Version is selected. CLI: `foreman kanban uncut <name>`.

If the selected Version disappears (Uncut, or `rm` of its last card), the
dropdown snaps back to Current.

## Board UI

Done column header carries the version dropdown and, in Current, Cut. The
header's collapse click target shrinks to the title; the dropdown and Cut
button are their own hit regions and never collapse the column.

Dropdown: **Current** pinned at top, then Versions newest-first by
`shipped.at`. Selection is view state, not persisted — same as column
collapse. App restart shows Current.

**Current:** Done lists ungrouped Done cards. Actions unchanged (detail,
rm, worktree discard). Cut visible. A card held back by a Cut looks like
any other Current card; its red worktree line is the reason.

**A Version selected:** Backlog / In Progress / Blocked are unchanged and
still dispatch. Done lists only that Version's cards. Cut is hidden.
A persistent banner in the Done column (`Archived · <name>`) plus the
dropdown not sitting on Current is the archive signal — you are looking at
an archived Done column, not the live pile. No Start / Restart / dispatch
on those cards (they are Done and shipped). Detail still opens and shows
the Version name beside the state and a **Commits** section listing
`shipped.commits` (short sha, selectable text; no subject lookup in v1).
`rm` still deletes. Uncut on the banner.

**Collapsed Done while a Version is selected:** the rail shows the Version
name and that Version's count (`Done · v0.4.9 · 7`), not the live count,
and the selection survives collapse/expand. Collapsing never resets the
dropdown.

## List and wire

`KanbanRequest` gains additive pieces:

- `action` also accepts `"cut"` and `"uncut"`.
- `name: Option<String>` with `skip_serializing_if = Option::is_none`, used
  by cut, uncut, and `list --shipped`.
- `all: bool` with `skip_serializing_if = is_false`, used by `list --all`.

v1 requests omit `name` and `all`; a v1 JSON without the keys still parses.
An old exe answers unknown `action` with an error. Replies stay `OpenReply`;
`cut`'s held-back line rides in `history`, the same field `list` uses.

```
foreman kanban cut <name>
foreman kanban uncut <name>
foreman kanban list [--state backlog|in_progress|blocked|done] [--shipped NAME] [--all] [--json]
```

- Bare `list` (no flags) is the live board: every state, shipped cards
  excluded. This matches what the four columns show in Current.
- `--state done` is Current: ungrouped Done only (matches the column).
- `--shipped NAME` lists that Version (case-insensitive). `--state` plus
  `--shipped` is an error unless `--state` is `done` (redundant, same result
  as `--shipped` alone). An unknown name is an empty list, exit `0` — list
  is a query, not a transition, and behaves like any filter that matches
  nothing.
- `--all` includes shipped cards; combinable with `--json`, an error with
  `--shipped` or `--state`. Human lines for shipped cards carry the Version
  (`[shipped v0.4.9]`, same idea as the worktree tail; commits are json
  only). `--json` includes the full `shipped` object when set.

`wait` is unchanged: it already keys off leaving In Progress, not off Cut.

## Transition rules (additive)

The 2026-08-28 table still holds. Cut/Uncut do not change `state`:

- Cut: ungrouped Done → still Done, `shipped` set. A candidate whose
  worktree is dirty, ahead, or unprobeable is held back and untouched.
- Uncut: shipped Done → ungrouped Done, `shipped` cleared.
- `done` still only from In Progress, and never writes `shipped`.
- `rm` still deletes the file from any state, shipped or not.
- Release (send to Backlog) still does not apply to Done.
- Teardown's worktree clear and Discard still write a shipped card's
  `worktree` and `updated`; they never touch `shipped`.

## Tests

Domain (`src/kanban.rs`, next to the existing transition table):

- Cut stamps every mergeable ungrouped Done card and no other card.
- Cut refuses empty ungrouped Done, blank name, `Current` in any case, and
  a duplicate in any case.
- One shared `at` on every card in the Cut; `updated` equals it.
- A candidate whose worktree is dirty, ahead, or unprobeable is held back,
  reported by id, and left unwritten; a Cut whose every candidate is held
  back is an error that writes nothing.
- Commit gathering buckets a mixed log by trailer, ignores commits without
  one, is bounded by the oldest candidate's `created`, and yields empty
  lists when git is unavailable.
- A simulated write failure mid-Cut leaves no card with that `shipped.name`.
- Cut reloads first: a card file dropped on disk after the last reload is
  included / a name written on disk after the last reload is a duplicate.
- Uncut clears matching cards (any case), including `commits`, and refuses
  an unknown name.
- Already-shipped cards survive a later Cut of a different name.
- A shipped card's `clear_worktree` bumps `updated` and leaves `shipped`.
- A `shipped` with an empty `name` on disk loads as unshipped.
- Both dispatch prompt renderings carry the trailer line verbatim.

List / json:

- Bare `list` omits shipped cards; `--all` includes them.
- `--state done` omits shipped cards.
- `--shipped NAME` is only that Version, case-insensitively; an unknown name
  is empty with exit `0`.
- Card file and json omit `shipped` when `None`; a shipped card round-trips
  the `{name, at, commits}` object, and `commits` is omitted when empty.

Wire (`src/control.rs`, same shape as existing `wire_compat` tests):

- `name` and `all` serialize away when unset; a v1 KanbanRequest JSON
  without them still parses.
- `parse_kanban_args` accepts `cut` / `uncut` / `list --shipped` /
  `list --all`, and rejects `--all` with `--shipped` or `--state`.

Board (`src/board.rs`, the existing headless `run_frame` / probe pattern):

- Cut button is drawn only in Current and only with ungrouped Done present.
- Selecting a Version lists only its cards and draws the banner; Uncut and
  `rm` of the last card snap back to Current.
- Collapsing Done keeps the selection and the rail shows the Version name.
- Clicking the dropdown or Cut does not collapse the column.
- The detail page of a shipped card shows its commits.

Screenshot evidence is additional when this ships, not the substitute.

## Docs when this ships

Not this spec — this file is the decision record and is not edited to match
later reality. The implementation commit updates:

- `docs/kanban-board.md` (how Cut/Uncut, the dropdown, and the trailer
  work; gotchas: Cut on the release branch, since a Cut rewrites many card
  files at once and two branches each cutting overlapping cards conflict
  per file; a card without trailer commits ships with none).
- `CONTEXT.md`: **Cut**, **Version**, **card trailer**; `_Avoid_: release`;
  the **Card** entry gains a pointer to Version.
- Both `foreman-kanban` skill copies (cut/uncut/`--shipped`/`--all`; bare
  `list` is live cards; the trailer line; workers are not expected to Cut —
  it is the ship ritual). Rebuild so the embed propagates.
- `HELP_KANBAN` in `src/control.rs`.

## Key files (when built)

- `src/kanban.rs` — `Card.shipped` (`Shipped { name, at, commits }`),
  `CardStore::cut` / `uncut` (batched write, revert), the trailer walk,
  list filters, json, the trailer line in `dispatch_prompt`.
- `src/board.rs` — Done dropdown, inline Cut field, archive banner, Uncut,
  collapsed-rail label, detail-page Commits section, `BoardAct` intents.
- `src/control.rs` — `KanbanRequest.name` / `.all`, parse, help.
- `src/wm.rs` — `kanban_dispatch` / `drain_board_acts` for the new acts;
  the worktree pre-check shared with `kanban_rm`.

## Rejected alternatives (summary)

| Idea | Why not |
|---|---|
| Watch `v*` tags, stamp by timestamp | Wrong bucket when Foreman was closed or a `done` lands after the tag; empty ships need extra files |
| Follow GitHub Releases | Network, remote, lag; update poll is the wrong repo |
| Fifth `CardState` | Cut is grouping, not a column; `done` stays the state |
| `.foreman/ships/` markers | Only needed so empty tag-ships do not re-fire |
| Stacked groups in Done | Turns the live column into a changelog |
| Hide/dim other columns | You still want to dispatch while browsing a cut |
| Merge on duplicate name | Silent hotfix-into-last-ship |
| Persist dropdown selection | Same rule as column collapse |
| Auto changelog / GH notes | Durable notes stay GitHub; the board is the grouping |
| Reconstruct from the tagged tree | The tag is not the data source; live files are |
| String `shipped`, ordered by `updated` | Discard on one shipped card reorders the Version; loses done-time |
| Refuse the whole Cut on one unmerged card | Blocks the ship ritual; holding the card back is enough |
| Cut an unmerged card anyway | The Version claims work that never merged |
| Cut writes as it goes | A failed write leaves a half Version whose name blocks the retry |
| Bare `list` dumps shipped cards | Agent-facing output grows with every ship; board default hides them |
| Confirm on Uncut | Reversible by recut; confirm is reserved for irreversible loss |
| Capture commits at `done` from refs | The integrate step has already fast-forwarded base; nothing to diff |
| Match cards to commits without a trailer | No signal survives rebase or squash; heuristics lie |
