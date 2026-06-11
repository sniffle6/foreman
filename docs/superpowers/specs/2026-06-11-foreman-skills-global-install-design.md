# Foreman self-installs its skills globally — design

Date: 2026-06-11
Status: approved (ready for implementation plan)

## Problem

Foreman spawns terminal sessions with env vars (`FOREMAN=1`,
`FOREMAN_PROJECT_ID`, `FOREMAN_TERMINAL_ID`, `FOREMAN_EXE`) regardless of the
session's working directory, and the `foreman-dispatch` / `foreman-chat` skills
invoke the `foreman` CLI through `$FOREMAN_EXE` (an absolute path, not PATH).
So the *capability* — spawning workers, using the project chat room — is fully
reachable from a terminal whose cwd is any repo.

What does **not** travel is the two skills that *teach* the agent to use that
capability. `foreman-dispatch` and `foreman-chat` currently live only in
foreman's own repo at `.claude/skills/`. When a foreman project points at an
**external** repo, the `claude` agent running there discovers skills from that
repo's `.claude/` plus `~/.claude/` — neither has the foreman skills. The agent
has the wires but no instructions.

Dispatched *workers* are unaffected (the chat convention is injected verbatim
through the prompt template, by design — no skill-discovery dependency). The gap
is only the lead agent the user converses with in an external-repo project.

## Goal

Make `foreman-dispatch` and `foreman-chat` discoverable by `claude` regardless
of working directory, without dirtying the user's external repos and without the
installed copies drifting from the source of truth.

## Approach

Foreman embeds the two skills in its binary and, on startup, idempotently
installs them into the user's global Claude config dir. The skills' frontmatter
descriptions are FOREMAN-gated ("Use when running inside foreman (the FOREMAN env
var is 1)…"), so they stay dormant in every non-foreman session.

### Considered alternatives (rejected)

- **Copy skills into the target repo's `.claude/skills/` at spawn** — dirties the
  user's git status in *their* repos. Rejected.
- **Per-terminal `CLAUDE_CONFIG_DIR` override pointing at a foreman-owned dir** —
  hides the user's entire real Claude config from foreman-spawned sessions.
  Rejected.
- **Distribute as a Claude Code plugin / marketplace entry** — a freight train
  for two markdown files; adds an install step and lets skills drift from the
  binary that understands their wire protocol. Rejected.

## Design

New module: `src/skills_install.rs`. One public entry point called once near the
top of `main()`, before the eframe loop. Synchronous (a handful of tiny file
ops, never a startup bottleneck — do not move to a thread).

### 1. Embedded source of truth

Each skill's `SKILL.md` is embedded via `include_str!`:

```rust
const DISPATCH_SKILL: &str = include_str!("../.claude/skills/foreman-dispatch/SKILL.md");
const CHAT_SKILL:     &str = include_str!("../.claude/skills/foreman-chat/SKILL.md");
```

The path is relative to this source file, so `../` lands at the repo root —
inside the package, cargo tracks the files for incremental rebuilds, and forward
slashes work on Windows. If a referenced file is missing at build time the build
breaks loudly — desirable: it forces the embed list to stay in sync.

The repo's `.claude/skills/` remains the single source of truth. Changing a
skill = edit that file + rebuild. The repo copy continues to serve
foreman-developing-foreman.

### 2. Target location

Resolve the Claude config dir:

1. `CLAUDE_CONFIG_DIR` if set **and non-empty** (treat empty string as unset).
2. Else `%USERPROFILE%\.claude`.

This matches Claude Code's own precedence for the personal skills dir. The
divergence from `keymap.rs` (which is `%APPDATA%`-rooted) is correct: `~/.claude`
is `USERPROFILE`-rooted — do not harmonize the two.

Install targets:
- `<config>\skills\foreman-dispatch\SKILL.md`
- `<config>\skills\foreman-chat\SKILL.md`

Windows-only assumption (`USERPROFILE`) is acceptable: foreman is ConPTY/Windows
today. A future `HOME` fallback is a one-liner if portability ever matters — not
written now.

### 3. Install logic — byte-compare, no marker

For each skill: build the final on-disk content (see §4), read the existing
target file if present, and write only if the file is missing or its bytes
differ. No hash, no marker file.

