---
name: foreman-change-control
description: Use when changing foreman and unsure what gate applies - before adding a dependency, deleting code, or touching control-plane JSON (OpenReply, skip_serializing_if, wire compat v1); when committing or writing commit messages; when tempted by git add -A or --no-verify; when editing the foreman-dispatch/foreman-chat/foreman-icat skill copies; when a proposal re-opens a settled decision (tiling tree, glow over wgpu, vsync, chat writer thread, "let ConPTY own the redraw", leader key); or when hooks or foreman-reviewer act stale.
---

# Foreman change control

How changes are classified, gated, and reviewed in this repo; the non-negotiables
with the incident behind each; and the settled do-not-re-litigate registry.

Claims here cite files and symbols, never line numbers — see the citation
doctrine in **foreman-docs-and-writing**.

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
(`docs/epics/window-tabbing-split-epic.md`, commit `583a38d`). On 2026-06-11
the **user** reversed it: a real Layout tree shipped and the entire 9-zone snap
system was deleted (`31a9120`), with a superseded banner added to the epic rather
than silent edits (`b42e92e`; the banner heads that epic).
Both lessons bind: (a) settled decisions bind agents, not the user; (b) when the
user reverses one, the old system is deleted wholesale and its docs get an explicit
supersede banner, not quiet rewording.

## Ask-first: the three tripwires

