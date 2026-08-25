# Foreman — Claude Code Project Memory

Fast, native desktop for running many AI-agent terminal sessions ("tmux built
for AI"). Rust + egui, real PTYs (`portable-pty`/ConPTY), full terminal emulation
(`alacritty_terminal`). **Hard requirement: it must be fast** — native, not
Electron/Tauri. That constraint has already decided several arguments; see
**foreman-failure-archaeology** before reopening one.

This file is deliberately thin. The knowledge lives in project skills and
`docs/` — see the routing table below. `docs/HANDOFF.md` is the authoritative
deep doc (vision, architecture, module map, next phases) and wins on any
conflict.

## Read this before you touch anything

Three ways to destroy work that no amount of care downstream will undo:

- **⚠ `$env:FOREMAN` = `1` means you are running INSIDE the foreman app.** Do
  NOT `Stop-Process foreman` — you kill your own host, every other terminal in
  it, and yourself mid-command. Build without touching the running exe:
  `cargo build --target-dir target/agent`.
- **Kill by exe path, never by name.** `Stop-Process -Name foreman` also kills
  the user's *installed* foreman (`%LOCALAPPDATA%\Programs\foreman`), which
  looks like a crash to them (incident: 2026-07-15). Only a `target\`-built
  instance holds the link lock.
- **Never use `VoidListener`** in a `Session`. Shells send `ESC [ 6 n` (DSR) at
  startup and hang until the terminal replies; `Listener` captures
  `Event::PtyWrite` and `pump()` writes it back. Skipping it = black pane that
  never prompts.

Everything else that has cost hours is in **foreman-debugging-playbook** and
**foreman-failure-archaeology**. Check them before diagnosing from scratch.

## Invariants that outlive any single file

- **Recursive compositor.** One `WindowManager` engine runs at the desktop level
  and again *inside* each project (`Content::Project(Box<WindowManager>)`).
  Focus cascades so exactly one terminal reads the keyboard.
- **Window rects are LOCAL** to their manager's `area`. Screen rect =
  `rect.translate(area.min)`. Mixing the two is the #1 "painted in the wrong
  place" bug.
- **Two window states, never three.** Every window is either tiled (a leaf in
  the `LayoutTree` of H/V splits) or floating. Zoom is an overlay — the tree is
  untouched.
- **Tabs are level-restricted.** Any window can tab onto any other in the *same*
  `WindowManager`: projects with projects, terminals with terminals.
- **foreman renders on glow (OpenGL), not wgpu.** That is a deliberate,
  measured choice, not a leftover: Windows loses the GPU device on sleep and
  display power transitions, and `egui-wgpu` responds with an unconditional
  `panic!` in `update_buffers` that aborts the process. `egui_glow` only logs.
  Do not "modernize" the `eframe` line in `Cargo.toml` back to wgpu — read
  `docs/gpu-device-loss.md` first; it records the side-by-side test that
  settled it. Crash evidence is `%APPDATA%\foreman\foreman_panic.log`
  (absolute and timestamped).

Details, seam map, threading model, and the borrow rules: **foreman-architecture-contract**.

## Where to look

| When you are… | Use |
|---|---|
| Building, testing, or fighting the toolchain | **foreman-build-and-env** |
| Something is broken and you want the known cause | **foreman-debugging-playbook** |
| Deciding where code belongs / touching wm, layout, control, chat | **foreman-architecture-contract** |
| In `terminal.rs`, `input.rs`, `caret.rs`, `frame.rs`, or decoding VT/ConPTY | **terminal-emulation-reference** |
| Writing egui and it's behaving oddly | **egui-immediate-mode-reference** |
| Running the app or driving the `foreman` CLI / control plane | **foreman-run-and-operate** |
| Proving behavior with evidence (`foreman send`/`snapshot`, perf, panics) | **foreman-diagnostics-and-tooling** |
| Deciding whether a change is *done* / adding tests | **foreman-validation-and-qa** |
| Adding a dep, deleting code, touching wire format, committing | **foreman-change-control** |
| Touching settings, keybindings, env vars, persisted config | **foreman-config-and-flags** |
| Writing a doc, epic, spec, or commit message | **foreman-docs-and-writing** |
| Tempted to retry a settled battle (resize reflow, vsync, snap zones) | **foreman-failure-archaeology** |
| Forming a theory or designing an experiment | **foreman-research-methodology** |

Vocabulary is `CONTEXT.md` (ubiquitous language). Decisions are `docs/adr/`.
Feature docs are one-per-subsystem in `docs/` — check for an existing one before
adding a new file.

## Agent surfaces

- **Issues:** GitHub Issues on `sniffle6/foreman` via `gh`. External PRs are not
  a triage surface. See `docs/agents/issue-tracker.md`.
- **Triage labels:** `needs-triage`, `needs-info`, `ready-for-agent`,
  `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

## Working agreement

- Quality- and speed-obsessed user; no flattery, push back on bad ideas.
- Verify by building + screenshotting — never claim it works without evidence.
  The GUI cannot be seen from the terminal.
- Don't hijack the user's mouse/keyboard to test.
- Commit only when asked — but subagents commit their own work.
