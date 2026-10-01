---
name: foreman-agent-state-campaign
description: Use when touching per-Session agent state (working / needs you / idle, the done marker, the Sessions-panel badge), adding a provider's hook events, building anything on top of state (idle-aware chat delivery, "jump to next needs-you", a fleet overview), proving a state claim with evidence, or when tempted to detect state from PTY output, screen text, or an agent's private files. Also READY_GRACE / a Member that never latches Ready. Read docs/agent-state.md first; this skill holds the why, the evidence protocol, and the fenced wrong paths.
---

# Agent state per Session: why hooks won, how to prove a claim, what not to try

**Status: built and merged.** Agent state ships from the provider CLIs' own
lifecycle hooks (`foreman title-event --event <Name>` over the title pipe),
not from reading PTY bytes. Confirm with `rg -n "fn apply_hook_event" src/wm.rs`
and `rg -n "AgentStateSlot" src/agent_state.rs` — hits mean the feature is in
the tree. Read in this order:

1. `docs/agent-state.md` — what it does, the event-to-state table, how to turn
   it on, the gotchas, and `## Key files`. **The feature doc wins** over
   anything here.
2. `docs/superpowers/specs/2026-09-25-agent-state-design.md` — the decision,
   the rejected alternatives, and the hook-capture evidence it rests on. Read
   it for *why*; never edit it to match later reality.

This skill keeps only what the feature doc does not carry: the reasoning that
rules out passive detection, the identity gate, the evidence protocol for any
state claim, the injection-timing gotchas, the fenced wrong paths, and
READY_GRACE as a separate hardening item. The earlier output-based campaign
(signal audit, composite state machine, numeric gates) is demoted to the
fallback section at the end; it is a plan for agents with no hooks, not the
product route.

Citations name a file and a symbol, never a line number.

## 1. Why PTY bytes cannot show turn state

Foreman sees PTY bytes, not turn state. A *PTY* (pseudo-terminal) is the OS
byte pipe a program reads and writes; on Windows the layer is ConPTY. A
`Session` owns one PTY plus an emulated screen (`src/terminal.rs`; depth in
**terminal-emulation-reference**).

From that byte stream, "working" is visible (bytes flowing) and "exited" is
visible (`Session::exited`). The three states a human actually needs
separated — needs you, idle, done — all look the same from outside: a quiet
screen with the cursor parked at the agent's input box. Spinners make it
worse by keeping the screen changing while the agent is parked at a prompt.
That is the reason the spec rejected passive detection, and it is not a
matter of better thresholds: the information is not in the bytes.

The chat-mentions design names the same gap for injection timing:
*"'Between turns' is not observable from where foreman sits… inject at the
wrong moment and you corrupt an agent's in-flight work"*
(`docs/superpowers/specs/2026-06-10-chat-mentions-design.md`, the ⚠ section).
Hook events are the first signal that actually observes turn boundaries,
which is what makes idle-aware chat delivery possible at all.

Hooks are not free of blind spots. The feature doc's gotchas list them: Claude
Esc fires nothing, Codex sends a late `PostToolUse` after `Interrupt`, Grok
has no permission event. Design on top of state with those in mind; a badge
can be late or stale, never wrong about the direction it moved.

## 2. The identity gate

An event only lands on a Session when three things match: the Foreman project
ID, the terminal ID (terminal IDs repeat across projects), and the provider
of the event against `Session::icon_kind` (`WindowManager::apply_hook_event`,
`src/wm.rs`).

The icon comes from the process scan. `proc::agent_for` refreshes the OS
process table every `REFRESH_EVERY`, and the pure `detect_agent`
(`src/proc.rs`) ranks every agent process under the shell by depth and picks
the one closest to the shell. So a `codex exec` launched from Claude's Bash
tool is deeper, the pane keeps its Claude identity, and Codex's events are
dropped. Two consequences:

- **Process-tree identity is load-bearing, not corroborating.** The earlier
  campaign treated the process scan as a weak side signal. It is now the gate
  that decides which provider owns a pane. A wrong icon means dropped or
  misapplied state events, so a tab-icon bug is also a state bug
  (**foreman-diagnostics-and-tooling** covers debugging a wrong icon).
