# Agent State in the Sessions Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show Working / Needs you / Idle (plus a "done" marker) for Claude, Codex, and Grok Sessions in the Sessions panel, driven by provider lifecycle hooks.

**Architecture:** Foreman installs guarded global hooks that run `foreman title-event --agent X --kind state`. The helper normalizes the hook payload to a tiny `StateEvent` and sends it over the existing private title pipe. The pipe server routes state messages into their own bounded channel; the GUI drains it and applies each event to a pure `AgentStateSlot` stored on the matching `Session`. The panel reads a badge from the slot.

**Tech Stack:** Rust, egui 0.34, serde_json, interprocess local sockets (existing).

**Spec:** `docs/superpowers/specs/2026-09-25-agent-state-design.md`

## Global Constraints

- The feature writes **zero bytes** into any Session. It only reads hook events.
- Event receipt never blocks the GUI thread: pipe reads stay on the server threads in `title_notify::serve`; the GUI only calls `try_recv`.
- State messages carry event kind and IDs only — never prompt text, tool input, or tool output.
- Claude and Codex state hooks run with `"async": true`.
- New setting `agent_state_badges` defaults to `false`. Disabling it hides all badges; it does not uninstall hooks (same policy as `auto_name_agent_sessions`).
- A Session shows no badge until an event from it is accepted, while its process has exited, or while its detected agent (`Session::icon_kind`) is not the event's provider.
- Build only with `cargo build --target-dir target/agent` / `cargo test --target-dir target/agent`. `$env:FOREMAN` is `1` in agent terminals: never `Stop-Process foreman`, never kill by name.
- Do not touch the `eframe` line in `Cargo.toml`.
- Commits: `type(scope): subject`, body says why, trailer `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`. Use `git commit -F <file>` for multi-line messages; verify with `git log -1 --format=%B`.

## Review Focus

- **Huge `PostToolUse` payloads.** A tool that printed megabytes still must deliver its state event; the naming helper's 64 KB stdin cap would silently drop it and pin the badge at Needs you. Test in Task 2.
- **Subagent events.** A subagent's `Stop`/`PostToolUse` must not change the main Session's state; a subagent's `PermissionRequest` must, because the human still has to answer it. Test in Task 2.
- **Reinstall order.** Installing naming hooks after state hooks (or the reverse, or both again) must leave the hook files byte-identical, or Codex demands re-trust of every hook. Test in Task 3.
- **Pane reuse.** After `claude` exits and a new agent session starts in the same terminal, late events carrying the old vendor session ID must be ignored. Test in Task 1.
- **Agent exits back to the shell without `SessionEnd`** (crash, Ctrl+C twice, pane still alive). The badge must disappear once the Session's icon is no longer that agent. Test in Task 1.

---

### Task 1: Pure state slot (`src/agent_state.rs`)

**Files:**
- Create: `src/agent_state.rs`
- Modify: `src/main.rs` (add `mod agent_state;` beside the other `mod` lines)

**Interfaces:**
- Consumes: `crate::terminal_titles::SourceAgent` (`Copy`, `label()`), `crate::icons::IconKind` (`agent_label()`).
- Produces:
  - `pub enum AgentState { Working, NeedsYou, Idle }`
  - `pub enum StateSignal { SessionStart, PromptSubmit, PermissionRequest, ToolDone, TurnEnd, Interrupt, SessionEnd }` (serde, snake_case)
  - `pub fn signal_for(hook_event_name: &str) -> Option<StateSignal>`
  - `pub struct AgentBadge { pub state: AgentState, pub finished: bool }`
  - `pub fn badge_label(badge: AgentBadge) -> &'static str`
  - `pub struct AgentStateSlot` with `apply(&mut self, source: SourceAgent, session_id: &str, signal: StateSignal)`, `clear_finished(&mut self)`, `badge(&self, icon: IconKind, exited: bool) -> Option<AgentBadge>`

- [ ] **Step 1: Write the failing tests**

