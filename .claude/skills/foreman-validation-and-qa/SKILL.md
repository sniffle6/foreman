---
name: foreman-validation-and-qa
description: Use when deciding whether a foreman change is done or a claim is proven — what evidence a GUI, terminal-behavior, wire-protocol, or performance claim requires; when adding, naming, or locating tests; when cargo test is red or flaky (e.g. human_post_appends..., bytes eaten before Ready, DSR races); before running the terminal acid test (Claude Code, vim, lazygit, less); or when unsure if VoidListener belongs in a test.
---

# Foreman Validation and QA

What counts as evidence, the acceptance gates a change must pass, the test
conventions, and the flaky-test policy.

Vocabulary is CONTEXT.md's: a **Session** is one running terminal (a live shell
or agent process plus its emulated screen); **Ready** is the state a Session
reaches once it has answered the program's startup device-status query —
injected input only lands after Ready; a **Snapshot** is a Session's rendered
screen captured as data. For the underlying terminal machinery (PTY, ConPTY,
DSR, alacritty_terminal) see **terminal-emulation-reference**.

## What counts as evidence

Match the claim to the required evidence. "It compiles", "works on my machine",
and "the code looks right" are never evidence (CLAUDE.md working agreement:
"Verify by building + screenshotting — don't claim it works without evidence").

| Claim type | Required evidence | How |
|---|---|---|
| GUI / visual ("the header renders", "the tab is colored") | A screenshot of the running app, **read back as an image** | The repo-local **build-screenshot** skill (`.claude/skills/build-screenshot/`). If you are headless and cannot screenshot, SAY SO and state that the user must run and eyeball it — do not claim the visual behavior. |
| Terminal behavior ("keys reach vim", "the post landed") | `foreman send` + `foreman snapshot` output over the Control plane (Inspection) | Verbs/flags: **foreman-run-and-operate**. Measurement recipes: **foreman-diagnostics-and-tooling**. `send` returns a Quiescence-settled screen, so the output is a settled Snapshot, not a mid-update race. |
| Wire-protocol change (Control plane request/reply JSON) | A byte-compat test asserting v1 byte-identity | Precedent: `chat_request_to_is_wire_compatible_with_v1` (`src/control.rs`) — asserts an untargeted request serializes with **no** `"to"` key (byte-identical to v1) and that a v1 payload still parses. Copy this shape for any new optional field. |
| Performance ("it's faster now") | Measured numbers, date-stamped | Harness patterns live in **foreman-diagnostics-and-tooling** (see also `docs/followups-latency-and-control.md` for the temp-instrument precedent). Quote numbers with dates; never "feels faster". |
| Pure-logic change | A unit test in the module's `#[cfg(test)] mod tests` | See conventions below. |

## Acceptance gates (in order)

Run these before calling any change done. What CI does and does not gate is
**foreman-build-and-env**'s; the short version is that nothing runs on an
ordinary push, so these gates are enforced by discipline, the repo hooks, and
the `foreman-reviewer` agent (`.claude/agents/foreman-reviewer.md`). Do not
claim "CI will catch it".

1. **`cargo check` clean** modulo the known warning baseline (baseline and
   toolchain traps: **foreman-build-and-env**).
2. **`cargo test` green** modulo *declared* in-flight WIP. Any red test is
   yours to explain: check `git log` for a declared-WIP note first, then treat
   it as your regression.
3. **Evidence per claim type** (table above).
4. **Feature doc** in `docs/` — update an existing doc for the same area rather
   than adding a new one; style and trust map: **foreman-docs-and-writing**.
5. **Commit only when asked** (CLAUDE.md working agreement). Classification and
   review gates: **foreman-change-control**.

