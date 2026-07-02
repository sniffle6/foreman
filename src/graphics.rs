//! Kitty graphics protocol — pets-first subset.
//! Spec: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md
//!
//! Pure module: no I/O, no egui, no alacritty mutation. `Session::pump` feeds
//! it the same PTY bytes alacritty parses (alacritty's vte discards APC, so
//! the grid never sees these sequences either way); `Session::show` asks what
//! to paint. Unsupported commands are skipped silently — the failure mode is
//! "image doesn't show", never a corrupted pane.

use std::collections::{HashMap, VecDeque};

/// One APC sequence's buffered payload cap.
const MAX_APC: usize = 8 * 1024 * 1024;

/// Byte offset in the fed chunk just past a completed command's `ESC \`.
/// `pump` advances alacritty over `chunk[..offset]`, then samples the cursor.
pub struct Cut {
    pub offset: usize,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Scan {
    #[default]
    Ground,
    Esc,
    Apc,
    ApcEsc,
}

#[derive(Default)]
pub struct Graphics {
    scan: Scan,
    /// Current APC body (starting at its 'G'), buffered only for graphics APCs.
    apc: Vec<u8>,
    seen_first: bool,
    is_graphics: bool,
    overflow: bool,
    /// Completed-but-unapplied commands, FIFO; one per emitted `Cut`.
    pending: VecDeque<Cmd>,
}

enum Cmd {
    /// Placeholder until Task 6 — a completed graphics APC body.
    #[allow(dead_code)]
    Raw(Vec<u8>),
}

impl Graphics {
    /// Scan one PTY chunk. Ground state fast-skips via byte search, so plain
    /// text costs one memchr-style pass and zero allocations.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Cut> {
        let mut cuts = Vec::new();
        let mut i = 0;
        while i < chunk.len() {
            match self.scan {
                Scan::Ground => match chunk[i..].iter().position(|&c| c == 0x1b) {
                    Some(off) => {
                        i += off + 1;
                        self.scan = Scan::Esc;
                    }
                    None => break,
                },
                Scan::Esc => {
                    match chunk[i] {
                        b'_' => {
                            self.apc.clear();
                            self.seen_first = false;
                            self.is_graphics = false;
                            self.overflow = false;
                            self.scan = Scan::Apc;
                        }
                        0x1b => {} // ESC ESC — stay in Esc for the next byte
                        _ => self.scan = Scan::Ground,
                    }
                    i += 1;
                }
                Scan::Apc => {
                    let end = chunk[i..]
                        .iter()
                        .position(|&c| c == 0x1b)
                        .map(|o| i + o)
                        .unwrap_or(chunk.len());
                    if i < end && !self.seen_first {
                        self.seen_first = true;
                        self.is_graphics = chunk[i] == b'G';
                    }
                    if self.is_graphics && !self.overflow {
                        if self.apc.len() + (end - i) > MAX_APC {
                            self.overflow = true;
                            self.apc.clear();
                        } else {
                            self.apc.extend_from_slice(&chunk[i..end]);
                        }
                    }
                    if end < chunk.len() {
                        self.scan = Scan::ApcEsc;
                        i = end + 1;
                    } else {
                        i = chunk.len();
                    }
                }
                Scan::ApcEsc => {
                    match chunk[i] {
                        b'\\' => {
                            // Sequence complete just past this byte.
                            if self.is_graphics && !self.overflow && self.ingest_apc() {
                                cuts.push(Cut { offset: i + 1 });
                            }
                            self.scan = Scan::Ground;
                        }
                        0x1b => {
                            // ESC ESC inside the APC: previous ESC was content
                            // we ignore; this one may still start the ST.
                        }
                        _ => self.scan = Scan::Ground, // aborted APC — discard
                    }
                    i += 1;
                }
            }
        }
        cuts
    }

    /// Parse a complete graphics APC body (`self.apc`, starting at 'G').
    /// Returns true when a command completed (caller emits a `Cut`).
    fn ingest_apc(&mut self) -> bool {
        let body = std::mem::take(&mut self.apc);
        self.pending.push_back(Cmd::Raw(body));
        true
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEQ: &[u8] = b"\x1b_Ga=T,t=d,f=32,s=1,v=1,c=1,r=1,q=2;AAAAAA==\x1b\\";

    #[test]
    fn plain_text_yields_no_cuts_and_buffers_nothing() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"the quick brown fox\r\njumps over\x1b[31mred\x1b[0m");
        assert!(cuts.is_empty());
        assert_eq!(g.pending_len(), 0);
    }

    #[test]
    fn one_sequence_yields_one_cut_just_past_the_terminator() {
        let mut g = Graphics::default();
        let mut input = b"before".to_vec();
        input.extend_from_slice(SEQ);
        input.extend_from_slice(b"after");
        let cuts = g.feed(&input);
        assert_eq!(cuts.len(), 1);
        assert_eq!(cuts[0].offset, 6 + SEQ.len());
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn sequence_split_at_every_byte_boundary_still_completes() {
        for split in 1..SEQ.len() {
            let mut g = Graphics::default();
            let a = g.feed(&SEQ[..split]);
            let b = g.feed(&SEQ[split..]);
            assert!(a.is_empty(), "no cut before the terminator (split {split})");
            assert_eq!(b.len(), 1, "split {split}");
            assert_eq!(b[0].offset, SEQ.len() - split, "split {split}");
        }
    }

    #[test]
    fn non_graphics_apc_is_ignored() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"\x1b_Xsome other apc\x1b\\text");
        assert!(cuts.is_empty());
        assert_eq!(g.pending_len(), 0);
    }

    #[test]
    fn lone_esc_at_chunk_end_does_not_lose_state() {
        let mut g = Graphics::default();
        assert!(g.feed(b"text\x1b").is_empty());
        assert!(g.feed(b"[31m more").is_empty()); // it was a CSI, not an APC
        assert!(g.feed(b"\x1b").is_empty());
        let cuts = g.feed(&SEQ[1..]); // rest of a graphics APC after the ESC
        assert_eq!(cuts.len(), 1);
    }

    #[test]
    fn oversized_apc_is_discarded_not_buffered() {
        let mut g = Graphics::default();
        let mut input = b"\x1b_G".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_APC + 2));
        input.extend_from_slice(b"\x1b\\");
        let cuts = g.feed(&input);
        assert!(cuts.is_empty());
        assert_eq!(g.pending_len(), 0);
    }
}
