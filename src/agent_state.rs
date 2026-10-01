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
