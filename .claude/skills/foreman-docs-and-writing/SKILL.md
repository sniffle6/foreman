---
name: foreman-docs-and-writing
description: Use when writing or updating any foreman doc, epic, contract, plan, spec, CONTEXT.md glossary entry, or skill; when deciding whether HANDOFF.md, CLAUDE.md, or a feature doc can be trusted; when a doc contradicts code (snap-tiling.md, foreman.md, epic status headers, the --settle-ms "not yet honored" note); when composing commit messages (type(scope) subject, Co-Authored-By trailer, the literal-"@" subject incident); or when editing .claude/skills or .codex/skills copies.
---

# foreman-docs-and-writing — the docs of record

How foreman's documentation system works: which doc to trust, which docs are
traps, the house style for writing new ones, glossary discipline, commit-message
conventions, and how the repo's skills are maintained. Repo root:
`H:/claude code/foreman`. Baseline: commit `7fda1c2` on `main` (as of 2026-07-01).

## The doc map — trust table (as of 2026-07-01)

Statuses below were verified against code on 2026-07-01, not copied from the
docs' own headers. **A doc's own status header is evidence, not truth.**

| Doc | Role | Trust (2026-07-01) |
|---|---|---|
| `CLAUDE.md` | Quick-load summary for Claude agents | **Most current summary.** Architecture list covers the major modules but not all 18 `src/*.rs` files. One stale spot: the "Session Context" section names `feat/browser-style-tabs` as active though that work is merged to `main` (commits `d4da700`, `31a3db4`) and the current branch is `main`. (Its "Memory system" pointer was also dead until 2026-07-02, when MEMORY.md was first written to that directory.) |
| `AGENTS.md` | Codex mirror of CLAUDE.md | Current; same content adapted for Codex. Has no Session Context section, so it lacks CLAUDE.md's stale spots. |
| `docs/HANDOFF.md` | DECLARED authoritative deep doc (HANDOFF.md:8-9; CLAUDE.md says "HANDOFF.md wins on any conflict") | **Drifted — trust section-by-section.** Current: §3 build/verify loop, §4 gotchas, §2 coordinate model. Stale: §2 "Architecture / files" names only 5 of 18 src modules (omits `control.rs`, `chat.rs`, `inspect.rs`, `caret.rs`, `keymap.rs`, `input.rs`, `settings.rs`, `dirpicker.rs`, …); §5 roadmap lists "AI-agent integration" as future though the Control plane, Chat room, and Inspection layer shipped; §6 says "After a feature, update `docs/foreman.md`" — dead practice, see house style. |
| `CONTEXT.md` | **Glossary of record** (ubiquitous language) | Current and growing — the "Frame plan" entry landed at HEAD `7fda1c2`. Glossary ONLY, by its own charter (CONTEXT.md:4-6). |
| `docs/tiling-tree.md`, `docs/chat-delivery.md`, `docs/cursor-rendering.md`, `docs/os-chrome.md`, `docs/settings-persistence.md`, `docs/terminal-zoom.md`, `docs/tab-icons.md`, `docs/terminal-selection.md`, `docs/project-directories.md`, `docs/conpty-resize-reflow.md` | Subsystem feature docs | Current (spot-verified: named symbols and key files exist in `src/`). |
| `docs/terminal-inspection.md` | Feature doc for `send`/`snapshot` | Mostly current, **one stale section**: it says `--settle-ms` is "accepted and parsed but not yet honored" — false; `src/wm.rs` honors it (`DEFAULT_SETTLE_MS`, `PendingSettle`, `advance_settles`, wm.rs:938 as of 2026-07-01). `src/control.rs`'s own doc comments repeat the same stale claim (control.rs:129-130, 548, 763) — even code comments drift. |
| `docs/snap-tiling.md` | **TRAP** | Describes DELETED code (`compose_zone`, `snap_or_tab`, `zone_rect`, `Zone`) with **no supersession banner**. The only surviving src mention is a historical comment at layout.rs:229. Superseded by `docs/tiling-tree.md`. |
| `docs/foreman.md` | **TRAP** (older narrative notes) | Snap-zone sections (`detect_zone`/`zone_rect`, the `foreman/index.html` mockup) describe deleted code; a current "Tiling tree + floating" section was appended (~line 222) but the stale sections above it carry no banner. CLAUDE.md already flags this doc. |
| `docs/epics/*.md` | Design + decision history | **Decision history reliable; status headers LAG code** — see below. |
| `docs/contracts/` | Pinned seam agreements + status trackers | `chat-handshake-contract.md` header says "approved; being implemented" (2026-06-11); the tracker `chat-handshake-remaining-work.md` says "deferred; inert surface removed", then "DONE — Part 1 … built 2026-06-27". **The tracker wins over the contract header.** |
| `docs/superpowers/` (12 plans, 8 specs, 1 session as of 2026-07-01) + `docs/plans/` (1 plan) | Workflow HISTORY — specs, executable plans, session resume-state | Not live guidance. Read for "why was it built this way", never for "how it works now". |
| `docs/chat-missing-features.md`, `docs/chat-persistence.md`, `docs/followups-latency-and-control.md` | Gap lists / build plans / dated session snapshot | Honest about their own status (`chat-persistence.md` says "designed, not built" — believe it). `followups-…` is a 2026-06-18 snapshot; treat as historical. |

