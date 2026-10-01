# Agent State in the Sessions Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show Working / Needs you / Idle (plus a "done" marker) for Claude, Codex, and Grok Sessions in the Sessions panel, driven by provider lifecycle hooks.

**Architecture:** The one managed hook Foreman already installs for Session naming (`foreman title-event --agent X`) is installed on a few more lifecycle events, with the event name baked into each handler as `--event <Name>`. The helper sends the existing `TitlePromptEvent` over the existing private pipe with a new `hook_event` field; `prompt` becomes optional and is set only for a main agent's `UserPromptSubmit`. The GUI drains the one channel it already drains, applies every event to a pure `AgentStateSlot` on the matching `Session` (after the same project + terminal + icon match naming uses), and starts naming only when `prompt` is present. The panel reads a badge from the slot.

**Tech Stack:** Rust, egui 0.34, serde_json, interprocess local sockets (existing).

**Spec:** `docs/superpowers/specs/2026-09-25-agent-state-design.md`. **The spec wins on any conflict with this plan.** An earlier revision of this plan built the spec's rejected alternatives (`--kind` flag, message envelope, split channels, session-id tracking, `SessionStart`/`SessionEnd`); do not reintroduce them.

## Global Constraints

- The feature writes **zero bytes** into any Session. It only reads hook events.
- Event receipt never blocks the GUI thread: pipe reads stay on the server threads in `title_notify::serve`; the GUI only calls `try_recv`.
- Messages carry the event name and IDs. `prompt` is present only on a main agent's `UserPromptSubmit`, exactly as naming sends it today. No tool input, tool output, or assistant text is ever forwarded.
- One hook command, one message type, one channel. No `--kind`, no envelope, no second channel.
- Claude and Codex hooks run with `"async": true` (Grok's schema has no per-handler async field).
- Identity is Foreman project ID + terminal ID (terminal IDs repeat across projects). An event whose provider does not match the Session's icon is dropped at apply time with `source_matches_icon`, the same check naming uses.
- New setting `agent_state_badges` defaults to `false`. The installed event set is derived from both `auto_name_agent_sessions` and `agent_state_badges`; changing either runs the installer, which adds the wanted events and removes managed handlers from unwanted ones. Turning state off removes the state hooks, so naming-only users never pay a helper launch per tool call.
- Reinstalling must leave hook files byte-identical when nothing changed, and must never move a handler within an event's array. Codex keys hook trust by position.
- A Session shows no badge until an event from it is accepted, while its process has exited, or while its detected agent (`Session::icon_kind`) is not the event's provider.
- Helper stdin cap is 16 MB (`PostToolUse` carries the tool's full output). The pipe server's per-message read cap stays 64 KB. `MAX_INFLIGHT` is 32.
- Build only with `cargo build --target-dir target/agent` / `cargo test --target-dir target/agent`. `$env:FOREMAN` is `1` in agent terminals: never `Stop-Process foreman`, never kill by name.
- Do not touch the `eframe` line in `Cargo.toml`.
- Commits: `type(scope): subject`, body says why, trailer `Co-Authored-By: Claude <your model> <noreply@anthropic.com>`. Use `git commit -F <file>` for multi-line messages; verify with `git log -1 --format=%B`. End every commit with the `Card:` trailer from your dispatch prompt.

## Review Focus

- **Huge `PostToolUse` payloads.** A tool that printed megabytes still must deliver its event; a 64 KB stdin cap would silently drop it and pin the badge at Needs you. Test in Task 2.
- **Another provider inside the pane.** Codex run from a Claude Session's Bash tool fires Codex hooks with the pane's terminal ID. Those events must not touch the pane's state, or a Claude permission prompt would be hidden behind a Codex badge. Test in Task 4.
- **Subagent events.** A subagent's `Stop` must not change the Session's state; a subagent's `PermissionRequest` and `PostToolUse` must, because the human answers its prompt and its tool finishing is what leaves Needs you. Test in Task 2.
- **Reinstall order.** Naming then state, state then naming, or either twice must never move an existing handler in `UserPromptSubmit`, or Codex demands re-trust of every hook. Test in Task 3.
- **Codex `PostToolUse` after `Interrupt`.** Codex delivers a late `PostToolUse` about 1.5 s after an interrupt; it must not wake an Idle Session. Test in Task 1.

---

### Task 1: Pure state slot (`src/agent_state.rs`)

**Files:**
- Create: `src/agent_state.rs`
- Modify: `src/main.rs` (add `mod agent_state;` beside `mod agent_hooks;`)

**Interfaces:**
- Consumes: `crate::terminal_titles::SourceAgent` (`Copy`, `label()`), `crate::icons::IconKind` (`agent_label()`).
- Produces:
  - `pub enum HookEvent { UserPromptSubmit, PermissionRequest, PostToolUse, Stop, StopFailure, Interrupt }` (serde, `Copy`), with `HookEvent::parse(&str) -> Option<Self>` and `HookEvent::name(self) -> &'static str`
  - `pub enum AgentState { Working, NeedsYou, Idle }`
  - `pub struct AgentBadge { pub state: AgentState, pub finished: bool }`
  - `pub fn badge_label(badge: AgentBadge) -> &'static str`
  - `pub struct AgentStateSlot` (`Default`) with `apply(&mut self, source: SourceAgent, event: HookEvent)`, `clear_finished(&mut self)`, `badge(&self, icon: IconKind, exited: bool) -> Option<AgentBadge>`

- [ ] **Step 1: Write the failing tests**

Create `src/agent_state.rs` containing only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::IconKind;
    use crate::terminal_titles::SourceAgent::{Claude, Codex};
    use HookEvent::*;

    fn run(slot: &mut AgentStateSlot, source: SourceAgent, events: &[HookEvent]) {
        for event in events {
            slot.apply(source, *event);
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
    fn hook_event_names_round_trip_and_unknown_names_are_rejected() {
        for event in [UserPromptSubmit, PermissionRequest, PostToolUse, Stop, StopFailure, Interrupt] {
            assert_eq!(HookEvent::parse(event.name()), Some(event));
        }
        assert_eq!(HookEvent::parse("SessionStart"), None);
        assert_eq!(HookEvent::parse("PreToolUse"), None);
        assert_eq!(HookEvent::parse(""), None);
    }

    #[test]
    fn a_turn_goes_working_then_done_and_focus_clears_done() {
        let mut slot = AgentStateSlot::default();
        assert_eq!(badge(&slot, IconKind::Claude), None, "no event yet = no badge");
        run(&mut slot, Claude, &[UserPromptSubmit, PostToolUse]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING);
        run(&mut slot, Claude, &[Stop]);
        assert_eq!(badge(&slot, IconKind::Claude), DONE);
        slot.clear_finished();
        assert_eq!(badge(&slot, IconKind::Claude), IDLE);
        run(&mut slot, Claude, &[StopFailure, UserPromptSubmit]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING, "a new prompt clears done");
    }

    #[test]
    fn permission_request_needs_you_until_the_tool_finishes() {
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, &[UserPromptSubmit, PermissionRequest]);
        assert_eq!(badge(&slot, IconKind::Claude), NEEDS);
        run(&mut slot, Claude, &[PostToolUse]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING);
    }

    #[test]
    fn post_tool_use_never_wakes_an_idle_session() {
        // Codex delivers PostToolUse ~1.5 s after Interrupt (captured 2026-09-25).
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Codex, &[UserPromptSubmit, Interrupt, PostToolUse]);
        assert_eq!(badge(&slot, IconKind::Codex), IDLE, "interrupt is not a finished turn");
        run(&mut slot, Codex, &[Stop, PostToolUse]);
        assert_eq!(badge(&slot, IconKind::Codex), DONE);
    }

    #[test]
    fn a_tool_result_before_any_prompt_shows_nothing() {
        // Badges enabled mid-turn: the first event seen is a PostToolUse.
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, &[PostToolUse]);
        assert_eq!(badge(&slot, IconKind::Claude), None);
    }

    #[test]
    fn badge_hides_when_exited_or_the_agent_left_the_pane() {
        let mut slot = AgentStateSlot::default();
        run(&mut slot, Claude, &[UserPromptSubmit]);
        assert_eq!(badge(&slot, IconKind::Claude), WORKING);
        assert_eq!(slot.badge(IconKind::Claude, true), None, "exited process");
        assert_eq!(badge(&slot, IconKind::PowerShell), None, "agent exited back to the shell");
        assert_eq!(badge(&slot, IconKind::Codex), None, "a different agent now runs here");
    }

    #[test]
    fn labels_are_distinct_per_state_and_done_differs_from_idle() {
        let labels: Vec<&str> = [WORKING, NEEDS, IDLE, DONE]
            .into_iter()
            .map(|b| badge_label(b.unwrap()))
            .collect();
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 4, "{labels:?}");
    }
}
```

Add `mod agent_state;` to `src/main.rs` next to `mod agent_hooks;`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent agent_state`
Expected: compile errors — `HookEvent`, `AgentStateSlot`, `badge_label` not found.

