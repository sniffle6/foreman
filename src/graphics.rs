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

/// Decoded-RGBA quota per session; oldest images evicted past this.
const MAX_STORE: usize = 64 * 1024 * 1024;
/// Placement cap — a misbehaving client can't accumulate unbounded overlays.
const MAX_PLACEMENTS: usize = 64;
/// Placements scrolled further than this into history are dropped for good.
const MAX_SCROLL_KEEP: usize = 10_000;
/// Ids we assign when the client omits i= (top bit dodges client-chosen ids).
const ANON_BASE: u32 = 0x8000_0000;
/// Decoded images are capped at 16M pixels (64 MiB RGBA) — a single image can
/// never exceed the whole store quota, and dimension-bomb inputs are rejected
/// before any large allocation.
const MAX_PIXELS: u64 = 16_000_000;

/// Term facts sampled at a cut (built by terminal.rs `term_view`).
pub struct TermView {
    pub cursor_col: usize,
    pub cursor_line: usize,
    pub alt_screen: bool,
    pub history_size: usize,
}

/// Viewport facts sampled at paint time.
pub struct ViewportView {
    pub alt_screen: bool,
    pub history_size: usize,
    pub display_offset: usize,
    pub screen_lines: usize,
}

/// One image to paint. `line` is a viewport row and may be negative (partially
/// scrolled off the top). `cols`/`rows` of 0 = derive the span from pixels.
pub struct Placed<'a> {
    pub id: u32,
    pub r#gen: u64,
    pub col: usize,
    pub line: isize,
    pub cols: u16,
    pub rows: u16,
    pub w: u32,
    pub h: u32,
    pub rgba: &'a [u8],
}

struct Image {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    r#gen: u64,
}

struct Placement {
    img: u32,
    col: usize,
    line: usize,
    alt: bool,
    /// history_size when placed — primary-screen placements scroll by the delta.
    history: usize,
    cols: u16,
    rows: u16,
}

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
    /// Partial chunked transmit reassembly: (header, accumulated payload).
    chunk: Option<(Header, Vec<u8>)>,
    images: HashMap<u32, Image>,
    placements: Vec<Placement>,
    next_anon: u32,
    r#gen: u64,
    store_bytes: usize,
}

#[derive(Default, Clone)]
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

enum Cmd {
    Transmit {
        header: Header,
        payload: Vec<u8>,
        display: bool,
    },
    Delete {
        spec: u8,
        id: Option<u32>,
    },
    Query {
        header: Header,
        payload: Vec<u8>,
    },
    Nop {
        reply: Option<Vec<u8>>,
    },
}

