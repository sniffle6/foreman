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

/// Stick-to-bottom scroll step for the chat log. Pure so the fiddly parts
/// (clamp, stick threshold, unstick-on-wheel-up, re-stick-at-bottom) are
/// testable without a live egui `Context`.
///
/// `total` is the laid-out content height, `viewport_h` the visible height,
/// `wheel_dy` this frame's vertical scroll delta (egui sign: positive scrolls
/// toward the top; pass `0.0` when the log isn't hovered). `scroll` is the
/// prior offset from the top of the log; `stick` whether the view was
/// following the tail. Returns `(scroll, stick, offset)`, where `offset` is
/// the px from the top to paint at. `scroll` is only rewritten on real wheel
/// input, so a growing `total` never slides an unstuck view.
pub fn scroll_step(
    total: f32,
    viewport_h: f32,
    wheel_dy: f32,
    mut scroll: f32,
    mut stick: bool,
) -> (f32, bool, f32) {
    let max = (total - viewport_h).max(0.0);
    if wheel_dy != 0.0 {
        let cur = if stick { max } else { scroll };
        scroll = (cur - wheel_dy).clamp(0.0, max);
        stick = scroll >= max - 1.0;
    }
    let offset = if stick { max } else { scroll.min(max) };
    (scroll, stick, offset)
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

    /// Plain broadcast post. Production posts route through [`Self::post_re`] /
    /// [`Self::post_to`] (the room always carries a to-set / re), so this
    /// convenience exists only for the log's own unit tests.
    #[cfg(test)]
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

    /// Posts a member still needs: every `Post` with `seq > after` addressed
    /// to `member_id` (`to` empty = broadcast, or `member_id` in `to`), oldest
    /// first. System entries are never delivered. `after` is the member's
    /// delivery cursor (`Tab::last_delivered_seq`); this is both the catch-up
    /// replay source and the dedup boundary (chat handshake contract).
    pub fn deliver_after(&self, member_id: &str, after: u64) -> Vec<&ChatMsg> {
        self.msgs
            .iter()
            .filter(|m| {
                m.kind == ChatKind::Post
                    && m.seq > after
                    && (m.to.is_empty() || m.to.iter().any(|t| t == member_id))
            })
            .collect()
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

/// One crew-board row. Identity is the Member id (`"t4"` / `"you"`); the viewer
/// renders by id/name/exited/last and resolves a click back to its window by id,
/// so a row carries no window coordinates.
pub struct CrewRow {
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

/// Per-window viewer state behind `Content::Chat`. The room is shared with
/// the project manager; everything else is this window's view of it. The
/// viewer PULLS crew rows and paint blocks from the room each draw (it owns
/// no pushed snapshot) — the borrow stays scoped to the read.
pub struct ChatView {
    pub room: std::rc::Rc<std::cell::RefCell<ChatRoom>>,
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
    /// Member id (`tN`) of the crew row clicked this frame; drained by the
    /// manager after the draw loop (content must never mutate sibling windows
    /// mid-draw). The manager re-resolves the live terminal from the id.
    pub click: Option<String>,
    /// In-progress input line text (slice 2).
    pub input: String,
    /// A submitted line awaiting the manager's drain (`drain_chat_posts`).
    pub pending_post: Option<String>,
}

impl ChatView {
    pub fn new(room: std::rc::Rc<std::cell::RefCell<ChatRoom>>) -> Self {
        // Watermark starts at the current tail: opening the window is the
        // act of looking, so the backlog is not "new".
        let last_seen = room.borrow().last_seq();
        Self {
            room,
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
            self.last_seen = self.room.borrow().last_seq();
        }
        self.was_active = active;
    }
}

/// One member's room-side state, keyed by the `tN`/`you` id.
struct MemberState {
    name: String,
    /// Delivery cursor: the highest seq this member has been handed.
    cursor: u64,
    exited: bool,
    is_human: bool,
}

/// A live member's presence as the wiring phase observes it each frame
/// (terminal still running, ready to receive injected input, exit marker).
pub struct LiveMember {
    pub id: String,
    pub name: String,
    pub ready: bool,
    pub exited: bool,
}

/// Lines to inject into one member's PTY this frame, in seq order.
pub struct Delivery {
    pub id: String,
    pub lines: Vec<String>,
}

/// The validated, presence-aware room: a [`ChatLog`] plus a member registry.
/// Composition over the log — all posting goes through [`ChatRoom::post`]
/// (validation, auto-join) and all injection through [`ChatRoom::tick`]
/// (presence reconcile + per-member outbox). The room owns no window ids;
/// the wiring phase re-attaches window coordinates to the [`CrewRow`]s.
pub struct ChatRoom {
    log: ChatLog,
    /// Members in join order — order drives delivery and pre-sort crew rows.
    /// A `Vec` (not a map) keeps insertion order explicit and matches the
    /// rest of the file's plain-data style; membership is small (a fleet).
    members: Vec<(String, MemberState)>,
}

/// The human pseudo-member id, mirrored from `WindowManager::HUMAN_ID`.
const HUMAN_ID: &str = "you";

impl ChatRoom {
    /// A fresh room with the human pre-registered as `you` (so the human is
    /// always a valid mention target and never auto-joins on first post).
    /// The project tag is NOT stored here — it is supplied per-frame to
    /// [`ChatRoom::tick`], since the window manager may assign/rename the tag
    /// after the room exists and the framed inject line must reflect the
    /// current tag.
    pub fn new() -> Self {
        let mut room = Self {
            log: ChatLog::new(),
            members: Vec::new(),
        };
        room.members.push((
            HUMAN_ID.to_string(),
            MemberState {
                name: HUMAN_ID.to_string(),
                cursor: 0,
                exited: false,
                is_human: true,
            },
        ));
        room
    }

    fn member(&self, id: &str) -> Option<&MemberState> {
        self.members.iter().find(|(k, _)| k == id).map(|(_, v)| v)
    }

    fn member_mut(&mut self, id: &str) -> Option<&mut MemberState> {
        self.members
            .iter_mut()
            .find(|(k, _)| k == id)
            .map(|(_, v)| v)
    }

    /// The single validated post path. Resolves delivery targets from `to`
    /// + leading `@mentions`, validates them all-or-nothing (registered, not
    /// self, not exited), auto-joins a new sender (with a `Joined` line
    /// ordered before the post), then appends. Returns the new seq.
    /// Strict: any bad target is an `Err` and mutates nothing — the human's
    /// prose-fallback demotion is caller policy, not the model's.
    pub fn post(
        &mut self,
        from: &str,
        text: &str,
        to: &[String],
        re: Option<u64>,
    ) -> Result<u64, String> {
        if text.trim().is_empty() {
            return Err("empty message".to_string());
        }
        let targets = effective_targets(to, text);
        // Validate ALL targets before mutating anything (all-or-nothing).
        for t in &targets {
            if t == from {
                return Err(format!("cannot target yourself ({t})"));
            }
            match self.member(t) {
                None => return Err(format!("unknown member {t}")),
                Some(m) if m.exited => return Err(format!("{t} has exited")),
                Some(_) => {}
            }
        }
        // Auto-join a new sender (never the pre-registered human), join line
        // BEFORE the post so the transcript reads join-then-speak.
        if from != HUMAN_ID && self.member(from).is_none() {
            self.join(from, from);
        }
        let name = self
            .member(from)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| from.to_string());
        let seq = self
            .log
            .post_re(from, &name, text, targets, re)
            .seq;
        Ok(seq)
    }

    /// Idempotent join: register `id` (cursor 0, not exited, not human) and
    /// append one `Joined` sys line; a second call for a present id is a
    /// no-op. Used by dispatch auto-join.
    pub fn join(&mut self, id: &str, name: &str) {
        if self.member(id).is_some() {
            return; // already a member: no duplicate Joined line
        }
        self.members.push((
            id.to_string(),
            MemberState {
                name: name.to_string(),
                cursor: 0,
                exited: false,
                is_human: false,
            },
        ));
        self.log.sys(ChatKind::Joined, id, name);
    }

    /// Append a post from the chat pane's input line. The human (`you`) is
    /// never a terminal: leading `@mentions` narrow delivery like a CLI post,
    /// but an invalid mention (unknown, exited, or the human's own seat)
    /// demotes the post to a plain broadcast instead of erroring — the input
    /// line has no error seat. Text is kept verbatim. Returns the new seq, or
    /// `None` for blank input. This is the human's policy half of [`Self::post`].
    pub fn post_human(&mut self, text: &str) -> Option<u64> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mentions = effective_targets(&[], text);
        let to = if !mentions.is_empty()
            && mentions
                .iter()
                .all(|m| self.member(m).is_some_and(|s| !s.is_human && !s.exited))
        {
            mentions
        } else {
            Vec::new() // bad mention -> broadcast (prose fallback)
        };
        Some(self.log.post_to(HUMAN_ID, HUMAN_ID, text, to).seq)
    }

    /// Per-frame reconcile + outbox. Marks vanished/exited members exited
    /// (one `Exited` line each), then for every ready live member hands it
    /// the posts addressed to it since its cursor (excluding its own), as
    /// framed lines, advancing its cursor to the tail. `project` is the
    /// current window-manager tag, supplied per-frame so the framed inject
    /// line (`[chat p1 #N] ...`) always reflects the live tag.
    pub fn tick(&mut self, project: &str, live: &[LiveMember]) -> Vec<Delivery> {
        // --- Refresh names: a present, non-human member tracks its live
        // display name (terminal renames flow to the crew board). The human
        // seat is never in `live` and is never touched.
        for l in live {
            if let Some(m) = self.member_mut(&l.id) {
                if !m.is_human {
                    m.name = l.name.clone();
                }
            }
        }
        // --- Reconcile presence: mark vanished/exited members (once each).
        // A member is gone if it is absent from `live`, or present with
        // exited == true. The human is never reconciled.
        let newly_exited: Vec<String> = self
            .members
            .iter()
            .filter(|(_, m)| !m.is_human && !m.exited)
            .filter(|(id, _)| {
                match live.iter().find(|l| &l.id == id) {
                    None => true,             // session gone
                    Some(l) => l.exited,      // session reports exit
                }
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &newly_exited {
            if let Some(m) = self.member_mut(id) {
                m.exited = true;
            }
            let name = self.member(id).map(|m| m.name.clone()).unwrap_or_default();
            self.log.sys(ChatKind::Exited, id, &name);
        }

        // --- Outbox: deliver to each ready, non-exited, registered live
        // member the posts addressed to it past its cursor (skipping its own),
        // then advance its cursor to the tail regardless.
        let tail = self.log.last_seq();
        let mut out = Vec::new();
        for l in live {
            if !l.ready || l.exited {
                continue; // not ready / exited: cursor stays (catch-up later)
            }
            let Some(state) = self.member(&l.id) else {
                continue; // unknown to the room
            };
            if state.exited {
                continue; // just reconciled exited this tick
            }
            let cursor = state.cursor;
            let lines: Vec<String> = self
                .log
                .deliver_after(&l.id, cursor)
                .into_iter()
                .filter(|m| m.from != l.id)
                .map(|m| m.frame(project))
                .collect();
            if let Some(m) = self.member_mut(&l.id) {
                m.cursor = tail; // advance even if nothing was addressed
            }
            if !lines.is_empty() {
                out.push(Delivery {
                    id: l.id.clone(),
                    lines,
                });
            }
        }
        out
    }

    /// Presence rows for the crew board, ordered like `refresh_chat_view`:
    /// live members stalest-first (never-heard oldest), the human seat between
    /// live and exited, exited members last. Rows carry no window
    /// coordinates — `win`/`tab` are placeholder 0 (the room owns no window
    /// ids; the wiring phase re-attaches them). `now` is unused for ordering
    /// (kept for parity with the caller, which may compute ages from it).
    pub fn crew(&self, now: std::time::Instant) -> Vec<CrewRow> {
        let _ = now;
        // Real members (everything but the human seat), in registry order.
        let mut rows: Vec<CrewRow> = self
            .members
            .iter()
            .filter(|(_, m)| !m.is_human)
            .map(|(id, m)| CrewRow {
                id: id.clone(),
                name: m.name.clone(),
                exited: m.exited,
                last: self.log.last_activity(id),
            })
            .collect();
        sort_crew(&mut rows);
        // The human seat sits between live members and the exited: it is "your
        // seat", not fleet status.
        let pos = rows.iter().take_while(|r| !r.exited).count();
        rows.insert(
            pos,
            CrewRow {
                id: HUMAN_ID.to_string(),
                name: HUMAN_ID.to_string(),
                exited: false,
                last: self.log.last_activity(HUMAN_ID),
            },
        );
        rows
    }

    /// Is `id` a registered member other than the human seat (`you`)?
    /// True regardless of exited state — an exited terminal is still a member
    /// (mirrors today's status output, which lists exited terminals).
    pub fn is_member(&self, id: &str) -> bool {
        self.member(id).is_some_and(|m| !m.is_human)
    }

    /// Seq of the most recent log entry (any kind). 0 on an empty room.
    pub fn last_seq(&self) -> u64 {
        self.log.last_seq()
    }

    /// Last `n` posts as display lines, oldest first (the `--history` verb).
    pub fn history(&self, n: usize) -> Vec<String> {
        self.log.tail_lines(n)
    }

    /// Viewer paint blocks for this room's log (NEW divider at `last_seen`).
    pub fn blocks(&self, last_seen: u64, compact: bool) -> Vec<ChatBlock> {
        build_blocks(self.log.msgs(), last_seen, compact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ChatRoom -------------------------------------------------------

    fn live(id: &str, ready: bool, exited: bool) -> LiveMember {
        LiveMember {
            id: id.to_string(),
            name: id.to_string(),
            ready,
            exited,
        }
    }

    #[test]
    fn new_room_is_empty_with_human_preregistered() {
        let room = ChatRoom::new();
        assert_eq!(room.last_seq(), 0, "fresh log has no entries");
        let you = room.member("you").expect("human pre-registered");
        assert!(you.is_human);
        assert!(!you.exited);
        assert_eq!(you.cursor, 0);
        assert_eq!(you.name, "you");
    }

    #[test]
    fn room_post_rejects_empty_text() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        assert!(room.post("t1", "", &[], None).is_err());
        assert!(room.post("t1", "   ", &[], None).is_err());
        assert_eq!(room.last_seq(), 1, "only the join line, no post appended");
    }

    #[test]
    fn room_post_returns_seq_and_appends() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        let seq = room.post("t1", "hello", &[], None).expect("ok");
        assert_eq!(seq, 2, "join is #1, post is #2");
        assert_eq!(room.last_seq(), 2);
        assert_eq!(room.history(9), vec!["#2 t1: hello"]);
    }

    #[test]
    fn room_post_resolves_flags_and_mentions_deduped() {
        let mut room = ChatRoom::new();
        for id in ["t1", "t2", "t3"] {
            room.join(id, id);
        }
        // flags first, then inline @mentions, deduped keeping first.
        let seq = room
            .post("t1", "@t2 @t3 go", &["t3".to_string()], None)
            .expect("ok");
        let line = &room.blocks(0, true);
        // assert via the framed delivery target order: t3 (flag) then t2 (inline)
        assert_eq!(seq, 4); // 3 joins + this post
        let _ = line;
        // deliver_after sees the to-set; verify order through history line
        assert_eq!(
            room.history(1),
            vec!["#4 t1→t3,t2: @t2 @t3 go"],
            "flag target precedes inline mention, deduped; text kept verbatim"
        );
    }

    #[test]
    fn room_post_rejects_unknown_target_all_or_nothing() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        let before = room.last_seq();
        assert!(room.post("t1", "@t9 hi", &[], None).is_err());
        assert_eq!(room.last_seq(), before, "nothing appended on bad target");
    }

    #[test]
    fn room_post_rejects_self_mention() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        assert!(
            room.post("t1", "note", &["t1".to_string()], None).is_err(),
            "a sender cannot target itself"
        );
        // the human mentioning itself is also a self-mention
        assert!(room.post("you", "@you note", &[], None).is_err());
    }

    #[test]
    fn room_post_rejects_exited_target() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        room.join("t2", "helper");
        // mark t2 exited via a tick where it has vanished from live
        room.tick("p1", &[live("t1", true, false)]);
        let before = room.last_seq();
        assert!(room.post("t1", "@t2 hi", &[], None).is_err());
        assert_eq!(room.last_seq(), before);
    }

    #[test]
    fn room_post_you_is_a_legal_target() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        assert!(room.post("t1", "@you eyes here", &[], None).is_ok());
    }

    #[test]
    fn room_post_auto_joins_sender_join_before_post() {
        let mut room = ChatRoom::new();
        // t5 never joined; posting auto-joins it with a Joined line FIRST.
        let seq = room.post("t5", "arrived", &[], None).expect("ok");
        assert_eq!(seq, 2, "Joined is #1, the post is #2");
        // the viewer sees a Joined sys entry then the post
        let blocks = room.blocks(0, true);
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                ChatBlock::Sys(_) => "S",
                ChatBlock::Header { .. } => "H",
                ChatBlock::Text { .. } => "T",
                ChatBlock::Divider => "D",
            })
            .collect();
        // D (last_seen 0) then S (join) then H,T (the post)
        assert_eq!(kinds, vec!["D", "S", "H", "T"]);
        assert!(room.member("t5").is_some());
    }

    #[test]
    fn room_post_human_does_not_auto_join() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        room.post("you", "hi team", &[], None).expect("ok");
        // no extra Joined line for the human: only t1's join + the post
        assert_eq!(room.last_seq(), 2);
    }

    #[test]
    fn room_post_human_narrows_on_valid_mention_and_demotes_on_bad() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        // a valid leading mention narrows delivery to t1
        let seq = room.post_human("@t1 eyes here").expect("posted");
        assert_eq!(seq, 2);
        assert_eq!(room.history(1), vec!["#2 you→t1: @t1 eyes here"]);
        // an unknown mention is NOT an error: demote to a plain broadcast,
        // text kept verbatim (the input line has no error seat)
        room.post_human("@t9 anyone?").expect("posted");
        assert_eq!(room.history(1), vec!["#3 you: @t9 anyone?"]);
        // the human's own seat is not a valid recipient -> also broadcast
        room.post_human("@you note to self").expect("posted");
        assert_eq!(room.history(1), vec!["#4 you: @you note to self"]);
    }

    #[test]
    fn room_post_human_empty_is_none() {
        let mut room = ChatRoom::new();
        assert!(room.post_human("   ").is_none());
        assert_eq!(room.last_seq(), 0, "blank input appends nothing");
    }

    #[test]
    fn crew_orders_stalest_live_first_human_between_exited_last() {
        let mut room = ChatRoom::new();
        // Join in an order that does NOT match the final sort.
        room.join("t1", "alpha");
        room.join("t3", "gamma");
        room.join("t5", "epsilon");
        room.join("t4", "delta"); // will be exited
        // Activity: t1 most recent, t5 older, t3 oldest (stalest). t4 never
        // heard beyond its join, then exits.
        room.post("t3", "old", &[], None).expect("ok"); // t3 speaks first
        room.post("t5", "mid", &[], None).expect("ok");
        room.post("t1", "new", &[], None).expect("ok"); // t1 most recent
        room.post("you", "human spoke", &[], None).expect("ok");
        // t4 vanishes -> exited.
        room.tick("p1", &[
            live("t1", true, false),
            live("t3", true, false),
            live("t5", true, false),
        ]);
        let rows = room.crew(std::time::Instant::now());
        let order: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        // live stalest-first: t3 (oldest post) < t5 < t1; then `you`; t4 last.
        assert_eq!(order, vec!["t3", "t5", "t1", "you", "t4"]);
        // exited flag is carried through.
        assert!(rows.iter().find(|r| r.id == "t4").unwrap().exited);
        assert!(!rows.iter().find(|r| r.id == "you").unwrap().exited);
    }

    #[test]
    fn tick_refreshes_member_name_from_live() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        // a present live member renames -> the room tracks it.
        room.tick("p1", &[LiveMember {
            id: "t1".to_string(),
            name: "worker A".to_string(),
            ready: true,
            exited: false,
        }]);
        let row = room
            .crew(std::time::Instant::now())
            .into_iter()
            .find(|r| r.id == "t1")
            .expect("t1 row");
        assert_eq!(row.name, "worker A");
    }

    #[test]
    fn is_member_excludes_human_and_unknown_includes_exited() {
        let mut room = ChatRoom::new();
        room.join("t1", "worker");
        room.join("t2", "helper");
        assert!(room.is_member("t1"), "joined id is a member");
        assert!(!room.is_member("you"), "human seat is not a member");
        assert!(!room.is_member("t9"), "unknown id is not a member");
        // exit t2: still a member (status output lists exited terminals).
        room.tick("p1", &[live("t1", true, false)]);
        assert!(room.member("t2").unwrap().exited, "precondition: t2 exited");
        assert!(room.is_member("t2"), "an exited terminal is still a member");
    }

    #[test]
    fn join_is_idempotent() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        room.join("t1", "alpha-renamed");
        // exactly one Joined line; the second call is a no-op (no rename either)
        assert_eq!(room.last_seq(), 1);
        assert_eq!(room.member("t1").unwrap().name, "alpha");
    }

    #[test]
    fn tick_delivers_each_post_exactly_once() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        room.join("t2", "beta");
        room.post("t1", "broadcast", &[], None).expect("ok");
        let d = room.tick("p1", &[live("t1", true, false), live("t2", true, false)]);
        // t2 gets the broadcast; t1 does not (its own post).
        let t2 = d.iter().find(|x| x.id == "t2").expect("t2 delivery");
        assert_eq!(t2.lines, vec!["[chat p1 #3] t1: broadcast"]);
        assert!(d.iter().all(|x| x.id != "t1"), "sender excluded");
        // a second tick with no new posts delivers nothing.
        let d2 = room.tick("p1", &[live("t1", true, false), live("t2", true, false)]);
        assert!(d2.is_empty(), "cursor advanced; nothing re-delivered");
    }

    #[test]
    fn tick_never_delivers_a_members_own_post() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        room.post("t1", "mine", &[], None).expect("ok");
        let d = room.tick("p1", &[live("t1", true, false)]);
        assert!(d.is_empty(), "t1 must not receive its own broadcast");
    }

    #[test]
    fn tick_catch_up_holds_cursor_until_ready() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        room.join("t2", "beta");
        room.post("t1", "hello", &[], None).expect("ok");
        // t2 not ready: gets nothing, cursor must NOT advance.
        let d = room.tick("p1", &[live("t1", true, false), live("t2", false, false)]);
        assert!(d.iter().all(|x| x.id != "t2"));
        // flip ready: the backlog arrives now.
        let d = room.tick("p1", &[live("t1", true, false), live("t2", true, false)]);
        let t2 = d.iter().find(|x| x.id == "t2").expect("backlog");
        assert_eq!(t2.lines, vec!["[chat p1 #3] t1: hello"]);
    }

    #[test]
    fn tick_targeting_excludes_unaddressed_but_advances_cursor() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        room.join("t2", "beta");
        room.join("t3", "gamma");
        // a post addressed only to t2.
        room.post("t1", "@t2 secret", &[], None).expect("ok");
        let d = room.tick("p1", &[
            live("t1", true, false),
            live("t2", true, false),
            live("t3", true, false),
        ]);
        assert!(d.iter().any(|x| x.id == "t2"), "t2 addressed");
        assert!(d.iter().all(|x| x.id != "t3"), "t3 not addressed");
        // t3's cursor still advanced: a later post is the only thing it sees.
        room.post("t1", "everyone", &[], None).expect("ok");
        let d = room.tick("p1", &[
            live("t1", true, false),
            live("t2", true, false),
            live("t3", true, false),
        ]);
        let t3 = d.iter().find(|x| x.id == "t3").expect("t3 broadcast");
        assert_eq!(
            t3.lines,
            vec!["[chat p1 #5] t1: everyone"],
            "t3 never re-scans the post it was not addressed in"
        );
    }

    #[test]
    fn tick_marks_vanished_or_exited_member_once() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        room.join("t2", "beta");
        let seq_before = room.last_seq();
        // t2 vanishes from live entirely -> one Exited line.
        room.tick("p1", &[live("t1", true, false)]);
        assert_eq!(room.last_seq(), seq_before + 1, "exactly one Exited line");
        let m = room.member("t2").unwrap();
        assert!(m.exited);
        // repeated ticks: no second Exited line.
        room.tick("p1", &[live("t1", true, false)]);
        room.tick("p1", &[live("t1", true, false), live("t2", false, true)]);
        assert_eq!(room.last_seq(), seq_before + 1, "Exited is once-only");
    }

    #[test]
    fn tick_never_marks_human_exited() {
        let mut room = ChatRoom::new();
        room.join("t1", "alpha");
        // human is never in `live`; it must never be marked exited.
        room.tick("p1", &[live("t1", true, false)]);
        assert!(!room.member("you").unwrap().exited);
    }

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

    #[test]
    fn scroll_step_sticks_to_tail_as_content_grows() {
        // Stuck + no wheel: offset tracks the bottom even as `total` grows.
        let (scroll, stick, offset) = scroll_step(1000.0, 200.0, 0.0, 0.0, true);
        assert_eq!((scroll, stick, offset), (0.0, true, 800.0));
        let (_, stick, offset) = scroll_step(1200.0, 200.0, 0.0, 0.0, true);
        assert_eq!((stick, offset), (true, 1000.0));
    }

    #[test]
    fn scroll_step_no_overflow_pins_to_top() {
        // Content shorter than the viewport: max is 0, nothing scrolls.
        let (scroll, stick, offset) = scroll_step(100.0, 200.0, 50.0, 0.0, true);
        assert_eq!((scroll, stick, offset), (0.0, true, 0.0));
    }

    #[test]
    fn scroll_step_wheel_up_unsticks_and_holds() {
        // A wheel-up (positive dy) off the bottom unsticks and moves up...
        let (scroll, stick, offset) = scroll_step(1000.0, 200.0, 50.0, 0.0, true);
        assert_eq!((scroll, stick, offset), (750.0, false, 750.0));
        // ...and an unstuck view holds its position as `total` grows (no wheel).
        let (scroll, stick, offset) = scroll_step(1200.0, 200.0, 0.0, 750.0, false);
        assert_eq!((scroll, stick, offset), (750.0, false, 750.0));
    }

    #[test]
    fn scroll_step_re_sticks_at_bottom() {
        // Wheel-down past the bottom clamps to max and re-sticks.
        let (scroll, stick, offset) = scroll_step(1000.0, 200.0, -100.0, 750.0, false);
        assert_eq!((scroll, stick, offset), (800.0, true, 800.0));
        // Boundary: landing within 1px of the bottom (max-1 == 799) re-sticks;
        // a pixel short stays unstuck.
        let (_, stick_near, _) = scroll_step(1000.0, 200.0, -2.0, 797.0, false);
        assert!(stick_near, "799 >= max-1 re-sticks");
        let (_, stick_off, _) = scroll_step(1000.0, 200.0, -1.0, 797.0, false);
        assert!(!stick_off, "798 < max-1 stays unstuck");
    }

    #[test]
    fn scroll_step_clamps_at_top() {
        // Wheel-up past the top clamps at 0, stays unstuck.
        let (scroll, stick, offset) = scroll_step(1000.0, 200.0, 100.0, 10.0, false);
        assert_eq!((scroll, stick, offset), (0.0, false, 0.0));
    }

    fn row(id: &str, exited: bool, last_secs_ago: Option<u64>) -> CrewRow {
        let now = SystemTime::now();
        CrewRow {
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
    fn deliver_after_returns_addressed_posts_past_cursor() {
        let mut log = ChatLog::new();
        log.post("t1", "a", "broadcast-1"); // #1 broadcast
        log.post_to("t1", "a", "for t2", vec!["t2".into()]); // #2 -> t2
        log.post_to("t1", "a", "for t3", vec!["t3".into()]); // #3 -> t3
        log.sys(ChatKind::Joined, "t9", "late"); // #4 system entry
        log.post("t1", "a", "broadcast-2"); // #5 broadcast

        // t2 caught up through #1: sees the later broadcast #5 and its own
        // targeted #2, never t3's #3, never the system entry #4.
        let got: Vec<u64> = log.deliver_after("t2", 1).iter().map(|m| m.seq).collect();
        assert_eq!(got, vec![2, 5]);

        // cursor already at the tail: nothing left to deliver.
        assert!(log.deliver_after("t2", 5).is_empty());

        // from seq 0, t3 sees both broadcasts and its own targeted #3.
        let got: Vec<u64> = log.deliver_after("t3", 0).iter().map(|m| m.seq).collect();
        assert_eq!(got, vec![1, 3, 5]);
    }
}
