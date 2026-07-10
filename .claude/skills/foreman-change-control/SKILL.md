---
name: foreman-change-control
description: Use when changing foreman and unsure what gate applies - before adding a dependency, deleting code, or touching control-plane JSON (OpenReply, skip_serializing_if, wire compat v1); when committing or writing commit messages; when tempted by git add -A or --no-verify; when editing foreman-dispatch/foreman-chat skill copies; when a proposal re-opens a settled decision (tiling tree, vsync, chat writer thread, "let ConPTY own the redraw", leader key); or when hooks or foreman-reviewer act stale.
---

# Foreman change control

How changes are classified, gated, and reviewed in this repo; the non-negotiables
with the incident behind each; and the settled do-not-re-litigate registry.
Baseline for every citation: commit `7fda1c2` on `main` (as of 2026-07-01, working
tree clean — the frame/geom paint-seam work landed in that commit the same day).

Domain terms used below: **PTY** = the OS pseudo-terminal object a Session's child
process is attached to; **DSR** = Device Status Report, the startup cursor query a
shell sends and blocks on. Full domain pack: **terminal-emulation-reference**.

## Change classes and their gates

| Class | Examples | Gate before it ships |
|---|---|---|
| **Mechanical fix** | typo, comment, `cargo fmt` fallout, local rename, test-only tweak | `cargo build` + `cargo test` green. The hooks are the review (see Review machinery). No doc. |
| **Behavior change** | anything a user or agent can observe: rendering, input, Layout tree, Control plane, chat delivery | tests + evidence (image evidence for GUI, Snapshot for Session behavior — standards live in **foreman-validation-and-qa**) + feature-doc update + CONTEXT.md entry if it adds a seam (see Feature-doc gate). |
| **Design reversal** | replacing a subsystem, contradicting anything in the settled registry below | **USER decision only.** Never reverse one yourself; never treat one as un-reversible by the user. |
| **Ask-first** | new dependency; deleting code or files; wire-protocol changes | explicit user OK *before* starting (see next section). |

**The incident behind the design-reversal class.** On 2026-06-05 the epic recorded
"We are **not** building a BSP tile tree" as settled-with-the-user
(`docs/epics/window-tabbing-split-epic.md:26-27`, commit `583a38d`). On 2026-06-11
the **user** reversed it: a real Layout tree shipped and the entire 9-zone snap
system was deleted (`31a9120`), with a superseded banner added to the epic rather
than silent edits (`b42e92e`; banner at `docs/epics/window-tabbing-split-epic.md:3-9`).
Both lessons bind: (a) settled decisions bind agents, not the user; (b) when the
user reverses one, the old system is deleted wholesale and its docs get an explicit
supersede banner, not quiet rewording.

## Ask-first: the three tripwires

