//! Project chat room model: an in-memory, append-only message log.
//! Pure data — the pipe/wm wiring lives in control.rs / wm.rs.
//! Spec: docs/superpowers/specs/2026-06-10-agent-group-chat-design.md §2.

use std::time::{Duration, SystemTime};

/// What a log entry is. System entries (join/exit) live in the same
/// append-only log so the transcript records membership changes, but they
/// are never injected into PTYs and never appear in `--history` output
/// (spec: chat-dispatcher-window §Model changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Post,
    Joined,
    Exited,
}

/// Crew-board staleness threshold: a live member unheard for this long
/// renders its age in amber.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// Relative age for the crew board: ("now"/"3m"/"2h", is_stale).
pub fn age_label(d: Duration) -> (String, bool) {
    let stale = d >= STALE_AFTER;
    let s = d.as_secs();
    let label = if s < 60 {
        "now".to_string()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    };
    (label, stale)
}

pub struct ChatMsg {
    pub seq: u64,
    pub from: String, // "t2" (or the slice-2 human id "you")
    /// Display name stamped at post time (the sender's tab title) so history
    /// never rewrites when a window is retitled. Falls back to `from`.
    pub name: String,
    pub text: String, // empty for system entries
    pub at: SystemTime,
    /// Mention target — render-only until v2 mention delivery sets it.
    pub to: Option<String>,
    pub kind: ChatKind,
}

impl ChatMsg {
    /// History/window line: `#14 t2: text`.
    pub fn line(&self) -> String {
        format!("#{} {}: {}", self.seq, self.from, self.text)
    }

    /// Injection framing with provenance: `[chat p1 #14] t2: text` —
    /// receivers can tell agent chat from their human, and the seq lets
    /// them reference earlier messages.
    pub fn frame(&self, project: &str) -> String {
        format!(
            "[chat {project} #{}] {}: {}",
            self.seq, self.from, self.text
        )
    }
}

/// Append-only room log. Seq is `len + 1` — messages are never removed in v1,
/// so no separate counter to drift.
pub struct ChatLog {
    msgs: Vec<ChatMsg>,
}

impl ChatLog {
    pub fn new() -> Self {
        Self { msgs: Vec::new() }
    }

    pub fn post(&mut self, from: &str, name: &str, text: &str) -> &ChatMsg {
        self.push(from, name, text, ChatKind::Post)
    }

    /// Append a membership event (join/exit). Text stays empty; the viewer
    /// derives the display line from kind + name + id.
    pub fn sys(&mut self, kind: ChatKind, from: &str, name: &str) -> &ChatMsg {
        debug_assert!(kind != ChatKind::Post, "use post() for user messages");
        self.push(from, name, "", kind)
    }

    fn push(&mut self, from: &str, name: &str, text: &str, kind: ChatKind) -> &ChatMsg {
        let name = if name.trim().is_empty() { from } else { name };
        let msg = ChatMsg {
            seq: self.msgs.len() as u64 + 1,
            from: from.to_string(),
            name: name.to_string(),
            text: text.to_string(),
            at: SystemTime::now(),
            to: None,
            kind,
        };
        self.msgs.push(msg);
        self.msgs.last().expect("just pushed")
    }

    /// Every entry, system lines included — the viewer's read path.
    pub fn msgs(&self) -> &[ChatMsg] {
        &self.msgs
    }

    /// Seq of the most recent entry (any kind); equals msgs.len() since seqs are assigned by length and entries are never removed.
    pub fn last_seq(&self) -> u64 {
        self.msgs.len() as u64
    }

    /// When `from` was last heard from (any entry kind) — crew-board ages.
    pub fn last_activity(&self, from: &str) -> Option<SystemTime> {
        self.msgs.iter().rev().find(|m| m.from == from).map(|m| m.at)
    }