### Epic status headers that lag code (verified 2026-07-01)

| Epic | Header claims | Code says |
|---|---|---|
| `keyboard-control-epic.md` | "Status: designed, not started" | Shipped — `src/keymap.rs` + `src/settings.rs` exist; the tabbing epic itself says "leader + data-driven keymap already shipped". |
| `window-tabbing-split-epic.md` | Also "designed, not started" | Tab stacks shipped (its own banner admits Phase 1 shipped; browser-style tabs landed at `d4da700`). |
| `terminal-inspection-epic.md` | `--attrs`/`--cursor` listed under "Remaining: Phase 4" | Built at `ec0af05` (control.rs:161-164). `--region`/`--wait-for`/`--since-seq` genuinely remain unbuilt (no hits in control.rs). |
| `terminal-completeness-epic.md` | "in progress", dated 2026-06-26 | Roughly accurate; test counts in it are dated. |

## The precedence rule (verified reality)

The manifest (CLAUDE.md) declares "HANDOFF.md wins on any conflict". That rule
predates the drift documented above. Do not rewrite the manifest; **operate by
this verified order instead**:

1. **Code** (`src/`) — always wins.
2. **Subsystem feature doc** (`docs/<feature>.md`) — freshest prose.
3. **CLAUDE.md / AGENTS.md** — current summary, minus its Session Context section.
4. **HANDOFF.md** — trust §3/§4 (build loop, gotchas); distrust §2 architecture
   list, §5 roadmap, §6 "update foreman.md".
5. **Epics / contracts** — decision history yes; status headers only after
   checking the tracker or code.

**Before acting on any doc claim, verify it against code.** One grep is cheaper
than one wrong build. Example — is a symbol a doc names still real?

```powershell
Set-Location "H:/claude code/foreman"
Get-ChildItem src/*.rs | Select-String -Pattern "compose_zone|snap_or_tab" | Select-Object -First 5   # empty = the doc is describing deleted code
```

## House style for feature docs

- **One doc per feature, not per commit.** Named after the feature:
  `docs/tiling-tree.md`, `docs/chat-delivery.md`.
- **Update the existing doc** when a feature changes; check `docs/` for an
  existing home before creating a new file.
- **Plain language** (grug-brain): short sentences, no jargon walls, written so
  a cold reader or a Sonnet-class model gets it. The repo's docs already read
  this way — match them.
- **Standard shape:** `## What it does`, how to use it, gotchas, and always a
  **`## Key files`** section listing the main src files with the load-bearing
  symbols. 14 of the feature docs and 3 epics carry `## Key files`
  (as of 2026-07-01).
- **Skip docs for trivial changes** (typo fixes, one-liners).
- **Code comments explain WHY, not WHAT.** No internal task/PR references in
  comments; external upstream links are fine (e.g. `microsoft/terminal #18725`
  at terminal.rs:745). Look at the module-level comments in `src/wm.rs` or
  `src/control.rs` for the register.
- Doc changes ride the feature commit or get their own `docs(scope):` commit —
  see commit conventions below. Doc updates are part of a change's definition of
  done; see **foreman-change-control** for the gating.

### Checklist — when a feature ships

- [ ] Feature doc created or updated (with `## Key files`).
- [ ] Any doc the change supersedes gets a **banner** (next section).
- [ ] Epic status header updated if the epic's phase state changed.
- [ ] New named seam? Add a CONTEXT.md glossary entry (below).
- [ ] CLAUDE.md **and** AGENTS.md architecture lines updated if a module was
      added/renamed (keep the pair in sync — they are Claude/Codex mirrors).
- [ ] Touched `.claude/skills/foreman-dispatch|foreman-chat`? Sync the
      `.codex/skills` twin and note that a rebuild is required (skills section).

## Supersession discipline

