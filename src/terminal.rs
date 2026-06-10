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

fn resolve(c: AnsiColor) -> Option<egui::Color32> {
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
    cols: usize,
    rows: usize,
    sel_anchor: Option<(usize, usize)>, // (row, col) where a selection drag began
    sel_head: Option<(usize, usize)>,   // (row, col) current selection end
}

fn read_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
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
        Self::spawn_with(build(argv), Shell::Cmd, ctx.clone()).or_else(|_| {
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
            cols,
            rows,
            sel_anchor: None,
            sel_head: None,
        })
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

    /// Write a synthetic note into the emulator (NOT the PTY): renders as a
    /// dim line in the pane. Used to announce a dispatched command before the
    /// child produces output — a `claude -p` worker is silent until done, and
    /// an empty pane reads as hung.
    pub fn inject_note(&mut self, text: &str) {
        let bytes = format!("\x1b[2m{text}\x1b[0m\r\n").into_bytes();
        self.parser.advance(&mut self.term, &bytes);
    }

    fn pump(&mut self) {
        while let Ok(bytes) = self.rx.try_recv() {
            self.parser.advance(&mut self.term, &bytes);
        }
        let reply = std::mem::take(&mut *self.resp.lock().unwrap());
        if !reply.is_empty() {
            let _ = self.writer.write_all(&reply);
            let _ = self.writer.flush();
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        if (cols == self.cols && rows == self.rows) || cols < 2 || rows < 1 {
            return;
        }
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

    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn read_input(&mut self, ui: &egui::Ui) {
        let mut out: Vec<u8> = Vec::new();
        let mut paste_event = false;
        let mut want_clip_paste = false; // Ctrl+Shift+V
        let mut copy_action = 0u8; // 1 = Ctrl+C (copy if selection, else interrupt); 2 = Ctrl+Shift+C
        let mut scroll: Option<Scroll> = None;
        ui.input(|i| {
            for ev in &i.events {
                match ev {
                    egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                    egui::Event::Paste(s) => {
                        out.extend_from_slice(s.as_bytes());
                        paste_event = true;
                    }
                    // egui may deliver Ctrl+C/Ctrl+X as these instead of Key events.
                    egui::Event::Copy | egui::Event::Cut => {
                        if copy_action == 0 {
                            copy_action = 1;
                        }
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        let ctrl = modifiers.ctrl || modifiers.command;
                        // Shift + Home/End/PageUp/PageDown scrolls the scrollback
                        // instead of going to the shell.
                        if modifiers.shift && !ctrl {
                            match key {
                                egui::Key::Home => {
                                    scroll = Some(Scroll::Top);
                                    continue;
                                }
                                egui::Key::End => {
                                    scroll = Some(Scroll::Bottom);
                                    continue;
                                }
                                egui::Key::PageUp => {
                                    scroll = Some(Scroll::PageUp);
                                    continue;
                                }
                                egui::Key::PageDown => {
                                    scroll = Some(Scroll::PageDown);
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        if !ctrl {
                            match key {
                                egui::Key::Enter => out.push(b'\r'),
                                egui::Key::Backspace => out.push(0x7f),
                                egui::Key::Tab => out.push(b'\t'),
                                egui::Key::Escape => out.push(0x1b),
                                egui::Key::ArrowUp => out.extend_from_slice(b"\x1b[A"),
                                egui::Key::ArrowDown => out.extend_from_slice(b"\x1b[B"),
                                egui::Key::ArrowRight => out.extend_from_slice(b"\x1b[C"),
                                egui::Key::ArrowLeft => out.extend_from_slice(b"\x1b[D"),
                                egui::Key::Home => out.extend_from_slice(b"\x1b[H"),
                                egui::Key::End => out.extend_from_slice(b"\x1b[F"),
                                _ => {}
                            }
                            continue;
                        }
                        // ctrl held — terminal-standard copy/paste + control codes
                        match (key, modifiers.shift) {
                            (egui::Key::C, false) => copy_action = 1,
                            (egui::Key::C, true) => copy_action = 2,
                            (egui::Key::V, _) => want_clip_paste = true,
                            (egui::Key::X, _) => {}
                            (k, false) => {
                                let n = k.name();
                                if n.len() == 1 {
                                    let up = n.as_bytes()[0].to_ascii_uppercase();
                                    if up.is_ascii_uppercase() {
                                        out.push(up - 0x40);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        });
        if let Some(s) = scroll {
            self.term.scroll_display(s);
        }
        // Ctrl+Shift+V reads the clipboard directly; Ctrl+V/Shift+Insert come via Event::Paste.
        if want_clip_paste && !paste_event {
            if let Some(txt) = read_clipboard() {
                out.extend_from_slice(txt.as_bytes());
            }
        }
        if !out.is_empty() {
            self.term.scroll_display(Scroll::Bottom);
            self.send(&out);
        }
        match copy_action {
            1 => {
                if let Some(txt) = self.selection_text() {
                    ui.ctx().copy_text(txt);
                    self.sel_anchor = None;
                    self.sel_head = None;
                } else {
                    self.send(&[0x03]); // Ctrl+C with no selection = interrupt
                }
            }
            2 => {
                if let Some(txt) = self.selection_text() {
                    ui.ctx().copy_text(txt);
                }
            }
            _ => {}
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
        let font = egui::FontId::monospace(13.0);
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

        // Mouse-wheel scrollback (works whenever the pane is hovered).
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                let lines = (dy / rh).round() as i32;
                if lines != 0 {
                    self.term.scroll_display(Scroll::Delta(lines));
                }
            }
        }

        let (cur_line, cur_col, cur_visible) = {
            let c = self.term.renderable_content();
            (
                c.cursor.point.line.0,
                c.cursor.point.column.0,
                c.cursor.shape != CursorShape::Hidden,
            )
        };

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
            let mut run_fg = FG;
            let mut run_bg: Option<egui::Color32> = None;
            let flush = |job: &mut LayoutJob,
                         run: &mut String,
                         fg: egui::Color32,
                         bg: Option<egui::Color32>| {
                if run.is_empty() {
                    return;
                }
                job.append(
                    run,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::monospace(13.0),
                        color: fg,
                        background: bg.unwrap_or(egui::Color32::TRANSPARENT),
                        ..Default::default()
                    },
                );
                run.clear();
            };
            for col in 0..ncols {
                let cell = &grid[Line(row as i32 - off)][Column(col)];
                let inverse = cell.flags.contains(Flags::INVERSE);
                let mut fg = resolve(cell.fg).unwrap_or(FG);
                let mut bg = resolve(cell.bg);
                if inverse {
                    let nb = fg;
                    fg = bg.unwrap_or(BG);
                    bg = Some(nb);
                }
                if cell.flags.contains(Flags::DIM) {
                    fg = fg.gamma_multiply(0.7);
                }
                if fg != run_fg || bg != run_bg {
                    flush(&mut job, &mut run, run_fg, run_bg);
                    run_fg = fg;
                    run_bg = bg;
                }
                run.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            flush(&mut job, &mut run, run_fg, run_bg);
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

        if active && cur_visible && cur_line >= 0 && off == 0 {
            let cx = rect.min.x + cur_col as f32 * cw;
            let cy = rect.min.y + cur_line as f32 * rh;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(cw, rh)),
                egui::CornerRadius::ZERO,
                egui::Color32::from_rgba_unmultiplied(231, 169, 63, 130),
            );
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
    fn shell_sessions_still_spawn_with_env() {
        let ctx = egui::Context::default();
        let env = [("FOREMAN".to_string(), "1".to_string())];
        assert!(Session::spawn(Shell::Cmd, None, &env, ctx).is_ok());
    }

    #[test]
    fn inject_note_renders_a_banner_line_in_the_grid() {
        let ctx = egui::Context::default();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        s.inject_note("dispatched: claude -p task");
        let grid = s.term.grid();
        let row: String = (0..40).map(|c| grid[Line(0)][Column(c)].c).collect();
        assert!(
            row.contains("dispatched: claude -p task"),
            "banner not in first grid row: {row:?}"
        );
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