/// Kitty `k=v,k=v` header. Unknown keys are ignored — tolerance is the spec's
/// safety story ("silently skip, never corrupt").
fn parse_header(h: &[u8]) -> Header {
    let mut out = Header {
        action: b't',
        medium: b'd',
        format: 32,
        ..Default::default()
    };
    for kv in h.split(|&b| b == b',') {
        let mut it = kv.splitn(2, |&b| b == b'=');
        let (Some(k), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        let num = |v: &[u8]| {
            std::str::from_utf8(v)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
        };
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

fn reply(out: &mut Vec<u8>, id: Option<u32>, msg: &[u8]) {
    out.extend_from_slice(b"\x1b_G");
    if let Some(id) = id {
        out.extend_from_slice(format!("i={id}").as_bytes());
    }
    out.push(b';');
    out.extend_from_slice(msg);
    out.extend_from_slice(b"\x1b\\");
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
                self.pending.push_back(Cmd::Transmit {
                    header: first,
                    payload: data,
                    display,
                });
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

    /// Apply the next pending command using sampled term facts. Called exactly
    /// once per `Cut`, in order (see terminal.rs `advance_scanned`).
    pub fn apply(&mut self, view: TermView, out: &mut Vec<u8>) {
        let Some(cmd) = self.pending.pop_front() else {
            return;
        };
        match cmd {
            Cmd::Transmit {
                header,
                payload,
                display,
            } => match decode_image(&header, &payload) {
                Ok((w, h, rgba)) => {
                    let id = header.id.unwrap_or_else(|| {
                        self.next_anon = self.next_anon.wrapping_add(1);
                        ANON_BASE | self.next_anon
                    });
                    self.r#gen += 1;
                    self.store_bytes += rgba.len();
                    if let Some(old) = self.images.insert(
                        id,
                        Image {
                            rgba,
                            w,
                            h,
                            r#gen: self.r#gen,
                        },
                    ) {
                        self.store_bytes -= old.rgba.len();
                    }
                    self.evict_over_quota();
                    if display {
                        // Retransmit with the same id replaces its placement
                        // (pets deletes first anyway; keeps rogue clients bounded).
                        self.placements.retain(|p| p.img != id);
                        self.placements.push(Placement {
                            img: id,
                            col: view.cursor_col,
                            line: view.cursor_line,
                            alt: view.alt_screen,
                            history: view.history_size,
                            cols: header.cols,
                            rows: header.rows,
                        });
                        if self.placements.len() > MAX_PLACEMENTS {
                            self.placements.remove(0);
                        }
                    }
                    if header.quiet == 0 {
                        reply(out, header.id, b"OK");
                    }
                }
                Err(e) => {
                    if header.quiet < 2 {
                        reply(out, header.id, e.as_bytes());
                    }
                }
            },
            Cmd::Delete { spec, id } => match (spec, id) {
                (b'I' | b'i', Some(id)) => {
                    self.placements.retain(|p| p.img != id);
                    if spec == b'I'
                        && let Some(img) = self.images.remove(&id)
                    {
                        self.store_bytes -= img.rgba.len();
                    }
                }
                _ => self.placements.clear(), // bare a=d / d=a: clear visible
            },
            Cmd::Query { header, payload } => {
                let ok = header.medium == b'd'
                    && matches!(header.format, 24 | 32 | 100)
                    && decode_image(&header, &payload).is_ok();
                if ok {
                    if header.quiet == 0 {
                        reply(out, header.id, b"OK");
                    }
                } else if header.quiet < 2 {
                    reply(out, header.id, b"ENOTSUPPORTED");
                }
            }
            Cmd::Nop { reply: r } => {
                if let Some(r) = r {
                    out.extend_from_slice(&r);
                }
            }
        }
        // Scroll-cull: placements far into history can never return to view.
        let hist = view.history_size;
        self.placements
            .retain(|p| p.alt || (p.history <= hist && hist - p.history < MAX_SCROLL_KEEP));
    }

    /// What's visible right now. `line` already accounts for scrollback offset;
    /// the painter clips partially-visible images.
    pub fn visible(&self, v: &ViewportView) -> Vec<Placed<'_>> {
        let mut out = Vec::new();
        for p in &self.placements {
            if p.alt != v.alt_screen {
                continue;
            }
            if !p.alt && p.history > v.history_size {
                continue; // scrollback shrank out from under this anchor
            }
            let line = if p.alt {
                p.line as isize
            } else {
                p.line as isize - v.history_size.saturating_sub(p.history) as isize
                    + v.display_offset as isize
            };
            if line >= v.screen_lines as isize || line < -300 {
                continue; // fully below, or absurdly far above (max rows is 300)
            }
            let Some(img) = self.images.get(&p.img) else {
                continue;
            };
            out.push(Placed {
                id: p.img,
                r#gen: img.r#gen,
                col: p.col,
                line,
                cols: p.cols,
                rows: p.rows,
                w: img.w,
                h: img.h,
                rgba: &img.rgba,
            });
        }
        out
    }

    /// Paint guard: `show` skips all image work when this is false.
    pub fn active(&self) -> bool {
        !self.placements.is_empty()
    }

    /// Texture-cache retention (terminal.rs drops textures for gone images).
    pub fn has_image(&self, id: u32) -> bool {
        self.images.contains_key(&id)
    }

    fn evict_over_quota(&mut self) {
        while self.store_bytes > MAX_STORE && self.images.len() > 1 {
            let Some((&id, _)) = self.images.iter().min_by_key(|(_, i)| i.r#gen) else {
                break;
            };
            if let Some(img) = self.images.remove(&id) {
                self.store_bytes -= img.rgba.len();
            }
            self.placements.retain(|p| p.img != id);
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

fn decode_image(h: &Header, b64: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "EBASE64")?;
    match h.format {
        100 => decode_png(&data),
        32 => {
            if h.pix_w == 0 || h.pix_h == 0 || (h.pix_w as u64) * (h.pix_h as u64) > MAX_PIXELS {
                return Err("EBADRAW");
            }
            let need = (h.pix_w as u64 * h.pix_h as u64 * 4) as usize;
            if data.len() != need {
                return Err("EBADRAW");
            }
            Ok((h.pix_w, h.pix_h, data))
        }
        24 => {
            if h.pix_w == 0 || h.pix_h == 0 || (h.pix_w as u64) * (h.pix_h as u64) > MAX_PIXELS {
                return Err("EBADRAW");
            }
            let need = (h.pix_w as u64 * h.pix_h as u64 * 3) as usize;
            if data.len() != need {
                return Err("EBADRAW");
            }
            let mut rgba = Vec::with_capacity(need / 3 * 4);
            for p in data.chunks_exact(3) {
                rgba.extend_from_slice(p);
                rgba.push(255);
            }
            Ok((h.pix_w, h.pix_h, rgba))
        }
        _ => Err("ENOTSUPPORTED"),
    }
}

// pub(crate): icat's downscale path reuses this exact normalization.
pub(crate) fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let mut dec = png::Decoder::new(data);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|_| "EBADPNG")?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_PIXELS {
        return Err("ETOOBIG");
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|_| "EBADPNG")?;
    buf.truncate(info.buffer_size());
    match info.color_type {
        png::ColorType::Rgba => Ok((w, h, buf)),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(buf.len() / 3 * 4);
            for p in buf.chunks_exact(3) {
                rgba.extend_from_slice(p);
                rgba.push(255);
            }
            Ok((w, h, rgba))
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(buf.len() * 2);
            for p in buf.chunks_exact(2) {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            Ok((w, h, rgba))
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(buf.len() * 4);
            for &v in &buf {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
            Ok((w, h, rgba))
        }
        // Indexed is EXPANDed away by Transformations::EXPAND.
        _ => Err("EBADPNG"),
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
        assert!(
            g.feed(b"\x1b_Ga=T,t=d,f=100,q=2,i=4,m=1;AAAA\x1b\\")
                .is_empty()
        );
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

    use base64::Engine as _;

    /// 2x2 raw RGBA red square, transmitted+displayed with the given id.
    fn red_transmit(id: u32) -> Vec<u8> {
        let rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 4)
            .flatten()
            .collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
        format!("\x1b_Ga=T,t=d,f=32,s=2,v=2,c=2,r=1,q=2,i={id};{b64}\x1b\\").into_bytes()
    }

    fn view(col: usize, line: usize) -> TermView {
        TermView {
            cursor_col: col,
            cursor_line: line,
            alt_screen: false,
            history_size: 0,
        }
    }

    const VP: ViewportView = ViewportView {
        alt_screen: false,
        history_size: 0,
        display_offset: 0,
        screen_lines: 40,
    };

    #[test]
    fn transmit_places_at_the_sampled_cursor() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        assert_eq!(g.feed(&red_transmit(5)).len(), 1);
        g.apply(view(3, 2), &mut out);
        assert!(out.is_empty()); // q=2 — silent
        let vis = g.visible(&VP);
        assert_eq!(vis.len(), 1);
        assert_eq!((vis[0].col, vis[0].line), (3, 2));
        assert_eq!((vis[0].w, vis[0].h), (2, 2));
        assert_eq!(vis[0].rgba[0..4], [255, 0, 0, 255]);
        assert!(g.active());
    }

    #[test]
    fn delete_by_id_removes_placement_and_frees_data() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(7));
        g.apply(view(0, 0), &mut out);
        g.feed(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
        g.apply(view(0, 0), &mut out);
        assert!(g.visible(&VP).is_empty());
        assert!(!g.has_image(7));
        assert!(!g.active());
    }

    #[test]
    fn primary_screen_placement_scrolls_with_history() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(1));
        // placed at line 30 when history was 100
        g.apply(
            TermView {
                cursor_col: 0,
                cursor_line: 30,
                alt_screen: false,
                history_size: 100,
            },
            &mut out,
        );
        // 15 more lines scrolled into history since
        let v = ViewportView {
            alt_screen: false,
            history_size: 115,
            display_offset: 0,
            screen_lines: 40,
        };
        assert_eq!(g.visible(&v)[0].line, 15);
        // scrolling back 5 lines shifts it back down
        let v = ViewportView {
            alt_screen: false,
            history_size: 115,
            display_offset: 5,
            screen_lines: 40,
        };
        assert_eq!(g.visible(&v)[0].line, 20);
    }

    #[test]
    fn alt_screen_placements_only_show_on_the_alt_screen() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(2));
        g.apply(
            TermView {
                cursor_col: 1,
                cursor_line: 1,
                alt_screen: true,
                history_size: 0,
            },
            &mut out,
        );
        assert!(g.visible(&VP).is_empty()); // primary viewport
        let alt = ViewportView {
            alt_screen: true,
            ..VP
        };
        assert_eq!(g.visible(&alt).len(), 1);
    }

    #[test]
    fn transmit_with_q0_replies_ok_and_bad_payload_replies_error() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        let rgba = [255u8, 0, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
        g.feed(format!("\x1b_Ga=T,t=d,f=32,s=1,v=1,i=5;{b64}\x1b\\").as_bytes());
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=5;OK\x1b\\");
        out.clear();
        g.feed(b"\x1b_Ga=T,t=d,f=32,s=9,v=9,i=6;AAAA\x1b\\"); // wrong size for 9x9
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=6;EBADRAW\x1b\\");
    }

    #[test]
    fn codex_style_query_probe_gets_ok() {
        // ratatui-image / codex probe: 1x1 f=24 with payload AAAA.
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=31;OK\x1b\\");
    }

    #[test]
    fn png_transmit_decodes() {
        // A real PNG encoded in the test to avoid a fixture: 1x1 opaque white.
        // Generated with the png crate itself so the bytes are always valid.
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, 1, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 255, 255, 255]).unwrap();
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(format!("\x1b_Ga=T,t=d,f=100,c=4,r=2,q=2,i=8;{b64}\x1b\\").as_bytes());
        g.apply(view(0, 0), &mut out);
        let vis = g.visible(&VP);
        assert_eq!((vis[0].w, vis[0].h), (1, 1));
        assert_eq!((vis[0].cols, vis[0].rows), (4, 2));
        assert_eq!(vis[0].rgba, [255, 255, 255, 255]);
    }

    #[test]
    fn store_quota_evicts_oldest_image() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        // Each image is 1024x1024 RGBA = 4 MiB decoded; 17 of them > 64 MiB.
        let rgba = vec![128u8; 1024 * 1024 * 4];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
        for id in 1..=17u32 {
            g.feed(format!("\x1b_Ga=T,t=d,f=32,s=1024,v=1024,q=2,i={id};{b64}\x1b\\").as_bytes());
            g.apply(view(0, 0), &mut out);
        }
        assert!(!g.has_image(1), "oldest image evicted past the quota");
        assert!(g.has_image(17));
    }

    #[test]
    fn history_shrink_never_panics_and_hides_stale_placements() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(1));
        g.apply(
            TermView {
                cursor_col: 0,
                cursor_line: 5,
                alt_screen: false,
                history_size: 100,
            },
            &mut out,
        );
        // Scrollback was cleared: history went backwards.
        let v = ViewportView {
            alt_screen: false,
            history_size: 0,
            display_offset: 0,
            screen_lines: 40,
        };
        assert!(g.visible(&v).is_empty());
    }

    #[test]
    fn raw_dimension_bomb_is_rejected_without_panicking() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(b"\x1b_Ga=T,t=d,f=32,s=2147483649,v=2147483649,i=1;AAAA\x1b\\");
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=1;EBADRAW\x1b\\");
        assert!(!g.has_image(1));
    }

    #[test]
    fn png_dimension_bomb_is_rejected_before_allocation() {
        fn crc32(data: &[u8]) -> u32 {
            let mut c: u32 = 0xFFFF_FFFF;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&100_000u32.to_be_bytes());
        ihdr.extend_from_slice(&100_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        let mut png_bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        png_bytes.extend_from_slice(&13u32.to_be_bytes());
        png_bytes.extend_from_slice(&ihdr);
        png_bytes.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        png_bytes.extend_from_slice(&0u32.to_be_bytes()); // zero-length IDAT
        png_bytes.extend_from_slice(b"IDAT");
        png_bytes.extend_from_slice(&crc32(b"IDAT").to_be_bytes());
        assert_eq!(decode_png(&png_bytes), Err("ETOOBIG"));
    }

    #[test]
    fn query_with_q1_suppresses_the_ok_reply() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24,q=1;AAAA\x1b\\");
        g.apply(view(0, 0), &mut out);
        assert!(out.is_empty());
    }
}