When a doc's subject is replaced, **add an explicit banner at the top naming
the successor and the date.** Do not delete the doc (history has value); do not
leave it bannerless (it becomes a trap).

- **Good example** — `docs/epics/window-tabbing-split-epic.md:3`:
  `> **PARTIALLY SUPERSEDED (2026-06-11):** … see docs/tiling-tree.md …` — names
  what died, what still holds, and the successor.
- **Counter-example** — `docs/snap-tiling.md`: commit `b42e92e`
  ("supersede zone-snap docs") bannered the epic and updated HANDOFF/CLAUDE.md
  but never touched snap-tiling.md itself. It still reads as live documentation
  of code that no longer exists. This is the exact failure mode the banner rule
  prevents. (If you are editing near it: adding the missing banner is a
  legitimate docs fix — route it through **foreman-change-control** like any
  change.)

Banner template:

```markdown
> **SUPERSEDED (YYYY-MM-DD):** <what replaced this and why, one sentence>.
> See `docs/<successor>.md`. Kept for decision history only.
```

## CONTEXT.md glossary discipline

`CONTEXT.md` is the vocabulary of record and **glossary only** — terms, one-line
meanings, and `_Avoid_:` synonym lists. No implementation detail; that lives in
`docs/` (CONTEXT.md:4-6 says so itself).

- **Add an entry when you introduce a named seam.** Observed pattern: the
  Input-encoding seam, Caret (né Caret gate), Quiescence settle, Cell metrics, Outbox, and
  Frame plan entries each landed alongside the code that created them
  ("Frame plan" arrived with `7fda1c2`, the commit that built `src/frame.rs`).
- **Use the glossary terms exactly** in docs, skills, commit messages, and code
  comments: Win (not pane), Session (not terminal/PTY), Content, Project (not
  workspace), Ready, Leader, Chord, Keymap, Dispatch, Control plane, Snapshot,
  Outbox, Caret (the gate was retired 2026-07-15), Cell metrics, Quiescence
  settle, Deferred action.
- **Never use an entry's `_Avoid_:` synonyms.** If you catch a doc using one,
  fixing it is in-scope for any docs pass.
- Entry shape: bold term, 1-3 line meaning, `_Avoid_:` line. Match the file.

## Commit conventions (from `git log`)

- **Format:** `type(scope): subject` — types observed: `feat`, `fix`, `docs`,
  `refactor`, `perf`, `style`. Scopes are modules or features: `terminal`, `wm`,
  `tabs`, `chat`, `control`, `render`, `inspect`, `caret`, `ui`, `claude-md`.
- **Body says why** (and cites test counts when relevant — e.g. `37687b5`'s body
  ends "177 tests green").
- **Trailer:** `Co-Authored-By: Claude <model> <noreply@anthropic.com>` on every
  agent-authored commit.
- **Prefer a new commit over `--amend`.**
- **The literal-"@" subject incident (`37687b5`):** its subject is a lone `@`,
  the real subject is buried in the body, and the body ends with another `@`.
  The pattern matches PowerShell here-string delimiters (`@'` / `'@`) being
  passed through a shell that treated them as literal text. Guard rails:
  - Write multi-line messages with a real PowerShell here-string — opening
    `@'` at end of the `git commit -m` line, closing `'@` at **column 0** on its
    own line — or use `git commit -F <msgfile>`. Never paste a here-string into
    a POSIX (Git Bash) shell.
  - **Verify after committing:** `git log -1 --format=%B` — three seconds, and
    it would have caught `37687b5`.

## Skill maintenance

Three repo skills exist in `.claude/skills/` with twins in `.codex/skills/`:
`foreman-dispatch`, `foreman-chat` (user-facing, for agents running INSIDE
foreman), and `build-screenshot` (developer-facing, repo-local).

| Rule | Detail |
|---|---|
| `.claude/skills/` is the source; `.codex/skills/` is the Codex adaptation | Keep them **semantically synced** when behavior changes; adapt commands, don't copy text: Claude variants use `claude` / `claude -p`, Codex variants use `codex` / `codex exec`. Codex variants also carry `agents/openai.yaml`. |
| **Editing dispatch/chat requires a REBUILD to propagate** | `src/skills_install.rs` embeds all four SKILL.md sources (plus the openai.yaml files) via `include_str!` (skills_install.rs:115-121 as of 2026-07-01) and installs them into the Claude and Codex **global** skill dirs at GUI startup — best-effort, never blocks launch. A source edit does nothing globally until the exe is rebuilt and run. |
| `build-screenshot` stays repo-local | It is NOT in skills_install.rs's embed list; AGENTS.md states only dispatch/chat are globally installed. Keep it that way. |
| User-facing skills keep their **stop sign** | dispatch and chat both open with "**This skill is complete. Do NOT read foreman source or docs to learn … mechanics**". Preserve that line in any edit — those skills serve agents who must not spelunk src. Your developer-facing library (these 16 skills) must likewise defer operational usage of dispatch/chat to them. |
| `build-screenshot` keeps `disable-model-invocation: true` | It spawns a real window on the user's desktop; it is user-triggered only. |

