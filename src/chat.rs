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
    /// Delivery targets (`t3` / the reserved `you`). Empty = broadcast.
    /// Set by v2 mention delivery; the viewer renders arrow meta + olive edge.
    pub to: Vec<String>,
    /// Handshake back-pointer (`--re <seq>`): the seq this post replies to.
    /// When the cited post is a `Post` whose to-set includes this sender, the
    /// reply counts as an ack. Rendered as a ` (re #N)` suffix; the post's own
    /// `seq` stays the leading authoritative id. None on plain posts.
    pub re: Option<u64>,
    pub kind: ChatKind,
}

impl ChatMsg {
    /// `t1` (broadcast) or `t1→t2,t3` (targeted) — the sender tag shared by
    /// injection framing and history lines. Untargeted output is byte-identical
    /// to v1: agents and tests parse it.
    fn from_tag(&self) -> String {
        if self.to.is_empty() {
            self.from.clone()
        } else {
            format!("{}→{}", self.from, self.to.join(","))
        }
    }

    /// Additive ` (re #N)` suffix when this post replies to one; empty
    /// otherwise, so untargeted/non-reply lines stay byte-identical to v1.
    fn re_suffix(&self) -> String {
        match self.re {
            Some(r) => format!(" (re #{r})"),
            None => String::new(),
        }
    }

    /// History/window line: `#14 t2: text` (or `#14 t2→t6 (re #9): text`).
    pub fn line(&self) -> String {
        format!(
            "#{} {}{}: {}",
            self.seq,
            self.from_tag(),
            self.re_suffix(),
            self.text
        )
    }