Create `src/agent_state.rs` containing only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::IconKind;
    use crate::terminal_titles::SourceAgent::{Claude, Codex};
    use StateSignal::*;

    fn run(slot: &mut AgentStateSlot, source: SourceAgent, id: &str, signals: &[StateSignal]) {
        for s in signals {
            slot.apply(source, id, *s);
        }
    }

    fn badge(slot: &AgentStateSlot, icon: IconKind) -> Option<AgentBadge> {
        slot.badge(icon, false)
    }

    const WORKING: Option<AgentBadge> = Some(AgentBadge { state: AgentState::Working, finished: false });
    const NEEDS: Option<AgentBadge> = Some(AgentBadge { state: AgentState::NeedsYou, finished: false });
    const IDLE: Option<AgentBadge> = Some(AgentBadge { state: AgentState::Idle, finished: false });
    const DONE: Option<AgentBadge> = Some(AgentBadge { state: AgentState::Idle, finished: true });

    #[test]
    fn hook_names_map_to_signals() {
        assert_eq!(signal_for("SessionStart"), Some(SessionStart));
        assert_eq!(signal_for("UserPromptSubmit"), Some(PromptSubmit));
        assert_eq!(signal_for("PermissionRequest"), Some(PermissionRequest));
        assert_eq!(signal_for("PostToolUse"), Some(ToolDone));
        assert_eq!(signal_for("Stop"), Some(TurnEnd));
        assert_eq!(signal_for("StopFailure"), Some(TurnEnd));
        assert_eq!(signal_for("Interrupt"), Some(Interrupt));
        assert_eq!(signal_for("SessionEnd"), Some(SessionEnd));
        assert_eq!(signal_for("PreToolUse"), None);
        assert_eq!(signal_for("Notification"), None);
    }

    #[test]
    fn a_normal_turn_goes_working_then_done() {
        let mut slot = AgentStateSlot::default();
        assert_eq!(badge(&slot, IconKind::Claude), None, "no event yet = no badge");
        run(&mut slot, Claude, "s1", &[SessionStart]);
        assert_eq!(badge(&slot, IconKind::Claude), IDLE);
        run(&mut slot, Claude, "s1", &[PromptSubmit, ToolDone]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING);
        run(&mut slot, Claude, "s1", &[TurnEnd]);
        assert_eq!(badge(&slot, IconKind::Claude), DONE);
        slot.clear_finished();
        assert_eq!(badge(&slot, IconKind::Claude), IDLE);
        run(&mut slot, Claude, "s1", &[TurnEnd, PromptSubmit]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING, "a new prompt clears done");
    }

    #[test]
    fn permission_request_needs_you_until_the_tool_finishes() {
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, "s1", &[PromptSubmit, PermissionRequest]);
        assert_eq!(badge(&slot, IconKind::Claude), NEEDS);
        run(&mut slot, Claude, "s1", &[ToolDone]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING);
    }

    #[test]
    fn tool_done_never_wakes_an_idle_session() {
        // Codex delivers PostToolUse ~1.5s after Interrupt (captured 2026-09-25).
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Codex, "c1", &[PromptSubmit, Interrupt, ToolDone]);
        assert_eq!(badge(&slot, IconKind::Codex), IDLE, "interrupt is not a finished turn");
        run(&mut slot, Codex, "c1", &[TurnEnd, ToolDone]);
        assert_eq!(badge(&slot, IconKind::Codex), DONE);
    }

    #[test]
    fn session_start_does_not_reset_a_turn_in_progress() {
        // Claude re-fires SessionStart after compaction, mid-turn.
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, "s1", &[PromptSubmit, SessionStart]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING);
    }

    #[test]
    fn a_new_session_replaces_the_old_one_and_stale_events_are_dropped() {
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, "old", &[PromptSubmit, TurnEnd]);
        run(&mut slot, Claude, "new", &[SessionStart]);
        assert_eq!(badge(&slot, IconKind::Claude), IDLE);
        run(&mut slot, Claude, "old", &[PermissionRequest, ToolDone, TurnEnd]);
        assert_eq!(badge(&slot, IconKind::Claude), IDLE, "late events from the old session are ignored");
    }

    #[test]
    fn session_end_clears_and_the_next_event_adopts() {
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, "s1", &[PromptSubmit, SessionEnd]);
        assert_eq!(badge(&slot, IconKind::Claude), None);
        run(&mut slot, Codex, "c1", &[PromptSubmit]);
        assert_eq!(badge(&slot, IconKind::Codex), WORKING);
    }

    #[test]
    fn badge_hides_when_exited_or_the_agent_is_no_longer_running() {
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, "s1", &[PromptSubmit]);
        assert_eq!(slot.badge(IconKind::Claude, true), None, "exited process");
        assert_eq!(badge(&slot, IconKind::PowerShell), None, "agent exited back to the shell");
        assert_eq!(badge(&slot, IconKind::Codex), None, "a different agent now runs here");
    }

    #[test]
    fn labels_follow_state_and_finished() {
        let labels: Vec<&str> = [WORKING, NEEDS, IDLE, DONE]
            .into_iter()
            .map(|b| badge_label(b.unwrap()))
            .collect();
        // Every state picks a distinct label; done differs from plain idle.
        let mut unique = labels.clone();
        unique.dedup();
        assert_eq!(unique.len(), 4, "{labels:?}");
    }
}
```

Add `mod agent_state;` to `src/main.rs` next to `mod agent_hooks;`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent agent_state`
Expected: compile errors — `AgentStateSlot`, `StateSignal`, `signal_for` not found.

- [ ] **Step 3: Implement**

Put this above the test module in `src/agent_state.rs`:

```rust
//! Per-Session agent state (Working / Needs you / Idle) driven by provider
//! lifecycle hooks. Pure: fed normalized signals, holds no I/O and never
//! writes to the Session. Design: docs/superpowers/specs/2026-09-25-agent-state-design.md.

use crate::icons::IconKind;
use crate::terminal_titles::SourceAgent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    NeedsYou,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSignal {
    SessionStart,
    PromptSubmit,
    PermissionRequest,
    ToolDone,
    TurnEnd,
    Interrupt,
    SessionEnd,
}

/// Map a provider's `hook_event_name` to a signal. Unknown names are ignored.
pub fn signal_for(hook_event_name: &str) -> Option<StateSignal> {
    Some(match hook_event_name {
        "SessionStart" => StateSignal::SessionStart,
        "UserPromptSubmit" => StateSignal::PromptSubmit,
        "PermissionRequest" => StateSignal::PermissionRequest,
        "PostToolUse" => StateSignal::ToolDone,
        "Stop" | "StopFailure" => StateSignal::TurnEnd,
        "Interrupt" => StateSignal::Interrupt,
        "SessionEnd" => StateSignal::SessionEnd,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentBadge {
    pub state: AgentState,
    /// The last turn ended and the Session has not had keyboard focus since.
    pub finished: bool,
}

pub fn badge_label(badge: AgentBadge) -> &'static str {
    match (badge.state, badge.finished) {
        (AgentState::NeedsYou, _) => "needs you",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, true) => "done",
        (AgentState::Idle, false) => "idle",
    }
}

#[derive(Clone, Debug, Default)]
pub struct AgentStateSlot {
    source: Option<SourceAgent>,
    session: Option<String>,
    state: Option<AgentState>,
    finished: bool,
}

impl AgentStateSlot {
    pub fn apply(&mut self, source: SourceAgent, session_id: &str, signal: StateSignal) {
        let same = self.source == Some(source) && self.session.as_deref() == Some(session_id);
        if !same {
            let starts = matches!(signal, StateSignal::SessionStart | StateSignal::PromptSubmit);
            // An occupied slot only changes hands on a start signal, so late
            // events from an earlier agent in this pane are dropped.
            if self.session.is_some() && !starts {
                return;
            }
            *self = Self {
                source: Some(source),
                session: Some(session_id.to_owned()),
                ..Self::default()
            };
        }
        match signal {
            StateSignal::SessionStart => {
                if self.state.is_none() {
                    self.state = Some(AgentState::Idle);
                }
            }
            StateSignal::PromptSubmit => {
                self.state = Some(AgentState::Working);
                self.finished = false;
            }
            StateSignal::PermissionRequest => self.state = Some(AgentState::NeedsYou),
            // Only leaves Needs you: Codex sends PostToolUse after Interrupt.
            StateSignal::ToolDone => {
                if self.state == Some(AgentState::NeedsYou) {
                    self.state = Some(AgentState::Working);
                }
            }
            StateSignal::TurnEnd => {
                self.state = Some(AgentState::Idle);
                self.finished = true;
            }
            StateSignal::Interrupt => self.state = Some(AgentState::Idle),
            StateSignal::SessionEnd => *self = Self::default(),
        }
    }

    pub fn clear_finished(&mut self) {
        self.finished = false;
    }

    pub fn badge(&self, icon: IconKind, exited: bool) -> Option<AgentBadge> {
        if exited {
            return None;
        }
        let source = self.source?;
        let state = self.state?;
        (icon.agent_label() == Some(source.label())).then_some(AgentBadge {
            state,
            finished: self.finished,
        })
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent agent_state`
Expected: all `agent_state::tests` PASS. Unused-code warnings are expected until Task 4.

