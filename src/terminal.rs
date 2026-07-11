use eframe::egui;
use eframe::egui::text::LayoutJob;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, GridCell, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::theme::*;

/// Coarse "some terminal produced output recently" signal for the render loop's
/// adaptive cadence. Private — poke it only through [`note_pty_output`] /
/// [`take_pty_output`]. Drives frame scheduling, never correctness.
static PTY_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Reader threads call this after delivering a PTY chunk.
pub fn note_pty_output() {
    PTY_OUTPUT.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Render loop: whether any terminal produced output since the last call, clearing
/// the flag so the next idle stretch can fall back to the slow tick.
pub fn take_pty_output() -> bool {
    PTY_OUTPUT.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Per-thread count of text layout calls during terminal paint. Thread-local so
/// parallel `cargo test` workers cannot clobber each other (unlike a process-wide
/// atomic). Cheap enough to leave on in normal builds so Session paint can call
/// [`note_layout_call`] without cfg gymnastics. Task 3 wires the note sites.
thread_local! {
    static LAYOUT_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has noted a layout call since the last reset.
pub(crate) fn layout_call_count() -> u64 {
    LAYOUT_CALLS.with(|c| c.get())
}

/// Zero the per-thread layout-call counter (test seam / frame gate).
pub(crate) fn reset_layout_call_count() {
    LAYOUT_CALLS.with(|c| c.set(0));
}

/// Record one Galley/layout creation on the Session paint path.
pub(crate) fn note_layout_call() {
    LAYOUT_CALLS.with(|c| c.set(c.get().saturating_add(1)));
}

fn indexed_rgb(i: u8) -> egui::Color32 {
    if (i as usize) < 16 {
        return PALETTE[i as usize];
    }
    if i >= 232 {
        let v = 8 + 10 * (i as u16 - 232);
        return egui::Color32::from_gray(v as u8);
    }
    let i = i - 16;
    let comp = |v: u8| -> u8 {
        if v == 0 {
            0
        } else {
            (55 + 40 * v as u16) as u8
        }
    };
    egui::Color32::from_rgb(comp(i / 36), comp((i / 6) % 6), comp(i % 6))
}

pub(crate) fn resolve(c: AnsiColor) -> Option<egui::Color32> {
    match c {
        AnsiColor::Spec(rgb) => Some(egui::Color32::from_rgb(rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) => Some(indexed_rgb(i)),
        AnsiColor::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground => Some(FG),
            NamedColor::Background => None,
            NamedColor::Cursor => Some(FG),
            NamedColor::Black => Some(PALETTE[0]),
            NamedColor::Red => Some(PALETTE[1]),
            NamedColor::Green => Some(PALETTE[2]),
            NamedColor::Yellow => Some(PALETTE[3]),
            NamedColor::Blue => Some(PALETTE[4]),
            NamedColor::Magenta => Some(PALETTE[5]),
            NamedColor::Cyan => Some(PALETTE[6]),
            NamedColor::White => Some(PALETTE[7]),
            NamedColor::BrightBlack => Some(PALETTE[8]),
            NamedColor::BrightRed => Some(PALETTE[9]),
            NamedColor::BrightGreen => Some(PALETTE[10]),
            NamedColor::BrightYellow => Some(PALETTE[11]),
            NamedColor::BrightBlue => Some(PALETTE[12]),
            NamedColor::BrightMagenta => Some(PALETTE[13]),
            NamedColor::BrightCyan => Some(PALETTE[14]),
            NamedColor::BrightWhite => Some(PALETTE[15]),
            _ => Some(FG),
        },
    }
}

/// Answer an OSC color query (`ColorRequest`): map alacritty's color-table index
/// to the RGB foreman actually paints. `index < 256` is a palette entry; the
/// named slots `Foreground`/`Background`/`Cursor` sit at 256/257/258. Apps query
/// these (OSC 10/11/12, OSC 4;N) to detect a light/dark background and theme
/// themselves — without an answer they fall back to a guess.
fn query_color(index: usize) -> alacritty_terminal::vte::ansi::Rgb {
    let c = if index < 256 {
        indexed_rgb(index as u8)
    } else if index == NamedColor::Background as usize {
        BG
    } else {
        // Foreground, Cursor, and the rarer named slots all use our foreground.
        FG
    };
    alacritty_terminal::vte::ansi::Rgb {
        r: c.r(),
        g: c.g(),
        b: c.b(),
    }
}

/// Resolved per-cell display style: foreground/background after the inverse swap
/// and dim, plus the line decorations. Pure, so the inverse/dim/flag logic is
/// unit-tested apart from the egui painter; `show` turns this into a `TextFormat`.
/// `Hash`/`Eq` so mono-paint can dedupe galleys by `(char, GlyphStyle)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GlyphStyle {
    pub(crate) fg: egui::Color32,
    pub(crate) bg: Option<egui::Color32>,
    pub(crate) underline: bool,
    pub(crate) strikethrough: bool,
}

pub(crate) fn glyph_style(flags: Flags, fg: AnsiColor, bg: AnsiColor) -> GlyphStyle {
    let mut fg = resolve(fg).unwrap_or(FG);
    let mut bg = resolve(bg);
    if flags.contains(Flags::INVERSE) {
        let old_fg = fg;
        fg = bg.unwrap_or(BG);
        bg = Some(old_fg);
    }
    if flags.contains(Flags::DIM) {
        fg = fg.gamma_multiply(0.7);
    }
    GlyphStyle {
        fg,
        bg,
        underline: flags.contains(Flags::UNDERLINE),
        strikethrough: flags.contains(Flags::STRIKEOUT),
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Shell {
    Cmd,
    PowerShell,
    Bash,
}
/// Pure: pick the PowerShell binary given a PATH value and an existence probe.
/// PowerShell 7 (`pwsh.exe`) ships PSReadLine 2.1+ with inline predictions;
/// Windows PowerShell 5.1 (`powershell.exe`) does not, so prefer pwsh when
/// installed. Returns the bare exe name — CreateProcess resolves it through
/// the same PATH this scanned.
fn preferred_powershell(
    path: Option<&std::ffi::OsStr>,
    exists: &dyn Fn(&Path) -> bool,
) -> &'static str {
    let Some(path) = path else {
        return "powershell.exe";
    };
    if std::env::split_paths(path).any(|dir| exists(&dir.join("pwsh.exe"))) {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}

impl Shell {
    fn program(self) -> &'static str {
        match self {
            Shell::Cmd => "cmd.exe",
            Shell::PowerShell => {
                // Probe PATH once per run; installing pwsh mid-run needs a restart.
                static PWSH: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
                *PWSH.get_or_init(|| {
                    preferred_powershell(std::env::var_os("PATH").as_deref(), &|p| p.is_file())
                })
            }
            Shell::Bash => "wsl.exe",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Shell::Cmd => "cmd",
            Shell::PowerShell => "powershell",
            Shell::Bash => "bash",
        }
    }
}

#[derive(Clone)]
struct Listener {
    out: Arc<Mutex<Vec<u8>>>,
    /// Latest OSC window title the program set (`ESC ] 0/2 ; … ST`). Used to tell
    /// what's running in a *hand-launched* shell (e.g. `claude` typed at a prompt)
    /// so the tab icon can follow it. `None` = no title / reset to default.
    title: Arc<Mutex<Option<String>>>,
}
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                if let Ok(mut b) = self.out.lock() {
                    b.extend_from_slice(text.as_bytes());
                }
            }
            Event::Title(t) => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = Some(t);
                }
            }
            Event::ResetTitle => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = None;
                }
            }
            // An app asked for one of our colors (OSC 10/11/12, OSC 4;N). Reply
            // with the real RGB via the PTY-write path, same as a device query.
            Event::ColorRequest(index, format) => {
                let reply = format(query_color(index));
                if let Ok(mut b) = self.out.lock() {
                    b.extend_from_slice(reply.as_bytes());
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Advance the parser over `bytes`, splitting at graphics cuts so each command
/// samples the cursor exactly where it completed in the stream (spec WS3).
/// alacritty sees byte-identical input — only the advance() boundaries move,
/// and chunk boundaries already occur anywhere. Zero cuts = today's code path.
fn advance_scanned<L: EventListener>(
    parser: &mut Processor,
    term: &mut Term<L>,
    graphics: &mut crate::graphics::Graphics,
    bytes: &[u8],
    replies: &mut Vec<u8>,
) {
    let cuts = graphics.feed(bytes);
    let mut at = 0;
    for cut in cuts {
        parser.advance(term, &bytes[at..cut.offset]);
        at = cut.offset;
        // A synchronized update (?2026h) buffers everything inside the parser,
        // so the Term would still show the PREVIOUS frame's cursor — codex
        // parks it at the composer caret, then emits the pet CUP inside the
        // sync block. Force-flush the buffered prefix so the sample reflects
        // the stream exactly up to this cut. The block's remainder still
        // applies within this pump pass, so no torn frame reaches the painter.
        if parser.sync_bytes_count() > 0 {
            parser.stop_sync(term);
        }
        graphics.apply(term_view(term), replies);
    }
    parser.advance(term, &bytes[at..]);
}

/// Lightweight second parser used only during a paste-scoped PowerShell
/// compatibility epoch. It identifies the byte that completes an absolute CUP
/// (`CSI H` / `CSI f`) so [`advance_psreadline_scanned`] can sample the natural
/// flow cursor immediately before alacritty applies that CUP.
///
/// PSReadLine 2.4.5 measures a non-BMP emoji as two independent UTF-16 chars.
/// When the pair reaches the final column it computes the final CUP as though
/// the glyph split across rows. Alacritty correctly defers the complete width-2
/// glyph, so that CUP lands one cell behind per straddled boundary. The raw VT
/// stream is otherwise ambiguous, hence this scanner is armed only for a
/// single-line, append-at-end PowerShell paste on the primary screen.
#[derive(Default)]
struct CupScanner {
    parser: alacritty_terminal::vte::Parser,
}

#[derive(Default)]
struct CupSink {
    cup: bool,
    invalidates_alias: bool,
    mode_barrier: bool,
}

impl alacritty_terminal::vte::Perform for CupSink {
    fn print(&mut self, _c: char) {
        self.invalidates_alias = true;
    }

    fn execute(&mut self, _byte: u8) {
        self.invalidates_alias = true;
    }

    fn hook(
        &mut self,
        _params: &alacritty_terminal::vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
        self.invalidates_alias = true;
    }

    fn csi_dispatch(
        &mut self,
        params: &alacritty_terminal::vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.cup = !ignore && intermediates.is_empty() && matches!(action, 'H' | 'f');
        self.mode_barrier = !ignore
            && intermediates == b"?"
            && matches!(action, 'h' | 'l')
            && params
                .iter()
                .flatten()
                .any(|param| matches!(*param, 47 | 1047 | 1049 | 2004));
        // SGR, cursor visibility/modes, and cursor style do not alter cells or
        // the numeric cursor. Everything else fails closed. Alternate-screen
        // and bracketed-paste transitions are explicit barriers so an enter +
        // exit pair in one PTY chunk cannot leave the paste epoch armed.
        self.invalidates_alias =
            (!self.cup && !matches!(action, 'm' | 'h' | 'l' | 'q')) || self.mode_barrier;
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {
        self.invalidates_alias = true;
    }
}

#[derive(Clone, Copy, Default)]
struct CupScanEvent {
    cup: bool,
    invalidates_alias: bool,
    mode_barrier: bool,
}

impl CupScanner {
    fn scan_byte(&mut self, byte: u8) -> CupScanEvent {
        let mut sink = CupSink::default();
        self.parser.advance(&mut sink, &[byte]);
        CupScanEvent {
            cup: sink.cup,
            invalidates_alias: sink.invalidates_alias,
            mode_barrier: sink.mode_barrier,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorAlias {
    /// The authoritative point stored in alacritty after the CUP.
    raw: Point,
    /// The natural whole-glyph flow endpoint immediately before the CUP.
    physical: Point,
}

#[derive(Default)]
struct CupScanResult {
    saw_cup: bool,
    hit_mode_barrier: bool,
    refreshed_alias: bool,
    alias: Option<CursorAlias>,
}

/// Serial-number ordering for the wrapping PTY generation counter. A value is
/// newer when it is within the forward half of the `u64` space.
fn generation_after(candidate: u64, baseline: u64) -> bool {
    let delta = candidate.wrapping_sub(baseline);
    delta != 0 && delta < (1u64 << 63)
}

/// Validate that `raw` is exactly `physical` with alacritty's whole-glyph wrap
/// padding removed. This walks the entire soft-wrapped chain: the live repro
/// spans 16 rows and six pads, so the input shadow's historical eight-row cap
/// is not sufficient here.
fn psreadline_cursor_alias<L: EventListener>(
    term: &Term<L>,
    raw: Point,
    physical: Point,
) -> Option<CursorAlias> {
    if raw == physical {
        return None;
    }
    let grid = term.grid();
    let cols = grid.columns();
    if cols == 0 || raw.column.0 >= cols || physical.column.0 >= cols {
        return None;
    }

    let history_top = -(grid.history_size() as i32);
    let mut start = physical.line;
    let mut walked = 0usize;
    while start.0 > history_top && walked < 4096 {
        let prev = Line(start.0 - 1);
        if !grid[prev][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            break;
        }
        start = prev;
        walked += 1;
    }
    if walked == 4096 || raw.line < start || raw.line > physical.line {
        return None;
    }

    // Every intervening row must belong to the same soft-wrapped logical line.
    for line in start.0..physical.line.0 {
        if !grid[Line(line)][Column(cols - 1)]
            .flags
            .contains(Flags::WRAPLINE)
        {
            return None;
        }
    }

    let flat =
        |point: Point| -> usize { (point.line.0 - start.0) as usize * cols + point.column.0 };
    let raw_flat = flat(raw);
    let physical_flat = flat(physical);
    if raw_flat >= physical_flat {
        return None;
    }

    let mut surrogate_pads = 0usize;
    for offset in 0..physical_flat {
        let line = Line(start.0 + (offset / cols) as i32);
        let col = Column(offset % cols);
        if grid[line][col]
            .flags
            .contains(Flags::LEADING_WIDE_CHAR_SPACER)
        {
            // PSReadLine already defers a BMP width-2 glyph (for example CJK)
            // as a unit, so its CUP includes that pad. The mismatch exists only
            // for a non-BMP glyph measured as two independent UTF-16 chars.
            let next = offset + 1;
            if next < physical_flat {
                let next_line = Line(start.0 + (next / cols) as i32);
                let next_col = Column(next % cols);
                let base = &grid[next_line][next_col];
                if base.flags.contains(Flags::WIDE_CHAR) && base.c > '\u{FFFF}' {
                    surrogate_pads += 1;
                }
            }
        }
    }

    (surrogate_pads > 0 && raw_flat + surrogate_pads == physical_flat)
        .then_some(CursorAlias { raw, physical })
}

/// Conservative arming gate for the compatibility epoch: the paste must begin
/// at the visible end of the current soft-wrapped line. Inline prediction text,
/// a mid-line cursor, or stale nonblank cells after the caret all fail closed.
fn cursor_at_content_end<L: EventListener>(term: &Term<L>, point: Point) -> bool {
    let grid = term.grid();
    let cols = grid.columns();
    if cols == 0 || point.column.0 >= cols {
        return false;
    }
    if grid.cursor.input_needs_wrap && point == grid.cursor.point {
        return true;
    }

    let last = Line(grid.screen_lines() as i32 - 1);
    let mut line = point.line;
    let mut first_col = point.column.0;
    let mut walked = 0usize;
    loop {
        for col in first_col..cols {
            if !grid[line][Column(col)].is_empty() {
                return false;
            }
        }
        if line >= last || !grid[line][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            return true;
        }
        walked += 1;
        if walked == 4096 {
            return false;
        }
        line += 1;
        first_col = 0;
    }
}

/// Byte-identical terminal advance with CUP endpoint observation layered over
/// the normal graphics scanner. No grid state is rewritten: a validated alias
/// is consumed only by caret/display reporting and wide-key sampling, leaving
/// subsequent VT parsing and CPR replies anchored to alacritty's authoritative
/// cursor.
fn advance_psreadline_scanned<L: EventListener>(
    parser: &mut Processor,
    term: &mut Term<L>,
    graphics: &mut crate::graphics::Graphics,
    scanner: &mut CupScanner,
    bytes: &[u8],
    replies: &mut Vec<u8>,
    initial_alias: Option<CursorAlias>,
) -> CupScanResult {
    let mut result = CupScanResult {
        alias: initial_alias,
        ..CupScanResult::default()
    };
    let mut at = 0usize;
    for (i, &byte) in bytes.iter().enumerate() {
        let event = scanner.scan_byte(byte);
        result.hit_mode_barrier |= event.mode_barrier;
        if event.invalidates_alias {
            result.alias = None;
        }
        if !event.cup {
            continue;
        }
        result.saw_cup = true;
        // Feed through the parameters but stop before H/f, while alacritty's
        // parser is still holding the CSI and the grid cursor is the flow end.
        advance_scanned(parser, term, graphics, &bytes[at..i], replies);
        let physical = term.grid().cursor.point;
        advance_scanned(parser, term, graphics, &bytes[i..=i], replies);
        let raw = term.grid().cursor.point;
        let candidate = psreadline_cursor_alias(term, raw, physical);
        if candidate.is_some() {
            result.alias = candidate;
            result.refreshed_alias = true;
        } else if !result.alias.is_some_and(|alias| alias.raw == raw) {
            // A redundant CUP to the same raw point preserves a validated
            // alias; any different unmatched CUP supersedes it.
            result.alias = None;
        }
        at = i + 1;
    }
    advance_scanned(parser, term, graphics, &bytes[at..], replies);

    // Printable output or a later relative cursor move invalidates an alias.
    if result
        .alias
        .is_some_and(|alias| term.grid().cursor.point != alias.raw)
    {
        result.alias = None;
    }
    result
}

fn term_view<L: EventListener>(term: &Term<L>) -> crate::graphics::TermView {
    let g = term.grid();
    crate::graphics::TermView {
        cursor_col: g.cursor.point.column.0,
        cursor_line: g.cursor.point.line.0.max(0) as usize,
        alt_screen: term.mode().contains(TermMode::ALT_SCREEN),
        history_size: g.history_size(),
    }
}

/// Resize the grid to `size`, keeping content anchored on a height grow.
///
/// ConPTY emits no repaint when a pane grows taller: it anchors existing
/// content (blank rows appear below) and keeps addressing the cursor by the
/// same absolute rows. `alacritty_terminal` instead pulls lines back from
/// scrollback, shifting content down — so the child's next absolute repaint
/// (e.g. PSReadLine echoing a keystroke via CUP) lands mid-scrollback, rows
/// above the prompt. Cancel the pull so both layouts agree. Shrinks and pure
/// width changes are untouched: shrink was probed clean, and width is the
/// separate wrapped-reflow divergence. See docs/conpty-resize-reflow.md.
fn resize_anchored<L: EventListener>(term: &mut Term<L>, size: Size) -> usize {
    let old_rows = term.screen_lines();
    if size.rows <= old_rows {
        term.resize(size);
        return 0;
    }
    // Apply the column step on its own first — a deliberate reordering
    // (native alacritty reflows lines before columns) so the cursor delta
    // below measures only the height step's history pull, unpolluted by
    // rewrap moving the cursor.
    if size.cols != term.columns() {
        term.resize(Size {
            cols: size.cols,
            rows: old_rows,
        });
    }
    let before = term.grid().cursor.point.line.0;
    term.resize(size);
    let pulled = (term.grid().cursor.point.line.0 - before).max(0) as usize;
    if pulled > 0 {
        let region = Line(0)..Line(size.rows as i32);
        // Term::resize rotated any live selection to track the pull; rotate
        // it back alongside the content (mirrors Term::scroll_up_relative).
        let rotated = term
            .selection
            .take()
            .and_then(|s| s.rotate(term, &region, pulled as i32));
        term.selection = rotated;
        let grid = term.grid_mut();
        grid.scroll_up(&region, pulled);
        // grow_lines moved both cursors down by exactly `pulled`; undo it.
        grid.cursor.point.line -= pulled;
        grid.saved_cursor.point.line -= pulled;
    }
    pulled
}

/// Cache key for a pane's grid-locked mono paint. All five inputs fully determine
/// the laid-out glyphs: content version, scroll position, grid dims, and font
/// size. Selection/caret are NOT here — they paint as separate overlays.
#[derive(Clone, Copy, PartialEq, Eq)]
struct MonoPaintKey {
    content_gen: u64,
    off: usize,
    cols: usize,
    rows: usize,
    font_bits: u32,
}

/// One grid-locked glyph ready to blit: grid identity + shared galley.
/// Blit position is recomputed every frame from current `CellMetrics` so a
/// cache hit after a pane move still paints at the live origin.
struct MonoGlyph {
    row: usize,
    col: usize,
    galley: std::sync::Arc<egui::Galley>,
}

/// Non-default SGR cell background (including blank/space cells). Stored as
/// grid identity + color + cell span; painted via `rect_filled(span_rect)` with
/// live metrics so width-2 CJK/mono-emoji covers both cells (spacers emit no bg).
struct MonoBg {
    row: usize,
    col: usize,
    /// Grid cells this bg covers (1 for ASCII, 2 for wide glyphs).
    width_cells: u8,
    color: egui::Color32,
}

/// Memoized per-placement galleys + bg rects + emoji stamp sites for a pane.
/// On key hit, show() re-blits only (0 layout_*). Rebuild skips blank galleys
/// and dedupes by (char, style). Absolute pixel positions are never cached.
/// `emoji_sites` ride the same mono key so cache hits still have stamp targets
/// without re-running `plan_paint` or busting mono memo when the atlas fills.
struct MonoPaintCache {
    key: MonoPaintKey,
    items: std::sync::Arc<Vec<MonoGlyph>>,
    bgs: std::sync::Arc<Vec<MonoBg>>,
    emoji_sites: std::sync::Arc<Vec<crate::frame::EmojiSite>>,
}

impl MonoPaintCache {
    fn empty() -> Self {
        Self {
            // Impossible key so the first get_or_rebuild always rebuilds.
            key: MonoPaintKey {
                content_gen: u64::MAX,
                off: usize::MAX,
                cols: 0,
                rows: 0,
                font_bits: 0,
            },
            items: std::sync::Arc::new(Vec::new()),
            bgs: std::sync::Arc::new(Vec::new()),
            emoji_sites: std::sync::Arc::new(Vec::new()),
        }
    }

    /// On key match: return cached items/bgs/sites (rebuild not called — 0 layouts).
    /// On miss: call `rebuild`, store, return. Production and tests share this path.
    fn get_or_rebuild(
        &mut self,
        key: MonoPaintKey,
        rebuild: impl FnOnce() -> (Vec<MonoGlyph>, Vec<MonoBg>, Vec<crate::frame::EmojiSite>),
    ) -> (
        std::sync::Arc<Vec<MonoGlyph>>,
        std::sync::Arc<Vec<MonoBg>>,
        std::sync::Arc<Vec<crate::frame::EmojiSite>>,
    ) {
        if self.key == key {
            return (
                self.items.clone(),
                self.bgs.clone(),
                self.emoji_sites.clone(),
            );
        }
        let (items, bgs, emoji_sites) = rebuild();
        self.key = key;
        self.items = std::sync::Arc::new(items);
        self.bgs = std::sync::Arc::new(bgs);
        self.emoji_sites = std::sync::Arc::new(emoji_sites);
        (
            self.items.clone(),
            self.bgs.clone(),
            self.emoji_sites.clone(),
        )
    }
}

/// Drop emoji textures whose px size no longer matches the live font-derived
/// size (zoom churn). Pure so unit tests can cover the eviction policy.
fn retain_emoji_textures_at_px<T>(map: &mut std::collections::HashMap<(char, u32), T>, px: u32) {
    map.retain(|(_, entry_px), _| *entry_px == px);
}

/// Center `tex_w × tex_h` inside `span` preserving aspect ratio (letterbox).
fn emoji_stamp_fit_rect(span: egui::Rect, tex_w: f32, tex_h: f32) -> egui::Rect {
    let tw = tex_w.max(1.0);
    let th = tex_h.max(1.0);
    let scale = (span.width() / tw).min(span.height() / th);
    let w = tw * scale;
    let h = th * scale;
    let min = egui::pos2(
        span.min.x + (span.width() - w) * 0.5,
        span.min.y + (span.height() - h) * 0.5,
    );
    egui::Rect::from_min_size(min, egui::vec2(w, h))
}

/// Rebuild content-only mono galleys + SGR bg cells from a paint plan.
///
/// Calls the injected `layout` once per **distinct** `(char, GlyphStyle)` —
/// callers should invoke [`note_layout_call`] inside that closure when doing a
/// real `layout_no_wrap` / `layout_job`. Skips space/`'\0'` for galleys (blank
/// cells); non-default `style.bg` is collected for every cell including spaces
/// so inverse/colored empties still paint. Positions are grid identity only —
/// blit uses live metrics every frame (`cell_rect` for glyphs, `span_rect` for bgs).
fn mono_paint_items(
    plan: &crate::frame::PaintPlan,
    layout: &mut dyn FnMut(char, GlyphStyle) -> std::sync::Arc<egui::Galley>,
) -> (Vec<MonoGlyph>, Vec<MonoBg>) {
    use std::collections::HashMap;
    let mut dedupe: HashMap<(char, GlyphStyle), std::sync::Arc<egui::Galley>> = HashMap::new();
    let mut items = Vec::new();
    let mut bgs = Vec::new();
    for g in &plan.glyphs {
        if let Some(color) = g.style.bg {
            bgs.push(MonoBg {
                row: g.row,
                col: g.col,
                width_cells: g.width_cells.max(1),
                color,
            });
        }
        if g.ch == ' ' || g.ch == '\0' {
            continue;
        }
        // Default-emoji-presentation scalars stay in items: the paint loop
        // suppresses their mono blit only when a color stamp actually resolved
        // (`stamped.contains`). Dropping them here painted NOTHING whenever the
        // raster failed (NullEmojiRaster) — tofu is the fail-open, not blank.
        let galley = match dedupe.get(&(g.ch, g.style)) {
            Some(arc) => arc.clone(),
            None => {
                let arc = layout(g.ch, g.style);
                dedupe.insert((g.ch, g.style), arc.clone());
                arc
            }
        };
        items.push(MonoGlyph {
            row: g.row,
            col: g.col,
            galley,
        });
    }
    (items, bgs)
}

/// Test/production seam alias: same as [`mono_paint_items`].
#[cfg(test)]
fn mono_paint_items_for_test(
    plan: &crate::frame::PaintPlan,
    layout: &mut dyn FnMut(char, GlyphStyle) -> std::sync::Arc<egui::Galley>,
) -> (Vec<MonoGlyph>, Vec<MonoBg>) {
    mono_paint_items(plan, layout)
}

/// Cross-frame state for wide-character key compensation.
///
/// `AwaitingEcho` is deliberately distinct from `Uninitialized`: after text,
/// paste, or an unmodeled chord, the live grid is known to be pre-input or
/// mid-redraw and must not immediately seed a new shadow.
enum WideShadowState {
    Uninitialized,
    Tracking {
        line: Vec<crate::input::CellWide>,
        col: usize,
        basis_gen: u64,
    },
    AwaitingEcho {
        invalidated_gen: u64,
    },
}

/// Keys whose press this Session actually processed. egui's key state is
/// global, while only the focused Session calls `read_input`; ownership keeps a
/// release from another pane from extending this Session's shadow lifetime.
#[derive(Default)]
struct WideKeyLatch(u8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WideKeyActivity {
    active: bool,
    blocks_reseed: bool,
}

impl WideKeyLatch {
    fn bit(key: egui::Key) -> Option<u8> {
        match key {
            egui::Key::ArrowLeft => Some(1 << 0),
            egui::Key::ArrowRight => Some(1 << 1),
            egui::Key::Backspace => Some(1 << 2),
            egui::Key::Delete => Some(1 << 3),
            _ => None,
        }
    }

    /// Update per-session ownership and report activity for this frame. Global
    /// `keys_down` bridges repeat gaps. A release missed while this Session was
    /// inactive clears stale ownership without inventing new activity.
    fn update(&mut self, input: &egui::InputState) -> WideKeyActivity {
        let owned_before = self.0;
        let mut active = false;
        let mut blocks_reseed = false;
        let mut fresh_bits = 0u8;
        for event in &input.events {
            let egui::Event::Key {
                key,
                pressed,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(bit) = Self::bit(*key) else {
                continue;
            };
            if *pressed {
                if !*repeat {
                    // A new physical press starts a new ownership epoch. This
                    // also discards a stale bit left by a release missed while
                    // the Session was inactive.
                    self.0 &= !bit;
                    fresh_bits |= bit;
                }
                if crate::input::wide_key_uses_shadow(*key, *modifiers) || self.0 & bit != 0 {
                    self.0 |= bit;
                    active = true;
                    blocks_reseed |= owned_before & bit != 0 && fresh_bits & bit == 0;
                }
            } else if self.0 & bit != 0 {
                self.0 &= !bit;
                active = true;
                blocks_reseed |= owned_before & bit != 0 && fresh_bits & bit == 0;
            }
        }

        for key in [
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
            egui::Key::Backspace,
            egui::Key::Delete,
        ] {
            let bit = Self::bit(key).expect("listed wide key has a latch bit");
            if self.0 & bit == 0 {
                continue;
            }
            if input.key_down(key) {
                active = true;
                blocks_reseed |= owned_before & bit != 0 && fresh_bits & bit == 0;
            } else {
                // The release happened while this Session was inactive. Clear
                // stale ownership without pretending it happened just now;
                // the previous input timestamp already owns any real tail.
                self.0 &= !bit;
            }
        }
        WideKeyActivity {
            active,
            blocks_reseed,
        }
    }
}

pub struct Session {
    term: Term<Listener>,
    parser: Processor,
    rx: Receiver<Vec<u8>>,
    resp: Arc<Mutex<Vec<u8>>>,
    /// Latest OSC title the running program set (shared with the `Listener`).
    osc_title: Arc<Mutex<Option<String>>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Kill-on-close job the child lives in: dropping the Session kills the
    /// whole process tree (see src/job.rs). None = assignment failed; the
    /// session still works, its tree just isn't reaped (old behavior).
    job: Option<crate::job::Job>,
    exit: Option<u32>,
    exit_noted: bool,
    pub shell: Shell,
    /// The argv this terminal was dispatched with (agent commands spawned via
    /// `spawn_argv`); `None` for a plain interactive shell. Drives the tab's
    /// agent logo (claude/codex) — see `icon_kind`.
    dispatch_argv: Option<Vec<String>>,
    /// PID of the process we spawned into the PTY (the shell, or a dispatched
    /// command). Root for the process-tree agent scan that catches a hand-typed
    /// `claude`/`codex` — see `icon_kind`.
    root_pid: Option<u32>,
    // The Session's stable Member id, stamped by the window manager at spawn
    // (== the `t{id}` it injects as FOREMAN_TERMINAL_ID). Unlike a Win id it
    // never changes — tabbing, untabbing, and moving leave it alone — so the
    // chat room and the agent always agree on "who". 0 until stamped.
    term_id: u64,
    cols: usize,
    rows: usize,
    // Dispatch banner queued by inject_note(); flushed (fitted to the real
    // width) by the first resize(). See inject_note for why it is deferred.
    pending_note: Option<String>,
    // Ready latch + chat inject queue + deferred submit (pure gate; Session
    // applies Action::Write). See crate::ready and CONTEXT.md "Ready gate".
    ready_gate: crate::ready::ReadyGate,
    // Bumped in pump() each time a batch of new PTY bytes arrives. A cheap
    // freshness signal the settle machinery polls to detect terminal activity.
    output_gen: u64,
    // When the last PTY chunk arrived; None until the child's first output.
    // Feeds keep_wide_shadow: output_gen advancing alone does not mean the
    // grid is trustworthy — one keypress echo on a long soft-wrapped line
    // arrives across many chunks over multiple frames.
    last_output_at: Option<std::time::Instant>,
    // Last time a shadow-using wide key (Backspace, Delete, Left, Right) was active.
    // Refreshed from egui's held-key state, not just repeat events, and retained
    // briefly after release so the final echo can drain.
    last_wide_key_at: Option<std::time::Instant>,
    wide_key_latch: WideKeyLatch,
    // Shadow cursor row for wide-key encoding, or an explicit wait-for-echo
    // state after tracking becomes unknowable. A plain Option cannot express
    // that distinction and used to re-sample the same stale grid next frame.
    wide_shadow: WideShadowState,
    // Paste-scoped PSReadLine compatibility. PSReadLine 2.4.5 can emit a final
    // CUP that omits alacritty's whole-glyph wrap pads for non-BMP emoji. The
    // scanner observes CUP endpoints only while armed; the alias never mutates
    // the grid and is used solely for caret/display reporting + wide-key sampling.
    psreadline_cup_scanner: Option<CupScanner>,
    psreadline_cursor_alias: Option<CursorAlias>,
    // PTY generation whose bytes established the current alias. A carried alias
    // must not immediately re-seed wide-key modeling after later input: until a
    // newer CUP observation arrives it describes the pre-input row.
    psreadline_cursor_alias_gen: Option<u64>,
    // Grid-content version for the render galley cache. Distinct from
    // output_gen (which means "child produced PTY bytes" and drives settle
    // quiescence): content_gen bumps on EVERY grid-content mutation, including
    // the inject_note banner that never rides pump(). Single source of truth
    // for "the galley is stale" — bump it wherever self.term's grid changes.
    content_gen: u64,
    // Memoized grid-locked mono paint + the key it was built for. On a key hit
    // show() re-blits Arc clones (0 layout_*). Invalidated by any key change.
    mono_paint: Option<MonoPaintCache>,
    /// Color-emoji rasterizer (DirectWrite on Windows, null elsewhere). Fail-open:
    /// `None` from `color_glyph` leaves the mono glyph alone.
    emoji_raster: Box<dyn crate::emoji_raster::EmojiRaster>,
    /// Separate emoji stamp texture cache. Key is `(char, font-derived px)` so
    /// zoom changes miss rather than stretching stale bitmaps. Evicted by px on
    /// paint — never invalidates the mono paint memo.
    emoji_textures: std::collections::HashMap<(char, u32), egui::TextureHandle>,
    // The Caret gate: decides which cell the painted caret rests at, de-jittering
    // a TUI's mid-redraw cursor moves. Owns cursor-stability and input-recency
    // state; fed every frame in show(). See `crate::caret`.
    caret: crate::caret::CaretGate,
    /// Kitty graphics state: overlay images only — the grid stays pure text.
    /// See src/graphics.rs and the spec.
    graphics: crate::graphics::Graphics,
    /// egui textures for graphics images, keyed by image id → (data generation,
    /// handle). The egui adapter stays here so `graphics` remains egui-free.
    textures: std::collections::HashMap<u32, (u64, egui::TextureHandle)>,
    /// Sub-line remainder of wheel scrolling. egui delivers a notch as smoothed
    /// per-frame fractions; carrying the remainder keeps gentle scrolls from
    /// rounding to nothing and fast flicks from over-emitting lines.
    scroll_accum: f32,
    /// Sub-notch remainder of Ctrl+Scroll zooming. Same smoothing problem as
    /// `scroll_accum`, but accumulated against the zoom notch size so a gentle
    /// Ctrl+wheel still eventually steps the font and a fast flick doesn't lurch.
    zoom_accum: f32,
    /// Diagnostic trace (FOREMAN_RX_DUMP=<file>): raw PTY chunks plus resize and
    /// host-reply markers. None (zero-cost) when the variable is unset.
    rx_dump: Option<std::fs::File>,
}

/// Smoothed-scroll points that equal one Ctrl+Scroll zoom notch. egui reports a
/// wheel notch as ~50 points of `smooth_scroll_delta`, so dividing by this gives
/// roughly one font step per physical notch.
const ZOOM_NOTCH_PX: f32 = 50.0;

fn read_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn clipboard_has_image() -> bool {
    arboard::Clipboard::new().is_ok_and(|mut c| c.get_image().is_ok())
}

/// The live global terminal font size, parked in egui's per-context data so every
/// `Session::show` reads the same value and any pane's Ctrl+Scroll handler can
/// update it without threading a param through the recursive window managers. The
/// app seeds it from `settings.json` each frame and reads it back to persist.
#[derive(Clone, Copy)]
struct FontSizeState(f32);

fn font_size_id() -> egui::Id {
    egui::Id::new("foreman::terminal_font_size")
}

/// Current global terminal font size (points), defaulting before the app seeds it.
pub fn font_size(ctx: &egui::Context) -> f32 {
    ctx.data_mut(|d| d.get_temp::<FontSizeState>(font_size_id()))
        .map(|s| s.0)
        .unwrap_or(crate::config::DEFAULT_FONT_SIZE)
}

/// Set the global terminal font size, clamped to the legible range.
pub fn set_font_size(ctx: &egui::Context, px: f32) {
    let px = px.clamp(crate::config::MIN_FONT_SIZE, crate::config::MAX_FONT_SIZE);
    ctx.data_mut(|d| d.insert_temp(font_size_id(), FontSizeState(px)));
}

/// Re-export: chat inject always brackets (see [`crate::ready::paste_wrap`]).
pub use crate::ready::paste_wrap;

fn sel_viewport_range(
    range: SelectionRange,
    display_offset: usize,
    screen_lines: usize,
    columns: usize,
) -> Option<crate::frame::SelRange> {
    let off = display_offset as i32;
    let start_row = range.start.line.0 + off;
    let end_row = range.end.line.0 + off;
    if end_row < 0 || start_row >= screen_lines as i32 {
        return None;
    }
    let start = if start_row < 0 {
        (0, 0)
    } else {
        (start_row as usize, range.start.column.0)
    };
    let end = if end_row >= screen_lines as i32 {
        (screen_lines.saturating_sub(1), columns.saturating_sub(1))
    } else {
        (end_row as usize, range.end.column.0)
    };
    Some(crate::frame::SelRange { start, end })
}

impl Session {
    pub fn spawn(
        shell: Shell,
        cwd: Option<&Path>,
        env: &[(String, String)],
        ctx: egui::Context,
    ) -> std::io::Result<Session> {
        let mut cmd = CommandBuilder::new(shell.program());
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        Self::spawn_with(cmd, shell, ctx)
    }

    /// Spawn an explicit argv (an agent command, not a shell). npm shims like
    /// `claude` are `.cmd` files CreateProcess can't run directly — if the
    /// direct spawn fails, retry once through `cmd /c`.
    pub fn spawn_argv(
        argv: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
        ctx: egui::Context,
    ) -> std::io::Result<Session> {
        if argv.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "argv is empty",
            ));
        }
        let build = |words: &[String]| {
            let mut c = CommandBuilder::new(&words[0]);
            for a in &words[1..] {
                c.arg(a);
            }
            if let Some(dir) = cwd {
                c.cwd(dir);
            }
            for (k, v) in env {
                c.env(k, v);
            }
            c
        };
        // Anything that routes through cmd.exe re-parses the whole command
        // line: a newline ENDS the command (the rest would execute as
        // follow-up cmd commands) and an embedded `"` flips its quote state.
        // Batch shims can never receive such args — refuse loudly instead of
        // silently truncating/injecting. Two routes hit cmd: CreateProcess
        // silently wraps an explicit .cmd/.bat target, and the bare-name npm
        // shim falls back to `cmd /c` below.
        let unsafe_for_cmd = argv.iter().any(|a| a.contains(['\n', '\r', '"']));
        let refuse = |e: String| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{} runs via a cmd-shim ({e}) which cannot carry newlines or \" in \
                     arguments — flatten the prompt to one quote-free line or install \
                     the tool as a native exe",
                    argv[0]
                ),
            )
        };
        let is_batch = std::path::Path::new(&argv[0])
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("cmd") || x.eq_ignore_ascii_case("bat"));
        if unsafe_for_cmd && is_batch {
            return Err(refuse("batch file".into()));
        }
        let mut session = Self::spawn_with(build(argv), Shell::Cmd, ctx.clone()).or_else(|e| {
            if unsafe_for_cmd {
                return Err(refuse(format!("not directly spawnable: {e}")));
            }
            let mut wrapped = vec!["cmd.exe".to_string(), "/c".to_string()];
            wrapped.extend_from_slice(argv);
            Self::spawn_with(build(&wrapped), Shell::Cmd, ctx)
        })?;
        // Remember what we dispatched so the tab can show the agent's logo.
        session.dispatch_argv = Some(argv.to_vec());
        Ok(session)
    }

    /// The latest OSC window title the running program set, if any.
    pub fn osc_title(&self) -> Option<String> {
        self.osc_title.lock().ok().and_then(|t| t.clone())
    }

    /// The icon for this terminal's tab, resolved in priority order:
    /// 1. the dispatched agent's argv (instant, `foreman open claude …`),
    /// 2. a hand-launched agent recognized from the program's OSC title (instant;
    ///    works when the program sets a useful title, e.g. Claude),
    /// 3. a hand-launched agent found in the OS process tree under the shell
    ///    (throttled; catches agents that set a useless title, e.g. Codex),
    /// 4. otherwise the shell's glyph.
    pub fn icon_kind(&self) -> crate::icons::IconKind {
        if let Some(k) = self
            .dispatch_argv
            .as_deref()
            .and_then(crate::icons::IconKind::from_argv)
        {
            return k;
        }
        if let Some(k) = self
            .osc_title()
            .and_then(|t| crate::icons::IconKind::from_title(&t))
        {
            return k;
        }
        if let Some(k) = self.root_pid.and_then(crate::proc::agent_for) {
            return k;
        }
        crate::icons::IconKind::for_shell(self.shell)
    }

    fn spawn_with(
        cmd: CommandBuilder,
        shell: Shell,
        ctx: egui::Context,
    ) -> std::io::Result<Session> {
        let (cols, rows) = (80usize, 24usize);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let root_pid = child.process_id();
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let (tx, rx) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            // 64 KiB: read() returns whatever is already available (never waits
            // to fill), so this only cuts chunk count under flood — fewer
            // to_vec()/channel/repaint/parse-setup ops per MiB. No latency or
            // ordering change for small output.
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        note_pty_output();
                        ctx.request_repaint();
                    }
                }
            }
        });

        let resp = Arc::new(Mutex::new(Vec::new()));
        let osc_title = Arc::new(Mutex::new(None));
        let term = Term::new(
            Config::default(),
            &Size { cols, rows },
            Listener {
                out: resp.clone(),
                title: osc_title.clone(),
            },
        );
        Ok(Session {
            term,
            parser: Processor::new(),
            rx,
            resp,
            osc_title,
            writer,
            master: pair.master,
            child,
            job: root_pid.and_then(crate::job::Job::assign),
            exit: None,
            exit_noted: false,
            shell,
            dispatch_argv: None,
            root_pid,
            term_id: 0,
            cols,
            rows,
            pending_note: None,
            ready_gate: crate::ready::ReadyGate::new(),
            output_gen: 0,
            last_output_at: None,
            last_wide_key_at: None,
            wide_key_latch: WideKeyLatch::default(),
            wide_shadow: WideShadowState::Uninitialized,
            psreadline_cup_scanner: None,
            psreadline_cursor_alias: None,
            psreadline_cursor_alias_gen: None,
            content_gen: 0,
            mono_paint: None,
            emoji_raster: crate::emoji_raster::system_emoji_raster(),
            emoji_textures: std::collections::HashMap::new(),
            caret: crate::caret::CaretGate::new(std::time::Instant::now()),
            graphics: crate::graphics::Graphics::default(),
            textures: std::collections::HashMap::new(),
            scroll_accum: 0.0,
            zoom_accum: 0.0,
            rx_dump: std::env::var_os("FOREMAN_RX_DUMP").and_then(|p| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .ok()
            }),
        })
    }

    /// The Session's stable Member id (see the `term_id` field). 0 until the
    /// window manager stamps it at spawn.
    pub fn term_id(&self) -> u64 {
        self.term_id
    }

    /// The shell's own process id, if the spawn reported one. Root of the
    /// descendant scan the close-confirm walks.
    pub fn root_pid(&self) -> Option<u32> {
        self.root_pid
    }

    /// Stamp the stable Member id. The window manager calls this once, right
    /// after spawn, with the same id it baked into FOREMAN_TERMINAL_ID.
    pub fn set_term_id(&mut self, id: u64) {
        self.term_id = id;
    }

    /// Is injected input safe to send? True once the startup DSR exchange has
    /// resolved AND the child has painted its first visible output. Either
    /// alone is insufficient — a passthrough ConPTY host answers the DSR
    /// microseconds after spawn, seconds before the child's input path opens,
    /// and bytes injected in that window are eaten. Owned by the Ready gate;
    /// latched during [`Session::pump`].
    pub fn ready(&self) -> bool {
        self.ready_gate.ready()
    }

    /// Exit code of the child process, once it has ended. Cached — `try_wait`
    /// is a cheap non-blocking poll until then.
    pub fn exited(&mut self) -> Option<u32> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait().ok().flatten().map(|s| s.exit_code());
        }
        self.exit
    }

    /// Cheap `&self` read of the exit latch (set by `exited`/`exit_to_note`,
    /// which the window manager polls every frame). True once the shell has been
    /// observed to exit — used to skip the close-confirm process scan for a dead
    /// terminal whose `root_pid` the OS may since have recycled.
    pub fn has_exited(&self) -> bool {
        self.exit.is_some()
    }

    /// Like `exited`, but reports the exit exactly once — for one-shot
    /// reactions like stamping the window title.
    pub fn exit_to_note(&mut self) -> Option<u32> {
        let code = self.exited()?;
        if self.exit_noted {
            return None;
        }
        self.exit_noted = true;
        Some(code)
    }

    /// Queue a synthetic note for the emulator (NOT the PTY): renders as a
    /// dim line in the pane. Used to announce a dispatched command before the
    /// child produces output — a `claude -p` worker is silent until done, and
    /// an empty pane reads as hung.
    ///
    /// Deferred, not written immediately: at spawn the grid is a placeholder
    /// 80x24, and writing a wide note there reflows on the first-frame shrink
    /// to the real pane size, stranding the note's head in scrollback (only an
    /// "…──" tail stays visible). The first resize() flushes it, fitted to the
    /// real width.
    pub fn inject_note(&mut self, text: &str) {
        self.pending_note = Some(text.to_string());
    }

    /// Deliver chat text into this session's stdin: bracketed paste, then a
    /// separate `\r` to submit (spec: agent-group-chat §3).
    /// Empty text is a no-op — a bare `\r` would submit the target's
    /// half-typed input. Live-verified on ConPTY (2026-06-10): claude
    /// sessions honor the bracketed-paste markers (multi-line lands as one
    /// input block), so the wrap stays unconditional in v1; gating on
    /// TermMode::BRACKETED_PASTE remains a possible hardening if non-claude
    /// members ever matter.
    /// The submit is DEFERRED by [`crate::ready::SUBMIT_DELAY`], not written
    /// with the paste: a back-to-back `\r` gets folded into the paste by
    /// Claude Code's burst detection and lands as a literal newline (live
    /// failure 2026-06-10 — message sat unsubmitted in the input box).
    /// pump() fires it once the deadline passes; the frame loop pumps every
    /// session every ~16ms, so no extra repaint plumbing is needed. Accepted
    /// quirks: two posts inside the window merge into one submitted turn for
    /// the receiver, and bytes buffered through a member's entire boot can
    /// still coalesce (residual; revisit with age-gating if it bites).
    pub fn inject_input(&mut self, text: &str) {
        let now = std::time::Instant::now();
        if let Some(crate::ready::Action::Write(bytes)) = self.ready_gate.try_inject(text, now) {
            self.send_external_input(&bytes);
        }
    }

    /// Raw PTY write — bypasses bracketed-paste and the submit delay. Used by
    /// `foreman send` to deliver pre-encoded bytes (text + key sequences).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.send_external_input(bytes);
    }

    /// Text half of `foreman send`. Keeping this distinct from raw key bytes
    /// lets the control path arm the same append-at-end PowerShell paste
    /// compatibility as the GUI clipboard paths.
    pub fn feed_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.arm_psreadline_paste_cursor(text);
        self.send_external_input(text.as_bytes());
    }

    /// The terminal's current mode flags — used by `foreman send` to encode
    /// named keys through the same path the live keyboard uses.
    pub fn term_mode(&self) -> alacritty_terminal::term::TermMode {
        *self.term.mode()
    }

    /// Cursor's logical (wrap-concatenated) row as [`crate::input::CellWide`]
    /// classes + cursor index — input seam for wide-char key encoding
    /// (`encode_key_wide` / `send --keys`). See `inspect::wide_row_at_cursor`.
    pub fn wide_line_at_cursor(&self) -> (Vec<crate::input::CellWide>, usize) {
        crate::inspect::wide_row_at_point(&self.term, self.input_cursor_point())
    }

    /// Grid point for input modeling. With no compatibility alias this remains
    /// alacritty's authoritative raw point (including a possible spacer).
    fn input_cursor_point(&self) -> Point {
        let raw = self.term.grid().cursor.point;
        self.psreadline_cursor_alias
            .filter(|alias| alias.raw == raw)
            .map(|alias| alias.physical)
            .unwrap_or(raw)
    }

    fn has_valid_cursor_alias(&self) -> bool {
        let raw = self.term.grid().cursor.point;
        self.psreadline_cursor_alias
            .is_some_and(|alias| alias.raw == raw)
    }

    fn has_fresh_cursor_alias(&self, invalidated_gen: u64) -> bool {
        self.has_valid_cursor_alias()
            && self
                .psreadline_cursor_alias_gen
                .is_some_and(|observed_gen| generation_after(observed_gen, invalidated_gen))
    }

    /// Point Foreman paints/reports. Preserve alacritty's normal renderable
    /// cursor normalization (wide-spacer snap, VI cursor) unless a validated
    /// PSReadLine paste alias explicitly overrides it.
    fn display_cursor_point(&self) -> Point {
        let raw = self.term.grid().cursor.point;
        self.psreadline_cursor_alias
            .filter(|alias| alias.raw == raw)
            .map(|alias| alias.physical)
            .unwrap_or_else(|| self.term.renderable_content().cursor.point)
    }

    fn effective_cursor_info(&self) -> crate::inspect::CursorInfo {
        let mut info = crate::inspect::cursor_info(&self.term);
        let point = self.display_cursor_point();
        info.row = point.line.0;
        info.col = point.column.0;
        info
    }

    fn clear_psreadline_paste_cursor(&mut self) {
        self.psreadline_cup_scanner = None;
        self.psreadline_cursor_alias = None;
        self.psreadline_cursor_alias_gen = None;
    }

    fn arm_psreadline_paste_cursor(&mut self, text: &str) {
        // Every new paste starts a new decision epoch. Multiline input and
        // app-owned bracketed-paste/TUI modes deliberately fail closed.
        let point = self.display_cursor_point();
        self.clear_psreadline_paste_cursor();
        let mode = *self.term.mode();
        if self.shell != Shell::PowerShell
            || mode.intersects(TermMode::ALT_SCREEN | TermMode::BRACKETED_PASTE)
            || text.is_empty()
            || text.contains(['\r', '\n'])
            || !cursor_at_content_end(&self.term, point)
        {
            return;
        }
        self.psreadline_cup_scanner = Some(CupScanner::default());
    }

    /// Counter bumped every time new PTY bytes arrive in `pump()`. The settle
    /// machinery polls this to detect whether a terminal is still producing output.
    pub fn output_gen(&self) -> u64 {
        self.output_gen
    }

    /// Pump pending PTY output into the grid, then return the rendered viewport
    /// as plain text rows (trailing spaces trimmed). Used by `foreman snapshot`.
    ///
    /// Each call pumps. For a consistent multi-field read (text + attrs and/or
    /// cursor) use [`Self::snapshot_all`] — chaining this with
    /// [`Self::snapshot_cells`] / [`Self::cursor_info`] can stitch fields from
    /// different PTY generations under active output.
    pub fn snapshot_text(&mut self, region: Option<crate::inspect::Region>) -> Vec<String> {
        self.pump();
        crate::inspect::snapshot_text(&self.term, region)
    }

    /// Pump pending PTY output, then return per-cell attribute data (`--attrs`).
    ///
    /// Each call pumps. For a consistent multi-field read use [`snapshot_all`].
    pub fn snapshot_cells(
        &mut self,
        region: Option<crate::inspect::Region>,
    ) -> Vec<Vec<crate::inspect::CellData>> {
        self.pump();
        crate::inspect::snapshot_cells(&self.term, region)
    }

    /// Pump pending PTY output, then return the cursor position + shape (`--cursor`).
    ///
    /// Each call pumps. For a consistent multi-field read use [`snapshot_all`].
    pub fn cursor_info(&mut self) -> crate::inspect::CursorInfo {
        self.pump();
        self.effective_cursor_info()
    }

    /// One pump, then the requested Inspection fields from that grid state.
    /// Prefer this over chaining [`snapshot_text`] / [`snapshot_cells`] /
    /// [`cursor_info`] when more than one field is needed — each of those
    /// pumps independently, so concurrent PTY output can tear the reply.
    pub fn snapshot_all(
        &mut self,
        attrs: bool,
        cursor: bool,
    ) -> (
        Vec<String>,
        Option<Vec<Vec<crate::inspect::CellData>>>,
        Option<crate::inspect::CursorInfo>,
    ) {
        self.pump();
        let text = crate::inspect::snapshot_text(&self.term, None);
        let cells = attrs.then(|| crate::inspect::snapshot_cells(&self.term, None));
        let cursor = cursor.then(|| self.effective_cursor_info());
        (text, cells, cursor)
    }

    fn pump(&mut self) {
        self.pump_at(std::time::Instant::now());
    }

    /// Drain PTY output and advance the Ready gate at `now` (injected so tests
    /// can drive submit delay without real sleep).
    fn pump_at(&mut self, now: std::time::Instant) {
        let mut greplies = Vec::new();
        while let Ok(bytes) = self.rx.try_recv() {
            if let Some(f) = self.rx_dump.as_mut() {
                let _ = f.write_all(&bytes);
            }
            self.ready_gate.on_rx_chunk(&bytes);
            if let Some(scanner) = self.psreadline_cup_scanner.as_mut() {
                let carried_alias_gen = self.psreadline_cursor_alias_gen;
                let result = advance_psreadline_scanned(
                    &mut self.parser,
                    &mut self.term,
                    &mut self.graphics,
                    scanner,
                    &bytes,
                    &mut greplies,
                    self.psreadline_cursor_alias,
                );
                if result.hit_mode_barrier {
                    self.clear_psreadline_paste_cursor();
                } else {
                    self.psreadline_cursor_alias = result.alias;
                    self.psreadline_cursor_alias_gen = match result.alias {
                        None => None,
                        Some(_) if result.refreshed_alias => Some(self.output_gen.wrapping_add(1)),
                        Some(_) => carried_alias_gen,
                    };
                }
            } else {
                advance_scanned(
                    &mut self.parser,
                    &mut self.term,
                    &mut self.graphics,
                    &bytes,
                    &mut greplies,
                );
            }
            if self
                .term
                .mode()
                .intersects(TermMode::ALT_SCREEN | TermMode::BRACKETED_PASTE)
            {
                self.clear_psreadline_paste_cursor();
            }
            self.output_gen = self.output_gen.wrapping_add(1);
            self.content_gen = self.content_gen.wrapping_add(1);
            self.last_output_at = Some(now);
            // A DSR can block the child inside GetConsoleScreenBufferInfo. Do
            // not leave its CPR behind later queued output; flush after the
            // exact chunk that completed the query.
            self.flush_graphics_replies(&mut greplies);
            self.flush_pty_replies();
        }
        // Defensive drains for replies already queued when pump() was entered.
        self.flush_graphics_replies(&mut greplies);
        self.flush_pty_replies();
        // Ready gate: flush queued injects + deferred submit (pure; we write).
        for action in self.ready_gate.poll(now) {
            match action {
                crate::ready::Action::Write(bytes) => self.send_external_input(&bytes),
            }
        }
    }

    fn flush_graphics_replies(&mut self, replies: &mut Vec<u8>) {
        // Graphics replies (a=q probes etc.) go straight back to the app — NOT
        // via `resp` and NOT through the Ready gate: that buffer's flush is
        // what latches DSR readiness, and a graphics reply must never fake it.
        if !replies.is_empty() {
            let replies = std::mem::take(replies);
            let _ = self.writer.write_all(&replies);
            let _ = self.writer.flush();
        }
    }

    fn flush_pty_replies(&mut self) {
        let reply = std::mem::take(&mut *self.resp.lock().unwrap());
        if !reply.is_empty() {
            let sent = self
                .writer
                .write_all(&reply)
                .and_then(|()| self.writer.flush())
                .is_ok();
            if let Some(f) = self.rx_dump.as_mut() {
                let _ = write!(f, "\n<<PTY_REPLY{}", if sent { "" } else { "_ERR" });
                for byte in &reply {
                    let _ = write!(f, " {byte:02x}");
                }
                let _ = writeln!(f, ">>");
            }
            // First device-status reply flushed back = the startup DSR scan is
            // done — half the readiness contract. Session decides "this was a
            // resp flush" and tells the gate; graphics never takes this path.
            self.ready_gate.on_dsr_reply_flushed(sent);
            if !sent {
                // Deliberate no-retry: the reply bytes are dropped. Writer
                // failures track pipe/child death, where the blocked query no
                // longer matters; re-queueing would risk duplicate CPRs. Log
                // so a wedged live session is at least observable.
                eprintln!(
                    "terminal: failed to write a {}-byte PTY reply; the child's \
                     pending console query may stay blocked",
                    reply.len()
                );
            }
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        // Known limitation: ConPTY's reflow still diverges from our grid. The
        // bundled #19535 lineage lazily asks us for the post-resize cursor before
        // a later screen-buffer query, but it cannot restore rows ConPTY dropped
        // or clear stale PSReadLine text. This is NOT a double reflow here, and
        // letting ConPTY own redraw was already disproven. Ctrl+L remains the
        // residual repair; full parity still requires conhost's reflow algorithm.
        // See docs/conpty-resize-reflow.md before touching this.
        if cols < 2 || rows < 1 {
            return;
        }
        if cols != self.cols || rows != self.rows {
            self.clear_psreadline_paste_cursor();
            let (oldc, oldr) = (self.cols, self.rows);
            self.cols = cols;
            self.rows = rows;
            let pulled = resize_anchored(&mut self.term, Size { cols, rows });
            // Reflow under a preserved scroll offset points the viewport at stale
            // content; snap back to the live prompt like a normal terminal.
            self.term.scroll_display(Scroll::Bottom);
            let pty_ok = self
                .master
                .resize(PtySize {
                    rows: rows as u16,
                    cols: cols as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .is_ok();
            // Diagnostic marker in the FOREMAN_RX_DUMP stream: interleaves the
            // resize (and what the anchor compensation did) with the raw ConPTY
            // bytes, so a live repro attributes divergence without guesswork.
            if let Some(f) = self.rx_dump.as_mut() {
                use std::io::Write;
                let cur = self.term.grid().cursor.point;
                let _ = writeln!(
                    f,
                    "\n<<RESIZE {oldc}x{oldr}->{cols}x{rows} pulled={pulled} cursor={},{} pty={}>>",
                    cur.line.0,
                    cur.column.0,
                    if pty_ok { "ok" } else { "ERR" }
                );
            }
        }
        // First resize() = first time the grid has its real render-time width
        // (show() calls this every frame). Flush the deferred note now, fitted
        // so it can never wrap — a wrapped note reflows its head into
        // scrollback on the spawn-time shrink (see inject_note).
        if let Some(note) = self.pending_note.take() {
            // Fit by display columns, not chars: CJK chars in a task prompt
            // occupy two cells each, and a char-count fit would still wrap.
            use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
            let fitted = if note.width() > cols {
                let budget = cols.saturating_sub(4);
                let mut used = 0;
                let head: String = note
                    .chars()
                    .take_while(|c| {
                        used += c.width().unwrap_or(0);
                        used <= budget
                    })
                    .collect();
                format!("{head}… ──")
            } else {
                note
            };
            let bytes = format!("\x1b[2m{fitted}\x1b[0m\r\n").into_bytes();
            self.parser.advance(&mut self.term, &bytes);
            self.content_gen = self.content_gen.wrapping_add(1);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Clipboard paste through the same mode-gated helper the keyboard paths
    /// use (`Event::Paste` / Ctrl+Shift+V): honors bracketed-paste mode and
    /// strips payload ESC. Right-click paste must not bypass this — raw
    /// clipboard bytes submit multi-line pastes line by line and let a
    /// malicious clipboard inject escape sequences.
    fn paste_text(&mut self, txt: &str) {
        if txt.is_empty() {
            return;
        }
        self.arm_psreadline_paste_cursor(txt);
        let seq = crate::input::paste_seq(*self.term.mode(), txt);
        self.send_external_input(&seq);
    }

    /// Pointer → buffer-coord selection point + cell side: the viewport cell
    /// under the pixel, shifted into buffer space by the scrollback offset.
    fn sel_point(&self, metrics: &crate::geom::CellMetrics, p: egui::Pos2) -> (Point, Side) {
        let (row, col) = metrics.cell_at(p);
        let point = viewport_to_point(
            self.term.grid().display_offset(),
            Point::new(row, Column(col)),
        );
        let side = if metrics.cell_right_half(p) {
            Side::Right
        } else {
            Side::Left
        };
        (point, side)
    }

    /// How long the PTY must stay quiet before a fresh grid sample is trusted
    /// for wide-key encoding. `output_gen` advancing means "a chunk arrived",
    /// not "the redraw finished": one keypress echo on a long soft-wrapped
    /// line spans many chunks across frames, and mid-redraw the cursor is
    /// transiently anywhere. Same trust model as the caret gate's settle.
    const WIDE_RESAMPLE_SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

    /// Typing grace: recent shadow-using key activity keeps the
    /// shadow alive briefly after release so the final echo can drain. The
    /// actual held-key signal comes from egui's `key_down`; this timeout is a
    /// tail, not the definition of an active hold.
    const WIDE_INPUT_GRACE: std::time::Duration = std::time::Duration::from_millis(150);

    /// Wide-shadow lifetime policy (pure; unit-tested): keep the simulated
    /// row while the grid sample it was built from is still current, while
    /// the child is still emitting (a mid-redraw grid lies about the cursor),
    /// or while a shadow-using key is physically held/reported this frame. Resample
    /// only after [`Self::WIDE_RESAMPLE_SETTLE`] of PTY silence and
    /// [`Self::WIDE_INPUT_GRACE`] after the active hold.
    fn keep_wide_shadow(
        sampled_gen: u64,
        output_gen: u64,
        last_output: Option<std::time::Instant>,
        last_wide_key: Option<std::time::Instant>,
        modeled_key_active: bool,
        now: std::time::Instant,
    ) -> bool {
        sampled_gen == output_gen
            || modeled_key_active
            || last_output.is_some_and(|t| now.duration_since(t) < Self::WIDE_RESAMPLE_SETTLE)
            || last_wide_key.is_some_and(|t| now.duration_since(t) < Self::WIDE_INPUT_GRACE)
    }

    /// An explicitly invalidated shadow may be rebuilt only after a later PTY
    /// generation is observed and both activity windows have closed. This is
    /// an observation heuristic, not a causal echo acknowledgement: unrelated
    /// output can advance the generation. It still prevents `None` from meaning
    /// "sample the same stale grid on the next frame".
    fn can_reseed_wide_shadow(
        invalidated_gen: u64,
        output_gen: u64,
        last_output: Option<std::time::Instant>,
        last_wide_key: Option<std::time::Instant>,
        current_activity_blocks: bool,
        now: std::time::Instant,
    ) -> bool {
        invalidated_gen != output_gen
            && !current_activity_blocks
            && !last_output.is_some_and(|t| now.duration_since(t) < Self::WIDE_RESAMPLE_SETTLE)
            && !last_wide_key.is_some_and(|t| now.duration_since(t) < Self::WIDE_INPUT_GRACE)
    }

    fn invalidate_wide_shadow(&mut self) {
        self.wide_shadow = WideShadowState::AwaitingEcho {
            invalidated_gen: self.output_gen,
        };
        self.last_wide_key_at = None;
    }

    /// Input written outside the modeled live-key path invalidates its shadow.
    /// Protocol replies intentionally use `send`/writer directly and never pass
    /// here: they do not represent child editing operations.
    fn send_external_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if bytes
            .iter()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0x03))
        {
            self.clear_psreadline_paste_cursor();
        }
        self.invalidate_wide_shadow();
        self.send(bytes);
    }

    /// Read this frame's keyboard input and apply it. The pure encoding lives in
    /// `crate::input::process_input` (terminal-completeness epic, Phase 2); this is
    /// the thin shell that supplies live state (term mode, selection), performs the
    /// side effects (clipboard read, copy, interrupt, scroll), and writes the bytes
    /// to the PTY.
    fn read_input(&mut self, ui: &egui::Ui) {
        let mode = *self.term.mode();
        let has_selection = self
            .term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term))
            .is_some();
        // Wide-char key encode: reuse the persisted shadow row while the
        // child's echo is pending. output_gen advancing is NOT the signal to
        // resample — one keypress echo on a long soft-wrapped line arrives
        // across many chunks over multiple frames, and a grid sampled between
        // chunks reports a transient mid-redraw cursor (the second
        // diagonal-tofu bug, docs/wide-chars.md). Resample only once the PTY
        // has been quiet for WIDE_RESAMPLE_SETTLE and the user is inactive.
        let now = std::time::Instant::now();
        let last_wide_key_before_activity = self.last_wide_key_at;
        let key_activity = ui.input(|i| self.wide_key_latch.update(i));
        if key_activity.active {
            // Observe/stamp before the keep/reseed decision. The latch tells
            // that decision whether this is an ongoing owned hold/release
            // (must keep) or a fresh press that may use a settled grid.
            self.last_wide_key_at = Some(now);
        }

        let prior = std::mem::replace(&mut self.wide_shadow, WideShadowState::Uninitialized);
        let (wide_cursor, wide_basis_gen, awaiting_gen) = match prior {
            WideShadowState::Tracking {
                line,
                col,
                basis_gen,
            } if Self::keep_wide_shadow(
                basis_gen,
                self.output_gen,
                self.last_output_at,
                last_wide_key_before_activity,
                key_activity.blocks_reseed,
                now,
            ) =>
            {
                (Some((line, col)), Some(basis_gen), None)
            }
            WideShadowState::Tracking { .. } | WideShadowState::Uninitialized => {
                let (line, col) = self.wide_line_at_cursor();
                (Some((line, col)), Some(self.output_gen), None)
            }
            // A validated paste alias was sampled at the natural flow endpoint
            // immediately before PSReadLine's final CUP. Unlike an arbitrary
            // just-arrived output chunk, this point is safe to seed from without
            // waiting another 50ms, so the first Backspace after visible paste
            // cannot split the trailing surrogate pair.
            WideShadowState::AwaitingEcho { invalidated_gen }
                if self.has_fresh_cursor_alias(invalidated_gen) =>
            {
                let (line, col) = self.wide_line_at_cursor();
                (Some((line, col)), Some(self.output_gen), None)
            }
            WideShadowState::AwaitingEcho { invalidated_gen }
                if Self::can_reseed_wide_shadow(
                    invalidated_gen,
                    self.output_gen,
                    self.last_output_at,
                    last_wide_key_before_activity,
                    key_activity.blocks_reseed,
                    now,
                ) =>
            {
                let (line, col) = self.wide_line_at_cursor();
                (Some((line, col)), Some(self.output_gen), None)
            }
            WideShadowState::AwaitingEcho { invalidated_gen } => {
                (None, None, Some(invalidated_gen))
            }
        };
        let had_wide_cursor = wide_cursor.is_some();
        let (mut outcome, pasted_text, submitted) = ui.input(|i| {
            let pasted_text = i.events.iter().find_map(|event| match event {
                egui::Event::Paste(text) if !text.is_empty() => Some(text.clone()),
                _ => None,
            });
            let submitted = i.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        ..
                    }
                )
            });
            (
                crate::input::process_input_wide(
                    &i.events,
                    i.modifiers,
                    mode,
                    has_selection,
                    wide_cursor
                        .as_ref()
                        .map(|(line, col)| (line.as_slice(), *col)),
                ),
                pasted_text,
                submitted,
            )
        });
        if submitted || outcome.interrupt {
            self.clear_psreadline_paste_cursor();
        } else if let Some(text) = pasted_text.as_deref() {
            self.arm_psreadline_paste_cursor(text);
        }

        if let Some(s) = outcome.scroll {
            self.term.scroll_display(s);
        }

        // Ctrl+0 resets the global terminal zoom to the default size.
        if outcome.zoom_reset {
            set_font_size(ui.ctx(), crate::config::DEFAULT_FONT_SIZE);
        }

        let mut bytes = std::mem::take(&mut outcome.pty_bytes);
        // Ctrl+Shift+V: the pure pass can't read the clipboard, so it flags the
        // request and we wrap the text here through the same mode-gated helper.
        let encoded_len = bytes.len();
        if outcome.paste_clipboard {
            if let Some(txt) = read_clipboard().filter(|txt| !txt.is_empty()) {
                if !submitted {
                    self.arm_psreadline_paste_cursor(&txt);
                }
                bytes.extend_from_slice(&crate::input::paste_seq(mode, &txt));
            } else if clipboard_has_image() {
                self.clear_psreadline_paste_cursor();
                // Image-only clipboard: forward raw Ctrl+V so agents (Claude,
                // Codex) run their native clipboard-image paste. Plain shells
                // see readline quoted-insert — harmless. (spec WS2)
                bytes.push(0x16);
            }
        }
        let clipboard_input_added = bytes.len() != encoded_len;
        if clipboard_input_added {
            outcome.wide_after = None;
        }
        // Key chords like Ctrl+J/Ctrl+M submit with encoded LF/CR without an
        // egui Enter event. End the paste epoch according to bytes actually
        // sent, matching the external-input path.
        if bytes
            .iter()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0x03))
        {
            self.clear_psreadline_paste_cursor();
        }

        // Persist a modeled shadow with the generation it was BUILT from, not
        // the current generation. When tracking drops, enter AwaitingEcho and
        // do not sample again until a later-observed PTY generation is quiet.
        // If already awaiting, any newly-sent input moves the required
        // generation forward so the same stale generation cannot re-seed it.
        let input_may_change_grid = !bytes.is_empty() || outcome.interrupt;
        self.wide_shadow = match outcome.wide_after.take() {
            Some((line, col)) => WideShadowState::Tracking {
                line,
                col,
                basis_gen: wide_basis_gen.expect("tracked input has a sampled basis"),
            },
            None => {
                if had_wide_cursor {
                    // A text/paste/unmodeled event dropped a previously valid
                    // model. Its earlier key grace must not protect a future,
                    // unrelated sample.
                    self.last_wide_key_at = None;
                }
                WideShadowState::AwaitingEcho {
                    invalidated_gen: if had_wide_cursor || input_may_change_grid {
                        self.output_gen
                    } else {
                        awaiting_gen.unwrap_or(self.output_gen)
                    },
                }
            }
        };

        if !bytes.is_empty() {
            self.term.scroll_display(Scroll::Bottom);
            self.caret.note_input(std::time::Instant::now());
            self.send(&bytes);
        }

        if outcome.copy {
            if let Some(txt) = self.term.selection_to_string() {
                ui.ctx().copy_text(txt);
                if outcome.copy_clears {
                    self.term.selection = None;
                }
            }
        } else if outcome.interrupt {
            self.send(&[0x03]); // Ctrl+C with no selection = interrupt
        }
    }

    /// Render the terminal into `rect`. Reads keyboard if `active`.
    /// `resp` is the content-area interaction from the window manager (used for
    /// mouse text selection and right-click paste).
    /// Keep an *inactive* (un-rendered) tab's PTY alive: drain the reader channel
    /// into the grid and answer any pending device queries (e.g. the startup DSR
    /// `ESC[6n`). The reader thread runs independently of rendering, so this only
    /// needs to advance the parser — without resizing, drawing, or reading input.
    /// Called every frame for tabs that are not the active tab so a backgrounded
    /// shell never hangs on a query and keeps producing output.
    pub fn keepalive(&mut self) {
        self.pump();
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        resp: &egui::Response,
    ) {
        let font_px = font_size(ui.ctx());
        let font = egui::FontId::monospace(font_px);
        let probe = ui
            .painter()
            .layout_no_wrap("M".to_string(), font.clone(), FG);
        let cw = probe.rect.width().max(1.0);
        let rh = probe.rect.height().max(1.0);
        let cols = ((rect.width() / cw).floor() as usize).clamp(2, 600);
        let rows = ((rect.height() / rh).floor() as usize).clamp(1, 300);
        self.resize(cols, rows);
        self.pump();
        let metrics = crate::geom::CellMetrics::new(rect, cw, rh, cols, rows);
        if active {
            // mouse text selection (the WM hands us the content-area drag)
            if resp.triple_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (point, side) = self.sel_point(&metrics, p);
                    self.term.selection = Some(Selection::new(SelectionType::Lines, point, side));
                }
            } else if resp.double_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (point, side) = self.sel_point(&metrics, p);
                    self.term.selection =
                        Some(Selection::new(SelectionType::Semantic, point, side));
                }
            } else if resp.drag_started() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (point, side) = self.sel_point(&metrics, p);
                    self.term.selection = Some(Selection::new(SelectionType::Simple, point, side));
                }
            } else if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (point, side) = self.sel_point(&metrics, p);
                    if let Some(sel) = self.term.selection.as_mut() {
                        sel.update(point, side);
                    }
                }
            } else if resp.clicked() {
                // clicked() also fires on the frame a double/triple-click
                // completes, so plain-click-clears must stay LAST in the chain.
                self.term.selection = None;
            }
            if resp.secondary_clicked() {
                if let Some(txt) = read_clipboard() {
                    self.paste_text(&txt);
                }
            }
            self.read_input(ui);
        }

        // Mouse-wheel (works whenever the pane is hovered). On the alternate
        // screen / under mouse reporting, foreman's own scrollback is empty, so
        // the wheel is forwarded to the app (mouse events or arrow keys) via the
        // pure `input::wheel_input` seam; otherwise it scrolls local scrollback.
        if resp.hovered() {
            // egui delivers a wheel notch as smoothed per-frame fractions. Rounding
            // each frame both drops gentle scrolls (→0) and over-emits fast ones, so
            // accumulate the sub-line remainder and emit only whole lines.
            let (dy, ctrl) = ui.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    i.modifiers.ctrl || i.modifiers.command,
                )
            });
            if ctrl && dy != 0.0 {
                // Ctrl+Scroll zooms the GLOBAL terminal font instead of scrolling.
                // Accumulate against the notch size (same smoothing as line scroll)
                // and step whole notches; the wheel is fully consumed here so it
                // neither moves scrollback nor reaches the app.
                let (steps, rem) = crate::input::wheel_steps(self.zoom_accum, dy, ZOOM_NOTCH_PX);
                self.zoom_accum = rem;
                if steps != 0.0 {
                    let next = crate::input::zoom_step(font_size(ui.ctx()), steps);
                    set_font_size(ui.ctx(), next);
                }
            } else if dy != 0.0 {
                let (steps, rem) = crate::input::wheel_steps(self.scroll_accum, dy, rh);
                self.scroll_accum = rem;
                let lines = steps as i32;
                if lines != 0 {
                    // pointer → 1-based viewport cell (mouse-protocol order)
                    let (col, row) = match resp.hover_pos() {
                        Some(p) => metrics.mouse_cell(p),
                        None => (1, 1),
                    };
                    let mode = *self.term.mode();
                    let action = crate::input::wheel_input(lines, mode, col, row);
                    match action {
                        // Forwarding writes INPUT to the app, so gate on `active`
                        // (focus) like the key path — hovering an unfocused pane
                        // must not inject keys/mouse into it. Scrollback is
                        // read-only and stays available on any hovered pane.
                        crate::input::WheelAction::Pty(b) => {
                            if active {
                                self.send(&b);
                            }
                        }
                        crate::input::WheelAction::Scrollback(s) => {
                            self.term.scroll_display(s);
                        }
                    }
                }
            }
        }

        let (cur_line, cur_col, cur_shape) = {
            let shape = self.term.renderable_content().cursor.shape;
            let point = self.display_cursor_point();
            (point.line.0, point.column.0, shape)
        };
        // De-jitter the caret through the Caret gate: a non-synchronized TUI
        // moves the cursor all over the screen while it redraws, so the gate
        // holds the committed spot until the cursor settles. See `crate::caret`.
        let cursor_draw = self.caret.observe(
            crate::caret::CursorModel {
                line: cur_line,
                col: cur_col,
                shape: cur_shape,
            },
            std::time::Instant::now(),
        );

        // Selection range in viewport coords: the ONE `term.selection` feeds both
        // the copy text (`selection_to_string`) and this highlight range.
        // `to_range` returns ordered buffer coords clamped to the live grid; the
        // cull maps them onto the visible viewport.
        let sel = self
            .term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term))
            .and_then(|r| {
                let grid = self.term.grid();
                sel_viewport_range(
                    r,
                    grid.display_offset(),
                    grid.screen_lines(),
                    grid.columns(),
                )
            });

        let overlays = crate::frame::overlays(self.term.grid(), &metrics, sel, cursor_draw);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::ZERO, BG);

        // Grid-locked mono paint — Strategy P: one galley per non-blank placement,
        // blit pos from live cell_rect (not free-flow advances; not cached Pos2).
        // Rebuilt only when the content/scroll/dims/font key changes; cache hit
        // re-blits only (0 layout_*). Blank galleys skipped; non-default SGR bg
        // rects cached for all cells incl. spaces. Galleys deduped by (char, style).
        // Selection + caret are overlays (below), so they never invalidate this.
        let key = MonoPaintKey {
            content_gen: self.content_gen,
            off: self.term.grid().display_offset(),
            cols: self.cols,
            rows: self.rows,
            font_bits: font_px.to_bits(),
        };
        // Plan only on miss so cache hits stay free of plan_paint + layout_*.
        // Built before borrowing mono_paint mutably (self.term vs cache).
        // emoji_sites are stored with the mono key so hits still stamp without
        // replan; atlas fill never changes this key (separate texture cache).
        let plan = match &self.mono_paint {
            Some(c) if c.key == key => None,
            _ => Some(crate::frame::plan_paint(self.term.grid(), &metrics)),
        };
        let (items, bgs, emoji_sites) = {
            let cache = self.mono_paint.get_or_insert_with(MonoPaintCache::empty);
            cache.get_or_rebuild(key, || {
                let plan = plan.expect("rebuild closure only runs on cache miss");
                let mut layout = |ch: char, st: GlyphStyle| {
                    note_layout_call();
                    let line = |on: bool| {
                        if on {
                            egui::Stroke::new(1.0, st.fg)
                        } else {
                            egui::Stroke::NONE
                        }
                    };
                    let mut job = LayoutJob::default();
                    job.wrap.max_width = f32::INFINITY;
                    // Cell SGR bg paints via the MonoBg rect pass (incl. blank cells);
                    // keep TextFormat background transparent so we don't double-fill.
                    job.append(
                        &ch.to_string(),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::monospace(font_px),
                            color: st.fg,
                            background: egui::Color32::TRANSPARENT,
                            underline: line(st.underline),
                            strikethrough: line(st.strikethrough),
                            ..Default::default()
                        },
                    );
                    painter.layout_job(job)
                };
                let (items, bgs) = mono_paint_items(&plan, &mut layout);
                (items, bgs, plan.emoji_sites)
            })
        };
        for bg in bgs.iter() {
            let end_col = bg.col + (bg.width_cells as usize).saturating_sub(1);
            painter.rect_filled(
                metrics.span_rect(bg.row, bg.col, end_col),
                egui::CornerRadius::ZERO,
                bg.color,
            );
        }

        // Color emoji stamps — resolve textures first so successful stamps can
        // suppress mono blit (mono outlines overhang the cell and fringe the
        // stamp). Fail-open: atlas None keeps mono. Separate texture cache;
        // never rebuilds mono on atlas hit/miss.
        let emoji_px = font_px.round().max(1.0) as u32;
        retain_emoji_textures_at_px(&mut self.emoji_textures, emoji_px);
        let mut stamp_ready: Vec<(&crate::frame::EmojiSite, egui::TextureHandle)> = Vec::new();
        if !emoji_sites.is_empty() {
            for site in emoji_sites.iter() {
                let tex = match self.emoji_textures.get(&(site.ch, emoji_px)) {
                    Some(t) => Some(t.clone()),
                    None => match self.emoji_raster.color_glyph(site.ch, emoji_px) {
                        Some(g) => {
                            let img = egui::ColorImage::from_rgba_unmultiplied(
                                [g.w as usize, g.h as usize],
                                &g.rgba,
                            );
                            // Nearest: color emoji are small bitmaps; linear
                            // stretch fringes alpha edges against the cell bg.
                            let t = ui.ctx().load_texture(
                                format!("emoji_{}_{}", site.ch as u32, emoji_px),
                                img,
                                egui::TextureOptions::NEAREST,
                            );
                            self.emoji_textures.insert((site.ch, emoji_px), t.clone());
                            Some(t)
                        }
                        None => None,
                    },
                };
                if let Some(tex) = tex {
                    stamp_ready.push((site, tex));
                }
            }
        }
        let stamped: std::collections::HashSet<(usize, usize)> =
            stamp_ready.iter().map(|(s, _)| (s.row, s.col)).collect();

        for g in items.iter() {
            if stamped.contains(&(g.row, g.col)) {
                continue; // color stamp covers this cell; mono would fringe
            }
            // Unstamped EP emoji fall through here on purpose: mono tofu is
            // the fail-open when the color raster has no glyph.
            painter.galley(metrics.cell_rect(g.row, g.col).min, g.galley.clone(), FG);
        }

        if !stamp_ready.is_empty() {
            let off = self.term.grid().display_offset() as i32;
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            for (site, tex) in &stamp_ready {
                let cell = &self.term.grid()[Line(site.row as i32 - off)][Column(site.col)];
                let style = glyph_style(cell.flags, cell.fg, cell.bg);
                let bg = style.bg.unwrap_or(BG);
                let end_col = site.col + (site.width_cells as usize).saturating_sub(1);
                let span = metrics.span_rect(site.row, site.col, end_col);
                // Cell bg underlay (clears mono hole + transparent stamp corners).
                painter.rect_filled(span, egui::CornerRadius::ZERO, bg);
                // Aspect-fit inside the span — stretch-to-fill squashes DWrite
                // padding into soft fringes.
                let size = tex.size_vec2();
                let fit = emoji_stamp_fit_rect(span, size.x, size.y);
                painter.image(tex.id(), fit, uv, egui::Color32::WHITE);
            }
        }

        // Kitty graphics overlay — images are pure overlay; the grid stays
        // text (spec: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md).
        // Drop textures whose image data is gone (deleted/evicted). Runs even
        // when no placements are live — a delete of the LAST image must still
        // release its texture (the paint block below is skipped entirely then).
        if !self.textures.is_empty() {
            self.textures.retain(|id, _| self.graphics.has_image(*id));
        }
        if self.graphics.active() {
            let (alt, hist, off, lines) = {
                let g = self.term.grid();
                (
                    self.term.mode().contains(TermMode::ALT_SCREEN),
                    g.history_size(),
                    g.display_offset(),
                    g.screen_lines(),
                )
            };
            let vv = crate::graphics::ViewportView {
                alt_screen: alt,
                history_size: hist,
                display_offset: off,
                screen_lines: lines,
            };
            for p in self.graphics.visible(&vv) {
                let tex = match self.textures.get(&p.id) {
                    Some((g, t)) if *g == p.r#gen => t.clone(), // cheap Arc clone
                    _ => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [p.w as usize, p.h as usize],
                            p.rgba,
                        );
                        let t = ui.ctx().load_texture(
                            format!("kittyimg{}", p.id),
                            img,
                            egui::TextureOptions::LINEAR,
                        );
                        self.textures.insert(p.id, (p.r#gen, t.clone()));
                        t
                    }
                };
                // c/r from the client when given (pets always sends them);
                // otherwise derive the cell span from pixel size.
                let cols_f = if p.cols > 0 {
                    p.cols as f32
                } else {
                    (p.w as f32 / cw).ceil().max(1.0)
                };
                let rows_f = if p.rows > 0 {
                    p.rows as f32
                } else {
                    (p.h as f32 / rh).ceil().max(1.0)
                };
                let min = egui::pos2(
                    rect.min.x + p.col as f32 * cw,
                    rect.min.y + p.line as f32 * rh,
                );
                let img_rect = egui::Rect::from_min_size(min, egui::vec2(cols_f * cw, rows_f * rh));
                painter.image(
                    tex.id(),
                    img_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        for r in &overlays.highlights {
            painter.rect_filled(*r, egui::CornerRadius::ZERO, SELECTION);
        }

        // caret — the gate chose the cell; focus (`active`) gates whether we paint.
        if active && let Some(r) = overlays.caret {
            painter.rect_filled(r, egui::CornerRadius::ZERO, CARET);
        }

        // scrollback indicator: thin right-edge thumb, shown only when there is
        // history and the user is scrolled back or hovering the pane.
        if let Some(r) = overlays.thumb
            && (overlays.scrolled_back || resp.hovered())
        {
            painter.rect_filled(r, egui::CornerRadius::same(2), SCROLL_THUMB);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::Line;

    type RecordedWrites = Arc<Mutex<Vec<(Vec<u8>, Option<String>)>>>;

    // -- keep_wide_shadow: shadow lifetime vs PTY quiescence -----------------

    #[test]
    fn wide_shadow_kept_while_gen_unchanged() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_secs(60);
        // Same gen: keep regardless of how long the PTY has been quiet.
        assert!(Session::keep_wide_shadow(5, 5, None, None, false, now));
        assert!(Session::keep_wide_shadow(5, 5, Some(t0), None, false, now));
    }

    #[test]
    fn wide_shadow_survives_mid_redraw_chunks() {
        // Gen advanced but the chunk landed 10ms ago — the long-line redraw
        // may still be in flight; the mid-redraw grid must not be trusted.
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_millis(10);
        assert!(Session::keep_wide_shadow(5, 7, Some(t0), None, false, now));
    }

    #[test]
    fn wide_shadow_resampled_once_pty_settles() {
        let t0 = std::time::Instant::now();
        let now = t0 + Session::WIDE_RESAMPLE_SETTLE + std::time::Duration::from_millis(10);
        // Quiet past the settle window: the grid is eligible under the
        // observed-quiescence heuristic.
        assert!(!Session::keep_wide_shadow(5, 7, Some(t0), None, false, now));
        // Gen advanced with no output timestamp at all: trust the grid.
        assert!(!Session::keep_wide_shadow(5, 7, None, None, false, now));
    }

    #[test]
    fn wide_shadow_kept_while_user_actively_typing_modeled_keys() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_millis(100);
        // Gen advanced and PTY quiet for 100ms (> 50ms settle), but user pressed
        // a wide key 100ms ago (< 150ms grace) -> keep shadow to avoid stale grid.
        assert!(Session::keep_wide_shadow(
            5,
            7,
            Some(t0),
            Some(t0),
            false,
            now
        ));
    }

    #[test]
    fn wide_shadow_kept_for_a_physical_hold_past_timestamp_grace() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_secs(2);
        // Slow repeat settings and a scheduler stall can both exceed 150ms.
        // egui's held state is authoritative for the active burst.
        assert!(Session::keep_wide_shadow(
            5,
            7,
            Some(t0),
            Some(t0),
            true,
            now
        ));
    }

    #[test]
    fn wide_shadow_resampled_after_user_inactivity() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_millis(160);
        // Gen advanced, PTY quiet, and user inactive for 160ms (> 150ms grace) -> resample.
        assert!(!Session::keep_wide_shadow(
            5,
            7,
            Some(t0),
            Some(t0),
            false,
            now
        ));
    }

    #[test]
    fn invalidated_shadow_cannot_reseed_from_the_same_generation() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_secs(2);
        assert!(!Session::can_reseed_wide_shadow(
            5, 5, None, None, false, now
        ));
    }

    #[test]
    fn invalidated_shadow_waits_for_new_output_to_settle() {
        let t0 = std::time::Instant::now();
        let recent = t0 + std::time::Duration::from_millis(10);
        assert!(!Session::can_reseed_wide_shadow(
            5,
            6,
            Some(t0),
            None,
            false,
            recent
        ));

        let settled = t0 + Session::WIDE_RESAMPLE_SETTLE + std::time::Duration::from_millis(10);
        assert!(Session::can_reseed_wide_shadow(
            5,
            6,
            Some(t0),
            None,
            false,
            settled
        ));
    }

    #[test]
    fn invalidated_shadow_can_reseed_before_a_fresh_press_after_prior_settle() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_secs(2);
        // The current press is applied after this decision. Only activity from
        // earlier frames should block a settled pre-key grid sample.
        assert!(Session::can_reseed_wide_shadow(
            5,
            6,
            Some(t0),
            Some(t0),
            false,
            now
        ));
    }

    #[test]
    fn invalidated_shadow_does_not_reseed_during_an_owned_hold_after_a_stall() {
        let t0 = std::time::Instant::now();
        let now = t0 + std::time::Duration::from_secs(2);
        assert!(!Session::can_reseed_wide_shadow(
            5,
            6,
            Some(t0),
            Some(t0),
            true,
            now
        ));
    }

    #[test]
    fn wide_key_activity_spans_repeat_gaps_and_observes_release_frame() {
        fn key_event(pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
            egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }
        }

        let ctx = egui::Context::default();
        let mut latch = WideKeyLatch::default();
        let mut activity = WideKeyActivity::default();
        let mut press = egui::RawInput::default();
        press
            .events
            .push(key_event(true, egui::Modifiers::default()));
        let _ = ctx.run_ui(press, |ui| activity = ui.input(|i| latch.update(i)));
        assert!(activity.active, "press frame is active");
        assert!(
            !activity.blocks_reseed,
            "first press may use a settled grid"
        );

        // No repeat event this frame: egui's persistent keys_down state keeps
        // the burst active through the OS repeat delay.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            activity = ui.input(|i| latch.update(i))
        });
        assert!(activity.active, "held key remains active between repeats");
        assert!(activity.blocks_reseed, "an owned hold cannot reseed");

        let mut release = egui::RawInput::default();
        release.events.push(key_event(
            false,
            egui::Modifiers {
                ctrl: true,
                ..egui::Modifiers::default()
            },
        ));
        let _ = ctx.run_ui(release, |ui| activity = ui.input(|i| latch.update(i)));
        assert!(
            activity.active && activity.blocks_reseed,
            "owned release starts grace even if modifiers changed"
        );

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            activity = ui.input(|i| latch.update(i))
        });
        assert!(!activity.active, "activity ends after release events drain");
    }

    #[test]
    fn wide_key_latch_ignores_a_release_owned_by_another_session() {
        let ctx = egui::Context::default();
        let mut latch = WideKeyLatch::default();
        let mut release = egui::RawInput::default();
        release.events.push(egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        let mut activity = WideKeyActivity {
            active: true,
            blocks_reseed: true,
        };
        let _ = ctx.run_ui(release, |ui| activity = ui.input(|i| latch.update(i)));
        assert_eq!(activity, WideKeyActivity::default());
    }

    #[test]
    fn wide_key_latch_clears_a_release_missed_while_inactive_without_new_grace() {
        let ctx = egui::Context::default();
        let mut latch = WideKeyLatch::default();
        let mut press = egui::RawInput::default();
        press.events.push(egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        let _ = ctx.run_ui(press, |ui| assert!(ui.input(|i| latch.update(i)).active));

        // The global release is processed while this Session is inactive, so
        // its latch does not see the release event.
        let mut release = egui::RawInput::default();
        release.events.push(egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        let _ = ctx.run_ui(release, |_| {});

        let mut activity = WideKeyActivity {
            active: true,
            blocks_reseed: true,
        };
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            activity = ui.input(|i| latch.update(i))
        });
        assert_eq!(
            activity,
            WideKeyActivity::default(),
            "reactivation cleanup must not invent a current release"
        );
    }

    #[test]
    fn wide_key_latch_treats_a_new_press_after_missed_release_as_fresh() {
        let ctx = egui::Context::default();
        let mut latch = WideKeyLatch::default();
        let key_event = |pressed| egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };

        let mut first_press = egui::RawInput::default();
        first_press.events.push(key_event(true));
        let _ = ctx.run_ui(first_press, |ui| {
            assert!(ui.input(|i| latch.update(i)).active)
        });

        let mut missed_release = egui::RawInput::default();
        missed_release.events.push(key_event(false));
        let _ = ctx.run_ui(missed_release, |_| {});

        // No intermediate active frame clears the stale latch bit. The next
        // non-repeat press must still begin a fresh ownership epoch.
        let mut next_press = egui::RawInput::default();
        next_press.events.push(key_event(true));
        let mut activity = WideKeyActivity::default();
        let _ = ctx.run_ui(next_press, |ui| activity = ui.input(|i| latch.update(i)));
        assert_eq!(
            activity,
            WideKeyActivity {
                active: true,
                blocks_reseed: false,
            }
        );
    }

    /// Live ConPTY/PSReadLine verification for the real Session::read_input
    /// path. Unlike `foreman send --keys`, this drives egui press/repeat/release
    /// frames, including a >150ms held-key gap, against a real cooked prompt.
    #[test]
    #[ignore = "diagnostic: drives a real PowerShell cooked-input session"]
    fn live_held_backspace_clears_wrapped_mixed_wide_input_buffer() {
        fn pump_for(session: &mut Session, duration: std::time::Duration) {
            let deadline = std::time::Instant::now() + duration;
            while std::time::Instant::now() < deadline {
                session.pump();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        fn input_frame(ctx: &egui::Context, session: &mut Session, pressed: bool, repeat: bool) {
            let mut raw = egui::RawInput::default();
            raw.events.push(egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed,
                repeat,
                modifiers: egui::Modifiers::default(),
            });
            let _ = ctx.run_ui(raw, |ui| session.read_input(ui));
        }

        let ctx = egui::Context::default();
        let shell = preferred_powershell(std::env::var_os("PATH").as_deref(), &|p| p.is_file());
        let argv = vec![
            shell.to_string(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            "function global:prompt { 'P> ' }".to_string(),
        ];
        let mut session = Session::spawn_argv(&argv, None, &[], ctx.clone()).expect("spawn");
        // P> occupies 3 cells; 40 content cells then fit exactly into 43 cols,
        // so the fixture wraps without putting a wide glyph across the margin.
        session.resize(43, 12);
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !session.ready() {
            session.pump();
            assert!(
                std::time::Instant::now() < ready_deadline,
                "PowerShell never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        pump_for(&mut session, std::time::Duration::from_millis(300));

        let payload = "a🤣中🥒b".repeat(30);
        let presses = payload.chars().count();
        session.feed(payload.as_bytes());
        pump_for(&mut session, std::time::Duration::from_millis(800));
        assert!(session.snapshot_text(None).join("\n").contains('🤣'));

        // External feed invalidated the old shadow. Once its output is quiet,
        // one event-free UI frame re-seeds from the settled grid.
        std::thread::sleep(std::time::Duration::from_millis(200));
        session.pump();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| session.read_input(ui));
        assert!(matches!(
            &session.wide_shadow,
            WideShadowState::Tracking { .. }
        ));

        for index in 0..presses {
            if index == 60 {
                // Expire both timestamp windows while the Backspace key remains
                // logically down. The per-session latch must still preserve the
                // simulated row rather than sampling a partial redraw.
                session.pump();
                std::thread::sleep(std::time::Duration::from_millis(220));
            } else {
                session.pump();
            }
            input_frame(&ctx, &mut session, true, index != 0);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        input_frame(&ctx, &mut session, false, false);
        pump_for(&mut session, std::time::Duration::from_millis(1200));

        let screen = session.snapshot_text(None).join("\n");
        assert!(
            !screen.contains('🤣'),
            "emoji input was not fully deleted: {screen:?}"
        );
        assert!(screen.contains("P>"), "prompt disappeared: {screen:?}");

        // Prove PSReadLine's input buffer is clean, independently of ConPTY's
        // known stale-cell residue: a surviving surrogate would prefix/corrupt
        // this command instead of producing a second marker as output.
        session.feed(b"Write-Output __WIDE_HOLD_OK__\r");
        pump_for(&mut session, std::time::Duration::from_millis(800));
        let command_screen = session.snapshot_text(None).join("\n");
        assert!(
            command_screen.matches("__WIDE_HOLD_OK__").count() >= 2,
            "post-hold command did not execute cleanly: {command_screen:?}"
        );

        // A doubled DEL can still leave one stale U+FFFD paint cell in ConPTY's
        // emitted redraw even though the cooked buffer is clean. Ctrl+L is the
        // documented upstream-residue repair and must clear it.
        session.feed(&[0x0c]);
        pump_for(&mut session, std::time::Duration::from_millis(500));
        let cleared = session.snapshot_text(None).join("\n");
        assert!(
            !cleared.contains('\u{FFFD}'),
            "Ctrl+L did not heal residue: {cleared:?}"
        );
    }

    /// Live paste-only proof for the PSReadLine surrogate-at-margin mismatch.
    /// The raw alacritty cursor remains at PSReadLine's CUP, while Foreman's
    /// effective caret and wide-key seam use the validated natural flow end.
    #[test]
    #[ignore = "diagnostic: drives a real PowerShell paste through ConPTY"]
    fn live_psreadline_paste_wrap_uses_the_whole_emoji_endpoint() {
        fn pump_for(session: &mut Session, duration: std::time::Duration) {
            let deadline = std::time::Instant::now() + duration;
            while std::time::Instant::now() < deadline {
                session.pump();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        let ctx = egui::Context::default();
        let shell = preferred_powershell(std::env::var_os("PATH").as_deref(), &|p| p.is_file());
        let argv = vec![
            shell.to_string(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            "function global:prompt { 'P> ' }".to_string(),
        ];
        let mut session = Session::spawn_argv(&argv, None, &[], ctx.clone()).expect("spawn");
        // spawn_argv labels arbitrary programs as Cmd for icon/encoding policy;
        // this fixture is explicitly PowerShell and needs the production gate.
        session.shell = Shell::PowerShell;
        session.resize(20, 8);
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !session.ready() {
            session.pump();
            assert!(
                std::time::Instant::now() < ready_deadline,
                "PowerShell never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        pump_for(&mut session, std::time::Duration::from_millis(300));

        // P>_ starts input at zero-based col 3. Sixteen ASCII cells leave the
        // emoji at the final column, which is the exact PSReadLine failure case.
        let payload = format!("{}🤣", "a".repeat(16));
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Paste(payload));
        let _ = ctx.run_ui(raw, |ui| session.read_input(ui));
        let alias_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while session.psreadline_cursor_alias.is_none() {
            session.pump();
            assert!(
                std::time::Instant::now() < alias_deadline,
                "live PSReadLine final CUP was not reconciled"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let alias = session
            .psreadline_cursor_alias
            .expect("live PSReadLine final CUP was not reconciled");
        assert_eq!(alias.physical.line, alias.raw.line);
        assert_eq!(alias.physical.column.0, alias.raw.column.0 + 1);
        assert_eq!(session.input_cursor_point(), alias.physical);
        assert_eq!(session.display_cursor_point(), alias.physical);
        let info = session.cursor_info();
        assert_eq!(
            (info.row, info.col),
            (alias.physical.line.0, alias.physical.column.0)
        );
        assert!(
            session
                .last_output_at
                .is_some_and(|at| at.elapsed() < Session::WIDE_RESAMPLE_SETTLE),
            "fixture accidentally waited past the ordinary shadow settle window"
        );

        // Drive the real GUI-key path. Starting from the alias must make this
        // one physical Backspace send the two DEL units PSReadLine needs for
        // the surrogate pair; a single DEL leaves U+FFFD in the cooked buffer.
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let mut press = egui::RawInput::default();
        press.events.push(key(true));
        let _ = ctx.run_ui(press, |ui| session.read_input(ui));
        let mut release = egui::RawInput::default();
        release.events.push(key(false));
        let _ = ctx.run_ui(release, |ui| session.read_input(ui));
        pump_for(&mut session, std::time::Duration::from_millis(800));
        let screen = session.snapshot_text(None).join("\n");
        assert!(
            !screen.contains('🤣'),
            "emoji survived one Backspace: {screen:?}"
        );
        assert!(
            !screen.contains('\u{FFFD}'),
            "Backspace left a surrogate half: {screen:?}"
        );
        assert!(
            screen.contains(&"a".repeat(16)),
            "Backspace over-deleted the narrow prefix: {screen:?}"
        );
    }

    struct TitleRecordingWriter {
        title: Arc<Mutex<Option<String>>>,
        writes: RecordedWrites,
    }

    impl Write for TitleRecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let title = self.title.lock().unwrap().clone();
            self.writes.lock().unwrap().push((bytes.to_vec(), title));
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "intentional test failure",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn named(n: NamedColor) -> AnsiColor {
        AnsiColor::Named(n)
    }

    #[test]
    fn layout_counter_increments_when_noted() {
        reset_layout_call_count();
        assert_eq!(layout_call_count(), 0);
        note_layout_call();
        note_layout_call();
        assert_eq!(layout_call_count(), 2);
        reset_layout_call_count();
        assert_eq!(layout_call_count(), 0);
    }

    fn default_style() -> GlyphStyle {
        GlyphStyle {
            fg: FG,
            bg: None,
            underline: false,
            strikethrough: false,
        }
    }

    /// Build a real Arc<Galley> via a headless Context (no window).
    /// Fonts are unavailable until the first `Context::run`, so we tick once.
    fn dummy_galley_for_tests(ch: char) -> std::sync::Arc<egui::Galley> {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                ch.to_string(),
                egui::FontId::monospace(14.0),
                egui::Color32::WHITE,
            )
        })
    }

    #[test]
    fn emoji_stamp_fit_preserves_aspect_and_centers() {
        let span = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(16.0, 16.0));
        // Tall texture: limited by height → width < span.
        let fit = emoji_stamp_fit_rect(span, 8.0, 16.0);
        assert!((fit.height() - 16.0).abs() < 0.01);
        assert!((fit.width() - 8.0).abs() < 0.01);
        assert!((fit.center().x - span.center().x).abs() < 0.01);
        assert!((fit.center().y - span.center().y).abs() < 0.01);
    }

    #[test]
    fn mono_paint_skips_blanks_and_dedupes_layouts() {
        reset_layout_call_count();
        let style = default_style();
        let styled_space = GlyphStyle {
            bg: Some(egui::Color32::from_rgb(0, 0, 80)),
            ..default_style()
        };
        let plan = crate::frame::PaintPlan {
            glyphs: vec![
                crate::frame::GlyphPlacement {
                    row: 0,
                    col: 0,
                    ch: 'a',
                    style,
                    width_cells: 1,
                },
                crate::frame::GlyphPlacement {
                    row: 0,
                    col: 1,
                    ch: 'a',
                    style,
                    width_cells: 1,
                },
                crate::frame::GlyphPlacement {
                    row: 0,
                    col: 2,
                    ch: ' ',
                    style: styled_space,
                    width_cells: 1,
                },
                crate::frame::GlyphPlacement {
                    row: 0,
                    col: 3,
                    ch: 'b',
                    style,
                    width_cells: 1,
                },
            ],
            emoji_sites: Vec::new(),
        };
        let m = crate::geom::CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(32.0, 16.0)),
            8.0,
            16.0,
            4,
            1,
        );
        let mut layout = |ch: char, _style: GlyphStyle| {
            note_layout_call();
            dummy_galley_for_tests(ch)
        };
        let (items, bgs) = mono_paint_items_for_test(&plan, &mut layout);
        assert_eq!(items.len(), 3, "blank galley skipped");
        assert_eq!(layout_call_count(), 2, "two 'a's share one layout; + 'b'");
        // Grid identity (not free-flow advances); blit pos from live metrics.
        assert_eq!((items[0].row, items[0].col), (0, 0));
        assert_eq!((items[1].row, items[1].col), (0, 1));
        assert_eq!((items[2].row, items[2].col), (0, 3));
        assert_eq!(
            m.cell_rect(items[0].row, items[0].col).min,
            m.cell_rect(0, 0).min
        );
        // Colored blank still yields a bg rect with no layout call.
        assert_eq!(bgs.len(), 1);
        assert_eq!((bgs[0].row, bgs[0].col), (0, 2));
        assert_eq!(bgs[0].width_cells, 1);
        assert_eq!(bgs[0].color, egui::Color32::from_rgb(0, 0, 80));
    }

    #[test]
    fn mono_paint_keeps_emoji_glyphs_for_fail_open() {
        // d5bc017 regression: default-emoji-presentation glyphs were dropped
        // from mono items unconditionally, so a failed color stamp
        // (NullEmojiRaster — DirectWrite init/COM failure) painted NOTHING.
        // The paint loop already suppresses mono under a *successful* stamp
        // (`stamped.contains`), so items must keep EP glyphs: tofu is the
        // fail-open, invisible is content loss.
        let style = default_style();
        let plan = crate::frame::PaintPlan {
            glyphs: vec![crate::frame::GlyphPlacement {
                row: 0,
                col: 0,
                ch: '🤣',
                style,
                width_cells: 2,
            }],
            emoji_sites: Vec::new(),
        };
        let mut layout = |ch: char, _style: GlyphStyle| dummy_galley_for_tests(ch);
        let (items, _bgs) = mono_paint_items_for_test(&plan, &mut layout);
        assert_eq!(
            items.len(),
            1,
            "EP glyph must stay in mono items (fail-open tofu when unstamped)"
        );
        assert_eq!((items[0].row, items[0].col), (0, 0));
    }

    #[test]
    fn mono_paint_wide_glyph_bg_spans_full_width() {
        // plan_paint skips WIDE_CHAR_SPACER, so width-2 SGR bg must ride the
        // primary placement's width_cells and paint via span_rect (2 cells).
        let style = GlyphStyle {
            bg: Some(egui::Color32::from_rgb(40, 0, 0)),
            ..default_style()
        };
        let plan = crate::frame::PaintPlan {
            glyphs: vec![crate::frame::GlyphPlacement {
                row: 0,
                col: 0,
                ch: '中',
                style,
                width_cells: 2,
            }],
            emoji_sites: Vec::new(),
        };
        let m = crate::geom::CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(16.0, 16.0)),
            8.0,
            16.0,
            2,
            1,
        );
        let mut layout = |ch: char, _style: GlyphStyle| dummy_galley_for_tests(ch);
        let (_items, bgs) = mono_paint_items_for_test(&plan, &mut layout);
        assert_eq!(bgs.len(), 1);
        assert_eq!((bgs[0].row, bgs[0].col), (0, 0));
        assert_eq!(bgs[0].width_cells, 2);
        assert_eq!(bgs[0].color, egui::Color32::from_rgb(40, 0, 0));
        let end_col = bgs[0].col + (bgs[0].width_cells as usize).saturating_sub(1);
        let span = m.span_rect(bgs[0].row, bgs[0].col, end_col);
        assert_eq!(span, m.span_rect(0, 0, 1));
        assert_eq!(span.width(), m.cell_rect(0, 0).width() * 2.0);
        // Half-width cell_rect must NOT equal the painted span.
        assert_ne!(span, m.cell_rect(0, 0));
    }

    #[test]
    fn mono_paint_cache_hit_does_zero_layouts() {
        reset_layout_call_count();
        let style = default_style();
        let plan = crate::frame::PaintPlan {
            glyphs: vec![
                crate::frame::GlyphPlacement {
                    row: 0,
                    col: 0,
                    ch: 'x',
                    style,
                    width_cells: 1,
                },
                crate::frame::GlyphPlacement {
                    row: 0,
                    col: 1,
                    ch: 'y',
                    style,
                    width_cells: 1,
                },
            ],
            emoji_sites: Vec::new(),
        };
        let key = MonoPaintKey {
            content_gen: 1,
            off: 0,
            cols: 2,
            rows: 1,
            font_bits: 14.0f32.to_bits(),
        };
        let mut cache = MonoPaintCache::empty();
        let mut layout = |_ch: char, _s: GlyphStyle| {
            note_layout_call();
            dummy_galley_for_tests('x')
        };
        let _ = cache.get_or_rebuild(key, || {
            let (items, bgs) = mono_paint_items(&plan, &mut layout);
            (items, bgs, plan.emoji_sites.clone())
        });
        let first = layout_call_count();
        assert!(first > 0);
        reset_layout_call_count();
        let _ = cache.get_or_rebuild(key, || {
            let (items, bgs) = mono_paint_items(&plan, &mut layout);
            (items, bgs, plan.emoji_sites.clone())
        });
        assert_eq!(layout_call_count(), 0, "cache hit must not layout");
    }

    #[test]
    fn mono_paint_cache_hit_tracks_pane_origin() {
        reset_layout_call_count();
        let style = default_style();
        let plan = crate::frame::PaintPlan {
            glyphs: vec![crate::frame::GlyphPlacement {
                row: 0,
                col: 0,
                ch: 'z',
                style,
                width_cells: 1,
            }],
            emoji_sites: Vec::new(),
        };
        let key = MonoPaintKey {
            content_gen: 1,
            off: 0,
            cols: 1,
            rows: 1,
            font_bits: 14.0f32.to_bits(),
        };
        let m_a = crate::geom::CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(8.0, 16.0)),
            8.0,
            16.0,
            1,
            1,
        );
        let m_b = crate::geom::CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(8.0, 16.0)),
            8.0,
            16.0,
            1,
            1,
        );
        let mut cache = MonoPaintCache::empty();
        let mut layout = |_ch: char, _s: GlyphStyle| {
            note_layout_call();
            dummy_galley_for_tests('z')
        };
        let (items_a, _, _) = cache.get_or_rebuild(key, || {
            let (items, bgs) = mono_paint_items(&plan, &mut layout);
            (items, bgs, plan.emoji_sites.clone())
        });
        assert!(layout_call_count() > 0);
        let pos_a = m_a.cell_rect(items_a[0].row, items_a[0].col).min;
        assert_eq!(pos_a, m_a.cell_rect(0, 0).min);

        reset_layout_call_count();
        let (items_b, _, _) = cache.get_or_rebuild(key, || {
            panic!("cache hit must not rebuild");
        });
        assert_eq!(layout_call_count(), 0, "origin change must not layout");
        let pos_b = m_b.cell_rect(items_b[0].row, items_b[0].col).min;
        assert_eq!(pos_b, m_b.cell_rect(0, 0).min);
        assert_ne!(pos_a, pos_b, "blit pos must track new pane origin");
        // Cached content is grid identity, not absolute pixels.
        assert_eq!((items_b[0].row, items_b[0].col), (0, 0));
    }

    #[test]
    fn emoji_texture_retain_evicts_stale_px() {
        let mut map = std::collections::HashMap::new();
        map.insert(('🥒', 14u32), 1u8);
        map.insert(('🚀', 14u32), 2u8);
        map.insert(('🥒', 20u32), 3u8);
        retain_emoji_textures_at_px(&mut map, 14);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&('🥒', 14)));
        assert!(map.contains_key(&('🚀', 14)));
        assert!(!map.contains_key(&('🥒', 20)));
    }

    #[test]
    fn mono_paint_cache_hit_keeps_emoji_sites_without_rebuild() {
        let sites = vec![crate::frame::EmojiSite {
            row: 0,
            col: 0,
            ch: '🥒',
            width_cells: 2,
        }];
        let plan = crate::frame::PaintPlan {
            glyphs: vec![crate::frame::GlyphPlacement {
                row: 0,
                col: 0,
                ch: '🥒',
                style: default_style(),
                width_cells: 2,
            }],
            emoji_sites: sites.clone(),
        };
        let key = MonoPaintKey {
            content_gen: 1,
            off: 0,
            cols: 2,
            rows: 1,
            font_bits: 14.0f32.to_bits(),
        };
        let mut cache = MonoPaintCache::empty();
        let mut layout = |_ch: char, _s: GlyphStyle| dummy_galley_for_tests('x');
        let (_, _, got) = cache.get_or_rebuild(key, || {
            let (items, bgs) = mono_paint_items(&plan, &mut layout);
            (items, bgs, plan.emoji_sites.clone())
        });
        assert_eq!(*got, sites);
        let (_, _, hit) = cache.get_or_rebuild(key, || {
            panic!("cache hit must not rebuild (sites still available)");
        });
        assert_eq!(*hit, sites);
    }

    #[test]
    fn preferred_powershell_finds_pwsh_in_first_path_dir() {
        let path = std::env::join_paths(["C:\\one", "C:\\two"]).unwrap();
        let got = preferred_powershell(Some(path.as_os_str()), &|p| {
            p == Path::new("C:\\one\\pwsh.exe")
        });
        assert_eq!(got, "pwsh.exe");
    }

    #[test]
    fn preferred_powershell_finds_pwsh_in_later_path_dir() {
        // pwsh only in the SECOND dir: the whole PATH is scanned.
        let path = std::env::join_paths(["C:\\one", "C:\\two"]).unwrap();
        let got = preferred_powershell(Some(path.as_os_str()), &|p| {
            p == Path::new("C:\\two\\pwsh.exe")
        });
        assert_eq!(got, "pwsh.exe");
    }

    #[test]
    fn preferred_powershell_falls_back_when_pwsh_absent() {
        let path = std::env::join_paths(["C:\\one", "C:\\two"]).unwrap();
        assert_eq!(
            preferred_powershell(Some(path.as_os_str()), &|_| false),
            "powershell.exe"
        );
    }

    #[test]
    fn preferred_powershell_falls_back_without_path_var() {
        // No PATH at all: never probe, just fall back.
        assert_eq!(preferred_powershell(None, &|_| true), "powershell.exe");
    }

    #[test]
    fn content_gen_bumps_on_injected_note() {
        // The dispatch banner is written straight to the emulator (NOT the PTY),
        // so it never rides pump(). The galley cache must still invalidate — this
        // is the one grid mutation output_gen deliberately does not cover.
        let ctx = egui::Context::default();
        let mut s = Session::spawn(Shell::PowerShell, None, &[], ctx).expect("spawn");
        let before = s.content_gen;
        s.inject_note("dispatched: test");
        s.resize(40, 10); // first resize flushes the pending note into the grid
        assert!(
            s.content_gen > before,
            "note injection must bump content_gen (before={before}, after={})",
            s.content_gen
        );
    }

    #[test]
    fn listener_answers_color_request_into_the_pty_buffer() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let l = Listener {
            out: out.clone(),
            title: Arc::new(Mutex::new(None)),
        };
        // Stand-in for alacritty's formatter: echo the RGB it is handed.
        let fmt =
            Arc::new(|c: alacritty_terminal::vte::ansi::Rgb| format!("R{}G{}B{}", c.r, c.g, c.b));
        l.send_event(Event::ColorRequest(NamedColor::Background as usize, fmt));
        let got = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert_eq!(got, format!("R{}G{}B{}", BG.r(), BG.g(), BG.b()));
    }

    #[test]
    fn query_color_maps_palette_and_named_slots() {
        // Palette entry 0 → our black; OSC 4;0 callers get it back.
        let p0 = query_color(0);
        assert_eq!(
            (p0.r, p0.g, p0.b),
            (PALETTE[0].r(), PALETTE[0].g(), PALETTE[0].b())
        );
        // OSC 11 (background) → our BG; OSC 10 (foreground) / cursor → our FG.
        let bg = query_color(NamedColor::Background as usize);
        assert_eq!((bg.r, bg.g, bg.b), (BG.r(), BG.g(), BG.b()));
        let fg = query_color(NamedColor::Foreground as usize);
        assert_eq!((fg.r, fg.g, fg.b), (FG.r(), FG.g(), FG.b()));
        let cur = query_color(NamedColor::Cursor as usize);
        assert_eq!((cur.r, cur.g, cur.b), (FG.r(), FG.g(), FG.b()));
    }

    #[test]
    fn glyph_style_plain_is_default_fg_no_bg() {
        let s = glyph_style(
            Flags::empty(),
            named(NamedColor::Foreground),
            named(NamedColor::Background),
        );
        assert_eq!(s.fg, FG);
        assert_eq!(s.bg, None);
        assert!(!s.underline && !s.strikethrough);
    }

    #[test]
    fn glyph_style_reads_underline_and_strikeout_flags() {
        let s = glyph_style(
            Flags::UNDERLINE | Flags::STRIKEOUT,
            named(NamedColor::Foreground),
            named(NamedColor::Background),
        );
        assert!(s.underline, "UNDERLINE flag must set underline");
        assert!(s.strikethrough, "STRIKEOUT flag must set strikethrough");
    }

    #[test]
    fn glyph_style_inverse_swaps_fg_and_bg() {
        // Default fg=FG, bg=None: inverse makes fg the background and bg the old fg.
        let s = glyph_style(
            Flags::INVERSE,
            named(NamedColor::Foreground),
            named(NamedColor::Background),
        );
        assert_eq!(s.fg, BG);
        assert_eq!(s.bg, Some(FG));
    }

    #[test]
    fn glyph_style_dim_darkens_the_foreground() {
        let plain = glyph_style(
            Flags::empty(),
            named(NamedColor::Foreground),
            named(NamedColor::Background),
        );
        let dim = glyph_style(
            Flags::DIM,
            named(NamedColor::Foreground),
            named(NamedColor::Background),
        );
        assert_ne!(dim.fg, plain.fg, "DIM must darken the foreground");
    }

    #[test]
    fn spawn_argv_runs_a_plain_exe() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        // cmd.exe sends a DSR (ESC[6n) at startup waiting for the terminal to reply.
        // pump() reads PTY output and writes back any pending device-status replies;
        // without it cmd.exe hangs before executing /c exit and never exits.
        let mut code = None;
        for _ in 0..750 {
            s.pump(); // answer DSR queries so cmd.exe can proceed to exit
            if let Some(c) = s.exited() {
                code = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(code, Some(0));
    }

    #[test]
    fn spawn_argv_falls_back_to_cmd_for_shims() {
        // npm-style shim: a .cmd file is not directly CreateProcess-able, so a
        // bare Ok here proves the cmd /c fallback path actually ran.
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("fake-agent.cmd");
        std::fs::write(&shim, "@echo shim ran\r\n@exit 0\r\n").unwrap();
        let ctx = egui::Context::default();
        let argv = vec![shim.to_string_lossy().to_string()];
        assert!(Session::spawn_argv(&argv, None, &[], ctx).is_ok());
    }

    #[test]
    fn spawn_argv_refuses_cmd_fallback_for_unsafe_args() {
        // cmd.exe re-parses the whole line on the fallback: a newline ends
        // the command (the remainder would EXECUTE as follow-up commands)
        // and an embedded quote flips its quote state. Refuse loudly rather
        // than silently truncate/inject.
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("fake-agent.cmd");
        std::fs::write(&shim, "@echo shim ran\r\n@exit 0\r\n").unwrap();
        let shim = shim.to_string_lossy().to_string();
        let ctx = egui::Context::default();
        for bad in ["line one\nline two", "cr only\rtail", "say \"hi\""] {
            // Explicit .cmd path: CreateProcess silently wraps it in cmd.exe,
            // so the pre-spawn guard must refuse.
            let argv = vec![shim.clone(), bad.to_string()];
            match Session::spawn_argv(&argv, None, &[], ctx.clone()) {
                Ok(_) => panic!("{bad:?} rode the silent cmd wrap"),
                Err(e) => assert!(e.to_string().contains("cmd-shim"), "{bad:?}: {e}"),
            }
            // Bare name with no native exe: the cmd /c fallback must refuse.
            let argv = vec!["no-such-tool-xyzzy".to_string(), bad.to_string()];
            match Session::spawn_argv(&argv, None, &[], ctx.clone()) {
                Ok(_) => panic!("{bad:?} rode the cmd /c fallback"),
                Err(e) => assert!(e.to_string().contains("cmd-shim"), "{bad:?}: {e}"),
            }
        }
    }

    #[test]
    fn shell_sessions_still_spawn_with_env() {
        let ctx = egui::Context::default();
        let env = [("FOREMAN".to_string(), "1".to_string())];
        assert!(Session::spawn(Shell::Cmd, None, &env, ctx).is_ok());
    }

    fn grid_row(s: &Session, line: i32, cols: usize) -> String {
        let grid = s.term.grid();
        (0..cols)
            .map(|c| {
                let ch = grid[Line(line)][Column(c)].c;
                if ch == '\0' { ' ' } else { ch }
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The banner must survive the first-render shrink. Injecting at the
    /// spawn-time 80-col grid and then resizing narrower reflows the row and
    /// strands its head in scrollback (the real bug: only "…──" was visible).
    /// Contract: the note is deferred to the first resize() and fitted to the
    /// real width, so it renders on row 0, whole, never wrapped.
    #[test]
    fn inject_note_survives_first_render_shrink() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        // 79 chars — the widest banner dispatch_banner() can produce.
        s.inject_note(&format!("── dispatched: {}… ──", "x".repeat(60)));
        s.resize(60, 24); // first render: pane narrower than the banner
        let row0 = grid_row(&s, 0, 60);
        assert!(
            row0.starts_with("── dispatched:"),
            "banner head missing from viewport row 0: {row0:?}"
        );
        assert!(
            row0.ends_with("… ──") && row0.chars().count() <= 60,
            "banner not fitted to width: {row0:?}"
        );
        assert!(
            !grid_row(&s, 1, 60).contains("──"),
            "banner wrapped onto row 1"
        );
        assert_eq!(
            s.term.grid().history_size(),
            0,
            "banner head stranded in scrollback"
        );
    }

    /// Wide (2-column) chars: the fit must count display columns, not chars.
    /// 49 chars of mostly-CJK is ~79 columns — a char-count fit passes it
    /// through unfitted at 60 cols and it wraps, re-stranding the head.
    #[test]
    fn inject_note_fits_wide_chars_by_display_width() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.inject_note(&format!("── dispatched: {}… ──", "汉".repeat(30)));
        s.resize(60, 24);
        let row0 = grid_row(&s, 0, 60);
        assert!(
            row0.starts_with("── dispatched:") && row0.ends_with("… ──"),
            "banner head missing or unfitted on row 0: {row0:?}"
        );
        assert!(
            !grid_row(&s, 1, 60).contains("──"),
            "banner wrapped onto row 1"
        );
        assert_eq!(
            s.term.grid().history_size(),
            0,
            "banner head stranded in scrollback"
        );
    }

    /// Pane that happens to render at exactly the spawn-time 80x24: resize()
    /// early-returns on the no-op size change, but the deferred note must
    /// still flush.
    #[test]
    fn inject_note_flushes_even_when_first_resize_is_a_noop() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.inject_note("── dispatched: claude -p task ──");
        s.resize(80, 24); // same as spawn size
        let row0 = grid_row(&s, 0, 80);
        assert!(
            row0.contains("dispatched: claude -p task"),
            "note not flushed on no-op resize: {row0:?}"
        );
    }

    // paste_wrap / InkScan pure tests live in ready.rs (with ReadyGate).

    #[test]
    fn paste_text_honors_bracketed_paste_and_strips_esc() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "session never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (tx, rx) = std::sync::mpsc::channel();
        s.rx = rx;
        let writes: RecordedWrites = Arc::new(Mutex::new(Vec::new()));
        s.writer = Box::new(TitleRecordingWriter {
            title: Arc::clone(&s.osc_title),
            writes: Arc::clone(&writes),
        });

        // App enables bracketed paste → wrapped, payload ESC stripped.
        tx.send(b"\x1b[?2004h".to_vec()).unwrap();
        s.pump();
        let basis = s.output_gen;
        s.wide_shadow = WideShadowState::Tracking {
            line: Vec::new(),
            col: 0,
            basis_gen: basis,
        };
        let before_empty = writes.lock().unwrap().len();
        s.paste_text("");
        assert_eq!(writes.lock().unwrap().len(), before_empty);
        assert!(matches!(
            &s.wide_shadow,
            WideShadowState::Tracking { basis_gen, .. } if *basis_gen == basis
        ));
        s.psreadline_cup_scanner = Some(CupScanner::default());
        s.psreadline_cursor_alias = Some(CursorAlias {
            raw: Point::new(Line(0), Column(0)),
            physical: Point::new(Line(0), Column(1)),
        });
        s.feed_text("");
        assert!(s.psreadline_cup_scanner.is_some());
        assert!(s.psreadline_cursor_alias.is_some());

        s.paste_text("a\x1b[201~b\nc");
        assert_eq!(
            writes.lock().unwrap().last().unwrap().0,
            b"\x1b[200~a[201~b\nc\x1b[201~"
        );
        assert!(matches!(
            &s.wide_shadow,
            WideShadowState::AwaitingEcho { invalidated_gen } if *invalidated_gen == basis
        ));

        // App disables it → plain bytes, ESC still stripped.
        tx.send(b"\x1b[?2004l".to_vec()).unwrap();
        s.pump();
        s.paste_text("a\x1b[201~b\nc");
        assert_eq!(writes.lock().unwrap().last().unwrap().0, b"a[201~b\nc");

        // Raw external feed follows the same invalidation path.
        let feed_gen = s.output_gen;
        s.wide_shadow = WideShadowState::Tracking {
            line: Vec::new(),
            col: 0,
            basis_gen: feed_gen,
        };
        s.feed(b"x");
        assert!(matches!(
            &s.wide_shadow,
            WideShadowState::AwaitingEcho { invalidated_gen } if *invalidated_gen == feed_gen
        ));
    }

    #[test]
    fn session_latches_ready_after_dsr_is_answered() {
        let ctx = egui::Context::default();
        // `pause` keeps the child alive past the DSR exchange so we can observe
        // the latch (cmd /c exit could race to exit first).
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        assert!(
            !s.ready(),
            "freshly spawned: not ready until DSR is answered"
        );
        // cmd.exe sends ESC[6n at startup; pump() flushes the reply back to the
        // PTY — that first flush is the readiness latch.
        let mut became_ready = false;
        for _ in 0..750 {
            s.pump();
            if s.ready() {
                became_ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            became_ready,
            "session never became ready (DSR never answered)"
        );
    }

    #[test]
    fn pump_flushes_cpr_before_later_rx_chunks_without_advancing_ready() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "session never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (tx, rx) = std::sync::mpsc::channel();
        s.rx = rx;
        let writes: RecordedWrites = Arc::new(Mutex::new(Vec::new()));
        s.writer = Box::new(TitleRecordingWriter {
            title: Arc::clone(&s.osc_title),
            writes: Arc::clone(&writes),
        });
        *s.osc_title.lock().unwrap() = None;
        s.resp.lock().unwrap().clear();
        s.ready_gate.clear_latch();
        s.ready_gate.pending_inject = vec!["held".to_string()];
        s.ready_gate.pending_submit = None;

        tx.send(b"\x1b[5;7H\x1b[6".to_vec()).unwrap();
        tx.send(b"n".to_vec()).unwrap();
        tx.send(b"\x1b]0;LATE\x07".to_vec()).unwrap();
        s.pump();

        let recorded = writes.lock().unwrap().clone();
        assert_eq!(recorded[0].0, b"\x1b[5;7R");
        assert_eq!(
            recorded[0].1, None,
            "later RX was parsed before the blocking CPR was written"
        );
        assert!(s.ready_gate.dsr_replied);
        assert!(!s.ready_gate.painted);
        assert!(!s.ready());
        assert_eq!(s.ready_gate.pending_inject, ["held"]);
        assert_eq!(*s.osc_title.lock().unwrap(), Some("LATE".to_string()));

        tx.send(b"X".to_vec()).unwrap();
        s.pump();

        let recorded = writes.lock().unwrap().clone();
        assert!(s.ready());
        assert!(s.ready_gate.pending_inject.is_empty());
        assert_eq!(recorded[1].0, paste_wrap("held"));
        assert_eq!(recorded[1].1.as_deref(), Some("LATE"));
    }

    #[test]
    fn failed_cpr_write_does_not_latch_readiness() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "session never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (tx, rx) = std::sync::mpsc::channel();
        s.rx = rx;
        s.writer = Box::new(FailingWriter);
        s.resp.lock().unwrap().clear();
        s.ready_gate.clear_latch();
        s.ready_gate.pending_inject = vec!["held".to_string()];

        tx.send(b"X\x1b[6n".to_vec()).unwrap();
        s.pump();

        assert!(s.ready_gate.painted);
        assert!(!s.ready_gate.dsr_replied);
        assert!(!s.ready());
        assert_eq!(s.ready_gate.pending_inject, ["held"]);
    }

    #[test]
    fn inject_before_ready_is_queued_then_flushed() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        assert!(!s.ready(), "freshly spawned: not ready");
        // A post during the startup window must be held, not pasted (a paste now
        // gets eaten by the DSR scan), so no submit is armed yet.
        s.inject_input("hello room");
        assert!(
            s.ready_gate.pending_submit.is_none(),
            "injection before ready must not arm the submit"
        );
        assert!(
            !s.ready_gate.pending_inject.is_empty(),
            "injection before ready must be queued"
        );
        // The pump that latches readiness also flushes the held post.
        let mut flushed = false;
        for _ in 0..750 {
            s.pump();
            if s.ready() && s.ready_gate.pending_inject.is_empty() {
                flushed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(flushed, "queued post never flushed after becoming ready");
        assert!(
            s.ready_gate.pending_submit.is_some(),
            "flushing the queue arms the deferred submit"
        );
    }

    #[test]
    fn ready_waits_for_the_childs_first_paint() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        // The DSR reply alone must not latch readiness: a passthrough ConPTY
        // host answers the startup DSR microseconds after spawn, seconds
        // before the child's input path is open — bytes injected in that
        // window are eaten (the 2026-07-03 chat-delivery regression).
        // Readiness must also require the child's first visible output.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "session never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !s.snapshot_text(None).join("").trim().is_empty(),
            "ready latched before the child painted anything — \
             input injected now would be eaten by the boot window"
        );
    }

    /// Session wiring: graphics replies must not go through the DSR latch path.
    /// The gate only advances on `on_dsr_reply_flushed`; paint alone (or any
    /// write that isn't a resp flush) leaves Ready false.
    #[test]
    fn graphics_reply_path_does_not_latch_ready() {
        let mut g = crate::ready::ReadyGate::new();
        g.on_rx_chunk(b"X"); // paint half
        // Simulate: Session flushed a graphics reply without calling
        // on_dsr_reply_flushed — Ready must stay false.
        assert!(!g.ready());
        assert!(!g.dsr_replied);
        // Only an explicit DSR flush outcome latches the other half.
        g.on_dsr_reply_flushed(true);
        assert!(g.ready());
    }

    /// Submit deferral with injected clock (no real sleep). Pure timing lives
    /// in ready.rs; this pins Session::inject_input + pump_at applying writes.
    #[test]
    fn inject_input_defers_the_submit_keypress() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        for _ in 0..750 {
            s.pump();
            if s.ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(s.ready(), "session never became ready");

        let (tx, rx) = std::sync::mpsc::channel();
        s.rx = rx;
        let writes: RecordedWrites = Arc::new(Mutex::new(Vec::new()));
        s.writer = Box::new(TitleRecordingWriter {
            title: Arc::clone(&s.osc_title),
            writes: Arc::clone(&writes),
        });
        let _ = tx; // no further rx; only write side matters

        let t0 = std::time::Instant::now();
        // Drive inject with a known clock via the gate + pump_at (inject_input
        // uses Instant::now(); pin deadline by overwriting after inject).
        let inject_gen = s.output_gen;
        s.wide_shadow = WideShadowState::Tracking {
            line: Vec::new(),
            col: 0,
            basis_gen: inject_gen,
        };
        s.inject_input("hello");
        assert!(matches!(
            &s.wide_shadow,
            WideShadowState::AwaitingEcho { invalidated_gen }
                if *invalidated_gen == inject_gen
        ));
        assert!(
            s.ready_gate.pending_submit.is_some(),
            "submit must be deferred, not written with the paste"
        );
        let due = s.ready_gate.pending_submit.unwrap();
        // Before deadline: pump must not fire \r.
        s.pump_at(due - std::time::Duration::from_millis(1));
        assert!(s.ready_gate.pending_submit.is_some());
        // Second post refreshes the deadline (accepted merge quirk).
        s.inject_input("world");
        let due2 = s.ready_gate.pending_submit.expect("still armed");
        s.pump_at(due2 - std::time::Duration::from_millis(1));
        assert!(s.ready_gate.pending_submit.is_some());
        let submit_gen = s.output_gen;
        s.wide_shadow = WideShadowState::Tracking {
            line: Vec::new(),
            col: 0,
            basis_gen: submit_gen,
        };
        s.pump_at(due2 + std::time::Duration::from_millis(1));
        assert!(
            s.ready_gate.pending_submit.is_none(),
            "a pump past the deadline fires the submit exactly once"
        );
        assert!(matches!(
            &s.wide_shadow,
            WideShadowState::AwaitingEcho { invalidated_gen }
                if *invalidated_gen == submit_gen
        ));
        let recorded = writes.lock().unwrap().clone();
        assert!(
            recorded.iter().any(|(b, _)| b == b"\r"),
            "deferred submit must write CR; got {recorded:?}"
        );
        let _ = t0;
    }

    #[test]
    fn inject_input_reaches_child_stdin() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        // `cmd /c pause` blocks until any key arrives on stdin. If the injected
        // bytes reach the child, pause consumes one and the process exits.
        // Wait until pause's prompt has rendered — proof the startup DSR
        // exchange resolved and the child is now blocked reading stdin
        // (bytes injected before that get eaten by the DSR scan).
        let ready = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            s.pump();
            if !grid_row(&s, 0, 80).trim().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < ready,
                "pause prompt never rendered"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        s.inject_input("hello room");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while s.exited().is_none() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "pause never saw the injected input"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn advance_scanned_places_at_the_cursor_where_the_command_completed() {
        use base64::Engine as _;
        let mut term = term_with(b"", 40, 10);
        let mut parser: Processor = Processor::new();
        let mut g = crate::graphics::Graphics::default();
        let mut replies = Vec::new();

        let rgba = [255u8, 0, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
        // Move to row 5, col 10 (1-based CUP), then place, then keep printing —
        // all in ONE chunk. The placement must sample the moved cursor, and the
        // trailing text must land where the app expects (grid untouched by APC).
        let bytes =
            format!("AB\x1b[5;10H\x1b_Ga=T,t=d,f=32,s=1,v=1,c=2,r=1,q=2,i=3;{b64}\x1b\\tail");
        advance_scanned(
            &mut parser,
            &mut term,
            &mut g,
            bytes.as_bytes(),
            &mut replies,
        );

        assert!(replies.is_empty());
        let vp = crate::graphics::ViewportView {
            alt_screen: false,
            history_size: 0,
            display_offset: 0,
            screen_lines: 10,
        };
        let vis = g.visible(&vp);
        assert_eq!(vis.len(), 1);
        assert_eq!((vis[0].col, vis[0].line), (9, 4)); // CUP 5;10 is 0-based (4,9)

        // grid_row(&Session, ..) doesn't apply here — this test drives a bare
        // Term<VoidListener> (the sanctioned pure-parse pattern), not a Session.
        let row = |line: i32| -> String {
            (0..40)
                .map(|c| {
                    let ch = term.grid()[Line(line)][Column(c)].c;
                    if ch == '\0' { ' ' } else { ch }
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert!(row(0).starts_with("AB"));
        assert!(row(4).contains("tail")); // vte ignored the APC
    }

    fn scan_psreadline_cup(bytes: &[u8], cols: usize) -> (Term<VoidListener>, CupScanResult) {
        let mut term = term_with(b"", cols, 8);
        let mut parser = Processor::new();
        let mut graphics = crate::graphics::Graphics::default();
        let mut scanner = CupScanner::default();
        let mut replies = Vec::new();
        let result = advance_psreadline_scanned(
            &mut parser,
            &mut term,
            &mut graphics,
            &mut scanner,
            bytes,
            &mut replies,
            None,
        );
        assert!(replies.is_empty());
        (term, result)
    }

    #[test]
    fn psreadline_cup_aliases_non_bmp_wrap_to_the_natural_flow_end() {
        // Seven columns: six ASCII cells leave one at the margin. PSReadLine
        // treats the surrogate halves as width 1 + 1 and emits CUP 2;2, while
        // alacritty defers the whole emoji and naturally ends at 2;3.
        let (term, result) = scan_psreadline_cup("abcdef🤣\x1b[2;2H".as_bytes(), 7);
        let alias = result.alias.expect("PSReadLine CUP should be aliased");
        assert!(result.saw_cup);
        assert_eq!(alias.raw, Point::new(Line(1), Column(1)));
        assert_eq!(alias.physical, Point::new(Line(1), Column(2)));
        assert_eq!(
            term.grid().cursor.point,
            alias.raw,
            "grid stays authoritative"
        );

        let (_, raw_idx) = crate::inspect::wide_row_at_cursor(&term);
        let (_, effective_idx) = crate::inspect::wide_row_at_point(&term, alias.physical);
        assert_eq!(raw_idx, 7, "raw CUP parks inside the emoji spacer");
        assert_eq!(effective_idx, 8, "alias parks after the complete emoji");
    }

    #[test]
    fn psreadline_cup_alias_accumulates_every_wrap_pad_in_a_long_line() {
        // Three straddled boundaries. The raw split-wide CUP is three cells
        // behind alacritty's natural whole-glyph endpoint.
        let bytes = "abcdef🤣abcd🤣abcd🤣\x1b[3;7H";
        let (term, result) = scan_psreadline_cup(bytes.as_bytes(), 7);
        let alias = result.alias.expect("all leading pads should reconcile");
        assert_eq!(alias.raw, Point::new(Line(2), Column(6)));
        assert_eq!(alias.physical, Point::new(Line(3), Column(2)));
        assert_eq!(term.grid().cursor.point, alias.raw);
    }

    #[test]
    fn psreadline_cup_does_not_alias_a_correct_whole_glyph_position() {
        let (term, result) = scan_psreadline_cup("abcdef🤣\x1b[2;3H".as_bytes(), 7);
        assert!(result.saw_cup);
        assert!(result.alias.is_none(), "correct CUP must remain untouched");
        assert_eq!(term.grid().cursor.point, Point::new(Line(1), Column(2)));

        // BMP CJK is one UTF-16 char of width 2, so PSReadLine already defers it
        // whole and produces the same correct position as alacritty.
        let (term, result) = scan_psreadline_cup("abcdef中\x1b[2;3H".as_bytes(), 7);
        assert!(result.alias.is_none());
        assert_eq!(term.grid().cursor.point, Point::new(Line(1), Column(2)));
    }

    #[test]
    fn psreadline_cup_counts_only_non_bmp_pads_on_a_mixed_wrapped_line() {
        // Both glyphs hit a one-cell margin. PSReadLine correctly includes the
        // CJK pad but omits the emoji pad, so the final CUP is behind by one,
        // not by the two LEADING spacers present in alacritty's grid.
        let bytes = "abcdef中abcd🤣\x1b[3;2H";
        let (term, result) = scan_psreadline_cup(bytes.as_bytes(), 7);
        let alias = result
            .alias
            .expect("non-BMP pad should reconcile beside CJK");
        assert_eq!(alias.raw, Point::new(Line(2), Column(1)));
        assert_eq!(alias.physical, Point::new(Line(2), Column(2)));
        assert_eq!(term.grid().cursor.point, alias.raw);
    }

    #[test]
    fn psreadline_cup_scanner_handles_a_sequence_split_between_pty_chunks() {
        let mut term = term_with(b"", 7, 4);
        let mut parser = Processor::new();
        let mut graphics = crate::graphics::Graphics::default();
        let mut scanner = CupScanner::default();
        let mut replies = Vec::new();
        let first = advance_psreadline_scanned(
            &mut parser,
            &mut term,
            &mut graphics,
            &mut scanner,
            "abcdef🤣\x1b[2;2".as_bytes(),
            &mut replies,
            None,
        );
        assert!(!first.saw_cup);
        let second = advance_psreadline_scanned(
            &mut parser,
            &mut term,
            &mut graphics,
            &mut scanner,
            b"H",
            &mut replies,
            first.alias,
        );
        assert_eq!(
            second.alias,
            Some(CursorAlias {
                raw: Point::new(Line(1), Column(1)),
                physical: Point::new(Line(1), Column(2)),
            })
        );
    }

    #[test]
    fn psreadline_paste_gate_requires_the_cursor_at_the_visible_line_end() {
        let term = term_with(b"PS> ", 20, 2);
        assert!(cursor_at_content_end(&term, term.grid().cursor.point));

        // Inline prediction or a mid-line paste leaves nonblank cells after the
        // cursor and must not arm the compatibility scanner.
        let term = term_with(b"PS> suggestion\x1b[1;5H", 20, 2);
        assert_eq!(term.grid().cursor.point, Point::new(Line(0), Column(4)));
        assert!(!cursor_at_content_end(&term, term.grid().cursor.point));
    }

    #[test]
    fn psreadline_alias_is_dropped_when_output_moves_after_the_cup() {
        let (_, result) = scan_psreadline_cup("abcdef🤣\x1b[2;2HX".as_bytes(), 7);
        assert!(result.saw_cup);
        assert!(
            result.alias.is_none(),
            "the alias applies only while the raw CUP remains the resting cursor"
        );
    }

    #[test]
    fn psreadline_alias_survives_a_redundant_cup_to_the_same_raw_point() {
        let (_, result) = scan_psreadline_cup("abcdef🤣\x1b[2;2H\x1b[2;2H".as_bytes(), 7);
        assert_eq!(
            result.alias,
            Some(CursorAlias {
                raw: Point::new(Line(1), Column(1)),
                physical: Point::new(Line(1), Column(2)),
            })
        );
    }

    #[test]
    fn psreadline_alias_drops_on_grid_mutation_even_if_cursor_returns() {
        let (term, result) = scan_psreadline_cup("abcdef🤣\x1b[2;2HX\x08".as_bytes(), 7);
        assert_eq!(term.grid().cursor.point, Point::new(Line(1), Column(1)));
        assert!(
            result.alias.is_none(),
            "print + BS must invalidate the alias"
        );

        let (term, result) = scan_psreadline_cup("abcdef🤣\x1b[2;2H\x1b[2J".as_bytes(), 7);
        assert_eq!(term.grid().cursor.point, Point::new(Line(1), Column(1)));
        assert!(result.alias.is_none(), "ED must invalidate the alias");
    }

    #[test]
    fn psreadline_alias_drops_on_alt_screen_enter_and_exit_in_one_chunk() {
        let (_, result) = scan_psreadline_cup(
            "abcdef🤣\x1b[2;2H\x1b[?1049h\x1b[2;2H\x1b[?1049l".as_bytes(),
            7,
        );
        assert!(result.hit_mode_barrier);
        assert!(result.alias.is_none());
    }

    #[test]
    fn psreadline_alias_drops_on_bracketed_paste_enter_and_exit_in_one_chunk() {
        let (_, result) = scan_psreadline_cup(
            "abcdef🤣\x1b[2;2H\x1b[?2004h\x1b[2;2H\x1b[?2004l".as_bytes(),
            7,
        );
        assert!(result.hit_mode_barrier);
        assert!(result.alias.is_none());
    }

    #[test]
    fn paste_alias_must_be_observed_after_the_input_that_invalidated_shadow() {
        // The paste's final CUP (gen 1) is newer than the paste write (gen 0),
        // so it is safe to seed the first immediate wide-key event.
        assert!(generation_after(1, 0));

        // Harmless later output may advance the session to gen 2 while carrying
        // that same alias. Text entered at gen 2 invalidates the shadow; the old
        // gen-1 alias must not be reused before a new CUP observation.
        assert!(!generation_after(1, 2));
        assert!(generation_after(3, 2));

        // Preserve serial ordering when the process-lifetime counter wraps.
        assert!(generation_after(0, u64::MAX));
        assert!(!generation_after(u64::MAX, 0));
    }

    #[test]
    fn session_rejects_a_carried_paste_alias_after_later_input_invalidation() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut session = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        session.resize(7, 8);
        session.shell = Shell::PowerShell;

        // Replace the live reader with a deterministic PTY-output seam. The
        // crafted stream establishes the same one-cell emoji alias as the live
        // PSReadLine fixture.
        let (tx, rx) = std::sync::mpsc::channel();
        session.rx = rx;
        session.psreadline_cup_scanner = Some(CupScanner::default());
        let invalidated_before_paste = session.output_gen;
        tx.send("\x1b[2J\x1b[Habcdef🤣\x1b[2;2H".as_bytes().to_vec())
            .unwrap();
        session.pump();

        let alias_gen = session
            .psreadline_cursor_alias_gen
            .expect("candidate CUP must stamp its PTY generation");
        assert!(session.has_fresh_cursor_alias(invalidated_before_paste));
        assert_eq!(alias_gen, session.output_gen);

        // SGR is deliberately non-mutating, so it carries the alias while the
        // session generation advances. Later input invalidates at that newer
        // generation; the carried pre-input alias must no longer bypass settle.
        tx.send(b"\x1b[31m".to_vec()).unwrap();
        session.pump();
        assert_eq!(session.psreadline_cursor_alias_gen, Some(alias_gen));
        assert!(session.has_valid_cursor_alias());
        session.invalidate_wide_shadow();
        let invalidated_after_input = session.output_gen;
        assert!(matches!(
            session.wide_shadow,
            WideShadowState::AwaitingEcho { invalidated_gen }
                if invalidated_gen == invalidated_after_input
        ));
        assert!(!session.has_fresh_cursor_alias(invalidated_after_input));
    }

    /// The codex pet frame rides INSIDE a synchronized update (?2026h..?2026l):
    /// BSU, delete, save, CUP(pet spot), chunked transmit, restore, ESU — with
    /// the cursor last parked at the composer caret by the PREVIOUS block. The
    /// vte parser buffers all sync-block bytes, so a naive cursor sample at the
    /// cut reads the stale caret; the anchor must be the in-block pet CUP.
    /// Distilled from a real codex 0.142.5 rx capture (codex_pet_rx_capture).
    #[test]
    fn sync_update_frame_anchors_at_the_pet_cup_not_the_stale_caret() {
        use base64::Engine as _;
        let mut term = term_with(b"", 80, 24);
        let mut parser: Processor = Processor::new();
        let mut g = crate::graphics::Graphics::default();
        let mut replies = Vec::new();

        let rgba = [255u8, 0, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
        let bytes = format!(
            "\x1b[14;3H\x1b[?2026l\
             \x1b[?2026h\
             \x1b_Ga=d,d=I,i=9,q=2\x1b\\\
             \x1b7\x1b[11;72H\
             \x1b_Ga=T,t=d,f=32,s=1,v=1,q=2,i=9,m=1;{b64}\x1b\\\
             \x1b_Gm=0;\x1b\\\
             \x1b8\x1b[?2026l"
        );
        advance_scanned(
            &mut parser,
            &mut term,
            &mut g,
            bytes.as_bytes(),
            &mut replies,
        );

        let vp = crate::graphics::ViewportView {
            alt_screen: false,
            history_size: 0,
            display_offset: 0,
            screen_lines: 24,
        };
        let vis = g.visible(&vp);
        assert_eq!(vis.len(), 1);
        // CUP 11;72 is 0-based (10, 71); the stale caret would be (13, 2).
        assert_eq!((vis[0].line, vis[0].col), (10, 71));
    }

    #[test]
    fn exit_is_noted_exactly_once() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let mut noted = None;
        for _ in 0..750 {
            s.pump();
            if let Some(c) = s.exit_to_note() {
                noted = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(noted, Some(0));
        assert_eq!(s.exit_to_note(), None); // second note must not fire
        assert_eq!(s.exited(), Some(0)); // plain exited() still reports
    }

    /// Closing a pane / dropping a Session must take the child process with
    /// it — an interactive shell never exits on its own when the PTY closes,
    /// and used to pile up as orphans (2,000+ found from test runs alone).
    #[test]
    fn dropping_a_session_kills_its_child() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string()]; // interactive — never exits by itself
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let pid = s.root_pid.expect("no root pid");
        let watch = crate::job::DeathWatch::open(pid).expect("cannot watch child");
        for _ in 0..10 {
            s.pump();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.exited().is_none(), "shell died prematurely");
        drop(s);
        assert!(
            watch.dead_within_ms(5000),
            "child cmd.exe survived Session drop"
        );
    }

    /// Diagnostic canary, machine-dependent: does THIS Windows' ConPTY pass a
    /// kitty graphics APC (`ESC _ G … ESC \`) through to the hosting terminal?
    /// The in-process pipeline is covered by pure tests; this exercises the OS
    /// layer in between. Run manually: cargo test --release conpty_passes -- --ignored
    #[test]
    #[ignore = "diagnostic: result depends on the OS conhost version"]
    fn conpty_passes_kitty_apc_through() {
        let ctx = egui::Context::default();
        let cmd = "Start-Sleep -Milliseconds 250; Write-Host MARKER; \
                   Write-Host ([char]27 + '_Ga=T,t=d,f=32,s=1,v=1,q=2,i=9;' \
                   + [Convert]::ToBase64String([byte[]](255,0,0,255)) + [char]27 + '\\'); \
                   $sw = [Diagnostics.Stopwatch]::StartNew(); \
                   $p = $Host.UI.RawUI.CursorPosition; \
                   $sw.Stop(); \
                   Write-Host ('AFTER_GCSBI {0},{1} ELAPSED_MS={2}' -f $p.X,$p.Y,$sw.ElapsedMilliseconds)";
        let argv = vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            cmd.to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let dump = std::env::temp_dir().join("foreman-conpty-apc-cpr.bin");
        let _ = std::fs::remove_file(&dump);
        s.rx_dump = std::fs::File::create(&dump).ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while s.exited().is_none() {
            s.pump();
            assert!(std::time::Instant::now() < deadline, "child never exited");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        s.pump(); // drain anything buffered after exit
        let screen: String = (0..24).map(|r| grid_row(&s, r, 80)).collect();
        assert!(
            screen.contains("MARKER"),
            "child produced no output at all — the ConPTY host failed to run, \
             so this proves nothing about APC passthrough"
        );
        assert!(
            s.graphics.has_image(9),
            "ConPTY stripped the kitty APC before it reached the terminal"
        );
        assert!(
            screen.contains("AFTER_GCSBI"),
            "screen-buffer query never resumed after the kitty APC"
        );
        let trace = std::fs::read(&dump).unwrap_or_default();
        let trace_text = String::from_utf8_lossy(&trace);
        let latency_ms: u64 = trace_text
            .split("ELAPSED_MS=")
            .nth(1)
            .and_then(|tail| {
                tail.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .expect("child did not report the post-APC query duration");
        assert!(
            latency_ms < 400,
            "post-APC screen-buffer query took {latency_ms}ms; likely hit ConPTY's 500ms CPR timeout"
        );
        let apc = trace
            .windows(3)
            .position(|window| window == b"\x1b_G")
            .expect("kitty APC missing from raw PTY trace");
        assert!(
            trace[apc..].windows(4).any(|window| window == b"\x1b[6n"),
            "ConPTY did not request a cursor report after the unknown kitty APC"
        );
        assert!(
            trace.windows(11).any(|window| window == b"<<PTY_REPLY"),
            "Foreman did not send the requested CPR"
        );
        assert!(
            !trace.windows(15).any(|window| window == b"<<PTY_REPLY_ERR"),
            "Foreman generated but failed to write the CPR"
        );
        println!("post-APC GCSBI completed in {latency_ms}ms");
    }

    /// Diagnostic canary #2, machine-dependent: does ConPTY preserve a cursor
    /// move that immediately precedes a kitty APC — (a) as a bare CUP, and
    /// (b) wrapped in codex's DECSC/CUP/APC/DECRC sandwich (net-zero cursor
    /// delta, which a diffing renderer may optimize away)? Prints the anchor
    /// each style produced; the CUP target is row 2, col 9 (0-based).
    /// Run manually: cargo test --release conpty_preserves -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic: result depends on the OS conhost version"]
    fn conpty_preserves_cursor_moves_for_apc() {
        for (style, seq) in [
            ("bare CUP", "[char]27 + '[3;10H' + $img"),
            (
                "DECSC sandwich",
                "[char]27 + '7' + [char]27 + '[3;10H' + $img + [char]27 + '8'",
            ),
        ] {
            let ctx = egui::Context::default();
            let cmd = format!(
                "$img = [char]27 + '_Ga=T,t=d,f=32,s=1,v=1,q=2,i=9;' \
                 + [Convert]::ToBase64String([byte[]](255,0,0,255)) + [char]27 + '\\'; \
                 Write-Host MARKER; [Console]::Write({seq}); Start-Sleep -Milliseconds 300"
            );
            let argv = vec![
                "powershell.exe".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                cmd,
            ];
            let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while s.exited().is_none() {
                s.pump();
                assert!(std::time::Instant::now() < deadline, "child never exited");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            s.pump();
            let screen: String = (0..24).map(|r| grid_row(&s, r, 80)).collect();
            assert!(
                screen.contains("MARKER"),
                "{style}: child produced no output"
            );
            let vp = crate::graphics::ViewportView {
                alt_screen: false,
                history_size: s.term.grid().history_size(),
                display_offset: 0,
                screen_lines: s.term.grid().screen_lines(),
            };
            let vis = s.graphics.visible(&vp);
            assert_eq!(vis.len(), 1, "{style}: image never arrived");
            println!(
                "{style}: anchored at (line {}, col {}) — CUP target was (2, 9)",
                vis[0].line, vis[0].col
            );
        }
    }

    fn resize_probe_scenario(label: &str, from: (usize, usize), to: (usize, usize)) {
        resize_probe_scenario_with(label, from, to, "1..40 | % { \"line $_\" }\r");
    }

    fn resize_probe_scenario_with(
        label: &str,
        from: (usize, usize),
        to: (usize, usize),
        fill: &str,
    ) {
        fn pump_for(s: &mut Session, ms: u64) {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                s.pump();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        fn esc(bytes: &[u8]) -> String {
            bytes
                .iter()
                .map(|&b| match b {
                    0x1b => "␛".to_string(),
                    b'\r' => "\\r".to_string(),
                    b'\n' => "\\n\n".to_string(),
                    0x20..=0x7e => (b as char).to_string(),
                    _ => format!("\\x{b:02x}"),
                })
                .collect()
        }
        fn dump_state(s: &mut Session, label: &str) {
            let cur = s.cursor_info();
            println!("=== {label}: cursor row={} col={} ===", cur.row, cur.col);
            for (i, row) in s.snapshot_text(None).iter().enumerate() {
                if !row.is_empty() {
                    println!("{i:3} |{row}");
                }
            }
        }

        let dump = std::env::temp_dir().join(format!("foreman_resize_probe_{label}.bin"));
        let _ = std::fs::remove_file(&dump);
        let ctx = egui::Context::default();
        let shell = std::env::var("FOREMAN_PROBE_SHELL").unwrap_or("powershell.exe".into());
        let argv = vec![shell, "-NoProfile".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.rx_dump = std::fs::File::create(&dump).ok();
        s.resize(from.0, from.1);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "shell never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        pump_for(&mut s, 1000); // let PSReadLine finish its startup render

        s.send(fill.as_bytes());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.snapshot_text(None).iter().any(|r| r.contains("line 40")) {
            assert!(std::time::Instant::now() < deadline, "output never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        pump_for(&mut s, 500);
        let len = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as usize;
        dump_state(&mut s, &format!("[{label}] before resize {from:?}"));
        let mark0 = len(&dump);

        s.resize(to.0, to.1);
        pump_for(&mut s, 1500);
        dump_state(&mut s, &format!("[{label}] after resize to {to:?}"));
        let mark1 = len(&dump);

        s.send(b"sdfs");
        pump_for(&mut s, 1500);
        dump_state(&mut s, &format!("[{label}] after typing sdfs"));

        let bytes = std::fs::read(&dump).unwrap_or_default();
        println!(
            "=== [{label}] raw ConPTY bytes: resize repaint ===\n{}",
            esc(&bytes[mark0.min(bytes.len())..mark1.min(bytes.len())])
        );
        println!(
            "=== [{label}] raw ConPTY bytes: typed echo ===\n{}",
            esc(&bytes[mark1.min(bytes.len())..])
        );
    }

    /// Simulate a mouse drag: step the width down 100→60 and back up at frame
    /// cadence (one Session::resize per ~16ms with pumping between), over
    /// wide `ls`-like rows that wrap when narrow. Then type and report where
    /// the echo lands relative to the prompt.
    fn drag_probe_scenario(label: &str, rows: usize) {
        fn pump_for(s: &mut Session, ms: u64) {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                s.pump();
                std::thread::sleep(std::time::Duration::from_millis(4));
            }
        }
        let ctx = egui::Context::default();
        let shell = std::env::var("FOREMAN_PROBE_SHELL").unwrap_or("powershell.exe".into());
        let argv = vec![shell, "-NoProfile".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.resize(100, rows);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "shell never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        pump_for(&mut s, 1000);
        s.send(b"1..40 | % { \"line $_ \" + (\"x\" * 80) }\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.snapshot_text(None).iter().any(|r| r.contains("line 40")) {
            assert!(std::time::Instant::now() < deadline, "output never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        pump_for(&mut s, 500);

        for w in (60..=100).rev().chain(60..=100).step_by(1) {
            s.resize(w, rows);
            pump_for(&mut s, 16);
        }
        pump_for(&mut s, 1500);

        s.send(b"sdfs");
        pump_for(&mut s, 1500);
        let rows_txt = s.snapshot_text(None);
        let cur = s.cursor_info();
        let echo = rows_txt.iter().position(|r| r.contains("sdfs"));
        let prompt = rows_txt.iter().rposition(|r| r.contains('>'));
        println!(
            "=== [{label}] after drag+typing: cursor row={} col={} echo_row={echo:?} prompt_row={prompt:?} ===",
            cur.row, cur.col
        );
        for (i, row) in rows_txt.iter().enumerate() {
            if !row.is_empty() {
                println!("{i:3} |{row}");
            }
        }
    }

    /// Reproduce the original width-shrink + Up-history failure. The pending
    /// input is deliberately wrapped before the resize; Up then makes
    /// PSReadLine query the console cursor and repaint the previous command.
    fn recall_probe_scenario(label: &str) {
        fn pump_for(s: &mut Session, ms: u64) {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                s.pump();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        fn esc(bytes: &[u8]) -> String {
            bytes
                .iter()
                .map(|&b| match b {
                    0x1b => "<ESC>".to_string(),
                    b'\r' => "\\r".to_string(),
                    b'\n' => "\\n\n".to_string(),
                    0x20..=0x7e => (b as char).to_string(),
                    _ => format!("\\x{b:02x}"),
                })
                .collect()
        }

        let dump = std::env::temp_dir().join(format!("foreman_resize_probe_{label}.bin"));
        let _ = std::fs::remove_file(&dump);
        let ctx = egui::Context::default();
        let argv = vec!["powershell.exe".to_string(), "-NoProfile".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.rx_dump = std::fs::File::create(&dump).ok();
        s.resize(100, 30);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "shell never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        pump_for(&mut s, 500);
        s.send(b"1..12 | % { \"line $_\" }\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.snapshot_text(None).iter().any(|r| r.contains("line 12")) {
            assert!(std::time::Instant::now() < deadline, "output never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        pump_for(&mut s, 500);

        let pending = format!("PENDING_{}", "z".repeat(80));
        s.send(pending.as_bytes());
        pump_for(&mut s, 500);
        let mark = std::fs::metadata(&dump).map(|m| m.len()).unwrap_or(0) as usize;

        s.resize(60, 30);
        pump_for(&mut s, 500);
        s.send(b"\x1b[A");
        pump_for(&mut s, 1500);

        let rows = s.snapshot_text(None);
        let cur = s.cursor_info();
        println!(
            "=== [{label}] after shrink+Up: cursor row={} col={} recalled_rows={:?} pending_rows={:?} ===",
            cur.row,
            cur.col,
            rows.iter()
                .enumerate()
                .filter_map(|(i, row)| row.contains("1..12").then_some(i))
                .collect::<Vec<_>>(),
            rows.iter()
                .enumerate()
                .filter_map(|(i, row)| row.contains("PENDING_").then_some(i))
                .collect::<Vec<_>>(),
        );
        for (i, row) in rows.iter().enumerate() {
            if !row.is_empty() {
                println!("{i:3} |{row}");
            }
        }
        let bytes = std::fs::read(&dump).unwrap_or_default();
        let phase = &bytes[mark.min(bytes.len())..];
        println!(
            "=== [{label}] raw ConPTY bytes + host replies: resize through Up ===\n{}",
            esc(phase)
        );
        println!(
            "=== [{label}] post-resize DSR count={} host CPR markers={} ===",
            phase.windows(4).filter(|w| *w == b"\x1b[6n").count(),
            phase.windows(11).filter(|w| *w == b"<<PTY_REPLY").count(),
        );
    }

    /// Diagnostic: does the bundled ConPTY request and consume a host cursor
    /// report before PSReadLine recalls history after a width shrink?
    /// Run: cargo test --release resize_recall_probe -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic: result depends on the bundled ConPTY version"]
    fn resize_recall_probe() {
        recall_probe_scenario("recall-after-shrink");
    }

    /// Diagnostic: after width overflow and recovery, does plain input land on
    /// the visible prompt? Run with the candidate pair beside the test exe.
    /// Run: cargo test --release resize_drag_probe -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic: result depends on the bundled ConPTY version"]
    fn resize_drag_probe() {
        drag_probe_scenario("drag-narrow-widen", 30);
    }

    /// Diagnostic, machine-dependent: after a pane resize, does plain typed
    /// input echo at the prompt or stranded mid-screen? Prints grid + cursor +
    /// raw ConPTY bytes for each phase so the divergence is attributable
    /// (foreman/alacritty grid state vs what ConPTY actually emitted).
    /// Run: cargo test --release resize_typing_probe -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic: result depends on the OS ConPTY version"]
    fn resize_typing_probe() {
        resize_probe_scenario("grow", (100, 30), (80, 45));
        resize_probe_scenario("shrink", (100, 45), (100, 30));
        // Wide rows that wrap when the pane narrows — the reflow divergence.
        resize_probe_scenario_with(
            "wrap-narrow",
            (100, 30),
            (80, 30),
            "1..40 | % { \"line $_ \" + (\"x\" * 90) }\r",
        );
        // ...and widening back out after the wrap.
        resize_probe_scenario_with(
            "wrap-widen",
            (80, 30),
            (100, 30),
            "1..40 | % { \"line $_ \" + (\"x\" * 70) }\r",
        );
    }

    /// ConPTY anchors its layout on a height grow (zero bytes emitted, blank
    /// rows appear below) and keeps addressing the cursor by the same absolute
    /// rows. So growing must NOT pull scrollback back in — content and cursor
    /// stay put, or every subsequent absolute repaint from the child lands
    /// mid-scrollback. See docs/conpty-resize-reflow.md, second manifestation.
    #[test]
    fn height_grow_anchors_content_instead_of_pulling_scrollback() {
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("line {i}\r\n"));
        }
        feed.push_str("PS>");
        let mut term = term_with(feed.as_bytes(), 80, 30);
        assert_eq!(
            term.grid().cursor.point.line.0,
            29,
            "setup: prompt on the last row"
        );

        resize_anchored(&mut term, Size { cols: 80, rows: 45 });

        let rows = crate::inspect::snapshot_text(&term, None);
        assert_eq!(
            term.grid().cursor.point.line.0,
            29,
            "cursor must stay on ConPTY's row"
        );
        assert_eq!(rows[29], "PS>", "prompt row must stay anchored");
        assert!(
            rows[30..].iter().all(|r| r.is_empty()),
            "the new rows appear blank below, as in ConPTY's layout"
        );
        // The lines alacritty pulled in must go back to scrollback, not vanish.
        term.scroll_display(Scroll::Top);
        let scrolled = crate::inspect::snapshot_text(&term, None);
        assert!(
            scrolled.iter().any(|r| r == "line 1"),
            "history intact after re-anchoring"
        );
    }

    /// A live selection must keep pointing at the same content when a height
    /// grow is re-anchored: `Term::resize` rotates the selection to track the
    /// history pull, so cancelling the pull must rotate it back.
    #[test]
    fn height_grow_keeps_selection_on_its_content() {
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("line {i}\r\n"));
        }
        feed.push_str("PS>");
        let mut term = term_with(feed.as_bytes(), 80, 30);
        // History holds lines 1-11, so viewport row 5 shows "line 17".
        select(
            &mut term,
            SelectionType::Simple,
            (5, 0, Side::Left),
            Some((5, 6, Side::Right)),
        );
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("line 17"),
            "setup: selection covers line 17"
        );

        resize_anchored(&mut term, Size { cols: 80, rows: 45 });

        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("line 17"),
            "selection must stick to its content across a re-anchored grow"
        );
    }

    /// Regression, machine-dependent: grow a pane taller while scrollback
    /// exists, then type. The echo and the caret must land on the prompt row,
    /// not rows above it inside old output (the height-grow divergence in
    /// docs/conpty-resize-reflow.md). Run manually:
    /// cargo test --release typed_echo_lands -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic: result depends on the OS ConPTY version"]
    fn typed_echo_lands_on_the_prompt_after_a_height_grow() {
        let ctx = egui::Context::default();
        let argv = vec!["powershell.exe".to_string(), "-NoProfile".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.resize(100, 30);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.ready() {
            s.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "shell never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        s.send(b"1..40 | % { \"line $_\" }\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !s.snapshot_text(None).iter().any(|r| r.contains("line 40")) {
            assert!(std::time::Instant::now() < deadline, "output never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Settle, then grow taller only — isolates the height policy from the
        // separately documented (and unfixable-cheap) width reflow divergence.
        std::thread::sleep(std::time::Duration::from_millis(300));
        s.pump();
        s.resize(100, 45);
        std::thread::sleep(std::time::Duration::from_millis(500));
        s.pump();
        s.send(b"sdfs");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.snapshot_text(None).iter().any(|r| r.contains("sdfs")) {
            assert!(std::time::Instant::now() < deadline, "echo never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let rows = s.snapshot_text(None);
        let echo = rows.iter().position(|r| r.contains("sdfs")).unwrap();
        let prompt = rows.iter().rposition(|r| r.contains('>')).unwrap();
        assert_eq!(
            echo,
            prompt,
            "typed echo stranded {} rows above the prompt:\n{rows:#?}",
            prompt.saturating_sub(echo)
        );
        let cur = s.cursor_info();
        assert_eq!(
            cur.row as usize, prompt,
            "caret stranded off the prompt row"
        );
    }

    /// Diagnostic capture: run the real codex TUI headlessly (pets enabled via
    /// ~/.codex/config.toml) and dump the raw post-ConPTY byte stream so pet
    /// frames can be inspected offline: where do conhost's cursor moves sit
    /// relative to the kitty chunk chain? No prompt is submitted — ambient pet
    /// frames render on idle, so this consumes zero codex usage.
    /// Run: cargo test --release codex_pet_rx_capture -- --ignored --nocapture
    /// (needs conpty.dll + OpenConsole.exe beside the test exe in deps/).
    #[test]
    #[ignore = "diagnostic: drives a real codex TUI"]
    fn codex_pet_rx_capture() {
        let dump = std::env::temp_dir().join("foreman-codex-rx.bin");
        let _ = std::fs::remove_file(&dump);
        // Safety: set before the Session exists, removed right after spawn;
        // this #[ignore]d diagnostic runs alone.
        unsafe { std::env::set_var("FOREMAN_RX_DUMP", &dump) };
        let ctx = egui::Context::default();
        let env = [("KITTY_WINDOW_ID".to_string(), "1".to_string())];
        let argv = vec!["codex".to_string()];
        let mut s = Session::spawn_argv(
            &argv,
            Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))),
            &env,
            ctx,
        )
        .expect("spawn failed");
        unsafe { std::env::remove_var("FOREMAN_RX_DUMP") };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        while std::time::Instant::now() < deadline && s.exited().is_none() {
            s.pump();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let vp = crate::graphics::ViewportView {
            alt_screen: s.term.mode().contains(TermMode::ALT_SCREEN),
            history_size: s.term.grid().history_size(),
            display_offset: 0,
            screen_lines: s.term.grid().screen_lines(),
        };
        println!(
            "cursor: line {} col {}  history {}  alt {}",
            s.term.grid().cursor.point.line.0,
            s.term.grid().cursor.point.column.0,
            vp.history_size,
            vp.alt_screen,
        );
        for p in s.graphics.visible(&vp) {
            println!(
                "placed id={} at (line {}, col {}) px {}x{} cells {:?}x{:?}",
                p.id, p.line, p.col, p.w, p.h, p.cols, p.rows
            );
        }
        for r in 0..s.rows as i32 {
            println!("|{}|", grid_row(&s, r, s.cols));
        }
        let _ = s.child.kill();
        let bytes = std::fs::read(&dump).expect("no dump written");
        println!("captured {} bytes -> {}", bytes.len(), dump.display());
        assert!(!bytes.is_empty(), "no rx captured");
    }

    /// Companion analyzer for `codex_pet_rx_capture`: walks the captured dump
    /// and prints every kitty APC header plus the raw (escaped) bytes between
    /// consecutive APCs, so the cursor moves around the chunk chain are
    /// visible. Pure file analysis — no PTY, no GUI.
    /// Run: cargo test --release codex_pet_rx_analyze -- --ignored --nocapture
    #[test]
    #[ignore = "diagnostic: analyzes a prior capture"]
    fn codex_pet_rx_analyze() {
        let dump = std::env::temp_dir().join("foreman-codex-rx.bin");
        let bytes = std::fs::read(&dump).expect("run codex_pet_rx_capture first");
        let esc = |b: &[u8]| -> String {
            b.iter()
                .map(|&c| match c {
                    0x1b => "\\e".to_string(),
                    0x20..=0x7e => (c as char).to_string(),
                    _ => format!("\\x{c:02x}"),
                })
                .collect()
        };
        let mut i = 0;
        let mut last_end = 0usize;
        while i + 2 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'_' && bytes[i + 2] == b'G' {
                let hdr_end = bytes[i..]
                    .iter()
                    .position(|&c| c == b';')
                    .map(|o| i + o)
                    .unwrap_or_else(|| (i + 80).min(bytes.len()));
                let mut end = bytes.len();
                let mut j = i + 3;
                while j + 1 < bytes.len() {
                    if bytes[j] == 0x1b && bytes[j + 1] == b'\\' {
                        end = j + 2;
                        break;
                    }
                    j += 1;
                }
                let gap = &bytes[last_end..i];
                let shown = if gap.len() > 600 {
                    format!(
                        "{} ...[{} bytes]... {}",
                        esc(&gap[..300]),
                        gap.len() - 600,
                        esc(&gap[gap.len() - 300..])
                    )
                } else {
                    esc(gap)
                };
                println!("GAP [{last_end}..{i}] ({}b): {shown}", gap.len());
                println!(
                    "APC @{i} len {}: {}",
                    end - i,
                    esc(&bytes[i..hdr_end.min(end)])
                );
                last_end = end;
                i = end;
            } else {
                i += 1;
            }
        }
        let tail = &bytes[last_end..(last_end + 600).min(bytes.len())];
        println!(
            "== trailing {} bytes after last APC ==\n{}",
            bytes.len() - last_end,
            esc(tail)
        );
    }

    #[test]
    #[ignore = "perf: cargo test --release scanner_overhead -- --ignored --nocapture"]
    fn scanner_overhead_on_plain_and_ansi_floods() {
        let plain = {
            let line = "x".repeat(120) + "\r\n";
            line.repeat(200_000).into_bytes() // ~24 MB
        };
        let ansi = {
            let mut v = Vec::new();
            for r in 0..200_000 {
                v.extend_from_slice(
                    format!(
                        "\x1b[{};1H\x1b[38;5;{}mrow of colorful tui text",
                        (r % 40) + 1,
                        r % 256
                    )
                    .as_bytes(),
                );
            }
            v
        };
        for (name, corpus) in [("plain", &plain), ("ansi", &ansi)] {
            let mut term = term_with(b"", 120, 40);
            let mut parser: Processor = Processor::new();
            let t0 = std::time::Instant::now();
            parser.advance(&mut term, corpus);
            let vte = t0.elapsed();
            let mut g = crate::graphics::Graphics::default();
            let t1 = std::time::Instant::now();
            let cuts = g.feed(corpus);
            let scan = t1.elapsed();
            assert!(cuts.is_empty());
            println!(
                "{name}: vte {vte:?} ({:.0} MB/s) | scanner {scan:?} ({:.0} MB/s) | overhead {:.2}%",
                corpus.len() as f64 / vte.as_secs_f64() / 1e6,
                corpus.len() as f64 / scan.as_secs_f64() / 1e6,
                100.0 * scan.as_secs_f64() / vte.as_secs_f64(),
            );
        }
    }

    fn sel_range((l0, c0): (i32, usize), (l1, c1): (i32, usize)) -> SelectionRange {
        SelectionRange {
            start: Point::new(Line(l0), Column(c0)),
            end: Point::new(Line(l1), Column(c1)),
            is_block: false,
        }
    }

    #[test]
    fn sel_viewport_range_passes_a_fully_visible_range_through() {
        let r = sel_viewport_range(sel_range((1, 2), (3, 4)), 0, 10, 20).unwrap();
        assert_eq!((r.start, r.end), ((1, 2), (3, 4)));
    }

    #[test]
    fn sel_viewport_range_shifts_with_display_offset_so_selection_sticks_to_content() {
        let r = sel_viewport_range(sel_range((1, 2), (3, 4)), 2, 10, 20).unwrap();
        assert_eq!((r.start, r.end), ((3, 2), (5, 4)));
    }

    #[test]
    fn sel_viewport_range_culls_a_start_above_the_viewport_to_the_origin() {
        // The start row is scrolled off the top; its column no longer applies
        // to the first visible row, which is mid-selection and fully covered.
        let r = sel_viewport_range(sel_range((-3, 5), (1, 4)), 1, 10, 20).unwrap();
        assert_eq!((r.start, r.end), ((0, 0), (2, 4)));
    }

    #[test]
    fn sel_viewport_range_culls_an_end_below_the_viewport_to_the_last_cell() {
        let r = sel_viewport_range(sel_range((3, 2), (12, 4)), 0, 10, 20).unwrap();
        assert_eq!((r.start, r.end), ((3, 2), (9, 19)));
    }

    #[test]
    fn sel_viewport_range_is_none_when_entirely_above_the_viewport() {
        assert!(sel_viewport_range(sel_range((-5, 0), (-2, 4)), 0, 10, 20).is_none());
    }

    #[test]
    fn sel_viewport_range_is_none_when_entirely_below_the_viewport() {
        assert!(sel_viewport_range(sel_range((2, 0), (4, 4)), 20, 10, 20).is_none());
    }

    // ---- pins of alacritty's Selection semantics (the Phase 4 contract) ----

    fn term_with(bytes: &[u8], cols: usize, rows: usize) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &Size { cols, rows }, VoidListener);
        feed_term(&mut term, bytes);
        term
    }

    fn feed_term(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, bytes);
    }

    fn select(
        term: &mut Term<VoidListener>,
        ty: SelectionType,
        (l0, c0, s0): (i32, usize, Side),
        head: Option<(i32, usize, Side)>,
    ) {
        let mut sel = Selection::new(ty, Point::new(Line(l0), Column(c0)), s0);
        if let Some((l1, c1, s1)) = head {
            sel.update(Point::new(Line(l1), Column(c1)), s1);
        }
        term.selection = Some(sel);
    }

    #[test]
    fn simple_selection_copies_the_dragged_span() {
        let mut term = term_with(b"hello world", 20, 4);
        select(
            &mut term,
            SelectionType::Simple,
            (0, 0, Side::Left),
            Some((0, 4, Side::Right)),
        );
        assert_eq!(term.selection_to_string().as_deref(), Some("hello"));
    }

    #[test]
    fn semantic_selection_expands_to_word_boundaries() {
        let mut term = term_with(b"cargo test --lib", 30, 4);
        // double-click lands mid-"test"
        select(&mut term, SelectionType::Semantic, (0, 7, Side::Left), None);
        assert_eq!(term.selection_to_string().as_deref(), Some("test"));
    }

    #[test]
    fn semantic_selection_takes_a_whole_path() {
        let mut term = term_with(b"see docs/epics/file.md here", 40, 4);
        select(&mut term, SelectionType::Semantic, (0, 8, Side::Left), None);
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("docs/epics/file.md")
        );
    }

    #[test]
    fn lines_selection_takes_the_whole_line_with_a_trailing_newline() {
        let mut term = term_with(b"alpha beta", 20, 4);
        select(&mut term, SelectionType::Lines, (0, 4, Side::Left), None);
        assert_eq!(term.selection_to_string().as_deref(), Some("alpha beta\n"));
    }

    #[test]
    fn cjk_selection_copies_whole_glyphs_and_highlights_their_full_span() {
        // 你 and 好 each occupy two columns (wide char + spacer), cols 0..=3.
        let mut term = term_with("你好".as_bytes(), 20, 4);
        select(
            &mut term,
            SelectionType::Simple,
            (0, 0, Side::Left),
            Some((0, 3, Side::Right)),
        );
        assert_eq!(term.selection_to_string().as_deref(), Some("你好"));
        let range = term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&term))
            .unwrap();
        let vr = sel_viewport_range(range, 0, 4, 20).unwrap();
        assert_eq!(
            (vr.start, vr.end),
            ((0, 0), (0, 3)),
            "highlight spans both columns of each glyph"
        );
    }

    #[test]
    fn semantic_selection_expands_over_a_cjk_run() {
        let mut term = term_with("你好 abc".as_bytes(), 20, 4);
        // double-click on 好 (its wide cell is col 2)
        select(&mut term, SelectionType::Semantic, (0, 2, Side::Left), None);
        assert_eq!(term.selection_to_string().as_deref(), Some("你好"));
    }

    #[test]
    fn selection_sticks_to_content_as_new_output_scrolls_it_into_history() {
        let mut term = term_with(b"target", 10, 2);
        select(
            &mut term,
            SelectionType::Simple,
            (0, 0, Side::Left),
            Some((0, 5, Side::Right)),
        );
        // Two more lines push "target" into scrollback; the Term rotates the
        // selection's buffer points with it (the Q5 stick-to-content behavior).
        feed_term(&mut term, b"\r\nnext\r\nmore");
        assert_eq!(term.selection_to_string().as_deref(), Some("target"));
    }

    #[test]
    fn out_of_bounds_selection_points_are_clamped_not_panicking() {
        // Stale drag coords can outlive a grid shrink (alt-screen/resize);
        // to_range must clamp them — Line/Column indexing panics otherwise and
        // a panic in the winit callback aborts the process.
        let mut term = term_with(b"hi", 4, 2);
        select(
            &mut term,
            SelectionType::Semantic,
            (50, 50, Side::Left),
            Some((-50, 50, Side::Right)),
        );
        let _ = term.selection_to_string();
        let _ = term.selection.as_ref().and_then(|s| s.to_range(&term));
    }

    #[test]
    fn selection_survives_an_actual_grid_shrink_without_panicking() {
        let mut term = term_with(b"0123456789 the quick brown fox", 40, 10);
        select(
            &mut term,
            SelectionType::Simple,
            (0, 35, Side::Left),
            Some((8, 39, Side::Right)),
        );
        term.resize(Size { cols: 4, rows: 2 });
        if let Some(r) = term.selection.as_ref().and_then(|s| s.to_range(&term)) {
            let _ = sel_viewport_range(
                r,
                term.grid().display_offset(),
                term.screen_lines(),
                term.columns(),
            );
        }
        let _ = term.selection_to_string();
    }
}