- **Same-provider children still overwrite the parent.** A `claude -p` inside
  a Claude Session shares the icon and the terminal ID, so its events change
  the pane's state. Accepted limitation; do not try to fix it with text
  sniffing.

The scan is WSL-blind (module docs in `src/proc.rs`): an agent inside a WSL
pane gets no icon, so it gets no state either.

## 3. Proving a state claim

Success is measured, never judged by eye. The shape:

1. **Label regimes by construction, never by reading the screen.** You know
   the agent is working because you submitted the prompt; you know it needs
   you because you asked for something that opens a permission dialog; you
   know it is idle because the turn finished and you waited. Drive a real
   agent from a PowerShell *outside* foreman using the Control plane
   (**foreman-run-and-operate** for the verbs).
2. **Pure mapping first.** `AgentStateSlot` (`src/agent_state.rs`) is a pure
   event-to-state function with tests. A new event, provider, or ranking rule
   gets a test there before anything touches the GUI. The exhaustive
   state-table shape is in **foreman-proof-and-analysis-toolkit**.
3. **End to end with evidence.** Screenshot the Sessions panel per regime
   (**build-screenshot**, user-run). Hook arrival itself is provable without
   the GUI: the capture recipe is in the spec's Evidence section.
4. **Asymmetric cost.** A false "needs you" interrupts a human for nothing and
   trains them to ignore the badge. A late one costs seconds. Any change to
   the mapping is judged by that asymmetry, not by overall accuracy.
5. **Zero bytes into any Session.** State is read from hook events only; the
   GUI only ever `try_recv`s. Any proposal that writes to a Session to learn
   its state is fenced below.

Vocabulary: Working, Needs you, Idle, and the done marker are in `CONTEXT.md`.
Use those names in code, docs, and commits.

## 4. Injection-timing gotchas