- [ ] **Step 3: Implement**

Put this above the test module in `src/agent_state.rs`:

```rust
//! Per-Session agent state (Working / Needs you / Idle) driven by provider
//! lifecycle hooks. Pure: fed hook events, holds no I/O, never writes to the
//! Session. Design: docs/superpowers/specs/2026-09-25-agent-state-design.md.

use crate::icons::IconKind;
use crate::terminal_titles::SourceAgent;

/// The provider hook events Foreman installs. The name is baked into each
/// installed handler (`--event <name>`), so the helper never has to find the
/// event name inside a provider's payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HookEvent {
    UserPromptSubmit,
    PermissionRequest,
    PostToolUse,
    Stop,
    StopFailure,
    Interrupt,
}

impl HookEvent {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "PermissionRequest" => Self::PermissionRequest,
            "PostToolUse" => Self::PostToolUse,
            "Stop" => Self::Stop,
            "StopFailure" => Self::StopFailure,
            "Interrupt" => Self::Interrupt,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PermissionRequest => "PermissionRequest",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::Interrupt => "Interrupt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    NeedsYou,
    Idle,
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
    state: Option<AgentState>,
    finished: bool,
}

impl AgentStateSlot {
    /// The caller has already matched the event to this Session and checked
    /// its provider against the pane's icon; the mapping is idempotent, so a
    /// duplicated event (Grok replays Claude's hooks) is harmless.
    pub fn apply(&mut self, source: SourceAgent, event: HookEvent) {
        self.source = Some(source);
        match event {
            HookEvent::UserPromptSubmit => {
                self.state = Some(AgentState::Working);
                self.finished = false;
            }
            HookEvent::PermissionRequest => self.state = Some(AgentState::NeedsYou),
            // Only leaves Needs you. It never wakes an Idle Session: Codex
            // delivers a PostToolUse after Interrupt.
            HookEvent::PostToolUse => {
                if self.state == Some(AgentState::NeedsYou) {
                    self.state = Some(AgentState::Working);
                }
            }
            HookEvent::Stop | HookEvent::StopFailure => {
                self.state = Some(AgentState::Idle);
                self.finished = true;
            }
            HookEvent::Interrupt => self.state = Some(AgentState::Idle),
        }
    }

    pub fn clear_finished(&mut self) {
        self.finished = false;
    }

    /// `None` hides the badge: no event yet, the process exited, or the pane
    /// no longer shows this provider's icon (the agent quit back to its shell,
    /// or a different agent runs here now).
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
Expected: all `agent_state::tests` PASS. Dead-code warnings are expected until Task 4.

- [ ] **Step 5: Commit**

```
feat(agent-state): Add pure per-Session agent state slot

