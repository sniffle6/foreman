use eframe::egui;
use eframe::egui::text::LayoutJob;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

pub const FG: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);
pub const BG: egui::Color32 = egui::Color32::from_rgb(20, 18, 15);

const PALETTE: [egui::Color32; 16] = [
    egui::Color32::from_rgb(43, 40, 36),
    egui::Color32::from_rgb(207, 91, 72),
    egui::Color32::from_rgb(148, 163, 109),
    egui::Color32::from_rgb(231, 169, 63),
    egui::Color32::from_rgb(96, 143, 176),
    egui::Color32::from_rgb(176, 122, 161),
    egui::Color32::from_rgb(116, 176, 164),
    egui::Color32::from_rgb(204, 198, 184),
    egui::Color32::from_rgb(111, 106, 93),
    egui::Color32::from_rgb(226, 97, 59),
    egui::Color32::from_rgb(174, 189, 127),
    egui::Color32::from_rgb(240, 197, 96),
    egui::Color32::from_rgb(122, 167, 199),
    egui::Color32::from_rgb(199, 155, 184),
    egui::Color32::from_rgb(143, 199, 187),
    egui::Color32::from_rgb(236, 231, 218),
];

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

/// Resolved per-cell display style: foreground/background after the inverse swap
/// and dim, plus the line decorations. Pure, so the inverse/dim/flag logic is
/// unit-tested apart from the egui painter; `show` turns this into a `TextFormat`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GlyphStyle {
    fg: egui::Color32,
    bg: Option<egui::Color32>,
    underline: bool,
    strikethrough: bool,
}