These bite anyone building on state (idle-aware delivery, "send when
ready") and anyone driving an agent for a fixture:

- **Prompt and Enter in two calls.** `foreman send` writes text then keys in
  one frame (`WindowManager::handle_ctrl`, `src/wm.rs`). Claude Code folds
  same-burst input into a paste and never submits. On the chat path the fix
  is the deferred submit, `SUBMIT_DELAY` in `src/ready.rs`; from the CLI,
  send `--text` and `--keys Enter` as two calls. The incident is in the
  const's doc comment and in **foreman-debugging-playbook**.
- **Expect `send` to wait.** While the agent streams, each reply takes up to
  `MAX_SETTLE_MS` (`src/wm.rs`); the settle deadline caps the wait.
- **Ready gates every injection.** `inject_input` queues until the Session
  latches Ready (`ReadyGate`, `src/ready.rs`); the Outbox skips non-Ready
  Members (`ChatRoom::tick`, `src/chat.rs`). State events do not change that:
  Idle means the agent is at its input box, not that the Session is Ready.
- **Idle is not "safe to type".** A late `Stop` or an invisible Claude Esc
  means Idle can lag reality. Treat state as advisory for delivery timing and
  keep the existing settle-based path as the floor.

## 5. Fenced wrong paths — do not attempt

| Fenced path | Why | Citation |
|---|---|---|
| Keyword-sniffing screen or output text for state ("Working…", "Allow?") | Agent UIs restyle across versions; the same fragility was rejected for chat kinds | `docs/chat-missing-features.md` "Explicitly NOT recommended" |
| Parsing an agent's private state or session files | Another tool's private format rots under you; agent-teams integration was rejected for exactly this | same section |
| Active interrogation — writing bytes into a Session to see how it reacts | You cannot know it is safe to write without the state you are trying to learn | mentions-design ⚠ section; the `SUBMIT_DELAY` incident |
| Driving the agent through a structured protocol instead of the TUI | Foreman hosts the real interactive TUI; that route only works when you own the conversation | spec, Rejected alternatives |
| Reading a provider's hook config to decide whether hooks are trusted | The icon check already covers "not installed", "not trusted", and "exited to shell" without reading private config | spec, States |

## 6. READY_GRACE — a separate hardening item

A Session whose child never answers the startup DSR never latches Ready, so
`inject_input` queues forever and the Outbox never delivers. The remedy is a
timeout fallback latch, `READY_GRACE`, designed in
`docs/followups-latency-and-control.md` §2 and still unbuilt. It is **not a
gate on agent-state work** and never was a dependency of the hook design.

What a builder needs to know:

- **Placement is ruled.** `src/ready.rs`'s module doc says the grace is
  deliberately not in the pure gate, because the gate never reads the clock.
  It belongs in the caller that owns the clock, feeding the gate an injected
  `now`. Make the grace injectable so the path is deterministically testable.
- **The pre-check is mandatory.** The existing Ready tests use `cmd.exe /c
  pause`, which *does* emit DSR. A grace-path test needs a child that
  provably never DSRs; prove that first with a huge grace and assert
  `ready()` stays false.
- **Probe carefully.** Plain `rg READY_GRACE src/` false-passes because the
  comment names it. Use `rg -n "const READY_GRACE" src/` — expect nothing
  until it is built.

Route the change through **foreman-change-control**; the frontier framing is
in **foreman-research-frontier**.

## 7. Fallback: output-based detection for agents with no hooks

Only for an agent CLI that exposes no lifecycle hooks. Not the product route,
and it cannot deliver Needs you (section 1). What it can honestly deliver is a
three-bin detector: working (bytes flowing), quiet, exited.

- **Inputs that already exist in-process:** `Session::output_gen`
  (`src/terminal.rs`), the pure quiescence `settle_tick` (`src/wm.rs`) with
  its user-editable window `Settings::send_settle_ms` (`src/config.rs`), the
  cursor from `snapshot --cursor` (`src/inspect.rs`), and `Session::exited`.
  The old cursor-rest gate was deleted; `git show 7fda1c2:src/caret.rs` is the
  worked example if you need resting-vs-redrawing again.
- **Shape:** a pure module fed observations per frame, unit-tested against
  recorded fixtures from labeled regimes (section 3), with every constant
  injectable. Predict first, then measure (**foreman-research-methodology**).
- **Gates before it paints anything:** zero false "needs you" over a long
  streaming run, a flap bound of one transition per constructed transition,
  and an acid run that behaves identically to a control run — trivially true
  because the detector writes zero bytes. Numbers are user sign-off items.
- **OSC 133 prompt marks** are a candidate signal for plain shells only
  (`docs/warp-feature-candidates.md` #1); they stop flowing the moment a TUI
  agent starts.

If the fallback ever grows a "needs you" bin, that is a scope change: stop
and get sign-off.

## When NOT to use this skill

- Running `send`/`snapshot`/`status` or looking up CLI flags →
  **foreman-run-and-operate**.
- A Session that is actually broken (black pane, swallowed input) →
  **foreman-debugging-playbook**.
- Hook file layout, `CLAUDE_CONFIG_DIR`, or the installer setting →
  **foreman-config-and-flags**.

## Provenance and maintenance

| Claim | Re-verify |
|---|---|
| Hook-driven state is in the tree | `rg -n "fn apply_hook_event" src/wm.rs; rg -n "AgentStateSlot" src/agent_state.rs` — hits expected |
| The icon gate ranks by depth under the shell | `rg -n "fn detect_agent" src/proc.rs` and read its doc comment |
| READY_GRACE still unbuilt | `rg -n "const READY_GRACE" src/` — expect nothing |
| The settle default is a setting under a hard cap | `rg -n "send_settle_ms" src/config.rs; rg -n "MAX_SETTLE_MS" src/wm.rs` |
| Quiescence gating still unsolved in the mentions design | `rg -n "unsolved" docs/superpowers/specs/2026-06-10-chat-mentions-design.md` |
| Codex-sets-username title quirk | `rg -n "username" docs/tab-icons.md` |
