//! The **Ready gate** (see CONTEXT.md): decide when a Session may accept
//! injected chat input, and what bytes that injection becomes.
//!
//! Ready latches only after the startup device-status reply has been flushed
//! *and* the child has painted first visible output. Posts that arrive earlier
//! are queued; a post becomes bracketed-paste bytes plus a deferred submit.
//! Pure decisions only — the gate never writes the PTY. Session applies
//! [`Action::Write`] and owns the DSR/graphics reply split (only a successful
//! `resp`-buffer flush may call [`ReadyGate::on_dsr_reply_flushed`]).
//!
//! All time is injected — the gate never reads the clock itself (same pattern
//! as the Caret gate). READY_GRACE (timeout fallback for non-DSR children) is
//! intentionally not here; see docs/followups-latency-and-control.md §2.

use std::time::{Duration, Instant};

/// Gap between a chat paste and its submitting `\r`. Claude Code's TUI folds
/// input arriving within the same few-ms burst as a paste INTO the paste, so
/// a `\r` written back-to-back with `ESC[201~` becomes a literal newline in
/// the input box instead of an Enter keypress. ~150ms is past the burst window
/// while still feeling instant.
pub const SUBMIT_DELAY: Duration = Duration::from_millis(150);

/// Session-facing effect from the gate. Exactly one variant by design — every
/// Ready/inject outcome is a PTY write. A second variant during implementation
/// is a shallow-module warning: push the state back inside the gate instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Write(Vec<u8>),
}

/// Bracketed-paste wrapper (`ESC[200~ … ESC[201~`): multi-line text lands in
/// the target's input box as one paste block instead of submitting per line
/// (spec: agent-group-chat §3). Chat inject always brackets in v1.
pub fn paste_wrap(text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(text.len() + 12);
    v.extend_from_slice(b"\x1b[200~");
    // Strip ESC so a quoted `ESC[201~` can't terminate the block early and
    // turn the rest of the message into live keystrokes (alacritty does the
    // same to paste payloads).
    v.extend(text.bytes().filter(|&b| b != 0x1b));
    v.extend_from_slice(b"\x1b[201~");
    v
}

/// Scanner for the first *visible* glyph in raw PTY output — printable bytes
/// OUTSIDE any escape/control sequence. A ConPTY host emits control-only
/// chrome (DSR, DA1, mode sets, cursor homing) long before its child paints,
/// and input written in that window is eaten; the first real ink is the
/// observable "the child is up" signal readiness waits for.
/// Chunk-boundary safe: state persists across calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum InkScan {
    #[default]
    Ground,
    Esc,
    Csi,
    /// OSC/DCS/APC/PM/SOS string body — consumed until BEL or ST.
    Str,
    /// ESC inside a string: `ESC \` (ST) terminates it.
    StrEsc,
    /// ESC ( ) * + charset designation — the designator byte is consumed.
    Charset,
}

impl InkScan {
    /// Advance over `bytes`; true as soon as a visible glyph is found.
    /// Spaces don't count — ConPTY paints space runs as erasure chrome.
    fn saw_ink(&mut self, bytes: &[u8]) -> bool {
        for &b in bytes {
            *self = match (*self, b) {
                (InkScan::Ground, 0x1b) => InkScan::Esc,
                (InkScan::Ground, 0x21..=0x7e | 0x80..=0xff) => return true,
                (InkScan::Ground, _) => InkScan::Ground,
                (InkScan::Esc, b'[') => InkScan::Csi,
                (InkScan::Esc, b']' | b'P' | b'_' | b'^' | b'X') => InkScan::Str,
                (InkScan::Esc, b'(' | b')' | b'*' | b'+') => InkScan::Charset,
                (InkScan::Esc, _) => InkScan::Ground,
                (InkScan::Csi, 0x40..=0x7e) => InkScan::Ground,
                (InkScan::Csi, _) => InkScan::Csi,
                (InkScan::Str, 0x07) => InkScan::Ground,
                (InkScan::Str, 0x1b) => InkScan::StrEsc,
                (InkScan::Str, _) => InkScan::Str,
                (InkScan::StrEsc, b'\\') => InkScan::Ground,
                (InkScan::StrEsc, 0x1b) => InkScan::StrEsc,
                (InkScan::StrEsc, _) => InkScan::Str,
                (InkScan::Charset, _) => InkScan::Ground,
            };
        }
        false
    }
}