Maps provider lifecycle hook events to Working / Needs you / Idle plus
a finished marker. Pure so the ordering rules captured from real
Claude/Codex/Grok hook traffic are unit-tested without a GUI.
```

---

### Task 2: The hook event on the title pipe (`src/title_notify.rs`)

**Files:**
- Modify: `src/title_notify.rs`
- Modify: `src/wm.rs` (`prepare_title_request` and its three test literals)

**Interfaces:**
- Consumes: `agent_state::HookEvent`.
- Produces:
  - `TitlePromptEvent` gains `pub hook_event: HookEvent` and `prompt` becomes `pub prompt: Option<String>`. Field order: `source_agent, hook_event, vendor_session_id, transcript_path, project_id, terminal_id, prompt`.
  - CLI: `foreman title-event --agent <claude|codex|grok> [--event <Name>]`. A missing `--event` means `UserPromptSubmit`, so a handler installed by an older Foreman keeps working until the installer rewrites it.
  - `fn parse_args(args: &[String]) -> Option<(SourceAgent, HookEvent)>` (private, tested).
  - Constants: `HELPER_INPUT_BYTES = 16 * 1024 * 1024` (helper stdin), `MESSAGE_BYTES = 64 * 1024` (server read; the old `INPUT_BYTES`), `MAX_INFLIGHT = 32`.
  - `serve` signature unchanged.

- [ ] **Step 1: Update the existing tests for the new shape**

In `src/title_notify.rs` tests:

- `claude_hook_payload_becomes_a_scoped_event`: call `normalize_event(SourceAgent::Claude, HookEvent::UserPromptSubmit, br#"..."#, ...)` and assert `event.prompt.as_deref() == Some("fix the auth race")` and `event.hook_event == HookEvent::UserPromptSubmit`.
- `subagents_and_grok_replaying_claude_hooks_are_ignored`: every `normalize_event` call gains `HookEvent::UserPromptSubmit` as its second argument.
- `connected_client_can_delay_its_first_write`, `one_way_pipe_round_trip_preserves_routing_identity`: the `TitlePromptEvent` literals gain `hook_event: HookEvent::UserPromptSubmit,` after `source_agent` and wrap the prompt as `prompt: Some("...".into())`.
- `partial_event_survives_empty_reads_and_incomplete_eof_times_out`: the payload becomes `br#"{"source_agent":"codex","hook_event":"UserPromptSubmit","vendor_session_id":"s1","terminal_id":"t1","prompt":"fix it"}"#` (the `[..30]` split still lands mid-object).
- Add `use crate::agent_state::HookEvent;` to the test module's imports.

In `src/wm.rs` tests, the three `crate::title_notify::TitlePromptEvent { .. }` literals (find them with `rg -n "TitlePromptEvent \{" src/wm.rs`) gain `hook_event: crate::agent_state::HookEvent::UserPromptSubmit,` and wrap their prompt as `prompt: Some("...".into())`.

- [ ] **Step 2: Write the failing tests**

Add to the `src/title_notify.rs` test module:

```rust
    fn env_fn(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map = env(pairs);
        move |name| map.get(name).cloned()
    }

    const IN_FOREMAN: &[(&str, &str)] = &[
        ("FOREMAN", "1"),
        ("FOREMAN_PROJECT_ID", "p9"),
        ("FOREMAN_TERMINAL_ID", "t7"),
    ];

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn event_arg_defaults_to_prompt_submit_and_rejects_unknown_names() {
        assert_eq!(
            parse_args(&args(&["--agent", "claude"])),
            Some((SourceAgent::Claude, HookEvent::UserPromptSubmit))
        );
        assert_eq!(
            parse_args(&args(&["--agent", "codex", "--event", "Interrupt"])),
            Some((SourceAgent::Codex, HookEvent::Interrupt))
        );
        assert_eq!(parse_args(&args(&["--agent", "grok", "--event", "SessionStart"])), None);
        assert_eq!(parse_args(&args(&["--event", "Stop"])), None);
    }

    #[test]
    fn state_events_carry_the_event_and_ids_but_no_payload_text() {
        let input = br#"{"session_id":"s1","prompt":"secret","last_assistant_message":"secret","tool_response":"secret"}"#;
        let event = normalize_event(SourceAgent::Claude, HookEvent::Stop, input, env_fn(IN_FOREMAN))
            .expect("state event");
        assert_eq!(event.hook_event, HookEvent::Stop);
        assert_eq!(event.prompt, None, "prompt rides only on UserPromptSubmit");
        assert_eq!(event.vendor_session_id, "s1");
        assert_eq!(event.project_id.as_deref(), Some("p9"));
        assert_eq!(event.terminal_id, "t7");
        let wire = serde_json::to_string(&event).unwrap();
        assert!(!wire.contains("secret"), "{wire}");
    }

    #[test]
    fn a_slash_command_prompt_is_a_state_event_without_naming() {
        let input = br#"{"session_id":"s1","prompt":"/help"}"#;
        let event = normalize_event(SourceAgent::Claude, HookEvent::UserPromptSubmit, input, env_fn(IN_FOREMAN))
            .expect("the turn still starts");
        assert_eq!(event.hook_event, HookEvent::UserPromptSubmit);
        assert_eq!(event.prompt, None, "nothing worth naming from");
    }

    #[test]
    fn subagent_events_are_dropped_except_permission_and_tool_done() {
        let payload = |event: &str| {
            format!(r#"{{"session_id":"s1","agent_id":"a1","hook_event_name":"{event}","prompt":"child work"}}"#)
        };
        let drop = [HookEvent::UserPromptSubmit, HookEvent::Stop, HookEvent::StopFailure, HookEvent::Interrupt];
        for event in drop {
            assert!(
                normalize_event(SourceAgent::Claude, event, payload(event.name()).as_bytes(), env_fn(IN_FOREMAN)).is_none(),
                "{event:?}"
            );
        }
        for event in [HookEvent::PermissionRequest, HookEvent::PostToolUse] {
            let got = normalize_event(SourceAgent::Claude, event, payload(event.name()).as_bytes(), env_fn(IN_FOREMAN))
                .unwrap_or_else(|| panic!("{event:?}"));
            assert_eq!(got.hook_event, event);
            assert_eq!(got.prompt, None);
        }
    }

    #[test]
    fn grok_duplicate_of_a_claude_state_hook_is_suppressed() {
        let mut pairs = IN_FOREMAN.to_vec();
        pairs.push(("GROK_SESSION_ID", "g1"));
        let input = br#"{"sessionId":"g1"}"#;
        assert!(normalize_event(SourceAgent::Claude, HookEvent::Stop, input, env_fn(&pairs)).is_none());
        let event = normalize_event(SourceAgent::Grok, HookEvent::Stop, input, env_fn(&pairs)).unwrap();
        assert_eq!(event.vendor_session_id, "g1");
    }

    #[test]
    fn helper_accepts_a_tool_payload_larger_than_one_message() {
        let big = "x".repeat(MESSAGE_BYTES as usize * 4);
        let input = format!(r#"{{"session_id":"s1","tool_response":"{big}"}}"#);
        assert!(input.len() as u64 > MESSAGE_BYTES);
        assert!(input.len() as u64 <= HELPER_INPUT_BYTES);
        let event = normalize_event(SourceAgent::Claude, HookEvent::PostToolUse, input.as_bytes(), env_fn(IN_FOREMAN))
            .expect("large tool output must not drop the event");
        assert!(serde_json::to_vec(&event).unwrap().len() < 1024, "the forwarded message stays small");
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent title_notify`
Expected: compile errors — `parse_args`, `HookEvent` field, `MESSAGE_BYTES`, `HELPER_INPUT_BYTES` not found.

- [ ] **Step 4: Implement**

In `src/title_notify.rs`:

1. Module doc and constants:

```rust
//! Passive hook-to-GUI notification lane: one message per provider lifecycle
//! hook, carrying the event name and routing IDs. A `UserPromptSubmit` from a
//! main agent also carries the prompt, which starts Session naming.

use crate::agent_state::HookEvent;

/// Helper stdin cap. `PostToolUse` payloads include the tool's full output.
const HELPER_INPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Pipe-server read cap. The forwarded message is the event and IDs only.
const MESSAGE_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const READ_TIMEOUT: Duration = Duration::from_secs(1);
/// Tool-heavy turns across several panes, doubled by Grok replaying Claude's
/// hooks, must not get a `Stop` turned away.
const MAX_INFLIGHT: usize = 32;
```

Replace every remaining `INPUT_BYTES` in `read_event_bytes` with `MESSAGE_BYTES`.

2. The message:

```rust
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TitlePromptEvent {
    pub source_agent: SourceAgent,
    pub hook_event: HookEvent,
    pub vendor_session_id: String,
    pub transcript_path: Option<String>,
    pub project_id: Option<String>,
    pub terminal_id: String,
    /// Present only for a main agent's `UserPromptSubmit` with a prompt worth
    /// naming from. State needs nothing but the event and the IDs.
    pub prompt: Option<String>,
}
```

3. Argument parsing and `client_main`:

```rust
fn parse_args(args: &[String]) -> Option<(SourceAgent, HookEvent)> {
    let value_of = |flag: &str| {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    };
    let source = SourceAgent::parse(value_of("--agent")?)?;
    let event = match value_of("--event") {
        Some(name) => HookEvent::parse(name)?,
        None => HookEvent::UserPromptSubmit,
    };
    Some((source, event))
}

pub fn client_main(args: &[String]) -> i32 {
    let Some((source, event)) = parse_args(args) else {
        return 0;
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .take(HELPER_INPUT_BYTES + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() as u64 > HELPER_INPUT_BYTES
    {
        return 0;
    }
    let Some(event) = normalize_event(source, event, &input, |name| std::env::var(name).ok()) else {
        return 0;
    };
    let Some(pipe) = std::env::var("FOREMAN_TITLE_PIPE").ok() else {
        return 0;
    };
    let _ = send_event(&pipe, &event);
    0
}
```

4. `normalize_event` gains the `event: HookEvent` parameter (second position). Keep the FOREMAN check, the Grok-duplicate check, and the JSON parse as they are, then replace the subagent block and the prompt line with:

```rust
    let subagent = [
        "agent_id",
        "agentId",
        "agent_type",
        "agentType",
        "subagent_type",
        "subagentType",
    ]
    .into_iter()
    .any(nonempty);
    // A subagent's turn is not the Session's turn, but its permission prompt
    // waits on the human and its tool finishing is what leaves Needs you.
    if subagent && !matches!(event, HookEvent::PermissionRequest | HookEvent::PostToolUse) {
        return None;
    }
    let prompt = if event == HookEvent::UserPromptSubmit && !subagent {
        value
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::terminal_titles::meaningful_prompt)
    } else {
        None
    };
```

The session-ID, transcript-path, and terminal-ID logic stays as it is. The returned literal gains `hook_event: event,` and uses `prompt,` (now an `Option`).

5. In `src/wm.rs` `prepare_title_request`, first line of the body:

```rust
        let Some(prompt) = event.prompt.as_deref() else {
            return None;
        };
```

and change `tab.agent_title.begin(&event.vendor_session_id, &event.prompt, epoch)?` to `tab.agent_title.begin(&event.vendor_session_id, prompt, epoch)?`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent title_notify` then `cargo test --target-dir target/agent prepare_title`
Expected: all PASS, including the pre-existing naming tests.

- [ ] **Step 6: Commit**

```
feat(title-notify): Carry the hook event name on the naming message

The one naming hook now reports which lifecycle event fired; prompt is
optional and rides only on a main agent's UserPromptSubmit. The helper
reads up to 16 MB so a PostToolUse with large tool output still lands.
```

---

### Task 3: Install hooks on the wanted events (`src/agent_hooks.rs`)

**Files:**
- Modify: `src/agent_hooks.rs`
- Modify: `src/main.rs` (the one `spawn_install` call at startup and the one in the settings-change block, so the tree compiles)

**Interfaces:**
- Consumes: `crate::config::Settings` (`auto_name_agent_sessions`, `agent_state_badges` — the field is added in Task 4; until then `from_settings` reads only `auto_name_agent_sessions` and sets `state: false`).
- Produces:
  - `pub struct HookWants { pub naming: bool, pub state: bool }` (`Copy`, `Default`, `PartialEq`) with `HookWants::from_settings(&Settings) -> Self`
  - `pub fn spawn_install(ctx: eframe::egui::Context, wants: HookWants) -> mpsc::Receiver<InstallReport>`
  - Installed command text: `title-event --agent <agent> --event <Event>`.

Events installed per agent when `wants.state`:

| Agent | File | Events |
|---|---|---|
| claude | `settings.json` | UserPromptSubmit, PermissionRequest, PostToolUse, Stop, StopFailure |
| codex | `hooks.json` | UserPromptSubmit, PermissionRequest, PostToolUse, Stop, Interrupt |
| grok | `hooks/foreman-session-naming.json` | UserPromptSubmit, PostToolUse, Stop, StopFailure |

`wants.naming` alone installs `UserPromptSubmit` only. Neither installs nothing and removes every managed handler. `PostToolUse` has no matcher: which tools prompt depends on the user's permission mode.

- [ ] **Step 1: Update the existing tests for the new signatures**

- `merge_preserves_unrelated_hooks_and_is_idempotent`: `merge_hooks(original, "claude", NAMING)` where the test module defines `const NAMING: HookWants = HookWants { naming: true, state: false };` and `const STATE: HookWants = HookWants { naming: true, state: true };`. The `text.matches(...)` assertion becomes `text.matches("title-event --agent claude --event UserPromptSubmit").count() == 1`.
- `merge_refuses_semantically_invalid_hook_containers`: `merge_hooks(x, "codex", NAMING)`.
- `managed_commands_match_each_windows_hook_shell`: `managed_command("codex", "UserPromptSubmit")` expects `... title-event --agent codex --event UserPromptSubmit >/dev/null 2>&1 || true; fi`; `managed_command("claude", "UserPromptSubmit")` expects `... title-event --agent claude --event UserPromptSubmit *> $null } } catch {}; exit 0`; `managed_command("grok", "UserPromptSubmit")`; both `merge_hook` calls become `merge_hooks(serde_json::json!({}), agent, NAMING)`.
- `codex_windows_relay_is_shell_neutral_for_absent_and_failing_helpers`: `merge_hooks(serde_json::json!({}), "codex", NAMING)`.
- `installer_creates_backups_once_and_refuses_malformed_json`: every `install_in(&roots)` becomes `install_in(&roots, NAMING)`.

- [ ] **Step 2: Write the failing tests**

Add to the `src/agent_hooks.rs` test module:

```rust
    fn roots_in(temp: &tempfile::TempDir) -> HookRoots {
        HookRoots {
            claude: temp.path().join("claude"),
            codex: temp.path().join("codex"),
            grok: temp.path().join("grok"),
        }
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn state_install_adds_each_providers_events_with_the_event_name() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        let report = install_in(&roots, STATE);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        for (agent, path) in [
            ("claude", roots.claude.join("settings.json")),
            ("codex", roots.codex.join("hooks.json")),
            ("grok", roots.grok.join("hooks").join(GROK_FILE)),
        ] {
            let root = read_json(&path);
            for event in wanted_events(agent, STATE) {
                let handler = &root["hooks"][*event][0]["hooks"][0];
                assert!(is_managed_handler(handler), "{agent} {event}: {handler}");
                let text = serde_json::to_string(handler).unwrap();
                assert!(text.contains(&format!("--event {event}")), "{agent} {event}: {text}");
            }
            assert!(root["hooks"].get("SessionStart").is_none(), "{agent} must not install SessionStart");
        }
        assert!(read_json(&roots.codex.join("hooks.json"))["hooks"].get("StopFailure").is_none());
        assert!(read_json(&roots.grok.join("hooks").join(GROK_FILE))["hooks"].get("PermissionRequest").is_none());
    }

    #[test]
    fn turning_state_off_removes_its_events_and_keeps_naming() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        install_in(&roots, STATE);
        let report = install_in(&roots, NAMING);
        assert!(report.changed > 0);
        let claude = read_json(&roots.claude.join("settings.json"));
        let hooks = claude["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().collect::<Vec<_>>(), vec!["UserPromptSubmit"], "{hooks:?}");
        let report = install_in(&roots, HookWants::default());
        assert!(report.changed > 0);
        let claude = read_json(&roots.claude.join("settings.json"));
        assert!(claude["hooks"].as_object().unwrap().is_empty(), "{claude}");
    }

    #[test]
    fn reinstalls_never_move_a_handler() {
        // Codex keys hook trust by position; any reorder forces re-trust.
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        std::fs::create_dir_all(&roots.codex).unwrap();
        let codex = roots.codex.join("hooks.json");
        std::fs::write(
            &codex,
            br#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"keep-me"}]}],"Stop":[{"hooks":[{"type":"command","command":"also-keep"}]}]}}"#,
        )
        .unwrap();
        install_in(&roots, NAMING);
        let after_naming = std::fs::read(&codex).unwrap();
        install_in(&roots, STATE);
        let after_state = read_json(&codex);
        let prompt_groups = after_state["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(prompt_groups[0]["hooks"][0]["command"], "keep-me");
        assert!(is_managed_handler(&prompt_groups[1]["hooks"][0]));
        assert_eq!(prompt_groups.len(), 2);
        let stop_groups = after_state["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_groups[0]["hooks"][0]["command"], "also-keep");
        assert!(is_managed_handler(&stop_groups[1]["hooks"][0]));
        assert_eq!(install_in(&roots, STATE).changed, 0, "state twice is a no-op");
        install_in(&roots, NAMING);
        assert_eq!(std::fs::read(&codex).unwrap(), after_naming, "back to naming restores the exact bytes");
    }

    #[test]
    fn an_older_handler_without_an_event_flag_is_replaced_in_place() {
        let old = serde_json::json!({"hooks":{"UserPromptSubmit":[
            {"hooks":[{"type":"command","command":"first"}]},
            {"hooks":[{"type":"command","command":"if [ -n \"${FOREMAN_EXE:-}\" ]; then \"$FOREMAN_EXE\" title-event --agent codex >/dev/null 2>&1 || true; fi"}]},
            {"hooks":[{"type":"command","command":"last"}]}
        ]}});
        let merged = merge_hooks(old, "codex", NAMING).unwrap();
        let groups = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0]["hooks"][0]["command"], "first");
        assert_eq!(groups[1]["hooks"][0]["command"], managed_command("codex", "UserPromptSubmit"));
        assert_eq!(groups[2]["hooks"][0]["command"], "last");
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent agent_hooks`
Expected: compile errors — `HookWants`, `wanted_events`, `merge_hooks` not found.

- [ ] **Step 4: Implement**

In `src/agent_hooks.rs`:

1. Module doc and types:

```rust
//! Managed global lifecycle hooks for Claude, Codex, and Grok: Session naming
//! and agent state share one hook command, installed on the events the
//! settings ask for.

/// Which features want hooks. The installed event set is derived from both,
/// so changing either setting reruns the installer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookWants {
    pub naming: bool,
    pub state: bool,
}

