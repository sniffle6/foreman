//! Kitty graphics protocol — pets-first subset.
//! Spec: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md
//!
//! Pure module: no I/O, no egui, no alacritty mutation. `Session::pump` feeds
//! it the same PTY bytes alacritty parses (alacritty's vte discards APC, so
//! the grid never sees these sequences either way); `Session::show` asks what
//! to paint. Unsupported commands are skipped silently — the failure mode is
//! "image doesn't show", never a corrupted pane.

use std::collections::VecDeque;

/// One APC sequence's buffered payload cap.
#[allow(dead_code)]
const MAX_APC: usize = 8 * 1024 * 1024;

/// Byte offset in the fed chunk just past a completed command's `ESC \`.
/// `pump` advances alacritty over `chunk[..offset]`, then samples the cursor.
#[allow(dead_code)]
pub struct Cut {
    pub offset: usize,
}

#[derive(Default, Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum Scan {
    #[default]
    Ground,
    Esc,
    Apc,
    ApcEsc,
}

#[derive(Default)]
#[allow(dead_code)]
pub struct Graphics {
    scan: Scan,
    /// Current APC body (starting at its 'G'), buffered only for graphics APCs.
    apc: Vec<u8>,
    seen_first: bool,
    is_graphics: bool,
    overflow: bool,
    /// Completed-but-unapplied commands, FIFO; one per emitted `Cut`.
    pending: VecDeque<Cmd>,
    /// Partial chunked transmit reassembly: (header, accumulated payload).
    chunk: Option<(Header, Vec<u8>)>,
}

#[derive(Default, Clone)]
#[allow(dead_code)]
struct Header {
    action: u8,
    medium: u8,
    format: u32,
    cols: u16,
    rows: u16,
    pix_w: u32,
    pix_h: u32,
    id: Option<u32>,
    more: bool,
    quiet: u8,
    delete: u8,
    has_action: bool,
    has_other: bool,
}

#[allow(dead_code)]
enum Cmd {
    #[allow(dead_code)]
    Transmit {
        header: Header,
        payload: Vec<u8>,
        display: bool,
    },
    #[allow(dead_code)]
    Delete {
        spec: u8,
        id: Option<u32>,
    },
    #[allow(dead_code)]
    Query {
        header: Header,
        payload: Vec<u8>,
    },
    #[allow(dead_code)]
    Nop {
        reply: Option<Vec<u8>>,
    },
}

/// Kitty `k=v,k=v` header. Unknown keys are ignored — tolerance is the spec's
/// safety story ("silently skip, never corrupt").
#[allow(dead_code)]
fn parse_header(h: &[u8]) -> Header {
    let mut out = Header {
        action: b't',
        medium: b'd',
        format: 32,
        ..Default::default()
    };
    for kv in h.split(|&b| b == b',') {
        let mut it = kv.splitn(2, |&b| b == b'=');
        let (Some(k), Some(v)) = (it.next(), it.next()) else { continue };
        let num = |v: &[u8]| std::str::from_utf8(v).ok().and_then(|s| s.parse::<u32>().ok());
        match k {
            b"a" => {
                out.action = v.first().copied().unwrap_or(b't');
                out.has_action = true;
            }
            b"t" => {
                out.medium = v.first().copied().unwrap_or(b'd');
                out.has_other = true;
            }
            b"f" => {
                out.format = num(v).unwrap_or(32);
                out.has_other = true;
            }
            b"c" => {
                out.cols = num(v).unwrap_or(0) as u16;
                out.has_other = true;
            }
            b"r" => {
                out.rows = num(v).unwrap_or(0) as u16;
                out.has_other = true;
            }
            b"s" => {
                out.pix_w = num(v).unwrap_or(0);
                out.has_other = true;
            }
            b"v" => {
                out.pix_h = num(v).unwrap_or(0);
                out.has_other = true;
            }
            b"i" => {
                out.id = num(v);
                out.has_other = true;
            }
            b"m" => out.more = num(v) == Some(1),
            b"q" => out.quiet = num(v).unwrap_or(0) as u8,
            b"d" => {
                out.delete = v.first().copied().unwrap_or(b'a');
                out.has_other = true;
            }
            _ => out.has_other = true,
        }
    }
    out
}

#[allow(dead_code)]
fn reply(out: &mut Vec<u8>, id: Option<u32>, msg: &[u8]) {
    out.extend_from_slice(b"\x1b_G");
    if let Some(id) = id {
        out.extend_from_slice(format!("i={id}").as_bytes());
    }
    out.push(b';');
    out.extend_from_slice(msg);
    out.extend_from_slice(b"\x1b\\");
}