/// Owns Ready latch + chat inject queue + deferred submit. Session feeds it
/// rx chunks and DSR flush outcomes, then applies [`Action`]s to the PTY writer.
pub struct ReadyGate {
    /// When to send the deferred chat-submit `\r`.
    pub(crate) pending_submit: Option<Instant>,
    /// Chat input that arrived before Ready; flushed by [`poll`] once latched.
    pub(crate) pending_inject: Vec<String>,
    /// Half of Ready: startup device-status reply flushed successfully.
    pub(crate) dsr_replied: bool,
    /// Half of Ready: first visible glyph in PTY output (InkScan).
    pub(crate) painted: bool,
    ink: InkScan,
    /// Latched true once `dsr_replied && painted`.
    ready: bool,
}

impl ReadyGate {
    pub fn new() -> Self {
        Self {
            pending_submit: None,
            pending_inject: Vec::new(),
            dsr_replied: false,
            painted: false,
            ink: InkScan::Ground,
            ready: false,
        }
    }

    /// Test/setup helper: clear both Ready halves and the ink scanner (does not
    /// touch the inject queue or submit timer).
    pub(crate) fn clear_latch(&mut self) {
        self.dsr_replied = false;
        self.painted = false;
        self.ink = InkScan::Ground;
        self.ready = false;
    }

    /// Is injected chat input safe to send?
    pub fn ready(&self) -> bool {
        self.ready
    }

    /// Feed a raw PTY rx chunk for the paint half of Ready (InkScan).
    pub fn on_rx_chunk(&mut self, bytes: &[u8]) {
        if !self.painted && self.ink.saw_ink(bytes) {
            self.painted = true;
        }
        self.recompute_ready();
    }

    /// Outcome of flushing the alacritty `resp` buffer (device-status / CPR).
    /// Only Session's PTY-reply path may call this — never graphics replies
    /// (those must not fake readiness).
    pub fn on_dsr_reply_flushed(&mut self, sent: bool) {
        if sent {
            self.dsr_replied = true;
            self.recompute_ready();
        }
    }

    /// Queue or emit chat text. Empty text is a no-op. When not Ready, holds
    /// the post; when Ready, returns paste bytes and arms the deferred submit.
    pub fn try_inject(&mut self, text: &str, now: Instant) -> Option<Action> {
        if text.is_empty() {
            return None;
        }
        if !self.ready {
            self.pending_inject.push(text.to_string());
            return None;
        }
        self.pending_submit = Some(now + SUBMIT_DELAY);
        Some(Action::Write(paste_wrap(text)))
    }

    /// Drain queued injects (once Ready) and fire a due deferred submit.
    /// Call every pump after rx/DSR updates with the same `now`.
    pub fn poll(&mut self, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if self.ready && !self.pending_inject.is_empty() {
            for text in std::mem::take(&mut self.pending_inject) {
                if let Some(a) = self.try_inject(&text, now) {
                    out.push(a);
                }
            }
        }
        if let Some(due) = self.pending_submit
            && now >= due
        {
            self.pending_submit = None;
            out.push(Action::Write(b"\r".to_vec()));
        }
        out
    }

    fn recompute_ready(&mut self) {
        // Injection is safe once the DSR scan resolved AND the child has
        // painted: a passthrough ConPTY host answers the DSR itself long
        // before the child's input path opens (2026-07-03 chat-delivery
        // regression).
        if self.dsr_replied && self.painted {
            self.ready = true;
        }
    }
}