impl HookWants {
    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        Self {
            naming: settings.auto_name_agent_sessions,
            state: settings.agent_state_badges,
        }
    }
}

/// Every event Foreman has ever managed, so an event that is no longer wanted
/// is cleaned up rather than left behind.
const KNOWN_EVENTS: [&str; 6] = [
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "Interrupt",
];

fn wanted_events(agent: &str, wants: HookWants) -> &'static [&'static str] {
    if wants.state {
        match agent {
            "claude" => &["UserPromptSubmit", "PermissionRequest", "PostToolUse", "Stop", "StopFailure"],
            "codex" => &["UserPromptSubmit", "PermissionRequest", "PostToolUse", "Stop", "Interrupt"],
            _ => &["UserPromptSubmit", "PostToolUse", "Stop", "StopFailure"],
        }
    } else if wants.naming {
        &["UserPromptSubmit"]
    } else {
        &[]
    }
}
```

Until Task 4 adds the field, write `state: false` in `from_settings` with a `// Task 4 wires the setting.` comment; Task 4 replaces it.

2. Thread `wants` through: `spawn_install(ctx, wants)` → `install(wants)` → `install_in(&roots, wants)` → `install_one(&path, agent, wants)`. `install_one` replaces its `merge_hook(root, agent)?` call with `merge_hooks(root, agent, wants)?`. Everything else in `install_one` (backup once, compare bytes, atomic write) stays.