| Tripwire | Verified precedent for why |
|---|---|
| **New dependency** | The keymap editor was cut by a grug-review precisely to ship "with a hardcoded `match` and zero new dependencies … The editor is earned, not assumed" (`docs/epics/keyboard-control-epic.md:27-30`). The chat-persistence design rejected a storage trait until "a SECOND real backend actually lands" (`docs/chat-persistence.md` decision #2). Additions are earned. |
| **Deleting code/files** | The `dead_code` warnings are documented false positives — test-only `pub` fns that "are NOT dead; don't delete" (`docs/followups-latency-and-control.md:79-84`). Deletions that *were* right (`--await-ack`, zone-snap) were user-sanctioned and recorded. |
| **Wire-protocol change** | Control-plane JSON v1 replies must stay byte-identical — see next section. |

## Wire compatibility: v1 stays byte-identical (non-negotiable)

Every field added to `OpenReply` / `ChatRequest` after v1 is `Option`/`Vec` with
`#[serde(default, skip_serializing_if = ...)]`, so an unset field is **omitted from
the wire** and old and new peers interoperate (`src/control.rs:34-60` for
`OpenReply.terminal/project/error/history/seq/cells/cursor`; `:84-100` for
`ChatRequest.from/to/re`; line numbers as of 2026-07-01).

Why it matters: the CLI client and the GUI server **can be different builds** —
the running fleet is often the release exe while debug builds proceed
(`docs/contracts/chat-handshake-remaining-work.md:110-115`), and `FOREMAN_EXE` is
pinned per Session at spawn time — and the globally installed
foreman-dispatch/foreman-chat skills on every machine speak this protocol. A
silent format change breaks deployed agents with no failing test unless the
compat tests exist.

Rule when adding a field:

1. `#[serde(default, skip_serializing_if = "Option::is_none")]` (or `Vec::is_empty`).
2. Add a compat test in `src/control.rs` modeled on the existing four
   (names verified, as of 2026-07-01):
   `chat_request_to_is_wire_compatible_with_v1` (`src/control.rs:1538`),
   `chat_request_re_is_wire_compatible` (`:1587`),
   `chat_history_request_is_wire_compatible_without_from` (`:1454`),
   `snapshot_reply_without_attrs_cursor_is_wire_compat` (`:1905`).
   Each asserts: the unset field serializes away, AND a v1 JSON without the key
   still parses.

CLI verb/flag ground truth is **foreman-run-and-operate**'s home.

## Ordering invariants (treat as change-control, not implementation detail)

| Invariant | Enforced at (as of 2026-07-01) | Consequence for changes |
|---|---|---|
| **Reply-before-inject**: a chat post's ack is sent before any delivery injection; injection happens on a later frame via the delivery sweep | `src/wm.rs:832-884` (comments at `:835`, `:873`, spec §3) | An injection cannot be undone, so bytes may only flow once the client is guaranteed to hear "ok". Never inject on the reply path. |
| **Close replies before teardown**: reply goes on the channel before the caller's Session (and its PTY) is killed; if the reply channel is dead, the close is **skipped entirely** | `src/wm.rs:913-925` | A self-close kills the caller's own process tree — reorder this and `foreman close` callers never hear the answer. |
| **Ids are never reused**: Win ids come from a monotonic counter; untab mints a NEW Win id | `src/wm.rs:699-702` (`self.next += 1`), comment at `:918`, test at `:4008` | A retried close errs loudly instead of double-closing. Never recycle ids "for tidiness". Member ids additionally stay stable for a Session's whole life (CONTEXT.md "Member id"). |
| **Stale requests drop unexecuted**: every verb checks `sent.elapsed() >= REPLY_TIMEOUT` (5 s, `src/control.rs:10`) before acting | all six verbs — `src/wm.rs:841,853,887,900,930,968` | New Control-plane verbs must do the elapsed check first. |

Why these exist architecturally → **foreman-architecture-contract**.

## Non-negotiables (with source and incident)

| # | Rule | Source / evidence |
|---|---|---|
| 1 | **Commit only when asked.** | Verbatim in AGENTS.md ("Commit only when asked.", Working Agreement) and `docs/HANDOFF.md:195`. |
| 2 | **Stage files by name; never `git add -A` / `git add .`.** | Every executed plan stages by name (see any `docs/superpowers/plans/*.md` commit step). Incident: `docs/superpowers/plans/2026-06-09-agent-dispatch.md:22` records live user edits in the tree that `-A` "would sweep … into feature commits". `.gitignore` only catches `/target`, `/.serena/`, `/.superpowers/`, `/.idea/`, `*.png`, `*.log` — scratch HTML (`_mockup.html`, `mocks/`) is tracked, and user WIP is never ignored. |
| 3 | **Evidence before claims.** "Do not claim visual behavior works without image evidence" (AGENTS.md, Working Agreement); Snapshot evidence for Session behavior. What counts → **foreman-validation-and-qa**. |
| 4 | **Don't hijack the user's mouse/keyboard to test.** | AGENTS.md / `docs/HANDOFF.md:194`. Use headless Inspection (send/Snapshot) instead → **foreman-diagnostics-and-tooling**. |
| 5 | **Keep edits scoped; avoid unrelated refactors.** | AGENTS.md, Working Agreement. |
| 6 | **No flattery; push back on bad ideas.** | `docs/HANDOFF.md:192`, CLAUDE.md. |
| 7 | **Never bypass hooks** (`--no-verify`, disabling `.claude/settings.json` hooks). | Library policy codified here (not quoted from a repo doc): the hooks ARE the mechanical-fix gate; nothing routes around change control. |
| 8 | **Wire compat v1** (section above). | `src/control.rs` comments + compat tests. |

## Three-way skill sync (editing dispatch/chat behavior)

The agent-operation skills exist in **three places** that must move together
(AGENTS.md "Foreman Skills"; `src/skills_install.rs`):

1. `.claude/skills/foreman-dispatch/SKILL.md` and `.claude/skills/foreman-chat/SKILL.md`
   — source copies, Claude wording (`claude` / `claude -p`).
2. `.codex/skills/...` — semantically synced Codex copies (`codex` / `codex exec`),
   plus `agents/openai.yaml` UI metadata.
3. **The exe itself**: `include_str!` embeds all of the above at **compile time**
   (`src/skills_install.rs:115-121`), and on every launch foreman overwrites the
   globally installed copies in the Claude/Codex config skill dirs (managed-by
   notice appended). **Editing the SKILL.md files without rebuilding propagates
   nothing.**

Checklist for any dispatch/chat behavior change:

- [ ] Update both `.claude/skills` and `.codex/skills` copies (adapt examples per agent).
- [ ] Rebuild (`cargo build`) so the embedded copies match.
- [ ] Renaming/dropping a shipped skill? Add the OLD directory name to
      `OBSOLETE_SKILLS` (`src/skills_install.rs:159-161`) so stale installs are
      deleted on next launch; the dir name MUST equal the frontmatter `name:`
      (`src/skills_install.rs:130-131` comment).
- [ ] `build-screenshot` stays repo-local — deliberately NOT embedded (AGENTS.md).

Operational usage of those skills is their own (user-facing) content — defer to
**foreman-dispatch** / **foreman-chat**; do not restate their mechanics here.

## Feature-doc gate (behavior changes only)

Observed, consistent pattern — each substantial feature commit ships code + doc +
glossary in one commit (verified via `git show --stat`):

- `d3bec20` — inspection layer + `docs/terminal-inspection.md` + CONTEXT.md (created, +156 lines).
- `32fe702` — CaretGate module + `docs/cursor-rendering.md` update + CONTEXT.md "Caret gate" entry.
- `9aeb72b` — ChatRoom module + `docs/chat-delivery.md` + CONTEXT.md "Member id" entry.

The gate: one doc per feature in `docs/`, updated in place if one exists, with a
"Key files" section; a new deliberate seam gets a CONTEXT.md entry under "Seams &
patterns". House style, doc trust map, glossary discipline → **foreman-docs-and-writing**.

**Drift flag:** `docs/HANDOFF.md:195` still says "After a feature, update
`docs/foreman.md`" — superseded in practice; CLAUDE.md marks `docs/foreman.md` as
older narrative notes. Write/update the per-feature doc instead.

## Commit conventions

Verified against the last 40 subjects (as of 2026-07-01, `git log --oneline -40`):

- Subject: `type(scope): imperative summary`. Types observed: `feat`, `fix`,
  `docs`, `refactor`, `perf`, `style`. Scopes observed: `wm`, `terminal`, `tabs`,
  `chat`, `keymap`, `keys`, `control`, `render`, `inspect`, `caret`, `ui`, `plan`,
  `claude-md`.
- Body: *why*, what moved, and an evidence line (e.g. "177 tests green" in `37687b5`).
- Trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` (present on 7 of
  the last 8 commits, as of 2026-07-01).

**Cautionary tale — commit `37687b5`.** Its subject is literally
`@ feat(wm): new terminals and projects tile by default` and its body ends with a
stray `@`: the PowerShell here-string delimiters (`@'` / `'@`) leaked into the
`git commit -m` argument. Prevention: the closing `'@` must sit at column 0 on its
own line; or write the message to a file and use `git commit -F`. Always verify
after committing:

```powershell
git -C "H:/claude code/foreman" log -1 --format=%B
```

These are the gates; message *house style* beyond them (wording, doc-commit
phrasing) is **foreman-docs-and-writing**'s home.

## Settled decisions — do NOT re-litigate

Each entry is verified against its pointer (as of 2026-07-01). Re-opening any of
these is a **design reversal** (user decision only).

| Decision | Recorded at | One-line rationale |
|---|---|---|
| Floating Wins kept AND a real Layout tree exists — the 2026-06-05 "no BSP tile tree" stance was user-reversed 2026-06-11 | `docs/epics/window-tabbing-split-epic.md:3-9` banner; `docs/tiling-tree.md` | Both states are load-bearing now; don't argue either away. |
| vsync stays on (default) | `docs/followups-latency-and-control.md:68-70` | Turning it off did not fix latency (the repaint metronome did); tearing/GPU-spin risk for nothing. |
| No writer thread for chat persistence — synchronous write on the post path | `docs/chat-persistence.md` decision #3 (design status: **designed, not built** as of 2026-07-01) | Proposed and "withdrawn by its own author": saves ~0 ms, adds a crash-loss window. |
| No storage trait / in-memory adapter for chat persistence | `docs/chat-persistence.md` decision #2 | Failed the deletion test; add a trait when a second real backend lands. |
| Exactly-once chat delivery rejected — at-least-once + dedup-by-seq | `docs/chat-persistence.md` decision #6 | Fails toward a duplicate "done", never a lost one. |
| Ligatures out of scope | `docs/epics/terminal-completeness-epic.md:360` | epaint shapes glyph-by-glyph. |
| Keyword-sniffing prose for blocked/done rejected → typed `--kind` | `docs/chat-missing-features.md:210` | Sniffing prose is fragile; the live agent-state problem is **foreman-agent-state-campaign**'s. |
| Agent-teams state-file parsing rejected | `docs/epics/agent-dispatch-epic.md:412-414` | Parsing another tool's private format breaks on any Claude update. |
| "Let ConPTY own the redraw" — tested, all four combinations failed | `docs/conpty-resize-reflow.md` | #19535's cursor sync is now bundled, but no frontend redraw-ownership strategy supplies conhost-parity content reflow. The fence applies to those failed strategies/full parity, not matched-pair package updates. Full chronicle → **foreman-failure-archaeology**. |
| Leader key over direct Chords | `docs/epics/keyboard-control-epic.md:24-26` | Direct Chords collide with the agent CLIs running inside Sessions. |
| Ready latch = successful first DSR reply flush + first child paint, NOT "DSR + output idle" | `docs/contracts/chat-handshake-remaining-work.md:29-33`; code in `Session::pump` | A streaming agent never goes output-idle, so the idle variant never latches Ready. Failed writes do not satisfy the latch. Note: `docs/contracts/chat-handshake-contract.md:74` still carries the older "post-DSR-settled" wording — superseded on this point. |
| `--await-ack` removed as an accepted-but-inert ("lying") API surface | `docs/contracts/chat-handshake-remaining-work.md:3-12` banner | Recover from commit `4607001` if unattended fleets ever need it. |
| Model/token status in Win headers rejected | `docs/epics/keyboard-control-epic.md:20-21` | The CLIs already render it inside the Session; duplicating it is noise + a sync problem. |

## Review machinery

Hooks in `.claude/settings.json` (verified 2026-07-01):

| Hook | Matcher | Script | Behavior |
|---|---|---|---|
| PreToolUse | `Bash` **only** | `.claude/hooks/kill-foreman.ps1` | If the command matches `cargo\s+(build\|run\|test)`, `Stop-Process -Name foreman` + 500 ms sleep. Always exits 0. **Does not fire for the PowerShell tool** — when building from PowerShell, kill the app yourself first (`Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`) or linking fails with `Access is denied (os error 5)`. |
| PostToolUse | `Edit\|Write` | `.claude/hooks/cargo-fmt.ps1` | Runs `cargo fmt` only when the edited path ends in `.rs`; prepends `C:\w64devkit\bin` and cargo to PATH. Always exits 0 — it never blocks, so a fmt failure is silent. |

**foreman-reviewer agent** (`.claude/agents/foreman-reviewer.md`, model sonnet):
invoke after edits to terminal-emulation code (`src/terminal.rs`, `input.rs`,
`caret.rs`, `geom.rs`, `frame.rs`, `inspect.rs`), WM/layout code (`src/wm.rs`,
`layout.rs`, `main.rs`), or control-plane/chat code (`src/control.rs`,
`chat.rs`), and before commits/PRs touching them. Reworked 2026-07-02: the old
zone-snap drift (it taught the model deleted 2026-06-11) is fixed; it now
reviews per-subsystem against the Layout-tree model, wire-compat v1, ordering
invariants, Ready gating, and the evidence gates, and routes to this skill
library for depth.

Build mechanics and the warning baseline → **foreman-build-and-env**. Symptom
triage → **foreman-debugging-playbook**.

## When NOT to use this skill

- Recreating the toolchain, build failures, warning baseline → **foreman-build-and-env**.
- What counts as evidence; test inventory and conventions → **foreman-validation-and-qa**.
- Running the app or the Control-plane CLI (verbs, flags, timeouts) → **foreman-run-and-operate**.
- Doc house style, trust map, glossary discipline → **foreman-docs-and-writing**.
- Why an invariant exists (threading, seams, design) → **foreman-architecture-contract**.
- History of investigations and dead ends → **foreman-failure-archaeology**.
- Measuring behavior instead of eyeballing → **foreman-diagnostics-and-tooling**.
- Actually dispatching agents or posting chat from inside foreman → **foreman-dispatch**, **foreman-chat** (user-facing).
- Building + screenshotting the GUI → **build-screenshot**.

## Provenance and maintenance

Written 2026-07-01 against commit `7fda1c2` (`main`, clean tree). All line numbers
are volatile; re-verify before trusting them. Run from anywhere:

```powershell
# Change-class incident: epic banner + deletion commits
Get-Content "H:/claude code/foreman/docs/epics/window-tabbing-split-epic.md" -TotalCount 10
git -C "H:/claude code/foreman" log --format='%h %ad %s' --date=short -1 31a9120

# Wire compat: fields + tests still present
Select-String -Path "H:/claude code/foreman/src/control.rs" -Pattern 'skip_serializing_if|wire_compat'

# Ordering invariants still in wm.rs
Select-String -Path "H:/claude code/foreman/src/wm.rs" -Pattern 'reply-before-inject|Reply BEFORE|never reused'

# Skill embed + obsolete hook
Select-String -Path "H:/claude code/foreman/src/skills_install.rs" -Pattern 'include_str|OBSOLETE_SKILLS'

# Hooks configuration and scripts
Get-Content "H:/claude code/foreman/.claude/settings.json"
Get-Content "H:/claude code/foreman/.claude/hooks/kill-foreman.ps1"

# foreman-reviewer teaches no dead zone-snap system (expect empty; the file's
# only 'zone' mentions are the guard telling reviewers zones were deleted)
Select-String -Path "H:/claude code/foreman/.claude/agents/foreman-reviewer.md" -Pattern 'snaps a new terminal to a zone'

# Commit conventions + trailer + the '@' cautionary tale
git -C "H:/claude code/foreman" log --oneline -30
git -C "H:/claude code/foreman" log -8 --format='%(trailers)'
git -C "H:/claude code/foreman" log -1 --format=%B 37687b5

# Settled-registry pointers
Select-String -Path "H:/claude code/foreman/docs/followups-latency-and-control.md" -Pattern 'vsync'
Select-String -Path "H:/claude code/foreman/docs/chat-persistence.md" -Pattern 'NO writer thread|rejected'
Select-String -Path "H:/claude code/foreman/docs/chat-missing-features.md" -Pattern 'Keyword-sniffing|Agent-teams'
Select-String -Path "H:/claude code/foreman/docs/conpty-resize-reflow.md" -Pattern 'own the redraw'
Select-String -Path "H:/claude code/foreman/docs/epics/keyboard-control-epic.md" -Pattern 'Rejected direct chords'
Select-String -Path "H:/claude code/foreman/docs/contracts/chat-handshake-remaining-work.md" -Pattern 'first-reply-flush|Latches true'

# Chat persistence still designed-not-built (no persistence code in chat.rs)
Select-String -Path "H:/claude code/foreman/src/chat.rs" -Pattern 'jsonl|persist' -Quiet
```