impl Default for ReadyGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_wrap_brackets_text_without_submitting() {
        let b = paste_wrap("line1\nline2");
        assert!(b.starts_with(b"\x1b[200~"));
        assert!(b.ends_with(b"\x1b[201~"));
        assert!(!b.contains(&b'\r'));
    }

    #[test]
    fn paste_wrap_neutralizes_embedded_paste_end() {
        let b = paste_wrap("a\x1b[201~rm -rf\r");
        assert_eq!(
            b,
            b"\x1b[200~a[201~rm -rf\r\x1b[201~".as_slice(),
            "embedded ESC must be stripped so it cannot close the paste early"
        );
    }

    #[test]
    fn ink_scan_ignores_control_chrome_and_finds_first_glyph() {
        let mut ink = InkScan::Ground;
        assert!(!ink.saw_ink(b"\x1b[1t"));
        assert!(!ink.saw_ink(b"\x1b[6n\x1b[c\x1b[?1004h\x1b[?9001h"));
        assert!(!ink.saw_ink(b"\x1b[1;1H"));
        assert!(!ink.saw_ink(b"\x1b]0;some title\x07"));
        assert!(!ink.saw_ink(b"\x1b_Gf=100,t=d;QUJD\x1b\\"));
        assert!(!ink.saw_ink(b"\x1b(B"));
        assert!(!ink.saw_ink(b"   \r\n\x08\x07"));
        assert!(ink.saw_ink(b"\x1b[?7l\x1b[?7hPress any key"));
    }

    #[test]
    fn ink_scan_survives_chunk_splits_inside_sequences() {
        let mut ink = InkScan::Ground;
        assert!(!ink.saw_ink(b"\x1b[1;1"));
        assert!(!ink.saw_ink(b"H"));
        assert!(!ink.saw_ink(b"\x1b]0;ti"));
        assert!(!ink.saw_ink(b"tle\x07"));
        assert!(ink.saw_ink(b"X"));
    }

    #[test]
    fn ready_needs_both_dsr_and_paint() {
        let mut g = ReadyGate::new();
        assert!(!g.ready());
        g.on_dsr_reply_flushed(true);
        assert!(!g.ready(), "DSR alone must not latch");
        g.on_rx_chunk(b"X");
        assert!(g.ready());
    }

    #[test]
    fn paint_alone_does_not_latch() {
        let mut g = ReadyGate::new();
        g.on_rx_chunk(b"X");
        assert!(!g.ready());
        g.on_dsr_reply_flushed(true);
        assert!(g.ready());
    }

    #[test]
    fn failed_dsr_flush_does_not_latch() {
        let mut g = ReadyGate::new();
        g.on_rx_chunk(b"X");
        g.on_dsr_reply_flushed(false);
        assert!(!g.ready());
        assert_eq!(g.dsr_replied, false);
    }

    #[test]
    fn inject_before_ready_queues_then_poll_flushes() {
        let t0 = Instant::now();
        let mut g = ReadyGate::new();
        assert!(g.try_inject("hello", t0).is_none());
        assert_eq!(g.pending_inject, ["hello"]);
        assert!(g.pending_submit.is_none());

        g.on_dsr_reply_flushed(true);
        g.on_rx_chunk(b"X");
        let acts = g.poll(t0);
        assert_eq!(acts, vec![Action::Write(paste_wrap("hello"))]);
        assert!(g.pending_inject.is_empty());
        assert!(g.pending_submit.is_some());
    }

    #[test]
    fn deferred_submit_fires_exactly_once_after_delay() {
        let t0 = Instant::now();
        let mut g = ReadyGate::new();
        g.on_dsr_reply_flushed(true);
        g.on_rx_chunk(b"X");

        let paste = g.try_inject("hello", t0).expect("paste");
        assert_eq!(paste, Action::Write(paste_wrap("hello")));
        assert!(g.poll(t0).is_empty(), "before deadline: no submit");

        // Second post inside the window refreshes the deadline (posts merge).
        let t1 = t0 + Duration::from_millis(50);
        let _ = g.try_inject("world", t1);
        assert!(g.poll(t1).is_empty());

        let t2 = t1 + SUBMIT_DELAY + Duration::from_millis(1);
        let acts = g.poll(t2);
        assert_eq!(acts, vec![Action::Write(b"\r".to_vec())]);
        assert!(g.pending_submit.is_none());
        assert!(g.poll(t2 + Duration::from_secs(1)).is_empty(), "once only");
    }

    #[test]
    fn empty_inject_is_noop() {
        let mut g = ReadyGate::new();
        g.on_dsr_reply_flushed(true);
        g.on_rx_chunk(b"X");
        assert!(g.try_inject("", Instant::now()).is_none());
        assert!(g.pending_inject.is_empty());
        assert!(g.pending_submit.is_none());
    }
}