3. Commands take the event: `managed_command(agent, event)`, `unix_managed_command(agent, event)`, `windows_powershell_relay(agent, event)`. Each replaces its literal `title-event --agent {agent}` with `title-event --agent {agent} --event {event}`. `is_managed_command` is unchanged: it matches on `title-event --agent `, so handlers written by an older Foreman (no `--event`) are still recognized and replaced.

4. Build the handler once per event, extracted from today's `merge_hook`:

```rust
fn managed_handler(agent: &str, event: &str) -> serde_json::Value {
    let mut handler = serde_json::json!({
        "type": "command",
        "command": managed_command(agent, event),
        "timeout": 1
    });
    let object = handler.as_object_mut().expect("handler literal is an object");
    // Both schemas support an explicit background command. The hook is
    // passive, so helper startup must never delay the agent. Grok's events
    // are already non-blocking and have no documented per-handler `async`.
    if matches!(agent, "claude" | "codex") {
        object.insert("async".into(), serde_json::json!(true));
    }
    if cfg!(windows) {
        if agent == "claude" {
            object.insert("shell".into(), serde_json::json!("powershell"));
        } else if agent == "codex" {
            // `commandWindows` selects a platform string, not an execution
            // shell: Codex uses the Session's selected shell when one exists,
            // so the encoded relay has to work under both cmd.exe and
            // PowerShell.
            object.insert(
                "commandWindows".into(),
                serde_json::json!(windows_powershell_relay(agent, event)),
            );
            object.insert("timeout".into(), serde_json::json!(2));
        }
    }
    handler
}
```

