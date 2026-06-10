//! Project chat room model: an in-memory, append-only message log.
//! Pure data — the pipe/wm wiring lives in control.rs / wm.rs.
//! Spec: docs/superpowers/specs/2026-06-10-agent-group-chat-design.md §2.

pub struct ChatMsg {
    pub seq: u64,
    pub from: String, // "t2"
    pub text: String,
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

    pub fn post(&mut self, from: &str, text: &str) -> &ChatMsg {
        let msg = ChatMsg {
            seq: self.msgs.len() as u64 + 1,
            from: from.to_string(),
            text: text.to_string(),
        };
        self.msgs.push(msg);
        self.msgs.last().expect("just pushed")
    }

    /// Last `n` messages as display lines, oldest first.
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        let start = self.msgs.len().saturating_sub(n);
        self.msgs[start..].iter().map(ChatMsg::line).collect()
    }

    /// Last `fit` PHYSICAL rows for the viewer, oldest first: a multi-line
    /// message occupies one row per line, so later messages are never
    /// overpainted. (`tail_lines` stays message-per-entry for `--history`.)
    pub fn tail_rows(&self, fit: usize) -> Vec<String> {
        let mut rows = Vec::with_capacity(fit);
        'outer: for m in self.msgs.iter().rev() {
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
        assert_eq!(log.post("t1", "first").seq, 1);
        assert_eq!(log.post("t3", "second").seq, 2);
    }

    #[test]
    fn line_and_frame_formats() {
        let mut log = ChatLog::new();
        let m = log.post("t2", "taking the parser refactor");
        assert_eq!(m.line(), "#1 t2: taking the parser refactor");
        assert_eq!(m.frame("p1"), "[chat p1 #1] t2: taking the parser refactor");
    }

    #[test]
    fn tail_lines_slices_the_end() {
        let mut log = ChatLog::new();
        for i in 1..=5 {
            log.post("t1", &format!("m{i}"));
        }
        assert_eq!(log.tail_lines(2), vec!["#4 t1: m4", "#5 t1: m5"]);
        assert_eq!(log.tail_lines(99).len(), 5);
        assert!(ChatLog::new().tail_lines(3).is_empty());
    }

    #[test]
    fn tail_rows_splits_multiline_messages_into_physical_rows() {
        let mut log = ChatLog::new();
        log.post("t1", "a\nb");
        log.post("t2", "c");
        // full window: 3 physical rows, oldest first
        assert_eq!(log.tail_rows(10), vec!["#1 t1: a", "b", "#2 t2: c"]);
        // tail-fit: the last 2 physical rows
        assert_eq!(log.tail_rows(2), vec!["b", "#2 t2: c"]);
        assert!(ChatLog::new().tail_rows(3).is_empty());
    }
}