    /// Last `n` POSTS as display lines, oldest first — the `--history` verb.
    /// System entries are excluded: agents asked for messages. The resulting
    /// seq gaps are harmless (seqs exist to be cited, not to be dense).
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        let mut lines: Vec<String> = self
            .msgs
            .iter()
            .rev()
            .filter(|m| m.kind == ChatKind::Post)
            .take(n)
            .map(ChatMsg::line)
            .collect();
        lines.reverse();
        lines
    }

    /// Last `fit` PHYSICAL rows for the viewer, oldest first: a multi-line
    /// message occupies one row per line, so later messages are never
    /// overpainted. (`tail_lines` stays message-per-entry for `--history`.)
    pub fn tail_rows(&self, fit: usize) -> Vec<String> {
        let mut rows = Vec::with_capacity(fit);
        'outer: for m in self.msgs.iter().rev().filter(|m| m.kind == ChatKind::Post) {
            let line = m.line();
            for l in line.rsplit('\n') {
                rows.push(l.to_string());
                if rows.len() == fit {
                    break 'outer;
                }
            }
        }
        rows.reverse();
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_assigns_increasing_seq_from_one() {
        let mut log = ChatLog::new();
        assert_eq!(log.post("t1", "worker A", "first").seq, 1);
        assert_eq!(log.post("t3", "worker B", "second").seq, 2);
    }

    #[test]
    fn post_stamps_name_time_and_kind() {
        let before = SystemTime::now();
        let mut log = ChatLog::new();
        let m = log.post("t2", "architect", "hi");
        assert_eq!(m.name, "architect");
        assert_eq!(m.kind, ChatKind::Post);
        assert_eq!(m.to, None);
        assert!(m.at >= before && m.at <= SystemTime::now());
        // blank display name falls back to the id
        let m = log.post("t9", "  ", "hi");
        assert_eq!(m.name, "t9");
    }

    #[test]
    fn line_and_frame_formats_unchanged() {
        // PROTOCOL FREEZE: --history and injection framing keep the v1 shape.
        let mut log = ChatLog::new();
        let m = log.post("t2", "architect", "taking the parser refactor");
        assert_eq!(m.line(), "#1 t2: taking the parser refactor");
        assert_eq!(m.frame("p1"), "[chat p1 #1] t2: taking the parser refactor");
    }

    #[test]
    fn sys_entries_get_seqs_but_stay_out_of_history() {
        let mut log = ChatLog::new();
        log.post("t1", "a", "m1");
        let j = log.sys(ChatKind::Joined, "t5", "architect");
        assert_eq!(j.seq, 2);
        assert_eq!(j.kind, ChatKind::Joined);
        log.post("t5", "architect", "m2");
        assert_eq!(
            log.tail_lines(10),
            vec!["#1 t1: m1".to_string(), "#3 t5: m2".to_string()],
            "history (--history) must skip system entries"
        );
        assert_eq!(log.last_seq(), 3);
        assert_eq!(log.msgs().len(), 3, "viewer sees everything");
    }

    #[test]
    fn tail_lines_slices_the_end() {
        let mut log = ChatLog::new();
        for i in 1..=5 {
            log.post("t1", "a", &format!("m{i}"));
        }
        assert_eq!(log.tail_lines(2), vec!["#4 t1: m4", "#5 t1: m5"]);
        assert_eq!(log.tail_lines(99).len(), 5);
        assert!(ChatLog::new().tail_lines(3).is_empty());
    }

    #[test]
    fn last_activity_is_latest_entry_of_any_kind() {
        let mut log = ChatLog::new();
        assert_eq!(log.last_activity("t4"), None);
        log.sys(ChatKind::Joined, "t4", "skeptic");
        let joined_at = log.msgs().last().unwrap().at;
        assert_eq!(log.last_activity("t4"), Some(joined_at), "never-posted member uses join time");
        log.post("t4", "skeptic", "hi");
        let posted_at = log.msgs().last().unwrap().at;
        assert_eq!(log.last_activity("t4"), Some(posted_at));
    }

    #[test]
    fn age_label_boundaries() {
        assert_eq!(age_label(Duration::from_secs(0)), ("now".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(59)), ("now".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(60)), ("1m".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(299)), ("4m".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(300)), ("5m".to_string(), true));
        assert_eq!(age_label(Duration::from_secs(3600)), ("1h".to_string(), true));
    }

    #[test]
    fn tail_rows_splits_multiline_messages_into_physical_rows() {
        let mut log = ChatLog::new();
        log.post("t1", "a", "a\nb");
        log.post("t2", "b", "c");
        // full window: 3 physical rows, oldest first
        assert_eq!(log.tail_rows(10), vec!["#1 t1: a", "b", "#2 t2: c"]);
        // tail-fit: the last 2 physical rows
        assert_eq!(log.tail_rows(2), vec!["b", "#2 t2: c"]);
        assert!(ChatLog::new().tail_rows(3).is_empty());
    }
}