- [ ] **Step 5: Commit**

```
feat(agent-state): Add pure per-Session agent state slot

Maps provider lifecycle hook names to Working / Needs you / Idle plus
a finished marker. Pure so the ordering rules captured from real
Claude/Codex/Grok hook traffic are unit-tested without a GUI.
```

---

### Task 2: State lane on the title pipe (`src/title_notify.rs`)

**Files:**
- Modify: `src/title_notify.rs`

**Interfaces:**
- Consumes: `agent_state::{StateSignal, signal_for}`.
- Produces:
  - `pub struct StateEvent { pub source_agent: SourceAgent, pub vendor_session_id: String, pub project_id: Option<String>, pub terminal_id: String, pub signal: StateSignal }`
  - `pub enum HookMessage { Title(TitlePromptEvent), State(StateEvent) }` (serde `tag = "kind"`, snake_case)
  - `pub fn serve(pipe: &str, title_tx: mpsc::SyncSender<TitlePromptEvent>, state_tx: mpsc::SyncSender<StateEvent>, ctx: eframe::egui::Context)` (new signature)
  - CLI: `foreman title-event --agent <claude|codex|grok> --kind state` (no `--kind` = naming, unchanged)

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `src/title_notify.rs` (it already has a `HashMap` import; reuse its env-closure style):

```rust
    fn state_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    const IN_FOREMAN: &[(&str, &str)] = &[
        ("FOREMAN", "1"),
        ("FOREMAN_PROJECT_ID", "p9"),
        ("FOREMAN_TERMINAL_ID", "t7"),
    ];

    #[test]
    fn state_event_carries_ids_and_signal_but_no_payload() {
        let input = br#"{"hook_event_name":"Stop","session_id":"s1","prompt":"secret","last_assistant_message":"secret"}"#;
        let event = normalize_state_event(SourceAgent::Claude, input, state_env(IN_FOREMAN)).unwrap();
        assert_eq!(event.signal, crate::agent_state::StateSignal::TurnEnd);
        assert_eq!(event.vendor_session_id, "s1");
        assert_eq!(event.project_id.as_deref(), Some("p9"));
        assert_eq!(event.terminal_id, "t7");
        let wire = serde_json::to_string(&HookMessage::State(event)).unwrap();
        assert!(!wire.contains("secret"), "{wire}");
    }

    #[test]
    fn state_event_ignores_unknown_hooks_and_non_foreman_shells() {
        let input = br#"{"hook_event_name":"PreToolUse","session_id":"s1"}"#;
        assert!(normalize_state_event(SourceAgent::Claude, input, state_env(IN_FOREMAN)).is_none());
        let stop = br#"{"hook_event_name":"Stop","session_id":"s1"}"#;
        assert!(normalize_state_event(SourceAgent::Claude, stop, state_env(&[])).is_none());
    }

    #[test]
    fn subagent_events_are_dropped_except_permission_requests() {
        let stop = br#"{"hook_event_name":"Stop","session_id":"s1","agent_id":"a1"}"#;
        assert!(normalize_state_event(SourceAgent::Claude, stop, state_env(IN_FOREMAN)).is_none());
        let ask = br#"{"hook_event_name":"PermissionRequest","session_id":"s1","agent_id":"a1"}"#;
        let event = normalize_state_event(SourceAgent::Claude, ask, state_env(IN_FOREMAN)).unwrap();
        assert_eq!(event.signal, crate::agent_state::StateSignal::PermissionRequest);
    }

    #[test]
    fn grok_duplicate_claude_hook_is_suppressed_for_state_too() {
        let mut pairs = IN_FOREMAN.to_vec();
        pairs.push(("GROK_SESSION_ID", "g1"));
        let input = br#"{"hook_event_name":"Stop","session_id":"g1"}"#;
        assert!(normalize_state_event(SourceAgent::Claude, input, state_env(&pairs)).is_none());
        let event = normalize_state_event(SourceAgent::Grok, input, state_env(&pairs)).unwrap();
        assert_eq!(event.vendor_session_id, "g1");
    }

    #[test]
    fn state_stdin_cap_admits_huge_tool_output() {
        let big = "x".repeat(INPUT_BYTES as usize * 4);
        let input = format!(r#"{{"hook_event_name":"PostToolUse","session_id":"s1","tool_response":"{big}"}}"#);
        assert!(input.len() as u64 > INPUT_BYTES);
        assert!(input.len() as u64 <= stdin_cap(true));
        assert_eq!(stdin_cap(false), INPUT_BYTES);
        assert!(normalize_state_event(SourceAgent::Claude, input.as_bytes(), state_env(IN_FOREMAN)).is_some());
    }

    #[test]
    fn a_complete_state_message_ends_the_read() {
        let event = StateEvent {
            source_agent: SourceAgent::Codex,
            vendor_session_id: "c1".into(),
            project_id: Some("p9".into()),
            terminal_id: "t12".into(),
            signal: crate::agent_state::StateSignal::Interrupt,
        };
        let bytes = serde_json::to_vec(&HookMessage::State(event.clone())).unwrap();
        let read = read_event_bytes(&mut std::io::Cursor::new(bytes), Duration::from_millis(200)).unwrap();
        assert_eq!(
            serde_json::from_slice::<HookMessage>(&read).unwrap(),
            HookMessage::State(event)
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent title_notify`
Expected: compile errors — `normalize_state_event`, `StateEvent`, `HookMessage`, `stdin_cap` not found.