    /// Injection framing with provenance: `[chat p1 #14] t2: text` —
    /// receivers can tell agent chat from their human, and the seq lets
    /// them reference earlier messages. A reply adds ` (re #N)`.
    pub fn frame(&self, project: &str) -> String {
        format!(
            "[chat {project} #{}] {}{}: {}",
            self.seq,
            self.from_tag(),
            self.re_suffix(),
            self.text
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
        self.push(from, name, text, ChatKind::Post, Vec::new(), None)
    }

    /// Post with delivery targets (mentions spec §4). `to` is stored verbatim
    /// (ids incl. the reserved `you`); resolution happened at the call site.
    pub fn post_to(&mut self, from: &str, name: &str, text: &str, to: Vec<String>) -> &ChatMsg {
        self.push(from, name, text, ChatKind::Post, to, None)
    }

    /// Post carrying a handshake back-pointer (`--re <seq>`). `to` and `re` are
    /// both stored verbatim; whether the `re` actually closes a handshake is the
    /// caller's check (the cited post must be a `Post` with this sender in its
    /// to-set). See [`ChatMsg::re`].
    pub fn post_re(
        &mut self,
        from: &str,
        name: &str,
        text: &str,
        to: Vec<String>,
        re: Option<u64>,
    ) -> &ChatMsg {
        self.push(from, name, text, ChatKind::Post, to, re)
    }

    /// Append a membership event (join/exit). Text stays empty; the viewer
    /// derives the display line from kind + name + id.
    pub fn sys(&mut self, kind: ChatKind, from: &str, name: &str) -> &ChatMsg {
        debug_assert!(kind != ChatKind::Post, "use post() for user messages");
        self.push(from, name, "", kind, Vec::new(), None)
    }

    fn push(
        &mut self,
        from: &str,
        name: &str,
        text: &str,
        kind: ChatKind,
        to: Vec<String>,
        re: Option<u64>,
    ) -> &ChatMsg {
        let name = if name.trim().is_empty() { from } else { name };
        let msg = ChatMsg {
            seq: self.msgs.len() as u64 + 1,
            from: from.to_string(),
            name: name.to_string(),
            text: text.to_string(),
            at: SystemTime::now(),
            to,
            re,
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
        self.msgs
            .iter()
            .rev()
            .find(|m| m.from == from)
            .map(|m| m.at)
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
}

/// What the viewer paints, in order. Pure data so grouping/divider/meta
/// logic is testable without egui.
pub enum ChatBlock {
    /// "— architect (t5) joined —"
    Sys(String),
    /// The amber NEW rule.
    Divider,
    /// Sender header for a run of consecutive messages.
    Header {
        name: String,
        id: String,
        meta: String,
    },
    /// One message body under the current header.
    Text { text: String, to: Vec<String> },
}

/// Flatten the log into paint order. `last_seen` is the NEW watermark;
/// `compact` trims meta to the seq (narrow windows).
pub fn build_blocks(msgs: &[ChatMsg], last_seen: u64, compact: bool) -> Vec<ChatBlock> {
    let mut out = Vec::new();
    let mut current: Option<&str> = None; // sender of the open group
    let mut divider_done = false;
    for m in msgs {
        if !divider_done && m.seq > last_seen {
            out.push(ChatBlock::Divider);
            divider_done = true;
            current = None; // a divider breaks the group like a sys line
        }
        match m.kind {
            ChatKind::Joined | ChatKind::Exited => {
                let verb = if m.kind == ChatKind::Joined {
                    "joined"
                } else {
                    "exited"
                };
                out.push(ChatBlock::Sys(format!(
                    "— {} ({}) {verb} —",
                    m.name, m.from
                )));
                current = None;
            }
            ChatKind::Post => {
                if current != Some(m.from.as_str()) || !m.to.is_empty() {
                    let mut meta = if compact {
                        format!("#{}", m.seq)
                    } else {
                        let t: chrono::DateTime<chrono::Local> = m.at.into();
                        format!("{} · #{} · {}", m.from, m.seq, t.format("%H:%M"))
                    };
                    if !m.to.is_empty() {
                        meta.push_str(&format!(" · → {}", m.to.join(",")));
                    }
                    out.push(ChatBlock::Header {
                        name: m.name.clone(),
                        id: m.from.clone(),
                        meta,
                    });
                }
                // A targeted message stands alone: the next message re-headers.
                current = if m.to.is_empty() {
                    Some(m.from.as_str())
                } else {
                    None
                };
                out.push(ChatBlock::Text {
                    text: m.text.clone(),
                    to: m.to.clone(),
                });
            }
        }
    }
    out
}

/// Is `id` a well-formed mention target — `t<digits>` or the reserved `you`?
/// Format only; existence/membership is the server's check (spec §5).
pub fn valid_chat_target(id: &str) -> bool {
    id == "you"
        || id
            .strip_prefix('t')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Leading-mention extraction (mentions spec §3): whitespace-separated tokens
/// at the START of the text matching `@t<digits>` or `@you`; stops at the
/// first non-mention token. Mentions stay in the text — this is a pure read.
pub fn leading_mentions(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map_while(|tok| {
            tok.strip_prefix('@')
                .filter(|id| valid_chat_target(id))
                .map(str::to_string)
        })
        .collect()
}

/// Flag targets first, then inline mentions, deduped keeping first occurrence
/// (spec §3) — the order is what framing renders and tests assert.
pub fn effective_targets(to_flags: &[String], text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in to_flags.iter().cloned().chain(leading_mentions(text)) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Resolution of one awaited handoff (`--await-ack`). The GUI-side ack-registry
/// computes this each tick from the delivery cursor (did the post reach the
/// awaited member?) and the log (did a matching `--re` reply appear?). The two
/// timed-out states map to t4/t5's two layers and DIFFERENT human/agent actions.
// Consumed by the ack-registry tick — built in the delivery-cursor mechanism
// step (see docs/contracts/chat-handshake-contract.md). Tested now; wired next.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckState {
    /// Within the window, no resolution yet — keep waiting.
    Pending,
    /// Timed out and the cursor never reached the post's seq for the member:
    /// the handoff never landed → resend / restart that member.
    NeverLanded,
    /// Timed out, delivered, but no `--re` reply: the member has it and is
    /// presumably still working → nudge, do NOT resend.
    LandedUnacked,
    /// The awaited member replied with a matching `--re` → done. Wins even past
    /// the timeout (a late ack still resolves).
    Acked,
}

/// Pure resolution of one awaited handoff. `delivered` = the cursor has reached
/// the post's seq for the awaited member; `acked` = a matching `--re` reply
/// exists; `timed_out` = the ack window has elapsed. An ack always wins; before
/// timeout with no ack we keep waiting; on timeout the cursor splits never-landed
/// from landed-unacked.
#[allow(dead_code)] // wired into the ack-registry tick next (see AckState).
pub fn resolve_ack(delivered: bool, acked: bool, timed_out: bool) -> AckState {
    if acked {
        AckState::Acked
    } else if !timed_out {
        AckState::Pending
    } else if delivered {
        AckState::LandedUnacked
    } else {
        AckState::NeverLanded
    }
}

/// One crew-board row, assembled by the owning project manager each frame.
/// `win`/`tab` locate the member for click-to-focus. Identity is the hosting
/// window's id — the same active-tab staleness family as the rest of chat.
pub struct CrewRow {
    pub win: crate::wm::WinId,
    pub tab: usize,
    pub id: String,   // "t4"
    pub name: String, // live tab title (exit marker stripped)
    pub exited: bool,
    pub last: Option<SystemTime>,
}

/// Crew-board order: live members stalest-first (the ones to worry about),
/// never-heard counting as oldest; exited members sink to the bottom.
pub fn sort_crew(rows: &mut [CrewRow]) {
    rows.sort_by(|a, b| {
        a.exited.cmp(&b.exited).then_with(|| a.last.cmp(&b.last)) // None sorts before Some(_) = oldest first
    });
}

/// Per-window viewer state behind `Content::Chat`. The log is shared with
/// the project manager; everything else is this window's view of it.
pub struct ChatView {
    pub log: std::rc::Rc<std::cell::RefCell<ChatLog>>,
    /// Refreshed by the owning manager before each draw (`refresh_chat_view`).
    pub crew: Vec<CrewRow>,
    /// NEW-divider watermark: highest seq seen while this window had focus.
    pub last_seen: u64,
    was_active: bool,
    /// Scroll offset from the TOP of the laid-out log, in px. Meaningful only
    /// while `stick` is false.
    pub scroll: f32,
    /// Follow the tail (autoscroll). Scrolling up unsticks — the view then
    /// holds its content position while new messages arrive — and scrolling
    /// back to the bottom re-sticks (spec: scrolling decision row).
    pub stick: bool,
    /// Crew row clicked this frame; drained by the manager after the draw
    /// loop (content must never mutate sibling windows mid-draw).
    pub click: Option<(crate::wm::WinId, usize)>,
    /// In-progress input line text (slice 2).
    pub input: String,
    /// A submitted line awaiting the manager's drain (`drain_chat_posts`).
    pub pending_post: Option<String>,
}

impl ChatView {
    pub fn new(log: std::rc::Rc<std::cell::RefCell<ChatLog>>) -> Self {
        // Watermark starts at the current tail: opening the window is the
        // act of looking, so the backlog is not "new".
        let last_seen = log.borrow().last_seq();
        Self {
            log,
            crew: Vec::new(),
            last_seen,
            was_active: false,
            scroll: 0.0,
            stick: true,
            click: None,
            input: String::new(),
            pending_post: None,
        }
    }

    /// Call once per rendered frame. The watermark advances only on the
    /// focus-LOSS edge, so everything that arrived during a focused stretch
    /// stays marked NEW until the user looks away and comes back.
    pub fn on_frame(&mut self, active: bool) {
        if self.was_active && !active {
            self.last_seen = self.log.borrow().last_seq();
        }
        self.was_active = active;
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
        assert!(m.to.is_empty());
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
        assert_eq!(
            log.last_activity("t4"),
            Some(joined_at),
            "never-posted member uses join time"
        );
        log.post("t4", "skeptic", "hi");
        let posted_at = log.msgs().last().unwrap().at;
        assert_eq!(log.last_activity("t4"), Some(posted_at));
    }

    #[test]
    fn age_label_boundaries() {
        assert_eq!(
            age_label(Duration::from_secs(0)),
            ("now".to_string(), false)
        );
        assert_eq!(
            age_label(Duration::from_secs(59)),
            ("now".to_string(), false)
        );
        assert_eq!(
            age_label(Duration::from_secs(60)),
            ("1m".to_string(), false)
        );
        assert_eq!(
            age_label(Duration::from_secs(299)),
            ("4m".to_string(), false)
        );
        assert_eq!(
            age_label(Duration::from_secs(300)),
            ("5m".to_string(), true)
        );
        assert_eq!(
            age_label(Duration::from_secs(3600)),
            ("1h".to_string(), true)
        );
    }

    fn row(id: &str, exited: bool, last_secs_ago: Option<u64>) -> CrewRow {
        let now = SystemTime::now();
        CrewRow {
            win: 1,
            tab: 0,
            id: id.to_string(),
            name: id.to_string(),
            exited,
            last: last_secs_ago.map(|s| now - Duration::from_secs(s)),
        }
    }

    #[test]
    fn sort_crew_puts_stalest_live_first_and_exited_last() {
        let mut rows = vec![
            row("t1", false, Some(5)),
            row("t3", true, Some(10)),
            row("t5", false, Some(600)),
            row("t4", false, None), // never heard: treated as oldest
        ];
        sort_crew(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order, vec!["t4", "t5", "t1", "t3"]);
    }

    fn msg(seq: u64, from: &str, text: &str, kind: ChatKind) -> ChatMsg {
        ChatMsg {
            seq,
            from: from.to_string(),
            name: format!("name-{from}"),
            text: text.to_string(),
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(seq * 60),
            to: Vec::new(),
            re: None,
            kind,
        }
    }

    #[test]
    fn build_blocks_groups_consecutive_senders() {
        let msgs = vec![
            msg(1, "t4", "a", ChatKind::Post),
            msg(2, "t4", "b", ChatKind::Post),
            msg(3, "t5", "c", ChatKind::Post),
        ];
        let blocks = build_blocks(&msgs, 3, true);
        let shape: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                ChatBlock::Header { .. } => "H",
                ChatBlock::Text { .. } => "T",
                ChatBlock::Sys(_) => "S",
                ChatBlock::Divider => "D",
            })
            .collect();
        assert_eq!(shape, vec!["H", "T", "T", "H", "T"]);
    }

    #[test]
    fn build_blocks_sys_lines_break_groups_and_render_labels() {
        let msgs = vec![
            msg(1, "t4", "a", ChatKind::Post),
            msg(2, "t5", "", ChatKind::Joined),
            msg(3, "t4", "b", ChatKind::Post),
        ];
        let blocks = build_blocks(&msgs, 3, true);
        assert!(matches!(&blocks[2], ChatBlock::Sys(s) if s == "— name-t5 (t5) joined —"));
        // t4 gets a fresh header after the sys line even though it also sent #1
        assert!(matches!(&blocks[3], ChatBlock::Header { .. }));
    }

    #[test]
    fn targeted_frame_and_line_carry_the_arrow() {
        let mut log = ChatLog::new();
        let m = log.post_to("t1", "boss", "go", vec!["t2".into(), "t3".into()]);
        assert_eq!(m.line(), "#1 t1→t2,t3: go");
        assert_eq!(m.frame("p1"), "[chat p1 #1] t1→t2,t3: go");
        // untargeted stays byte-identical (regression — v1 agents parse this)
        let m = log.post("t2", "worker", "ok");
        assert_eq!(m.line(), "#2 t2: ok");
        assert_eq!(m.frame("p1"), "[chat p1 #2] t2: ok");
    }

    #[test]
    fn build_blocks_places_divider_and_formats_meta() {
        let mut m3 = msg(3, "t5", "c", ChatKind::Post);
        m3.to = vec!["t4".to_string(), "you".to_string()];
        let msgs = vec![msg(1, "t4", "a", ChatKind::Post), m3];
        // compact: seq only (+ arrow)
        let blocks = build_blocks(&msgs, 1, true);
        assert!(matches!(&blocks[0], ChatBlock::Header { meta, .. } if meta == "#1"));
        assert!(
            matches!(&blocks[2], ChatBlock::Divider),
            "divider above first seq > last_seen"
        );
        assert!(matches!(&blocks[3], ChatBlock::Header { meta, .. } if meta == "#3 · → t4,you"));
        assert!(matches!(&blocks[4], ChatBlock::Text { to, .. } if to == &["t4", "you"]));
        // comfortable: id · seq · HH:MM (don't assert the clock digits — tz-dependent)
        // last_seen 0 => everything is new, so the divider sits at blocks[0]
        // and the first header lands at blocks[1].
        let blocks = build_blocks(&msgs, 0, false);
        assert!(matches!(&blocks[0], ChatBlock::Divider));
        // Byte-length compare is timezone-invariant: HH:MM is always 5 ASCII bytes; the · separators are 2 bytes each in both strings.
        assert!(matches!(&blocks[1], ChatBlock::Header { meta, .. }
            if meta.starts_with("t4 · #1 · ") && meta.len() == "t4 · #1 · 00:00".len()));
        // nothing new => no divider
        let blocks = build_blocks(&msgs, 99, true);
        assert!(!blocks.iter().any(|b| matches!(b, ChatBlock::Divider)));
    }

    #[test]
    fn leading_mentions_take_only_the_leading_run() {
        assert_eq!(leading_mentions("@t3 take the parser"), vec!["t3"]);
        assert_eq!(leading_mentions("@t2 @you go"), vec!["t2", "you"]);
        // stops at the first non-mention token — later @s are prose
        assert_eq!(leading_mentions("@t2 hello @t3"), vec!["t2"]);
        // mid-prose mentions never target
        assert!(leading_mentions("per @t3's report, done").is_empty());
        // non-id @tokens are prose, and stop extraction
        assert!(leading_mentions("@bogus @t2 hi").is_empty());
        assert!(leading_mentions("@t hi").is_empty()); // no digits
        assert!(leading_mentions("").is_empty());
    }

    #[test]
    fn effective_targets_union_flags_then_inline_deduped() {
        let flags = vec!["t3".to_string()];
        assert_eq!(effective_targets(&flags, "@t2 @t3 go"), vec!["t3", "t2"]);
        assert!(effective_targets(&[], "plain broadcast").is_empty());
        assert_eq!(effective_targets(&[], "@you need eyes"), vec!["you"]);
    }

    #[test]
    fn re_renders_as_suffix_and_keeps_own_seq_leading() {
        let mut log = ChatLog::new();
        log.post("t6", "proto", "need GET /orders shape"); // #1
        let m = log.post_re(
            "t7",
            "mech",
            "status is an enum",
            vec!["t6".into()],
            Some(1),
        );
        // own #N stays leading authoritative; (re #N) is an additive suffix
        assert_eq!(m.line(), "#2 t7→t6 (re #1): status is an enum");
        assert_eq!(
            m.frame("p1"),
            "[chat p1 #2] t7→t6 (re #1): status is an enum"
        );
    }

    #[test]
    fn re_none_is_byte_identical_to_v1() {
        let mut log = ChatLog::new();
        let m = log.post("t2", "worker", "ok");
        assert_eq!(m.line(), "#1 t2: ok");
        assert_eq!(m.frame("p1"), "[chat p1 #1] t2: ok");
    }

    #[test]
    fn resolve_ack_covers_the_four_states() {
        // an ack wins regardless of delivery / timeout (a late ack still resolves)
        assert_eq!(resolve_ack(true, true, true), AckState::Acked);
        assert_eq!(resolve_ack(false, true, false), AckState::Acked);
        // before timeout, no ack yet => keep waiting
        assert_eq!(resolve_ack(true, false, false), AckState::Pending);
        assert_eq!(resolve_ack(false, false, false), AckState::Pending);
        // on timeout the cursor splits the two transport layers
        assert_eq!(resolve_ack(true, false, true), AckState::LandedUnacked);
        assert_eq!(resolve_ack(false, false, true), AckState::NeverLanded);
    }
}