5. Replace `merge_hook` with `merge_hooks` and `merge_event`:

```rust
fn merge_hooks(
    mut root: serde_json::Value,
    agent: &str,
    wants: HookWants,
) -> Result<serde_json::Value, String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "the hook file must contain a JSON object".to_string())?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "the existing `hooks` value must be a JSON object".to_string())?;
    let wanted = wanted_events(agent, wants);
    for event in KNOWN_EVENTS {
        merge_event(hooks, agent, event, wanted.contains(&event))?;
    }
    Ok(root)
}

/// Update one event's handler list in place: the first managed handler is
/// replaced where it stands, extra managed handlers are removed, and a new
/// one is appended only when none existed. Unmanaged handlers never move:
/// Codex keys hook trust by position.
fn merge_event(
    hooks: &mut serde_json::Map<String, serde_json::Value>,
    agent: &str,
    event: &str,
    wanted: bool,
) -> Result<(), String> {
    let desired = wanted.then(|| managed_handler(agent, event));
    let Some(groups) = hooks.get_mut(event) else {
        if let Some(handler) = desired {
            hooks.insert(event.into(), serde_json::json!([{ "hooks": [handler] }]));
        }
        return Ok(());
    };
    let groups = groups
        .as_array_mut()
        .ok_or_else(|| format!("the existing `hooks.{event}` value must be an array"))?;
    let mut placed = false;
    for group in groups.iter_mut() {
        let Some(handlers) = group
            .get_mut("hooks")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let had_handlers = !handlers.is_empty();
        let mut kept = Vec::with_capacity(handlers.len());
        for handler in handlers.drain(..) {
            if !is_managed_handler(&handler) {
                kept.push(handler);
            } else if !placed && let Some(desired) = &desired {
                kept.push(desired.clone());
                placed = true;
            }
        }
        *handlers = kept;
        // A group we emptied goes away; a group the user left empty stays.
        if had_handlers && handlers.is_empty() {
            *group = serde_json::Value::Null;
        }
    }
    groups.retain(|group| !group.is_null());
    if let Some(handler) = desired
        && !placed
    {
        groups.push(serde_json::json!({ "hooks": [handler] }));
    }
    if groups.is_empty() {
        hooks.remove(event);
    }
    Ok(())
}
```

6. In `src/main.rs`, both `agent_hooks::spawn_install(...)` calls gain a second argument: `agent_hooks::HookWants::from_settings(&startup_settings)` at startup, and `agent_hooks::HookWants::from_settings(&self.settings)` in the settings-change block. Task 4 rewrites the trigger logic; this step only makes the tree compile.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent agent_hooks`
Expected: all PASS, including the pre-existing installer tests.

- [ ] **Step 6: Commit**

```
feat(agent-hooks): Install the naming hook on lifecycle events for state