- [ ] **Step 3: Implement**

In `src/title_notify.rs`:

1. Add constants and types near the top:

```rust
/// Hook stdin cap for state events. PostToolUse payloads include tool output,
/// which can be large; only the event name and IDs are forwarded.
const STATE_INPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct StateEvent {
    pub source_agent: SourceAgent,
    pub vendor_session_id: String,
    pub project_id: Option<String>,
    pub terminal_id: String,
    pub signal: crate::agent_state::StateSignal,
}

/// One message on the private hook pipe. Naming and state share the pipe and
/// helper; `serve` routes each kind into its own channel so a naming backlog
/// can never crowd out a state change.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookMessage {
    Title(TitlePromptEvent),
    State(StateEvent),
}

fn stdin_cap(state: bool) -> u64 {
    if state { STATE_INPUT_BYTES } else { INPUT_BYTES }
}
```

2. Raise `MAX_INFLIGHT` from 8 to 16 (state connections are tiny and short; the cap still bounds threads).

3. Extract the vendor-session-ID and subagent logic out of `normalize_event` into helpers, and make `normalize_event` call them (behavior unchanged):

```rust
fn in_foreman_and_not_grok_duplicate(source: SourceAgent, getenv: &impl Fn(&str) -> Option<String>) -> bool {
    getenv("FOREMAN").as_deref() == Some("1")
        // Grok imports Claude-compatible hooks; its own hook carries better identity.
        && !(source == SourceAgent::Claude && getenv("GROK_SESSION_ID").is_some())
}

fn is_subagent(value: &serde_json::Value) -> bool {
    ["agent_id", "agentId", "agent_type", "agentType", "subagent_type", "subagentType"]
        .into_iter()
        .any(|name| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|v| !v.trim().is_empty())
        })
}

fn vendor_session_id(
    source: SourceAgent,
    value: &serde_json::Value,
    getenv: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let from_payload = || {
        value
            .get("session_id")
            .or_else(|| value.get("sessionId"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let id = match source {
        SourceAgent::Grok => getenv("GROK_SESSION_ID").or_else(from_payload),
        SourceAgent::Claude | SourceAgent::Codex => from_payload(),
    }?;
    let id = id.trim().to_string();
    (!id.is_empty()).then_some(id)
}

fn terminal_scope(getenv: &impl Fn(&str) -> Option<String>) -> Option<(Option<String>, String)> {
    let terminal_id = getenv("FOREMAN_TERMINAL_ID")?.trim().to_string();
    if terminal_id.is_empty() {
        return None;
    }
    let project_id = getenv("FOREMAN_PROJECT_ID").filter(|value| !value.trim().is_empty());
    Some((project_id, terminal_id))
}

fn normalize_state_event(
    source: SourceAgent,
    input: &[u8],
    getenv: impl Fn(&str) -> Option<String>,
) -> Option<StateEvent> {
    if !in_foreman_and_not_grok_duplicate(source, &getenv) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(input).ok()?;
    let name = value
        .get("hook_event_name")
        .or_else(|| value.get("hookEventName"))
        .and_then(serde_json::Value::as_str)?;
    let signal = crate::agent_state::signal_for(name)?;
    // A subagent's turn is not the Session's turn, but its permission prompt
    // still waits on the human.
    if is_subagent(&value) && signal != crate::agent_state::StateSignal::PermissionRequest {
        return None;
    }
    let vendor_session_id = vendor_session_id(source, &value, &getenv)?;
    let (project_id, terminal_id) = terminal_scope(&getenv)?;
    Some(StateEvent { source_agent: source, vendor_session_id, project_id, terminal_id, signal })
}
```

Rewrite the body of `normalize_event` to use `in_foreman_and_not_grok_duplicate`, `is_subagent`, `vendor_session_id`, and `terminal_scope` in the same order as today (FOREMAN check, Grok duplicate check, JSON parse, subagent check, prompt, session ID, transcript path, terminal). The existing naming tests must stay green unchanged.

4. `client_main`: detect `--kind state`, pick the cap, and send a `HookMessage`:

```rust
    let state = args.windows(2).any(|pair| pair[0] == "--kind" && pair[1] == "state");
    let cap = stdin_cap(state);
    let mut input = Vec::new();
    if std::io::stdin().take(cap + 1).read_to_end(&mut input).is_err()
        || input.len() as u64 > cap
    {
        return 0;
    }
    let getenv = |name: &str| std::env::var(name).ok();
    let message = if state {
        normalize_state_event(source, &input, getenv).map(HookMessage::State)
    } else {
        normalize_event(source, &input, getenv).map(HookMessage::Title)
    };
    let Some(message) = message else {
        return 0;
    };
```

Then `send_event(&pipe, &message)`; change `send_event` to take `&HookMessage`.

5. `read_event_bytes`: the completeness check becomes `serde_json::from_slice::<HookMessage>(&bytes).is_ok()`.

6. `serve`: new signature with `title_tx` and `state_tx`; after reading, route:

```rust
            let routed = bytes
                .and_then(|bytes| serde_json::from_slice::<HookMessage>(&bytes).ok())
                .is_some_and(|message| match message {
                    HookMessage::Title(event) => title_tx.try_send(event).is_ok(),
                    HookMessage::State(event) => state_tx.try_send(event).is_ok(),
                });
            if routed {
                ctx.request_repaint();
            }
```

Update the existing `serve` doc comment: overload drops a naming attempt or leaves a badge stale until the next event, never delays input.

- [ ] **Step 4: Update the one caller so the tree compiles**

In `src/main.rs`, where `title_notify::serve` is spawned, create the state channel and pass it; stash the receiver in `TitleRuntime` in Task 4. For now:

```rust
            let (state_event_tx, _state_event_rx) = std::sync::mpsc::sync_channel(64);
            std::thread::spawn(move || {
                title_notify::serve(&title_pipe, title_event_tx, state_event_tx, title_ctx)
            });
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent title_notify`
Expected: all `title_notify::tests` PASS, including the pre-existing naming tests.

- [ ] **Step 6: Commit**

```
feat(title-notify): Carry agent state events on the hook pipe

State and naming share the private pipe and helper; the server routes
each message kind into its own channel so naming bursts cannot crowd
out state changes. State messages carry only the event and IDs.
```

---

### Task 3: State hook installer (`src/agent_hooks.rs`)

**Files:**
- Modify: `src/agent_hooks.rs`

**Interfaces:**
- Produces:
  - `pub enum HookSet { Naming, State }`
  - `pub fn spawn_install(ctx: eframe::egui::Context, set: HookSet) -> mpsc::Receiver<InstallReport>` (new parameter)
  - Installed command for state: the naming command with ` --kind state` appended after `--agent <agent>`.

Events installed for `HookSet::State`:

| Agent | File | Events |
|---|---|---|
| claude | `settings.json` | SessionStart, UserPromptSubmit, PermissionRequest, PostToolUse, Stop, StopFailure, SessionEnd |
| codex | `hooks.json` | SessionStart, UserPromptSubmit, PermissionRequest, PostToolUse, Stop, Interrupt, SessionEnd |
| grok | `hooks/foreman-agent-state.json` | SessionStart, UserPromptSubmit, PostToolUse, Stop, StopFailure, SessionEnd |

`PostToolUse` is installed without a matcher: which tools prompt depends on the user's permission mode, so no matcher can be correct. The cost is one async helper launch per tool call.

- [ ] **Step 1: Write the failing tests**

Add to `src/agent_hooks.rs` tests:

```rust
    fn roots_in(temp: &tempfile::TempDir) -> HookRoots {
        HookRoots {
            claude: temp.path().join("claude"),
            codex: temp.path().join("codex"),
            grok: temp.path().join("grok"),
        }
    }

    #[test]
    fn state_install_adds_every_state_event_with_the_state_kind() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        let report = install_in(&roots, HookSet::State);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let codex: serde_json::Value =
            serde_json::from_slice(&std::fs::read(roots.codex.join("hooks.json")).unwrap()).unwrap();
        for event in state_events("codex") {
            let handler = &codex["hooks"][event][0]["hooks"][0];
            assert_eq!(managed_kind_of(handler), Some(HookSet::State), "{event}");
        }
        assert!(roots.grok.join("hooks").join(GROK_STATE_FILE).exists());
    }

    #[test]
    fn naming_and_state_installs_never_reorder_each_other() {
        // Codex keys hook trust by position; any reorder forces re-trust.
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        install_in(&roots, HookSet::Naming);
        install_in(&roots, HookSet::State);
        let files = [roots.claude.join("settings.json"), roots.codex.join("hooks.json")];
        let snapshot: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();
        assert_eq!(install_in(&roots, HookSet::Naming).changed, 0);
        assert_eq!(install_in(&roots, HookSet::State).changed, 0);
        let after: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();
        assert_eq!(snapshot, after);
        let codex: serde_json::Value = serde_json::from_slice(&after[1]).unwrap();
        let kinds: Vec<Option<HookSet>> = codex["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| managed_kind_of(&group["hooks"][0]))
            .collect();
        assert_eq!(kinds, vec![Some(HookSet::Naming), Some(HookSet::State)]);
    }

    #[test]
    fn state_commands_are_classified_as_state_including_encoded_relays() {
        for agent in ["claude", "codex", "grok"] {
            assert_eq!(managed_kind(&managed_command(agent, HookSet::State)), Some(HookSet::State));
            assert_eq!(managed_kind(&managed_command(agent, HookSet::Naming)), Some(HookSet::Naming));
        }
        assert_eq!(managed_kind("keep-me"), None);
    }
```

Update the existing tests' calls: `install_in(&roots)` → `install_in(&roots, HookSet::Naming)`, `merge_hook(x, agent)` → `merge_hook(x, agent, "UserPromptSubmit", HookSet::Naming)`, `managed_command(agent)` → `managed_command(agent, HookSet::Naming)`, `is_managed_handler(h)` → `managed_kind_of(h).is_some()`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent agent_hooks`
Expected: compile errors — `HookSet`, `state_events`, `managed_kind` not found.

- [ ] **Step 3: Implement**

1. Types and event lists:

```rust
const GROK_STATE_FILE: &str = "foreman-agent-state.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookSet {
    Naming,
    State,
}

fn state_events(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude" => &["SessionStart", "UserPromptSubmit", "PermissionRequest", "PostToolUse", "Stop", "StopFailure", "SessionEnd"],
        "codex" => &["SessionStart", "UserPromptSubmit", "PermissionRequest", "PostToolUse", "Stop", "Interrupt", "SessionEnd"],
        _ => &["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop", "StopFailure", "SessionEnd"],
    }
}

fn events_for(agent: &str, set: HookSet) -> &'static [&'static str] {
    match set {
        HookSet::Naming => &["UserPromptSubmit"],
        HookSet::State => state_events(agent),
    }
}
```

2. Serialize installs: two sets may install concurrently from separate threads and both rewrite `settings.json`/`hooks.json`.

```rust
static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
```

`install()` takes `set`, holds `INSTALL_LOCK.lock()` (recover a poisoned lock with `unwrap_or_else(|e| e.into_inner())`) for its whole body, then calls `install_in(&roots, set)`. `spawn_install(ctx, set)` passes `set` through.

3. `install_in(roots, set)`: Grok's target is `GROK_FILE` for Naming and `GROK_STATE_FILE` for State; `install_one(path, agent, set)` passes `set` to the merge.

4. Commands: build the helper arguments once and thread them through all three command shapes.

```rust
fn helper_args(agent: &str, set: HookSet) -> String {
    match set {
        HookSet::Naming => format!("title-event --agent {agent}"),
        HookSet::State => format!("title-event --agent {agent} --kind state"),
    }
}
```