fn glyph_style(flags: Flags, fg: AnsiColor, bg: AnsiColor) -> GlyphStyle {
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
impl Shell {
    fn program(self) -> &'static str {
        match self {
            Shell::Cmd => "cmd.exe",
            Shell::PowerShell => "powershell.exe",
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
}
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            if let Ok(mut b) = self.out.lock() {
                b.extend_from_slice(text.as_bytes());
            }
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

pub struct Session {
    term: Term<Listener>,
    parser: Processor,
    rx: Receiver<Vec<u8>>,
    resp: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exit: Option<u32>,
    exit_noted: bool,
    pub shell: Shell,
    // The Session's stable Member id, stamped by the window manager at spawn
    // (== the `t{id}` it injects as FOREMAN_TERMINAL_ID). Unlike a Win id it
    // never changes — tabbing, untabbing, and moving leave it alone — so the
    // chat room and the agent always agree on "who". 0 until stamped.
    term_id: u64,
    cols: usize,
    rows: usize,
    sel_anchor: Option<(usize, usize)>, // (row, col) where a selection drag began
    sel_head: Option<(usize, usize)>,   // (row, col) current selection end
    // Dispatch banner queued by inject_note(); flushed (fitted to the real
    // width) by the first resize(). See inject_note for why it is deferred.
    pending_note: Option<String>,
    // When to send the deferred chat-submit `\r`; fired by pump(). See
    // inject_input for why the submit cannot ride with the paste.
    pending_submit: Option<std::time::Instant>,
    // Latches true once the startup DSR (`ESC[6n`) has been answered — the
    // point after which injected input is no longer eaten by the device-status
    // scan. Catch-up replay and cursor advance gate on this (chat handshake
    // contract: the cursor advances only on inject into a READY session).
    ready: bool,
    // Bumped in pump() each time a batch of new PTY bytes arrives. A cheap
    // freshness signal the settle machinery polls to detect terminal activity.
    output_gen: u64,
    // The Caret gate: decides which cell the painted caret rests at, de-jittering
    // a TUI's mid-redraw cursor moves. Owns cursor-stability and input-recency
    // state; fed every frame in show(). See `crate::caret`.
    caret: crate::caret::CaretGate,
    /// Sub-line remainder of wheel scrolling. egui delivers a notch as smoothed
    /// per-frame fractions; carrying the remainder keeps gentle scrolls from
    /// rounding to nothing and fast flicks from over-emitting lines.
    scroll_accum: f32,
    /// Sub-notch remainder of Ctrl+Scroll zooming. Same smoothing problem as
    /// `scroll_accum`, but accumulated against the zoom notch size so a gentle
    /// Ctrl+wheel still eventually steps the font and a fast flick doesn't lurch.
    zoom_accum: f32,
}

/// Gap between a chat paste and its submitting `\r`. Claude Code's TUI folds
/// input arriving within the same few-ms burst as a paste INTO the paste, so
/// a `\r` written back-to-back with `ESC[201~` becomes a literal newline in
/// the input box instead of an Enter keypress (the same reason tmux users
/// must `send-keys "msg"; sleep; send-keys Enter`). One frame later is not
/// enough under load; ~150ms is comfortably past the burst window while
/// still feeling instant.
const SUBMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

/// Smoothed-scroll points that equal one Ctrl+Scroll zoom notch. egui reports a
/// wheel notch as ~50 points of `smooth_scroll_delta`, so dividing by this gives
/// roughly one font step per physical notch.
const ZOOM_NOTCH_PX: f32 = 50.0;

fn read_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
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

/// Bracketed-paste wrapper (`ESC[200~ … ESC[201~`): multi-line text lands in
/// the target's input box as one paste block instead of submitting per line
/// (spec: agent-group-chat §3).
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
        Self::spawn_with(build(argv), Shell::Cmd, ctx.clone()).or_else(|e| {
            if unsafe_for_cmd {
                return Err(refuse(format!("not directly spawnable: {e}")));
            }
            let mut wrapped = vec!["cmd.exe".to_string(), "/c".to_string()];
            wrapped.extend_from_slice(argv);
            Self::spawn_with(build(&wrapped), Shell::Cmd, ctx)
        })
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
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
            }
        });

        let resp = Arc::new(Mutex::new(Vec::new()));
        let term = Term::new(
            Config::default(),
            &Size { cols, rows },
            Listener { out: resp.clone() },
        );
        Ok(Session {
            term,
            parser: Processor::new(),
            rx,
            resp,
            writer,
            master: pair.master,
            child,
            exit: None,
            exit_noted: false,
            shell,
            term_id: 0,
            cols,
            rows,
            sel_anchor: None,
            sel_head: None,
            pending_note: None,
            pending_submit: None,
            ready: false,
            output_gen: 0,
            caret: crate::caret::CaretGate::new(std::time::Instant::now()),
            scroll_accum: 0.0,
            zoom_accum: 0.0,
        })
    }

    /// The Session's stable Member id (see the `term_id` field). 0 until the
    /// window manager stamps it at spawn.
    pub fn term_id(&self) -> u64 {
        self.term_id
    }

    /// Stamp the stable Member id. The window manager calls this once, right
    /// after spawn, with the same id it baked into FOREMAN_TERMINAL_ID.
    pub fn set_term_id(&mut self, id: u64) {
        self.term_id = id;
    }

    /// Has the startup DSR exchange resolved? Once true, injected chat input
    /// reaches the child instead of being swallowed by the device-status scan.
    /// Latched by [`Session::pump`] on the first reply flushed back to the PTY.
    pub fn ready(&self) -> bool {
        self.ready
    }

    /// Exit code of the child process, once it has ended. Cached — `try_wait`
    /// is a cheap non-blocking poll until then.
    pub fn exited(&mut self) -> Option<u32> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait().ok().flatten().map(|s| s.exit_code());
        }
        self.exit
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

    fn cell_at(&self, rect: egui::Rect, cw: f32, rh: f32, pos: egui::Pos2) -> (usize, usize) {
        let col =
            (((pos.x - rect.min.x) / cw).floor() as i64).clamp(0, self.cols as i64 - 1) as usize;
        let row =
            (((pos.y - rect.min.y) / rh).floor() as i64).clamp(0, self.rows as i64 - 1) as usize;
        (row, col)
    }

    fn selection_text(&self) -> Option<String> {
        let (a, b) = (self.sel_anchor?, self.sel_head?);
        if a == b {
            return None;
        }
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        let grid = self.term.grid();
        let off = grid.display_offset() as i32;
        // Selection coords were cached on an earlier frame; the grid may have shrunk
        // since (TUI alt-screen/resize), so clamp every index to the grid's REAL
        // bounds — both Line and Column panic if indexed out of range (same hazard
        // the render loop in `show` guards against).
        let g_cols = grid.columns();
        let g_lines = grid.screen_lines();
        let mut out = String::new();
        for row in s.0..=e.0 {
            if row >= g_lines {
                break;
            }
            let c0 = if row == s.0 { s.1 } else { 0 };
            let c1 = if row == e.0 {
                e.1
            } else {
                g_cols.saturating_sub(1)
            };
            let mut line = String::new();
            for col in c0..=c1.min(g_cols.saturating_sub(1)) {
                let ch = grid[Line(row as i32 - off)][Column(col)].c;
                line.push(if ch == '\0' { ' ' } else { ch });
            }
            out.push_str(line.trim_end());
            if row != e.0 {
                out.push('\n');
            }
        }
        (!out.is_empty()).then_some(out)
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
    /// The submit is DEFERRED by [`SUBMIT_DELAY`], not written with the
    /// paste: a back-to-back `\r` gets folded into the paste by Claude
    /// Code's burst detection and lands as a literal newline (live failure
    /// 2026-06-10 — message sat unsubmitted in the input box). pump() fires
    /// it once the deadline passes; the frame loop pumps every session every
    /// ~16ms, so no extra repaint plumbing is needed. Accepted quirks: two
    /// posts inside the window merge into one submitted turn for the
    /// receiver, and bytes buffered through a member's entire boot can still
    /// coalesce (residual; revisit with age-gating if it bites).
    pub fn inject_input(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.send(&paste_wrap(text));
        self.pending_submit = Some(std::time::Instant::now() + SUBMIT_DELAY);
    }

    /// Raw PTY write — bypasses bracketed-paste and the submit delay. Used by
    /// `foreman send` to deliver pre-encoded bytes (text + key sequences).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.send(bytes);
    }

    /// The terminal's current mode flags — used by `foreman send` to encode
    /// named keys through the same path the live keyboard uses.
    pub fn term_mode(&self) -> alacritty_terminal::term::TermMode {
        *self.term.mode()
    }

    /// Counter bumped every time new PTY bytes arrive in `pump()`. The settle
    /// machinery polls this to detect whether a terminal is still producing output.
    pub fn output_gen(&self) -> u64 {
        self.output_gen
    }

    /// Pump pending PTY output into the grid, then return the rendered viewport
    /// as plain text rows (trailing spaces trimmed). Used by `foreman snapshot`.
    pub fn snapshot_text(&mut self, region: Option<crate::inspect::Region>) -> Vec<String> {
        self.pump();
        crate::inspect::snapshot_text(&self.term, region)
    }

    /// Pump pending PTY output, then return per-cell attribute data (`--attrs`).
    pub fn snapshot_cells(
        &mut self,
        region: Option<crate::inspect::Region>,
    ) -> Vec<Vec<crate::inspect::CellData>> {
        self.pump();
        crate::inspect::snapshot_cells(&self.term, region)
    }

    /// Pump pending PTY output, then return the cursor position + shape (`--cursor`).
    pub fn cursor_info(&mut self) -> crate::inspect::CursorInfo {
        self.pump();
        crate::inspect::cursor_info(&self.term)
    }

    fn pump(&mut self) {
        while let Ok(bytes) = self.rx.try_recv() {
            self.parser.advance(&mut self.term, &bytes);
            self.output_gen = self.output_gen.wrapping_add(1);
        }
        let reply = std::mem::take(&mut *self.resp.lock().unwrap());
        if !reply.is_empty() {
            let _ = self.writer.write_all(&reply);
            let _ = self.writer.flush();
            // First device-status reply flushed back = the startup DSR scan is
            // done; input injected from here on reaches the child (see `ready`).
            self.ready = true;
        }
        // Deferred chat submit (see inject_input).
        if let Some(due) = self.pending_submit
            && std::time::Instant::now() >= due
        {
            self.pending_submit = None;
            self.send(b"\r");
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        if cols < 2 || rows < 1 {
            return;
        }
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.term.resize(Size { cols, rows });
            // Reflow under a preserved scroll offset points the viewport at stale
            // content; snap back to the live prompt like a normal terminal.
            self.term.scroll_display(Scroll::Bottom);
            let _ = self.master.resize(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
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
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Read this frame's keyboard input and apply it. The pure encoding lives in
    /// `crate::input::process_input` (terminal-completeness epic, Phase 2); this is
    /// the thin shell that supplies live state (term mode, selection), performs the
    /// side effects (clipboard read, copy, interrupt, scroll), and writes the bytes
    /// to the PTY.
    fn read_input(&mut self, ui: &egui::Ui) {
        let mode = *self.term.mode();
        let has_selection = match (self.sel_anchor, self.sel_head) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let outcome = ui.input(|i| crate::input::process_input(&i.events, mode, has_selection));

        if let Some(s) = outcome.scroll {
            self.term.scroll_display(s);
        }

        // Ctrl+0 resets the global terminal zoom to the default size.
        if outcome.zoom_reset {
            set_font_size(ui.ctx(), crate::config::DEFAULT_FONT_SIZE);
        }

        let mut bytes = outcome.pty_bytes;
        // Ctrl+Shift+V: the pure pass can't read the clipboard, so it flags the
        // request and we wrap the text here through the same mode-gated helper.
        if outcome.paste_clipboard {
            if let Some(txt) = read_clipboard() {
                bytes.extend_from_slice(&crate::input::paste_seq(mode, &txt));
            }
        }
        if !bytes.is_empty() {
            self.term.scroll_display(Scroll::Bottom);
            self.caret.note_input(std::time::Instant::now());
            self.send(&bytes);
        }

        if outcome.copy {
            if let Some(txt) = self.selection_text() {
                ui.ctx().copy_text(txt);
                if outcome.copy_clears {
                    self.sel_anchor = None;
                    self.sel_head = None;
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
        if active {
            // mouse text selection (the WM hands us the content-area drag)
            if resp.drag_started() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let c = self.cell_at(rect, cw, rh, p);
                    self.sel_anchor = Some(c);
                    self.sel_head = Some(c);
                }
            } else if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.sel_head = Some(self.cell_at(rect, cw, rh, p));
                }
            } else if resp.clicked() {
                self.sel_anchor = None;
                self.sel_head = None;
            }
            if resp.secondary_clicked() {
                if let Some(txt) = read_clipboard() {
                    self.send(txt.as_bytes());
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
            let (dy, ctrl) =
                ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl || i.modifiers.command));
            if ctrl && dy != 0.0 {
                // Ctrl+Scroll zooms the GLOBAL terminal font instead of scrolling.
                // Accumulate against the notch size (same smoothing as line scroll)
                // and step whole notches; the wheel is fully consumed here so it
                // neither moves scrollback nor reaches the app.
                self.zoom_accum += dy / ZOOM_NOTCH_PX;
                let steps = self.zoom_accum.trunc();
                self.zoom_accum -= steps;
                if steps != 0.0 {
                    let next = crate::input::zoom_step(font_size(ui.ctx()), steps);
                    set_font_size(ui.ctx(), next);
                }
            } else if dy != 0.0 {
                self.scroll_accum += dy / rh;
                let lines = self.scroll_accum.trunc() as i32;
                self.scroll_accum -= lines as f32;
                if lines != 0 {
                    // pointer → 1-based viewport cell
                    let (col, row) = match resp.hover_pos() {
                        Some(p) => (
                            (((p.x - rect.min.x) / cw).floor() as i32 + 1).clamp(1, cols as i32) as u16,
                            (((p.y - rect.min.y) / rh).floor() as i32 + 1).clamp(1, rows as i32) as u16,
                        ),
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
            let c = self.term.renderable_content();
            (c.cursor.point.line.0, c.cursor.point.column.0, c.cursor.shape)
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

        let grid = self.term.grid();
        let off = grid.display_offset() as i32;
        let hist = grid.history_size();
        // `pump()` advanced the parser THIS frame, so the grid's real size can
        // momentarily differ from the cached cols/rows (alt-screen swap, reset, or
        // column-mode from a full-screen TUI like `claude`). Index against the
        // grid's ACTUAL bounds: a stale index panics, and a panic across the winit
        // callback aborts the whole process.
        let ncols = self.cols.min(grid.columns());
        let nrows = self.rows.min(grid.screen_lines());
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        for row in 0..nrows {
            let mut run = String::new();
            let mut run_style = GlyphStyle {
                fg: FG,
                bg: None,
                underline: false,
                strikethrough: false,
            };
            let flush = |job: &mut LayoutJob, run: &mut String, st: GlyphStyle| {
                if run.is_empty() {
                    return;
                }
                let line = |on: bool| {
                    if on {
                        egui::Stroke::new(1.0, st.fg)
                    } else {
                        egui::Stroke::NONE
                    }
                };
                job.append(
                    run,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::monospace(font_px),
                        color: st.fg,
                        background: st.bg.unwrap_or(egui::Color32::TRANSPARENT),
                        underline: line(st.underline),
                        strikethrough: line(st.strikethrough),
                        ..Default::default()
                    },
                );
                run.clear();
            };
            for col in 0..ncols {
                let cell = &grid[Line(row as i32 - off)][Column(col)];
                let style = glyph_style(cell.flags, cell.fg, cell.bg);
                if style != run_style {
                    flush(&mut job, &mut run, run_style);
                    run_style = style;
                }
                run.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            flush(&mut job, &mut run, run_style);
            job.append("\n", 0.0, egui::TextFormat::default());
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::ZERO, BG);
        let galley = painter.layout_job(job);
        painter.galley(rect.min, galley, FG);

        // selection highlight (translucent amber over selected cells)
        if let (Some(a), Some(b)) = (self.sel_anchor, self.sel_head) {
            if a != b {
                let (s, e) = if a <= b { (a, b) } else { (b, a) };
                let hl = egui::Color32::from_rgba_unmultiplied(231, 169, 63, 70);
                for row in s.0..=e.0 {
                    let c0 = if row == s.0 { s.1 } else { 0 };
                    let c1 = if row == e.0 {
                        e.1
                    } else {
                        self.cols.saturating_sub(1)
                    };
                    if c1 < c0 {
                        continue;
                    }
                    let x = rect.min.x + c0 as f32 * cw;
                    let y = rect.min.y + row as f32 * rh;
                    let w = (c1 - c0 + 1) as f32 * cw;
                    painter.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, rh)),
                        egui::CornerRadius::ZERO,
                        hl,
                    );
                }
            }
        }

        if let crate::caret::CursorDraw::At { line, col, shape } = cursor_draw
            && active
            && line >= 0
            && off == 0
        {
            let cx = rect.min.x + col as f32 * cw;
            let cy = rect.min.y + line as f32 * rh;
            let amber = egui::Color32::from_rgba_unmultiplied(231, 169, 63, 130);
            // Honor the shape the program asked for: beam (insert mode) and
            // underline are thin bars; block and anything else fill the cell.
            let cur_rect = match shape {
                CursorShape::Beam => {
                    egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(2.0, rh))
                }
                CursorShape::Underline => {
                    egui::Rect::from_min_size(egui::pos2(cx, cy + rh - 2.0), egui::vec2(cw, 2.0))
                }
                _ => egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(cw, rh)),
            };
            painter.rect_filled(cur_rect, egui::CornerRadius::ZERO, amber);
        }

        // scrollback indicator: thin right-edge thumb, shown only when there is
        // history and the user is scrolled back or hovering the pane.
        let total = self.rows + hist;
        if hist > 0 && total > self.rows && (off > 0 || resp.hovered()) {
            let track_h = rect.height();
            let thumb_h = (track_h * self.rows as f32 / total as f32).max(16.0);
            let top_frac = (hist as i32 - off).max(0) as f32 / total as f32;
            let thumb_y = (rect.min.y + track_h * top_frac).min(rect.max.y - thumb_h);
            let w = 4.0;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.max.x - w, thumb_y),
                    egui::vec2(w, thumb_h),
                ),
                egui::CornerRadius::same(2),
                egui::Color32::from_rgba_unmultiplied(231, 169, 63, 150),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(n: NamedColor) -> AnsiColor {
        AnsiColor::Named(n)
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
        for _ in 0..100 {
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

    #[test]
    fn paste_wrap_brackets_text_without_submitting() {
        let b = paste_wrap("line1\nline2");
        assert_eq!(b, b"\x1b[200~line1\nline2\x1b[201~".to_vec());
        assert!(!b.ends_with(b"\r"), "submit must be a separate write");
    }

    #[test]
    fn paste_wrap_neutralizes_embedded_paste_end() {
        let b = paste_wrap("a\x1b[201~rm -rf\r");
        // only the two framing markers may contain ESC
        let interior = &b[6..b.len() - 6];
        assert!(
            !interior.contains(&0x1b),
            "payload ESC must be stripped: {b:?}"
        );
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
        for _ in 0..200 {
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
    fn inject_input_defers_the_submit_keypress() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.inject_input("hello");
        assert!(
            s.pending_submit.is_some(),
            "submit must be deferred, not written with the paste"
        );
        s.pump();
        assert!(
            s.pending_submit.is_some(),
            "a pump before the deadline must not fire the submit"
        );
        // a second post inside the window refreshes the deadline (posts merge
        // into one submitted turn — accepted quirk)
        s.inject_input("world");
        assert!(s.pending_submit.is_some());
        std::thread::sleep(SUBMIT_DELAY + std::time::Duration::from_millis(30));
        s.pump();
        assert!(
            s.pending_submit.is_none(),
            "a pump past the deadline fires the submit exactly once"
        );
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
    fn exit_is_noted_exactly_once() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        let mut noted = None;
        for _ in 0..100 {
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
}
