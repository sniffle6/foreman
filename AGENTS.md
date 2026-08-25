# Foreman - Codex Project Guide

Fast, native desktop for running many AI-agent terminal sessions ("tmux built
for AI"). Rust + egui, real PTYs (`portable-pty`/ConPTY), full terminal emulation
(`alacritty_terminal`). Hard requirement: it must be fast: native, not
Electron/Tauri. That constraint has already settled several arguments; check
`.claude/skills/foreman-failure-archaeology/SKILL.md` before reopening one.

This file is deliberately thin. The knowledge lives in the reference
library under `.claude/skills/` and in `docs/` - see the routing table below.
`docs/HANDOFF.md` is the authoritative deep doc (vision, architecture, complete
module map, next phases) and wins on any conflict.

**About `.claude/skills/`:** despite the directory name, that library is not
Claude-specific and is not off-limits to you. It is the project's reference
material, kept in one place so it cannot drift into two copies. Read the
`SKILL.md` files directly with your file tools. `.codex/skills/` holds only the
few skills that need Codex-flavored commands (`codex exec` rather than
`claude -p`).

## Read this before you touch anything

Three ways to destroy work that care downstream will not undo:

- **If `$env:FOREMAN` is `1`, you are running INSIDE the foreman app.** Do NOT
  `Stop-Process foreman` - you kill your own host, every other terminal in it,
  and yourself mid-command. Build without touching the running exe:
  `cargo build --target-dir target/agent`.
- **Kill by exe path, never by name.** `Stop-Process -Name foreman` also kills
  the user's *installed* foreman (`%LOCALAPPDATA%\Programs\foreman`), which
  looks like a crash to them (incident: 2026-07-15). Only a `target\`-built
  instance holds the link lock.
- **Never use `VoidListener`** in a `Session`. Shells send `ESC [ 6 n` (DSR) at
  startup and hang until the terminal replies; `Listener` captures
  `Event::PtyWrite` and `pump()` writes it back. Skipping it means a black pane
  that never prompts.

Everything else that has cost hours is in
`.claude/skills/foreman-debugging-playbook/SKILL.md` and
`.claude/skills/foreman-failure-archaeology/SKILL.md`. Read those before
diagnosing from scratch.

## Invariants that outlive any single file

- **Recursive compositor.** One `WindowManager` engine runs at the desktop level
  and again *inside* each project (`Content::Project(Box<WindowManager>)`).
  Focus cascades so exactly one terminal reads the keyboard.
- **Window rects are LOCAL** to their manager's `area`. Screen rect is
  `rect.translate(area.min)`. Mixing the two is the top cause of "painted in the
  wrong place" bugs.
- **Two window states, never three.** Every window is either tiled (a leaf in
  the `LayoutTree` of H/V splits) or floating. Zoom is an overlay; the tree is
  untouched.
- **Tabs are level-restricted.** Any window can tab onto any other in the *same*
  `WindowManager`: projects with projects, terminals with terminals.

Seam map, threading model, and borrow rules:
`.claude/skills/foreman-architecture-contract/SKILL.md`.

## Where to look

Paths below are files to read, not skills to invoke.

| When you are... | Read |
|---|---|
| Building, testing, or fighting the toolchain | `.claude/skills/foreman-build-and-env/SKILL.md` |
| Something is broken and you want the known cause | `.claude/skills/foreman-debugging-playbook/SKILL.md` |
| Deciding where code belongs / touching wm, layout, control, chat | `.claude/skills/foreman-architecture-contract/SKILL.md` |
| In `terminal.rs`, `input.rs`, `caret.rs`, `frame.rs`, or decoding VT/ConPTY | `.claude/skills/terminal-emulation-reference/SKILL.md` |
| Writing egui and it behaves oddly | `.claude/skills/egui-immediate-mode-reference/SKILL.md` |
| Running the app or driving the `foreman` CLI / control plane | `.claude/skills/foreman-run-and-operate/SKILL.md` |
| Proving behavior with evidence (`foreman send`/`snapshot`, perf, panics) | `.claude/skills/foreman-diagnostics-and-tooling/SKILL.md` |
| Deciding whether a change is *done* / adding tests | `.claude/skills/foreman-validation-and-qa/SKILL.md` |
| Adding a dep, deleting code, touching wire format, committing | `.claude/skills/foreman-change-control/SKILL.md` |
| Touching settings, keybindings, env vars, persisted config | `.claude/skills/foreman-config-and-flags/SKILL.md` |
| Writing a doc, epic, spec, or commit message | `.claude/skills/foreman-docs-and-writing/SKILL.md` |
| Tempted to retry a settled battle (resize reflow, vsync, snap zones) | `.claude/skills/foreman-failure-archaeology/SKILL.md` |
| Forming a theory or designing an experiment | `.claude/skills/foreman-research-methodology/SKILL.md` |
| Verifying GUI behavior with a screenshot | `.codex/skills/build-screenshot` |

Vocabulary is `CONTEXT.md` (ubiquitous language). Decisions are `docs/adr/`.
Feature docs are one-per-subsystem in `docs/`; check for an existing one before
adding a new file.

## Agent surfaces

- **Issues:** GitHub Issues on `sniffle6/foreman` via `gh`. External PRs are not
  a triage surface. See `docs/agents/issue-tracker.md`.
- **Triage labels:** `needs-triage`, `needs-info`, `ready-for-agent`,
  `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

## Paired skill copies

`foreman-dispatch`, `foreman-chat`, and `foreman-icat` exist in BOTH
`.claude/skills/` and `.codex/skills/`, because `src/skills_install.rs` embeds
them and installs them into the Claude and Codex global skill dirs at startup so
agents in any repo can find them. When you change dispatch or chat behavior:
edit both copies, keep them semantically identical, adapt only the command
examples (`claude` / `claude -p` versus `codex` / `codex exec`), then rebuild to
propagate. `build-screenshot` is twinned in both dirs, embedded in neither; the
Claude copy carries `disable-model-invocation: true`, the Codex copy does not.

## Working Agreement

- Quality- and speed-obsessed user; no flattery, push back on bad ideas.
- Verify with build/tests, and with screenshots when GUI behavior changes. Never
  claim visual behavior works without image evidence; the GUI cannot be
  inspected from terminal output.
- Do not needlessly hijack the user's mouse or keyboard to test.
- Keep edits scoped; avoid unrelated refactors.
- Commit only when asked.