| Tripwire | Verified precedent for why |
|---|---|
| **New dependency** | The keymap editor was cut by a grug-review precisely to ship "with a hardcoded `match` and zero new dependencies … The editor is earned, not assumed" (`docs/epics/keyboard-control-epic.md`). The chat-persistence design rejected a storage trait until "a SECOND real backend actually lands" (`docs/chat-persistence.md` decision #2). Additions are earned. |
| **Deleting code/files** | The `dead_code` warnings are documented false positives — test-only `pub` fns that "are NOT dead; don't delete" (`docs/followups-latency-and-control.md`). Deletions that *were* right (`--await-ack`, zone-snap) were user-sanctioned and recorded. |
| **Wire-protocol change** | Control-plane JSON v1 replies must stay byte-identical — see next section. |

## Wire compatibility: v1 stays byte-identical (non-negotiable)

Every field added to `OpenReply` / `ChatRequest` after v1 is `Option`/`Vec` with
`#[serde(default, skip_serializing_if = ...)]`, so an unset field is **omitted from
the wire** and old and new peers interoperate — see the `OpenReply` and
`ChatRequest` struct definitions in `src/control.rs`. Derive the current set of
post-v1 fields with `rg -n 'skip_serializing_if' src/control.rs`.

Why it matters: the CLI client and the GUI server **can be different builds** —
the running fleet is often the release exe while debug builds proceed
(`docs/contracts/chat-handshake-remaining-work.md`), and `FOREMAN_EXE` is
pinned per Session at spawn time — and the globally installed
foreman-dispatch/foreman-chat/foreman-icat skills on every machine speak this
protocol. A silent format change breaks deployed agents with no failing test
unless the compat tests exist.

Rule when adding a field:

1. `#[serde(default, skip_serializing_if = "Option::is_none")]` (or `Vec::is_empty`).
2. Add a compat test in `src/control.rs` modeled on the existing ones — list
   them with `rg -n 'fn .*wire_compat' src/control.rs`
   (`chat_request_to_is_wire_compatible_with_v1` is the canonical model).
   Each asserts: the unset field serializes away, AND a v1 JSON without the key
   still parses.

CLI verb/flag ground truth is **foreman-run-and-operate**'s home.

## Ordering invariants (treat as change-control, not implementation detail)

| Invariant | Enforced at | Consequence for changes |
|---|---|---|
| **Reply-before-inject**: a chat post's ack is sent before any delivery injection; injection happens on a later frame via the per-frame delivery sweep | `src/wm.rs` `handle_ctrl` (`CtrlMsg::Chat` arm) posts + replies; `src/wm.rs` `chat_tick` (which drives `ChatLog::tick` in `src/chat.rs`) injects on a later frame; spec §3 | An injection cannot be undone, so bytes may only flow once the client is guaranteed to hear "ok". Never inject on the reply path. |
| **Close replies before teardown**: reply goes on the channel before the caller's Session (and its PTY) is killed; if the reply channel is dead, the close is **skipped entirely** | `src/wm.rs` `handle_ctrl` (`CtrlMsg::Close` arm) — grep `Reply BEFORE closing` | A self-close kills the caller's own process tree — reorder this and `foreman close` callers never hear the answer. |
| **Ids are never reused**: Win ids come from a monotonic counter; untab mints a NEW Win id | `src/wm.rs` — every mint is `let id = self.next; self.next += 1;` | A retried close errs loudly instead of double-closing. Never recycle ids "for tidiness". Member ids additionally stay stable for a Session's whole life (CONTEXT.md "Member id"). |
| **Stale requests drop unexecuted**: every verb checks `sent.elapsed() >= REPLY_TIMEOUT` (5 s, `src/control.rs` `REPLY_TIMEOUT`) before acting | every `CtrlMsg::*` arm in `src/wm.rs` `handle_ctrl` — `rg -n 'CtrlMsg::' src/wm.rs` lists them | New Control-plane verbs must do the elapsed check first. |

Why these exist architecturally → **foreman-architecture-contract**.

## Non-negotiables (with source and incident)

| # | Rule | Source / evidence |
|---|---|---|
| 1 | **Commit only when asked.** | Verbatim in AGENTS.md ("Commit only when asked.", Working Agreement) and `docs/HANDOFF.md`. |
| 2 | **Stage files by name; never `git add -A` / `git add .`.** | Every executed plan stages by name (see any `docs/superpowers/plans/*.md` commit step). Incident: `docs/superpowers/plans/2026-06-09-agent-dispatch.md` records live user edits in the tree that `-A` "would sweep … into feature commits". `.gitignore` covers build/tool dirs, `*.png`/`*.log`, and a few scratch artifacts (read it — it is short); scratch HTML (`_mockup.html`, `mocks/`) is tracked, and user WIP is never ignored. |
| 3 | **Evidence before claims.** | "Do not claim visual behavior works without image evidence" (AGENTS.md, Working Agreement); Snapshot evidence for Session behavior. What counts → **foreman-validation-and-qa**. |
| 4 | **Don't hijack the user's mouse/keyboard to test.** | AGENTS.md / `docs/HANDOFF.md` Working Agreement. Use headless Inspection (send/Snapshot) instead → **foreman-diagnostics-and-tooling**. |
| 5 | **Keep edits scoped; avoid unrelated refactors.** | AGENTS.md, Working Agreement. |
| 6 | **No flattery; push back on bad ideas.** | `docs/HANDOFF.md` Working Agreement, CLAUDE.md. |
| 7 | **Never bypass hooks** (`--no-verify`, disabling `.claude/settings.json` hooks). | Library policy codified here (not quoted from a repo doc): the hooks ARE the mechanical-fix gate; nothing routes around change control. |
| 8 | **Wire compat v1** (section above). | `src/control.rs` comments + compat tests. |

## Three-way skill sync (editing an EMBEDDED skill: dispatch, chat, icat)

**Three skills are embedded in the exe: `foreman-dispatch`, `foreman-chat`, and
`foreman-icat`** — plus their `.codex/skills` twins and those twins'
`agents/openai.yaml` variants. Each embedded skill exists in **three places**
that must move together (`src/skills_install.rs`):

1. `.claude/skills/<name>/SKILL.md` — source copy, Claude wording
   (`claude` / `claude -p`). Listed in `CLAUDE_SKILLS`.
2. `.codex/skills/<name>/SKILL.md` — semantically synced Codex copy
   (`codex` / `codex exec`), plus `agents/openai.yaml` UI metadata. Listed in
   `CODEX_SKILLS`.
3. **The exe itself**: `include_str!` embeds every one of those sources at
   **compile time** (`src/skills_install.rs`), and `install()` — gated on
   `Settings::install_skills` (default true; `src/main.rs` checks it at startup)
   — overwrites the globally installed copies in the Claude/Codex config skill
   dirs on every launch, with a managed-by notice appended. **Editing a SKILL.md
   without rebuilding propagates nothing**: the globally installed copy keeps
   serving the old text to every agent on the machine while the repo looks fixed.

Derive the current embed list — never trust a list written down here:

```powershell
Select-String -Path "H:/claude code/foreman/src/skills_install.rs" -Pattern 'include_str!'
```

Checklist for any embedded-skill behavior change:

- [ ] Update both the `.claude/skills` and `.codex/skills` copies (adapt examples
      per agent), and the Codex `agents/openai.yaml` if the change touches its
      description.
- [ ] Rebuild (`cargo build`) so the embedded copies match. Not optional.
- [ ] Renaming/dropping a shipped skill? Add the OLD directory name to
      `OBSOLETE_SKILLS` in `src/skills_install.rs` so stale installs are deleted
      on next launch; the dir name MUST equal the frontmatter `name:` (the
      doc comment on `CLAUDE_SKILLS` says why).
- [ ] `build-screenshot` is twinned in `.claude/skills` and `.codex/skills` but
      embedded in NEITHER — editing it needs no rebuild. The Claude copy carries
      `disable-model-invocation: true`; the Codex copy does not.

Operational usage of those skills is their own (user-facing) content — defer to
**foreman-dispatch** / **foreman-chat** / **foreman-icat**; do not restate their
mechanics here.

## Feature-doc gate (behavior changes only)

Observed, consistent pattern — each substantial feature commit ships code + doc +
glossary in one commit (verified via `git show --stat`):

- `d3bec20` — inspection layer + `docs/terminal-inspection.md` + CONTEXT.md (created, +156 lines).
- `32fe702` — CaretGate module + `docs/cursor-rendering.md` update + CONTEXT.md "Caret gate" entry.
- `9aeb72b` — ChatRoom module + `docs/chat-delivery.md` + CONTEXT.md "Member id" entry.

The gate: one doc per feature in `docs/`, updated in place if one exists, with a
"Key files" section; a new deliberate seam gets a CONTEXT.md entry under "Seams &
patterns". House style, doc trust map, glossary discipline → **foreman-docs-and-writing**.

`docs/HANDOFF.md` §6 records "after a feature, update `docs/foreman.md`" as **dead
practice** and points at the per-feature doc rule; CLAUDE.md likewise marks
`docs/foreman.md` as older narrative notes. Follow the per-feature doc rule.

## Commit conventions

Derive the live convention rather than trusting a list: `git log --oneline -40`.

- Subject: `type(scope): imperative summary`. Types in use: `feat`, `fix`,
  `docs`, `chore`, `refactor`, `perf`, `style`. Scope is the module or feature
  touched — read the log for the current vocabulary.
- Body: *why*, what moved, and an evidence line (e.g. the tests-green line in
  `37687b5`'s body).
- Trailer: `Co-Authored-By: Claude <model> <noreply@anthropic.com>` on every
  agent-authored commit.

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

Each entry names where it was settled. Re-opening any of these is a **design
reversal** (user decision only).

| Decision | Recorded at | One-line rationale |
|---|---|---|
| **Renderer is glow (OpenGL), not wgpu** — settled by commit `ba803ef` | `docs/gpu-device-loss.md` (the full record, with the side-by-side A/B); the `eframe` line in `Cargo.toml` | Windows loses the GPU device on sleep and display power transitions. `egui-wgpu` responds with an unconditional `panic!` in `update_buffers`, which aborts the process; `egui_glow` only logs. `default-features = false` on the `eframe` dep is **load-bearing** — eframe prefers wgpu when both backends are enabled, so re-adding default features silently reverses the decision. Do not "modernize" it back. |
| Floating Wins kept AND a real Layout tree exists — the 2026-06-05 "no BSP tile tree" stance was user-reversed 2026-06-11 | `docs/epics/window-tabbing-split-epic.md` banner; `docs/tiling-tree.md` | Both states are load-bearing now; don't argue either away. |
| vsync stays on (default) | `docs/followups-latency-and-control.md` | Turning it off did not fix latency (the repaint metronome did); tearing/GPU-spin risk for nothing. |
| No writer thread for chat persistence — synchronous write on the post path | `docs/chat-persistence.md` decision #3 (design status: **designed, not built**) | Proposed and "withdrawn by its own author": saves ~0 ms, adds a crash-loss window. |
| No storage trait / in-memory adapter for chat persistence | `docs/chat-persistence.md` decision #2 | Failed the deletion test; add a trait when a second real backend lands. |
| Exactly-once chat delivery rejected — at-least-once + dedup-by-seq | `docs/chat-persistence.md` decision #6 | Fails toward a duplicate "done", never a lost one. |
| Ligatures out of scope | `docs/epics/terminal-completeness-epic.md` | epaint shapes glyph-by-glyph. |
| Keyword-sniffing prose for blocked/done rejected → typed `--kind` | `docs/chat-missing-features.md` | Sniffing prose is fragile; the live agent-state problem is **foreman-agent-state-campaign**'s. |
| Agent-teams state-file parsing rejected | `docs/epics/agent-dispatch-epic.md` | Parsing another tool's private format breaks on any Claude update. |
| "Let ConPTY own the redraw" — tested, all four combinations failed | `docs/conpty-resize-reflow.md` | #19535's cursor sync is now bundled, but no frontend redraw-ownership strategy supplies conhost-parity content reflow. The fence applies to those failed strategies/full parity, not matched-pair package updates. Full chronicle → **foreman-failure-archaeology**. |
| Leader key over direct Chords | `docs/epics/keyboard-control-epic.md` | Direct Chords collide with the agent CLIs running inside Sessions. |
| Ready latch = successful first DSR reply flush + first child paint, NOT "DSR + output idle" | `docs/contracts/chat-handshake-remaining-work.md`; code in `Session::pump` | A streaming agent never goes output-idle, so the idle variant never latches Ready. Failed writes do not satisfy the latch. Note: `docs/contracts/chat-handshake-contract.md` still carries the older "post-DSR-settled" wording — superseded on this point. |
| `--await-ack` removed as an accepted-but-inert ("lying") API surface | `docs/contracts/chat-handshake-remaining-work.md` banner | Recover from commit `4607001` if unattended fleets ever need it. |
| Model/token status in Win headers rejected | `docs/epics/keyboard-control-epic.md` | The CLIs already render it inside the Session; duplicating it is noise + a sync problem. |

## Review machinery

Hooks live in `.claude/settings.json` with their scripts under `.claude/hooks/`.
The full inventory — every event, matcher, script and exit-code contract — is in
**foreman-config-and-flags**, which owns the configuration axis. It is not
repeated here: this section used to carry a second copy of that table, and a
second copy is a second thing to forget when a hook is added.

Two properties matter to change control specifically. Every hook **always exits
0 on its own failure**, so a broken hook is silent and never blocks work — with
one deliberate exception, `cite-guard.ps1`, which exits 2 so its findings come
back as feedback. And the `PreToolUse` kill hook fires on the **Bash tool only**.

Building from the **PowerShell** tool skips the PreToolUse hook, so nothing kills
the running exe and the link fails with `Access is denied (os error 5)`. Kill it
yourself first — by exe path, never by name:

```powershell
# ⚠ NOT when FOREMAN=1. That means this session is running INSIDE foreman: this
# would kill your own host, every other terminal in it, and you mid-command.
# In that case use `cargo build --target-dir target/agent`, or ask the user.
Get-Process foreman -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -like "$PWD\target\*" } |
    Stop-Process -Force
```

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

- Doc house style, trust map, glossary discipline → **foreman-docs-and-writing**.
- What counts as evidence for a claim → **foreman-validation-and-qa**.
- Why an invariant exists (threading, seams, design) → **foreman-architecture-contract**.

## Provenance and maintenance

Re-verify these before leaning on them; they are the load-bearing claims here
that code can move out from under. All read-only.

```powershell
# The embedded-skill list (drives the rebuild rule) + the obsolete hook
Select-String -Path "H:/claude code/foreman/src/skills_install.rs" -Pattern 'include_str!|OBSOLETE_SKILLS'

# Wire compat: post-v1 fields are still optional, and the compat tests exist
Select-String -Path "H:/claude code/foreman/src/control.rs" -Pattern 'skip_serializing_if|wire_compat'

# Renderer pin: eframe must stay default-features = false + "glow"
Select-String -Path "H:/claude code/foreman/Cargo.toml" -Pattern 'eframe|glow|wgpu'

# Hooks still configured as described
Get-Content "H:/claude code/foreman/.claude/settings.json"
```