`managed_command(agent, set)`, `unix_managed_command(agent, set)`, and `windows_powershell_relay(agent, set)` replace their literal `title-event --agent {agent}` with `helper_args(agent, set)`.

5. Classification replaces `is_managed_command` / `is_managed_handler`:

```rust
fn classify_script(script: &str) -> Option<HookSet> {
    if !(script.contains("FOREMAN_EXE") && script.contains("title-event --agent ")) {
        return None;
    }
    Some(if script.contains("--kind state") { HookSet::State } else { HookSet::Naming })
}

fn managed_kind(command: &str) -> Option<HookSet> {
    if let Some(set) = classify_script(command) {
        return Some(set);
    }
    let encoded = command.strip_prefix(POWERSHELL_RELAY_PREFIX)?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let utf16: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
    classify_script(&String::from_utf16(&utf16).ok()?)
}

fn managed_kind_of(handler: &serde_json::Value) -> Option<HookSet> {
    ["command", "commandWindows", "command_windows"]
        .into_iter()
        .filter_map(|field| handler.get(field).and_then(serde_json::Value::as_str))
        .find_map(managed_kind)
}
```

6. `merge_hook(root, agent, event, set)` updates **in place**: build the desired handler exactly as today (`async` for claude/codex, `shell`/`commandWindows`/`timeout` overrides on Windows), then in `hooks[event]`:
   - find the first group whose `hooks` array holds a handler with `managed_kind_of(h) == Some(set)`; replace that handler with the desired one;
   - remove any further handlers of that same set from every group, dropping groups left empty (the existing `retain_mut` logic, restricted to `== Some(set)`);
   - if none was found, push `{"hooks": [handler]}` at the end.
   Handlers of the other set and unmanaged handlers are never moved.

7. `install_one` loops `for event in events_for(agent, set)` calling `merge_hook` on the same root before comparing and writing once.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent agent_hooks`
Expected: all PASS, including the pre-existing installer tests.

- [ ] **Step 5: Commit**

```
feat(agent-hooks): Install lifecycle hooks for agent state

Adds a state hook set beside naming. Managed handlers are updated in
place so installing one set never reorders the other; Codex keys hook
trust by position and would otherwise ask the user to re-trust.
```

---

### Task 4: Setting, install trigger, and GUI drain

**Files:**
- Modify: `src/config.rs`, `src/settings_menu.rs`, `src/main.rs`, `src/terminal.rs`, `src/wm.rs`

**Interfaces:**
- Consumes: `title_notify::StateEvent`, `agent_state::AgentStateSlot`, `agent_hooks::{spawn_install, HookSet}`.
- Produces:
  - `Settings::agent_state_badges: bool` (default `false`)
  - `Session::agent_state(&self) -> &AgentStateSlot`, `Session::agent_state_mut(&mut self) -> &mut AgentStateSlot`
  - `WindowManager::apply_state_event(&mut self, event: &title_notify::StateEvent) -> bool` (true when a Session matched)

- [ ] **Step 1: Write the failing tests**

In `src/config.rs` tests, beside the `auto_name_agent_sessions` default/round-trip assertions:

```rust
        assert!(!s.agent_state_badges);
