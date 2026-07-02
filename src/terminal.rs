use eframe::egui;
use eframe::egui::text::LayoutJob;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
        graphics.apply(term_view(term), replies);
    }
    parser.advance(term, &bytes[at..]);
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
    // When to send the deferred chat-submit `\r`; fired by pump(). See
    // inject_input for why the submit cannot ride with the paste.
    pending_submit: Option<std::time::Instant>,
    // Chat input that arrived before `ready`; held here and flushed by pump()
    // once the startup DSR scan resolves (see inject_input).
    pending_inject: Vec<String>,
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
            let mut buf = [0u8; 8192];
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
            exit: None,
            exit_noted: false,
            shell,
            dispatch_argv: None,
            root_pid,
            term_id: 0,
            cols,
            rows,
            pending_note: None,
            pending_submit: None,
            pending_inject: Vec::new(),
            ready: false,
            output_gen: 0,
            caret: crate::caret::CaretGate::new(std::time::Instant::now()),
            graphics: crate::graphics::Graphics::default(),
            textures: std::collections::HashMap::new(),
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
        if !self.ready {
            // Hold the post until the startup DSR scan resolves; a paste sent now
            // gets swallowed by it. pump() flushes the queue once ready.
            self.pending_inject.push(text.to_string());
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
        let mut greplies = Vec::new();
        while let Ok(bytes) = self.rx.try_recv() {
            advance_scanned(
                &mut self.parser,
                &mut self.term,
                &mut self.graphics,
                &bytes,
                &mut greplies,
            );
            self.output_gen = self.output_gen.wrapping_add(1);
        }
        // Graphics replies (a=q probes etc.) go straight back to the app — NOT
        // via `resp`: that buffer's flush is what latches `ready` (the DSR
        // contract), and a graphics reply must never fake readiness.
        if !greplies.is_empty() {
            let _ = self.writer.write_all(&greplies);
            let _ = self.writer.flush();
        }
        let reply = std::mem::take(&mut *self.resp.lock().unwrap());
        if !reply.is_empty() {
            let _ = self.writer.write_all(&reply);
            let _ = self.writer.flush();
            // First device-status reply flushed back = the startup DSR scan is
            // done; input injected from here on reaches the child (see `ready`).
            self.ready = true;
        }
        // Now that the scan is done, flush any post that arrived before readiness
        // (inject_input held it). Ready is true here, so each reaches the child.
        if self.ready && !self.pending_inject.is_empty() {
            for text in std::mem::take(&mut self.pending_inject) {
                self.inject_input(&text);
            }
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
        // Known limitation: narrowing past a wrapped prompt then recalling history
        // (Up) corrupts the line until Ctrl+L. This is ConPTY's reflow diverging
        // from our grid (microsoft/terminal #18725), NOT a double reflow here —
        // ConPTY reports a cursor inconsistent with its own repaint. Letting
        // ConPTY own the redraw does not help; only conhost-parity reflow would.
        // See docs/conpty-resize-reflow.md before touching this.
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
        let outcome =
            ui.input(|i| crate::input::process_input(&i.events, i.modifiers, mode, has_selection));

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
            } else if clipboard_has_image() {
                // Image-only clipboard: forward raw Ctrl+V so agents (Claude,
                // Codex) run their native clipboard-image paste. Plain shells
                // see readline quoted-insert — harmless. (spec WS2)
                bytes.push(0x16);
            }
        }
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
            let c = self.term.renderable_content();
            (
                c.cursor.point.line.0,
                c.cursor.point.column.0,
                c.cursor.shape,
            )
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

        // The frame's paint geometry + content (pure). show() only replays it here,
        // deciding visibility (focus/hover) and the paint style (colors, radii).
        let plan = crate::frame::plan(self.term.grid(), &metrics, sel, cursor_draw);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::ZERO, BG);

        // Text: one TextFormat append per style run, a newline after each row.
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        for runs in &plan.rows {
            for r in runs {
                let st = r.style;
                let line = |on: bool| {
                    if on {
                        egui::Stroke::new(1.0, st.fg)
                    } else {
                        egui::Stroke::NONE
                    }
                };
                job.append(
                    &r.text,
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
            }
            job.append("\n", 0.0, egui::TextFormat::default());
        }
        let galley = painter.layout_job(job);
        painter.galley(rect.min, galley, FG);

        // Kitty graphics overlay — images are pure overlay; the grid stays
        // text (spec: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md).
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
            // Drop textures whose image data is gone (deleted/evicted).
            self.textures.retain(|id, _| self.graphics.has_image(*id));
        }

        for r in &plan.highlights {
            painter.rect_filled(*r, egui::CornerRadius::ZERO, SELECTION);
        }

        // caret — the gate chose the cell; focus (`active`) gates whether we paint.
        if active && let Some(r) = plan.caret {
            painter.rect_filled(r, egui::CornerRadius::ZERO, CARET);
        }

        // scrollback indicator: thin right-edge thumb, shown only when there is
        // history and the user is scrolled back or hovering the pane.
        if let Some(r) = plan.thumb
            && (plan.scrolled_back || resp.hovered())
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

    fn named(n: NamedColor) -> AnsiColor {
        AnsiColor::Named(n)
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
    fn inject_before_ready_is_queued_then_flushed() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        assert!(!s.ready(), "freshly spawned: not ready");
        // A post during the startup window must be held, not pasted (a paste now
        // gets eaten by the DSR scan), so no submit is armed yet.
        s.inject_input("hello room");
        assert!(
            s.pending_submit.is_none(),
            "injection before ready must not arm the submit"
        );
        assert!(
            !s.pending_inject.is_empty(),
            "injection before ready must be queued"
        );
        // The pump that latches readiness also flushes the held post.
        let mut flushed = false;
        for _ in 0..200 {
            s.pump();
            if s.ready() && s.pending_inject.is_empty() {
                flushed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(flushed, "queued post never flushed after becoming ready");
        assert!(
            s.pending_submit.is_some(),
            "flushing the queue arms the deferred submit"
        );
    }

    #[test]
    fn inject_input_defers_the_submit_keypress() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        // Injection now waits for readiness; clear the startup DSR scan first so
        // this test exercises the submit-defer timing, not the readiness gate.
        for _ in 0..200 {
            s.pump();
            if s.ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(s.ready(), "session never became ready");
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