The installed event set is derived from the naming and state settings.
Handlers are updated in place and removed from unwanted events, so a
reinstall never moves a handler: Codex keys hook trust by position.
```

---

### Task 4: Setting, install trigger, GUI apply

**Files:**
- Modify: `src/config.rs`, `src/settings_menu.rs`, `src/main.rs`, `src/terminal.rs`, `src/wm.rs`, `src/agent_hooks.rs` (`from_settings`)

**Interfaces:**
- Consumes: `title_notify::TitlePromptEvent` (`hook_event`, `prompt`), `agent_state::AgentStateSlot`, `agent_hooks::{spawn_install, HookWants}`.
- Produces:
  - `Settings::agent_state_badges: bool` (default `false`)
  - `Session::agent_state(&self) -> &AgentStateSlot`, `Session::agent_state_mut(&mut self) -> &mut AgentStateSlot`
  - `WindowManager::apply_hook_event(&mut self, event: &title_notify::TitlePromptEvent) -> bool` (true when a Session accepted it)
  - `App::hook_reinstall: bool` (an install requested while one was running)

- [ ] **Step 1: Write the failing tests**

In `src/config.rs` tests: in `new_fields_default_when_missing_from_old_file` add `assert!(!s.agent_state_badges);` beside the `auto_name_agent_sessions` line; in `settings_roundtrip_preserves_new_fields` set `s.agent_state_badges = true;` and assert `back.agent_state_badges`.

In `src/wm.rs` tests, next to the `prepare_title_request` test that sets `wm.tag` and `set_osc_title_for_test`:

```rust
    #[test]
    fn hook_events_reach_only_the_matching_project_terminal_and_provider() {
        use crate::agent_state::{AgentState, HookEvent};
        use crate::terminal_titles::SourceAgent;
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p9".into());
        let id = wm.add_terminal(Shell::Cmd, &ctx).expect("shell");
        {
            let window = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(session) = &mut window.tabs[0].content else {
                panic!("expected terminal");
            };
            session.set_osc_title_for_test(Some("claude".into()));
        }
        let event = |source: SourceAgent, project: &str, hook_event: HookEvent| {
            crate::title_notify::TitlePromptEvent {
                source_agent: source,
                hook_event,
                vendor_session_id: "s1".into(),
                transcript_path: None,
                project_id: Some(project.into()),
                terminal_id: term_tag(id),
                prompt: None,
            }
        };
        let state = |wm: &mut WindowManager| {
            let window = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(session) = &mut window.tabs[0].content else {
                panic!("expected terminal");
            };
            session.agent_state().badge(session.icon_kind(), false).map(|b| b.state)
        };
        assert!(
            !wm.apply_hook_event(&event(SourceAgent::Claude, "p1", HookEvent::UserPromptSubmit)),
            "terminal ids repeat across projects"
        );
        assert_eq!(state(&mut wm), None);
        assert!(wm.apply_hook_event(&event(SourceAgent::Claude, "p9", HookEvent::PermissionRequest)));
        assert_eq!(state(&mut wm), Some(AgentState::NeedsYou));
        // Codex launched from this Claude pane's Bash tool shares its terminal
        // id. Its events must not overwrite the pane's state.
        assert!(!wm.apply_hook_event(&event(SourceAgent::Codex, "p9", HookEvent::Stop)));
        assert_eq!(state(&mut wm), Some(AgentState::NeedsYou));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent hook_events_reach config`
Expected: compile errors — `agent_state_badges`, `apply_hook_event`, `agent_state` not found.

- [ ] **Step 3: Implement**

1. `src/config.rs`: add after `auto_name_agent_sessions`:

```rust
    /// Show Working / Needs you / Idle on agent rows in the Sessions panel.
    /// Enabling installs guarded global lifecycle hooks; disabling removes them.
    pub agent_state_badges: bool,
```

and `agent_state_badges: false,` in `Default`.

2. `src/agent_hooks.rs` `HookWants::from_settings`: `state: settings.agent_state_badges`.

3. `src/settings_menu.rs`: add `Field::AgentStateBadges` after `AutoNameAgentSessions` in the enum; a `RowSpec` right after the `AutoNameAgentSessions` row:

```rust
            RowSpec {
                field: Field::AgentStateBadges,
                label: "Show agent state in the Sessions panel",
                desc: "Installs guarded global hooks on a few lifecycle events; Codex may ask you to trust them",
                kind: Kind::Toggle,
            },
```

and the two match arms beside `AutoNameAgentSessions`: `Field::AgentStateBadges => flip(&mut s.agent_state_badges),` and `Field::AgentStateBadges => s.agent_state_badges.to_string(),`. Fix any other exhaustive `Field` match the compiler reports the same way.

4. `src/terminal.rs` `Session`: add a field `agent_state: crate::agent_state::AgentStateSlot,` (initialize `agent_state: Default::default(),` in the `Session { .. }` literal that sets `dispatch_argv: None`), the two accessors:

```rust
    pub fn agent_state(&self) -> &crate::agent_state::AgentStateSlot {
        &self.agent_state
    }

    pub fn agent_state_mut(&mut self) -> &mut crate::agent_state::AgentStateSlot {
        &mut self.agent_state
    }
```

and in `show`, right after `self.clear_bell();` inside `if active {`:

```rust
            self.agent_state.clear_finished();
```

5. `src/wm.rs`, beside `prepare_title_request`:

```rust
    /// Apply a hook event to the Session it names. Terminal ids repeat across
    /// projects, so the project tag must match too. A provider that does not
    /// match the pane's icon is another agent running inside this one; its
    /// events are dropped so they cannot overwrite the pane's state.
    pub fn apply_hook_event(&mut self, event: &crate::title_notify::TitlePromptEvent) -> bool {
        if self.tag.as_deref() == event.project_id.as_deref() {
            for window in &mut self.windows {
                for tab in &mut window.tabs {
                    let Content::Terminal(session) = &mut tab.content else {
                        continue;
                    };
                    if term_tag(session.term_id()) != event.terminal_id {
                        continue;
                    }
                    if !source_matches_icon(event.source_agent, session.icon_kind()) {
                        return false;
                    }
                    session
                        .agent_state_mut()
                        .apply(event.source_agent, event.hook_event);
                    return true;
                }
            }
        }
        self.windows.iter_mut().any(|window| {
            window.tabs.iter_mut().any(|tab| match &mut tab.content {
                Content::Project(child) => child.apply_hook_event(event),
                _ => false,
            })
        })
    }
```

6. `src/main.rs`:

   - `App` gains `hook_reinstall: bool` (doc: `/// A hook install was requested while one was running; rerun when it ends.`), initialized `false` in `App::new`.
   - Replace the body of `drain_title_events` so every event is applied and only prompt-bearing ones start naming:

```rust
    fn drain_title_events(&mut self) -> bool {
        let mut activity = false;
        while let Ok(event) = self.title_events.try_recv() {
            activity = true;
            // State applies even while badges are hidden, so enabling the
            // setting shows the current state instead of waiting for a turn.
            self.desktop.apply_hook_event(&event);
            if event.prompt.is_none() || !self.settings.auto_name_agent_sessions {
                continue;
            }
            let epoch = self.title_epoch.load(std::sync::atomic::Ordering::Acquire);
            let Some(request) = self.desktop.prepare_title_request(
                &event,
                self.settings.title_provider,
                &self.settings.title_model,
                epoch,
            ) else {
                continue;
            };
            if let Err(error) = self.title_requests.try_send(request) {
                let request = match error {
                    std::sync::mpsc::TrySendError::Full(request)
                    | std::sync::mpsc::TrySendError::Disconnected(request) => request,
                };
                self.desktop
                    .apply_title_result(terminal_titles::TitleResult {
                        identity: request.identity,
                        title: Err(terminal_titles::TitleError::Stale),
                    });
            }
        }
        // (the existing `title_results` drain stays below, unchanged)
```

   - Add the request helper and make the poller rerun a queued request:

```rust
    /// At most one installer runs at a time; a request during a run is kept
    /// and replayed when it finishes, so a quick on-off-on never loses the
    /// final state.
    fn request_hook_install(&mut self, ctx: &egui::Context) {
        if self.hook_install.is_some() {
            self.hook_reinstall = true;
            return;
        }
        let wants = agent_hooks::HookWants::from_settings(&self.settings);
        self.hook_install = Some(agent_hooks::spawn_install(ctx.clone(), wants));
    }
```

     `poll_hook_install` takes `ctx: &egui::Context`; after it sets `self.hook_install = None` and pushes its notification, add:

```rust
        if self.hook_reinstall {
            self.hook_reinstall = false;
            self.request_hook_install(ctx);
        }
```

     Its notification strings become `"Agent hooks are up to date"`, `"Agent hooks installed ({})"`, and `"Agent hook install failed: {}"`. Update the call site in `service_background` to `self.poll_hook_install(ctx);`.

   - In the settings-change block: before `if *live_cfg != self.settings {`, add `let wants_before = agent_hooks::HookWants::from_settings(&self.settings);`. Replace the `if !naming_was_enabled && ... { self.hook_install = Some(...) }` inside `if naming_changed { .. }` with nothing (keep the epoch bump and `invalidate_title_requests`), and after that block add:

```rust
        if agent_hooks::HookWants::from_settings(&self.settings) != wants_before {
            self.request_hook_install(&ctx);
        }
```

     Delete `naming_was_enabled` if nothing else reads it.
   - Startup (inside the `run_native` closure): 

```rust
            let wants = agent_hooks::HookWants::from_settings(&startup_settings);
            let hook_install = (wants != agent_hooks::HookWants::default())
                .then(|| agent_hooks::spawn_install(cc.egui_ctx.clone(), wants));
```

   - Raise the title channel: `std::sync::mpsc::sync_channel(64)` where `title_event_tx` is created (tool-heavy turns across several panes send more than one event between frames).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent`
Expected: whole suite PASS (known-flaky tests per **foreman-validation-and-qa** may need one re-run; report any that fail twice).

- [ ] **Step 5: Commit**

```
feat(agent-state): Apply hook events to their Session

Adds the agent_state_badges setting (off by default). The installer
reruns whenever the naming or state toggle changes, and drained hook
events reach the matching Session after the same project, terminal,
and provider match naming uses. Focus clears the finished marker, as
it does the Bell.
```

---

### Task 5: Panel badges (`src/wm.rs`, `src/panel.rs`)

**Files:**
- Modify: `src/wm.rs` (`panel_model`), `src/panel.rs`

**Interfaces:**
- Consumes: `Session::agent_state`, `AgentStateSlot::badge`, `agent_state::{AgentBadge, AgentState, badge_label}`, `Settings::agent_state_badges`.
- Produces: `TabEntry::agent: Option<AgentBadge>`, `RowPaintOwned::agent: Option<AgentBadge>`.

The project-row roll-up ("needs you" on a collapsed Project) is deferred by the spec. Do not add it.

- [ ] **Step 1: Write the failing test**

In `src/wm.rs` tests, beside `bell_latches_the_stack_until_cleared_and_reaches_the_panel`:

```rust
    #[test]
    fn agent_state_reaches_the_panel_row() {
        use crate::agent_state::{AgentState, HookEvent};
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
        let ask = crate::title_notify::TitlePromptEvent {
            source_agent: crate::terminal_titles::SourceAgent::Claude,
            hook_event: HookEvent::PermissionRequest,
            vendor_session_id: "s1".into(),
            transcript_path: None,
            project_id: Some("p1".into()),
            terminal_id: term_tag(id),
            prompt: None,
        };
        assert!(m.apply_hook_event(&ask));

        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let mut desk = WindowManager::new();
        desk.push_win(7, Tab::fixed("proj", Content::Project(Box::new(m))), r);
        let pm = desk.panel_model();
        let states: Vec<Option<AgentState>> =
            pm.projects[0].tabs.iter().map(|t| t.agent.map(|b| b.state)).collect();
        assert_eq!(
            states.iter().filter(|s| **s == Some(AgentState::NeedsYou)).count(),
            1,
            "{states:?}"
        );
        assert_eq!(states.iter().filter(|s| s.is_none()).count(), 1, "plain shell shows nothing");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --target-dir target/agent agent_state_reaches_the_panel`
Expected: compile error — no field `agent` on `TabEntry`.

- [ ] **Step 3: Implement the model**

- `src/panel.rs` `TabEntry`, after `bell`:

```rust
    /// Hook-driven agent state; `None` = no badge (not an agent, no event yet,
    /// or exited). Not gated by the setting here: the panel gates at paint.
    pub agent: Option<crate::agent_state::AgentBadge>,
```

- `src/wm.rs` `panel_model`, in the `tabs.push(TabEntry { .. })` literal after `bell:`:

```rust
                            agent: match &t.content {
                                Content::Terminal(s) => {
                                    s.agent_state().badge(s.icon_kind(), s.has_exited())
                                }
                                _ => None,
                            },
```

Fix every other `TabEntry` literal the compiler reports (tests included) with `agent: None`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --target-dir target/agent agent_state_reaches_the_panel`
Expected: PASS.

- [ ] **Step 5: Implement painting**

In `src/panel.rs`:

1. `RowPaintOwned`, after `bell`: `/// Agent state label for this terminal row (already gated by the setting).` `agent: Option<crate::agent_state::AgentBadge>,`.
2. In each layout that builds `RowPaintOwned` for tab rows, compute once per pass next to its `bell_gate`: `let state_gate = crate::config::live(ui.ctx()).agent_state_badges;` and set `agent: state_gate.then_some(t.agent).flatten(),`. Project rows get `agent: None`. Any other `RowPaintOwned` literal gets `agent: None`.
3. Title reserve, in the `reserve` chain right after the `} else if over {` arm:

```rust
        } else if rp.agent.is_some_and(|b| b.state == crate::agent_state::AgentState::NeedsYou) {
            58.0 // "needs you" label outranks the bell dot
        } else if rp.bell {
            20.0 // pulsing bell dot
        } else if rp.agent.is_some() {
            50.0 // "working" / "idle" / "done" label
```

   (the existing `} else if rp.bell {` arm is replaced by the three above; the `minimized || background_tab` arm follows unchanged).
4. Right-edge slot. The attention order is hover buttons > Needs you > Bell > other states > `min`/`tab`. Replace the head of the `} else if rp.bell {` arm so the chain reads:

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

   with a helper next to the other `paint_*` functions:

```rust
/// Agent state in the right-edge slot. Read-only: the draw pass never
/// mutates state (focus clears the done marker in `Session::show`).
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

- [ ] **Step 6: Build and run the full suite**

Run: `cargo build --target-dir target/agent` then `cargo test --target-dir target/agent`
Expected: clean build; suite PASS.

- [ ] **Step 7: Commit**

```
feat(panel): Show agent state on Session rows

Rows show working / needs you / idle / done from the Session's hook
state. Needs you outranks the Bell because it blocks work; the other
states rank below it. Hidden unless the setting is on.
```

---

### Task 6: Live verification, docs, and cleanup

**Files:**
- Create: `docs/agent-state.md`
- Modify: `CONTEXT.md`, `.claude/skills/foreman-agent-state-campaign/SKILL.md`, `docs/tab-icons.md` (one line: the icon now also gates the state badge)
- Delete: `docs/superpowers/plans/2026-09-25-agent-state.md` (this plan) once the feature ships

- [ ] **Step 1: Live check with real agents (needs the user)**

Build `cargo build --target-dir target/agent`. Ask the user to launch `target/agent/debug/foreman.exe`, enable **Show agent state in the Sessions panel**, trust the new Codex hooks with `/hooks`, and run the **build-screenshot** skill (user-only) at each point. Drive prompts with `foreman send` (text and Enter as two separate calls; in Git Bash set `MSYS_NO_PATHCONV=1` before sending `/exit`). Expected per row:

| Action | Claude row | Codex row (`codex --no-daemon`) |
|---|---|---|
| Launch, no prompt | nothing | nothing |
| Prompt running | working | working |
| Ask a question (Claude) | needs you | — |
| Answer it | working, then done | — |
| Turn ends while unfocused | done | done |
| Focus the pane | idle | idle |
| Esc mid-turn | stays working (accepted gap) | idle |
| Run `codex exec "echo hi"` from a Claude prompt | Claude row unchanged | — |
| `/exit` | row badge gone | row badge gone |
| Toggle the setting off | badges gone; `~/.codex/hooks.json` has only `UserPromptSubmit` | same |

Also confirm by hand: typing in a pane while its agent streams feels unchanged, and the panel stays responsive during a tool-heavy turn.

- [ ] **Step 2: Write `docs/agent-state.md`**

Follow **foreman-docs-and-writing**: `## What it does`, `## How to turn it on`, `## Gotchas` (the accepted limitations from the spec, Codex trust, badges hidden when off, toggling reinstalls hooks), and `## Key files` naming `src/agent_state.rs` `AgentStateSlot` / `HookEvent`, `src/title_notify.rs` `normalize_event` / `parse_args`, `src/agent_hooks.rs` `HookWants` / `merge_event`, `src/wm.rs` `apply_hook_event`, `src/panel.rs` `paint_state_label`. No line numbers, no counts.

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