```

and in the round-trip test set `s.agent_state_badges = true;` and assert `back.agent_state_badges`.

In `src/wm.rs` tests, model on the existing `prepare_title_request` test that sets `wm.tag` and `set_osc_title_for_test`:

```rust
    #[test]
    fn state_events_reach_only_the_matching_project_and_terminal() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p9".into());
        let id = wm.add_terminal(Shell::Cmd, &ctx).expect("shell");
        let event = |project: &str, terminal: String| crate::title_notify::StateEvent {
            source_agent: crate::terminal_titles::SourceAgent::Claude,
            vendor_session_id: "s1".into(),
            project_id: Some(project.into()),
            terminal_id: terminal,
            signal: crate::agent_state::StateSignal::PromptSubmit,
        };
        assert!(!wm.apply_state_event(&event("p1", term_tag(id))), "terminal ids repeat across projects");
        assert!(wm.apply_state_event(&event("p9", term_tag(id))));
        let window = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
        let Content::Terminal(session) = &mut window.tabs[0].content else {
            panic!("expected terminal");
        };
        session.set_osc_title_for_test(Some("claude".into()));
        assert_eq!(
            session.agent_state().badge(session.icon_kind(), false).map(|b| b.state),
            Some(crate::agent_state::AgentState::Working)
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent state_events_reach config`
Expected: compile errors — `agent_state_badges`, `apply_state_event`, `agent_state` not found.

- [ ] **Step 3: Implement**

1. `src/config.rs`: add after `auto_name_agent_sessions`:

```rust
    /// Show Working / Needs you / Idle on agent rows in the Sessions panel.
    /// Enabling installs guarded global lifecycle hooks.
    pub agent_state_badges: bool,
```

default `agent_state_badges: false,`.

2. `src/settings_menu.rs`: add `Field::AgentStateBadges`; a `RowSpec` right after the `AutoNameAgentSessions` row:

```rust
            RowSpec {
                field: Field::AgentStateBadges,
                label: "Show agent state in the Sessions panel",
                desc: "Enabling installs guarded global hooks; Codex may ask you to trust them",
                kind: Kind::Toggle,
            },
```

and the two match arms beside `AutoNameAgentSessions`: `Field::AgentStateBadges => flip(&mut s.agent_state_badges),` and `Field::AgentStateBadges => s.agent_state_badges.to_string(),`. Fix any other exhaustive `Field` match the compiler reports the same way.

3. `src/terminal.rs` `Session`: add a field `agent_state: crate::agent_state::AgentStateSlot,` (initialize `agent_state: Default::default(),` in the `Ok(Session { .. })` literal), the two accessors, and in the per-frame UI path right after `self.clear_bell();` inside `if active {`:

```rust
            self.agent_state.clear_finished();
```

4. `src/wm.rs`: add beside `prepare_title_request`:

```rust
    /// Apply a hook state event to the Session it names. Terminal ids repeat
    /// across projects, so the project tag must match too.
    pub fn apply_state_event(&mut self, event: &crate::title_notify::StateEvent) -> bool {
        if self.tag.as_deref() == event.project_id.as_deref() {
            for window in &mut self.windows {
                for tab in &mut window.tabs {
                    if let Content::Terminal(session) = &mut tab.content
                        && term_tag(session.term_id()) == event.terminal_id
                    {
                        session.agent_state_mut().apply(
                            event.source_agent,
                            &event.vendor_session_id,
                            event.signal,
                        );
                        return true;
                    }
                }
            }
        }
        self.windows.iter_mut().any(|window| {
            window.tabs.iter_mut().any(|tab| match &mut tab.content {
                Content::Project(child) => child.apply_state_event(event),
                _ => false,
            })
        })
    }
```

5. `src/main.rs`:
   - `TitleRuntime` gains `state_events: std::sync::mpsc::Receiver<title_notify::StateEvent>` and `state_hook_install: Option<std::sync::mpsc::Receiver<agent_hooks::InstallReport>>`; `App` gains the same two fields, wired in `App::new`.
   - At startup, replace the `_state_event_rx` from Task 2 with the real receiver; set `state_hook_install: startup_settings.agent_state_badges.then(|| agent_hooks::spawn_install(cc.egui_ctx.clone(), agent_hooks::HookSet::State))`. The naming call becomes `spawn_install(ctx, agent_hooks::HookSet::Naming)`.
   - Both test constructors that build `TitleRuntime` get `state_events` from a `sync_channel(1)` and `state_hook_install: None`.
   - Add the drain and call it from `service_background` right after `drain_title_events`:

```rust
    fn drain_state_events(&mut self) -> bool {
        let mut activity = false;
        while let Ok(event) = self.state_events.try_recv() {
            activity = true;
            // Always applied, even while hidden, so enabling the setting shows
            // current state instead of waiting for the next turn.
            self.desktop.apply_state_event(&event);
        }
        activity
    }
```

   - In the settings-change block beside `naming_changed`: when `agent_state_badges` flips from false to true and `self.state_hook_install.is_none()`, spawn `agent_hooks::spawn_install(ctx.clone(), agent_hooks::HookSet::State)`.
   - Generalize `poll_hook_install` into a helper that takes the receiver slot and a noun, and call it twice: `"Agent naming hooks"` for `hook_install`, `"Agent state hooks"` for `state_hook_install`. Messages keep today's three shapes (`… are up to date`, `… installed (n)`, `… install failed: …`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent`
Expected: whole suite PASS (known-flaky tests per **foreman-validation-and-qa** may need one re-run; report any that fail twice).

- [ ] **Step 5: Commit**

```
feat(agent-state): Route hook state events to their Session

Adds the agent_state_badges setting (off by default), installs the
state hooks when it is enabled, and applies drained events to the
matching Session. Focus clears the finished marker, as it does the Bell.
```

---

### Task 5: Panel badges (`src/wm.rs`, `src/panel.rs`)

**Files:**
- Modify: `src/wm.rs` (`panel_model`), `src/panel.rs`

**Interfaces:**
- Consumes: `Session::agent_state`, `AgentStateSlot::badge`, `agent_state::{AgentBadge, AgentState, badge_label}`, `Settings::agent_state_badges`.
- Produces: `TabEntry::agent: Option<AgentBadge>`, `ProjectEntry::needs_you: bool`, `RowPaintOwned::agent: Option<AgentBadge>`.

- [ ] **Step 1: Write the failing test**

In `src/wm.rs` tests, extend the pattern of `bell_latches_the_stack_until_cleared_and_reaches_the_panel`:

```rust
    #[test]
    fn agent_state_reaches_the_panel_and_needs_you_bubbles_to_the_project() {
        let ctx = egui::Context::default();
        let mut m = WindowManager::new();
        m.tag = Some("p1".into());
        let id = m.add_terminal(Shell::Cmd, &ctx).expect("shell");
        m.add_terminal(Shell::Cmd, &ctx).expect("second shell");
        {
            let window = m.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(session) = &mut window.tabs[0].content else {
                panic!("expected terminal");
            };
            session.set_osc_title_for_test(Some("claude".into()));
        }
        let ask = crate::title_notify::StateEvent {
            source_agent: crate::terminal_titles::SourceAgent::Claude,
            vendor_session_id: "s1".into(),
            project_id: Some("p1".into()),
            terminal_id: term_tag(id),
            signal: crate::agent_state::StateSignal::PermissionRequest,
        };
        assert!(m.apply_state_event(&ask));

        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let mut desk = WindowManager::new();
        desk.push_win(7, Tab::fixed("proj", Content::Project(Box::new(m))), r);
        let pm = desk.panel_model();
        assert!(pm.projects[0].needs_you);
        let states: Vec<Option<crate::agent_state::AgentState>> =
            pm.projects[0].tabs.iter().map(|t| t.agent.map(|b| b.state)).collect();
        assert_eq!(
            states.iter().filter(|s| **s == Some(crate::agent_state::AgentState::NeedsYou)).count(),
            1
        );
        assert_eq!(states.iter().filter(|s| s.is_none()).count(), 1, "plain shell shows nothing");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --target-dir target/agent agent_state_reaches_the_panel`
Expected: compile errors — `needs_you` / `agent` fields missing.

- [ ] **Step 3: Implement the model**

- `src/panel.rs` `TabEntry`: add

```rust
    /// Hook-driven agent state; `None` = no badge (not an agent, no event yet,
    /// or exited). Not yet gated by the setting — the panel gates at paint.
    pub agent: Option<crate::agent_state::AgentBadge>,
```

- `ProjectEntry`: add `pub needs_you: bool,` (doc: any tab's badge is Needs you).
- `src/wm.rs` `panel_model`, in the `tabs.push(TabEntry { .. })` literal:

```rust
                            agent: match &t.content {
                                Content::Terminal(s) => {
                                    s.agent_state().badge(s.icon_kind(), s.has_exited())
                                }
                                _ => None,
                            },
```

and in `ProjectEntry { .. }`: `needs_you: tabs.iter().any(|t| t.agent.is_some_and(|b| b.state == crate::agent_state::AgentState::NeedsYou)),`. Fix every other `TabEntry`/`ProjectEntry` literal the compiler reports (tests included) with `agent: None` / `needs_you: false`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --target-dir target/agent agent_state_reaches_the_panel`
Expected: PASS.

- [ ] **Step 5: Implement painting**

In `src/panel.rs`:

1. `RowPaintOwned`: add `agent: Option<crate::agent_state::AgentBadge>,` (doc: already gated by the setting).
2. In both layouts that build `RowPaintOwned` for tab rows and project rows, compute once per pass `let state_gate = crate::config::live(ui.ctx()).agent_state_badges;` next to `bell_gate`, and set:
   - tab rows: `agent: t.agent.filter(|_| state_gate),`
   - project rows: `agent: (folded && state_gate && proj.needs_you).then_some(crate::agent_state::AgentBadge { state: crate::agent_state::AgentState::NeedsYou, finished: false }),`
   (`folded` is the same local the bell field uses on project rows.) Any remaining `RowPaintOwned` literal gets `agent: None`.
3. Title reserve: in the `reserve` chain, add a branch right after `} else if over {`:

```rust
        } else if rp.agent.is_some() {
            58.0 // state label ("needs you" is the widest)
```

4. Right-edge slot: the attention order is buttons (hover) > Needs you > Bell > other states > `min`/`tab`. Replace the `} else if rp.bell {` arm's head so the chain reads:

```rust
        } else if let Some(badge) = rp
            .agent
            .filter(|b| b.state == crate::agent_state::AgentState::NeedsYou)
        {
            paint_state_label(&p, row, badge, th.bell);
        } else if rp.bell {
            // (existing pulsing dot, unchanged)
        } else if let Some(badge) = rp.agent {
            let color = if badge.finished { th.text } else { th.dim };
            paint_state_label(&p, row, badge, color);
        } else if rp.minimized {
```

with a small helper next to the other `paint_*` functions:

```rust
fn paint_state_label(
    p: &egui::Painter,
    row: egui::Rect,
    badge: crate::agent_state::AgentBadge,
    color: egui::Color32,
) {
    p.text(
        egui::pos2(row.max.x - 8.0, row.center().y),
        egui::Align2::RIGHT_CENTER,
        crate::agent_state::badge_label(badge),
        egui::FontId::proportional(10.0),
        color,
    );
}
```

Painting stays read-only: no state mutation in the draw pass (focus clearing already happens in `Session`'s UI path from Task 4).

- [ ] **Step 6: Build and run the full suite**

Run: `cargo build --target-dir target/agent` then `cargo test --target-dir target/agent`
Expected: clean build; suite PASS.

- [ ] **Step 7: Commit**

```
feat(panel): Show agent state on Session rows

Rows show working / needs you / idle / done; a collapsed Project shows
needs you when any child does. Needs you outranks the Bell because it
blocks work; the other states rank below it.
```

---

### Task 6: Live verification, docs, and cleanup

**Files:**
- Create: `docs/agent-state.md`
- Modify: `CONTEXT.md`, `.claude/skills/foreman-agent-state-campaign/SKILL.md`
- Delete: `docs/superpowers/plans/2026-09-25-agent-state.md` (this plan) once the feature ships

- [ ] **Step 1: Live check with real agents (needs the user)**

Build `cargo build --target-dir target/agent`. Ask the user to launch `target/agent/debug/foreman.exe`, enable **Show agent state in the Sessions panel**, trust the new Codex hooks with `/hooks`, and run the **build-screenshot** skill (user-only) at each point. Drive prompts with `foreman send` (text and Enter as two separate calls; in Git Bash set `MSYS_NO_PATHCONV=1` before sending `/exit`). Expected per row:

| Action | Claude row | Codex row (`codex --no-daemon`) |
|---|---|---|
| Launch, no prompt | idle | nothing (SessionStart is lazy) |
| Prompt running | working | working |
| Ask a question (Claude) | needs you | — |
| Answer it | working, then done | — |
| Turn ends while unfocused | done | done |
| Focus the pane | idle | idle |
| Esc mid-turn | stays working (accepted gap) | idle |
| `/exit` | row badge gone | row badge gone |

Also confirm by hand: typing in a pane while its agent streams feels unchanged, and the panel stays responsive during a tool-heavy turn.

- [ ] **Step 2: Write `docs/agent-state.md`**

Follow **foreman-docs-and-writing**: `## What it does`, `## How to turn it on`, `## Gotchas` (the four accepted limitations from the spec, Codex trust, badges hidden when off), and `## Key files` naming `src/agent_state.rs` `AgentStateSlot`, `src/title_notify.rs` `HookMessage` / `normalize_state_event` / `serve`, `src/agent_hooks.rs` `HookSet` / `merge_hook`, `src/wm.rs` `apply_state_event`, `src/panel.rs` `paint_state_label`. No line numbers, no counts.

- [ ] **Step 3: Glossary**

Add to `CONTEXT.md`, matching its entry shape:
- **Agent state** — Working / Needs you / Idle of an agent Session, from provider lifecycle hooks. _Avoid_: status, activity.
- **Needs you** — the agent is blocked on the human: a permission prompt or question. _Avoid_: waiting, blocked, needs-input.
- **Done marker** — set when a turn ends, cleared when the Session gets keyboard focus. _Avoid_: finished, complete, unread.

- [ ] **Step 4: Point the campaign skill at the shipped design**

At the top of `.claude/skills/foreman-agent-state-campaign/SKILL.md`, add:

```markdown
> **SUPERSEDED (2026-09-25):** Agent state ships from provider lifecycle hooks,
> not passive PTY detection. See `docs/agent-state.md` and
> `docs/superpowers/specs/2026-09-25-agent-state-design.md`. Kept for the
> Ready/READY_GRACE material and decision history.
```

Run `pwsh -File .claude/hooks/cite-guard.ps1 -All`; expect no new findings.

- [ ] **Step 5: Delete this plan and commit**

```
git rm docs/superpowers/plans/2026-09-25-agent-state.md
```

```
docs(agent-state): Document hook-driven agent state

Feature doc, glossary terms, and a supersession banner on the passive
detection campaign. Removes the executed plan.
```