## Plans, specs, sessions — write like the repo does

The lifecycle itself (hunch → spec → plan → result) is owned by
**foreman-research-methodology**; this is only the house format:

- **Specs** (`docs/superpowers/specs/`) record **rejected alternatives and
  accepted tradeoffs**, not just the winner — see the inspection epic's
  "design-it-twice … three parallel interface explorations → the hybrid" and the
  keyboard-control epic's "Decision history (settled with the user): … →
  **rejected**" blocks.
- **Plans** (`docs/superpowers/plans/`, `docs/plans/`) are checkbox-executable:
  `- [ ] **Step N:** …` steps a cold session can run top-to-bottom
  (e.g. `2026-06-11-tree-floating-windows.md`).
- **Sessions** (`docs/superpowers/sessions/`) are resume-state for interrupted
  work.
- All are dated `YYYY-MM-DD-<slug>.md` and become history the moment the work
  lands — never edit them to match later reality; supersede or let them age.

## When NOT to use this skill

- Build/toolchain setup, warning baseline → **foreman-build-and-env**.
- Running the app or the control CLI's verbs/flags/timeouts → **foreman-run-and-operate**
  (and, for agents inside foreman, the user-facing **foreman-dispatch** / **foreman-chat**).
- Whether a change is allowed and how it's gated/reviewed → **foreman-change-control**.
- What counts as evidence for a claim in a doc → **foreman-validation-and-qa**.
- The history of investigations/dead ends themselves (vs. how to write them up) → **foreman-failure-archaeology**.
- Architecture invariants and seam meanings beyond glossary discipline → **foreman-architecture-contract**.
- The spec→plan→result lifecycle discipline → **foreman-research-methodology**.

## Provenance and maintenance

Written 2026-07-01 against `main` @ `7fda1c2` (clean tree at time of writing).
Re-verify drift-prone claims before trusting this table — all commands run from
`H:/claude code/foreman`, all read-only:

| Claim | Re-verify with |
|---|---|
| snap-tiling.md still bannerless / describes deleted code | `Get-Content docs/snap-tiling.md -TotalCount 5` and `Get-ChildItem src/*.rs \| Select-String "compose_zone"` |
| `--settle-ms` honored but docs/comments say otherwise | `Select-String -Path src/wm.rs -Pattern "DEFAULT_SETTLE_MS"` vs `Select-String -Path docs/terminal-inspection.md,src/control.rs -Pattern "not yet honored"` |
| Epic status headers lag | `Get-Content docs/epics/keyboard-control-epic.md -TotalCount 4` vs `Test-Path src/keymap.rs` |
| `--attrs`/`--cursor` built; `--wait-for`/`--since-seq`/`--region` not | `Select-String -Path src/control.rs -Pattern "--attrs\|--cursor\|--wait-for\|--since-seq\|--region"` |
| HANDOFF architecture list names 5 modules; src has 18 | `(Get-ChildItem src/*.rs).Count` vs `Select-String -Path docs/HANDOFF.md -Pattern "src/\w+\.rs"` |
| CLAUDE.md Session Context stale (branch, memory dir) | `git branch --show-current` and `Get-Content CLAUDE.md \| Select-Object -Last 10` |
| Contract tracker wins over contract header | `Get-Content docs/contracts/chat-handshake-remaining-work.md -TotalCount 12` |
| Commit format + trailer | `git log -10 --format='%h %s%n  %(trailers:key=Co-Authored-By)'` |
| The "@" incident | `git show 37687b5 --no-patch --format=%B` |
| skills_install embeds only dispatch/chat | `Select-String -Path src/skills_install.rs -Pattern "include_str"` |
| Superpowers counts (12 plans / 8 specs / 1 session) | `Get-ChildItem docs/superpowers -Recurse -File \| Group-Object Directory` |
| CONTEXT.md still glossary-only + entry list | `Get-Content CONTEXT.md -TotalCount 10` and `Select-String -Path CONTEXT.md -Pattern '^\*\*'` |