#[allow(dead_code)]
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
                        .position(|&c| matches!(c, 0x1b | 0x18 | 0x1a))
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
                        // ESC may be the ST; CAN/SUB abort the string like vte.
                        self.scan = if chunk[end] == 0x1b {
                            Scan::ApcEsc
                        } else {
                            Scan::Ground
                        };
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
        let rest = &body[1..]; // strip the 'G' (guaranteed by is_graphics)
        let (header, payload) = match rest.iter().position(|&b| b == b';') {
            Some(p) => (&rest[..p], &rest[p + 1..]),
            None => (rest, &rest[..0]),
        };
        let h = parse_header(header);

        let bare_continuation = !h.has_action && !h.has_other;

        if self.chunk.is_some() {
            if bare_continuation {
                let (_, data) = self.chunk.as_mut().expect("checked above");
                if data.len() + payload.len() > MAX_APC {
                    // Runaway chain — drop it whole; its trailing bare
                    // continuations are discarded by the orphan arm below.
                    self.chunk = None;
                    return false;
                }
                data.extend_from_slice(payload);
                if h.more {
                    return false;
                }
                let (first, data) = self.chunk.take().expect("checked above");
                let display = first.action == b'T';
                self.pending.push_back(Cmd::Transmit { header: first, payload: data, display });
                return true;
            }
            // A real command interleaved mid-chain violates the kitty
            // protocol; the chain is unrecoverable — drop it, honor the
            // command via the normal dispatch below.
            self.chunk = None;
        } else if bare_continuation {
            // Orphan continuation (no chain in flight, e.g. after a runaway
            // drop): not a command — discard silently (tolerance rule).
            return false;
        }

        match h.action {
            b'T' | b't' => {
                if h.more {
                    self.chunk = Some((h, payload.to_vec()));
                    return false;
                }
                let display = h.action == b'T';
                self.pending.push_back(Cmd::Transmit {
                    header: h,
                    payload: payload.to_vec(),
                    display,
                });
                true
            }
            b'd' => {
                self.pending.push_back(Cmd::Delete {
                    spec: h.delete,
                    id: h.id,
                });
                true
            }
            b'q' => {
                self.pending.push_back(Cmd::Query {
                    header: h,
                    payload: payload.to_vec(),
                });
                true
            }
            _ => {
                // Unsupported action: skip silently; honest error only when the
                // client asked for responses (q<2 suppresses nothing... q=2 all).
                let r = (h.quiet < 2).then(|| {
                    let mut v = Vec::new();
                    reply(&mut v, h.id, b"ENOTSUPPORTED");
                    v
                });
                self.pending.push_back(Cmd::Nop { reply: r });
                true
            }
        }
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

    #[test]
    fn can_or_sub_aborts_the_apc_like_vte() {
        for abort in [0x18u8, 0x1a] {
            let mut g = Graphics::default();
            // A stray CAN/SUB inside an APC aborts it; the following genuine
            // sequence must still be seen (vte is back in Ground — so are we).
            let mut input = b"\x1b_Gjunk".to_vec();
            input.push(abort);
            input.extend_from_slice(SEQ);
            let cuts = g.feed(&input);
            assert_eq!(cuts.len(), 1, "abort byte {abort:#x}");
            assert_eq!(cuts[0].offset, input.len(), "abort byte {abort:#x}");
        }
    }

    // Codex chunk format: first chunk carries the full header + m=1,
    // continuations are ESC_Gm=<flag>;<chunk>ESC\ (see codex image_protocol.rs).
    #[test]
    fn chunked_transmit_completes_only_on_the_final_chunk() {
        let mut g = Graphics::default();
        let c1 = g.feed(b"\x1b_Ga=T,t=d,f=100,c=4,r=3,q=2,i=9,m=1;cG5n\x1b\\");
        assert!(c1.is_empty());
        let c2 = g.feed(b"\x1b_Gm=1;cG5n\x1b\\");
        assert!(c2.is_empty());
        let c3 = g.feed(b"\x1b_Gm=0;cG5n\x1b\\");
        assert_eq!(c3.len(), 1);
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn delete_by_id_parses_codex_format() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
        assert_eq!(cuts.len(), 1);
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn unknown_action_queues_a_nop() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"\x1b_Ga=z,q=2;\x1b\\");
        assert_eq!(cuts.len(), 1); // one cut per completed command, even a nop
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn interleaved_command_mid_chain_drops_the_chain_and_honors_the_command() {
        let mut g = Graphics::default();
        assert!(g.feed(b"\x1b_Ga=T,t=d,f=100,q=2,i=4,m=1;AAAA\x1b\\").is_empty());
        // An explicit command mid-chain kills the chain and is itself queued.
        let cuts = g.feed(b"\x1b_Ga=d,d=I,i=9,q=2;\x1b\\");
        assert_eq!(cuts.len(), 1);
        assert_eq!(g.pending_len(), 1);
        // The dead chain's stale trailing continuation is discarded.
        assert!(g.feed(b"\x1b_Gm=0;AAAA\x1b\\").is_empty());
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn orphan_bare_continuation_is_discarded_not_promoted() {
        let mut g = Graphics::default();
        assert!(g.feed(b"\x1b_Gm=0;junkdata\x1b\\").is_empty());
        assert_eq!(g.pending_len(), 0);
    }
}