Rationale (this overturns an earlier "content-hash marker" idea): a marker is a
second piece of state whose only job is avoiding two ~4 KB reads, while
introducing a desync failure mode that defeats the feature — if the user deletes
or edits an installed `SKILL.md`, a content-hash marker still matches the
embedded bytes, so foreman never repairs it. Byte-compare is the same startup
I/O, carries zero extra state, and is self-healing against deletion/corruption.

### 4. User-edit semantics: clobber-always, declared

Installed files are foreman-managed and overwritten whenever they differ from the
embedded content. To make this explicit rather than a surprise, a fixed notice
line is appended to the embedded content at install time, e.g.:

```
<!-- managed by foreman; edits are overwritten on launch -->
```

The on-disk comparison is against `embedded + notice`, so the notice itself does
not cause a perpetual rewrite. Same-name collision with a user's own pre-existing
`foreman-dispatch`/`foreman-chat` skill is unlikely (names are well namespaced)
and the notice covers it; no "is this ours?" detection layer is built.

### 5. Atomic writes

Each write goes to a temp file in the **same** directory (e.g. `SKILL.md.tmp`)
followed by `std::fs::rename` over the target. This prevents a spawned `claude`
(which scans `~/.claude/skills` at session start) from ever reading a
half-written file — a truncated skill is worse than a missing one.

This is the entire concurrency story; no lockfiles, no retries. Two same-version
foreman instances write identical bytes (harmless). Two different-version
instances are last-write-wins and converge as soon as the newer one launches
last. Accepted.

### 6. Obsolete-skill cleanup hook

A hardcoded list, empty today:

```rust
const OBSOLETE_SKILLS: &[&str] = &[];
```

On install, any directory named in this list under `<config>\skills\` is removed.
This is the rename/removal hook: when a skill is ever renamed or dropped, add its
old name here so stale copies don't linger on every machine and confuse the agent
with duplicates. No manifest/registry is built now (YAGNI); the convention is
documented in the module doc comment.

### 7. Failure handling

Entirely best-effort. Any failure — `USERPROFILE` unset, directory create or
write error — is swallowed and logged to stderr; the app continues. The env
wiring already works and the skills still exist in foreman's own repo; a failed
global install must never block the desktop from launching. (stderr is invisible
for an Explorer-launched GUI, but this matches the established `keymap.rs`
best-effort pattern and the failure is non-critical.)

## Accepted tradeoff

Global install means these two skills appear in the skill list of **every**
`claude` session on the machine, forever — including after foreman is uninstalled
(there is intentionally no uninstall path). The FOREMAN-gated frontmatter
descriptions are the load-bearing mitigation that keeps them dormant elsewhere;
keeping that gating accurate is a hard requirement on those `SKILL.md` files.
This is a deliberate decision, recorded here so it is not mistaken for an
accident.

## Testing

Unit tests must not touch the real `~/.claude`. The filesystem-writing function
takes an explicit base dir so tests point it at a temp dir (mirrors how
`keymap.rs` keeps its write/read contract testable):

1. **Fresh install**: empty base dir → both `SKILL.md` files written with the
   expected `embedded + notice` content.
2. **No-op when current**: running again over an up-to-date dir performs no write
   (verify by mtime or a write counter / injected writer).
3. **Repair when differing**: a target file with different bytes (simulating a
   stale or hand-edited copy) is rewritten to match.
4. **Repair when missing**: a deleted target file is recreated.
5. **Obsolete cleanup**: a dir named in `OBSOLETE_SKILLS` (test override) is
   removed; unrelated dirs are left untouched.
6. **Config-dir resolution**: non-empty `CLAUDE_CONFIG_DIR` wins; empty string is
   treated as unset and falls back to `USERPROFILE`.
7. **Atomic write**: no `*.tmp` file remains after a successful install.

## Out of scope

- Making dispatch work from a session foreman never spawned (requires the
  `FOREMAN_*` env + control pipe; fundamentally an inside-foreman concern).
- Uninstall / cleanup on foreman removal.
- Non-Windows config-dir resolution.

## Touch list

- `src/skills_install.rs` (new) — embed, resolve, compare, atomic-write, cleanup.
- `src/main.rs` — call the installer once at startup before the eframe loop.
- Module doc comment records the `OBSOLETE_SKILLS` rename convention.
- `CLAUDE.md` / `docs/HANDOFF.md` — one line noting skills auto-install from
  `.claude/skills/` on startup (edit + rebuild propagates them).