The hooks (`.claude/settings.json`): PreToolUse on Bash runs
`kill-foreman.ps1` (kills a running `foreman.exe` before any `cargo
build|run|test` so the link doesn't fail with `Access is denied (os error 5)`);
PostToolUse on Edit|Write of `.rs` files runs `cargo fmt`.

## The acid test (any terminal-touching change)

From `docs/epics/terminal-completeness-epic.md`, § "The acid test". Open these
inside a foreman Session and actually use them, before and after the change:

1. **Claude Code** and **Codex CLI** — colors, styled text, key handling,
   smoothness under streaming output.
2. **vim** — `:syntax on` (colors + bold), F-keys, `Alt+b`/`Alt+f`, mouse click
   to position cursor, `i`-mode cursor shape.
3. **lazygit** / **htop** — mouse click and scroll on the UI.
4. **less** / **man** — bold headings, `/pattern` search inside the app.

The epic's rule, verbatim: "If the agent CLIs themselves degrade, that outranks
everything else here." Don't claim a terminal phase done without running its
acid-test app.

## Where the tests are

`rg -c '#\[test\]' src/ | sort -t: -k2 -rn` — all tests are module-local
`#[cfg(test)] mod tests`; no `tests/` directory.

Run per-module for speed: `cargo test wm::`, `cargo test layout::`. **Not**
`--lib` — `foreman` is bin-only and that form errors; see
**foreman-build-and-env**.

## Test conventions (copy these precedents)

**Pure-seam extraction is THE testing strategy.** Logic entangled with the GUI
gets carved into a pure module whose interface IS the test surface. Worked
examples in-tree: `input.rs` (byte-equality key/mouse encoding; module doc: "no
dependency on the GUI, the PTY, or the Session"), the chat Outbox
(`ChatRoom::tick` returns exactly the framed lines each Ready Member still
needs — pure, so the delivery guarantee tests need no terminal), `settle_tick`
(`src/wm.rs` — Quiescence settle as a pure function), the Ready gate
(`src/ready.rs`), `layout.rs`, `geom.rs` (Cell metrics) and `frame.rs`
(paint plan). The extraction recipe itself is
**foreman-proof-and-analysis-toolkit**'s.

**Transition-table contract tests.** Encode a state machine's full behavior
table as named cases. Historical precedent:
`compose_zone_matches_full_transition_table`, deleted with the Zone machinery
in the 2026-06-11 tree migration — `docs/snap-tiling.md` still shows the
pattern behind its own SUPERSEDED banner. The living example of the shape is
`src/ready.rs`'s gate tests (`ready_needs_both_dsr_and_paint`, …): every arm of
the machine gets its own named case.

**Wire round-trips against a real in-process server.** `pipe_roundtrip`
(`src/control.rs`) spawns `serve()` on a background thread with a unique
pipe name (`foreman-test-{pid}` so parallel runs and a live foreman don't
collide), fakes the GUI thread on a channel, and **retries while the listener
binds** — "no sleep-and-hope". Copy it for any new Control plane verb.

**Serde forward/backward compat.** `config.rs`: missing fields fall
back to defaults, known fields round-trip, unknown fields from a newer foreman
are ignored. Required for any persisted settings struct.

**Naming.** Test names are behavior sentences in snake_case
(`plan_clamps_stale_metrics_to_grid_bounds`), stating the contract, not the
function called.

## PTY-test discipline (Session-level tests)

The one domain fact you need (details in **terminal-emulation-reference**):
shells send a device-status query (DSR, `ESC [ 6 n`) at startup and scan for
the reply. **Bytes injected before a Session is Ready get eaten by that scan.**
`Session::ready()` (`src/terminal.rs`) reads the gate in `src/ready.rs`, which
latches inside `pump()` — so a test that never pumps never gets a Ready
Session.

Rules, each backed by an in-tree precedent:

- **Canonical fresh-Session pattern** — `inject_input_reaches_child_stdin`
  (`src/terminal.rs`): spawn `cmd /c pause` (exits on any stdin byte, making
  delivery observable), loop `pump()` until the prompt renders (proof the DSR
  exchange resolved), inject, then loop `pump()` until `exited()`. Deadlines on
  both loops (10 s), 10–50 ms sleeps, never a bare sleep-and-hope.
- **Chat-delivery pattern** — `chat_broadcast_hits_members_only_excluding_sender`
  and `human_post_appends_with_reserved_id_and_broadcasts_to_all_members`
  (src/wm.rs): each iteration, `keepalive()` every Session (pumps, latching
  Ready) then `wm.chat_tick()` (the Outbox delivers only to Ready Members),
  until the observable effect lands — deadline-bounded loop.
- **`inject_input` now queues pre-Ready bytes** (`pending_inject` in
  `src/ready.rs`, flushed once the gate latches) — the product fix the
  2026-06-11 flake plan filed as future work has since landed. Tests must still
  pump in a loop (nothing else latches Ready in a test), and the raw `feed()`
  path used by `foreman send` bypasses the queue entirely.
- **NEVER serialize the suite to hide a race.** The full diagnosis is
  `docs/plans/2026-06-11-fix-flaky-chat-broadcast-test.md`: the flake appeared
  only under full-suite parallel load (DSR resolving late under dozens of
  concurrent conhost spawns); serializing "would only hide the race". Fix the
  test's delivery loop, not the scheduler.
- **`VoidListener` is fine for driven `Term` fixtures, never for live
  Sessions.** Several test modules build `Term<VoidListener>` fed fixed bytes
  — no PTY, no window, correct by design (the `src/inspect.rs` module doc spells
  out why the seam is generic over the listener). A live
  Session with `VoidListener` drops the DSR reply path: black screen, shell
  hangs forever (the CLAUDE.md "DSR trap"). Triage for that symptom:
  **foreman-debugging-playbook**.

### Flaky policy

A flake is a synchronization bug in the test. Do not: retry in CI-style loops,
lengthen deadlines blindly, or serialize. Do: reproduce under full-suite
parallel load, root-cause against the discipline above, fix the loop. The
evidence bar for "flake gone" (precedent: the 2026-06-11 plan, whose failure
reproduced in nearly every full run): **three consecutive green full runs** —

```powershell
1..3 | ForEach-Object { cargo test 2>&1 | Select-String 'test result' }
```

## How to add a test

1. Put it in the module's `#[cfg(test)] mod tests` block (bottom of the file).
2. Prefer driving a pure seam. If the logic is entangled with the GUI, extract
   the seam first (**foreman-proof-and-analysis-toolkit**; name it with
   CONTEXT.md's seam vocabulary).
3. For Session-level behavior, reuse the fixtures: `pause_argv()`
   (`src/wm.rs` test module, `cmd.exe /c pause`) with `wm.add_terminal_cmd(...)` or
   `Session::spawn_argv(&pause_argv(), None, &[], ctx)`;
   `egui::Context::default()` works headless. Follow the PTY discipline above.
4. Run the module, not the world: `cargo test control::` (full suite for the
   final gate).

## Operational constraints while testing

- While a **release fleet** is running (`target\release\foreman.exe` in daily
  use): debug `cargo test` is safe — the test harness links its own binary and
  doesn't touch the release exe. **Never `cargo test --release`, never kill the
  fleet** (documented in `docs/plans/2026-06-11-fix-flaky-chat-broadcast-test.md`).
- **Gotcha:** the PreToolUse hook (`.claude/hooks/kill-foreman.ps1`) kills
  foreman when a Bash command matches `cargo (build|run|test)`. It filters by
  exe path — only instances run from *this repo's* `target\` dir — so an
  installed foreman survives, but a **release fleet built from this repo does
  not**. It also no-ops entirely when `FOREMAN=1` (you are inside foreman). If
  such a fleet must stay alive, reconcile with the user before running cargo
  through the Bash tool; the reasons behind both fences are in
  **foreman-build-and-env**.
- Many tests in `wm.rs` and `terminal.rs` spawn real ConPTY children
  (`cmd /c pause` fixtures); they are load-sensitive by nature — that is
  exactly why the flaky policy exists.

## When NOT to use this skill

- Measurement *mechanics* — how to drive `foreman send`/`snapshot`, the
  screenshot script, the latency harness → **foreman-diagnostics-and-tooling**.
  This skill says what evidence a claim needs; that one says how to produce it.
- Build breaks, toolchain, the warning baseline, what CI gates →
  **foreman-build-and-env**.

## Provenance and maintenance

The claims here that are both load-bearing and prone to rot, with the command
that settles each:

| Claim | Re-verify (repo root) |
|---|---|
| `VoidListener` never in live-Session code | `rg -n 'VoidListener' src/` — every hit must sit inside a `#[cfg(test)]` module or a `Term` fixture (the `src/inspect.rs` module doc `//!` mentions it in prose — that one is expected, not a violation); one in a live `Session` is the DSR black-screen trap |
| Pre-Ready inject queue still exists | `rg -n 'pending_inject' src/` |
| Hook kill behavior (path filter, `FOREMAN=1` no-op) | `Get-Content .claude/hooks/kill-foreman.ps1` |
| Acid-test list unchanged | read § "The acid test" in `docs/epics/terminal-completeness-epic.md` |
